//! Static mapping-conflict detection.
//!
//! Several modules write the SAME AutoMap bus / sink pins from their mapping
//! cards (Remapper `mappings`, Touch Zones + Virtual Menu `zone_maps`, Lean
//! `lean_left`/`lean_right`). When two cards target the same output pin the
//! engine's collector merge picks ONE — the other silently does nothing (the
//! report that started this: a swipe→"2" card was overridden by a Virtual Menu
//! also bound to "2"). This scan walks the patch's nodes ONCE and reports, per
//! output pin, every card that writes it, so a card renderer can badge a
//! collision with the other owner's module name.
//!
//! Scope is a single `Snarl` (the patch the rendering node lives in) — the
//! collector merge is per processing graph, so same-patch coverage catches the
//! practical cases. Macro-port / Virtual-Menu *target* pins are excluded: those
//! route into the macro collector namespaces where multiple writers MERGE by
//! design, so shared writers there are not a conflict.

use super::*;

/// One card that writes a given output pin. `param_key` + `idx` identify the
/// card within its node's params array, so a card can recognise ITS OWN writer
/// entry and only flag the *others*.
pub(crate) struct PinWriter {
    pub(crate) node: NodeId,
    pub(crate) param_key: &'static str,
    pub(crate) idx: usize,
    /// Human module name for the badge tooltip ("Remapper", "Menu", …).
    pub(crate) module_label: &'static str,
}

/// Output pin id → every card that writes it (across the scanned patch).
pub(crate) type ConflictMap = std::collections::HashMap<String, Vec<PinWriter>>;

/// A single card's conflicts: for each of its out-pins that another card also
/// writes, the pin id and the distinct "Module" / "Module · Node" owner labels
/// of those OTHER writers (deduped, stable order).
pub(crate) struct CardConflict {
    pub(crate) pins: Vec<(String, Vec<String>)>,
}

/// Which mapping modules write bus pins, and the params arrays their cards live
/// in. `None` for every other module_id.
fn writer_sources(module_id: &str) -> Option<(&'static str, &'static [&'static str])> {
    match module_id {
        "module.remapper"    => Some(("Remapper",    &["mappings"])),
        "module.touch_zones" => Some(("Touch Zones", &["zone_maps"])),
        "module.menu"        => Some(("Menu",        &["zone_maps"])),
        "module.lean"        => Some(("Lean",        &["lean_left", "lean_right"])),
        _ => None,
    }
}

/// A pin worth flagging for collisions: a real bus/sink pin, NOT a macro-port or
/// Virtual-Menu target (those merge by design).
fn is_conflict_relevant_pin(pin: &str) -> bool {
    flexinput_core::macros::parse_macro_pin(pin).is_none()
        && flexinput_core::menu::parse_target_pin(pin).is_none()
}

/// Walk every mapping-module node in the patch and record each card that writes
/// each output pin. O(total cards); cheap enough to run once per body render.
pub(crate) fn scan_mapping_conflicts(snarl: &Snarl<NodeData>) -> ConflictMap {
    let mut map: ConflictMap = std::collections::HashMap::new();
    for (node, n) in snarl.nodes_ids_data() {
        let data = &n.value;
        let Some((label, keys)) = writer_sources(&data.module_id) else { continue };
        for key in keys {
            let Some(arr) = data.params.get(*key).and_then(|v| v.as_array()) else { continue };
            for (idx, card) in arr.iter().enumerate() {
                let Some(outs) = card.get("out").and_then(|v| v.as_array()) else { continue };
                for p in outs.iter().filter_map(|v| v.as_str()) {
                    if !is_conflict_relevant_pin(p) { continue; }
                    map.entry(p.to_string()).or_default().push(PinWriter {
                        node,
                        param_key: key,
                        idx,
                        module_label: label,
                    });
                }
            }
        }
    }
    map
}

/// True when `pin` (a canonical button id like `"btn_guide"`) is used as a
/// mapping INPUT anywhere in `snarl` or its nested sub-patches — i.e. a Remapper
/// card reads it. Used to decide whether a single system button (Guide / Capture
/// / Mic) is free to bind as a standalone shortcut, or already spoken for. Only
/// Remapper `in` pins are checked: Touch Zones / Menu take touch/zone inputs and
/// Lean is gyro-driven, so a face/system button is never their source.
pub(crate) fn pin_used_as_mapping_input(snarl: &Snarl<NodeData>, pin: &str) -> bool {
    for (_id, n) in snarl.nodes_ids_data() {
        let data = &n.value;
        if data.module_id == "module.remapper" {
            if let Some(arr) = data.params.get("mappings").and_then(|v| v.as_array()) {
                for card in arr {
                    if card.get("in").and_then(|v| v.as_array())
                        .map(|ins| ins.iter().filter_map(|v| v.as_str()).any(|p| p == pin))
                        .unwrap_or(false)
                    {
                        return true;
                    }
                }
            }
        }
        if let Some(sp) = &data.subpatch {
            if pin_used_as_mapping_input(&sp.snarl, pin) { return true; }
        }
    }
    false
}

/// Build the per-card conflict view for the card identified by
/// (`node`, `param_key`, `idx`) with the given `out_pins`. Returns `None` when
/// no out-pin of this card is also written by another card. The owner labels
/// name only the OTHER writers (this card is filtered out), and count how many
/// distinct nodes carry each module so "Menu" becomes "Menu ×2" when two menus
/// collide, or "Menu · <node name>" isn't needed — we keep it compact.
pub(crate) fn card_conflict_for(
    map: &ConflictMap,
    node: NodeId,
    param_key: &str,
    idx: usize,
    out_pins: &[String],
) -> Option<CardConflict> {
    let mut pins: Vec<(String, Vec<String>)> = Vec::new();
    for pin in out_pins {
        let Some(writers) = map.get(pin) else { continue };
        // Distinct owner labels among writers that are NOT this exact card.
        let mut labels: Vec<String> = Vec::new();
        for w in writers {
            let is_self = w.node == node && w.param_key == param_key && w.idx == idx;
            if is_self { continue; }
            if !labels.iter().any(|l| l == w.module_label) {
                labels.push(w.module_label.to_string());
            }
        }
        if !labels.is_empty() {
            pins.push((pin.clone(), labels));
        }
    }
    if pins.is_empty() { None } else { Some(CardConflict { pins }) }
}

impl CardConflict {
    /// A one-line tooltip: "Also mapped in Menu: 2 — its output wins over this
    /// card". Kept short; lists each colliding pin with the other owners.
    pub(crate) fn tooltip(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (pin, labels) in &self.pins {
            let name = remapper_pin_display(pin);
            parts.push(format!("“{name}” also mapped in {}", labels.join(", ")));
        }
        format!(
            "⚠ Output conflict — {}. When two cards drive the same output only one \
             wins (last writer in the merge); the others do nothing.",
            parts.join("; "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(node: usize, key: &'static str, idx: usize, label: &'static str) -> PinWriter {
        PinWriter { node: NodeId(node), param_key: key, idx, module_label: label }
    }

    #[test]
    fn single_writer_is_not_a_conflict() {
        let mut map: ConflictMap = std::collections::HashMap::new();
        map.insert("key_2".into(), vec![writer(1, "zone_maps", 0, "Touch Zones")]);
        let out = vec!["key_2".to_string()];
        // The only writer IS this card → no conflict.
        assert!(card_conflict_for(&map, NodeId(1), "zone_maps", 0, &out).is_none());
    }

    #[test]
    fn other_module_writing_same_pin_flags_conflict() {
        let mut map: ConflictMap = std::collections::HashMap::new();
        map.insert("key_2".into(), vec![
            writer(1, "zone_maps", 0, "Touch Zones"), // this card
            writer(2, "zone_maps", 3, "Menu"),        // the Menu that overrides it
        ]);
        let out = vec!["key_2".to_string()];
        let cf = card_conflict_for(&map, NodeId(1), "zone_maps", 0, &out)
            .expect("collision with the Menu must be flagged");
        assert_eq!(cf.pins.len(), 1);
        assert_eq!(cf.pins[0].0, "key_2");
        assert_eq!(cf.pins[0].1, vec!["Menu".to_string()]);
    }

    #[test]
    fn duplicate_owner_labels_are_deduped() {
        let mut map: ConflictMap = std::collections::HashMap::new();
        // Two OTHER Remapper cards write the same pin → one "Remapper" label, once.
        map.insert("mouse_left".into(), vec![
            writer(1, "mappings", 0, "Remapper"), // this card
            writer(1, "mappings", 4, "Remapper"),
            writer(9, "mappings", 2, "Remapper"),
        ]);
        let out = vec!["mouse_left".to_string()];
        let cf = card_conflict_for(&map, NodeId(1), "mappings", 0, &out).unwrap();
        assert_eq!(cf.pins[0].1, vec!["Remapper".to_string()]);
    }

    #[test]
    fn macro_and_menu_target_pins_are_excluded_from_scan() {
        // Macro-port / menu-target pins merge by design and must not be scanned.
        assert!(!is_conflict_relevant_pin("macro:abc123"));
        assert!(is_conflict_relevant_pin("key_2"));
        assert!(is_conflict_relevant_pin("right_stick"));
    }
}
