//! The MIDI editor window — one editor for choosing a MIDI message (the
//! Remapper's MIDI… button) and for tuning one already on a card (a card's MIDI
//! chips), for the mouse and the gamepad alike.
//!
//! It is modal while open: Up/Down walk its rows in order, Left/Right change the
//! focused value, South acts on a button row, East closes. When the config
//! overlay is up it renders OVER THE GAME, in the same always-on-top viewport
//! the KB/M picker uses, because the main window is behind the game there.

use super::*;

use crate::canvas::viewer::{
    midi_add_rows_ui, midi_chip_level, midi_chip_rows, midi_chip_set_level, midi_chip_step,
    midi_chip_value, midi_pick_built, midi_pick_nav_adjust, ring_row, MidiAddRow, MidiChipRow,
    MidiModalPurpose, MidiModalRequest,
};
use crate::gamepad_nav::MidiModal;

/// What the mouse did in the window this frame, applied once `&mut self` is
/// back (the window itself is drawn from `&self`, so it can also render inside
/// another viewport's closure).
#[derive(Default)]
pub(crate) struct MidiModalOutcome {
    close: bool,
    add_input: bool,
    add_output: bool,
    /// JSM insert: write the message's name into the editor.
    insert: bool,
    /// Chip editor: a row stepped by its ◀ / ▶ buttons.
    step: Option<(MidiChipRow, i32)>,
    /// Chip editor: a level dragged to a value.
    set_level: Option<(MidiChipRow, f64)>,
    /// A row clicked, so the pad's focus follows the mouse.
    focus: Option<usize>,
}

impl FlexInputApp {
    pub(crate) fn open_midi_modal(&mut self, req: MidiModalRequest, viewport: Option<egui::ViewportId>) {
        self.gamepad_nav.midi_modal = Some(MidiModal {
            inner: req.inner,
            path: req.path,
            purpose: req.purpose,
            row: 0,
            viewport,
        });
    }

    pub(crate) fn close_midi_modal(&mut self) {
        self.gamepad_nav.midi_modal = None;
    }

    /// The card a chip editor is tuning.
    fn midi_modal_card(&self, m: &MidiModal) -> Option<serde_json::Map<String, serde_json::Value>> {
        let MidiModalPurpose::Chip { card, ref cards_key, .. } = m.purpose else { return None };
        self.picker_node(&m.path, m.inner)?
            .params.get(cards_key)?.as_array()?.get(card)?.as_object().cloned()
    }

    /// Apply `f` to the card a chip editor is tuning, in every live copy of the
    /// node (the tab canvas and any open sub-patch editor).
    fn midi_modal_edit_card(&mut self, m: &MidiModal, f: &dyn Fn(&mut serde_json::Map<String, serde_json::Value>) -> bool) {
        let MidiModalPurpose::Chip { card, ref cards_key, .. } = m.purpose else { return };
        let key = cards_key.clone();
        self.picker_write(&m.path, m.inner, &move |node| {
            if let Some(serde_json::Value::Array(cards)) = node.params.get_mut(&key) {
                if let Some(serde_json::Value::Object(obj)) = cards.get_mut(card) {
                    f(obj);
                }
            }
        });
    }

    /// The rows the pad walks, for the open editor.
    fn midi_modal_row_count(&self, m: &MidiModal) -> usize {
        match &m.purpose {
            MidiModalPurpose::Add => MidiAddRow::ALL.len(),
            // Type, channel, number, then Insert.
            MidiModalPurpose::JsmInsert { .. } => JSM_INSERT_ROW + 1,
            MidiModalPurpose::Chip { side_out, pin_idx, .. } => self.midi_modal_card(m)
                .and_then(|c| {
                    let key = if *side_out { "out" } else { "in" };
                    c.get(key)?.as_array()?.get(*pin_idx)?.as_str()
                        .map(|p| midi_chip_rows(p, *side_out).len())
                })
                .unwrap_or(0),
        }
    }

    /// Which adds the card's current phase allows: an input while the card is
    /// still collecting its trigger, an output once the input is latched — the
    /// same rule as the Remapper body.
    fn midi_modal_add_allowed(&self, m: &MidiModal) -> (bool, bool) {
        let phase = self.picker_target_param_str(&m.path, m.inner, "ui_phase").unwrap_or_default();
        let learning = phase == "learning";
        let latched = learning || phase == "ready_to_learn";
        (!learning, latched)
    }

    /// Add the message the editor builds to the card being drafted.
    fn midi_modal_add(&mut self, ctx: &egui::Context, as_output: bool) {
        let Some(m) = self.gamepad_nav.midi_modal.clone() else { return };
        let Some(pin) = midi_pick_built(ctx, m.inner) else { return };
        let (in_ok, out_ok) = self.midi_modal_add_allowed(&m);
        let id = pin.to_id();
        if as_output {
            // An output has to name one channel and be a message that can be sent.
            if !out_ok || !pin.is_output_capable() { return; }
            let mut draft = self.picker_draft_vec(&m.path, m.inner, "draft_output");
            if !draft.contains(&id) {
                draft.push(id);
                self.picker_set_draft(&m.path, m.inner, "draft_output", &draft);
            }
        } else {
            if !in_ok { return; }
            let mut draft = self.picker_draft_vec(&m.path, m.inner, "draft_input");
            if !draft.contains(&id) {
                draft.push(id);
                self.picker_set_draft(&m.path, m.inner, "draft_input", &draft);
                // Latch it, the way a played chord latches when it is released:
                // left capturing, the body's state machine would replace this
                // draft the moment the pad was touched, and Learn would clear it.
                self.picker_write(&m.path, m.inner, &|node| {
                    node.params.insert("ui_phase".into(), serde_json::Value::from("ready_to_learn"));
                    node.params.insert("_nav_capture_armed".into(), serde_json::Value::Bool(false));
                });
            }
        }
    }

    /// Why the message the editor builds can't be written on this side of a JSM
    /// binding, if it can't.
    fn midi_modal_jsm_problem(ctx: &egui::Context, m: &MidiModal, output: bool) -> Option<&'static str> {
        use flexinput_core::midi::MidiPin;
        let Some(pin) = midi_pick_built(ctx, m.inner) else {
            return Some("Finish the message first (a SysEx needs F0 … F7).");
        };
        match (output, &pin) {
            (true, MidiPin::Bpm | MidiPin::Playing) => {
                Some("That is state a port reports — there is nothing to send.")
            }
            (false, MidiPin::Bpm) => Some("A tempo drives a value, not a binding."),
            (false, MidiPin::SysEx(_)) => Some("A SysEx can be played, not pressed."),
            _ => None,
        }
    }

    /// Write the message's `MIDI_*` name into the JSM editor that asked, and
    /// close. The editor's body puts it in place (`take_jsm_midi_insert`).
    fn midi_modal_jsm_insert(&mut self, ctx: &egui::Context) {
        let Some(m) = self.gamepad_nav.midi_modal.clone() else { return };
        let MidiModalPurpose::JsmInsert { output, cursor } = m.purpose else { return };
        if Self::midi_modal_jsm_problem(ctx, &m, output).is_some() {
            return;
        }
        let Some(pin) = midi_pick_built(ctx, m.inner) else { return };
        let Some(tag) = flexinput_engine::eval::jsm_midi_tag(&pin.to_id()) else { return };
        crate::canvas::viewer::set_jsm_midi_insert(
            ctx,
            &m.path,
            m.inner,
            crate::canvas::viewer::JsmMidiInsert { tag, output, cursor },
        );
        self.close_midi_modal();
    }

    /// Drive the open editor from the pad.
    pub(crate) fn drive_midi_modal(
        &mut self,
        ctx: &egui::Context,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        nav: &crate::gamepad_nav::NavInput,
    ) {
        use crate::gamepad_nav::NavDir;
        let Some(m) = self.gamepad_nav.midi_modal.clone() else { return };
        if nav.is_rising("btn_east") {
            self.close_midi_modal();
            return;
        }
        let rows = self.midi_modal_row_count(&m);
        if rows == 0 {
            // A chip that has nothing left to tune (its card was deleted, or it
            // was retyped): nothing to show, so don't hold the pad.
            self.close_midi_modal();
            return;
        }
        let mut row = m.row.min(rows - 1);
        match step_dir {
            // A list you walk in order: the ends are ends, not a wrap back to
            // the top — that is what made the old inline row feel lost.
            Some(NavDir::Up) => row = row.saturating_sub(1),
            Some(NavDir::Down) => row = (row + 1).min(rows - 1),
            Some(NavDir::Left) | Some(NavDir::Right) => {
                let delta = if step_dir == Some(NavDir::Right) { 1 } else { -1 };
                match &m.purpose {
                    MidiModalPurpose::Add => {
                        if let Some(field) = MidiAddRow::ALL[row].field() {
                            midi_pick_nav_adjust(ctx, m.inner, field, delta);
                        }
                    }
                    MidiModalPurpose::JsmInsert { .. } => {
                        if row < JSM_INSERT_ROW {
                            midi_pick_nav_adjust(ctx, m.inner, row, delta);
                        }
                    }
                    MidiModalPurpose::Chip { side_out, pin_idx, .. } => {
                        let (side_out, pin_idx) = (*side_out, *pin_idx);
                        if let Some(r) = self.midi_modal_chip_row(&m, row) {
                            self.midi_modal_edit_card(&m, &move |c| {
                                midi_chip_step(c, side_out, pin_idx, r, delta)
                            });
                        }
                    }
                }
            }
            None => {}
        }
        if let Some(open) = self.gamepad_nav.midi_modal.as_mut() {
            open.row = row;
        }
        if nav.is_rising("btn_south") {
            match m.purpose {
                MidiModalPurpose::Add => match MidiAddRow::ALL[row] {
                    MidiAddRow::AddInput => self.midi_modal_add(ctx, false),
                    MidiAddRow::AddOutput => self.midi_modal_add(ctx, true),
                    _ => {}
                },
                MidiModalPurpose::JsmInsert { .. } if row == JSM_INSERT_ROW => {
                    self.midi_modal_jsm_insert(ctx);
                }
                _ => {}
            }
        }
    }

    /// The chip row at `row` of a chip editor.
    fn midi_modal_chip_row(&self, m: &MidiModal, row: usize) -> Option<MidiChipRow> {
        let MidiModalPurpose::Chip { side_out, pin_idx, .. } = m.purpose else { return None };
        let card = self.midi_modal_card(m)?;
        let key = if side_out { "out" } else { "in" };
        let pin = card.get(key)?.as_array()?.get(pin_idx)?.as_str()?.to_string();
        midi_chip_rows(&pin, side_out).get(row).copied()
    }

    /// Draw the editor (read-only on `self`) and report what the mouse did.
    pub(crate) fn midi_modal_window(&self, ctx: &egui::Context) -> MidiModalOutcome {
        let mut out = MidiModalOutcome::default();
        let Some(m) = self.gamepad_nav.midi_modal.clone() else { return out };
        let title = match m.purpose {
            MidiModalPurpose::Add => "MIDI message",
            MidiModalPurpose::Chip { side_out: false, .. } => "MIDI input",
            MidiModalPurpose::Chip { side_out: true, .. } => "MIDI output",
            MidiModalPurpose::JsmInsert { output: true, .. } => "MIDI to play",
            MidiModalPurpose::JsmInsert { output: false, .. } => "MIDI that presses",
        };
        let hint = match m.purpose {
            MidiModalPurpose::JsmInsert { .. } => {
                "Up/Down: row   Left/Right: change   South: insert   East: close"
            }
            _ => "Up/Down: row   Left/Right: change   South: add   East: close",
        };
        egui::Window::new(title)
            .id(egui::Id::new("gp_midi_modal"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(hint)
                        .small().color(egui::Color32::from_gray(150)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Done").size(13.0)).clicked() {
                            out.close = true;
                        }
                    });
                });
                ui.add_space(6.0);
                match m.purpose {
                    MidiModalPurpose::Add => self.midi_modal_add_ui(ui, &m, &mut out),
                    MidiModalPurpose::Chip { side_out, pin_idx, .. } => {
                        self.midi_modal_chip_ui(ui, &m, side_out, pin_idx, &mut out)
                    }
                    MidiModalPurpose::JsmInsert { output, .. } => {
                        self.midi_modal_jsm_ui(ui, &m, output, &mut out)
                    }
                }
            });
        ctx.request_repaint();
        out
    }

    fn midi_modal_add_ui(&self, ui: &mut egui::Ui, m: &MidiModal, out: &mut MidiModalOutcome) {
        let focused = MidiAddRow::ALL.get(m.row).copied();
        let rects = midi_add_rows_ui(ui, m.inner, focused);
        for (i, r) in rects.iter().enumerate() {
            if ui.rect_contains_pointer(*r) && ui.input(|i| i.pointer.any_click()) {
                out.focus = Some(i);
            }
        }
        ui.add_space(6.0);
        let built = midi_pick_built(ui.ctx(), m.inner);
        let (in_ok, out_ok) = self.midi_modal_add_allowed(m);
        let out_capable = built.as_ref().is_some_and(|p| p.is_output_capable());
        ui.horizontal(|ui| {
            let resp = ui.add_enabled(in_ok && built.is_some(),
                egui::Button::new(egui::RichText::new("Add as input").size(13.0)))
                .on_disabled_hover_text(if !in_ok {
                    "The output is being learned — the input is settled."
                } else {
                    "Finish the message first (a SysEx needs F0 … F7)."
                });
            if focused == Some(MidiAddRow::AddInput) { ring_row(ui, resp.rect); }
            if resp.clicked() { out.add_input = true; }

            let resp = ui.add_enabled(out_ok && out_capable,
                egui::Button::new(egui::RichText::new("Add as output").size(13.0)))
                .on_disabled_hover_text(if !out_ok {
                    "Latch an input first: learn it, or add one here."
                } else {
                    "An output has to name one channel, and be a message that can be sent."
                });
            if focused == Some(MidiAddRow::AddOutput) { ring_row(ui, resp.rect); }
            if resp.clicked() { out.add_output = true; }
        });
    }

    fn midi_modal_jsm_ui(&self, ui: &mut egui::Ui, m: &MidiModal, output: bool, out: &mut MidiModalOutcome) {
        let focused = MidiAddRow::ALL.get(m.row).copied().filter(|_| m.row < JSM_INSERT_ROW);
        let rects = midi_add_rows_ui(ui, m.inner, focused);
        for (i, r) in rects.iter().enumerate() {
            if ui.rect_contains_pointer(*r) && ui.input(|i| i.pointer.any_click()) {
                out.focus = Some(i);
            }
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(if output {
            "Any channel: the config's MIDI_CHANNEL."
        } else {
            "Any channel: the config's MIDI_IN_CHANNEL."
        }).small().weak());
        ui.add_space(4.0);
        let problem = Self::midi_modal_jsm_problem(ui.ctx(), m, output);
        let tag = midi_pick_built(ui.ctx(), m.inner)
            .and_then(|p| flexinput_engine::eval::jsm_midi_tag(&p.to_id()));
        ui.horizontal(|ui| {
            let resp = ui.add_enabled(problem.is_none() && tag.is_some(),
                egui::Button::new(egui::RichText::new("Insert").size(13.0)))
                .on_disabled_hover_text(problem.unwrap_or("Nothing a config can name."));
            if m.row == JSM_INSERT_ROW { ring_row(ui, resp.rect); }
            if resp.clicked() { out.insert = true; }
            if let Some(t) = &tag {
                ui.label(egui::RichText::new(t).monospace().small());
            }
        });
    }

    fn midi_modal_chip_ui(
        &self,
        ui: &mut egui::Ui,
        m: &MidiModal,
        side_out: bool,
        pin_idx: usize,
        out: &mut MidiModalOutcome,
    ) {
        let Some(card) = self.midi_modal_card(m) else {
            ui.label(egui::RichText::new("This card is gone.").weak());
            return;
        };
        let key = if side_out { "out" } else { "in" };
        let Some(pin_id) = card.get(key).and_then(|v| v.as_array())
            .and_then(|a| a.get(pin_idx)).and_then(|v| v.as_str()).map(str::to_string)
        else {
            ui.label(egui::RichText::new("This chip is gone.").weak());
            return;
        };
        ui.label(egui::RichText::new(crate::canvas::viewer::midi_pin_label(&pin_id)).strong());
        ui.add_space(4.0);
        for (i, row) in midi_chip_rows(&pin_id, side_out).into_iter().enumerate() {
            let resp = ui.horizontal(|ui| {
                ui.add_sized([80.0, 18.0], egui::Label::new(
                    egui::RichText::new(row.label()).small().weak()));
                if ui.small_button("◀").clicked() { out.step = Some((row, -1)); }
                match midi_chip_level(&card, row) {
                    Some(v) => {
                        let mut v = v;
                        if ui.add(egui::DragValue::new(&mut v).speed(0.5)).changed() {
                            out.set_level = Some((row, v));
                        }
                    }
                    None => {
                        ui.add_sized([90.0, 18.0], egui::Label::new(
                            midi_chip_value(&card, side_out, pin_idx, row)));
                    }
                }
                if ui.small_button("▶").clicked() { out.step = Some((row, 1)); }
            }).response;
            if m.row == i { ring_row(ui, resp.rect); }
            if resp.clicked() || (ui.rect_contains_pointer(resp.rect) && ui.input(|i| i.pointer.any_click())) {
                out.focus = Some(i);
            }
        }
        if side_out {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(
                "Levels belong to the card: every note / value it sends uses them.")
                .small().weak());
        }
    }

    /// Apply what the mouse did in the window.
    pub(crate) fn apply_midi_modal_outcome(&mut self, ctx: &egui::Context, out: MidiModalOutcome) {
        let Some(m) = self.gamepad_nav.midi_modal.clone() else { return };
        if let Some(row) = out.focus {
            if let Some(open) = self.gamepad_nav.midi_modal.as_mut() { open.row = row; }
        }
        if out.add_input { self.midi_modal_add(ctx, false); }
        if out.add_output { self.midi_modal_add(ctx, true); }
        if out.insert { self.midi_modal_jsm_insert(ctx); }
        if let MidiModalPurpose::Chip { side_out, pin_idx, .. } = m.purpose {
            if let Some((row, delta)) = out.step {
                self.midi_modal_edit_card(&m, &move |c| midi_chip_step(c, side_out, pin_idx, row, delta));
            }
            if let Some((row, v)) = out.set_level {
                self.midi_modal_edit_card(&m, &move |c| midi_chip_set_level(c, row, v));
            }
        }
        if out.close { self.close_midi_modal(); }
    }

    /// Draw the editor in the surface the user is working in — the routing the
    /// KB/M picker uses. While the config overlay is up the main window is
    /// behind the game, so a window opened without a surface is promoted over
    /// the game. A sub-patch editor draws its own session inside its window
    /// (see `show_subpatch_editors`); if that window has closed, the session
    /// falls back to the main window rather than becoming unreachable.
    pub(crate) fn draw_midi_modal(&mut self, ctx: &egui::Context) {
        let Some(vp) = self.gamepad_nav.midi_modal.as_ref().map(|m| m.viewport) else { return };
        let picker_vp = crate::config_overlay::picker_viewport_id();
        let overlay = crate::config_overlay::config_overlay_visible(ctx);
        let vp = match vp {
            None if overlay => Some(picker_vp),
            // An over-the-game session outlives nothing: the overlay closed.
            Some(v) if v == picker_vp && !overlay => None,
            Some(v) if v != picker_vp => {
                let owner_open = self.sub_patch_editors.iter().any(|e| egui::ViewportId::from_hash_of(
                    ("subpatch_editor", e.tab_idx, e.node_id.0)) == v);
                if owner_open { Some(v) } else { None }
            }
            other => other,
        };
        if let Some(m) = self.gamepad_nav.midi_modal.as_mut() { m.viewport = vp; }
        let out = match vp {
            None => self.midi_modal_window(ctx),
            Some(v) if v == picker_vp => show_over_game(ctx, |vctx| self.midi_modal_window(vctx)),
            // Drawn by its editor window.
            Some(_) => return,
        };
        self.apply_midi_modal_outcome(ctx, out);
    }
}

/// The JSM insert editor's button row, after its type / channel / number rows
/// (which are the Add editor's first three, stepped by the same field index).
const JSM_INSERT_ROW: usize = 3;

/// Render `f` in the always-on-top, see-through viewport that floats over the
/// game while the config overlay is up — the one the KB/M picker uses. Every
/// modal a pad can open from the overlay has to land here: the main window is
/// behind the game, so a modal drawn there is one nobody can see.
pub(crate) fn show_over_game<R: Default>(ctx: &egui::Context, f: impl FnOnce(&egui::Context) -> R) -> R {
    let monitor = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 1.0 && s.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));
    let builder = egui::ViewportBuilder::default()
        .with_title("FlexInput Picker")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_resizable(false)
        .with_has_shadow(false)
        .with_active(false)
        .with_position(egui::pos2(0.0, 0.0))
        .with_inner_size(monitor);
    let mut f = Some(f);
    let mut result = R::default();
    ctx.show_viewport_immediate(
        crate::config_overlay::picker_viewport_id(),
        builder,
        |vctx, _class| {
            if let Some(f) = f.take() {
                result = f(vctx);
            }
        },
    );
    result
}
