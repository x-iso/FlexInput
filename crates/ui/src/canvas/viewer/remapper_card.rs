//! Pixel-accurate mapping card renderer shared by Remapper / Map Action /
//! Touch Zones / Lean card lists.

use super::*;

/// Pixel-accurate mapping card outcome.
pub(crate) struct MappingCardResult {
    /// True if the delete (×) button was clicked.
    pub(crate) delete_clicked: bool,
    /// True if the mapping object was modified by any header control.
    pub(crate) changed: bool,
    /// Drag interaction on the card *body* (the area below the header strip).
    /// Used by the caller's reorder state machine. `None` when reordering is
    /// disabled for this card.
    pub(crate) body_drag: Option<egui::Response>,
    /// The card's full on-screen rect (origin + size) at its natural (un-lifted)
    /// position, in the current UI's coordinate space.
    pub(crate) rect: egui::Rect,
}

/// Render a mapping card pixel-accurate to Figma node 358:2 (Frame 1 → Group 1).
///
/// Card dimensions: 358×102 (card_w × CARD_H below).
///   Header strip (y=0..31): controls all at y=5, h=20:
///     × (5,5,20×20) | mode (33,5,20×20) | time gap (61,5,137×20)
///                   | hold (206,5,62×20) | turbo (274,5,80×20)
///   Body strip (y=31..102, fill #3C3C3C):
///     in chip   (5,40,48×20)   chord chips start at (62,37), 26×26, pitch 32
///     out chip  (5,72,48×20)   chord chips start at (62,69)
///
/// All sizes are in Figma px and rendered 1:1. Drawn at the current cursor
/// position; the caller is responsible for sizing its container to fit.
pub(crate) fn remapper_mapping_card_pixel(
    ui: &mut egui::Ui,
    node_id: NodeId,
    mapping_idx: usize,
    mapping: &mut serde_json::Map<String, Value>,
    in_pins: &[String],
    out_pins: Option<&[String]>,    // None → Map Action variant (single row)
    skin: crate::canvas::remapper_icons::Skin,
    allow_analog_mode: bool,        // true for Lean cards and Remapper/Map Action (since analog support added)
    reorder_enabled: bool,          // sense a drag on the body for reorder
    drag_offset_y: f32,             // visual lift (paint offset) while dragging this card
    nav_scope: &str,                // nav-temp key scope: "mappings" / "lean_left" / "lean_right"
                                    // — disambiguates the two Lean lists sharing one node
    has_curve_below: bool,          // a response-curve section is rendered flush below this card
                                    // → square the bottom corners so the two read as ONE card
    conflict: Option<&CardConflict>, // Some → another card also drives one of this card's out
                                     // pins; paint a ⚠ badge + amber outline (Issue: silent override)
) -> MappingCardResult {
    // ── Figma palette ─────────────────────────────────────────────────────
    const C_CARD_BG:   Color32 = Color32::from_rgb(0x2D, 0x2D, 0x2D);  // outer
    const C_BORDER:    Color32 = Color32::BLACK;
    const C_BODY_BG:   Color32 = Color32::from_rgb(0x3C, 0x3C, 0x3C);  // body
    const C_PILL_DARK: Color32 = Color32::from_rgb(0x1B, 0x1B, 0x1B);  // time-gap left
    const C_PILL_MID:  Color32 = Color32::from_rgb(0x4A, 0x4A, 0x4A);  // value box / toggle pill
    const C_CHECK_BG:  Color32 = Color32::from_rgb(0xD9, 0xD9, 0xD9);  // checkbox
    const C_INOUT_BG:  Color32 = Color32::from_rgb(0x76, 0x76, 0x76);  // in/out chip
    const C_TEXT:      Color32 = Color32::WHITE;

    // Card width is parameterized to fill the parent body. The mockup card
    // is 358 wide at design scale; we scale all internal positions by the
    // ratio of actual to design width so the layout stays pixel-correct.
    const DESIGN_W: f32 = 358.0;
    const RADIUS: f32 = 5.0;
    const TEXT_SIZE_HEADER: f32 = 13.0;
    const TEXT_SIZE_INOUT:  f32 = 11.0;
    const TEXT_SIZE_VALUE:  f32 = 13.0;
    // Use available width capped at design size + leave space for the
    // module's scrollbar on the right. The parent body is REMAP_DESIGN_W
    // wide; the scrollbar takes ~7px on the right, plus we want a small
    // visual gap.
    let card_w = ui.available_width().min(DESIGN_W).max(280.0);
    let _ = TEXT_SIZE_VALUE;

    let s = card_w / DESIGN_W;

    // Measure chord rows so the card can grow vertically when a chord wraps.
    // chip_size = 26*s, gap = 6*s, plus = 12*s; row width budget is from
    // `chord_x_start` (62*s) to `card_w - 5*s` on the right edge.
    let chip_size = 26.0 * s;
    let chord_gap = 6.0 * s;
    let plus_w = 12.0 * s;
    let chord_x_start = 62.0 * s;
    let chord_avail_w = (card_w - 5.0 * s) - chord_x_start;
    let row_pitch_y = (chip_size + 6.0 * s).max(32.0 * s);

    let measure_rows = |pins: &[String]| -> usize {
        let mut rows = 1usize;
        let mut x = 0.0f32;
        let mut first = true;
        for p in pins {
            if matches!(p.as_str(), "touchpad_any") { continue; }
            let next_w = chip_size + if first { 0.0 } else { plus_w + chord_gap };
            if !first && x + next_w > chord_avail_w {
                rows += 1;
                x = chip_size + chord_gap; // next row starts with this chip
            } else {
                x += next_w;
                if !first { /* already counted */ }
                x += chord_gap;
            }
            first = false;
        }
        rows.max(1)
    };

    let in_rows  = measure_rows(in_pins);
    let out_rows = out_pins.map(measure_rows).unwrap_or(0);

    // Header strip is 31px; each chord row reserves `row_pitch_y` of body
    // space; bottom padding ~8px. Map Action has no out row.
    let header_h = 31.0 * s;
    let bottom_pad = 8.0 * s;
    let body_h = in_rows as f32 * row_pitch_y
        + if out_pins.is_some() { out_rows as f32 * row_pitch_y } else { 0.0 }
        + bottom_pad;
    let card_h = header_h + body_h;
    let (natural_rect, _) = ui.allocate_exact_size(
        egui::vec2(card_w, card_h),
        egui::Sense::hover(),
    );
    // While this card is being dragged for reorder, lift its *painted* and
    // *interactive* geometry by `drag_offset_y` so it visually follows the
    // pointer, while the layout slot it vacated stays reserved (the caller
    // opens the insertion gap elsewhere). Layout-affecting code keeps using
    // `natural_rect`; everything visual uses the lifted `card_rect`.
    let card_rect = natural_rect.translate(egui::vec2(0.0, drag_offset_y));
    let card_origin = card_rect.min;
    // Intersect with the parent's clip rect so we don't paint outside the
    // body's visible band (otherwise layout-mode preview leaks card shapes
    // above/below the container, since visual-transform doesn't clip
    // descendant painter shapes).
    let painter_clip = ui.clip_rect().intersect(card_rect);
    let painter = ui.painter().with_clip_rect(painter_clip);

    // ── Paint outer card + body fill ──────────────────────────────────────
    // When a response-curve section is drawn flush below, square the bottom
    // corners (both outer frame + body fill) so the section's frame closes the
    // card off — the two share one continuous border.
    let radius_i = RADIUS as u8;
    let outer_cr = if has_curve_below {
        egui::CornerRadius { nw: radius_i, ne: radius_i, sw: 0, se: 0 }
    } else {
        egui::CornerRadius::same(radius_i)
    };
    painter.rect(
        card_rect,
        outer_cr,
        C_CARD_BG,
        egui::Stroke::new(1.0, C_BORDER),
        egui::epaint::StrokeKind::Inside,
    );
    // Body fills the bottom portion (header strip is whatever sits above).
    let body_top_y = 31.0 * s;
    let body_rect = egui::Rect::from_min_max(
        card_origin + egui::vec2(0.0, body_top_y),
        card_origin + egui::vec2(card_w, card_h),
    );
    let body_cr = if has_curve_below { egui::CornerRadius::ZERO } else { egui::CornerRadius::same(radius_i) };
    painter.rect_filled(body_rect, body_cr, C_BODY_BG);
    // Square off the body's top corners by overpainting the top edge with a
    // small rect — the rounded radius lives only on the bottom of the card.
    painter.rect_filled(
        egui::Rect::from_min_size(body_rect.min, egui::vec2(card_w, RADIUS)),
        0.0,
        C_BODY_BG,
    );

    // Helpers: `at` scales a (Figma x,y) into painter space; `sz` scales a
    // (Figma w,h) size. `s` is the design-to-actual scale factor (see above).
    let at = |x: f32, y: f32| card_origin + egui::vec2(x * s, y * s);
    let sz = |w: f32, h: f32| egui::vec2(w * s, h * s);

    let mut changed = false;
    let mut delete_clicked = false;

    // ── Gamepad-nav selection state for this card ───────────────────────────
    // The nav driver publishes (pass, selected_idx, entered) keyed by node id,
    // and (pass, field) for the focused header field. We glow the selected card
    // and (when entered) the focused field; field rects are captured below as
    // each header control is laid out: [press-mode, time-gap, hold, turbo].
    // Viewport-agnostic nav pass so the card highlight shows in the config
    // overlay's own viewport (see `crate::widgets::nav_pass`). Distinct from the
    // local `pass` used below to publish the card RECTS (a same-viewport channel).
    let cur_pass = crate::widgets::nav_pass(ui.ctx());
    let (nav_card_sel, nav_card_entered) = ui.ctx()
        .data(|d| d.get_temp::<(u64, usize, bool)>(egui::Id::new(("gp_nav_remap_card", node_id.0, nav_scope))))
        .filter(|(p, _, _)| cur_pass.saturating_sub(*p) <= 1)
        .map(|(_, i, e)| (Some(i), e))
        .unwrap_or((None, false));
    let nav_card_field: Option<u64> = ui.ctx()
        .data(|d| d.get_temp::<(u64, u64)>(egui::Id::new(("gp_nav_remap_card_field", node_id.0, nav_scope))))
        .filter(|(p, _)| cur_pass.saturating_sub(*p) <= 1)
        .map(|(_, f)| f);
    let nav_this = nav_card_sel == Some(mapping_idx);
    let mut nav_field_rects = [egui::Rect::NOTHING; 4];

    // Helper to paint a button background with idle + hover states. Matches
    // the visual weight of the header pills so × and mode read as buttons.
    let paint_button_bg = |painter: &egui::Painter, r: egui::Rect, hovered: bool| {
        painter.rect_filled(r, 3.0, C_PILL_MID); // idle fill
        if hovered {
            painter.rect_filled(r, 3.0, Color32::from_white_alpha(28));
        }
    };

    // ── × delete button: (5,5,20×20) ───────────────────────────────────────
    {
        let r = egui::Rect::from_min_size(at(5.0, 5.0), sz(20.0, 20.0));
        let resp = ui.interact(r, ui.id().with(("del", mapping_idx)), egui::Sense::click());
        paint_button_bg(&painter, r, resp.hovered());
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(16.0 * s),
            C_TEXT,
        );
        if resp.clicked() { delete_clicked = true; }
    }

    // ── Press-mode glyph button: (33,5,20×20) ─────────────────────────────
    let mode_now = mapping.get("mode").and_then(|v| v.as_str()).unwrap_or("down").to_string();
    {
        let glyph = remapper_press_mode_glyph(&mode_now);
        let r = egui::Rect::from_min_size(at(33.0, 5.0), sz(20.0, 20.0));
        nav_field_rects[0] = r;
        let resp = ui.interact(r, ui.id().with(("pm", mapping_idx)),
            egui::Sense::click()).on_hover_text(
                format!("Press mode: {}", remapper_press_mode_label(&mode_now)));
        paint_button_bg(&painter, r, resp.hovered());
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(15.0 * s),
            C_TEXT,
        );
        // Scoped via ui.id() so it inherits the caller's push_id (e.g. each
        // lean section pushes (side, idx) — without this, Lean Left[0] and
        // Lean Right[0] popups collide on the same global id.
        let popup_id = ui.id().with(("fxi_press_mode_popup", mapping_idx));
        if resp.clicked() { egui::Popup::toggle_id(ui.ctx(), popup_id); }
        let mut picked: Option<&'static str> = None;
        popup_below_widget(
            &resp, popup_id,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(140.0);
                let mut options: Vec<(&'static str, &'static str, &'static str)> = vec![
                    ("down",       "↓", "Normal (gate)"),
                    ("short",      "↕", "Short press"),
                    ("long",       "⇓", "Long press"),
                    ("double",     "↡", "Double tap"),
                    ("on_press",   "↧", "On press"),
                    ("on_release", "↥", "On release"),
                ];
                if allow_analog_mode {
                    options.push(("analog", "∿", "Analog"));
                }
                for (val, g, label) in options {
                    if ui.selectable_label(mode_now == val,
                        format!("{g}  {label}")).clicked() { picked = Some(val); }
                }
            },
        );
        if let Some(new_mode) = picked {
            if new_mode == "down" {
                mapping.remove("mode");
                mapping.remove("window_ms");
                mapping.remove("sustain");
            } else {
                mapping.insert("mode".to_string(), Value::String(new_mode.to_string()));
                if !mapping.contains_key("window_ms") {
                    mapping.insert("window_ms".to_string(), serde_json::json!(200.0));
                }
            }
            changed = true;
            egui::Popup::close_id(ui.ctx(), popup_id);
        }
    }

    // ── time gap pill: outer (61,5,137×20), valuebox (135,5,63×20) ────────
    let turbo_on = mapping.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
    // Modes that read the time-gap value:
    //   short/long/double — window timing; analog — per-tap duration;
    //   on_press/on_release — emitted trigger duration (see apply_press_mode);
    //   any mode with turbo on — turbo period.
    let gap_applies = matches!(mode_now.as_str(),
        "short" | "long" | "double" | "analog" | "on_press" | "on_release") || turbo_on;
    // Turbo is only meaningful for sustained/continuous gates. It's grayed for
    // short, double, and the edge-trigger modes (on_press/on_release) — turbo
    // on a one-shot edge pulse has no sensible meaning.
    let turbo_applies = !matches!(mode_now.as_str(),
        "short" | "double" | "on_press" | "on_release");
    {
        let outer = egui::Rect::from_min_size(at(61.0, 5.0), sz(137.0, 20.0));
        let value_box = egui::Rect::from_min_size(at(61.0 + 74.0, 5.0), sz(63.0, 20.0));
        nav_field_rects[1] = value_box;
        let alpha = if gap_applies { 255 } else { 77 };
        let mul = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
        painter.rect_filled(outer, RADIUS, mul(C_PILL_DARK));
        painter.rect_filled(value_box, RADIUS, mul(C_PILL_MID));
        painter.text(
            at(61.0 + 5.0, 5.0 + 10.0),
            egui::Align2::LEFT_CENTER,
            "time gap",
            egui::FontId::proportional(TEXT_SIZE_HEADER * s),
            mul(C_TEXT),
        );
        // Editable value: a tiny DragValue sized to the value_box.
        let mut gap_ms = mapping.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(value_box)
                .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight)),
        );
        child.add_enabled_ui(gap_applies, |ui| {
            ui.spacing_mut().interact_size.y = 18.0 * s;
            let resp = ui.add(
                egui::DragValue::new(&mut gap_ms)
                    .speed(5.0).range(10.0f32..=5000.0)
                    .custom_formatter(|n, _| format!("{n:.0} ms")),
            );
            if resp.changed() {
                if let Some(n) = serde_json::Number::from_f64(gap_ms as f64) {
                    mapping.insert("window_ms".to_string(), Value::Number(n));
                    changed = true;
                }
            }
        });
    }

    // ── hold pill: (206,5,62×20), checkbox at +(45,3,14×14) ──────────────
    // In `analog` mode, `hold` toggles short-tap vs long-tap pulse trains.
    let hold_applies = mode_now == "long" || mode_now == "analog";
    {
        let outer = egui::Rect::from_min_size(at(206.0, 5.0), sz(62.0, 20.0));
        let cb_rect = egui::Rect::from_min_size(at(206.0 + 45.0, 5.0 + 3.0), sz(14.0, 14.0));
        nav_field_rects[2] = outer;
        let alpha = if hold_applies { 255 } else { 77 };
        let mul = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
        painter.rect_filled(outer, RADIUS, mul(C_PILL_MID));
        painter.text(
            at(206.0 + 5.0, 5.0 + 10.0),
            egui::Align2::LEFT_CENTER,
            "hold",
            egui::FontId::proportional(TEXT_SIZE_HEADER * s),
            mul(C_TEXT),
        );
        painter.rect_filled(cb_rect, 3.0, mul(C_CHECK_BG));
        let mut hold = mapping.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
        if hold {
            painter.rect_filled(cb_rect.shrink(3.0 * s), 1.0, mul(C_PILL_DARK));
        }
        let resp = ui.interact(outer, ui.id().with(("hold", mapping_idx)),
            if hold_applies { egui::Sense::click() } else { egui::Sense::hover() });
        if hold_applies && resp.clicked() {
            hold = !hold;
            mapping.insert("sustain".to_string(), Value::Bool(hold));
            changed = true;
        }
    }

    // ── turbo pill: (274,5,80×20), checkbox at +(63,3,14×14) ──────────────
    {
        let outer = egui::Rect::from_min_size(at(274.0, 5.0), sz(80.0, 20.0));
        let cb_rect = egui::Rect::from_min_size(at(274.0 + 63.0, 5.0 + 3.0), sz(14.0, 14.0));
        nav_field_rects[3] = outer;
        let alpha = if turbo_applies { 255 } else { 77 };
        let mul = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
        painter.rect_filled(outer, RADIUS, mul(C_PILL_MID));
        painter.text(
            at(274.0 + 5.0, 5.0 + 10.0),
            egui::Align2::LEFT_CENTER,
            "turbo",
            egui::FontId::proportional(TEXT_SIZE_HEADER * s),
            mul(C_TEXT),
        );
        painter.rect_filled(cb_rect, 3.0, mul(C_CHECK_BG));
        // Effective turbo is off when the mode doesn't support it; clear a
        // stale stored `turbo:true` so it can't silently affect the engine.
        let mut turbo = turbo_on && turbo_applies;
        if turbo_on && !turbo_applies {
            mapping.remove("turbo");
            changed = true;
        }
        if turbo {
            painter.rect_filled(cb_rect.shrink(3.0 * s), 1.0, mul(C_PILL_DARK));
        }
        let resp = ui.interact(outer, ui.id().with(("turbo", mapping_idx)),
            if turbo_applies { egui::Sense::click() } else { egui::Sense::hover() });
        if turbo_applies && resp.clicked() {
            turbo = !turbo;
            mapping.insert("turbo".to_string(), Value::Bool(turbo));
            changed = true;
        }
    }

    // ── in / out label pill (label + arrow) ───────────────────────────────
    let draw_io_pill = |label: &str, ox: f32, oy: f32| {
        let r = egui::Rect::from_min_size(at(ox, oy), sz(48.0, 20.0));
        painter.rect_filled(r, RADIUS, C_INOUT_BG);
        painter.text(
            at(ox + 4.0, oy + 10.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(TEXT_SIZE_INOUT * s),
            C_TEXT,
        );
        painter.text(
            at(ox + 29.0, oy + 10.0),
            egui::Align2::LEFT_CENTER,
            "→",
            egui::FontId::proportional(13.0 * s),
            C_TEXT,
        );
    };
    draw_io_pill("in", 5.0, 40.0);

    // ── chord chip row (painter-driven, wrap on overflow) ─────────────────
    // Each chip is `chip_size` square. Label pill center is at y=50 (in row)
    // and y=82 (out row); chord chip center matches by anchoring chip top-y
    // = label-pill top-y - 3 (since chip is 6px taller than pill, half of
    // that = 3 puts both centers at the same y).
    let chord_y_in = 37.0 * s;    // = label_y(40) - 3, center at y=50
    let chord_y_out_first = 69.0 * s; // = label_y(72) - 3, center at y=82

    let chord_painter = painter.clone();
    let render_chord_row_painter = |row_y_start: f32, pins: &[String]| {
        let mut cur_x = chord_x_start;
        let mut row_y = row_y_start;
        let mut first = true;
        for p in pins {
            let render_id: &str = match p.as_str() {
                "touchpad_left"   => "touch_left",
                "touchpad_center" => "touch_center",
                "touchpad_right"  => "touch_right",
                "touchpad_any"    => continue, // shown as the click overlay
                other => other,
            };
            // Width this chip will consume (incl. its leading "+", if any).
            let prefix = if first { 0.0 } else { plus_w + chord_gap };
            // Wrap to the next row if this chip doesn't fit. The first chip
            // on a wrapped row drops its "+", since the prior row ended
            // with the trailing chip already.
            if !first && cur_x - chord_x_start + prefix + chip_size > chord_avail_w {
                row_y += row_pitch_y;
                cur_x = chord_x_start;
                first = true;
            }
            if !first {
                chord_painter.text(
                    card_origin + egui::vec2(cur_x + plus_w * 0.5, row_y + chip_size * 0.5),
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::proportional(chip_size * 0.5),
                    Color32::WHITE,
                );
                cur_x += plus_w + chord_gap;
            }
            first = false;
            let chip_top_left = card_origin + egui::vec2(cur_x, row_y);
            let painted_w = paint_chord_chip_to_rect(
                &chord_painter, ui.ctx(), chip_top_left, chip_size, render_id, skin,
            );
            cur_x += painted_w + chord_gap;
        }
    };

    render_chord_row_painter(chord_y_in, in_pins);
    if let Some(out_pins) = out_pins {
        // Out row starts after the in row's actual wrapped height — keep
        // label pill paired with the first chip on the row.
        let out_label_y = 40.0 * s + in_rows as f32 * row_pitch_y - 32.0 * s + 32.0 * s;
        let out_label_design_y = (40.0 + in_rows as f32 * 32.0) / s;
        let _ = out_label_y;
        let _ = out_label_design_y;
        // Simpler: derive out row's start-y from in_rows so wrap pushes
        // the out row down by exactly one row pitch per extra in-row.
        let extra = (in_rows as f32 - 1.0) * row_pitch_y;
        draw_io_pill("out", 5.0, 72.0 + extra / s);
        render_chord_row_painter(chord_y_out_first + extra, out_pins);
    }

    // ── Body drag handle (reorder) ─────────────────────────────────────────
    // The whole body strip (below the 31px header) is the drag handle. The
    // header keeps its own button interactions; body chips are paint-only so
    // there's no conflict. A grab cursor signals the affordance on hover.
    let body_drag = if reorder_enabled {
        let handle_rect = egui::Rect::from_min_max(
            card_origin + egui::vec2(0.0, body_top_y),
            card_rect.max,
        );
        let resp = ui.interact(
            handle_rect,
            ui.id().with(("card_drag", mapping_idx)),
            egui::Sense::click_and_drag(),
        );
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        Some(resp)
    } else {
        None
    };

    // ── Gamepad-nav selection: PUBLISH global rects (do NOT paint here) ──────
    // The remapper body renders inside a child TSTransform layer; painting onto
    // a foreground layer from in here (and re-locking the ctx graphics RwLock
    // mid-paint) deadlocks epaint. Also `card_rect`/field rects are in the child
    // layer's LOCAL space — painting them directly put the glow far off-screen.
    // So we convert to GLOBAL space and publish; the nav driver (top-level,
    // outside any sublayer) draws the glow + handles auto-scroll.
    if nav_this {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let card_g = to_global * card_rect;
        let field_g = nav_card_field
            .and_then(|f| nav_field_rects.get(f as usize).copied())
            .filter(|fr| fr.is_finite() && fr.width() > 0.5)
            .map(|fr| to_global * fr);
        // Publish the card-list viewport (global) too, so the nav driver can CLIP
        // the card glow to it — a tall expanded card scrolled partly out of view
        // gets its ring cropped at the visible edge instead of spilling past it.
        let clip_g = to_global * ui.clip_rect();
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                egui::Id::new(("gp_nav_remap_card_rects", node_id.0, nav_scope)),
                (pass, card_g, field_g, nav_card_entered));
            d.insert_temp(
                egui::Id::new(("gp_nav_remap_viewport", node_id.0, nav_scope)),
                (pass, clip_g));
        });

        // Auto-scroll: if the selected card is outside the visible band, request
        // a body-space scroll so it comes into view. This only touches data,
        // never graphics, so it's deadlock-safe.
        let clip = ui.clip_rect();
        let mut need = 0.0f32;
        if card_rect.top() < clip.top() + 4.0 {
            need = card_rect.top() - (clip.top() + 4.0);
        } else if card_rect.bottom() > clip.bottom() - 4.0 {
            need = card_rect.bottom() - (clip.bottom() - 4.0);
        }
        if need.abs() > 1.0 {
            let body_delta = need / s.max(0.01);
            ui.ctx().data_mut(|d| d.insert_temp(
                egui::Id::new(("gp_nav_remap_scroll", node_id.0)),
                (pass, body_delta)));
            request_repaint_throttled(ui.ctx());
        }
    }

    // ── Output-conflict badge ──────────────────────────────────────────────
    // Another card drives one of this card's out pins; the engine's collector
    // merge keeps only one, so the loser silently does nothing. Ring the card
    // amber and drop a ⚠ in the bottom-right (out-row) corner with a tooltip
    // naming the colliding pin(s) + other module(s).
    if let Some(cf) = conflict {
        const C_WARN: Color32 = Color32::from_rgb(0xF2, 0xB0, 0x2E); // amber
        let warn_cr = if has_curve_below {
            egui::CornerRadius { nw: radius_i, ne: radius_i, sw: 0, se: 0 }
        } else {
            egui::CornerRadius::same(radius_i)
        };
        painter.rect_stroke(
            card_rect, warn_cr,
            egui::Stroke::new(1.5, C_WARN),
            egui::epaint::StrokeKind::Inside,
        );
        let bsz = 18.0 * s;
        let badge_rect = egui::Rect::from_min_size(
            card_rect.right_bottom() + egui::vec2(-(bsz + 5.0 * s), -(bsz + 5.0 * s)),
            egui::vec2(bsz, bsz),
        );
        painter.circle_filled(badge_rect.center(), bsz * 0.5, Color32::from_black_alpha(160));
        painter.text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            "⚠",
            egui::FontId::proportional(13.0 * s),
            C_WARN,
        );
        ui.interact(badge_rect, ui.id().with(("conflict", mapping_idx)), egui::Sense::hover())
            .on_hover_text(cf.tooltip());
    }

    MappingCardResult {
        delete_clicked, changed,
        body_drag, rect: natural_rect,
    }
}

/// Render a chord (list of pin ids) as chips separated by "+". When any
/// pin is a click-zone variant, the chord is rewritten so the touchpad
/// click icon appears once at the front, then plain zone chips follow:
///   ["touchpad_left", "touchpad_center"]  →  click + zone_L + zone_C
/// rather than the visually heavier zone+overlay-per-chip form.
pub(crate) fn remapper_render_chord(ui: &mut egui::Ui, pins: &[String], skin: crate::canvas::remapper_icons::Skin) {
    use crate::canvas::remapper_icons::Skin;
    let click_zone = |p: &str| matches!(p,
        "touchpad_left" | "touchpad_center" | "touchpad_right" | "touchpad_any");
    let has_click = pins.iter().any(|p| click_zone(p));
    // Synthetic "click" chip rendered from the click-overlay SVG. Only
    // emitted when the chord actually contains click-zone pins.
    let mut first = true;
    let emit_sep = |ui: &mut egui::Ui, first: &mut bool| {
        if !*first {
            ui.label(egui::RichText::new("+").size(14.0).strong().color(Color32::WHITE));
        }
        *first = false;
    };
    if has_click && skin == Skin::Playstation {
        emit_sep(ui, &mut first);
        // Render touchpad_any's icon (the swipe-down SVG) as the click chip.
        remapper_render_chip(ui, "touchpad_any", skin);
    }
    for p in pins {
        // Substitute click-zone pins with their plain-zone equivalents so
        // the click indicator isn't duplicated on every zone chip.
        let render_id: &str = match p.as_str() {
            "touchpad_left"   => "touch_left",
            "touchpad_center" => "touch_center",
            "touchpad_right"  => "touch_right",
            "touchpad_any"    => continue, // already shown as the click chip
            other => other,
        };
        emit_sep(ui, &mut first);
        remapper_render_chip(ui, render_id, skin);
    }
}
