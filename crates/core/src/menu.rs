//! Virtual Menu pin namespace — shared between the engine evaluator and the
//! UI body so dynamic port ids stay in lockstep (mirrors [`crate::touchzones`]).
//!
//! The Virtual Menu divides a screen rectangle into zones (same BSP
//! [`crate::touchzones::ZoneNode`] tree as Touch Zones, single field, stable
//! leaf ids) and, while summoned by an activation input, highlights the zone
//! the pointer source points at; deactivation (or an explicit confirm) selects
//! it. Ports mode exposes per-zone pins; mapping mode routes zones through
//! internal mapping cards instead.
//!
//! Port id schema (`output_pin_ids`):
//!   slot 0: `"automap_pass"` — AutoMap passthrough sentinel (carries no Signal)
//!   slot 1: `"menu_open"`    — Bool, true while the menu overlay is up
//!   slot 2: `"menu_hover"`   — Float, hovered zone id (-1 when none/closed)
//!   then per zone (ports mode): `"m{id}_act"` (Bool, zone highlighted) and
//!   `"m{id}_sel"` (Bool, selection pulse).
//!
//! Ids are save-format — never rename.

/// AutoMap passthrough sentinel (same convention as Touch Zones).
pub const PASS_PIN: &str = "automap_pass";
/// Fixed Bool output: menu currently open.
pub const OPEN_PIN: &str = "menu_open";
/// Fixed Float output: hovered zone id, -1.0 when none (or menu closed).
pub const HOVER_PIN: &str = "menu_hover";

/// Per-zone output component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuComp {
    /// Bool — the zone is highlighted while the menu is open.
    Active,
    /// Bool — one-shot pulse when the zone is selected.
    Selected,
}

/// A parsed dynamic port id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    Open,
    Hover,
    Zone { id: u32, comp: MenuComp },
}

/// Port id for a zone component, e.g. `zone_pin_id(3, Selected) == "m3_sel"`.
pub fn zone_pin_id(id: u32, comp: MenuComp) -> String {
    match comp {
        MenuComp::Active => format!("m{id}_act"),
        MenuComp::Selected => format!("m{id}_sel"),
    }
}

/// Human-readable zone pin label, e.g. `"Z3 •"` / `"Z3 ✓"`.
pub fn zone_pin_label(id: u32, comp: MenuComp) -> String {
    match comp {
        MenuComp::Active => format!("Z{id} •"),
        MenuComp::Selected => format!("Z{id} ✓"),
    }
}

// ── Macro-style target pins ──────────────────────────────────────────────────
//
// A menu is CONTROLLED from other mapping modules (Remapper / Touch Zones /
// Lean) the way Macro ports are targeted: its identity (menu_id param) backs
// stable pin ids that appear in mapping `out` arrays and route into the shared
// macro collector namespaces instead of the AutoMap bus. The menu evaluator
// reads them back by its own id. Ids are save-format — never rename.

/// Which aspect of the menu a target pin drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPin {
    /// Bool — the menu's Show control (open per `activation_mode`).
    Show,
    /// Bool — the menu's Select control (commit per `select_on`).
    Select,
    /// Bool — each press moves to the next cell in [`cycle_order`]: the
    /// highlight while the menu is open, else straight to a selection.
    Next,
    /// Bool — [`TargetPin::Next`] the other way round.
    Prev,
}

impl TargetPin {
    /// Every target, in the order the pickers list them.
    pub const ALL: [TargetPin; 4] = [TargetPin::Show, TargetPin::Select, TargetPin::Next, TargetPin::Prev];

    /// The verb the pickers show after the menu's name ("Menu — Next").
    pub fn verb(self) -> &'static str {
        match self {
            TargetPin::Show => "Show",
            TargetPin::Select => "Select",
            TargetPin::Next => "Next",
            TargetPin::Prev => "Previous",
        }
    }
}

/// Target pin id, e.g. `target_pin("1f3a9c2b", TargetPin::Show) ==
/// "menu:1f3a9c2b_show"`.
pub fn target_pin(menu_id: &str, t: TargetPin) -> String {
    match t {
        TargetPin::Show => format!("menu:{menu_id}_show"),
        TargetPin::Select => format!("menu:{menu_id}_sel"),
        TargetPin::Next => format!("menu:{menu_id}_next"),
        TargetPin::Prev => format!("menu:{menu_id}_prev"),
    }
}

/// Parse a macro-style menu target pin into `(menu_id, aspect)`.
pub fn parse_target_pin(pin: &str) -> Option<(&str, TargetPin)> {
    let rest = pin.strip_prefix("menu:")?;
    let (id, comp) = rest.rsplit_once('_')?;
    if id.is_empty() {
        return None;
    }
    match comp {
        "show" => Some((id, TargetPin::Show)),
        "sel" => Some((id, TargetPin::Select)),
        "next" => Some((id, TargetPin::Next)),
        "prev" => Some((id, TargetPin::Prev)),
        _ => None,
    }
}

// ── Cycling ──────────────────────────────────────────────────────────────────
//
// Next / Previous step through the cells in READING order of the zone tree's
// unit space: by each cell's top edge, then its left edge. A grid reads row by
// row, left to right; radial mode (x = angle, y = radius) reads ring by ring,
// clockwise from the origin — so the order follows what's on screen, not the
// tree's traversal (a column-first tree would otherwise run down columns).

/// Zone ids in cycling order.
pub fn cycle_order(tree: &crate::touchzones::ZoneNode) -> Vec<u32> {
    let mut zones = tree.zones();
    // Quantized so cells on one row compare by x even when their dividers
    // sit a hair apart.
    let q = |f: f32| (f * 1000.0).round() as i32;
    zones.sort_by_key(|&(id, [x0, y0, _, _])| (q(y0), q(x0), id));
    zones.into_iter().map(|(id, _)| id).collect()
}

/// One step through `order` from `from` (`dir` > 0 forward, < 0 back),
/// wrapping at both ends. From nowhere (or a zone no longer in the tree),
/// forward starts at the first cell and back at the last. `None` only for an
/// empty order or `dir == 0`.
pub fn cycle_step(order: &[u32], from: Option<u32>, dir: i32) -> Option<u32> {
    if order.is_empty() || dir == 0 {
        return None;
    }
    let n = order.len();
    let at = from.and_then(|z| order.iter().position(|&o| o == z));
    let i = match at {
        Some(i) if dir > 0 => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None if dir > 0 => 0,
        None => n - 1,
    };
    Some(order[i])
}

// ── Radial geometry ──────────────────────────────────────────────────────────
//
// Radial mode is a POLAR PROJECTION of the zone tree: the tree's unit space
// maps x → angle (0 at 12 o'clock, increasing CLOCKWISE, seam fixed at the
// top) and y → radius (0 = dead-center hub edge, 1 = outer rim). Columns are
// sectors, rows are concentric rings, and every tree operation (dividers,
// partial splits, merges) works unchanged — grid and radial share the same
// zone data and ids; only locate and painting go through the mapping.

/// Polar coordinates of a pointer given as a CENTERED vector in screen coords
/// (+y down), any magnitude. Returns `(angle fraction 0..1 clockwise from 12
/// o'clock, magnitude)`. The caller gates the magnitude (dead center) and
/// rescales it into the ring's radius fraction itself.
pub fn radial_unit(x: f32, y: f32) -> (f32, f32) {
    let tau = std::f32::consts::TAU;
    let ang = x.atan2(-y).rem_euclid(tau) / tau;
    (ang, (x * x + y * y).sqrt())
}

/// Parse a dynamic port id into a [`Pin`]. Returns `None` for the passthrough
/// sentinel or any unrecognized id.
pub fn parse_pin(id: &str) -> Option<Pin> {
    match id {
        OPEN_PIN => return Some(Pin::Open),
        HOVER_PIN => return Some(Pin::Hover),
        _ => {}
    }
    let rest = id.strip_prefix('m')?;
    let (num, comp) = rest.split_once('_')?;
    let id: u32 = num.parse().ok()?;
    let comp = match comp {
        "act" => MenuComp::Active,
        "sel" => MenuComp::Selected,
        _ => return None,
    };
    Some(Pin::Zone { id, comp })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_ids_roundtrip() {
        assert_eq!(parse_pin(PASS_PIN), None);
        assert_eq!(parse_pin(OPEN_PIN), Some(Pin::Open));
        assert_eq!(parse_pin(HOVER_PIN), Some(Pin::Hover));
        assert_eq!(
            parse_pin(&zone_pin_id(7, MenuComp::Active)),
            Some(Pin::Zone { id: 7, comp: MenuComp::Active })
        );
        assert_eq!(
            parse_pin(&zone_pin_id(0, MenuComp::Selected)),
            Some(Pin::Zone { id: 0, comp: MenuComp::Selected })
        );
        assert_eq!(parse_pin("m3_bogus"), None);
        assert_eq!(parse_pin("z0_x"), None);
    }

    #[test]
    fn radial_unit_angles() {
        // Clockwise from 12 o'clock: up = 0, right = 0.25, down = 0.5, left = 0.75.
        assert!((radial_unit(0.0, -1.0).0 - 0.0).abs() < 1e-6);
        assert!((radial_unit(1.0, 0.0).0 - 0.25).abs() < 1e-6);
        assert!((radial_unit(0.0, 1.0).0 - 0.5).abs() < 1e-6);
        assert!((radial_unit(-1.0, 0.0).0 - 0.75).abs() < 1e-6);
        // Just left of straight up wraps to just under 1.0 (the seam is fixed
        // at the top — the tree's x-space has no wraparound).
        assert!(radial_unit(-0.01, -1.0).0 > 0.99);
        // Magnitude passes through untouched.
        let (_, mag) = radial_unit(0.0, -0.5);
        assert!((mag - 0.5).abs() < 1e-6);
    }

    #[test]
    fn target_pins_roundtrip() {
        assert_eq!(
            parse_target_pin(&target_pin("1f3a9c2b", TargetPin::Show)),
            Some(("1f3a9c2b", TargetPin::Show))
        );
        assert_eq!(
            parse_target_pin(&target_pin("1f3a9c2b", TargetPin::Select)),
            Some(("1f3a9c2b", TargetPin::Select))
        );
        for t in TargetPin::ALL {
            assert_eq!(parse_target_pin(&target_pin("1f3a9c2b", t)), Some(("1f3a9c2b", t)));
        }
        assert_eq!(parse_target_pin("menu:_show"), None);
        assert_eq!(parse_target_pin("menu:abc_bogus"), None);
        assert_eq!(parse_target_pin("macro:abc"), None);
        assert_eq!(parse_target_pin("menu_open"), None);
    }

    #[test]
    fn cycle_order_reads_rows_then_columns() {
        use crate::touchzones::{Axis, ZoneNode};
        // 2×2 grid: ids are row-major already.
        assert_eq!(cycle_order(&ZoneNode::from_grid(&[0.5], &[0.5])), vec![0, 1, 2, 3]);
        // Column-first tree: left column split top (5) / bottom (7), right
        // column one tall cell (9). Traversal is 5, 7, 9; reading order puts
        // the tall right cell on the top row, beside 5.
        let tree = ZoneNode::Split {
            axis: Axis::V, t: 0.5,
            a: Box::new(ZoneNode::Split {
                axis: Axis::H, t: 0.5,
                a: Box::new(ZoneNode::Leaf(5)),
                b: Box::new(ZoneNode::Leaf(7)),
            }),
            b: Box::new(ZoneNode::Leaf(9)),
        };
        assert_eq!(cycle_order(&tree), vec![5, 9, 7]);
    }

    #[test]
    fn cycle_step_wraps_both_ways() {
        let order = [5, 9, 7];
        assert_eq!(cycle_step(&order, Some(5), 1), Some(9));
        assert_eq!(cycle_step(&order, Some(7), 1), Some(5), "forward wraps to the first");
        assert_eq!(cycle_step(&order, Some(5), -1), Some(7), "back wraps to the last");
        assert_eq!(cycle_step(&order, None, 1), Some(5));
        assert_eq!(cycle_step(&order, None, -1), Some(7));
        assert_eq!(cycle_step(&order, Some(42), 1), Some(5), "a vanished zone starts over");
        assert_eq!(cycle_step(&order, Some(5), 0), None);
        assert_eq!(cycle_step(&[], None, 1), None);
    }
}
