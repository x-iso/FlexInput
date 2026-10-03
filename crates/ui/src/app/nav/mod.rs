//! Gamepad-navigation driving, split by the area of the UI being driven.
//!
//! Every child adds methods to `FlexInputApp` via its own `impl` block. Those
//! methods are `pub(crate)` rather than private: a private method defined in a
//! child module is invisible to `app.rs` (and to sibling nav modules), and the
//! nav clusters call across each other constantly.

use super::*;

mod area;
mod config;
mod curves;
mod fields;
mod gp_settings;
mod left_panel;
mod legend;
mod midi_modal;
mod pickers;
mod remap;
mod touch_zones;

pub(crate) use midi_modal::{show_over_game, MidiModalOutcome};

// Viewport-agnostic free fns the config overlay reuses (drawn on the overlay
// viewport, not just the main window).
pub(crate) use remap::draw_remap_card_glow;
pub(crate) use config::draw_config_field_glow;

/// The nav `outer` that stands for the TAB CANVAS itself, not a sub-patch on it.
///
/// The shared editors address a widget as `(outer, inner)` — node `inner` inside
/// sub-patch `outer` — which left every node sitting directly on the tab out of
/// reach: the config overlay could FOCUS a pin pinned from an Advanced-mode
/// canvas but never enter it. With this as `outer`, [`nav_scope`] resolves to the
/// tab's own snarl and the same editors drive it. Never a real node id (snarl ids
/// are slab indices).
pub(crate) const NAV_TAB_CANVAS: egui_snarl::NodeId = egui_snarl::NodeId(usize::MAX);

/// The snarl a nav `outer` addresses: the inner snarl of sub-patch `outer`, or
/// the tab canvas itself for [`NAV_TAB_CANVAS`].
pub(crate) fn nav_scope(
    tab: &egui_snarl::Snarl<crate::canvas::NodeData>,
    outer: egui_snarl::NodeId,
) -> Option<&egui_snarl::Snarl<crate::canvas::NodeData>> {
    if outer == NAV_TAB_CANVAS {
        return Some(tab);
    }
    let inner: &egui_snarl::Snarl<crate::canvas::NodeData> = &tab.get_node(outer)?.subpatch.as_ref()?.snarl;
    Some(inner)
}

/// Mutable [`nav_scope`].
pub(crate) fn nav_scope_mut(
    tab: &mut egui_snarl::Snarl<crate::canvas::NodeData>,
    outer: egui_snarl::NodeId,
) -> Option<&mut egui_snarl::Snarl<crate::canvas::NodeData>> {
    if outer == NAV_TAB_CANVAS {
        return Some(tab);
    }
    let inner: &mut egui_snarl::Snarl<crate::canvas::NodeData> = &mut tab.get_node_mut(outer)?.subpatch.as_mut()?.snarl;
    Some(inner)
}

/// The sub-patch path (outermost first) a nav `outer` stands for — what the
/// pickers and modals address their target node by.
pub(crate) fn nav_path(outer: egui_snarl::NodeId) -> Vec<usize> {
    if outer == NAV_TAB_CANVAS { Vec::new() } else { vec![outer.0] }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn the_tab_canvas_outer_resolves_to_the_tab_itself() {
        let mut tab: egui_snarl::Snarl<crate::canvas::NodeData> = egui_snarl::Snarl::new();
        assert!(std::ptr::eq(nav_scope(&tab, NAV_TAB_CANVAS).unwrap(), &tab));
        let tab_ptr: *const _ = &tab;
        assert!(std::ptr::eq(nav_scope_mut(&mut tab, NAV_TAB_CANVAS).unwrap(), tab_ptr));
        // A sub-patch outer that isn't there resolves to nothing, as before.
        assert!(nav_scope(&tab, egui_snarl::NodeId(0)).is_none());
    }

    #[test]
    fn the_tab_canvas_outer_is_the_empty_path() {
        assert!(nav_path(NAV_TAB_CANVAS).is_empty());
        assert_eq!(nav_path(egui_snarl::NodeId(7)), vec![7]);
    }
}
