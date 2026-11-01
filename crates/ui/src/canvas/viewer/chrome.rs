//! Graph chrome: the Add-module menu, channel/graph colors, shared palettes.

use super::*;

/// The wires dropped on empty space that a new module will take. `moved_from` is the pin
/// they were picked up from, if they're existing wires being moved: the ones that find a
/// place on the module leave it. The type is the first wire's, which picks the modules to
/// offer; each wire then finds its own pin.
pub(crate) enum WireDir<'a> {
    FromOutput { srcs: &'a [OutPinId], from_type: SignalType, moved_from: Option<InPinId> },
    FromInput  { dsts: &'a [InPinId],  to_type:   SignalType, moved_from: Option<OutPinId> },
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
            // Finger-sized rows can make a long category taller than the screen.
            let max_height = ui.ctx().content_rect().height() - 32.0;
            egui::ScrollArea::vertical().max_height(max_height).show(ui, |ui| {
                for desc in cat_modules {
                    if ui.button(desc.display_name).clicked() {
                        let node_id = snarl.insert_node(pos, NodeData::from(desc));
                        if let Some(wire) = &wire {
                            connect_dropped_wires(snarl, wire, desc, node_id);
                        }
                        ui.close();
                    }
                }
            });
        });
    }
}

/// Delete the wires a move was carrying, which are still connected at `from`.
pub(crate) fn delete_moved_wires(snarl: &mut Snarl<NodeData>, carried: &AnyPins, from: AnyPin) {
    match (carried, from) {
        (AnyPins::Out(srcs), AnyPin::In(from)) => {
            for &src in *srcs {
                snarl.disconnect(src, from);
            }
        }
        (AnyPins::In(dsts), AnyPin::Out(from)) => {
            for &dst in *dsts {
                snarl.disconnect(from, dst);
            }
        }
        _ => {}
    }
}

/// Connect wires dropped on empty space to the new module just made for them. Each takes
/// the first of its pins that accepts it; a wire none accepts stays where it was.
fn connect_dropped_wires(
    snarl: &mut Snarl<NodeData>,
    wire: &WireDir,
    desc: &ModuleDescriptor,
    node_id: NodeId,
) {
    match wire {
        WireDir::FromOutput { srcs, moved_from, .. } => {
            for &src in *srcs {
                let from_type = snarl[src.node].outputs[src.output].signal_type;
                // An Auto-Map input takes one wire, so a second one looks further.
                let found = desc.inputs.iter().enumerate().find(|&(input, p)| {
                    p.signal_type.accepts(from_type)
                        && (p.signal_type != SignalType::AutoMap
                            || snarl.in_pin(InPinId { node: node_id, input }).remotes.is_empty())
                });
                if let Some((input, _)) = found {
                    snarl.connect(src, InPinId { node: node_id, input });
                    if let Some(from) = moved_from {
                        snarl.disconnect(src, *from);
                    }
                }
            }
        }
        WireDir::FromInput { dsts, moved_from, .. } => {
            for &dst in *dsts {
                let to_type = snarl[dst.node].inputs[dst.input].signal_type;
                let found = desc.outputs.iter().enumerate()
                    .find(|(_, p)| to_type.accepts(p.signal_type));
                if let Some((output, _)) = found {
                    if to_type == SignalType::AutoMap {
                        snarl.drop_inputs(dst);
                    }
                    snarl.connect(OutPinId { node: node_id, output }, dst);
                    if let Some(from) = moved_from {
                        snarl.disconnect(*from, dst);
                    }
                }
            }
        }
    }
}

// ── Pin label color helpers ───────────────────────────────────────────────────

/// Label tint for a device inlet the Auto-Map bus drives. Derived from the
/// AutoMap type colour (`[255, 140, 40]`, the colour of the bus pin and its
/// wire) lifted 25% toward white, because that colour was picked for a filled
/// chip and reads muddy as small text on the node's dark body.
pub(crate) const AUTOMAP_INLET_LABEL: Color32 = Color32::from_rgb(255, 169, 94);

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

#[cfg(test)]
mod dropped_wire_tests {
    use super::*;

    fn desc(inputs: &[SignalType], outputs: &[SignalType]) -> ModuleDescriptor {
        let pins = |types: &[SignalType]| {
            types.iter().map(|&t| PinDescriptor::new("pin", t)).collect()
        };
        ModuleDescriptor {
            id: "test.module",
            display_name: "Test",
            category: "Test",
            inputs: pins(inputs),
            outputs: pins(outputs),
        }
    }

    fn add(snarl: &mut Snarl<NodeData>, desc: &ModuleDescriptor) -> NodeId {
        snarl.insert_node(egui::Pos2::ZERO, NodeData::from(desc))
    }

    fn wires(snarl: &Snarl<NodeData>) -> Vec<(OutPinId, InPinId)> {
        let mut wires: Vec<_> = snarl.wires().collect();
        wires.sort_by_key(|(o, i)| (o.node.0, i.node.0));
        wires
    }

    /// Every wire of a bundle moves to the new module, not just the first.
    #[test]
    fn a_moved_bundle_of_sources_all_lands_on_the_new_module() {
        use SignalType::Float;
        let mut snarl = Snarl::new();
        let source = desc(&[], &[Float]);
        let (s1, s2) = (add(&mut snarl, &source), add(&mut snarl, &source));
        let old = add(&mut snarl, &desc(&[Float], &[]));
        let (o1, o2) = (OutPinId { node: s1, output: 0 }, OutPinId { node: s2, output: 0 });
        let from = InPinId { node: old, input: 0 };
        snarl.connect(o1, from);
        snarl.connect(o2, from);

        let module = desc(&[Float], &[]);
        let new = add(&mut snarl, &module);
        let srcs = [o1, o2];
        let wire = WireDir::FromOutput { srcs: &srcs, from_type: Float, moved_from: Some(from) };
        connect_dropped_wires(&mut snarl, &wire, &module, new);

        let to = InPinId { node: new, input: 0 };
        assert_eq!(wires(&snarl), [(o1, to), (o2, to)]);
    }

    /// A new wire leaves what's already there alone.
    #[test]
    fn a_new_wire_moves_nothing() {
        use SignalType::Float;
        let mut snarl = Snarl::new();
        let s = add(&mut snarl, &desc(&[], &[Float]));
        let old = add(&mut snarl, &desc(&[Float], &[]));
        let (src, kept) = (OutPinId { node: s, output: 0 }, InPinId { node: old, input: 0 });
        snarl.connect(src, kept);

        let module = desc(&[Float], &[]);
        let new = add(&mut snarl, &module);
        let srcs = [src];
        let wire = WireDir::FromOutput { srcs: &srcs, from_type: Float, moved_from: None };
        connect_dropped_wires(&mut snarl, &wire, &module, new);

        assert_eq!(wires(&snarl), [(src, kept), (src, InPinId { node: new, input: 0 })]);
    }

    /// An Auto-Map input takes one wire. A bundle spreads over the ones a module has, and
    /// what doesn't fit stays connected where it was rather than being lost.
    #[test]
    fn auto_map_inputs_take_one_wire_each() {
        use SignalType::AutoMap;
        let mut snarl = Snarl::new();
        let source = desc(&[], &[AutoMap]);
        let (s1, s2) = (add(&mut snarl, &source), add(&mut snarl, &source));
        let old = add(&mut snarl, &desc(&[AutoMap], &[]));
        let (o1, o2) = (OutPinId { node: s1, output: 0 }, OutPinId { node: s2, output: 0 });
        let from = InPinId { node: old, input: 0 };
        snarl.connect(o1, from);
        snarl.connect(o2, from);

        let module = desc(&[AutoMap], &[]);
        let new = add(&mut snarl, &module);
        let srcs = [o1, o2];
        let wire = WireDir::FromOutput { srcs: &srcs, from_type: AutoMap, moved_from: Some(from) };
        connect_dropped_wires(&mut snarl, &wire, &module, new);

        assert_eq!(wires(&snarl), [(o1, InPinId { node: new, input: 0 }), (o2, from)]);
    }

    /// An outlet's wires move too: the new module drives everything the old pin did.
    #[test]
    fn a_moved_bundle_of_targets_all_lands_on_the_new_module() {
        use SignalType::Float;
        let mut snarl = Snarl::new();
        let old = add(&mut snarl, &desc(&[], &[Float]));
        let sink = desc(&[Float], &[]);
        let (d1, d2) = (add(&mut snarl, &sink), add(&mut snarl, &sink));
        let from = OutPinId { node: old, output: 0 };
        let (i1, i2) = (InPinId { node: d1, input: 0 }, InPinId { node: d2, input: 0 });
        snarl.connect(from, i1);
        snarl.connect(from, i2);

        let module = desc(&[], &[Float]);
        let new = add(&mut snarl, &module);
        let dsts = [i1, i2];
        let wire = WireDir::FromInput { dsts: &dsts, to_type: Float, moved_from: Some(from) };
        connect_dropped_wires(&mut snarl, &wire, &module, new);

        let out = OutPinId { node: new, output: 0 };
        assert_eq!(wires(&snarl), [(out, i1), (out, i2)]);
    }

    /// Deleting takes the wires being carried, and only those.
    #[test]
    fn delete_takes_only_the_carried_wires() {
        use SignalType::Float;
        let mut snarl = Snarl::new();
        let source = desc(&[], &[Float]);
        let (s1, s2) = (add(&mut snarl, &source), add(&mut snarl, &source));
        let sink = add(&mut snarl, &desc(&[Float], &[]));
        let (o1, o2) = (OutPinId { node: s1, output: 0 }, OutPinId { node: s2, output: 0 });
        let pin = InPinId { node: sink, input: 0 };
        snarl.connect(o1, pin);
        snarl.connect(o2, pin);

        let carried = [o1];
        delete_moved_wires(&mut snarl, &AnyPins::Out(&carried), AnyPin::In(pin));

        assert_eq!(wires(&snarl), [(o2, pin)]);
    }
}
