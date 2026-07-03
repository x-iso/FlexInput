use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use glam::Vec2;
use gilrs::{Axis, Button, EventType, Gilrs, GilrsBuilder};
use hidapi::HidApi;

use flexinput_core::Signal;

use crate::{
    gyro::{self, GyroManager},
    identification::ControllerKind,
    layouts,
    DeviceBackend, PhysicalDevice,
};

// ── XInput force-feedback FFI (no windows-sys dependency) ────────────────────
// Pattern mirrors crates/devices/src/hidhide.rs win32 module.
#[cfg(windows)]
mod xinput_ffi {
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct XINPUT_VIBRATION {
        pub w_left_motor_speed:  u16,
        pub w_right_motor_speed: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct XINPUT_GAMEPAD {
        pub w_buttons:       u16,
        pub b_left_trigger:  u8,
        pub b_right_trigger: u8,
        pub s_thumb_lx:      i16,
        pub s_thumb_ly:      i16,
        pub s_thumb_rx:      i16,
        pub s_thumb_ry:      i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct XINPUT_STATE {
        pub dw_packet_number: u32,
        pub gamepad:          XINPUT_GAMEPAD,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct XINPUT_CAPABILITIES {
        pub b_type:    u8,
        pub b_sub_type: u8,
        pub w_flags:   u16,
        pub gamepad:   XINPUT_GAMEPAD,
        pub vibration: XINPUT_VIBRATION,
    }

    #[link(name = "xinput")]
    extern "system" {
        /// Sets vibration state for an XInput controller slot (0-3).
        /// Returns 0 on success, ERROR_DEVICE_NOT_CONNECTED (1167) if the
        /// controller is disconnected.
        pub fn XInputSetState(dw_user_index: u32, p_vibration: *const XINPUT_VIBRATION) -> u32;
        /// Reads the live state of an XInput slot (0-3). Returns 0 when a
        /// controller occupies the slot, ERROR_DEVICE_NOT_CONNECTED (1167) when empty.
        pub fn XInputGetState(dw_user_index: u32, p_state: *mut XINPUT_STATE) -> u32;
        /// Reads static capabilities (type/subtype/flags) for an XInput slot.
        pub fn XInputGetCapabilities(
            dw_user_index: u32,
            dw_flags: u32,
            p_capabilities: *mut XINPUT_CAPABILITIES,
        ) -> u32;
    }
}

/// One XInput user-index slot's live state, from a direct `XInputGetState` probe.
///
/// XInput exposes a fixed 4-slot model (indices 0-3) and assigns slots by device
/// arrival order — there is no API to *set* a slot. `connected` marks an occupied
/// slot; `packet` is XInput's change counter, which advances whenever that slot's
/// input changes. The packet delta is the key correlation signal: the slot the
/// user is physically pressing advances on its own, while the slot FlexInput drives
/// (its own virtual companion) advances in lockstep with our writes. Off Windows
/// every slot reports empty.
#[derive(Clone, Copy, Debug, Default)]
pub struct XInputSlotInfo {
    pub index:     u32,
    pub connected: bool,
    pub packet:    u32,
    pub buttons:   u16,
    /// XINPUT_CAPABILITIES subtype (e.g. 1 = GAMEPAD). 0 when unknown/empty.
    pub sub_type:  u8,
}

/// Probe all four XInput user-index slots. Pure read via the XInput API (no
/// SetupAPI / hidapi enumeration), so it is cheap enough to call periodically and
/// safe off the device-io hot path. Off Windows returns four empty slots.
pub fn probe_xinput_slots() -> [XInputSlotInfo; 4] {
    let mut out = [XInputSlotInfo::default(); 4];
    for i in 0..4u32 {
        out[i as usize].index = i;
        #[cfg(windows)]
        unsafe {
            use xinput_ffi::*;
            let mut st: XINPUT_STATE = Default::default();
            if XInputGetState(i, &mut st) == 0 {
                let slot = &mut out[i as usize];
                slot.connected = true;
                slot.packet = st.dw_packet_number;
                slot.buttons = st.gamepad.w_buttons;
                let mut caps: XINPUT_CAPABILITIES = Default::default();
                if XInputGetCapabilities(i, 0, &mut caps) == 0 {
                    slot.sub_type = caps.b_sub_type;
                }
            }
        }
    }
    out
}

/// Throttled cache over [`probe_xinput_slots`]. `XInputGetState` on an *empty*
/// slot is comparatively expensive, so polling all four every UI frame would add
/// latency; this refreshes at most every ~200 ms and returns the cached snapshot
/// otherwise. Safe to call from per-frame UI code (e.g. the slot indicator).
pub fn probe_xinput_slots_cached() -> [XInputSlotInfo; 4] {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, [XInputSlotInfo; 4])>> = Mutex::new(None);
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some((t, slots)) = guard.as_ref() {
        if t.elapsed() < Duration::from_millis(200) {
            return *slots;
        }
    }
    let slots = probe_xinput_slots();
    *guard = Some((Instant::now(), slots));
    slots
}

pub struct GilrsBackend {
    gilrs: Gilrs,
    gyro: GyroManager,
    /// XInput user index (0-3) for each `gilrs:<kind>:<inst>` device_id string.
    /// Rebuilt at the start of each poll() call from the same kind_seen counter
    /// used for the dev-string, so indices never drift on reconnect.
    xinput_idx: HashMap<String, u32>,
    /// Last-written rumble state per XInput slot to avoid redundant XInputSetState calls.
    /// (left_motor_byte, right_motor_byte) in 0-255 range.
    xinput_rumble: HashMap<u32, (u8, u8)>,
    /// Per-gilrs-gamepad-id raw event count since the last `take_event_counts`.
    /// Used by the I/O thread to compute live per-device polling rates.
    event_counts: HashMap<usize, u32>,
    /// Maps gilrs-internal gamepad id → device_id string. Refreshed each
    /// `poll()` so the I/O thread can convert event counts to per-device
    /// rates.
    id_to_dev: HashMap<usize, String>,
    /// Cached own-virtual classification for PS-family pads, keyed by
    /// `(vid, pid, vp_idx)`. Rebuilt by `refresh_virtual_classification()` during
    /// `enumerate()` (which does the hidapi path lookup) and read without I/O by
    /// `poll()` / `lookup_phys()` at full rate. See those methods for why the
    /// HID instance path is the discriminator (gilrs name/uuid are unusable).
    virt_cache: HashMap<(u16, u16, usize), bool>,
    /// Set when a gilrs Connected/Disconnected/Dropped event is seen in `poll()`,
    /// and on first run. `enumerate()` only re-runs the own-virtual classification
    /// (the ~200 ms hidapi `refresh_devices`) when this is set, then clears it.
    /// The virtual/real disposition can only change when a device is plugged or
    /// unplugged, so re-classifying every 2 s in steady state was pure cost — and
    /// that hidapi refresh ran ON the real-time I/O loop, freezing ALL input for
    /// ~200 ms each time (the periodic-gap bug). See `refresh_virtual_classification`.
    dev_set_dirty: bool,
    /// Resolved XInput user-index slot per physical-XInput `dev_id`, used for the
    /// focus-independent `XInputGetState` read. XInput exposes no device identity,
    /// so we correlate each gilrs XInput pad to a slot by matching live state
    /// (buttons + sticks) while the app is focused, then CACHE it so the mapping
    /// survives focus loss. This is what makes the direct read land on the pad's
    /// actually-allocated slot rather than its mere arrival index — correct even
    /// when the virtual was deployed first or the slots were reordered.
    xinput_slot_for: HashMap<String, u32>,
    /// Job side of the background refresh/classify worker ("hid-classify").
    /// `None` when hidapi failed to init or the thread couldn't spawn —
    /// classification then falls back to "nothing is virtual" (the old no-api
    /// behavior).
    class_tx: Option<mpsc::Sender<ClassifyJob>>,
    /// Result side of the worker; drained (latest-wins) at the top of
    /// `enumerate()`.
    class_rx: Option<mpsc::Receiver<ClassifyDone>>,
    /// Generation of the newest classification job sent / adopted. While
    /// `class_done_gen < class_req_gen` a rebuild is in flight and `enumerate()`
    /// serves the previous stable list instead of classifying from stale data.
    class_req_gen: u64,
    class_done_gen: u64,
    /// The last device list `enumerate()` built — re-served while a
    /// classification rebuild is in flight so the UI list never flickers and the
    /// I/O thread never waits on hidapi.
    last_enum: Vec<PhysicalDevice>,
}

/// Request to the "hid-classify" worker: refresh the shared hidapi list, then
/// classify these `(vid, pid, vp_idx)` PS-family keys as own-virtual or real.
struct ClassifyJob {
    gen: u64,
    keys: Vec<(u16, u16, usize)>,
}

/// Worker result: the freshly-computed own-virtual cache for `gen`'s key set.
struct ClassifyDone {
    gen: u64,
    cache: HashMap<(u16, u16, usize), bool>,
}

/// Background worker loop: owns the slow side of the shared hidapi instance.
/// Coalesces queued jobs to the newest (a burst of device-set changes needs
/// only the final classification), refreshes the device list (~110-200 ms on
/// Windows — the exact call that used to stall the 500 Hz I/O loop), and
/// classifies under the same lock so the list can't change between refresh and
/// classification.
fn classify_worker(
    api: Arc<Mutex<HidApi>>,
    jobs: mpsc::Receiver<ClassifyJob>,
    done: mpsc::Sender<ClassifyDone>,
) {
    while let Ok(mut job) = jobs.recv() {
        while let Ok(newer) = jobs.try_recv() {
            job = newer;
        }
        let cache = match api.lock() {
            Ok(mut a) => {
                let _ = a.refresh_devices();
                job.keys
                    .iter()
                    .map(|&(v, p, i)| ((v, p, i), gyro::classify_own_virtual(&a, v, p, i)))
                    .collect()
            }
            Err(_) => HashMap::new(),
        };
        if done.send(ClassifyDone { gen: job.gen, cache }).is_err() {
            break; // backend dropped — exit the thread
        }
    }
}

impl GilrsBackend {
    pub fn try_new() -> Option<Self> {
        let gilrs = GilrsBuilder::new().with_default_filters(false).build().ok()?;
        let gyro = GyroManager::new();

        // Spawn the background refresh/classify worker. It shares the gyro
        // layer's hidapi instance (single HidApi per process) and is the only
        // code allowed to call the slow refresh_devices().
        let mut class_tx = None;
        let mut class_rx = None;
        if let Some(api) = gyro.shared_api() {
            let (jtx, jrx) = mpsc::channel::<ClassifyJob>();
            let (dtx, drx) = mpsc::channel::<ClassifyDone>();
            let spawned = std::thread::Builder::new()
                .name("hid-classify".into())
                .spawn(move || classify_worker(api, jrx, dtx))
                .is_ok();
            if spawned {
                class_tx = Some(jtx);
                class_rx = Some(drx);
            }
        }

        Some(Self {
            gilrs,
            gyro,
            xinput_idx: HashMap::new(),
            xinput_rumble: HashMap::new(),
            event_counts: HashMap::new(),
            id_to_dev: HashMap::new(),
            virt_cache: HashMap::new(),
            dev_set_dirty: true, // force classification on first enumerate
            xinput_slot_for: HashMap::new(),
            class_tx,
            class_rx,
            class_req_gen: 0,
            class_done_gen: 0,
            last_enum: Vec::new(),
        })
    }
}

/// Per-pad disposition resolved from cached classification via
/// [`GilrsBackend::disposition_for`]. Replaces the old inline name-marker +
/// ViGEm-count heuristic with a direct, path-based virtual/real decision.
#[derive(Clone, Copy)]
enum PadDisposition {
    /// Keep this pad. `is_virt` = it's one of FlexInput's own emulated devices
    /// (our HIDMaestro virtual Xbox 360, or a path-classified PS-family virtual).
    Keep { is_virt: bool },
    /// Hide this pad from the device list entirely: a redundant second face of a
    /// device we already keep (the HIDMaestro identity HID node when the OS
    /// surfaces it alongside the companion — see `disposition_for`).
    Drop,
}

impl GilrsBackend {
    /// Kick an async own-virtual classification rebuild for the current pad set.
    /// Replaces the old synchronous `refresh_virtual_classification()`: the
    /// expensive part (hidapi `refresh_devices` + per-instance path lookup, ~110-
    /// 200 ms on Windows) now runs on the "hid-classify" worker, so `enumerate()`
    /// never blocks the 500 Hz I/O loop. The cache maps `(vid, pid, vp_idx) →
    /// is_own_virtual`, where `vp_idx` is the Nth-device index per VID/PID in
    /// gilrs walk order — the same index the gyro layer uses, keeping them
    /// correlated.
    fn kick_classification(&mut self) {
        // Cheap pass: snapshot PS-family (vid, pid, vp_idx) keys from the gilrs
        // walk. (Only PS-family is path-classifiable: DS4/DualSense expose a real
        // HID path hidapi can read. The HIDMaestro XInput companion is an
        // XInput-API device with no matching hidapi HID entry — `disposition_for`
        // handles it by name/PID.)
        let mut keys: Vec<(u16, u16, usize)> = Vec::new();
        let mut vp_idx: HashMap<(u16, u16), usize> = HashMap::new();
        for (_, pad) in self.gilrs.gamepads() {
            if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                let idx = *vp_idx.entry(vp).or_insert(0);
                *vp_idx.get_mut(&vp).unwrap() += 1;
                if is_ps_family(vp.0, vp.1) {
                    keys.push((vp.0, vp.1, idx));
                }
            }
        }
        self.class_req_gen += 1;
        let sent = matches!(
            &self.class_tx,
            Some(tx) if tx.send(ClassifyJob { gen: self.class_req_gen, keys }).is_ok()
        );
        if !sent {
            // No worker (hidapi unavailable or thread gone): fall back to
            // "nothing is virtual", the same behavior the old code had with no
            // hidapi — and mark the generation done so enumerate() proceeds.
            self.virt_cache.clear();
            self.class_done_gen = self.class_req_gen;
        }
    }

    /// Adopt the newest finished classification from the worker, if any.
    fn adopt_classification(&mut self) {
        let Some(rx) = &self.class_rx else { return };
        let mut latest: Option<ClassifyDone> = None;
        while let Ok(d) = rx.try_recv() {
            if latest.as_ref().map_or(true, |l| d.gen > l.gen) {
                latest = Some(d);
            }
        }
        if let Some(d) = latest {
            if d.gen > self.class_done_gen {
                self.virt_cache = d.cache;
                self.class_done_gen = d.gen;
            }
        }
    }

    /// Per-pad disposition from cached state only (no I/O) — safe to call at
    /// 500 Hz. `vp_idx` accumulates the per-VID/PID running index for the PS-family
    /// own-virtual cache lookup, threaded through the caller's walk. (XInput pads
    /// are resolved separately by `find_own_virtual_gilrs_idx`, so they don't use
    /// `vp_idx` and the old ViGEm kept-count is gone.)
    fn disposition_for(
        &self,
        vp: Option<(u16, u16)>,
        name: &str,
        vp_idx: &mut HashMap<(u16, u16), usize>,
    ) -> PadDisposition {
        let Some(vp) = vp else {
            return PadDisposition::Keep { is_virt: false };
        };

        // OUR HIDMaestro virtual Xbox 360 surfaces to gilrs's WGI backend as a
        // pad named "HIDMaestro XInput Companion" with the profile USB PID 0x02FF —
        // groundtruthed live: deploying the virtual with no physical pad shows exactly
        // that one pad and it drives Steam. So 0x02FF / the "HIDMaestro" name IS our
        // virtual — tag it directly. Deterministic and order-independent; no slot or
        // correlation guessing needed. (Earlier sessions wrongly believed 0x02FF was a
        // dead sibling and the companion appeared as 0x028E — the opposite of reality;
        // that inversion is what made every previous read-side attempt fail.)
        //
        // Win10's WGI hid the sibling identity HID node, so the companion was the
        // ONLY 02FF pad. Win11 26H1's GameInput-backed WGI surfaces that node too
        // (as a second controller, named from the 02FF metadata — "Xbox One
        // Controller"), so the same virtual showed up twice (groundtruthed live on
        // 26H1 build 28000). The companion is the working pad; anything else that
        // matches the 02FF signature but NOT the HIDMaestro name is the redundant
        // face — hide it. On Win10, and once the face descriptor is vendor-usage
        // (not a gamepad to WGI at all), the Drop arm simply never fires.
        if is_hidmaestro_virtual_xinput(vp.0, vp.1, name) {
            if name.to_ascii_lowercase().contains("hidmaestro") {
                return PadDisposition::Keep { is_virt: true };
            }
            return PadDisposition::Drop;
        }

        let idx = *vp_idx.entry(vp).or_insert(0);
        *vp_idx.get_mut(&vp).unwrap() += 1;

        // PS-family: own-virtual decided by cached HID-path classification (DS4 /
        // DualSense expose a real HID path hidapi can read and match by HIDMAESTRO).
        if is_ps_family(vp.0, vp.1) {
            let is_virt = self.virt_cache.get(&(vp.0, vp.1, idx)).copied().unwrap_or(false);
            return PadDisposition::Keep { is_virt };
        }

        // Any other XInput-kind pad (physical Xbox 360/One, etc.) is a real device:
        // keep it as physical. A physical Xbox is 045E:028E "Xbox 360 Controller[ for
        // Windows]" and never matches the HIDMaestro tag above. (Legacy ViGEm
        // virtuals, if any still exist before ViGEm removal, also fall here as
        // physical — the same as before HIDMaestro XInput.)
        PadDisposition::Keep { is_virt: false }
    }
}

impl DeviceBackend for GilrsBackend {
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        puffin::profile_function!();
        // Own-virtual classification is rebuilt ONLY when the device set actually
        // changed (`dev_set_dirty`, set by poll() on a Connected/Disconnected/
        // Dropped event, by a gyro open retry wanting a fresh list, and on first
        // run) — and the rebuild runs on the "hid-classify" WORKER, never here:
        // both the old 2 s periodic freeze and its variable-gap successor (the
        // gyro retry path refreshing inline every RETRY_INTERVAL while a pad
        // failed to open) were this hidapi refresh running on the I/O loop.
        self.adopt_classification();
        if self.gyro.take_refresh_wanted() {
            self.dev_set_dirty = true;
        }
        if self.dev_set_dirty && self.last_enum.is_empty() {
            // First run (nothing served yet): kick eagerly so the bounded wait
            // below can catch the result — the first list should not misfile
            // virtuals from an empty cache.
            self.kick_classification();
            self.dev_set_dirty = false;
        }
        if self.class_done_gen < self.class_req_gen {
            // Rebuild in flight. On the very first enumerate give the worker a
            // bounded window so startup doesn't show an empty device list; after
            // that, serve the previous stable list and pick the result up next
            // cycle (≤2 s) — classifying new pads from a stale cache could
            // misfile a virtual as physical.
            if self.last_enum.is_empty() {
                if let Some(rx) = &self.class_rx {
                    if let Ok(d) = rx.recv_timeout(Duration::from_millis(400)) {
                        if d.gen > self.class_done_gen {
                            self.virt_cache = d.cache;
                            self.class_done_gen = d.gen;
                        }
                    }
                }
            }
            if self.class_done_gen < self.class_req_gen {
                return self.last_enum.clone();
            }
        }
        // NOTE: the deferred `kick_classification()` happens at the END of this
        // function, after the walk below. Kicking here and then checking
        // "in flight → serve last_enum" made every call that observed a dirty
        // pad set serve the STALE list — and a pad whose gyro open keeps
        // failing sets `refresh_wanted` every RETRY_INTERVAL, so a disconnected
        // Switch Pro never left the list (its stale entry kept the gyro retry
        // alive, which kept the list stale: a feedback loop). Building the walk
        // first means disconnects drop out immediately (gilrs stops yielding
        // the pad); only virtual/real classification lags one worker round,
        // guarded by the hotplug re-kick below.

        // Per-pad keep/virtual decision (path-based for PS, correlation-based for
        // XInput). Snapshot VID/PID first (drops the gilrs borrow), then resolve
        // dispositions from caches so it can't drift between walks.
        let disp: Vec<PadDisposition> = {
            let pads: Vec<(Option<(u16, u16)>, String)> = self
                .gilrs
                .gamepads()
                .map(|(_, pad)| (pad.vendor_id().zip(pad.product_id()), pad.name().to_string()))
                .collect();
            let mut vp_idx: HashMap<(u16, u16), usize> = HashMap::new();
            pads.into_iter()
                .map(|(vp, name)| self.disposition_for(vp, &name, &mut vp_idx))
                .collect()
        };
        let mut kind_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut virt_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut result = Vec::new();

        for (i, (_id, pad)) in self.gilrs.gamepads().enumerate() {
            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());
            let (dev_id, _inst, is_virt) = match disp.get(i) {
                Some(PadDisposition::Keep { is_virt }) => {
                    gilrs_device_id(*is_virt, kind, &mut kind_seen, &mut virt_seen)
                }
                _ => continue, // Drop (ViGEm virtual beyond real count) or missing
            };

            let display_name = if kind == ControllerKind::Generic {
                pad.name().to_string()
            } else if is_virt {
                format!("{} (virtual)", kind.display_name())
            } else {
                kind.display_name().to_string()
            };

            result.push(PhysicalDevice {
                id: dev_id,
                display_name,
                kind,
                outputs: layouts::outputs_for(kind),
                inputs: layouts::inputs_for(kind),
                instance_path: None,
                // Cheap (already read for `kind`); the slow HID-instance-id lookup
                // for HidHide is deferred to an off-thread reconcile that reads these.
                vid: pad.vendor_id(),
                pid: pad.product_id(),
            });
        }

        // Hotplug race guard: a PS-family pad missing from the cache means the
        // pad set changed between the classify job's snapshot and this walk —
        // its disposition defaulted to "real", which could misfile a virtual.
        // Mark dirty so the next cycle re-kicks the worker with current keys.
        {
            let mut vp_idx: HashMap<(u16, u16), usize> = HashMap::new();
            for (_, pad) in self.gilrs.gamepads() {
                if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                    let idx = *vp_idx.entry(vp).or_insert(0);
                    *vp_idx.get_mut(&vp).unwrap() += 1;
                    if is_ps_family(vp.0, vp.1) && !self.virt_cache.contains_key(&(vp.0, vp.1, idx)) {
                        self.dev_set_dirty = true;
                    }
                }
            }
        }

        // Deferred kick (see note at the top of this function): runs after the
        // walk so the list served this cycle reflects the CURRENT pad set even
        // when it just changed; the fresh classification lands next cycle.
        if self.dev_set_dirty {
            self.kick_classification();
            // Drop cached XInput slot correlations: a connect/disconnect/reorder can
            // move a pad to a different slot, so the mapping must be re-learned from
            // live state rather than trusted across a device-set change.
            self.xinput_slot_for.clear();
            self.dev_set_dirty = false;
        }

        self.last_enum = result.clone();
        result
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        puffin::profile_function!();
        // Rebuild XInput slot map at the start of each poll so it stays in sync
        // with kind_seen even after device reconnects.
        self.xinput_idx.clear();

        // Drain events. Raw events auto-update axis and button state.
        // axis_dpad_to_button is intentionally NOT applied: on BT Switch Pro it creates
        // conflicting synthetic button releases that fight the native WGI DPad button events,
        // causing flicker and broken diagonals. DPad discrete outputs are derived manually
        // below from both axis_data (HAT/USB path) and button_data (BT/WGI path).
        let mut ev_count = 0u32;
        {
            puffin::profile_scope!("gilrs_next_event_drain");
            while let Some(ev) = self.gilrs.next_event() {
                // Count raw events per gilrs gamepad id so the I/O thread can
                // compute live per-device polling rates. We bump on every event
                // (Axis/Button/Connected/Disconnected/Dropped) — a single iter of
                // device data typically produces several events together, which
                // matches what users see as "device polled this frame".
                *self.event_counts.entry(usize::from(ev.id)).or_insert(0) += 1;
                // A device was plugged/unplugged → the own-virtual classification
                // may have changed, so let the next enumerate() re-run it (the only
                // time the ~200 ms hidapi refresh is worth paying). See `dev_set_dirty`.
                if matches!(ev.event, EventType::Connected | EventType::Disconnected | EventType::Dropped) {
                    self.dev_set_dirty = true;
                }
                self.gilrs.update(&ev);
                ev_count += 1;
            }
        }
        // event count is captured below as a data field on a dedicated scope
        // to avoid the function-body profile_scope trap (RAII guard would
        // otherwise span to function exit and falsely include later work).
        let _ = ev_count;

        // Flush staged rumble / lightbar outputs *before* reading inputs so any
        // device.sink writes from the previous frame land on the controller.
        {
            puffin::profile_scope!("gyro_flush_outputs");
            self.gyro.flush_outputs();
        }

        let mut out = Vec::new();
        let mut kind_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut virt_seen: HashMap<ControllerKind, usize> = HashMap::new();
        // Track per-(VID,PID) instance index for gyro correlation.
        let mut gyro_idx: HashMap<(u16, u16), usize> = HashMap::new();
        // Rebuild the gilrs-id → device-id map this frame so event counts
        // can be resolved to per-device polling rates.
        self.id_to_dev.clear();

        // Resolve per-pad dispositions from caches (no I/O) before the walk, so
        // `disposition_for(&self)` doesn't conflict with `&mut self.id_to_dev`
        // inside the loop. Uses the same cached classification enumerate() built.
        let disp: Vec<PadDisposition> = {
            let pads: Vec<(Option<(u16, u16)>, String)> = self
                .gilrs
                .gamepads()
                .map(|(_, pad)| (pad.vendor_id().zip(pad.product_id()), pad.name().to_string()))
                .collect();
            let mut vp_idx: HashMap<(u16, u16), usize> = HashMap::new();
            pads.into_iter()
                .map(|(vp, name)| self.disposition_for(vp, &name, &mut vp_idx))
                .collect()
        };

        // Per-XInput-pad live gilrs state (dev_id, button mask, lx, ly), collected
        // during the walk and correlated to actual XInput slots afterward.
        let mut xi_corr: Vec<(String, u16, f32, f32)> = Vec::new();

        // Wrap the gamepads() walk in an explicit block so the profile
        // scope's RAII guard ends with the for loop — NOT with the function.
        // Bare profile_scope! at this depth would falsely include the
        // post-walk `out` return at function exit (the misleading-scope
        // trap we've hit twice now). Matching `}` is at the original
        // for-loop closing brace, with an extra `}` to close this wrapper.
        {
        puffin::profile_scope!("gilrs_gamepads_walk");
        for (i, (gilrs_id, pad)) in self.gilrs.gamepads().enumerate() {
            // Keep/virtual decided up front (path-based for PS, name/PID for our
            // HIDMaestro virtual) so IDs stay in sync with enumerate().
            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());
            let (dev, inst, is_virt) = match disp.get(i) {
                Some(PadDisposition::Keep { is_virt }) => {
                    gilrs_device_id(*is_virt, kind, &mut kind_seen, &mut virt_seen)
                }
                _ => continue, // Drop (ViGEm virtual beyond real count) or missing
            };
            self.id_to_dev.insert(usize::from(gilrs_id), dev.clone());

            // Record XInput slot for rumble routing: `inst` is the 0-based kind_seen
            // index = the XInput user index. Only REAL (physical) XInput pads register
            // a slot; our own HIDMaestro virtual is `is_virt`, so `!is_virt` excludes
            // it from the physical XInput read-back loopback (which would feed its own
            // state back into the OS).
            if kind == ControllerKind::XInput && !is_virt {
                self.xinput_idx.insert(dev.clone(), inst as u32);
            }

            for (axis, pin_id) in axis_map(kind) {
                let v = pad.axis_data(*axis).map_or(0.0, |d| d.value());
                out.push((dev.clone(), pin_id.to_string(), Signal::Float(v)));
            }

            // In BT mode gilrs maps Switch Pro buttons by WGI label order (Nintendo's A/B/X/Y
            // labels → gilrs South/East/West/North), reversing physical positions. Use a
            // corrective map when the pad name doesn't say "Pro Controller" (USB mode).
            let is_switch_bt = kind == ControllerKind::SwitchPro
                && !pad.name().to_ascii_lowercase().contains("pro controller");
            let btn_map: &[(Button, &str)] = if is_switch_bt {
                BUTTON_MAP_SWITCH_BT
            } else {
                button_map(kind)
            };
            for (button, pin_id) in btn_map.iter() {
                let pressed = pad.button_data(*button).map_or(false, |d| d.is_pressed());
                out.push((dev.clone(), pin_id.to_string(), Signal::Bool(pressed)));
            }

            // Universal DPad discrete outputs: combine axis_data (HAT/USB path) and
            // button_data (WGI/BT path).  Since axis_dpad_to_button is no longer applied,
            // button_data only reflects native button events (WGI flags etc.) while
            // axis_data reflects HAT-switch events.  OR-combining both handles all paths:
            //   • USB HAT (DS4, Switch Pro USB): axis_data non-zero, button_data zero.
            //   • BT WGI (Switch Pro BT): axis_data zero, button_data non-zero.
            //   • XInput / WGI gamepad: button_data set by WGI DPad flags.
            {
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                let du = dy >  0.5 || pad.button_data(Button::DPadUp).map_or(false,    |d| d.is_pressed());
                let dd = dy < -0.5 || pad.button_data(Button::DPadDown).map_or(false,  |d| d.is_pressed());
                let dr = dx >  0.5 || pad.button_data(Button::DPadRight).map_or(false, |d| d.is_pressed());
                let dl = dx < -0.5 || pad.button_data(Button::DPadLeft).map_or(false,  |d| d.is_pressed());
                out.push((dev.clone(), "dpad_up".into(),    Signal::Bool(du)));
                out.push((dev.clone(), "dpad_down".into(),  Signal::Bool(dd)));
                out.push((dev.clone(), "dpad_right".into(), Signal::Bool(dr)));
                out.push((dev.clone(), "dpad_left".into(),  Signal::Bool(dl)));

                // Reconstruct dpad Vec2 and axis pins from button states when axis_data is zero
                // (BT WGI path has no HAT axis events).  Diagonal magnitude is normalised.
                if dx == 0.0 && dy == 0.0 && (du || dd || dr || dl) {
                    let bx = if dr { 1.0f32 } else if dl { -1.0 } else { 0.0 };
                    let by = if du { 1.0f32 } else if dd { -1.0 } else { 0.0 };
                    let (nx, ny) = if bx != 0.0 && by != 0.0 {
                        (bx * std::f32::consts::FRAC_1_SQRT_2, by * std::f32::consts::FRAC_1_SQRT_2)
                    } else { (bx, by) };
                    out.push((dev.clone(), "dpad_x".into(), Signal::Float(nx)));
                    out.push((dev.clone(), "dpad_y".into(), Signal::Float(ny)));
                    out.push((dev.clone(), "dpad".into(), Signal::Vec2(glam::Vec2::new(nx, ny))));
                }
            }

            let lx = axis_val(&pad, Axis::LeftStickX);
            let ly = axis_val(&pad, Axis::LeftStickY);
            out.push((dev.clone(), "left_stick".into(), Signal::Vec2(Vec2::new(lx, ly))));

            let rx = axis_val(&pad, Axis::RightStickX);
            let ry = axis_val(&pad, Axis::RightStickY);
            out.push((dev.clone(), "right_stick".into(), Signal::Vec2(Vec2::new(rx, ry))));

            if matches!(kind, ControllerKind::XInput | ControllerKind::DualShock4 | ControllerKind::DualSense | ControllerKind::Generic) {
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(dx, dy))));
            } else if kind == ControllerKind::SwitchPro {
                // DPad comes as Axis::DPadX/Y (HAT switch) — already emitted by axis_map;
                // just build the bundled Vec2 from the same values.
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(dx, dy))));
            }

            // Analog triggers: ButtonData::value() works for both XInput and native HID.
            // Switch Pro ZL/ZR are digital-only and handled by button_map already.
            if !matches!(kind, ControllerKind::SwitchPro) {
                let lt = pad.button_data(Button::LeftTrigger2).map_or(0.0, |d| d.value());
                let rt = pad.button_data(Button::RightTrigger2).map_or(0.0, |d| d.value());
                let (l_pin, r_pin) = ("left_trigger", "right_trigger");
                out.push((dev.clone(), l_pin.into(), Signal::Float(lt)));
                out.push((dev.clone(), r_pin.into(), Signal::Float(rt)));
            }

            // Gyro via raw HID for DS4 / DualSense. Only REAL pads carry a HID
            // handle worth reading; our own virtual (is_virt) is excluded from the
            // gyro index space entirely so a real pad's idx can never land on the
            // virtual's hidapi entry. The gilrs/WGI walk order and hidapi
            // device_list order diverge once a same-VID/PID virtual is present (the
            // ROOT-enumerated virtual jumps position on a physical reconnect), so a
            // shared positional index crossed them — the DualSense override below
            // then overwrote a real pad's live sticks/buttons with the virtual's
            // neutral report (the post-reconnect freeze bug). Counting + reading
            // reals only keeps both index spaces stable. See gyro::open_device
            // (real-only list) and lookup_phys (real-only output index).
            if !is_virt {
            if let Some((vid, pid)) = pad.vendor_id().zip(pad.product_id()) {
                let vp = (vid, pid);
                let idx = *gyro_idx.entry(vp).or_insert(0);
                gyro_idx.insert(vp, idx + 1);

                if let Some(g) = self.gyro.read(vid, pid, idx) {
                    // Attribute HID IMU/state reports to the per-device polling
                    // rate. gilrs's event stream only fires on axis/button state
                    // changes, so a still gyro-only device would otherwise read 0 Hz.
                    let hid_n = self.gyro.take_event_count(vid, pid, idx);
                    if hid_n > 0 {
                        *self.event_counts.entry(usize::from(gilrs_id)).or_insert(0) += hid_n;
                    }
                    out.push((dev.clone(), "gyro_x".into(),  Signal::Float(g.gyro_x)));
                    out.push((dev.clone(), "gyro_y".into(),  Signal::Float(g.gyro_y)));
                    out.push((dev.clone(), "gyro_z".into(),  Signal::Float(g.gyro_z)));
                    out.push((dev.clone(), "accel_x".into(), Signal::Float(g.accel_x)));
                    out.push((dev.clone(), "accel_y".into(), Signal::Float(g.accel_y)));
                    out.push((dev.clone(), "accel_z".into(), Signal::Float(g.accel_z)));
                    if g.has_touchpad {
                        out.push((dev.clone(), "touch1_x".into(),      Signal::Float(g.touch1.x)));
                        out.push((dev.clone(), "touch1_y".into(),      Signal::Float(g.touch1.y)));
                        out.push((dev.clone(), "touch1_active".into(), Signal::Bool(g.touch1.active)));
                        out.push((dev.clone(), "touch2_x".into(),      Signal::Float(g.touch2.x)));
                        out.push((dev.clone(), "touch2_y".into(),      Signal::Float(g.touch2.y)));
                        out.push((dev.clone(), "touch2_active".into(), Signal::Bool(g.touch2.active)));
                        // gilrs's Windows backend doesn't fire Button::C for the touchpad
                        // click on DS4/DualSense, and DualSense's mute button isn't mapped
                        // at all — so we override with the values parsed from the raw HID
                        // report. Pushed last so the per-frame HashMap lookup wins.
                        out.push((dev.clone(), "btn_touchpad".into(),  Signal::Bool(g.touchpad_click)));
                    }
                    if matches!(kind, ControllerKind::DualSense) {
                        out.push((dev.clone(), "btn_mute".into(), Signal::Bool(g.mic_button)));
                    }
                    // Physical battery charge (DS4/DualSense), surfaced on the
                    // `battery` source pin so FlexInput's representation can show
                    // the real pad's level. Virtual pads always report 100% (the
                    // encode side), so this is informational and never loops into
                    // a virtual report.
                    if let Some(bat) = g.battery {
                        out.push((dev.clone(), "battery".into(), Signal::Float(bat)));
                    }
                    // DualSense: override all axes and buttons with raw HID values.
                    // gilrs on Windows HID maps the 6 axes by USB Usage ID order rather than
                    // logical gamepad position, placing L2/R2 where RightStickX/Y should be
                    // and RX/RY under LeftZ/RightZ. Parsing the report directly avoids this.
                    if let Some(ds) = g.dualsense {
                        out.push((dev.clone(), "left_stick_x".into(),  Signal::Float(ds.lx)));
                        out.push((dev.clone(), "left_stick_y".into(),  Signal::Float(ds.ly)));
                        out.push((dev.clone(), "right_stick_x".into(), Signal::Float(ds.rx)));
                        out.push((dev.clone(), "right_stick_y".into(), Signal::Float(ds.ry)));
                        out.push((dev.clone(), "left_stick".into(),    Signal::Vec2(Vec2::new(ds.lx, ds.ly))));
                        out.push((dev.clone(), "right_stick".into(),   Signal::Vec2(Vec2::new(ds.rx, ds.ry))));
                        out.push((dev.clone(), "left_trigger".into(),  Signal::Float(ds.l2)));
                        out.push((dev.clone(), "right_trigger".into(), Signal::Float(ds.r2)));
                        out.push((dev.clone(), "btn_south".into(),   Signal::Bool(ds.btn_south)));
                        out.push((dev.clone(), "btn_east".into(),    Signal::Bool(ds.btn_east)));
                        out.push((dev.clone(), "btn_west".into(),    Signal::Bool(ds.btn_west)));
                        out.push((dev.clone(), "btn_north".into(),   Signal::Bool(ds.btn_north)));
                        out.push((dev.clone(), "btn_lb".into(),      Signal::Bool(ds.btn_l1)));
                        out.push((dev.clone(), "btn_rb".into(),      Signal::Bool(ds.btn_r1)));
                        out.push((dev.clone(), "btn_lt_dig".into(),  Signal::Bool(ds.btn_l2)));
                        out.push((dev.clone(), "btn_rt_dig".into(),  Signal::Bool(ds.btn_r2)));
                        out.push((dev.clone(), "btn_ls".into(),      Signal::Bool(ds.btn_ls)));
                        out.push((dev.clone(), "btn_rs".into(),      Signal::Bool(ds.btn_rs)));
                        out.push((dev.clone(), "btn_start".into(),   Signal::Bool(ds.btn_options)));
                        out.push((dev.clone(), "btn_back".into(),    Signal::Bool(ds.btn_create)));
                        out.push((dev.clone(), "btn_guide".into(),   Signal::Bool(ds.btn_ps)));
                        out.push((dev.clone(), "dpad_up".into(),     Signal::Bool(ds.dpad_up)));
                        out.push((dev.clone(), "dpad_down".into(),   Signal::Bool(ds.dpad_down)));
                        out.push((dev.clone(), "dpad_left".into(),   Signal::Bool(ds.dpad_left)));
                        out.push((dev.clone(), "dpad_right".into(),  Signal::Bool(ds.dpad_right)));
                        let dx = if ds.dpad_right { 1.0f32 } else if ds.dpad_left { -1.0 } else { 0.0 };
                        let dy = if ds.dpad_up    { 1.0f32 } else if ds.dpad_down { -1.0 } else { 0.0 };
                        let (ndx, ndy) = if dx != 0.0 && dy != 0.0 {
                            (dx * std::f32::consts::FRAC_1_SQRT_2, dy * std::f32::consts::FRAC_1_SQRT_2)
                        } else { (dx, dy) };
                        out.push((dev.clone(), "dpad_x".into(),  Signal::Float(ndx)));
                        out.push((dev.clone(), "dpad_y".into(),  Signal::Float(ndy)));
                        out.push((dev.clone(), "dpad".into(),    Signal::Vec2(Vec2::new(ndx, ndy))));
                    }
                    // Switch Pro: override gilrs button output with raw-HID button data.
                    // This bypasses gilrs's WGI backend which loses D-Pad diagonals (8-position
                    // switch only emits cardinal events), garbles A/B/X/Y vs South/East/West/North
                    // by Nintendo label vs physical position, and mis-routes Plus/Minus/Home/Capture.
                    // Pushing last makes these the authoritative values in the IO-thread HashMap.
                    if let Some(sb) = g.switch_buttons {
                        // Sticks: raw-HID values calibrated from SPI flash are authoritative.
                        // gilrs's WGI stick mapping can land on the wrong axes when the HID
                        // device tree shifts (e.g. another driver install changes enumeration),
                        // so we override unconditionally for Switch Pro.
                        out.push((dev.clone(), "left_stick_x".into(),  Signal::Float(sb.lstick_x)));
                        out.push((dev.clone(), "left_stick_y".into(),  Signal::Float(sb.lstick_y)));
                        out.push((dev.clone(), "right_stick_x".into(), Signal::Float(sb.rstick_x)));
                        out.push((dev.clone(), "right_stick_y".into(), Signal::Float(sb.rstick_y)));
                        out.push((dev.clone(), "left_stick".into(),    Signal::Vec2(Vec2::new(sb.lstick_x, sb.lstick_y))));
                        out.push((dev.clone(), "right_stick".into(),   Signal::Vec2(Vec2::new(sb.rstick_x, sb.rstick_y))));
                        // Face buttons by physical position (Nintendo's labels are weird):
                        out.push((dev.clone(), "btn_south".into(), Signal::Bool(sb.btn_b))); // B = south
                        out.push((dev.clone(), "btn_east".into(),  Signal::Bool(sb.btn_a))); // A = east
                        out.push((dev.clone(), "btn_west".into(),  Signal::Bool(sb.btn_y))); // Y = west
                        out.push((dev.clone(), "btn_north".into(), Signal::Bool(sb.btn_x))); // X = north
                        out.push((dev.clone(), "btn_lb".into(),    Signal::Bool(sb.btn_l)));
                        out.push((dev.clone(), "btn_rb".into(),    Signal::Bool(sb.btn_r)));
                        out.push((dev.clone(), "btn_lt_dig".into(),Signal::Bool(sb.btn_zl)));
                        out.push((dev.clone(), "btn_rt_dig".into(),Signal::Bool(sb.btn_zr)));
                        out.push((dev.clone(), "btn_ls".into(),    Signal::Bool(sb.btn_lstick)));
                        out.push((dev.clone(), "btn_rs".into(),    Signal::Bool(sb.btn_rstick)));
                        out.push((dev.clone(), "btn_start".into(), Signal::Bool(sb.btn_plus)));
                        out.push((dev.clone(), "btn_back".into(),  Signal::Bool(sb.btn_minus)));
                        out.push((dev.clone(), "btn_guide".into(), Signal::Bool(sb.btn_home)));
                        out.push((dev.clone(), "btn_capture".into(),Signal::Bool(sb.btn_capture)));
                        out.push((dev.clone(), "dpad_up".into(),   Signal::Bool(sb.dpad_up)));
                        out.push((dev.clone(), "dpad_down".into(), Signal::Bool(sb.dpad_down)));
                        out.push((dev.clone(), "dpad_left".into(), Signal::Bool(sb.dpad_left)));
                        out.push((dev.clone(), "dpad_right".into(),Signal::Bool(sb.dpad_right)));
                        // Reconstruct DPad axis/Vec2 from authoritative button bits (BT path
                        // sends no axis events at all; diagonals get √2/2 magnitude).
                        let bx = if sb.dpad_right { 1.0f32 } else if sb.dpad_left { -1.0 } else { 0.0 };
                        let by = if sb.dpad_up    { 1.0f32 } else if sb.dpad_down { -1.0 } else { 0.0 };
                        let (nx, ny) = if bx != 0.0 && by != 0.0 {
                            (bx * std::f32::consts::FRAC_1_SQRT_2, by * std::f32::consts::FRAC_1_SQRT_2)
                        } else { (bx, by) };
                        out.push((dev.clone(), "dpad_x".into(), Signal::Float(nx)));
                        out.push((dev.clone(), "dpad_y".into(), Signal::Float(ny)));
                        out.push((dev.clone(), "dpad".into(),   Signal::Vec2(Vec2::new(nx, ny))));
                    }
                }
            }
            } // close `if !is_virt` gyro guard

            // ── Physical XInput: authoritative read via XInputGetState ──────────
            // gilrs reads Xbox pads through its WGI backend, which only delivers
            // input while FlexInput holds window focus — so the moment the user
            // tabs to their game, the gilrs values freeze and the mapped virtual
            // pad stops moving. XInputGetState is NOT focus-gated (it polls device
            // state directly), so we re-read the pad here and push the values LAST,
            // overriding the (possibly stale) gilrs pushes in the IO-thread HashMap.
            // This mirrors the DualSense/Switch raw-HID override above — same
            // "bypass gilrs's WGI limitations by reading the device directly" idea.
            // `inst` is this physical pad's XInput user index (the same value used
            // for rumble routing); for the common single-physical layout it is the
            // occupied slot. (After a manual slot reorder the index can diverge —
            // tracked as a follow-up.)
            // Correlation sample for EVERY XInput pad (physical and our own
            // virtual): its live gilrs state, matched to an actual XInput slot in
            // the post-walk pass below. Virtual pads are sampled only so their slot
            // gets CLAIMED and excluded from physical assignment — we never read a
            // virtual slot as an input source (that would loop our output back in).
            if kind == ControllerKind::XInput {
                xi_corr.push((
                    dev.clone(),
                    gilrs_xinput_button_mask(&pad),
                    axis_val(&pad, Axis::LeftStickX),
                    axis_val(&pad, Axis::LeftStickY),
                ));
            }

            #[cfg(windows)]
            if kind == ControllerKind::XInput && !is_virt {
                use xinput_ffi::*;
                // Land on the pad's correlated slot (falls back to the arrival index
                // until live state resolves it — see `xinput_slot_for`).
                let slot = self.xinput_slot_for.get(&dev).copied().unwrap_or(inst as u32);
                let mut st: XINPUT_STATE = Default::default();
                if unsafe { XInputGetState(slot, &mut st) } == 0 {
                    let gp = st.gamepad;
                    let b = gp.w_buttons;
                    let has = |mask: u16| (b & mask) != 0;
                    // XInput button bitmasks.
                    const DPAD_UP: u16 = 0x0001; const DPAD_DOWN: u16 = 0x0002;
                    const DPAD_LEFT: u16 = 0x0004; const DPAD_RIGHT: u16 = 0x0008;
                    const START: u16 = 0x0010; const BACK: u16 = 0x0020;
                    const LTHUMB: u16 = 0x0040; const RTHUMB: u16 = 0x0080;
                    const LSHOULDER: u16 = 0x0100; const RSHOULDER: u16 = 0x0200;
                    const GUIDE: u16 = 0x0400;
                    const A: u16 = 0x1000; const BTN_B: u16 = 0x2000;
                    const X: u16 = 0x4000; const Y: u16 = 0x8000;
                    // Sticks: i16 → -1..1 (XInput Y is up-positive, matching gilrs).
                    let norm = |v: i16| (v as f32 / 32767.0).clamp(-1.0, 1.0);
                    let lx = norm(gp.s_thumb_lx); let ly = norm(gp.s_thumb_ly);
                    let rx = norm(gp.s_thumb_rx); let ry = norm(gp.s_thumb_ry);
                    let lt = gp.b_left_trigger as f32 / 255.0;
                    let rt = gp.b_right_trigger as f32 / 255.0;
                    // XINPUT_GAMEPAD_TRIGGER_THRESHOLD = 30.
                    let lt_dig = gp.b_left_trigger > 30;
                    let rt_dig = gp.b_right_trigger > 30;
                    let du = has(DPAD_UP); let dd = has(DPAD_DOWN);
                    let dl = has(DPAD_LEFT); let dr = has(DPAD_RIGHT);
                    let dx = if dr { 1.0f32 } else if dl { -1.0 } else { 0.0 };
                    let dy = if du { 1.0f32 } else if dd { -1.0 } else { 0.0 };
                    let (ndx, ndy) = if dx != 0.0 && dy != 0.0 {
                        (dx * std::f32::consts::FRAC_1_SQRT_2, dy * std::f32::consts::FRAC_1_SQRT_2)
                    } else { (dx, dy) };
                    let push_f = |out: &mut Vec<(String, String, Signal)>, p: &str, v: f32|
                        out.push((dev.clone(), p.into(), Signal::Float(v)));
                    let push_b = |out: &mut Vec<(String, String, Signal)>, p: &str, v: bool|
                        out.push((dev.clone(), p.into(), Signal::Bool(v)));
                    push_f(&mut out, "left_stick_x", lx);
                    push_f(&mut out, "left_stick_y", ly);
                    push_f(&mut out, "right_stick_x", rx);
                    push_f(&mut out, "right_stick_y", ry);
                    out.push((dev.clone(), "left_stick".into(),  Signal::Vec2(Vec2::new(lx, ly))));
                    out.push((dev.clone(), "right_stick".into(), Signal::Vec2(Vec2::new(rx, ry))));
                    push_f(&mut out, "left_trigger", lt);
                    push_f(&mut out, "right_trigger", rt);
                    push_b(&mut out, "btn_south", has(A));
                    push_b(&mut out, "btn_east",  has(BTN_B));
                    push_b(&mut out, "btn_west",  has(X));
                    push_b(&mut out, "btn_north", has(Y));
                    push_b(&mut out, "btn_lb", has(LSHOULDER));
                    push_b(&mut out, "btn_rb", has(RSHOULDER));
                    push_b(&mut out, "btn_lt_dig", lt_dig);
                    push_b(&mut out, "btn_rt_dig", rt_dig);
                    push_b(&mut out, "btn_ls", has(LTHUMB));
                    push_b(&mut out, "btn_rs", has(RTHUMB));
                    push_b(&mut out, "btn_start", has(START));
                    push_b(&mut out, "btn_back",  has(BACK));
                    push_b(&mut out, "btn_guide", has(GUIDE));
                    push_b(&mut out, "dpad_up", du);
                    push_b(&mut out, "dpad_down", dd);
                    push_b(&mut out, "dpad_left", dl);
                    push_b(&mut out, "dpad_right", dr);
                    push_f(&mut out, "dpad_x", ndx);
                    push_f(&mut out, "dpad_y", ndy);
                    out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(ndx, ndy))));
                }
            }
        }
        } // end gilrs_gamepads_walk profile_scope block

        // ── Correlate XInput pads to their actual slots (focus-tolerant) ────────
        // XInput has no device-identity API, so we match each gilrs XInput pad to
        // the connected slot whose live state (buttons + stick signs) agrees, then
        // cache it. We only (re)assign a pad while it shows activity (a neutral pad
        // is ambiguous), so the mapping is learned the first time the user touches
        // the pad WHILE FOCUSED and then persists through focus loss. Virtual pads
        // are matched too — solely to claim their slot so a physical never resolves
        // onto our own virtual (which would feed our output back as input).
        #[cfg(windows)]
        if !xi_corr.is_empty() {
            use xinput_ffi::*;
            // Snapshot all four slots once: (connected, buttons, lx, ly).
            let mut slots = [(false, 0u16, 0i16, 0i16); 4];
            for i in 0..4u32 {
                let mut st: XINPUT_STATE = Default::default();
                if unsafe { XInputGetState(i, &mut st) } == 0 {
                    slots[i as usize] =
                        (true, st.gamepad.w_buttons, st.gamepad.s_thumb_lx, st.gamepad.s_thumb_ly);
                }
            }
            let mut claimed = [false; 4];
            // Re-claim slots already cached + still consistent so an active pad
            // can't steal a slot another pad is correctly resolved to.
            for (dev, xmask, _, _) in &xi_corr {
                if let Some(&s) = self.xinput_slot_for.get(dev) {
                    let s = s as usize;
                    if s < 4 && slots[s].0 && slots[s].1 == *xmask {
                        claimed[s] = true;
                    }
                }
            }
            // Coarse stick-sign agreement (a slot in deadzone matches a centered pad).
            let sign_ok = |a: f32, b: i16| {
                (a > 0.3 && b > 8000) || (a < -0.3 && b < -8000) || (a.abs() <= 0.3 && b.abs() <= 12000)
            };
            for (dev, xmask, lx, ly) in &xi_corr {
                let active = *xmask != 0 || lx.abs() > 0.3 || ly.abs() > 0.3;
                if !active { continue; }
                // Keep an already-consistent cached slot.
                if let Some(&s) = self.xinput_slot_for.get(dev) {
                    let s = s as usize;
                    if s < 4 && slots[s].0 && slots[s].1 == *xmask { continue; }
                }
                let candidates: Vec<usize> = (0..4)
                    .filter(|&s| {
                        !claimed[s]
                            && slots[s].0
                            && slots[s].1 == *xmask
                            && sign_ok(*lx, slots[s].2)
                            && sign_ok(*ly, slots[s].3)
                    })
                    .collect();
                if candidates.len() == 1 {
                    let s = candidates[0];
                    claimed[s] = true;
                    if self.xinput_slot_for.get(dev).copied() != Some(s as u32) {
                        self.xinput_slot_for.insert(dev.clone(), s as u32);
                    }
                }
            }
        }

        out
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        // ── XInput path: dispatch via XInputSetState ──────────────────────────
        if let Some(&xinput_slot) = self.xinput_idx.get(device_id) {
            let byte = match signal {
                Signal::Float(f) => (f.clamp(0.0, 1.0) * 255.0) as u8,
                Signal::Bool(b)  => if b { 255 } else { 0 },
                _ => return,
            };
            let entry = self.xinput_rumble.entry(xinput_slot).or_insert((0, 0));
            match pin_id {
                "rumble_strong" => entry.0 = byte,
                "rumble_weak"   => entry.1 = byte,
                _ => return, // other pin names are not XInput rumble
            }
            #[cfg(windows)]
            unsafe {
                use xinput_ffi::*;
                let vib = XINPUT_VIBRATION {
                    // 0-255 byte mapped to 0-65535 motor speed: multiply by 257 (= 0xFFFF / 0xFF).
                    w_left_motor_speed:  (entry.0 as u16).saturating_mul(257),
                    w_right_motor_speed: (entry.1 as u16).saturating_mul(257),
                };
                XInputSetState(xinput_slot, &vib);
            }
            return;
        }

        // ── PS/Switch HID path via GyroManager ───────────────────────────────
        let (vid, pid, idx) = match self.lookup_phys(device_id) {
            Some(t) => t,
            None => return,
        };
        // Scale Float 0–1 to the semantic range for each pin.
        // Pins with custom ranges are listed explicitly; everything else uses 0–255.
        let f = match signal {
            Signal::Float(f) => f.clamp(0.0, 1.0),
            Signal::Bool(b)  => if b { 1.0 } else { 0.0 },
            _ => return,
        };
        let byte = match pin_id {
            // Trigger mode: 0=Off, 1=Feedback, 2=Weapon, 3=Vibration → scale to 0–3
            "trigger_r_mode" | "trigger_l_mode" => (f * 3.0).round() as u8,
            // Trigger zones: 0–9 along trigger travel → scale to 0–9
            "trigger_r_start" | "trigger_r_end" |
            "trigger_l_start" | "trigger_l_end" => (f * 9.0).round() as u8,
            // Trigger force: 0–7 → scale to 0–7
            "trigger_r_strength" | "trigger_l_strength" => (f * 7.0).round() as u8,
            // Player LED: 0=off, 1=P1, 2=P2, 3=P3, 4=P4 → scale to 0–4
            "player_led" => (f * 4.0).round() as u8,
            // Mic LED: 0=off, 1=on, 2=pulsing → scale to 0–2
            "mic_led" => (f * 2.0).round() as u8,
            // Everything else (rumble 0–255, lightbar 0–255, trigger freq 0–255): linear
            _ => (f * 255.0) as u8,
        };
        self.gyro.set_output_byte(vid, pid, idx, pin_id, byte);
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        // Map gilrs-id counts to device-id, summing in case multiple gilrs ids
        // somehow resolved to the same device id (shouldn't happen, but safe).
        let mut acc: HashMap<String, u32> = HashMap::new();
        for (gilrs_id, count) in self.event_counts.drain() {
            if let Some(dev) = self.id_to_dev.get(&gilrs_id) {
                *acc.entry(dev.clone()).or_insert(0) += count;
            }
        }
        for (dev, count) in acc {
            out.push((dev, count));
        }
        out
    }

    fn set_spike_filter(&mut self, device_id: &str, enabled: bool, sensitivity_pct: f32) {
        if let Some((vid, pid, idx)) = self.lookup_phys(device_id) {
            self.gyro.set_spike_filter(vid, pid, idx, enabled, sensitivity_pct);
        }
    }
}

impl GilrsBackend {
    /// Resolve a `gilrs:<kind>:<inst>` device id to the (vid, pid, instance index)
    /// tuple that GyroManager keys its open HID handles by. Uses the same cached
    /// keep/own-virtual disposition and per-VID/PID indexing as `enumerate()` /
    /// `poll()`, so the gyro idx returned here matches the one those use. Read-
    /// only (no I/O); relies on the cache `enumerate()` last refreshed.
    fn lookup_phys(&self, device_id: &str) -> Option<(u16, u16, usize)> {
        let mut kind_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut virt_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut vp_idx: HashMap<(u16, u16), usize> = HashMap::new();
        // REAL-only gyro idx (matches poll's `gyro_idx`, which now skips is_virt):
        // our own virtual is excluded from the HID handle index space (see
        // gyro::open_device), so count and match real pads only. `disposition_for`
        // is still called for every pad so its classification index (`vp_idx`)
        // advances over the full list; a virtual `device_id` has no gyro handle and
        // falls through to None.
        let mut vp_seen: HashMap<(u16, u16), usize> = HashMap::new();
        for (_id, pad) in self.gilrs.gamepads() {
            let vp = pad.vendor_id().zip(pad.product_id());
            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());
            let is_virt = match self.disposition_for(vp, pad.name(), &mut vp_idx) {
                PadDisposition::Keep { is_virt } => is_virt,
                // Dropped face: not in the device-id space and never a gyro
                // target; skip before vp_seen so real-pad indices stay aligned
                // with poll()'s walk.
                PadDisposition::Drop => continue,
            };
            if is_virt { continue; } // own virtual: never a gyro/HID target
            let (dev, _inst, _is_virt) =
                gilrs_device_id(is_virt, kind, &mut kind_seen, &mut virt_seen);
            if dev == device_id {
                let (v, p) = vp?;
                let idx = *vp_seen.get(&(v, p)).unwrap_or(&0);
                return Some((v, p, idx));
            }
            if let Some((v, p)) = vp {
                *vp_seen.entry((v, p)).or_insert(0) += 1;
            }
        }
        None
    }
}

fn axis_val(pad: &gilrs::Gamepad, axis: Axis) -> f32 {
    pad.axis_data(axis).map_or(0.0, |d| d.value())
}

/// True if this VID/PID is a PlayStation-family controller (DS4 / DualSense)
/// that the gyro/HID layer opens by path and can therefore classify as
/// virtual-vs-real directly. Switch Pro shares the same hidapi addressing but
/// has no virtual counterpart (we don't emulate it), so it's harmless either
/// way; including the PS PIDs is what matters for the own-virtual decision.
fn is_ps_family(vid: u16, pid: u16) -> bool {
    vid == 0x054C
        && matches!(pid, 0x05C4 | 0x09CC | 0x0BA0 | 0x0CE6 | 0x0DF2)
}

/// True for OUR HIDMaestro virtual Xbox 360 as gilrs's WGI backend reports it:
/// FriendlyName "HIDMaestro XInput Companion" with the profile USB PID **0x02FF**
/// (Xbox One wired family, see profiles/xbox360.json). Groundtruthed live: deploying
/// the virtual with no physical pad yields exactly this one pad and it drives Steam —
/// so this IS the working virtual, not a dead sibling. A PHYSICAL Xbox is 045E:028E
/// "Xbox 360 Controller[ for Windows]" and never matches, so this cleanly separates
/// ours from physical with no slot/correlation guessing. The name check is the primary
/// signal; the 0x02FF PID is belt-and-suspenders (and the reason the profile keeps it).
fn is_hidmaestro_virtual_xinput(vid: u16, pid: u16, name: &str) -> bool {
    (vid == 0x045E && pid == 0x02FF) || name.to_ascii_lowercase().contains("hidmaestro")
}

/// Compute the stable `gilrs:` device id for a pad given its pre-computed
/// own-virtual flag (from [`GilrsBackend::disposition_for`]). Own emulated devices
/// get a `v`-prefixed instance (`gilrs:dualsense:v0`) counted independently of
/// real devices, so a real controller keeps a contiguous `gilrs:dualsense:0`
/// index regardless of plug order — fixing the collision where the dedup dropped
/// the real device when a virtual of the same model already existed. `kind_seen`
/// tracks real-device instances per kind; `virt_seen` tracks emulated ones.
fn gilrs_device_id(
    is_virt: bool,
    kind: ControllerKind,
    kind_seen: &mut std::collections::HashMap<ControllerKind, usize>,
    virt_seen: &mut std::collections::HashMap<ControllerKind, usize>,
) -> (String, usize, bool) {
    if is_virt {
        let inst = *virt_seen.get(&kind).unwrap_or(&0);
        virt_seen.insert(kind, inst + 1);
        (format!("gilrs:{}:v{}", kind.id_slug(), inst), inst, true)
    } else {
        let inst = *kind_seen.get(&kind).unwrap_or(&0);
        kind_seen.insert(kind, inst + 1);
        (format!("gilrs:{}:{}", kind.id_slug(), inst), inst, false)
    }
}

/// gilrs button state of `pad` packed into the XInput `wButtons` bit layout, so it
/// can be compared directly against an `XINPUT_GAMEPAD.w_buttons` for slot
/// correlation. Best-effort: D-Pad reads the discrete buttons (the WGI path FlexInput
/// uses for Xbox pads reports them as buttons).
fn gilrs_xinput_button_mask(pad: &gilrs::Gamepad) -> u16 {
    let p = |b: Button| pad.button_data(b).map_or(false, |d| d.is_pressed());
    let mut m = 0u16;
    if p(Button::DPadUp)       { m |= 0x0001; }
    if p(Button::DPadDown)     { m |= 0x0002; }
    if p(Button::DPadLeft)     { m |= 0x0004; }
    if p(Button::DPadRight)    { m |= 0x0008; }
    if p(Button::Start)        { m |= 0x0010; }
    if p(Button::Select)       { m |= 0x0020; }
    if p(Button::LeftThumb)    { m |= 0x0040; }
    if p(Button::RightThumb)   { m |= 0x0080; }
    if p(Button::LeftTrigger)  { m |= 0x0100; } // LB
    if p(Button::RightTrigger) { m |= 0x0200; } // RB
    if p(Button::Mode)         { m |= 0x0400; } // Guide
    if p(Button::South)        { m |= 0x1000; }
    if p(Button::East)         { m |= 0x2000; }
    if p(Button::West)         { m |= 0x4000; }
    if p(Button::North)        { m |= 0x8000; }
    m
}

fn axis_map(kind: ControllerKind) -> &'static [(Axis, &'static str)] {
    match kind {
        ControllerKind::DualShock4 | ControllerKind::DualSense => AXIS_MAP_DS4,
        ControllerKind::SwitchPro                              => AXIS_MAP_SWITCH,
        _                                                      => AXIS_MAP_STANDARD,
    }
}

const AXIS_MAP_STANDARD: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // Triggers emitted separately via ButtonData::value() — Axis::LeftZ/RightZ
    // is not reliably populated on all Windows gilrs backends.
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

const AXIS_MAP_DS4: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // Triggers emitted separately via ButtonData::value().
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

const AXIS_MAP_SWITCH: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // DPad is a HAT switch reported as axes on Windows; read directly.
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

fn button_map(kind: ControllerKind) -> &'static [(Button, &'static str)] {
    match kind {
        ControllerKind::DualShock4 | ControllerKind::DualSense => BUTTON_MAP_PLAYSTATION,
        ControllerKind::SwitchPro  => BUTTON_MAP_SWITCH,
        _                          => BUTTON_MAP_XINPUT,
    }
}

// DPad discrete outputs are emitted via the unified axis+button path in poll(); omitted here.
const BUTTON_MAP_XINPUT: &[(Button, &str)] = &[
    (Button::South,         "btn_south"),
    (Button::East,          "btn_east"),
    (Button::West,          "btn_west"),
    (Button::North,         "btn_north"),
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_start"),
    (Button::Select,        "btn_back"),
    (Button::Mode,          "btn_guide"),
];

const BUTTON_MAP_PLAYSTATION: &[(Button, &str)] = &[
    (Button::South,         "btn_south"),
    (Button::East,          "btn_east"),
    (Button::West,          "btn_west"),
    (Button::North,         "btn_north"),
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_start"),
    (Button::Select,        "btn_back"),
    (Button::Mode,          "btn_guide"),
    (Button::C,             "btn_touchpad"),
];

// USB / wired Switch Pro: gilrs Button::South = physical south (B), East = A, West = Y, North = X.
const BUTTON_MAP_SWITCH: &[(Button, &str)] = &[
    (Button::South,         "btn_south"),
    (Button::East,          "btn_east"),
    (Button::West,          "btn_west"),
    (Button::North,         "btn_north"),
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_start"),   // Plus
    (Button::Select,        "btn_back"),    // Minus
    (Button::Mode,          "btn_guide"),   // Home
    (Button::C,             "btn_capture"), // Capture
    // DPad omitted: emitted separately via axis+button combo in poll() to avoid BT flicker.
];

// Bluetooth Switch Pro: gilrs is driven by Windows Gaming Input which maps Nintendo labels
// rather than physical positions (Nintendo's A label → WGI A → gilrs South).
// Additionally Home/Capture appear at gilrs Select/Start in BT mode.
const BUTTON_MAP_SWITCH_BT: &[(Button, &str)] = &[
    (Button::South, "btn_east"),    // WGI A = Nintendo A (east physical position)
    (Button::East,  "btn_south"),   // WGI B = Nintendo B (south physical position)
    (Button::West,  "btn_north"),   // WGI X = Nintendo X (north physical position)
    (Button::North, "btn_west"),    // WGI Y = Nintendo Y (west physical position)
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    // In BT mode: Start fires for Home, Select fires for Capture (opposite of USB).
    (Button::Start,  "btn_guide"),   // Home → gilrs Start in BT
    (Button::Select, "btn_capture"), // Capture → gilrs Select in BT
    (Button::Mode,   "btn_start"),   // Plus (if it fires Mode in BT)
    (Button::C,      "btn_back"),    // Minus (if it fires C in BT)
    // DPad omitted: emitted separately via axis+button combo in poll().
];
