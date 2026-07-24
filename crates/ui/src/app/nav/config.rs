//! Gamepad navigation of the config overlay (M3.5): move focus between the
//! pinned tweak-pins. The focused pin's upstream physical device passes through
//! to the game (the overlay resolves passthrough from `gamepad_nav.config_index`),
//! so you feel and steer the parameter you're adjusting while every other input
//! stays suppressed. Value editing via the gamepad is M3.6 — for now the mouse
//! adjusts while the gamepad focuses/steers.
//!
//! Unlike the main-UI nav areas, this one relies on the config overlay's own
//! selective source-block for output suppression (NOT `ui_nav_suppress` /
//! `io_bypass`, which drop ALL output and would also kill the passthrough).

use super::*;

impl FlexInputApp {
    /// Drive config-overlay focus with the resolved nav device. Called from
    /// `run_gamepad_nav` while the config overlay is visible; owns the frame.
    pub(crate) fn nav_drive_config_overlay(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
    ) {
        use crate::gamepad_nav::{self as gn, NavDir};

        // Navigable pins published by the overlay this frame: (item_index, rect).
        let targets = crate::config_overlay::config_nav_targets(ctx);
        if targets.is_empty() {
            self.gamepad_nav.config_index = None;
            self.gamepad_nav.repeat_dir = None;
            return;
        }
        // Drop focus if the focused item is no longer a published target.
        if let Some(i) = self.gamepad_nav.config_index {
            if !targets.iter().any(|(idx, _)| *idx == i) {
                self.gamepad_nav.config_index = None;
            }
        }

        // Directional intent: d-pad edges, else a left-stick step on engage
        // (simple re-arm auto-repeat, matching the picker feel).
        let mut dir: Option<NavDir> = None;
        if nav.is_rising("dpad_up") {
            dir = Some(NavDir::Up);
        } else if nav.is_rising("dpad_down") {
            dir = Some(NavDir::Down);
        } else if nav.is_rising("dpad_left") {
            dir = Some(NavDir::Left);
        } else if nav.is_rising("dpad_right") {
            dir = Some(NavDir::Right);
        }
        if dir.is_none() {
            match gn::stick_dir(nav.lstick) {
                Some(d) => {
                    if self.gamepad_nav.repeat_dir != Some(d) {
                        self.gamepad_nav.repeat_dir = Some(d);
                        dir = Some(d);
                    }
                }
                None => self.gamepad_nav.repeat_dir = None,
            }
        }

        if let Some(dir) = dir {
            // `config_index` is an ITEM index; map to/from the target position.
            let cur_pos = self
                .gamepad_nav
                .config_index
                .and_then(|item| targets.iter().position(|(idx, _)| *idx == item));
            let rects: Vec<egui::Rect> = targets.iter().map(|(_, r)| *r).collect();
            if let Some(next_pos) = nearest_rect_in_dir(&rects, cur_pos, dir) {
                self.gamepad_nav.config_index = Some(targets[next_pos].0);
            }
            ctx.request_repaint();
        }
        // The overlay reads `config_index` to light up the focused pin and pass
        // its upstream device through; nothing else to do here (value edit = M3.6).
    }
}

/// Nearest rect in a direction from the current one (top-left-most when none).
/// A bare-rect mirror of `gamepad_nav::nearest_target_rect_in_dir`.
fn nearest_rect_in_dir(
    rects: &[egui::Rect],
    cur: Option<usize>,
    dir: crate::gamepad_nav::NavDir,
) -> Option<usize> {
    use crate::gamepad_nav::NavDir;
    if rects.is_empty() {
        return None;
    }
    let cur = match cur.filter(|&i| i < rects.len()) {
        Some(i) => i,
        None => {
            return rects
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let (ca, cb) = (a.center(), b.center());
                    (ca.y, ca.x)
                        .partial_cmp(&(cb.y, cb.x))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i);
        }
    };
    let c = rects[cur].center();
    let mut best: Option<(usize, f32)> = None;
    for (i, r) in rects.iter().enumerate() {
        if i == cur {
            continue;
        }
        let o = r.center();
        let (dx, dy) = (o.x - c.x, o.y - c.y);
        let (primary, cross) = match dir {
            NavDir::Right => (dx, dy),
            NavDir::Left => (-dx, dy),
            NavDir::Down => (dy, dx),
            NavDir::Up => (-dy, dx),
        };
        if primary <= 0.0 {
            continue;
        }
        let score = primary + 2.0 * cross.abs();
        if best.map(|(_, s)| score < s).unwrap_or(true) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}
