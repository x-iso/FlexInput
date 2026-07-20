//! Remapper-family shared helpers: upstream device resolution, pressed-set
//! capture, chip/chord painting, skin resolution, press-mode glyphs.

use super::*;







// ── Remapper body ────────────────────────────────────────────────────────────
//
// State (persisted in node.params, all serde_json values):
//
//   ui_phase       : "idle" | "capturing" | "ready_to_learn" | "learning"
//   draft_input    : Array<String> (canonical AutoMap pin IDs)
//   draft_output   : Array<String>
//   mappings       : Array<{ in: Array<String>, out: Array<String> }>
//   skin           : "auto" | "xbox" | "playstation" | "switchpro" | "kbm"
//   _pressed_prev  : Array<String> (internal: last frame's pressed set)
//
// Capture algorithm (max-simultaneous-set, latched on full release):
//   1. Build pressed_now from live_signals filtered by the upstream device id.
//   2. While pressed_prev was empty, a new press starts a fresh burst:
//        - If draft was already latched from a previous burst, replace it.
//        - Otherwise begin accumulating into draft.
//   3. Within a burst, draft |= pressed_now (so we capture the peak combo).
//   4. On full release (pressed_now empty, draft non-empty), latch: advance
//      phase to ready_to_learn for input or capture-done for output.

/// Resolve the device id at the other end of an AutoMap input pin. Returns the
/// `device_id` param string of the directly-upstream `device.source` node, or
/// None if the pin is unwired / upstream is not a device source.
///
/// Walks at most one hop. Cross-subpatch and collector/fork chains are not
/// followed here — for the Remapper's capture UX the common case is
/// `Device → Remapper`. More complex topologies can be added later by reusing
/// the engine-side `find_automap_device_rec` from app.rs.
pub(crate) fn remapper_upstream_device_id(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    input_idx: usize,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> Option<String> {
    let pin = snarl.in_pin(InPinId { node: node_id, input: input_idx });
    let src = *pin.remotes.first()?;
    crate::app::find_automap_device_id_for_viewer(snarl, src, automap_parent)
}

/// Read which canonical AutoMap pins are currently asserted (Bool == true)
/// for the given upstream device id.
pub(crate) fn remapper_pressed_now(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    dev_id: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    // Prefer the ANALOG trigger over the digital L2/R2 button when the pad exposes
    // it: a PS/Xbox pad fires `btn_lt_dig`/`btn_rt_dig` WHENEVER the trigger is
    // pulled (alongside the analog axis), which would otherwise always win the
    // capture and rob the mapping of its analog value + response curve. Keyed on
    // whether the analog pin is actually present (digital-only pads like the Switch
    // Pro have none, so they keep capturing the digital button — their only signal).
    let has = |pin: &str| live_signals.contains_key(&(dev_id.to_string(), pin.to_string()));
    let lt_analog = has("left_trigger");
    let rt_analog = has("right_trigger");
    for ap in am_canon::ALL_PINS {
        if ap.signal_type != SignalType::Bool { continue; }
        // Touch-active pins are canonical (Splitter/Collector use them) but
        // suppressed in Remapper Learn — the zone synthesis below already
        // expresses the same information as touch_left/center/right.
        if ap.id == "touch1_active" || ap.id == "touch2_active" { continue; }
        if ap.id == "btn_lt_dig" && lt_analog { continue; }
        if ap.id == "btn_rt_dig" && rt_analog { continue; }
        // btn_touchpad is conditionally suppressed below: when a finger is
        // on the pad during a click, the specific-zone pin is the right
        // capture target; when there is no finger, the bare click stands as
        // a "click anywhere" mapping.
        if ap.id == "btn_touchpad" { continue; }
        if let Some(sig) = live_signals.get(&(dev_id.to_string(), ap.id.to_string())) {
            if sig.as_bool() {
                out.push(ap.id.to_string());
            }
        }
    }
    // Synthetic stick-cardinal pins. Mirror of the rule in
    // `flexinput_engine::eval::derive_stick_cardinals` so what Learn captures
    // matches what the eval engine will trigger on.
    const T_CARDINAL: f32 = 0.5;
    const T_DIAGONAL: f32 = 0.4;
    const DOM: f32 = 1.5;
    for (xpin, ypin, up, down, left, right) in [
        ("left_stick_x",  "left_stick_y",
         "left_stick_up",  "left_stick_down",
         "left_stick_left", "left_stick_right"),
        ("right_stick_x", "right_stick_y",
         "right_stick_up", "right_stick_down",
         "right_stick_left", "right_stick_right"),
    ] {
        let read = |pin: &str| -> f32 {
            live_signals
                .get(&(dev_id.to_string(), pin.to_string()))
                .map(|s| match s {
                    Signal::Float(v) => *v,
                    Signal::Vec2(v) => v.x,
                    _ => 0.0,
                })
                .unwrap_or(0.0)
        };
        let x = read(xpin);
        let y = read(ypin);
        let ax = x.abs();
        let ay = y.abs();
        let diagonal = ax > T_DIAGONAL && ay > T_DIAGONAL;
        if diagonal && x >  T_DIAGONAL
            || x >  T_CARDINAL && (ay < T_CARDINAL ||  x >  DOM * ay) { out.push(right.to_string()); }
        if diagonal && x < -T_DIAGONAL
            || x < -T_CARDINAL && (ay < T_CARDINAL || -x >  DOM * ay) { out.push(left.to_string()); }
        if diagonal && y >  T_DIAGONAL
            || y >  T_CARDINAL && (ax < T_CARDINAL ||  y >  DOM * ax) { out.push(up.to_string()); }
        if diagonal && y < -T_DIAGONAL
            || y < -T_CARDINAL && (ax < T_CARDINAL || -y >  DOM * ax) { out.push(down.to_string()); }
    }
    // Analog triggers. `left_trigger`/`right_trigger` are Float pins (skipped by
    // the Bool loop above), so Learn would otherwise never capture an analog
    // trigger — leaving it un-mappable as an analog input (no response curve /
    // activation threshold). When the analog pin exists, capture it once pulled past
    // a threshold (matching the stick-cardinal treatment); the digital L2/R2 button
    // was suppressed above so this analog capture is the sole one. Digital-only pads
    // (Switch Pro) have no analog pin here and kept their digital button instead.
    const T_TRIGGER: f32 = 0.5;
    for (analog, present) in [("left_trigger", lt_analog), ("right_trigger", rt_analog)] {
        if !present { continue; }
        let v = live_signals.get(&(dev_id.to_string(), analog.to_string()))
            .map(|s| s.as_float()).unwrap_or(0.0);
        if v > T_TRIGGER { out.push(analog.to_string()); }
    }
    out
}

/// Read live OS keyboard + mouse state as canonical AutoMap pin IDs. Used in
/// the Remapper's `learning` phase so the user can map to keys/mouse buttons
/// that are otherwise only present on the bus when a virtual KB/M sink is wired.
pub(crate) fn remapper_kbm_pressed_now(
    ui: &egui::Ui,
    panic_shortcut: &crate::app::PanicShortcut,
) -> Vec<String> {
    let mut out = Vec::new();
    ui.input(|i| {
        let m = i.modifiers;
        if m.shift { out.push("key_shift".to_string()); }
        if m.ctrl  { out.push("key_ctrl".to_string()); }
        if m.alt   { out.push("key_alt".to_string()); }
        // egui maps Cmd (Mac) and Win/Super into `command` — surface as key_win
        // on Windows, key_ctrl is already covered above.
        if m.command && !m.ctrl { out.push("key_win".to_string()); }

        // Every other egui key. Shift/Ctrl/Alt/Cmd are not in Key::ALL — they
        // are reported through i.modifiers above, so no risk of double-adding.
        for &key in egui::Key::ALL {
            if i.key_down(key) {
                let id = remapper_key_to_pin_id(key);
                if !out.iter().any(|p| p == &id) {
                    out.push(id);
                }
            }
        }

        // Mouse buttons and scroll are intentionally NOT captured here.
        // They cannot be live-learned because the user must click Add (LMB) to
        // confirm a mapping — that very click would otherwise latch as part of
        // the captured combo. They are added via the Special dropdown instead.

        // Block the panic-mode chord from being captured. If the currently
        // held set matches the configured Panic shortcut, drop it so the user
        // cannot accidentally rebind the emergency-stop onto a Remapper output.
        // We check exact equality (same modifiers + same key) so adjacent
        // chords still work — only the exact panic combo is filtered.
        if let Some(ref panic_key_name) = panic_shortcut.key {
            let panic_id = if matches!(panic_key_name.as_str(), "Escape") {
                "key_escape".to_string()
            } else {
                format!("key_{}", panic_key_name.to_ascii_lowercase())
            };
            let modifiers_match =
                m.shift   == panic_shortcut.shift
                && m.ctrl == panic_shortcut.ctrl
                && m.alt  == panic_shortcut.alt
                && (m.command && !m.ctrl) == panic_shortcut.win;
            if modifiers_match && out.iter().any(|p| p == &panic_id) {
                out.retain(|p| p != &panic_id
                    && p != "key_shift" && p != "key_ctrl"
                    && p != "key_alt"   && p != "key_win");
            }
        }
    });
    out
}

/// Render a chip for one canonical pin: SVG icon if mapped under `skin`,
/// otherwise the textual display name. Chip height is fixed at 22 logical px
/// to align with surrounding text. The SVG is rasterized + cached in egui
/// memory keyed on (pin_id, skin, size, tint).
pub(crate) fn remapper_render_chip(ui: &mut egui::Ui, pin_id: &str, skin: crate::canvas::remapper_icons::Skin) {
    use crate::canvas::remapper_icons;
    const CHIP_H: f32 = 28.0;
    // Macro-port pins (and macro-style Virtual-Menu targets): resolve name +
    // icon through the per-frame registry (published by app.rs). Icon chip with
    // the name as tooltip, or a plain name label when the port has no icon. A
    // dangling id (port deleted while the mapping still references it) renders
    // a struck placeholder.
    if flexinput_core::macros::parse_macro_pin(pin_id).is_some()
        || flexinput_core::menu::parse_target_pin(pin_id).is_some()
    {
        match crate::macro_icons::registry_entry(pin_id) {
            Some(entry) => {
                let hover = format!("{} ({})", entry.name, entry.signal_type.display_name());
                if let Some(tex) = crate::macro_icons::macro_port_icon_texture(
                    ui.ctx(), &entry.icon, &entry.icon_svg, CHIP_H)
                {
                    ui.add(egui::Image::new(&tex)
                        .fit_to_exact_size(egui::vec2(CHIP_H, CHIP_H))
                        .tint(Color32::WHITE))
                        .on_hover_text(hover);
                } else {
                    ui.label(egui::RichText::new(&entry.name).size(13.0).strong())
                        .on_hover_text(hover);
                }
            }
            None => {
                ui.label(egui::RichText::new("target?").size(13.0).weak().strikethrough())
                    .on_hover_text("This macro port / menu no longer exists");
            }
        }
        return;
    }
    if let Some(bytes) = remapper_icons::pin_svg(skin, pin_id) {
        let size_px = (CHIP_H * ui.ctx().pixels_per_point()).round() as u32;
        let tint = egui::Color32::TRANSPARENT;
        let cache_key = egui::Id::new(("remapper_icon", bytes.as_ptr() as usize, size_px));
        let tex = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key))
            .or_else(|| {
                let text = std::str::from_utf8(bytes).ok()?;
                let img = rasterize_svg_recolored(text, size_px, size_px, "override", tint)?;
                let handle = ui.ctx().load_texture(
                    format!("remapper_icon_{:p}", bytes.as_ptr()),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
                Some(handle)
            });
        if let Some(tex) = tex {
            let resp = ui.add(egui::Image::new(&tex)
                .fit_to_exact_size(egui::vec2(CHIP_H, CHIP_H))
                .tint(Color32::WHITE));
            // Overlay the extra-button label (e.g. "PL1") so one generic paddle
            // glyph can stand for both paddle rows on a side.
            if let Some(label) = remapper_icons::extra_button_label(pin_id) {
                paint_icon_label(ui, resp.rect, label);
            }
            return;
        }
    }
    ui.label(egui::RichText::new(remapper_pin_display(pin_id)).size(13.0).strong());
}

/// Paint a short label centered over an icon rect (used for extra-button
/// paddle glyphs). Draws a thin dark outline behind the text so it stays legible
/// over the white glyph regardless of the underlying shape.
pub(crate) fn paint_icon_label(ui: &egui::Ui, rect: egui::Rect, label: &str) {
    let painter = ui.painter_at(rect);
    let font = egui::FontId::proportional((rect.height() * 0.34).max(9.0));
    let center = rect.center();
    // Cheap outline: draw the text offset in dark, then the bright text on top.
    for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
        painter.text(
            center + egui::vec2(dx, dy),
            egui::Align2::CENTER_CENTER,
            label,
            font.clone(),
            Color32::from_black_alpha(200),
        );
    }
    painter.text(center, egui::Align2::CENTER_CENTER, label, font, Color32::WHITE);
}

/// Paint a chord chip directly via the painter at `top_left`, sized
/// `chip_h × chip_h`. Resolution order for the icon:
///   1. SVG for the *current* skin → render at full tint.
///   2. SVG for any *other* skin → render at gray tint (the mapping was
///      created on a different device; we still show the icon so the user
///      sees which input it represents, but mark it visually inert).
///   3. No SVG anywhere → fall back to a left-aligned text pill, gray.
///
/// Returns the painted chip width (= `chip_h` for icons, larger for text).
pub(crate) fn paint_chord_chip_to_rect(
    painter: &egui::Painter,
    ctx: &egui::Context,
    top_left: egui::Pos2,
    chip_h: f32,
    pin_id: &str,
    skin: crate::canvas::remapper_icons::Skin,
) -> f32 {
    use crate::canvas::remapper_icons::{self, Skin};

    // Macro-port pins (and macro-style Virtual-Menu targets): registry icon,
    // else a pill with the port's NAME (the raw "macro:{id}" / "menu:{id}_show"
    // token means nothing to the user). Dangling ids (port/menu deleted,
    // mapping kept) paint a dimmed placeholder pill.
    if flexinput_core::macros::parse_macro_pin(pin_id).is_some()
        || flexinput_core::menu::parse_target_pin(pin_id).is_some()
    {
        match crate::macro_icons::registry_entry(pin_id) {
            Some(entry) => {
                if let Some(tex) = crate::macro_icons::macro_port_icon_texture(
                    ctx, &entry.icon, &entry.icon_svg, chip_h)
                {
                    let rect = egui::Rect::from_min_size(top_left, egui::vec2(chip_h, chip_h));
                    painter.image(tex.id(), rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE);
                    return chip_h;
                }
                return paint_text_pill(painter, top_left, chip_h, entry.name, false);
            }
            None => return paint_text_pill(painter, top_left, chip_h, "target?".to_string(), true),
        }
    }

    // Probe current skin first; fall back to any other skin that has the
    // icon. `tinted` is true when we matched a non-current skin — the chip
    // is then dimmed to communicate that it isn't available on the device.
    let mut found: Option<(&'static [u8], bool)> = None;
    if let Some(b) = remapper_icons::pin_svg(skin, pin_id) {
        found = Some((b, false));
    } else {
        for s in [Skin::Xbox, Skin::Playstation, Skin::SwitchPro, Skin::Kbm] {
            if s == skin { continue; }
            if let Some(b) = remapper_icons::pin_svg(s, pin_id) {
                found = Some((b, true));
                break;
            }
        }
    }

    if let Some((bytes, dim)) = found {
        let size_px = (chip_h * ctx.pixels_per_point()).round() as u32;
        let cache_key = egui::Id::new(("remapper_icon", bytes.as_ptr() as usize, size_px));
        let tex = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_key))
            .or_else(|| {
                let text = std::str::from_utf8(bytes).ok()?;
                let img = rasterize_svg_recolored(text, size_px, size_px, "override", Color32::TRANSPARENT)?;
                let handle = ctx.load_texture(
                    format!("remapper_icon_{:p}", bytes.as_ptr()),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                ctx.data_mut(|d| d.insert_temp(cache_key, handle.clone()));
                Some(handle)
            });
        if let Some(tex) = tex {
            let rect = egui::Rect::from_min_size(top_left, egui::vec2(chip_h, chip_h));
            let tint = if dim { Color32::from_rgba_unmultiplied(255, 255, 255, 95) }
                       else  { Color32::WHITE };
            painter.image(tex.id(), rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                tint);
            // Extra-button label overlay (e.g. "PL1") — same outline-then-fill as
            // paint_icon_label, inlined since we have a bare painter here.
            if let Some(label) = remapper_icons::extra_button_label(pin_id) {
                let font = egui::FontId::proportional((chip_h * 0.34).max(9.0));
                let c = rect.center();
                let fg = if dim { Color32::from_rgba_unmultiplied(255, 255, 255, 95) } else { Color32::WHITE };
                for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                    painter.text(c + egui::vec2(dx, dy), egui::Align2::CENTER_CENTER,
                        label, font.clone(), Color32::from_black_alpha(200));
                }
                painter.text(c, egui::Align2::CENTER_CENTER, label, font, fg);
            }
            return chip_h;
        }
    }

    // Last-resort text pill — still useful for non-canonical pins (e.g.
    // unmapped keys). Dimmed so it reads as "label, no icon available".
    paint_text_pill(painter, top_left, chip_h, remapper_pin_display(pin_id), true)
}

/// Paint a rounded text pill at `top_left` and return its width. `dim` uses
/// the muted "label, no icon available" text; bright text is for labels that
/// ARE the intended rendering (e.g. a macro port's name).
pub(crate) fn paint_text_pill(
    painter: &egui::Painter,
    top_left: egui::Pos2,
    chip_h: f32,
    label: String,
    dim: bool,
) -> f32 {
    let font = egui::FontId::proportional(chip_h * 0.48);
    let text_col = if dim {
        Color32::from_rgba_unmultiplied(255, 255, 255, 160)
    } else {
        Color32::WHITE
    };
    let galley = painter.layout_no_wrap(label, font, text_col);
    let text_w = galley.size().x;
    let pad_x = chip_h * 0.30;
    let pill_w = text_w + pad_x * 2.0;
    let rect = egui::Rect::from_min_size(top_left, egui::vec2(pill_w, chip_h));
    painter.rect_filled(rect, chip_h * 0.18, Color32::from_rgba_unmultiplied(0x76, 0x76, 0x76, 140));
    painter.galley(
        egui::pos2(rect.left() + pad_x, rect.center().y - galley.size().y * 0.5),
        galley,
        text_col,
    );
    pill_w
}

/// Render the long-arrow SVG glyph between a mapping's input chips and its
/// output chips. Rasterized once via the existing SVG path and cached in egui
/// memory keyed on target size. Alpha-0 tint preserves the SVG's own color.
pub(crate) fn remapper_render_arrow(ui: &mut egui::Ui) {
    use crate::canvas::remapper_icons;
    const H: f32 = 22.0;
    let size_px = (H * ui.ctx().pixels_per_point()).round() as u32;
    let cache_key = egui::Id::new(("remapper_arrow_svg", size_px));
    let tex = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key))
        .or_else(|| {
            let text = std::str::from_utf8(remapper_icons::ARROW_LONG_SVG).ok()?;
            let img = rasterize_svg_recolored(text, size_px, size_px, "override", Color32::TRANSPARENT)?;
            let handle = ui.ctx().load_texture(
                "remapper_arrow_long",
                img,
                egui::TextureOptions::LINEAR,
            );
            ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
            Some(handle)
        });
    if let Some(tex) = tex {
        ui.add(egui::Image::new(&tex)
            .fit_to_exact_size(egui::vec2(H, H))
            .tint(Color32::WHITE));
    } else {
        ui.label(egui::RichText::new("→").size(14.0).weak());
    }
}

/// Detect the upstream device family for a Remapper's AutoMap input, falling
/// back to Xbox when no device is wired or auto detection fails. The user's
/// manual override in `node.params["skin"]` takes precedence.
pub(crate) fn remapper_resolve_skin(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    override_param: &str,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> crate::canvas::remapper_icons::Skin {
    use crate::canvas::remapper_icons::Skin;
    let chosen = Skin::from_str(override_param);
    if chosen != Skin::Auto { return chosen; }
    let dev = remapper_upstream_device_id(snarl, node_id, 0, automap_parent);
    match dev {
        Some(d) => crate::canvas::remapper_icons::skin_from_device_id(&d),
        None => Skin::Xbox,
    }
}

/// True for the synthetic touchpad-swipe output pins (continuous → analog mode).
pub(crate) fn remapper_out_is_swipe(pin_id: &str) -> bool {
    matches!(pin_id, "touch_swipe_x" | "touch_swipe_y")
}

/// Canonical pin id for an arbitrary egui Key. Modifiers and Escape get their
/// canonical short names so they round-trip with am_canon::ALL_PINS. Anything
/// else becomes `key_<lowercase debug>` (e.g. `key_a`, `key_space`, `key_f5`).
pub(crate) fn remapper_key_to_pin_id(key: egui::Key) -> String {
    match key {
        egui::Key::Escape => "key_escape".to_string(),
        // egui has no CapsLock variant; on Windows winit reports the Caps
        // Lock physical key as F18 through egui. Treat F18 as Caps Lock so
        // it captures correctly and uses the existing capslock SVG/enigo.
        egui::Key::F18 => "key_capslock".to_string(),
        // Egui exposes shifted-character variants for several keys; fold
        // them back to the physical key so they pick up the right icon
        // and map to the unshifted canonical pin.
        egui::Key::OpenCurlyBracket  => "key_openbracket".to_string(),
        egui::Key::CloseCurlyBracket => "key_closebracket".to_string(),
        egui::Key::Colon             => "key_semicolon".to_string(),
        egui::Key::Pipe              => "key_backslash".to_string(),
        egui::Key::Questionmark      => "key_slash".to_string(),
        egui::Key::Exclamationmark   => "key_1".to_string(),
        egui::Key::Plus              => "key_equals".to_string(),
        _ => format!("key_{}", format!("{:?}", key).to_lowercase()),
    }
}

pub(crate) fn remapper_pin_display(pin_id: &str) -> String {
    if let Some(p) = am_canon::ALL_PINS.iter().find(|p| p.id == pin_id) {
        return p.display_name.to_string();
    }
    // Synthetic stick-cardinal pins (derived inside Remapper, not canonical).
    match pin_id {
        "left_stick_up"     => return "L.Stick Up".into(),
        "left_stick_down"   => return "L.Stick Down".into(),
        "left_stick_left"   => return "L.Stick Left".into(),
        "left_stick_right"  => return "L.Stick Right".into(),
        "right_stick_up"    => return "R.Stick Up".into(),
        "right_stick_down"  => return "R.Stick Down".into(),
        "right_stick_left"  => return "R.Stick Left".into(),
        "right_stick_right" => return "R.Stick Right".into(),
        "touchpad_left"     => return "Touchpad Left (Click)".into(),
        "touchpad_center"   => return "Touchpad Center (Click)".into(),
        "touchpad_right"    => return "Touchpad Right (Click)".into(),
        "touchpad_any"      => return "Touchpad Click (Any)".into(),
        "touch_left"        => return "Touchpad Left (Touch)".into(),
        "touch_center"      => return "Touchpad Center (Touch)".into(),
        "touch_right"       => return "Touchpad Right (Touch)".into(),
        "touch_swipe_x"     => return "Touchpad Swipe ↔".into(),
        "touch_swipe_y"     => return "Touchpad Swipe ↕".into(),
        // Virtual Menu card trigger tokens (the zone's selection / highlight).
        "menu_sel"          => return "Select".into(),
        "menu_hover"        => return "Hover".into(),
        _ => {}
    }
    // Macro ports and Virtual-Menu targets: the raw "macro:{id}" /
    // "menu:{id}_show" token means nothing to the user — show the registry
    // name (the port's name / "Menu — Show"). Dangling ids fall through to
    // the raw token, which at least marks the mapping as broken.
    if flexinput_core::macros::parse_macro_pin(pin_id).is_some()
        || flexinput_core::menu::parse_target_pin(pin_id).is_some()
    {
        if let Some(e) = crate::macro_icons::registry_entry(pin_id) {
            return e.name;
        }
    }
    // Fall back to a humanised form of the raw id. `key_space` → "Space",
    // `key_a` → "A", `key_f5` → "F5". Unknown prefix → return id as-is.
    if let Some(rest) = pin_id.strip_prefix("key_") {
        let mut chars = rest.chars();
        let first = chars.next().unwrap_or('?').to_ascii_uppercase();
        return format!("{}{}", first, chars.as_str());
    }
    pin_id.to_string()
}

/// Short, skin-aware label for a nav (face) button used in gamepad-flow status
/// hints. `which` is "north" / "east" / "south" / "west". Xbox uses letters, PS
/// the shapes, Switch the swapped A/B layout; anything else falls back to the
/// cardinal name in brackets. (Reserved for future status-hint use.)
#[allow(dead_code)]
pub(crate) fn nav_button_label(skin: crate::canvas::remapper_icons::Skin, which: &str) -> &'static str {
    use crate::canvas::remapper_icons::Skin;
    match (skin, which) {
        (Skin::Xbox, "north") => "Y",
        (Skin::Xbox, "east")  => "B",
        (Skin::Xbox, "south") => "A",
        (Skin::Xbox, "west")  => "X",
        (Skin::Playstation, "north") => "△",
        (Skin::Playstation, "east")  => "○",
        (Skin::Playstation, "south") => "✕",
        (Skin::Playstation, "west")  => "□",
        (_, "north") => "North",
        (_, "east")  => "East",
        (_, "south") => "South",
        (_, "west")  => "West",
        _ => "?",
    }
}

pub(crate) fn remapper_read_str_array(node: &NodeData, key: &str) -> Vec<String> {
    node.params.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

pub(crate) fn remapper_write_str_array(node: &mut NodeData, key: &str, vals: &[String]) {
    let arr: Vec<Value> = vals.iter().map(|s| Value::String(s.clone())).collect();
    node.params.insert(key.to_string(), Value::Array(arr));
}

/// Press-mode glyphs shown on the per-mapping mode button. The popup menu
/// the button opens uses the same glyphs as visual cues.
pub(crate) fn remapper_press_mode_glyph(mode: &str) -> &'static str {
    match mode {
        "short"      => "↕",
        "long"       => "⇓",
        "double"     => "↡",
        "on_press"   => "↧",
        "on_release" => "↥",
        "analog"     => "∿",
        _            => "↓",
    }
}

pub(crate) fn remapper_press_mode_label(mode: &str) -> &'static str {
    match mode {
        "short"      => "Short press",
        "long"       => "Long press",
        "double"     => "Double tap",
        "on_press"   => "On press",
        "on_release" => "On release",
        "analog"     => "Analog",
        _            => "Normal (gate)",
    }
}
