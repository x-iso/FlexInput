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

/// Feedback pin pairs that flow BACKWARD along an AutoMap wire.
///
/// When an AutoMap wire connects a physical device.source → virtual device.sink (carrying
/// forward gamepad signals: sticks, buttons, etc.), feedback signals (rumble, lightbar,
/// LEDs) flow back along the same wire in the reverse direction. This is silent and
/// automatic — no separate wires required, no UI changes.
///
/// Each entry: (virtual_output_pin_id, &[matching_physical_input_pin_ids]).
/// The engine looks up the virtual sink's output signal under `virtual_output_pin_id`
/// and, if a matching physical haptic input pin exists, routes the value there.
///
/// To extend: add a new entry for any new device-specific haptic input pin alias.
/// Example: when a new controller adopts pin "trigger_l_rumble", add it to the
/// rumble_strong entry alongside "hd_l_amp" / "hd_rumble_l".
///
/// DualSense note: `ds_l_amp`/`ds_r_amp` are NOT listed here. DualSense already
/// exposes `rumble_strong`/`rumble_weak` (classic motors, XInput-compatible),
/// which is the right fallback when a generic patch wires `rumble_strong`.
/// To drive HD haptics, the user explicitly wires `ds_l_amp` + `ds_l_freq`
/// (and the R pair) in their patch — same shape as Switch Pro's HD rumble.
pub const FEEDBACK_PAIRS: &[(&str, &[&str])] = &[
    ("rumble_strong", &["rumble_strong", "hd_l_amp", "hd_rumble_l"]),
    ("rumble_weak",   &["rumble_weak",   "hd_r_amp", "hd_rumble_r"]),
    ("lightbar_r",    &["lightbar_r"]),
    ("lightbar_g",    &["lightbar_g"]),
    ("lightbar_b",    &["lightbar_b"]),
];

/// Resolve feedback pin-id mapping for the AutoMap reverse-flow channel.
/// Given a virtual sink's output pin id (e.g. "rumble_strong"), returns the
/// first matching pin from `physical_input_pins` (e.g. "hd_l_amp" on Switch Pro).
pub fn resolve_feedback_pin<'a>(
    virtual_out_pin: &str,
    physical_input_pins: &[&'a str],
) -> Option<&'a str> {
    let entry = FEEDBACK_PAIRS.iter().find(|(src, _)| *src == virtual_out_pin)?;
    for &candidate in entry.1 {
        if let Some(&p) = physical_input_pins.iter().find(|&&p| p == candidate) {
            return Some(p);
        }
    }
    None
}

/// A single auto-mappable signal in the canonical gamepad bus.
pub struct AutoMapPin {
    pub id: &'static str,
    pub display_name: &'static str,
    pub signal_type: SignalType,
}

use crate::SignalType;

/// Every haptic / feedback INPUT port across all controller families — the
/// universal set of injection targets exposed as inlets by the Feedback Control
/// module. This is the union of all `inputs_for(kind)` families in
/// `crates/devices/src/layouts.rs`. When injected onto a pad that lacks a given
/// port, the value is silently dropped (the pin simply isn't in the device's
/// haptic-input list — see the engine feedback pass / `resolve_feedback_pin`).
///
/// Keep this in sync with `layouts.rs`; `feedback_inlet_union_covers_all_families`
/// in `crates/devices` (or the core test below) guards against drift.
pub const FEEDBACK_INLET_PINS: &[AutoMapPin] = &[
    // ── Classic rumble (XInput-compatible; every pad) ─────────────────────────
    AutoMapPin { id: "rumble_strong", display_name: "Rumble (strong)",         signal_type: SignalType::Float },
    AutoMapPin { id: "rumble_weak",   display_name: "Rumble (weak)",           signal_type: SignalType::Float },
    // ── Light bar (DS4 / DualSense) ───────────────────────────────────────────
    AutoMapPin { id: "lightbar_r",    display_name: "Light Bar R",             signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_g",    display_name: "Light Bar G",             signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_b",    display_name: "Light Bar B",             signal_type: SignalType::Float },
    // ── Switch Pro HD rumble (per-side amp + carrier freq) ────────────────────
    AutoMapPin { id: "hd_l_amp",      display_name: "HD Rumble L: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_l_freq",     display_name: "HD Rumble L: Frequency",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_r_amp",      display_name: "HD Rumble R: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_r_freq",     display_name: "HD Rumble R: Frequency",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_rumble_l",   display_name: "HD Rumble L (legacy)",    signal_type: SignalType::Float },
    AutoMapPin { id: "hd_rumble_r",   display_name: "HD Rumble R (legacy)",    signal_type: SignalType::Float },
    // ── DualSense HD haptics (USB only) ───────────────────────────────────────
    AutoMapPin { id: "ds_l_amp",      display_name: "DS Haptic L: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "ds_l_freq",     display_name: "DS Haptic L: Frequency",  signal_type: SignalType::Float },
    AutoMapPin { id: "ds_r_amp",      display_name: "DS Haptic R: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "ds_r_freq",     display_name: "DS Haptic R: Frequency",  signal_type: SignalType::Float },
    // ── DualSense LEDs ────────────────────────────────────────────────────────
    AutoMapPin { id: "player_led",    display_name: "Player LED",              signal_type: SignalType::Float },
    AutoMapPin { id: "mic_led",       display_name: "Mic LED",                 signal_type: SignalType::Float },
    // ── DualSense adaptive triggers ───────────────────────────────────────────
    AutoMapPin { id: "trigger_r_mode",     display_name: "R.Trigger Mode",     signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_start",    display_name: "R.Trigger Start",    signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_end",      display_name: "R.Trigger End",      signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_strength", display_name: "R.Trigger Strength", signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_freq",     display_name: "R.Trigger Freq",     signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_mode",     display_name: "L.Trigger Mode",     signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_start",    display_name: "L.Trigger Start",    signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_end",      display_name: "L.Trigger End",      signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_strength", display_name: "L.Trigger Strength", signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_freq",     display_name: "L.Trigger Freq",     signal_type: SignalType::Float },
];

/// Basic feedback OUTPUT taps the game is requesting on the virtual destination
/// device — exposed as outlets by the Feedback Control module. This is the
/// subset that virtual source-pins actually publish via `poll_outputs`
/// (`*_SOURCE_PINS` in `crates/virtual/src/layouts.rs`): classic rumble plus
/// the DS4/DualSense light bar.
pub const FEEDBACK_OUTLET_PINS: &[AutoMapPin] = &[
    AutoMapPin { id: "rumble_strong", display_name: "Rumble (strong)", signal_type: SignalType::Float },
    AutoMapPin { id: "rumble_weak",   display_name: "Rumble (weak)",   signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_r",    display_name: "Light Bar R",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_g",    display_name: "Light Bar G",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_b",    display_name: "Light Bar B",     signal_type: SignalType::Float },
];

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
    AutoMapPin { id: "btn_south",     display_name: "South",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_east",      display_name: "East",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_west",      display_name: "West",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_north",     display_name: "North",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lb",        display_name: "LB",                   signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rb",        display_name: "RB",                   signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lt_dig",    display_name: "LT (dig)",             signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rt_dig",    display_name: "RT (dig)",             signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_ls",        display_name: "L.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rs",        display_name: "R.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_start",     display_name: "Start",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_back",      display_name: "Back",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_guide",     display_name: "Guide",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_capture",   display_name: "Capture",              signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_mute",      display_name: "Mute",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_touchpad",  display_name: "Touchpad Click",       signal_type: SignalType::Bool },
    AutoMapPin { id: "touch1_x",      display_name: "Touch 1 X",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch1_y",      display_name: "Touch 1 Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch1_active", display_name: "Touch 1 Active",       signal_type: SignalType::Bool },
    AutoMapPin { id: "touch2_x",      display_name: "Touch 2 X",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch2_y",      display_name: "Touch 2 Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch2_active", display_name: "Touch 2 Active",       signal_type: SignalType::Bool },
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
    AutoMapPin { id: "mouse",         display_name: "Mouse: XY (delta)",    signal_type: SignalType::Vec2 },
    AutoMapPin { id: "mouse_x",       display_name: "Mouse: X (delta)",     signal_type: SignalType::Float },
    AutoMapPin { id: "mouse_y",       display_name: "Mouse: Y (delta)",     signal_type: SignalType::Float },
];

/// Family-specific button glyph for a cross-family pin, or `None` if the pin
/// is family-neutral. `family_slug` is the middle segment of a gilrs device ID
/// (e.g. `"dualsense"` from `"gilrs:dualsense:0"`). Pass `None` / unknown to get
/// the neutral positional label embedded in [`ALL_PINS`] (e.g. "South").
///
/// Used by the AutoMap Splitter body to swap "South (B/Cross/X)" for the single
/// label that matches the actually-connected upstream device.
pub fn family_label(pin_id: &str, family_slug: Option<&str>) -> Option<&'static str> {
    let fam = family_slug?;
    let (xi, ps, sw) = match pin_id {
        "btn_south"  => ("A", "Cross", "B"),
        "btn_east"   => ("B", "Circle", "A"),
        "btn_west"   => ("X", "Square", "Y"),
        "btn_north"  => ("Y", "Triangle", "X"),
        "btn_lb"     => ("LB", "L1", "L"),
        "btn_rb"     => ("RB", "R1", "R"),
        "btn_lt_dig" => ("LT", "L2", "ZL"),
        "btn_rt_dig" => ("RT", "R2", "ZR"),
        "btn_start"  => ("Start", "Options", "+"),
        "btn_back"   => ("Back", "Share", "−"),
        "btn_guide"  => ("Guide", "PS", "Home"),
        _ => return None,
    };
    Some(match fam {
        "xinput"                  => xi,
        "ds4" | "dualsense"       => ps,
        "switch_pro"              => sw,
        _                         => return None,
    })
}

/// Given lists of source and destination pin IDs, returns `(src_id, dst_id)` pairs
/// for every auto-mappable signal. A single source pin can map to multiple destination
/// pins when a semantic group bridges digital ↔ analog (e.g. `btn_lt_dig` fires both
/// `btn_lt_dig` AND `left_trigger` on a virtual DS4) — type coercion happens later.
pub fn resolve_mapping<'a>(src_pins: &[&'a str], dst_pins: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let mut result = Vec::new();
    let mut claimed_dst = std::collections::HashSet::new();

    // Pass 1: direct ID matches across ALL sources first. A direct match (same name on
    // both sides) is the authoritative owner of its destination and must not be pre-empted
    // by an earlier source's semantic fan-out. Doing every direct match before any fan-out
    // makes ordering of `src_pins` irrelevant.
    for &src_id in src_pins {
        if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == src_id) {
            if claimed_dst.insert(dst_id) {
                result.push((src_id, dst_id));
            }
        }
    }

    // Pass 2: semantic group fan-out for destinations a direct match didn't claim.
    // Lets btn_capture↔btn_mute bridge when only one name exists on each side.
    for &src_id in src_pins {
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

    // Pass 3: the digital↔analog trigger bridge is special — both the analog
    // (`left_trigger`/`right_trigger`) and digital (`btn_lt_dig`/`btn_rt_dig`) source pins
    // must reach the analog destination, even though pass 1 already gave it a direct owner.
    // On controllers whose analog trigger is dead (Switch Pro ZL/ZR are digital-only, so
    // `left_trigger` is never published), the engine combines these by magnitude and the
    // live digital press wins. Without this extra mapping the digital source could never
    // drive the analog trigger. Emitted in addition to (not instead of) the direct mapping.
    for &(dig, analog) in &[("btn_lt_dig", "left_trigger"), ("btn_rt_dig", "right_trigger")] {
        let has_dig_src = src_pins.contains(&dig);
        let has_analog_dst = dst_pins.contains(&analog);
        if has_dig_src && has_analog_dst && !result.iter().any(|&(s, d)| s == dig && d == analog) {
            result.push((dig, analog));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every gamepad source node exposes both the analog trigger pin and the digital
    // trigger button pin (see gamepad::standard_outputs). A virtual gamepad sink exposes
    // both too. The analog destination must be reachable from BOTH sources so the engine
    // can pick whichever carries a live signal — critical for Switch Pro / DualSense, whose
    // ZL/ZR (or whose HID-parsed L2/R2) only populate the digital pin in some paths.
    #[test]
    fn digital_trigger_reaches_analog_destination() {
        // Order deliberately puts analog before digital, matching standard_outputs().
        let src = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
        let dst = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
        let m = resolve_mapping(&src, &dst);

        assert!(m.contains(&("btn_lt_dig", "left_trigger")), "digital LT must reach analog LT: {m:?}");
        assert!(m.contains(&("btn_rt_dig", "right_trigger")), "digital RT must reach analog RT: {m:?}");
        // Direct matches still present.
        assert!(m.contains(&("left_trigger", "left_trigger")));
        assert!(m.contains(&("btn_lt_dig", "btn_lt_dig")));
    }

    // When the source has no analog trigger pin at all (a hypothetical digital-only node),
    // the digital source must still drive the analog destination.
    #[test]
    fn digital_only_source_drives_analog() {
        let src = ["btn_lt_dig", "btn_rt_dig"];
        let dst = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
        let m = resolve_mapping(&src, &dst);
        assert!(m.contains(&("btn_lt_dig", "left_trigger")), "{m:?}");
        assert!(m.contains(&("btn_rt_dig", "right_trigger")), "{m:?}");
    }

    #[test]
    fn capture_mute_bridge_still_works() {
        let m = resolve_mapping(&["btn_capture"], &["btn_mute"]);
        assert_eq!(m, vec![("btn_capture", "btn_mute")]);
    }

    // No duplicate (src, dst) pairs — the engine would double-process them.
    #[test]
    fn no_duplicate_pairs() {
        let src = ["left_trigger", "btn_lt_dig"];
        let dst = ["left_trigger", "btn_lt_dig"];
        let mut m = resolve_mapping(&src, &dst);
        let n = m.len();
        m.sort();
        m.dedup();
        assert_eq!(m.len(), n, "duplicate pairs in resolve_mapping output");
    }
}
