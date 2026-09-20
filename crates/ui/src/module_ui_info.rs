//! Per-module UI/graph classification — the UI layer of the module-registry
//! seam (Phase C).
//!
//! Centralizes the uniform boolean predicates ABOUT a module (properties, not
//! rendering) that were otherwise scattered as ad-hoc `matches!(module_id, …)`
//! lists across graph-building, pin glow, and gamepad-nav. Each predicate is the
//! kind of fact a future runtime plugin would declare about itself; the actual
//! body/pinned RENDER dispatch stays match-based for now (heterogeneous
//! signatures — see the Phase C notes). Keyed by `module_id` string.
//!
//! When adding a module that shares one of these behaviours, add its id here
//! rather than to each call site.

/// A module that RE-PUBLISHES the AutoMap bus into its OWN `collector:{uid}` key
/// (mirroring the Collector's phase-1 copy). A node downstream of one of these
/// must read from that collector key rather than recursing past it — the graph
/// builder's automap-source resolver stops here. `feedback_control` is NOT in
/// this set (it recurses past to the upstream device).
pub(crate) fn republishes_automap_bus(module_id: &str) -> bool {
    matches!(
        module_id,
        "module.automap_collect"
            | "module.audio_stream_haptics"
            | "module.jsm"
            | "module.network_send"
            | "module.network_recv"
    )
}

/// A module whose output pin glow comes from WALKING ITS AUTOMAP INPUT (it
/// passes the bus straight through / only injects feedback), rather than from
/// its own `last_out`. Without this its passthrough output slot (`last_out[0]`
/// = None) would never light despite live upstream signals.
pub(crate) fn glows_from_automap_input(module_id: &str) -> bool {
    matches!(
        module_id,
        "module.automap_split"
            | "module.automap_collect"
            | "module.remapper"
            | "module.audio_stream_haptics"
            | "module.jsm"
            | "module.touch_zones"
    )
}

/// A module whose body carries a gamepad-navigable response curve (the nav
/// driver can open + edit its curve points). Curve-primitive nodes plus modules
/// that embed one (ASTH's EQ, Vec Reshape).
pub(crate) fn has_nav_response_curve(module_id: &str) -> bool {
    matches!(
        module_id,
        "module.response_curve"
            | "module.vec_response_curve"
            | "module.twoway_response_curve"
            | "module.audio_stream_haptics"
            | "module.vec_reshape"
            | "generator.envelope"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asth_carries_the_expected_ui_properties() {
        // The pilot module must be in every set it was hardcoded into before the
        // registry, and a plain node must be in none.
        let asth = "module.audio_stream_haptics";
        assert!(republishes_automap_bus(asth));
        assert!(glows_from_automap_input(asth));
        assert!(has_nav_response_curve(asth));
        for f in [republishes_automap_bus, glows_from_automap_input, has_nav_response_curve] {
            assert!(!f("module.label"), "a plain module must not match any UI property");
        }
    }

    // The JSM module publishes the bus it was handed, with the config applied:
    // a downstream node must read its collector key, not walk past it to the pad.
    #[test]
    fn jsm_republishes_the_bus_it_was_given() {
        assert!(republishes_automap_bus("module.jsm"));
        assert!(glows_from_automap_input("module.jsm"));
        assert!(!has_nav_response_curve("module.jsm"));
    }

    #[test]
    fn republish_set_matches_the_engine_injectors() {
        // Mirrors the graph builder's automap-source stop set. feedback_control is
        // deliberately excluded (it recurses past to the upstream device).
        assert!(republishes_automap_bus("module.automap_collect"));
        assert!(republishes_automap_bus("module.network_send"));
        assert!(republishes_automap_bus("module.network_recv"));
        assert!(!republishes_automap_bus("module.feedback_control"));
    }

    // A module that hands on a bus of its own has to be one the Combiner can
    // trace, or that input offers nothing and the Combiner lists no conflicts to
    // resolve — which is what happened to the JSM module.
    #[test]
    fn every_bus_republisher_is_one_the_combiner_knows() {
        for module in ["module.automap_collect", "module.audio_stream_haptics",
                       "module.jsm", "module.network_send", "module.network_recv"] {
            assert!(republishes_automap_bus(module), "{module} should republish");
            assert!(
                crate::canvas::viewer::bus_label(module).is_some(),
                "{module} republishes a bus, so the Combiner must have a name for it",
            );
        }
        assert!(crate::canvas::viewer::bus_label("module.curve").is_none(),
            "a module that isn't bus-shaped has no business in that list");
    }
}
