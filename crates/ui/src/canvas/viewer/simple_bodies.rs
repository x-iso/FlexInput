//! Small leaf node bodies: generic pin helpers, math/selector/split,
//! dropdown, sink, constant, switch, label/SVG, counter/logic, knob,
//! delay/average/DC filter.

use super::*;

// ── Generic pin removal helpers ───────────────────────────────────────────────

pub(crate) fn remove_input_pin(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<OutPinId>> = inputs[rm_idx..].iter().map(|p| p.remotes.clone()).collect();
    for i in 0..tail.len() {
        snarl.drop_inputs(InPinId { node: node_id, input: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.inputs.remove(rm_idx);
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_in = InPinId { node: node_id, input: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(remote, new_in);
        }
    }
}

pub(crate) fn remove_output_pin(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<egui_snarl::InPinId>> = outputs[rm_idx..].iter().map(|o| o.remotes.clone()).collect();
    for i in 0..tail.len() {
        snarl.drop_outputs(OutPinId { node: node_id, output: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.outputs.remove(rm_idx);
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_out = OutPinId { node: node_id, output: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(new_out, remote);
        }
    }
}

// ── Math variadic body ────────────────────────────────────────────────────────

pub(crate) fn pin_letter(idx: usize) -> String {
    if idx < 26 { format!("{}", (b'a' + idx as u8) as char) }
    else { format!("in_{}", idx) }
}

pub(crate) fn show_math_variadic_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    let n = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(2);
    ui.horizontal(|ui| {
        if ui.small_button("+").on_hover_text("Add input").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let name = pin_letter(node.inputs.len());
                node.inputs.push(PinDescriptor::new(name, SignalType::Any));
            }
        }
        if n > 2 && ui.small_button("−").on_hover_text("Remove last input").clicked() {
            remove_input_pin(node_id, n - 1, inputs, snarl);
        }
    });
}

// ── Selector body ─────────────────────────────────────────────────────────────

pub(crate) fn show_selector_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // inputs[0] = select (fixed); inputs[1..] = in_0, in_1, ... (dynamic)
    let n_value = snarl.get_node(node_id).map(|n| n.inputs.len().saturating_sub(1)).unwrap_or(2);
    let mut interp = snarl.get_node(node_id)
        .and_then(|n| n.params.get("interpolate").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    ui.vertical(|ui| {
        ui.set_min_width(80.0);
        let mut to_remove: Option<usize> = None;
        for i in 0..n_value {
            ui.horizontal(|ui| {
                if n_value > 2 {
                    if ui.small_button("×").clicked() { to_remove = Some(i + 1); }
                } else {
                    ui.add_space(18.0);
                }
                ui.label(egui::RichText::new(format!("in_{i}")).small());
            });
        }
        if let Some(rm) = to_remove {
            remove_input_pin(node_id, rm, inputs, snarl);
        }
        ui.add_space(2.0);
        if ui.small_button("+ input").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len() - 1;
                node.inputs.push(PinDescriptor::new(format!("in_{next}"), SignalType::Any));
            }
        }
        let interp_before = interp;
        ui.checkbox(&mut interp, egui::RichText::new("Interp").small());
        if interp != interp_before {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("interpolate".to_string(), Value::Bool(interp));
            }
        }
    });
}

// ── Split body ────────────────────────────────────────────────────────────────

pub(crate) fn show_split_body(
    node_id: NodeId,
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    let n_out = snarl.get_node(node_id).map(|n| n.outputs.len()).unwrap_or(2);
    let mut interp = snarl.get_node(node_id)
        .and_then(|n| n.params.get("interpolate").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    ui.vertical(|ui| {
        ui.set_min_width(80.0);
        let mut to_remove: Option<usize> = None;
        for i in 0..n_out {
            ui.horizontal(|ui| {
                if n_out > 2 {
                    if ui.small_button("×").clicked() { to_remove = Some(i); }
                } else {
                    ui.add_space(18.0);
                }
                ui.label(egui::RichText::new(format!("out_{i}")).small());
            });
        }
        if let Some(rm) = to_remove {
            remove_output_pin(node_id, rm, outputs, snarl);
        }
        ui.add_space(2.0);
        if ui.small_button("+ output").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.outputs.len();
                node.outputs.push(PinDescriptor::new(format!("out_{next}"), SignalType::Any));
            }
        }
        let interp_before = interp;
        ui.checkbox(&mut interp, egui::RichText::new("Interp").small());
        if interp != interp_before {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("interpolate".to_string(), Value::Bool(interp));
            }
        }
    });
}

// ── Dropdown ──────────────────────────────────────────────────────────────────
//
// Body layout (top-down):
//   • ComboBox of the current options (pinnable element "selection",
//     resizable width via egui::Resize so a pinned widget honours the
//     user's chosen size in a sub-patch layout).
//   • Vertical list of entries. Each row:
//       [≡ drag handle] [× remove] [label (double-click / Alt-click to edit)]
//   • "+" button below the last row to append a new entry.
//
// Persisted params:
//   options:        Array<String>
//   selected_index: u64
//   box_width:      f64 (optional, persists the user's resize of the ComboBox)
//
// When the currently-selected entry is removed, selection clamps to the new
// last index (matches the "Clamp to last" behaviour discussed in design).
pub(crate) fn dropdown_read_options(node: &NodeData) -> Vec<String> {
    node.params.get("options")
        .and_then(|v| v.as_array())
        .map(|a| a.iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect())
        .unwrap_or_else(|| vec!["Option 1".to_string(), "Option 2".to_string()])
}

pub(crate) fn dropdown_write_options(node: &mut NodeData, opts: &[String]) {
    let arr: Vec<Value> = opts.iter().map(|s| Value::String(s.clone())).collect();
    node.params.insert("options".to_string(), Value::Array(arr));
}

pub(crate) fn dropdown_read_selected(node: &NodeData, n: usize) -> usize {
    let idx = node.params.get("selected_index")
        .and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    if n == 0 { 0 } else { idx.min(n - 1) }
}

pub(crate) fn dropdown_write_selected(node: &mut NodeData, idx: usize) {
    node.params.insert("selected_index".to_string(), Value::from(idx as u64));
}

/// Per-node editing state held in egui memory: which row is being renamed and
/// the in-progress text buffer for it. Cleared when editing ends (Enter,
/// Escape, focus loss, or row delete).
#[derive(Clone, Default)]
pub(crate) struct DropdownEditState {
    editing: Option<usize>,
    buf: String,
    /// Set when we just entered edit mode so we can focus the TextEdit on the
    /// next frame.
    request_focus: bool,
}

pub(crate) fn dropdown_edit_state(ctx: &egui::Context, node_id: NodeId) -> DropdownEditState {
    ctx.data(|d| d.get_temp::<DropdownEditState>(
        egui::Id::new(("fxi_dropdown_edit", node_id.0))
    )).unwrap_or_default()
}

pub(crate) fn dropdown_set_edit_state(ctx: &egui::Context, node_id: NodeId, st: DropdownEditState) {
    ctx.data_mut(|d| d.insert_temp(
        egui::Id::new(("fxi_dropdown_edit", node_id.0)),
        st,
    ));
}

pub(crate) fn show_dropdown_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // Snapshot params up-front.
    let mut options = snarl.get_node(node_id).map(dropdown_read_options).unwrap_or_default();
    let selected = snarl.get_node(node_id).map(|n| dropdown_read_selected(n, options.len())).unwrap_or(0);
    let box_width = snarl.get_node(node_id)
        .and_then(|n| n.params.get("box_width").and_then(|v| v.as_f64()))
        .map(|v| v as f32).unwrap_or(140.0).clamp(80.0, 600.0);

    let mut new_selected = selected;
    let mut options_changed = false;
    let mut selected_changed = false;

    ui.vertical(|ui| {
        ui.set_min_width(box_width.max(140.0));

        // ── ComboBox (pinnable) ──────────────────────────────────────────
        let mut combo_rect = egui::Rect::NOTHING;
        egui::Resize::default()
            .id_salt(("dropdown_combo", node_id))
            .default_size([box_width, 22.0])
            .min_size([80.0, 22.0])
            .max_size([600.0, 22.0])
            .resizable([true, false])
            .show(ui, |ui| {
                let current = options.get(selected).cloned().unwrap_or_default();
                let avail = ui.available_width();
                let inner = ui.allocate_ui([avail, 22.0].into(), |ui| {
                    egui::ComboBox::from_id_salt(("dropdown_combo_sel", node_id.0))
                        .selected_text(if current.is_empty() { "—".to_string() } else { current })
                        .width(avail)
                        .show_ui(ui, |ui| {
                            for (i, opt) in options.iter().enumerate() {
                                let label = if opt.is_empty() { format!("(empty {i})") } else { opt.clone() };
                                if ui.selectable_label(i == selected, label).clicked() {
                                    new_selected = i;
                                    selected_changed = true;
                                }
                            }
                        });
                });
                combo_rect = inner.response.rect;
            });
        // Persist the new width if the user resized horizontally.
        let new_w = combo_rect.width().clamp(80.0, 600.0);
        if (new_w - box_width).abs() > 0.5 {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("box_width".to_string(), Value::from(new_w as f64));
            }
        }
        register_exposable_element(ui, node_id, "selection", combo_rect);

        ui.add_space(4.0);

        // ── Editor list ─────────────────────────────────────────────────
        let mut to_remove: Option<usize> = None;
        let mut to_move: Option<(usize, isize)> = None;
        let mut edit_state = dropdown_edit_state(ui.ctx(), node_id);
        let alt = ui.input(|i| i.modifiers.alt);

        for i in 0..options.len() {
            let mut row_remove = false;
            let mut row_move: isize = 0;
            let mut commit_edit: Option<String> = None;
            let mut cancel_edit = false;

            ui.horizontal(|ui| {
                // Drag handle. Drag vertically beyond half a row → reorder.
                let handle = ui.add(egui::Label::new(
                    egui::RichText::new("≡").monospace()
                ).sense(egui::Sense::click_and_drag()));
                if handle.dragged() {
                    let dy = handle.drag_delta().y;
                    // Accumulate drag in egui memory so a slow drag still
                    // crosses the threshold instead of stalling on per-frame
                    // sub-pixel deltas.
                    let key = egui::Id::new(("fxi_dropdown_drag", node_id.0, i));
                    let acc: f32 = ui.ctx().data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0) + dy;
                    let row_h = 22.0_f32;
                    let steps = (acc / row_h).trunc() as isize;
                    if steps != 0 {
                        row_move = steps;
                        ui.ctx().data_mut(|d| d.insert_temp(key, acc - steps as f32 * row_h));
                    } else {
                        ui.ctx().data_mut(|d| d.insert_temp(key, acc));
                    }
                } else {
                    let key = egui::Id::new(("fxi_dropdown_drag", node_id.0, i));
                    ui.ctx().data_mut(|d| d.remove_temp::<f32>(key));
                }

                if ui.small_button("×").clicked() {
                    row_remove = true;
                }

                let is_editing = edit_state.editing == Some(i);
                if is_editing {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut edit_state.buf)
                            .desired_width(ui.available_width().max(60.0))
                    );
                    if edit_state.request_focus {
                        resp.request_focus();
                        edit_state.request_focus = false;
                    }
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let esc   = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if esc {
                        cancel_edit = true;
                    } else if enter || resp.lost_focus() {
                        commit_edit = Some(edit_state.buf.clone());
                    }
                } else {
                    let label_text = if options[i].is_empty() {
                        format!("(empty {i})")
                    } else {
                        options[i].clone()
                    };
                    let label = ui.add(egui::Label::new(label_text)
                        .sense(egui::Sense::click()));

                    // Double-click OR Alt-click → enter edit mode.
                    if label.double_clicked() || (alt && label.clicked()) {
                        edit_state.editing = Some(i);
                        edit_state.buf = options[i].clone();
                        edit_state.request_focus = true;
                    }

                    // Single click (no Alt) → select this entry.
                    if label.clicked() && !alt && !label.double_clicked() {
                        new_selected = i;
                        selected_changed = true;
                    }

                    // Right-click context menu.
                    label.context_menu(|ui| {
                        if ui.button("Rename").clicked() {
                            edit_state.editing = Some(i);
                            edit_state.buf = options[i].clone();
                            edit_state.request_focus = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui.add_enabled(i > 0, egui::Button::new("Move up")).clicked() {
                            row_move = -1;
                            ui.close();
                        }
                        if ui.add_enabled(i + 1 < options.len(),
                            egui::Button::new("Move down")).clicked()
                        {
                            row_move = 1;
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Delete").clicked() {
                            row_remove = true;
                            ui.close();
                        }
                    });
                }
            });

            if let Some(new_text) = commit_edit {
                options[i] = new_text;
                options_changed = true;
                edit_state.editing = None;
                edit_state.buf.clear();
            }
            if cancel_edit {
                edit_state.editing = None;
                edit_state.buf.clear();
            }
            if row_remove { to_remove = Some(i); }
            if row_move != 0 { to_move = Some((i, row_move)); }
        }

        // ── Add button ──────────────────────────────────────────────────
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            if ui.small_button("+").clicked() {
                let next_n = options.len() + 1;
                options.push(format!("Option {next_n}"));
                options_changed = true;
            }
        });

        // ── Apply mutations ─────────────────────────────────────────────
        if let Some((i, dir)) = to_move {
            let target = (i as isize + dir).clamp(0, options.len() as isize - 1) as usize;
            if target != i {
                options.swap(i, target);
                options_changed = true;
                // Keep selection pointing at the same logical entry.
                if new_selected == i {
                    new_selected = target;
                    selected_changed = true;
                } else if new_selected == target {
                    new_selected = i;
                    selected_changed = true;
                }
            }
        }
        if let Some(rm) = to_remove {
            if rm < options.len() {
                options.remove(rm);
                options_changed = true;
                // Stop editing if the deleted row was the active editor.
                if edit_state.editing == Some(rm) {
                    edit_state.editing = None;
                    edit_state.buf.clear();
                } else if let Some(e) = edit_state.editing {
                    if e > rm { edit_state.editing = Some(e - 1); }
                }
                // Selection clamp: shift down if a row before the selection
                // was deleted, otherwise clamp to the new last index.
                if options.is_empty() {
                    new_selected = 0;
                } else if rm < new_selected {
                    new_selected = new_selected.saturating_sub(1);
                    selected_changed = true;
                } else if new_selected >= options.len() {
                    new_selected = options.len() - 1;
                    selected_changed = true;
                }
            }
        }

        dropdown_set_edit_state(ui.ctx(), node_id, edit_state);
    });

    if options_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            dropdown_write_options(node, &options);
        }
    }
    if selected_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            dropdown_write_selected(node, new_selected);
        }
    }
}

/// Renderer used when the dropdown's ComboBox is pinned into a sub-patch
/// layout. Fills the full container (both axes) so the user can resize the
/// layout slot freely in either direction; the selected-option text scales
/// with the container's height. Click opens a popup of options.
pub(crate) fn render_dropdown_selection(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let options = inner_snarl.get_node(inner_id).map(dropdown_read_options).unwrap_or_default();
    let selected = inner_snarl.get_node(inner_id)
        .map(|n| dropdown_read_selected(n, options.len()))
        .unwrap_or(0);
    let current = options.get(selected).cloned().unwrap_or_default();

    let avail = egui::vec2(container.x.max(40.0), container.y.max(16.0));
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click());

    // Visual: a button-like frame matching the active widget visuals so it
    // sits naturally next to other pinned widgets in the layout.
    let visuals = ui.style().interact(&resp);
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        visuals.corner_radius,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );

    // Reserve a slice on the right for the ▼ caret so the label never overlaps it.
    let caret_w = (rect.height() * 0.7).clamp(10.0, 24.0);
    let text_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(6.0, 0.0),
        egui::pos2(rect.max.x - caret_w, rect.max.y),
    );

    // Text size scales with available height. Leave ~25% vertical padding so
    // the glyphs don't kiss the frame; clamp to a sensible range.
    let mut font_size = (rect.height() * 0.7).clamp(8.0, 64.0);
    let label = if current.is_empty() { "—".to_string() } else { current };
    // If the laid-out text is wider than the text slot at the height-derived
    // size, shrink the font to fit so long entries stay readable.
    let max_w = text_rect.width().max(1.0);
    let measure = |size: f32| -> f32 {
        let galley = painter.layout_no_wrap(
            label.clone(),
            egui::FontId::proportional(size),
            visuals.text_color(),
        );
        galley.size().x
    };
    let w = measure(font_size);
    if w > max_w {
        font_size = (font_size * (max_w / w)).max(8.0);
    }

    painter.text(
        egui::pos2(text_rect.min.x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &label,
        egui::FontId::proportional(font_size),
        visuals.text_color(),
    );

    // ▼ caret, centred in the reserved slot, scaled to height.
    let caret_size = (rect.height() * 0.45).clamp(8.0, 22.0);
    painter.text(
        egui::pos2(rect.max.x - caret_w * 0.5, rect.center().y),
        egui::Align2::CENTER_CENTER,
        "▼",
        egui::FontId::proportional(caret_size),
        visuals.text_color(),
    );

    // Popup menu: open on click, close on selection or outside-click.
    let popup_id = egui::Id::new(("dropdown_pinned_popup", inner_id.0));
    if resp.clicked() {
        egui::Popup::toggle_id(ui.ctx(), popup_id);
    }
    let mut chosen: Option<usize> = None;
    popup_below_widget(
        &resp, popup_id,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(rect.width().max(80.0));
            for (i, opt) in options.iter().enumerate() {
                let label = if opt.is_empty() { format!("(empty {i})") } else { opt.clone() };
                if ui.selectable_label(i == selected, label).clicked() {
                    chosen = Some(i);
                }
            }
        },
    );
    if chosen.is_some() {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }
    if let Some(i) = chosen {
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            dropdown_write_selected(node, i);
        }
    }
}

pub(crate) fn show_sink_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let device_id = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    if device_id != "virtual.keymouse" {
        // Virtual gamepad sinks have no body controls — the rumble-feedback
        // shaping (floor/max/curve) lives in the node header alongside the
        // keymouse Mouse × slider (see show_header).
        return;
    }

    let fixed_count = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("fixed_input_count").and_then(|v| v.as_u64()))
        .unwrap_or(0) as usize;

    let is_learning = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("learning").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    if is_learning {
        ui.label(egui::RichText::new("Press a key… (Esc cancels)").italics().small());

        let key_pressed = ui.input(|i| {
            i.events.iter().find_map(|e| {
                if let egui::Event::Key { key, pressed: true, .. } = e {
                    Some(*key)
                } else {
                    None
                }
            })
        });

        if let Some(key) = key_pressed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("learning".to_string(), Value::Bool(false));
            }

            if key != egui::Key::Escape {
                let pin_name = format!("{key:?}");
                let already_has = snarl
                    .get_node(node_id)
                    .map(|n| n.inputs.iter().any(|p| p.name == pin_name))
                    .unwrap_or(false);

                if !already_has {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.inputs.push(PinDescriptor::new(&pin_name, SignalType::Bool));
                        // Keep input_pin_ids in sync for routing.
                        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
                            ids.push(Value::String(pin_name));
                        }
                    }
                }
            }
        }
    } else {
        ui.horizontal(|ui| {
            if ui.small_button("+ Learn key").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(true));
                }
            }
            let has_unused_learned = inputs.iter().skip(fixed_count).any(|p| p.remotes.is_empty());
            if has_unused_learned && ui.small_button("Clear unused").clicked() {
                clear_unused_inputs(node_id, inputs, fixed_count, snarl);
            }
        });
    }
}

/// Per-device rumble-feedback shaping controls on a virtual gamepad's sink node.
/// Floor/Max/Curve shape ONLY the game/app rumble forwarded back to a physical
/// pad via AutoMap (see `shape_hd_feedback`); direct rumble wiring is untouched.
/// Defaults (floor 0.35, max 1.0, exp 0.6) match the original tuned boost.
pub(crate) fn show_constant_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let value = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("value").and_then(|v| v.as_f64()))
        .unwrap_or(0.0) as f32;
    let mut v = value;
    let resp = ui.add(egui::DragValue::new(&mut v).speed(0.01));
    if resp.changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
    register_exposable_element(ui, node_id, "value", resp.rect);
}

// ── Switch ────────────────────────────────────────────────────────────────────
//
// Persisted params (all optional, defaults applied at read time):
//   active:                   bool   — current state. UI clicks and the engine
//                                      both write here; engine reconciles with
//                                      the direct/latch inputs and writes back.
//   caption_on / caption_off: String — text shown for each state.
//   svg_on   / svg_off:       String — SVG source for the icon (empty = none).
//   svg_on_rev / svg_off_rev: u64    — bump on edit to invalidate texture cache.
//   caption_pos_on / caption_pos_off: "top" | "bottom" | "left" | "right"
//                                    — placement of caption relative to icon.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptionPos { Top, Bottom, Left, Right }

impl CaptionPos {
    fn from_str(s: &str) -> Self {
        match s { "top" => Self::Top, "bottom" => Self::Bottom, "left" => Self::Left, _ => Self::Right }
    }
    fn as_str(self) -> &'static str {
        match self { Self::Top => "top", Self::Bottom => "bottom", Self::Left => "left", Self::Right => "right" }
    }
}

pub(crate) struct SwitchState {
    pub(crate) caption: String,
    pub(crate) svg_data: String,
    pub(crate) svg_rev: u64,
    pub(crate) pos: CaptionPos,
}

pub(crate) fn read_switch_state(node: &NodeData, on: bool) -> SwitchState {
    let (cap_key, svg_key, rev_key, pos_key, default_cap, default_pos) = if on {
        ("caption_on", "svg_on", "svg_on_rev", "caption_pos_on", "ON", "right")
    } else {
        ("caption_off", "svg_off", "svg_off_rev", "caption_pos_off", "OFF", "right")
    };
    SwitchState {
        caption: node.params.get(cap_key).and_then(|v| v.as_str())
            .unwrap_or(default_cap).to_string(),
        svg_data: node.params.get(svg_key).and_then(|v| v.as_str())
            .unwrap_or("").to_string(),
        svg_rev: node.params.get(rev_key).and_then(|v| v.as_u64()).unwrap_or(0),
        pos: CaptionPos::from_str(
            node.params.get(pos_key).and_then(|v| v.as_str()).unwrap_or(default_pos)
        ),
    }
}

/// Right-click submenu under "ON state…" / "OFF state…". Lets the user rename
/// the caption, load/clear an SVG icon, and choose where the caption sits
/// relative to the icon. All actions write directly into `node.params`.
pub(crate) fn switch_state_submenu(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    on: bool,
) {
    let st = snarl.get_node(node_id).map(|n| read_switch_state(n, on)).unwrap_or(SwitchState {
        caption: (if on { "ON" } else { "OFF" }).to_string(),
        svg_data: String::new(), svg_rev: 0, pos: CaptionPos::Right,
    });
    let (cap_key, svg_key, rev_key, pos_key) = if on {
        ("caption_on", "svg_on", "svg_on_rev", "caption_pos_on")
    } else {
        ("caption_off", "svg_off", "svg_off_rev", "caption_pos_off")
    };

    // Caption text-edit.
    let mut caption = st.caption.clone();
    ui.horizontal(|ui| {
        ui.label("Caption:");
        let resp = ui.add(egui::TextEdit::singleline(&mut caption).desired_width(120.0));
        if resp.changed() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert(cap_key.to_string(), Value::String(caption.clone()));
            }
        }
    });

    ui.separator();

    // SVG icon: Load… / Clear.
    let has_svg = !st.svg_data.is_empty();
    ui.horizontal(|ui| {
        if ui.button("Load SVG…").clicked() {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file()
            }) {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert(svg_key.to_string(), Value::String(text));
                        node.params.insert(rev_key.to_string(), Value::from(st.svg_rev + 1));
                    }
                }
            }
            ui.close();
        }
        if has_svg && ui.button("Clear icon").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.remove(svg_key);
                node.params.insert(rev_key.to_string(), Value::from(st.svg_rev + 1));
            }
            ui.close();
        }
    });

    ui.separator();

    // Caption position relative to icon. Only meaningful when an icon is set,
    // but we still expose the choice so the user can pre-configure.
    ui.label("Caption position:");
    let mut chosen = st.pos;
    for (label, value) in [
        ("Above", CaptionPos::Top),
        ("Below", CaptionPos::Bottom),
        ("Left",  CaptionPos::Left),
        ("Right", CaptionPos::Right),
    ] {
        if ui.radio(chosen == value, label).clicked() {
            chosen = value;
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert(pos_key.to_string(), Value::String(value.as_str().to_string()));
            }
        }
    }
}

/// Rasterise an SVG to a square texture, cached per (node, side, rev, size).
/// Returns None on parse failure or empty SVG.
pub(crate) fn switch_icon_texture(
    ui: &egui::Ui,
    node_uid: usize,
    side: &'static str,  // "on" or "off"
    svg: &str,
    rev: u64,
    px: u32,
) -> Option<egui::TextureHandle> {
    if svg.is_empty() || px == 0 { return None; }
    let key = egui::Id::new(("switch_icon_tex", node_uid, side, rev, px));
    if let Some(h) = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(h);
    }
    let img = rasterize_svg_recolored(svg, px, px, "override", egui::Color32::TRANSPARENT)?;
    let handle = ui.ctx().load_texture(
        format!("switch-{}-{}-{}", node_uid, side, rev),
        img,
        egui::TextureOptions::LINEAR,
    );
    ui.ctx().data_mut(|d| d.insert_temp(key, handle.clone()));
    Some(handle)
}

/// Lay out caption + icon inside `rect` according to `pos`. Returns (caption_rect, icon_rect).
/// Either rect may be empty (caption empty → no caption_rect; no icon → no icon_rect).
pub(crate) fn switch_layout_icon_caption(
    rect: egui::Rect,
    has_caption: bool,
    has_icon: bool,
    pos: CaptionPos,
) -> (Option<egui::Rect>, Option<egui::Rect>) {
    if !has_caption && !has_icon { return (None, None); }
    if !has_caption { return (None, Some(rect)); }
    if !has_icon    { return (Some(rect), None); }

    // Both present: caption sits on the chosen side. Icon claims the
    // remaining square (or the larger axis if the rect isn't square).
    let pad = 2.0_f32;
    match pos {
        CaptionPos::Left => {
            let cap_w = (rect.width() * 0.45).clamp(20.0, rect.width() - 20.0);
            let cap = egui::Rect::from_min_size(rect.min, egui::vec2(cap_w, rect.height()));
            let icon = egui::Rect::from_min_max(
                egui::pos2(rect.min.x + cap_w + pad, rect.min.y),
                rect.max,
            );
            (Some(cap), Some(icon))
        }
        CaptionPos::Right => {
            let cap_w = (rect.width() * 0.45).clamp(20.0, rect.width() - 20.0);
            let icon = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x - cap_w - pad, rect.max.y),
            );
            let cap = egui::Rect::from_min_max(
                egui::pos2(rect.max.x - cap_w, rect.min.y),
                rect.max,
            );
            (Some(cap), Some(icon))
        }
        CaptionPos::Top => {
            let cap_h = (rect.height() * 0.35).clamp(12.0, rect.height() - 16.0);
            let cap = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), cap_h));
            let icon = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.min.y + cap_h + pad),
                rect.max,
            );
            (Some(cap), Some(icon))
        }
        CaptionPos::Bottom => {
            let cap_h = (rect.height() * 0.35).clamp(12.0, rect.height() - 16.0);
            let icon = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x, rect.max.y - cap_h - pad),
            );
            let cap = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, rect.max.y - cap_h),
                egui::vec2(rect.width(), cap_h),
            );
            (Some(cap), Some(icon))
        }
    }
}

/// Paint icon + caption into the button rect for the current state.
/// `fill_text` is the caption color; `icon_tint` is the SVG tint
/// (Color32::WHITE for no tint, fully transparent for tint disabled).
pub(crate) fn paint_switch_content(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node_uid: usize,
    state: &SwitchState,
    on: bool,
    text_col: egui::Color32,
) {
    let has_caption = !state.caption.is_empty();
    let has_icon = !state.svg_data.is_empty();
    let (cap_rect, icon_rect) = switch_layout_icon_caption(rect, has_caption, has_icon, state.pos);

    if let Some(ir) = icon_rect {
        // Square the icon within its slot.
        let side_px = ir.width().min(ir.height()).round().max(1.0);
        let icon_box = egui::Rect::from_center_size(ir.center(), egui::vec2(side_px, side_px));
        let tex_px = (side_px as u32).max(8);
        let side_id = if on { "on" } else { "off" };
        if let Some(tex) = switch_icon_texture(ui, node_uid, side_id, &state.svg_data, state.svg_rev, tex_px) {
            ui.painter().image(
                tex.id(),
                icon_box,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
    if let Some(cr) = cap_rect {
        // Scale the caption to fill its slot. ~75% of slot height,
        // shrink-to-fit horizontally so long captions stay readable.
        let mut size = (cr.height() * 0.7).clamp(8.0, 64.0);
        let painter = ui.painter();
        let measure = |s: f32| -> f32 {
            painter.layout_no_wrap(
                state.caption.clone(),
                egui::FontId::proportional(s),
                text_col,
            ).size().x
        };
        let max_w = cr.width().max(1.0);
        let w = measure(size);
        if w > max_w {
            size = (size * (max_w / w)).max(8.0);
        }
        painter.text(
            cr.center(),
            egui::Align2::CENTER_CENTER,
            &state.caption,
            egui::FontId::proportional(size),
            text_col,
        );
    }
}

pub(crate) fn read_switch_active(node: &NodeData) -> bool {
    // Prefer the engine's last emitted value — it reconciles UI clicks with
    // direct/latch inputs and is the authoritative current state. Fall back
    // to the persisted `active` when no eval result is available yet (patch
    // just opened, paused engine, etc.).
    if let Some(Some(Signal::Bool(b))) = node.extra.last_out.first() {
        return *b;
    }
    node.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Click handler shared by canvas body + pinned renderer. Bumps a monotonic
/// `ui_toggle_seq` counter (engine toggles once per increment) and writes an
/// optimistic flipped `active` so the button visually updates the same frame.
pub(crate) fn switch_handle_click(node: &mut NodeData, current_active: bool) {
    let seq = node.params.get("ui_toggle_seq").and_then(|v| v.as_u64()).unwrap_or(0);
    node.params.insert("ui_toggle_seq".to_string(), Value::from(seq.wrapping_add(1)));
    node.params.insert("active".to_string(), Value::Bool(!current_active));
}

pub(crate) fn show_switch_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let active = snarl.get_node(node_id).map(read_switch_active).unwrap_or(false);
    let state = snarl.get_node(node_id).map(|n| read_switch_state(n, active)).unwrap_or(SwitchState {
        caption: (if active { "ON" } else { "OFF" }).to_string(),
        svg_data: String::new(), svg_rev: 0, pos: CaptionPos::Right,
    });

    // Compute a button size that comfortably fits the chosen caption + icon.
    let has_icon = !state.svg_data.is_empty();
    let has_caption = !state.caption.is_empty();
    let min_w = if has_icon || has_caption { 64.0 } else { 48.0 };
    let h = if has_icon { 32.0 } else { 22.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(min_w, h), egui::Sense::click());

    let visuals = if active {
        ui.style().visuals.selection.bg_fill
    } else {
        ui.style().visuals.widgets.inactive.bg_fill
    };
    let stroke_col = if active {
        ui.style().visuals.selection.stroke.color
    } else {
        ui.style().visuals.widgets.inactive.bg_stroke.color
    };
    let text_col = if active {
        ui.style().visuals.strong_text_color()
    } else {
        ui.style().visuals.text_color()
    };

    let painter = ui.painter_at(rect);
    painter.rect(rect, 4.0, visuals,
        egui::Stroke::new(1.0, stroke_col), egui::StrokeKind::Inside);
    paint_switch_content(ui, rect, node_id.0, &state, active, text_col);

    if resp.clicked() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            switch_handle_click(node, active);
        }
    }
    register_exposable_element(ui, node_id, "toggle", rect);
}

/// Body for the Text/Label module: editable multiline text + font-size slider.
/// No I/O; purely visual annotation. Persists `text` and `font_size` in params.
pub(crate) fn show_label_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // Module-resident "box_width" — user-resizable width for the wrapped text
    // body. Default 160.0 for parity with the legacy unresizable layout.
    let box_width = snarl.get_node(node_id)
        .and_then(|n| n.params.get("box_width").and_then(|v| v.as_f64()))
        .map(|v| v as f32)
        .unwrap_or(160.0)
        .clamp(60.0, 800.0);

    // Render text body. Capture its rect to position the resize grip at the
    // bottom-right corner.
    let outer = ui.allocate_ui(egui::vec2(box_width, 0.0), |ui| {
        show_label_body_sized(node_id, ui, snarl, box_width, 0.0);
    });
    let body_rect = outer.response.rect;

    // Small corner grip at bottom-right of the text body. Width-only drag
    // (matches the "wrap to width, height auto" body behavior).
    const GRIP: f32 = 10.0;
    let grip_rect = egui::Rect::from_min_size(
        egui::pos2(body_rect.max.x - GRIP, body_rect.max.y - GRIP),
        egui::vec2(GRIP, GRIP),
    );
    let h_resp = ui.interact(
        grip_rect,
        egui::Id::new(("label_box_grip", node_id.0)),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter();
    let col = if h_resp.hovered() || h_resp.dragged() {
        egui::Color32::from_rgba_unmultiplied(180, 230, 255, 180)
    } else {
        egui::Color32::from_rgba_unmultiplied(150, 220, 255, 90)
    };
    // Three diagonal corner stripes for the grip.
    let stroke = egui::Stroke::new(1.2, col);
    for k in 1..=3 {
        let off = k as f32 * (GRIP / 4.0);
        painter.line_segment(
            [egui::pos2(grip_rect.max.x - off, grip_rect.max.y),
             egui::pos2(grip_rect.max.x,       grip_rect.max.y - off)],
            stroke,
        );
    }
    if h_resp.dragged_by(egui::PointerButton::Primary) {
        let dx = h_resp.drag_delta().x;
        if dx.abs() > f32::EPSILON {
            let new_w = (box_width + dx).clamp(60.0, 800.0);
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("box_width".into(), Value::from(new_w as f64));
            }
        }
    }
    register_exposable_element(ui, node_id, "text", body_rect);
}

/// Same as `show_label_body` but with explicit width / height for use when the
/// label is pinned to a sub-patch body and the user has resized its container.
/// Pass `height = 0.0` to let the text edit auto-size vertically.
pub(crate) fn show_label_body_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    width: f32,
    height: f32,
) {
    let (mut text, font_size, col) = snarl.get_node(node_id).map(|n| {
        let t = n.params.get("text").and_then(|v| v.as_str()).unwrap_or("Label").to_string();
        let f = n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32;
        let c = read_label_color(n);
        (t, f, c)
    }).unwrap_or_else(|| ("Label".to_string(), 14.0, egui::Color32::from_rgb(220, 220, 220)));

    let prev_text = text.clone();

    let mut edit = egui::TextEdit::multiline(&mut text)
        .font(egui::FontId::proportional(font_size))
        .text_color(col)
        .desired_width(width.max(40.0));
    if height > 8.0 {
        // Approximate row count from chosen height + font size for a reasonable
        // initial layout; the TextEdit will still scroll if content exceeds it.
        let line_h = (font_size * 1.4).max(10.0);
        let rows = (height / line_h).floor().max(1.0) as usize;
        edit = edit.desired_rows(rows);
    } else {
        edit = edit.desired_rows(2);
    }
    ui.add(edit);

    if text != prev_text {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("text".to_string(), Value::String(text));
        }
    }
}

/// Read the text color stored on a label node. Defaults to a soft gray when
/// the param is missing or malformed (older patches saved before color support).
pub(crate) fn read_label_color(n: &NodeData) -> egui::Color32 {
    match n.params.get("color").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == 4 => egui::Color32::from_rgba_unmultiplied(
            arr[0].as_u64().unwrap_or(220) as u8,
            arr[1].as_u64().unwrap_or(220) as u8,
            arr[2].as_u64().unwrap_or(220) as u8,
            arr[3].as_u64().unwrap_or(255) as u8,
        ),
        _ => egui::Color32::from_rgb(220, 220, 220),
    }
}

/// Read the SVG tint color. Default is fully transparent (alpha = 0 = no tint).
pub(crate) fn read_svg_tint(n: &NodeData) -> egui::Color32 {
    match n.params.get("tint").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == 4 => egui::Color32::from_rgba_unmultiplied(
            arr[0].as_u64().unwrap_or(255) as u8,
            arr[1].as_u64().unwrap_or(255) as u8,
            arr[2].as_u64().unwrap_or(255) as u8,
            arr[3].as_u64().unwrap_or(0) as u8,
        ),
        _ => egui::Color32::TRANSPARENT,
    }
}

pub(crate) fn show_svg_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let outer = ui.allocate_ui(egui::vec2(160.0, 0.0), |ui| {
        show_svg_body_sized(node_id, ui, snarl, egui::vec2(160.0, 160.0));
    });
    register_exposable_element(ui, node_id, "image", outer.response.rect);
}

/// Renders the loaded SVG into a resizable area. The SVG is rasterized via
/// usvg/resvg to an RGBA pixmap whose pixels we then recolor according to the
/// chosen mode (Override = blend toward chosen color; Additive = add chosen
/// color on top). The recolored pixmap is uploaded as an egui texture and
/// painted into the rect. The texture is cached in egui memory keyed by
/// (node, rev, target_size, mode, color) so resizing/recoloring force a new
/// rasterization but unchanged frames don't.
pub(crate) fn show_svg_body_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (data, rev, tint, mode) = snarl.get_node(node_id).map(|n| {
        let d = n.params.get("svg_data").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let r = n.params.get("svg_rev").and_then(|v| v.as_u64()).unwrap_or(0);
        let t = read_svg_tint(n);
        let m = n.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("override").to_string();
        (d, r, t, m)
    }).unwrap_or_default();

    let target = egui::vec2(container.x.max(16.0), container.y.max(16.0));
    let (rect, _) = ui.allocate_exact_size(target, egui::Sense::hover());

    if data.is_empty() {
        let painter = ui.painter_at(rect);
        painter.rect_stroke(rect, 4.0,
            egui::Stroke::new(1.0, egui::Color32::from_gray(70)),
            egui::StrokeKind::Inside);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No SVG loaded\nclick Load… in header",
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(150),
        );
        return;
    }

    let pw = (rect.width().round() as u32).max(1);
    let ph = (rect.height().round() as u32).max(1);
    let cache_key = egui::Id::new((
        "svg_tex_cache", node_id.0, rev, pw, ph, mode.as_str(),
        tint.r(), tint.g(), tint.b(), tint.a(),
    ));
    let cached = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key));
    let tex = match cached {
        Some(t) => t,
        None => {
            match rasterize_svg_recolored(&data, pw, ph, &mode, tint) {
                Some(image) => {
                    let handle = ui.ctx().load_texture(
                        format!("svg-{}-{}", node_id.0, rev),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
                    handle
                }
                None => {
                    ui.painter_at(rect).text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "SVG parse failed",
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgb(220, 80, 80),
                    );
                    return;
                }
            }
        }
    };

    ui.painter().image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

// ── Macro Output body ─────────────────────────────────────────────────────────

/// Body for the Macro Output node: the list of user-defined ports, edited
/// inline — icon (menu of the embedded macro icon set), name, type (Bool /
/// Float / Vec2 / Any), remove. Every change rewrites the `macro_ports`
/// param, the node's dynamic output pins, and `output_pin_ids` together so
/// the three can never drift. Removal drops the port's wires and shifts the
/// ones above down a slot (same surgery as the AutoMap Splitter).
pub(crate) fn show_macro_body(node_id: NodeId, outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    use flexinput_core::macros as mac;

    let Some(node) = snarl.get_node(node_id) else { return };
    let mut ports = mac::ports_from_params(&node.params);
    let mut changed = false;

    // First open of a fresh node: seed one port so the node has a pin and the
    // picker immediately shows something assignable.
    if ports.is_empty() && node.params.get(mac::MACRO_PORTS_PARAM).is_none() {
        ports.push(mac::MacroPortDef {
            id: mac::new_port_id(),
            name: "Macro 1".to_string(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: SignalType::Bool,
        });
        changed = true;
    }

    let mut remove_idx: Option<usize> = None;

    // The snarl body ui lays out HORIZONTALLY by default — without this
    // wrapper every port row lands side by side on one line.
    ui.vertical(|ui| {
    ui.set_min_width(230.0);

    for (i, port) in ports.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            // Icon button + picker (shared with the Virtual Menu module): the
            // custom-SVG loader, category dropdown, name search, and the
            // embedded-set grid all live in `icon_picker_button`.
            if let Some((k, svg)) = crate::canvas::menu_body::icon_picker_button(
                ui,
                egui::Id::new((node_id, "macro_icon_grid", i)),
                &port.icon,
                &port.icon_svg,
            ) {
                port.icon = k;
                port.icon_svg = svg;
                changed = true;
            }

            let name_resp = ui.add(
                egui::TextEdit::singleline(&mut port.name).desired_width(96.0));
            if name_resp.changed() { changed = true; }

            egui::ComboBox::from_id_salt((node_id, "macro_ty", i))
                .selected_text(port.signal_type.display_name())
                .width(58.0)
                .show_ui(ui, |ui| {
                    for ty in [SignalType::Bool, SignalType::Float, SignalType::Vec2, SignalType::Any] {
                        if ui.selectable_label(port.signal_type == ty, ty.display_name()).clicked()
                            && port.signal_type != ty
                        {
                            port.signal_type = ty;
                            changed = true;
                        }
                    }
                });

            if ui.small_button("✕").on_hover_text("Remove port (drops its wires)").clicked() {
                remove_idx = Some(i);
            }
        });
    }

    ui.add_space(2.0);
    if ui.small_button("+ Add output").clicked() {
        ports.push(mac::MacroPortDef {
            id: mac::new_port_id(),
            name: format!("Macro {}", ports.len() + 1),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: SignalType::Bool,
        });
        changed = true;
    }
    }); // end ui.vertical

    if let Some(rm) = remove_idx {
        // Drop the removed port's wires and reconnect the ones above one slot
        // down, so wiring follows its port.
        let tail: Vec<Vec<egui_snarl::InPinId>> = outputs
            .iter()
            .skip(rm)
            .map(|o| o.remotes.clone())
            .collect();
        for i in 0..tail.len() {
            snarl.drop_outputs(OutPinId { node: node_id, output: rm + i });
        }
        for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
            let new_out = OutPinId { node: node_id, output: rm + shift - 1 };
            for remote in remotes {
                snarl.connect(new_out, remote);
            }
        }
        ports.remove(rm);
        changed = true;
    }

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
            node.params.insert(
                "output_pin_ids".to_string(),
                Value::Array(ports.iter().map(|p| Value::String(mac::macro_pin_id(&p.id))).collect()),
            );
            node.outputs = ports
                .iter()
                .map(|p| PinDescriptor::new(p.name.clone(), p.signal_type))
                .collect();
        }
    }
}

/// Rasterize an SVG to a rect of `(w, h)` and recolor the resulting pixmap
/// according to `mode`:
///   - "override": each pixel's RGB is lerped toward `tint.rgb` by `tint.a/255`,
///     preserving the SVG's own per-pixel alpha (so silhouette is kept).
///   - "additive": `tint.rgb * (tint.a / 255)` is added to each pixel's RGB
///     (clamped), again preserving the SVG's alpha.
/// Returns None if the SVG can't be parsed.
pub(crate) fn rasterize_svg_recolored(
    svg_text: &str,
    w: u32,
    h: u32,
    mode: &str,
    tint: egui::Color32,
) -> Option<egui::ColorImage> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg_text, &opt).ok()?;

    let svg_size = tree.size();
    let sx = w as f32 / svg_size.width().max(1.0);
    let sy = h as f32 / svg_size.height().max(1.0);
    let scale = sx.min(sy);
    let render_w = (svg_size.width() * scale).round().max(1.0) as u32;
    let render_h = (svg_size.height() * scale).round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    // Center the rendered SVG in the target rect (preserves aspect ratio).
    let off_x = ((w as f32 - render_w as f32) * 0.5).max(0.0);
    let off_y = ((h as f32 - render_h as f32) * 0.5).max(0.0);
    let transform = tiny_skia::Transform::from_translate(off_x, off_y)
        .post_scale(1.0, 1.0)
        .pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Recolor in place. tiny_skia stores premultiplied RGBA; we work in
    // straight-alpha space then re-premultiply on write.
    let blend = (tint.a() as f32 / 255.0).clamp(0.0, 1.0);
    let tr = tint.r() as f32;
    let tg = tint.g() as f32;
    let tb = tint.b() as f32;
    let pixels = pixmap.pixels_mut();
    if blend > 0.0 {
        match mode {
            "additive" => {
                for px in pixels.iter_mut() {
                    let a = px.alpha() as f32;
                    if a == 0.0 { continue; }
                    // Un-premultiply.
                    let r = (px.red()   as f32) / a * 255.0;
                    let g = (px.green() as f32) / a * 255.0;
                    let b = (px.blue()  as f32) / a * 255.0;
                    let r2 = (r + tr * blend).min(255.0);
                    let g2 = (g + tg * blend).min(255.0);
                    let b2 = (b + tb * blend).min(255.0);
                    // Re-premultiply.
                    let af = a / 255.0;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r2 * af).round() as u8,
                        (g2 * af).round() as u8,
                        (b2 * af).round() as u8,
                        a as u8,
                    ).unwrap_or(*px);
                }
            }
            _ => {
                // Override: lerp original RGB toward tint RGB by `blend`.
                for px in pixels.iter_mut() {
                    let a = px.alpha() as f32;
                    if a == 0.0 { continue; }
                    let r = (px.red()   as f32) / a * 255.0;
                    let g = (px.green() as f32) / a * 255.0;
                    let b = (px.blue()  as f32) / a * 255.0;
                    let r2 = r + (tr - r) * blend;
                    let g2 = g + (tg - g) * blend;
                    let b2 = b + (tb - b) * blend;
                    let af = a / 255.0;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r2 * af).round() as u8,
                        (g2 * af).round() as u8,
                        (b2 * af).round() as u8,
                        a as u8,
                    ).unwrap_or(*px);
                }
            }
        }
    }

    // Convert tiny-skia premultiplied RGBA → egui::ColorImage (also premultiplied).
    let raw = pixmap.data();
    let pixels: Vec<egui::Color32> = raw.chunks_exact(4)
        .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
        .collect();
    Some(egui::ColorImage {
        size: [w as usize, h as usize],
        pixels,
        source_size: egui::Vec2::new(w as f32, h as f32),
    })
}

pub(crate) fn show_counter_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (mode, normalized, step_p, min_p, max_p) = snarl.get_node(node_id).map(|n| {
        let mode       = n.params.get("mode")      .and_then(|v| v.as_str()) .unwrap_or("loop").to_string();
        let normalized = n.params.get("normalized").and_then(|v| v.as_bool()).unwrap_or(false);
        let step_p     = n.params.get("step_param").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
        let min_p      = n.params.get("min_param") .and_then(|v| v.as_f64()).unwrap_or(0.0)  as f32;
        let max_p      = n.params.get("max_param") .and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
        (mode, normalized, step_p, min_p, max_p)
    }).unwrap_or_default();

    let step_wired  = inputs.get(3).map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let min_wired   = inputs.get(4).map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let max_wired   = inputs.get(5).map(|p| !p.remotes.is_empty()).unwrap_or(false);

    let mut mode       = mode;
    let mut normalized = normalized;
    let mut step_p     = step_p;
    let mut min_p      = min_p;
    let mut max_p      = max_p;
    let mut changed    = false;

    let mut mode_rect:    Option<egui::Rect> = None;
    let mut range_rect:   Option<egui::Rect> = None;
    let mut step_rect:    Option<egui::Rect> = None;
    let mut minmax_rect:  Option<egui::Rect> = None;
    ui.vertical(|ui| {
        // Row 1: counting mode
        let r = ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut mode, "loop".into(),      egui::RichText::new("Loop").small()).changed();
            changed |= ui.selectable_value(&mut mode, "limit".into(),     egui::RichText::new("Limit").small()).changed();
            changed |= ui.selectable_value(&mut mode, "bounce".into(),    egui::RichText::new("Bounce").small()).changed();
            changed |= ui.selectable_value(&mut mode, "unlimited".into(), egui::RichText::new("Unlimited").small()).changed();
        });
        mode_rect = Some(r.response.rect);

        // Row 2: output range + reset button
        let r = ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut normalized, false, egui::RichText::new("Raw").small()).changed();
            changed |= ui.selectable_value(&mut normalized, true,  egui::RichText::new("0..1").small()).changed();
            if ui.small_button("↺").on_hover_text("Reset counter").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    while node.extra.aux_f32.len() < 2 { node.extra.aux_f32.push(0.0); }
                    node.extra.aux_f32[0] = 0.0;
                    node.extra.aux_f32[1] = 1.0;
                    node.extra.aux_f32_dirty = true;
                }
            }
        });
        range_rect = Some(r.response.rect);

        // Row 3: Step
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Step").small().weak());
            changed |= ui.add_enabled(!step_wired, egui::DragValue::new(&mut step_p).speed(0.1).range(0.001..=10000.0)).changed();
        });
        step_rect = Some(r.response.rect);

        // Row 4: Min / Max
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Min").small().weak());
            changed |= ui.add_enabled(!min_wired, egui::DragValue::new(&mut min_p).speed(0.1)).changed();
            ui.label(egui::RichText::new("Max").small().weak());
            let max_active = !max_wired && mode != "unlimited";
            changed |= ui.add_enabled(max_active, egui::DragValue::new(&mut max_p).speed(0.1)).changed();
        });
        minmax_rect = Some(r.response.rect);
    });
    if let Some(r) = mode_rect   { register_exposable_element(ui, node_id, "mode",       r); }
    if let Some(r) = range_rect  { register_exposable_element(ui, node_id, "range_mode", r); }
    if let Some(r) = step_rect   { register_exposable_element(ui, node_id, "step",       r); }
    if let Some(r) = minmax_rect { register_exposable_element(ui, node_id, "min_max",    r); }

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("mode".into(),       Value::String(mode));
            node.params.insert("normalized".into(), Value::Bool(normalized));
            if let Some(n) = Number::from_f64(step_p as f64) { node.params.insert("step_param".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(min_p  as f64) { node.params.insert("min_param".into(),  Value::Number(n)); }
            if let Some(n) = Number::from_f64(max_p  as f64) { node.params.insert("max_param".into(),  Value::Number(n)); }
        }
    }
}

pub(crate) fn show_logic_delay_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (mode, time, unit) = snarl.get_node(node_id).map(|n| {
        let mode = n.params.get("mode").and_then(|v| v.as_str()).unwrap_or("delay_false").to_string();
        let time = n.params.get("time").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let unit = n.params.get("unit").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
        (mode, time, unit)
    }).unwrap_or_default();

    let mut mode = mode;
    let mut time = time as f32;
    let mut unit = unit;
    let mut changed = false;

    let r1 = ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut mode, "delay_true".into(),  egui::RichText::new("Delay ON").small()).changed();
        changed |= ui.selectable_value(&mut mode, "delay_false".into(), egui::RichText::new("Delay OFF").small()).changed();
    });
    let r2 = ui.horizontal(|ui| {
        let limit = if unit == "ms" { 60_000.0 } else { 10_000.0 };
        changed |= ui.add(egui::DragValue::new(&mut time).speed(1.0).range(0.0..=limit)).changed();
        changed |= ui.selectable_value(&mut unit, "ms".into(),      egui::RichText::new("ms").small()).changed();
        changed |= ui.selectable_value(&mut unit, "samples".into(), egui::RichText::new("frames").small()).changed();
    });
    register_exposable_element(ui, node_id, "mode", r1.response.rect);
    register_exposable_element(ui, node_id, "time", r2.response.rect);

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("mode".into(), Value::String(mode));
            node.params.insert("unit".into(), Value::String(unit));
            if let Some(n) = Number::from_f64(time as f64) {
                node.params.insert("time".into(), Value::Number(n));
            }
        }
    }
}

/// Inverse (module id `math.negate`): bipolar `-v` by default, or a unipolar
/// mirror inside `0..max` when the checkbox is ticked. The max box is greyed
/// out while unipolar is off since it has no effect there.
pub(crate) fn show_inverse_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (unipolar, max) = snarl.get_node(node_id).map(|n| {
        let unipolar = n.params.get("unipolar").and_then(|v| v.as_bool()).unwrap_or(false);
        let max = n.params.get("unipolar_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        (unipolar, max)
    }).unwrap_or((false, 1.0));

    let mut unipolar = unipolar;
    let mut max = max;
    let mut changed = false;

    let r = ui.vertical(|ui| {
        changed |= ui
            .checkbox(&mut unipolar, egui::RichText::new("Unipolar").small())
            .on_hover_text("Mirror the signal inside 0..max instead of flipping its sign")
            .changed();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(unipolar, |ui| {
                ui.label(egui::RichText::new("max").small());
                changed |= ui
                    .add(egui::DragValue::new(&mut max).speed(0.01).range(0.0..=1000.0))
                    .on_hover_text("Input value that maps to 0; also the output at input 0")
                    .changed();
            });
        });
    });
    register_exposable_element(ui, node_id, "unipolar", r.response.rect);

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("unipolar".into(), Value::Bool(unipolar));
            if let Some(n) = Number::from_f64(max as f64) {
                node.params.insert("unipolar_max".into(), Value::Number(n));
            }
        }
    }
}

/// Quantize (`math.quantize`): grid factor + rounding mode. The factor box is
/// greyed out while the Factor pin is wired, since the wire wins.
pub(crate) fn show_quantize_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    let (factor, mode) = snarl.get_node(node_id).map(|n| {
        let f = n.params.get("factor").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let m = n.params.get("mode").and_then(|v| v.as_str()).unwrap_or("round").to_string();
        (f, m)
    }).unwrap_or((1.0, "round".to_string()));

    let factor_wired = inputs.get(1).map(|p| !p.remotes.is_empty()).unwrap_or(false);

    let mut factor = factor;
    let mut mode = mode;
    let mut changed = false;

    let r1 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("factor").small());
        ui.add_enabled_ui(!factor_wired, |ui| {
            changed |= ui
                .add(egui::DragValue::new(&mut factor).speed(0.05).range(0.0..=10_000.0))
                .on_hover_text("Steps per unit: 1 = integers, 2 = halves, 4 = quarters")
                .changed();
        });
        if factor_wired {
            ui.label(egui::RichText::new("(wired)").small().weak());
        }
    });
    let r2 = ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut mode, "round".into(), egui::RichText::new("Round").small()).changed();
        changed |= ui.selectable_value(&mut mode, "floor".into(), egui::RichText::new("Floor").small()).changed();
        changed |= ui.selectable_value(&mut mode, "ceil".into(),  egui::RichText::new("Ceil").small()).changed();
        changed |= ui.selectable_value(&mut mode, "trunc".into(), egui::RichText::new("Trunc").small()).changed();
    });
    register_exposable_element(ui, node_id, "factor", r1.response.rect);
    register_exposable_element(ui, node_id, "mode",   r2.response.rect);

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("mode".into(), Value::String(mode));
            if let Some(n) = Number::from_f64(factor as f64) {
                node.params.insert("factor".into(), Value::Number(n));
            }
        }
    }
}

/// Vec to Deflection (`module.vec_to_deflection`): pick the Angle output's unit.
pub(crate) fn show_vec_to_deflection_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let degrees = snarl.get_node(node_id)
        .and_then(|n| n.params.get("degrees").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut degrees = degrees;
    let mut changed = false;

    let r = ui.horizontal(|ui| {
        changed |= ui
            .selectable_value(&mut degrees, false, egui::RichText::new("0..1").small())
            .on_hover_text("Angle as a fraction of a full turn, wrapping to 0 at 1")
            .changed();
        changed |= ui
            .selectable_value(&mut degrees, true, egui::RichText::new("0..360°").small())
            .on_hover_text("Angle in degrees, wrapping to 0 at 360")
            .changed();
    });
    register_exposable_element(ui, node_id, "angle_unit", r.response.rect);

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("degrees".into(), Value::Bool(degrees));
        }
    }
}

pub(crate) fn show_or_equal_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let or_equal = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("or_equal").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut v = or_equal;
    if ui.checkbox(&mut v, egui::RichText::new("or equal").small()).changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("or_equal".to_string(), Value::Bool(v));
        }
    }
}

pub(crate) fn show_knob_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (value, bipolar) = snarl.get_node(node_id).map(|n| {
        let v = n.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
        (v, b)
    }).unwrap_or((0.0, false));

    let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
    let mut v = value.clamp(lo, hi);
    let mut bipolar = bipolar;
    let mut value_changed = false;
    let mut mode_changed = false;

    let mut knob_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        egui::Resize::default()
            .id_salt(("knob", node_id))
            .default_size([80.0, 80.0])
            .min_size([40.0, 30.0])
            .show(ui, |ui| {
                let available = ui.available_size();
                let aspect = available.x / available.y.max(1.0);
                let (rect, resp) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
                knob_rect = Some(rect);

                if resp.double_clicked() {
                    v = 0.0f32.clamp(lo, hi);
                    value_changed = true;
                } else if resp.dragged() {
                    let delta = resp.drag_delta();
                    let range = hi - lo;
                    let norm_delta = if aspect >= 2.0 {
                        delta.x / rect.width()
                    } else {
                        -delta.y / rect.height()
                    };
                    v = (v + norm_delta * range).clamp(lo, hi);
                    value_changed = true;
                }

                if resp.hovered() {
                    let scroll = ui.input(|i| i.raw_scroll_delta.y);
                    if scroll != 0.0 {
                        v = (v + scroll * 0.005 * (hi - lo)).clamp(lo, hi);
                        value_changed = true;
                    }
                }

                let t = (v - lo) / (hi - lo);
                let painter = ui.painter_at(rect);
                let active = resp.hovered() || resp.dragged();

                if aspect >= 2.0 {
                    draw_knob_h_fader(&painter, rect, t, bipolar, active);
                } else if aspect <= 0.5 {
                    draw_knob_v_fader(&painter, rect, t, bipolar, active);
                } else {
                    draw_knob_rotary(&painter, rect, t, bipolar, active);
                }
            });

        ui.horizontal(|ui| {
            mode_changed |= ui.selectable_value(&mut bipolar, false, egui::RichText::new("Uni").small()).changed();
            mode_changed |= ui.selectable_value(&mut bipolar, true,  egui::RichText::new("Bi").small()).changed();
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("{:.3}", v)).small().weak().monospace());
        });
    });
    if let Some(rect) = knob_rect {
        register_exposable_element(ui, node_id, "value", rect);
    }

    if value_changed || mode_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if mode_changed {
                node.params.insert("bipolar".to_string(), Value::Bool(bipolar));
                let new_lo = if bipolar { -1.0f64 } else { 0.0f64 };
                let cur_v = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if let Some(n) = Number::from_f64(cur_v.clamp(new_lo, 1.0)) {
                    node.params.insert("value".to_string(), Value::Number(n));
                }
            }
            if value_changed {
                if let Some(n) = Number::from_f64(v as f64) {
                    node.params.insert("value".to_string(), Value::Number(n));
                }
            }
        }
    }
}

// Knob angle mapping: 135° screen angle (lower-left / ~7 o'clock) sweeping
// 270° clockwise to ~45° (lower-right / ~5 o'clock). t=0 → min, t=1 → max.
pub(crate) fn knob_angle_rad(t: f32) -> f32 {
    (135.0_f32 + t * 270.0_f32).to_radians()
}

pub(crate) fn draw_knob_rotary(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.5 - 4.0).max(10.0);
    let track_r = radius * 0.72;
    let n = 64usize;

    painter.circle_filled(center, radius, Color32::from_gray(30));
    painter.circle_stroke(center, radius, egui::Stroke::new(1.0, Color32::from_gray(55)));

    let arc_pts = |t0: f32, t1: f32| -> Vec<egui::Pos2> {
        (0..=n).map(|i| {
            let ti = t0 + (i as f32 / n as f32) * (t1 - t0);
            let a = knob_angle_rad(ti);
            egui::pos2(center.x + track_r * a.cos(), center.y + track_r * a.sin())
        }).collect()
    };

    // Background track
    painter.add(egui::Shape::line(arc_pts(0.0, 1.0), egui::Stroke::new(3.0, Color32::from_gray(50))));

    // Value arc
    let accent = if bipolar { Color32::from_rgb(100, 150, 255) } else { Color32::from_rgb(80, 200, 120) };
    let (a0, a1) = if bipolar { if t >= 0.5 { (0.5, t) } else { (t, 0.5) } } else { (0.0, t) };
    if (a1 - a0).abs() > 0.001 {
        painter.add(egui::Shape::line(arc_pts(a0, a1), egui::Stroke::new(3.0, accent)));
    }

    // Indicator
    let va = knob_angle_rad(t);
    painter.line_segment(
        [
            egui::pos2(center.x + radius * 0.18 * va.cos(), center.y + radius * 0.18 * va.sin()),
            egui::pos2(center.x + radius * 0.62 * va.cos(), center.y + radius * 0.62 * va.sin()),
        ],
        egui::Stroke::new(2.0, if active { Color32::WHITE } else { Color32::from_gray(200) }),
    );
    painter.circle_filled(center, 2.5, Color32::from_gray(85));
}

pub(crate) fn draw_knob_h_fader(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    let margin = 8.0f32;
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width() - 2.0 * margin, 6.0),
    );
    painter.rect_filled(track_rect, 3.0, Color32::from_gray(40));

    let handle_x = track_rect.left() + t * track_rect.width();
    let accent = if bipolar { Color32::from_rgb(100, 150, 255) } else { Color32::from_rgb(80, 200, 120) };

    let fill = if bipolar {
        let cx = track_rect.center().x;
        if handle_x >= cx {
            egui::Rect::from_min_max(egui::pos2(cx, track_rect.top()), egui::pos2(handle_x, track_rect.bottom()))
        } else {
            egui::Rect::from_min_max(egui::pos2(handle_x, track_rect.top()), egui::pos2(cx, track_rect.bottom()))
        }
    } else {
        egui::Rect::from_min_max(track_rect.min, egui::pos2(handle_x, track_rect.bottom()))
    };
    painter.rect_filled(fill, 0.0, accent);

    if bipolar {
        let cx = track_rect.center().x;
        painter.line_segment(
            [egui::pos2(cx, track_rect.top() - 2.0), egui::pos2(cx, track_rect.bottom() + 2.0)],
            egui::Stroke::new(1.0, Color32::from_gray(110)),
        );
    }

    let r = if active { 7.0 } else { 5.0 };
    painter.circle_filled(
        egui::pos2(handle_x, rect.center().y), r,
        if active { Color32::WHITE } else { Color32::from_gray(200) },
    );
}

pub(crate) fn draw_knob_v_fader(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    let margin = 8.0f32;
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(6.0, rect.height() - 2.0 * margin),
    );
    painter.rect_filled(track_rect, 3.0, Color32::from_gray(40));

    // t=1 → top of track, t=0 → bottom
    let handle_y = track_rect.bottom() - t * track_rect.height();
    let accent = if bipolar { Color32::from_rgb(100, 150, 255) } else { Color32::from_rgb(80, 200, 120) };

    let fill = if bipolar {
        let cy = track_rect.center().y;
        if handle_y <= cy {
            egui::Rect::from_min_max(egui::pos2(track_rect.left(), handle_y), egui::pos2(track_rect.right(), cy))
        } else {
            egui::Rect::from_min_max(egui::pos2(track_rect.left(), cy), egui::pos2(track_rect.right(), handle_y))
        }
    } else {
        egui::Rect::from_min_max(egui::pos2(track_rect.left(), handle_y), track_rect.max)
    };
    painter.rect_filled(fill, 0.0, accent);

    if bipolar {
        let cy = track_rect.center().y;
        painter.line_segment(
            [egui::pos2(track_rect.left() - 2.0, cy), egui::pos2(track_rect.right() + 2.0, cy)],
            egui::Stroke::new(1.0, Color32::from_gray(110)),
        );
    }

    let r = if active { 7.0 } else { 5.0 };
    painter.circle_filled(
        egui::pos2(rect.center().x, handle_y), r,
        if active { Color32::WHITE } else { Color32::from_gray(200) },
    );
}

// ── clear_unused helper ───────────────────────────────────────────────────────

pub(crate) fn clear_unused_inputs(
    node_id: NodeId,
    inputs: &[InPin],
    fixed_count: usize,
    snarl: &mut Snarl<NodeData>,
) {
    let connected_removable: Vec<(usize, Vec<OutPinId>)> = inputs
        .iter()
        .skip(fixed_count)
        .filter(|p| !p.remotes.is_empty())
        .map(|p| (p.id.input, p.remotes.clone()))
        .collect();

    for pin in inputs.iter().skip(fixed_count) {
        snarl.drop_inputs(InPinId { node: node_id, input: pin.id.input });
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept_pins: Vec<_> = connected_removable
            .iter()
            .map(|(idx, _)| node.inputs[*idx].clone())
            .collect();
        let kept_ids: Vec<_> = if let Some(Value::Array(ids)) = node.params.get("input_pin_ids") {
            connected_removable.iter()
                .map(|(idx, _)| ids.get(*idx).cloned().unwrap_or(Value::String(String::new())))
                .collect()
        } else {
            vec![]
        };

        node.inputs.truncate(fixed_count);
        node.inputs.extend(kept_pins);

        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
            ids.truncate(fixed_count);
            ids.extend(kept_ids);
        }
    } else {
        return;
    }

    for (new_idx, (_, remotes)) in connected_removable.iter().enumerate() {
        let new_pin = InPinId { node: node_id, input: fixed_count + new_idx };
        for &remote in remotes {
            snarl.connect(remote, new_pin);
        }
    }
}

// ── Processing module body renderers ──────────────────────────────────────────

pub(crate) fn show_delay_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let delay_ms = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("delay_ms").and_then(|v| v.as_f64()))
        .unwrap_or(100.0) as f32;
    let mut v = delay_ms;
    let r = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("ms").small());
        if ui
            .add(egui::DragValue::new(&mut v).speed(1.0).range(0.0..=60_000.0))
            .changed()
        {
            if let (Some(node), Some(n)) = (
                snarl.get_node_mut(node_id),
                Number::from_f64(v as f64),
            ) {
                node.params.insert("delay_ms".into(), Value::Number(n));
            }
        }
    });
    register_exposable_element(ui, node_id, "ms", r.response.rect);
    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Ch").small().weak());
        if ui.small_button("+").on_hover_text("Add channel").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len() + 1;
                node.inputs.push(PinDescriptor::new(format!("In {}", next), SignalType::Float));
                node.outputs.push(PinDescriptor::new(format!("Out {}", next), SignalType::Float));
            }
        }
        if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
            remove_input_pin(node_id, n_channels - 1, inputs, snarl);
            remove_output_pin(node_id, n_channels - 1, outputs, snarl);
        }
    });
}

pub(crate) fn show_average_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (buf_size, spike_mad) = snarl
        .get_node(node_id)
        .map(|n| {
            let bs = n.params.get("buf_size").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
            let sm = n.params.get("spike_mad").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            (bs, sm)
        })
        .unwrap_or((10.0, 0.0));

    let mut bs = buf_size;
    let mut sm = spike_mad;
    let mut changed = false;

    let r1 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Samples").small());
        changed |= ui.add(egui::DragValue::new(&mut bs).speed(1.0).range(1.0..=10_000.0)).changed();
    });
    let r2 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Spike MAD").small())
            .on_hover_text("Outlier threshold in median absolute deviations. 0 = off. Try 3.0 to start.");
        changed |= ui.add(egui::DragValue::new(&mut sm).speed(0.1).range(0.0..=20.0).max_decimals(1)).changed();
    });
    register_exposable_element(ui, node_id, "samples",   r1.response.rect);
    register_exposable_element(ui, node_id, "spike_mad", r2.response.rect);

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(bs as f64) { node.params.insert("buf_size".into(),  Value::Number(n)); }
            if let Some(n) = Number::from_f64(sm as f64) { node.params.insert("spike_mad".into(), Value::Number(n)); }
        }
    }

    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Ch").small().weak());
        if ui.small_button("+").on_hover_text("Add channel").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len() + 1;
                node.inputs.push(PinDescriptor::new(format!("In {}", next), SignalType::Float));
                node.outputs.push(PinDescriptor::new(format!("Out {}", next), SignalType::Float));
            }
        }
        if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
            remove_input_pin(node_id, n_channels - 1, inputs, snarl);
            remove_output_pin(node_id, n_channels - 1, outputs, snarl);
        }
    });
}

pub(crate) fn show_dc_filter_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (window_ms, decay_ms) = snarl
        .get_node(node_id)
        .map(|n| {
            let w = n.params.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
            let d = n.params.get("decay_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
            (w, d)
        })
        .unwrap_or((500.0, 200.0));

    let mut w = window_ms;
    let mut d = decay_ms;
    let mut changed = false;

    let r1 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Window ms").small());
        changed |= ui.add(egui::DragValue::new(&mut w).speed(10.0).range(10.0..=60_000.0)).changed();
    });
    let r2 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Decay ms").small());
        changed |= ui.add(egui::DragValue::new(&mut d).speed(10.0).range(10.0..=60_000.0)).changed();
    });
    register_exposable_element(ui, node_id, "window_ms", r1.response.rect);
    register_exposable_element(ui, node_id, "decay_ms",  r2.response.rect);

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(w as f64) { node.params.insert("window_ms".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(d as f64) { node.params.insert("decay_ms".into(),  Value::Number(n)); }
        }
    }

    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Ch").small().weak());
        if ui.small_button("+").on_hover_text("Add channel").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len() + 1;
                node.inputs.push(PinDescriptor::new(format!("In {}", next), SignalType::Float));
                node.outputs.push(PinDescriptor::new(format!("Out {}", next), SignalType::Float));
            }
        }
        if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
            remove_input_pin(node_id, n_channels - 1, inputs, snarl);
            remove_output_pin(node_id, n_channels - 1, outputs, snarl);
        }
    });
}
