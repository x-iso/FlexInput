mod curve;
pub mod header_controls;
pub(crate) mod input_viewer;
pub(crate) mod menu_body;
pub mod node;
pub(crate) mod overlay_body;
pub mod remapper_icons;
pub mod viewer;

pub use node::NodeData;
pub use node::OverlayLayout;
pub use viewer::FlexViewer;

use std::collections::{HashMap, HashSet};

use egui_snarl::{ui::{get_selected_nodes, SnarlStyle}, InPinId, NodeId, OutPinId, Snarl};
use flexinput_core::{PinDescriptor, ModuleDescriptor, Signal, SignalType};
use flexinput_devices::PhysicalDevice;
use flexinput_virtual::{SinkPin, SourcePin, VirtualDevice};
use serde_json::Value;

use crate::app::request_repaint_throttled;

const MAX_UNDO: usize = 50;

/// Allocate a fresh, process-unique salt for a `Canvas`'s snarl id. Each
/// `Canvas` (tab canvas or sub-patch editor inner canvas) gets a distinct
/// salt so its pan/zoom (`SnarlState.to_global`, stored in egui temp memory
/// keyed by the snarl id) is independent from every other canvas's — they no
/// longer share one transform via the old constant `"flexinput_canvas"` id.
/// Callers with a *stable* identity (a tab, or a sub-patch node) overwrite the
/// default with a derived salt via `set_view_salt`, so the view persists across
/// tab switches / sub-patch reopen rather than resetting each time.
pub(crate) fn next_canvas_salt() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    // Start high so a fresh per-instance salt never collides with a small
    // derived tab/sub-patch salt (those are folded through splitmix64 and
    // effectively random, but the offset keeps the spaces visibly distinct).
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Per-device param seeds applied when a device node is first added to the
/// canvas. Sourced from `AppSettings` so users can set workspace-wide defaults
/// in Settings → Device defaults.
#[derive(Clone, Copy)]
pub struct DeviceParamDefaults {
    pub stick_deadzone: f32,
    pub gyro_mult: f32,
    pub mouse_sensitivity: f32,
    /// Rumble-forwarding shape for virtual pad sinks (floor..max band +
    /// response exponent); see `header_controls::render_rumble_feedback_controls`.
    pub rumble_floor: f32,
    pub rumble_max: f32,
    pub rumble_exp: f32,
}

impl Default for DeviceParamDefaults {
    fn default() -> Self {
        Self {
            stick_deadzone: 0.1,
            gyro_mult: 1.0,
            mouse_sensitivity: 100.0,
            rumble_floor: header_controls::RUMBLE_DEF_FLOOR,
            rumble_max: header_controls::RUMBLE_DEF_MAX,
            rumble_exp: header_controls::RUMBLE_DEF_EXP,
        }
    }
}

/// Map a legacy ViGEm virtual-device id to its HIDMaestro equivalent, preserving
/// the instance suffix. Returns `None` when `id` isn't a migratable ViGEm id (so
/// the caller leaves it untouched).
///
/// `virtual.xinput` → `virtual.hm.xinput`, `virtual.ds4` → `virtual.hm.ds4`
/// (and `virtual.xinput.2` → `virtual.hm.xinput.2`, etc.). The HIDMaestro kinds
/// expose the SAME sink/source pin layouts (XINPUT_* / DS4_*), so only the id
/// string changes — every wire stays connected to its pin. `virtual.keymouse`
/// and any already-HIDMaestro id are not ViGEm and return `None`.
pub fn migrate_vigem_device_id(id: &str) -> Option<String> {
    // Only the kind prefix is swapped; `rest` (either "" or ".N") is preserved.
    for (old, new) in [("virtual.xinput", "virtual.hm.xinput"), ("virtual.ds4", "virtual.hm.ds4")] {
        if let Some(rest) = id.strip_prefix(old) {
            // Guard against a longer kind that merely starts with the same text
            // (e.g. a hypothetical "virtual.ds4x"): the remainder must be empty
            // or a dotted instance suffix.
            if rest.is_empty() || rest.starts_with('.') {
                return Some(format!("{new}{rest}"));
            }
        }
    }
    None
}

/// Map a legacy Generic-pad button pin id to its positional replacement, or
/// `None` when the id needs no rewrite (already positional, or not a button).
///
/// The Generic layout was the last one using a private vocabulary for these
/// four buttons; the AutoMap bus only ever knew the positional names, so these
/// pins were invisible to Remapper / Splitter / Collector / gamepad-nav.
pub fn migrate_generic_button_pin(id: &str) -> Option<&'static str> {
    Some(match id {
        "btn_lstick" => "btn_ls",
        "btn_rstick" => "btn_rs",
        "btn_select" => "btn_back",
        "btn_mode"   => "btn_guide",
        _ => return None,
    })
}

/// Bring a loaded `Snarl` up to date with the current module descriptors for
/// pin-layout changes that can't be expressed through `#[serde(default)]`, and
/// migrate legacy ViGEm device ids to their HIDMaestro equivalents.
///
/// - Map Action gained a second output (`out_analog`, Float) in addition to its
///   original Bool `out`. Patches saved before that have a single serialized
///   output pin; append the missing pin so the node renders and wires correctly.
///   The original wire stays on pin 0 (`out`), exactly as before.
/// - ViGEm → HIDMaestro: rewrite `device_id` on `device.sink` / `device.source`
///   nodes (`virtual.xinput*` → `virtual.hm.xinput*`, `virtual.ds4*` →
///   `virtual.hm.ds4*`). Pin layouts are identical between the two backends, so
///   wires are preserved; the migrated id then deploys via the HIDMaestro path
///   (which prompts to install the driver if needed). Idempotent — already-
///   HIDMaestro ids are left alone.
/// - Negate → Inverse: the `math.negate` module kept its id but gained a new
///   display name. Nodes still carrying the old stock title are retitled;
///   user-renamed nodes are left alone.
/// - Generic-pad pin vocabulary: the Generic device layout used to name the
///   stick-clicks / menu buttons `btn_lstick`/`btn_rstick`/`btn_select`/
///   `btn_mode` while every other layout (and both backends) used the
///   positional `btn_ls`/`btn_rs`/`btn_back`/`btn_guide`. A `device.source`
///   node persists its pin ids in `output_pin_ids`, so a patch saved against
///   the old names must be rewritten or those four pins route to nothing.
///   Position in the list is unchanged, so wires stay on their pins.
///
/// Recurses into sub-patches.
pub fn migrate_loaded_snarl(snarl: &mut Snarl<NodeData>) {
    for (_, node) in snarl.nodes_ids_data_mut() {
        if node.value.module_id == "module.map_action" && node.value.outputs.len() < 2 {
            node.value.outputs = vec![
                PinDescriptor::new("Gate",   SignalType::Bool),
                PinDescriptor::new("Analog", SignalType::Float),
            ];
        }
        // Audio Stream Haptics gained raw-analysis output pins (band EFs + band
        // carrier freqs in Hz) after output[0] (AutoMap). Patches saved before that
        // only have the AutoMap pin — append the missing Floats so the new pins
        // appear. output[0] stays put, so any existing AutoMap wire is preserved.
        if node.value.module_id == "module.audio_stream_haptics" && node.value.outputs.len() < 7 {
            let want = [
                ("AutoMap", SignalType::AutoMap),
                ("LF EF L", SignalType::Float),
                ("HF EF L", SignalType::Float),
                ("LF EF R", SignalType::Float),
                ("HF EF R", SignalType::Float),
                ("LF Hz",   SignalType::Float),
                ("HF Hz",   SignalType::Float),
            ];
            node.value.outputs = want.iter()
                .map(|(name, ty)| PinDescriptor::new(*name, *ty))
                .collect();
        }
        if matches!(node.value.module_id.as_str(), "device.sink" | "device.source") {
            if let Some(new_id) = node
                .value
                .params
                .get("device_id")
                .and_then(|v| v.as_str())
                .and_then(migrate_vigem_device_id)
            {
                node.value.params.insert("device_id".to_string(), Value::from(new_id));
            }
        }
        // Pre-rumble-defaults patches: a virtual gamepad sink (non-keymouse)
        // with no rumble params relied on the old implicit 0.35/1.0/0.6 boost.
        // Newly created pads always carry these keys now, so their absence
        // uniquely identifies an old patch — backfill the legacy values so it
        // keeps the feel it was saved with. Idempotent: touched pads already
        // have the keys and are skipped.
        if node.value.module_id == "device.sink" {
            let dev = node.value.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
            if dev.starts_with("virtual.") && !dev.starts_with("virtual.keymouse")
                && !node.value.params.contains_key("rumble_floor")
            {
                use header_controls::{RUMBLE_LEGACY_EXP, RUMBLE_LEGACY_FLOOR, RUMBLE_LEGACY_MAX};
                node.value.params.insert("rumble_floor".into(), Value::from(RUMBLE_LEGACY_FLOOR as f64));
                node.value.params.insert("rumble_max".into(), Value::from(RUMBLE_LEGACY_MAX as f64));
                node.value.params.insert("rumble_exp".into(), Value::from(RUMBLE_LEGACY_EXP as f64));
            }
        }
        // "Negate" was renamed to "Inverse" (module id unchanged). Only rewrite
        // the stock title — a node the user renamed keeps their name.
        if node.value.module_id == "math.negate" && node.value.display_name == "Negate" {
            node.value.display_name = "Inverse".to_string();
        }
        // Generic-pad stick-click / menu pins → positional names. Applies to
        // ANY device.source (the old ids only ever existed on Generic pads, so
        // a native pad simply has nothing to rewrite). Idempotent.
        if node.value.module_id == "device.source" {
            if let Some(Value::Array(ids)) = node.value.params.get_mut("output_pin_ids") {
                for id in ids.iter_mut() {
                    let Some(new) = id.as_str().and_then(migrate_generic_button_pin) else { continue };
                    *id = Value::String(new.to_string());
                }
            }
        }
        if let Some(sp) = node.value.subpatch.as_mut() {
            migrate_loaded_snarl(&mut sp.snarl);
        }
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::canvas::node::{UiSubPatch, NodeData};
    use std::collections::HashMap;

    #[test]
    fn vigem_id_maps_to_hidmaestro_preserving_instance() {
        assert_eq!(migrate_vigem_device_id("virtual.xinput").as_deref(), Some("virtual.hm.xinput"));
        assert_eq!(migrate_vigem_device_id("virtual.ds4").as_deref(), Some("virtual.hm.ds4"));
        assert_eq!(migrate_vigem_device_id("virtual.xinput.2").as_deref(), Some("virtual.hm.xinput.2"));
        assert_eq!(migrate_vigem_device_id("virtual.ds4.3").as_deref(), Some("virtual.hm.ds4.3"));
    }

    #[test]
    fn non_vigem_ids_are_left_alone() {
        // Already HIDMaestro → no double-migration (idempotent).
        assert_eq!(migrate_vigem_device_id("virtual.hm.xinput"), None);
        assert_eq!(migrate_vigem_device_id("virtual.hm.ds4.1"), None);
        // Other kinds untouched.
        assert_eq!(migrate_vigem_device_id("virtual.keymouse"), None);
        assert_eq!(migrate_vigem_device_id("gilrs:dualsense:0"), None);
        // Prefix-only false positive guard: a longer kind that merely starts
        // with "virtual.ds4" must NOT match.
        assert_eq!(migrate_vigem_device_id("virtual.ds4x"), None);
    }

    /// A patch saved when the Generic layout still used `btn_lstick` &co gets
    /// its `device.source` pin ids rewritten in place — same positions, so
    /// existing wires stay on their pins. Idempotent, recurses into sub-patches,
    /// and leaves non-button ids alone.
    #[test]
    fn migrate_rewrites_legacy_generic_button_pin_ids() {
        fn generic_source() -> NodeData {
            let mut params = HashMap::new();
            params.insert("device_id".to_string(), Value::String("sdl:generic:i0".to_string()));
            params.insert("output_pin_ids".to_string(), Value::Array(
                ["left_stick", "btn_south", "btn_lstick", "btn_rstick",
                 "btn_start", "btn_select", "btn_mode", "automap_out"]
                    .iter().map(|s| Value::String(s.to_string())).collect(),
            ));
            NodeData {
                module_id: "device.source".to_string(),
                display_name: "Steam Controller".to_string(),
                category: "Device".to_string(),
                inputs: vec![],
                outputs: vec![],
                params,
                subpatch: None,
                extra: Default::default(),
            }
        }
        let ids_of = |n: &NodeData| -> Vec<String> {
            n.params["output_pin_ids"].as_array().unwrap().iter()
                .map(|v| v.as_str().unwrap().to_string()).collect()
        };
        let expected: Vec<String> = ["left_stick", "btn_south", "btn_ls", "btn_rs",
             "btn_start", "btn_back", "btn_guide", "automap_out"]
            .iter().map(|s| s.to_string()).collect();

        let mut snarl: Snarl<NodeData> = Snarl::new();
        let pad = snarl.insert_node(egui::Pos2::ZERO, generic_source());

        let mut inner: Snarl<NodeData> = Snarl::new();
        let nested = inner.insert_node(egui::Pos2::ZERO, generic_source());
        let mut sp = UiSubPatch::default();
        sp.snarl = Box::new(inner);
        let mut host = generic_source();
        host.module_id = "subpatch".to_string();
        host.params.remove("output_pin_ids");
        host.subpatch = Some(Box::new(sp));
        let host_id = snarl.insert_node(egui::pos2(20.0, 0.0), host);

        migrate_loaded_snarl(&mut snarl);
        assert_eq!(ids_of(snarl.get_node(pad).unwrap()), expected);
        let inner_sp = snarl.get_node(host_id).unwrap().subpatch.as_ref().unwrap();
        assert_eq!(ids_of(inner_sp.snarl.get_node(nested).unwrap()), expected);

        // Idempotent: a second pass over already-migrated ids is a no-op.
        migrate_loaded_snarl(&mut snarl);
        assert_eq!(ids_of(snarl.get_node(pad).unwrap()), expected);
    }

    /// Negate → Inverse retitles only nodes still carrying the stock name;
    /// a hand-renamed node keeps the user's title. Recurses into sub-patches.
    #[test]
    fn migrate_retitles_stock_negate_but_not_renamed_nodes() {
        fn negate(display_name: &str) -> NodeData {
            NodeData {
                module_id: "math.negate".to_string(),
                display_name: display_name.to_string(),
                category: "Math".to_string(),
                inputs: vec![],
                outputs: vec![],
                params: HashMap::new(),
                subpatch: None,
                extra: Default::default(),
            }
        }

        let mut snarl: Snarl<NodeData> = Snarl::new();
        let stock  = snarl.insert_node(egui::Pos2::ZERO, negate("Negate"));
        let custom = snarl.insert_node(egui::pos2(10.0, 0.0), negate("Flip Trigger"));

        let mut inner: Snarl<NodeData> = Snarl::new();
        let nested = inner.insert_node(egui::Pos2::ZERO, negate("Negate"));
        let mut sp = UiSubPatch::default();
        sp.snarl = Box::new(inner);
        let mut host = negate("Host");
        host.module_id = "subpatch".to_string();
        host.subpatch = Some(Box::new(sp));
        let host_id = snarl.insert_node(egui::pos2(20.0, 0.0), host);

        migrate_loaded_snarl(&mut snarl);

        assert_eq!(snarl.get_node(stock).unwrap().display_name, "Inverse");
        assert_eq!(snarl.get_node(custom).unwrap().display_name, "Flip Trigger");
        let inner_sp = snarl.get_node(host_id).unwrap().subpatch.as_ref().unwrap();
        assert_eq!(inner_sp.snarl.get_node(nested).unwrap().display_name, "Inverse");
    }

    /// A `device.sink`/`device.source` node with a ViGEm id is rewritten in place,
    /// and the migration recurses into sub-patches.
    #[test]
    fn migrate_loaded_snarl_rewrites_device_nodes_and_subpatches() {
        fn device_node(module_id: &str, device_id: &str) -> NodeData {
            let mut params = HashMap::new();
            params.insert("device_id".to_string(), Value::from(device_id));
            NodeData {
                module_id: module_id.to_string(),
                display_name: "Dev".to_string(),
                category: "Device".to_string(),
                inputs: vec![],
                outputs: vec![],
                params,
                subpatch: None,
                extra: Default::default(),
            }
        }

        let mut snarl: Snarl<NodeData> = Snarl::new();
        snarl.insert_node(egui::Pos2::ZERO, device_node("device.sink", "virtual.xinput"));
        snarl.insert_node(egui::pos2(10.0, 0.0), device_node("device.source", "virtual.ds4.1"));
        // Untouched kinds.
        snarl.insert_node(egui::pos2(20.0, 0.0), device_node("device.sink", "virtual.keymouse"));

        // A sub-patch holding a nested ViGEm sink — must also migrate.
        let mut inner: Snarl<NodeData> = Snarl::new();
        inner.insert_node(egui::Pos2::ZERO, device_node("device.sink", "virtual.ds4"));
        let mut sp = UiSubPatch::default();
        sp.snarl = Box::new(inner);
        let mut host = device_node("subpatch", "");
        host.subpatch = Some(Box::new(sp));
        let host_id = snarl.insert_node(egui::pos2(30.0, 0.0), host);

        migrate_loaded_snarl(&mut snarl);

        let ids: Vec<String> = snarl
            .nodes_ids_data()
            .filter_map(|(_, n)| n.value.params.get("device_id").and_then(|v| v.as_str()).map(String::from))
            .collect();
        assert!(ids.contains(&"virtual.hm.xinput".to_string()), "xinput sink migrated");
        assert!(ids.contains(&"virtual.hm.ds4.1".to_string()), "ds4 source migrated w/ instance");
        assert!(ids.contains(&"virtual.keymouse".to_string()), "keymouse untouched");

        // Nested sub-patch sink migrated.
        let host_node = snarl.get_node(host_id).expect("host present");
        let inner_sp = host_node.subpatch.as_ref().expect("subpatch present");
        let inner_id = inner_sp
            .snarl
            .nodes_ids_data()
            .find_map(|(_, n)| n.value.params.get("device_id").and_then(|v| v.as_str()).map(String::from))
            .expect("inner device node present");
        assert_eq!(inner_id, "virtual.hm.ds4", "nested ds4 sink migrated");
    }

    /// A virtual gamepad sink saved before rumble became configurable (no
    /// rumble params) is backfilled with the legacy shaping on load, so the
    /// patch keeps its old feel. keymouse sinks and pads that already carry
    /// rumble params are left untouched.
    #[test]
    fn migrate_backfills_legacy_rumble_on_old_virtual_pads() {
        use header_controls::{RUMBLE_LEGACY_EXP, RUMBLE_LEGACY_FLOOR};

        fn sink(device_id: &str) -> NodeData {
            let mut params = HashMap::new();
            params.insert("device_id".to_string(), Value::from(device_id));
            NodeData {
                module_id: "device.sink".to_string(),
                display_name: "Dev".to_string(),
                category: "Device".to_string(),
                inputs: vec![],
                outputs: vec![],
                params,
                subpatch: None,
                extra: Default::default(),
            }
        }

        let mut snarl: Snarl<NodeData> = Snarl::new();
        // Old pad: HIDMaestro id, no rumble params → should be backfilled.
        let old_pad = snarl.insert_node(egui::Pos2::ZERO, sink("virtual.hm.xinput"));
        // keymouse: never gets rumble params.
        let km = snarl.insert_node(egui::pos2(10.0, 0.0), sink("virtual.keymouse"));
        // Already-tuned pad: explicit rumble_floor must be preserved, not clobbered.
        let mut tuned = sink("virtual.hm.ds4");
        tuned.params.insert("rumble_floor".into(), Value::from(0.8_f64));
        let tuned_id = snarl.insert_node(egui::pos2(20.0, 0.0), tuned);

        migrate_loaded_snarl(&mut snarl);

        let floor_of = |id| snarl.get_node(id).unwrap().params.get("rumble_floor")
            .and_then(|v| v.as_f64()).map(|f| f as f32);
        let exp_of = |id| snarl.get_node(id).unwrap().params.get("rumble_exp")
            .and_then(|v| v.as_f64()).map(|f| f as f32);

        assert_eq!(floor_of(old_pad), Some(RUMBLE_LEGACY_FLOOR), "old pad floor backfilled");
        assert_eq!(exp_of(old_pad), Some(RUMBLE_LEGACY_EXP), "old pad exp backfilled");
        assert_eq!(floor_of(km), None, "keymouse gets no rumble params");
        assert_eq!(floor_of(tuned_id), Some(0.8), "tuned pad's explicit value preserved");
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct UiPatch {
    pub version: u32,
    pub snarl: Snarl<NodeData>,
    /// IDs of virtual output devices that were active (e.g. `"virtual.xinput.0"`).
    pub virtual_device_ids: Vec<String>,
    /// Exe filenames that auto-switch to this tab (e.g. `["game.exe"]`).
    #[serde(default)]
    pub bound_exes: Vec<String>,
    /// Bypass output when the bound process is not in focus.
    #[serde(default)]
    pub auto_bypass: bool,
    /// Path of the .fxsp preset most recently loaded into this tab's
    /// sub-patch (Easy mode). Used to restore the "current preset"
    /// link after reopening the app. Falls back to content-hash
    /// matching against the preset index when the path is stale.
    #[serde(default)]
    pub easy_preset_path: Option<std::path::PathBuf>,
    /// Screen-overlay layout for this patch (pinned module elements +
    /// decorations on the transparent info overlay). Default-empty so
    /// pre-overlay .fxp files keep loading.
    #[serde(default, skip_serializing_if = "OverlayLayout::is_empty")]
    pub overlay: OverlayLayout,
    /// Config-overlay layout (curated editable tweak-pins, M3). Default-empty
    /// and skipped when empty so pre-config `.fxp` files keep loading.
    #[serde(default, skip_serializing_if = "OverlayLayout::is_empty")]
    pub config: OverlayLayout,
}

#[derive(Clone)]
pub(crate) struct ClipboardData {
    nodes: Vec<(egui::Pos2, NodeData)>,
    /// Internal wires encoded as (from_node_idx, from_pin, to_node_idx, to_pin).
    internal_wires: Vec<(usize, usize, usize, usize)>,
}

/// One-shot view manipulation requested by an external action (e.g.
/// loading a patch). Applied on the canvas's next render and then cleared.
#[derive(Clone, Copy, Debug)]
pub enum PendingViewAction {
    /// Pan to the centroid of all nodes, keeping current zoom.
    Center,
    /// Pan + zoom so every node fits in the viewport with margin.
    ZoomToFit,
}

pub struct Canvas {
    pub snarl: Snarl<NodeData>,
    style: SnarlStyle,
    /// Pending wire right-click context menu: (from, to, screen position).
    wire_ctx_menu: Option<(OutPinId, InPinId, egui::Pos2)>,
    /// True on the frame the wire menu was first opened; suppresses the outside-click close check.
    wire_ctx_just_opened: bool,
    /// Active inline rename: (node id, edit buffer, popup position).
    rename_state: Option<(egui_snarl::NodeId, String, egui::Pos2)>,
    undo_stack: Vec<Snarl<NodeData>>,
    redo_stack: Vec<Snarl<NodeData>>,
    /// Fingerprint (serialized FNV-1a) of the snarl as of the last committed
    /// undo state. Used by the value-edit detector in `show()` to recognise
    /// when a param/value gesture has actually changed persistent state vs.
    /// the live-signal churn in `NodeExtra` (which is `#[serde(skip)]` and so
    /// never affects the fingerprint).
    committed_fingerprint: u64,
    /// Snarl clone captured at the START of an in-progress value gesture (the
    /// first frame the snarl diverged from `committed_fingerprint` while the
    /// user was interacting). Becomes the undo entry when the gesture settles.
    pending_value_baseline: Option<Snarl<NodeData>>,
    clipboard: Option<ClipboardData>,
    /// Incremented every time copy_selected() is called. Used by app.rs to detect
    /// whether the user actually copied something in an inner canvas this frame,
    /// without relying on clipboard content comparison (which breaks for same-count copies).
    pub(crate) clipboard_gen: u64,
    /// Incremented every time the snarl mutates (any push_undo / push_snapshot
    /// path, or undo/redo). Used by show_subpatch_editors to skip the
    /// per-frame `*sp.snarl.clone()` pre-sync when the outer canvas hasn't
    /// changed since the editor last synced — that clone dominates frame
    /// time for large inner graphs (50+ nodes) and is wasted on idle frames.
    pub(crate) mutation_gen: u64,
    /// Set this frame when the user requests to open a subpatch editor window.
    pub pending_edit_subpatch: Option<egui_snarl::NodeId>,
    /// Set this frame when the user picks "Pin element …" on an inner canvas
    /// node. (NodeId, element_id, source_size). `source_size = [0,0]` means
    /// "no measured size" — the receiver should fall back to a default.
    pub pending_expose_module: Option<(egui_snarl::NodeId, String, [f32; 2])>,
    /// True when this canvas is the inner graph of a sub-patch editor window.
    /// Hides inlet/outlet nodes from the outer context menu.
    pub is_inner: bool,
    /// NodeId.0 values of inner nodes currently pinned to the outer body.
    /// Populated by show_subpatch_editors before rendering the inner canvas.
    pub pinned_inner_ids: std::collections::HashSet<usize>,
    /// Updated each frame from the snarl view transform. Used to spawn
    /// new device nodes at the center of the current viewport.
    last_view_center_canvas: Option<egui::Pos2>,
    /// One-shot view action to apply on the next render. Set by app.rs
    /// after loading a patch when the `on_patch_load` setting is not Off.
    /// Consumed during `draw_zoom_controls` (which already has the
    /// `SnarlState` + snarl_rect needed).
    pub pending_view_action: Option<PendingViewAction>,
    /// Nodes scheduled for a one-shot spawn-glow flash, with the Instant
    /// they were inserted. The viewer paints a fading outline halo around
    /// these nodes for ~400 ms.
    spawn_glow: std::collections::HashMap<egui_snarl::NodeId, std::time::Instant>,
    /// Salt mixed into this canvas's snarl id so its pan/zoom is keyed
    /// independently of every other canvas (see `next_canvas_salt`). Defaults
    /// to a process-unique value; tab canvases and sub-patch editors overwrite
    /// it with a *stable* derived salt (`set_view_salt`) so the view survives
    /// tab switches and sub-patch close→reopen. `pub(crate)` so `easy::layout`
    /// can key the measured-node-rect stash (`viewer::final_node_rect`).
    pub(crate) view_salt: u64,
}

impl Canvas {
    pub fn new() -> Self {
        let mut style = SnarlStyle::default();
        style.collapsible = Some(true);
        // Place input/output pins just outside the node frame so they
        // don't collide with header content (notably the collapse
        // chevron, which egui-snarl always anchors at the node's left
        // edge regardless of header_frame margin).
        style.pin_placement = Some(egui_snarl::ui::PinPlacement::Outside { margin: 2.0 });
        style.header_drag_space = Some(egui::vec2(0.0, 0.0));
        // Translucent, slightly-darkened node/header fills so wires routed
        // behind a node (Auto-Map feedback loops in particular) remain
        // visibly continuous with their target port. egui-snarl paints the
        // header frame ON TOP of the body frame, so the header's alpha
        // composites against the body — to keep its *effective* opacity
        // close to the body's we use a low alpha on the header itself.
        let body_fill   = egui::Color32::from_rgba_unmultiplied(22, 24, 28, 160);
        let header_fill = egui::Color32::from_rgba_unmultiplied(0,  0,  0,  38);
        let border      = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(70, 70, 76, 200));
        style.node_frame = Some(
            egui::Frame::default()
                .fill(body_fill)
                .stroke(border)
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(6))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 2],
                    blur: 8,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(120),
                }),
        );
        style.header_frame = Some(
            egui::Frame::default()
                .fill(header_fill)
                .corner_radius(egui::CornerRadius {
                    nw: 6, ne: 6, sw: 0, se: 0,
                })
                .inner_margin(egui::Margin::symmetric(6, 4)),
        );
        Canvas {
            snarl: Snarl::new(),
            style,
            wire_ctx_menu: None,
            wire_ctx_just_opened: false,
            rename_state: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            // Empty snarl fingerprint; recomputed lazily on the first
            // interaction frame and refreshed after every commit/undo/redo.
            committed_fingerprint: 0,
            pending_value_baseline: None,
            clipboard: None,
            clipboard_gen: 0,
            mutation_gen: 0,
            pending_edit_subpatch: None,
            pending_expose_module: None,
            is_inner: false,
            pinned_inner_ids: std::collections::HashSet::new(),
            last_view_center_canvas: None,
            pending_view_action: None,
            spawn_glow: std::collections::HashMap::new(),
            view_salt: next_canvas_salt(),
        }
    }

    /// Pin this canvas's snarl-id salt to a stable, caller-derived value so its
    /// pan/zoom is remembered across re-creation (tab switches, sub-patch
    /// editor close→reopen). Without this a fresh `Canvas` gets a new unique
    /// salt each time and would re-frame on every open.
    pub fn set_view_salt(&mut self, salt: u64) {
        self.view_salt = salt;
    }

    /// This canvas's view salt — used to derive a *nested* sub-patch editor's
    /// stable salt from its parent editor's salt + the child node id.
    pub fn view_salt(&self) -> u64 {
        self.view_salt
    }

    pub fn can_undo(&self) -> bool { !self.undo_stack.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo_stack.is_empty() }

    /// Take a clone of the current snarl, for use as an undo baseline (e.g. the
    /// gamepad nav driver snapshots before a value-edit gesture begins).
    pub(crate) fn snapshot_for_undo(&self) -> Snarl<NodeData> {
        self.snarl.clone()
    }

    /// Commit a previously-taken `baseline` as one undo entry, but only if the
    /// snarl actually changed since. Used by the gamepad nav edit gesture
    /// (snapshot on enter-edit, commit on exit) — its direct `node.params`
    /// writes don't go through the pointer/keyboard `track_value_edits` path.
    pub(crate) fn commit_undo_if_changed(&mut self, baseline: Snarl<NodeData>) {
        if self.snarl_fingerprint() != Self::fingerprint_of(&baseline) {
            self.push_snapshot(baseline);
        }
    }

    /// FNV-1a fingerprint of an arbitrary snarl (same scheme as
    /// `snarl_fingerprint`, for comparing a stored baseline).
    fn fingerprint_of(snarl: &Snarl<NodeData>) -> u64 {
        let bytes = serde_json::to_vec(snarl).unwrap_or_default();
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in &bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// FNV-1a hash of the serialized snarl. `NodeData.extra` is `#[serde(skip)]`,
    /// so the live per-frame signal history / readouts churned by the processing
    /// thread do NOT affect this — only persistent state (params, positions,
    /// wires, subpatch contents) does. Mirrors `hash_subpatch` in
    /// `easy/center_panel.rs`.
    fn snarl_fingerprint(&self) -> u64 {
        let bytes = serde_json::to_vec(&self.snarl).unwrap_or_default();
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in &bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Adopt the current snarl as the committed undo baseline. Called after any
    /// path that pushes an undo entry or replaces the snarl wholesale, so the
    /// value-edit detector in `show()` won't mistake that change for a fresh
    /// user gesture. Also drops any half-captured value gesture.
    fn sync_committed_fingerprint(&mut self) {
        self.committed_fingerprint = self.snarl_fingerprint();
        self.pending_value_baseline = None;
    }

    /// Snapshot the current snarl state onto the undo stack, clearing redo.
    fn push_undo(&mut self) {
        self.undo_stack.push(self.snarl.clone());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.mutation_gen = self.mutation_gen.wrapping_add(1);
        self.sync_committed_fingerprint();
    }

    /// Push an externally-taken pre-mutation snapshot onto the undo stack.
    fn push_snapshot(&mut self, snapshot: Snarl<NodeData>) {
        self.undo_stack.push(snapshot);
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.mutation_gen = self.mutation_gen.wrapping_add(1);
        self.sync_committed_fingerprint();
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.snarl.clone());
            self.snarl = prev;
            self.mutation_gen = self.mutation_gen.wrapping_add(1);
            self.sync_committed_fingerprint();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.snarl.clone());
            self.snarl = next;
            self.mutation_gen = self.mutation_gen.wrapping_add(1);
            self.sync_committed_fingerprint();
        }
    }

    /// Commit-on-settle undo capture for in-node value/param edits — sliders,
    /// drag-values, color pickers, text fields, dropdowns, toggles, point
    /// editors. These write straight into `node.params` (or a nested subpatch's
    /// params, for pinned widgets) without changing node/wire counts, so the
    /// structural-mutation detector misses them.
    ///
    /// Strategy: snapshot the pre-gesture state the moment params first diverge
    /// under user input, hold it untouched while the user keeps dragging/typing,
    /// and push exactly ONE undo entry when the interaction ends. `pre`, when
    /// supplied, is a pre-mutation snarl clone already taken this frame (reused
    /// as the baseline to avoid a second clone).
    ///
    /// Called from `Canvas::show()` AND from the Easy-mode pinned-body render
    /// path (`easy::center_panel`), which mutates `self.snarl` directly without
    /// going through `show()`. Must be invoked only on frames that could mutate
    /// the snarl (gate on interaction) so idle frames stay allocation-free.
    pub(crate) fn track_value_edits(
        &mut self,
        ctx: &egui::Context,
        pre: Option<&Snarl<NodeData>>,
    ) {
        let now_fp = self.snarl_fingerprint();
        // Lazily seed the committed fingerprint on the first interaction frame
        // after construction/load so a no-op interaction isn't read as an edit.
        if self.committed_fingerprint == 0 && self.pending_value_baseline.is_none() {
            self.committed_fingerprint = now_fp;
        }

        // "Ongoing" = the user is still mid-gesture: a button held down, an
        // active drag, or a focused text field. A click that changes a value
        // and releases on the same frame is NOT ongoing.
        let interaction_ongoing = ctx.input(|i| i.pointer.any_down())
            || ctx.is_using_pointer()
            || ctx.wants_keyboard_input();
        // "This-frame input" = any pointer/keyboard activity this frame,
        // including a press+release click or a key. Distinguishes a user-driven
        // edit from a snarl swap done outside any UI interaction (patch load,
        // device panel, preset apply, undo/redo) which carries no input.
        let input_this_frame = interaction_ongoing
            || ctx.input(|i| {
                i.pointer.any_pressed()
                    || i.pointer.any_released()
                    || i.events.iter().any(|e|
                        matches!(e, egui::Event::Key { pressed: true, .. }
                            | egui::Event::Text(_)))
            });

        if now_fp != self.committed_fingerprint {
            // Capture the pre-gesture baseline on the first diverging frame.
            if self.pending_value_baseline.is_none() {
                if input_this_frame {
                    // User-driven value edit — stash the pre-gesture snapshot as
                    // the future undo entry (reuse this frame's clone when given).
                    self.pending_value_baseline =
                        Some(pre.cloned().unwrap_or_else(|| self.snarl.clone()));
                } else {
                    // Divergence with NO input this frame came from outside a UI
                    // interaction — a patch load, device-panel add/remove, preset
                    // apply, or an undo/redo that replaced the snarl. Those paths
                    // own their undo (or intentionally have none); adopt the new
                    // state as the committed baseline without an undo entry.
                    self.committed_fingerprint = now_fp;
                }
            }
            // Commit once the gesture has settled. `interaction_ongoing` stays
            // true for every frame of a slider/point drag or while a text field
            // is focused, so we never commit mid-gesture — the single held
            // baseline coalesces the whole gesture into one undo entry. The
            // instant it goes false (pointer released, focus left) the value is
            // final, so we commit immediately. This also covers a same-frame
            // click+release (toggle, dropdown, color swatch), which captures and
            // commits in one pass so a baseline is never left dangling.
            if !interaction_ongoing {
                if let Some(baseline) = self.pending_value_baseline.take() {
                    self.push_snapshot(baseline);
                }
                self.committed_fingerprint = now_fp;
            }
        } else if self.pending_value_baseline.is_some() {
            // User dragged the value back to where it started (or the edit was
            // reverted) — nothing actually changed, so drop the pending baseline
            // without polluting the undo stack.
            self.pending_value_baseline = None;
        }
    }

    /// Return a clone of the current clipboard for app-level cross-boundary paste.
    pub(crate) fn clipboard(&self) -> Option<ClipboardData> {
        self.clipboard.clone()
    }

    /// Seed the canvas clipboard from an external source before calling paste().
    /// Used by FlexInputApp to implement cross-boundary paste: the app writes
    /// app_clipboard into the target canvas, then calls paste() via the normal
    /// Ctrl+V path.
    pub(crate) fn set_clipboard(&mut self, data: ClipboardData) {
        self.clipboard = Some(data);
    }

    /// Insert AutoMap Splitter and Collector bridge nodes at the boundary when
    /// pasting across a patch/sub-patch boundary (D-04 item 3).
    ///
    /// For each pin name in `boundary_pins`, this inserts one AutoMap Splitter
    /// node (module_id = "module.automap_split") and one AutoMap Collector node
    /// (module_id = "module.automap_collect") in the target canvas, positioned
    /// near `base_pos`. Non-AutoMap boundary wires should be dropped by the caller.
    ///
    /// Returns the NodeIds of inserted bridge nodes so the caller can connect them.
    ///
    /// RESERVED: built for the cross-boundary paste flow (D-04 item 3) which
    /// was never wired up — paste currently drops boundary wires instead.
    #[allow(dead_code)]
    pub(crate) fn insert_automap_bridge(
        &mut self,
        boundary_pins: &[String],
        base_pos: egui::Pos2,
    ) -> Vec<NodeId> {
        self.push_undo();
        let mut bridge_ids = Vec::new();
        for (i, pin_name) in boundary_pins.iter().enumerate() {
            let offset = egui::vec2(0.0, i as f32 * 60.0);
            // Insert AutoMap Splitter (source side of boundary)
            let mut splitter_params = HashMap::new();
            splitter_params.insert("pin".into(), serde_json::Value::String(pin_name.clone()));
            let splitter_id = self.snarl.insert_node(
                base_pos + offset,
                NodeData {
                    module_id: "module.automap_split".into(),
                    display_name: format!("AutoMap Split: {pin_name}"),
                    category: "AutoMap".into(),
                    inputs: vec![],
                    outputs: vec![],
                    params: splitter_params,
                    subpatch: None,
                    extra: crate::canvas::node::NodeExtra::default(),
                },
            );
            // Insert AutoMap Collector (destination side of boundary)
            let mut collector_params = HashMap::new();
            collector_params.insert("pin".into(), serde_json::Value::String(pin_name.clone()));
            let collector_id = self.snarl.insert_node(
                base_pos + offset + egui::vec2(160.0, 0.0),
                NodeData {
                    module_id: "module.automap_collect".into(),
                    display_name: format!("AutoMap Collect: {pin_name}"),
                    category: "AutoMap".into(),
                    inputs: vec![],
                    outputs: vec![],
                    params: collector_params,
                    subpatch: None,
                    extra: crate::canvas::node::NodeExtra::default(),
                },
            );
            bridge_ids.push(splitter_id);
            bridge_ids.push(collector_id);
        }
        bridge_ids
    }

    /// Copy selected nodes (and internal wires) to the clipboard.
    fn copy_selected(&mut self, selected: &[NodeId]) {
        if selected.is_empty() { return; }

        let nodes: Vec<(egui::Pos2, NodeData)> = selected.iter()
            .filter_map(|&id| self.snarl.get_node_info(id).map(|n| (n.pos, n.value.clone())))
            .filter(|(_, d)| !matches!(d.module_id.as_str(), "device.source" | "device.sink"))
            .collect();

        let selected_set: HashSet<NodeId> = selected.iter().copied().collect();
        let id_to_idx: HashMap<NodeId, usize> = selected.iter()
            .enumerate()
            .filter_map(|(i, &id)| self.snarl.get_node(id).is_some().then_some((id, i)))
            .collect();

        let internal_wires: Vec<(usize, usize, usize, usize)> = self.snarl.wires()
            .filter(|(out, inp)| selected_set.contains(&out.node) && selected_set.contains(&inp.node))
            .filter_map(|(out, inp)| {
                let from_idx = *id_to_idx.get(&out.node)?;
                let to_idx   = *id_to_idx.get(&inp.node)?;
                Some((from_idx, out.output, to_idx, inp.input))
            })
            .collect();

        self.clipboard = Some(ClipboardData { nodes, internal_wires });
        self.clipboard_gen = self.clipboard_gen.wrapping_add(1);
    }

    /// Paste clipboard nodes offset by a fixed amount, restoring internal wires.
    ///
    /// Wire indices are validated against the copied node pin counts before
    /// connecting; stale or malformed entries are silently dropped so that a
    /// tampered clipboard cannot cause a panic or corrupt the graph.
    fn paste(&mut self) {
        let clipboard = match self.clipboard.clone() { Some(c) => c, None => return };
        self.push_undo();
        let offset = egui::vec2(40.0, 40.0);
        let new_ids: Vec<NodeId> = clipboard.nodes.iter()
            .map(|(pos, data)| self.snarl.insert_node(*pos + offset, data.clone()))
            .collect();
        for (from_idx, from_pin, to_idx, to_pin) in clipboard.internal_wires {
            // Bounds-check node indices first.
            if from_idx >= new_ids.len() || to_idx >= new_ids.len() { continue; }
            // Bounds-check pin indices against the node's declared pin counts.
            // This guards against stale ClipboardData whose wire indices no longer
            // match the (possibly edited) node pin lists (T-04-01).
            let from_pin_ok = clipboard.nodes.get(from_idx)
                .map_or(false, |(_, d)| from_pin < d.outputs.len());
            let to_pin_ok   = clipboard.nodes.get(to_idx)
                .map_or(false, |(_, d)| to_pin   < d.inputs.len());
            if !from_pin_ok || !to_pin_ok { continue; }
            self.snarl.connect(
                OutPinId { node: new_ids[from_idx], output: from_pin },
                InPinId  { node: new_ids[to_idx],   input:  to_pin  },
            );
        }
    }

    /// Delete selected nodes and attempt to bridge wires around them where types are compatible.
    fn delete_selected_with_rewire(&mut self, selected: &[NodeId]) {
        let selected_set: HashSet<NodeId> = selected.iter().copied().collect();
        let all_wires: Vec<(OutPinId, InPinId)> = self.snarl.wires().collect();

        for &b in selected {
            let incoming: Vec<OutPinId> = all_wires.iter()
                .filter(|(out, inp)| inp.node == b && !selected_set.contains(&out.node))
                .map(|(out, _)| *out)
                .collect();
            let outgoing: Vec<InPinId> = all_wires.iter()
                .filter(|(out, inp)| out.node == b && !selected_set.contains(&inp.node))
                .map(|(_, inp)| *inp)
                .collect();

            for &a_out in &incoming {
                let a_type = self.snarl.get_node(a_out.node)
                    .and_then(|n| n.outputs.get(a_out.output))
                    .map(|p| p.signal_type);
                for &c_in in &outgoing {
                    let c_type = self.snarl.get_node(c_in.node)
                        .and_then(|n| n.inputs.get(c_in.input))
                        .map(|p| p.signal_type);
                    if let (Some(at), Some(ct)) = (a_type, c_type) {
                        if ct.accepts(at) {
                            self.snarl.connect(a_out, c_in);
                        }
                    }
                }
            }
        }

        for &node in selected {
            if self.snarl.get_node(node).is_some() {
                self.snarl.remove_node(node);
            }
        }
    }

    pub fn show(
        &mut self,
        descriptors: &[ModuleDescriptor],
        live_device_ids: &HashSet<String>,
        live_signals: &HashMap<(String, String), Signal>,
        panic_shortcut: &crate::app::PanicShortcut,
        physical_devices: &[PhysicalDevice],
        device_rates: &HashMap<String, u32>,
        param_defaults: DeviceParamDefaults,
        ui: &mut egui::Ui,
        automap_parent: Option<crate::canvas::viewer::AutomapGlowParent<'_>>,
        ping_requests: Option<&crate::easy::io_panel::PingRequests>,
    ) -> Option<NodeId> {
        let ctx = ui.ctx().clone();

        // Clear any stale AutoMap-chip header-rect stash for nodes that
        // were ADDED THIS FRAME. Slab reuses freed NodeId slots, so a
        // newly-spawned device.source / device.sink can inherit the
        // previous occupant's stashed rect — which snarl's show_output
        // then reads on frame 1 before the new node's header gets to
        // refresh it, producing a one-frame chip flash at the old
        // location. We track this via spawn_glow which is populated by
        // the add_* methods at insertion time.
        {
            let now = std::time::Instant::now();
            for (&node_id, &spawn_t) in self.spawn_glow.iter() {
                // Only clear within the first ~50 ms; after that the
                // node has redrawn its header at least once and the
                // stash is current.
                if now.duration_since(spawn_t).as_millis() > 50 { continue; }
                // AutoMap pin Y is now derived directly from snarl's per-frame
                // `pin_ui.clip_rect()` via `automap_chevron_y` — no cross-frame
                // cache to invalidate on node spawn.
                let _ = node_id;
            }
        }

        // Refresh node/header frame fills from the current egui visuals so
        // theme changes apply to the canvas immediately. We rebuild only
        // the fill colors; the rest of `self.style` (collapsible, pins,
        // shadow, etc.) was set in `Canvas::new` and stays put.
        {
            let v = &ui.visuals().clone();
            let panel = v.panel_fill;
            let darken = |c: egui::Color32, n: i16| {
                let f = |x: u8| (x as i16 - n).clamp(0, 255) as u8;
                egui::Color32::from_rgba_unmultiplied(f(c.r()), f(c.g()), f(c.b()), c.a())
            };
            let lighten = |c: egui::Color32, n: i16| darken(c, -n);
            // Pick a body fill that's slightly different from the panel
            // so nodes pop against the canvas; light theme wants a darker
            // node, dark theme wants a slightly brighter node.
            let is_dark = v.dark_mode;
            let body_rgb = if is_dark { lighten(panel, 6) } else { darken(panel, 14) };
            let body_fill = egui::Color32::from_rgba_unmultiplied(
                body_rgb.r(), body_rgb.g(), body_rgb.b(), 200);
            let header_fill = if is_dark {
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 38)
            } else {
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 18)
            };
            let border_color = if is_dark {
                egui::Color32::from_rgba_unmultiplied(70, 70, 76, 200)
            } else {
                egui::Color32::from_rgba_unmultiplied(150, 150, 158, 200)
            };
            if let Some(f) = self.style.node_frame.as_mut() {
                f.fill = body_fill;
                f.stroke = egui::Stroke::new(1.0, border_color);
            }
            if let Some(f) = self.style.header_frame.as_mut() {
                f.fill = header_fill;
            }

            // See-through mode: thin the snarl canvas backdrop ONLY
            // (nodes/headers above keep their opaque fills). We build a
            // bg_frame that's byte-identical to snarl's default
            // `Frame::canvas(style)` except the fill alpha is reduced —
            // same inner_margin, corner_radius, stroke, and stroke
            // color. This preserves the rect layout the Virtual
            // Devices / Physical Devices tab labels anchor against.
            //
            // In opaque mode we set `bg_frame = None` so snarl uses
            // its default — anything else risks subtle drift from
            // Frame::canvas's actual values across egui versions.
            let see_through_on: bool = ui.ctx().data(|d|
                d.get_temp::<bool>(egui::Id::new(SEE_THROUGH_DATA_KEY))
            ).unwrap_or(false);
            let see_through_alpha: f32 = ui.ctx().data(|d|
                d.get_temp::<f32>(egui::Id::new(SEE_THROUGH_ALPHA_KEY))
            ).unwrap_or(1.0);
            if see_through_on {
                // See-through mode: the snarl backdrop fill (between
                // modules) gets the user-chosen alpha so the desktop
                // bleeds through. Stroke / inner_margin / corner all
                // stay byte-identical to `Frame::canvas(style)` — only
                // the FILL alpha differs from opaque mode. Module
                // bodies, headers, wires, and frame outline keep
                // their normal appearance.
                let base = v.extreme_bg_color;
                let a = (see_through_alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
                let fill = egui::Color32::from_rgba_unmultiplied(
                    base.r(), base.g(), base.b(), a,
                );
                self.style.bg_frame = Some(
                    egui::Frame::new()
                        .inner_margin(2)
                        .corner_radius(v.widgets.noninteractive.corner_radius)
                        .fill(fill)
                        .stroke(v.window_stroke())
                );
            } else {
                self.style.bg_frame = None;
            }
        }

        // ── Pre-show snapshot for viewer-driven mutations ─────────────────────
        // Only clone the snarl when this frame could plausibly mutate it.
        // Snarl's built-in `show()` mutates on pointer drag (node move),
        // wire drag/disconnect, etc. — all gated on pointer activity. When
        // the user is idle, `show()` is read-only, so the clone is pure
        // waste. At 60 fps with a 50-node graph this clone dominates the
        // frame; gating it drops the cost to zero on idle frames.
        //
        // We snapshot when:
        //   - any pointer button is currently down (drag in progress), OR
        //   - the pointer just released this frame (drag completed), OR
        //   - any keyboard key is pressed this frame.
        // False positives are fine (we just pay the clone); false negatives
        // would break undo, so the conditions are intentionally generous.
        let needs_snapshot = ui.ctx().input(|i| {
            i.pointer.any_down()
                || i.pointer.any_released()
                || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
        });
        let pre_snapshot = if needs_snapshot {
            Some(self.snarl.clone())
        } else {
            None
        };
        let pre_counts = (
            self.snarl.nodes_ids_data().count(),
            self.snarl.wires().count(),
        );

        let mut viewer = FlexViewer {
            descriptors,
            ctx: ctx.clone(),
            live_device_ids,
            live_signals,
            device_rates,
            panic_shortcut,
            physical_devices,
            pending_wire_menu: None,
            rename_request: None,
            replace_request: None,
            edit_subpatch_request: None,
            is_inner_canvas: self.is_inner,
            expose_module_request: None,
            pinned_inner_ids: self.pinned_inner_ids.clone(),
            group_request: false,
            push_undo_request: false,
            param_defaults,
            calibrate_request: None,
            automap_parent,
            ping_requests,
            view_salt: self.view_salt,
        };
        // Capture the snarl_id BEFORE show so we can manipulate SnarlState
        // (zoom / pan) from the zoom-control overlay below. The salt is
        // per-canvas (`view_salt`) so each tab / sub-patch keeps its own
        // pan/zoom instead of sharing one transform via a constant id.
        let id_salt = ("flexinput_canvas", self.view_salt);
        let snarl_id = ui.make_persistent_id(id_salt);
        let snarl_rect = ui.available_rect_before_wrap();
        {
            puffin::profile_scope!("snarl_show");
            self.snarl.show(&mut viewer, &self.style, id_salt, ui);
        }
        let calibrate_request = viewer.calibrate_request;

        // Snapshot the current viewport-center in canvas space so new-node
        // spawns can land at the user's current view rather than (0,0).
        {
            use egui_snarl::ui::SnarlState;
            let st = SnarlState::load(ui.ctx(), snarl_id, &self.snarl, snarl_rect, 0.2, 2.0);
            let t = st.to_global();
            let center = snarl_rect.center();
            let canvas_center = (center - t.translation) / t.scaling;
            self.last_view_center_canvas = Some(egui::pos2(canvas_center.x, canvas_center.y));
        }

        // ── Edge auto-scroll while dragging a wire ──────────────────────────
        // When the user drags a new wire out of an inlet/outlet pin and the
        // cursor approaches a viewport edge, pan the canvas toward that edge so
        // a distant module that doesn't fit on screen (at a usable zoom) can be
        // reached without zooming out. Gated on an in-progress wire drag
        // (`SnarlState::has_new_wires`) per the request; the pan speed scales
        // with how deep the cursor is into the edge band and is framerate-
        // independent (dt-scaled), and we request a repaint so it keeps
        // scrolling smoothly while the cursor is held near the edge.
        {
            use egui_snarl::ui::SnarlState;
            let mut st = SnarlState::load(ui.ctx(), snarl_id, &self.snarl, snarl_rect, 0.2, 2.0);
            if st.has_new_wires() {
                let cursor = ui.ctx().input(|i| i.pointer.latest_pos());
                // Edge band = 15% of each axis (the "10-20%" the request asked
                // for). Only auto-scroll while the cursor is near the canvas
                // (inside the rect expanded by one band), so dragging far into
                // another panel doesn't pan.
                let band = egui::vec2(snarl_rect.width() * 0.15, snarl_rect.height() * 0.15);
                if let Some(c) = cursor.filter(|c| snarl_rect.expand2(band).contains(*c)) {
                    let mut dir = egui::Vec2::ZERO;
                    if c.x < snarl_rect.left() + band.x {
                        dir.x += ((snarl_rect.left() + band.x - c.x) / band.x).clamp(0.0, 1.0);
                    } else if c.x > snarl_rect.right() - band.x {
                        dir.x -= ((c.x - (snarl_rect.right() - band.x)) / band.x).clamp(0.0, 1.0);
                    }
                    if c.y < snarl_rect.top() + band.y {
                        dir.y += ((snarl_rect.top() + band.y - c.y) / band.y).clamp(0.0, 1.0);
                    } else if c.y > snarl_rect.bottom() - band.y {
                        dir.y -= ((c.y - (snarl_rect.bottom() - band.y)) / band.y).clamp(0.0, 1.0);
                    }
                    if dir != egui::Vec2::ZERO {
                        // Max ~900 logical px/sec at the very edge. Panning the
                        // view toward an edge = translating the graph the other
                        // way (translation maps graph→screen), so a positive
                        // `dir` at the left edge (content to the left) moves the
                        // graph right, revealing it.
                        const MAX_SPEED: f32 = 900.0;
                        let dt = ui.input(|i| i.stable_dt).clamp(1.0 / 240.0, 1.0 / 30.0);
                        let t = st.to_global();
                        st.set_to_global(egui::emath::TSTransform {
                            translation: t.translation + dir * MAX_SPEED * dt,
                            scaling: t.scaling,
                        });
                        st.store(&self.snarl, ui.ctx());
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        // ── One-shot view action (e.g. center / zoom-to-fit after patch load) ─
        if let Some(action) = self.pending_view_action.take() {
            use egui_snarl::ui::{NodeState, SnarlState};
            const MIN_SCALE: f32 = 0.2;
            const MAX_SCALE: f32 = 2.0;
            // Build bounding box of all nodes — uses each node's measured
            // rect (top-left + size) so wide nodes aren't clipped on the
            // right edge of the framed view.
            let style = ui.ctx().style();
            let mut bb = egui::Rect::NOTHING;
            for (node_id, info) in self.snarl.nodes_ids_data() {
                let ns_id = snarl_id.with(("snarl-node", node_id));
                let ns = NodeState::load(ui.ctx(), ns_id, &style.spacing);
                let openness = if info.open { 1.0 } else { 0.0 };
                bb = bb.union(ns.node_rect(info.pos, openness));
            }
            if bb.is_finite() {
                let mut state = SnarlState::load(
                    ui.ctx(), snarl_id, &self.snarl, snarl_rect, MIN_SCALE, MAX_SCALE,
                );
                match action {
                    PendingViewAction::Center => {
                        // Keep current zoom; pan so the bb centroid lands
                        // on the viewport center.
                        let t = state.to_global();
                        let target_canvas = bb.center();
                        let viewport_center = snarl_rect.center();
                        let new_translation = viewport_center.to_vec2()
                            - target_canvas.to_vec2() * t.scaling;
                        state.set_to_global(egui::emath::TSTransform {
                            translation: new_translation,
                            scaling: t.scaling,
                        });
                    }
                    PendingViewAction::ZoomToFit => {
                        bb = bb.expand(100.0);
                        state.look_at(bb, snarl_rect, MIN_SCALE, MAX_SCALE);
                    }
                }
                state.store(&self.snarl, ui.ctx());
                // We modified the view after the snarl already rendered
                // this frame; request another paint so the user sees the
                // new framing immediately.
                request_repaint_throttled(ui.ctx());
            }
        }

        // ── Spawn-glow overlay ────────────────────────────────────────────────
        // Paint a fading outline pulse around recently-added nodes so the
        // user sees where they landed (especially handy when they spawn
        // off-screen after a pan, before we centred them).
        draw_spawn_glow(ui, snarl_id, snarl_rect, &self.snarl, &mut self.spawn_glow);

        // ── Zoom-control overlay (lower-right corner) ────────────────────────
        draw_zoom_controls(ui, snarl_id, snarl_rect, &self.snarl);

        // ── Detect structural mutations from viewer callbacks ─────────────────
        let post_counts = (
            self.snarl.nodes_ids_data().count(),
            self.snarl.wires().count(),
        );
        // Tracks whether ANY undo entry was committed this frame (structural
        // here, or rename/replace/group/wire-menu paths below). The value-edit
        // detector at the end of `show()` skips committing when this is set, so
        // a single user action never produces two undo entries.
        let mut committed_this_frame = false;
        if pre_counts != post_counts || viewer.push_undo_request {
            // Keep `pre_snapshot` intact for the value-edit detector by cloning
            // here; this extra clone only happens on the rare frame where a
            // structural mutation lands, never on plain value-drag frames.
            if let Some(snap) = pre_snapshot.clone() {
                self.push_snapshot(snap);
            } else {
                // Snapshot was skipped (gating thought the frame was idle)
                // but a mutation slipped through. Fall back to cloning the
                // current (post-mutation) snarl so the undo history at
                // least doesn't get out of sync. Worst case: one undo
                // step replays itself. This branch should be very rare —
                // if it fires often, the gating heuristic above needs to
                // be widened.
                self.push_snapshot(self.snarl.clone());
            }
            committed_this_frame = true;
        }

        if let Some(pending) = viewer.pending_wire_menu {
            self.wire_ctx_menu = Some(pending);
            self.wire_ctx_just_opened = true;
        }

        // ── Hot-swap: replace device node ─────────────────────────────────────
        if let Some((node_id, dev_idx)) = viewer.replace_request {
            if let Some(new_device) = physical_devices.get(dev_idx) {
                self.push_undo();
                replace_device_node(&mut self.snarl, node_id, new_device);
            }
        }

        // ── Rename popup ──────────────────────────────────────────────────────
        if let Some(node_id) = viewer.rename_request {
            let current = self.snarl.get_node(node_id)
                .map(|n| n.display_name.clone())
                .unwrap_or_default();
            let pos = ui.ctx().input(|i| i.pointer.latest_pos().unwrap_or_default());
            self.rename_state = Some((node_id, current, pos));
        }

        self.pending_edit_subpatch  = viewer.edit_subpatch_request;
        self.pending_expose_module  = viewer.expose_module_request;
        // Stash group_request flag; acted on after `selected` is fetched below.
        let group_from_menu = viewer.group_request;

        let mut commit_name: Option<(egui_snarl::NodeId, String)> = None;
        let mut close_rename = false;

        if let Some((node_id, ref mut buf, pos)) = self.rename_state {
            let mut open = true;
            egui::Window::new("Rename")
                .id(egui::Id::new("rename_module_window"))
                .fixed_pos(pos)
                .resizable(false)
                .collapsible(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(buf)
                            .desired_width(200.0)
                            .hint_text("Module name"),
                    );
                    if !resp.has_focus() {
                        resp.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            commit_name = Some((node_id, buf.clone()));
                            close_rename = true;
                        }
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            close_rename = true;
                        }
                    });
                });
            if !open {
                close_rename = true;
            }
        }

        if let Some((nid, name)) = commit_name {
            if self.snarl.get_node(nid).map(|n| n.display_name != name).unwrap_or(false) {
                self.push_undo();
            }
            if let Some(node) = self.snarl.get_node_mut(nid) {
                node.display_name = name;
            }
        }
        if close_rename {
            self.rename_state = None;
        }

        // ── Wire right-click context menu ─────────────────────────────────────
        if let Some((out_id, in_id, pos)) = self.wire_ctx_menu {
            // Read pin signal types for filtering compatible modules.
            let out_sig = self.snarl.get_node(out_id.node)
                .and_then(|n| n.outputs.get(out_id.output))
                .map(|p| p.signal_type);
            let in_sig = self.snarl.get_node(in_id.node)
                .and_then(|n| n.inputs.get(in_id.input))
                .map(|p| p.signal_type);

            // Pre-collect compatible modules grouped by category.
            let mut cats: Vec<(&str, Vec<usize>)> = vec![];
            for (i, d) in descriptors.iter().enumerate() {
                if d.inputs.is_empty() || d.outputs.is_empty() { continue; }
                let in_ok = out_sig.map_or(true, |t| d.inputs.iter().any(|p| p.signal_type.accepts(t)));
                let out_ok = in_sig.map_or(true, |t| d.outputs.iter().any(|p| t.accepts(p.signal_type)));
                if in_ok && out_ok {
                    if let Some(entry) = cats.iter_mut().find(|(c, _)| *c == d.category) {
                        entry.1.push(i);
                    } else {
                        cats.push((d.category, vec![i]));
                    }
                }
            }

            let mut close = false;
            let mut delete = false;
            let mut insert_idx: Option<usize> = None;

            let area_resp = egui::Area::new(egui::Id::new("wire_ctx_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(150.0);
                        if ui.button("✖ Delete wire").clicked() {
                            delete = true;
                            close = true;
                        }
                        if !cats.is_empty() {
                            ui.separator();
                            ui.label(egui::RichText::new("Insert between…").small().weak());
                            for (cat, indices) in &cats {
                                ui.menu_button(*cat, |ui| {
                                    for &i in indices {
                                        if ui.button(descriptors[i].display_name).clicked() {
                                            insert_idx = Some(i);
                                            close = true;
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        }
                    });
                });

            if delete {
                self.push_undo();
                self.snarl.disconnect(out_id, in_id);
            }
            if let Some(i) = insert_idx {
                self.push_undo();
                insert_between(&mut self.snarl, &descriptors[i], out_id, in_id);
            }

            // Close on click outside (skip the frame the menu first appeared).
            if !self.wire_ctx_just_opened {
                let ptr = ui.input(|i| i.pointer.latest_pos().unwrap_or_default());
                let clicked = ui.input(|i| i.pointer.any_click());
                if clicked && !area_resp.response.rect.contains(ptr) {
                    close = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            }
            self.wire_ctx_just_opened = false;
            if close {
                self.wire_ctx_menu = None;
            }
        }

        // ── Keyboard shortcuts and modifier tooltip ────────────────────────────
        // Get selected nodes from snarl's egui state. Must use the same
        // per-canvas salt as the `show` pass above so selection resolves
        // against this canvas's snarl id.
        let snarl_id = ui.make_persistent_id(("flexinput_canvas", self.view_salt));
        let selected = get_selected_nodes(snarl_id, ui.ctx());

        // Group triggered via node right-click menu (viewer sets group_from_menu).
        if group_from_menu && !selected.is_empty() {
            let groupable: Vec<NodeId> = selected.iter().copied()
                .filter(|&id| self.snarl.get_node(id)
                    .map_or(true, |n| !matches!(n.module_id.as_str(), "device.source" | "device.sink")))
                .collect();
            self.group_selected_into_subpatch(&groupable);
        }

        // Only process shortcuts when no overlay is open.
        // Use direct event matching with modifiers — more robust than key_pressed() + separate
        // modifier check, because Ctrl+V may arrive as Event::Paste on some platforms.
        let overlay_open = self.rename_state.is_some()
            || self.wire_ctx_menu.is_some();
        if !overlay_open {
            let del      = ui.input(|i| i.key_pressed(egui::Key::Delete));
            // Ctrl+C may arrive as Event::Copy on Windows instead of Event::Key.
            let ctrl_c   = ui.input(|i| i.events.iter().any(|e| matches!(e,
                egui::Event::Key { key: egui::Key::C, pressed: true, modifiers, .. }
                if modifiers.ctrl && !modifiers.shift
            ) || matches!(e, egui::Event::Copy)));
            // Ctrl+V may arrive as Event::Paste on Windows instead of Event::Key.
            let ctrl_v   = ui.input(|i| i.events.iter().any(|e| matches!(e,
                egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. }
                if modifiers.ctrl && !modifiers.shift
            ) || matches!(e, egui::Event::Paste(_))));
            let ctrl_z   = ui.input(|i| i.events.iter().any(|e| matches!(e,
                egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. }
                if modifiers.ctrl && !modifiers.shift
            )));
            let ctrl_sz  = ui.input(|i| i.events.iter().any(|e| matches!(e,
                egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. }
                if modifiers.ctrl && modifiers.shift
            )));
            let ctrl_g   = ui.input(|i| i.events.iter().any(|e| matches!(e,
                egui::Event::Key { key: egui::Key::G, pressed: true, modifiers, .. }
                if modifiers.ctrl && !modifiers.shift
            )));

            if del && !selected.is_empty() {
                self.push_undo();
                self.delete_selected_with_rewire(&selected);
            }
            if ctrl_c && !selected.is_empty() {
                self.copy_selected(&selected);
                // Write a sentinel to the OS clipboard so egui-winit fires Event::Paste
                // on the next Ctrl+V (it only does so when the OS clipboard is non-empty).
                // We ignore the text content on paste and use our internal ClipboardData.
                ctx.copy_text("__flexinput_nodes__".to_string());
                #[cfg(debug_assertions)]
                eprintln!("[canvas] ctrl_c: copied {} nodes, inner_wires={}",
                    self.clipboard.as_ref().map(|c| c.nodes.len()).unwrap_or(0),
                    self.clipboard.as_ref().map(|c| c.internal_wires.len()).unwrap_or(0));
            }
            if ctrl_v {
                #[cfg(debug_assertions)]
                eprintln!("[canvas] ctrl_v: clipboard has {} nodes",
                    self.clipboard.as_ref().map(|c| c.nodes.len()).unwrap_or(0));
                self.paste();
            }
            if ctrl_z {
                self.undo();
            }
            if ctrl_sz {
                self.redo();
            }
            if ctrl_g && selected.len() >= 2 {
                let groupable: Vec<NodeId> = selected.iter().copied()
                    .filter(|&id| self.snarl.get_node(id)
                        .map_or(true, |n| !matches!(n.module_id.as_str(), "device.source" | "device.sink")))
                    .collect();
                self.group_selected_into_subpatch(&groupable);
            }
        }

        // ── Modifier key tooltip ───────────────────────────────────────────────
        let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let has_sel = !selected.is_empty();
        let has_clip = self.clipboard.is_some();

        let mut lines: Vec<&'static str> = Vec::new();

        if ctrl && shift {
            lines.push("Ctrl+Shift+Z  Redo");
            lines.push("Ctrl+Z        Undo");
        } else if ctrl {
            lines.push("Ctrl+Z        Undo");
            if has_sel { lines.push("Ctrl+C        Copy selected"); }
            if has_clip { lines.push("Ctrl+V        Paste"); }
            if has_sel { lines.push("Ctrl+G        Group into sub-patch"); }
        } else if shift {
            lines.push("Shift+Drag    Multi-select region");
            lines.push("Shift+Click   Toggle node selection");
        } else if has_sel {
            lines.push("Delete        Remove selected");
            lines.push("Ctrl+C        Copy selected");
        }

        if !lines.is_empty() {
            egui::Area::new(egui::Id::new("modifier_tooltip"))
                .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style())
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_min_width(180.0);
                            for line in &lines {
                                ui.label(egui::RichText::new(*line).small().monospace());
                            }
                        });
                });
        }

        // ── Value/param-edit undo capture (commit-on-settle) ──────────────────
        // Structural changes (add/delete/wire/paste/rename/group/replace) are
        // already committed above. `track_value_edits` catches the rest: in-node
        // value edits — sliders, drag-values, color pickers, text fields,
        // dropdowns, toggles, point editors — which write straight into
        // `node.params` during `show()` without changing node/wire counts. We
        // only run it on frames that could plausibly mutate the snarl (the same
        // `needs_snapshot` gate the pre-show clone uses) so idle frames stay
        // allocation-free.
        if needs_snapshot && !committed_this_frame {
            self.track_value_edits(&ctx, pre_snapshot.as_ref());
        }

        calibrate_request
    }

    /// Return a sensible canvas-space spawn position for a new node:
    /// the center of the current viewport if known, else a fallback.
    /// Populated each frame from the snarl show; if `show` hasn't been
    /// called yet this returns a stacked default.
    pub fn spawn_position(&self) -> egui::Pos2 {
        if let Some(p) = self.last_view_center_canvas {
            return p;
        }
        let n = self.snarl.nodes_ids_data().count();
        egui::pos2(80.0, 80.0 + n as f32 * 220.0)
    }

    /// Add a physical device as a source node. No-op if already present.
    pub fn add_device_source(
        &mut self,
        device: &PhysicalDevice,
        default_collapsed: bool,
        defaults: DeviceParamDefaults,
    ) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.source"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
        });
        if already_present {
            return;
        }

        let outputs = device
            .outputs
            .iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let inputs = device
            .inputs
            .iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device.id.clone()));
        params.insert("deadzone".to_string(), Value::from(defaults.stick_deadzone as f64));
        params.insert("gyro_multiplier".to_string(), Value::from(defaults.gyro_mult as f64));
        params.insert("output_pin_ids".to_string(), Value::Array(
            device.outputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));
        params.insert("input_pin_ids".to_string(), Value::Array(
            device.inputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));

        // Count existing source nodes with the same base display name.
        let base = &device.display_name;
        let same_name_ids: Vec<NodeId> = self.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == "device.source"
                && device_base_name(&n.value.display_name) == base.as_str())
            .map(|(id, _)| id)
            .collect();
        let inst = same_name_ids.len(); // 0-based index for the new node

        // When the second device of this name is added, retroactively number the first one.
        if inst == 1 {
            if let Some(first_id) = same_name_ids.into_iter().next() {
                if let Some(n) = self.snarl.get_node_mut(first_id) {
                    n.display_name = format!("{} #1", base);
                }
            }
        }

        let display_name = if inst == 0 {
            base.clone()
        } else {
            format!("{} #{}", base, inst + 1)
        };

        let node = NodeData {
            module_id: "device.source".to_string(),
            display_name,
            category: "Device".to_string(),
            inputs,
            outputs,
            params,
            subpatch: None,
            extra: Default::default(),
        };

        let pos = self.spawn_position();
        let new_id = if default_collapsed {
            self.snarl.insert_node_collapsed(pos, node)
        } else {
            self.snarl.insert_node(pos, node)
        };
        self.spawn_glow.insert(new_id, std::time::Instant::now());
    }

    /// Add a physical device's input pins as a sink node (e.g. MIDI OUT port).
    /// No-op if already present (keyed by device id).
    pub fn add_physical_sink(
        &mut self,
        device: &PhysicalDevice,
        default_collapsed: bool,
        _defaults: DeviceParamDefaults,
    ) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
        });
        if already_present {
            return;
        }

        let fixed_count = device.inputs.len();
        let inputs = device.inputs.iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device.id.clone()));
        params.insert("fixed_input_count".to_string(), Value::Number(fixed_count.into()));
        params.insert("input_pin_ids".to_string(), Value::Array(
            device.inputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));

        let base = &device.display_name;
        let same_name_ids: Vec<NodeId> = self.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == "device.sink"
                && device_base_name(&n.value.display_name) == base.as_str())
            .map(|(id, _)| id)
            .collect();
        let inst = same_name_ids.len();

        if inst == 1 {
            if let Some(first_id) = same_name_ids.into_iter().next() {
                if let Some(n) = self.snarl.get_node_mut(first_id) {
                    n.display_name = format!("{} #1", base);
                }
            }
        }

        let display_name = if inst == 0 {
            base.clone()
        } else {
            format!("{} #{}", base, inst + 1)
        };

        let node = NodeData {
            module_id: "device.sink".to_string(),
            display_name,
            category: "Device".to_string(),
            inputs,
            outputs: vec![],
            params,
            subpatch: None,
            extra: Default::default(),
        };

        let pos = self.spawn_position();
        let new_id = if default_collapsed {
            self.snarl.insert_node_collapsed(pos, node)
        } else {
            self.snarl.insert_node(pos, node)
        };
        self.spawn_glow.insert(new_id, std::time::Instant::now());
    }

    /// Add a virtual device as a single sink node with optional feedback output pins.
    /// No-op if already present (keyed by device id).
    pub fn add_virtual_sink(
        &mut self,
        device: &dyn VirtualDevice,
        default_collapsed: bool,
        defaults: DeviceParamDefaults,
    ) {
        self.add_virtual_sink_static(
            device.id(),
            &device.display_name().to_string(),
            device.sink_pins(),
            device.source_pins(),
            default_collapsed,
            defaults,
        );
    }

    /// Add a virtual-device sink node from static pin metadata, without needing a
    /// live `VirtualDevice` instance. Lets the UI place the node immediately while
    /// the device itself is built asynchronously by the device-ops worker (whose
    /// result lands in the shared pool). No-op if a node for `device_id` already
    /// exists. `sink_pins`/`source_pins` are the kind's static layouts (see
    /// `flexinput_virtual::kind_pin_metadata`).
    pub fn add_virtual_sink_static(
        &mut self,
        device_id: &str,
        display_name: &str,
        sink_pins: &[SinkPin],
        source_pins: &[SourcePin],
        default_collapsed: bool,
        defaults: DeviceParamDefaults,
    ) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(device_id)
        });
        if already_present { return; }

        let fixed_count = sink_pins.len();
        let inputs = sink_pins.iter()
            .map(|p: &SinkPin| PinDescriptor::new(p.display_name, p.signal_type))
            .collect();
        let outputs = source_pins.iter()
            .map(|p: &SourcePin| PinDescriptor::new(p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device_id.to_string()));
        params.insert("fixed_input_count".to_string(), Value::Number(fixed_count.into()));
        params.insert("input_pin_ids".to_string(), Value::Array(
            sink_pins.iter().map(|p| Value::String(p.id.to_string())).collect(),
        ));
        params.insert("output_pin_ids".to_string(), Value::Array(
            source_pins.iter().map(|p| Value::String(p.id.to_string())).collect(),
        ));
        // Mouse sensitivity is keymouse-only; harmless on other virtual sinks
        // since their header doesn't surface a slider for it.
        if device_id.starts_with("virtual.keymouse") {
            params.insert("mouse_sensitivity".to_string(),
                Value::from(defaults.mouse_sensitivity as f64));
        } else if device_id.starts_with("virtual.") {
            // Bake the user's default rumble shaping into the node so the pad is
            // self-describing per-patch (changing the Settings default later
            // won't retroactively reshape this saved pad), and so the absence of
            // these keys reliably marks a pre-rumble-defaults patch for
            // `migrate_loaded_snarl` to backfill with the legacy feel.
            params.insert("rumble_floor".to_string(),
                Value::from(defaults.rumble_floor as f64));
            params.insert("rumble_max".to_string(),
                Value::from(defaults.rumble_max as f64));
            params.insert("rumble_exp".to_string(),
                Value::from(defaults.rumble_exp as f64));
        }

        let node = NodeData {
            module_id: "device.sink".to_string(),
            display_name: display_name.to_string(),
            category: "Device".to_string(),
            inputs,
            outputs,
            params,
            subpatch: None,
            extra: Default::default(),
        };

        let pos = self.spawn_position();
        let new_id = if default_collapsed {
            self.snarl.insert_node_collapsed(pos, node)
        } else {
            self.snarl.insert_node(pos, node)
        };
        self.spawn_glow.insert(new_id, std::time::Instant::now());
    }
}

/// Produce a persistence-safe clone of a snarl for saving to disk. For Network
/// Send/Receive nodes whose `net_keep` flag is not set, strip the identity
/// params — the Send node's pasted peer code (`net_peer`) and the Receive node's
/// secret key (`net_secret`) — so a shared patch or a workspace/recovery backup
/// never leaks a pairing code or node key. With `net_keep` ticked the user has
/// opted in and they're preserved (stable code across restarts, travels with the
/// patch). Recurses into sub-patch inner snarls so nested network nodes are
/// covered too.
pub fn sanitize_snarl_for_save(snarl: &Snarl<NodeData>) -> Snarl<NodeData> {
    let mut out = snarl.clone();
    for node in out.nodes_mut() {
        sanitize_node_for_save(node);
    }
    out
}

fn sanitize_node_for_save(node: &mut NodeData) {
    if matches!(node.module_id.as_str(), "module.network_send" | "module.network_recv") {
        let keep = node.params.get("net_keep").and_then(|v| v.as_bool()).unwrap_or(false);
        if !keep {
            node.params.remove("net_peer");
            node.params.remove("net_secret");
        }
    }
    if let Some(sp) = node.subpatch.as_mut() {
        for inner in sp.snarl.nodes_mut() {
            sanitize_node_for_save(inner);
        }
    }
}

impl Canvas {
    /// Serialize the canvas + virtual device list to a `.fxp` file chosen by the user.
    /// Returns the chosen path on success so the caller can update the tab title.
    pub fn save_patch(
        &self,
        virtual_device_ids: Vec<String>,
        bound_exes: Vec<String>,
        auto_bypass: bool,
        easy_preset_path: Option<std::path::PathBuf>,
        overlay: OverlayLayout,
        config: OverlayLayout,
    ) -> Option<std::path::PathBuf> {
        let path = crate::overlay::with_overlay_not_topmost(|| {
            rfd::FileDialog::new()
                .add_filter("FlexInput Patch", &["fxp"])
                .set_file_name("patch.fxp")
                .save_file()
        })?;

        let patch = UiPatch {
            version: 1,
            snarl: sanitize_snarl_for_save(&self.snarl),
            virtual_device_ids,
            bound_exes,
            auto_bypass,
            easy_preset_path,
            overlay,
            config,
        };
        if let Ok(json) = serde_json::to_string_pretty(&patch) {
            let _ = std::fs::write(&path, json);
        }
        Some(path)
    }

    /// Open a `.fxp` file and return the loaded Canvas, virtual device IDs,
    /// bound exes, auto-bypass flag, the path, and the Easy-mode preset path
    /// (if the file recorded one).
    /// Returns `None` if the user cancels or the file is invalid.
    pub fn load_patch() -> Option<(
        Canvas,
        Vec<String>,
        Vec<String>,
        bool,
        std::path::PathBuf,
        Option<std::path::PathBuf>,
        OverlayLayout,
        OverlayLayout,
    )> {
        // Accept both `.fxp` (full patches) and `.fxsp` (sub-patch
        // presets). For .fxsp, build an empty canvas and drop in a
        // single `subpatch` node carrying the loaded UiSubPatch — the
        // user gets a one-click way to open a preset directly from
        // File→Load Patch, and the app.rs caller switches to Easy
        // mode if the result is Easy-compatible.
        let path = crate::overlay::with_overlay_not_topmost(|| {
            rfd::FileDialog::new()
                .add_filter("FlexInput Patch", &["fxp", "fxsp"])
                .add_filter("Full Patch (.fxp)", &["fxp"])
                .add_filter("Sub-Patch (.fxsp)", &["fxsp"])
                .pick_file()
        })?;

        let json = std::fs::read_to_string(&path).ok()?;
        let is_subpatch_file = path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("fxsp"))
            .unwrap_or(false);

        if is_subpatch_file {
            // .fxsp → wrap into an Easy-shaped canvas.
            #[derive(serde::Deserialize)]
            struct SubPatchFileLite {
                #[allow(dead_code)]
                version: u32,
                sub_patch: crate::canvas::node::UiSubPatch,
            }
            let file: SubPatchFileLite = serde_json::from_str(&json).ok()?;
            let sp = file.sub_patch;
            let mut canvas = Canvas::new();
            // Build the outer `subpatch` node with pin descriptors
            // mirrored from the sub-patch's declared pins, so external
            // wires (added later in Advanced mode) line up correctly.
            use flexinput_core::PinDescriptor;
            let inputs: Vec<PinDescriptor> = sp.pins_in.iter()
                .map(|p| PinDescriptor::new(&p.name, p.signal_type))
                .collect();
            let outputs: Vec<PinDescriptor> = sp.pins_out.iter()
                .map(|p| PinDescriptor::new(&p.name, p.signal_type))
                .collect();
            let display = if sp.display_name.is_empty() {
                path.file_stem().map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Sub-patch".into())
            } else {
                sp.display_name.clone()
            };
            let node = NodeData {
                module_id: "subpatch".into(),
                display_name: display,
                category: "Patch".into(),
                inputs,
                outputs,
                params: std::collections::HashMap::new(),
                subpatch: Some(Box::new(sp)),
                extra: Default::default(),
            };
            canvas.snarl.insert_node(canvas.spawn_position(), node);
            return Some((
                canvas,
                Vec::new(),    // no virtual devices yet — Easy mode
                               // adds them when user picks an output
                Vec::new(),    // bound_exes
                false,         // auto_bypass
                path.clone(),
                Some(path),    // record fxsp path so Easy mode's
                               // restore_preset_link can re-link
                OverlayLayout::default(), // presets carry no overlay
                OverlayLayout::default(), // …and no config overlay
            ));
        }

        // .fxp → normal full-patch load path.
        let patch: UiPatch = serde_json::from_str(&json).ok()?;
        let mut canvas = Canvas::new();
        canvas.snarl = patch.snarl;
        migrate_ds4_pin_ids(&mut canvas);
        migrate_loaded_snarl(&mut canvas.snarl);
        // Migrate the legacy device-id list the same way as the snarl, so the
        // caller (which seeds the deploy set from it) doesn't spin up a ViGEm
        // pad alongside the migrated HIDMaestro one. Dedup in case both the old
        // and new id were already present.
        let mut virtual_device_ids: Vec<String> = Vec::new();
        for id in patch.virtual_device_ids {
            let migrated = migrate_vigem_device_id(&id).unwrap_or(id);
            if !virtual_device_ids.contains(&migrated) {
                virtual_device_ids.push(migrated);
            }
        }
        Some((
            canvas,
            virtual_device_ids,
            patch.bound_exes,
            patch.auto_bypass,
            path,
            patch.easy_preset_path,
            patch.overlay,
            patch.config,
        ))
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

/// Rename DS4 pin IDs that were changed between v0.3 and v0.4 so that old
/// patches continue to route correctly.
fn migrate_ds4_pin_ids(canvas: &mut Canvas) {
    const RENAMES: &[(&str, &str)] = &[
        ("l2",           "left_trigger"),
        ("r2",           "right_trigger"),
        ("btn_cross",    "btn_south"),
        ("btn_circle",   "btn_east"),
        ("btn_square",   "btn_west"),
        ("btn_triangle", "btn_north"),
        ("btn_l1",       "btn_lb"),
        ("btn_r1",       "btn_rb"),
        ("btn_l2_dig",   "btn_lt_dig"),
        ("btn_r2_dig",   "btn_rt_dig"),
        ("btn_l3",       "btn_ls"),
        ("btn_r3",       "btn_rs"),
        ("btn_options",  "btn_start"),
        ("btn_share",    "btn_back"),
        ("btn_ps",       "btn_guide"),
    ];
    for (_, node) in canvas.snarl.nodes_ids_data_mut() {
        if node.value.module_id != "device.sink" { continue; }
        if let Some(Value::Array(ids)) = node.value.params.get_mut("input_pin_ids") {
            for id in ids.iter_mut() {
                if let Some(s) = id.as_str() {
                    if let Some(&(_, new)) = RENAMES.iter().find(|&&(old, _)| old == s) {
                        *id = Value::String(new.to_string());
                    }
                }
            }
        }
    }
}

/// Strip a trailing " #N" suffix from a device display name, returning the base name.
fn device_base_name(name: &str) -> &str {
    if let Some(pos) = name.rfind(" #") {
        let suffix = &name[pos + 2..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return &name[..pos];
        }
    }
    name
}

/// Swap a device.source or device.sink node to a different physical device, preserving
/// wire connections by matching pin IDs between the old and new device.
fn replace_device_node(
    snarl: &mut Snarl<NodeData>,
    node_id: NodeId,
    new_device: &PhysicalDevice,
) {
    let Some(node) = snarl.get_node(node_id) else { return };
    let module_id = node.module_id.clone();
    let is_source = module_id == "device.source";
    let is_sink   = module_id == "device.sink";
    if !is_source && !is_sink { return; }

    // Capture old pin ID lists for name-based reconnection.
    let old_out_ids: Vec<String> = node.params
        .get("output_pin_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| Value::as_str(v).unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let old_in_ids: Vec<String> = node.params
        .get("input_pin_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| Value::as_str(v).unwrap_or("").to_string()).collect())
        .unwrap_or_default();

    // Snapshot all wires touching this node before modifying anything.
    let all_wires: Vec<(OutPinId, InPinId)> = snarl.wires().collect();
    let out_conns: Vec<(usize, InPinId)> = all_wires.iter()
        .filter_map(|(o, i)| if o.node == node_id { Some((o.output, *i)) } else { None })
        .collect();
    let in_conns: Vec<(usize, OutPinId)> = all_wires.iter()
        .filter_map(|(o, i)| if i.node == node_id { Some((i.input, *o)) } else { None })
        .collect();

    // Disconnect everything first.
    for &(out_idx, inp) in &out_conns {
        snarl.disconnect(OutPinId { node: node_id, output: out_idx }, inp);
    }
    for &(in_idx, out) in &in_conns {
        snarl.disconnect(out, InPinId { node: node_id, input: in_idx });
    }

    // Update node data.
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.display_name = new_device.display_name.clone();
        node.params.insert("device_id".to_string(), Value::String(new_device.id.clone()));
        if is_source {
            node.outputs = new_device.outputs.iter()
                .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
                .collect();
            node.inputs = new_device.inputs.iter()
                .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
                .collect();
            node.params.insert("output_pin_ids".to_string(), Value::Array(
                new_device.outputs.iter().map(|p| Value::String(p.id.clone())).collect(),
            ));
            node.params.insert("input_pin_ids".to_string(), Value::Array(
                new_device.inputs.iter().map(|p| Value::String(p.id.clone())).collect(),
            ));
        } else {
            node.inputs = new_device.inputs.iter()
                .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
                .collect();
            node.outputs = vec![];
            node.params.insert("fixed_input_count".to_string(),
                Value::Number((new_device.inputs.len() as u64).into()));
            node.params.insert("input_pin_ids".to_string(), Value::Array(
                new_device.inputs.iter().map(|p| Value::String(p.id.clone())).collect(),
            ));
        }
    }

    // Reconnect wires by matching old pin IDs to new pin positions.
    // Falls back to cross-controller aliases when an exact match is absent.
    let new_out_ids: Vec<&str> = new_device.outputs.iter().map(|p| p.id.as_str()).collect();
    let new_in_ids:  Vec<&str> = new_device.inputs.iter().map(|p| p.id.as_str()).collect();

    for (old_idx, target) in out_conns {
        if let Some(pin_id) = old_out_ids.get(old_idx) {
            if let Some(new_idx) = resolve_pin(pin_id, &new_out_ids) {
                snarl.connect(OutPinId { node: node_id, output: new_idx }, target);
            }
        }
    }
    for (old_idx, source) in in_conns {
        if let Some(pin_id) = old_in_ids.get(old_idx) {
            if let Some(new_idx) = resolve_pin(pin_id, &new_in_ids) {
                snarl.connect(source, InPinId { node: node_id, input: new_idx });
            }
        }
    }
}

/// Cross-controller pin equivalence groups (positional / functional equivalents).
const COMPAT_GROUPS: &[&[&str]] = &[
    // Face buttons by position
    &["btn_cross",    "btn_b",      "btn_south"],   // South
    &["btn_circle",   "btn_a",      "btn_east"],    // East
    &["btn_square",   "btn_y",      "btn_west"],    // West
    &["btn_triangle", "btn_x",      "btn_north"],   // North
    // Shoulder bumpers
    &["btn_l1",       "btn_l",      "btn_lb"],
    &["btn_r1",       "btn_r",      "btn_rb"],
    // Triggers: analog float ↔ digital bool
    &["l2",           "left_trigger",  "btn_zl"],
    &["r2",           "right_trigger", "btn_zr"],
    // Stick clicks
    &["btn_l3",       "btn_ls"],
    &["btn_r3",       "btn_rs"],
    // Menu buttons
    &["btn_options",  "btn_plus",   "btn_start"],
    &["btn_share",    "btn_minus",  "btn_back"],
    &["btn_ps",       "btn_home",   "btn_guide"],
    // Extra action buttons (mic mute ↔ capture/screenshot)
    &["btn_mute",     "btn_capture"],
];

/// Subtle outline pulse around recently-added nodes. Queries the real
/// per-node `NodeState` from snarl's egui memory so the halo matches the
/// node's actual screen rect — including after collapse and after snarl's
/// first-frame size measurement. Painted in a foreground layer so the
/// glow sits above the node frame, not behind it.
///
/// Skips the very first frame after spawn because snarl hasn't measured
/// the node yet on that frame; drawing then would produce a halo of the
/// wrong size (and snarl will request a discard / re-paint anyway).
fn draw_spawn_glow(
    ui: &mut egui::Ui,
    snarl_id: egui::Id,
    snarl_rect: egui::Rect,
    snarl: &Snarl<NodeData>,
    glow: &mut std::collections::HashMap<egui_snarl::NodeId, std::time::Instant>,
) {
    use egui_snarl::ui::{NodeState, SnarlState};
    const DURATION_MS: f32 = 600.0;
    const FRAME_SKIP_MS: f32 = 16.0; // skip the first ~one frame
    if glow.is_empty() { return; }
    let now = std::time::Instant::now();
    glow.retain(|_, t| now.duration_since(*t).as_secs_f32() * 1000.0 < DURATION_MS);
    if glow.is_empty() { return; }
    request_repaint_throttled(ui.ctx());

    // Canvas → screen transform.
    let st = SnarlState::load(ui.ctx(), snarl_id, snarl, snarl_rect, 0.2, 2.0);
    let t = st.to_global();

    // Foreground painter — above snarl's node and wire layers.
    let layer = egui::LayerId::new(egui::Order::Foreground, snarl_id.with("spawn_glow"));
    let mut painter = ui.ctx().layer_painter(layer);
    painter.set_clip_rect(snarl_rect);

    let style = ui.ctx().style();
    for (&node_id, &spawn_t) in glow.iter() {
        let age_ms = now.duration_since(spawn_t).as_secs_f32() * 1000.0;
        if age_ms < FRAME_SKIP_MS { continue; }
        let Some(info) = snarl.get_node_info(node_id) else { continue; };

        // Pull the snarl-measured node size for this node.
        let ns_id = snarl_id.with(("snarl-node", node_id));
        let ns = NodeState::load(ui.ctx(), ns_id, &style.spacing);
        // openness 1.0 if open, 0.0 if collapsed (snarl interpolates but
        // we don't need the in-between for a brief flash).
        let openness = if info.open { 1.0 } else { 0.0 };
        let canvas_rect = ns.node_rect(info.pos, openness);
        // Transform to screen space.
        let top_left = canvas_rect.min.to_vec2() * t.scaling + t.translation;
        let size     = canvas_rect.size() * t.scaling;
        let rect     = egui::Rect::from_min_size(top_left.to_pos2(), size);

        let p = (age_ms / DURATION_MS).clamp(0.0, 1.0);
        let alpha = (1.0 - p).powf(1.5);
        let halo  = egui::Color32::from_rgba_unmultiplied(120, 180, 255, (180.0 * alpha) as u8);
        let bloom = egui::Color32::from_rgba_unmultiplied(120, 180, 255, (45.0 * alpha) as u8);

        painter.rect_stroke(
            rect.expand(7.0),
            egui::CornerRadius::same(10),
            egui::Stroke::new(8.0, bloom),
            egui::epaint::StrokeKind::Outside,
        );
        painter.rect_stroke(
            rect.expand(3.0),
            egui::CornerRadius::same(8),
            egui::Stroke::new(2.5, halo),
            egui::epaint::StrokeKind::Outside,
        );
    }
}

/// Shared egui data key for the see-through toggle. The zoom overlay writes
/// here when the eye icon is clicked; `FlexInputApp::update` reads it each
/// frame and reflects the value into `settings.see_through_active` so the
/// panel/window-fill alpha override takes effect.
///
/// Lives in a `Cell`-style data slot so neither the canvas nor the app need
/// to thread a mutable reference through `Canvas::show`. Default value
/// `false` means the very first frame after launch matches the persisted
/// setting once the app writes it back.
pub const SEE_THROUGH_DATA_KEY: &str = "flexinput::see_through_active";
pub const SEE_THROUGH_ALPHA_KEY: &str = "flexinput::see_through_alpha";

/// Lower-right overlay with canvas zoom controls.
/// Layout (left → right): [👁] [−] [100%] [+] [fit].
/// Painted in a foreground egui Area so the strip sits visually above
/// and interactively above the snarl node layer.
fn draw_zoom_controls(
    ui: &mut egui::Ui,
    snarl_id: egui::Id,
    snarl_rect: egui::Rect,
    snarl: &Snarl<NodeData>,
) {
    use egui_snarl::ui::SnarlState;

    const MIN_SCALE: f32 = 0.2;
    const MAX_SCALE: f32 = 2.0;

    let mut state = SnarlState::load(ui.ctx(), snarl_id, snarl, snarl_rect, MIN_SCALE, MAX_SCALE);
    let cur_scale = state.to_global().scaling;
    let pct = (cur_scale * 100.0).round() as i32;

    // Pre-measure the strip so we can compute a bottom-right placement.
    // Layout: [eye] [sep] [-] [zoom%] [+] [fit] ≈ 24 + 8 + 24 + 48 + 24 + 24 + paddings
    let margin = 10.0_f32;
    let est_w  = 196.0_f32;
    let est_h  = 36.0_f32;
    let anchor = egui::pos2(
        snarl_rect.right()  - est_w - margin,
        snarl_rect.bottom() - est_h - margin,
    );

    // Foreground area placed at an absolute screen-space position inside
    // the snarl rect, so it stays anchored to the canvas (not the
    // viewport / side panels).
    let area = egui::Area::new(snarl_id.with("zoom_overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .interactable(true);

    let mut clicked_minus  = false;
    let mut clicked_reset  = false;
    let mut clicked_plus   = false;
    let mut clicked_fit    = false;

    area.show(ui.ctx(), |ui| {
        let bg = ui.visuals().window_fill();
        let frame = egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 230))
            .stroke(egui::Stroke::new(
                1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(6, 4));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;

                // (See-through eye toggle relocated to the title bar so it
                // works from both Easy and Advanced mode — see
                // `app::render_eye_toggle`.)

                clicked_minus = ui.add(egui::Button::new(
                    egui::RichText::new("−").monospace())
                    .min_size(egui::vec2(24.0, 22.0)))
                    .on_hover_text("Zoom out")
                    .clicked();
                // Current-zoom display doubles as the "reset to 100%"
                // button — visually it looks like a button, so the
                // affordance is obvious without a dedicated 100% label.
                clicked_reset = ui.add(egui::Button::new(
                    egui::RichText::new(format!("{}%", pct)).monospace())
                    .min_size(egui::vec2(48.0, 22.0)))
                    .on_hover_text("Reset to 100%")
                    .clicked();
                clicked_plus = ui.add(egui::Button::new(
                    egui::RichText::new("+").monospace())
                    .min_size(egui::vec2(24.0, 22.0)))
                    .on_hover_text("Zoom in")
                    .clicked();
                clicked_fit = ui.add(egui::Button::new(
                    egui::RichText::new("⛶").monospace())
                    .min_size(egui::vec2(24.0, 22.0)))
                    .on_hover_text("Fit all nodes")
                    .clicked();
            });
        });
    });

    if clicked_minus {
        let new_scale = (cur_scale / 1.25).clamp(MIN_SCALE, MAX_SCALE);
        zoom_about(&mut state, snarl_rect.center(), new_scale);
    }
    if clicked_reset {
        zoom_about(&mut state, snarl_rect.center(), 1.0);
    }
    if clicked_plus {
        let new_scale = (cur_scale * 1.25).clamp(MIN_SCALE, MAX_SCALE);
        zoom_about(&mut state, snarl_rect.center(), new_scale);
    }
    if clicked_fit {
        // Match snarl's own double-click "fit-to-view" behavior: union
        // of each node's full RECT (top-left + measured size), not just
        // its top-left position. Without the size, large nodes would
        // poke off the right/bottom of the framed view.
        use egui_snarl::ui::NodeState;
        let style = ui.ctx().style();
        let mut bb = egui::Rect::NOTHING;
        for (node_id, info) in snarl.nodes_ids_data() {
            let ns_id = snarl_id.with(("snarl-node", node_id));
            let ns = NodeState::load(ui.ctx(), ns_id, &style.spacing);
            let openness = if info.open { 1.0 } else { 0.0 };
            let r = ns.node_rect(info.pos, openness);
            bb = bb.union(r);
        }
        if bb.is_finite() {
            bb = bb.expand(100.0);
            state.look_at(bb, snarl_rect, MIN_SCALE, MAX_SCALE);
        }
    }

    state.store(snarl, ui.ctx());
}

/// Apply a new zoom level while keeping `pivot` (screen-space point) fixed.
fn zoom_about(state: &mut egui_snarl::ui::SnarlState, pivot: egui::Pos2, new_scale: f32) {
    let t = state.to_global();
    let cur_scale = t.scaling;
    if (new_scale - cur_scale).abs() < 1e-4 { return; }
    // Solve: new translation such that new_to_global maps the same canvas
    // point under `pivot` to `pivot` again.
    //   pivot_canvas = (pivot - t.translation) / cur_scale
    //   pivot = pivot_canvas * new_scale + new_translation
    let pivot_canvas = (pivot - t.translation) / cur_scale;
    let new_translation = pivot - pivot_canvas * new_scale;
    state.set_to_global(egui::emath::TSTransform {
        translation: new_translation,
        scaling: new_scale,
    });
}

/// Find the position of `old_id` in `candidates`, with alias fallback.
fn resolve_pin(old_id: &str, candidates: &[&str]) -> Option<usize> {
    if let Some(i) = candidates.iter().position(|&id| id == old_id) {
        return Some(i);
    }
    for group in COMPAT_GROUPS {
        if group.contains(&old_id) {
            for &alias in *group {
                if alias != old_id {
                    if let Some(i) = candidates.iter().position(|&id| id == alias) {
                        return Some(i);
                    }
                }
            }
        }
    }
    None
}

// ── Selected-node grouping into a subpatch ────────────────────────────────────

/// Result of attempting to group selected nodes into a subpatch.
#[derive(Debug)]
pub enum GroupResult {
    /// Grouping succeeded; the new subpatch `NodeId` is returned.
    /// Non-AutoMap boundary wires were dropped; AutoMap boundary wires got inlet/outlet ports.
    /// (The payload is only read by unit tests; production callers use `matches!`.)
    #[allow(dead_code)]
    Ok(NodeId),
    /// Nothing to group — the selection was empty.
    EmptySelection,
}

/// Classification of a wire relative to the selected node set.
#[derive(Debug)]
struct BoundaryWire {
    /// Source pin in the outer canvas.
    out_id: OutPinId,
    /// Destination pin in the outer canvas.
    in_id: InPinId,
    /// Signal type carried by the wire (from the output-side pin).
    signal_type: SignalType,
    /// True if data flows FROM outside into the selected set (incoming).
    /// False if data flows FROM inside the selected set to outside (outgoing).
    incoming: bool,
    /// Pin name on the boundary (used as subpatch inlet/outlet display name).
    pin_name: String,
}

/// Attempt to group `selected` nodes into a new `subpatch` node.
///
/// Phase-1 constraint: only wires whose signal type is `AutoMap` or whose
/// output-side pin name is a canonical `ALL_PINS` ID are allowed to cross
/// the subpatch boundary. Non-conforming boundary wires cause the operation
/// to return `GroupResult::NonCanonicalBoundaryPin`.
///
/// On success, selected nodes are removed from the outer canvas, their
/// internal wires are preserved inside the inner snarl, and one
/// `subpatch.inlet` / `subpatch.outlet` node is inserted per boundary wire.
/// The outer canvas receives a single `subpatch` node wired to all former
/// boundary connections. An undo snapshot is taken before any mutations.
pub fn group_into_subpatch(
    snarl: &mut Snarl<NodeData>,
    undo_stack: &mut Vec<Snarl<NodeData>>,
    selected: &[NodeId],
) -> GroupResult {
    if selected.is_empty() {
        return GroupResult::EmptySelection;
    }

    let selected_set: HashSet<NodeId> = selected.iter().copied().collect();

    // ── Classify all wires in the outer canvas ────────────────────────────────

    let all_wires: Vec<(OutPinId, InPinId)> = snarl.wires().collect();

    let mut boundary_wires: Vec<BoundaryWire> = Vec::new();

    for &(out_id, in_id) in &all_wires {
        let from_inside = selected_set.contains(&out_id.node);
        let to_inside   = selected_set.contains(&in_id.node);

        if from_inside == to_inside {
            // Both inside or both outside — internal or fully external; skip.
            continue;
        }

        // Determine signal type from the output-side pin.
        let (sig_type, pin_name) = if from_inside {
            // Outgoing: from selected node's output pin to outside.
            let t = snarl.get_node(out_id.node)
                .and_then(|n| n.outputs.get(out_id.output))
                .map(|p| (p.signal_type, p.name.clone()))
                .unwrap_or((SignalType::Any, String::new()));
            t
        } else {
            // Incoming: from outside into selected node's input pin.
            // Use the output-side type (the signal being produced).
            let t = snarl.get_node(out_id.node)
                .and_then(|n| n.outputs.get(out_id.output))
                .map(|p| (p.signal_type, p.name.clone()))
                .unwrap_or((SignalType::Any, String::new()));
            t
        };

        // ── Boundary wire policy ──────────────────────────────────────────────
        // AutoMap-typed signals or canonical AutoMap pin names get an inlet/outlet
        // port and are reconnected through the subpatch boundary.
        // All other signal types (Float, Bool, Vec2, etc.) are dropped — the
        // external wire is removed and not reconnected. This matches the intent:
        // grouping disconnects non-AutoMap boundary wires; the user can rewire
        // manually or use AutoMap routing for inter-patch signal flow.
        let is_automap_boundary = sig_type == SignalType::AutoMap
            || flexinput_core::automap::ALL_PINS.iter().any(|ap| ap.id == pin_name);

        if !is_automap_boundary {
            // Drop this boundary wire — don't reconnect, don't block grouping.
            continue;
        }

        boundary_wires.push(BoundaryWire {
            out_id,
            in_id,
            signal_type: sig_type,
            incoming: !from_inside,
            pin_name,
        });
    }

    // ── Snapshot undo state before mutating ───────────────────────────────────

    undo_stack.push(snarl.clone());
    if undo_stack.len() > MAX_UNDO {
        undo_stack.remove(0);
    }

    // ── Build inner snarl: copy selected nodes ────────────────────────────────

    let mut inner_snarl: Snarl<NodeData> = Snarl::new();

    // Map from outer NodeId to inner NodeId so we can reconnect internal wires.
    let mut outer_to_inner: HashMap<NodeId, NodeId> = HashMap::new();

    for &outer_id in selected {
        if let Some(info) = snarl.get_node_info(outer_id) {
            let inner_id = inner_snarl.insert_node(info.pos, info.value.clone());
            outer_to_inner.insert(outer_id, inner_id);
        }
    }

    // Restore internal wires inside the inner snarl.
    for &(out_id, in_id) in &all_wires {
        if selected_set.contains(&out_id.node) && selected_set.contains(&in_id.node) {
            if let (Some(&inner_out_node), Some(&inner_in_node)) = (
                outer_to_inner.get(&out_id.node),
                outer_to_inner.get(&in_id.node),
            ) {
                inner_snarl.connect(
                    OutPinId { node: inner_out_node, output: out_id.output },
                    InPinId  { node: inner_in_node,  input:  in_id.input  },
                );
            }
        }
    }

    // ── Insert inlet / outlet nodes for boundary wires ────────────────────────
    // Each boundary wire gets its own inlet or outlet node.
    // pin_index is assigned in order (0, 1, 2, …) separately for inlets and outlets.

    let mut inlet_idx: usize = 0;
    let mut outlet_idx: usize = 0;

    // Map: (outer OutPinId, outer InPinId) → inner inlet NodeId
    let mut inlet_map:  HashMap<(OutPinId, InPinId), NodeId> = HashMap::new();
    // Map: (outer OutPinId, outer InPinId) → inner outlet NodeId
    let mut outlet_map: HashMap<(OutPinId, InPinId), NodeId> = HashMap::new();

    // Centroid of selected nodes for inlet/outlet placement.
    let centroid = {
        let positions: Vec<egui::Pos2> = selected.iter()
            .filter_map(|&id| snarl.get_node_info(id).map(|n| n.pos))
            .collect();
        let n = positions.len() as f32;
        if n > 0.0 {
            egui::pos2(
                positions.iter().map(|p| p.x).sum::<f32>() / n,
                positions.iter().map(|p| p.y).sum::<f32>() / n,
            )
        } else {
            egui::pos2(0.0, 0.0)
        }
    };

    for bw in &boundary_wires {
        if bw.incoming {
            // Insert a subpatch.inlet node in the inner snarl.
            let mut params = HashMap::new();
            params.insert("pin_index".to_string(),
                Value::Number(inlet_idx.into()));
            params.insert("signal_type".to_string(),
                serde_json::to_value(bw.signal_type).unwrap_or(Value::Null));

            let inlet_node = NodeData {
                module_id: "subpatch.inlet".to_string(),
                display_name: bw.pin_name.clone(),
                category: "SubPatch".to_string(),
                inputs: vec![],
                outputs: vec![PinDescriptor::new("out", bw.signal_type)],
                params,
                subpatch: None,
                extra: Default::default(),
            };

            // Position inlets to the left of the centroid.
            let pos = egui::pos2(
                centroid.x - 220.0,
                centroid.y + (inlet_idx as f32 * 60.0),
            );
            let inner_inlet_id = inner_snarl.insert_node(pos, inlet_node);
            inlet_map.insert((bw.out_id, bw.in_id), inner_inlet_id);

            // Wire: inlet.out → inner destination node's input.
            if let Some(&inner_dst) = outer_to_inner.get(&bw.in_id.node) {
                inner_snarl.connect(
                    OutPinId { node: inner_inlet_id, output: 0 },
                    InPinId  { node: inner_dst,       input:  bw.in_id.input },
                );
            }

            inlet_idx += 1;
        } else {
            // Insert a subpatch.outlet node in the inner snarl.
            let mut params = HashMap::new();
            params.insert("pin_index".to_string(),
                Value::Number(outlet_idx.into()));
            params.insert("signal_type".to_string(),
                serde_json::to_value(bw.signal_type).unwrap_or(Value::Null));

            let outlet_node = NodeData {
                module_id: "subpatch.outlet".to_string(),
                display_name: bw.pin_name.clone(),
                category: "SubPatch".to_string(),
                inputs: vec![PinDescriptor::new("in", bw.signal_type)],
                outputs: vec![],
                params,
                subpatch: None,
                extra: Default::default(),
            };

            // Position outlets to the right of the centroid.
            let pos = egui::pos2(
                centroid.x + 220.0,
                centroid.y + (outlet_idx as f32 * 60.0),
            );
            let inner_outlet_id = inner_snarl.insert_node(pos, outlet_node);
            outlet_map.insert((bw.out_id, bw.in_id), inner_outlet_id);

            // Wire: inner source node's output → outlet.in
            if let Some(&inner_src) = outer_to_inner.get(&bw.out_id.node) {
                inner_snarl.connect(
                    OutPinId { node: inner_src,        output: bw.out_id.output },
                    InPinId  { node: inner_outlet_id,  input:  0               },
                );
            }

            outlet_idx += 1;
        }
    }

    // ── Build the outer subpatch node ─────────────────────────────────────────

    use crate::canvas::node::UiSubPatch;
    use flexinput_core::SubPatchPin;

    let pins_in: Vec<SubPatchPin> = boundary_wires.iter()
        .filter(|bw| bw.incoming)
        .map(|bw| SubPatchPin { name: bw.pin_name.clone(), signal_type: bw.signal_type })
        .collect();
    let pins_out: Vec<SubPatchPin> = boundary_wires.iter()
        .filter(|bw| !bw.incoming)
        .map(|bw| SubPatchPin { name: bw.pin_name.clone(), signal_type: bw.signal_type })
        .collect();

    let outer_inputs: Vec<PinDescriptor> = pins_in.iter()
        .map(|p| PinDescriptor::new(p.name.as_str(), p.signal_type))
        .collect();
    let outer_outputs: Vec<PinDescriptor> = pins_out.iter()
        .map(|p| PinDescriptor::new(p.name.as_str(), p.signal_type))
        .collect();

    let subpatch_data = UiSubPatch {
        display_name: "Sub-patch".to_string(),
        pins_in,
        pins_out,
        snarl: Box::new(inner_snarl),
        items: vec![],
        overlay_items: vec![],
        config_items: vec![],
        exposed_modules: vec![],
        decorations: vec![],
        snap_enabled: false,
        snap_grid_px: 8,
        selected_item: None,
        selected_items: Vec::new(),
        cycle_pos: None,
    };

    let subpatch_node = NodeData {
        module_id: "subpatch".to_string(),
        display_name: "Sub-patch".to_string(),
        category: "Utility".to_string(),
        inputs: outer_inputs,
        outputs: outer_outputs,
        params: HashMap::new(),
        subpatch: Some(Box::new(subpatch_data)),
        extra: Default::default(),
    };

    // Insert the subpatch at the centroid position.
    let subpatch_id = snarl.insert_node(centroid, subpatch_node);

    // ── Reconnect outer boundary wires to the new subpatch node ───────────────

    let mut inlet_port: usize = 0;
    let mut outlet_port: usize = 0;

    for bw in &boundary_wires {
        if bw.incoming {
            // The external source → subpatch input port.
            snarl.connect(
                bw.out_id,
                InPinId { node: subpatch_id, input: inlet_port },
            );
            inlet_port += 1;
        } else {
            // The subpatch output port → external destination.
            snarl.connect(
                OutPinId { node: subpatch_id, output: outlet_port },
                bw.in_id,
            );
            outlet_port += 1;
        }
    }

    // ── Remove selected nodes from outer canvas ───────────────────────────────
    // Disconnection happens automatically when the node is removed.

    for &outer_id in selected {
        if snarl.get_node(outer_id).is_some() {
            snarl.remove_node(outer_id);
        }
    }

    GroupResult::Ok(subpatch_id)
}

/// Public wrapper that calls `group_into_subpatch` through a `Canvas`.
impl Canvas {
    /// Attempt to group `selected` nodes into a `subpatch` node.
    ///
    /// Returns `GroupResult::Ok(id)` on success. On
    /// `GroupResult::NonCanonicalBoundaryPin`, no mutation has occurred — the
    /// caller should surface the error to the user before retrying.
    pub fn group_selected_into_subpatch(&mut self, selected: &[NodeId]) -> GroupResult {
        group_into_subpatch(&mut self.snarl, &mut self.undo_stack, selected)
    }
}

// ── Regression tests for canvas clipboard semantics ──────────────────────────

#[cfg(test)]
mod clipboard_tests {
    use super::*;
    use egui_snarl::{InPinId, NodeId, OutPinId};
    use flexinput_core::{PinDescriptor, SignalType};
    use std::collections::HashMap;

    /// Build a minimal `NodeData` with the given number of outputs and inputs.
    fn make_node(n_out: usize, n_in: usize) -> NodeData {
        NodeData {
            module_id: "test.node".to_string(),
            display_name: "Test".to_string(),
            category: "Test".to_string(),
            outputs: (0..n_out)
                .map(|i| PinDescriptor::new(format!("out{i}"), SignalType::Float))
                .collect(),
            inputs: (0..n_in)
                .map(|i| PinDescriptor::new(format!("in{i}"), SignalType::Float))
                .collect(),
            params: HashMap::new(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    /// Insert `node` into a fresh Canvas and return its NodeId.
    fn add_node(canvas: &mut Canvas, pos: egui::Pos2, node: NodeData) -> NodeId {
        canvas.snarl.insert_node(pos, node)
    }

    // ── Test: copy populates clipboard ────────────────────────────────────────

    #[test]
    fn copy_selected_populates_clipboard() {
        let mut canvas = Canvas::new();
        let id = add_node(&mut canvas, egui::pos2(10.0, 20.0), make_node(1, 1));

        assert!(canvas.clipboard.is_none(), "clipboard should start empty");
        canvas.copy_selected(&[id]);
        assert!(canvas.clipboard.is_some(), "clipboard should be set after copy");

        let cb = canvas.clipboard.as_ref().unwrap();
        assert_eq!(cb.nodes.len(), 1, "one node should be in clipboard");
        let (pos, _data) = &cb.nodes[0];
        assert!(
            (pos.x - 10.0).abs() < 0.001 && (pos.y - 20.0).abs() < 0.001,
            "clipboard position should match original node position"
        );
    }

    // ── Test: empty selection leaves clipboard unchanged ──────────────────────

    #[test]
    fn copy_empty_selection_leaves_clipboard_unchanged() {
        let mut canvas = Canvas::new();
        canvas.copy_selected(&[]);
        assert!(canvas.clipboard.is_none());
    }

    // ── Test: paste produces fresh NodeIds ────────────────────────────────────

    #[test]
    fn paste_produces_fresh_node_ids() {
        let mut canvas = Canvas::new();
        let orig_id = add_node(&mut canvas, egui::pos2(0.0, 0.0), make_node(1, 0));
        canvas.copy_selected(&[orig_id]);

        let before: Vec<NodeId> = canvas.snarl.nodes_ids_data().map(|(id, _)| id).collect();
        canvas.paste();
        let after: Vec<NodeId> = canvas.snarl.nodes_ids_data().map(|(id, _)| id).collect();

        assert_eq!(after.len(), before.len() + 1, "paste should add exactly one new node");

        let new_ids: Vec<NodeId> = after.into_iter().filter(|id| !before.contains(id)).collect();
        assert!(!new_ids.contains(&orig_id), "pasted node must have a different NodeId from the original");
    }

    // ── Test: pasted nodes are offset from original positions ─────────────────

    #[test]
    fn paste_offsets_node_positions() {
        let mut canvas = Canvas::new();
        let orig_id = add_node(&mut canvas, egui::pos2(100.0, 50.0), make_node(1, 0));
        canvas.copy_selected(&[orig_id]);
        canvas.paste();

        // Find nodes other than the original.
        let pasted: Vec<egui::Pos2> = canvas.snarl
            .nodes_ids_data()
            .filter(|(id, _)| *id != orig_id)
            .filter_map(|(id, _)| canvas.snarl.get_node_info(id).map(|n| n.pos))
            .collect();

        assert_eq!(pasted.len(), 1, "exactly one pasted node expected");
        let p = pasted[0];
        assert!(
            (p.x - 140.0).abs() < 0.001 && (p.y - 90.0).abs() < 0.001,
            "pasted node should be offset by (40, 40); got ({}, {})",
            p.x, p.y
        );
    }

    // ── Test: internal wires are reconstructed after paste ────────────────────

    #[test]
    fn paste_reconstructs_internal_wires() {
        let mut canvas = Canvas::new();
        // Node A has one output; Node B has one input.
        let id_a = add_node(&mut canvas, egui::pos2(0.0, 0.0), make_node(1, 0));
        let id_b = add_node(&mut canvas, egui::pos2(200.0, 0.0), make_node(0, 1));

        // Connect A.out[0] → B.in[0]
        canvas.snarl.connect(
            OutPinId { node: id_a, output: 0 },
            InPinId  { node: id_b, input:  0 },
        );

        canvas.copy_selected(&[id_a, id_b]);
        canvas.paste();

        // Count total wires: original + pasted should each have one wire.
        let wire_count = canvas.snarl.wires().count();
        assert_eq!(wire_count, 2, "expected 2 wires (original + reconstructed internal); got {wire_count}");
    }

    // ── Test: boundary (external) wires are NOT reconnected ───────────────────

    #[test]
    fn paste_drops_boundary_wires() {
        let mut canvas = Canvas::new();
        // id_a: one output — it's the "upstream" node NOT copied.
        let id_upstream = add_node(&mut canvas, egui::pos2(-200.0, 0.0), make_node(1, 0));
        // id_b: one input, one output — the node we will copy.
        let id_b = add_node(&mut canvas, egui::pos2(0.0, 0.0), make_node(1, 1));
        // id_downstream: one input — NOT copied.
        let id_downstream = add_node(&mut canvas, egui::pos2(200.0, 0.0), make_node(0, 1));

        // Boundary wire in: upstream → b
        canvas.snarl.connect(
            OutPinId { node: id_upstream, output: 0 },
            InPinId  { node: id_b,        input:  0 },
        );
        // Boundary wire out: b → downstream
        canvas.snarl.connect(
            OutPinId { node: id_b,          output: 0 },
            InPinId  { node: id_downstream, input:  0 },
        );

        // Copy only id_b (not upstream or downstream).
        canvas.copy_selected(&[id_b]);
        canvas.paste();

        // The pasted node (id_b copy) should have zero wires.
        // Original wires (upstream→b, b→downstream) remain: 2 total.
        let wire_count = canvas.snarl.wires().count();
        assert_eq!(
            wire_count, 2,
            "only original boundary wires should exist; pasted node must not be rewired; got {wire_count}"
        );
    }

    // ── Test: malformed wire indices in clipboard are silently dropped ─────────

    #[test]
    fn paste_ignores_out_of_bounds_wire_indices() {
        let mut canvas = Canvas::new();
        let id = add_node(&mut canvas, egui::pos2(0.0, 0.0), make_node(1, 1));
        canvas.copy_selected(&[id]);

        // Inject malformed wire: both indices in range (only one node) but pin
        // indices deliberately out of bounds.
        if let Some(ref mut cb) = canvas.clipboard {
            cb.internal_wires.push((0, 99, 0, 99)); // pin 99 does not exist
        }

        // Paste must not panic.
        canvas.paste();

        // Only the pasted node should exist (original + pasted = 2 nodes, 0 wires).
        let wire_count = canvas.snarl.wires().count();
        assert_eq!(wire_count, 0, "malformed wire should be dropped; no wires expected");
    }

    // ── Tests required by plan 01-07 (cross-boundary clipboard contract) ─────

    #[test]
    fn fresh_canvas_has_no_clipboard() {
        let canvas = Canvas::new();
        assert!(canvas.clipboard().is_none(), "fresh canvas clipboard should be None");
    }

    #[test]
    fn set_clipboard_makes_clipboard_accessible() {
        let mut canvas = Canvas::new();
        let data = ClipboardData {
            nodes: vec![(egui::pos2(0.0, 0.0), make_node(1, 0))],
            internal_wires: vec![],
        };
        canvas.set_clipboard(data);
        assert!(canvas.clipboard().is_some(), "clipboard() should return Some after set_clipboard()");
    }

    #[test]
    fn paste_after_set_clipboard_inserts_node() {
        let mut src = Canvas::new();
        let src_id = add_node(&mut src, egui::pos2(10.0, 20.0), make_node(1, 0));
        src.copy_selected(&[src_id]);
        let cb = src.clipboard().unwrap();

        // Simulate cross-boundary paste: target canvas starts empty, receives clipboard from app.
        let mut target = Canvas::new();
        target.set_clipboard(cb);
        target.paste();

        let count = target.snarl.nodes_ids_data().count();
        assert_eq!(count, 1, "paste should insert exactly one node into the target canvas");
    }

    #[test]
    fn paste_calls_push_undo() {
        let mut canvas = Canvas::new();
        let id = add_node(&mut canvas, egui::pos2(10.0, 20.0), make_node(1, 0));
        canvas.copy_selected(&[id]);
        let before = canvas.undo_stack.len();
        canvas.paste();
        assert_eq!(canvas.undo_stack.len(), before + 1, "paste() must push undo before inserting nodes");
    }

    // ── Tests required by plan 01-08 (named interface contract) ──────────────

    #[test]
    fn copy_selected_captures_nodes() {
        let mut canvas = Canvas::new();
        let id = add_node(&mut canvas, egui::pos2(10.0, 20.0), make_node(1, 0));
        canvas.copy_selected(&[id]);
        let cb = canvas.clipboard.clone().expect("clipboard should be set after copy_selected");
        assert_eq!(cb.nodes.len(), 1, "clipboard should contain exactly one node");
    }

    #[test]
    fn paste_inserts_at_offset() {
        let mut canvas = Canvas::new();
        let pos = egui::pos2(10.0, 20.0);
        let id = add_node(&mut canvas, pos, make_node(1, 0));
        canvas.copy_selected(&[id]);
        canvas.paste();
        let count = canvas.snarl.nodes_ids_data().count();
        assert_eq!(count, 2, "paste should insert one additional node (original + copy)");
    }

    #[test]
    fn paste_with_empty_clipboard_is_noop() {
        let mut canvas = Canvas::new();
        let before = canvas.snarl.nodes_ids_data().count();
        canvas.paste(); // clipboard is None — should be a no-op
        let after = canvas.snarl.nodes_ids_data().count();
        assert_eq!(before, after, "paste with no clipboard should not change node count");
    }
}

/// Disconnect a wire and insert `desc` between its endpoints, auto-connecting compatible pins.
fn insert_between(
    snarl: &mut Snarl<NodeData>,
    desc: &ModuleDescriptor,
    out_id: OutPinId,
    in_id: InPinId,
) {
    let from_pos = snarl.get_node_info(out_id.node).map(|n| n.pos).unwrap_or_default();
    let to_pos   = snarl.get_node_info(in_id.node) .map(|n| n.pos).unwrap_or_default();
    let insert_pos = egui::pos2(
        (from_pos.x + to_pos.x) * 0.5,
        (from_pos.y + to_pos.y) * 0.5,
    );

    let out_type = snarl.get_node(out_id.node)
        .and_then(|n| n.outputs.get(out_id.output))
        .map(|p| p.signal_type);
    let in_type = snarl.get_node(in_id.node)
        .and_then(|n| n.inputs.get(in_id.input))
        .map(|p| p.signal_type);

    snarl.disconnect(out_id, in_id);
    let new_id = snarl.insert_node(insert_pos, NodeData::from(desc));

    if let Some(idx) = desc.inputs.iter().position(|p| out_type.map_or(true, |t| p.signal_type.accepts(t))) {
        snarl.connect(out_id, InPinId { node: new_id, input: idx });
    }
    if let Some(idx) = desc.outputs.iter().position(|p| in_type.map_or(true, |t| t.accepts(p.signal_type))) {
        snarl.connect(OutPinId { node: new_id, output: idx }, in_id);
    }
}

// ── Grouping tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod group_tests {
    use super::*;
    use flexinput_core::{PinDescriptor, SignalType};
    use std::collections::HashMap;

    /// Build a node with one AutoMap output.
    fn make_automap_source() -> NodeData {
        NodeData {
            module_id: "module.automap_split".to_string(),
            display_name: "AutoMap Source".to_string(),
            category: "AutoMap".to_string(),
            outputs: vec![PinDescriptor::new("automap_out", SignalType::AutoMap)],
            inputs: vec![],
            params: HashMap::new(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    /// Build a node with one AutoMap input.
    fn make_automap_sink() -> NodeData {
        NodeData {
            module_id: "module.automap_collect".to_string(),
            display_name: "AutoMap Sink".to_string(),
            category: "AutoMap".to_string(),
            inputs: vec![PinDescriptor::new("automap_in", SignalType::AutoMap)],
            outputs: vec![],
            params: HashMap::new(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    /// Build a generic node with one Float output and one Float input.
    fn make_float_node() -> NodeData {
        NodeData {
            module_id: "test.float_node".to_string(),
            display_name: "Float Node".to_string(),
            category: "Test".to_string(),
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
            inputs: vec![PinDescriptor::new("in", SignalType::Float)],
            params: HashMap::new(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    // ── Test: empty selection returns EmptySelection ──────────────────────────

    #[test]
    fn group_empty_selection_is_noop() {
        let mut canvas = Canvas::new();
        let result = canvas.group_selected_into_subpatch(&[]);
        assert!(
            matches!(result, GroupResult::EmptySelection),
            "empty selection should return EmptySelection"
        );
        // Canvas should be unmodified.
        assert_eq!(canvas.snarl.nodes_ids_data().count(), 0);
    }

    // ── Test: selected nodes are moved into the subpatch's inner snarl ────────

    #[test]
    fn group_moves_selected_nodes_into_inner_snarl() {
        let mut canvas = Canvas::new();
        // Insert two AutoMap nodes to be grouped.
        let id_a = canvas.snarl.insert_node(egui::pos2(0.0, 0.0), make_automap_source());
        let id_b = canvas.snarl.insert_node(egui::pos2(200.0, 0.0), make_automap_sink());
        // Wire them internally.
        canvas.snarl.connect(
            OutPinId { node: id_a, output: 0 },
            InPinId  { node: id_b, input:  0 },
        );

        let result = canvas.group_selected_into_subpatch(&[id_a, id_b]);
        let sp_id = match result {
            GroupResult::Ok(id) => id,
            other => panic!("expected Ok, got {:?}", other),
        };

        // Original nodes should be gone from outer canvas.
        assert!(canvas.snarl.get_node(id_a).is_none(), "id_a should be removed from outer canvas");
        assert!(canvas.snarl.get_node(id_b).is_none(), "id_b should be removed from outer canvas");

        // Only the subpatch node should remain.
        let outer_nodes: Vec<NodeId> = canvas.snarl.nodes_ids_data().map(|(id, _)| id).collect();
        assert_eq!(outer_nodes.len(), 1, "only subpatch node should remain in outer canvas");
        assert_eq!(outer_nodes[0], sp_id);

        // The subpatch's inner snarl should hold two nodes (the original pair).
        let sp_node = canvas.snarl.get_node(sp_id).expect("subpatch node must exist");
        let inner = sp_node.subpatch.as_ref().expect("subpatch field must be populated");
        let inner_count = inner.snarl.nodes_ids_data().count();
        assert_eq!(
            inner_count, 2,
            "inner snarl should contain exactly the two grouped nodes; got {inner_count}"
        );
    }

    // ── Test: internal wires are preserved inside the inner snarl ─────────────

    #[test]
    fn group_preserves_internal_wires() {
        let mut canvas = Canvas::new();
        let id_a = canvas.snarl.insert_node(egui::pos2(0.0, 0.0), make_automap_source());
        let id_b = canvas.snarl.insert_node(egui::pos2(200.0, 0.0), make_automap_sink());
        canvas.snarl.connect(
            OutPinId { node: id_a, output: 0 },
            InPinId  { node: id_b, input:  0 },
        );

        let result = canvas.group_selected_into_subpatch(&[id_a, id_b]);
        let sp_id = match result {
            GroupResult::Ok(id) => id,
            other => panic!("expected Ok, got {:?}", other),
        };

        let sp_node = canvas.snarl.get_node(sp_id).expect("subpatch node must exist");
        let inner = sp_node.subpatch.as_ref().expect("subpatch field must be populated");
        // Internal wire (a.out[0] → b.in[0]) must be preserved.
        let inner_wire_count = inner.snarl.wires().count();
        assert_eq!(
            inner_wire_count, 1,
            "internal wire should be preserved inside inner snarl; got {inner_wire_count}"
        );
    }

    // ── Test: incoming boundary wire → inlet node created ─────────────────────

    #[test]
    fn group_creates_inlet_for_incoming_boundary_wire() {
        let mut canvas = Canvas::new();
        // External upstream node (NOT grouped).
        let id_upstream = canvas.snarl.insert_node(
            egui::pos2(-200.0, 0.0), make_automap_source(),
        );
        // Internal node (grouped).
        let id_inner = canvas.snarl.insert_node(
            egui::pos2(0.0, 0.0), make_automap_sink(),
        );
        // Boundary incoming wire.
        canvas.snarl.connect(
            OutPinId { node: id_upstream, output: 0 },
            InPinId  { node: id_inner,    input:  0 },
        );

        let result = canvas.group_selected_into_subpatch(&[id_inner]);
        let sp_id = match result {
            GroupResult::Ok(id) => id,
            other => panic!("expected Ok, got {:?}", other),
        };

        // Outer subpatch node should have one input (the inlet port).
        let sp_node = canvas.snarl.get_node(sp_id).expect("subpatch node must exist");
        assert_eq!(
            sp_node.inputs.len(), 1,
            "subpatch node should have one input pin for the incoming boundary wire"
        );

        // Inner snarl should contain the grouped node + one inlet node.
        let inner = sp_node.subpatch.as_ref().expect("subpatch field must be populated");
        let has_inlet = inner.snarl.nodes_ids_data()
            .any(|(_, n)| n.value.module_id == "subpatch.inlet");
        assert!(has_inlet, "inner snarl must contain a subpatch.inlet node");
    }

    // ── Test: outgoing boundary wire → outlet node created ────────────────────

    #[test]
    fn group_creates_outlet_for_outgoing_boundary_wire() {
        let mut canvas = Canvas::new();
        // Internal node (grouped) with one AutoMap output.
        let id_inner = canvas.snarl.insert_node(
            egui::pos2(0.0, 0.0), make_automap_source(),
        );
        // External downstream node (NOT grouped).
        let id_downstream = canvas.snarl.insert_node(
            egui::pos2(200.0, 0.0), make_automap_sink(),
        );
        // Boundary outgoing wire.
        canvas.snarl.connect(
            OutPinId { node: id_inner,      output: 0 },
            InPinId  { node: id_downstream, input:  0 },
        );

        let result = canvas.group_selected_into_subpatch(&[id_inner]);
        let sp_id = match result {
            GroupResult::Ok(id) => id,
            other => panic!("expected Ok, got {:?}", other),
        };

        // Outer subpatch node should have one output (the outlet port).
        let sp_node = canvas.snarl.get_node(sp_id).expect("subpatch node must exist");
        assert_eq!(
            sp_node.outputs.len(), 1,
            "subpatch node should have one output pin for the outgoing boundary wire"
        );

        // Inner snarl should contain the grouped node + one outlet node.
        let inner = sp_node.subpatch.as_ref().expect("subpatch field must be populated");
        let has_outlet = inner.snarl.nodes_ids_data()
            .any(|(_, n)| n.value.module_id == "subpatch.outlet");
        assert!(has_outlet, "inner snarl must contain a subpatch.outlet node");
    }

    // ── Test: non-canonical boundary pin rejects grouping (T-05-01) ───────────

    #[test]
    fn group_drops_non_automap_boundary_wire() {
        let mut canvas = Canvas::new();
        // External upstream node emitting a Float signal (not AutoMap).
        let id_upstream = canvas.snarl.insert_node(
            egui::pos2(-200.0, 0.0), make_float_node(),
        );
        // Internal node accepting a Float.
        let id_inner = canvas.snarl.insert_node(
            egui::pos2(0.0, 0.0), make_float_node(),
        );
        // Boundary incoming wire with non-AutoMap signal — should be dropped, not block grouping.
        canvas.snarl.connect(
            OutPinId { node: id_upstream, output: 0 },
            InPinId  { node: id_inner,    input:  0 },
        );

        let result = canvas.group_selected_into_subpatch(&[id_inner]);
        // Grouping succeeds — non-AutoMap boundary wire is dropped.
        assert!(
            matches!(result, GroupResult::Ok(_)),
            "grouping with a non-AutoMap boundary wire should succeed (wire dropped): got {:?}", result
        );
        // The inner node moved into the subpatch — it no longer exists in the outer canvas.
        assert!(canvas.snarl.get_node(id_inner).is_none(), "id_inner should have moved into subpatch");
        // The upstream node stays in the outer canvas.
        assert!(canvas.snarl.get_node(id_upstream).is_some(), "id_upstream must remain in outer canvas");
    }

    // ── Test: undo snapshot is pushed before mutation ─────────────────────────

    #[test]
    fn group_pushes_undo_snapshot() {
        let mut canvas = Canvas::new();
        let id_a = canvas.snarl.insert_node(egui::pos2(0.0, 0.0), make_automap_source());
        let id_b = canvas.snarl.insert_node(egui::pos2(200.0, 0.0), make_automap_sink());

        assert!(!canvas.can_undo(), "undo stack should be empty before grouping");

        let _ = canvas.group_selected_into_subpatch(&[id_a, id_b]);

        assert!(canvas.can_undo(), "undo snapshot should be pushed after grouping");
    }
}
