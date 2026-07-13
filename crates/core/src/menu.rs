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
}
