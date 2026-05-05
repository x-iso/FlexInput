/// Semantically equivalent pin-ID groups for any remaining cross-family differences.
/// All button families now use positional names (btn_south/east/west/north, btn_lb/rb,
/// btn_ls/rs, btn_start/back/guide) so most cross-family connections work via direct
/// ID match. Only truly unique cross-device aliases stay here.
const SEMANTIC_GROUPS: &[&[&str]] = &[
    // Device-unique special buttons that share purpose across families.
    &["btn_capture", "btn_mute"],
    // Digital triggers auto-map to analog trigger pins (Bool → Float coercion applied separately).
    &["btn_lt_dig", "left_trigger"],
    &["btn_rt_dig", "right_trigger"],
];

/// A single auto-mappable signal in the canonical gamepad bus.
pub struct AutoMapPin {
    pub id: &'static str,
    pub display_name: &'static str,
    pub signal_type: SignalType,
}

use crate::SignalType;

/// Canonical list of all gamepad signals that AutoMap covers.
/// Used by AutoMap Splitter/Collector UIs and for signal resolution.
/// Order: bundled vectors first, then individual axes, then buttons.
pub const ALL_PINS: &[AutoMapPin] = &[
    AutoMapPin { id: "left_stick",    display_name: "Left Stick",           signal_type: SignalType::Vec2 },
    AutoMapPin { id: "right_stick",   display_name: "Right Stick",          signal_type: SignalType::Vec2 },
    AutoMapPin { id: "dpad",          display_name: "D-Pad",                signal_type: SignalType::Vec2 },
    AutoMapPin { id: "left_stick_x",  display_name: "L.Stick X",            signal_type: SignalType::Float },
    AutoMapPin { id: "left_stick_y",  display_name: "L.Stick Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "right_stick_x", display_name: "R.Stick X",            signal_type: SignalType::Float },
    AutoMapPin { id: "right_stick_y", display_name: "R.Stick Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "left_trigger",  display_name: "Left Trigger (analog)", signal_type: SignalType::Float },
    AutoMapPin { id: "right_trigger", display_name: "Right Trigger (analog)",signal_type: SignalType::Float },
    AutoMapPin { id: "dpad_x",        display_name: "D-Pad X",              signal_type: SignalType::Float },
    AutoMapPin { id: "dpad_y",        display_name: "D-Pad Y",              signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_x",        display_name: "Gyro X (roll)",        signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_y",        display_name: "Gyro Y (pitch)",       signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_z",        display_name: "Gyro Z (yaw)",         signal_type: SignalType::Float },
    AutoMapPin { id: "accel_x",       display_name: "Accel X",              signal_type: SignalType::Float },
    AutoMapPin { id: "accel_y",       display_name: "Accel Y",              signal_type: SignalType::Float },
    AutoMapPin { id: "accel_z",       display_name: "Accel Z",              signal_type: SignalType::Float },
    AutoMapPin { id: "btn_south",     display_name: "South (B/Cross/X)",    signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_east",      display_name: "East (A/Circle/O)",    signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_west",      display_name: "West (X/Square/Y)",    signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_north",     display_name: "North (Y/Triangle/X)", signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lb",        display_name: "LB / L1 / L",          signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rb",        display_name: "RB / R1 / R",          signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lt_dig",    display_name: "LT dig / L2 / ZL",     signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rt_dig",    display_name: "RT dig / R2 / ZR",     signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_ls",        display_name: "L.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rs",        display_name: "R.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_start",     display_name: "Start / + / Options",  signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_back",      display_name: "Back / − / Share",     signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_guide",     display_name: "Guide / Home / PS",    signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_capture",   display_name: "Capture",              signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_mute",      display_name: "Mute",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_touchpad",  display_name: "Touchpad Click",       signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_up",       display_name: "D-Pad Up",             signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_down",     display_name: "D-Pad Down",           signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_left",     display_name: "D-Pad Left",           signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_right",    display_name: "D-Pad Right",          signal_type: SignalType::Bool },
    // ── Virtual Keyboard & Mouse static pins ──────────────────────────────────
    // IDs match KEYMOUSE_DEFAULT_PINS in crates/virtual/src/layouts.rs so they
    // direct-match through resolve_mapping when wired to a virtual KB/M sink.
    AutoMapPin { id: "key_escape",    display_name: "Key: Escape",          signal_type: SignalType::Bool },
    AutoMapPin { id: "key_shift",     display_name: "Key: Shift",           signal_type: SignalType::Bool },
    AutoMapPin { id: "key_ctrl",      display_name: "Key: Ctrl",            signal_type: SignalType::Bool },
    AutoMapPin { id: "key_alt",       display_name: "Key: Alt",             signal_type: SignalType::Bool },
    AutoMapPin { id: "key_win",       display_name: "Key: Win",             signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_left",    display_name: "Mouse: LMB",           signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_right",   display_name: "Mouse: RMB",           signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_middle",  display_name: "Mouse: MMB",           signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_back",    display_name: "Mouse: Back",          signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_forward", display_name: "Mouse: Forward",       signal_type: SignalType::Bool },
    AutoMapPin { id: "scroll_up",     display_name: "Mouse: Scroll Up",     signal_type: SignalType::Bool },
    AutoMapPin { id: "scroll_down",   display_name: "Mouse: Scroll Down",   signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_x",       display_name: "Mouse: X (delta)",     signal_type: SignalType::Float },
    AutoMapPin { id: "mouse_y",       display_name: "Mouse: Y (delta)",     signal_type: SignalType::Float },
];

/// Given lists of source and destination pin IDs, returns `(src_id, dst_id)` pairs
/// for every auto-mappable signal. A single source pin can map to multiple destination
/// pins when a semantic group bridges digital ↔ analog (e.g. `btn_lt_dig` fires both
/// `btn_lt_dig` AND `left_trigger` on a virtual DS4) — type coercion happens later.
pub fn resolve_mapping<'a>(src_pins: &[&'a str], dst_pins: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let mut result = Vec::new();
    let mut claimed_dst = std::collections::HashSet::new();

    for &src_id in src_pins {
        // 1. Direct ID match — same name on both sides (always wins for that destination).
        if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == src_id) {
            if claimed_dst.insert(dst_id) {
                result.push((src_id, dst_id));
            }
        }

        // 2. Semantic group match — fan out to every other group member that exists on the
        // destination side (and isn't already claimed).  Lets btn_lt_dig drive left_trigger
        // alongside the direct btn_lt_dig→btn_lt_dig mapping, btn_capture↔btn_mute, etc.
        if let Some(group) = SEMANTIC_GROUPS.iter().find(|g| g.contains(&src_id)).copied() {
            for &group_id in group {
                if group_id == src_id { continue; }
                if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == group_id) {
                    if claimed_dst.insert(dst_id) {
                        result.push((src_id, dst_id));
                    }
                }
            }
        }
    }

    result
}
