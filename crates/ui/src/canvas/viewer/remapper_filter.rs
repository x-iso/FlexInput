//! Mapping-list display filter (All / follow-input / stick-group).

use super::*;

// ── Mapping-list display filter ───────────────────────────────────────────────
//
// Remapper / Map Action / Lean all render a (sometimes long) list of mapping
// cards. The filter row above the list lets the user narrow it to:
//   • All mappings        — neutral chip, default.
//   • Filter: {input}     — GREEN. "Follow live input": the currently-pressed
//                           gamepad/KB-M input set is latched regardless of
//                           which chip is active, so the green/blue labels
//                           always PREVIEW their target. The set LATCHES — it
//                           keeps the last detected input(s) after release and
//                           updates to the newest press. The list only narrows
//                           once green is clicked; a card passes if it contains
//                           ANY latched
//                           input.
//   • All {Stick} mappings— BLUE. Enabled when any latched input is stick-
//                           derived; matches any card referencing ANY direction
//                           of that same stick (the whole vector group). Greyed
//                           when no latched input is stick-derived.
//
// "Filterable pins" are a card's captured INPUT chord for Remapper/Map Action
// (the `in` field) and its captured OUTPUT chord for Lean (the `out` field —
// Lean cards have no input, the lean direction is the trigger, so we match the
// pressed input against the assigned outputs instead). The blue group also
// matches analog destinations (stick axes/cardinals) on the output side.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MapFilterKind { All, Input, Stick }

/// Persisted per (node, section) filter state: which chip is active + the
/// latched "current input(s)" the green/blue chips resolve against.
#[derive(Clone)]
pub(crate) struct MapFilterState {
    pub(crate) kind: MapFilterKind,
    /// Last live-pressed input set (latched across release; updated to the
    /// newest non-empty press). Empty until the user presses something while
    /// the green/blue filter is active.
    pub(crate) current_inputs: Vec<String>,
}

impl Default for MapFilterState {
    fn default() -> Self {
        MapFilterState { kind: MapFilterKind::All, current_inputs: Vec::new() }
    }
}

impl MapFilterState {
    /// First latched input that belongs to an input group, if any, with its
    /// group label + member pins.
    fn group(&self) -> Option<(&'static str, &'static [&'static str])> {
        self.current_inputs.iter().find_map(|p| input_group_of(p))
    }
}

/// Map a pin id to the input group it belongs to: (group label, member pin
/// ids). Groups bundle every representation a card might have captured — for
/// the analog groups that means the Vec2, both axes, and the four synthetic
/// cardinals; for D-Pad the Vec2, axes, and four cardinals; for buttons the
/// related set. The grouped filter matches any card referencing any member.
pub(crate) fn input_group_of(pin_id: &str) -> Option<(&'static str, &'static [&'static str])> {
    const LEFT_STICK: &[&str] = &[
        "left_stick", "left_stick_x", "left_stick_y",
        "left_stick_up", "left_stick_down", "left_stick_left", "left_stick_right",
    ];
    const RIGHT_STICK: &[&str] = &[
        "right_stick", "right_stick_x", "right_stick_y",
        "right_stick_up", "right_stick_down", "right_stick_left", "right_stick_right",
    ];
    const DPAD: &[&str] = &[
        "dpad", "dpad_x", "dpad_y",
        "dpad_up", "dpad_down", "dpad_left", "dpad_right",
    ];
    const TRIGGERS: &[&str] = &[
        "left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig",
    ];
    const FACE: &[&str] = &[
        "btn_south", "btn_east", "btn_west", "btn_north",
    ];
    const BUMPERS: &[&str] = &["btn_lb", "btn_rb"];
    // Menu cluster: Back/Select/Share (−) + Start/Options (+).
    const MENU: &[&str] = &["btn_back", "btn_start"];
    // System cluster: Guide/Home, Capture/Share-button, Mic/Mute.
    const SYSTEM: &[&str] = &["btn_guide", "btn_capture", "btn_mute"];
    // Stick clicks (L3/R3) — a natural pair too.
    const STICK_CLICKS: &[&str] = &["btn_ls", "btn_rs"];
    for (label, members) in [
        ("Left Stick", LEFT_STICK),
        ("Right Stick", RIGHT_STICK),
        ("D-Pad", DPAD),
        ("Triggers", TRIGGERS),
        ("Face Buttons", FACE),
        ("Bumpers", BUMPERS),
        ("Menu", MENU),
        ("System", SYSTEM),
        ("Stick Clicks", STICK_CLICKS),
    ] {
        if members.contains(&pin_id) { return Some((label, members)); }
    }
    None
}

/// Does a mapping pass the active filter? `filter_pins` is the card's input
/// chord (Remapper/Map Action) or output chord (Lean).
pub(crate) fn mapping_passes_filter(state: &MapFilterState, filter_pins: &[String]) -> bool {
    match state.kind {
        MapFilterKind::All => true,
        MapFilterKind::Input => {
            if state.current_inputs.is_empty() { return true; }
            // Any-of: card passes if it contains any latched input.
            filter_pins.iter().any(|p| state.current_inputs.iter().any(|q| q == p))
        }
        MapFilterKind::Stick => {
            match state.group() {
                Some((_, members)) =>
                    filter_pins.iter().any(|p| members.contains(&p.as_str())),
                // No latched input belongs to a group → grouped filter is
                // inert; show all (the chip renders greyed, so the user can't
                // actually select this state, but guard defensively).
                None => true,
            }
        }
    }
}

/// Render the three filter chips and return the resolved filter state. The
/// caller persists nothing — state lives in egui temp data keyed by
/// `filter_id`. `live_input` is the set of currently-pressed pin ids (gamepad
/// + KB/M), used to drive the green "follow live input" behaviour. Returns the
/// active `MapFilterState` to test each card against.
pub(crate) fn mapping_filter_row(
    ui: &mut egui::Ui,
    filter_id: egui::Id,
    count_label: &str,
    live_input: &[String],
    skin: crate::canvas::remapper_icons::Skin,
) -> MapFilterState {
    let _ = skin; // reserved: could render the input as an icon chip later
    let mut state: MapFilterState =
        ui.ctx().data(|d| d.get_temp(filter_id)).unwrap_or_default();

    // Always follow the live input set and LATCH it — regardless of which chip
    // is active. A non-empty press replaces the latched set; releasing keeps
    // the last set. This lets the green/blue chips PREVIEW what they'd filter
    // to (their labels stay live) even while "All mappings" is selected; the
    // list only narrows once the user actually clicks green or blue.
    if !live_input.is_empty() && live_input != state.current_inputs.as_slice() {
        state.current_inputs = live_input.to_vec();
    }

    // Colors. Neutral pill matches the card header mid-grey; green/blue are
    // muted so an active chip reads as "selected" without glare.
    const C_NEUTRAL:  Color32 = Color32::from_rgb(0x4A, 0x4A, 0x4A);
    const C_GREEN:    Color32 = Color32::from_rgb(0x2E, 0x7D, 0x46);
    const C_GREEN_HI: Color32 = Color32::from_rgb(0x3F, 0xA8, 0x5F);
    const C_BLUE:     Color32 = Color32::from_rgb(0x2C, 0x5A, 0x8C);
    const C_BLUE_HI:  Color32 = Color32::from_rgb(0x42, 0x82, 0xC4);

    let group = state.group();

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;

        // ── All mappings chip ──────────────────────────────────────────────
        let all_active = state.kind == MapFilterKind::All;
        let all_txt = format!("All mappings {count_label}");
        let all_resp = filter_chip(
            ui, &all_txt, all_active,
            C_NEUTRAL, Color32::from_white_alpha(36), true,
        );
        if all_resp.clicked() { state.kind = MapFilterKind::All; }

        // ── Green "Filter: {input}" chip ───────────────────────────────────
        // Label previews the latched input even under "All" (the latch updates
        // every frame above). Clicking just switches the active filter to it.
        let input_active = state.kind == MapFilterKind::Input;
        let input_label = filter_inputs_label(&state.current_inputs);
        let green_resp = filter_chip(
            ui, &input_label, input_active,
            C_GREEN, C_GREEN_HI, true,
        ).on_hover_text(
            "Show only mappings that contain the input(s) you press.\nLatches the last detected input(s); keeps filtering after release.",
        );
        if green_resp.clicked() { state.kind = MapFilterKind::Input; }

        // ── Blue "Grouped mappings" chip ───────────────────────────────────
        // Always rendered (stable row width); greyed + non-interactive when no
        // latched input belongs to a group. When active it shows the resolved
        // group (Left/Right Stick, D-Pad, Triggers, Face Buttons).
        let (group_label, group_enabled) = match group {
            Some((grp, _)) => (format!("Grouped: {grp}"), true),
            None => ("Grouped mappings".to_string(), false),
        };
        let group_active = state.kind == MapFilterKind::Stick;
        let blue_resp = filter_chip(
            ui, &group_label, group_active,
            C_BLUE, C_BLUE_HI, group_enabled,
        ).on_hover_text(
            "Show every mapping in the same input group as the latched input\n(e.g. all D-Pad directions, all face buttons, the whole stick).",
        );
        if group_enabled && blue_resp.clicked() { state.kind = MapFilterKind::Stick; }
        // If the user was on the grouped filter but no latched input belongs to
        // a group any more, fall back to All so the list never silently shows
        // everything under a now-inert grouped chip.
        if state.kind == MapFilterKind::Stick && group.is_none() {
            state.kind = MapFilterKind::All;
        }
    });

    ui.ctx().data_mut(|d| d.insert_temp(filter_id, state.clone()));
    state
}

/// Gamepad-nav: cycle a Remapper/Map-Action node's mapping filter by `dir`
/// (+1 forward, -1 back). Mirrors the chip click order All → Input → Stick,
/// skipping Stick when no input group is currently latched (matches the UI's
/// own greyed-Stick guard). The filter state lives in egui temp keyed by
/// `("fxi_remap_filter", node_id.0)`.
pub fn nav_cycle_remapper_filter(ctx: &egui::Context, inner_node_id: usize, dir: i32) {
    let filter_id = egui::Id::new(("fxi_remap_filter", inner_node_id));
    let mut state: MapFilterState =
        ctx.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    let has_group = state.group().is_some();
    // Available kinds in cycle order; Stick only when a group is latched.
    let kinds: &[MapFilterKind] = if has_group {
        &[MapFilterKind::All, MapFilterKind::Input, MapFilterKind::Stick]
    } else {
        &[MapFilterKind::All, MapFilterKind::Input]
    };
    let cur = kinds.iter().position(|k| *k == state.kind).unwrap_or(0) as i32;
    let next = (cur + dir).rem_euclid(kinds.len() as i32) as usize;
    state.kind = kinds[next];
    ctx.data_mut(|d| d.insert_temp(filter_id, state));
}

/// Compact pin name for the filter row, kept short so all three chips fit on
/// one line. Stick cardinals/axes abbreviate to "LS Left", "RS Up", "LS X";
/// D-Pad to "D-Pad Left"; everything else falls back to the canonical display
/// name (already short for buttons — "LB", "South", "Start", …).
pub(crate) fn filter_pin_label(pin_id: &str) -> String {
    match pin_id {
        "left_stick_up"     => "LS Up".into(),
        "left_stick_down"   => "LS Down".into(),
        "left_stick_left"   => "LS Left".into(),
        "left_stick_right"  => "LS Right".into(),
        "right_stick_up"    => "RS Up".into(),
        "right_stick_down"  => "RS Down".into(),
        "right_stick_left"  => "RS Left".into(),
        "right_stick_right" => "RS Right".into(),
        "left_stick_x"  => "LS X".into(),
        "left_stick_y"  => "LS Y".into(),
        "right_stick_x" => "RS X".into(),
        "right_stick_y" => "RS Y".into(),
        "left_stick"    => "LS".into(),
        "right_stick"   => "RS".into(),
        "dpad_up"    => "D-Pad Up".into(),
        "dpad_down"  => "D-Pad Down".into(),
        "dpad_left"  => "D-Pad Left".into(),
        "dpad_right" => "D-Pad Right".into(),
        _ => remapper_pin_display(pin_id),
    }
}

/// Build the green chip's label from the latched input set: "Filter: <input>"
/// with a "+N" suffix when a chord was latched.
pub(crate) fn filter_inputs_label(inputs: &[String]) -> String {
    match inputs.split_first() {
        None => "Filter: press input".to_string(),
        Some((first, rest)) if rest.is_empty() =>
            format!("Filter: {}", filter_pin_label(first)),
        Some((first, rest)) =>
            format!("Filter: {} +{}", filter_pin_label(first), rest.len()),
    }
}

/// A small pill button used by the filter row. `base` is the idle fill, `hi`
/// the hover/active fill. When `enabled` is false it paints dim and reports a
/// non-interactive (hover-only) response.
pub(crate) fn filter_chip(
    ui: &mut egui::Ui,
    text: &str,
    active: bool,
    base: Color32,
    hi: Color32,
    enabled: bool,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(12.0),
        Color32::WHITE,
    );
    let pad = egui::vec2(8.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(
        size,
        if enabled { egui::Sense::click() } else { egui::Sense::hover() },
    );
    let fill = if !enabled {
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 60)
    } else if active {
        hi
    } else if resp.hovered() {
        // Blend toward hi on hover.
        Color32::from_rgba_unmultiplied(
            ((base.r() as u16 + hi.r() as u16) / 2) as u8,
            ((base.g() as u16 + hi.g() as u16) / 2) as u8,
            ((base.b() as u16 + hi.b() as u16) / 2) as u8,
            255,
        )
    } else {
        base
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, fill);
    if active {
        painter.rect_stroke(
            rect, 4.0,
            egui::Stroke::new(1.0, Color32::from_white_alpha(180)),
            egui::epaint::StrokeKind::Inside,
        );
    }
    let text_col = if enabled { Color32::WHITE } else { Color32::from_white_alpha(120) };
    painter.galley(
        rect.min + pad,
        galley.clone(),
        text_col,
    );
    resp
}
