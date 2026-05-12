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
