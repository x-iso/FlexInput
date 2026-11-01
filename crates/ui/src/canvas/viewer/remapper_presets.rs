//! Remapper presets: a Remapper's cards saved to and loaded from a `.fxremap`
//! file, from two icons in the body's top-right corner — in the body, so a
//! pinned whole-module widget (Easy mode, the config overlay) has them too.
//!
//! A preset holds the card list and nothing else: `mappings` is all the engine
//! reads of a Remapper, the rest of its params being the body's capture state.
//!
//! ```json
//! { "flexinput_remapper": 1, "mappings": [ { "in": ["btn_south"], "out": ["key_space"] } ] }
//! ```

use super::*;

/// The format's marker, and its version.
const PRESET_MARKER: &str = "flexinput_remapper";
const PRESET_VERSION: u64 = 1;
const PRESET_EXT: &str = "fxremap";
/// How long the result of a save or load stays up.
const STATUS_SECS: f32 = 6.0;

/// What a preset file holds for a Remapper node.
pub(crate) fn remapper_preset_contents(node: &NodeData) -> Value {
    let cards = node.params.get("mappings").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::json!({ PRESET_MARKER: PRESET_VERSION, "mappings": cards })
}

/// A preset file's cards, or why it can't be read as one.
pub(crate) fn remapper_preset_parse(text: &str) -> Result<Vec<Value>, String> {
    const NOT_ONE: &str = "it isn't a Remapper preset";
    let doc: Value = serde_json::from_str(text).map_err(|_| NOT_ONE.to_string())?;
    let version = doc.get(PRESET_MARKER).and_then(|v| v.as_u64()).ok_or(NOT_ONE)?;
    if version > PRESET_VERSION {
        return Err("it was saved by a newer version of FlexInput".to_string());
    }
    doc.get("mappings").and_then(|v| v.as_array()).cloned().ok_or_else(|| NOT_ONE.to_string())
}

/// Make a Remapper's cards a preset's. They go through the card migrations a
/// loaded patch gets, so a preset saved by an older build loads as it would
/// have in a patch.
pub(crate) fn remapper_preset_apply(node: &mut NodeData, cards: Vec<Value>) {
    node.params.insert("mappings".to_string(), Value::Array(cards));
    crate::canvas::migrate_mapping_cards(node);
}

/// The result of the last save or load, as the body shows it.
#[derive(Clone)]
struct PresetStatus {
    text: String,
    failed: bool,
    at: std::time::Instant,
}

fn status_id(path: &[usize], node_id: NodeId) -> egui::Id {
    egui::Id::new(("fxi_remap_preset_status", path, node_id.0))
}

fn set_status(ctx: &egui::Context, id: egui::Id, text: String, failed: bool) {
    ctx.data_mut(|d| d.insert_temp(id, PresetStatus { text, failed, at: std::time::Instant::now() }));
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn cards_phrase(n: usize) -> String {
    if n == 1 { "1 card".to_string() } else { format!("{n} cards") }
}

/// The Save (💾) and Load (📂) icons, laid out right to left from the current
/// cursor. The file dialog runs on the UI thread, as the Area Mapper's presets
/// and the curve files do, so a load lands in the same frame as its click and
/// the canvas's edit tracking records it for undo. Returns whether the cards
/// changed.
pub(crate) fn remapper_preset_icons(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> bool {
    let status = status_id(&subpatch_path(automap_parent), node_id);
    let n_cards = snarl.get_node(node_id)
        .and_then(|n| n.params.get("mappings").and_then(|v| v.as_array()).map(|a| a.len()))
        .unwrap_or(0);
    let icon = |glyph: &str| egui::Button::new(egui::RichText::new(glyph).size(12.0)).small();
    let mut changed = false;

    // Right to left: Load sits in the corner, Save beside it.
    let load = ui.add(icon("📂"))
        .on_hover_text("Load cards from a .fxremap preset (replaces this Remapper's cards)")
        .clicked();
    let save = ui.add_enabled(n_cards > 0, icon("💾"))
        .on_hover_text("Save this Remapper's cards to a .fxremap preset")
        .on_disabled_hover_text("No cards to save yet")
        .clicked();

    if save {
        let contents = snarl.get_node(node_id).map(remapper_preset_contents);
        let picked = crate::overlay::with_overlay_not_topmost(|| {
            rfd::FileDialog::new()
                .add_filter("FlexInput Remapper", &[PRESET_EXT])
                .set_file_name(format!("remapper.{PRESET_EXT}"))
                .save_file()
        });
        if let (Some(path), Some(contents)) = (picked, contents) {
            let name = file_name(&path);
            let written = serde_json::to_string_pretty(&contents).map_err(|e| e.to_string())
                .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
            match written {
                Ok(()) => set_status(ui.ctx(), status, format!("Saved {} to {name}.", cards_phrase(n_cards)), false),
                Err(e) => set_status(ui.ctx(), status, format!("Couldn't save {name}: {e}"), true),
            }
        }
    }
    if load {
        let picked = crate::overlay::with_overlay_not_topmost(|| {
            rfd::FileDialog::new().add_filter("FlexInput Remapper", &[PRESET_EXT]).pick_file()
        });
        if let Some(path) = picked {
            let name = file_name(&path);
            let read = std::fs::read_to_string(&path).map_err(|e| e.to_string())
                .and_then(|text| remapper_preset_parse(&text));
            match (read, snarl.get_node_mut(node_id)) {
                (Ok(cards), Some(node)) => {
                    let n = cards.len();
                    remapper_preset_apply(node, cards);
                    changed = true;
                    // From a pinned widget on the config overlay, the canvas's
                    // own edit tracking can't see this write.
                    mark_overlay_param_write(ui.ctx());
                    set_status(ui.ctx(), status, format!("Loaded {} from {name}.", cards_phrase(n)), false);
                }
                (Err(e), _) => set_status(ui.ctx(), status, format!("Couldn't load {name}: {e}."), true),
                (Ok(_), None) => {}
            }
        }
    }
    changed
}

/// The result of the last save or load, for a few seconds.
pub(crate) fn remapper_preset_status(
    ui: &mut egui::Ui,
    node_id: NodeId,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    let id = status_id(&subpatch_path(automap_parent), node_id);
    let Some(status) = ui.ctx().data(|d| d.get_temp::<PresetStatus>(id)) else { return };
    let age = status.at.elapsed().as_secs_f32();
    if age > STATUS_SECS {
        ui.ctx().data_mut(|d| d.remove::<PresetStatus>(id));
        return;
    }
    let text = egui::RichText::new(status.text).size(12.0);
    ui.label(if status.failed { text.color(Color32::from_rgb(232, 150, 65)) } else { text.weak() });
    // Come back to take it down when its time is up.
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(STATUS_SECS - age + 0.05));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn remapper(mappings: Value) -> NodeData {
        let mut params = std::collections::HashMap::new();
        params.insert("mappings".to_string(), mappings);
        params.insert("ui_phase".to_string(), json!("capturing"));
        NodeData {
            module_id: "module.remapper".to_string(),
            display_name: "Remapper".to_string(),
            category: "AutoMap".to_string(),
            inputs: vec![],
            outputs: vec![],
            params,
            subpatch: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_preset_holds_the_cards_and_reads_back_as_them() {
        let cards = json!([
            { "in": ["btn_south"], "out": ["key_space"] },
            { "in": ["btn_lb", "btn_east"], "out": ["key_lctrl", "key_c"], "out_gap": [0.0, 50.0], "mode": "short" },
        ]);
        let file = remapper_preset_contents(&remapper(cards.clone()));
        assert_eq!(file[PRESET_MARKER], json!(PRESET_VERSION));
        // Only the cards: the body's capture state isn't part of a preset.
        assert!(file.get("ui_phase").is_none());
        let text = serde_json::to_string_pretty(&file).unwrap();
        assert_eq!(Value::Array(remapper_preset_parse(&text).unwrap()), cards);
    }

    #[test]
    fn a_remapper_with_no_cards_saves_an_empty_list() {
        let mut node = remapper(json!([]));
        node.params.remove("mappings");
        assert_eq!(remapper_preset_contents(&node)["mappings"], json!([]));
    }

    #[test]
    fn anything_else_is_refused_with_a_reason() {
        assert!(remapper_preset_parse("not json").unwrap_err().contains("isn't a Remapper preset"));
        // Another FlexInput file, or a preset with its cards missing.
        assert!(remapper_preset_parse(r#"{"flexinput_area": 1, "mappings": []}"#).is_err());
        assert!(remapper_preset_parse(r#"{"flexinput_remapper": 1}"#).is_err());
        assert!(remapper_preset_parse(r#"{"flexinput_remapper": 1, "mappings": {}}"#).is_err());
        assert!(remapper_preset_parse(r#"{"flexinput_remapper": 2, "mappings": []}"#)
            .unwrap_err().contains("newer version"));
    }

    #[test]
    fn loading_replaces_the_cards_and_migrates_them_as_a_patch_would() {
        let mut node = remapper(json!([{ "in": ["btn_north"], "out": ["key_q"] }]));
        // Saved by a build that let a card of buttons alone be Analog.
        remapper_preset_apply(&mut node, vec![
            json!({ "in": ["btn_south"], "out": ["key_e"], "mode": "analog", "window_ms": 200.0 }),
            json!({ "in": ["left_trigger"], "out": ["key_f"], "mode": "analog" }),
        ]);
        let cards = node.params["mappings"].as_array().unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0]["in"], json!(["btn_south"]));
        assert!(cards[0].get("mode").is_none() && cards[0].get("window_ms").is_none());
        // An analog input keeps its Analog mode.
        assert_eq!(cards[1]["mode"], json!("analog"));
        // The capture state is left alone.
        assert_eq!(node.params["ui_phase"], json!("capturing"));
    }
}
