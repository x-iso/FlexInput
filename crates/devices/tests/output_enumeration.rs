//! Integration tests for controller output and input pin layout tables.
//! These are pure-data tests requiring no hardware or HID access.
use flexinput_devices::{layouts, identification::ControllerKind};

#[test]
fn xinput_has_rumble_pins() {
    let inputs = layouts::inputs_for(ControllerKind::XInput);
    assert!(
        inputs.iter().any(|p| p.id == "rumble_strong"),
        "XInput inputs_for must include rumble_strong, got: {:?}",
        inputs.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    assert!(
        inputs.iter().any(|p| p.id == "rumble_weak"),
        "XInput inputs_for must include rumble_weak"
    );
}

#[test]
fn dualsense_has_adaptive_trigger_pins() {
    let inputs = layouts::inputs_for(ControllerKind::DualSense);
    assert!(
        inputs.iter().any(|p| p.id == "adaptive_trigger_l"),
        "DualSense inputs_for must include adaptive_trigger_l"
    );
    assert!(
        inputs.iter().any(|p| p.id == "adaptive_trigger_r"),
        "DualSense inputs_for must include adaptive_trigger_r"
    );
}

#[test]
fn ds4_has_lightbar_pins() {
    let inputs = layouts::inputs_for(ControllerKind::DualShock4);
    assert!(inputs.iter().any(|p| p.id == "lightbar_r"), "DS4 must have lightbar_r");
    assert!(inputs.iter().any(|p| p.id == "lightbar_g"), "DS4 must have lightbar_g");
    assert!(inputs.iter().any(|p| p.id == "lightbar_b"), "DS4 must have lightbar_b");
}

#[test]
fn switch_pro_has_hd_rumble_pins() {
    let inputs = layouts::inputs_for(ControllerKind::SwitchPro);
    assert!(inputs.iter().any(|p| p.id == "hd_rumble_l"), "SwitchPro must have hd_rumble_l");
    assert!(inputs.iter().any(|p| p.id == "hd_rumble_r"), "SwitchPro must have hd_rumble_r");
}

#[test]
fn all_supported_kinds_have_automap_out() {
    for kind in [
        ControllerKind::XInput,
        ControllerKind::DualShock4,
        ControllerKind::DualSense,
        ControllerKind::SwitchPro,
    ] {
        let outputs = layouts::outputs_for(kind);
        assert!(
            outputs.iter().any(|p| p.id == "automap_out"),
            "{kind:?} outputs_for must include automap_out"
        );
    }
}

#[test]
fn generic_does_not_panic() {
    // Generic controllers must enumerate without panicking even if output list is minimal.
    let _ = layouts::outputs_for(ControllerKind::Generic);
    let _ = layouts::inputs_for(ControllerKind::Generic);
}
