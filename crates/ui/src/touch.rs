//! Touchscreen helpers for the app around the graph; the graph's own are in
//! `egui_snarl::ui::touch`, which also decides when a finger is what's in use.

use egui::containers::menu::{MenuButton, MenuConfig};

pub(crate) use egui_snarl::ui::touch::{
    context_menu, is_active, menu_list_scroll, menu_scroll, menu_style_for,
};

/// [`egui::Ui::menu_button`] for a menu button outside a menu, sized for the input that
/// opens it. Inside a menu, the plain one already inherits the menu's size.
pub(crate) fn menu_button<'a, R>(
    ui: &mut egui::Ui,
    title: impl egui::IntoAtoms<'a>,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let config = MenuConfig::new().style(menu_style_for(ui.ctx()));
    let (response, inner) = MenuButton::new(title).config(config).ui(ui, content);
    egui::InnerResponse::new(inner.map(|inner| inner.inner), response)
}

/// How far a finger travels before its movement counts, as egui judges a drag.
const DRAG_DISTANCE: f32 = 6.0;

/// How far from a bend handle a second finger still finds it, in points on screen.
const BEND_REACH: f32 = 28.0;

/// What a finger can belong to. A finger goes to the most specific thing it came down on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ScrollRank {
    /// A whole page of widgets.
    Page,
    /// A widget on that page that scrolls by itself.
    Widget,
    /// A handle a second finger drags.
    Handle,
}

#[derive(Clone, Copy)]
struct Offer {
    owner: egui::Id,
    rank: ScrollRank,
    distance: f32,
}

/// One finger on the screen, tracked from the raw touch events: egui only follows one of
/// them, as the pointer, and tells its widgets nothing about the rest.
#[derive(Clone, Copy)]
struct Finger {
    id: egui::TouchId,
    origin: egui::Pos2,
    pos: egui::Pos2,
    /// Movement this pass.
    delta: egui::Vec2,
    /// The one egui-winit turns into the pointer.
    is_pointer: bool,
    /// Has travelled far enough that it isn't a tap or a wobble.
    moved: bool,
    /// Came down this pass, so whatever it landed on can still offer to take it.
    fresh: bool,
    ended: bool,
    /// What this finger works, for as long as it's down. Decided once, when it lands, so
    /// that a gesture can't slide from one thing to another.
    owner: Option<egui::Id>,
    offer: Option<Offer>,
}

#[derive(Clone, Default)]
struct Fingers {
    pass: Option<u64>,
    list: Vec<Finger>,
}

impl Fingers {
    /// `any_touches` is egui's own word on whether a finger is down. Nothing has to be asking
    /// about fingers on the frame one lifts, so this is what stops one staying down forever.
    fn advance(
        &mut self,
        pass: u64,
        touches: &[(egui::TouchId, egui::TouchPhase, egui::Pos2)],
        any_touches: bool,
    ) {
        self.pass = Some(pass);
        self.list.retain(|f| !f.ended);
        for f in &mut self.list {
            f.delta = egui::Vec2::ZERO;
            f.fresh = false;
            if let Some(offer) = f.offer.take() {
                f.owner = Some(offer.owner);
            }
        }
        for &(id, phase, pos) in touches {
            if phase == egui::TouchPhase::Start {
                // As egui-winit does: the pointer is the first finger down while it has none.
                let is_pointer = !self.list.iter().any(|f| f.is_pointer && !f.ended);
                self.list.retain(|f| f.id != id);
                self.list.push(Finger {
                    id,
                    origin: pos,
                    pos,
                    delta: egui::Vec2::ZERO,
                    is_pointer,
                    moved: false,
                    fresh: true,
                    ended: false,
                    owner: None,
                    offer: None,
                });
            } else if let Some(f) = self.list.iter_mut().find(|f| f.id == id) {
                f.delta += pos - f.pos;
                f.pos = pos;
                f.moved |= pos.distance(f.origin) > DRAG_DISTANCE;
                f.ended = phase != egui::TouchPhase::Move;
            }
        }
        if !any_touches {
            for f in &mut self.list {
                f.ended = true;
            }
        }
    }
}

impl Finger {
    /// Take up an offer if it's better than the one this finger already has. The later of
    /// two equals wins, as the later is drawn on top.
    fn offer(&mut self, offer: Offer) {
        let better = self.offer.is_none_or(|o| {
            offer.rank > o.rank || (offer.rank == o.rank && offer.distance <= o.distance)
        });
        if better {
            self.offer = Some(offer);
        }
    }
}

/// Bring the fingers up to this pass, once, and work with them. Per viewport, as touches
/// are. Runs inside `ctx.data_mut`, so `f` can't touch `ctx`.
fn with_fingers<R>(ctx: &egui::Context, f: impl FnOnce(&mut Fingers) -> R) -> R {
    let id = egui::Id::new("flexinput_fingers").with(ctx.viewport_id());
    let pass = ctx.cumulative_pass_nr();
    let stale = ctx.data_mut(|d| d.get_temp_mut_or_default::<Fingers>(id).pass != Some(pass));
    let (touches, any_touches): (Vec<_>, bool) = if stale {
        ctx.input(|i| {
            let touches = i.events.iter().filter_map(|e| match e {
                egui::Event::Touch { id, phase, pos, .. } => Some((*id, *phase, *pos)),
                _ => None,
            }).collect();
            (touches, i.any_touches())
        })
    } else {
        (Vec::new(), false)
    };
    ctx.data_mut(|d| {
        let fingers = d.get_temp_mut_or_default::<Fingers>(id);
        if stale {
            fingers.advance(pass, &touches, any_touches);
        }
        f(fingers)
    })
}

/// How far (in points, down is positive) fingers moved the content of a scroller that has
/// no widget of its own to catch a drag. `rect` is in screen space, `host` is the layer it
/// draws on, and `owner` stays the same from frame to frame. A scroller that has nothing
/// to scroll passes `can_scroll` false, and a finger landing on it goes to what's behind.
///
/// This is for scrollers holding widgets on transform layers of their own, which hide any
/// background widget from egui's hit test wherever they cover it. It also lets a second
/// finger scroll what the first isn't using, which egui has no idea of.
///
/// A finger that lands on the scroller stays its alone, at either end of its travel too:
/// the pointer's only while no widget took the drag, as a button under a finger that
/// moves stops being a click. Egui's hit test isn't public, so a second finger landing on
/// a button still scrolls; it couldn't press one anyway.
pub(crate) fn finger_scroll(
    ctx: &egui::Context,
    owner: egui::Id,
    rect: egui::Rect,
    host: egui::LayerId,
    rank: ScrollRank,
    can_scroll: bool,
) -> f32 {
    let landed: Vec<(egui::TouchId, egui::Pos2)> = with_fingers(ctx, |fingers| {
        fingers.list.iter()
            .filter(|f| can_scroll && f.fresh && rect.contains(f.origin))
            .map(|f| (f.id, f.origin))
            .collect()
    });
    // Sublayers aren't areas, so the top area at a point on the scroller is its root, or
    // the background if the root's own rect doesn't reach there. Anything else is a
    // window lying over the scroller.
    let root = ctx.memory(|mem| {
        let mut layer = host;
        while let Some(parent) = mem.areas().parent_layer(layer) {
            layer = parent;
        }
        layer
    });
    let on_host: Vec<egui::TouchId> = landed.into_iter()
        .filter(|&(_, origin)| {
            ctx.layer_id_at(origin)
                .is_some_and(|top| top == root || top.order == egui::Order::Background)
        })
        .map(|(id, _)| id)
        .collect();

    let popup_open = egui::Popup::is_any_open(ctx);
    let pointer_taken = ctx.dragged_id().is_some();
    with_fingers(ctx, |fingers| {
        let mut best = 0.0_f32;
        for f in &mut fingers.list {
            if on_host.contains(&f.id) {
                f.offer(Offer { owner, rank, distance: 0.0 });
            }
            let free = !popup_open && !(f.is_pointer && pointer_taken);
            if f.owner == Some(owner) && f.moved && free && f.delta.y.abs() > best.abs() {
                best = f.delta.y;
            }
        }
        best
    })
}

/// Is a finger down, or did the last one just lift? A finger other than the pointer can
/// be the last to go, on a frame that shows no other input.
pub(crate) fn fingers_working(i: &egui::InputState) -> bool {
    i.any_touches() || i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. }))
}

/// A graph's bend handles for touch. They show while a finger holds one of the graph's
/// points, so that another finger can bend the curve around it, which only Alt could ask
/// for. That finger isn't the pointer, so the handles' own responses never hear of it.
///
/// Begin before drawing the handles, ask `finger` for each, and `finish` after the points.
pub(crate) struct BendFingers {
    key: egui::Id,
    touch: bool,
    exposed: bool,
    held: bool,
}

impl BendFingers {
    pub(crate) fn begin(ui: &egui::Ui, graph: egui::Id) -> Self {
        let key = graph.with("bends_exposed");
        let touch = is_active(ui.ctx());
        let exposed = touch && ui.ctx().data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
        // Fingers are tracked only when asked about, and this graph needs to have seen the
        // first one land to tell it from the second, before it has a handle to show.
        if touch {
            with_fingers(ui.ctx(), |_| ());
        }
        Self { key, touch, exposed, held: false }
    }

    pub(crate) fn exposed(&self) -> bool {
        self.exposed
    }

    /// A handle's dot, big enough to see beside a finger when it's up for one.
    pub(crate) fn dot_radius(&self, radius: f32) -> f32 {
        if self.exposed { radius * 1.6 } else { radius }
    }

    /// Is a second finger working this handle? What it moved vertically this frame, in the
    /// `Ui`'s coordinates, as `Response::drag_delta` would say.
    pub(crate) fn finger(&mut self, ui: &egui::Ui, handle: egui::Id, at: egui::Pos2) -> Option<f32> {
        if !self.exposed {
            return None;
        }
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
        let center = to_global * at;
        let moved = with_fingers(ui.ctx(), |fingers| {
            let mut moved = None;
            for f in &mut fingers.list {
                if f.is_pointer {
                    continue;
                }
                let distance = f.origin.distance(center);
                if f.fresh && f.owner.is_none() && distance <= BEND_REACH {
                    f.offer(Offer { owner: handle, rank: ScrollRank::Handle, distance });
                }
                if f.owner == Some(handle) {
                    moved = Some(if f.moved { f.delta.y } else { 0.0 });
                }
            }
            moved
        });
        self.held |= moved.is_some();
        moved.map(|y| y / to_global.scaling)
    }

    /// Egui takes a finger resting on a point for a right click, which deletes the point.
    /// With another finger down, it's holding the point for that finger's sake.
    pub(crate) fn keeps_point(&self, ui: &egui::Ui, point: &egui::Response) -> bool {
        point.long_touched()
            && with_fingers(ui.ctx(), |fingers| fingers.list.iter().filter(|f| !f.ended).count() > 1)
    }

    /// How far a finger resting on this point is through deleting it, 0..1, if it is. Egui
    /// calls it a right click once the finger has stayed for its long-touch time.
    fn removal_progress(&self, ui: &egui::Ui, point: &egui::Response) -> Option<f32> {
        let alone = with_fingers(ui.ctx(), |fingers| fingers.list.iter().filter(|f| !f.ended).count() <= 1);
        if !self.touch || !alone || !point.is_pointer_button_down_on() {
            return None;
        }
        let long_touch = ui.ctx().options(|o| o.input_options.max_click_duration);
        ui.input(|i| {
            if !i.pointer.could_any_button_be_click() {
                return None;
            }
            let held = i.time - i.pointer.press_start_time()?;
            Some((held / long_touch).clamp(0.0, 1.0) as f32)
        })
    }

    /// Warn that the point under a resting finger is about to go. Moving off in time keeps
    /// it. Call this for a point that can be deleted, drawn over the graph's own clip so
    /// that the ring shows whole around a point at its edge.
    pub(crate) fn removal_ring(&self, ui: &egui::Ui, point: &egui::Response, at: egui::Pos2) {
        const WARNING: egui::Color32 = egui::Color32::from_rgb(235, 80, 60);
        if let Some(progress) = self.removal_progress(ui, point) {
            let scale = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default().scaling;
            let painter = ui.painter().with_clip_rect(egui::Rect::EVERYTHING);
            egui_snarl::ui::touch::paint_hold_ring(&painter, at, scale, progress, WARNING);
            ui.ctx().request_repaint();
        }
    }

    /// `point_down`: a finger is holding one of the graph's points. Handles stay until the
    /// fingers on them and on the points are all gone.
    pub(crate) fn finish(self, ui: &egui::Ui, point_down: bool) {
        if !self.touch {
            return;
        }
        let holding = ui.input(|i| i.pointer.primary_down());
        let exposed = point_down || (self.exposed && (holding || self.held));
        if exposed != self.exposed {
            ui.ctx().data_mut(|d| d.insert_temp(self.key, exposed));
        }
        if exposed {
            egui_snarl::ui::touch::block_pinch(ui.ctx());
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, RawInput, TouchDeviceId, TouchId, TouchPhase, pos2};

    fn touch(phase: TouchPhase, pos: Pos2) -> Event {
        Event::Touch {
            device_id: TouchDeviceId(1),
            id: TouchId(1),
            phase,
            pos,
            force: None,
        }
    }

    pub(crate) fn input(time: f64, events: Vec<Event>) -> RawInput {
        RawInput { time: Some(time), events, ..Default::default() }
    }

    /// The pointer events egui-winit sends alongside a touch.
    pub(crate) fn finger_down(pos: Pos2) -> Vec<Event> {
        vec![
            touch(TouchPhase::Start, pos),
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ]
    }

    pub(crate) fn finger_move(pos: Pos2) -> Vec<Event> {
        vec![touch(TouchPhase::Move, pos), Event::PointerMoved(pos)]
    }

    pub(crate) fn finger_up(pos: Pos2) -> Vec<Event> {
        vec![
            touch(TouchPhase::End, pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
            Event::PointerGone,
        ]
    }

    fn run(ctx: &egui::Context, raw: RawInput, mut f: impl FnMut(&egui::Context)) {
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            f(ctx);
        });
    }

    /// Sizes follow the input used last, not whether a touchscreen exists.
    #[test]
    fn touch_mode_follows_the_last_input() {
        let ctx = egui::Context::default();
        let seen = std::cell::Cell::new(None);
        let probe = |ctx: &egui::Context| seen.set(Some(is_active(ctx)));

        run(&ctx, input(0.0, vec![]), probe);
        assert_eq!(seen.get(), Some(false), "a mouse until shown otherwise");
        run(&ctx, input(0.1, finger_down(pos2(10.0, 10.0))), probe);
        assert_eq!(seen.get(), Some(true));
        run(&ctx, input(0.2, vec![]), probe);
        assert_eq!(seen.get(), Some(true), "stays while nothing happens");
        run(&ctx, input(0.3, vec![Event::PointerMoved(pos2(20.0, 20.0))]), probe);
        assert_eq!(seen.get(), Some(false), "a pointer event with no touch is a mouse");
    }

    /// A finger egui-winit doesn't turn into the pointer: only `Touch` events.
    pub(crate) fn other_finger(phase: TouchPhase, pos: Pos2) -> Vec<Event> {
        vec![Event::Touch {
            device_id: TouchDeviceId(1),
            id: TouchId(2),
            phase,
            pos,
            force: None,
        }]
    }

    /// A page with a list on it, as Easy mode's body and a pinned Remapper are.
    struct Scrollers {
        ctx: egui::Context,
        time: f64,
        list_can_scroll: bool,
        page_dy: f32,
        list_dy: f32,
    }

    impl Scrollers {
        const LIST_AT: Pos2 = pos2(300.0, 300.0);
        const PAGE_AT: Pos2 = pos2(30.0, 30.0);

        fn new() -> Self {
            let mut s = Self {
                ctx: egui::Context::default(),
                time: 0.0,
                list_can_scroll: true,
                page_dy: 0.0,
                list_dy: 0.0,
            };
            s.frame(vec![]);
            s
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.time += 1.0 / 60.0;
            let page = egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
            let list = egui::Rect::from_min_size(pos2(200.0, 200.0), egui::vec2(200.0, 200.0));
            let host = egui::LayerId::background();
            let (mut page_dy, mut list_dy) = (0.0, 0.0);
            let can = self.list_can_scroll;
            run(&self.ctx, input(self.time, events), |ctx| {
                let (page_id, list_id) = (egui::Id::new("page"), egui::Id::new("list"));
                page_dy = finger_scroll(ctx, page_id, page, host, ScrollRank::Page, true);
                list_dy = finger_scroll(ctx, list_id, list, host, ScrollRank::Widget, can);
            });
            self.page_dy = page_dy;
            self.list_dy = list_dy;
        }
    }

    /// A drag scrolls once it's clear it isn't a tap, by what the finger moved.
    #[test]
    fn a_finger_drag_scrolls_past_the_click_distance() {
        let mut s = Scrollers::new();
        s.frame(finger_down(Scrollers::PAGE_AT));
        assert_eq!(s.page_dy, 0.0);
        s.frame(finger_move(Scrollers::PAGE_AT + egui::vec2(0.0, 2.0)));
        assert_eq!(s.page_dy, 0.0, "could still be a tap");
        s.frame(finger_move(Scrollers::PAGE_AT + egui::vec2(0.0, 30.0)));
        assert_eq!(s.page_dy, 28.0);
        assert_eq!(s.list_dy, 0.0);
    }

    /// A finger on the list scrolls the list and never the page, including after a frame in
    /// which it didn't move, which is what used to let the page join in.
    #[test]
    fn a_finger_on_a_list_never_scrolls_the_page() {
        let mut s = Scrollers::new();
        let at = Scrollers::LIST_AT;
        s.frame(finger_down(at));
        s.frame(finger_move(at + egui::vec2(0.0, 10.0)));
        assert_eq!((s.page_dy, s.list_dy), (0.0, 10.0));
        for _ in 0..3 {
            s.frame(vec![]);
            assert_eq!((s.page_dy, s.list_dy), (0.0, 0.0));
        }
        s.frame(finger_move(at + egui::vec2(0.0, 25.0)));
        assert_eq!((s.page_dy, s.list_dy), (0.0, 15.0));
        s.frame(finger_move(at + egui::vec2(0.0, 300.0)));
        assert_eq!(s.page_dy, 0.0, "even leaving the list keeps it the list's");
    }

    /// A list with nothing to scroll doesn't get in the way of the page.
    #[test]
    fn a_list_that_fits_lets_the_page_scroll() {
        let mut s = Scrollers::new();
        s.list_can_scroll = false;
        let at = Scrollers::LIST_AT;
        s.frame(finger_down(at));
        s.frame(finger_move(at + egui::vec2(0.0, 10.0)));
        assert_eq!((s.page_dy, s.list_dy), (10.0, 0.0));
    }

    /// One finger on the list, another on the page: each scrolls its own.
    #[test]
    fn a_second_finger_scrolls_the_page_while_the_first_scrolls_the_list() {
        let mut s = Scrollers::new();
        let (list, page) = (Scrollers::LIST_AT, Scrollers::PAGE_AT);
        s.frame(finger_down(list));
        s.frame(other_finger(TouchPhase::Start, page));
        s.frame(finger_move(list + egui::vec2(0.0, 10.0)));
        assert_eq!((s.page_dy, s.list_dy), (0.0, 10.0));
        s.frame(other_finger(TouchPhase::Move, page + egui::vec2(0.0, 20.0)));
        assert_eq!((s.page_dy, s.list_dy), (20.0, 0.0));
        let mut both = finger_move(list + egui::vec2(0.0, 13.0));
        both.extend(other_finger(TouchPhase::Move, page + egui::vec2(0.0, 25.0)));
        s.frame(both);
        assert_eq!((s.page_dy, s.list_dy), (5.0, 3.0));
    }

    /// The page keeps following a finger that outlives the pointer.
    #[test]
    fn a_second_finger_keeps_scrolling_after_the_first_lifts() {
        let mut s = Scrollers::new();
        let (list, page) = (Scrollers::LIST_AT, Scrollers::PAGE_AT);
        s.frame(finger_down(list));
        s.frame(other_finger(TouchPhase::Start, page));
        s.frame(vec![
            touch(TouchPhase::End, list),
            Event::PointerButton {
                pos: list,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
            Event::PointerGone,
        ]);
        s.frame(other_finger(TouchPhase::Move, page + egui::vec2(0.0, 20.0)));
        assert_eq!(s.page_dy, 20.0);
    }

    /// A widget that took the pointer's drag keeps it, though a second finger still scrolls.
    #[test]
    fn a_widget_dragged_by_the_pointer_stops_the_pointer_scrolling() {
        let ctx = egui::Context::default();
        let page = egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let slider = egui::Rect::from_min_size(pos2(50.0, 50.0), egui::vec2(200.0, 200.0));
        let at = pos2(100.0, 100.0);
        let dy = std::cell::Cell::new(0.0);
        let frame = |t: f64, events: Vec<Event>| {
            let _ = ctx.run(input(t, events), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.interact(slider, egui::Id::new("slider"), egui::Sense::drag());
                });
                dy.set(finger_scroll(
                    ctx, egui::Id::new("page"), page, egui::LayerId::background(),
                    ScrollRank::Page, true,
                ));
            });
        };
        frame(0.0, vec![]);
        frame(0.02, vec![]);
        frame(0.04, finger_down(at));
        frame(0.06, finger_move(at + egui::vec2(0.0, 30.0)));
        assert_eq!(dy.get(), 0.0);
        let other = pos2(500.0, 500.0);
        frame(0.08, other_finger(TouchPhase::Start, other));
        frame(0.10, vec![]);
        frame(0.12, other_finger(TouchPhase::Move, other + egui::vec2(0.0, 30.0)));
        assert_eq!(dy.get(), 30.0);
    }

    /// A window lying over the page keeps its touches from it.
    #[test]
    fn a_window_over_the_page_keeps_its_touches() {
        let ctx = egui::Context::default();
        let page = egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let at = pos2(100.0, 100.0);
        let dy = std::cell::Cell::new(0.0);
        let frame = |t: f64, events: Vec<Event>| {
            run(&ctx, input(t, events), |ctx| {
                egui::Area::new(egui::Id::new("window")).fixed_pos(pos2(50.0, 50.0)).show(ctx, |ui| {
                    ui.allocate_exact_size(egui::vec2(200.0, 200.0), egui::Sense::hover());
                });
                dy.set(finger_scroll(
                    ctx, egui::Id::new("page"), page, egui::LayerId::background(),
                    ScrollRank::Page, true,
                ));
            });
        };
        frame(0.0, vec![]);
        frame(0.02, vec![]);
        frame(0.04, finger_down(at));
        frame(0.06, finger_move(at + egui::vec2(0.0, 30.0)));
        assert_eq!(dy.get(), 0.0);
    }

    /// A finger whose lift nobody saw doesn't stay down in the count.
    #[test]
    fn a_lift_nobody_saw_still_counts() {
        let ctx = egui::Context::default();
        let count = |t: f64, events: Vec<Event>, look: bool| {
            let mut n = None;
            run(&ctx, input(t, events), |ctx| {
                if look {
                    n = Some(with_fingers(ctx, |fingers| {
                        fingers.list.iter().filter(|f| !f.ended).count()
                    }));
                }
            });
            n
        };
        let at = pos2(100.0, 100.0);
        count(0.0, finger_down(at), true);
        assert_eq!(count(0.02, vec![], true), Some(1));
        count(0.04, vec![
            touch(TouchPhase::End, at),
            Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
            Event::PointerGone,
        ], false);
        assert_eq!(count(0.06, vec![], true), Some(0));
    }

    /// Bend handles appear while a finger holds a point, follow a second finger found near
    /// them, and leave when every finger has.
    #[test]
    fn bend_handles_follow_a_second_finger() {
        let ctx = egui::Context::default();
        let graph = egui::Id::new("graph");
        let (a, b) = (egui::Id::new("a"), egui::Id::new("b"));
        let (at_a, at_b) = (pos2(100.0, 100.0), pos2(130.0, 100.0));
        // (exposed, a's finger, b's finger)
        let frame = |t: f64, events: Vec<Event>, point_down: bool| {
            let mut seen = (false, None, None);
            run(&ctx, input(t, events), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut bends = BendFingers::begin(ui, graph);
                    seen = (
                        bends.exposed(),
                        bends.finger(ui, a, at_a),
                        bends.finger(ui, b, at_b),
                    );
                    bends.finish(ui, point_down);
                });
            });
            seen
        };
        let point = pos2(50.0, 50.0);
        assert_eq!(frame(0.0, vec![], false), (false, None, None));
        assert_eq!(frame(0.02, finger_down(point), true), (false, None, None));
        assert_eq!(frame(0.04, vec![], true), (true, None, None), "shown while a point is held");

        // Nearer b, though a's in reach too.
        let on = pos2(120.0, 100.0);
        frame(0.06, other_finger(TouchPhase::Start, on), true);
        assert_eq!(frame(0.08, vec![], true), (true, None, Some(0.0)), "b has it");
        let moved = other_finger(TouchPhase::Move, on + egui::vec2(0.0, 20.0));
        assert_eq!(frame(0.10, moved, true), (true, None, Some(20.0)));

        // The point's finger lifts: the handle's finger keeps it all up.
        let lift = vec![
            touch(TouchPhase::End, point),
            Event::PointerButton {
                pos: point,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
            Event::PointerGone,
        ];
        assert!(frame(0.12, lift, false).0);
        assert_eq!(frame(0.14, vec![], false), (true, None, Some(0.0)));
        frame(0.16, other_finger(TouchPhase::End, on + egui::vec2(0.0, 20.0)), false);
        frame(0.18, vec![], false);
        assert_eq!(frame(0.20, vec![], false), (false, None, None), "all gone: so are they");
    }

    /// A second finger that lands away from every handle isn't working one.
    #[test]
    fn a_second_finger_far_from_the_handles_finds_nothing() {
        let ctx = egui::Context::default();
        let frame = |t: f64, events: Vec<Event>| {
            let mut seen = None;
            run(&ctx, input(t, events), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut bends = BendFingers::begin(ui, egui::Id::new("graph"));
                    seen = bends.finger(ui, egui::Id::new("a"), pos2(100.0, 100.0));
                    bends.finish(ui, true);
                });
            });
            seen
        };
        frame(0.0, vec![]);
        frame(0.02, finger_down(pos2(50.0, 50.0)));
        frame(0.04, other_finger(TouchPhase::Start, pos2(400.0, 400.0)));
        assert_eq!(frame(0.06, other_finger(TouchPhase::Move, pos2(400.0, 450.0))), None);
    }

    /// The real editors, driven by two fingers: the one holding a point, the other bending
    /// the curve beside it.
    mod real_graphs {
        use super::*;
        use crate::canvas::NodeData;
        use crate::canvas::viewer::{paint_envelope_curve_graph, paint_response_curve_graph};
        use egui_snarl::{NodeId, Snarl};

        const RECT: egui::Rect = egui::Rect {
            min: pos2(100.0, 100.0),
            max: pos2(500.0, 500.0),
        };

        fn node(module_id: &str) -> NodeData {
            NodeData {
                module_id: module_id.to_string(),
                display_name: "graph".to_string(),
                category: "Test".to_string(),
                inputs: vec![],
                outputs: vec![],
                params: Default::default(),
                subpatch: None,
                extra: Default::default(),
            }
        }

        struct Rig {
            ctx: egui::Context,
            snarl: Snarl<NodeData>,
            id: NodeId,
            envelope: bool,
            time: f64,
        }

        impl Rig {
            fn new(module_id: &str) -> Self {
                let mut snarl = Snarl::new();
                let id = snarl.insert_node(Pos2::ZERO, node(module_id));
                let mut rig = Rig {
                    ctx: egui::Context::default(),
                    snarl,
                    id,
                    envelope: module_id == "module.envelope",
                    time: 0.0,
                };
                rig.frame(vec![]);
                rig.frame(vec![]);
                rig
            }

            fn frame(&mut self, events: Vec<Event>) {
                self.time += 1.0 / 30.0;
                let raw = RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 800.0))),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                };
                let Rig { ctx, snarl, id, envelope, .. } = self;
                let _ = ctx.run(raw, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let bg = ui.interact(RECT, egui::Id::new("bg"), egui::Sense::click());
                        if *envelope {
                            paint_envelope_curve_graph(*id, ui, snarl, RECT, bg, None);
                        } else {
                            paint_response_curve_graph(*id, ui, snarl, RECT, bg, false, None);
                        }
                    });
                });
            }

            fn wait(&mut self, secs: f64) {
                for _ in 0..(secs * 30.0).ceil() as usize {
                    self.frame(vec![]);
                }
            }

            fn points(&self) -> Vec<Vec<f64>> {
                serde_json::from_value(self.snarl.get_node(self.id).unwrap().params["points"].clone()).unwrap()
            }

            fn biases(&self) -> Vec<f64> {
                serde_json::from_value(self.snarl.get_node(self.id).unwrap().params["biases"].clone()).unwrap()
            }

            /// A point's place on screen: graphs run 0..1 both ways here, up the screen.
            fn screen(p: &[f64]) -> Pos2 {
                pos2(
                    RECT.left() + p[0] as f32 * RECT.width(),
                    RECT.bottom() - p[1] as f32 * RECT.height(),
                )
            }

            /// Enter touch mode; the widgets register a frame later.
            fn touch_mode(&mut self) {
                self.frame(finger_down(pos2(20.0, 20.0)));
                self.frame(vec![
                    touch(TouchPhase::End, pos2(20.0, 20.0)),
                    Event::PointerButton {
                        pos: pos2(20.0, 20.0),
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                    Event::PointerGone,
                ]);
                self.wait(0.1);
            }
        }

        fn bends_with_a_second_finger(module_id: &str) {
            let mut r = Rig::new(module_id);
            r.touch_mode();
            let points = r.points();
            let (first, second) = (Rig::screen(&points[0]), Rig::screen(&points[1]));
            r.frame(finger_down(first));
            r.frame(finger_move(first + egui::vec2(8.0, 0.0)));
            r.frame(vec![]);

            // The bend handle starts halfway along the straight segment.
            let handle = first + (second - first) * 0.5;
            let before = r.biases()[0];
            r.frame(other_finger(TouchPhase::Start, handle));
            r.frame(vec![]);
            r.frame(other_finger(TouchPhase::Move, handle + egui::vec2(0.0, -40.0)));
            r.frame(other_finger(TouchPhase::Move, handle + egui::vec2(0.0, -80.0)));
            assert!(r.biases()[0] > before + 0.01, "{module_id}: {:?}", r.biases());
        }

        #[test]
        fn a_response_curve_bends_under_a_second_finger() {
            bends_with_a_second_finger("module.response_curve");
        }

        #[test]
        fn an_envelope_bends_under_a_second_finger() {
            bends_with_a_second_finger("module.envelope");
        }

        /// The second finger bends; the first keeps moving its point meanwhile.
        #[test]
        fn the_first_finger_keeps_dragging_its_point() {
            let mut r = Rig::new("module.response_curve");
            r.touch_mode();
            let points = r.points();
            let first = Rig::screen(&points[0]);
            let handle = first + (Rig::screen(&points[1]) - first) * 0.5;
            r.frame(finger_down(first));
            r.frame(finger_move(first + egui::vec2(10.0, 0.0)));
            r.frame(other_finger(TouchPhase::Start, handle));
            r.frame(vec![]);
            let at = r.points()[0][0];
            r.frame(finger_move(first + egui::vec2(50.0, 0.0)));
            assert!(r.points()[0][0] > at, "{:?}", r.points());
        }

        /// How far through deleting a point the ring shows, seen from the real point widget's
        /// response through the same call the graphs make.
        #[test]
        fn the_removal_ring_runs_until_the_point_goes_or_the_finger_moves() {
            let ctx = egui::Context::default();
            let point = egui::Rect::from_center_size(pos2(100.0, 100.0), egui::vec2(12.0, 12.0));
            let frame = |t: f64, events: Vec<Event>| {
                let mut seen = None;
                run(&ctx, input(t, events), |ctx| {
                    egui::Area::new(egui::Id::new("graph")).fixed_pos(pos2(0.0, 0.0)).show(ctx, |ui| {
                        ui.allocate_exact_size(egui::vec2(300.0, 300.0), egui::Sense::hover());
                        let r = ui.interact(point, egui::Id::new("point"), egui::Sense::click_and_drag());
                        let bends = BendFingers::begin(ui, egui::Id::new("g"));
                        seen = bends.removal_progress(ui, &r);
                    });
                });
                seen
            };
            let at = point.center();
            frame(0.0, vec![]);
            frame(0.02, vec![]);
            assert_eq!(frame(0.1, finger_down(at)), Some(0.0));
            let half = frame(0.5, vec![]).expect("still holding");
            assert!((half - 0.5).abs() < 0.01, "{half}");
            assert_eq!(frame(1.0, vec![]), None, "past the long touch: it's gone");

            // Moving off cancels it.
            frame(1.1, finger_up(at));
            frame(1.2, vec![]);
            frame(1.3, finger_down(at));
            assert!(frame(1.4, vec![]).is_some());
            assert_eq!(frame(1.5, finger_move(at + egui::vec2(20.0, 0.0))), None);
            frame(1.6, finger_up(at + egui::vec2(20.0, 0.0)));

            // A second finger means it isn't going anywhere.
            frame(1.7, vec![]);
            frame(1.8, finger_down(at));
            assert!(frame(1.9, vec![]).is_some());
            assert_eq!(frame(2.0, other_finger(TouchPhase::Start, pos2(250.0, 250.0))), None);
        }

        /// The ring's end is when the point really goes.
        #[test]
        fn a_point_lasts_until_the_ring_is_full() {
            let mut r = Rig::new("module.response_curve");
            let id = r.id;
            r.snarl.get_node_mut(id).unwrap().params.insert(
                "points".into(),
                serde_json::json!([[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]),
            );
            r.snarl.get_node_mut(id).unwrap().params.insert("biases".into(), serde_json::json!([0.0, 0.0]));
            r.touch_mode();
            r.frame(vec![]);
            let middle = Rig::screen(&r.points()[1]);
            r.frame(finger_down(middle));
            r.wait(0.7);
            assert_eq!(r.points().len(), 3, "not yet");
            r.wait(0.2);
            assert_eq!(r.points().len(), 2, "gone");
        }

        /// A finger resting on a point for long enough deletes it, as a long touch does,
        /// but not while another finger is down.
        #[test]
        fn a_long_touch_deletes_a_point_only_alone() {
            for (second_finger, want) in [(false, 2), (true, 3)] {
                let mut r = Rig::new("module.response_curve");
                let id = r.id;
                r.snarl.get_node_mut(id).unwrap().params.insert(
                    "points".into(),
                    serde_json::json!([[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]),
                );
                r.snarl.get_node_mut(id).unwrap().params.insert(
                    "biases".into(),
                    serde_json::json!([0.0, 0.0]),
                );
                r.touch_mode();
                r.frame(vec![]);
                let middle = Rig::screen(&r.points()[1]);
                r.frame(finger_down(middle));
                if second_finger {
                    r.frame(other_finger(TouchPhase::Start, pos2(700.0, 700.0)));
                }
                r.wait(1.0);
                assert_eq!(r.points().len(), want, "second finger: {second_finger}");
            }
        }
    }

    /// The graph driven frame by frame, with the events egui-winit really sends. What matters
    /// here is the frames: the pointer vanishing with the finger, egui's long touch ending a
    /// drag at 0.8 s, a popup that needs a position on the frame there isn't one.
    mod graph {
        use super::*;
        use egui::Vec2;
        use egui_snarl::ui::{
            AnyPin, AnyPins, PinInfo, PinWireInfo, SnarlPin, SnarlStyle, SnarlViewer,
        };
        use egui_snarl::{InPin, InPinId, OutPin, OutPinId, Snarl};
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;

        type Pins = Rc<RefCell<HashMap<AnyPin, Pos2>>>;

        /// A pin that reports where it landed on screen.
        struct Probe {
            pin: AnyPin,
            seen: Pins,
        }

        impl SnarlPin for Probe {
            fn draw(
                self,
                snarl_style: &SnarlStyle,
                style: &egui::Style,
                rect: egui::Rect,
                painter: &egui::Painter,
            ) -> PinWireInfo {
                let to_global = painter.ctx().layer_transform_to_global(painter.layer_id())
                    .unwrap_or(egui::emath::TSTransform::IDENTITY);
                self.seen.borrow_mut().insert(self.pin, to_global * rect.center());
                PinInfo::circle().draw(snarl_style, style, rect, painter)
            }
        }

        #[derive(Default)]
        struct Viewer {
            pins: Pins,
            refused: Option<InPinId>,
            /// Each frame the dropped-wire menu showed: how many wires, and from where.
            drop_menu: Vec<(usize, Option<AnyPin>)>,
            graph_menu_frames: usize,
            /// Wires asked to disconnect, which is how a wire's menu is asked for.
            wire_menus: Vec<(OutPinId, InPinId)>,
            /// Give nodes a body with a widget that takes a drag, like a slider.
            sliders: bool,
            slider_at: Option<Pos2>,
            node_menu_frames: usize,
        }

        impl SnarlViewer<(usize, usize)> for Viewer {
            fn title(&mut self, _: &(usize, usize)) -> String {
                "node".to_string()
            }
            fn inputs(&mut self, node: &(usize, usize)) -> usize {
                node.0
            }
            fn outputs(&mut self, node: &(usize, usize)) -> usize {
                node.1
            }
            fn show_input(
                &mut self,
                pin: &InPin,
                _: &mut egui::Ui,
                _: &mut Snarl<(usize, usize)>,
            ) -> impl SnarlPin + 'static {
                Probe { pin: AnyPin::In(pin.id), seen: self.pins.clone() }
            }
            fn show_output(
                &mut self,
                pin: &OutPin,
                _: &mut egui::Ui,
                _: &mut Snarl<(usize, usize)>,
            ) -> impl SnarlPin + 'static {
                Probe { pin: AnyPin::Out(pin.id), seen: self.pins.clone() }
            }
            fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<(usize, usize)>) {
                if self.refused != Some(to.id) {
                    snarl.connect(from.id, to.id);
                }
            }
            fn disconnect(&mut self, from: &OutPin, to: &InPin, _: &mut Snarl<(usize, usize)>) {
                self.wire_menus.push((from.id, to.id));
            }
            fn can_connect(&mut self, _: &OutPin, to: &InPin, _: &Snarl<(usize, usize)>) -> bool {
                self.refused != Some(to.id)
            }
            fn has_body(&mut self, _: &(usize, usize)) -> bool {
                self.sliders
            }
            fn show_body(
                &mut self,
                _: egui_snarl::NodeId,
                _: &[InPin],
                _: &[OutPin],
                ui: &mut egui::Ui,
                _: &mut Snarl<(usize, usize)>,
            ) {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 60.0), egui::Sense::drag());
                let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
                    .unwrap_or(egui::emath::TSTransform::IDENTITY);
                self.slider_at = Some(to_global * rect.center());
            }
            fn has_node_menu(&mut self, _: &(usize, usize)) -> bool {
                true
            }
            fn show_node_menu(
                &mut self,
                _: egui_snarl::NodeId,
                _: &[InPin],
                _: &[OutPin],
                _: &mut egui::Ui,
                _: &mut Snarl<(usize, usize)>,
            ) {
                self.node_menu_frames += 1;
            }
            fn has_graph_menu(&mut self, _: Pos2, _: &mut Snarl<(usize, usize)>) -> bool {
                true
            }
            fn show_graph_menu(&mut self, _: Pos2, _: &mut egui::Ui, _: &mut Snarl<(usize, usize)>) {
                self.graph_menu_frames += 1;
            }
            fn has_dropped_wire_menu(&mut self, _: AnyPins, _: &mut Snarl<(usize, usize)>) -> bool {
                true
            }
            fn show_dropped_wire_menu(
                &mut self,
                _: Pos2,
                _: &mut egui::Ui,
                pins: AnyPins,
                moved_from: Option<AnyPin>,
                _: &mut Snarl<(usize, usize)>,
            ) {
                let n = match pins {
                    AnyPins::Out(pins) => pins.len(),
                    AnyPins::In(pins) => pins.len(),
                };
                self.drop_menu.push((n, moved_from));
            }
        }

        const EMPTY: Pos2 = pos2(30.0, 770.0);

        /// Source `a` wired to `b`, and `c` with its input free.
        struct Rig {
            ctx: egui::Context,
            snarl: Snarl<(usize, usize)>,
            viewer: Viewer,
            style: SnarlStyle,
            time: f64,
            modifiers: egui::Modifiers,
            a: OutPinId,
            b: InPinId,
            c: InPinId,
        }

        impl Rig {
            fn new() -> Self {
                Self::with_viewer(Viewer::default())
            }

            fn with_viewer(viewer: Viewer) -> Self {
                let mut snarl = Snarl::new();
                let a = snarl.insert_node(pos2(300.0, 300.0), (0, 1));
                let b = snarl.insert_node(pos2(600.0, 150.0), (1, 0));
                let c = snarl.insert_node(pos2(600.0, 450.0), (1, 0));
                let a = OutPinId { node: a, output: 0 };
                let b = InPinId { node: b, input: 0 };
                let c = InPinId { node: c, input: 0 };
                snarl.connect(a, b);
                let mut rig = Rig {
                    ctx: egui::Context::default(),
                    snarl,
                    viewer,
                    style: SnarlStyle::new(),
                    time: 0.0,
                    modifiers: Default::default(),
                    a,
                    b,
                    c,
                };
                rig.idle(5);
                for pin in rig.viewer.pins.borrow().values() {
                    assert!(pin.distance(EMPTY) > 80.0, "EMPTY must be far from the pins");
                }
                rig
            }

            fn frame(&mut self, events: Vec<Event>) {
                self.time += 1.0 / 30.0;
                let raw = RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))),
                    time: Some(self.time),
                    modifiers: self.modifiers,
                    events,
                    ..Default::default()
                };
                let Rig { ctx, snarl, viewer, style, .. } = self;
                let _ = ctx.run(raw, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        snarl.show(viewer, style, "graph", ui);
                    });
                });
            }

            fn idle(&mut self, frames: usize) {
                for _ in 0..frames {
                    self.frame(vec![]);
                }
            }

            fn wait(&mut self, secs: f64) {
                self.idle((secs * 30.0).ceil() as usize);
            }

            fn pin(&self, pin: AnyPin) -> Pos2 {
                self.viewer.pins.borrow()[&pin]
            }

            fn down(&mut self, pos: Pos2) {
                self.frame(finger_down(pos));
            }

            fn slide(&mut self, pos: Pos2) {
                self.frame(finger_move(pos));
            }

            fn up(&mut self, pos: Pos2) {
                self.frame(vec![
                    touch(TouchPhase::End, pos),
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                    Event::PointerGone,
                ]);
                self.idle(2);
            }

            /// Put the app in touch mode; pins are only widened from the next frame on.
            fn touch_mode(&mut self) {
                self.down(EMPTY);
                self.up(EMPTY);
                self.viewer.drop_menu.clear();
            }

            fn mouse_to(&mut self, pos: Pos2) {
                self.frame(vec![Event::PointerMoved(pos)]);
            }

            fn mouse_button(&mut self, pos: Pos2, pressed: bool) {
                self.frame(vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: self.modifiers,
                }]);
            }

            fn wires(&self) -> Vec<(OutPinId, InPinId)> {
                self.snarl.wires().collect()
            }
        }

        /// Holding a wired pin for the second picks its wires up; they land on the next pin
        /// and leave the old one. This hold outlives the point egui ends a drag.
        #[test]
        fn holding_a_pin_moves_its_wires() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b, c) = (r.a, r.b, r.c);
            let from = r.pin(AnyPin::In(b));
            r.down(from);
            r.wait(1.1);
            assert_eq!(r.wires(), [(a, b)], "still connected while in the air");
            let to = r.pin(AnyPin::In(c));
            r.slide(from + (to - from) * 0.5);
            r.slide(to);
            r.up(to);
            assert_eq!(r.wires(), [(a, c)]);
            assert!(r.viewer.drop_menu.is_empty());
        }

        /// Letting go of picked-up wires where they came from changes nothing.
        #[test]
        fn holding_an_outlet_lifts_its_wires_and_can_land_back() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b) = (r.a, r.b);
            let from = r.pin(AnyPin::Out(a));
            r.down(from);
            r.wait(1.1);
            r.up(from);
            assert_eq!(r.wires(), [(a, b)], "let go where it started: nothing changes");
        }

        /// Sliding off a wired pin before the second is up draws a new wire, as with a mouse.
        #[test]
        fn dragging_off_a_wired_pin_adds_a_wire() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b, c) = (r.a, r.b, r.c);
            let from = r.pin(AnyPin::Out(a));
            r.down(from);
            r.wait(0.2);
            let to = r.pin(AnyPin::In(c));
            r.slide(from + (to - from) * 0.5);
            r.slide(to);
            r.up(to);
            let mut wires = r.wires();
            wires.sort_by_key(|(_, i)| i.node.0);
            assert_eq!(wires, [(a, b), (a, c)]);
        }

        /// A fingertip wobbles: that isn't a drag, and doesn't lose the hold.
        #[test]
        fn a_wobbling_finger_still_holds() {
            let mut r = Rig::new();
            r.touch_mode();
            let (b, c) = (r.b, r.c);
            let from = r.pin(AnyPin::In(b));
            r.down(from);
            r.wait(0.3);
            r.slide(from + egui::vec2(9.0, 0.0));
            r.wait(0.8);
            let to = r.pin(AnyPin::In(c));
            r.slide(to);
            r.up(to);
            assert_eq!(r.wires(), [(r.a, c)]);
        }

        /// A pin with no wires has nothing to hold, but a long touch mustn't kill the drag
        /// either, as egui's does.
        #[test]
        fn a_long_touch_on_a_free_pin_still_draws_a_wire() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b, c) = (r.a, r.b, r.c);
            let from = r.pin(AnyPin::In(c));
            r.down(from);
            r.wait(0.9);
            let to = r.pin(AnyPin::Out(a));
            r.slide(from + (to - from) * 0.5);
            r.slide(to);
            r.up(to);
            let mut wires = r.wires();
            wires.sort_by_key(|(_, i)| i.node.0);
            assert_eq!(wires, [(a, b), (a, c)]);
        }

        /// Letting go early does nothing at all.
        #[test]
        fn letting_go_before_the_second_changes_nothing() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b) = (r.a, r.b);
            let from = r.pin(AnyPin::In(b));
            r.down(from);
            r.wait(0.5);
            r.up(from);
            r.idle(5);
            assert_eq!(r.wires(), [(a, b)]);
            assert!(r.viewer.drop_menu.is_empty());
        }

        /// The drop menu opens on the frame the pointer vanishes and stays once the finger
        /// is gone, knowing where the wires came from. Tapping away is a cancel.
        #[test]
        fn dropping_lifted_wires_on_empty_space_opens_the_menu() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b) = (r.a, r.b);
            let from = r.pin(AnyPin::In(b));
            r.down(from);
            r.wait(1.1);
            r.slide(from + (EMPTY - from) * 0.5);
            r.slide(EMPTY);
            r.up(EMPTY);
            r.idle(10);
            let last = *r.viewer.drop_menu.last().expect("menu opened");
            assert_eq!(last, (1, Some(AnyPin::In(b))));
            assert!(r.viewer.drop_menu.len() >= 10, "stays open with no finger down");
            assert_eq!(r.wires(), [(a, b)]);

            let shown = r.viewer.drop_menu.len();
            let away = pos2(1100.0, 30.0);
            r.down(away);
            r.up(away);
            r.idle(3);
            assert!(r.viewer.drop_menu.len() <= shown + 2, "tapping away closes it");
            assert_eq!(r.wires(), [(a, b)], "and the wires never left");
        }

        /// A new wire dropped on empty space opens the menu too, with nothing to delete.
        #[test]
        fn dropping_a_new_wire_on_empty_space_opens_the_menu() {
            let mut r = Rig::new();
            r.touch_mode();
            let from = r.pin(AnyPin::In(r.c));
            r.down(from);
            r.slide(from + (EMPTY - from) * 0.5);
            r.slide(EMPTY);
            r.up(EMPTY);
            assert_eq!(*r.viewer.drop_menu.last().expect("menu opened"), (1, None));
        }

        /// A wired bundle the target refuses stays where it was.
        #[test]
        fn a_refused_move_keeps_the_wires() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b, c) = (r.a, r.b, r.c);
            let from = r.pin(AnyPin::In(b));
            r.down(from);
            r.wait(1.1);
            r.viewer.refused = Some(c);
            let to = r.pin(AnyPin::In(c));
            r.slide(to);
            r.up(to);
            assert_eq!(r.wires(), [(a, b)]);
        }

        /// A finger is wide: the pin takes a touch well off its shape, and a wire dropped
        /// near one finds it.
        #[test]
        fn a_finger_gets_more_room_than_a_mouse() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b, c) = (r.a, r.b, r.c);
            let off = Vec2::new(14.0, 0.0);
            let from = r.pin(AnyPin::Out(a)) + off;
            r.down(from);
            let to = r.pin(AnyPin::In(c)) - Vec2::new(25.0, 12.0);
            r.slide(from + (to - from) * 0.5);
            r.slide(to);
            r.up(to);
            let mut wires = r.wires();
            wires.sort_by_key(|(_, i)| i.node.0);
            assert_eq!(wires, [(a, b), (a, c)]);

            // A mouse at the same spot just pans the canvas.
            let mut r = Rig::new();
            let (a, b) = (r.a, r.b);
            let from = r.pin(AnyPin::Out(a)) + off;
            let to = r.pin(AnyPin::In(r.c));
            r.mouse_to(from);
            r.mouse_to(from);
            r.mouse_button(from, true);
            r.mouse_to(to);
            r.mouse_to(to);
            r.mouse_button(to, false);
            r.idle(2);
            assert_eq!(r.wires(), [(a, b)]);
            assert!(r.viewer.drop_menu.is_empty());
        }

        /// A long touch on empty canvas opens the graph menu, which outlives the finger.
        #[test]
        fn the_graph_menu_outlives_the_finger() {
            let mut r = Rig::new();
            r.touch_mode();
            let spot = pos2(1100.0, 30.0);
            r.down(spot);
            r.wait(0.9);
            assert!(r.viewer.graph_menu_frames > 0, "long touch opens it");
            r.up(spot);
            let shown = r.viewer.graph_menu_frames;
            r.idle(5);
            assert_eq!(r.viewer.graph_menu_frames, shown + 5);
        }

        fn wire_middle(r: &Rig) -> Pos2 {
            let (from, to) = (r.pin(AnyPin::Out(r.a)), r.pin(AnyPin::In(r.b)));
            from + (to - from) * 0.5
        }

        /// Holding a finger on a wire for a second asks for that wire's menu, instead of the
        /// canvas's that a long touch would otherwise open on the way there.
        #[test]
        fn holding_a_wire_opens_its_menu() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b) = (r.a, r.b);
            let at = wire_middle(&r);
            r.down(at);
            r.wait(0.9);
            assert!(r.viewer.wire_menus.is_empty(), "not before the second is up");
            assert_eq!(r.viewer.graph_menu_frames, 0, "past egui's long touch, on a wire");
            r.wait(0.2);
            assert_eq!(r.viewer.wire_menus, [(a, b)]);
            assert_eq!(r.wires(), [(a, b)], "it's only a menu");
            r.wait(0.5);
            assert_eq!(r.viewer.wire_menus.len(), 1, "and only once");
            assert_eq!(r.viewer.graph_menu_frames, 0);
        }

        /// A finger near a wire finds it; one too far away gets the canvas's menu.
        #[test]
        fn a_wire_is_found_within_a_fingers_reach() {
            let mut r = Rig::new();
            r.touch_mode();
            let at = wire_middle(&r) + Vec2::new(0.0, 15.0);
            r.down(at);
            r.wait(1.1);
            assert_eq!(r.viewer.wire_menus.len(), 1);

            let mut r = Rig::new();
            r.touch_mode();
            let far = pos2(1100.0, 30.0);
            r.down(far);
            r.wait(1.1);
            assert!(r.viewer.wire_menus.is_empty());
            assert!(r.viewer.graph_menu_frames > 0);
        }

        /// Lifting early does nothing.
        #[test]
        fn letting_go_of_a_wire_early_does_nothing() {
            let mut r = Rig::new();
            r.touch_mode();
            let at = wire_middle(&r);
            r.down(at);
            r.wait(0.5);
            r.up(at);
            r.idle(40);
            assert!(r.viewer.wire_menus.is_empty());
        }

        /// The canvas holds still under a finger on a wire, wobbling or not, and moving off
        /// is an ordinary pan with no menu to follow.
        #[test]
        fn moving_off_a_wire_pans_the_canvas_instead() {
            let mut r = Rig::new();
            r.touch_mode();
            let pin = AnyPin::In(r.b);
            let at = wire_middle(&r);
            let start = r.pin(pin);
            r.down(at);
            r.slide(at + Vec2::new(9.0, 0.0));
            r.idle(2);
            assert_eq!(r.pin(pin), start, "a wobble doesn't pan");

            r.slide(at + Vec2::new(60.0, 0.0));
            r.slide(at + Vec2::new(90.0, 0.0));
            r.idle(2);
            assert!(r.pin(pin).x > start.x + 20.0, "{:?} -> {:?}", start, r.pin(pin));
            r.wait(1.2);
            assert!(r.viewer.wire_menus.is_empty());
        }

        /// Dragging the empty canvas with a finger still pans it, away from any wire.
        #[test]
        fn a_finger_still_pans_the_canvas() {
            let mut r = Rig::new();
            r.touch_mode();
            let pin = AnyPin::In(r.b);
            let start = r.pin(pin);
            let at = pos2(1100.0, 30.0);
            r.down(at);
            r.slide(at + Vec2::new(-40.0, 0.0));
            r.slide(at + Vec2::new(-80.0, 0.0));
            r.idle(2);
            assert!(r.pin(pin).x < start.x - 20.0);
        }

        /// A place on b's frame, clear of its pin's hit area.
        fn on_node(r: &Rig) -> Pos2 {
            r.pin(AnyPin::In(r.b)) + Vec2::new(30.0, 0.0)
        }

        /// How far apart the view shows two pins, which only a zoom changes.
        fn span(r: &Rig) -> f32 {
            r.pin(AnyPin::Out(r.a)).distance(r.pin(AnyPin::In(r.b)))
        }

        fn node_pos(r: &Rig) -> Pos2 {
            r.snarl.get_node_info(r.b.node).unwrap().pos
        }

        /// Spread two fingers apart around where they landed.
        fn spread(r: &mut Rig, first: Pos2, second: Pos2) {
            for step in 1..6 {
                let by = Vec2::new(0.0, step as f32 * 20.0);
                let mut events = finger_move(first - by);
                events.extend(other_finger(TouchPhase::Move, second + by));
                r.frame(events);
            }
        }

        /// One finger on a node still moves it, for the tests below to mean something.
        #[test]
        fn one_finger_on_a_node_moves_it() {
            let mut r = Rig::new();
            r.touch_mode();
            let (at, before) = (on_node(&r), node_pos(&r));
            r.down(at);
            r.slide(at + Vec2::new(40.0, 0.0));
            r.slide(at + Vec2::new(80.0, 0.0));
            assert!(node_pos(&r).x > before.x + 10.0);
        }

        /// One finger on a node and another beside it pinch the view. The node stays.
        #[test]
        fn a_pinch_over_a_node_zooms_and_leaves_the_node() {
            let mut r = Rig::new();
            r.touch_mode();
            let (at, before, zoom) = (on_node(&r), node_pos(&r), span(&r));
            let other = at + Vec2::new(300.0, 100.0);
            r.down(at);
            r.frame(other_finger(TouchPhase::Start, other));
            spread(&mut r, at, other);
            assert_eq!(node_pos(&r), before, "the node stays in the graph");
            assert!(span(&r) > zoom + 1.0, "{zoom} -> {}", span(&r));
        }

        /// Moving both fingers together over a node pans.
        #[test]
        fn two_fingers_over_a_node_pan() {
            let mut r = Rig::new();
            r.touch_mode();
            let (at, before) = (on_node(&r), node_pos(&r));
            let other = at + Vec2::new(300.0, 0.0);
            let pin = r.pin(AnyPin::In(r.c));
            r.down(at);
            r.frame(other_finger(TouchPhase::Start, other));
            for step in 1..5 {
                let by = Vec2::new(0.0, step as f32 * 20.0);
                let mut events = finger_move(at + by);
                events.extend(other_finger(TouchPhase::Move, other + by));
                r.frame(events);
            }
            r.idle(2);
            assert_eq!(node_pos(&r), before);
            assert!(r.pin(AnyPin::In(r.c)).y > pin.y + 20.0, "{pin:?} -> {:?}", r.pin(AnyPin::In(r.c)));
        }

        /// Lifting one finger of a pinch doesn't turn the other into a node drag.
        #[test]
        fn the_finger_left_after_a_pinch_does_not_drag_the_node() {
            let mut r = Rig::new();
            r.touch_mode();
            let (at, before) = (on_node(&r), node_pos(&r));
            let other = at + Vec2::new(300.0, 100.0);
            r.down(at);
            r.frame(other_finger(TouchPhase::Start, other));
            spread(&mut r, at, other);
            r.frame(other_finger(TouchPhase::End, other));
            r.slide(at + Vec2::new(60.0, 0.0));
            r.slide(at + Vec2::new(120.0, 0.0));
            assert_eq!(node_pos(&r), before);

            // And the next touch is an ordinary one again.
            let (end, spot) = (at + Vec2::new(120.0, 0.0), on_node(&r));
            r.up(end);
            r.down(spot);
            r.slide(spot + Vec2::new(40.0, 0.0));
            r.slide(spot + Vec2::new(80.0, 0.0));
            assert!(node_pos(&r).x > before.x + 10.0);
        }

        /// A finger on a pin isn't drawing a wire once it's part of a pinch, and the wide
        /// hit area makes landing on one easy.
        #[test]
        fn a_pinch_over_a_pin_zooms_and_draws_nothing() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b) = (r.a, r.b);
            let (at, zoom) = (r.pin(AnyPin::In(r.c)), span(&r));
            let other = at + Vec2::new(300.0, 100.0);
            r.down(at);
            r.frame(other_finger(TouchPhase::Start, other));
            spread(&mut r, at, other);
            assert!(span(&r) > zoom + 1.0);
            r.frame(other_finger(TouchPhase::End, other));
            r.up(at - Vec2::new(0.0, 100.0));
            assert_eq!(r.wires(), [(a, b)]);
            assert!(r.viewer.drop_menu.is_empty());
        }

        /// A wired pin's hold gives way to a pinch too.
        #[test]
        fn a_pinch_cancels_a_hold_on_a_pin() {
            let mut r = Rig::new();
            r.touch_mode();
            let (a, b) = (r.a, r.b);
            let at = r.pin(AnyPin::In(b));
            let other = at + Vec2::new(300.0, 100.0);
            r.down(at);
            r.wait(0.3);
            r.frame(other_finger(TouchPhase::Start, other));
            r.wait(1.0);
            r.frame(other_finger(TouchPhase::End, other));
            r.up(at);
            assert_eq!(r.wires(), [(a, b)]);
            assert!(r.viewer.drop_menu.is_empty(), "nothing got picked up");
        }

        /// A wire being dragged keeps its finger: the view zooms around it and doesn't pan.
        #[test]
        fn a_second_finger_zooms_while_a_wire_is_dragged() {
            let mut r = Rig::new();
            r.touch_mode();
            let (from, zoom) = (r.pin(AnyPin::In(r.c)), span(&r));
            let at = from + Vec2::new(-100.0, 0.0);
            r.down(from);
            r.slide(from + Vec2::new(-50.0, 0.0));
            r.slide(at);
            let other = at + Vec2::new(-200.0, -200.0);
            r.frame(other_finger(TouchPhase::Start, other));
            spread(&mut r, at, other);
            assert!(span(&r) != zoom);
        }

        /// A finger on a widget in a node's body is working it. The canvas doesn't pinch.
        #[test]
        fn a_finger_on_a_widget_stops_the_pinch() {
            let mut r = Rig::with_viewer(Viewer { sliders: true, ..Default::default() });
            r.touch_mode();
            r.idle(3);
            let (at, zoom) = (r.viewer.slider_at.expect("drawn"), span(&r));
            let other = pos2(1100.0, 100.0);
            r.down(at);
            r.slide(at + Vec2::new(0.0, 10.0));
            r.slide(at + Vec2::new(0.0, 20.0));
            r.frame(other_finger(TouchPhase::Start, other));
            spread(&mut r, at + Vec2::new(0.0, 20.0), other);
            assert_eq!(span(&r), zoom);
        }

        /// A finger resting while another moves anchors the pinch, and no menu comes of it.
        #[test]
        fn no_menus_come_of_a_resting_finger_in_a_pinch() {
            for place in 0..3 {
                let mut r = Rig::new();
                r.touch_mode();
                let at = match place {
                    0 => pos2(1100.0, 30.0),
                    1 => on_node(&r),
                    _ => wire_middle(&r),
                };
                let other = pos2(1100.0, 500.0);
                r.down(at);
                r.frame(other_finger(TouchPhase::Start, other));
                r.wait(1.2);
                assert_eq!(r.viewer.graph_menu_frames, 0, "place {place}");
                assert_eq!(r.viewer.node_menu_frames, 0, "place {place}");
                assert!(r.viewer.wire_menus.is_empty(), "place {place}");
            }
        }

        /// A block stamped in another window doesn't reach this one. The pass counts are per
        /// window, so the main window's stamp used to freeze a sub-patch editor's view.
        #[test]
        fn a_pinch_block_in_another_window_does_not_freeze_this_one() {
            let mut r = Rig::new();
            r.touch_mode();
            let other_window = egui::ViewportId::from_hash_of("main window");
            for n in 0..200 {
                let mut raw = input(r.time + n as f64 * 0.001, vec![]);
                raw.viewport_id = other_window;
                raw.viewports.insert(other_window, Default::default());
                let _ = r.ctx.run(raw, |ctx| egui_snarl::ui::touch::block_pinch(ctx));
            }
            let (at, pin) = (pos2(1100.0, 30.0), r.pin(AnyPin::In(r.b)));
            r.down(at);
            r.slide(at + Vec2::new(-40.0, 0.0));
            r.slide(at + Vec2::new(-80.0, 0.0));
            r.idle(2);
            assert!(r.pin(AnyPin::In(r.b)).x < pin.x - 20.0, "the view is frozen");
        }

        /// Two fingers on the canvas pinch it, unless a widget is using them.
        #[test]
        fn a_pinch_zooms_only_when_no_widget_blocks_it() {
            for blocked in [false, true] {
                let mut r = Rig::new();
                r.touch_mode();
                let pin = AnyPin::In(r.b);
                let start = r.pin(pin);
                let (p1, p2) = (pos2(1050.0, 100.0), pos2(1050.0, 300.0));
                r.down(p1);
                r.frame(other_finger(TouchPhase::Start, p2));
                for step in 1..6 {
                    if blocked {
                        egui_snarl::ui::touch::block_pinch(&r.ctx);
                    }
                    let spread = step as f32 * 20.0;
                    let mut events = finger_move(p1 - Vec2::new(0.0, spread));
                    events.extend(other_finger(TouchPhase::Move, p2 + Vec2::new(0.0, spread)));
                    r.frame(events);
                }
                assert_eq!(r.pin(pin) == start, blocked, "blocked: {blocked}");
            }
        }

        /// The mouse behaves as before: a new wire dropped on empty space opens the menu.
        #[test]
        fn a_mouse_still_drops_wires_on_empty_space() {
            let mut r = Rig::new();
            let from = r.pin(AnyPin::In(r.c));
            r.mouse_to(from);
            r.mouse_to(from);
            r.mouse_button(from, true);
            r.mouse_to(from + (EMPTY - from) * 0.5);
            r.mouse_to(EMPTY);
            r.mouse_button(EMPTY, false);
            r.idle(2);
            assert_eq!(*r.viewer.drop_menu.last().expect("menu opened"), (1, None));
        }

        /// Ctrl+drag moves wires by the same path: given up on, they stay; landed, they move.
        #[test]
        fn ctrl_dragging_moves_wires_only_when_they_land() {
            let mut r = Rig::new();
            let (a, b) = (r.a, r.b);
            r.modifiers = egui::Modifiers::COMMAND;
            let from = r.pin(AnyPin::In(b));
            r.mouse_to(from);
            r.mouse_to(from);
            r.mouse_button(from, true);
            r.mouse_to(from + (EMPTY - from) * 0.5);
            r.mouse_to(EMPTY);
            assert_eq!(r.wires(), [(a, b)], "still connected in the air");
            r.mouse_button(EMPTY, false);
            r.idle(2);
            assert_eq!(*r.viewer.drop_menu.last().expect("menu opened"), (1, Some(AnyPin::In(b))));
            assert_eq!(r.wires(), [(a, b)]);

            let mut r = Rig::new();
            r.modifiers = egui::Modifiers::COMMAND;
            let (a, b, c) = (r.a, r.b, r.c);
            let from = r.pin(AnyPin::In(b));
            let to = r.pin(AnyPin::In(c));
            r.mouse_to(from);
            r.mouse_to(from);
            r.mouse_button(from, true);
            r.mouse_to(from + (to - from) * 0.5);
            r.mouse_to(to);
            r.mouse_to(to);
            r.mouse_button(to, false);
            r.idle(2);
            assert_eq!(r.wires(), [(a, c)]);
        }
    }

    /// A context menu on a short screen, opened by a finger that's then lifted.
    struct MenuRig {
        ctx: egui::Context,
        rows: usize,
        /// Put the rows in a list that scrolls for a mouse as well, as Add module's are.
        list: bool,
        /// What the rows say, before their number.
        label: &'static str,
        time: f64,
        /// Where the first row is on screen, while the menu is open.
        first_row: Option<egui::Rect>,
        popup: Option<egui::Rect>,
        scroll_bar: bool,
    }

    impl MenuRig {
        const SCREEN: egui::Vec2 = egui::vec2(800.0, 400.0);
        const ANCHOR: Pos2 = pos2(100.0, 100.0);

        fn new(rows: usize, list: bool) -> Self {
            let mut r = Self {
                ctx: egui::Context::default(),
                rows,
                list,
                label: "Row",
                time: 0.0,
                first_row: None,
                popup: None,
                scroll_bar: false,
            };
            r.frame(vec![]);
            r
        }

        fn opened(rows: usize) -> Self {
            Self::opened_at(rows, Self::ANCHOR)
        }

        fn opened_at(rows: usize, at: Pos2) -> Self {
            let mut r = Self::new(rows, false);
            r.open_by_finger(at);
            r
        }

        fn open_by_finger(&mut self, at: Pos2) {
            self.frame(finger_down(at));
            self.frame(vec![]);
            self.time += 1.0;
            self.frame(vec![]);
            self.frame(finger_up(at));
            self.frame(vec![]);
            self.frame(vec![]);
            assert!(self.first_row.is_some(), "the menu is open");
        }

        /// By right click, with the rows in a list that scrolls for a mouse too.
        fn opened_by_mouse(rows: usize) -> Self {
            let mut r = Self::new(rows, true);
            r.open_by_mouse();
            r
        }

        fn open_by_mouse(&mut self) {
            let at = Self::ANCHOR;
            self.frame(vec![Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.frame(vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Secondary,
                    pressed,
                    modifiers: Default::default(),
                }]);
            }
            self.frame(vec![]);
            self.frame(vec![]);
            assert!(self.first_row.is_some(), "the menu is open");
        }

        fn close(&mut self) {
            self.frame(vec![Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }]);
            self.frame(vec![]);
            self.frame(vec![]);
            assert!(self.first_row.is_none(), "the menu is closed");
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.time += 1.0 / 30.0;
            let mut raw = input(self.time, events);
            raw.screen_rect = Some(egui::Rect::from_min_size(Pos2::ZERO, Self::SCREEN));
            let (rows, list, label) = (self.rows, self.list, self.label);
            let (mut first, mut popup) = (None, None);
            let out = self.ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let bg = ui.interact(
                        egui::Rect::from_min_size(Pos2::ZERO, Self::SCREEN),
                        egui::Id::new("bg"),
                        egui::Sense::click(),
                    );
                    let mut add_rows = |ui: &mut egui::Ui| {
                        for n in 0..rows {
                            let row = ui.button(format!("{label} {n}")).rect;
                            if n == 0 {
                                first = Some(row);
                            }
                        }
                    };
                    context_menu(&bg, |ui| {
                        if list {
                            menu_list_scroll(ui, |ui| add_rows(ui));
                        } else {
                            add_rows(ui);
                        }
                    });
                    popup = ctx.read_response(egui_snarl::ui::touch::context_menu_id(&bg)).map(|r| r.rect);
                });
            });
            self.first_row = first;
            self.popup = popup;
            self.scroll_bar = out.shapes.iter().any(|clipped| is_scroll_bar(&clipped.shape));
        }
    }

    /// A strip you can see, tall and narrow, as nothing else in these menus is.
    fn is_scroll_bar(shape: &egui::Shape) -> bool {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().any(is_scroll_bar),
            egui::Shape::Rect(r) => {
                let size = r.rect.size();
                size.x <= 12.0 && size.y >= 20.0 && r.fill.a() > 0
            }
            _ => false,
        }
    }

    /// More rows than the screen has room for: the menu fits on it, and with no pointer
    /// anywhere near, shows that there's more to scroll to.
    #[test]
    fn a_touch_menu_too_long_for_the_screen_scrolls_and_says_so() {
        let r = MenuRig::opened(30);
        let popup = r.popup.expect("drawn");
        assert!(popup.height() <= MenuRig::SCREEN.y, "{popup:?}");
        assert!(popup.top() >= 0.0 && popup.bottom() <= MenuRig::SCREEN.y, "{popup:?}");
        assert!(r.scroll_bar, "the strip shows with the finger gone");
    }

    /// The bar mustn't take its room out of the rows: the menu sizes itself to them, and
    /// that squeezed "Reset" onto two lines.
    #[test]
    fn a_scrolling_touch_menu_keeps_its_rows_on_one_line() {
        let single = MenuRig::opened(3).first_row.unwrap();
        let scrolling = MenuRig::opened(30);
        assert!(scrolling.scroll_bar);
        let row = scrolling.first_row.unwrap();
        assert_eq!(row.height(), single.height(), "the label wrapped");
        assert!(row.width() >= single.width(), "{} < {}", row.width(), single.width());
    }

    /// A menu that fits on the screen shows it all, however little room is below where it
    /// opened: it moves up rather than scrolling.
    #[test]
    fn a_touch_menu_that_fits_on_screen_shows_every_row() {
        for at in [MenuRig::ANCHOR, pos2(100.0, 350.0)] {
            let r = MenuRig::opened_at(6, at);
            let popup = r.popup.expect("drawn");
            assert!(!r.scroll_bar, "at {at:?}: scrolling with room to spare, {popup:?}");
            let rows = 6.0 * r.first_row.unwrap().height();
            assert!(popup.height() >= rows, "at {at:?}: {} < {rows}", popup.height());
            assert!(popup.top() >= 0.0 && popup.bottom() <= MenuRig::SCREEN.y, "at {at:?}: {popup:?}");
        }
    }

    /// A menu too long for the screen uses the screen, rather than a little of it.
    #[test]
    fn a_touch_menu_too_long_for_the_screen_fills_it() {
        let popup = MenuRig::opened(30).popup.expect("drawn");
        assert!(popup.height() >= MenuRig::SCREEN.y - 60.0, "{popup:?}");
    }

    /// Add module's lists scroll for a mouse too, and with room they show every row.
    #[test]
    fn a_mouse_menu_list_with_room_shows_every_row() {
        let r = MenuRig::opened_by_mouse(8);
        let popup = r.popup.expect("drawn");
        let rows = 8.0 * r.first_row.unwrap().height();
        assert!(popup.height() >= rows, "{} < {rows}", popup.height());
    }

    /// A menu last opened by mouse used to come back at the mouse's size, which a scroll
    /// area can't grow out of: a small box to scroll around in, its labels squeezed.
    #[test]
    fn a_touch_menu_is_not_stuck_at_the_size_a_mouse_left_it() {
        let mut r = MenuRig::new(6, false);
        r.open_by_mouse();
        r.close();
        r.open_by_finger(MenuRig::ANCHOR);
        let (row, popup) = (r.first_row.unwrap(), r.popup.expect("drawn"));
        assert_eq!(row.height(), MenuRig::opened(3).first_row.unwrap().height(), "the label wrapped");
        assert!(!r.scroll_bar, "scrolling with room to spare, {popup:?}");
        assert!(popup.height() >= 6.0 * row.height(), "{popup:?}");
    }

    /// The same menu can come back with more in it, as a curve's does for another lane.
    /// A longer list shows whole, though the menu remembers being shorter.
    #[test]
    fn a_menu_that_comes_back_longer_shows_every_row() {
        let mut r = MenuRig::new(2, false);
        r.open_by_finger(MenuRig::ANCHOR);
        r.close();
        r.rows = 6;
        r.open_by_finger(MenuRig::ANCHOR);
        let (row, popup) = (r.first_row.unwrap(), r.popup.expect("drawn"));
        assert!(!r.scroll_bar, "{popup:?}");
        assert!(popup.height() >= 6.0 * row.height(), "{popup:?}");
    }

    /// And longer labels stay on their line, though it remembers being narrower.
    #[test]
    fn a_menu_that_comes_back_wider_keeps_its_labels_on_one_line() {
        let mut r = MenuRig::new(3, false);
        r.open_by_finger(MenuRig::ANCHOR);
        let height = r.first_row.unwrap().height();
        r.close();
        r.label = "A label a good deal longer than before";
        r.open_by_finger(MenuRig::ANCHOR);
        assert_eq!(r.first_row.unwrap().height(), height);
    }

    /// The bar lies over the rows' padding: they run as far across as with no bar at all.
    #[test]
    fn the_scroll_bar_takes_no_room_from_the_rows() {
        let gap = |rows: usize| {
            let r = MenuRig::opened(rows);
            r.popup.expect("drawn").right() - r.first_row.unwrap().right()
        };
        let (scrolling, fitting) = (gap(30), gap(3));
        assert!((scrolling - fitting).abs() < 1.0, "{scrolling} vs {fitting}");
    }

    /// A menu keeps the size it opened at, and stays open, when the other input reaches it:
    /// a finger tapping a menu a mouse opened mustn't lose it on the way.
    #[test]
    fn a_menu_does_not_change_when_the_other_input_reaches_it() {
        let mut r = MenuRig::new(6, false);
        r.open_by_mouse();
        let size = r.popup.expect("drawn").size();
        // A second finger's Touch event marks touch as in use, without clicking anything.
        r.frame(vec![Event::Touch {
            device_id: TouchDeviceId(1),
            id: TouchId(9),
            phase: TouchPhase::Start,
            pos: pos2(700.0, 380.0),
            force: None,
        }]);
        r.frame(vec![]);
        assert!(r.first_row.is_some(), "still open");
        assert_eq!(r.popup.expect("drawn").size(), size);
    }

    /// And the other way, a mouse menu doesn't stay the size of the finger's.
    #[test]
    fn a_mouse_menu_is_not_stuck_at_the_size_a_finger_left_it() {
        let mut r = MenuRig::new(6, false);
        r.open_by_mouse();
        let mouse_size = r.popup.expect("drawn").size();
        r.close();
        r.open_by_finger(MenuRig::ANCHOR);
        r.close();
        r.open_by_mouse();
        assert_eq!(r.popup.expect("drawn").size(), mouse_size);
    }

    /// With everything in view there's nothing to say.
    #[test]
    fn a_touch_menu_that_fits_shows_no_scroll_bar() {
        let r = MenuRig::opened(3);
        assert!(!r.scroll_bar);
    }

    /// A finger drags the rows along, and doesn't close the menu doing it.
    #[test]
    fn a_finger_scrolls_a_long_touch_menu() {
        let mut r = MenuRig::opened(30);
        let (top, popup) = (r.first_row.unwrap().top(), r.popup.unwrap());
        let at = popup.center();
        r.frame(finger_down(at));
        r.frame(finger_move(at - egui::vec2(0.0, 40.0)));
        r.frame(finger_move(at - egui::vec2(0.0, 90.0)));
        r.frame(finger_up(at - egui::vec2(0.0, 90.0)));
        r.frame(vec![]);
        let row = r.first_row.expect("still open");
        assert!(row.top() < top - 20.0, "{top} -> {}", row.top());
    }

    /// A menu row under a finger is about 10 mm tall, under a mouse what it always was.
    #[test]
    fn touch_menus_have_finger_sized_rows() {
        let ctx = egui::Context::default();
        let row = |style: fn(&mut egui::Style)| {
            let height = std::cell::Cell::new(0.0);
            run(&ctx, input(0.0, vec![]), |ctx| {
                egui::Area::new(egui::Id::new("row")).show(ctx, |ui| {
                    style(ui.style_mut());
                    height.set(ui.button("Add module").rect.height());
                });
            });
            height.get()
        };
        let finger = row(egui_snarl::ui::touch::menu_style);
        let mouse = row(egui::containers::menu::menu_style);
        assert!(finger >= 36.0, "{finger}");
        assert!(mouse < 24.0, "{mouse}");
    }
}
