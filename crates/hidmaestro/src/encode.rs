//! Backend-neutral gamepad state → HID report encoder.
//!
//! Port of the relevant parts of `HidReportBuilder.BuildReportInto` (v1.3.17):
//! per-axis normalized write by usage, hat octant encoding, button packing with
//! the profile's `buttonMap`, and trigger→button derivation. Semantic-axis
//! *resolution* heuristics are not ported — instead [`GamepadState`] exposes
//! roles directly and the encoder places them via the profile's `axisMap`
//! (usage→role) plus standard usage fallbacks. This is exact for the preset
//! profiles FlexInput ships.

use crate::descriptor::{write_bits, write_field, InputField};
use crate::profile::Profile;

/// 8-way hat direction (matches HMHat / HID hat octants). `Neutral` writes the
/// descriptor's null state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hat {
    #[default]
    Neutral,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

impl Hat {
    /// HMHat octant value 1..8 (0 = neutral). Matches HMHat enum ordering.
    fn octant(self) -> i32 {
        match self {
            Hat::Neutral => 0,
            Hat::North => 1,
            Hat::NorthEast => 2,
            Hat::East => 3,
            Hat::SouthEast => 4,
            Hat::South => 5,
            Hat::SouthWest => 6,
            Hat::West => 7,
            Hat::NorthWest => 8,
        }
    }

    /// Derive a hat from d-pad up/down/left/right booleans (8-way).
    pub fn from_dpad(up: bool, down: bool, left: bool, right: bool) -> Hat {
        match (up, down, left, right) {
            (true, _, false, false) => Hat::North,
            (true, _, false, true) => Hat::NorthEast,
            (false, false, false, true) => Hat::East,
            (_, true, false, true) => Hat::SouthEast,
            (_, true, false, false) => Hat::South,
            (_, true, true, false) => Hat::SouthWest,
            (false, false, true, false) => Hat::West,
            (true, _, true, false) => Hat::NorthWest,
            _ => Hat::Neutral,
        }
    }
}

/// Backend-neutral gamepad state. Sticks/triggers are normalized:
/// sticks `0.0..1.0` with `0.5` = center; triggers `0.0..1.0` with `0.0` =
/// released. `buttons` is an HMButton-style bitmask (bit 0 = A/Cross, …).
#[derive(Debug, Clone, Default)]
pub struct GamepadState {
    pub left_stick_x: f32,
    pub left_stick_y: f32,
    pub right_stick_x: f32,
    pub right_stick_y: f32,
    pub left_trigger: f32,
    pub right_trigger: f32,
    pub buttons: u32,
    pub hat: Hat,
    /// Gyro (pitch, yaw, roll) and accel (x, y, z), normalized to the signal
    /// graph reference: `±1.0` = `±GYRO_REF_DPS` / `±ACCEL_REF_G` (see
    /// `flexinput_devices::gyro`). The encoder rescales to the device's int16
    /// LSB range when the profile declares an IMU region.
    pub gyro_pitch: f32,
    pub gyro_yaw: f32,
    pub gyro_roll: f32,
    pub accel_x: f32,
    pub accel_y: f32,
    pub accel_z: f32,
    /// Touchpad fingers. Normalized X/Y in roughly `[-1, 1]` (centre = 0), matching
    /// the physical-side read range (`flexinput_devices::gyro::parse_dualsense_touch`),
    /// so a real DualSense's touch can be mapped straight onto a virtual one. Only
    /// emitted when the profile declares `touch_fingers`; `active=false` writes the
    /// "finger not touching" sentinel.
    pub touch1: TouchOut,
    pub touch2: TouchOut,
}

/// One touchpad finger for encoding into a virtual DS4/DualSense report.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TouchOut {
    pub active: bool,
    pub x: f32,
    pub y: f32,
}

impl GamepadState {
    /// Neutral state: sticks centered, triggers released, no buttons, hat
    /// neutral. (Note: per-axis defaults during encode also center signed axes,
    /// so an all-zero `GamepadState` still encodes a centered report.)
    pub fn neutral() -> Self {
        GamepadState {
            left_stick_x: 0.5,
            left_stick_y: 0.5,
            right_stick_x: 0.5,
            right_stick_y: 0.5,
            ..Default::default()
        }
    }
}

/// HMButton bit positions (subset we map). Mirrors HMButton.
pub mod button {
    pub const A: u32 = 1 << 0; // Cross
    pub const B: u32 = 1 << 1; // Circle
    pub const X: u32 = 1 << 2; // Square
    pub const Y: u32 = 1 << 3; // Triangle
    pub const LEFT_BUMPER: u32 = 1 << 4;
    pub const RIGHT_BUMPER: u32 = 1 << 5;
    pub const BACK: u32 = 1 << 6; // Share
    pub const START: u32 = 1 << 7; // Options
    pub const LEFT_STICK: u32 = 1 << 8; // L3
    pub const RIGHT_STICK: u32 = 1 << 9; // R3
    pub const GUIDE: u32 = 1 << 10; // PS
    pub const TOUCHPAD: u32 = 1 << 11;
    pub const LT_DIGITAL: u32 = 1 << 12; // L2 digital click
    pub const RT_DIGITAL: u32 = 1 << 13; // R2 digital click
    pub const MUTE: u32 = 1 << 14; // DualSense mic-mute button
}

/// HID usage (page 0x01 Generic Desktop) for the standard axes.
mod usage {
    pub const X: u16 = 0x30;
    pub const Y: u16 = 0x31;
    pub const Z: u16 = 0x32;
    pub const RX: u16 = 0x33;
    pub const RY: u16 = 0x34;
    pub const RZ: u16 = 0x35;
    pub const HAT: u16 = 0x39;
}

/// Resolve which descriptor usage carries each semantic role for `profile`,
/// honoring its `axisMap` (usage-hex → role) and falling back to the common
/// Xbox-style convention (X/Y = left, Z/Rz = right, Rx/Ry = triggers).
struct AxisRoles {
    left_x: Option<u16>,
    left_y: Option<u16>,
    right_x: Option<u16>,
    right_y: Option<u16>,
    left_trigger: Option<u16>,
    right_trigger: Option<u16>,
}

impl AxisRoles {
    fn resolve(profile: &Profile) -> Self {
        // Defaults: X/Y left stick; right stick + triggers vary per family, so
        // start them unset and let axisMap (or the Sony fallback) fill them.
        let mut r = AxisRoles {
            left_x: Some(usage::X),
            left_y: Some(usage::Y),
            right_x: None,
            right_y: None,
            left_trigger: None,
            right_trigger: None,
        };
        // axisMap entries override by role (e.g. DS4: 0x32→rightStickX,
        // 0x33→leftTrigger, 0x34→rightTrigger, 0x35→rightStickY).
        for (usage_hex, role) in &profile.axis_map {
            let Ok(u) = parse_usage(usage_hex) else { continue };
            match role.to_ascii_lowercase().as_str() {
                "leftstickx" => r.left_x = Some(u),
                "leftsticky" => r.left_y = Some(u),
                "rightstickx" => r.right_x = Some(u),
                "rightsticky" => r.right_y = Some(u),
                "lefttrigger" => r.left_trigger = Some(u),
                "righttrigger" => r.right_trigger = Some(u),
                _ => {}
            }
        }
        // If axisMap didn't set the right stick / triggers, fall back to the
        // Xbox-style convention so non-Sony presets still work.
        r.right_x.get_or_insert(usage::Z);
        r.right_y.get_or_insert(usage::RZ);
        r.left_trigger.get_or_insert(usage::RX);
        r.right_trigger.get_or_insert(usage::RY);
        r
    }
}

fn parse_usage(s: &str) -> Result<u16, ()> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(t, 16).map_err(|_| ())
}

/// Encode `state` into a freshly-built report for `profile`, returning the
/// on-wire bytes (including the leading Report-ID byte when the descriptor
/// declares one). This is what gets copied into the SHM input frame's `Data[]`.
pub fn encode_report(profile: &Profile, state: &GamepadState) -> Vec<u8> {
    let mut report = vec![0u8; profile.report.byte_size()];
    encode_report_into(profile, state, &mut report);
    report
}

/// Encode into a caller-provided buffer (must be at least `byte_size()`).
pub fn encode_report_into(profile: &Profile, state: &GamepadState, report: &mut [u8]) {
    let r = &profile.report;
    let id_offset = if r.report_id != 0 { 8 } else { 0 };

    // Clear + write the report-ID byte.
    let n = r.byte_size().min(report.len());
    report[..n].fill(0);
    if r.report_id != 0 && !report.is_empty() {
        report[0] = r.report_id;
    }

    let roles = AxisRoles::resolve(profile);
    let put = |report: &mut [u8], usage: Option<u16>, v: f32| {
        if let Some(u) = usage {
            if let Some(f) = r.field(0x01, u) {
                write_field(report, id_offset, f, v.clamp(0.0, 1.0) as f64);
            }
        }
    };

    // Auto-default every declared analog axis first (signed-centered → 0.5,
    // unsigned → 0.0), so axes we don't drive sit at a sane rest value —
    // mirrors the per-axis default pass in BuildReportInto.
    for f in r.fields.iter().filter(|f| f.usage_page == 0x01 && !f.is_constant) {
        if is_axis_usage(f.usage) {
            let def: f64 = if f.logical_min < 0 { 0.5 } else { 0.0 };
            write_field(report, id_offset, f, def);
        }
    }

    // Now overwrite the semantic axes we actually drive.
    put(report, roles.left_x, state.left_stick_x);
    put(report, roles.left_y, state.left_stick_y);
    put(report, roles.right_x, state.right_stick_x);
    put(report, roles.right_y, state.right_stick_y);
    put(report, roles.left_trigger, state.left_trigger);
    put(report, roles.right_trigger, state.right_trigger);

    // Hat (octant → descriptor range), if the descriptor declares one.
    if let Some(hat) = r.field(0x01, usage::HAT) {
        let range = hat.logical_max - hat.logical_min + 1;
        let oct = state.hat.octant();
        let raw = if oct == 0 {
            // Neutral: null state outside the logical range.
            if hat.logical_min == 0 { hat.logical_max + 1 } else { 0 }
        } else {
            hat.logical_min + (oct - 1) * range / 8
        };
        write_bits(report, hat.bit_offset + id_offset, hat.bit_size, raw);
    }

    // Buttons: HMButton bit → descriptor button index via buttonMap (identity
    // when absent), then set the descBtn-th button field.
    let buttons: Vec<&InputField> = r
        .fields
        .iter()
        .filter(|f| f.usage_page == 0x09)
        .collect();
    let mut mask = state.buttons;
    while mask != 0 {
        let b = mask.trailing_zeros() as usize;
        mask &= mask - 1;
        let desc_btn = match &profile.button_map {
            Some(m) if b < m.len() => m[b],
            _ => b as i32,
        };
        if desc_btn >= 0 && (desc_btn as usize) < buttons.len() {
            let f = buttons[desc_btn as usize];
            write_bits(report, f.bit_offset + id_offset, f.bit_size, 1);
        }
    }

    // IMU + touchpad: byte-addressed regions the HID descriptor exposes only as
    // opaque vendor blobs, driven from the profile's `extendedReport` layout.
    encode_extended(profile, state, report);
}

/// The HIDMaestro companion's GIP button-word bit flags — **NOT** `XINPUT_GAMEPAD`
/// `wButtons` order. Mapped empirically against `XInputGetState` (verified live
/// 2026-06-15): setting GIP-word bit `B` produced the XInput button on the right
/// (the binary driver gives no spec). The face/shoulder/stick/menu buttons form a clean
/// dense block 0x0001..0x0200; the d-pad sits in the high nibble (Up confirmed at
/// 0x0400; the remaining directions are a 4-bit hat — see `gip_dpad`).
mod gip_button {
    pub const A: u16 = 0x0001;
    pub const B: u16 = 0x0002;
    pub const X: u16 = 0x0004;
    pub const Y: u16 = 0x0008;
    pub const LEFT_SHOULDER: u16 = 0x0010;
    pub const RIGHT_SHOULDER: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const BACK: u16 = 0x0100;
    pub const START: u16 = 0x0200;
    /// The d-pad is an HMHat octant (0 = neutral, 1 = N … 8 = NW) packed into
    /// bits 10..14 of the GIP button word, NOT four independent direction bits.
    /// Verified live: 0x0400 (octant 1) → Up; 0x1000 (octant 4) → Down+Right (SE);
    /// 0x2000 (octant 8) → Up+Left (NW). Shift the octant left by `DPAD_SHIFT`.
    pub const DPAD_SHIFT: u32 = 10;
}

/// Build the 14-byte GIP input payload from `state`, for the HIDMaestro XUSB
/// companion (Xbox360/XInput pad), written into the SHM input section's
/// `GipData[14]` region (offset 264).
///
/// **The HIDMaestro companion's GIP layout is NOT `XINPUT_GAMEPAD` order** — it
/// was mapped empirically against `XInputGetState` (the binary driver gives no
/// spec). Verified layout (live against `XInputGetState`, 2026-06-15):
/// ```text
/// [0..2]   LX (u16 LE; 0x0000 = full left, 0x8000 = center, 0xFFFF = full right)
/// [2..4]   LY (u16 LE; INVERTED vs X: 0x0000 = full UP, 0x8000 = center, 0xFFFF = down)
/// [4..6]   RX (u16 LE; like LX)
/// [6..8]   RY (u16 LE; INVERTED like LY)
/// [8]      LT (u8; companion rescales to XInput's 0..63, so 255 → LT=63)
/// [10]     RT (u8; same)          [9],[11] unused
/// [12..14] buttons (u16 LE, the companion's OWN order — see `gip_button`)
/// ```
/// Sticks are UNSIGNED here (center = 0x8000), unlike `XINPUT_GAMEPAD`'s signed
/// i16. Input arrives normalized `0.0..1.0` (0.5 = center); Y is already in
/// up-from-1.0 sense from `PinState::state` (it inverts once), and the unsigned
/// 0xFFFF = up mapping matches that without a second flip. The button word is the
/// companion's own bit order (A=0x0001, …, d-pad in the high nibble), NOT
/// XINPUT_GAMEPAD's; d-pad comes from `state.hat`.
pub fn gip_from_state(state: &GamepadState) -> [u8; 14] {
    let mut g = [0u8; 14];

    // Sticks: normalized 0..1 → unsigned 0..65535 (0.5 → 0x8000 center). All four
    // axes pass straight through: `GamepadState` Y already counts up (PinState's
    // state() inverts Y once), and the companion's GIP Y maps high→up, so no
    // second flip is needed (a second flip re-inverts Y — the observed bug).
    let to_u16 = |n: f32| -> u16 { (n.clamp(0.0, 1.0) * 65535.0).round() as u16 };
    let lx = to_u16(state.left_stick_x);
    let ly = to_u16(state.left_stick_y);
    let rx = to_u16(state.right_stick_x);
    let ry = to_u16(state.right_stick_y);
    g[0..2].copy_from_slice(&lx.to_le_bytes());
    g[2..4].copy_from_slice(&ly.to_le_bytes());
    g[4..6].copy_from_slice(&rx.to_le_bytes());
    g[6..8].copy_from_slice(&ry.to_le_bytes());

    // Triggers: u16 LE at [8..10] (LT) and [10..12] (RT). The companion scales the
    // u16 down by ~4 to XInput's 0..255, so the LOW byte alone tops out at LT=63
    // (≈0.25 — the cap symptom); full scale needs ~1020. Verified live: u16 0x03FC
    // (1020) → LT=254, 0x0FFF → 255. Write trigger * 1020.
    let trig_u16 = |n: f32| -> u16 { (n.clamp(0.0, 1.0) * 1020.0).round() as u16 };
    g[8..10].copy_from_slice(&trig_u16(state.left_trigger).to_le_bytes());
    g[10..12].copy_from_slice(&trig_u16(state.right_trigger).to_le_bytes());

    // Buttons (u16 LE @ 12): the companion's own GIP button order (mapped live,
    // NOT XINPUT_GAMEPAD). Guide has no slot in the confirmed block (it aliased
    // into the d-pad nibble during probing), so it's intentionally omitted — XInput
    // treats Guide as undocumented anyway.
    let mut w: u16 = 0;
    let b = state.buttons;
    if b & button::A != 0 { w |= gip_button::A; }
    if b & button::B != 0 { w |= gip_button::B; }
    if b & button::X != 0 { w |= gip_button::X; }
    if b & button::Y != 0 { w |= gip_button::Y; }
    if b & button::LEFT_BUMPER != 0 { w |= gip_button::LEFT_SHOULDER; }
    if b & button::RIGHT_BUMPER != 0 { w |= gip_button::RIGHT_SHOULDER; }
    if b & button::BACK != 0 { w |= gip_button::BACK; }
    if b & button::START != 0 { w |= gip_button::START; }
    if b & button::LEFT_STICK != 0 { w |= gip_button::LEFT_THUMB; }
    if b & button::RIGHT_STICK != 0 { w |= gip_button::RIGHT_THUMB; }
    // D-pad: HMHat octant (0..8) packed into bits 10..14 (verified live).
    w |= (state.hat.octant() as u16) << gip_button::DPAD_SHIFT;
    g[12..14].copy_from_slice(&w.to_le_bytes());

    g
}

/// Write the int16-LE little-endian value `v` at byte `off` (RID-inclusive
/// offset into the on-wire report), if in bounds.
fn write_i16_le(report: &mut [u8], off: usize, v: i16) {
    if off + 1 < report.len() {
        let b = v.to_le_bytes();
        report[off] = b[0];
        report[off + 1] = b[1];
    }
}

/// Encode the gyro/accel/touchpad regions from `state` into `report` using the
/// profile's byte-addressed `extendedReport` layout.
///
/// Gyro/accel arrive normalized (`±1.0` = `±GYRO_REF_DPS` / `±ACCEL_REF_G`).
/// DS4/DualSense run factory ±2000 dps / ±8 G over the full int16 range, and the
/// signal-graph reference is exactly those values, so `±1.0 → ±32767`.
///
/// Touchpad: each active finger's normalized X/Y is packed into its declared
/// 4-byte block (reverse of the physical-side parse); an inactive finger gets
/// bit 7 set on its first byte ("finger not touching"), which is what a real
/// Sony pad sends and what Steam expects (a zeroed block reads as a stuck touch
/// at 0,0). See `write_dualsense_touch`.
fn encode_extended(profile: &Profile, state: &GamepadState, report: &mut [u8]) {
    let ext = &profile.extended;
    let to_i16 = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;

    if let Some(o) = ext.gyro_pitch {
        write_i16_le(report, o, to_i16(state.gyro_pitch));
    }
    if let Some(o) = ext.gyro_yaw {
        write_i16_le(report, o, to_i16(state.gyro_yaw));
    }
    if let Some(o) = ext.gyro_roll {
        write_i16_le(report, o, to_i16(state.gyro_roll));
    }
    if let Some(o) = ext.accel_x {
        write_i16_le(report, o, to_i16(state.accel_x));
    }
    if let Some(o) = ext.accel_y {
        write_i16_le(report, o, to_i16(state.accel_y));
    }
    if let Some(o) = ext.accel_z {
        write_i16_le(report, o, to_i16(state.accel_z));
    }

    // Battery: always report a full/healthy charge. A virtual pad has no real
    // battery, and a low/critical reading makes some hosts disable haptics (and
    // shows an alarming "controller low" prompt). Both Sony families pack the
    // level in the low nibble: DualSense 0..8 (8 = full); DS4 0..10 (cabled-full
    // reports ~11). Writing low-nibble 0x08 reads as full on DualSense and
    // comfortably "not low" on DS4 — enough to keep haptics enabled — while
    // leaving the high-nibble charge/cable status untouched.
    if let Some(o) = ext.battery_level {
        if o < report.len() {
            report[o] = (report[o] & 0xF0) | 0x08;
        }
    }

    // Touchpad: the first two declared fingers carry touch1 / touch2. Pack an
    // active finger's normalized X/Y into its 4-byte block (the exact reverse of
    // the physical-side `parse_dualsense_touch`); an inactive finger gets bit 7
    // set on its first byte ("finger not touching"), which is what a real Sony
    // pad sends and what Steam expects (a zeroed block reads as a stuck touch at
    // 0,0). Any fingers beyond the two we model stay inactive.
    let touches = [&state.touch1, &state.touch2];
    for (i, &finger_off) in ext.touch_fingers.iter().enumerate() {
        let t = touches.get(i).copied();
        match t {
            Some(t) if t.active => write_dualsense_touch(report, finger_off, t.x, t.y, i as u8),
            _ => {
                if finger_off < report.len() {
                    report[finger_off] |= 0x80;
                }
            }
        }
    }
}

/// Pack one active touchpad finger into the 4-byte `dualsense_touch_point` block
/// at `off` — the inverse of `flexinput_devices::gyro::parse_dualsense_touch`.
/// Byte layout: `[contact, x_lo, (y_lo<<4)|x_hi, y_hi]`, X 12-bit 0..1919, Y
/// 12-bit 0..1079. `contact` low 7 bits carry a finger id; bit 7 = inactive (we
/// clear it for an active finger). `x`/`y` are normalized roughly `[-1, 1]` with
/// centre 0 (matching the read side), `+y` = up (touchpad Y increases downward).
fn write_dualsense_touch(report: &mut [u8], off: usize, x: f32, y: f32, id: u8) {
    if off + 4 > report.len() {
        return;
    }
    const HALF_W: f32 = 1920.0 / 2.0;
    const HALF_H: f32 = 1080.0 / 2.0;
    let raw_x = (x * HALF_W + HALF_W).round().clamp(0.0, 1919.0) as u16;
    let raw_y = (-y * HALF_H + HALF_H).round().clamp(0.0, 1079.0) as u16;
    report[off] = id & 0x7F; // active: bit 7 clear; low bits = finger id
    report[off + 1] = (raw_x & 0xFF) as u8; // x_lo
    report[off + 2] = (((raw_y & 0x0F) << 4) | ((raw_x >> 8) & 0x0F)) as u8; // y_lo|x_hi
    report[off + 3] = ((raw_y >> 4) & 0xFF) as u8; // y_hi
}

fn is_axis_usage(u: u16) -> bool {
    matches!(
        u,
        usage::X | usage::Y | usage::Z | usage::RX | usage::RY | usage::RZ
    )
}

/// Accumulates FlexInput sink-pin writes into a [`GamepadState`].
///
/// Bridges FlexInput's signal conventions to the encoder's:
/// - Sticks arrive as `-1.0..1.0` (0 = center) and are mapped to `0.0..1.0`
///   (0.5 = center). Y is **inverted** — FlexInput up is +1, HID up is the low
///   end of the axis (matches the ViGEm DS4 backend's `float_to_*` convention).
/// - Triggers arrive as `0.0..1.0` (0 = released) and pass through.
/// - Buttons and d-pad directions are booleans.
///
/// Call [`set`](Self::set) per pin during `VirtualDevice::send`, then
/// [`state`](Self::state) in `flush` to get the encodable snapshot.
#[derive(Debug, Clone)]
pub struct PinState {
    lx: f32,
    ly: f32,
    rx: f32,
    ry: f32,
    lt: f32,
    rt: f32,
    buttons: u32,
    dpad_up: bool,
    dpad_down: bool,
    dpad_left: bool,
    dpad_right: bool,
    gyro_x: f32,
    gyro_y: f32,
    gyro_z: f32,
    accel_x: f32,
    accel_y: f32,
    accel_z: f32,
    t1x: f32,
    t1y: f32,
    t1_active: bool,
    t2x: f32,
    t2y: f32,
    t2_active: bool,
}

impl Default for PinState {
    fn default() -> Self {
        // Centered sticks, released triggers, nothing pressed.
        PinState {
            lx: 0.0,
            ly: 0.0,
            rx: 0.0,
            ry: 0.0,
            lt: 0.0,
            rt: 0.0,
            buttons: 0,
            dpad_up: false,
            dpad_down: false,
            dpad_left: false,
            dpad_right: false,
            gyro_x: 0.0,
            gyro_y: 0.0,
            gyro_z: 0.0,
            accel_x: 0.0,
            accel_y: 0.0,
            accel_z: 0.0,
            t1x: 0.0,
            t1y: 0.0,
            t1_active: false,
            t2x: 0.0,
            t2y: 0.0,
            t2_active: false,
        }
    }
}

impl PinState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset to neutral (sticks centered, triggers released, no buttons).
    pub fn reset(&mut self) {
        *self = PinState::default();
    }

    /// Apply one FlexInput pin write. `f` is the float view of the signal
    /// (`as_float`), `b` the bool view (`as_bool`) — callers pass both so the
    /// pin decides which to use. Vec2 pins are decomposed by the caller into
    /// the `_x`/`_y` variants. Unknown pins are ignored.
    pub fn set(&mut self, pin: &str, f: f32, b: bool) {
        match pin {
            "left_stick_x" => self.lx = f,
            "left_stick_y" => self.ly = f,
            "right_stick_x" => self.rx = f,
            "right_stick_y" => self.ry = f,
            "left_trigger" => self.lt = f,
            "right_trigger" => self.rt = f,
            "dpad_up" => self.dpad_up = b,
            "dpad_down" => self.dpad_down = b,
            "dpad_left" => self.dpad_left = b,
            "dpad_right" => self.dpad_right = b,
            "gyro_x" => self.gyro_x = f,
            "gyro_y" => self.gyro_y = f,
            "gyro_z" => self.gyro_z = f,
            "accel_x" => self.accel_x = f,
            "accel_y" => self.accel_y = f,
            "accel_z" => self.accel_z = f,
            "touch1_x" => self.t1x = f,
            "touch1_y" => self.t1y = f,
            "touch1_active" => self.t1_active = b,
            "touch2_x" => self.t2x = f,
            "touch2_y" => self.t2y = f,
            "touch2_active" => self.t2_active = b,
            // Digital L2/R2: set the descriptor's digital-trigger button bit. A
            // real DS4 also drives the analog axis to full on a digital press, so
            // mirror it to the analog trigger *only* when no analog source already
            // drove it (the AutoMap digital→analog bridge, or a real analog
            // trigger, sets `lt`/`rt` first). Without the mirror a digital-only
            // upstream (Switch Pro ZL/ZR) would click the button but leave the
            // analog axis at rest.
            "btn_lt_dig" => {
                if b { self.buttons |= button::LT_DIGITAL; } else { self.buttons &= !button::LT_DIGITAL; }
                if b && self.lt == 0.0 { self.lt = 1.0; }
            }
            "btn_rt_dig" => {
                if b { self.buttons |= button::RT_DIGITAL; } else { self.buttons &= !button::RT_DIGITAL; }
                if b && self.rt == 0.0 { self.rt = 1.0; }
            }
            _ => {
                if let Some(bit) = ds_button_bit(pin) {
                    if b {
                        self.buttons |= bit;
                    } else {
                        self.buttons &= !bit;
                    }
                }
            }
        }
    }

    /// Snapshot as an encodable [`GamepadState`]. Maps stick `-1..1` → `0..1`
    /// (Y inverted) and folds d-pad booleans into the hat.
    pub fn state(&self) -> GamepadState {
        let to_unsigned = |v: f32| ((v.clamp(-1.0, 1.0) + 1.0) * 0.5).clamp(0.0, 1.0);
        GamepadState {
            left_stick_x: to_unsigned(self.lx),
            left_stick_y: to_unsigned(-self.ly),
            right_stick_x: to_unsigned(self.rx),
            right_stick_y: to_unsigned(-self.ry),
            left_trigger: self.lt.clamp(0.0, 1.0),
            right_trigger: self.rt.clamp(0.0, 1.0),
            buttons: self.buttons,
            hat: Hat::from_dpad(self.dpad_up, self.dpad_down, self.dpad_left, self.dpad_right),
            // Inverse of the physical DS4/DualSense IMU decode (see
            // flexinput_devices::gyro::build): the physical side reads wire
            // (pitch,yaw,roll)/(side,vertical,fwd) and remaps to the standard
            // signal-graph (roll,pitch,yaw)/(side,fwd,vertical). We invert so a
            // virtual pad's wire report matches what a real pad would emit:
            //   gyroPitch = gyro_y, gyroYaw = -gyro_z, gyroRoll = gyro_x
            //   accelX = accel_x, accelY(vertical) = accel_z, accelZ(fwd) = accel_y
            gyro_pitch: self.gyro_y,
            gyro_yaw: -self.gyro_z,
            gyro_roll: self.gyro_x,
            accel_x: self.accel_x,
            accel_y: self.accel_z,
            accel_z: self.accel_y,
            // Touchpad passes through unchanged (already in the read-side's
            // normalized [-1,1] range); the encoder packs it into finger bytes.
            touch1: TouchOut { active: self.t1_active, x: self.t1x, y: self.t1y },
            touch2: TouchOut { active: self.t2_active, x: self.t2x, y: self.t2y },
        }
    }
}

/// FlexInput DS4/DualSense button pin id → HMButton bit.
pub fn ds_button_bit(pin: &str) -> Option<u32> {
    Some(match pin {
        "btn_south" => button::A,
        "btn_east" => button::B,
        "btn_west" => button::X,
        "btn_north" => button::Y,
        "btn_lb" => button::LEFT_BUMPER,
        "btn_rb" => button::RIGHT_BUMPER,
        "btn_back" => button::BACK,
        "btn_start" => button::START,
        "btn_ls" => button::LEFT_STICK,
        "btn_rs" => button::RIGHT_STICK,
        "btn_guide" => button::GUIDE,
        "btn_touchpad" => button::TOUCHPAD,
        "btn_mute" => button::MUTE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::presets::DUALSHOCK_4_V2_JSON;

    fn ds4() -> Profile {
        Profile::from_json(DUALSHOCK_4_V2_JSON).unwrap()
    }

    // Switch Pro → DS4 over AutoMap drives the DS4 sink's digital trigger pins
    // (`btn_lt_dig`/`btn_rt_dig`) because Switch Pro's ZL/ZR are digital-only.
    // The DS4 descriptor carries an L2/R2 *digital* button bit (descriptor button
    // 6/7 → byte 6 bits 2/3) AND a separate L2/R2 *analog* axis (byte 8/9). A real
    // pad asserts both on a full press, so the virtual one must too — otherwise the
    // press lands on neither (the dead-end this guards against).
    #[test]
    fn ds4_digital_triggers_set_button_bit_and_analog() {
        let p = ds4();
        let neutral = encode_report(&p, &GamepadState::neutral());

        let mut ps = PinState::new();
        ps.set("btn_lt_dig", 0.0, true);
        let rep = encode_report(&p, &ps.state());
        assert_eq!(rep[6] & 0x04, 0x04, "L2 digital → byte6 bit2");
        assert_eq!(rep[8], 0xff, "L2 digital mirrors analog L2 → byte8 full");

        let mut ps = PinState::new();
        ps.set("btn_rt_dig", 0.0, true);
        let rep = encode_report(&p, &ps.state());
        assert_eq!(rep[6] & 0x08, 0x08, "R2 digital → byte6 bit3");
        assert_eq!(rep[9], 0xff, "R2 digital mirrors analog R2 → byte9 full");

        // Released digital trigger leaves both at rest.
        let mut ps = PinState::new();
        ps.set("btn_lt_dig", 0.0, false);
        let rep = encode_report(&p, &ps.state());
        assert_eq!(rep[6], neutral[6], "released → byte6 unchanged");
        assert_eq!(rep[8], neutral[8], "released → analog L2 at rest");
    }

    // When a real analog trigger value is present, the digital press must NOT
    // clobber it — the analog source wins, the digital bit still asserts.
    #[test]
    fn ds4_analog_trigger_survives_digital_press() {
        let p = ds4();
        let mut ps = PinState::new();
        ps.set("left_trigger", 0.5, true); // real analog half-press
        ps.set("btn_lt_dig", 0.0, true);   // digital also pressed
        let rep = encode_report(&p, &ps.state());
        assert_eq!(rep[6] & 0x04, 0x04, "digital bit still set");
        // 0.5 over [0,255] → ~127/128, not 255.
        assert!((rep[8] as i32 - 128).abs() <= 2, "analog half-press preserved, got {}", rep[8]);
    }

    #[test]
    fn neutral_report_centers_sticks() {
        let p = ds4();
        let rep = encode_report(&p, &GamepadState::neutral());
        assert_eq!(rep.len(), 64);
        assert_eq!(rep[0], 0x01); // Report ID
        // X,Y,Z,Rz at bytes 1..5 (after RID) — all centered ≈ 0x80 (127 or 128).
        for byte in &rep[1..5] {
            assert!((*byte as i32 - 128).abs() <= 1, "axis not centered: {byte}");
        }
    }

    #[test]
    fn left_stick_full_right_writes_x_max() {
        let p = ds4();
        let mut st = GamepadState::neutral();
        st.left_stick_x = 1.0;
        let rep = encode_report(&p, &st);
        // X is usage 0x30 at bit 0 → byte index 1 (after RID byte).
        assert_eq!(rep[1], 255);
    }

    #[test]
    fn triggers_map_to_rx_ry_via_axismap() {
        let p = ds4();
        let mut st = GamepadState::neutral();
        st.left_trigger = 1.0;
        let rep = encode_report(&p, &st);
        // axisMap: 0x33 (Rx) = leftTrigger → bit 56 → byte index 7 (+RID).
        let rx = p.field(0x01, usage::RX).unwrap();
        let byte_idx = (rx.bit_offset / 8) as usize + 1; // +1 for RID byte
        assert_eq!(rep[byte_idx], 255, "left trigger should drive Rx to max");
    }

    #[test]
    fn cross_button_sets_descriptor_button_via_map() {
        let p = ds4();
        // buttonMap[0] (A/Cross) → descriptor button index 1 → Btn2 at bit 37.
        let mut st = GamepadState::neutral();
        st.buttons = button::A;
        let rep = encode_report(&p, &st);
        // bit 37 → byte (37/8=4)+RID=5, bit (37%8=5).
        assert_eq!(rep[5] & (1 << 5), 1 << 5, "Cross should set Btn2 (bit 37)");
    }

    #[test]
    fn pinstate_neutral_is_centered() {
        let ps = PinState::new();
        let st = ps.state();
        assert!((st.left_stick_x - 0.5).abs() < 1e-6);
        assert!((st.left_stick_y - 0.5).abs() < 1e-6);
        assert_eq!(st.left_trigger, 0.0);
        assert_eq!(st.buttons, 0);
        assert_eq!(st.hat, Hat::Neutral);
    }

    #[test]
    fn pinstate_maps_sticks_and_inverts_y() {
        let mut ps = PinState::new();
        ps.set("left_stick_x", 1.0, true); // full right
        ps.set("left_stick_y", 1.0, true); // FlexInput "up" (+1)
        let st = ps.state();
        assert!((st.left_stick_x - 1.0).abs() < 1e-6, "x full right → 1.0");
        // Y inverted: FlexInput up (+1) → HID low end (0.0).
        assert!((st.left_stick_y - 0.0).abs() < 1e-6, "y up → 0.0 (inverted)");
    }

    #[test]
    fn pinstate_buttons_and_dpad() {
        let mut ps = PinState::new();
        ps.set("btn_south", 1.0, true);
        ps.set("dpad_up", 1.0, true);
        let st = ps.state();
        assert_eq!(st.buttons, button::A);
        assert_eq!(st.hat, Hat::North);
    }

    #[test]
    fn ds4_dpad_encodes_standard_hat_octants() {
        // Regression: d-pad → hat must produce the standard DS4 octant codes
        // (neutral=8, up=0, right=2, down=4, left=6) at the hat nibble.
        let p = ds4();
        let hat = p.report.field(0x01, 0x39).unwrap();
        let byte = ((hat.bit_offset + 8) / 8) as usize; // RID-inclusive
        let octant = |up, down, left, right| {
            let mut ps = PinState::new();
            ps.set("dpad_up", 0.0, up);
            ps.set("dpad_down", 0.0, down);
            ps.set("dpad_left", 0.0, left);
            ps.set("dpad_right", 0.0, right);
            encode_report(&p, &ps.state())[byte] & 0x0F
        };
        assert_eq!(octant(false, false, false, false), 8, "neutral");
        assert_eq!(octant(true, false, false, false), 0, "up");
        assert_eq!(octant(false, false, false, true), 2, "right");
        assert_eq!(octant(false, true, false, false), 4, "down");
        assert_eq!(octant(false, false, true, false), 6, "left");
    }

    #[test]
    fn dualsense_parses_imu_and_touch_layout() {
        let p = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        let e = &p.extended;
        assert!(e.has_gyro(), "DualSense declares gyro");
        assert!(e.has_accel(), "DualSense declares accel");
        // From the preset: gyroPitch@16, gyroYaw@18, gyroRoll@20, accelX@22…
        assert_eq!(e.gyro_pitch, Some(16));
        assert_eq!(e.gyro_yaw, Some(18));
        assert_eq!(e.gyro_roll, Some(20));
        assert_eq!(e.accel_x, Some(22));
        // Two touchpad fingers at bytes 33 and 37.
        assert_eq!(e.touch_fingers, vec![33, 37]);
    }

    // The DS4 IMU/touchpad byte offsets MUST match the real DualShock 4 USB report
    // 0x01 layout that Steam (and our own parse_ds4 read-back) decode at:
    //   timestamp@10-11, battery@12, gyro@13-18, accel@19-24, touch fingers@35,39.
    // The vendored profile originally had these +2 (IMU) / +3 (touch) too high,
    // which shifted every IMU value one int16 slot — pitch landed on yaw, yaw on
    // roll, roll on accelX, etc., and the first slot read empty. Cross-checked
    // against parse_ds4's go=13/ao=19 in flexinput_devices::gyro.
    #[test]
    fn ds4_imu_offsets_match_real_report() {
        let e = &ds4().extended;
        assert_eq!(e.gyro_pitch, Some(13), "gyroPitch@13 (was wrongly 15)");
        assert_eq!(e.gyro_yaw,   Some(15));
        assert_eq!(e.gyro_roll,  Some(17));
        assert_eq!(e.accel_x,    Some(19));
        assert_eq!(e.accel_y,    Some(21));
        assert_eq!(e.accel_z,    Some(23));
        assert_eq!(e.touch_fingers, vec![35, 39], "touch fingers@35,39 (were 38,42)");
    }

    // End-to-end: a wire gyro signal must encode into the exact byte the real DS4
    // carries that axis at, so Steam reads it on the right axis. gyro_y(wire pitch)
    // → raw gyroPitch slot → byte 13.
    #[test]
    fn ds4_gyro_pitch_encodes_to_byte_13() {
        let p = ds4();
        let mut ps = PinState::new();
        ps.set("gyro_y", 1.0, true); // wire pitch = full
        let rep = encode_report(&p, &ps.state());
        // PinState maps wire gyro_y → raw gyro_pitch; +1.0 → +32767 at byte 13.
        assert_eq!(i16::from_le_bytes([rep[13], rep[14]]), 32767, "wire pitch → byte13");
        // The old (wrong) slot 15 must now be untouched by pitch alone.
        assert_eq!(i16::from_le_bytes([rep[15], rep[16]]), 0, "byte15 (yaw slot) stays 0");
    }

    #[test]
    fn motor_offsets_differ_ds4_vs_dualsense() {
        // DS4 output report (RID 0x05): rightMotor@4, leftMotor@5.
        let ds4 = ds4();
        assert_eq!(ds4.extended.out_right_motor, Some(4));
        assert_eq!(ds4.extended.out_left_motor, Some(5));
        // DualSense output report (RID 0x02): rightMotor@3, leftMotor@4.
        let ds = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        assert_eq!(ds.extended.out_right_motor, Some(3));
        assert_eq!(ds.extended.out_left_motor, Some(4));
    }

    // The DualSense mic-mute button must reach the report on its own descriptor
    // button (Button 15 / buttons[14]) without disturbing the touchpad-click
    // button (Button 14). Both are real buttons a game/Steam reads; before this
    // fix `btn_mute` was silently dropped (no HMButton bit, no buttonMap entry).
    #[test]
    fn dualsense_mute_button_sets_its_own_bit() {
        let p = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        let buttons: Vec<_> = p.report.fields.iter().filter(|f| f.usage_page == 0x09).collect();
        assert!(buttons.len() >= 15, "DualSense descriptor declares 15 buttons");
        let mute_f = buttons[14]; // Button 15
        let touch_f = buttons[13]; // Button 14 (touchpad click)

        // Mute alone → its bit set, touchpad-click bit clear.
        let mut ps = PinState::new();
        ps.set("btn_mute", 0.0, true);
        let rep = encode_report(&p, &ps.state());
        let bit = |f: &InputField, r: &[u8]| {
            let byte = (f.bit_offset as usize + 8) / 8; // RID-inclusive
            (r[byte] >> ((f.bit_offset as usize + 8) % 8)) & 1
        };
        assert_eq!(bit(mute_f, &rep), 1, "mute sets Button 15");
        assert_eq!(bit(touch_f, &rep), 0, "mute does not set touchpad-click");

        // Touchpad-click alone → touchpad bit set, mute clear (no cross-talk).
        let mut ps = PinState::new();
        ps.set("btn_touchpad", 0.0, true);
        let rep = encode_report(&p, &ps.state());
        assert_eq!(bit(touch_f, &rep), 1, "touchpad sets Button 14");
        assert_eq!(bit(mute_f, &rep), 0, "touchpad does not set mute");
    }

    #[test]
    fn dualsense_neutral_stamps_touchpad_inactive() {
        let p = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        let rep = encode_report(&p, &GamepadState::neutral());
        // Inactive sentinel: bit 7 set on each finger's first byte.
        assert_eq!(rep[33] & 0x80, 0x80, "finger 0 marked inactive");
        assert_eq!(rep[37] & 0x80, 0x80, "finger 1 marked inactive");
    }

    // The virtual pad must always report a full/healthy battery so hosts keep
    // haptics enabled. DualSense battery byte 53 low nibble = level (8 = full).
    #[test]
    fn dualsense_reports_full_battery() {
        let p = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        assert_eq!(p.extended.battery_level, Some(53), "battery@53 parsed");
        let rep = encode_report(&p, &GamepadState::neutral());
        assert_eq!(rep[53] & 0x0F, 0x08, "battery level = full (8)");
    }

    // DS4 carries battery at byte 12 (plain uint8). Same full-charge stamp.
    #[test]
    fn ds4_reports_full_battery() {
        let p = ds4();
        assert_eq!(p.extended.battery_level, Some(12), "battery@12 parsed");
        let rep = encode_report(&p, &GamepadState::neutral());
        assert_eq!(rep[12] & 0x0F, 0x08, "battery level healthy/full");
    }

    /// Decode a 4-byte DualSense touch block back to (active, raw_x, raw_y) — the
    /// exact inverse used by the physical-side parser — to verify the encoder's
    /// bit packing round-trips.
    fn decode_touch(block: &[u8]) -> (bool, u16, u16) {
        let active = (block[0] & 0x80) == 0;
        let x_lo = block[1] as u16;
        let mid = block[2] as u16;
        let y_hi = block[3] as u16;
        let raw_x = ((mid & 0x0F) << 8) | x_lo;
        let raw_y = (y_hi << 4) | ((mid & 0xF0) >> 4);
        (active, raw_x, raw_y)
    }

    #[test]
    fn dualsense_active_touch_packs_and_round_trips() {
        let p = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        let mut ps = PinState::new();
        // Finger 1 at centre (0,0); finger 2 near top-right (+0.5, +0.5).
        ps.set("touch1_active", 0.0, true);
        ps.set("touch1_x", 0.0, false);
        ps.set("touch1_y", 0.0, false);
        ps.set("touch2_active", 0.0, true);
        ps.set("touch2_x", 0.5, false);
        ps.set("touch2_y", 0.5, false);
        let rep = encode_report(&p, &ps.state());

        // Finger 1 @33: active, centre → raw (960, 540).
        let (a1, x1, y1) = decode_touch(&rep[33..37]);
        assert!(a1, "finger 1 active (bit7 clear)");
        assert_eq!((x1, y1), (960, 540), "centre maps to mid-sensor");

        // Finger 2 @37: active, (+0.5,+0.5). raw_x = 0.5*960+960 = 1440;
        // raw_y = -0.5*540+540 = 270 (Y inverted: +y is up).
        let (a2, x2, y2) = decode_touch(&rep[37..41]);
        assert!(a2, "finger 2 active");
        assert_eq!((x2, y2), (1440, 270));
    }

    #[test]
    fn dualsense_inactive_finger_ignores_position() {
        // A finger with active=false must stamp the sentinel regardless of x/y.
        let p = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        let mut ps = PinState::new();
        ps.set("touch1_x", 0.9, false);
        ps.set("touch1_y", -0.9, false);
        // active left false
        let rep = encode_report(&p, &ps.state());
        assert_eq!(rep[33] & 0x80, 0x80, "inactive finger 1 keeps sentinel");
    }

    #[test]
    fn dualsense_gyro_accel_encode_to_int16() {
        let p = Profile::from_json(crate::profile::presets::DUALSENSE_JSON).unwrap();
        let mut st = GamepadState::neutral();
        // Full positive pitch → gyroPitch@16 = +32767 (0x7FFF LE).
        st.gyro_pitch = 1.0;
        // Full positive accel x → accelX@22 = +32767.
        st.accel_x = 1.0;
        let rep = encode_report(&p, &st);
        assert_eq!(i16::from_le_bytes([rep[16], rep[17]]), 32767, "gyro pitch max");
        assert_eq!(i16::from_le_bytes([rep[22], rep[23]]), 32767, "accel x max");
    }

    #[test]
    fn pinstate_gyro_remaps_to_wire_semantics() {
        let mut ps = PinState::new();
        ps.set("gyro_x", 0.5, true); // roll
        ps.set("gyro_z", 0.25, true); // yaw
        let st = ps.state();
        assert_eq!(st.gyro_roll, 0.5, "gyro_x → wire roll");
        assert_eq!(st.gyro_yaw, -0.25, "gyro_z → wire yaw (negated)");
    }

    // Locks the GIP layout mapped live against XInputGetState (2026-06-15). If
    // someone "fixes" gip_from_state to XINPUT_GAMEPAD order, these break.
    #[test]
    fn gip_neutral_centers_sticks_and_clears_rest() {
        let g = gip_from_state(&GamepadState::neutral());
        // Sticks centered at 0x8000 (unsigned).
        assert_eq!(u16::from_le_bytes([g[0], g[1]]), 0x8000, "LX center");
        assert_eq!(u16::from_le_bytes([g[2], g[3]]), 0x8000, "LY center");
        assert_eq!(u16::from_le_bytes([g[4], g[5]]), 0x8000, "RX center");
        assert_eq!(u16::from_le_bytes([g[6], g[7]]), 0x8000, "RY center");
        assert_eq!(g[8], 0, "LT released");
        assert_eq!(g[10], 0, "RT released");
        assert_eq!(u16::from_le_bytes([g[12], g[13]]), 0, "no buttons");
    }

    #[test]
    fn gip_buttons_use_companion_order() {
        let bit = |b: u32| {
            let mut st = GamepadState::neutral();
            st.buttons = b;
            let g = gip_from_state(&st);
            u16::from_le_bytes([g[12], g[13]])
        };
        assert_eq!(bit(button::A), 0x0001);
        assert_eq!(bit(button::B), 0x0002);
        assert_eq!(bit(button::X), 0x0004);
        assert_eq!(bit(button::Y), 0x0008);
        assert_eq!(bit(button::LEFT_BUMPER), 0x0010);
        assert_eq!(bit(button::RIGHT_BUMPER), 0x0020);
        assert_eq!(bit(button::LEFT_STICK), 0x0040);
        assert_eq!(bit(button::RIGHT_STICK), 0x0080);
        assert_eq!(bit(button::BACK), 0x0100);
        assert_eq!(bit(button::START), 0x0200);
    }

    // Triggers are a u16 the companion scales by ~1/4: full press → 1020 (0x03FC),
    // so its >>2 yields ~255. (Live-verified: 0x03FC→LT=254, 0x0FFF→255.)
    #[test]
    fn gip_triggers_are_u16_full_scale() {
        let mut st = GamepadState::neutral();
        st.left_trigger = 1.0;
        st.right_trigger = 1.0;
        let g = gip_from_state(&st);
        assert_eq!(u16::from_le_bytes([g[8], g[9]]), 1020, "LT full → 1020");
        assert_eq!(u16::from_le_bytes([g[10], g[11]]), 1020, "RT full → 1020");
        // Half press ~510.
        let mut st = GamepadState::neutral();
        st.left_trigger = 0.5;
        let g = gip_from_state(&st);
        assert_eq!(u16::from_le_bytes([g[8], g[9]]), 510, "LT half → 510");
    }

    #[test]
    fn gip_dpad_is_octant_nibble() {
        let oct = |h: Hat| {
            let mut st = GamepadState::neutral();
            st.hat = h;
            let g = gip_from_state(&st);
            u16::from_le_bytes([g[12], g[13]]) >> 10
        };
        assert_eq!(oct(Hat::Neutral), 0);
        assert_eq!(oct(Hat::North), 1, "Up → octant 1 (0x0400)");
        assert_eq!(oct(Hat::SouthEast), 4, "Down+Right → octant 4 (0x1000)");
        assert_eq!(oct(Hat::NorthWest), 8, "Up+Left → octant 8 (0x2000)");
    }

    #[test]
    fn gip_full_right_stick_is_max() {
        let mut st = GamepadState::neutral();
        st.left_stick_x = 1.0;
        let g = gip_from_state(&st);
        assert_eq!(u16::from_le_bytes([g[0], g[1]]), 0xFFFF, "LX full right → 0xFFFF");
    }

    // All four stick axes pass straight through (GamepadState Y is already
    // up-counting from PinState's single flip; no second flip in the GIP encoder).
    #[test]
    fn gip_axes_pass_straight_through() {
        let mut st = GamepadState::neutral();
        st.left_stick_y = 1.0; // up
        st.right_stick_y = 1.0;
        let g = gip_from_state(&st);
        assert_eq!(u16::from_le_bytes([g[2], g[3]]), 0xFFFF, "LY up → 0xFFFF");
        assert_eq!(u16::from_le_bytes([g[6], g[7]]), 0xFFFF, "RY up → 0xFFFF");
        st = GamepadState::neutral();
        st.left_stick_x = 1.0;
        let g = gip_from_state(&st);
        assert_eq!(u16::from_le_bytes([g[0], g[1]]), 0xFFFF, "LX right → 0xFFFF");
    }

    #[test]
    fn pinstate_encodes_through_profile() {
        // End-to-end: pin writes → state → DS4 report bytes.
        let p = ds4();
        let mut ps = PinState::new();
        ps.set("left_stick_x", 1.0, true); // full right → X = 0xFF
        let rep = encode_report(&p, &ps.state());
        assert_eq!(rep[1], 255, "left_stick_x=+1 should drive X to max");
    }
}
