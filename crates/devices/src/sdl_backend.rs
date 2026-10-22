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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec2;

use flexinput_core::raw_pad::{self, RawLayout, RawPadProfiles, RawReading};
use flexinput_core::{Signal, SignalType};

use crate::{
    gyro::{ACCEL_REF_G, GYRO_REF_DPS},
    identification::ControllerKind,
    layouts,
    spike::SpikeFilter,
    DeviceBackend, DevicePin, PhysicalDevice,
};

use sdl3::gamepad::{Axis, Button, Gamepad};
use sdl3::joystick::{HatState, Joystick, JoystickId};
use sdl3::sensor::SensorType;
use sdl3::{GamepadSubsystem, JoystickSubsystem, Sdl};

/// Device-id prefix for SDL pads. Kept distinct from gilrs's `gilrs:` prefix so
/// ids never collide and `send()` routing (which keys off the prefix) is
/// unambiguous. Full form: `sdl:generic:<inst>`.
const ID_PREFIX: &str = "sdl";

// ── SDL sensor frame → FlexInput's canonical IMU frame ─────────────────────
//
// SDL and FlexInput use DIFFERENT body frames, so SDL sensor data cannot be
// copied onto the `gyro_*`/`accel_*` pins directly — doing so lands every axis
// on the wrong pin with the wrong sign. This is the SDL-side equivalent of the
// permutation the Sony parser does (`flexinput_devices::gyro::build`).
//
// SDL's gamepad-sensor frame (controller held in front of you), per SDL3's
// `SDL_sensor.h`:
//   d[0] X: left → right
//   d[1] Y: bottom → top   (a pad at rest reads +Y ≈ 1 g on the accel)
//   d[2] Z: farther → closer (toward you)
//
// Canonical frame (see `HidReading`): x = forward (+nose up), y = side
// (+right grip down), z = vertical (+flat).
//
// Both mappings were VERIFIED against a DualSense read through SDL and compared
// to its native (canonical-correct) parser. Accel matched the docs-derived
// permutation; gyro needed pitch and yaw inverted, because canonical's gyro
// convention is empirically clockwise-positive on roll/yaw rather than the pure
// right-handed rotation of the accel frame — so the pseudovector picks up two
// sign flips a single shared matrix can't carry. Hence two functions.

/// Accelerometer: SDL `[x,y,z]` → canonical `[forward, side, vertical]`.
fn sdl_accel_to_canonical(d: [f32; 3]) -> [f32; 3] {
    [-d[2], -d[0], d[1]]
}

/// Gyroscope: SDL `[x,y,z]` → canonical `[roll, pitch, yaw]`. Same axis
/// assignment as accel; pitch (y) and yaw (z) carry the opposite sign (verified
/// on hardware — see the note above).
fn sdl_gyro_to_canonical(d: [f32; 3]) -> [f32; 3] {
    [-d[2], d[0], -d[1]]
}

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
    /// For the joysticks SDL has no gamepad mapping for — `SDL_GetGamepads`
    /// never lists those, only `SDL_GetJoysticks` does.
    joystick_subsystem: JoystickSubsystem,
    /// Opened generic pads, keyed by SDL joystick instance id.
    pads: HashMap<JoystickId, OpenPad>,
    /// Opened raw joysticks (see [`RawPad`]), keyed the same way.
    raw_pads: HashMap<JoystickId, RawPad>,
}

/// A joystick SDL has no gamepad mapping for — an unknown DirectInput pad, a
/// wheel, a flight stick — read through SDL's joystick API as numbered axes,
/// buttons and hats. Each element is published under its raw pin
/// (`raw:a0`, `raw:b3`, `raw:h0`) and the user's learned layout
/// (`flexinput_core::raw_pad`) turns them into canonical pins.
struct RawPad {
    /// Dropping it calls `SDL_CloseJoystick`.
    joystick: Joystick,
    /// `sdl:raw:<inst>`.
    dev_id: String,
    name: String,
    vid: Option<u16>,
    pid: Option<u16>,
    /// Which learned layout applies (`raw_pad::profile_key`).
    profile_key: String,
    /// The layout compiled from the current profiles; empty until learned.
    layout: RawLayout,
    /// Pin ids per element, built once so the poll doesn't format them.
    axis_pins: Vec<String>,
    button_pins: Vec<String>,
    hat_pins: Vec<String>,
    /// This poll's readings, kept to avoid reallocating every poll.
    axes: Vec<f32>,
    buttons: Vec<bool>,
    hats: Vec<Vec2>,
    /// Last-sent rumble (strong, weak), as for `OpenPad`.
    last_rumble: (u8, u8),
    /// The pad's HID tier (`raw_hid`): the fields SDL doesn't read. `None`
    /// while it opens, or when the pad has no HID path or wouldn't open.
    hid: Option<crate::raw_hid::HidTier>,
    /// SDL's HID path for the pad, kept to reopen a stalled tier.
    hid_path: Option<String>,
    /// When the tier was last (re)requested, to space reopens out.
    hid_requested: Option<Instant>,
    /// Outlier spike filter over a learned motion sensor, as `OpenPad`'s.
    spike: SpikeFilter,
}

fn compile_layout(profiles: &RawPadProfiles, key: &str) -> RawLayout {
    profiles.get(key).map(|p| p.compile()).unwrap_or_default()
}

/// SDL's hat bits → (+x right, +y up).
fn hat_vec(h: HatState) -> Vec2 {
    match h {
        HatState::Centered => Vec2::ZERO,
        HatState::Up => Vec2::new(0.0, 1.0),
        HatState::Down => Vec2::new(0.0, -1.0),
        HatState::Left => Vec2::new(-1.0, 0.0),
        HatState::Right => Vec2::new(1.0, 0.0),
        HatState::RightUp => Vec2::new(1.0, 1.0),
        HatState::RightDown => Vec2::new(1.0, -1.0),
        HatState::LeftUp => Vec2::new(-1.0, 1.0),
        HatState::LeftDown => Vec2::new(-1.0, -1.0),
    }
}

/// One opened SDL gamepad plus the per-device state we carry across polls.
struct OpenPad {
    /// The live SDL handle. Held for the pad's lifetime — SDL zeroes the state
    /// of a closed gamepad, and rumble/sensor calls need it open. Dropping it
    /// calls `SDL_CloseGamepad`.
    gamepad: Gamepad,
    /// Stable device id string (`sdl:<kind>:<inst>`). Assigned once at open so
    /// it survives re-enumeration as long as the pad stays connected. The kind
    /// slug is what `skin_from_device_id` matches on for the icon / 3D model, so
    /// a DualSense-through-SDL reads `sdl:dualsense:N`, not `sdl:generic:N`.
    dev_id: String,
    /// Detected controller kind — drives the pin layout, icon, 3D model and the
    /// kind-gated device UI (calibration, card options). `Generic` unless the
    /// global-SDL switch is on and SDL opened a pad a native parser would own.
    kind: ControllerKind,
    /// Whether this pad reports a gyroscope/accelerometer (checked once at open;
    /// sensors were enabled then too). Gates the per-poll sensor reads.
    has_gyro: bool,
    has_accel: bool,
    /// Frozen-IMU watchdog (see `ImuWatchdog`).
    imu_watch: ImuWatchdog,
    /// Number of touchpads SDL reports for this pad (0 for most generic pads).
    num_touchpads: u16,
    /// Which Misc pins are capacitive sensors on this pad
    /// (`automap::capacitive_misc_mask`), published each poll on
    /// `CAPACITIVE_MASK_PIN` for the "Suppress capacitive touch" mute.
    capacitive_mask: u8,
    /// Last-sent rumble (strong, weak) as 0-255 bytes, to skip redundant
    /// `set_rumble` calls. SDL rumble is re-armed with a long duration each
    /// change and refreshed periodically so it doesn't auto-expire.
    last_rumble: (u8, u8),
    /// Last-sent lightbar colour (r, g, b), to skip redundant `set_led` calls.
    /// SDL drives the DualSense/DS4 lightbar (and player LEDs on some pads).
    last_led: (u8, u8, u8),
    /// Outlier spike filter over this pad's six IMU axes. Lives on the pad so
    /// it is born and dropped with the connection — a reconnect gets a fresh
    /// window rather than judging new samples against pre-disconnect ones.
    spike: SpikeFilter,
}

/// Revives a pad whose IMU stream went dead while the pad itself stayed open.
///
/// A pad's IMU is switched on by a ONE-SHOT command to the device (the Steam
/// Controller's `SETTING_IMU_MODE` feature report, a Switch Pro's IMU/full-report
/// subcommands). Anything that resets the device's settings afterwards — Steam
/// Input reconfiguring the pad when it loses or regains it (HidHide cloaking it
/// the moment a virtual output is assigned), a Bluetooth mode switch that didn't
/// take — turns the stream off, while SDL still believes the sensors are on.
/// Buttons and sticks keep working (SDL re-sends the Steam Controller's
/// lizard-mode-off every 3 s, but never the IMU mode), so the pad looks alive
/// with gyro/accel frozen at their last value.
///
/// Calling `sensor_set_enabled(true)` again does NOTHING: SDL returns early when
/// the sensor is already enabled (`SDL_SetGamepadSensorEnabled`), so the command
/// never reaches the device. Only an off→on toggle of every sensor makes SDL
/// re-send it. A live IMU never repeats a six-axis sample bit-for-bit for long —
/// accel noise alone rules it out — so a sample frozen for `STALL` means the
/// stream is off, and the watchdog toggles, backing off if it doesn't help.
struct ImuWatchdog {
    last: [f32; 6],
    last_change: Instant,
    next_kick: Instant,
    kicks: u32,
}

impl ImuWatchdog {
    const STALL: Duration = Duration::from_millis(500);
    /// First few kicks come quickly (the fix usually takes on the first); a pad
    /// that stays frozen after that is left alone except for a slow retry.
    const FAST_KICKS: u32 = 5;
    const FAST_GAP: Duration = Duration::from_secs(1);
    const SLOW_GAP: Duration = Duration::from_secs(10);

    fn new(now: Instant) -> Self {
        Self { last: [f32::NAN; 6], last_change: now, next_kick: now, kicks: 0 }
    }

    /// Feed one raw sample; true when the sensors should be toggled now.
    fn observe(&mut self, sample: [f32; 6], now: Instant) -> bool {
        if sample.iter().zip(&self.last).any(|(a, b)| a.to_bits() != b.to_bits()) {
            self.last = sample;
            self.last_change = now;
            self.next_kick = now;
            self.kicks = 0;
            return false;
        }
        if now.duration_since(self.last_change) < Self::STALL || now < self.next_kick {
            return false;
        }
        self.kicks += 1;
        let gap = if self.kicks < Self::FAST_KICKS { Self::FAST_GAP } else { Self::SLOW_GAP };
        self.next_kick = now + gap;
        true
    }
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
    /// Joystick ids `sync_raw_pads` turned down (our own virtual, a kind a
    /// native parser owns, nothing to read). Kept apart from `rejected_ids`,
    /// which is pruned against the GAMEPAD list and would forget these every
    /// pass.
    raw_rejected: HashSet<JoystickId>,
    /// Every learned raw-pad layout, as last pushed by the I/O loop.
    raw_profiles: Arc<RawPadProfiles>,
    /// Opens raw pads' HID tiers off this thread; started with the first
    /// raw pad.
    hid_opener: Option<crate::raw_hid::Opener>,
    /// Global "route every pad through SDL" switch (see
    /// `DeviceBackend::set_sdl_all_pads`). When set, `sync_open_pads` claims
    /// EVERY gamepad, not just the ones kind-detect calls `Generic`.
    sdl_all_pads: bool,
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
            raw_rejected: HashSet::new(),
            raw_profiles: Arc::new(RawPadProfiles::new()),
            hid_opener: None,
            sdl_all_pads: false,
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
        let joystick_subsystem = sdl.joystick().ok()?;
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
            joystick_subsystem,
            pads: HashMap::new(),
            raw_pads: HashMap::new(),
        })
    }

    /// Open every currently-connected generic gamepad we haven't opened yet, and
    /// drop handles for pads that have gone away. Returns nothing; mutates
    /// `self.pads`. Called from `poll()`/`enumerate()` so the open set tracks the
    /// live device set.
    fn sync_open_pads(&mut self) {
        let sdl_all = self.sdl_all_pads;
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
            // `gamepads()` lists only joysticks SDL HAS a mapping for; the rest
            // are opened as raw pads by `sync_raw_pads`.
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
            //
            // …unless the global-SDL switch is on, where SDL deliberately claims
            // every pad — including ones gilrs would normally own — so a native
            // pad can be read through SDL for comparison.
            let vid = gamepad.vendor_id();
            let pid = gamepad.product_id();
            let name = gamepad.name().unwrap_or_default();
            let kind = ControllerKind::detect(&name, vid, pid);

            // Never surface FlexInput's OWN HIDMaestro virtual XInput companion
            // (vid 045E / pid 02FF, or a "HIDMaestro"-named pad) through SDL —
            // regardless of the sdl_all switch. Reading our own emulated output
            // back as input would create a feedback loop, and it must never appear
            // as a real controller. Remember the verdict so we don't re-probe it.
            if let (Some(v), Some(p)) = (vid, pid) {
                if crate::gilrs_backend::is_hidmaestro_virtual_xinput(v, p, &name) {
                    rejected.insert(id);
                    continue;
                }
            }
            // Nor one of our own composite USB personas, which SDL sees as a
            // wired Sony pad; the USB serial we gave it is the tell.
            #[cfg(windows)]
            if gamepad.serial_number().is_some_and(|s| flexinput_usbip::is_own_serial(&s)) {
                rejected.insert(id);
                continue;
            }
            if !sdl_all && kind != ControllerKind::Generic {
                // Close (drop) and leave it to gilrs.
                rejected.insert(id);
                continue;
            }

            // Id carries the detected kind so the UI's skin/model/icon
            // resolution (which string-matches the id) picks the right one. The
            // instance suffix is the pad's SERIAL NUMBER when SDL exposes one, so
            // the id is STABLE across a disconnect/reconnect — the canvas node
            // bound to `sdl:dualsense:<serial>` re-attaches to the same physical
            // pad instead of a fresh `:<n>` that orphans the mapping. Two identical
            // pads keep distinct ids because their serials differ. Serial-less pads
            // fall back to the monotonic counter (unstable across reconnect, but
            // still unique this session — the best we can do without a serial).
            let serial = gamepad.serial_number();
            let inst = match serial.as_deref().map(sanitize_inst) {
                Some(s) if !s.is_empty() => s,
                _ => {
                    let n = *next_inst;
                    *next_inst += 1;
                    format!("i{n}")
                }
            };
            let dev_id = format!("{ID_PREFIX}:{}:{}", kind.id_slug(), inst);

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

            // Diagnostics: connection_state() distinguishes Wired vs Wireless
            // (the Switch Pro streams gyro/accel over USB but freezes them over
            // Bluetooth — this confirms which link we're on), and path() reveals
            // whether an own-virtual (HIDMaestro/ViGEm) pad carries a recognizable
            // marker we can filter on. Cheap, logged once per open.
            let conn = match gamepad.connection_state() {
                Ok(sdl3::joystick::ConnectionState::Wired) => "wired",
                Ok(sdl3::joystick::ConnectionState::Wireless) => "wireless",
                Ok(_) => "unknown",
                Err(_) => "err",
            };
            let path = gamepad.path();
            eprintln!(
                "[sdl] opened pad {dev_id} name={name:?} vid={vid:04X?} pid={pid:04X?} \
                 conn={conn} gyro={has_gyro} accel={has_accel} touchpads={num_touchpads} \
                 serial={serial:?} path={path:?}"
            );

            pads.insert(
                id,
                OpenPad {
                    gamepad,
                    dev_id,
                    kind,
                    has_gyro,
                    has_accel,
                    imu_watch: ImuWatchdog::new(Instant::now()),
                    num_touchpads,
                    capacitive_mask: match (vid, pid) {
                        (Some(v), Some(p)) => flexinput_core::automap::capacitive_misc_mask(v, p),
                        _ => 0,
                    },
                    last_rumble: (0, 0),
                    last_led: (0, 0, 0),
                    spike: SpikeFilter::new(),
                },
            );
        }
    }

    /// Open every joystick SDL has NO gamepad mapping for as a [`RawPad`], and
    /// drop the ones that have gone. These are what `SDL_GetGamepads` leaves
    /// out: unknown DirectInput pads, wheels, flight sticks. gilrs lists the
    /// same pads too; the device-io dedup keeps this entry instead, as it
    /// does every SDL entry.
    fn sync_raw_pads(&mut self) {
        let Some(state) = self.state.as_mut() else { return };
        let state = &mut **state;
        let ids = match state.joystick_subsystem.joysticks() {
            Ok(ids) => ids,
            Err(_) => return,
        };
        let present: HashSet<JoystickId> = ids.iter().copied().collect();
        state.raw_pads.retain(|id, _| present.contains(id));
        self.raw_rejected.retain(|id| present.contains(id));

        for id in ids {
            if state.raw_pads.contains_key(&id)
                || self.raw_rejected.contains(&id)
                || state.gamepad_subsystem.is_gamepad(id)
            {
                continue;
            }
            // Identity BEFORE opening: these are cached by SDL's enumeration,
            // no device I/O, so a pad turned down here costs nothing to re-ask.
            let (name, vid, pid) = unsafe {
                use sdl3::sys::joystick::{
                    SDL_GetJoystickNameForID, SDL_GetJoystickProductForID, SDL_GetJoystickVendorForID,
                };
                let p = SDL_GetJoystickNameForID(id);
                let name = if p.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                };
                let v = SDL_GetJoystickVendorForID(id);
                let pr = SDL_GetJoystickProductForID(id);
                (name, (v != 0).then_some(v), (pr != 0).then_some(pr))
            };
            // Never our own virtual, whatever the switch says (see the same
            // guard in `sync_open_pads`).
            if let (Some(v), Some(p)) = (vid, pid) {
                if crate::gilrs_backend::is_hidmaestro_virtual_xinput(v, p, &name) {
                    self.raw_rejected.insert(id);
                    continue;
                }
            }
            // A pad one of the native parsers reads (DualShock 4, DualSense,
            // Switch Pro, Joy-Con 2) belongs to them even unmapped. Generic is
            // the point of this path; XInput is allowed because kind-detect
            // calls ANY Microsoft VID that, and a real XInput pad is always an
            // SDL gamepad — an unmapped 045E joystick is something like a
            // SideWinder.
            let kind = ControllerKind::detect(&name, vid, pid);
            if !matches!(kind, ControllerKind::Generic | ControllerKind::XInput) {
                self.raw_rejected.insert(id);
                continue;
            }
            let joystick = match state.joystick_subsystem.open(id) {
                Ok(j) => j,
                // Not remembered: an open can fail transiently mid-arrival.
                Err(_) => continue,
            };
            let (n_axes, n_buttons, n_hats) =
                (joystick.num_axes(), joystick.num_buttons(), joystick.num_hats());
            if n_axes + n_buttons + n_hats == 0 {
                self.raw_rejected.insert(id);
                continue;
            }
            let serial = unsafe {
                use sdl3::sys::joystick::{SDL_GetJoystickFromID, SDL_GetJoystickSerial};
                let js = SDL_GetJoystickFromID(id);
                let p = if js.is_null() { std::ptr::null() } else { SDL_GetJoystickSerial(js) };
                (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
            };
            let inst = match serial.as_deref().map(sanitize_inst) {
                Some(s) if !s.is_empty() => s,
                _ => {
                    let n = self.next_inst;
                    self.next_inst += 1;
                    format!("i{n}")
                }
            };
            let dev_id = format!("{}{inst}", raw_pad::RAW_DEVICE_PREFIX);
            // The name the device list shows is the one the profile is keyed
            // on — the UI derives the key from the list (`profile_key`).
            let name = if name.is_empty() { "Unknown controller".to_string() } else { name };
            let profile_key = raw_pad::profile_key(vid, pid, &name);
            let layout = compile_layout(&self.raw_profiles, &profile_key);
            // The id-based GUID query, not `Joystick::guid`: that one panics on
            // a zero GUID, and a panic here would take the whole I/O loop down.
            eprintln!(
                "[sdl] opened raw joystick {dev_id} name={name:?} vid={vid:04X?} pid={pid:04X?} \
                 axes={n_axes} buttons={n_buttons} hats={n_hats} guid={} learned={}",
                state.gamepad_subsystem.guid_for_id(id),
                !layout.is_empty(),
            );
            // SDL caps nothing at 256, but a pin index is a u8.
            let cap = |n: u32| n.min(256) as usize;
            let pins = |n: u32, mk: fn(u8) -> raw_pad::RawSrc| {
                (0..cap(n)).map(|i| mk(i as u8).pin_id()).collect::<Vec<_>>()
            };
            // The HID tier opens the collection SDL reads, by SDL's own path
            // (DirectInput reports the HID interface path), off this thread.
            let path = unsafe {
                let p = sdl3::sys::joystick::SDL_GetJoystickPathForID(id);
                (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
            };
            let hid_path = path.filter(|p| p.to_ascii_lowercase().contains("hid"));
            match &hid_path {
                Some(path) => {
                    if self.hid_opener.is_none() {
                        self.hid_opener = crate::raw_hid::Opener::spawn();
                    }
                    if let Some(o) = &self.hid_opener {
                        o.request(&dev_id, path);
                    }
                }
                None => eprintln!("[raw-hid] {dev_id}: no HID path from SDL — SDL's elements only"),
            }
            state.raw_pads.insert(
                id,
                RawPad {
                    joystick,
                    dev_id,
                    name,
                    vid,
                    pid,
                    profile_key,
                    layout,
                    axis_pins: pins(n_axes, raw_pad::RawSrc::Axis),
                    button_pins: pins(n_buttons, raw_pad::RawSrc::Button),
                    hat_pins: pins(n_hats, raw_pad::RawSrc::Hat),
                    axes: Vec::with_capacity(cap(n_axes)),
                    buttons: Vec::with_capacity(cap(n_buttons)),
                    hats: Vec::with_capacity(cap(n_hats)),
                    last_rumble: (0, 0),
                    hid: None,
                    hid_requested: hid_path.as_ref().map(|_| Instant::now()),
                    hid_path,
                    spike: SpikeFilter::new(),
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
    fn name(&self) -> &'static str { "SDL" }

    fn set_spike_filter(&mut self, device_id: &str, enabled: bool, sensitivity_pct: f32, window: u8) {
        // Same lookup shape as `send`: pads are keyed by SDL joystick id, and
        // the UI addresses them by the `sdl:<kind>:<inst>` string. Cheap to do
        // every tick — `set_config` returns immediately when nothing changed.
        let Some(state) = self.state.as_mut() else { return };
        if let Some(pad) = state.pads.values_mut().find(|p| p.dev_id == device_id) {
            pad.spike.bind_capture(device_id);
            pad.spike.set_config(enabled, sensitivity_pct, window);
        } else if let Some(pad) = state.raw_pads.values_mut().find(|p| p.dev_id == device_id) {
            pad.spike.bind_capture(device_id);
            pad.spike.set_config(enabled, sensitivity_pct, window);
        }
    }

    fn set_raw_pad_profiles(&mut self, profiles: &Arc<RawPadProfiles>) {
        if Arc::ptr_eq(&self.raw_profiles, profiles) {
            return;
        }
        self.raw_profiles = Arc::clone(profiles);
        if let Some(state) = self.state.as_mut() {
            for pad in state.raw_pads.values_mut() {
                pad.layout = compile_layout(&self.raw_profiles, &pad.profile_key);
            }
        }
    }

    fn set_sdl_all_pads(&mut self, on: bool) {
        if on == self.sdl_all_pads {
            return;
        }
        self.sdl_all_pads = on;
        // The gate verdict is remembered per id, and a mode change flips which
        // pads pass it. Clear the rejections AND drop every already-opened pad
        // so the next `sync_open_pads` re-probes the full set under the new rule
        // — otherwise a native pad SDL grabbed while ON would stay open (and keep
        // SDL_CloseGamepad from handing it back to gilrs) after switching OFF.
        self.rejected_ids.clear();
        if let Some(state) = self.state.as_mut() {
            state.pads.clear();
        }
    }

    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        puffin::profile_function!();
        if !self.ensure_init() {
            return Vec::new();
        }
        self.sync_open_pads();
        self.sync_raw_pads();

        let mut result = Vec::new();
        let Some(state) = self.state.as_ref() else { return result };
        for pad in state.pads.values() {
            let name = pad.gamepad.name().unwrap_or_else(|| "SDL Gamepad".to_string());
            result.push(PhysicalDevice {
                id: pad.dev_id.clone(),
                display_name: name,
                kind: pad.kind,
                outputs: layouts::outputs_for_pad(
                    pad.kind, pad.gamepad.vendor_id(), pad.gamepad.product_id()),
                inputs: layouts::inputs_for(pad.kind),
                instance_path: None,
                vid: pad.gamepad.vendor_id(),
                pid: pad.gamepad.product_id(),
            });
        }
        for pad in state.raw_pads.values() {
            // Ports for what has been learned, then the bus. A pad learned
            // nothing yet is just its Auto-Map port: the node's Learn adds the
            // rest.
            let mut outputs: Vec<DevicePin> = self.raw_profiles.get(&pad.profile_key)
                .map(|p| p.ports())
                .unwrap_or_default()
                .into_iter()
                .map(|(id, display_name, signal_type)| DevicePin { id, display_name, signal_type })
                .collect();
            outputs.push(DevicePin {
                id: "automap_out".into(),
                display_name: "Auto-Map".into(),
                signal_type: SignalType::AutoMap,
            });
            result.push(PhysicalDevice {
                id: pad.dev_id.clone(),
                display_name: pad.name.clone(),
                kind: ControllerKind::Generic,
                outputs,
                inputs: layouts::inputs_for(ControllerKind::Generic),
                instance_path: None,
                vid: pad.vid,
                pid: pad.pid,
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

        // `as_mut` because each pad's spike filter is stepped in place below.
        // Nothing else in this loop touches `self` — the change-detection
        // hashes are deferred to `sig_updates` for exactly that reason.
        let Some(state) = self.state.as_mut() else { return out };
        for (id, pad) in &mut state.pads {
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
            // Pin names MUST match what `enumerate()` advertised for this pad,
            // i.e. `layouts::outputs_for(pad.kind)`. Every layout — Generic
            // included — now uses the positional vocabulary, so this block is
            // kind-independent. It briefly wasn't: the Generic layout named the
            // stick-clicks/menu buttons btn_lstick/btn_rstick/btn_select/btn_mode
            // and this emitted the native names only for non-Generic kinds. Those
            // Generic-only ids aren't in `automap::ALL_PINS`, so LS/RS/Select/
            // Guide vanished from every AutoMap path on a Steam Controller (or any
            // third-party pad) while direct wires kept working.
            let b = |btn: Button| g.button(btn);
            out.push((dev.clone(), "btn_south".into(), Signal::Bool(b(Button::South))));
            out.push((dev.clone(), "btn_east".into(),  Signal::Bool(b(Button::East))));
            out.push((dev.clone(), "btn_west".into(),  Signal::Bool(b(Button::West))));
            out.push((dev.clone(), "btn_north".into(), Signal::Bool(b(Button::North))));
            out.push((dev.clone(), "btn_lb".into(), Signal::Bool(b(Button::LeftShoulder))));
            out.push((dev.clone(), "btn_rb".into(), Signal::Bool(b(Button::RightShoulder))));
            out.push((dev.clone(), "btn_ls".into(), Signal::Bool(b(Button::LeftStick))));
            out.push((dev.clone(), "btn_rs".into(), Signal::Bool(b(Button::RightStick))));
            out.push((dev.clone(), "btn_start".into(), Signal::Bool(b(Button::Start))));
            out.push((dev.clone(), "btn_back".into(),  Signal::Bool(b(Button::Back))));
            out.push((dev.clone(), "btn_guide".into(), Signal::Bool(b(Button::Guide))));

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

            // ── Extra "Misc1" button. SDL routes the pad's share-class button to
            // Button::Misc1 (Xbox Share, PS5 mic-mute, Switch Capture, etc.). The
            // native layouts give it a kind-specific pin name — btn_mute on the
            // DualSense, btn_capture on the Switch Pro — while the generic layout
            // exposes it (plus the rear paddles and Misc2-6) under the generic
            // btn_misc* names. Emit under the name the advertised layout declares.
            match pad.kind {
                ControllerKind::DualSense => {
                    out.push((dev.clone(), "btn_mute".into(), Signal::Bool(b(Button::Misc1))));
                    // SDL gives the Edge's back buttons as the first paddle pair
                    // and its Fn buttons as the second, as our pins name them.
                    if crate::identification::is_dualsense_edge(g.vendor_id(), g.product_id()) {
                        out.push((dev.clone(), "btn_paddle_l1".into(), Signal::Bool(b(Button::LeftPaddle1))));
                        out.push((dev.clone(), "btn_paddle_r1".into(), Signal::Bool(b(Button::RightPaddle1))));
                        out.push((dev.clone(), "btn_paddle_l2".into(), Signal::Bool(b(Button::LeftPaddle2))));
                        out.push((dev.clone(), "btn_paddle_r2".into(), Signal::Bool(b(Button::RightPaddle2))));
                    }
                }
                ControllerKind::SwitchPro => {
                    out.push((dev.clone(), "btn_capture".into(), Signal::Bool(b(Button::Misc1))));
                }
                ControllerKind::Generic => {
                    // Rear paddles + misc buttons live only on the generic layout;
                    // SDL returns false for absent buttons, harmless to emit.
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
                }
                // XInput / DS4 have no extra share-class pin in their layout.
                _ => {}
            }
            // Touchpad click is a Button in SDL, and SDL's gamepad API has only ONE
            // touchpad button. A two-pad device (Steam Controller 2, Steam Deck)
            // reports its LEFT pad click here and its RIGHT pad click on Misc2
            // (`btn_misc2` above); Touch Zones reads that as Pad B's click
            // (`touchzones::field_click_pins`). Finger contact on either pad comes
            // from the touchpad API below as touch1_* / touch2_*.
            out.push((dev.clone(), "btn_touchpad".into(), Signal::Bool(b(Button::Touchpad))));
            if pad.capacitive_mask != 0 {
                out.push((dev.clone(), flexinput_core::automap::CAPACITIVE_MASK_PIN.into(),
                          Signal::Float(pad.capacitive_mask as f32)));
            }

            // ── Gyro / accel via SDL sensor API. Normalized to the shared
            // ±reference (GYRO_REF_DPS / ACCEL_REF_G) so SDL gyro drops straight
            // into gyro→aim mappings authored for DS4/DualSense/Switch. ─────────
            // Gyro and accel are ONE six-axis sample as far as the spike
            // filter is concerned, so both are read and normalized before
            // either is emitted. A pad with only one of the two still works:
            // the absent half stays zero, which the filter reads as a
            // perfectly quiet axis and leaves alone, and its pins are simply
            // not pushed.
            let mut imu = [0.0f32; 6];
            let mut raw = [0.0f32; 6];
            let mut got_gyro = false;
            let mut got_accel = false;
            if pad.has_gyro {
                let mut d = [0.0f32; 3];
                if g.sensor_get_data(SensorType::Gyroscope, &mut d).is_ok() {
                    raw[..3].copy_from_slice(&d);
                    // SDL's frame → canonical, THEN scale. SDL gyro is rad/s;
                    // convert to deg/s and normalize to the ±ref scale.
                    let c = sdl_gyro_to_canonical(d);
                    let to_norm = |rad_s: f32| (rad_s.to_degrees() / GYRO_REF_DPS).clamp(-1.0, 1.0);
                    imu[0] = to_norm(c[0]);
                    imu[1] = to_norm(c[1]);
                    imu[2] = to_norm(c[2]);
                    got_gyro = true;
                }
            }
            if pad.has_accel {
                let mut d = [0.0f32; 3];
                if g.sensor_get_data(SensorType::Accelerometer, &mut d).is_ok() {
                    raw[3..].copy_from_slice(&d);
                    // SDL's frame → canonical, THEN scale. SDL accel is m/s²;
                    // convert to G and normalize to the ±ref scale.
                    let c = sdl_accel_to_canonical(d);
                    const G: f32 = 9.806_65;
                    let to_norm = |ms2: f32| ((ms2 / G) / ACCEL_REF_G).clamp(-1.0, 1.0);
                    imu[3] = to_norm(c[0]);
                    imu[4] = to_norm(c[1]);
                    imu[5] = to_norm(c[2]);
                    got_accel = true;
                }
            }
            if got_gyro || got_accel {
                // Frozen stream → toggle every sensor off and back on, the only
                // way to make SDL re-send the enable (see `ImuWatchdog`).
                if pad.imu_watch.observe(raw, Instant::now()) {
                    for (has, ty) in [(pad.has_gyro, SensorType::Gyroscope),
                                      (pad.has_accel, SensorType::Accelerometer)] {
                        if has { let _ = g.sensor_set_enabled(ty, false); }
                    }
                    for (has, ty) in [(pad.has_gyro, SensorType::Gyroscope),
                                      (pad.has_accel, SensorType::Accelerometer)] {
                        if has { let _ = g.sensor_set_enabled(ty, true); }
                    }
                    eprintln!("[sdl] IMU frozen on {dev} — re-enabled sensors (kick {})",
                              pad.imu_watch.kicks);
                }
                let imu = pad.spike.push(imu);
                if got_gyro {
                    out.push((dev.clone(), "gyro_x".into(), Signal::Float(imu[0])));
                    out.push((dev.clone(), "gyro_y".into(), Signal::Float(imu[1])));
                    out.push((dev.clone(), "gyro_z".into(), Signal::Float(imu[2])));
                }
                if got_accel {
                    out.push((dev.clone(), "accel_x".into(), Signal::Float(imu[3])));
                    out.push((dev.clone(), "accel_y".into(), Signal::Float(imu[4])));
                    out.push((dev.clone(), "accel_z".into(), Signal::Float(imu[5])));
                }
            }

            // ── Touchpad fingers via raw FFI (the safe wrapper only exposes
            // capability, not finger data). Emit on touch1_*/touch2_* normalized
            // to [-1,1] (SDL gives 0..1, so 2*v-1) to match the raw DualSense pins.
            //
            // A device with TWO touchpads (Steam Controller: left + right pads)
            // maps each PAD to a field — pad 0 → touch1 (field 0), pad 1 → touch2
            // (field 1) — so split-mode Touch Zones addresses the two sides
            // independently. A single-touchpad device (DualSense/DS4) keeps the
            // two-fingers-on-one-pad convention (finger 0/1 → the two fields).
            let emit_finger = |out: &mut Vec<_>, f: Option<(f32, f32)>,
                               pin_x: &str, pin_y: &str, pin_a: &str| {
                if let Some(f) = f {
                    // Y is negated: SDL reports the touchpad top as 0, but the
                    // canonical touch pins (matching the native DualSense path)
                    // put +Y at the top. Verified against the native path.
                    out.push((dev.clone(), pin_x.to_string(), Signal::Float(f.0 * 2.0 - 1.0)));
                    out.push((dev.clone(), pin_y.to_string(), Signal::Float(1.0 - f.1 * 2.0)));
                    out.push((dev.clone(), pin_a.to_string(), Signal::Bool(true)));
                } else {
                    out.push((dev.clone(), pin_a.to_string(), Signal::Bool(false)));
                }
            };
            if pad.num_touchpads >= 2 {
                let p0 = read_touchpad_fingers(*id, 0).into_iter().flatten().next();
                let p1 = read_touchpad_fingers(*id, 1).into_iter().flatten().next();
                emit_finger(&mut out, p0, "touch1_x", "touch1_y", "touch1_active");
                emit_finger(&mut out, p1, "touch2_x", "touch2_y", "touch2_active");
            } else if pad.num_touchpads == 1 {
                let fingers = read_touchpad_fingers(*id, 0);
                emit_finger(&mut out, fingers.first().copied().flatten(),
                    "touch1_x", "touch1_y", "touch1_active");
                emit_finger(&mut out, fingers.get(1).copied().flatten(),
                    "touch2_x", "touch2_y", "touch2_active");
            }

            // Change-detection for the live Hz approximation: hash this pad's
            // slice of `out` and compare to last poll.
            let h = hash_signals(&out[start_len..]);
            sig_updates.push((dev.clone(), h));
        }

        // ── Raw joysticks: every element under its raw pin, then whatever the
        // learned layout makes of them. ──────────────────────────────────────
        // HID tiers that finished opening join their pad first.
        if let Some(opener) = &self.hid_opener {
            for (dev_id, opened) in opener.finished() {
                let Some(pad) = state.raw_pads.values_mut().find(|p| p.dev_id == dev_id) else {
                    continue; // gone before its HID tier opened
                };
                match opened {
                    Ok(o) => {
                        let tier = o.into_tier(&dev_id);
                        if let Some(info) = crate::raw_hid::hid_info(&dev_id) {
                            eprintln!(
                                "[raw-hid] {dev_id}: HID tier open — {} field(s) past SDL, {} vendor blob(s) {:?}",
                                info.fields.len(), info.blobs.len(), info.blobs,
                            );
                        }
                        pad.hid = Some(tier);
                    }
                    Err(e) => eprintln!("[raw-hid] {dev_id}: {e} — SDL's elements only"),
                }
            }
        }
        for pad in state.raw_pads.values_mut() {
            let start_len = out.len();
            let js = &pad.joystick;
            pad.axes.clear();
            pad.buttons.clear();
            pad.hats.clear();
            for (i, pin) in pad.axis_pins.iter().enumerate() {
                let v = js.axis(i as u32).map_or(0.0, |v| (v as f32 / 32767.0).clamp(-1.0, 1.0));
                pad.axes.push(v);
                out.push((pad.dev_id.clone(), pin.clone(), Signal::Float(v)));
            }
            for (i, pin) in pad.button_pins.iter().enumerate() {
                let b = js.button(i as u32).unwrap_or(false);
                pad.buttons.push(b);
                out.push((pad.dev_id.clone(), pin.clone(), Signal::Bool(b)));
            }
            for (i, pin) in pad.hat_pins.iter().enumerate() {
                let h = js.hat(i as u32).map_or(Vec2::ZERO, hat_vec);
                pad.hats.push(h);
                out.push((pad.dev_id.clone(), pin.clone(), Signal::Vec2(h)));
            }
            if let Some(hid) = pad.hid.as_mut() {
                hid.drain();
                hid.publish(&mut out);
            }
            // A stalled tier is dropped and opened again, at most every
            // 10 s (see `HidTier::stalled`).
            if pad.hid.as_ref().is_some_and(|h| h.stalled())
                && pad.hid_requested.is_none_or(|t| t.elapsed() >= Duration::from_secs(10))
            {
                if let (Some(o), Some(path)) = (&self.hid_opener, &pad.hid_path) {
                    eprintln!("[raw-hid] {}: no reports for a while — reopening the HID tier", pad.dev_id);
                    pad.hid = None;
                    pad.hid_requested = Some(Instant::now());
                    o.request(&pad.dev_id, path);
                }
            }
            if !pad.layout.is_empty() {
                let reports: &dyn raw_pad::ReportSource = match &pad.hid {
                    Some(h) => &h.reports,
                    None => &(),
                };
                let reading = RawReading { axes: &pad.axes, buttons: &pad.buttons, hats: &pad.hats, reports };
                let dev = &pad.dev_id;
                pad.layout.apply(&reading, |id, s| out.push((dev.clone(), id.to_string(), s)));
                // A learned motion sensor: through the spike filter as one
                // six-axis sample, like the SDL gamepad path's.
                let (gyro, accel) = pad.layout.motion(&reading);
                if gyro.is_some() || accel.is_some() {
                    let mut imu = [0.0f32; 6];
                    imu[..3].copy_from_slice(&gyro.unwrap_or_default());
                    imu[3..].copy_from_slice(&accel.unwrap_or_default());
                    let imu = pad.spike.push(imu);
                    if gyro.is_some() {
                        for (pin, v) in ["gyro_x", "gyro_y", "gyro_z"].iter().zip(&imu[..3]) {
                            out.push((dev.clone(), pin.to_string(), Signal::Float(*v)));
                        }
                    }
                    if accel.is_some() {
                        for (pin, v) in ["accel_x", "accel_y", "accel_z"].iter().zip(&imu[3..]) {
                            out.push((dev.clone(), pin.to_string(), Signal::Float(*v)));
                        }
                    }
                }
            }
            let h = hash_signals(&out[start_len..]);
            sig_updates.push((pad.dev_id.clone(), h));
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
        if raw_pad::is_raw_device_id(device_id) {
            // A raw joystick has nothing but rumble, where its driver offers
            // force feedback at all (SDL's DirectInput driver plays it).
            let Some(pad) = state.raw_pads.values_mut().find(|p| p.dev_id == device_id) else {
                return;
            };
            match pin_id {
                "rumble_strong" => pad.last_rumble.0 = byte,
                "rumble_weak" => pad.last_rumble.1 = byte,
                _ => return,
            }
            let (strong, weak) = pad.last_rumble;
            let _ = pad.joystick.set_rumble(
                (strong as u16).saturating_mul(257),
                (weak as u16).saturating_mul(257),
                1000,
            );
            return;
        }
        let Some(pad) = state.pads.values_mut().find(|p| p.dev_id == device_id) else {
            return;
        };
        // Which motor (if any) this pin drives. SDL exposes only two rumble
        // motors (low-freq "strong", high-freq "weak"), so every haptic pin a
        // pad's layout declares is collapsed onto those two:
        //   • rumble_strong/weak — the XInput-style pins (DS4/DualSense layouts).
        //   • hd_l_amp/hd_r_amp + legacy hd_rumble_l/r — the HD-rumble AMPLITUDE
        //     pins. SDL can't reproduce true dual-band HD rumble, but mapping
        //     per-side amplitude onto the two motors means a Switch Pro through
        //     SDL still rumbles at all: its layout has NO rumble_strong/weak pin,
        //     so without this it was silent. Left side → strong (low) motor,
        //     right side → weak (high) motor.
        // Frequency/second-carrier HD pins have no SDL analogue and are dropped.
        let rumble_motor = match pin_id {
            "rumble_strong" | "hd_l_amp" | "hd_rumble_l" => Some(0),
            "rumble_weak" | "hd_r_amp" | "hd_rumble_r" => Some(1),
            _ => None,
        };
        if let Some(motor) = rumble_motor {
            if motor == 0 { pad.last_rumble.0 = byte; } else { pad.last_rumble.1 = byte; }
            // SDL rumble takes 0..65535 per motor. Re-arm with a long duration so
            // it persists between updates (games send a steady stream; we refresh
            // on every change). low = strong (low-freq motor), high = weak.
            let (strong, weak) = pad.last_rumble;
            let _ = pad.gamepad.set_rumble(
                (strong as u16).saturating_mul(257),
                (weak as u16).saturating_mul(257),
                1000, // 1s; the next change re-arms it well before expiry.
            );
            return;
        }
        match pin_id {
            // Lightbar (DualSense / DS4). SDL drives it via SDL_SetGamepadLED;
            // a pad without an LED just returns an error we ignore. Only push
            // on a real change so we don't spam the HID endpoint.
            "lightbar_r" | "lightbar_g" | "lightbar_b" => {
                match pin_id {
                    "lightbar_r" => pad.last_led.0 = byte,
                    "lightbar_g" => pad.last_led.1 = byte,
                    _ => pad.last_led.2 = byte,
                }
                let (r, g, b) = pad.last_led;
                let _ = pad.gamepad.set_led(r, g, b);
            }
            _ => {}
        }
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

/// Sanitize a pad serial for use as the `:<inst>` segment of a device id.
/// Keeps only ASCII alphanumerics (lowercased) so the result can't contain a
/// `:` — a serial like a Bluetooth MAC (`AA:BB:CC:…`) would otherwise break the
/// `sdl:<slug>:<inst>` split that `phys_pad_slug` and the id parsers rely on.
fn sanitize_inst(serial: &str) -> String {
    serial
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDL→canonical accel remap, pinned so a stray edit can't silently
    /// re-scramble every SDL device's accel. Verified on hardware.
    #[test]
    fn sdl_accel_maps_to_canonical() {
        // Exact permutation: canonical = [-z, -x, y].
        assert_eq!(sdl_accel_to_canonical([1.0, 2.0, 3.0]), [-3.0, -1.0, 2.0]);
        // A pad at rest reads +1 g up SDL Y → canonical +Z (vertical, flat).
        assert_eq!(sdl_accel_to_canonical([0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]);
    }

    /// A serial with colons (e.g. a Bluetooth MAC) must not survive into the id
    /// segment — it would break the `sdl:<slug>:<inst>` split.
    #[test]
    fn sanitize_inst_strips_separators() {
        assert_eq!(sanitize_inst("AA:BB:CC:DD:EE:FF"), "aabbccddeeff");
        assert_eq!(sanitize_inst("Pro-Ctrl_01"), "proctrl01");
        assert_eq!(sanitize_inst(""), "");
        // No `:` can ever appear, so the id always splits into exactly 3 fields.
        assert!(!sanitize_inst("a:b:c").contains(':'));
    }

    /// The gyro remap shares the accel axis assignment but inverts pitch (y) and
    /// yaw (z) — canonical gyro is clockwise-positive, not the accel frame's
    /// right-handed rotation. Verified on hardware (DualSense through SDL vs
    /// native).
    #[test]
    fn sdl_gyro_maps_to_canonical() {
        assert_eq!(sdl_gyro_to_canonical([1.0, 2.0, 3.0]), [-3.0, 1.0, -2.0]);
        // Roll (canonical x) keeps the accel sign; pitch/yaw are flipped.
        let a = sdl_accel_to_canonical([4.0, 5.0, 6.0]);
        let g = sdl_gyro_to_canonical([4.0, 5.0, 6.0]);
        assert_eq!(g[0], a[0], "roll matches accel");
        assert_eq!(g[1], -a[1], "pitch is inverted vs accel");
        assert_eq!(g[2], -a[2], "yaw is inverted vs accel");
    }

    /// Kicks only after a sample is frozen for `STALL`, backs off between kicks,
    /// and forgets the streak the moment data moves again.
    #[test]
    fn imu_watchdog_kicks_only_on_a_frozen_stream() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = ImuWatchdog::new(t0);
        let s = [0.1, 0.2, 0.3, 0.0, -1.0, 0.0];

        assert!(!w.observe(s, ms(0)));
        assert!(!w.observe(s, ms(499)), "a repeat within STALL is normal between reports");
        assert!(w.observe(s, ms(500)), "frozen for STALL → kick");
        assert!(!w.observe(s, ms(900)), "no second kick inside the gap");
        assert!(w.observe(s, ms(1500)));

        let mut moved = s;
        moved[3] = 0.001;
        assert!(!w.observe(moved, ms(1600)), "live data never kicks");
        assert_eq!(w.kicks, 0, "streak resets once data moves");

        for k in 1..ImuWatchdog::FAST_KICKS {
            assert!(w.observe(moved, ms(2100 + (k as u64 - 1) * 1000)));
        }
        let t = 2100 + (ImuWatchdog::FAST_KICKS as u64 - 2) * 1000;
        assert!(w.observe(moved, ms(t + 1000)), "last fast kick");
        assert!(!w.observe(moved, ms(t + 2000)), "then backs off to SLOW_GAP");
        assert!(w.observe(moved, ms(t + 11000)));
    }
}
