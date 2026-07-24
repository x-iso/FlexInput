//! The context-sensitive gamepad button legend: which hints apply to the
//! current nav state, and the bottom bar that renders them.

use super::*;

impl FlexInputApp {

    /// Context-sensitive button legend for the current gamepad-nav state.
    /// Returns ordered `(glyphs, label)` hints, where `glyphs` is one or more
    /// gamepad pin ids drawn side-by-side before the label (e.g. LS + D-pad for
    /// "Navigate", LB + RB for "Tab"). Directional helpers (`hint_move`,
    /// `hint_horiz`, `hint_vert`) bundle the stick + matching D-pad glyphs.
    pub(crate) fn gp_legend_hints(&self) -> Vec<(Vec<&'static str>, &'static str)> {
        use crate::gamepad_nav::EditLevel;

        // Stick + D-pad bundles so both navigation methods are advertised.
        // `_move` uses the all-direction glyphs; `_horiz`/`_vert` use the
        // axis-specific (both-arrows) glyphs.
        let hint_move  = || vec!["left_stick", "dpad"];                       // any direction
        let hint_horiz = || vec!["left_stick_horizontal", "dpad_horizontal"]; // left/right axis
        let hint_vert  = || vec!["left_stick_vertical", "dpad_vertical"];     // up/down axis

        // Modal contexts take priority over the sub-patch edit level.
        if self.gamepad_nav.kbm_picker_open {
            return vec![
                (hint_move(), "Move"),
                (vec!["btn_south"], "Add key"),
                (vec!["btn_north"], "Clear chord"),
                (vec!["btn_east"], "Done"),
            ];
        }
        if self.gamepad_nav.settings_open {
            // While a shortcut row is learning, the panel is listening for a
            // combo — show the capture hints (release to bind, East to abort).
            if self.gamepad_nav.chord_learn.is_some() {
                return vec![
                    (vec![], "Hold a 2+ button combo, release to bind"),
                    (vec!["btn_east"], "Press alone: cancel"),
                ];
            }
            return vec![
                (hint_vert(), "Move"),
                (vec!["btn_south"], if self.gamepad_nav.settings_editing { "Apply" } else { "Edit" }),
                (hint_horiz(), "Adjust"),
                (vec!["btn_west"], "Fine"),
                (vec!["btn_north"], "Clear shortcut"),
                (vec!["btn_east"], "Close"),
            ];
        }
        if self.gamepad_nav.alt_tab_active {
            return vec![
                (vec!["right_stick"], "Switch window"),
                (vec!["btn_back"], "Release to commit"),
            ];
        }
        if self.gamepad_nav.preset_nav_open {
            return vec![
                (hint_move(), "Move"),
                (vec!["btn_south"], "Apply preset"),
                (vec!["btn_start"], "Close"),
            ];
        }
        if self.gamepad_nav.press_mode_open {
            return vec![
                (hint_vert(), "Move"),
                (vec!["btn_south"], "Apply"),
                (vec!["btn_east"], "Cancel"),
            ];
        }
        if self.gamepad_nav.left_edit.is_some() {
            return vec![
                (hint_horiz(), "Adjust"),
                (vec!["btn_west"], "Fine"),
                (vec!["btn_north"], "Reset"),
                (vec!["btn_east"], "Done"),
            ];
        }

        match self.gamepad_nav.edit_level {
            EditLevel::Widget => vec![
                (hint_move(), "Navigate"),
                (vec!["right_stick"], "Cursor"),
                (vec!["btn_south", "right_trigger"], "Select / Edit"),
                (vec!["btn_north"], "Show/Hide devices"),
                (vec!["btn_lb", "btn_rb"], "Tab"),
                (vec!["btn_start"], "Presets"),
                (vec!["btn_start"], "Hold: Settings"),
                (vec!["btn_back"], "Alt-Tab"),
                (vec!["btn_ls"], "Undo"),
                (vec!["btn_rs"], "Redo"),
            ],
            EditLevel::Editing => {
                // Row-type (multi-field) widgets split the axes: horizontal =
                // select field, vertical = adjust value. Single-value widgets
                // (knob / constant) adjust on any direction.
                let multi = self.nav_active_outer_id()
                    .map(|o| matches!(self.nav_selected_kind(o),
                        NavWidgetKind::MultiField))
                    .unwrap_or(false);
                if multi {
                    vec![
                        (hint_horiz(), "Select field"),
                        (hint_vert(), "Adjust"),
                        (vec!["btn_south"], "Confirm"),
                        (vec!["btn_west"], "Fine"),
                        (vec!["btn_north"], "Reset"),
                        (vec!["btn_east"], "Back"),
                    ]
                } else {
                    vec![
                        (hint_move(), "Adjust"),
                        (vec!["btn_south"], "Confirm"),
                        (vec!["btn_west"], "Fine"),
                        (vec!["btn_north"], "Reset"),
                        (vec!["btn_east"], "Back"),
                    ]
                }
            }
            EditLevel::CurveDots => vec![
                (hint_move(), "Pick dot"),
                (vec!["btn_south"], "Edit dot"),
                (vec!["right_trigger"], "Add dot"),
                (vec!["left_trigger"], "Delete dot"),
                (vec!["btn_east"], "Back"),
            ],
            EditLevel::CurveDot => vec![
                (hint_move(), "Move dot"),
                (vec!["btn_west"], "Fine"),
                (vec!["btn_east"], "Back"),
            ],
            EditLevel::RemapScroll => vec![
                (hint_move(), "Navigate"),
                (vec!["btn_south"], "Select / Enter"),
                (vec!["btn_north"], "Reset card"),
                (vec!["btn_west"], "Delete card"),
                (vec!["left_trigger", "right_trigger"], "Filter"),
                (vec!["btn_east"], "Back"),
            ],
            EditLevel::RemapCard => vec![
                (hint_horiz(), "Field"),
                (hint_vert(), "Adjust"),
                (vec!["btn_south"], "Toggle / Open"),
                (vec!["btn_north"], "Reset card"),
                (vec!["btn_east"], "Back"),
            ],
            EditLevel::TzLines => {
                // Add/remove only shown in mapping mode (ports mode is move-only).
                let mapping = self.nav_active_outer_id()
                    .and_then(|o| self.nav_selected_inner_node(o).map(|i| self.tz_is_mapping(o, i)))
                    .unwrap_or(false);
                let split = self.nav_active_outer_id()
                    .and_then(|o| self.nav_selected_inner_node(o).map(|i| self.tz_n_fields(o, i) > 1))
                    .unwrap_or(false);
                let mut v = vec![
                    (hint_horiz(), "Col line"),
                    (hint_vert(), "Row line"),
                    (vec!["btn_south"], "Grab"),
                    (vec!["btn_north"], "Recenter"),
                ];
                if mapping {
                    v.push((vec!["btn_west"], "Remove"));
                    v.push((vec!["left_trigger", "right_trigger"], "Add line"));
                }
                if split { v.push((vec!["btn_lb", "btn_rb"], "Pad")); }
                v.push((vec!["btn_east"], "Back"));
                v
            }
            EditLevel::TzGrab => vec![
                (hint_move(), "Move line"),
                (vec!["btn_north"], "Recenter"),
                (vec!["btn_south"], "Drop"),
                (vec!["btn_east"], "Drop"),
            ],
            EditLevel::TzCards => {
                // Two-row nav (actions + cards + optional curve), mirroring the
                // Remapper. West/LT-RT only shown when relevant.
                let ids = self.nav_active_outer_id()
                    .and_then(|o| self.nav_selected_inner_node(o).map(|i| (o, i)));
                let has_mouse = ids.map(|(o, i)| self.nav_tz_has_mouse_card(o, i)).unwrap_or(false);
                let has_analog = ids.map(|(o, i)| self.nav_tz_has_analog_card(o, i)).unwrap_or(false);
                let mut v = vec![
                    (hint_move(), "Navigate"),
                    (vec!["btn_south"], "Select / Enter"),
                    (vec!["btn_west"], "Delete card"),
                    (vec!["btn_lb", "btn_rb"], "Zone"),
                ];
                // LT/RT cycles/nudges whichever value row is focused (tp_mode or
                // mouse_speed); show the hint when either exists.
                if has_analog { v.push((vec!["left_trigger", "right_trigger"], "Touchpad mode")); }
                if has_mouse { v.push((vec!["left_trigger", "right_trigger"], "Mouse speed")); }
                v.push((vec!["btn_east"], "Back"));
                v
            }
        }
    }

    /// Resolve the active Easy sub-patch outer node id (the `subpatch` node in
    /// the active tab), if any. Used by the legend to inspect the selected
    /// widget kind.
    pub(crate) fn nav_active_outer_id(&self) -> Option<egui_snarl::NodeId> {
        self.tabs.get(self.active_tab)?.canvas.snarl
            .nodes_ids_data()
            .find(|(_, n)| n.value.module_id == "subpatch")
            .map(|(id, _)| id)
    }

    /// Legend hint groups for the CONFIG OVERLAY, pre-rasterized to controller
    /// icons so the overlay (a separate viewport, drawn without `&self`) can
    /// paint the SAME bar as Easy mode. While editing a pin the shared
    /// `gp_legend_hints` already returns the right per-state hints (CurveDots,
    /// Editing…); at focus level we show config-specific move/select hints. Empty
    /// when no nav-enabled pad is driving. Each group is `(glyphs, label)`.
    pub(crate) fn config_legend_specs(
        &self,
        ctx: &egui::Context,
    ) -> Vec<(Vec<crate::config_overlay::ConfigGlyph>, String)> {
        use crate::config_overlay::ConfigGlyph;
        let Some(dev) = self.gamepad_nav.active_dev.clone() else { return Vec::new(); };
        let skin = crate::canvas::remapper_icons::skin_from_device_id(&dev);
        let hints: Vec<(Vec<&'static str>, &'static str)> =
            if self.gamepad_nav.edit_level != crate::gamepad_nav::EditLevel::Widget {
                // Editing a pin: reuse the shared per-state hints verbatim.
                self.gp_legend_hints()
            } else {
                // Focus level: move between pins, cursor-pick, enter, exit.
                vec![
                    (vec!["left_stick", "dpad"], "Move"),
                    (vec!["right_stick"], "Cursor"),
                    (vec!["btn_south", "right_trigger"], "Select / Edit"),
                    (vec!["btn_east"], "Exit edit"),
                    (vec!["btn_back"], "Alt-Tab"),
                ]
            };
        hints
            .into_iter()
            .map(|(pins, label)| {
                let glyphs = pins
                    .iter()
                    .map(|pin| match self.gp_legend_glyph(ctx, skin, pin) {
                        Some(t) => ConfigGlyph::Tex(t),
                        None => ConfigGlyph::Token(gp_pin_token(pin).to_string()),
                    })
                    .collect();
                (glyphs, label.to_string())
            })
            .collect()
    }

    /// Bottom legend bar listing the active gamepad's button actions for the
    /// current nav context. Visible only while a nav-enabled gamepad drives the
    /// UI (`active_dev` set this frame by `run_gamepad_nav`).
    pub(crate) fn draw_gp_legend_bar(&self, ctx: &egui::Context) {
        let Some(dev) = self.gamepad_nav.active_dev.clone() else { return; };
        let skin = crate::canvas::remapper_icons::skin_from_device_id(&dev);
        let hints = self.gp_legend_hints();
        if hints.is_empty() { return; }

        egui::TopBottomPanel::bottom("gp_legend_bar")
            .resizable(false)
            .show_separator_line(true)
            .frame(egui::Frame::default()
                .fill(ctx.style().visuals.panel_fill)
                .inner_margin(egui::Margin::symmetric(12, 6)))
            .show(ctx, |ui| {
                // Manual measured flow so each hint (glyphs + shared label) wraps
                // onto the next row AS A UNIT. egui's `horizontal_wrapped` reflows
                // at the individual-widget level and would orphan a glyph from its
                // label; a nested `horizontal` doesn't wrap at all (it overflows +
                // crops). So we measure each hint group's width, greedily pack the
                // groups into rows, and render each row as its own horizontal line.
                // Inter-hint dividers are FIXED-height lines. Sizes are 1.2x base.
                const GLYPH: f32 = 21.6;
                const ITEM_PAD: f32 = 4.8; // around the "/" between multi-glyphs
                const LABEL_GAP: f32 = 2.4; // glyph → label
                const DIV_GAP: f32 = 5.0;   // around the inter-hint divider
                let div_stroke = egui::Stroke::new(1.0, ui.visuals().weak_text_color());
                let label_font = egui::FontId::proportional(14.4);
                let slash_font = egui::FontId::proportional(13.2);
                let token_font = egui::FontId::proportional(14.4);
                let measure = |text: &str, font: &egui::FontId| -> f32 {
                    ui.painter()
                        .layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE)
                        .size().x
                };
                let slash_w = measure("/", &slash_font);

                // ── Build per-hint render specs + measured widths ──
                enum Elem { Tex(egui::TextureHandle), Token(String) }
                struct Spec { elems: Vec<Elem>, label: String, width: f32 }
                let mut specs: Vec<Spec> = Vec::with_capacity(hints.len());
                for (pins, label) in &hints {
                    let mut elems = Vec::new();
                    let mut w = 0.0f32;
                    for (j, pin) in pins.iter().enumerate() {
                        if j > 0 { w += 2.0 * ITEM_PAD + slash_w; }
                        if let Some(tex) = self.gp_legend_glyph(ctx, skin, pin) {
                            w += GLYPH;
                            elems.push(Elem::Tex(tex));
                        } else {
                            let tok = gp_pin_token(pin).to_string();
                            w += measure(&tok, &token_font);
                            elems.push(Elem::Token(tok));
                        }
                    }
                    w += LABEL_GAP + measure(label, &label_font);
                    specs.push(Spec { elems, label: label.to_string(), width: w });
                }

                // ── Greedily pack groups into rows ──
                let div_w = 2.0 * DIV_GAP + 1.0;
                let budget = (ui.available_width() - 4.0).max(1.0);
                let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
                let mut row_w = 0.0f32;
                for (i, s) in specs.iter().enumerate() {
                    let row_empty = rows.last().unwrap().is_empty();
                    let extra = if row_empty { s.width } else { div_w + s.width };
                    if !row_empty && row_w + extra > budget {
                        rows.push(vec![i]);
                        row_w = s.width;
                    } else {
                        rows.last_mut().unwrap().push(i);
                        row_w += extra;
                    }
                }

                // ── Render row by row ──
                ui.vertical(|ui| {
                    for row in &rows {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (k, &hi) in row.iter().enumerate() {
                                if k > 0 {
                                    ui.add_space(DIV_GAP);
                                    let (r, _) = ui.allocate_exact_size(
                                        egui::vec2(1.0, GLYPH), egui::Sense::hover());
                                    ui.painter().vline(
                                        r.center().x, r.top()..=r.bottom(), div_stroke);
                                    ui.add_space(DIV_GAP);
                                }
                                let s = &specs[hi];
                                for (j, elem) in s.elems.iter().enumerate() {
                                    if j > 0 {
                                        ui.add_space(ITEM_PAD);
                                        ui.label(egui::RichText::new("/").size(13.2).weak());
                                        ui.add_space(ITEM_PAD);
                                    }
                                    match elem {
                                        Elem::Tex(tex) => {
                                            ui.add(egui::Image::new(
                                                (tex.id(), egui::vec2(GLYPH, GLYPH))));
                                        }
                                        Elem::Token(tok) => {
                                            ui.label(egui::RichText::new(tok).strong().size(14.4));
                                        }
                                    }
                                }
                                ui.add_space(LABEL_GAP);
                                ui.label(egui::RichText::new(&s.label).size(14.4));
                            }
                        });
                    }
                });
            });
    }

    /// Cached glyph texture for a gamepad button pin under a skin (white-bg-free
    /// SVG rendered with native colors). Cached per (skin, pin) on ctx temp data.
    pub(crate) fn gp_legend_glyph(&self, ctx: &egui::Context,
        skin: crate::canvas::remapper_icons::Skin, pin: &str)
        -> Option<egui::TextureHandle>
    {
        let key = egui::Id::new(("gp_legend_glyph", skin.as_str(), pin));
        if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
            return Some(t);
        }
        let bytes = crate::canvas::remapper_icons::pin_svg(skin, pin)?;
        let svg = std::str::from_utf8(bytes).ok()?;
        // Native colors (transparent tint → recolor pass skipped).
        let img = crate::canvas::viewer::rasterize_svg_recolored(
            svg, 36, 36, "override", egui::Color32::TRANSPARENT)?;
        let t = ctx.load_texture(format!("gp_legend_{}_{}", skin.as_str(), pin),
            img, egui::TextureOptions::LINEAR);
        ctx.data_mut(|d| d.insert_temp(key, t.clone()));
        Some(t)
    }
}
