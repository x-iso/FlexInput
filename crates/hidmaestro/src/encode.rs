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
}

fn is_axis_usage(u: u16) -> bool {
    matches!(
        u,
        usage::X | usage::Y | usage::Z | usage::RX | usage::RY | usage::RZ
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::presets::DUALSHOCK_4_V2_JSON;

    fn ds4() -> Profile {
        Profile::from_json(DUALSHOCK_4_V2_JSON).unwrap()
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
}
