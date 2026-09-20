//! Scrolling inside a node body, where egui's own scrolling can't reach.
//!
//! Two things are in the way, both from the canvas being an `egui::Scene`:
//!
//! 1. **The canvas pans with the wheel.** A Scene pans by the scroll delta it
//!    reads from the input *before* the node bodies are drawn
//!    (`register_pan_and_zoom` runs ahead of the contents), and nothing consumes
//!    it first — so a scroll area in a body scrolled *and* dragged the whole
//!    canvas along with it.
//! 2. **A scroll area in a body never sees the pointer.** `ScrollArea` only takes
//!    the wheel while `ui.rect_contains_pointer(..)` holds, and that asks
//!    `Context::layer_id_at`, which walks *registered areas* only. A Scene's
//!    layer is a sublayer of a panel, not an `Area`, so the lookup finds nothing
//!    and the area concludes the pointer is elsewhere — however plainly it is
//!    over the text. (Ordinary widget hovering is fine: that goes through the
//!    widget hit test, not this.)
//!
//! So the body claims the wheel over its rect, the canvas lifts the delta out of
//! the input before the Scene can pan with it, and the body applies what it was
//! given to its own scroll offset by hand. [`scrolling_body`] does all three.

use std::collections::HashMap;

use egui::{Context, Id, Rect, Ui, Vec2, ViewportId};

/// Where the claims and the lifted delta live in egui's temp memory.
const CLAIMS: &str = "fxi_wheel_claims";
const HELD: &str = "fxi_wheel_held";

#[derive(Clone, Copy)]
struct Claim {
    /// Pointer positions are per-viewport, so a rect only means anything in the
    /// viewport that drew it (`ctx.data` itself is shared across all of them).
    viewport: ViewportId,
    /// In global coordinates — the body draws inside the Scene's transformed
    /// layer, the pointer is reported outside it.
    rect: Rect,
    /// The pass that drew it, so a node that scrolled off screen or went away
    /// stops holding the wheel.
    pass: u64,
}

#[derive(Clone, Default)]
struct Claims(HashMap<Id, Claim>);

#[derive(Clone, Copy)]
struct Held {
    claimant: Id,
    delta: Vec2,
}

/// A scroll area in a node body: it takes the wheel turned over it, keeps the
/// canvas from panning with it, and scrolls itself.
///
/// Pass the area configured however you like; its offset is this function's
/// business. Anywhere egui can scroll on its own (the config overlay, say) this
/// simply shows the area, because there is no lifted delta to apply.
pub(crate) fn scrolling_body<R>(
    ui: &mut Ui,
    claimant: Id,
    area: egui::ScrollArea,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::scroll_area::ScrollAreaOutput<R> {
    let offset_key = here(ui.ctx(), claimant).with("offset");
    let wheel = take_claimed_wheel(ui.ctx(), claimant);
    let mut area = area;
    if wheel.y != 0.0 {
        // Where we left it, plus this turn of the wheel. Over- or under-shooting
        // is fine: the area clamps the offset to its own content.
        let was = ui.ctx().data(|d| d.get_temp::<f32>(offset_key)).unwrap_or(0.0);
        area = area.vertical_scroll_offset(was - wheel.y);
    }
    let out = area.show(ui, add_contents);
    // Remember where it ended up — including a drag of the scroll bar, so the
    // wheel carries on from wherever the bar was left.
    ui.ctx().data_mut(|d| d.insert_temp(offset_key, out.state.offset.y));

    // The box the wheel belongs to, from the next frame on. `inner_rect` stops
    // at the content, so take in the scroll bar's lane beside it.
    let bar = ui.style().spacing.scroll.allocated_width();
    let claimed = out.inner_rect.with_max_x(out.inner_rect.max.x + bar);
    claim_wheel(ui, claimant, claimed);
    out
}

/// The same body, drawn in two viewports (a node body and its pinned copy on the
/// config overlay), is two claimants — `ctx.data` is shared across viewports, and
/// pointer positions are not.
fn here(ctx: &Context, claimant: Id) -> Id {
    claimant.with(ctx.viewport_id())
}

/// Ask for the wheel over `rect` (in this `Ui`'s coordinates), from the next
/// frame on. Call it every frame the body draws.
pub(crate) fn claim_wheel(ui: &Ui, claimant: Id, rect: Rect) {
    let ctx = ui.ctx();
    let to_global = ctx.layer_transform_to_global(ui.layer_id()).unwrap_or_default();
    let claim = Claim {
        viewport: ctx.viewport_id(),
        rect: to_global * rect,
        pass: ctx.cumulative_pass_nr(),
    };
    let key = here(ctx, claimant);
    ctx.data_mut(|d| {
        let claims = d.get_temp_mut_or_default::<Claims>(Id::new(CLAIMS));
        claims.0.insert(key, claim);
    });
}

/// Take the wheel out of the input if the pointer is over a claimed rect, so the
/// canvas doesn't pan with it. Call this immediately before the Scene runs.
pub(crate) fn take_wheel_from_canvas(ctx: &Context) {
    ctx.data_mut(|d| d.remove::<Held>(Id::new(HELD)));

    let pass = ctx.cumulative_pass_nr();
    let viewport = ctx.viewport_id();
    let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else { return };
    let claimant = ctx.data_mut(|d| {
        let claims = d.get_temp_mut_or_default::<Claims>(Id::new(CLAIMS));
        // A claim is good for the frame after it was made; older than that and
        // the body that made it isn't on screen any more.
        claims.0.retain(|_, c| pass.saturating_sub(c.pass) <= 1);
        claims.0.iter()
            .find(|(_, c)| c.viewport == viewport && c.rect.contains(pos))
            .map(|(id, _)| *id)
    });
    let Some(claimant) = claimant else { return };

    let delta = ctx.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta));
    if delta != Vec2::ZERO {
        ctx.data_mut(|d| d.insert_temp(Id::new(HELD), Held { claimant, delta }));
    }
}

/// What the canvas lifted for this body this frame, taken so it is used once.
fn take_claimed_wheel(ctx: &Context, claimant: Id) -> Vec2 {
    let key = here(ctx, claimant);
    let held = ctx.data(|d| d.get_temp::<Held>(Id::new(HELD)))
        .filter(|h| h.claimant == key);
    match held {
        Some(h) => {
            ctx.data_mut(|d| d.remove::<Held>(Id::new(HELD)));
            h.delta
        }
        None => Vec2::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHEEL: Vec2 = egui::vec2(0.0, -40.0);
    /// Where the pointer sits — inside the scroll area the test draws.
    const POINTER: egui::Pos2 = egui::pos2(60.0, 60.0);

    fn claimant() -> Id { Id::new("test_body") }

    /// One pass of a canvas with a scrolling body in it. Returns what the Scene
    /// would have panned by, and where the body's scroll area ended up.
    fn pass(ctx: &Context, pointer: egui::Pos2, wheel: Vec2) -> (Vec2, f32) {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0))),
            ..Default::default()
        };
        input.events.push(egui::Event::PointerMoved(pointer));
        if wheel != Vec2::ZERO {
            input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: wheel,
                modifiers: Default::default(),
            });
        }
        let (mut panned, mut offset) = (Vec2::ZERO, 0.0);
        let _ = ctx.run(input, |ctx| {
            take_wheel_from_canvas(ctx);
            // What the canvas's Scene would pan by, where it reads it.
            panned = ctx.input(|i| i.smooth_scroll_delta);
            egui::CentralPanel::default().show(ctx, |ui| {
                let out = scrolling_body(
                    ui,
                    claimant(),
                    egui::ScrollArea::vertical().id_salt("editor").max_height(100.0),
                    |ui| ui.label("line\n".repeat(60)),
                );
                offset = out.state.offset.y;
            });
        });
        (panned, offset)
    }

    // The whole point: the wheel over the body scrolls the body, and the canvas
    // stays where it was.
    #[test]
    fn the_wheel_over_a_body_scrolls_it_and_not_the_canvas() {
        let ctx = Context::default();
        // The first pass has nothing claimed yet, so the canvas still gets it.
        let (panned, offset) = pass(&ctx, POINTER, WHEEL);
        assert_ne!(panned, Vec2::ZERO, "an unclaimed frame still pans the canvas");
        assert_eq!(offset, 0.0);

        let (panned, offset) = pass(&ctx, POINTER, WHEEL);
        assert_eq!(panned, Vec2::ZERO, "the canvas must not pan under the editor");
        assert!(offset > 0.0, "and the editor scrolls instead");

        // Scrolling on: the offset keeps moving, and back up again. egui smooths
        // a turn of the wheel over several frames, so the way back takes a few.
        let (_, further) = pass(&ctx, POINTER, WHEEL);
        assert!(further > offset, "another turn scrolls further: {further} vs {offset}");
        let mut peak = further;
        let mut back = further;
        for _ in 0..6 {
            let (_, o) = pass(&ctx, POINTER, -WHEEL);
            peak = peak.max(o);
            back = o;
        }
        assert!(back < peak, "and the other way comes back: {back} vs {peak}");
    }

    // egui hands a turn of the wheel over as a few frames' worth of smaller
    // deltas, so the body has to add them up from where it left off. Restarting
    // each frame would land a fraction of the way down.
    #[test]
    fn a_turn_of_the_wheel_adds_up_over_the_frames_it_is_smoothed_across() {
        let ctx = Context::default();
        pass(&ctx, POINTER, Vec2::ZERO); // the first pass only claims the rect
        pass(&ctx, POINTER, WHEEL);
        let mut offset = 0.0;
        for _ in 0..10 {
            offset = pass(&ctx, POINTER, Vec2::ZERO).1;
        }
        assert!(offset > 30.0,
            "a 40-point turn should land about 40 points down, not {offset}");
    }

    // Away from the body, the canvas keeps the wheel and the body sits still.
    #[test]
    fn the_wheel_elsewhere_is_left_to_the_canvas() {
        let ctx = Context::default();
        pass(&ctx, POINTER, WHEEL);
        let (_, scrolled) = pass(&ctx, POINTER, WHEEL);
        assert!(scrolled > 0.0);

        let (panned, offset) = pass(&ctx, egui::pos2(380.0, 280.0), WHEEL);
        assert_ne!(panned, Vec2::ZERO, "away from the editor the canvas pans again");
        assert_eq!(offset, scrolled, "and the editor stays where it was");
    }

    // A body that stops drawing (scrolled off screen, node deleted) stops holding
    // the wheel rather than leaving a dead rect behind.
    #[test]
    fn a_claim_lapses_once_the_body_stops_drawing() {
        let ctx = Context::default();
        pass(&ctx, POINTER, WHEEL);
        let (panned, _) = pass(&ctx, POINTER, WHEEL);
        assert_eq!(panned, Vec2::ZERO);

        // Passes that draw no body at all: the claim holds for one, then lapses.
        let no_body = |ctx: &Context| {
            let mut seen = Vec2::ZERO;
            let _ = ctx.run(egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(POINTER),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: WHEEL,
                        modifiers: Default::default(),
                    },
                ],
                ..Default::default()
            }, |ctx| {
                take_wheel_from_canvas(ctx);
                seen = ctx.input(|i| i.smooth_scroll_delta);
            });
            seen
        };
        no_body(&ctx);
        assert_ne!(no_body(&ctx), Vec2::ZERO, "a stale claim lets the canvas pan again");
    }
}
