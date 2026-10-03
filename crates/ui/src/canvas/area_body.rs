//! Area Mapper module body: the options row, the area field (cells, borders,
//! gradients, live point), the selected border's gradient settings, the
//! selected cell's icon + name, and its mapping cards. Each of those except the
//! cell row is also a pinnable element (`options`, `field`, `border`, `cards`).
//!
//! The geometry is `flexinput_core::area::AreaLayout`, stored in
//! `area_layout`. Cards are the Touch Zones / Virtual Menu cards
//! (`zone_maps`, keyed by cell id in `z`, rendered by
//! `render_touch_zone_cards` in its Area Mapper variant), and the per-cell
//! icon + name reuse the menu's `zone_meta`.
//!
//! Editing: drag a border to move it, double-click to centre it; click a cell
//! to select it, a border to open its gradient settings; right-click to add a
//! border through the point or remove the one under it. With symmetry on
//! (`area_sym`, the default) every edit acts on the border's whole mirror set.
//! Mouse and gamepad edits go through the one [`apply_area_edits`].
//!
//! Some of what the pad edits lives inside the layout rather than in a param,
//! and the gamepad's field editor only writes params. So the layout keeps a few
//! param MIRRORS, reconciled by [`area_sync`] each time the module draws:
//! `area_shape` (a request the layout follows), the selected border's gradient
//! (`area_g_*`, `area_sq`, `area_sq_rc`) and the selected cell's touch gate,
//! rounding and pressure (`area_cell_touch`, `area_cell_round`,
//! `area_cell_press`). A
//! mirror that differs from what sync last wrote is an edit and goes into the
//! layout; otherwise sync rewrites it from the layout, so mouse edits show up.
//!
//! LAYERS: every layer's keys (`AREA_LAYER_KEYS`: layout, cell meta, touch
//! gates, colour, analog mode) are stored in `area_layers`; the layer being
//! edited also sits under the plain keys, which every editor here works on.
//! `area_layer` asks for a layer; [`area_sync`] stores the loaded one back and
//! swaps the asked-for one in. Cards carry their layer as `f` — the card
//! list's "field" — so the shared card list filters by it unchanged.
//!
//! UI-only params: `sel_zone` (selected cell id, shared with the card list),
//! `_area_sel_border` (the border whose settings are open, by position so it
//! survives index shifts), `field_w` (field size), the sync snapshots
//! `_area_g_sync` / `_area_cell_sync`.

use std::collections::HashMap;

use egui_snarl::{NodeId, Snarl};
use flexinput_core::area::{AreaLayout, BorderRef, DigitalMode, Gradient, Phase, Region, Shape};
use flexinput_core::Signal;
use flexinput_engine::eval::{
    area_layout_of, area_touch_pin, AREA_ANALOG_MS_DEFAULT, AREA_INPUTS, AREA_INPUT_PARAM,
    AREA_LAYERS_PARAM, AREA_LAYER_ANALOG_PARAM, AREA_LAYER_COLOR_PARAM, AREA_LAYER_KEYS,
    AREA_LAYER_LOADED_PARAM, AREA_LAYER_MS_PARAM, AREA_LAYER_STRIDE, AREA_LAYOUT_PARAM,
    AREA_ANALOG_HOLD_DEFAULT, AREA_LAYER_HOLD_PARAM, AREA_LAYER_MIX_PARAM, AREA_LAYER_PULSE_PARAM,
    AREA_ANALOG_FULL_DEFAULT, AREA_LAYER_FULL_PARAM, AREA_LAYER_MID_PARAM, AREA_LAYER_RAMP_PARAM,
    AREA_PASS_SOURCE_PARAM, AREA_PRESS_HOLD_PARAM, AREA_PRESS_LOCK_PARAM, AREA_PRESS_MODES,
    AREA_PRESS_MODE_PARAM, AREA_PRESS_MS_PARAM, AREA_PRESS_TURBO_PARAM, AREA_TOUCH_CELLS_PARAM,
};
use serde_json::{json, Value};

use super::menu_body::{
    icon_picker_button, menu_zone_meta, paint_grid_zone_cell, paint_menu_cursor, paint_radial_zone,
    plate_fill, write_zone_meta_entry, RingGeom, ZoneColors, ZoneMeta,
};
use super::viewer::{
    apply_widget_scale, identity_curve, mapping_curve_editor, publish_nav_field_rects,
    register_exposable_element, remapper_upstream_device_ids, render_touch_zone_cards,
    tz_apply_pick, tz_pick_kind, AutomapGlowParent,
};
use super::NodeData;

type LiveSignals = HashMap<(String, String), Signal>;

/// Symmetric editing param.
pub(crate) const AREA_SYM_PARAM: &str = "area_sym";
/// The shape, as a request the layout follows (see the module docs).
pub(crate) const AREA_SHAPE_PARAM: &str = "area_shape";
/// Mirrors of the selected border's gradient.
pub(crate) const AREA_G_ON: &str = "area_g_on";
pub(crate) const AREA_G_W: &str = "area_g_w";
pub(crate) const AREA_G_CURVE: &str = "area_g_curve";
pub(crate) const AREA_G_KEYS: &str = "area_g_keys";
pub(crate) const AREA_G_MS: &str = "area_g_ms";
pub(crate) const AREA_G_THR: &str = "area_g_thr";
pub(crate) const AREA_G_PHASE: &str = "area_g_phase";
/// Mirror of the selected border's squareness (a circle's ring border).
pub(crate) const AREA_G_SQ: &str = "area_sq";
/// Mirror of whether the selected ring border squares by rounding a square's
/// corners (rather than blending).
pub(crate) const AREA_G_RC: &str = "area_sq_rc";
/// Mirror of the selected rectangle cell's corner rounding.
pub(crate) const AREA_CELL_ROUND: &str = "area_cell_round";
/// Mirror of the selected rectangle cell's pressure.
pub(crate) const AREA_CELL_PRESSURE: &str = "area_cell_press";
/// Mirror of the selected cell's touch gate.
pub(crate) const AREA_CELL_TOUCH: &str = "area_cell_touch";
const G_SYNC: &str = "_area_g_sync";
const CELL_SYNC: &str = "_area_cell_sync";
pub(crate) const SEL_BORDER: &str = "_area_sel_border";

/// Crossfade curve presets, as the gamepad cycles them. "custom" is whatever
/// the mouse drew; choosing it changes nothing.
pub(crate) const CURVE_PRESETS: &[&str] = &["linear", "ease_in", "ease_out", "s_curve", "custom"];
pub(crate) const KEY_MODES: &[&str] = &["pwm", "taps", "threshold"];
pub(crate) const PHASES: &[&str] = &["alternate", "independent"];

/// Default field size (px, square).
const FIELD_DEFAULT: f32 = 240.0;
const FIELD_MIN: f32 = 140.0;
const FIELD_MAX: f32 = 640.0;
/// How close (px) the pointer must be to grab a border.
const GRAB_PX: f32 = 6.0;

// ── Param access ──────────────────────────────────────────────────────────────

pub(crate) fn area_layout(node: &NodeData) -> AreaLayout {
    area_layout_of(&node.params)
}

fn set_layout(node: &mut NodeData, layout: &AreaLayout) {
    set_if_diff(node, AREA_LAYOUT_PARAM, layout.to_value());
}

/// Write a param only when it changes, so a draw that reconciles nothing
/// leaves the node untouched.
fn set_if_diff(node: &mut NodeData, key: &str, v: Value) {
    if node.params.get(key) != Some(&v) {
        node.params.insert(key.to_string(), v);
    }
}

pub(crate) fn area_symmetric(node: &NodeData) -> bool {
    node.params.get(AREA_SYM_PARAM).and_then(|v| v.as_bool()).unwrap_or(true)
}

pub(crate) fn area_input(node: &NodeData) -> String {
    node.params.get(AREA_INPUT_PARAM).and_then(|v| v.as_str()).unwrap_or("left_stick").to_string()
}

pub(crate) fn selected_cell(node: &NodeData, layout: &AreaLayout) -> u32 {
    let ids = layout.cell_ids();
    let sel = node.params.get("sel_zone").and_then(|v| v.as_u64()).map(|v| v as u32);
    sel.filter(|s| ids.contains(s)).unwrap_or_else(|| ids.first().copied().unwrap_or(0))
}

fn touch_cells(node: &NodeData) -> Vec<u32> {
    node.params.get(AREA_TOUCH_CELLS_PARAM).and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_u64().map(|i| i as u32)).collect())
        .unwrap_or_default()
}

/// The live point (centred, +Y up) and each cell's weight, from the engine's
/// mirror (see `eval_area_mapper_node`).
pub(crate) fn area_live(node: &NodeData) -> (Option<egui::Vec2>, HashMap<u32, f32>) {
    let out = &node.extra.last_out;
    let point = match out.get(1) {
        Some(Some(Signal::Vec2(v))) => Some(egui::vec2(v.x, v.y)),
        _ => None,
    };
    let layer = loaded_layer(node) as u32;
    let weights = out.iter().skip(2).filter_map(|s| match s {
        Some(Signal::Vec2(v)) if v.x as u32 / AREA_LAYER_STRIDE == layer => {
            Some((v.x as u32 % AREA_LAYER_STRIDE, v.y))
        }
        _ => None,
    }).collect();
    (point, weights)
}

/// A border named by POSITION rather than index, so a selection survives
/// edits that renumber borders.
pub(crate) fn border_key(layout: &AreaLayout, r: BorderRef) -> Option<Value> {
    let pos = layout.border(r)?.pos;
    Some(match r {
        BorderRef::Edge(_) => json!({ "k": "edge", "p": pos }),
        BorderRef::Cut { band, .. } => json!({ "k": "cut", "b": band, "p": pos }),
    })
}

pub(crate) fn border_from_key(layout: &AreaLayout, v: &Value) -> Option<BorderRef> {
    let p = v.get("p")?.as_f64()? as f32;
    match v.get("k")?.as_str()? {
        "edge" => layout.edges.iter().position(|e| (e.pos - p).abs() < 1e-3).map(BorderRef::Edge),
        _ => {
            let band = v.get("b")?.as_u64()? as usize;
            layout.find_cut(band, p).map(|cut| BorderRef::Cut { band, cut })
        }
    }
}

/// The selected border, if it still exists.
pub(crate) fn selected_border(node: &NodeData, layout: &AreaLayout) -> Option<BorderRef> {
    node.params.get(SEL_BORDER).and_then(|v| border_from_key(layout, v))
}

/// What a border is called, for its settings panel and menus.
fn border_noun(shape: Shape, r: BorderRef) -> &'static str {
    match (shape, r) {
        (Shape::Circle, BorderRef::Edge(_)) => "Ring border",
        (Shape::Circle, BorderRef::Cut { .. }) => "Sector border",
        (Shape::Rect, BorderRef::Edge(_)) => "Row border",
        (Shape::Rect, BorderRef::Cut { .. }) => "Column border",
    }
}

/// Is a border's width shown in degrees (a sector border) rather than as a
/// share of the area's half-width?
pub(crate) fn border_width_in_degrees(shape: Shape, r: BorderRef) -> bool {
    shape == Shape::Circle && matches!(r, BorderRef::Cut { .. })
}

/// A new gradient's width: a third of the smaller neighbouring cell, so it
/// never swallows either side.
fn default_gradient_width(layout: &AreaLayout, r: BorderRef) -> f32 {
    let room = match r {
        BorderRef::Edge(e) => {
            let a = layout.band_span(e);
            let b = layout.band_span(e + 1);
            (a[1] - a[0]).min(b[1] - b[0])
        }
        BorderRef::Cut { band, cut } => {
            let n = layout.bands[band].cells.len();
            let (lo, hi) = match layout.shape {
                Shape::Circle => ((cut + n - 1) % n, cut),
                Shape::Rect => (cut, cut + 1),
            };
            let span = |i: usize| { let s = layout.cell_span(band, i); s[1] - s[0] };
            span(lo).min(span(hi))
        }
    };
    (room / 3.0).max(0.02)
}

/// Every border in a fixed order — the index the gamepad focuses borders by.
/// A circle band with one cut has no second cell, so that cut is inert and
/// left out.
pub(crate) fn all_borders(layout: &AreaLayout) -> Vec<BorderRef> {
    let mut out: Vec<BorderRef> = (0..layout.edges.len()).map(BorderRef::Edge).collect();
    for (b, band) in layout.bands.iter().enumerate() {
        if layout.shape == Shape::Circle && band.cuts.len() == 1 {
            continue;
        }
        for c in 0..band.cuts.len() {
            out.push(BorderRef::Cut { band: b, cut: c });
        }
    }
    out
}

fn curve_preset_points(name: &str) -> Option<Vec<[f32; 2]>> {
    Some(match name {
        "linear" => Vec::new(),
        "ease_in" => vec![[0.0, 0.0], [0.6, 0.25], [1.0, 1.0]],
        "ease_out" => vec![[0.0, 0.0], [0.4, 0.75], [1.0, 1.0]],
        "s_curve" => vec![[0.0, 0.0], [0.3, 0.1], [0.7, 0.9], [1.0, 1.0]],
        _ => return None,
    })
}

fn curve_preset_of(pts: &[[f32; 2]]) -> &'static str {
    let same = |a: &[[f32; 2]], b: &[[f32; 2]]| a.len() == b.len()
        && a.iter().zip(b).all(|(p, q)| (p[0] - q[0]).abs() < 1e-4 && (p[1] - q[1]).abs() < 1e-4);
    if pts.is_empty() || same(pts, &identity_curve()) {
        return "linear";
    }
    CURVE_PRESETS.iter().copied()
        .find(|n| curve_preset_points(n).is_some_and(|p| same(pts, &p)))
        .unwrap_or("custom")
}

fn curve_preset_label(name: &str) -> &'static str {
    match name {
        "linear" => "Linear",
        "ease_in" => "Ease in",
        "ease_out" => "Ease out",
        "s_curve" => "S-curve",
        _ => "Custom",
    }
}

// ── Layers ────────────────────────────────────────────────────────────────────

/// The layer being asked for (the tabs, the gamepad).
pub(crate) const AREA_LAYER_PARAM: &str = "area_layer";
/// A loaded index meaning "the plain keys hold nothing to store back".
const LAYER_STALE: u64 = u64::MAX;
/// Layer colours, in the order new layers take them.
const LAYER_PALETTE: [[u8; 4]; 6] = [
    [96, 120, 156, 217], [230, 150, 60, 230], [90, 190, 110, 230],
    [170, 110, 220, 230], [60, 180, 190, 230], [220, 90, 90, 230],
];

/// A card's layer.
fn card_layer(c: &Value) -> usize {
    c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize
}

/// The layer being edited (whose data sits under the plain keys).
pub(crate) fn loaded_layer(node: &NodeData) -> usize {
    match node.params.get(AREA_LAYER_LOADED_PARAM).and_then(|v| v.as_u64()) {
        Some(LAYER_STALE) | None => 0,
        Some(k) => k as usize,
    }
}

/// How many layers there are.
pub(crate) fn layer_count(node: &NodeData) -> usize {
    node.params.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(1).max(1)
}

/// A layer's colour.
fn layer_color(get: impl Fn(&str) -> Option<Value>, k: usize) -> [u8; 4] {
    get(AREA_LAYER_COLOR_PARAM).and_then(|v| {
        let a = v.as_array()?;
        let c = |i: usize| a.get(i).and_then(|x| x.as_u64()).map(|x| x.min(255) as u8);
        Some([c(0)?, c(1)?, c(2)?, c(3).unwrap_or(255)])
    }).unwrap_or(LAYER_PALETTE[k % LAYER_PALETTE.len()])
}

/// The loaded layer's keys as a layer object.
fn layer_snapshot(node: &NodeData) -> Value {
    let mut obj = serde_json::Map::new();
    for k in AREA_LAYER_KEYS {
        if let Some(v) = node.params.get(*k) {
            obj.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(obj)
}

/// Store the loaded layer's keys back into the stack.
fn store_loaded_layer(node: &mut NodeData) {
    let loaded = node.params.get(AREA_LAYER_LOADED_PARAM).and_then(|v| v.as_u64()).unwrap_or(0);
    if loaded == LAYER_STALE {
        return;
    }
    let snap = layer_snapshot(node);
    if let Some(layers) = node.params.get_mut(AREA_LAYERS_PARAM).and_then(|v| v.as_array_mut()) {
        if let Some(slot) = layers.get_mut(loaded as usize) {
            if *slot != snap {
                *slot = snap;
            }
        }
    }
}

/// Keep the layer stack in order and swap the asked-for layer in.
fn sync_layers(node: &mut NodeData) {
    if node.params.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array()).is_none_or(|a| a.is_empty()) {
        let snap = layer_snapshot(node);
        node.params.insert(AREA_LAYERS_PARAM.into(), json!([snap]));
        node.params.insert(AREA_LAYER_LOADED_PARAM.into(), json!(0));
    }
    store_loaded_layer(node);
    let len = layer_count(node);
    let want = node.params.get(AREA_LAYER_PARAM).and_then(|v| v.as_f64())
        .map(|v| v.round().max(0.0) as usize).unwrap_or(0).min(len - 1);
    let loaded = node.params.get(AREA_LAYER_LOADED_PARAM).and_then(|v| v.as_u64()).unwrap_or(LAYER_STALE);
    if loaded != want as u64 {
        let layer = node.params.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array())
            .and_then(|a| a.get(want)).cloned().unwrap_or(json!({}));
        for k in AREA_LAYER_KEYS {
            match layer.get(*k) {
                Some(v) => { node.params.insert(k.to_string(), v.clone()); }
                None => { node.params.remove(*k); }
            }
        }
        node.params.insert(AREA_LAYER_LOADED_PARAM.into(), json!(want));
        for k in [SEL_BORDER, G_SYNC, CELL_SYNC, "sel_zone", AREA_SHAPE_PARAM] {
            node.params.remove(k);
        }
    }
    set_if_diff(node, AREA_LAYER_PARAM, json!(want));
    if !node.params.contains_key(AREA_LAYER_COLOR_PARAM) {
        let c = LAYER_PALETTE[want % LAYER_PALETTE.len()];
        node.params.insert(AREA_LAYER_COLOR_PARAM.into(), json!(c));
    }
}

/// Add a layer — an inner disc and an outer ring, for a modifier held by how
/// far the stick is pushed — and switch to it.
pub(crate) fn add_layer(node: &mut NodeData) {
    area_sync(node);
    let k = layer_count(node);
    let layer = json!({
        AREA_LAYOUT_PARAM: AreaLayout::circle_rings(0.7).to_value(),
        AREA_LAYER_COLOR_PARAM: LAYER_PALETTE[k % LAYER_PALETTE.len()],
    });
    if let Some(layers) = node.params.get_mut(AREA_LAYERS_PARAM).and_then(|v| v.as_array_mut()) {
        layers.push(layer);
    }
    node.params.insert(AREA_LAYER_PARAM.into(), json!(k));
    area_sync(node);
}

/// Remove layer `k` and its cards (never the last layer).
pub(crate) fn remove_layer(node: &mut NodeData, k: usize) {
    area_sync(node);
    let len = layer_count(node);
    if len <= 1 || k >= len {
        return;
    }
    if let Some(layers) = node.params.get_mut(AREA_LAYERS_PARAM).and_then(|v| v.as_array_mut()) {
        layers.remove(k);
    }
    if let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
        cards.retain(|c| card_layer(c) != k);
        for c in cards.iter_mut() {
            let f = card_layer(c);
            if f > k {
                c["f"] = json!(f - 1);
            }
        }
    }
    // The plain keys held the removed layer (or an index now shifted): load afresh.
    node.params.insert(AREA_LAYER_LOADED_PARAM.into(), json!(LAYER_STALE));
    node.params.insert(AREA_LAYER_PARAM.into(), json!(k.min(len - 2)));
    area_sync(node);
}

/// Replace the loaded layer's layout with a preset, keeping cards on the
/// cells it still has (presets number cells by direction alike).
pub(crate) fn apply_preset(node: &mut NodeData, layout: &AreaLayout) {
    area_sync(node);
    set_layout(node, layout);
    retain_cells(node, &layout.cell_ids());
    node.params.remove(SEL_BORDER);
    node.params.insert(AREA_SHAPE_PARAM.into(), json!(layout.shape.as_str()));
    area_sync(node);
}

/// The module's whole setup as a preset file's content.
pub(crate) fn module_preset(node: &mut NodeData) -> Value {
    area_sync(node);
    let mut obj = serde_json::Map::new();
    obj.insert("flexinput_area".into(), json!(1));
    for k in [AREA_LAYERS_PARAM, "zone_maps", AREA_INPUT_PARAM, AREA_SYM_PARAM, AREA_PASS_SOURCE_PARAM,
              AREA_PRESS_LOCK_PARAM, AREA_PRESS_MODE_PARAM, AREA_PRESS_MS_PARAM, AREA_PRESS_HOLD_PARAM,
              AREA_PRESS_TURBO_PARAM]
    {
        if let Some(v) = node.params.get(k) {
            obj.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(obj)
}

/// Load a preset file's setup into the module. Returns whether it took.
pub(crate) fn load_module_preset(node: &mut NodeData, preset: &Value) -> bool {
    let Some(layers) = preset.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array()).filter(|a| !a.is_empty()) else {
        return false;
    };
    node.params.insert(AREA_LAYERS_PARAM.into(), Value::Array(layers.clone()));
    for k in ["zone_maps", AREA_INPUT_PARAM, AREA_SYM_PARAM, AREA_PASS_SOURCE_PARAM,
              AREA_PRESS_LOCK_PARAM, AREA_PRESS_MODE_PARAM, AREA_PRESS_MS_PARAM, AREA_PRESS_HOLD_PARAM,
              AREA_PRESS_TURBO_PARAM]
    {
        match preset.get(k) {
            Some(v) => { node.params.insert(k.to_string(), v.clone()); }
            None => { node.params.remove(k); }
        }
    }
    node.params.insert(AREA_LAYER_LOADED_PARAM.into(), json!(LAYER_STALE));
    node.params.insert(AREA_LAYER_PARAM.into(), json!(0));
    area_sync(node);
    true
}

/// With one press mode for all, write it onto every card so each shows it
/// (the engine applies it either way).
fn sync_press(node: &mut NodeData) {
    if !node.params.get(AREA_PRESS_LOCK_PARAM).and_then(|v| v.as_bool()).unwrap_or(false) {
        return;
    }
    let mode = node.params.get(AREA_PRESS_MODE_PARAM).cloned().unwrap_or(json!("down"));
    let ms = node.params.get(AREA_PRESS_MS_PARAM).cloned().unwrap_or(json!(200.0));
    let hold = node.params.get(AREA_PRESS_HOLD_PARAM).cloned().unwrap_or(json!(false));
    let turbo = node.params.get(AREA_PRESS_TURBO_PARAM).cloned().unwrap_or(json!(false));
    let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) else { return };
    for c in cards.iter_mut() {
        let Some(o) = c.as_object_mut() else { continue };
        for (k, v) in [("mode", &mode), ("window_ms", &ms), ("sustain", &hold), ("turbo", &turbo)] {
            if o.get(k) != Some(v) {
                o.insert(k.to_string(), v.clone());
            }
        }
    }
}

// ── Sync ──────────────────────────────────────────────────────────────────────

/// The gradient mirror fields: (snapshot field, param).
const G_FIELDS: [(&str, &str); 9] = [
    ("sq", AREA_G_SQ),
    ("rc", AREA_G_RC),
    ("on", AREA_G_ON),
    ("w", AREA_G_W),
    ("curve", AREA_G_CURVE),
    ("keys", AREA_G_KEYS),
    ("ms", AREA_G_MS),
    ("thr", AREA_G_THR),
    ("phase", AREA_G_PHASE),
];

/// Two mirror values alike? Numbers within float noise.
fn same_value(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a.and_then(|v| v.as_f64()), b.and_then(|v| v.as_f64())) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-6,
        _ => a == b,
    }
}

/// A border's gradient as mirror values (defaults when it has none).
fn gradient_view(layout: &AreaLayout, r: BorderRef) -> Value {
    let square = layout.border(r).map(|b| b.square).unwrap_or(0.0);
    let corners = layout.border(r).is_some_and(|b| b.corners);
    let g = layout.border(r).and_then(|b| b.gradient.clone());
    let on = g.is_some();
    let g = g.unwrap_or_else(|| Gradient::with_width(default_gradient_width(layout, r)));
    json!({
        "sq": square,
        "rc": corners,
        "on": on,
        "w": g.width,
        "curve": curve_preset_of(&g.curve),
        "keys": g.digital.as_str(),
        "ms": g.period_ms,
        "thr": g.threshold,
        "phase": g.phase.as_str(),
    })
}

fn gradient_from_view(view: &Value, layout: &AreaLayout, r: BorderRef) -> Option<Gradient> {
    if !view.get("on").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
    let f = |k: &str, d: f32| view.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
    let s = |k: &str| view.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let existing = layout.border(r).and_then(|b| b.gradient.as_ref());
    let curve = curve_preset_points(s("curve"))
        .or_else(|| existing.map(|g| g.curve.clone()))
        .unwrap_or_default();
    Some(Gradient {
        width: f("w", default_gradient_width(layout, r)).max(0.01),
        curve,
        digital: DigitalMode::from_str(s("keys")),
        period_ms: f("ms", 100.0).clamp(1.0, 1000.0),
        threshold: f("thr", 0.5).clamp(0.0, 1.0),
        phase: Phase::from_str(s("phase")),
    })
}

/// Reconcile the node's params with its layout (see the module docs):
/// defaults on first draw, the shape request, a valid selected cell, and the
/// gradient / touch-gate mirrors. Every renderer of the module calls it first.
pub(crate) fn area_sync(node: &mut NodeData) {
    sync_layers(node);
    if !node.params.contains_key(AREA_LAYOUT_PARAM) {
        set_layout(node, &AreaLayout::default_for(Shape::Circle));
    }
    for (k, v) in [(AREA_SYM_PARAM, json!(true)), (AREA_INPUT_PARAM, json!("left_stick"))] {
        if !node.params.contains_key(k) {
            node.params.insert(k.into(), v);
        }
    }
    let mut layout = area_layout(node);
    let want = node.params.get(AREA_SHAPE_PARAM).and_then(|v| v.as_str()).map(Shape::from_str);
    match want {
        Some(s) if s != layout.shape => {
            switch_shape(node, s);
            layout = area_layout(node);
        }
        Some(_) => {}
        None => {
            node.params.insert(AREA_SHAPE_PARAM.into(), json!(layout.shape.as_str()));
        }
    }
    let sel = selected_cell(node, &layout);
    let layer = loaded_layer(node) as u64;
    set_if_diff(node, "sel_field", json!(layer));
    if node.params.get("sel_zone").and_then(|v| v.as_u64()) != Some(sel as u64) {
        node.params.insert("sel_zone".into(), json!(sel));
    }
    let border_changed = sync_border(node, &mut layout);
    if sync_cell(node, &mut layout) || border_changed {
        set_layout(node, &layout);
    }
    for (k, v) in [
        (AREA_LAYER_ANALOG_PARAM, json!(false)),
        (AREA_LAYER_MS_PARAM, json!(AREA_ANALOG_MS_DEFAULT)),
        (AREA_LAYER_MIX_PARAM, json!("direction")),
        (AREA_LAYER_PULSE_PARAM, json!("smooth")),
        (AREA_LAYER_HOLD_PARAM, json!(AREA_ANALOG_HOLD_DEFAULT)),
        (AREA_LAYER_RAMP_PARAM, json!(0.0)),
        (AREA_LAYER_MID_PARAM, json!(0.5)),
        (AREA_LAYER_FULL_PARAM, json!(AREA_ANALOG_FULL_DEFAULT)),
    ] {
        if !node.params.contains_key(k) {
            node.params.insert(k.into(), v);
        }
    }
    sync_press(node);
    store_loaded_layer(node);
}

/// Start the layout over in a new shape. Both defaults number the directions
/// alike, so cards on those cells keep their direction; cards on any other
/// cell go, or a later new cell would inherit them.
fn switch_shape(node: &mut NodeData, shape: Shape) {
    let layout = AreaLayout::default_for(shape);
    let ids = layout.cell_ids();
    set_layout(node, &layout);
    retain_cells(node, &ids);
    node.params.remove(SEL_BORDER);
    node.params.insert(AREA_SHAPE_PARAM.into(), json!(shape.as_str()));
}

/// Apply edited gradient mirrors to the selected border, then rewrite the
/// mirrors from the layout. Returns whether the layout changed.
fn sync_border(node: &mut NodeData, layout: &mut AreaLayout) -> bool {
    let Some((key, r)) = node.params.get(SEL_BORDER).cloned()
        .and_then(|k| border_from_key(layout, &k).map(|r| (k, r)))
    else {
        node.params.remove(G_SYNC);
        return false;
    };
    let mut view = gradient_view(layout, r);
    let snap = node.params.get(G_SYNC).filter(|s| s.get("border") == Some(&key)).cloned();
    let (mut grad_edit, mut square_edit, mut corners_edit) = (false, None, None);
    if let Some(snap) = snap {
        for (field, param) in G_FIELDS {
            let p = node.params.get(param);
            if !same_value(p, snap.get(field)) {
                if let Some(p) = p {
                    if field == "sq" {
                        square_edit = p.as_f64().map(|v| v as f32);
                    } else if field == "rc" {
                        corners_edit = p.as_bool();
                    } else {
                        view[field] = p.clone();
                        grad_edit = true;
                    }
                }
            }
        }
    }
    if grad_edit {
        let g = gradient_from_view(&view, layout, r);
        layout.set_gradient(r, g, area_symmetric(node));
    }
    if let (Some(rc), BorderRef::Edge(e)) = (corners_edit, r) {
        layout.set_edge_corners(e, rc);
    }
    if let (Some(sq), BorderRef::Edge(e)) = (square_edit, r) {
        layout.set_edge_square(e, sq);
    }
    let changed = grad_edit || square_edit.is_some() || corners_edit.is_some();
    if changed {
        view = gradient_view(layout, r);
    }
    for (field, param) in G_FIELDS {
        set_if_diff(node, param, view[field].clone());
    }
    let mut snap = view;
    snap["border"] = key;
    set_if_diff(node, G_SYNC, snap);
    changed
}

/// The same for the selected cell's touch gate and (rectangle) corner
/// rounding. Returns whether the layout changed.
fn sync_cell(node: &mut NodeData, layout: &mut AreaLayout) -> bool {
    let cell = selected_cell(node, layout);
    let mut on = touch_cells(node).contains(&cell);
    let mut changed = false;
    let snap = node.params.get(CELL_SYNC).filter(|s| s.get("cell") == Some(&json!(cell))).cloned();
    if let Some(snap) = snap {
        let p = node.params.get(AREA_CELL_TOUCH).and_then(|v| v.as_bool());
        if p.is_some() && p != snap.get("on").and_then(|v| v.as_bool()) {
            on = p.unwrap_or(false);
            let mut gates = touch_cells(node);
            gates.retain(|c| *c != cell);
            if on { gates.push(cell); }
            node.params.insert(AREA_TOUCH_CELLS_PARAM.into(), json!(gates));
        }
        let r = node.params.get(AREA_CELL_ROUND);
        if r.is_some() && !same_value(r, snap.get("round")) {
            let r = r.and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            layout.set_cell_round(cell, r, area_symmetric(node));
            changed = true;
        }
        let q = node.params.get(AREA_CELL_PRESSURE);
        if q.is_some() && !same_value(q, snap.get("press")) {
            let q = q.and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            layout.set_cell_pressure(cell, q, area_symmetric(node));
            changed = true;
        }
    }
    let round = layout.cell_round_of(cell);
    let press = layout.cell_pressure_of(cell);
    set_if_diff(node, AREA_CELL_TOUCH, json!(on));
    set_if_diff(node, AREA_CELL_ROUND, json!(round));
    set_if_diff(node, AREA_CELL_PRESSURE, json!(press));
    set_if_diff(node, CELL_SYNC, json!({ "cell": cell, "on": on, "round": round, "press": press }));
    changed
}

/// Drop cards, cell metadata and touch gates of cells no longer in `ids`.
fn retain_cells(n: &mut NodeData, ids: &[u32]) {
    let layer = loaded_layer(n);
    if let Some(cards) = n.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
        cards.retain(|c| card_layer(c) != layer
            || ids.contains(&(c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32)));
    }
    if let Some(meta) = n.params.get_mut("zone_meta").and_then(|v| v.as_object_mut()) {
        meta.retain(|k, _| k.parse::<u32>().is_ok_and(|id| ids.contains(&id)));
    }
    let gates: Vec<u32> = touch_cells(n).into_iter().filter(|c| ids.contains(c)).collect();
    n.params.insert(AREA_TOUCH_CELLS_PARAM.into(), json!(gates));
}

/// Move each removed cell's cards onto the cell it merged into, and forget
/// its metadata.
fn apply_merges(n: &mut NodeData, merges: &[(u32, u32)]) {
    if merges.is_empty() {
        return;
    }
    let layer = loaded_layer(n);
    if let Some(cards) = n.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
        for c in cards.iter_mut().filter(|c| card_layer(c) == layer) {
            let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if let Some((_, kept)) = merges.iter().find(|(removed, _)| *removed == z) {
                c["z"] = json!(kept);
            }
        }
    }
    for (removed, _) in merges {
        write_zone_meta_entry(n, *removed, &ZoneMeta::default());
    }
    let gates: Vec<u32> = touch_cells(n).into_iter()
        .filter(|c| !merges.iter().any(|(removed, _)| removed == c)).collect();
    n.params.insert(AREA_TOUCH_CELLS_PARAM.into(), json!(gates));
}

fn has_cards(n: &NodeData, id: u32) -> bool {
    let layer = loaded_layer(n);
    n.params.get("zone_maps").and_then(|v| v.as_array())
        .is_some_and(|a| a.iter().any(|c| card_layer(c) == layer
            && c.get("z").and_then(|v| v.as_u64()) == Some(id as u64)))
}

// ── Edits ─────────────────────────────────────────────────────────────────────

/// A layout edit, from the mouse or the pad.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AreaEdit {
    /// Select a cell (the card list follows).
    Select(u32),
    /// Select a border (its settings open), or none.
    SelectBorder(Option<BorderRef>),
    /// Move a border to a position along its own coordinate.
    Move(BorderRef, f32),
    /// Move a border by an amount.
    Nudge(BorderRef, f32),
    AddCut(usize, f32),
    /// Add a ring / row border through a field point.
    AddEdgeAt(f32, f32),
    Recenter(BorderRef),
    Remove(BorderRef),
    ToggleGradient(BorderRef),
}

/// What applying edits did.
#[derive(Default)]
pub(crate) struct EditResult {
    /// An undo step's worth happened (a move is one only when its drag ends).
    pub(crate) structural: bool,
    /// Where the last moved border ended up — a circle cut moved past 12
    /// o'clock renumbers its band, so a grab follows it by this.
    pub(crate) moved_to: Option<BorderRef>,
}

/// Apply edits to the node's layout, honouring its symmetry setting.
pub(crate) fn apply_area_edits(node: &mut NodeData, edits: &[AreaEdit]) -> EditResult {
    let mut layout = area_layout(node);
    let sym = area_symmetric(node);
    let mut res = EditResult::default();
    let mut dirty = false;
    for e in edits {
        match *e {
            AreaEdit::Select(id) => {
                node.params.insert("sel_field".into(), json!(loaded_layer(node) as u64));
                node.params.insert("sel_zone".into(), json!(id));
            }
            AreaEdit::SelectBorder(r) => match r.and_then(|r| border_key(&layout, r)) {
                Some(k) => { node.params.insert(SEL_BORDER.into(), k); }
                None => { node.params.remove(SEL_BORDER); }
            },
            AreaEdit::Move(r, pos) | AreaEdit::Nudge(r, pos) => {
                let Some(cur) = layout.border(r).map(|b| b.pos) else { continue };
                let pos = if matches!(e, AreaEdit::Nudge(..)) { cur + pos } else { pos };
                let moved = match r {
                    BorderRef::Edge(i) => layout.move_edge(i, pos, sym),
                    BorderRef::Cut { band, cut } => layout.move_cut(band, cut, pos, sym),
                };
                if moved {
                    dirty = true;
                    let now = match r {
                        BorderRef::Edge(i) => Some(BorderRef::Edge(i)),
                        BorderRef::Cut { band, .. } => {
                            let d = |c: f32| { let x = (c - pos).rem_euclid(1.0); x.min(1.0 - x) };
                            layout.bands[band].cuts.iter().enumerate()
                                .min_by(|a, b| d(a.1.pos).total_cmp(&d(b.1.pos)))
                                .map(|(cut, _)| BorderRef::Cut { band, cut })
                        }
                    };
                    res.moved_to = now;
                    // A selected border is followed by position, so it moves with it.
                    if let Some(k) = now.and_then(|r| border_key(&layout, r)) {
                        if node.params.contains_key(SEL_BORDER) {
                            node.params.insert(SEL_BORDER.into(), k);
                        }
                    }
                }
            }
            AreaEdit::AddCut(band, v) => {
                layout.add_cut(band, v, sym);
                dirty = true;
                res.structural = true;
            }
            AreaEdit::AddEdgeAt(x, y) => {
                layout.add_edge_at(x, y, sym);
                dirty = true;
                res.structural = true;
            }
            AreaEdit::Recenter(r) => {
                let pos = if sym { layout.centered_pos_symmetric(r) } else { layout.centered_pos(r) };
                if let Some(pos) = pos {
                    let moved = match r {
                        BorderRef::Edge(i) => layout.move_edge(i, pos, sym),
                        BorderRef::Cut { band, cut } => layout.move_cut(band, cut, pos, sym),
                    };
                    dirty |= moved;
                    res.structural |= moved;
                }
            }
            AreaEdit::Remove(r) => {
                let keep = |id: u32| has_cards(node, id);
                let merges = match r {
                    BorderRef::Edge(i) => layout.remove_edge(i, sym, keep),
                    BorderRef::Cut { band, cut } => layout.remove_cut(band, cut, sym, keep),
                };
                apply_merges(node, &merges);
                node.params.remove(SEL_BORDER);
                dirty = true;
                res.structural = true;
            }
            AreaEdit::ToggleGradient(r) => {
                let graded = layout.border(r).is_some_and(|b| b.gradient.is_some());
                let g = (!graded).then(|| Gradient::with_width(default_gradient_width(&layout, r)));
                layout.set_gradient(r, g, sym);
                dirty = true;
                res.structural = true;
            }
        }
    }
    if dirty {
        set_layout(node, &layout);
    }
    res
}

// ── Field geometry ────────────────────────────────────────────────────────────

/// Screen mapping of the area: centred field coords (+Y up, ±1) ↔ screen.
/// Everything is drawn from the layout's points, so squared rings come out
/// right without a case of their own.
#[derive(Clone, Copy)]
struct FieldGeom {
    center: egui::Pos2,
    /// Screen size of one unit (half the area's width).
    unit: f32,
}

impl FieldGeom {
    fn of(rect: egui::Rect) -> Self {
        Self { center: rect.center(), unit: (rect.width().min(rect.height()) * 0.5 - 6.0).max(10.0) }
    }
    fn to_screen(&self, x: f32, y: f32) -> egui::Pos2 {
        self.center + egui::vec2(x, -y) * self.unit
    }
    fn pt(&self, p: [f32; 2]) -> egui::Pos2 {
        self.to_screen(p[0], p[1])
    }
    fn to_area(&self, p: egui::Pos2) -> (f32, f32) {
        let d = (p - self.center) / self.unit;
        (d.x, -d.y)
    }
    fn ring(&self) -> RingGeom {
        RingGeom { center: self.center, r_in: 0.0, r_out: self.unit, origin: 0.0 }
    }
    /// The square the rectangle shape fills.
    fn square(&self) -> egui::Rect {
        egui::Rect::from_center_size(self.center, egui::vec2(self.unit * 2.0, self.unit * 2.0))
    }
}

/// How finely a region is sampled: (along the cut coordinate, along the band
/// coordinate). A circle's rays are straight, so one step outward is exact.
fn region_steps(layout: &AreaLayout, region: Region) -> (usize, usize) {
    match layout.shape {
        Shape::Circle => {
            let turn = match region {
                Region::Cell { band, index } => { let [a, b] = layout.cell_span(band, index); b - a }
                Region::EdgeBand { .. } => 1.0,
                Region::CutBand { half, .. } => half * 2.0,
            };
            (((turn * 96.0).ceil() as usize).clamp(2, 96), 1)
        }
        Shape::Rect if layout.is_curved() => (12, 12),
        Shape::Rect => (1, 1),
    }
}

fn region_mesh(layout: &AreaLayout, g: &FieldGeom, region: Region, fill: egui::Color32) -> egui::Shape {
    let (ns, nt) = region_steps(layout, region);
    let mut mesh = egui::Mesh::default();
    for i in 0..=ns {
        for j in 0..=nt {
            let q = layout.region_point(region, i as f32 / ns as f32, j as f32 / nt as f32);
            mesh.colored_vertex(g.pt(q), fill);
        }
    }
    let row = (nt + 1) as u32;
    for i in 0..ns as u32 {
        for j in 0..nt as u32 {
            let a = i * row + j;
            mesh.add_triangle(a, a + 1, a + row);
            mesh.add_triangle(a + 1, a + row + 1, a + row);
        }
    }
    egui::Shape::mesh(mesh)
}

/// A region's outline, once round its parameter square.
fn region_outline(layout: &AreaLayout, g: &FieldGeom, region: Region) -> Vec<egui::Pos2> {
    let (ns, nt) = region_steps(layout, region);
    let at = |s: f32, t: f32| g.pt(layout.region_point(region, s, t));
    let mut pts = Vec::with_capacity(2 * (ns + nt));
    for i in 0..ns { pts.push(at(i as f32 / ns as f32, 0.0)); }
    for j in 0..nt { pts.push(at(1.0, j as f32 / nt as f32)); }
    for i in (1..=ns).rev() { pts.push(at(i as f32 / ns as f32, 1.0)); }
    for j in (1..=nt).rev() { pts.push(at(0.0, j as f32 / nt as f32)); }
    pts
}

/// A border as a polyline on screen.
fn border_polyline(layout: &AreaLayout, g: &FieldGeom, r: BorderRef) -> Vec<egui::Pos2> {
    let n = match (layout.shape, r) {
        (Shape::Circle, BorderRef::Edge(_)) => 96,
        (Shape::Rect, _) if layout.is_curved() => 24,
        _ => 1,
    };
    (0..=n).map(|k| g.pt(layout.border_point(r, k as f32 / n as f32))).collect()
}

fn distance_to_polyline(p: egui::Pos2, pts: &[egui::Pos2]) -> f32 {
    pts.windows(2).map(|w| {
        let (a, b) = (w[0], w[1]);
        let ab = b - a;
        let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
        (a + ab * t - p).length()
    }).fold(f32::INFINITY, f32::min)
}

fn border_under(layout: &AreaLayout, g: &FieldGeom, p: egui::Pos2) -> Option<BorderRef> {
    all_borders(layout).into_iter()
        .map(|r| (r, distance_to_polyline(p, &border_polyline(layout, g, r))))
        .filter(|(_, d)| *d <= GRAB_PX)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(r, _)| r)
}

fn paint_border(p: &egui::Painter, layout: &AreaLayout, g: &FieldGeom, r: BorderRef, stroke: egui::Stroke) {
    p.add(egui::Shape::line(border_polyline(layout, g, r), stroke));
}

/// Paint a gradient's crossfade band.
fn paint_gradient_band(p: &egui::Painter, layout: &AreaLayout, g: &FieldGeom, r: BorderRef, fill: egui::Color32) {
    let Some(grad) = layout.border(r).and_then(|b| b.gradient.as_ref()) else { return };
    let half = grad.width * 0.5;
    let region = match r {
        BorderRef::Edge(edge) => Region::EdgeBand { edge, half },
        BorderRef::Cut { band, cut } => Region::CutBand { band, cut, half },
    };
    p.add(region_mesh(layout, g, region, fill));
}

/// The outer boundary of the area: the unit circle, or the square.
fn area_outline(layout: &AreaLayout, g: &FieldGeom) -> Vec<egui::Pos2> {
    const N: usize = 96;
    (0..N).map(|k| {
        let f = k as f32 / N as f32;
        match layout.shape {
            Shape::Circle => {
                let a = f * std::f32::consts::TAU;
                g.to_screen(a.sin(), a.cos())
            }
            Shape::Rect => {
                let u = f * 4.0;
                let (x, y) = match u as usize {
                    0 => (-1.0 + 2.0 * u.fract(), 1.0),
                    1 => (1.0, 1.0 - 2.0 * u.fract()),
                    2 => (1.0 - 2.0 * u.fract(), -1.0),
                    _ => (-1.0, -1.0 + 2.0 * u.fract()),
                };
                g.to_screen(x, y)
            }
        }
    }).collect()
}

/// A bubbly rectangle sampled for drawing: which cell owns each vertex of an
/// `N`×`N` grid of blocks tiling the area, where the owner changes along each
/// block edge (found by bisection — a seam is two gauges meeting or a shape's
/// edge), every cell's centroid and sample count, and the seams traced
/// through the blocks (marching squares). Regions are filled block by block
/// from the same crossings, so fills and seams line up and leave no gaps. In
/// area coordinates; cached per layout.
struct BubbleField {
    /// Owner cell id per vertex, rows top to bottom.
    owner: Vec<u32>,
    /// Crossing on the edge (i, j) → (i + 1, j), at `j * N + i`.
    h_cross: Vec<Option<[f32; 2]>>,
    /// Crossing on the edge (i, j) → (i, j + 1), at `j * (N + 1) + i`.
    v_cross: Vec<Option<[f32; 2]>>,
    /// Cell id → (centroid, sample count).
    cells: HashMap<u32, ([f32; 2], usize)>,
    /// Seam pieces and the two cells either side.
    seams: Vec<([f32; 2], [f32; 2], (u32, u32))>,
    /// Per seam piece, the border the two cells share, if any.
    seam_border: Vec<Option<BorderRef>>,
    /// Per vertex, whether a gradient crossfades there; with the crossings
    /// of that, laid out like `h_cross` / `v_cross`. Empty without gradients.
    fade: Vec<bool>,
    fade_h: Vec<Option<[f32; 2]>>,
    fade_v: Vec<Option<[f32; 2]>>,
}

const BUBBLE_N: usize = 96;

fn bubble_vertex(i: usize, j: usize) -> [f32; 2] {
    let step = 2.0 / BUBBLE_N as f32;
    [-1.0 + i as f32 * step, 1.0 - j as f32 * step]
}

impl BubbleField {
    fn build(layout: &AreaLayout) -> BubbleField {
        let n = BUBBLE_N;
        let m = n + 1;
        let mut owners: Vec<(usize, usize)> = Vec::with_capacity(m * m);
        let mut cells: HashMap<u32, ([f32; 2], usize)> = HashMap::new();
        for j in 0..m {
            for i in 0..m {
                let [x, y] = bubble_vertex(i, j);
                let (b, k) = layout.locate(x, y);
                owners.push((b, k));
                let e = cells.entry(layout.bands[b].cells[k]).or_insert(([0.0, 0.0], 0));
                e.0[0] += x;
                e.0[1] += y;
                e.1 += 1;
            }
        }
        for (c, count) in cells.values_mut() {
            c[0] /= *count as f32;
            c[1] /= *count as f32;
        }
        // Where along p → q the owner stops being `a`.
        let cross = |p: [f32; 2], a: (usize, usize), q: [f32; 2]| -> [f32; 2] {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..10 {
                let t = (lo + hi) * 0.5;
                if layout.locate(p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t) == a { lo = t } else { hi = t }
            }
            let t = (lo + hi) * 0.5;
            [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]
        };
        let own = |i: usize, j: usize| owners[j * m + i];
        let mut h_cross = vec![None; n * m];
        let mut v_cross = vec![None; m * n];
        for j in 0..m {
            for i in 0..n {
                if own(i, j) != own(i + 1, j) {
                    h_cross[j * n + i] = Some(cross(bubble_vertex(i, j), own(i, j), bubble_vertex(i + 1, j)));
                }
            }
        }
        for j in 0..n {
            for i in 0..m {
                if own(i, j) != own(i, j + 1) {
                    v_cross[j * m + i] = Some(cross(bubble_vertex(i, j), own(i, j), bubble_vertex(i, j + 1)));
                }
            }
        }
        let id = |o: (usize, usize)| layout.bands[o.0].cells[o.1];
        // Where the gradients crossfade, as the engine weighs it: a solid band
        // that follows the seams however they bend.
        let graded = layout.edges.iter().chain(layout.bands.iter().flat_map(|b| b.cuts.iter()))
            .any(|b| b.gradient.is_some());
        let (mut fade, mut fade_h, mut fade_v) = (Vec::new(), Vec::new(), Vec::new());
        if graded {
            let fades = |x: f32, y: f32| layout.weights(x, y, |_: &Gradient, t: f32| t).len() >= 2;
            fade = (0..m * m).map(|k| { let [x, y] = bubble_vertex(k % m, k / m); fades(x, y) }).collect();
            let cross = |p: [f32; 2], q: [f32; 2], from: bool| -> [f32; 2] {
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..10 {
                    let t = (lo + hi) * 0.5;
                    if fades(p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t) == from { lo = t } else { hi = t }
                }
                let t = (lo + hi) * 0.5;
                [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]
            };
            fade_h = vec![None; n * m];
            fade_v = vec![None; m * n];
            for j in 0..m {
                for i in 0..n {
                    let (a, b) = (fade[j * m + i], fade[j * m + i + 1]);
                    if a != b {
                        fade_h[j * n + i] = Some(cross(bubble_vertex(i, j), bubble_vertex(i + 1, j), a));
                    }
                }
            }
            for j in 0..n {
                for i in 0..m {
                    let (a, b) = (fade[j * m + i], fade[(j + 1) * m + i]);
                    if a != b {
                        fade_v[j * m + i] = Some(cross(bubble_vertex(i, j), bubble_vertex(i, j + 1), a));
                    }
                }
            }
        }
        let mut field = BubbleField {
            owner: owners.iter().map(|o| id(*o)).collect(),
            h_cross,
            v_cross,
            cells,
            seams: Vec::new(),
            seam_border: Vec::new(),
            fade,
            fade_h,
            fade_v,
        };
        for j in 0..n {
            for i in 0..n {
                let edges = field.block_edges(i, j);
                let hits: Vec<([f32; 2], (u32, u32))> = edges.iter()
                    .filter_map(|(a, b, c)| c.map(|c| (c, (*a, *b))))
                    .collect();
                match hits.len() {
                    0 => {}
                    2 => field.seams.push((hits[0].0, hits[1].0, hits[0].1)),
                    _ => {
                        // A junction: join each crossing to their middle.
                        let mid = hits.iter().fold([0.0, 0.0], |a, h| [a[0] + h.0[0], a[1] + h.0[1]]);
                        let mid = [mid[0] / hits.len() as f32, mid[1] / hits.len() as f32];
                        for h in &hits {
                            field.seams.push((h.0, mid, h.1));
                        }
                    }
                }
            }
        }
        field.seam_border = field.seams.iter().map(|(_, _, (a, b))| layout.border_between_ids(*a, *b)).collect();
        field
    }

    /// The border under a point (px), along the seams it now forms.
    fn border_under(&self, g: &FieldGeom, p: egui::Pos2) -> Option<BorderRef> {
        self.seams.iter().zip(&self.seam_border)
            .filter_map(|((a, b, _), r)| {
                let d = distance_to_polyline(p, &[g.pt(*a), g.pt(*b)]);
                r.filter(|_| d <= GRAB_PX).map(|r| (r, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(r, _)| r)
    }

    /// A block's corners, clockwise from top-left.
    fn block_corners(i: usize, j: usize) -> [(usize, usize); 4] {
        [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)]
    }

    fn owner_at(&self, c: (usize, usize)) -> u32 {
        self.owner[c.1 * (BUBBLE_N + 1) + c.0]
    }

    /// The block's four edges in corner order: (owner at the start, owner at
    /// the end, crossing if they differ).
    fn block_edges(&self, i: usize, j: usize) -> [(u32, u32, Option<[f32; 2]>); 4] {
        let (n, m) = (BUBBLE_N, BUBBLE_N + 1);
        let c = Self::block_corners(i, j);
        let o: Vec<u32> = c.iter().map(|c| self.owner_at(*c)).collect();
        [
            (o[0], o[1], self.h_cross[j * n + i]),
            (o[1], o[2], self.v_cross[j * m + i + 1]),
            (o[2], o[3], self.h_cross[(j + 1) * n + i]),
            (o[3], o[0], self.v_cross[j * m + i]),
        ]
    }

    /// The cached field for this layout, rebuilt when the layout changes.
    fn cached(ctx: &egui::Context, host: egui::Id, layout: &AreaLayout) -> std::sync::Arc<BubbleField> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        layout.to_value().to_string().hash(&mut h);
        let key = h.finish();
        let id = host.with("bubbles");
        if let Some((k, f)) = ctx.data(|d| d.get_temp::<(u64, std::sync::Arc<BubbleField>)>(id)) {
            if k == key {
                return f;
            }
        }
        let f = std::sync::Arc::new(BubbleField::build(layout));
        ctx.data_mut(|d| d.insert_temp(id, (key, f.clone())));
        f
    }

    /// Fill cell `id`'s region: each block's part of it, cut along the seam.
    fn fill(&self, p: &egui::Painter, g: &FieldGeom, id: u32, color: egui::Color32) {
        self.fill_where(p, g, color, |c| self.owner_at(c) == id, &self.h_cross, &self.v_cross);
    }

    /// Fill where the gradients crossfade.
    fn fill_fades(&self, p: &egui::Painter, g: &FieldGeom, color: egui::Color32) {
        if self.fade.is_empty() {
            return;
        }
        let m = BUBBLE_N + 1;
        self.fill_where(p, g, color, |c| self.fade[c.1 * m + c.0], &self.fade_h, &self.fade_v);
    }

    /// Fill the region whose vertices are `inside`: each block's part of it,
    /// cut at the crossings on its edges (laid out like `h_cross` / `v_cross`).
    fn fill_where(
        &self,
        p: &egui::Painter,
        g: &FieldGeom,
        color: egui::Color32,
        inside: impl Fn((usize, usize)) -> bool,
        h: &[Option<[f32; 2]>],
        v: &[Option<[f32; 2]>],
    ) {
        let (n, m) = (BUBBLE_N, BUBBLE_N + 1);
        let mut mesh = egui::Mesh::default();
        for j in 0..BUBBLE_N {
            for i in 0..BUBBLE_N {
                let corners = Self::block_corners(i, j);
                let crosses = [h[j * n + i], v[j * m + i + 1], h[(j + 1) * n + i], v[j * m + i]];
                let mut poly: Vec<egui::Pos2> = Vec::with_capacity(8);
                for k in 0..4 {
                    let (a, b) = (inside(corners[k]), inside(corners[(k + 1) % 4]));
                    if a {
                        let [x, y] = bubble_vertex(corners[k].0, corners[k].1);
                        poly.push(g.to_screen(x, y));
                    }
                    if a != b {
                        if let Some([x, y]) = crosses[k] {
                            poly.push(g.to_screen(x, y));
                        }
                    }
                }
                if poly.len() < 3 {
                    continue;
                }
                let base = mesh.vertices.len() as u32;
                for q in &poly {
                    mesh.colored_vertex(*q, color);
                }
                for k in 1..poly.len() as u32 - 1 {
                    mesh.add_triangle(base, base + k, base + k + 1);
                }
            }
        }
        p.add(egui::Shape::mesh(mesh));
    }

    /// A cell's centre on screen and the room it has for content (px).
    fn centre(&self, g: &FieldGeom, id: u32) -> Option<(egui::Pos2, f32)> {
        let (c, count) = self.cells.get(&id)?;
        let side = (*count as f32).sqrt() * 2.0 / BUBBLE_N as f32 * g.unit;
        Some((g.to_screen(c[0], c[1]), side * 0.8))
    }
}

/// Paint a bubbly rectangle's cells: plate, highlights, content, seams.
#[allow(clippy::too_many_arguments)]
fn paint_bubbles(
    ui: &egui::Ui,
    p: &egui::Painter,
    layout: &AreaLayout,
    g: &FieldGeom,
    field: &BubbleField,
    look: &CellLook<'_>,
) {
    p.rect_filled(g.square(), 4.0, plate_fill(look.colors.main));
    for band in &layout.bands {
        for &id in &band.cells {
            let w = look.weights.get(&id).copied().unwrap_or(0.0);
            let full = look.hard_cell == Some(id) && w >= 0.999;
            if full {
                field.fill(p, g, id, look.colors.hi.gamma_multiply(0.30));
            } else if w > 0.0 {
                field.fill(p, g, id, look.colors.hi.gamma_multiply(0.30 * w));
            }
            if look.nav_cell == Some(id) {
                field.fill(p, g, id, look.nav_accent.gamma_multiply(0.22));
            }
            if let Some((c, room)) = field.centre(g, id) {
                paint_cell_content(ui, p, c, room, id, look.zone_maps, look.meta, full, egui::Color32::WHITE);
            }
        }
    }
    // Gradient bands: where the engine crossfades, however the seams bend.
    field.fill_fades(p, g, look.colors.accent.gamma_multiply(0.14));
    for ((a, b, (i, j)), r) in field.seams.iter().zip(&field.seam_border) {
        let touches = |c: Option<u32>| c.is_some_and(|c| c == *i || c == *j);
        let full = look.hard_cell.filter(|id| look.weights.get(id).copied().unwrap_or(0.0) >= 0.999);
        let lit = r.is_some_and(|r| look.lit.contains(&r));
        let stroke = if lit {
            egui::Stroke::new(2.5, if look.lit_grabbed { look.nav_grabbed } else { look.lit_color })
        } else if touches(look.nav_cell) {
            egui::Stroke::new(2.5, look.nav_accent)
        } else if touches(full) {
            egui::Stroke::new(2.0, look.colors.hi)
        } else if touches(Some(look.sel)) {
            egui::Stroke::new(2.0, look.colors.hi.gamma_multiply(0.6))
        } else {
            egui::Stroke::new(1.5, look.colors.main)
        };
        p.line_segment([g.pt(*a), g.pt(*b)], stroke);
    }
    p.rect_stroke(g.square(), 4.0, egui::Stroke::new(1.5, look.colors.main), egui::StrokeKind::Middle);
}

/// What a cell painter needs to know beyond the geometry.
struct CellLook<'a> {
    /// Borders lit (hovered / selected / gamepad-focused, with mirrors): their
    /// seams are drawn lit.
    lit: &'a [BorderRef],
    lit_color: egui::Color32,
    lit_grabbed: bool,
    nav_grabbed: egui::Color32,
    weights: &'a HashMap<u32, f32>,
    hard_cell: Option<u32>,
    sel: u32,
    nav_cell: Option<u32>,
    nav_accent: egui::Color32,
    zone_maps: &'a [Value],
    meta: &'a HashMap<u32, ZoneMeta>,
    colors: ZoneColors,
}

/// The screen centre of a cell, for its content and the gamepad walk.
fn cell_center(layout: &AreaLayout, g: &FieldGeom, band: usize, index: usize) -> egui::Pos2 {
    let [v0, v1] = layout.cell_span(band, index);
    // A whole disc's centre is the centre itself.
    if layout.shape == Shape::Circle && band == 0 && v1 - v0 >= 0.999 {
        return g.center;
    }
    g.pt(layout.region_point(Region::Cell { band, index }, 0.5, 0.5))
}

/// Points a border is reachable at, for the gamepad walk: a cut at its middle;
/// an edge (a whole ring or row) beside each cell of the band beyond it, so it
/// is near from every cell along it.
fn border_points(layout: &AreaLayout, g: &FieldGeom, r: BorderRef) -> Vec<egui::Pos2> {
    match r {
        BorderRef::Cut { .. } => vec![g.pt(layout.border_point(r, 0.5))],
        BorderRef::Edge(e) => (0..layout.bands[e + 1].cells.len()).map(|i| {
            let [v0, v1] = layout.cell_span(e + 1, i);
            let mid = (v0 + v1) * 0.5;
            let s = match layout.shape {
                Shape::Circle => mid.rem_euclid(1.0),
                Shape::Rect => (mid + 1.0) * 0.5,
            };
            g.pt(layout.border_point(r, s))
        }).collect(),
    }
}

/// A cell whose shape the plain wedge painter can't draw (a squared ring bends
/// it): plate, outline, and its icons or name at its centre.
#[allow(clippy::too_many_arguments)]
fn paint_curved_cell(
    ui: &egui::Ui,
    p: &egui::Painter,
    layout: &AreaLayout,
    g: &FieldGeom,
    (band, index, id): (usize, usize, u32),
    hovered: bool,
    selected: bool,
    zone_maps: &[Value],
    meta: &HashMap<u32, ZoneMeta>,
    colors: ZoneColors,
) {
    let region = Region::Cell { band, index };
    let fill = if hovered { colors.hi.gamma_multiply(0.30) } else { plate_fill(colors.main) };
    p.add(region_mesh(layout, g, region, fill));
    let stroke = if hovered {
        egui::Stroke::new(2.0, colors.hi)
    } else if selected {
        egui::Stroke::new(2.0, colors.hi.gamma_multiply(0.6))
    } else {
        egui::Stroke::new(1.0, colors.main)
    };
    p.add(egui::Shape::closed_line(region_outline(layout, g, region), stroke));

    // Room for content: the cell's narrower middle span.
    let at = |s: f32, t: f32| g.pt(layout.region_point(region, s, t));
    let room = (at(0.2, 0.5) - at(0.8, 0.5)).length().min((at(0.5, 0.2) - at(0.5, 0.8)).length());
    let center = cell_center(layout, g, band, index);
    paint_cell_content(ui, p, center, room, id, zone_maps, meta, hovered, egui::Color32::WHITE);
}

/// A cell's icons (or icon override), name, or bare index, centred on
/// `center` within `room` px.
#[allow(clippy::too_many_arguments)]
fn paint_cell_content(
    ui: &egui::Ui,
    p: &egui::Painter,
    center: egui::Pos2,
    room: f32,
    id: u32,
    zone_maps: &[Value],
    meta: &HashMap<u32, ZoneMeta>,
    hovered: bool,
    tint: egui::Color32,
) {
    let m = meta.get(&id);
    let label = m.map(|m| m.label.as_str()).unwrap_or("");
    let ic = (room * 0.5).clamp(10.0, 26.0);
    let has_override = m.is_some_and(|m| !m.icon.is_empty() || !m.svg.is_empty());
    let pins = if has_override { Vec::new() } else { super::menu_body::zone_out_pins(zone_maps, id) };
    let icon_y = if label.is_empty() { center.y } else { center.y - ic * 0.35 };
    if let (true, Some(m)) = (has_override, m) {
        if let Some(tex) = crate::macro_icons::macro_port_icon_texture(ui.ctx(), &m.icon, &m.svg, ic) {
            p.image(tex.id(), egui::Rect::from_center_size(egui::pos2(center.x, icon_y), egui::vec2(ic, ic)),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
    } else if !pins.is_empty() {
        let gap = 3.0;
        let fit = (((room * 0.9 + gap) / (ic + gap)).floor() as usize).clamp(1, pins.len());
        let mut x = center.x - (fit as f32 * ic + (fit - 1) as f32 * gap) * 0.5;
        for pin in pins.iter().take(fit) {
            let w = super::viewer::paint_chord_chip_tinted(
                p, ui.ctx(), egui::pos2(x, icon_y - ic * 0.5), ic, pin, super::remapper_icons::Skin::Kbm, tint);
            x += w + gap;
        }
    } else if label.is_empty() {
        p.text(center, egui::Align2::CENTER_CENTER, format!("{id}"),
            egui::FontId::proportional((room * 0.3).clamp(10.0, 20.0)),
            if hovered { egui::Color32::WHITE } else { egui::Color32::from_gray(150) });
    }
    if !label.is_empty() {
        let (font, text) = super::menu_body::fit_zone_label(p, label, 11.0, room.max(20.0));
        p.text(egui::pos2(center.x, center.y + ic * 0.45), egui::Align2::CENTER_TOP, text, font,
            if hovered { egui::Color32::WHITE } else { egui::Color32::from_gray(200) });
    }
}

/// An analog layer's direction cells and where their mappings sit: out along
/// each cell's direction, mid-band (a rectangle's at the cell's centre).
fn analog_chips(layout: &AreaLayout, g: &FieldGeom) -> Vec<(u32, egui::Pos2, (f32, f32))> {
    let mut out = Vec::new();
    for (b, band) in layout.bands.iter().enumerate() {
        if b == 0 && layout.has_centre_disc() {
            continue;
        }
        let [lo, hi] = layout.band_span(b);
        for (i, &id) in band.cells.iter().enumerate() {
            let Some((dx, dy)) = layout.analog_dir(b, i) else { continue };
            let at = match layout.shape {
                Shape::Circle => {
                    let r = (lo + hi) * 0.5;
                    g.to_screen(dx * r, dy * r)
                }
                Shape::Rect => cell_center(layout, g, b, i),
            };
            out.push((id, at, (dx, dy)));
        }
    }
    out
}

/// The cell a click in an analog layer's view means: the centre inside the
/// deadzone, else the nearest direction's mapping.
fn analog_cell_at(layout: &AreaLayout, g: &FieldGeom, hp: egui::Pos2) -> u32 {
    let (x, y) = g.to_area(hp);
    if layout.has_centre_disc() && (x * x + y * y).sqrt() < layout.edges[0].pos {
        return layout.bands[0].cells[0];
    }
    analog_chips(layout, g).into_iter()
        .min_by(|a, b| (a.1 - hp).length().total_cmp(&(b.1 - hp).length()))
        .map(|c| c.0)
        .unwrap_or_else(|| layout.locate_id(x, y))
}

/// An analog layer drawn as what it does — its borders and gradients don't
/// apply: the deadzone, and each direction's mapping out along its spoke,
/// the spoke lit as far as that key is held (its share of the time).
#[allow(clippy::too_many_arguments)]
fn paint_analog_view(
    ui: &egui::Ui,
    p: &egui::Painter,
    layout: &AreaLayout,
    g: &FieldGeom,
    look: &CellLook<'_>,
    full: f32,
    full_lit: bool,
) {
    let main = look.colors.main;
    match layout.shape {
        Shape::Circle => {
            p.circle_filled(g.center, g.unit, plate_fill(main));
            p.circle_stroke(g.center, g.unit, egui::Stroke::new(1.5, main));
        }
        Shape::Rect => {
            p.rect_filled(g.square(), 4.0, plate_fill(main));
            p.rect_stroke(g.square(), 4.0, egui::Stroke::new(1.5, main), egui::StrokeKind::Middle);
        }
    }
    let share = |id: u32| look.weights.get(&id).copied().unwrap_or(0.0);
    let dz = if layout.has_centre_disc() { layout.edges[0].pos } else { 0.0 };
    let room = (g.unit * 0.42).max(24.0);
    for (id, at, (dx, dy)) in analog_chips(layout, g) {
        let from = g.to_screen(dx * dz, dy * dz);
        let to = match layout.shape {
            Shape::Circle => g.to_screen(dx, dy),
            Shape::Rect => at,
        };
        p.line_segment([from, to], egui::Stroke::new(1.0, main.gamma_multiply(0.6)));
        let w = share(id);
        if w > 0.0 {
            p.line_segment([from, from + (to - from) * w], egui::Stroke::new(5.0, look.colors.hi.gamma_multiply(0.8)));
        }
        let plate = room * 0.62;
        p.circle_filled(at, plate, if w > 0.0 {
            look.colors.hi.gamma_multiply(0.12 + 0.3 * w)
        } else {
            plate_fill(main)
        });
        let ring = if look.nav_cell == Some(id) {
            egui::Stroke::new(2.5, look.nav_accent)
        } else if look.sel == id {
            egui::Stroke::new(2.0, look.colors.hi.gamma_multiply(0.6))
        } else {
            egui::Stroke::new(1.0, main)
        };
        p.circle_stroke(at, plate, ring);
        paint_cell_content(ui, p, at, room, id, look.zone_maps, look.meta, w >= 0.5, egui::Color32::WHITE);
    }
    // The full-push ring: beyond it the keys hold steadily.
    let ring = if full_lit {
        egui::Stroke::new(2.5, look.colors.accent)
    } else {
        egui::Stroke::new(1.5, look.colors.accent.gamma_multiply(0.6))
    };
    p.circle_stroke(g.center, full.max(dz) * g.unit, ring);
    if layout.has_centre_disc() {
        let id = layout.bands[0].cells[0];
        let r = dz * g.unit;
        let full = share(id) >= 0.999;
        p.circle_filled(g.center, r, if full { look.colors.hi.gamma_multiply(0.3) } else { main.gamma_multiply(0.18) });
        if look.nav_cell == Some(id) {
            p.circle_stroke(g.center, r, egui::Stroke::new(2.5, look.nav_accent));
        } else if look.sel == id {
            p.circle_stroke(g.center, r, egui::Stroke::new(2.0, look.colors.hi.gamma_multiply(0.6)));
        }
        paint_cell_content(ui, p, g.center, r * 1.2, id, look.zone_maps, look.meta, full, egui::Color32::WHITE);
    }
}

/// Paint the area and run its mouse editing. Returns the field rect and
/// whether an undo step's worth of editing happened.
fn area_field(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    colors: ZoneColors,
    size: f32,
    resizable: bool,
) -> (egui::Rect, bool) {
    let Some(node) = snarl.get_node(node_id) else { return (egui::Rect::NOTHING, false) };
    let layout = area_layout(node);
    let sym = area_symmetric(node);
    let (point, weights) = area_live(node);
    // The cards by layer: this layer's draw as usual, the others' tinted
    // where their own cells are.
    let card_layer = |c: &Value| c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let all_cards: Vec<Value> = node.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let zone_maps: Vec<Value> = all_cards.iter().filter(|c| card_layer(c) == loaded_layer(node)).cloned().collect();
    let meta = menu_zone_meta(node);
    let full = node.params.get(AREA_LAYER_FULL_PARAM).and_then(|v| v.as_f64()).map(|v| v as f32)
        .unwrap_or(AREA_ANALOG_FULL_DEFAULT);
    let sel = selected_cell(node, &layout);
    let sel_border = selected_border(node, &layout);
    let picking = tz_pick_kind(snarl, node_id).is_some();
    let ctx = ui.ctx().clone();

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let g = FieldGeom::of(rect);
    let p = ui.painter_at(rect.expand(2.0));
    // Pointer positions in this layer's space: the canvas is zoomed and
    // panned, and egui reports the press origin in screen space.
    let to_global = ctx.layer_transform_to_global(ui.layer_id()).unwrap_or(egui::emath::TSTransform::IDENTITY);
    let from_global = to_global.inverse();
    let hover_pos = resp.hover_pos();
    // Per host: the same node can be drawn in the editor and pinned at once.
    let host = egui::Id::new((ctx.viewport_id(), ui.layer_id(), node_id, "area_field"));
    let drag_id = host.with("drag");
    let drag_state: Option<Value> = ctx.data(|d| d.get_temp::<Value>(drag_id));
    let dragging: Option<BorderRef> = drag_state.as_ref().and_then(|v| border_from_key(&layout, v));
    let drag_offset = drag_state.as_ref().and_then(|v| v.get("off")).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let full_id = host.with("full_drag");
    let full_dragging = ctx.data(|d| d.get_temp::<bool>(full_id)).unwrap_or(false);
    // An analog layer shows its directions, not its borders: only the
    // deadzone's ring stays to drag.
    let analog = node.params.get(AREA_LAYER_ANALOG_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let analog_edge = (analog && layout.has_centre_disc()).then_some(BorderRef::Edge(0));
    let bubbles = (!analog && layout.is_bubbly()).then(|| BubbleField::cached(&ctx, host, &layout));
    // A bubbly layout's borders are grabbed along the seams they form, then
    // by their grid line.
    let under = |hp: egui::Pos2| {
        if analog {
            return analog_edge.filter(|r| distance_to_polyline(hp, &border_polyline(&layout, &g, *r)) <= GRAB_PX);
        }
        bubbles.as_ref().and_then(|f| f.border_under(&g, hp)).or_else(|| border_under(&layout, &g, hp))
    };
    // An analog layer's full-push ring, grabbed where no border is.
    let near_full = |hp: egui::Pos2| analog && under(hp).is_none()
        && ((hp - g.center).length() - full * g.unit).abs() <= GRAB_PX;
    // Gamepad focus on the ring rides the Touch Zones seam channel (an Area
    // Mapper has no seam of its own); grabbed is the shared flag.
    let nav_ring = analog && ctx
        .data(|d| d.get_temp::<(u64, bool)>(egui::Id::new(("gp_nav_tz_seam", node_id.0))))
        .is_some_and(|(pass, on)| on && crate::widgets::nav_pass_matches(&ctx, pass));
    let full_lit = nav_ring || full_dragging || (dragging.is_none() && hover_pos.is_some_and(near_full));
    let cell_at = |hp: egui::Pos2| {
        if analog {
            analog_cell_at(&layout, &g, hp)
        } else {
            let (x, y) = g.to_area(hp);
            layout.locate_id(x, y)
        }
    };
    let hovered_border = dragging.or_else(|| hover_pos.and_then(under));
    let borders = all_borders(&layout);

    // Gamepad focus, on the Touch Zones channels (axis 0, line = border index).
    let cur_pass = crate::widgets::nav_pass(&ctx);
    let nav_line: Option<(usize, bool)> = ctx
        .data(|d| d.get_temp::<(u64, u64, u64, u64, bool)>(egui::Id::new(("gp_nav_tz", node_id.0))))
        .filter(|(pass, f, a, _, _)| cur_pass.saturating_sub(*pass) <= 2 && *f == 0 && *a == 0)
        .map(|(_, _, _, l, grabbed)| (l as usize, grabbed));
    let nav_cell: Option<u32> = ctx
        .data(|d| d.get_temp::<(u64, u64, u64)>(egui::Id::new(("gp_nav_tz_zone", node_id.0))))
        .filter(|(pass, f, z)| *f == 0 && *z != u64::MAX && crate::widgets::nav_pass_matches(&ctx, *pass))
        .map(|(_, _, z)| z as u32);
    let nav_style = crate::widgets::NavHighlightStyle::of(&ctx);

    // ── Cells ── plain wedges and rectangles through the menu's painters
    // (curved names, icon layout); bent ones through the layout's own points.
    let hard_cell = point.map(|pt| layout.locate_id(pt.x, pt.y));
    let curved = layout.is_curved();
    if layout.shape == Shape::Rect && !curved && bubbles.is_none() {
        p.rect_filled(g.square(), 4.0, plate_fill(colors.main));
    }
    let ring = g.ring();
    let mut nav_targets: Vec<(u8, u32, u32, egui::Pos2)> = Vec::new();
    if analog {
        for (id, at, _) in analog_chips(&layout, &g) {
            nav_targets.push((0, id, 0, to_global * at));
        }
        if layout.has_centre_disc() {
            nav_targets.push((0, layout.bands[0].cells[0], 0, to_global * g.center));
        }
        // The full-push ring (kind 2), on the diagonals so it is near from
        // every side without sitting on a 4-way direction.
        for k in 0..4 {
            let a = std::f32::consts::FRAC_PI_4 + k as f32 * std::f32::consts::FRAC_PI_2;
            nav_targets.push((2, 0, 0, to_global * g.to_screen(a.sin() * full, a.cos() * full)));
        }
    }
    for (b, band) in layout.bands.iter().enumerate() {
        if analog {
            break;
        }
        let [lo, hi] = layout.band_span(b);
        for (i, &id) in band.cells.iter().enumerate() {
            let [v0, v1] = layout.cell_span(b, i);
            let w = weights.get(&id).copied().unwrap_or(0.0);
            let full = hard_cell == Some(id) && w >= 0.999;
            let focused = nav_cell == Some(id);
            if let Some(field) = &bubbles {
                // A bubble's centre is where its area actually is.
                let c = field.centre(&g, id).map(|(c, _)| c).unwrap_or_else(|| cell_center(&layout, &g, b, i));
                nav_targets.push((0, id, 0, to_global * c));
                continue;
            }
            nav_targets.push((0, id, 0, to_global * cell_center(&layout, &g, b, i)));
            match (layout.shape, curved) {
                (_, true) => paint_curved_cell(ui, &p, &layout, &g, (b, i, id), full, sel == id,
                    &zone_maps, &meta, colors),
                (Shape::Circle, false) => {
                    paint_radial_zone(ui, &ring, id, [v0, lo, v1, hi], full, sel == id, &zone_maps, &meta, colors);
                }
                (Shape::Rect, false) => {
                    let zr = egui::Rect::from_two_pos(g.to_screen(v0, hi), g.to_screen(v1, lo)).shrink(1.5);
                    paint_grid_zone_cell(ui, zr, id, full, &zone_maps, &meta, colors);
                    if sel == id {
                        p.rect_stroke(zr, 5.0, egui::Stroke::new(2.0, colors.hi.gamma_multiply(0.6)), egui::StrokeKind::Inside);
                    }
                }
            }
            let region = Region::Cell { band: b, index: i };
            if w > 0.0 && !full {
                p.add(region_mesh(&layout, &g, region, colors.hi.gamma_multiply(0.30 * w)));
            }
            if focused {
                p.add(region_mesh(&layout, &g, region, nav_style.accent.gamma_multiply(0.22)));
                p.add(egui::Shape::closed_line(region_outline(&layout, &g, region),
                    egui::Stroke::new(2.5, nav_style.accent)));
            }
        }
    }

    let nav_border = nav_line.and_then(|(l, grabbed)| borders.get(l).map(|r| (*r, grabbed)));
    let lit_from = nav_border.map(|(r, _)| r).or(hovered_border).or(sel_border);
    let lit: Vec<BorderRef> = lit_from
        .map(|r| if sym { layout.mirror_set(r) } else { vec![r] })
        .unwrap_or_default();
    if analog {
        paint_analog_view(ui, &p, &layout, &g, &CellLook {
            lit: &lit,
            lit_color: colors.accent,
            lit_grabbed: false,
            nav_grabbed: nav_style.grabbed,
            weights: &weights, hard_cell, sel, nav_cell, nav_accent: nav_style.accent,
            zone_maps: &zone_maps, meta: &meta, colors,
        }, full, full_lit);
    }
    if let Some(field) = &bubbles {
        paint_bubbles(ui, &p, &layout, &g, field, &CellLook {
            lit: &lit,
            lit_color: if nav_border.is_some() { nav_style.accent } else { colors.accent },
            lit_grabbed: nav_border.is_some_and(|(_, grabbed)| grabbed),
            nav_grabbed: nav_style.grabbed,
            weights: &weights, hard_cell, sel, nav_cell, nav_accent: nav_style.accent,
            zone_maps: &zone_maps, meta: &meta, colors,
        });
    }

    // ── Gradient bands, then borders ── (bubbles keep the grid as a faint
    // skeleton: the lines that are dragged, not where the cells now meet).
    if bubbles.is_none() && !analog {
        for r in &borders {
            paint_gradient_band(&p, &layout, &g, *r, colors.accent.gamma_multiply(0.14));
        }
    }
    // The other layers' borders, faint in their colours.
    if let Some(node) = snarl.get_node(node_id) {
        let loaded = loaded_layer(node);
        for (k, l) in node.params.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
            if k == loaded { continue; }
            let Some(other) = l.get(AREA_LAYOUT_PARAM).and_then(AreaLayout::from_value) else { continue };
            let c = layer_color(|key| l.get(key).cloned(), k);
            let col = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 150);
            let other_analog = l.get(AREA_LAYER_ANALOG_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
            for r in all_borders(&other) {
                if other_analog && !(other.has_centre_disc() && r == BorderRef::Edge(0)) {
                    continue; // an analog layer's borders don't apply
                }
                paint_border(&p, &other, &g, r, egui::Stroke::new(1.0, col));
            }
            // Its mappings where its cells are, in its colour.
            let cards: Vec<Value> = all_cards.iter().filter(|c| card_layer(c) == k).cloned().collect();
            if cards.is_empty() {
                continue;
            }
            let spots: Vec<(u32, egui::Pos2)> = if other_analog {
                let mut v: Vec<(u32, egui::Pos2)> = analog_chips(&other, &g).into_iter().map(|c| (c.0, c.1)).collect();
                if other.has_centre_disc() {
                    v.push((other.bands[0].cells[0], g.center));
                }
                v
            } else {
                other.bands.iter().enumerate().flat_map(|(b, band)| {
                    let other = &other;
                    let g = &g;
                    band.cells.iter().enumerate().map(move |(i, &id)| (id, cell_center(other, g, b, i)))
                }).collect()
            };
            let tint = egui::Color32::from_rgb(c[0], c[1], c[2]);
            let no_meta = HashMap::new();
            for (id, at) in spots {
                if super::menu_body::zone_out_pins(&cards, id).is_empty() {
                    continue;
                }
                paint_cell_content(ui, &p, at, g.unit * 0.3, id, &cards, &no_meta, false, tint);
            }
        }
    }
    // A bubbly layout's grid lines are only handles: lit ones show on the
    // seams (above), their lines stay faint unless they form no seam at all.
    let seamed: Vec<BorderRef> = bubbles.as_ref()
        .map(|f| f.seam_border.iter().flatten().copied().collect()).unwrap_or_default();
    for (i, r) in borders.iter().enumerate() {
        if analog && analog_edge != Some(*r) {
            continue;
        }
        for pt in border_points(&layout, &g, *r) {
            nav_targets.push((1, 0, i as u32, to_global * pt));
        }
        let graded = layout.border(*r).is_some_and(|b| b.gradient.is_some());
        let stroke = match nav_border {
            Some((nr, grabbed)) if lit.contains(r) && lit_from == Some(nr) && !seamed.contains(r) => {
                egui::Stroke::new(3.0, if grabbed { nav_style.grabbed } else { nav_style.accent })
            }
            _ if lit.contains(r) && !seamed.contains(r) => egui::Stroke::new(2.5, colors.accent),
            _ if bubbles.is_some() => egui::Stroke::new(1.0, colors.main.gamma_multiply(if graded { 0.5 } else { 0.3 })),
            _ if graded => egui::Stroke::new(1.0, colors.accent.gamma_multiply(0.7)),
            _ => egui::Stroke::new(1.5, colors.main),
        };
        paint_border(&p, &layout, &g, *r, stroke);
    }
    if (layout.shape == Shape::Circle || curved) && !analog {
        p.add(egui::Shape::closed_line(area_outline(&layout, &g),
            egui::Stroke::new(1.0, colors.main.gamma_multiply(0.6))));
    }

    let nav_pass_now = crate::widgets::nav_pass(&ctx);
    ctx.data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_tz_targets", node_id.0, 0usize)), (nav_pass_now, nav_targets)));

    // ── Live point ──
    if let Some(pt) = point {
        let end = g.to_screen(pt.x.clamp(-1.2, 1.2), pt.y.clamp(-1.2, 1.2));
        paint_menu_cursor(&p, g.center, end, colors.hi);
    }

    // ── Interaction ──
    if hovered_border.is_some() || full_lit {
        ctx.set_cursor_icon(if dragging.is_some() || full_dragging { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }
    let mut edits: Vec<AreaEdit> = Vec::new();
    let mut new_full: Option<f32> = None;
    if resp.drag_started() {
        // Grab what was under the PRESS, not where the pointer has moved by the
        // time egui calls it a drag.
        let origin = ctx.input(|i| i.pointer.press_origin()).map(|p| from_global * p).or(hover_pos);
        if origin.is_some_and(near_full) {
            ctx.data_mut(|d| d.insert_temp(full_id, true));
        } else if let Some((r, hp)) = origin.and_then(|hp| under(hp).map(|r| (r, hp))) {
            if let Some(mut k) = border_key(&layout, r) {
                // Hold the border by where it was grabbed (a seam can sit off
                // its grid line), not snap it to the pointer.
                let (x, y) = g.to_area(hp);
                let mut off = layout.border(r).map(|b| b.pos).unwrap_or(0.0) - layout.border_coord_of(r, x, y);
                if layout.shape == Shape::Circle && matches!(r, BorderRef::Cut { .. }) {
                    off = (off + 0.5).rem_euclid(1.0) - 0.5;
                }
                k["off"] = json!(off);
                ctx.data_mut(|d| d.insert_temp(drag_id, k));
            }
            edits.push(AreaEdit::SelectBorder(Some(r)));
        }
    }
    let mut structural = false;
    if let (Some(r), Some(pos)) = (dragging, resp.interact_pointer_pos()) {
        if resp.dragged() {
            let (x, y) = g.to_area(pos);
            edits.push(AreaEdit::Move(r, layout.border_coord_of(r, x, y) + drag_offset));
        }
    }
    if resp.drag_stopped() && dragging.is_some() {
        ctx.data_mut(|d| d.remove::<Value>(drag_id));
        structural = true;
    }
    if full_dragging {
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
            let dz = if layout.has_centre_disc() { layout.edges[0].pos } else { 0.0 };
            new_full = Some(((pos - g.center).length() / g.unit).clamp(dz + 0.05, 1.0));
        }
        if resp.drag_stopped() {
            ctx.data_mut(|d| d.remove::<bool>(full_id));
            structural = true;
        }
    }
    let mut pick_dest: Option<u32> = None;
    if resp.clicked() {
        if let Some(hp) = resp.interact_pointer_pos() {
            match under(hp) {
                Some(r) if !picking => edits.push(AreaEdit::SelectBorder(Some(r))),
                _ => {
                    let id = cell_at(hp);
                    if picking {
                        pick_dest = Some(id);
                    } else {
                        edits.push(AreaEdit::Select(id));
                        edits.push(AreaEdit::SelectBorder(None));
                    }
                }
            }
        }
    }
    if resp.double_clicked() {
        if let Some(r) = hover_pos.and_then(under) {
            edits.push(AreaEdit::Recenter(r));
        }
    }
    // Context menu: what's under the pointer at the right-click.
    let ctx_id = host.with("ctx_menu");
    if resp.secondary_clicked() {
        if let Some(hp) = resp.interact_pointer_pos() {
            let (x, y) = g.to_area(hp);
            let target = under(hp).and_then(|r| border_key(&layout, r));
            ctx.data_mut(|d| d.insert_temp(ctx_id, (x, y, target)));
        }
    }
    resp.context_menu(|ui| {
        let Some((x, y, target)) = ui.ctx().data(|d| d.get_temp::<(f32, f32, Option<Value>)>(ctx_id)) else {
            return;
        };
        let band = layout.band_of(x, y);
        let v = layout.cut_coord(x, y);
        let (cut_noun, edge_noun) = match layout.shape {
            Shape::Circle => ("sector border", "ring border"),
            Shape::Rect => ("column border", "row border"),
        };
        if let Some(r) = target.as_ref().and_then(|k| border_from_key(&layout, k)) {
            let graded = layout.border(r).is_some_and(|b| b.gradient.is_some());
            if ui.button(if graded { "Make hard" } else { "Make gradient" }).clicked() {
                edits.push(AreaEdit::ToggleGradient(r));
                edits.push(AreaEdit::SelectBorder(Some(r)));
                ui.close();
            }
            if ui.button("Settings…").clicked() {
                edits.push(AreaEdit::SelectBorder(Some(r)));
                ui.close();
            }
            if ui.button("Centre between neighbours").clicked() {
                edits.push(AreaEdit::Recenter(r));
                ui.close();
            }
            if ui.button(format!("Remove {}", border_noun(layout.shape, r).to_lowercase())).clicked() {
                edits.push(AreaEdit::Remove(r));
                ui.close();
            }
        } else {
            if ui.button(format!("Add {cut_noun} here")).clicked() {
                edits.push(AreaEdit::AddCut(band, v));
                ui.close();
            }
            if ui.button(format!("Add {edge_noun} here")).clicked() {
                edits.push(AreaEdit::AddEdgeAt(x, y));
                ui.close();
            }
        }
    });

    // Resize grip, bottom-right (the node body only — a pin is sized by its frame).
    if resizable {
        let grip = egui::Rect::from_min_size(rect.right_bottom() - egui::vec2(12.0, 12.0), egui::vec2(12.0, 12.0));
        let gr = ui.interact(grip, host.with("grip"), egui::Sense::drag());
        let col = if gr.hovered() || gr.dragged() { colors.accent } else { colors.main.gamma_multiply(0.8) };
        for k in 0..3 {
            let o = 3.0 + k as f32 * 3.0;
            p.line_segment([grip.right_bottom() - egui::vec2(o, 1.0), grip.right_bottom() - egui::vec2(1.0, o)],
                egui::Stroke::new(1.0, col));
        }
        if gr.hovered() || gr.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe);
        }
        if gr.dragged() {
            let d = gr.drag_delta();
            let new = (size + d.x.max(d.y)).clamp(FIELD_MIN, FIELD_MAX);
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert("field_w".into(), json!(new));
            }
        }
        structural |= gr.drag_stopped();
    }

    if let (Some(f), Some(node)) = (new_full, snarl.get_node_mut(node_id)) {
        node.params.insert(AREA_LAYER_FULL_PARAM.into(), json!((f * 100.0).round() / 100.0));
        store_loaded_layer(node);
    }
    if !edits.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            let res = apply_area_edits(node, &edits);
            structural |= res.structural;
            if let (Some(_), Some(mut k)) = (dragging, res.moved_to.and_then(|r| border_key(&area_layout(node), r))) {
                k["off"] = json!(drag_offset);
                ctx.data_mut(|d| d.insert_temp(drag_id, k));
            }
        }
    }
    if let Some(dest) = pick_dest {
        tz_apply_pick(snarl, node_id, dest as usize);
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("sel_zone".into(), json!(dest));
        }
        structural = true;
    }
    (rect, structural)
}

// ── Options row ───────────────────────────────────────────────────────────────

/// The options row: input, shape, symmetric editing, pass-through, and the
/// selected cell's corner rounding (a rectangle) after the
/// selected cell's touch gate (for a stick). Field rects publish in that order
/// — the gamepad's field list for the `options` element mirrors it.
pub(crate) fn show_area_options_row(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> bool {
    let devs = remapper_upstream_device_ids(snarl, node_id, 0, automap_parent);
    let carried = |pin: &str| devs.iter().any(|d| live_signals.contains_key(&(d.clone(), pin.to_string())));
    let mut options: Vec<&str> = AREA_INPUTS.iter().copied()
        .filter(|p| carried(p) || carried(&format!("{p}_x")))
        .collect();
    let Some(n) = snarl.get_node(node_id) else { return false };
    let current = area_input(n);
    if options.is_empty() {
        options = AREA_INPUTS.to_vec();
    }
    if !options.contains(&current.as_str()) {
        // Keep the current pick listed even when its device is unplugged.
        if let Some(p) = AREA_INPUTS.iter().find(|p| **p == current) { options.push(p); }
    }
    let shape = area_layout(n).shape;
    let mut round = n.params.get(AREA_CELL_ROUND).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mut press = n.params.get(AREA_CELL_PRESSURE).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mut press_changed = false;
    let mut sym = area_symmetric(n);
    let mut pass = n.params.get(AREA_PASS_SOURCE_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut touched = n.params.get(AREA_CELL_TOUCH).and_then(|v| v.as_bool()).unwrap_or(false);
    let cell = selected_cell(n, &area_layout(n));
    let stick = !current.starts_with("touch");
    let touch_pin = area_touch_pin(&current);
    let touch_known = devs.iter().any(|d| live_signals.contains_key(&(d.clone(), touch_pin.clone())));
    let label_of = |p: &str| match p {
        "left_stick" => "Left stick",
        "right_stick" => "Right stick",
        "touch1" => "Touch 1",
        "touch2" => "Touch 2",
        _ => "?",
    };

    let mut picked: Option<String> = None;
    let mut new_shape: Option<Shape> = None;
    let (mut sym_changed, mut pass_changed, mut touch_changed, mut round_changed) = (false, false, false, false);
    let mut rects: Vec<egui::Rect> = Vec::new();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Input").small().weak());
        let r = egui::ComboBox::from_id_salt((node_id, "area_input"))
            .selected_text(egui::RichText::new(label_of(&current)).small())
            .width(88.0)
            .show_ui(ui, |ui| {
                for p in &options {
                    if ui.selectable_label(*p == current, label_of(p)).clicked() && *p != current {
                        picked = Some(p.to_string());
                    }
                }
            })
            .response
            .on_hover_text("The XY pair laid onto the area. It is consumed: the bus carries it on at rest unless Pass is on.");
        rects.push(r.rect);
        let r = egui::ComboBox::from_id_salt((node_id, "area_shape"))
            .selected_text(egui::RichText::new(if shape == Shape::Circle { "◯ Circle" } else { "▢ Rect" }).small())
            .width(72.0)
            .show_ui(ui, |ui| {
                for (s, name) in [(Shape::Circle, "◯ Circle"), (Shape::Rect, "▢ Rect")] {
                    if ui.selectable_label(shape == s, name).clicked() && shape != s {
                        new_shape = Some(s);
                    }
                }
            })
            .response
            .on_hover_text("Circular: rings cut into sectors. Rectangular: rows cut into columns. Switching starts the layout over; cards on the eight directions and the centre keep their cells.");
        rects.push(r.rect);

        let r = ui.checkbox(&mut sym, "Sym")
            .on_hover_text("Symmetric editing: a border placed in one quarter appears mirrored in all four, and they move together.");
        sym_changed = r.changed();
        rects.push(r.rect);
        let r = ui.checkbox(&mut pass, "Pass")
            .on_hover_text("Pass the input through as well as mapping it. Off: the bus carries it on at rest.");
        pass_changed = r.changed();
        rects.push(r.rect);
        // A touchpad point only exists while touched, so the gate is for sticks.
        if stick {
            let tip = if touch_known {
                format!("Cell {cell} only counts while the stick is touched (capacitive stick).")
            } else {
                format!("Cell {cell} only counts while the stick is touched. Needs a pad that reports stick touch — none connected does.")
            };
            let r = ui.add_enabled(touch_known || touched, egui::Checkbox::new(&mut touched, format!("Cell {cell} touched")))
                .on_hover_text(&tip)
                .on_disabled_hover_text(&tip);
            touch_changed = r.changed();
            rects.push(r.rect);
        }
        // A rectangle cell rounds its inner corners, up to a circle.
        if shape == Shape::Rect {
            let mut pct = round * 100.0;
            let r = ui.add(egui::DragValue::new(&mut pct).range(0.0..=100.0).speed(0.5)
                    .prefix(format!("Cell {cell} round ")).suffix("%"))
                .on_hover_text("Round this cell like a bubble: same area, up to a circle (an ellipse for an oblong cell). Past its sides it shares the overlap with its neighbours, or pushes into them with more pressure; it gives up its corners. Equal bubbles meet in flat seams.");
            if r.changed() {
                round = pct / 100.0;
                round_changed = true;
            }
            rects.push(r.rect);
            let mut pct = press * 100.0;
            let r = ui.add(egui::DragValue::new(&mut pct).range(0.0..=100.0).speed(0.5)
                    .prefix("pressure ").suffix("%"))
                .on_hover_text("Where this cell's shape overlaps another's, the higher-pressure one pushes into the lower; at equal pressure they share the overlap. A round cell at full pressure among unpressurised ones takes its whole circle.");
            if r.changed() {
                press = pct / 100.0;
                press_changed = true;
            }
            rects.push(r.rect);
        }
    });
    publish_nav_field_rects(ui, node_id, &rects);
    let preset_changed = ui.horizontal(|ui| presets_menu(node_id, ui, snarl)).inner;

    let Some(n) = snarl.get_node_mut(node_id) else { return false };
    let mut changed = preset_changed;
    if let Some(p) = picked {
        n.params.insert(AREA_INPUT_PARAM.into(), json!(p));
        changed = true;
    }
    if let Some(s) = new_shape {
        n.params.insert(AREA_SHAPE_PARAM.into(), json!(s.as_str()));
        changed = true;
    }
    if sym_changed {
        n.params.insert(AREA_SYM_PARAM.into(), json!(sym));
        changed = true;
    }
    if round_changed {
        n.params.insert(AREA_CELL_ROUND.into(), json!(round));
        changed = true;
    }
    if press_changed {
        n.params.insert(AREA_CELL_PRESSURE.into(), json!(press));
        changed = true;
    }
    if pass_changed {
        n.params.insert(AREA_PASS_SOURCE_PARAM.into(), json!(pass));
        changed = true;
    }
    if touch_changed {
        n.params.insert(AREA_CELL_TOUCH.into(), json!(touched));
        changed = true;
    }
    if changed {
        area_sync(n);
    }
    changed
}

// ── Layers row ────────────────────────────────────────────────────────────────

/// The layer tabs (click to edit one; + adds, ✕ removes the edited one), its
/// colour, and its analog mode. Field rects publish in the order the
/// gamepad's `layers` list mirrors: the tabs (as one "Layer" value), Analog,
/// then while analog its mix, its pulses, their time, the ramp (smooth) and
/// the midpoint. (The full zone is the ring on the field.)
pub(crate) fn show_area_layer_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let Some(n) = snarl.get_node(node_id) else { return false };
    let count = layer_count(n);
    let active = loaded_layer(n);
    let colors: Vec<[u8; 4]> = (0..count).map(|k| {
        if k == active {
            layer_color(|key| n.params.get(key).cloned(), k)
        } else {
            let l = n.params.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array()).and_then(|a| a.get(k)).cloned()
                .unwrap_or(json!({}));
            layer_color(|key| l.get(key).cloned(), k)
        }
    }).collect();
    let mut analog = n.params.get(AREA_LAYER_ANALOG_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut ms = n.params.get(AREA_LAYER_MS_PARAM).and_then(|v| v.as_f64()).unwrap_or(AREA_ANALOG_MS_DEFAULT as f64) as f32;
    let mut hold = n.params.get(AREA_LAYER_HOLD_PARAM).and_then(|v| v.as_f64()).unwrap_or(AREA_ANALOG_HOLD_DEFAULT as f64) as f32;
    let mut by_direction = n.params.get(AREA_LAYER_MIX_PARAM).and_then(|v| v.as_str()) != Some("keys");
    let mut smooth = n.params.get(AREA_LAYER_PULSE_PARAM).and_then(|v| v.as_str()) != Some("pwm");
    let (mix_before, pulse_before, hold_before) = (by_direction, smooth, hold);
    let mut ramp = n.params.get(AREA_LAYER_RAMP_PARAM).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mut mid = n.params.get(AREA_LAYER_MID_PARAM).and_then(|v| v.as_f64()).unwrap_or(0.5) as f32 * 100.0;
    let (mut ramp_changed, mut mid_changed) = (false, false);
    let mut pick: Option<usize> = None;
    let (mut add, mut remove) = (false, false);
    let mut new_color: Option<[u8; 4]> = None;
    let (mut analog_changed, mut ms_changed) = (false, false);
    let mut rects: Vec<egui::Rect> = Vec::new();
    ui.horizontal(|ui| {
        let tabs = ui.horizontal(|ui| {
            for (k, c) in colors.iter().enumerate() {
                let col = egui::Color32::from_rgb(c[0], c[1], c[2]);
                let text = egui::RichText::new(format!("● {}", k + 1)).color(if k == active { col } else { col.gamma_multiply(0.7) });
                if ui.selectable_label(k == active, text).on_hover_text(format!("Edit layer {}", k + 1)).clicked() && k != active {
                    pick = Some(k);
                }
            }
        }).response.rect;
        rects.push(tabs);
        if ui.small_button("+").on_hover_text("Add a layer: it acts on the same input alongside the others (e.g. an outer ring holding Shift whichever way you push).").clicked() {
            add = true;
        }
        if count > 1 && ui.small_button("✕").on_hover_text("Remove this layer and its mappings.").clicked() {
            remove = true;
        }
        let c = colors[active];
        let mut c32 = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
        if egui::widgets::color_picker::color_edit_button_srgba(ui, &mut c32, egui::widgets::color_picker::Alpha::OnlyBlend)
            .on_hover_text("This layer's colour.").changed()
        {
            let [r, g, b, a] = c32.to_srgba_unmultiplied();
            new_color = Some([r, g, b, a]);
        }
        let r = ui.checkbox(&mut analog, "Analog")
            .on_hover_text("Drive this layer's keys from the stick's deflection instead of its borders: past the centre (the deadzone) the keys pulse so the game moves the way and as far as you push. Borders and gradients don't apply. Best on a 4-way circle with one key per direction.");
        analog_changed = r.changed();
        rects.push(r.rect);
    });
    // The analog settings, on a row of their own.
    if analog {
        ui.horizontal(|ui| {
            let r = egui::ComboBox::from_id_salt(("area_mix", node_id))
                .selected_text(if by_direction { "Direction" } else { "Per key" })
                .width(80.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut by_direction, true, "Direction")
                        .on_hover_text("Steer between a key and the diagonal (W ↔ W+D): a full push always walks, only its direction pulses. For games that walk every way at the same speed — most of them.");
                    ui.selectable_value(&mut by_direction, false, "Per key")
                        .on_hover_text("Each key carries its own axis (W at the push up, D at the push right). For games whose diagonal is faster than straight.");
                }).response;
            rects.push(r.rect);
            let r = egui::ComboBox::from_id_salt(("area_pulse", node_id))
                .selected_text(if smooth { "Smooth" } else { "Fixed PWM" })
                .width(86.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut smooth, true, "Smooth")
                        .on_hover_text("Every press and gap at least the minimum, as often as the push allows; a change of push shows at once. A light push gives short taps far apart instead of presses too short for the game to see.");
                    ui.selectable_value(&mut smooth, false, "Fixed PWM")
                        .on_hover_text("One press per fixed period, its length the push. Short pushes can make presses shorter than a frame, which games miss.");
                }).response;
            rects.push(r.rect);
            let r = if smooth {
                ui.add(egui::DragValue::new(&mut hold).range(1.0..=500.0).speed(1.0).prefix("Min ").suffix(" ms"))
                    .on_hover_text("The shortest press or gap. Make it at least two of the game's frames (≈ 34 ms at 60 fps, 14 ms at 144) so it reads every one; longer if the game ignores short taps.")
            } else {
                ui.add(egui::DragValue::new(&mut ms).range(1.0..=1000.0).speed(1.0).prefix("PWM ").suffix(" ms"))
                    .on_hover_text("The pulse period. Games read keys once a frame, so keep it a few frames long (≈ 50–100 ms at 60 fps): shorter pulses get sampled at random and feel jittery.")
            };
            ms_changed = r.changed();
            rects.push(r.rect);
            if smooth {
                let r = ui.add(egui::DragValue::new(&mut ramp).range(0.0..=1000.0).speed(2.0).prefix("Ramp ").suffix(" ms"))
                    .on_hover_text("How long the game takes to get from standing to full speed (and back). Set it to match and the presses are timed against the game's own acceleration, holding its speed at your push instead of stopping and starting. Raise it until the movement stops stuttering; if the speed then wanders or lags, it's too high. 0 = the game moves at full speed at once.");
                ramp_changed = r.changed();
                rects.push(r.rect);
            }
            let r = ui.add(egui::DragValue::new(&mut mid).range(5.0..=95.0).speed(0.5).prefix("Half at ").suffix("%"))
                .on_hover_text("How far you push for the keys to hold half the time. Below 50% light pushes hold longer (faster, fewer gaps at low deflection); above, finer control near the centre.");
            mid_changed = r.changed();
            rects.push(r.rect);
        });
    }
    publish_nav_field_rects(ui, node_id, &rects);

    let Some(n) = snarl.get_node_mut(node_id) else { return false };
    let mut changed = false;
    if let Some(k) = pick {
        n.params.insert(AREA_LAYER_PARAM.into(), json!(k));
        area_sync(n);
    }
    if add {
        add_layer(n);
        changed = true;
    }
    if remove {
        remove_layer(n, active);
        changed = true;
    }
    if let Some(c) = new_color {
        n.params.insert(AREA_LAYER_COLOR_PARAM.into(), json!(c));
        changed = true;
    }
    if analog_changed {
        n.params.insert(AREA_LAYER_ANALOG_PARAM.into(), json!(analog));
        changed = true;
    }
    if ms_changed {
        n.params.insert(AREA_LAYER_MS_PARAM.into(), json!(ms));
        changed = true;
    }
    if by_direction != mix_before {
        n.params.insert(AREA_LAYER_MIX_PARAM.into(), json!(if by_direction { "direction" } else { "keys" }));
        changed = true;
    }
    if smooth != pulse_before {
        n.params.insert(AREA_LAYER_PULSE_PARAM.into(), json!(if smooth { "smooth" } else { "pwm" }));
        changed = true;
    }
    if hold != hold_before {
        n.params.insert(AREA_LAYER_HOLD_PARAM.into(), json!(hold));
        changed = true;
    }
    if ramp_changed {
        n.params.insert(AREA_LAYER_RAMP_PARAM.into(), json!(ramp));
        changed = true;
    }
    if mid_changed {
        n.params.insert(AREA_LAYER_MID_PARAM.into(), json!(mid / 100.0));
        changed = true;
    }
    if changed {
        area_sync(n);
    }
    changed
}

// ── Press row ─────────────────────────────────────────────────────────────────

fn press_mode_label(m: &str) -> &'static str {
    match m {
        "short" => "Short",
        "long" => "Long",
        "double" => "Double",
        "on_press" => "On press",
        "on_release" => "On release",
        _ => "Down",
    }
}

/// One press mode for every mapping: when on, its mode, time, Hold and Turbo
/// rule every card of every layer. Field rects publish in the order the
/// gamepad's `press` list mirrors: the switch, then Mode, Time, Hold, Turbo.
pub(crate) fn show_area_press_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let Some(n) = snarl.get_node(node_id) else { return false };
    let mut lock = n.params.get(AREA_PRESS_LOCK_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut mode = n.params.get(AREA_PRESS_MODE_PARAM).and_then(|v| v.as_str()).unwrap_or("down").to_string();
    let mut ms = n.params.get(AREA_PRESS_MS_PARAM).and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
    let mut hold = n.params.get(AREA_PRESS_HOLD_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut turbo = n.params.get(AREA_PRESS_TURBO_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut changed = false;
    let mut rects: Vec<egui::Rect> = Vec::new();
    ui.horizontal(|ui| {
        let r = ui.checkbox(&mut lock, "One press mode for all")
            .on_hover_text("Rule every mapping's press mode from here, instead of card by card.");
        changed |= r.changed();
        rects.push(r.rect);
        if lock {
            let r = egui::ComboBox::from_id_salt((node_id, "area_press_mode"))
                .selected_text(press_mode_label(&mode))
                .width(78.0)
                .show_ui(ui, |ui| {
                    for m in AREA_PRESS_MODES {
                        if ui.selectable_label(mode == *m, press_mode_label(m)).clicked() && mode != *m {
                            mode = m.to_string();
                            changed = true;
                        }
                    }
                })
                .response;
            rects.push(r.rect);
            let r = ui.add(egui::DragValue::new(&mut ms).range(1.0..=5000.0).speed(2.0).suffix(" ms"))
                .on_hover_text("The press mode's time window (pulse length for On press / On release, Enter / Leave).");
            changed |= r.changed();
            rects.push(r.rect);
            let r = ui.checkbox(&mut hold, "Hold");
            changed |= r.changed();
            rects.push(r.rect);
            let r = ui.checkbox(&mut turbo, "Turbo");
            changed |= r.changed();
            rects.push(r.rect);
        }
    });
    publish_nav_field_rects(ui, node_id, &rects);
    if changed {
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert(AREA_PRESS_LOCK_PARAM.into(), json!(lock));
            n.params.insert(AREA_PRESS_MODE_PARAM.into(), json!(mode));
            n.params.insert(AREA_PRESS_MS_PARAM.into(), json!(ms));
            n.params.insert(AREA_PRESS_HOLD_PARAM.into(), json!(hold));
            n.params.insert(AREA_PRESS_TURBO_PARAM.into(), json!(turbo));
            area_sync(n);
        }
    }
    changed
}

// ── Presets ───────────────────────────────────────────────────────────────────

/// The Presets menu: built-in layouts for the edited layer, and saving /
/// loading the whole module as a `.fxarea` file.
fn presets_menu(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let mut changed = false;
    ui.menu_button("Presets", |ui| {
        let mut layout: Option<AreaLayout> = None;
        if ui.button("Circle: 4-way").clicked() { layout = Some(AreaLayout::circle_ways(4)); }
        if ui.button("Circle: 8-way").clicked() { layout = Some(AreaLayout::circle_ways(8)); }
        if ui.button("Circle: inner / outer").clicked() { layout = Some(AreaLayout::circle_rings(0.7)); }
        if ui.button("Rectangle: 3×3").clicked() { layout = Some(AreaLayout::rect_grid()); }
        if let Some(l) = layout {
            if let Some(n) = snarl.get_node_mut(node_id) {
                apply_preset(n, &l);
                changed = true;
            }
            ui.close();
        }
        ui.separator();
        if ui.button("Save…").on_hover_text("Save the whole module — every layer, its mappings and settings.").clicked() {
            if let Some(n) = snarl.get_node_mut(node_id) {
                let preset = module_preset(n);
                if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                    rfd::FileDialog::new()
                        .add_filter("FlexInput Area", &["fxarea"])
                        .set_file_name("area.fxarea")
                        .save_file()
                }) {
                    if let Ok(json) = serde_json::to_string_pretty(&preset) {
                        let _ = std::fs::write(path, json);
                    }
                }
            }
            ui.close();
        }
        if ui.button("Load…").on_hover_text("Replace this module's layers, mappings and settings with a saved file.").clicked() {
            let preset: Option<Value> = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new().add_filter("FlexInput Area", &["fxarea"]).pick_file()
            })
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok());
            if let (Some(preset), Some(n)) = (preset, snarl.get_node_mut(node_id)) {
                changed |= load_module_preset(n, &preset);
            }
            ui.close();
        }
    }).response.on_hover_text("Layouts for this layer, and saving / loading the whole module.");
    changed
}

// ── Gradient settings ─────────────────────────────────────────────────────────

/// The selected border's settings: hard / gradient, and the gradient's width,
/// curve and key behaviour. `with_curve` adds the curve editor under the preset
/// (the node body; a pin keeps to the rows). Field rects publish in the order
/// the gamepad's `border` field list mirrors: Gradient, Square and Round
/// corners (a circle's ring border), Width, Curve, Keys,
/// then Period or Threshold, then Phase for PWM. Returns whether anything
/// changed.
fn border_settings(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, colors: ZoneColors, with_curve: bool) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let mut layout = area_layout(node);
    let sym = area_symmetric(node);
    let Some(r) = selected_border(node, &layout) else {
        ui.label(egui::RichText::new("No border selected — click one in the field.").small().weak());
        return false;
    };
    let Some(border) = layout.border(r).cloned() else { return false };
    let mirrored = if sym { layout.mirror_set(r).len() } else { 1 };
    let mut changed = false;
    let mut close = false;
    let mut grad = border.gradient.clone();
    let mut rects: Vec<egui::Rect> = Vec::new();

    ui.horizontal(|ui| {
        let mut title = border_noun(layout.shape, r).to_string();
        if mirrored > 1 {
            title.push_str(&format!(" ×{mirrored}"));
        }
        ui.label(egui::RichText::new(title).strong().color(colors.accent));
        let mut on = grad.is_some();
        let resp = ui.checkbox(&mut on, "Gradient")
            .on_hover_text("Crossfade the cells on either side of this border instead of switching at it.");
        rects.push(resp.rect);
        if resp.changed() {
            grad = on.then(|| Gradient::with_width(default_gradient_width(&layout, r)));
            changed = true;
        }
        if ui.small_button("✕").on_hover_text("Close").clicked() {
            close = true;
        }
    });
    let mut new_square: Option<f32> = None;
    let mut new_corners: Option<bool> = None;
    if let (Shape::Circle, BorderRef::Edge(_)) = (layout.shape, r) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Square").small().weak());
            let mut pct = border.square * 100.0;
            let resp = ui.add(egui::DragValue::new(&mut pct).range(0.0..=100.0).speed(0.5).suffix("%"))
                .on_hover_text("Take this ring from a circle (0%) to a square (100%) — e.g. a square deadzone. It stops short where it would touch the next ring.");
            rects.push(resp.rect);
            if resp.changed() {
                new_square = Some(pct / 100.0);
            }
            let mut corners = border.corners;
            let resp = ui.checkbox(&mut corners, "Round corners")
                .on_hover_text("On: a square with its corners rounded off — the corner radius shrinks from the whole circle (0%) to nothing (100%). Off: the circle and the square blended.");
            rects.push(resp.rect);
            if resp.changed() {
                new_corners = Some(corners);
            }
        });
    }
    if let Some(g) = grad.as_mut() {
        ui.horizontal(|ui| {
            // Width, in the border's own units: degrees for a sector border,
            // else a share of the area's half-width.
            ui.label(egui::RichText::new("Width").small().weak());
            let deg = border_width_in_degrees(layout.shape, r);
            let (scale, suffix, max) = if deg { (360.0, "°", 120.0) } else { (100.0, "%", 100.0) };
            let mut shown = g.width * scale;
            let resp = ui.add(egui::DragValue::new(&mut shown).range(1.0..=max).speed(0.5).suffix(suffix))
                .on_hover_text("How wide the crossfade is, centred on the border.");
            rects.push(resp.rect);
            if resp.changed() {
                g.width = shown / scale;
                changed = true;
            }
            ui.label(egui::RichText::new("Curve").small().weak());
            let preset = curve_preset_of(&g.curve);
            let resp = egui::ComboBox::from_id_salt((node_id, "area_grad_curve_preset"))
                .selected_text(curve_preset_label(preset))
                .width(72.0)
                .show_ui(ui, |ui| {
                    for name in CURVE_PRESETS.iter().filter(|n| **n != "custom") {
                        if ui.selectable_label(preset == *name, curve_preset_label(name)).clicked() && preset != *name {
                            g.curve = curve_preset_points(name).unwrap_or_default();
                            changed = true;
                        }
                    }
                })
                .response
                .on_hover_text("How the far cell's share grows across the band (the near cell gets the rest).");
            rects.push(resp.rect);
        });
        if with_curve {
            let mut pts = if g.curve.is_empty() { identity_curve() } else { g.curve.clone() };
            if mapping_curve_editor(
                ui, egui::Id::new((node_id, "area_grad_curve")), &mut pts, None, None,
                colors.accent, &ui.visuals().clone(), None, None, false, None,
            ) {
                g.curve = if curve_preset_of(&pts) == "linear" { Vec::new() } else { pts };
                changed = true;
            }
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Keys").small().weak());
            let label = |m: DigitalMode| match m {
                DigitalMode::Pwm => "PWM",
                DigitalMode::Taps => "Tap train",
                DigitalMode::Threshold => "Threshold",
            };
            let resp = egui::ComboBox::from_id_salt((node_id, "area_grad_dig"))
                .selected_text(label(g.digital))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for m in [DigitalMode::Pwm, DigitalMode::Taps, DigitalMode::Threshold] {
                        if ui.selectable_label(g.digital == m, label(m)).clicked() && g.digital != m {
                            g.digital = m;
                            changed = true;
                        }
                    }
                })
                .response
                .on_hover_text("How a key or button in a fading cell is driven. PWM: pulses with duty = the cell's share. Tap train: taps faster as the share grows. Threshold: held once the share reaches a level. Analog outputs always take the share itself.");
            rects.push(resp.rect);
            if g.digital == DigitalMode::Threshold {
                let mut t = g.threshold;
                let resp = ui.add(egui::DragValue::new(&mut t).range(0.0..=1.0).speed(0.01).fixed_decimals(2))
                    .on_hover_text("The share at which the key holds.");
                rects.push(resp.rect);
                if resp.changed() {
                    g.threshold = t;
                    changed = true;
                }
            } else {
                let mut ms = g.period_ms;
                let tip = if g.digital == DigitalMode::Pwm { "PWM period." } else { "Slowest tap period (at the smallest share)." };
                let resp = ui.add(egui::DragValue::new(&mut ms).range(1.0..=1000.0).speed(1.0).suffix(" ms"))
                    .on_hover_text(tip);
                rects.push(resp.rect);
                if resp.changed() {
                    g.period_ms = ms;
                    changed = true;
                }
            }
            if g.digital == DigitalMode::Pwm {
                let name = |ph: Phase| if ph == Phase::Alternate { "Alternate" } else { "Independent" };
                let resp = egui::ComboBox::from_id_salt((node_id, "area_grad_phase"))
                    .selected_text(name(g.phase))
                    .width(84.0)
                    .show_ui(ui, |ui| {
                        for ph in [Phase::Alternate, Phase::Independent] {
                            if ui.selectable_label(g.phase == ph, name(ph)).clicked() && g.phase != ph {
                                g.phase = ph;
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Alternate: the cells take turns within one period, each for its share — never both at once, so two keys emulate one analog direction. Independent: each cell pulses on its own clock; pulses may overlap.");
                rects.push(resp.rect);
            }
        });
    }
    publish_nav_field_rects(ui, node_id, &rects);

    let Some(node) = snarl.get_node_mut(node_id) else { return false };
    if close {
        node.params.remove(SEL_BORDER);
    }
    if changed {
        layout.set_gradient(r, grad, sym);
    }
    if let (Some(rc), BorderRef::Edge(e)) = (new_corners, r) {
        layout.set_edge_corners(e, rc);
        changed = true;
    }
    if let (Some(sq), BorderRef::Edge(e)) = (new_square, r) {
        layout.set_edge_square(e, sq);
        changed = true;
    }
    if changed {
        set_layout(node, &layout);
    }
    changed
}

// ── Selected cell ─────────────────────────────────────────────────────────────

/// The selected cell's icon + name override.
fn cell_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let layout = area_layout(node);
    let cell = selected_cell(node, &layout);
    let meta = menu_zone_meta(node).get(&cell).cloned().unwrap_or_default();
    let mut new_meta: Option<ZoneMeta> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("Cell {cell}")).small().strong());
        ui.label(egui::RichText::new("icon").small().weak())
            .on_hover_text("Show one picked icon on this cell instead of its mapping icons.");
        if let Some((k, s)) = icon_picker_button(ui, egui::Id::new((node_id, "area_cell_icon", cell)), &meta.icon, &meta.svg) {
            new_meta = Some(ZoneMeta { icon: k, svg: s, label: meta.label.clone() });
        }
        ui.label(egui::RichText::new("name").small().weak());
        let mut label = meta.label.clone();
        if ui.add(egui::TextEdit::singleline(&mut label).desired_width(80.0)
            .id_salt((node_id, "area_cell_name", cell))).changed()
        {
            new_meta = Some(ZoneMeta { icon: meta.icon.clone(), svg: meta.svg.clone(), label });
        }
    });
    let Some(node) = snarl.get_node_mut(node_id) else { return false };
    if let Some(m) = &new_meta {
        write_zone_meta_entry(node, cell, m);
    }
    new_meta.is_some()
}

// ── Body + pins ───────────────────────────────────────────────────────────────

fn sync_and_colors(snarl: &mut Snarl<NodeData>, node_id: NodeId) -> ZoneColors {
    if let Some(node) = snarl.get_node_mut(node_id) {
        area_sync(node);
    }
    snarl.get_node(node_id).map(|n| {
        let k = loaded_layer(n);
        ZoneColors::build(layer_color(|key| n.params.get(key).cloned(), k), super::menu_body::MENU_HIGHLIGHT_DEFAULT)
    }).unwrap_or_else(ZoneColors::fallback)
}

/// The node body. Returns whether an undo step should be pushed.
pub(crate) fn show_area_mapper_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> bool {
    let colors = sync_and_colors(snarl, node_id);
    let visuals = ui.visuals().clone();
    let size = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_w").and_then(|v| v.as_f64()))
        .unwrap_or(FIELD_DEFAULT as f64) as f32;
    let mut changed = false;

    ui.vertical(|ui| {
        let opts = ui.vertical(|ui| {
            changed |= show_area_options_row(node_id, ui, snarl, live_signals, automap_parent);
        }).response.rect;
        register_exposable_element(ui, node_id, "options", opts);
        let layers = ui.vertical(|ui| {
            changed |= show_area_layer_row(node_id, ui, snarl);
        }).response.rect;
        register_exposable_element(ui, node_id, "layers", layers);
        let (field, structural) = area_field(node_id, ui, snarl, colors, size.clamp(FIELD_MIN, FIELD_MAX), true);
        changed |= structural;
        register_exposable_element(ui, node_id, "field", field);
        ui.label(egui::RichText::new("Drag a border to move it, double-click to centre it · click a border for its gradient · right-click to add or remove")
            .small().weak());
        let border = egui::Frame::group(ui.style()).show(ui, |ui| {
            changed |= border_settings(node_id, ui, snarl, colors, true);
        }).response.rect;
        register_exposable_element(ui, node_id, "border", border);
        changed |= cell_row(node_id, ui, snarl);
        let press = ui.vertical(|ui| {
            changed |= show_area_press_row(node_id, ui, snarl);
        }).response.rect;
        register_exposable_element(ui, node_id, "press", press);
        ui.add_space(4.0);
        let cards = ui.vertical(|ui| {
            render_touch_zone_cards(node_id, ui, snarl, &visuals, colors.accent, live_signals, automap_parent);
        }).response.rect;
        register_exposable_element(ui, node_id, "cards", cards);
    });
    changed
}

/// The area field pinned: the largest square that fits the frame, with the
/// same editing as the node body.
pub(crate) fn render_area_field_pinned(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let colors = sync_and_colors(snarl, inner_id);
    let (rect, _) = ui.allocate_exact_size(container, egui::Sense::hover());
    let size = container.x.min(container.y).max(40.0);
    let mut child = ui.new_child(egui::UiBuilder::new()
        .max_rect(egui::Rect::from_center_size(rect.center(), egui::vec2(size, size))));
    area_field(inner_id, &mut child, snarl, colors, size, false);
}

/// The options row pinned.
pub(crate) fn render_area_options_pinned(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    sync_and_colors(snarl, inner_id);
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(420.0, 22.0));
    show_area_options_row(inner_id, ui, snarl, live_signals, automap_parent);
}

/// The layer tabs pinned.
pub(crate) fn render_area_layers_pinned(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    sync_and_colors(snarl, inner_id);
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(300.0, 22.0));
    show_area_layer_row(inner_id, ui, snarl);
}

/// The one-press-mode row pinned.
pub(crate) fn render_area_press_pinned(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    sync_and_colors(snarl, inner_id);
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(360.0, 22.0));
    show_area_press_row(inner_id, ui, snarl);
}

/// The selected border's settings pinned (rows only; the curve editor stays
/// in the node body, the preset is here).
pub(crate) fn render_area_border_pinned(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let colors = sync_and_colors(snarl, inner_id);
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(320.0, 70.0));
    ui.vertical(|ui| {
        border_settings(inner_id, ui, snarl, colors, false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> NodeData {
        let d = flexinput_modules::area::registrations().remove(0).descriptor;
        let mut n = NodeData::from(&d);
        area_sync(&mut n);
        n
    }

    /// A click in an analog layer's view picks the deadzone or the nearest
    /// direction's mapping, wherever the cuts are.
    #[test]
    fn an_analog_click_picks_the_nearest_direction() {
        let l = AreaLayout::circle_ways(4);
        let g = FieldGeom::of(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(212.0, 212.0)));
        assert_eq!(analog_cell_at(&l, &g, g.to_screen(0.05, 0.1)), 0);
        assert_eq!(analog_cell_at(&l, &g, g.to_screen(0.9, 0.2)), 3);
        assert_eq!(analog_cell_at(&l, &g, g.to_screen(-0.1, -0.95)), 5);
        // A new analog layer mixes by direction with smooth pulses.
        let n = node();
        assert_eq!(n.params.get(AREA_LAYER_MIX_PARAM), Some(&json!("direction")));
        assert_eq!(n.params.get(AREA_LAYER_PULSE_PARAM), Some(&json!("smooth")));
    }

    fn select_border(n: &mut NodeData, r: BorderRef) {
        apply_area_edits(n, &[AreaEdit::SelectBorder(Some(r))]);
        area_sync(n);
    }

    #[test]
    fn first_sync_fills_in_the_defaults() {
        let n = node();
        assert_eq!(area_layout(&n), AreaLayout::default_for(Shape::Circle));
        assert_eq!(n.params.get(AREA_SHAPE_PARAM), Some(&json!("circle")));
        assert_eq!(n.params.get(AREA_SYM_PARAM), Some(&json!(true)));
    }

    #[test]
    fn the_shape_param_switches_the_layout_and_keeps_direction_cards() {
        let mut n = node();
        n.params.insert("zone_maps".into(), json!([{ "z": 1, "out": ["key_w"] }, { "z": 42, "out": ["key_q"] }]));
        n.params.insert(AREA_SHAPE_PARAM.into(), json!("rect"));
        area_sync(&mut n);
        assert_eq!(area_layout(&n).shape, Shape::Rect);
        let cards = n.params["zone_maps"].as_array().unwrap();
        assert_eq!(cards.len(), 1, "the card on a cell the rect lacks is gone");
        assert_eq!(cards[0]["z"], json!(1));
    }

    #[test]
    fn a_gradient_mirror_edit_reaches_the_layout_and_its_mirror_set() {
        let mut n = node();
        let r = BorderRef::Cut { band: 1, cut: 0 };
        select_border(&mut n, r);
        assert_eq!(n.params.get(AREA_G_ON), Some(&json!(false)));
        // The pad turns it on, then picks Threshold.
        n.params.insert(AREA_G_ON.into(), json!(true));
        area_sync(&mut n);
        let l = area_layout(&n);
        assert_eq!(l.bands[1].cuts.iter().filter(|c| c.gradient.is_some()).count(), 4, "symmetric");
        n.params.insert(AREA_G_KEYS.into(), json!("threshold"));
        n.params.insert(AREA_G_THR.into(), json!(0.7));
        area_sync(&mut n);
        let g = area_layout(&n).border(r).unwrap().gradient.clone().unwrap();
        assert_eq!(g.digital, DigitalMode::Threshold);
        assert!((g.threshold - 0.7).abs() < 1e-6);
    }

    #[test]
    fn a_mouse_edit_to_the_layout_shows_up_in_the_mirrors() {
        let mut n = node();
        let r = BorderRef::Edge(0);
        select_border(&mut n, r);
        // The mouse panel writes the layout directly.
        let mut l = area_layout(&n);
        let mut g = Gradient::with_width(0.2);
        g.curve = curve_preset_points("s_curve").unwrap();
        l.set_gradient(r, Some(g), true);
        set_layout(&mut n, &l);
        area_sync(&mut n);
        assert_eq!(n.params.get(AREA_G_ON), Some(&json!(true)));
        assert_eq!(n.params.get(AREA_G_CURVE), Some(&json!("s_curve")));
        // And a sync with nothing edited changes nothing.
        let before = n.params.clone();
        area_sync(&mut n);
        assert_eq!(n.params, before);
    }

    #[test]
    fn a_curve_preset_from_the_pad_replaces_the_curve() {
        let mut n = node();
        let r = BorderRef::Edge(0);
        select_border(&mut n, r);
        n.params.insert(AREA_G_ON.into(), json!(true));
        area_sync(&mut n);
        n.params.insert(AREA_G_CURVE.into(), json!("ease_in"));
        area_sync(&mut n);
        let g = area_layout(&n).border(r).unwrap().gradient.clone().unwrap();
        assert_eq!(g.curve, curve_preset_points("ease_in").unwrap());
    }

    #[test]
    fn the_cell_touch_mirror_follows_the_selected_cell() {
        let mut n = node();
        apply_area_edits(&mut n, &[AreaEdit::Select(3)]);
        area_sync(&mut n);
        n.params.insert(AREA_CELL_TOUCH.into(), json!(true));
        area_sync(&mut n);
        assert_eq!(touch_cells(&n), vec![3]);
        // Selecting another cell shows ITS gate, without carrying the edit over.
        apply_area_edits(&mut n, &[AreaEdit::Select(4)]);
        area_sync(&mut n);
        assert_eq!(n.params.get(AREA_CELL_TOUCH), Some(&json!(false)));
        assert_eq!(touch_cells(&n), vec![3]);
    }

    #[test]
    fn a_nudge_moves_a_border_and_follows_it() {
        let mut n = node();
        let res = apply_area_edits(&mut n, &[AreaEdit::Nudge(BorderRef::Edge(0), 0.1)]);
        assert!((area_layout(&n).edges[0].pos - 0.4).abs() < 1e-5);
        assert_eq!(res.moved_to, Some(BorderRef::Edge(0)));
    }

    #[test]
    fn the_square_mirror_squares_a_ring() {
        let mut n = node();
        select_border(&mut n, BorderRef::Edge(0));
        assert_eq!(n.params.get(AREA_G_SQ), Some(&json!(0.0)));
        n.params.insert(AREA_G_SQ.into(), json!(1.0));
        area_sync(&mut n);
        assert_eq!(area_layout(&n).edges[0].square, 1.0);
    }

    #[test]
    fn the_corners_mirror_switches_how_a_ring_squares() {
        let mut n = node();
        select_border(&mut n, BorderRef::Edge(0));
        n.params.insert(AREA_G_SQ.into(), json!(0.5));
        area_sync(&mut n);
        n.params.insert(AREA_G_RC.into(), json!(true));
        area_sync(&mut n);
        let e = &area_layout(&n).edges[0];
        assert!(e.corners && (e.square - 0.5).abs() < 1e-6, "{e:?}");
    }

    #[test]
    fn the_cell_round_mirror_rounds_the_selected_cell_and_its_mirrors() {
        let mut n = node();
        n.params.insert(AREA_SHAPE_PARAM.into(), json!("rect"));
        area_sync(&mut n);
        apply_area_edits(&mut n, &[AreaEdit::Select(1)]);
        area_sync(&mut n);
        n.params.insert(AREA_CELL_ROUND.into(), json!(0.8));
        area_sync(&mut n);
        let l = area_layout(&n);
        assert_eq!(l.cell_round_of(1), 0.8);
        assert_eq!(l.cell_round_of(5), 0.8, "symmetric: the bottom-middle too");
        // Another cell shows its own rounding.
        apply_area_edits(&mut n, &[AreaEdit::Select(0)]);
        area_sync(&mut n);
        assert_eq!(n.params.get(AREA_CELL_ROUND), Some(&json!(0.0)));
    }

    #[test]
    fn the_pressure_mirror_pressurises_the_selected_cell() {
        let mut n = node();
        n.params.insert(AREA_SHAPE_PARAM.into(), json!("rect"));
        area_sync(&mut n);
        n.params.insert(AREA_CELL_ROUND.into(), json!(1.0));
        n.params.insert(AREA_CELL_PRESSURE.into(), json!(1.0));
        area_sync(&mut n);
        let l = area_layout(&n);
        assert_eq!(l.cell_pressure_of(0), 1.0);
        // The middle now owns its whole circle.
        assert_eq!(l.locate_id(0.37, 0.0), 0);
    }

    #[test]
    fn layers_swap_in_and_keep_their_own_layout_and_cards() {
        let mut n = node();
        n.params.insert("zone_maps".into(), json!([{ "z": 1, "out": ["key_w"] }]));
        add_layer(&mut n);
        assert_eq!(layer_count(&n), 2);
        assert_eq!(loaded_layer(&n), 1);
        assert_eq!(area_layout(&n), AreaLayout::circle_rings(0.7), "the new layer is loaded");
        assert_eq!(n.params.get("sel_field"), Some(&json!(1)), "the card list follows the layer");
        // Edit layer 1, switch back: layer 0 is as it was, layer 1 kept.
        apply_preset(&mut n, &AreaLayout::circle_ways(8));
        n.params.insert(AREA_LAYER_PARAM.into(), json!(0));
        area_sync(&mut n);
        assert_eq!(area_layout(&n), AreaLayout::default_for(Shape::Circle));
        let stored = n.params[AREA_LAYERS_PARAM][1][AREA_LAYOUT_PARAM].clone();
        assert_eq!(AreaLayout::from_value(&stored), Some(AreaLayout::circle_ways(8)));
        // A preset on layer 0 keeps layer 0's card, never another layer's.
        assert_eq!(n.params["zone_maps"].as_array().unwrap().len(), 1);
        // Removing layer 1 drops its cards and keeps layer 0's.
        n.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()).unwrap().push(json!({ "f": 1, "z": 1, "out": ["key_shift"] }));
        remove_layer(&mut n, 1);
        assert_eq!(layer_count(&n), 1);
        assert_eq!(n.params["zone_maps"].as_array().unwrap().len(), 1);
        assert_eq!(n.params["zone_maps"][0]["out"], json!(["key_w"]));
    }

    #[test]
    fn a_module_preset_round_trips() {
        let mut a = node();
        add_layer(&mut a);
        a.params.insert("zone_maps".into(), json!([{ "f": 1, "z": 1, "out": ["key_shift"] }]));
        let preset = module_preset(&mut a);
        let mut b = node();
        assert!(load_module_preset(&mut b, &preset));
        assert_eq!(layer_count(&b), 2);
        assert_eq!(b.params["zone_maps"], a.params["zone_maps"]);
        assert_eq!(loaded_layer(&b), 0);
    }

    #[test]
    fn one_press_mode_rules_every_card() {
        let mut n = node();
        n.params.insert("zone_maps".into(), json!([{ "z": 1, "out": ["key_w"], "mode": "long" }]));
        n.params.insert(AREA_PRESS_LOCK_PARAM.into(), json!(true));
        n.params.insert(AREA_PRESS_MODE_PARAM.into(), json!("double"));
        area_sync(&mut n);
        assert_eq!(n.params["zone_maps"][0]["mode"], json!("double"));
    }

    #[test]
    fn removing_a_border_carries_the_cards_over() {
        let mut n = node();
        n.params.insert("zone_maps".into(), json!([{ "z": 1, "out": ["key_w"] }]));
        // Cut 0 (22.5°) separates up (1) and up-right (2); symmetric removal.
        apply_area_edits(&mut n, &[AreaEdit::Remove(BorderRef::Cut { band: 1, cut: 0 })]);
        let l = area_layout(&n);
        let z = n.params["zone_maps"][0]["z"].as_u64().unwrap() as u32;
        assert!(l.cell_ids().contains(&z), "the card sits on a cell that exists");
    }
}
