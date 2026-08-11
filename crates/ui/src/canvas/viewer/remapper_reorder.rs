//! Mapping-card drag-to-reorder machinery.

use super::*;

// ── Mapping-card drag-to-reorder ──────────────────────────────────────────────
//
// Cards are painter-driven and variable-height (chords wrap), and may live in a
// transformed layer (the pinned whole-module widget). Reorder is implemented
// against that reality:
//   • The card body (below the header) is the drag handle (Sense in the card).
//   • While a card is dragged, it visually lifts (paint offset) and the list
//     opens an insertion gap at the drop target; other cards slide.
//   • The target index is derived from the live pointer-Y compared against the
//     PREVIOUS frame's card centers (recorded in egui temp data). A one-frame
//     lag during an active drag is imperceptible (we repaint each frame).
//   • Commit happens on drag release; the caller splices the underlying array.

/// One row of the per-node layout map: (mapping index, center-Y, height).
pub(crate) type CardLayoutRow = (usize, f32, f32);

#[derive(Clone, Default)]
pub(crate) struct ReorderPersist {
    /// Active drag: (from_index, accumulated_dy). None when idle.
    drag: Option<(usize, f32)>,
    /// Last frame's visible card geometry, in the card UI's LOCAL coordinate
    /// space (same space the drag Response reports positions in). Recording
    /// local — not global — coords keeps the target math correct inside the
    /// pinned whole-module widget's transformed layer.
    layout: Vec<CardLayoutRow>,
    /// Last frame's pointer Y in that same local space, captured from the
    /// dragged card's `interact_pointer_pos()`. Used to resolve the insertion
    /// target one frame later (imperceptible during an active drag).
    pointer_y: Option<f32>,
}

/// Live view of reorder state for the current frame. Built once before the card
/// loop, queried per card, finalized after.
pub(crate) struct ReorderView {
    state_id: egui::Id,
    enabled: bool,
    /// (from, accum_dy) carried from last frame.
    drag: Option<(usize, f32)>,
    /// Insertion target computed from last-frame layout + live pointer. Only
    /// meaningful while `drag` is Some.
    target: Option<usize>,
    /// Display slot the dragged card occupied last frame (so we can suppress a
    /// redundant gap right where it already sits).
    from_slot: Option<usize>,
    /// Fresh layout being accumulated this frame.
    new_layout: Vec<CardLayoutRow>,
    /// Pointer Y (local space) captured this frame from the dragged card.
    new_pointer_y: Option<f32>,
    /// Approx card height (px) for sizing the insertion gap. Taken from the
    /// dragged card's last-known height, falling back to a default.
    gap_h: f32,
    /// Set on release; caller reads via `take_commit`.
    commit: Option<(usize, usize)>,
}

/// Host discriminator for the persisted drag state.
///
/// Callers key `state_id` on the NODE (`("fxi_remap_reorder", node_id.0)`), but
/// `ctx.data()` is one map shared by every viewport — so the same node rendered
/// in two places at once (a sub-patch editor window plus a whole-module pin of
/// that node on the parent canvas or an overlay) had both hosts reading and
/// writing a single `ReorderPersist`. The host that isn't being dragged records
/// `pointer_y: None` and last frame's `dy`, and whichever renders last wins:
/// `target` stays `None` forever, so no insertion gap is ever drawn and
/// `commit` — which requires `Some(target)` — never fires, while the drag lift
/// gets reverted each frame. Net effect: drag-to-reorder silently stops working
/// the moment the node is visible in a second place.
///
/// `(viewport_id, layer_id)` separates them: different windows differ by
/// viewport, the canvas and a pin differ by layer, and two pins of one node
/// differ by layer too (see `pin_host_scope`). Deliberately NOT `ui.id()` —
/// that moves more readily and would drop an in-flight drag.
fn reorder_host_scope(ui: &egui::Ui) -> egui::Id {
    egui::Id::new((ui.ctx().viewport_id(), ui.layer_id()))
}

impl ReorderView {
    pub(crate) fn begin(ui: &egui::Ui, state_id: egui::Id, enabled: bool) -> Self {
        // Scoped here rather than at each call site so a new reorder list can't
        // forget it (there are four: Remapper, Map Action, Touch Zones, Lean).
        let state_id = state_id.with(reorder_host_scope(ui));
        let persist: ReorderPersist =
            ui.ctx().data(|d| d.get_temp(state_id)).unwrap_or_default();
        let mut drag = if enabled { persist.drag } else { None };

        // Fold in any auto-scroll compensation published by the enclosing
        // whole-module widget last frame, so the lifted card stays glued to the
        // pointer while the body scrolls under it. Keyed by this body's layer.
        // Consume-and-clear so each published delta is applied exactly once.
        if let Some((_, dy)) = drag.as_mut() {
            let comp_id = egui::Id::new(("fxi_reorder_scroll_comp", ui.layer_id()));
            let comp = ui.ctx().data_mut(|d| {
                let v = d.get_temp::<f32>(comp_id).unwrap_or(0.0);
                if v != 0.0 { d.insert_temp(comp_id, 0.0_f32); }
                v
            });
            *dy += comp;
        }
        let drag = drag;

        // Compute insertion target from last frame's pointer Y (in card-local
        // space) vs last frame's card centers (same space). Target is the
        // index the dragged card would land *before* (== layout.len() means
        // "after the last card").
        let mut target = None;
        let mut gap_h = 96.0;
        let mut from_slot = None;
        if let Some((from, _)) = drag {
            for (slot, (i, _, h)) in persist.layout.iter().enumerate() {
                if *i == from { gap_h = *h; from_slot = Some(slot); }
            }
            if let Some(py) = persist.pointer_y {
                // Cards are in display order in `layout`; find the first whose
                // center is below the pointer → insert before it.
                let mut t = persist.layout.len();
                for (slot, (_, cy, _)) in persist.layout.iter().enumerate() {
                    if py < *cy { t = slot; break; }
                }
                target = Some(t);
            }
        }

        ReorderView {
            state_id, enabled, drag, target, from_slot,
            new_layout: Vec::new(),
            new_pointer_y: None,
            gap_h,
            commit: None,
        }
    }

    /// Visual lift to pass as `drag_offset_y` for card `idx`.
    pub(crate) fn offset_for(&self, idx: usize) -> f32 {
        match self.drag {
            Some((from, dy)) if from == idx => dy,
            _ => 0.0,
        }
    }

    /// True when the insertion target sits exactly where the dragged card
    /// already is (its own slot or the slot just after) — in that case opening
    /// a gap would just double the displacement, so suppress it.
    fn target_is_noop(&self) -> bool {
        match (self.from_slot, self.target) {
            (Some(f), Some(t)) => t == f || t == f + 1,
            _ => false,
        }
    }

    /// Whether an insertion gap should be drawn *before* the card that will be
    /// rendered at display position `slot` (0-based among visible cards).
    pub(crate) fn gap_before(&self, slot: usize) -> Option<f32> {
        if self.target_is_noop() { return None; }
        match (self.drag, self.target) {
            (Some(_), Some(t)) if t == slot => Some(self.gap_h * 0.5),
            _ => None,
        }
    }

    /// Trailing gap after the final visible card (target past the end).
    pub(crate) fn gap_after_last(&self, visible_count: usize) -> Option<f32> {
        if self.target_is_noop() { return None; }
        match (self.drag, self.target) {
            (Some(_), Some(t)) if t >= visible_count => Some(self.gap_h * 0.5),
            _ => None,
        }
    }

    /// Record this card's geometry and fold its drag interaction into the
    /// state machine. `idx` is the mapping's array index.
    pub(crate) fn observe(&mut self, idx: usize, result: &MappingCardResult) {
        self.new_layout.push((idx, result.rect.center().y, result.rect.height()));
        if !self.enabled { return; }
        let Some(resp) = result.body_drag.as_ref() else { return };
        if resp.drag_started() {
            self.drag = Some((idx, 0.0));
        } else if resp.dragged() {
            if let Some((from, dy)) = self.drag.as_mut() {
                if *from == idx { *dy += resp.drag_delta().y; }
            }
            // Capture the pointer in this card's LOCAL space for next-frame
            // target resolution. `interact_pointer_pos` already accounts for
            // the responding layer's transform.
            if let Some(p) = resp.interact_pointer_pos() {
                self.new_pointer_y = Some(p.y);
            }
        } else if resp.drag_stopped() {
            if let Some((from, _)) = self.drag {
                if from == idx {
                    if let Some(to) = self.target {
                        self.commit = Some((from, to));
                    }
                    self.drag = None;
                }
            }
        }
    }

    /// Persist state for next frame; returns a pending reorder to apply.
    pub(crate) fn finish(mut self, ui: &egui::Ui) -> Option<(usize, usize)> {
        let commit = self.commit.take();
        // Sort layout by center-Y so display order is correct even if the
        // loop visited indices out of order (it doesn't, but be safe).
        self.new_layout.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let active = self.drag.is_some();
        let persist = ReorderPersist {
            drag: self.drag,
            layout: self.new_layout,
            pointer_y: self.new_pointer_y,
        };
        ui.ctx().data_mut(|d| d.insert_temp(self.state_id, persist));
        // Publish a per-layer "card drag in progress" flag so the enclosing
        // whole-module widget can auto-scroll the body toward the drag edge.
        // Keyed by the body layer (== this ui's layer) so the outer reads it
        // with the same key. Cleared (set false) when idle.
        let flag_id = egui::Id::new(("fxi_reorder_drag_active", ui.layer_id()));
        ui.ctx().data_mut(|d| d.insert_temp(flag_id, active));
        if active { request_repaint_throttled(ui.ctx()); }
        commit
    }
}

/// Draw a subtle insertion-gap highlight and reserve `h` vertical space.
pub(crate) fn draw_insertion_gap(ui: &mut egui::Ui, h: f32) {
    let w = ui.available_width().min(358.0).max(100.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let band = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width(), 3.0),
    );
    let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(rect));
    painter.rect_filled(band, 1.5, Color32::from_rgb(0x3F, 0xA8, 0x5F));
}

/// Move element `from` to land before display position `to` (where `to` counts
/// slots in the *current* order; `to == len` appends). No-op if it doesn't move.
pub(crate) fn reorder_array(arr: &mut Vec<Value>, from: usize, to: usize) {
    if from >= arr.len() { return; }
    // `to` is an insertion slot in the original indexing. After removing
    // `from`, indices above it shift down by one.
    let to = to.min(arr.len());
    if to == from || to == from + 1 { return; } // already in place
    let item = arr.remove(from);
    let insert_at = if to > from { to - 1 } else { to };
    let insert_at = insert_at.min(arr.len());
    arr.insert(insert_at, item);
}

#[cfg(test)]
mod mapping_list_tests {
    use crate::canvas::viewer::*;
    use serde_json::Value;

    fn arr(items: &[&str]) -> Vec<Value> {
        items.iter().map(|s| Value::String((*s).to_string())).collect()
    }
    fn ids(arr: &[Value]) -> Vec<String> {
        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
    }

    // ── reorder_array: insertion-slot semantics + index shift ──────────────
    // `to` is an insertion slot in the ORIGINAL indexing (the slot the dragged
    // card lands *before*); `to == len` appends. Moving down must account for
    // the gap left behind once `from` is removed.

    #[test]
    fn reorder_move_down_past_one() {
        // [A,B,C,D], drag A (idx0) to land before slot 2 → after B.
        let mut a = arr(&["A", "B", "C", "D"]);
        reorder_array(&mut a, 0, 2);
        assert_eq!(ids(&a), vec!["B", "A", "C", "D"]);
    }

    #[test]
    fn reorder_move_to_end() {
        // drag A to slot len (append).
        let mut a = arr(&["A", "B", "C"]);
        reorder_array(&mut a, 0, 3);
        assert_eq!(ids(&a), vec!["B", "C", "A"]);
    }

    #[test]
    fn reorder_move_up() {
        // [A,B,C,D], drag D (idx3) to land before slot 1 → between A and B.
        let mut a = arr(&["A", "B", "C", "D"]);
        reorder_array(&mut a, 3, 1);
        assert_eq!(ids(&a), vec!["A", "D", "B", "C"]);
    }

    #[test]
    fn reorder_noop_same_slot() {
        // target == from or from+1 is a no-op (card already there).
        let mut a = arr(&["A", "B", "C"]);
        reorder_array(&mut a, 1, 1);
        assert_eq!(ids(&a), vec!["A", "B", "C"]);
        reorder_array(&mut a, 1, 2);
        assert_eq!(ids(&a), vec!["A", "B", "C"]);
    }

    #[test]
    fn reorder_out_of_range_from_is_safe() {
        let mut a = arr(&["A", "B"]);
        reorder_array(&mut a, 5, 0);
        assert_eq!(ids(&a), vec!["A", "B"]);
    }

    #[test]
    fn reorder_target_past_end_clamps() {
        let mut a = arr(&["A", "B", "C"]);
        reorder_array(&mut a, 0, 99);
        assert_eq!(ids(&a), vec!["B", "C", "A"]);
    }

    // ── input_group_of ────────────────────────────────────────────────────
    #[test]
    fn input_group_classifies_all_groups() {
        assert_eq!(input_group_of("left_stick_up").map(|(l, _)| l), Some("Left Stick"));
        assert_eq!(input_group_of("left_stick_x").map(|(l, _)| l), Some("Left Stick"));
        assert_eq!(input_group_of("left_stick").map(|(l, _)| l), Some("Left Stick"));
        assert_eq!(input_group_of("right_stick_left").map(|(l, _)| l), Some("Right Stick"));
        // D-Pad: Vec2, axes, and cardinals all map to the D-Pad group.
        assert_eq!(input_group_of("dpad_up").map(|(l, _)| l), Some("D-Pad"));
        assert_eq!(input_group_of("dpad").map(|(l, _)| l), Some("D-Pad"));
        assert_eq!(input_group_of("dpad_x").map(|(l, _)| l), Some("D-Pad"));
        // Triggers: analog + digital.
        assert_eq!(input_group_of("left_trigger").map(|(l, _)| l), Some("Triggers"));
        assert_eq!(input_group_of("btn_rt_dig").map(|(l, _)| l), Some("Triggers"));
        // Face buttons (canonical diamond ids).
        assert_eq!(input_group_of("btn_south").map(|(l, _)| l), Some("Face Buttons"));
        assert_eq!(input_group_of("btn_north").map(|(l, _)| l), Some("Face Buttons"));
        // Bumpers / Menu / System / Stick clicks.
        assert_eq!(input_group_of("btn_lb").map(|(l, _)| l), Some("Bumpers"));
        assert_eq!(input_group_of("btn_rb").map(|(l, _)| l), Some("Bumpers"));
        assert_eq!(input_group_of("btn_back").map(|(l, _)| l), Some("Menu"));
        assert_eq!(input_group_of("btn_start").map(|(l, _)| l), Some("Menu"));
        assert_eq!(input_group_of("btn_guide").map(|(l, _)| l), Some("System"));
        assert_eq!(input_group_of("btn_capture").map(|(l, _)| l), Some("System"));
        assert_eq!(input_group_of("btn_mute").map(|(l, _)| l), Some("System"));
        assert_eq!(input_group_of("btn_ls").map(|(l, _)| l), Some("Stick Clicks"));
        assert_eq!(input_group_of("btn_rs").map(|(l, _)| l), Some("Stick Clicks"));
        // Still ungrouped: touchpad click, gyro/accel axes, etc.
        assert_eq!(input_group_of("btn_touchpad"), None);
        assert_eq!(input_group_of("gyro_x"), None);
    }

    // ── mapping_passes_filter ─────────────────────────────────────────────
    fn state(kind: MapFilterKind, input: &str) -> MapFilterState {
        let inputs = if input.is_empty() { vec![] } else { vec![input.to_string()] };
        MapFilterState { kind, current_inputs: inputs }
    }
    fn state_multi(kind: MapFilterKind, inputs: &[&str]) -> MapFilterState {
        MapFilterState { kind, current_inputs: inputs.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn filter_all_passes_everything() {
        let s = state(MapFilterKind::All, "");
        assert!(mapping_passes_filter(&s, &["btn_a".into()]));
        assert!(mapping_passes_filter(&s, &[]));
    }

    #[test]
    fn filter_input_matches_only_containing_card() {
        let s = state(MapFilterKind::Input, "btn_a");
        assert!(mapping_passes_filter(&s, &["btn_a".into(), "btn_b".into()]));
        assert!(!mapping_passes_filter(&s, &["btn_x".into()]));
    }

    #[test]
    fn filter_input_empty_current_shows_all() {
        // Green selected but nothing pressed yet → don't hide everything.
        let s = state(MapFilterKind::Input, "");
        assert!(mapping_passes_filter(&s, &["btn_a".into()]));
    }

    #[test]
    fn filter_stick_matches_any_direction_of_same_stick() {
        // Pressing left_stick_up; a card mapped from left_stick_left (a
        // different direction of the SAME stick) should pass the blue filter.
        let s = state(MapFilterKind::Stick, "left_stick_up");
        assert!(mapping_passes_filter(&s, &["left_stick_left".into()]));
        assert!(mapping_passes_filter(&s, &["left_stick_x".into()]));
        // A right-stick card must NOT match a left-stick group.
        assert!(!mapping_passes_filter(&s, &["right_stick_up".into()]));
        // A plain button card must not match.
        assert!(!mapping_passes_filter(&s, &["btn_a".into()]));
    }

    #[test]
    fn filter_stick_matches_analog_destination_for_lean() {
        // Lean cards filter by OUTPUT; an analog destination (stick axis) on
        // the output side must match the blue group when the live input is a
        // direction of that stick.
        let s = state(MapFilterKind::Stick, "right_stick_right");
        assert!(mapping_passes_filter(&s, &["right_stick_y".into()]));
        assert!(mapping_passes_filter(&s, &["right_stick".into()]));
    }

    #[test]
    fn filter_group_dpad_bundles_all_directions() {
        // Pressing one D-Pad direction groups every D-Pad representation.
        let s = state(MapFilterKind::Stick, "dpad_left");
        assert!(mapping_passes_filter(&s, &["dpad_right".into()]));
        assert!(mapping_passes_filter(&s, &["dpad".into()]));
        assert!(mapping_passes_filter(&s, &["dpad_y".into()]));
        assert!(!mapping_passes_filter(&s, &["left_stick_left".into()]));
    }

    #[test]
    fn filter_group_face_buttons_and_triggers() {
        let face = state(MapFilterKind::Stick, "btn_south");
        assert!(mapping_passes_filter(&face, &["btn_north".into()]));
        assert!(mapping_passes_filter(&face, &["btn_east".into()]));
        assert!(!mapping_passes_filter(&face, &["btn_lb".into()]));

        let trig = state(MapFilterKind::Stick, "left_trigger");
        assert!(mapping_passes_filter(&trig, &["right_trigger".into()]));
        assert!(mapping_passes_filter(&trig, &["btn_lt_dig".into()]));
        assert!(!mapping_passes_filter(&trig, &["btn_south".into()]));
    }

    #[test]
    fn filter_input_chord_matches_any_of_latched() {
        // A latched chord (LB + A) shows mappings containing EITHER input.
        let s = state_multi(MapFilterKind::Input, &["btn_lb", "btn_a"]);
        assert!(mapping_passes_filter(&s, &["btn_a".into()]));
        assert!(mapping_passes_filter(&s, &["btn_lb".into(), "btn_x".into()]));
        assert!(!mapping_passes_filter(&s, &["btn_y".into()]));
    }

    #[test]
    fn filter_stick_uses_first_stick_in_latched_set() {
        // Latched set has a button AND a stick direction; blue resolves to the
        // stick group regardless of ordering of non-stick entries.
        let s = state_multi(MapFilterKind::Stick, &["btn_a", "left_stick_down"]);
        assert!(mapping_passes_filter(&s, &["left_stick_left".into()]));
        assert!(!mapping_passes_filter(&s, &["right_stick_up".into()]));
    }

    #[test]
    fn filter_label_uses_filter_prefix_and_chord_count() {
        assert_eq!(filter_inputs_label(&[]), "Filter: press input");
        assert_eq!(
            filter_inputs_label(&["btn_south".to_string()]),
            format!("Filter: {}", filter_pin_label("btn_south")),
        );
        let two = vec!["btn_lb".to_string(), "btn_south".to_string()];
        assert_eq!(
            filter_inputs_label(&two),
            format!("Filter: {} +1", filter_pin_label("btn_lb")),
        );
    }

    #[test]
    fn filter_pin_label_abbreviates_sticks() {
        assert_eq!(filter_pin_label("left_stick_left"), "LS Left");
        assert_eq!(filter_pin_label("right_stick_up"), "RS Up");
        assert_eq!(filter_pin_label("left_stick_x"), "LS X");
        assert_eq!(filter_pin_label("right_stick"), "RS");
        assert_eq!(filter_pin_label("dpad_left"), "D-Pad Left");
        // Non-stick falls back to the canonical (already-short) display name.
        assert_eq!(filter_pin_label("btn_lb"), remapper_pin_display("btn_lb"));
    }
}
