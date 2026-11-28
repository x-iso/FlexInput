//! Editing a layer on a canvas: arming it, recording what changes, and putting
//! the patch's own settings back (see `crate::layers`).

use std::borrow::Cow;
use std::collections::HashMap;

use egui_snarl::Snarl;
use serde_json::Value;

use super::{Canvas, NodeData};
use crate::layers::{self, LayerAct, LayerScope};

/// A layer being edited: what to put back when editing stops.
pub(crate) struct ArmedLayer {
    pub(crate) scope: LayerScope,
    pub(crate) id: String,
    /// Every node's params in the layer's reach as they were before its settings
    /// went in, by path from the top canvas: the patch's own settings.
    baseline: HashMap<Vec<usize>, HashMap<String, Value>>,
    /// The canvas as it was, and its undo history: editing a layer is no step of
    /// the patch's, and nothing undone in the patch's history while editing could
    /// bring a layer's setting back into it.
    before: Snarl<NodeData>,
    undo: Vec<Snarl<NodeData>>,
    redo: Vec<Snarl<NodeData>>,
}

impl Canvas {
    /// The layer being edited: where it lives and its id.
    pub fn armed_layer(&self) -> Option<(&LayerScope, &str)> {
        self.armed.as_ref().map(|a| (&a.scope, a.id.as_str()))
    }

    /// Start editing layer `id` of `scope`: its settings go into the canvas, and
    /// what changes from here on is the layer's. Another layer being edited is
    /// finished first.
    pub fn arm_layer(&mut self, scope: LayerScope, id: &str) {
        self.disarm_layer();
        let Some(layer) = layers::layers_in(&self.layers, &self.snarl, &scope)
            .and_then(|ls| ls.iter().find(|l| l.id == id))
            .cloned()
        else {
            return;
        };
        let Some(reach) = layers::scope_snarl(&self.snarl, &scope) else { return };
        let mut baseline = HashMap::new();
        layers::collect_params(reach, scope.prefix(), &mut baseline);
        let before = self.undo_clone();
        let armed = ArmedLayer {
            id: layer.id.clone(),
            baseline,
            before,
            undo: std::mem::take(&mut self.undo_stack),
            redo: std::mem::take(&mut self.redo_stack),
            scope: scope.clone(),
        };
        layers::put_settings(&mut self.snarl, scope.prefix(), &layer.settings);
        self.armed = Some(Box::new(armed));
        self.mutation_gen = self.mutation_gen.wrapping_add(1);
        self.sync_committed_fingerprint();
    }

    /// Stop editing: what changed is the layer's now, and the patch's own settings
    /// go back. Changes to the patch itself made meanwhile (a module added, a
    /// wire) stay, as one step to undo.
    pub fn disarm_layer(&mut self) {
        let Some(armed) = self.armed.take() else { return };
        let ArmedLayer { scope, id, baseline, before, undo, redo } = *armed;
        let settings = layers::changed_settings(&self.snarl, scope.prefix(), &baseline);
        let views = layers::card_list_views(&self.snarl, &baseline);
        layers::restore_baseline(&mut self.snarl, &baseline);
        // Cards that switch layers are the patch's, whichever layer is being edited: one
        // made, changed or taken away meanwhile is so in the patch.
        layers::keep_switch_cards(&mut self.snarl, &views);
        if let Some(layer) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope)
            .and_then(|ls| ls.iter_mut().find(|l| l.id == id))
        {
            layer.settings = settings;
        }
        self.undo_stack = undo;
        self.redo_stack = redo;
        let fingerprint = |s: &Snarl<NodeData>| serde_json::to_vec(s).unwrap_or_default();
        if fingerprint(&self.undo_clone()) != fingerprint(&before) {
            self.undo_stack.push(before);
            self.redo_stack.clear();
        }
        self.mutation_gen = self.mutation_gen.wrapping_add(1);
        self.sync_committed_fingerprint();
    }

    /// The canvas with the patch's own settings: what a save writes. The same as
    /// the canvas but while a layer is being edited.
    pub fn base_snarl(&self) -> Cow<'_, Snarl<NodeData>> {
        match &self.armed {
            None => Cow::Borrowed(&self.snarl),
            Some(a) => {
                let mut s = self.snarl.clone();
                layers::restore_baseline(&mut s, &a.baseline);
                // A sub-patch's layer is in the canvas: with what it has so far.
                if a.scope != LayerScope::Patch {
                    let settings = layers::changed_settings(&self.snarl, a.scope.prefix(), &a.baseline);
                    let mut none = Vec::new();
                    if let Some(l) = layers::layers_in_mut(&mut none, &mut s, &a.scope)
                        .and_then(|ls| ls.iter_mut().find(|l| l.id == a.id))
                    {
                        l.settings = settings;
                    }
                }
                Cow::Owned(s)
            }
        }
    }

    /// The patch's layers as a save writes them: with the settings the one being
    /// edited has so far.
    pub fn saved_layers(&self) -> Vec<layers::Layer> {
        let mut out = self.layers.clone();
        if let Some(a) = self.armed.as_ref().filter(|a| a.scope == LayerScope::Patch) {
            if let Some(l) = out.iter_mut().find(|l| l.id == a.id) {
                l.settings = layers::changed_settings(&self.snarl, &[], &a.baseline);
            }
        }
        out
    }

    /// What the layers on screen change, to tint: the ones on (`active`, in the
    /// order they came on), the one a chip previews, and the one being edited on
    /// top - its changes so far.
    pub fn layer_tints(&self, active: &[String], preview: Option<&str>) -> layers::LayerTints {
        let mut tints = layers::LayerTints::default();
        let all = layers::all_layers(&self.layers, &self.snarl);
        let armed = self.armed.as_ref().map(|a| a.id.as_str());
        let shown = active.iter().map(String::as_str)
            .chain(preview)
            .filter(|id| Some(*id) != armed);
        for id in shown {
            if let Some((scope, _, index, l)) = all.iter().find(|(_, _, _, l)| l.id == id) {
                let color = layers::layer_color(l, *index);
                tints.add(&self.snarl, scope.prefix(), &l.id, &layers::layer_label(l), color, &l.settings, false);
            }
        }
        if let Some(a) = self.armed.as_ref() {
            if let Some((_, _, index, l)) = all.iter().find(|(_, _, _, l)| l.id == a.id) {
                let changes = layers::changed_settings(&self.snarl, a.scope.prefix(), &a.baseline);
                let color = layers::layer_color(l, *index);
                tints.add(&self.snarl, a.scope.prefix(), &l.id, &layers::layer_label(l), color, &changes, true);
            }
        }
        tints
    }

    /// A value a layer showed was edited where it showed (`layers::hide_layer_values`): it
    /// is the layer's new value. `prefix` is the path to the layer's own canvas, `path`
    /// the node's from the top.
    pub fn edit_layer_value(&mut self, layer_id: &str, prefix: &[usize], path: &[usize], key: &str, value: serde_json::Value) {
        let scope = if prefix.is_empty() { LayerScope::Patch } else { LayerScope::SubPatch(prefix.to_vec()) };
        let Some(rel) = path.strip_prefix(prefix) else { return };
        let Some(l) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope)
            .and_then(|ls| ls.iter_mut().find(|l| l.id == layer_id))
        else {
            return;
        };
        let at = l.settings.iter().position(|s| s.path == rel && s.key == key);
        match (at, value.is_null()) {
            // `null`: the layer no longer changes it.
            (Some(i), true) => { l.settings.remove(i); }
            (None, true) => return,
            (Some(i), false) if l.settings[i].value == value => return,
            (Some(i), false) => l.settings[i].value = value,
            (None, false) => l.settings.push(layers::LayerSetting { path: rel.to_vec(), key: key.to_string(), value }),
        }
        self.mutation_gen = self.mutation_gen.wrapping_add(1);
    }

    /// Give every card an id of its own (`layers::give_cards_ids`): a layer names cards by
    /// it. Not an undo step: the card was the step.
    pub fn give_cards_ids(&mut self) -> bool {
        let changed = layers::give_cards_ids(&mut self.snarl);
        if changed {
            self.mutation_gen = self.mutation_gen.wrapping_add(1);
            self.sync_committed_fingerprint();
        }
        changed
    }

    /// What each layer of `scope` changes, a line per setting, for its chip.
    pub fn layer_details(&self, scope: &LayerScope) -> Vec<Vec<String>> {
        let Some(ls) = layers::layers_in(&self.layers, &self.snarl, scope) else { return Vec::new() };
        ls.iter().map(|l| {
            match self.armed.as_ref().filter(|a| a.id == l.id && a.scope == *scope) {
                Some(a) => {
                    let now = layers::Layer {
                        settings: layers::changed_settings(&self.snarl, scope.prefix(), &a.baseline),
                        ..l.clone()
                    };
                    layers::describe(&now, scope.prefix(), &self.snarl)
                }
                None => layers::describe(l, scope.prefix(), &self.snarl),
            }
        }).collect()
    }

    /// Do `act` to a layer of `scope`, from its chip.
    pub fn layer_act(&mut self, scope: LayerScope, act: LayerAct) {
        let editing = |c: &Canvas, id: &str| c.armed_layer().is_some_and(|(s, a)| *s == scope && a == id);
        let edits = !matches!(act, LayerAct::Switch { .. });
        match act {
            LayerAct::Switch { id, on } => flexinput_engine::request_layer(&id, on),
            LayerAct::Arm(id) => self.arm_layer(scope.clone(), &id),
            LayerAct::Disarm => self.disarm_layer(),
            LayerAct::Add => {
                let Some(ls) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope) else { return };
                let name = layers::fresh_name(ls);
                ls.push(layers::Layer { id: flexinput_core::layers::new_layer_id(), name, ..Default::default() });
            }
            LayerAct::Rename { id, name } => {
                if let Some(l) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope)
                    .and_then(|ls| ls.iter_mut().find(|l| l.id == id))
                {
                    l.name = name;
                }
            }
            LayerAct::Recolor { id, color } => {
                if let Some(l) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope)
                    .and_then(|ls| ls.iter_mut().find(|l| l.id == id))
                {
                    l.color = Some(color);
                }
            }
            LayerAct::SetIcon { id, icon } => {
                if let Some(l) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope)
                    .and_then(|ls| ls.iter_mut().find(|l| l.id == id))
                {
                    l.icon = icon;
                }
            }
            LayerAct::Clear(id) => {
                if editing(self, &id) {
                    // Back to the patch's own settings, still editing.
                    if let Some(a) = self.armed.as_ref() {
                        let baseline = a.baseline.clone();
                        layers::restore_baseline(&mut self.snarl, &baseline);
                    }
                }
                if let Some(l) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope)
                    .and_then(|ls| ls.iter_mut().find(|l| l.id == id))
                {
                    l.settings.clear();
                }
            }
            LayerAct::Delete(id) => {
                if editing(self, &id) {
                    self.disarm_layer();
                }
                flexinput_engine::request_layer(&id, false);
                if let Some(ls) = layers::layers_in_mut(&mut self.layers, &mut self.snarl, &scope) {
                    ls.retain(|l| l.id != id);
                }
            }
        }
        // The patch changed: the crash-recovery save picks it up, and an open
        // editor of a sub-patch (whose layers are in its node) reloads.
        if edits {
            self.mutation_gen = self.mutation_gen.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::{Layer, LayerSetting};
    use flexinput_core::{PinDescriptor, SignalType};

    fn gain_node(v: f64) -> NodeData {
        NodeData {
            module_id: "math.multiply".into(),
            display_name: String::new(),
            category: String::new(),
            inputs: vec![PinDescriptor::new("in", SignalType::Float)],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
            params: [("gain".to_string(), Value::from(v))].into_iter().collect(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    fn gain(c: &Canvas, s: &Snarl<NodeData>, n: egui_snarl::NodeId) -> f64 {
        let _ = c;
        s.get_node(n).unwrap().params["gain"].as_f64().unwrap()
    }

    /// Editing a layer: its settings show while armed, what changes is the layer's,
    /// a save meanwhile gets the patch's own, and the patch gets them back after.
    #[test]
    fn editing_a_layer_never_touches_the_patch() {
        let mut c = Canvas::default();
        let n = c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), gain_node(1.0));
        c.layers.push(Layer {
            id: "aa".into(),
            name: "Aim".into(),
            settings: vec![LayerSetting { path: vec![n.0], key: "gain".into(), value: Value::from(0.5) }],
            ..Default::default()
        });
        c.arm_layer(LayerScope::Patch, "aa");
        assert_eq!(gain(&c, &c.snarl, n), 0.5, "the layer's setting shows while editing");
        c.snarl.get_node_mut(n).unwrap().params.insert("gain".into(), Value::from(0.25));
        assert_eq!(gain(&c, &c.base_snarl(), n), 1.0, "a save gets the patch's own");
        assert_eq!(c.saved_layers()[0].settings[0].value, Value::from(0.25), "and the layer as it is so far");
        c.disarm_layer();
        assert_eq!(gain(&c, &c.snarl, n), 1.0, "the patch's own setting is back");
        assert_eq!(c.layers[0].settings, vec![LayerSetting { path: vec![n.0], key: "gain".into(), value: Value::from(0.25) }]);
        assert!(c.undo_stack.is_empty(), "editing a layer is no step of the patch's");
    }

    /// What a layer on changes is tinted in its colour; while it's edited, what it
    /// changes so far.
    #[test]
    fn what_a_layer_changes_is_tinted_in_its_colour() {
        let mut c = Canvas::default();
        let n = c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), gain_node(1.0));
        c.layers.push(Layer {
            id: "aa".into(),
            name: "Aim".into(),
            settings: vec![LayerSetting { path: vec![n.0], key: "gain".into(), value: Value::from(0.5) }],
            color: Some([10, 20, 30]),
            ..Default::default()
        });
        assert!(c.layer_tints(&[], None).nodes.is_empty(), "off: nothing to tint");
        let t = c.layer_tints(&["aa".into()], None);
        let (module, list) = t.node(&[n.0]).expect("the module it changes");
        assert_eq!(module, "math.multiply");
        assert_eq!((list[0].key.as_str(), list[0].color), ("gain", eframe::egui::Color32::from_rgb(10, 20, 30)));
        assert!(c.layer_tints(&[], Some("aa")).node(&[n.0]).is_some(), "a chip under the pointer previews it");
        c.arm_layer(LayerScope::Patch, "aa");
        c.snarl.get_node_mut(n).unwrap().params.insert("gain".into(), Value::from(0.25));
        let t = c.layer_tints(&[], None);
        assert_eq!(t.node(&[n.0]).unwrap().1[0].value, Value::from(0.25), "being edited: as it is now");
        assert_eq!(c.layer_details(&LayerScope::Patch), vec![vec![" › gain: 0.25".to_string()]]);
    }

    /// A layer on shows its value in the module while it's drawn, and an edit made there
    /// goes to the layer; the patch keeps its own.
    #[test]
    fn a_value_a_layer_shows_is_edited_into_the_layer() {
        let ctx = eframe::egui::Context::default();
        let mut c = Canvas::default();
        let n = c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), gain_node(1.0));
        c.layers.push(Layer {
            id: "aa".into(),
            settings: vec![LayerSetting { path: vec![n.0], key: "gain".into(), value: Value::from(0.5) }],
            ..Default::default()
        });
        layers::publish_tints(&ctx, c.layer_tints(&["aa".into()], None));
        let node = c.snarl.get_node_mut(n).unwrap();
        let shown = layers::show_layer_values(&ctx, &[n.0], node).expect("a value to show");
        assert_eq!(node.params["gain"], Value::from(0.5), "drawn with the layer's value");
        node.params.insert("gain".into(), Value::from(0.75));
        layers::hide_layer_values(&ctx, node, shown);
        assert_eq!(gain(&c, &c.snarl, n), 1.0, "the patch keeps its own");
        for (layer, prefix, path, key, value) in layers::take_layer_edits(&ctx) {
            c.edit_layer_value(&layer, &prefix, &path, &key, value);
        }
        assert_eq!(c.layers[0].settings[0].value, Value::from(0.75), "the edit is the layer's");
    }

    fn remapper(cards: serde_json::Value) -> NodeData {
        let mut n = gain_node(1.0);
        n.module_id = "module.remapper".into();
        n.params.insert("mappings".into(), cards);
        n
    }

    fn card_ids(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().filter_map(|c| flexinput_core::layers::card_id(c).map(str::to_string)).collect()
    }

    /// A layer edited by arming keeps only the cards it changes, adds and takes away; the
    /// patch keeps its own; with the layer on, the module shows the layer's cards, and an edit
    /// of one there is the layer's.
    #[test]
    fn a_layer_changes_cards_by_id() {
        use serde_json::json;
        let ctx = eframe::egui::Context::default();
        let mut c = Canvas::default();
        let n = c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), remapper(json!([
            { "in": ["btn_south"], "out": ["btn_east"] },
            { "in": ["btn_north"], "out": ["btn_west"] },
        ])));
        assert!(c.give_cards_ids());
        let own = c.snarl.get_node(n).unwrap().params["mappings"].clone();
        let ids = card_ids(&own);
        assert_eq!(ids.len(), 2);
        c.layer_act(LayerScope::Patch, LayerAct::Add);
        let lid = c.layers[0].id.clone();
        c.arm_layer(LayerScope::Patch, &lid);
        {
            let cards = c.snarl.get_node_mut(n).unwrap().params.get_mut("mappings").unwrap().as_array_mut().unwrap();
            cards[0]["out"] = json!(["btn_north"]);
            cards.remove(1);
            cards.push(json!({ "in": ["btn_lb"], "out": ["key_q"] }));
        }
        c.give_cards_ids();
        c.disarm_layer();
        assert_eq!(c.snarl.get_node(n).unwrap().params["mappings"], own, "the patch keeps its own cards");
        let diff = &c.layers[0].settings[0].value;
        assert_eq!(diff["$removed"], json!([ids[1].clone()]));
        assert_eq!(diff["$cards"].as_array().unwrap().len(), 2, "one changed, one added");
        // On: the module shows the layer's cards.
        layers::publish_tints(&ctx, c.layer_tints(&[lid.clone()], None));
        let node = c.snarl.get_node_mut(n).unwrap();
        let shown = layers::show_layer_values(&ctx, &[n.0], node).expect("cards to show");
        let view = node.params["mappings"].clone();
        assert_eq!(card_ids(&view).len(), 2);
        assert_eq!(view[0]["out"], json!(["btn_north"]));
        // An edit of the shown card goes to the layer.
        node.params.get_mut("mappings").unwrap()[0]["out"] = json!(["btn_west"]);
        layers::hide_layer_values(&ctx, node, shown);
        assert_eq!(c.snarl.get_node(n).unwrap().params["mappings"], own);
        for (layer, prefix, path, key, value) in layers::take_layer_edits(&ctx) {
            c.edit_layer_value(&layer, &prefix, &path, &key, value);
        }
        let changed = c.layers[0].settings[0].value["$cards"].as_array().unwrap().clone();
        assert!(changed.iter().any(|card| card["out"] == json!(["btn_west"])), "{changed:?}");
        // The engine runs the same.
        let table = layers::engine_table(&c.layers, &c.snarl, None);
        let merged = flexinput_core::layers::merge_cards(Some(&own), &table[0].overrides[0].value);
        assert_eq!(merged[0]["out"], json!(["btn_west"]));
    }

    /// A card a layer changes draws its curve in the layer's colour over the patch's own
    /// card's; Touch Zones' and Lean's cards get ids as the Remapper's do.
    #[test]
    fn a_layer_changed_cards_curve_has_the_patchs_under_it() {
        use serde_json::json;
        let ctx = eframe::egui::Context::default();
        let mut c = Canvas::default();
        let n = c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), remapper(json!([
            { "in": ["left_trigger"], "out": ["right_trigger"], "curve": [[0.0, 0.0], [1.0, 1.0]] },
        ])));
        let mut tz = gain_node(1.0);
        tz.module_id = "module.touch_zones".into();
        tz.params.insert("zone_maps".into(), json!([{ "in": ["tz_touch"], "out": ["key_a"] }]));
        let z = c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), tz);
        assert!(c.give_cards_ids());
        assert!(flexinput_core::layers::card_id(&c.snarl.get_node(z).unwrap().params["zone_maps"][0]).is_some());
        let card = c.snarl.get_node(n).unwrap().params["mappings"][0].clone();
        let id = flexinput_core::layers::card_id(&card).unwrap().to_string();
        let mut layered = card.clone();
        layered["curve"] = json!([[0.0, 0.0], [0.5, 0.9], [1.0, 1.0]]);
        c.layers.push(Layer {
            id: "aa".into(),
            color: Some([1, 2, 3]),
            settings: vec![LayerSetting {
                path: vec![n.0],
                key: "mappings".into(),
                value: json!({ "$cards": [layered.clone()], "$removed": [] }),
            }],
            ..Default::default()
        });
        layers::publish_tints(&ctx, c.layer_tints(&["aa".into()], None));
        let (color, own) = layers::card_curve_tint(&ctx, n.0, "mappings", layered.as_object().unwrap())
            .expect("the layer changes this card");
        assert_eq!(color, eframe::egui::Color32::from_rgb(1, 2, 3));
        assert_eq!(own, Some(vec![[0.0, 0.0], [1.0, 1.0]]), "the patch's own card's curve");
        assert_eq!(id.len(), 8);
    }

    /// Values shown for gamepad nav aren't in an undo step.
    #[test]
    fn an_undo_step_never_carries_a_shown_value() {
        let ctx = eframe::egui::Context::default();
        let mut c = Canvas::default();
        let n = c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), gain_node(1.0));
        c.layers.push(Layer {
            id: "aa".into(),
            settings: vec![LayerSetting { path: vec![n.0], key: "gain".into(), value: Value::from(0.5) }],
            ..Default::default()
        });
        layers::publish_tints(&ctx, c.layer_tints(&["aa".into()], None));
        let shown = layers::show_all_layer_values(&ctx, &mut c.snarl);
        assert_eq!(gain(&c, &c.snarl, n), 0.5, "nav sees the layer's value");
        assert_eq!(gain(&c, &c.undo_clone(), n), 1.0, "an undo step the patch's");
        layers::hide_all_layer_values(&ctx, &mut c.snarl, shown);
        assert_eq!(gain(&c, &c.snarl, n), 1.0);
    }

    /// A module added while editing stays, as one step to undo.
    #[test]
    fn a_patch_change_while_editing_is_one_undo_step() {
        let mut c = Canvas::default();
        c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), gain_node(1.0));
        c.layer_act(LayerScope::Patch, LayerAct::Add);
        let id = c.layers[0].id.clone();
        assert_eq!(c.layers[0].name, "Layer 1");
        c.arm_layer(LayerScope::Patch, &id);
        c.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), gain_node(2.0));
        c.disarm_layer();
        assert_eq!(c.snarl.nodes_ids_data().count(), 2);
        assert!(c.layers[0].settings.is_empty(), "a new module's settings are the patch's");
        assert_eq!(c.undo_stack.len(), 1);
        c.undo();
        assert_eq!(c.snarl.nodes_ids_data().count(), 1);
    }
}
