//! SDL3-backed gamepad input for controllers FlexInput does not parse natively.
//!
//! `GilrsBackend` + the raw-HID path own the controllers FlexInput handles well
//! (Xbox/XInput, DualShock 4, DualSense, Switch Pro) — including their tuned
//! gyro/touchpad/HD-haptic overrides. This backend fills the gap: it enumerates
//! ONLY pads that [`ControllerKind::detect`] classifies as [`ControllerKind::Generic`]
//! (Steam Controller, 8BitDo, arcade sticks, third-party pads), so no physical
//! device is ever surfaced by both backends. For those generic pads it relays
//! everything SDL exposes — sticks, buttons, analog triggers, gyro/accel (SDL
//! sensor API, requires the `hidapi` feature), the touchpad, and the extra
//! rear-paddle / misc buttons — onto the same pin vocabulary the raw-HID path
//! uses, so downstream mappings behave identically regardless of the source.
//!
//! ## Threading
//! SDL's event pump and gamepad state must be driven from the thread that
//! initialized SDL. The whole backend lives on the single `device-io` thread
//! (see `spawn_io_thread`), and SDL is initialized lazily on the first `poll()`
//! so init happens on that thread. The SDL handles are `!Send`/`!Sync` and are
//! never touched from anywhere else — `DeviceBackend`'s `&mut self` methods make
//! that a compile-time guarantee.

use std::collections::HashMap;

use glam::Vec2;

use flexinput_core::Signal;

use crate::{
    gyro::{ACCEL_REF_G, GYRO_REF_DPS},
    identification::ControllerKind,
    layouts, DeviceBackend, PhysicalDevice,
};

use sdl3::gamepad::{Axis, Button, Gamepad};
use sdl3::joystick::JoystickId;
use sdl3::sensor::SensorType;
use sdl3::{GamepadSubsystem, Sdl};

/// Device-id prefix for SDL pads. Kept distinct from gilrs's `gilrs:` prefix so
/// ids never collide and `send()` routing (which keys off the prefix) is
/// unambiguous. Full form: `sdl:generic:<inst>`.
const ID_PREFIX: &str = "sdl";

/// Thread-confinement wrapper for SDL's `!Send` handles.
///
/// SDL's `Sdl`/`GamepadSubsystem`/`Gamepad` hold raw pointers and are `!Send` on
/// purpose (the `subsystem!` macro even marks the gamepad subsystem `nosync`).
/// FlexInput's `DeviceBackend` must be `Send` because the whole `backends` Vec is
/// *moved onto* the dedicated `device-io` thread once at spawn — but after that
/// move every access goes through `&mut self` methods on that one thread. So the
/// SDL state is created on, lives on, and is used from exactly one thread; the
/// only `Send` requirement is the initial move.
///
/// This newtype carries that invariant: the inner value is only ever constructed
/// and dereferenced from the device-io thread (all call sites are `SdlBackend`'s
/// `&mut self` methods). We never hand out the inner value or a reference to it
/// across a thread boundary. Under that invariant the move-only `Send` is sound.
struct ThreadConfined<T>(T);
// SAFETY: see the doc comment above — the wrapped SDL handles are confined to the
// device-io thread; `Send` here only authorizes moving the not-yet-used state
// onto that thread at backend construction, never concurrent cross-thread use.
unsafe impl<T> Send for ThreadConfined<T> {}

impl<T> std::ops::Deref for ThreadConfined<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> std::ops::DerefMut for ThreadConfined<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// All the SDL handles, grouped so a single `ThreadConfined` covers them. Created
/// lazily on the first poll (on the device-io thread).
struct SdlState {
    /// The SDL context; kept alive for the process. Subsystems borrow from it,
    /// so it must outlive them — declared first, dropped last.
    _sdl: Sdl,
    gamepad_subsystem: GamepadSubsystem,
    /// Opened generic pads, keyed by SDL joystick instance id.
    pads: HashMap<JoystickId, OpenPad>,
}

/// One opened SDL gamepad plus the per-device state we carry across polls.
struct OpenPad {
    /// The live SDL handle. Held for the pad's lifetime — SDL zeroes the state
    /// of a closed gamepad, and rumble/sensor calls need it open. Dropping it
    /// calls `SDL_CloseGamepad`.
    gamepad: Gamepad,
    /// Stable device id string (`sdl:generic:<inst>`). Assigned once at open so
    /// it survives re-enumeration as long as the pad stays connected.
    dev_id: String,
    /// Whether this pad reports a gyroscope/accelerometer (checked once at open;
    /// sensors were enabled then too). Gates the per-poll sensor reads.
    has_gyro: bool,
    has_accel: bool,
    /// Number of touchpads SDL reports for this pad (0 for most generic pads).
    num_touchpads: u16,
    /// Last-sent rumble (strong, weak) as 0-255 bytes, to skip redundant
    /// `set_rumble` calls. SDL rumble is re-armed with a long duration each
    /// change and refreshed periodically so it doesn't auto-expire.
    last_rumble: (u8, u8),
}

pub struct SdlBackend {
    /// All SDL handles, confined to the device-io thread. `None` until the first
    /// `poll()`/`enumerate()` initializes SDL on that thread.
    state: Option<ThreadConfined<SdlState>>,
    /// Monotonic counter for the `:<inst>` suffix, so two generic pads get
    /// distinct ids (`sdl:generic:0`, `sdl:generic:1`) that don't reshuffle
    /// when one disconnects.
    next_inst: usize,
    /// Per-device raw event tally since the last `take_event_counts`, for the
    /// live per-device Hz display. SDL doesn't hand us an event count, so we
    /// approximate: one "event" per poll in which any input changed.
    event_counts: HashMap<String, u32>,
    /// Last full signal snapshot per device, used to detect change for the
    /// event-count approximation above.
    last_sig: HashMap<String, u64>,
    /// True once we've tried (and failed) to init SDL, so we don't spam init
    /// attempts every poll on a machine where SDL can't start.
    init_failed: bool,
    /// Joystick ids already probed and REJECTED (non-gamepad, or a kind gilrs
    /// owns). The probe requires `SDL_OpenGamepad`, which does device I/O —
    /// ~140 ms on a cold Bluetooth pad — so re-probing every 2 s enumerate
    /// was a periodic io-thread stall for as long as the open stayed slow.
    /// Ids are per-connection: a reconnect gets a fresh id and a fresh probe.
    /// Pruned alongside `pads` when a device disappears.
    rejected_ids: std::collections::HashSet<JoystickId>,
}

impl SdlBackend {
    pub fn new() -> Self {
        Self {
            state: None,
            next_inst: 0,
            event_counts: HashMap::new(),
            last_sig: HashMap::new(),
            init_failed: false,
            rejected_ids: std::collections::HashSet::new(),
        }
    }

    /// Lazily initialize SDL + the gamepad subsystem + event pump on the calling
    /// (device-io) thread. Returns false if SDL is unavailable — the backend
    /// then behaves as an empty source (no panics, gilrs still works).
    fn ensure_init(&mut self) -> bool {
        if self.state.is_some() {
            return true;
        }
        if self.init_failed {
            return false;
        }
        match Self::try_init() {
            Some(state) => {
                self.state = Some(ThreadConfined(state));
                eprintln!("[sdl] initialized gamepad subsystem");
                true
            }
            None => {
                eprintln!("[sdl] init failed — SDL gamepad source disabled");
                self.init_failed = true;
                false
            }
        }
    }

    fn try_init() -> Option<SdlState> {
        let sdl = sdl3::init().ok()?;
        let gamepad_subsystem = sdl.gamepad().ok()?;
        // Do NOT create an EventPump / pump the SDL event queue. This backend
        // lives on the real-time device-io loop (up to 4 kHz); pumping the whole
        // SDL event system there is expensive (it drives window messages + full
        // hotplug detection) and was throttling the loop to a crawl on some PCs.
        // Instead we disable gamepad event processing and call
        // `gamepad_subsystem.update()` per poll, which refreshes only gamepad
        // state — SDL explicitly supports this: "If gamepad events are disabled,
        // you must call SDL_UpdateGamepads() yourself."
        gamepad_subsystem.set_events_processing_state(false);
        Some(SdlState {
            _sdl: sdl,
            gamepad_subsystem,
            pads: HashMap::new(),
        })
    }

    /// Open every currently-connected generic gamepad we haven't opened yet, and
    /// drop handles for pads that have gone away. Returns nothing; mutates
    /// `self.pads`. Called from `poll()`/`enumerate()` so the open set tracks the
    /// live device set.
    fn sync_open_pads(&mut self) {
        let Some(state) = self.state.as_mut() else { return };
        // Split the borrow: `gamepad_subsystem` (read) vs `pads` (write) are
        // distinct fields, so take them separately to satisfy the borrow checker.
        let state = &mut **state;
        let gs = &state.gamepad_subsystem;
        let pads = &mut state.pads;
        let next_inst = &mut self.next_inst;
        let rejected = &mut self.rejected_ids;

        let ids = match gs.gamepads() {
            Ok(ids) => ids,
            Err(_) => return,
        };

        // Drop pads no longer present (disconnected), and forget rejection
        // verdicts for departed ids so the set can't grow unbounded.
        let present: std::collections::HashSet<JoystickId> = ids.iter().copied().collect();
        pads.retain(|id, _| present.contains(id));
        rejected.retain(|id| present.contains(id));

        for id in ids {
            if pads.contains_key(&id) || rejected.contains(&id) {
                continue;
            }
            // Only SDL-classified GAMEPADS (have a mapping); raw joysticks without
            // a gamepad mapping are skipped — FlexInput's pin model is gamepad-shaped.
            if !gs.is_gamepad(id) {
                rejected.insert(id);
                continue;
            }
            let gamepad = match gs.open(id) {
                Ok(g) => g,
                // Open failures are NOT remembered: they can be transient
                // (device busy mid-arrival) and a stuck-unopenable pad is rarer
                // than a reconnect; the id vanishes with the device anyway.
                Err(_) => continue,
            };
            // Dedup gate: if this pad is a kind gilrs already owns (Xbox / DS4 /
            // DualSense / Switch Pro), skip it so it isn't surfaced twice. Only
            // `Generic` pads belong to SDL. REMEMBER the verdict: the probe
            // above costs an SDL_OpenGamepad (device I/O, ~140 ms on a cold BT
            // pad), and re-probing the same id every 2 s enumerate was a
            // periodic io-thread stall.
            let vid = gamepad.vendor_id();
            let pid = gamepad.product_id();
            let name = gamepad.name().unwrap_or_default();
            let kind = ControllerKind::detect(&name, vid, pid);
            if kind != ControllerKind::Generic {
                // Close (drop) and leave it to gilrs.
                rejected.insert(id);
                continue;
            }

            let dev_id = format!("{ID_PREFIX}:generic:{}", *next_inst);
            *next_inst += 1;

            // Enable IMU sensors if present so per-poll reads return data. SDL's
            // sensor API is compiled in because we enable the `sdl3` crate's
            // `hidapi` feature (see crates/devices/Cargo.toml); a pad without a
            // sensor simply reports it disabled and we skip the per-poll read.
            let mut has_gyro = false;
            let mut has_accel = false;
            if gamepad.sensor_set_enabled(SensorType::Gyroscope, true).is_ok() {
                has_gyro = gamepad.sensor_enabled(SensorType::Gyroscope);
            }
            if gamepad.sensor_set_enabled(SensorType::Accelerometer, true).is_ok() {
                has_accel = gamepad.sensor_enabled(SensorType::Accelerometer);
            }
            let num_touchpads = gamepad.touchpads_count();

            eprintln!(
                "[sdl] opened generic pad {dev_id} name={name:?} vid={vid:04X?} pid={pid:04X?} \
                 gyro={has_gyro} accel={has_accel} touchpads={num_touchpads}"
            );

            pads.insert(
                id,
                OpenPad {
                    gamepad,
                    dev_id,
                    has_gyro,
                    has_accel,
                    num_touchpads,
                    last_rumble: (0, 0),
                },
            );
        }
    }
}

impl Default for SdlBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceBackend for SdlBackend {
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        puffin::profile_function!();
        if !self.ensure_init() {
            return Vec::new();
        }
        self.sync_open_pads();

        let mut result = Vec::new();
        let Some(state) = self.state.as_ref() else { return result };
        for pad in state.pads.values() {
            let name = pad.gamepad.name().unwrap_or_else(|| "SDL Gamepad".to_string());
            result.push(PhysicalDevice {
                id: pad.dev_id.clone(),
                display_name: name,
                kind: ControllerKind::Generic,
                outputs: layouts::outputs_for(ControllerKind::Generic),
                inputs: layouts::inputs_for(ControllerKind::Generic),
                instance_path: None,
                vid: pad.gamepad.vendor_id(),
                pid: pad.gamepad.product_id(),
            });
        }
        result
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        puffin::profile_function!();
        if !self.ensure_init() {
            return Vec::new();
        }
        // Refresh gamepad state only (NOT the full event pump). Cheap enough for
        // the real-time loop; see try_init for why we don't pump_events here.
        // Device open/close (sync_open_pads) is done in enumerate() on its slow
        // ~2 s cadence, NOT per poll — hotplug scanning is too heavy for the hot
        // path and was part of the loop-throttling regression.
        if let Some(state) = self.state.as_ref() {
            state.gamepad_subsystem.update();
        }

        let mut out = Vec::new();
        // Collect the change-detection hashes to update after the borrow of pads ends.
        let mut sig_updates: Vec<(String, u64)> = Vec::new();

        let Some(state) = self.state.as_ref() else { return out };
        for (id, pad) in &state.pads {
            let dev = &pad.dev_id;
            let g = &pad.gamepad;
            let start_len = out.len();

            // ── Sticks (i16 → -1..1). SDL Y is down-positive; negate so up = +1
            // to match the rest of FlexInput (gilrs/XInput up-positive). ────────
            let norm = |v: i16| (v as f32 / 32767.0).clamp(-1.0, 1.0);
            let lx = norm(g.axis(Axis::LeftX));
            let ly = -norm(g.axis(Axis::LeftY));
            let rx = norm(g.axis(Axis::RightX));
            let ry = -norm(g.axis(Axis::RightY));
            out.push((dev.clone(), "left_stick_x".into(), Signal::Float(lx)));
            out.push((dev.clone(), "left_stick_y".into(), Signal::Float(ly)));
            out.push((dev.clone(), "right_stick_x".into(), Signal::Float(rx)));
            out.push((dev.clone(), "right_stick_y".into(), Signal::Float(ry)));
            out.push((dev.clone(), "left_stick".into(), Signal::Vec2(Vec2::new(lx, ly))));
            out.push((dev.clone(), "right_stick".into(), Signal::Vec2(Vec2::new(rx, ry))));

            // ── Triggers: SDL trigger axes are 0..32767 → 0..1. ────────────────
            let lt = (g.axis(Axis::TriggerLeft) as f32 / 32767.0).clamp(0.0, 1.0);
            let rt = (g.axis(Axis::TriggerRight) as f32 / 32767.0).clamp(0.0, 1.0);
            out.push((dev.clone(), "left_trigger".into(), Signal::Float(lt)));
            out.push((dev.clone(), "right_trigger".into(), Signal::Float(rt)));
            // Digital trigger = past a small threshold (matches XInput's ~30/255).
            out.push((dev.clone(), "btn_lt_dig".into(), Signal::Bool(lt > 0.117)));
            out.push((dev.clone(), "btn_rt_dig".into(), Signal::Bool(rt > 0.117)));

            // ── Face / shoulder / stick-click / menu buttons ───────────────────
            // Pin names follow the GENERIC layout (gamepad::standard_outputs):
            // note btn_lstick/btn_rstick (not _ls/_rs) and btn_select/btn_mode
            // (not _back/_guide). Emitting the wrong names silently breaks routing.
            let b = |btn: Button| g.button(btn);
            out.push((dev.clone(), "btn_south".into(), Signal::Bool(b(Button::South))));
            out.push((dev.clone(), "btn_east".into(),  Signal::Bool(b(Button::East))));
            out.push((dev.clone(), "btn_west".into(),  Signal::Bool(b(Button::West))));
            out.push((dev.clone(), "btn_north".into(), Signal::Bool(b(Button::North))));
            out.push((dev.clone(), "btn_lb".into(), Signal::Bool(b(Button::LeftShoulder))));
            out.push((dev.clone(), "btn_rb".into(), Signal::Bool(b(Button::RightShoulder))));
            out.push((dev.clone(), "btn_lstick".into(), Signal::Bool(b(Button::LeftStick))));
            out.push((dev.clone(), "btn_rstick".into(), Signal::Bool(b(Button::RightStick))));
            out.push((dev.clone(), "btn_start".into(),  Signal::Bool(b(Button::Start))));
            out.push((dev.clone(), "btn_select".into(), Signal::Bool(b(Button::Back))));
            out.push((dev.clone(), "btn_mode".into(),   Signal::Bool(b(Button::Guide))));

            // ── D-Pad: SDL exposes it as discrete buttons. Emit discrete +
            // reconstruct the axis/Vec2 (√2/2 on diagonals) like the raw paths. ─
            let du = b(Button::DPadUp);
            let dd = b(Button::DPadDown);
            let dl = b(Button::DPadLeft);
            let dr = b(Button::DPadRight);
            out.push((dev.clone(), "dpad_up".into(),    Signal::Bool(du)));
            out.push((dev.clone(), "dpad_down".into(),  Signal::Bool(dd)));
            out.push((dev.clone(), "dpad_left".into(),  Signal::Bool(dl)));
            out.push((dev.clone(), "dpad_right".into(), Signal::Bool(dr)));
            let dx = if dr { 1.0f32 } else if dl { -1.0 } else { 0.0 };
            let dy = if du { 1.0f32 } else if dd { -1.0 } else { 0.0 };
            let (ndx, ndy) = if dx != 0.0 && dy != 0.0 {
                (dx * std::f32::consts::FRAC_1_SQRT_2, dy * std::f32::consts::FRAC_1_SQRT_2)
            } else {
                (dx, dy)
            };
            out.push((dev.clone(), "dpad_x".into(), Signal::Float(ndx)));
            out.push((dev.clone(), "dpad_y".into(), Signal::Float(ndy)));
            out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(ndx, ndy))));

            // ── Extra buttons (rear paddles / misc). Only meaningful on pads that
            // have them; SDL returns false for absent buttons, harmless to emit. ─
            out.push((dev.clone(), "btn_paddle_l1".into(), Signal::Bool(b(Button::LeftPaddle1))));
            out.push((dev.clone(), "btn_paddle_r1".into(), Signal::Bool(b(Button::RightPaddle1))));
            out.push((dev.clone(), "btn_paddle_l2".into(), Signal::Bool(b(Button::LeftPaddle2))));
            out.push((dev.clone(), "btn_paddle_r2".into(), Signal::Bool(b(Button::RightPaddle2))));
            out.push((dev.clone(), "btn_misc1".into(), Signal::Bool(b(Button::Misc1))));
            out.push((dev.clone(), "btn_misc2".into(), Signal::Bool(b(Button::Misc2))));
            out.push((dev.clone(), "btn_misc3".into(), Signal::Bool(b(Button::Misc3))));
            out.push((dev.clone(), "btn_misc4".into(), Signal::Bool(b(Button::Misc4))));
            out.push((dev.clone(), "btn_misc5".into(), Signal::Bool(b(Button::Misc5))));
            out.push((dev.clone(), "btn_misc6".into(), Signal::Bool(b(Button::Misc6))));
            // Touchpad click is a Button in SDL.
            out.push((dev.clone(), "btn_touchpad".into(), Signal::Bool(b(Button::Touchpad))));

            // ── Gyro / accel via SDL sensor API. Normalized to the shared
            // ±reference (GYRO_REF_DPS / ACCEL_REF_G) so SDL gyro drops straight
            // into gyro→aim mappings authored for DS4/DualSense/Switch. ─────────
            if pad.has_gyro {
                let mut d = [0.0f32; 3];
                if g.sensor_get_data(SensorType::Gyroscope, &mut d).is_ok() {
                    // SDL gyro is rad/s; convert to deg/s, then to the ±ref scale.
                    let to_norm = |rad_s: f32| (rad_s.to_degrees() / GYRO_REF_DPS).clamp(-1.0, 1.0);
                    out.push((dev.clone(), "gyro_x".into(), Signal::Float(to_norm(d[0]))));
                    out.push((dev.clone(), "gyro_y".into(), Signal::Float(to_norm(d[1]))));
                    out.push((dev.clone(), "gyro_z".into(), Signal::Float(to_norm(d[2]))));
                }
            }
            if pad.has_accel {
                let mut d = [0.0f32; 3];
                if g.sensor_get_data(SensorType::Accelerometer, &mut d).is_ok() {
                    // SDL accel is m/s²; convert to G, then to the ±ref scale.
                    const G: f32 = 9.806_65;
                    let to_norm = |ms2: f32| ((ms2 / G) / ACCEL_REF_G).clamp(-1.0, 1.0);
                    out.push((dev.clone(), "accel_x".into(), Signal::Float(to_norm(d[0]))));
                    out.push((dev.clone(), "accel_y".into(), Signal::Float(to_norm(d[1]))));
                    out.push((dev.clone(), "accel_z".into(), Signal::Float(to_norm(d[2]))));
                }
            }

            // ── Touchpad fingers via raw FFI (the safe wrapper only exposes
            // capability, not finger data). Read up to 2 fingers on touchpad 0
            // and emit on touch1_*/touch2_* (normalized to [-1,1] to match the
            // raw DualSense touch pins: SDL gives 0..1, so 2*v-1). ──────────────
            if pad.num_touchpads > 0 {
                let fingers = read_touchpad_fingers(*id, 0);
                for (fi, (pin_x, pin_y, pin_a)) in
                    [("touch1_x", "touch1_y", "touch1_active"),
                     ("touch2_x", "touch2_y", "touch2_active")]
                    .iter()
                    .enumerate()
                {
                    if let Some(f) = fingers.get(fi).copied().flatten() {
                        out.push((dev.clone(), pin_x.to_string(), Signal::Float(f.0 * 2.0 - 1.0)));
                        out.push((dev.clone(), pin_y.to_string(), Signal::Float(f.1 * 2.0 - 1.0)));
                        out.push((dev.clone(), pin_a.to_string(), Signal::Bool(true)));
                    } else {
                        out.push((dev.clone(), pin_a.to_string(), Signal::Bool(false)));
                    }
                    let _ = (pin_x, pin_y);
                }
            }

            // Change-detection for the live Hz approximation: hash this pad's
            // slice of `out` and compare to last poll.
            let h = hash_signals(&out[start_len..]);
            sig_updates.push((dev.clone(), h));
        }

        // Update event-count approximation (one tick counted if the pad's signal
        // set changed since last poll — SDL gives no raw event count).
        for (dev, h) in sig_updates {
            let changed = self.last_sig.get(&dev).copied() != Some(h);
            if changed {
                *self.event_counts.entry(dev.clone()).or_insert(0) += 1;
                self.last_sig.insert(dev, h);
            }
        }

        out
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        // Only handle our own ids; other backends handle theirs.
        if !device_id.starts_with(&format!("{ID_PREFIX}:")) {
            return;
        }
        let byte = match signal {
            Signal::Float(f) => (f.clamp(0.0, 1.0) * 255.0) as u8,
            Signal::Bool(b) => if b { 255 } else { 0 },
            _ => return,
        };
        // Find the pad by device id.
        let Some(state) = self.state.as_mut() else { return };
        let Some(pad) = state.pads.values_mut().find(|p| p.dev_id == device_id) else {
            return;
        };
        match pin_id {
            "rumble_strong" => pad.last_rumble.0 = byte,
            "rumble_weak" => pad.last_rumble.1 = byte,
            _ => return,
        }
        // SDL rumble takes 0..65535 per motor. Re-arm with a long duration so it
        // persists between updates (games send a steady stream; we refresh on
        // every change). low = strong (low-freq motor), high = weak (high-freq).
        let (strong, weak) = pad.last_rumble;
        let lo = (strong as u16).saturating_mul(257);
        let hi = (weak as u16).saturating_mul(257);
        // 1s duration; the next change re-arms it well before expiry.
        let _ = pad.gamepad.set_rumble(lo, hi, 1000);
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        self.event_counts.drain().collect()
    }
}

/// Read up to `MAX` fingers from `touchpad` on the SDL gamepad identified by
/// `id`, via the raw FFI `SDL_GetGamepadTouchpadFinger` (the safe wrapper only
/// exposes touchpad *capability*, not finger data). Returns, per finger slot,
/// `Some((x, y))` when that finger is down (x/y normalized 0..1), else `None`.
///
/// Safety: `SDL_GetGamepadFromID` returns the SDL-owned gamepad pointer for a
/// currently-open instance id; we only read into stack locals and never retain
/// the pointer. Runs on the device-io thread with SDL initialized (guaranteed by
/// the caller), which is SDL's threading requirement.
fn read_touchpad_fingers(id: JoystickId, touchpad: i32) -> Vec<Option<(f32, f32)>> {
    use sdl3::sys::gamepad::{SDL_GetGamepadFromID, SDL_GetGamepadTouchpadFinger};
    const MAX: i32 = 2;
    let mut out = Vec::with_capacity(MAX as usize);
    unsafe {
        let gp = SDL_GetGamepadFromID(id);
        if gp.is_null() {
            return vec![None; MAX as usize];
        }
        for finger in 0..MAX {
            let mut down = false;
            let mut x: f32 = 0.0;
            let mut y: f32 = 0.0;
            let mut pressure: f32 = 0.0;
            let ok = SDL_GetGamepadTouchpadFinger(
                gp, touchpad, finger, &mut down, &mut x, &mut y, &mut pressure,
            );
            if ok && down {
                out.push(Some((x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))));
            } else {
                out.push(None);
            }
        }
    }
    out
}

/// Cheap order-independent-ish hash of a signal slice, for change detection only
/// (not correctness-critical). Quantizes floats so tiny jitter doesn't count as
/// a change every poll.
fn hash_signals(slice: &[(String, String, Signal)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (_, pin, sig) in slice {
        pin.hash(&mut h);
        match sig {
            Signal::Bool(b) => (b, 0u8).hash(&mut h),
            Signal::Float(f) => ((f * 200.0) as i32, 1u8).hash(&mut h),
            Signal::Vec2(v) => (((v.x * 200.0) as i32, (v.y * 200.0) as i32), 2u8).hash(&mut h),
            _ => 3u8.hash(&mut h),
        }
    }
    h.finish()
}
