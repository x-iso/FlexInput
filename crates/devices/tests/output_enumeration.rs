/// Integration tests for `layouts::outputs_for` and `layouts::inputs_for`.
///
/// These tests verify that each supported controller kind exposes the correct
/// feedback (haptic/rumble/lightbar) pin set and that `outputs_for` always
/// includes the `automap_out` bus pin.  No hardware is required — the functions
/// are pure data-table lookups.
use flexinput_devices::{layouts, identification::ControllerKind};

// ── automap_out presence ───────────────────────────────────────────────────────

#[test]
fn all_supported_kinds_have_automap_out_pin() {
    for kind in [
        ControllerKind::XInput,
        ControllerKind::DualShock4,
        ControllerKind::DualSense,
        ControllerKind::SwitchPro,
        ControllerKind::Generic,
    ] {
        let outputs = layouts::outputs_for(kind);
        assert!(
            outputs.iter().any(|p| p.id == "automap_out"),
            "{kind:?} outputs_for must include automap_out",
        );
    }
}

#[test]
fn midi_kinds_return_empty_outputs() {
    assert!(layouts::outputs_for(ControllerKind::MidiIn).is_empty());
    assert!(layouts::outputs_for(ControllerKind::MidiOut).is_empty());
}

// ── Feedback Control inlet union coverage ───────────────────────────────────────

/// The Feedback Control module exposes `FEEDBACK_INLET_PINS` (core) as its
/// universal injection inlets. Every haptic input pin any controller family
/// actually exposes via `inputs_for` MUST appear in that union — otherwise a
/// supported port would be silently unreachable from the node. Guards against
/// `layouts.rs` adding a haptic pin without updating the core constant.
#[test]
fn feedback_inlet_union_covers_all_input_families() {
    use flexinput_core::automap::FEEDBACK_INLET_PINS;
    let union: std::collections::HashSet<&str> =
        FEEDBACK_INLET_PINS.iter().map(|p| p.id).collect();
    for kind in [
        ControllerKind::XInput,
        ControllerKind::DualShock4,
        ControllerKind::DualSense,
        ControllerKind::SwitchPro,
        ControllerKind::Generic,
    ] {
        for pin in layouts::inputs_for(kind) {
            assert!(
                union.contains(pin.id.as_str()),
                "{kind:?} haptic input pin `{}` is missing from FEEDBACK_INLET_PINS \
                 — add it to crates/core/src/automap.rs",
                pin.id,
            );
        }
    }
}

// ── DualShock 4 ───────────────────────────────────────────────────────────────

#[test]
fn ds4_inputs_include_rumble_pins() {
    let inputs = layouts::inputs_for(ControllerKind::DualShock4);
    assert!(inputs.iter().any(|p| p.id == "rumble_strong"),
        "DS4 must expose rumble_strong");
    assert!(inputs.iter().any(|p| p.id == "rumble_weak"),
        "DS4 must expose rumble_weak");
}

#[test]
fn ds4_inputs_include_lightbar_pins() {
    let inputs = layouts::inputs_for(ControllerKind::DualShock4);
    assert!(inputs.iter().any(|p| p.id == "lightbar_r"),
        "DS4 must expose lightbar_r");
    assert!(inputs.iter().any(|p| p.id == "lightbar_g"),
        "DS4 must expose lightbar_g");
    assert!(inputs.iter().any(|p| p.id == "lightbar_b"),
        "DS4 must expose lightbar_b");
}

// ── DualSense ─────────────────────────────────────────────────────────────────

#[test]
fn dualsense_inputs_include_rumble_and_lightbar() {
    let inputs = layouts::inputs_for(ControllerKind::DualSense);
    for pin in ["rumble_strong", "rumble_weak", "lightbar_r", "lightbar_g", "lightbar_b"] {
        assert!(inputs.iter().any(|p| p.id == pin),
            "DualSense must expose {pin}");
    }
}

#[test]
fn dualsense_inputs_include_adaptive_trigger_pins() {
    let inputs = layouts::inputs_for(ControllerKind::DualSense);
    for pin in [
        "trigger_r_mode", "trigger_r_start", "trigger_r_end", "trigger_r_strength", "trigger_r_freq",
        "trigger_l_mode", "trigger_l_start", "trigger_l_end", "trigger_l_strength", "trigger_l_freq",
    ] {
        assert!(inputs.iter().any(|p| p.id == pin),
            "DualSense must expose {pin}");
    }
}

#[test]
fn dualsense_inputs_include_led_pins() {
    let inputs = layouts::inputs_for(ControllerKind::DualSense);
    assert!(inputs.iter().any(|p| p.id == "player_led"),
        "DualSense must expose player_led");
    assert!(inputs.iter().any(|p| p.id == "mic_led"),
        "DualSense must expose mic_led");
}

#[test]
fn dualsense_inputs_include_hd_haptic_pins() {
    let inputs = layouts::inputs_for(ControllerKind::DualSense);
    for pin in ["ds_l_amp", "ds_l_freq", "ds_r_amp", "ds_r_freq"] {
        assert!(inputs.iter().any(|p| p.id == pin),
            "DualSense must expose {pin} for HD haptic feedback");
    }
}

#[test]
fn dualsense_keeps_xinput_compatible_rumble_alongside_hd_haptics() {
    // HD haptics are USB-only; rumble_strong/weak must remain so that BT
    // connections still get a feedback path that works.
    let inputs = layouts::inputs_for(ControllerKind::DualSense);
    assert!(inputs.iter().any(|p| p.id == "rumble_strong"));
    assert!(inputs.iter().any(|p| p.id == "rumble_weak"));
}

// ── XInput ────────────────────────────────────────────────────────────────────

#[test]
fn xinput_inputs_include_rumble_pins() {
    let inputs = layouts::inputs_for(ControllerKind::XInput);
    assert!(inputs.iter().any(|p| p.id == "rumble_strong"),
        "XInput must expose rumble_strong");
    assert!(inputs.iter().any(|p| p.id == "rumble_weak"),
        "XInput must expose rumble_weak");
}

#[test]
fn xinput_inputs_have_no_lightbar_or_trigger_pins() {
    let inputs = layouts::inputs_for(ControllerKind::XInput);
    assert!(!inputs.iter().any(|p| p.id.starts_with("lightbar_")),
        "XInput must not expose lightbar pins (no RGB hardware)");
    assert!(!inputs.iter().any(|p| p.id.starts_with("trigger_r_") || p.id.starts_with("trigger_l_")),
        "XInput must not expose DualSense adaptive trigger pins");
    assert!(!inputs.iter().any(|p| p.id == "player_led" || p.id == "mic_led"),
        "XInput must not expose DualSense LED pins");
}

// ── Switch Pro ────────────────────────────────────────────────────────────────

#[test]
fn switch_pro_inputs_include_hd_rumble_pins() {
    let inputs = layouts::inputs_for(ControllerKind::SwitchPro);
    assert!(inputs.iter().any(|p| p.id == "hd_rumble_l"),
        "Switch Pro must expose hd_rumble_l");
    assert!(inputs.iter().any(|p| p.id == "hd_rumble_r"),
        "Switch Pro must expose hd_rumble_r");
}

// ── Generic fallback ──────────────────────────────────────────────────────────

#[test]
fn generic_inputs_do_not_panic_and_have_rumble() {
    // Must not panic on an unrecognised controller kind.
    let inputs = layouts::inputs_for(ControllerKind::Generic);
    // Generic controllers get basic rumble as graceful fallback.
    assert!(inputs.iter().any(|p| p.id == "rumble_strong"),
        "Generic must expose at least rumble_strong as fallback");
    assert!(inputs.iter().any(|p| p.id == "rumble_weak"),
        "Generic must expose at least rumble_weak as fallback");
}

#[test]
fn generic_outputs_do_not_panic() {
    // Must not panic; returns the standard gamepad sensor pin set + automap_out.
    let outputs = layouts::outputs_for(ControllerKind::Generic);
    assert!(!outputs.is_empty(), "Generic outputs_for must not return an empty list");
    assert!(outputs.iter().any(|p| p.id == "automap_out"),
        "Generic outputs_for must include automap_out");
}

// ── Sensor outputs cross-check ─────────────────────────────────────────────────
// Verify that the sensor output side (buttons/axes) is not accidentally empty.

// ── SDL backend dedup + extended Generic pins ──────────────────────────────────
// The SDL backend enumerates ONLY pads that `ControllerKind::detect` classifies
// as `Generic`; every kind gilrs owns must therefore detect as non-`Generic` so
// SDL drops it and no controller is surfaced twice. These are the exact VID/PIDs
// the SDL open path runs `detect` on. Pure — no SDL device needed.

#[test]
fn sdl_dedup_gilrs_owned_pads_detect_non_generic() {
    // (vendor, product, label) for controllers gilrs + the raw-HID path own.
    let owned: &[(u16, u16, &str)] = &[
        (0x045E, 0x028E, "Xbox 360"),
        (0x045E, 0x02FF, "Xbox One / HIDMaestro companion"),
        (0x054C, 0x05C4, "DualShock 4 v1"),
        (0x054C, 0x09CC, "DualShock 4 v2"),
        (0x054C, 0x0CE6, "DualSense"),
        (0x054C, 0x0DF2, "DualSense Edge"),
        (0x057E, 0x2009, "Switch Pro"),
    ];
    for (vid, pid, label) in owned {
        let kind = ControllerKind::detect("", Some(*vid), Some(*pid));
        assert_ne!(
            kind,
            ControllerKind::Generic,
            "{label} ({vid:04X}:{pid:04X}) must NOT detect as Generic, or the SDL \
             backend would surface it as a duplicate of the gilrs/raw-HID device",
        );
    }
}

#[test]
fn sdl_generic_pad_actually_detects_generic() {
    // A third-party pad SDL should own (8BitDo VID, arbitrary PID) — the SDL
    // filter keeps it precisely because it is Generic.
    let kind = ControllerKind::detect("8BitDo Pro 2", Some(0x2DC8), Some(0x6003));
    assert_eq!(
        kind,
        ControllerKind::Generic,
        "an unrecognized third-party pad must detect as Generic so SDL enumerates it",
    );
}

#[test]
fn generic_outputs_expose_sdl_relayed_pins() {
    // The SDL backend relays gyro/accel, touchpad, and extra buttons for Generic
    // pads; those pins must be declared in the Generic layout or a sink could
    // never map them. Guards against dropping the layout extension.
    let outputs = layouts::outputs_for(ControllerKind::Generic);
    let has = |id: &str| outputs.iter().any(|p| p.id == id);
    for pin in [
        "gyro_x", "gyro_y", "gyro_z", "accel_x", "accel_y", "accel_z",
        "touch1_x", "touch1_y", "touch1_active",
        "touch2_x", "touch2_y", "touch2_active", "btn_touchpad",
        "btn_paddle_l1", "btn_paddle_r1", "btn_paddle_l2", "btn_paddle_r2",
        "btn_misc1", "btn_misc2",
    ] {
        assert!(has(pin), "Generic outputs must expose `{pin}` for the SDL backend");
    }
}

#[test]
fn all_supported_kinds_have_sensor_outputs() {
    for kind in [
        ControllerKind::XInput,
        ControllerKind::DualShock4,
        ControllerKind::DualSense,
        ControllerKind::SwitchPro,
        ControllerKind::Generic,
    ] {
        let outputs = layouts::outputs_for(kind);
        // Must have at least one pin beyond automap_out (sensor data pins).
        assert!(
            outputs.iter().any(|p| p.id != "automap_out"),
            "{kind:?} outputs_for must include at least one sensor data pin",
        );
    }
}
