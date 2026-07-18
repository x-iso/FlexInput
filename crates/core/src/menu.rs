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
}

/// Target pin id, e.g. `target_pin("1f3a9c2b", TargetPin::Show) ==
/// "menu:1f3a9c2b_show"`.
pub fn target_pin(menu_id: &str, t: TargetPin) -> String {
    match t {
        TargetPin::Show => format!("menu:{menu_id}_show"),
        TargetPin::Select => format!("menu:{menu_id}_sel"),
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
        _ => None,
    }
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
        assert_eq!(parse_target_pin("menu:_show"), None);
        assert_eq!(parse_target_pin("menu:abc_bogus"), None);
        assert_eq!(parse_target_pin("macro:abc"), None);
        assert_eq!(parse_target_pin("menu_open"), None);
    }
}
