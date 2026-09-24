//! Guidance lines that name pad buttons with the pad's OWN glyphs.
//!
//! "A = Start" is only true on an Xbox pad. On a DualSense it is ✕, on a Switch
//! Pro the A is in the other place entirely — so a hint written in letters is
//! wrong for two of the three families a user might be holding, and worst for
//! the Switch, where the letter is right but the position isn't.
//!
//! A hint is written with `{pin}` placeholders and drawn with the glyph set the
//! connected pad publishes each frame (`macro_icons::current_gp_skin`), the same
//! resolution the `gp:<pin>` icon keys use. A pad whose family has no art for a
//! pin borrows another family's (`gp_pin_svg` already falls back), and if there
//! is none at all the placeholder degrades to a short word rather than vanishing.

/// A pin with no glyph anywhere: what to write instead. Kept to the few pins
/// hints actually use, so an unexpected one shows its id and gets noticed rather
/// than silently reading as something it isn't.
fn fallback(pin: &str) -> &str {
    match pin {
        "btn_south" => "South",
        "btn_east" => "East",
        "btn_west" => "West",
        "btn_north" => "North",
        "dpad_left" => "◄",
        "dpad_right" => "►",
        "dpad_up" => "▲",
        "dpad_down" => "▼",
        other => other,
    }
}

/// One glyph, rasterized for this pad's family at roughly a line's height.
///
/// Goes through the same `gp:<pin>` resolver every other icon site uses, so the
/// cache is shared and a pad change restyles these hints along with everything
/// else. The art keeps its OWN colours — the rasterizer only recolours when
/// asked to, and these are not asked to — so it is drawn untinted, exactly as
/// the mapping-card chips and zone overlays draw the same glyphs.
fn glyph(ctx: &egui::Context, pin: &str, size: f32) -> Option<egui::TextureHandle> {
    crate::macro_icons::macro_port_icon_texture(ctx, &format!("gp:{pin}"), "", size)
}

/// Draw a guidance line, `{pin}` placeholders becoming the connected pad's own
/// button glyphs, as its OWN wrapped row.
///
/// ```text
/// pad_hint(ui, "{btn_south} Start · {dpad_left}{dpad_right} method");
/// ```
///
/// Wrapped, because these lines live in columns that can be genuinely narrow —
/// the JSM tune panel beside an editor, a pinned widget sized by hand.
pub(crate) fn pad_hint(ui: &mut egui::Ui, hint: &str) {
    ui.horizontal_wrapped(|ui| pad_hint_inline(ui, hint));
}

/// The same, drawn into the row the caller has already opened — for a hint that
/// has to share one line with something else and must NOT wrap onto a second
/// (a pinned widget whose frame was sized for one).
pub(crate) fn pad_hint_inline(ui: &mut egui::Ui, hint: &str) {
    let h = ui.text_style_height(&egui::TextStyle::Small);
    // Tight, so a glyph sits against the word it belongs to rather than floating
    // between two of them.
    ui.spacing_mut().item_spacing.x = 2.0;
    for part in parts(hint) {
        match part {
            Part::Text(t) => {
                if !t.trim().is_empty() {
                    ui.label(egui::RichText::new(t.trim()).small().weak());
                }
            }
            Part::Pin(p) => match glyph(ui.ctx(), p, h) {
                Some(tex) => {
                    // WHITE is the identity for a tint multiply: the glyph keeps
                    // its own colours at full opacity. Dimming it to the weak
                    // text colour the words use also multiplies that colour's
                    // 60% ALPHA through the art, which washes a solid shape out
                    // far more than it fades a letterform.
                    ui.add(
                        egui::Image::new(&tex)
                            .fit_to_exact_size(egui::vec2(h * 1.15, h * 1.15))
                            .tint(egui::Color32::WHITE),
                    );
                }
                None => {
                    // No art anywhere for this one: the word stands in for a
                    // button, so it reads as one rather than as prose.
                    ui.label(egui::RichText::new(fallback(p)).small().strong());
                }
            },
        }
    }
}

#[derive(PartialEq, Debug)]
enum Part<'a> {
    Text(&'a str),
    Pin(&'a str),
}

/// Split a hint into its literal runs and its `{pin}` placeholders.
///
/// An unclosed `{` is literal text: a hint is authored in this file's own source,
/// so the useful behaviour for a typo is to show it, not to swallow the rest of
/// the line.
fn parts(hint: &str) -> Vec<Part<'_>> {
    let mut out = Vec::new();
    let mut rest = hint;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|i| open + i) else { break };
        if open > 0 {
            out.push(Part::Text(&rest[..open]));
        }
        out.push(Part::Pin(&rest[open + 1..close]));
        rest = &rest[close + 1..];
    }
    if !rest.is_empty() {
        out.push(Part::Text(rest));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hint_splits_into_words_and_buttons() {
        assert_eq!(
            parts("{btn_south} Start · {dpad_left}{dpad_right} method"),
            vec![
                Part::Pin("btn_south"),
                Part::Text(" Start · "),
                Part::Pin("dpad_left"),
                Part::Pin("dpad_right"),
                Part::Text(" method"),
            ]
        );
    }

    #[test]
    fn a_line_with_no_buttons_is_one_run() {
        assert_eq!(parts("Aim straight ahead first"), vec![Part::Text("Aim straight ahead first")]);
        assert_eq!(parts(""), vec![]);
    }

    #[test]
    fn an_unclosed_brace_stays_text_rather_than_eating_the_line() {
        assert_eq!(parts("press {btn_south to go"), vec![Part::Text("press {btn_south to go")]);
    }

    #[test]
    fn every_button_a_hint_names_has_something_to_show() {
        // Either art in some family, or a word. A hint that renders as nothing
        // is worse than one that renders as the wrong shape.
        for pin in [
            "btn_south", "btn_east", "btn_west", "btn_north",
            "dpad_left", "dpad_right", "dpad_up", "dpad_down",
        ] {
            let art = crate::canvas::remapper_icons::gp_pin_svg(
                crate::canvas::remapper_icons::Skin::Xbox,
                pin,
            );
            assert!(
                art.is_some() || fallback(pin) != pin,
                "`{pin}` would render as its own id"
            );
        }
    }
}
