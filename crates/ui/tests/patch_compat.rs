//! Patch format backward-compatibility tests.
//! Tests use UiPatch (the actual .fxp file format) not the engine Patch struct.
use flexinput_ui::UiPatch;

#[test]
fn patch_round_trip_v1() {
    let json = include_str!("fixtures/compat_v1_basic.json");
    let patch: UiPatch = serde_json::from_str(json)
        .expect("failed to deserialize compat_v1_basic.json fixture");
    assert_eq!(patch.version, 1, "fixture version must be 1");

    let re_serialized = serde_json::to_string(&patch)
        .expect("failed to re-serialize patch");
    let patch2: UiPatch = serde_json::from_str(&re_serialized)
        .expect("failed to deserialize re-serialized patch");

    assert_eq!(patch2.version, patch.version, "round-trip must preserve version");
    assert!(patch2.virtual_device_ids.is_empty(), "round-trip must preserve empty virtual_device_ids");
    assert!(!patch2.auto_bypass, "round-trip must preserve auto_bypass=false");
}

#[test]
fn patch_with_missing_optional_fields_uses_defaults() {
    // Verify #[serde(default)] fields work for forward-compat (older files lack new fields).
    let minimal = r#"{"version":1,"snarl":{"nodes":{},"wires":[]},"virtual_device_ids":[]}"#;
    let patch: UiPatch = serde_json::from_str(minimal)
        .expect("patch missing optional fields should deserialize with defaults");
    assert_eq!(patch.version, 1);
    assert!(!patch.auto_bypass, "auto_bypass should default to false");
    assert!(patch.bound_exes.is_empty(), "bound_exes should default to empty");
}
