//! Patch format backward-compatibility tests for the core Patch struct.
//!
//! These tests verify that core::Patch deserialization does not break when loading
//! older `.fxp`-format JSON that was written before Phase 1 device feedback changes.
//! They also verify roundtrip fidelity: node IDs, wire topology, and PATCH_VERSION
//! must survive a serialize → deserialize cycle unchanged.

use flexinput_core::{Patch, PATCH_VERSION};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_fixture(name: &str) -> Patch {
    let json = match name {
        "compat_v1_basic" => include_str!("fixtures/compat_v1_basic.json"),
        "compat_v1_device_feedback" => include_str!("fixtures/compat_v1_device_feedback.json"),
        _ => panic!("unknown fixture: {name}"),
    };
    serde_json::from_str(json).unwrap_or_else(|e| {
        panic!("failed to deserialize fixture '{name}': {e}")
    })
}

// ---------------------------------------------------------------------------
// Task 1 coverage: fixtures can be loaded
// ---------------------------------------------------------------------------

#[test]
fn fixture_compat_v1_basic_deserializes() {
    let patch = load_fixture("compat_v1_basic");
    assert_eq!(patch.version, 1, "fixture version must be 1");
    assert_eq!(patch.nodes.len(), 2, "basic fixture has 2 nodes");
    assert_eq!(patch.wires.len(), 1, "basic fixture has 1 wire");
}

#[test]
fn fixture_compat_v1_device_feedback_deserializes() {
    let patch = load_fixture("compat_v1_device_feedback");
    assert_eq!(patch.version, 1, "fixture version must be 1");
    assert_eq!(patch.nodes.len(), 2, "device-feedback fixture has 2 nodes");
    assert_eq!(patch.wires.len(), 1, "device-feedback fixture has 1 wire");
}

// ---------------------------------------------------------------------------
// Task 2 coverage: roundtrip and stability tests
// ---------------------------------------------------------------------------

#[test]
fn patch_version_constant_is_1() {
    assert_eq!(PATCH_VERSION, 1, "PATCH_VERSION must remain 1 for backward compat");
}

#[test]
fn basic_fixture_roundtrip_preserves_node_ids() {
    let original = load_fixture("compat_v1_basic");

    // Collect original node IDs.
    let original_ids: Vec<Uuid> = original.nodes.iter().map(|n| n.id).collect();

    // Roundtrip: serialize then deserialize.
    let json = serde_json::to_string(&original)
        .expect("failed to serialize basic fixture");
    let roundtripped: Patch = serde_json::from_str(&json)
        .expect("failed to deserialize roundtripped basic fixture");

    let roundtripped_ids: Vec<Uuid> = roundtripped.nodes.iter().map(|n| n.id).collect();
    assert_eq!(
        original_ids, roundtripped_ids,
        "node IDs must be preserved through roundtrip"
    );
}

#[test]
fn basic_fixture_roundtrip_preserves_wire_topology() {
    let original = load_fixture("compat_v1_basic");

    let json = serde_json::to_string(&original)
        .expect("serialize");
    let roundtripped: Patch = serde_json::from_str(&json)
        .expect("deserialize");

    assert_eq!(
        original.wires.len(),
        roundtripped.wires.len(),
        "wire count must be preserved"
    );

    // Verify each wire endpoint is identical.
    for (orig, rt) in original.wires.iter().zip(roundtripped.wires.iter()) {
        assert_eq!(orig.from_node, rt.from_node, "from_node must survive roundtrip");
        assert_eq!(orig.from_pin, rt.from_pin, "from_pin must survive roundtrip");
        assert_eq!(orig.to_node, rt.to_node, "to_node must survive roundtrip");
        assert_eq!(orig.to_pin, rt.to_pin, "to_pin must survive roundtrip");
    }
}

#[test]
fn basic_fixture_roundtrip_preserves_version() {
    let original = load_fixture("compat_v1_basic");
    let json = serde_json::to_string(&original).expect("serialize");
    let roundtripped: Patch = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(roundtripped.version, original.version, "version must survive roundtrip");
}

#[test]
fn device_feedback_fixture_roundtrip_preserves_params() {
    let original = load_fixture("compat_v1_device_feedback");

    let json = serde_json::to_string(&original).expect("serialize");
    let roundtripped: Patch = serde_json::from_str(&json).expect("deserialize");

    // Ensure params are preserved for both nodes.
    for (orig, rt) in original.nodes.iter().zip(roundtripped.nodes.iter()) {
        assert_eq!(
            orig.params, rt.params,
            "params for node '{}' must survive roundtrip",
            orig.module_id
        );
    }
}

#[test]
fn node_without_params_roundtrips_cleanly() {
    // A node with no params should omit "params" from serialized JSON
    // (due to skip_serializing_if = HashMap::is_empty) and deserialize back
    // to an empty HashMap (due to #[serde(default)]).
    let original = load_fixture("compat_v1_basic");

    // The second node (index 1) has no params in the fixture.
    let node_without_params = &original.nodes[1];
    assert!(
        node_without_params.params.is_empty(),
        "second node in basic fixture should have no params"
    );

    let json = serde_json::to_string(&original).expect("serialize");

    // "params" should be absent for the parameterless node.
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse json");
    let node_json = &parsed["nodes"][1];
    assert!(
        node_json.get("params").is_none(),
        "params key must be omitted when empty (skip_serializing_if)"
    );

    // And it must deserialize back to empty.
    let roundtripped: Patch = serde_json::from_str(&json).expect("deserialize");
    assert!(
        roundtripped.nodes[1].params.is_empty(),
        "params must deserialize to empty HashMap when key absent"
    );
}

#[test]
fn optional_subpatch_absent_in_v1_fixtures() {
    // v1 fixtures do not include subpatch; verify they deserialize to None.
    let basic = load_fixture("compat_v1_basic");
    for node in &basic.nodes {
        assert!(
            node.subpatch.is_none(),
            "v1 basic fixture nodes must have no subpatch"
        );
    }

    let device = load_fixture("compat_v1_device_feedback");
    for node in &device.nodes {
        assert!(
            node.subpatch.is_none(),
            "v1 device-feedback fixture nodes must have no subpatch"
        );
    }
}

#[test]
fn default_patch_uses_patch_version_constant() {
    let patch = Patch::default();
    assert_eq!(
        patch.version, PATCH_VERSION,
        "default Patch must carry PATCH_VERSION"
    );
    assert!(patch.nodes.is_empty(), "default Patch has no nodes");
    assert!(patch.wires.is_empty(), "default Patch has no wires");
}
