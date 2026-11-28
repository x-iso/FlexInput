//! Layers: sets of module settings a patch or a sub-patch turns on and off while
//! it runs, as Steam Input's action layers do (see `flexinput_core::layers` for
//! the pins cards turn them on with, and `flexinput_engine::layers` for how the
//! engine runs them).
//!
//! A layer holds only the settings it changes. A patch's layers live on its
//! canvas and can change any module, inside sub-patches too; a sub-patch's live in
//! the sub-patch, travel with it into a `.fxsp` preset, and reach only what is
//! inside it. Wiring is never part of a layer: an AutoMap Selector driven by a
//! layer's card switches wire paths.
//!
//! **Editing a layer.** A layer is *armed* to edit it: its settings go into the
//! canvas so the modules show (and the engine plays) them, and every setting
//! changed while it is armed is the layer's. Disarming takes the changes into the
//! layer and puts the patch's own settings back. While armed, saves get the
//! patch's own settings (`Canvas::base_snarl`), and the canvas keeps its undo
//! history aside, so nothing of the layer leaks into the patch.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use eframe::egui;
use egui_snarl::{NodeId, Snarl};
use flexinput_core::layers::{layer_pin, parse_layer_pin, LayerAction};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::canvas::NodeData;

/// A layer of a patch or a sub-patch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// What its pins carry: `layer:{id}` and the rest (`flexinput_core::layers`).
    pub id: String,
    pub name: String,
    /// The settings it changes, by where they are in its own canvas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<LayerSetting>,
    /// Its colour: its chip, and the tint on what it changes. Unset, it takes one
    /// from [`PALETTE`] by its place in its list ([`layer_color`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    /// An emoji before its name, from the sub-patch icon set; "" for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
}

/// The colours a layer can take; a new one takes the next.
pub const PALETTE: [[u8; 3]; 8] = [
    [236, 150, 54],  // amber
    [88, 178, 102],  // green
    [178, 110, 230], // violet
    [64, 170, 220],  // sky
    [232, 96, 132],  // rose
    [214, 204, 72],  // yellow
    [80, 200, 186],  // teal
    [226, 112, 72],  // coral
];

/// Layer `l`'s colour, `index` being its place in its list.
pub fn layer_color(l: &Layer, index: usize) -> egui::Color32 {
    let [r, g, b] = l.color.unwrap_or(PALETTE[index % PALETTE.len()]);
    egui::Color32::from_rgb(r, g, b)
}

/// Its name, with its icon in front.
pub fn layer_label(l: &Layer) -> String {
    let name = if l.name.is_empty() { "Layer" } else { l.name.as_str() };
    // An icon of the set is drawn as an image beside it; an emoji picked before is text.
    if l.icon.is_empty() || crate::macro_icons::is_icon_key(&l.icon) { name.to_string() } else { format!("{} {name}", l.icon) }
}

/// One setting a layer changes: param `key` of the node `path` leads to, from the
/// layer's own canvas (the patch's, or its sub-patch's) down through sub-patches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerSetting {
    pub path: Vec<usize>,
    pub key: String,
    pub value: Value,
}

/// Where a layer lives: on the patch, or in the sub-patch these node ids lead to
/// from the top.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LayerScope {
    Patch,
    SubPatch(Vec<usize>),
}

impl LayerScope {
    /// Node ids from the top canvas down to the layer's own canvas.
    pub fn prefix(&self) -> &[usize] {
        match self {
            LayerScope::Patch => &[],
            LayerScope::SubPatch(p) => p,
        }
    }
}

/// Something done to a layer from its chip.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerAct {
    /// Turn it on or off (as a card would).
    Switch { id: String, on: bool },
    /// Start editing its settings.
    Arm(String),
    /// Stop editing.
    Disarm,
    Add,
    Rename { id: String, name: String },
    Recolor { id: String, color: [u8; 3] },
    /// "" for none.
    SetIcon { id: String, icon: String },
    /// Forget every setting it changes.
    Clear(String),
    Delete(String),
}

/// Params a layer never changes: what a node is and how it is wired, which the
/// engine's graph is built from and a layer can't swap at run time.
const STRUCTURAL: &[&str] = &[
    "device_id",
    "input_pin_ids",
    "output_pin_ids",
    "automap_port",
    "pin_index",
    "easy_primary",
    "easy_show",
    "easy_icon",
    "macro_ports",
    "menu_id",
    "n_inputs",
    "n_outputs",
    "zone_mode",
    "jsm_macros",
    // A mapping module's capture in progress: what it's doing now, not a setting.
    "ui_phase",
    "draft_input",
    "draft_output",
    // How a module is laid out on screen, not what it does.
    "ui_w",
    "ui_h",
    "jsm_show_knobs",
    "jsm_strip_frac",
    "jsm_knob_side",
    "jsm_editor_frac",
];

/// Whether a layer can change param `key`: not one of the engine's own (`_…`) or
/// a structural one.
pub fn layered_key(key: &str) -> bool {
    !key.starts_with('_') && !STRUCTURAL.contains(&key)
}

/// The canvas a layer of `scope` lives in, inside `top`.
pub fn scope_snarl<'a>(top: &'a Snarl<NodeData>, scope: &LayerScope) -> Option<&'a Snarl<NodeData>> {
    let mut snarl = top;
    for &id in scope.prefix() {
        snarl = &snarl.get_node(NodeId(id))?.subpatch.as_ref()?.snarl;
    }
    Some(snarl)
}

fn scope_snarl_mut<'a>(top: &'a mut Snarl<NodeData>, scope: &LayerScope) -> Option<&'a mut Snarl<NodeData>> {
    let mut snarl = top;
    for &id in scope.prefix() {
        snarl = &mut snarl.get_node_mut(NodeId(id))?.subpatch.as_mut()?.snarl;
    }
    Some(snarl)
}

/// The layers of `scope`: the patch's (`patch`), or a sub-patch's.
pub fn layers_in<'a>(patch: &'a [Layer], top: &'a Snarl<NodeData>, scope: &LayerScope) -> Option<&'a [Layer]> {
    match scope {
        LayerScope::Patch => Some(patch),
        LayerScope::SubPatch(p) => {
            let (&last, up) = p.split_last()?;
            let parent = scope_snarl(top, &LayerScope::SubPatch(up.to_vec()))?;
            Some(&parent.get_node(NodeId(last))?.subpatch.as_ref()?.layers)
        }
    }
}

/// [`layers_in`], to change them.
pub fn layers_in_mut<'a>(
    patch: &'a mut Vec<Layer>,
    top: &'a mut Snarl<NodeData>,
    scope: &LayerScope,
) -> Option<&'a mut Vec<Layer>> {
    match scope {
        LayerScope::Patch => Some(patch),
        LayerScope::SubPatch(p) => {
            let (&last, up) = p.split_last()?;
            let parent = scope_snarl_mut(top, &LayerScope::SubPatch(up.to_vec()))?;
            Some(&mut parent.get_node_mut(NodeId(last))?.subpatch.as_mut()?.layers)
        }
    }
}

/// A name no layer of `layers` has yet: "Layer 1", "Layer 2", …
pub fn fresh_name(layers: &[Layer]) -> String {
    (1..)
        .map(|i| format!("Layer {i}"))
        .find(|n| !layers.iter().any(|l| &l.name == n))
        .unwrap_or_default()
}

/// Every node's params under `snarl`, sub-patches' nodes among them, by path from
/// the top canvas (`prefix` is the path to `snarl`).
pub fn collect_params(
    snarl: &Snarl<NodeData>,
    prefix: &[usize],
    out: &mut HashMap<Vec<usize>, HashMap<String, Value>>,
) {
    for (id, node) in snarl.nodes_ids_data() {
        let mut path = prefix.to_vec();
        path.push(id.0);
        if let Some(sp) = node.value.subpatch.as_ref() {
            collect_params(&sp.snarl, &path, out);
        }
        out.insert(path, node.value.params.clone());
    }
}

fn node_at_mut<'a>(top: &'a mut Snarl<NodeData>, path: &[usize]) -> Option<&'a mut NodeData> {
    let (&last, up) = path.split_last()?;
    let mut snarl = top;
    for &id in up {
        snarl = &mut snarl.get_node_mut(NodeId(id))?.subpatch.as_mut()?.snarl;
    }
    snarl.get_node_mut(NodeId(last))
}

fn node_at<'a>(top: &'a Snarl<NodeData>, path: &[usize]) -> Option<&'a NodeData> {
    let (&last, up) = path.split_last()?;
    let mut snarl = top;
    for &id in up {
        snarl = &snarl.get_node(NodeId(id))?.subpatch.as_ref()?.snarl;
    }
    snarl.get_node(NodeId(last))
}

/// Put `settings` (paths from the canvas `prefix` leads to) into `top`. Returns
/// whether any landed.
pub fn put_settings(top: &mut Snarl<NodeData>, prefix: &[usize], settings: &[LayerSetting]) -> bool {
    let mut any = false;
    for s in settings {
        if !layered_key(&s.key) {
            continue;
        }
        let path: Vec<usize> = prefix.iter().chain(&s.path).copied().collect();
        if let Some(node) = node_at_mut(top, &path) {
            // A card list keeps the patch's cards that switch layers, as the engine runs it.
            let value = if flexinput_core::layers::is_card_list(&s.key) {
                flexinput_core::layers::merge_cards(node.params.get(&s.key), &s.value)
            } else {
                s.value.clone()
            };
            node.params.insert(s.key.clone(), value);
            any = true;
        }
    }
    any
}

/// What changed from `baseline` to `top`, as settings of the canvas `prefix`
/// leads to: each layered param that is different now (or new). A node added
/// since has no baseline and changes nothing; a param taken away is left out.
pub fn changed_settings(
    top: &Snarl<NodeData>,
    prefix: &[usize],
    baseline: &HashMap<Vec<usize>, HashMap<String, Value>>,
) -> Vec<LayerSetting> {
    let mut out = Vec::new();
    let mut paths: Vec<&Vec<usize>> = baseline.keys().collect();
    paths.sort();
    for path in paths {
        let Some(node) = node_at(top, path) else { continue };
        let was = &baseline[path];
        let mut keys: Vec<&String> = node.params.keys().filter(|k| layered_key(k)).collect();
        keys.sort();
        for key in keys {
            let now = &node.params[key];
            // A card list: the cards changed, added and taken away, by id.
            if flexinput_core::layers::is_card_list(key) {
                if let Some(diff) = flexinput_core::layers::card_diff(was.get(key), now) {
                    out.push(LayerSetting { path: path[prefix.len()..].to_vec(), key: key.clone(), value: diff });
                }
                continue;
            }
            if was.get(key) != Some(now) {
                out.push(LayerSetting {
                    path: path[prefix.len()..].to_vec(),
                    key: key.clone(),
                    value: now.clone(),
                });
            }
        }
    }
    out
}

/// Give every card of the patch's mapping modules an id of its own (`card_id`), sub-patches
/// included: a card just made, Learned, pasted or loaded from an older patch has none, and a
/// layer names cards by id. Returns whether any changed; cheap when none did.
pub fn give_cards_ids(top: &mut Snarl<NodeData>) -> bool {
    let mut changed = false;
    let ids: Vec<NodeId> = top.nodes_ids_data().map(|(id, _)| id).collect();
    for id in ids {
        let Some(node) = top.get_node_mut(id) else { continue };
        for key in flexinput_core::layers::CARD_LISTS {
            if let Some(Value::Array(cards)) = node.params.get_mut(key) {
                changed |= flexinput_core::layers::give_cards_ids(cards);
            }
        }
        if let Some(sp) = node.subpatch.as_mut() {
            changed |= give_cards_ids(&mut sp.snarl);
        }
    }
    changed
}

/// `v` (a serialized sub-patch) without its cards' ids, for comparing it with a preset's file
/// saved before cards had them.
pub fn strip_card_ids(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for key in flexinput_core::layers::CARD_LISTS {
                if let Some(Value::Array(cards)) = o.get_mut(key) {
                    for c in cards.iter_mut() {
                        if let Some(c) = c.as_object_mut() {
                            c.remove(flexinput_core::layers::CARD_ID);
                        }
                    }
                }
            }
            o.values_mut().for_each(strip_card_ids);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_card_ids),
        _ => {}
    }
}

/// The card lists of the nodes `baseline` covers, as they are now (a layer's view of
/// them), for [`keep_switch_cards`] after the patch's own come back.
pub fn card_list_views(top: &Snarl<NodeData>, baseline: &HashMap<Vec<usize>, HashMap<String, Value>>) -> Vec<(Vec<usize>, String, Value)> {
    let mut out = Vec::new();
    for p in baseline.keys() {
        let Some(n) = node_at(top, p) else { continue };
        for key in flexinput_core::layers::CARD_LISTS {
            if let Some(v) = n.params.get(key) {
                out.push((p.clone(), key.to_string(), v.clone()));
            }
        }
    }
    out
}

/// Cards that turn layers on and off are always the patch's: one made, changed or taken
/// away in a layer's view of a list (`views`, see [`card_list_views`]) is so in the patch's
/// own list.
pub fn keep_switch_cards(top: &mut Snarl<NodeData>, views: &[(Vec<usize>, String, Value)]) {
    for (path, key, view) in views {
        let Some(node) = node_at_mut(top, path) else { continue };
        let Some(view) = view.as_array() else { continue };
        let list = node.params.entry(key.clone()).or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(own) = list {
            keep_switch_cards_in(own, view);
        }
    }
}

fn keep_switch_cards_in(own: &mut Vec<Value>, view: &[Value]) {
    use flexinput_core::layers::{card_id, card_switches_layer};
    let same = |a: &Value, b: &Value| card_id(a).is_some() && card_id(a) == card_id(b);
    own.retain(|c| !card_switches_layer(c) || view.iter().any(|v| same(v, c)));
    for v in view.iter().filter(|c| card_switches_layer(c)) {
        match own.iter_mut().find(|c| same(c, v)) {
            Some(slot) => *slot = v.clone(),
            None => own.push(v.clone()),
        }
    }
}

/// Put `baseline`'s layered params back into `top` wherever they differ: the
/// patch's own settings, after a layer's were in it.
pub fn restore_baseline(top: &mut Snarl<NodeData>, baseline: &HashMap<Vec<usize>, HashMap<String, Value>>) {
    for (path, was) in baseline {
        let Some(node) = node_at_mut(top, path) else { continue };
        let keys: Vec<String> = node.params.keys().chain(was.keys())
            .filter(|k| layered_key(k))
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for key in keys {
            match was.get(&key) {
                Some(v) if node.params.get(&key) != Some(v) => {
                    node.params.insert(key, v.clone());
                }
                None => {
                    node.params.remove(&key);
                }
                _ => {}
            }
        }
    }
}

/// The patch's layers and every sub-patch's, as the engine runs them: each
/// setting's path from the top canvas. `skip` is the layer being edited, whose
/// settings are in the patch's graph already.
pub fn engine_table(patch: &[Layer], top: &Snarl<NodeData>, skip: Option<&str>) -> Vec<flexinput_engine::LayerSnap> {
    fn push(layers: &[Layer], prefix: &[usize], skip: Option<&str>, out: &mut Vec<flexinput_engine::LayerSnap>) {
        for l in layers {
            if Some(l.id.as_str()) == skip {
                continue;
            }
            out.push(flexinput_engine::LayerSnap {
                id: l.id.clone(),
                overrides: l.settings.iter()
                    .filter(|s| layered_key(&s.key))
                    .map(|s| flexinput_engine::LayerOverride {
                        path: prefix.iter().chain(&s.path).copied().collect(),
                        key: s.key.clone(),
                        value: s.value.clone(),
                    })
                    .collect(),
            });
        }
    }
    fn walk(snarl: &Snarl<NodeData>, prefix: &[usize], skip: Option<&str>, out: &mut Vec<flexinput_engine::LayerSnap>) {
        for (id, node) in snarl.nodes_ids_data() {
            let Some(sp) = node.value.subpatch.as_ref() else { continue };
            let mut path = prefix.to_vec();
            path.push(id.0);
            push(&sp.layers, &path, skip, out);
            walk(&sp.snarl, &path, skip, out);
        }
    }
    let mut out = Vec::new();
    push(patch, &[], skip, &mut out);
    walk(top, &[], skip, &mut out);
    out
}

/// One layer of the patch or of a sub-patch: where it lives, that place's name,
/// its place in its list (for its colour) and the layer.
pub type PlacedLayer<'a> = (LayerScope, String, usize, &'a Layer);

/// Every layer of the patch and its sub-patches.
pub fn all_layers<'a>(patch: &'a [Layer], top: &'a Snarl<NodeData>) -> Vec<PlacedLayer<'a>> {
    fn walk<'a>(snarl: &'a Snarl<NodeData>, prefix: &[usize], out: &mut Vec<PlacedLayer<'a>>) {
        for (id, node) in snarl.nodes_ids_data() {
            let Some(sp) = node.value.subpatch.as_ref() else { continue };
            let mut path = prefix.to_vec();
            path.push(id.0);
            let name = if sp.display_name.is_empty() { node.value.display_name.clone() } else { sp.display_name.clone() };
            for (i, l) in sp.layers.iter().enumerate() {
                out.push((LayerScope::SubPatch(path.clone()), name.clone(), i, l));
            }
            walk(&sp.snarl, &path, out);
        }
    }
    let mut out: Vec<PlacedLayer<'a>> = patch.iter().enumerate()
        .map(|(i, l)| (LayerScope::Patch, String::new(), i, l))
        .collect();
    walk(top, &[], &mut out);
    out
}

/// What the picker and the cards show for each layer pin: "Aim — Toggle", with
/// the sub-patch's name for a sub-patch's layer.
pub fn display_entries(patch: &[Layer], top: &Snarl<NodeData>) -> Vec<crate::macro_icons::MacroDisplayEntry> {
    let mut out = Vec::new();
    for (scope, place, index, layer) in all_layers(patch, top) {
        let name = layer_label(layer);
        let tint = layer_color(layer, index);
        for action in LayerAction::ALL {
            let name = match scope {
                LayerScope::Patch => format!("{name} — {}", action.verb()),
                LayerScope::SubPatch(_) => format!("{place} › {name} — {}", action.verb()),
            };
            out.push(crate::macro_icons::MacroDisplayEntry {
                pin: layer_pin(&layer.id, action),
                name,
                icon: if crate::macro_icons::is_icon_key(&layer.icon) { layer.icon.clone() } else { String::new() },
                icon_svg: String::new(),
                signal_type: flexinput_core::SignalType::Bool,
                tint: Some([tint.r(), tint.g(), tint.b()]),
            });
        }
    }
    out
}

/// The chords (two buttons or more) of the patch's cards that switch layers, its
/// sub-patches' among them: FlexInput's own gamepad nav leaves a press of one to the card
/// (`gamepad_nav::ChordDefer`).
pub fn switch_chords(canvas: &crate::canvas::Canvas) -> Vec<Vec<String>> {
    fn walk(snarl: &Snarl<NodeData>, out: &mut Vec<Vec<String>>) {
        for (_, n) in snarl.nodes_ids_data() {
            if matches!(n.value.module_id.as_str(), "module.remapper" | "module.map_action") {
                let cards = n.value.params.get(flexinput_core::layers::CARDS_KEY).and_then(|v| v.as_array());
                for card in cards.into_iter().flatten().filter(|c| flexinput_core::layers::card_switches_layer(c)) {
                    let chord: Vec<String> = card.get("in").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    if chord.len() >= 2 && !out.contains(&chord) {
                        out.push(chord);
                    }
                }
            }
            if let Some(sp) = n.value.subpatch.as_ref() {
                walk(&sp.snarl, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&canvas.snarl, &mut out);
    out
}

/// Give every layer in the patch an id of its own. Two sub-patches loaded from one
/// preset carry the same layer ids, and a card in one would turn on the other's
/// layer too: the later one gets new ids, and the cards inside it follow. Returns
/// whether anything changed. Cheap when nothing clashes, which is always but
/// right after a paste or a preset load.
pub fn give_layers_own_ids(patch: &[Layer], top: &mut Snarl<NodeData>) -> bool {
    fn walk(snarl: &mut Snarl<NodeData>, taken: &mut HashSet<String>) -> bool {
        let mut changed = false;
        let ids: Vec<NodeId> = snarl.nodes_ids_data().map(|(id, _)| id).collect();
        for id in ids {
            let Some(sp) = snarl.get_node_mut(id).and_then(|n| n.subpatch.as_mut()) else { continue };
            let mut renamed: Vec<(String, String)> = Vec::new();
            for l in &mut sp.layers {
                if !taken.insert(l.id.clone()) {
                    let fresh = loop {
                        let f = flexinput_core::layers::new_layer_id();
                        if !taken.contains(&f) {
                            break f;
                        }
                    };
                    taken.insert(fresh.clone());
                    renamed.push((std::mem::replace(&mut l.id, fresh.clone()), fresh));
                }
            }
            if !renamed.is_empty() {
                rename_refs(&mut sp.snarl, &renamed);
                changed = true;
            }
            changed |= walk(&mut sp.snarl, taken);
        }
        changed
    }
    let mut taken: HashSet<String> = patch.iter().map(|l| l.id.clone()).collect();
    walk(top, &mut taken)
}

/// Point every layer pin in `snarl`'s params (cards' `out` lists, wherever they
/// keep them) from each old id to its new one.
fn rename_refs(snarl: &mut Snarl<NodeData>, renamed: &[(String, String)]) {
    fn fix(v: &mut Value, renamed: &[(String, String)]) {
        match v {
            Value::String(s) => {
                if let Some((id, action)) = parse_layer_pin(s) {
                    if let Some((_, new)) = renamed.iter().find(|(old, _)| old == id) {
                        *s = layer_pin(new, action);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|v| fix(v, renamed)),
            Value::Object(o) => o.values_mut().for_each(|v| fix(v, renamed)),
            _ => {}
        }
    }
    let ids: Vec<NodeId> = snarl.nodes_ids_data().map(|(id, _)| id).collect();
    for id in ids {
        let Some(node) = snarl.get_node_mut(id) else { continue };
        node.params.values_mut().for_each(|v| fix(v, renamed));
        if let Some(sp) = node.subpatch.as_mut() {
            rename_refs(&mut sp.snarl, renamed);
        }
    }
}

// ── What a layer changes ─────────────────────────────────────────────────────

/// A value as a layer's list shows it: a number to three places, a word.
fn value_text(v: &Value) -> String {
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e9 => format!("{f:.0}"),
            Some(f) => format!("{f:.3}").trim_end_matches('0').trim_end_matches('.').to_string(),
            None => n.to_string(),
        },
        Value::Bool(b) => if *b { "on".into() } else { "off".into() },
        Value::String(s) => s.clone(),
        Value::Null => "none".into(),
        _ => "changed".into(),
    }
}

/// What a layer changes, a line each: "Gyro › speed: 0.5". `prefix` is the path to
/// its own canvas inside `top`.
pub fn describe(layer: &Layer, prefix: &[usize], top: &Snarl<NodeData>) -> Vec<String> {
    layer.settings.iter()
        .filter(|s| layered_key(&s.key))
        .map(|s| {
            let path: Vec<usize> = prefix.iter().chain(&s.path).copied().collect();
            let module = node_at(top, &path).map_or("(gone)".to_string(), |n| n.display_name.clone());
            if flexinput_core::layers::is_card_diff(&s.value) {
                let n = |k: &str| s.value.get(k).and_then(|v| v.as_array()).map_or(0, |a| a.len());
                let (changed, removed) = (n(flexinput_core::layers::CARDS_CHANGED), n(flexinput_core::layers::CARDS_REMOVED));
                return format!("{module} › cards: {changed} changed or added, {removed} taken away");
            }
            format!("{module} › {}: {}", s.key, value_text(&s.value))
        })
        .collect()
}

/// One setting a layer shows as changed: its colour, the layer and its value.
#[derive(Clone, Debug)]
pub struct Tint {
    pub color: egui::Color32,
    pub layer: String,
    pub key: String,
    pub value: Value,
    /// The layer's id and the path to its own canvas, for an edit of the value it
    /// shows to go back to it.
    pub layer_id: String,
    pub prefix: Vec<usize>,
    /// The layer being edited: its values are in the canvas already.
    pub armed: bool,
    /// For a card list, the module's own list: what a changed card was, and the cards the
    /// layer takes away.
    pub own: Option<Value>,
}

/// What the layers on screen change, for the canvas and the pins to tint: by node
/// path from the top canvas, the node's module and its changed params (the layer
/// on top last).
#[derive(Clone, Debug, Default)]
pub struct LayerTints {
    pub nodes: HashMap<Vec<usize>, (String, Vec<Tint>)>,
}

impl LayerTints {
    /// Add `settings` of layer `layer_id` (named `layer`, paths from `prefix`) on top of
    /// what's there. `armed`: the layer being edited.
    #[allow(clippy::too_many_arguments)]
    pub fn add(&mut self, top: &Snarl<NodeData>, prefix: &[usize], layer_id: &str, layer: &str, color: egui::Color32, settings: &[LayerSetting], armed: bool) {
        // Only what a layer can change: an older layer may hold a module's layout too.
        for s in settings.iter().filter(|s| layered_key(&s.key)) {
            let path: Vec<usize> = prefix.iter().chain(&s.path).copied().collect();
            let Some(node) = node_at(top, &path) else { continue };
            let entry = self.nodes.entry(path).or_insert_with(|| (node.module_id.clone(), Vec::new()));
            entry.1.retain(|t| t.key != s.key);
            let own = (!armed && flexinput_core::layers::is_card_list(&s.key))
                .then(|| node.params.get(&s.key).cloned())
                .flatten();
            entry.1.push(Tint {
                color,
                layer: layer.to_string(),
                key: s.key.clone(),
                value: s.value.clone(),
                layer_id: layer_id.to_string(),
                prefix: prefix.to_vec(),
                armed,
                own,
            });
        }
    }

    pub fn node(&self, path: &[usize]) -> Option<&(String, Vec<Tint>)> {
        self.nodes.get(path)
    }
}

fn tints_id() -> egui::Id {
    egui::Id::new("fxi_layer_tints")
}

fn tint_prefix_id() -> egui::Id {
    egui::Id::new("fxi_layer_tint_prefix")
}

fn preview_id() -> egui::Id {
    egui::Id::new("fxi_layer_preview")
}

/// Publish this frame's tints (`Canvas::layer_tints`).
pub fn publish_tints(ctx: &egui::Context, tints: LayerTints) {
    egui::style::set_selection_accent_hook(selection_accent);
    ctx.data_mut(|d| d.insert_temp(tints_id(), std::sync::Arc::new(tints)));
}

/// This frame's tints.
pub fn tints(ctx: &egui::Context) -> std::sync::Arc<LayerTints> {
    ctx.data(|d| d.get_temp::<std::sync::Arc<LayerTints>>(tints_id())).unwrap_or_default()
}

/// The layer a chip under the pointer previews, if one was hovered a moment ago.
pub fn previewed(ctx: &egui::Context) -> Option<String> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<(u64, String)>(preview_id()))
        .filter(|(p, _)| pass.saturating_sub(*p) <= 2)
        .map(|(_, id)| id)
}

fn in_pin_id() -> egui::Id {
    egui::Id::new("fxi_layer_tint_in_pin")
}

/// Module controls drawn from here on belong to the canvas `prefix` leads to (a
/// canvas sets it as it starts drawing).
pub fn set_tint_prefix(ctx: &egui::Context, prefix: Vec<usize>) {
    ctx.data_mut(|d| d.insert_temp(tint_prefix_id(), prefix));
}

/// Draw a pin with `f`: its module belongs to the canvas `prefix` leads to (a pin
/// on a sub-patch's body draws its module from inside that sub-patch). A pin of one
/// control (`whole` false) is tinted as a whole ([`tint_pin`]); a whole module's pin
/// marks each control inside it, as the canvas does.
pub fn with_pin_tint<R>(ctx: &egui::Context, prefix: Vec<usize>, whole: bool, f: impl FnOnce() -> R) -> R {
    let old = tint_prefix(ctx);
    let was_in_pin = ctx.data(|d| d.get_temp::<bool>(in_pin_id())).unwrap_or(false);
    ctx.data_mut(|d| {
        d.insert_temp(tint_prefix_id(), prefix);
        d.insert_temp(in_pin_id(), !whole);
    });
    let r = f();
    ctx.data_mut(|d| {
        d.insert_temp(tint_prefix_id(), old);
        d.insert_temp(in_pin_id(), was_in_pin);
    });
    r
}

/// Tint a pinned control of the node at `path` if a layer on screen changes what it
/// edits. A pin whose module the gamepad tables don't break down (a whole module,
/// a graph) gets a thin frame when the layer changes anything of its module.
pub fn tint_pin(ui: &egui::Ui, path: &[usize], element_id: &str, rect: egui::Rect) {
    let tints = tints(ui.ctx());
    let Some((module, list)) = tints.node(path) else { return };
    let keys = control_keys(module, element_id);
    if let Some(t) = list.iter().rev().find(|t| keys.contains(&t.key)) {
        paint_tint(ui.painter(), rect.shrink(1.5), t.color);
        note_tinted(ui, rect, t.color);
    } else if whole_pin(module, element_id) {
        // A whole module or a graph: its controls are marked inside; a frame says the
        // layer changes something of it.
        if let Some(t) = list.last() {
            ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, t.color), egui::StrokeKind::Inside);
        }
    }
}

/// The canvas path module controls drawn now belong to ([`with_tint_prefix`]).
pub fn tint_prefix(ctx: &egui::Context) -> Vec<usize> {
    ctx.data(|d| d.get_temp::<Vec<usize>>(tint_prefix_id())).unwrap_or_default()
}

/// The params a control of a module edits, where its gamepad table names them.
fn element_keys(module_id: &str, element_id: &str) -> Vec<&'static str> {
    use crate::app::NavField;
    crate::app::element_fields(module_id, element_id, None)
        .into_iter()
        .flat_map(|f| match f.field {
            NavField::Value { key, .. } | NavField::Enum { key, .. } | NavField::Toggle { key } => vec![key],
            NavField::EnumPair { key_a, key_b, .. } => vec![key_a, key_b],
            _ => Vec::new(),
        })
        .collect()
}

/// The params a control may edit, for tinting: its gamepad table's, else a param named as
/// the control is (`value`, `mode`) or with `_param` after it.
fn control_keys(module_id: &str, element_id: &str) -> Vec<String> {
    let keys = element_keys(module_id, element_id);
    if !keys.is_empty() {
        return keys.into_iter().map(str::to_string).collect();
    }
    // Controls named otherwise than what they edit.
    let alias: &[&str] = match (module_id, element_id) {
        ("module.switch", "toggle") => &["active"],
        ("module.dropdown", "selection") => &["selected_index"],
        _ => &[],
    };
    let mut keys = vec![element_id.to_string(), format!("{element_id}_param")];
    keys.extend(alias.iter().map(|k| k.to_string()));
    keys
}

/// Tint control `element_id` of node `node` (a uid in the canvas drawn now) if a
/// layer on screen changes what it edits: a frame round it in the layer's colour.
pub fn tint_element(ui: &egui::Ui, node: usize, element_id: &str, rect: egui::Rect) {
    let tints = tints(ui.ctx());
    if tints.nodes.is_empty() || ui.ctx().data(|d| d.get_temp::<bool>(in_pin_id())).unwrap_or(false) {
        return;
    }
    let mut path = tint_prefix(ui.ctx());
    path.push(node);
    let Some((module, list)) = tints.node(&path) else { return };
    let keys = control_keys(module, element_id);
    let Some(t) = list.iter().rev().find(|t| keys.contains(&t.key)) else { return };
    paint_tint(ui.painter(), rect, t.color);
    note_tinted(ui, rect, t.color);
}

fn paint_tint(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let rect = rect.expand(1.5);
    painter.rect_filled(rect, 4.0, color.gamma_multiply(0.14));
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.5_f32, color), egui::StrokeKind::Outside);
}

/// Tinted controls by layer of the screen: this pass's (being gathered) and the last.
#[derive(Clone, Default)]
struct TintedRects {
    pass: u64,
    now: Vec<(egui::LayerId, egui::Rect, egui::Color32)>,
    last: Vec<(egui::LayerId, egui::Rect, egui::Color32)>,
}

fn tinted_rects_id() -> egui::Id {
    egui::Id::new("fxi_layer_tinted_rects")
}

/// Note a tinted control, for the accents drawn inside it next pass ([`accent`]).
fn note_tinted(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let pass = ui.ctx().cumulative_pass_nr();
    let layer = ui.layer_id();
    ui.ctx().data_mut(|d| {
        let t = d.get_temp_mut_or_default::<TintedRects>(tinted_rects_id());
        if t.pass != pass {
            t.last = std::mem::take(&mut t.now);
            t.pass = pass;
        }
        t.now.push((layer, rect, color));
    });
}

/// The accent for something drawn at `rect`: the colour of a layer changing the control
/// it's part of, else the theme's. A value box or a picked option inside a control a layer
/// changes wears the layer's colour where it wears the accent.
pub fn accent(ui: &egui::Ui, rect: egui::Rect) -> egui::Color32 {
    accent_at(ui.ctx(), ui.layer_id(), rect).unwrap_or(ui.visuals().selection.bg_fill)
}

/// [`accent`]: the layer's colour, if one changes the control at `rect` on `layer`.
pub fn accent_at(ctx: &egui::Context, layer: egui::LayerId, rect: egui::Rect) -> Option<egui::Color32> {
    let pass = ctx.cumulative_pass_nr();
    let at = rect.center();
    ctx.data(|d| d.get_temp::<TintedRects>(tinted_rects_id()))
        .and_then(|t| {
            // The last pass's tints only: nothing tinted since (no layer on any more)
            // leaves an old list behind, which mustn't colour anything.
            let list = if t.pass == pass {
                t.last
            } else if t.pass + 1 == pass {
                t.now
            } else {
                return None;
            };
            list.into_iter().rev().find(|(l, r, _)| *l == layer && r.contains(at)).map(|(_, _, c)| c)
        })
}

/// egui's selected buttons (an option picked in a row) inside a control a layer changes
/// wear the layer's colour too (`egui::style::set_selection_accent_hook`).
fn selection_accent(ctx: &egui::Context, layer: egui::LayerId, rect: egui::Rect) -> Option<(egui::Color32, egui::Color32)> {
    accent_at(ctx, layer, rect).map(|c| (c, crate::widgets::text_on(c)))
}

/// The values a node shows from layers over its own while it's drawn
/// ([`show_layer_values`]): each key, its own value and the one shown, and the tint.
pub struct Shown {
    path: Vec<usize>,
    /// Each param: its own value, the one shown, and the layer's tint.
    items: Vec<(Option<Value>, Value, Tint)>,
}

fn layer_edits_id() -> egui::Id {
    egui::Id::new("fxi_layer_value_edits")
}

/// An edit of a value a layer shows: the layer (its id, the path to its canvas), the
/// node's path from the top, the param and the new value.
pub type LayerValueEdit = (String, Vec<usize>, Vec<usize>, String, Value);

/// Put the values the layers on screen give the node at `path` into it for its drawing:
/// its controls show what the layer sets. Numbers, switches and words only - a list (a
/// curve's points, cards) stays the patch's and the layer's is drawn over it. The layer
/// being edited needs none: its values are in the canvas.
pub fn show_layer_values(ctx: &egui::Context, path: &[usize], node: &mut NodeData) -> Option<Shown> {
    let tints = tints(ctx);
    let (_, list) = tints.node(path)?;
    let mut items = Vec::new();
    for t in list.iter().filter(|t| !t.armed) {
        // A card list shows the layer's cards over the module's own; another list (a
        // curve's points) stays the module's, the layer's drawn over it.
        let shown = if flexinput_core::layers::is_card_list(&t.key) {
            flexinput_core::layers::merge_cards(node.params.get(&t.key), &t.value)
        } else if t.value.is_array() || t.value.is_object() {
            continue;
        } else {
            t.value.clone()
        };
        let own = node.params.insert(t.key.clone(), shown.clone());
        items.push((own, shown, t.clone()));
    }
    (!items.is_empty()).then(|| Shown { path: path.to_vec(), items })
}

/// Take the values [`show_layer_values`] put in out again. One changed while drawn was
/// edited where the layer's value showed: it goes to that layer ([`take_layer_edits`]).
pub fn hide_layer_values(ctx: &egui::Context, node: &mut NodeData, shown: Shown) {
    let mut edits: Vec<LayerValueEdit> = Vec::new();
    for (own, shown_value, t) in shown.items {
        let now = match own.clone() {
            Some(v) => node.params.insert(t.key.clone(), v),
            None => node.params.remove(&t.key),
        };
        let Some(now) = now.filter(|now| *now != shown_value) else { continue };
        // A card list edited: the layer's cards are what it changes from the module's own
        // now (none left: `null`, the layer no longer changes them). What only the UI keeps
        // on a card (`_…`, a fold) stays on the module's own card, layer or not.
        let value = if flexinput_core::layers::is_card_list(&t.key) {
            keep_card_ui_state(node, &t.key, &now);
            if let (Some(Value::Array(own_list)), Some(view)) = (node.params.get_mut(&t.key), now.as_array()) {
                keep_switch_cards_in(own_list, view);
            }
            flexinput_core::layers::card_diff(own.as_ref(), &now).unwrap_or(Value::Null)
        } else {
            now
        };
        if value == t.value {
            continue;
        }
        edits.push((t.layer_id, t.prefix, shown.path.clone(), t.key, value));
    }
    if !edits.is_empty() {
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<LayerValueEdit>>(layer_edits_id()).extend(edits));
    }
}

/// Copy the `_…` fields of the cards in `shown` onto the node's own cards with the same ids.
fn keep_card_ui_state(node: &mut NodeData, key: &str, shown: &Value) {
    let Some(Value::Array(own)) = node.params.get_mut(key) else { return };
    for card in shown.as_array().into_iter().flatten() {
        let Some(id) = flexinput_core::layers::card_id(card) else { continue };
        let Some(mine) = own.iter_mut().find(|c| flexinput_core::layers::card_id(c) == Some(id)) else { continue };
        let (Some(from), Some(to)) = (card.as_object(), mine.as_object_mut()) else { continue };
        for (k, v) in from.iter().filter(|(k, _)| k.starts_with('_')) {
            to.insert(k.clone(), v.clone());
        }
    }
}

thread_local! {
    /// What [`show_all_layer_values`] put into the canvas and hasn't taken out yet: by node
    /// path, param and the node's own value. An undo snapshot taken meanwhile gets the own
    /// values back ([`unshow`]).
    static SHOWN_NOW: std::cell::RefCell<Vec<(Vec<usize>, String, Option<Value>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Put the values the layers on screen show into every node of `top` that has some, for
/// code that reads and writes them outside drawing - gamepad nav editing a value - to see
/// and edit what's shown. [`hide_all_layer_values`] takes them out again.
pub fn show_all_layer_values(ctx: &egui::Context, top: &mut Snarl<NodeData>) -> Vec<Shown> {
    let tints = tints(ctx);
    let mut out = Vec::new();
    for path in tints.nodes.keys() {
        let Some(node) = node_at_mut(top, path) else { continue };
        if let Some(shown) = show_layer_values(ctx, path, node) {
            SHOWN_NOW.with(|s| {
                let mut s = s.borrow_mut();
                for (own, _, t) in &shown.items {
                    s.push((path.clone(), t.key.clone(), own.clone()));
                }
            });
            out.push(shown);
        }
    }
    out
}

/// Take out what [`show_all_layer_values`] put in; an edit made meanwhile goes to its layer.
pub fn hide_all_layer_values(ctx: &egui::Context, top: &mut Snarl<NodeData>, shown: Vec<Shown>) {
    for s in shown {
        if let Some(node) = node_at_mut(top, &s.path) {
            hide_layer_values(ctx, node, s);
        }
    }
    SHOWN_NOW.with(|s| s.borrow_mut().clear());
}

/// A snapshot of the canvas taken while layer values are shown in it, with the nodes' own
/// values back: an undo step never carries a layer's value into the patch.
pub fn unshow(snap: &mut Snarl<NodeData>) {
    SHOWN_NOW.with(|s| {
        for (path, key, own) in s.borrow().iter() {
            let Some(node) = node_at_mut(snap, path) else { continue };
            match own {
                Some(v) => { node.params.insert(key.clone(), v.clone()); }
                None => { node.params.remove(key); }
            }
        }
    });
}

/// Mark card `card` of list `list_key` on node `node` (a uid in the canvas drawn now) if a
/// layer on screen changes or adds it: a frame and fill in the layer's colour, and its
/// accent buttons in that colour. `rect` is the card's.
pub fn tint_card(ui: &egui::Ui, node: usize, list_key: &str, card: &serde_json::Map<String, Value>, rect: egui::Rect) {
    let Some(id) = card.get(flexinput_core::layers::CARD_ID).and_then(|v| v.as_str()) else { return };
    let tints = tints(ui.ctx());
    if tints.nodes.is_empty() {
        return;
    }
    let mut path = tint_prefix(ui.ctx());
    path.push(node);
    let Some((_, list)) = tints.node(&path) else { return };
    let changes = |t: &&Tint| {
        t.key == list_key
            && t.value.get(flexinput_core::layers::CARDS_CHANGED).and_then(|v| v.as_array())
                .is_some_and(|a| a.iter().any(|c| flexinput_core::layers::card_id(c) == Some(id)))
    };
    let Some(t) = list.iter().rev().find(changes) else { return };
    paint_tint(ui.painter(), rect, t.color);
    note_tinted(ui, rect, t.color);
}

/// A card a layer on screen changes or adds, for its curve: the layer's colour, and the
/// curve the module's own card has (`None` for a card the layer adds, or one with no curve
/// of its own - the straight line).
pub fn card_curve_tint(ctx: &egui::Context, node: usize, list_key: &str, card: &serde_json::Map<String, Value>) -> Option<(egui::Color32, Option<Vec<[f32; 2]>>)> {
    let id = card.get(flexinput_core::layers::CARD_ID).and_then(|v| v.as_str())?;
    let tints = tints(ctx);
    let mut path = tint_prefix(ctx);
    path.push(node);
    let (_, list) = tints.node(&path)?;
    let t = list.iter().rev().find(|t| {
        t.key == list_key
            && t.value.get(flexinput_core::layers::CARDS_CHANGED).and_then(|v| v.as_array())
                .is_some_and(|a| a.iter().any(|c| flexinput_core::layers::card_id(c) == Some(id)))
    })?;
    let own = t.own.as_ref().and_then(|v| v.as_array())
        .and_then(|a| a.iter().find(|c| flexinput_core::layers::card_id(c) == Some(id)))
        .map(|c| c.get("curve").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
            }).collect::<Vec<_>>())
            .filter(|p| p.len() >= 2)
            .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]));
    Some((t.color, own))
}

/// The cards of list `list_key` on node `node` that a layer on screen takes away, drawn
/// below the module's cards: dimmed and struck through, in the layer's colour.
pub fn show_removed_cards(ui: &mut egui::Ui, node: usize, list_key: &str) {
    let tints = tints(ui.ctx());
    if tints.nodes.is_empty() {
        return;
    }
    let mut path = tint_prefix(ui.ctx());
    path.push(node);
    let Some((_, list)) = tints.node(&path) else { return };
    for t in list.iter().filter(|t| t.key == list_key) {
        let removed: Vec<&str> = t.value.get(flexinput_core::layers::CARDS_REMOVED).and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let own = t.own.as_ref().and_then(|v| v.as_array());
        for id in removed {
            let Some(card) = own.and_then(|a| a.iter().find(|c| flexinput_core::layers::card_id(c) == Some(id))) else { continue };
            let pins = |k: &str| card.get(k).and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str())
                    .map(crate::canvas::viewer::remapper_pin_display)
                    .collect::<Vec<_>>().join(" + "))
                .unwrap_or_default();
            let text = format!("{} → {}", pins("in"), pins("out"));
            ui.add(egui::Label::new(egui::RichText::new(text).strikethrough().color(t.color.gamma_multiply(0.7))))
                .on_hover_text(format!("{} takes this card away while it is on.", t.layer));
        }
    }
}

/// A layer as the modules' own menus offer it (a JSM tab switched to by a layer): where it
/// lives, its label and colour, and its settings.
#[derive(Clone, Debug)]
pub struct LayerInfo {
    pub id: String,
    /// The path to its own canvas: it reaches only nodes under it.
    pub prefix: Vec<usize>,
    pub label: String,
    pub color: egui::Color32,
    pub settings: Vec<LayerSetting>,
}

impl LayerInfo {
    /// The value this layer sets param `key` of the node at `path` (from the top) to.
    pub fn sets(&self, path: &[usize], key: &str) -> Option<&Value> {
        let rel = path.strip_prefix(self.prefix.as_slice())?;
        self.settings.iter().find(|s| s.path == rel && s.key == key).map(|s| &s.value)
    }
}

/// Every layer of the patch and its sub-patches, for [`publish_layer_list`].
pub fn layer_infos(patch: &[Layer], top: &Snarl<NodeData>) -> Vec<LayerInfo> {
    all_layers(patch, top).into_iter()
        .map(|(scope, place, index, l)| LayerInfo {
            id: l.id.clone(),
            prefix: scope.prefix().to_vec(),
            label: match scope {
                LayerScope::Patch => layer_label(l),
                LayerScope::SubPatch(_) => format!("{place} › {}", layer_label(l)),
            },
            color: layer_color(l, index),
            settings: l.settings.clone(),
        })
        .collect()
}

fn layer_list_id() -> egui::Id {
    egui::Id::new("fxi_layer_list")
}

/// Publish this frame's layers, for menus of modules that offer them.
pub fn publish_layer_list(ctx: &egui::Context, list: Vec<LayerInfo>) {
    ctx.data_mut(|d| d.insert_temp(layer_list_id(), std::sync::Arc::new(list)));
}

/// This frame's layers ([`publish_layer_list`]).
pub fn layer_list(ctx: &egui::Context) -> std::sync::Arc<Vec<LayerInfo>> {
    ctx.data(|d| d.get_temp::<std::sync::Arc<Vec<LayerInfo>>>(layer_list_id())).unwrap_or_default()
}

/// Set param `key` of the node at `path` (from the top) to `value` in layer `layer` - `Null`
/// takes it out of the layer. Done with the next frame's edits (`take_layer_edits`).
pub fn queue_layer_setting(ctx: &egui::Context, layer: &LayerInfo, path: &[usize], key: &str, value: Value) {
    let edit: LayerValueEdit = (layer.id.clone(), layer.prefix.clone(), path.to_vec(), key.to_string(), value);
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<LayerValueEdit>>(layer_edits_id()).push(edit));
}

/// The edits made to values layers showed since the last call (`Canvas::edit_layer_value`).
pub fn take_layer_edits(ctx: &egui::Context) -> Vec<LayerValueEdit> {
    ctx.data_mut(|d| d.remove_temp::<Vec<LayerValueEdit>>(layer_edits_id())).unwrap_or_default()
}

/// The curve a layer on screen gives the response curve `node` (a uid in the canvas drawn
/// now), to draw over its own (`points`, `biases`): its points, its bends and its colour.
/// The layer being edited has its curve in the canvas already.
pub fn curve_tint(ctx: &egui::Context, node: usize, points: &[[f32; 2]], biases: &[f32]) -> Option<(Vec<[f32; 2]>, Vec<f32>, egui::Color32)> {
    curve_tint_keys(ctx, node, "points", "biases", points, biases)
}

/// [`curve_tint`] for a curve kept under other params: `points_key`, and `biases_key`
/// ("" for a curve with no bends).
pub fn curve_tint_keys(
    ctx: &egui::Context,
    node: usize,
    points_key: &str,
    biases_key: &str,
    points: &[[f32; 2]],
    biases: &[f32],
) -> Option<(Vec<[f32; 2]>, Vec<f32>, egui::Color32)> {
    let tints = tints(ctx);
    let mut path = tint_prefix(ctx);
    path.push(node);
    let (_, list) = tints.node(&path)?;
    let find = |key: &str| (!key.is_empty()).then(|| list.iter().rev().find(|t| t.key == key && !t.armed)).flatten();
    let (pts_t, bias_t) = (find(points_key), find(biases_key));
    let color = pts_t.or(bias_t)?.color;
    let lp: Vec<[f32; 2]> = pts_t.and_then(|t| t.value.as_array())
        .map(|a| a.iter().filter_map(|p| {
            let p = p.as_array()?;
            Some([p.first()?.as_f64()? as f32, p.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_else(|| points.to_vec());
    let mut lb: Vec<f32> = bias_t.and_then(|t| t.value.as_array())
        .map(|a| a.iter().map(|b| b.as_f64().unwrap_or(0.0) as f32).collect())
        .unwrap_or_else(|| biases.to_vec());
    lb.resize(lp.len().saturating_sub(1), 0.0);
    (lp.len() >= 2).then_some((lp, lb, color))
}

/// Draw over a graph the curves layers on screen give it, each in its layer's colour: for
/// each curve of the graph (its points param, its bends param or "", its own points and
/// bends), the layer's, mapped to the screen by `line` as the graph maps its own.
pub fn paint_curve_overlays(
    ctx: &egui::Context,
    painter: &egui::Painter,
    node: usize,
    curves: &[(&str, &str, &[[f32; 2]], &[f32])],
    line: impl Fn(&[[f32; 2]], &[f32]) -> Vec<egui::Pos2>,
) {
    for &(points_key, biases_key, points, biases) in curves {
        if let Some((lp, lb, color)) = curve_tint_keys(ctx, node, points_key, biases_key, points, biases) {
            painter.add(egui::Shape::line(line(&lp, &lb), egui::Stroke::new(2.0_f32, color)));
        }
    }
}

/// Whether a pin is a whole module (or a graph) rather than one control: what its module's
/// gamepad table doesn't break down.
pub fn whole_pin(module_id: &str, element_id: &str) -> bool {
    element_keys(module_id, element_id).is_empty()
}

/// A node a layer on screen changes: a frame round it in the layer's colour, and a
/// badge on its top edge listing what the layers set. `path` is its path from the
/// top canvas; `rect` its frame.
pub fn mark_node(ui: &egui::Ui, path: &[usize], rect: egui::Rect) {
    let tints = tints(ui.ctx());
    let Some((_, list)) = tints.node(path) else { return };
    let Some(top) = list.last() else { return };
    let painter = ui.painter().clone().with_clip_rect(ui.clip_rect());
    painter.rect_stroke(rect, 6.0, egui::Stroke::new(1.5_f32, top.color), egui::StrokeKind::Outside);
    let mut layers: Vec<(&str, egui::Color32)> = Vec::new();
    for t in list {
        if !layers.iter().any(|(n, _)| *n == t.layer) {
            layers.push((t.layer.as_str(), t.color));
        }
    }
    let font = egui::FontId::proportional(crate::settings::ui_size(10.0));
    let mut x = rect.left() + 10.0;
    let y = rect.top();
    let mut badge = egui::Rect::NOTHING;
    for (name, color) in &layers {
        let text = crate::widgets::text_on(*color);
        let galley = painter.layout_no_wrap(name.to_string(), font.clone(), text);
        let r = egui::Rect::from_min_size(egui::pos2(x, y - galley.size().y / 2.0 - 2.0), galley.size() + egui::vec2(10.0, 4.0));
        painter.rect_filled(r, r.height() / 2.0, *color);
        painter.galley(r.min + egui::vec2(5.0, 2.0), galley, text);
        badge = badge.union(r);
        x = r.right() + 4.0;
    }
    let lines: Vec<String> = list.iter().map(|t| format!("{}: {} = {}", t.layer, t.key, value_text(&t.value))).collect();
    let resp = ui.interact(badge, ui.id().with(("layer_badge", path.to_vec())), egui::Sense::hover());
    resp.on_hover_text(format!("Layers change this module:\n{}", lines.join("\n")));
}

// ── The chips ────────────────────────────────────────────────────────────────

/// Is layer `id` being edited (armed)?
pub type Armed<'a> = Option<&'a str>;

/// One chip per layer and a + after them: click a chip to turn its layer on or
/// off, right-click it to edit its settings, name, colour or icon. `active` is what
/// the engine has on (`flexinput_engine::active_layers`); `details` lists what each
/// layer changes ([`describe`]), shown on hover. Hovering a chip previews what its
/// layer changes on the canvas.
pub fn chips(
    ui: &mut egui::Ui,
    salt: impl Hash + Copy,
    layers: &[Layer],
    details: &[Vec<String>],
    active: &[String],
    armed: Armed<'_>,
) -> Option<LayerAct> {
    let mut act = None;
    let font = egui::FontId::proportional(crate::settings::ui_size(12.0));
    for (i, layer) in layers.iter().enumerate() {
        let color = layer_color(layer, i);
        let on = active.iter().any(|a| a == &layer.id);
        let editing = armed == Some(layer.id.as_str());
        let label = layer_label(layer);
        let text = if editing { format!("● {label}") } else { label.clone() };
        let galley = ui.painter().layout_no_wrap(text, font.clone(), egui::Color32::PLACEHOLDER);
        let pad = egui::vec2(8.0, 3.0);
        let icon_w = if crate::macro_icons::is_icon_key(&layer.icon) { font.size + 4.0 } else { 0.0 };
        let size = galley.size() + pad * 2.0 + egui::vec2(icon_w, 0.0);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let radius = egui::CornerRadius::same((size.y / 2.0) as u8);
        if on {
            ui.painter().rect_filled(rect, radius, color);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, radius, color.gamma_multiply(0.22));
        }
        let width = if editing { 2.0_f32 } else { 1.0_f32 };
        ui.painter().rect_stroke(rect, radius, egui::Stroke::new(width, color), egui::StrokeKind::Inside);
        let text_color = if on { crate::widgets::text_on(color) } else { ui.visuals().text_color() };
        if icon_w > 0.0 {
            let r = egui::Rect::from_min_size(rect.min + pad, egui::Vec2::splat(font.size));
            crate::macro_icons::paint_icon(ui.ctx(), ui.painter(), &layer.icon, r.translate(egui::vec2(0.0, (galley.size().y - font.size) * 0.5)), text_color);
        }
        ui.painter().galley(rect.min + pad + egui::vec2(icon_w, 0.0), galley, text_color);
        if resp.hovered() {
            let pass = ui.ctx().cumulative_pass_nr();
            ui.ctx().data_mut(|d| d.insert_temp(preview_id(), (pass, layer.id.clone())));
        }
        let lines = details.get(i).cloned().unwrap_or_default();
        let resp = resp.on_hover_ui(|ui| {
            if editing {
                ui.label(format!("Editing {label}: settings you change now are this layer's. \
                                  Right-click › Done editing to finish."));
            } else {
                ui.label(format!("Layer {label}. Click to turn it {}; right-click to edit it. \
                                  Cards turn it on from the Choose picker's Layers tab.",
                                 if on { "off" } else { "on" }));
            }
            ui.separator();
            if lines.is_empty() {
                ui.weak("It changes nothing yet.");
            } else {
                for l in lines.iter().take(24) {
                    ui.label(egui::RichText::new(l).color(color));
                }
                if lines.len() > 24 {
                    ui.weak(format!("… and {} more", lines.len() - 24));
                }
            }
        });
        if resp.clicked() {
            act = Some(LayerAct::Switch { id: layer.id.clone(), on: !on });
        }
        // Closed by its buttons or a click outside: a click in the name or the icon search
        // is the menu's.
        egui::Popup::context_menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            if editing {
                if ui.button("Done editing").clicked() {
                    act = Some(LayerAct::Disarm);
                    ui.close();
                }
            } else if ui.button("Edit settings")
                .on_hover_text("Settings you change from now on go into this layer, not the patch, until Done editing.")
                .clicked()
            {
                act = Some(LayerAct::Arm(layer.id.clone()));
                ui.close();
            }
            let mut buf = layer.name.clone();
            ui.horizontal(|ui| {
                ui.label("Name");
                if ui.text_edit_singleline(&mut buf).changed() {
                    act = Some(LayerAct::Rename { id: layer.id.clone(), name: buf.clone() });
                }
            });
            ui.horizontal(|ui| {
                ui.label("Colour");
                for c in PALETTE {
                    let swatch = egui::Color32::from_rgb(c[0], c[1], c[2]);
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
                    ui.painter().circle_filled(r.center(), 7.0, swatch);
                    if swatch == color {
                        ui.painter().circle_stroke(r.center(), 7.5, egui::Stroke::new(2.0_f32, ui.visuals().text_color()));
                    }
                    if resp.clicked() {
                        act = Some(LayerAct::Recolor { id: layer.id.clone(), color: c });
                    }
                }
                let mut picked = [color.r(), color.g(), color.b()];
                if ui.color_edit_button_srgb(&mut picked).changed() {
                    act = Some(LayerAct::Recolor { id: layer.id.clone(), color: picked });
                }
            });
            egui::containers::menu::SubMenuButton::new("Icon")
                .config(egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, |ui| {
                if let Some(icon) = crate::easy::subpatches::icon_grid(ui, &layer.icon) {
                    act = Some(LayerAct::SetIcon { id: layer.id.clone(), icon });
                    ui.close();
                }
            });
            ui.separator();
            let settings = layer.settings.len();
            if ui.add_enabled(settings > 0, egui::Button::new(format!("Clear its {settings} settings"))).clicked() {
                act = Some(LayerAct::Clear(layer.id.clone()));
                ui.close();
            }
            if ui.button("Delete layer").clicked() {
                act = Some(LayerAct::Delete(layer.id.clone()));
                ui.close();
            }
        });
        let _ = salt;
    }
    let label = if layers.is_empty() { "+ Layer" } else { "+" };
    let color = ui.visuals().text_color();
    if ui.add(egui::Button::new(egui::RichText::new(label).font(font).color(color)).frame(false))
        .on_hover_text("Add a layer: a set of settings a card (or this chip) turns on and off while the patch runs.")
        .clicked()
    {
        act = Some(LayerAct::Add);
    }
    act
}

#[cfg(test)]
mod tests {
    use super::*;
    use flexinput_core::{PinDescriptor, SignalType};

    fn node(module_id: &str, params: &[(&str, Value)]) -> NodeData {
        NodeData {
            module_id: module_id.into(),
            display_name: module_id.into(),
            category: String::new(),
            inputs: vec![PinDescriptor::new("in", SignalType::Float)],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
            params: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    fn sub(inner: Snarl<NodeData>, layers: Vec<Layer>) -> NodeData {
        let mut n = node("subpatch", &[]);
        n.subpatch = Some(Box::new(crate::canvas::node::UiSubPatch {
            snarl: Box::new(inner),
            layers,
            ..Default::default()
        }));
        n
    }

    fn layer(id: &str, settings: Vec<LayerSetting>) -> Layer {
        Layer { id: id.into(), name: id.into(), settings, ..Default::default() }
    }

    fn set(path: &[usize], key: &str, v: f64) -> LayerSetting {
        LayerSetting { path: path.to_vec(), key: key.into(), value: Value::from(v) }
    }

    /// Recording: what changed while armed becomes the layer's, and the patch gets
    /// its own back. The engine's own params and structural ones are never a
    /// layer's.
    #[test]
    fn what_changes_while_armed_is_the_layers() {
        let p = egui::Pos2::ZERO;
        let mut top: Snarl<NodeData> = Snarl::new();
        let gain = top.insert_node(p, node("math.multiply", &[("gain", Value::from(1.0))]));
        let mut base = HashMap::new();
        collect_params(&top, &[], &mut base);
        {
            let n = top.get_node_mut(gain).unwrap();
            n.params.insert("gain".into(), Value::from(2.0));
            n.params.insert("speed".into(), Value::from(3.0));
            n.params.insert("_automap_device_id".into(), Value::from("gilrs:x"));
            n.params.insert("device_id".into(), Value::from("gilrs:y"));
        }
        let got = changed_settings(&top, &[], &base);
        assert_eq!(got, vec![set(&[gain.0], "gain", 2.0), set(&[gain.0], "speed", 3.0)]);
        restore_baseline(&mut top, &base);
        let n = top.get_node(gain).unwrap();
        assert_eq!(n.params["gain"], Value::from(1.0));
        assert!(!n.params.contains_key("speed"), "a setting the patch didn't have goes again");
        assert_eq!(n.params["device_id"], Value::from("gilrs:y"), "structural params aren't the layer's to put back");
        assert!(put_settings(&mut top, &[], &got));
        assert_eq!(top.get_node(gain).unwrap().params["gain"], Value::from(2.0));
    }

    /// The engine's table: a sub-patch's layer reaches only inside it, by the path
    /// from the top; the layer being edited is left out.
    #[test]
    fn the_engine_gets_every_layer_with_paths_from_the_top() {
        let p = egui::Pos2::ZERO;
        let mut inner: Snarl<NodeData> = Snarl::new();
        let g = inner.insert_node(p, node("math.multiply", &[]));
        let mut top: Snarl<NodeData> = Snarl::new();
        let sp = top.insert_node(p, sub(inner, vec![layer("bb", vec![set(&[g.0], "gain", 2.0)])]));
        let patch = vec![layer("aa", vec![set(&[sp.0, g.0], "gain", 3.0)])];
        let table = engine_table(&patch, &top, None);
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].overrides[0].path, vec![sp.0, g.0]);
        assert_eq!(table[1].id, "bb");
        assert_eq!(table[1].overrides[0].path, vec![sp.0, g.0], "prefixed with the sub-patch");
        assert_eq!(engine_table(&patch, &top, Some("bb")).len(), 1, "the armed layer is the patch's graph now");
        let names: Vec<String> = display_entries(&patch, &top).into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"aa — Toggle".to_string()), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("› bb — Hold")), "{names:?}");
    }

    /// The generic icons (letters, numbers) are in the set, first, and draw.
    #[test]
    fn the_generic_icons_are_there() {
        for key in ["generic_letter_a", "generic_letter_b", "generic_number_1", "generic_number_2", "generic_number_3", "generic_number_4"] {
            let svg = crate::macro_icons::macro_icon_svg(key).unwrap_or_else(|| panic!("{key} in the set"));
            let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).unwrap_or_else(|e| panic!("{key}: {e}"));
            assert!(tree.root().has_children(), "{key} draws something");
            assert!(crate::macro_icons::is_icon_key(key));
        }
        assert!(crate::macro_icons::ICON_CATEGORIES.contains(&"Generic"));
        assert!(!crate::macro_icons::is_icon_key("🎯"), "an emoji picked before stays text");
    }

    /// A layer's JSM tab: what it sets the node's active tab to, from the top.
    #[test]
    fn a_layer_knows_the_tab_it_switches_to() {
        let info = LayerInfo {
            id: "aa".into(),
            prefix: vec![4],
            label: "Aim".into(),
            color: egui::Color32::WHITE,
            settings: vec![set(&[7], "jsm_active_tab", 2.0)],
        };
        assert_eq!(info.sets(&[4, 7], "jsm_active_tab").and_then(|v| v.as_f64()), Some(2.0));
        assert_eq!(info.sets(&[7], "jsm_active_tab"), None, "outside its sub-patch");
    }

    /// A control's params come from the module's gamepad table.
    #[test]
    fn a_controls_params_come_from_its_gamepad_table() {
        assert_eq!(element_keys("module.delay", "ms"), vec!["delay_ms"]);
        assert!(element_keys("module.delay", "no_such_control").is_empty());
    }

    #[test]
    fn text_reads_on_any_colour() {
        use crate::widgets::text_on;
        assert_eq!(text_on(egui::Color32::from_rgb(0, 92, 128)), egui::Color32::WHITE, "the default accent");
        assert_ne!(text_on(egui::Color32::from_rgb(240, 230, 140)), egui::Color32::WHITE, "a light yellow");
    }

    /// Two copies of one preset: the second's layers get ids of their own, and its
    /// cards follow them.
    #[test]
    fn a_second_copy_of_a_preset_gets_layers_of_its_own() {
        let p = egui::Pos2::ZERO;
        let card = |id: &str| {
            let mut inner: Snarl<NodeData> = Snarl::new();
            inner.insert_node(p, node("module.remapper", &[(
                "mappings",
                serde_json::json!([{ "in": ["btn_south"], "out": [format!("layer:{id}_toggle")] }]),
            )]));
            inner
        };
        let mut top: Snarl<NodeData> = Snarl::new();
        let a = top.insert_node(p, sub(card("cc"), vec![layer("cc", vec![])]));
        let b = top.insert_node(p, sub(card("cc"), vec![layer("cc", vec![])]));
        assert!(give_layers_own_ids(&[], &mut top));
        let id_of = |n: NodeId| top.get_node(n).unwrap().subpatch.as_ref().unwrap().layers[0].id.clone();
        let out_of = |n: NodeId| {
            let sp = top.get_node(n).unwrap().subpatch.as_ref().unwrap();
            let (_, card) = sp.snarl.nodes_ids_data().next().map(|(i, n)| (i, n.value.clone())).unwrap();
            card.params["mappings"][0]["out"][0].as_str().unwrap().to_string()
        };
        let (ia, ib) = (id_of(a), id_of(b));
        assert_ne!(ia, ib);
        assert_eq!(out_of(a), format!("layer:{ia}_toggle"));
        assert_eq!(out_of(b), format!("layer:{ib}_toggle"), "the card follows its layer");
        assert!(!give_layers_own_ids(&[], &mut top), "nothing clashes now");
    }
}
