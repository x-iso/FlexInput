//! Graph chrome: the Add-module menu, channel/graph colors, shared palettes.

use super::*;

pub(crate) enum WireDir {
    FromOutput { src: OutPinId, from_type: SignalType },
    FromInput  { dst: InPinId,  to_type:   SignalType },
}

pub(crate) fn show_module_menu(
    pos: egui::Pos2,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    descriptors: &[ModuleDescriptor],
    wire: Option<WireDir>,
    is_inner_canvas: bool,
) {
    let mut categories: Vec<&str> = vec![];
    for d in descriptors {
        // Inlet/Outlet nodes (category "SubPatch") are only available inside sub-patch editors.
        if !is_inner_canvas && d.category == "SubPatch" { continue; }
        if !categories.contains(&d.category) {
            categories.push(d.category);
        }
    }

    for cat in categories {
        let cat_modules: Vec<&ModuleDescriptor> = descriptors
            .iter()
            .filter(|d| {
                d.category == cat
                    && match &wire {
                        None => true,
                        Some(WireDir::FromOutput { from_type, .. }) => {
                            d.inputs.iter().any(|p| p.signal_type.accepts(*from_type))
                        }
                        Some(WireDir::FromInput { to_type, .. }) => {
                            d.outputs.iter().any(|p| to_type.accepts(p.signal_type))
                        }
                    }
            })
            .collect();

        if cat_modules.is_empty() {
            continue;
        }

        ui.menu_button(cat, |ui| {
            for desc in cat_modules {
                if ui.button(desc.display_name).clicked() {
                    let node_id = snarl.insert_node(pos, NodeData::from(desc));
                    match &wire {
                        Some(WireDir::FromOutput { src, from_type }) => {
                            if let Some((idx, _)) = desc
                                .inputs
                                .iter()
                                .enumerate()
                                .find(|(_, p)| p.signal_type.accepts(*from_type))
                            {
                                snarl.connect(*src, InPinId { node: node_id, input: idx });
                            }
                        }
                        Some(WireDir::FromInput { dst, to_type }) => {
                            if let Some((idx, _)) = desc
                                .outputs
                                .iter()
                                .enumerate()
                                .find(|(_, p)| to_type.accepts(p.signal_type))
                            {
                                snarl.connect(OutPinId { node: node_id, output: idx }, *dst);
                            }
                        }
                        None => {}
                    }
                    ui.close();
                }
            }
        });
    }
}

// ── Pin label color helpers ───────────────────────────────────────────────────

pub(crate) fn channel_label_color(module_id: &str, ch: usize) -> Option<Color32> {
    match module_id {
        "display.vectorscope" | "display.oscilloscope" | "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve" => {
            Some(MULTI_COLORS[ch % MULTI_COLORS.len()])
        }
        // trigscope: ch 0 is "trig" (no color), ch 1+ are data channels
        "display.trigscope" => if ch == 0 { None } else { Some(MULTI_COLORS[(ch - 1) % MULTI_COLORS.len()]) },
        // selector: ch 0 is "select" (no color), ch 1+ are the value inputs
        "module.selector" => if ch == 0 { None } else { Some(MULTI_COLORS[(ch - 1) % MULTI_COLORS.len()]) },
        "module.split" | "module.delay" | "module.average" | "module.dc_filter" => {
            Some(MULTI_COLORS[ch % MULTI_COLORS.len()])
        }
        _ => None,
    }
}

// ── Display module body renderers ─────────────────────────────────────────────

/// Default graph background: the dark base (`gray 16`) at 60% opacity so the
/// sub-patch / canvas background shows through. Shared by all graph displays
/// (Response Curve, Oscilloscope, Vectorscope) on both the editor-body and the
/// pinned-layout render paths. Layout widgets may override this via
/// `PinGraphOverride::background`.
/// Unmultiplied source is (16,16,16,153); premultiplied bytes ≈ round(16*153/255)=10.
pub(crate) const GRAPH_BG_DEFAULT: Color32 = Color32::from_rgba_premultiplied(10, 10, 10, 153);

/// Per-channel line/dot color, honoring an optional `PinGraphOverride`'s
/// `channel_colors[ch]` when present, else the built-in palette.
pub(crate) fn graph_channel_color(ov: Option<&crate::canvas::node::PinGraphOverride>, ch: usize) -> Color32 {
    ov.and_then(|o| o.channel_colors.get(ch).copied().flatten())
        .map(rgba_to_color32)
        .unwrap_or(MULTI_COLORS[ch % MULTI_COLORS.len()])
}

/// Resolved graph chrome from an optional override: (background fill, outline
/// stroke). The outline is `None` when no override outline is set or its width
/// rounds to zero — graphs draw no frame by default.
pub(crate) fn graph_chrome(
    ov: Option<&crate::canvas::node::PinGraphOverride>,
) -> (Color32, Option<egui::Stroke>) {
    let bg = ov.and_then(|o| o.background).map(rgba_to_color32).unwrap_or(GRAPH_BG_DEFAULT);
    let outline = ov.and_then(|o| {
        let px = o.outline_px.unwrap_or(0.0);
        let col = o.outline.map(rgba_to_color32)?;
        (px > 0.05 && col.a() > 0).then(|| egui::Stroke::new(px, col))
    });
    (bg, outline)
}

/// Default gridline / axis color — same neutral hue as the graph axis labels
/// (`rgba 180,180,180,160`), brighter than the old `gray 35–55` lines.
pub(crate) const GRAPH_GRID_DEFAULT: Color32 = Color32::from_rgba_premultiplied(113, 113, 113, 160);

/// Resolved grid colors from an optional override: `(faint, axis)`. `axis` is
/// the user-chosen (or default) gridline color used for the brighter zero /
/// centre / diagonal lines; `faint` is the dimmer subdivision color, derived as
/// ~64% of the axis color so a single override swatch drives both intensities
/// consistently (mirrors the old 35-vs-55 gray relationship).
pub(crate) fn graph_grid_colors(ov: Option<&crate::canvas::node::PinGraphOverride>) -> (Color32, Color32) {
    let axis = ov.and_then(|o| o.gridline).map(rgba_to_color32).unwrap_or(GRAPH_GRID_DEFAULT);
    let faint = Color32::from_rgba_unmultiplied(
        (axis.r() as f32 * 0.64) as u8,
        (axis.g() as f32 * 0.64) as u8,
        (axis.b() as f32 * 0.64) as u8,
        axis.a(),
    );
    (faint, axis)
}

// 12 perceptually-spread colors for multi-pin modules (selector inputs, split outputs, etc.).
// The first four (red/green/blue/yellow) double as the oscilloscope channel colors.
pub(crate) const MULTI_COLORS: [Color32; 12] = [
    Color32::from_rgb(255, 80,  80),   //  0 red
    Color32::from_rgb(80,  220, 80),   //  1 green
    Color32::from_rgb(80,  140, 255),  //  2 blue
    Color32::from_rgb(255, 220, 50),   //  3 yellow
    Color32::from_rgb(80,  220, 220),  //  4 cyan
    Color32::from_rgb(220, 80,  220),  //  5 magenta
    Color32::from_rgb(255, 140, 40),   //  6 orange
    Color32::from_rgb(140, 255, 80),   //  7 lime
    Color32::from_rgb(180, 100, 255),  //  8 violet
    Color32::from_rgb(255, 120, 160),  //  9 pink
    Color32::from_rgb(40,  200, 160),  // 10 teal
    Color32::from_rgb(200, 200, 80),   // 11 olive
];
