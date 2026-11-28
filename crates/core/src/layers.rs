//! Layers: sets of settings a patch switches on and off while it runs, as Steam
//! Input's action layers do. A layer holds only the settings it changes; while it
//! is on they replace the patch's own, and the layer turned on last wins a setting
//! two of them change.
//!
//! A mapping card turns a layer on by targeting one of its pins - the same way it
//! targets a Macro Output port, with no wire to anything:
//!
//! | pin                  | the layer is on                    |
//! |----------------------|------------------------------------|
//! | `layer:{id}`         | while the card holds it            |
//! | `layer:{id}_toggle`  | from one press to the next         |
//! | `layer:{id}_on`      | from a press, until turned off     |
//! | `layer:{id}_off`     | (a press turns it off)             |
//!
//! Cards publish them into the macro namespace (`macros::SIGS_NS`); the engine
//! reads them at the end of each tick. The id is a random stable token like a
//! macro port's, so renaming a layer never breaks a card.

/// What every layer pin starts with.
pub const PREFIX: &str = "layer:";

/// What a card's press does to a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerAction {
    /// On while the card holds it.
    Hold,
    /// Each press flips it.
    Toggle,
    /// A press turns it on.
    On,
    /// A press turns it off.
    Off,
}

impl LayerAction {
    pub const ALL: [LayerAction; 4] = [LayerAction::Hold, LayerAction::Toggle, LayerAction::On, LayerAction::Off];

    fn suffix(self) -> &'static str {
        match self {
            LayerAction::Hold => "",
            LayerAction::Toggle => "_toggle",
            LayerAction::On => "_on",
            LayerAction::Off => "_off",
        }
    }

    /// The word for it on a card and in the picker.
    pub fn verb(self) -> &'static str {
        match self {
            LayerAction::Hold => "Hold",
            LayerAction::Toggle => "Toggle",
            LayerAction::On => "On",
            LayerAction::Off => "Off",
        }
    }
}

/// The pin a card targets to do `action` to layer `id`.
pub fn layer_pin(id: &str, action: LayerAction) -> String {
    format!("{PREFIX}{id}{}", action.suffix())
}

/// The layer and the action a pin names, or `None` if it isn't a layer pin.
pub fn parse_layer_pin(pin: &str) -> Option<(&str, LayerAction)> {
    let rest = pin.strip_prefix(PREFIX)?;
    for action in [LayerAction::Toggle, LayerAction::On, LayerAction::Off] {
        if let Some(id) = rest.strip_suffix(action.suffix()) {
            return (!id.is_empty()).then_some((id, action));
        }
    }
    (!rest.is_empty()).then_some((rest, LayerAction::Hold))
}

/// The param a mapping module keeps its cards in.
pub const CARDS_KEY: &str = "mappings";

/// Every param that is a list of mapping cards: a Remapper's or Map Action's, Touch Zones',
/// and 3DOF to 2D's Lean sides. A layer changes them card by card.
pub const CARD_LISTS: [&str; 4] = [CARDS_KEY, "zone_maps", "lean_left", "lean_right"];

/// Whether param `key` is a list of mapping cards ([`CARD_LISTS`]).
pub fn is_card_list(key: &str) -> bool {
    CARD_LISTS.contains(&key)
}

/// Whether a mapping card turns a layer on or off: one of its outputs is a layer pin.
pub fn card_switches_layer(card: &serde_json::Value) -> bool {
    card.get("out")
        .and_then(|v| v.as_array())
        .is_some_and(|a| a.iter().any(|p| p.as_str().is_some_and(|p| parse_layer_pin(p).is_some())))
}

/// A card's stable id (`"id"`, 8 hex characters): what a layer names it by. Given to every
/// card that hasn't one (`give_cards_ids`).
pub const CARD_ID: &str = "id";

/// In a layer's value for a card list: the cards it changes or adds, whole...
pub const CARDS_CHANGED: &str = "$cards";
/// ...and the ids of the module's cards it takes away.
pub const CARDS_REMOVED: &str = "$removed";

/// A card's id, if it has one.
pub fn card_id(card: &serde_json::Value) -> Option<&str> {
    card.get(CARD_ID).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

/// Whether a layer's value for a card list names cards one by one ([`card_diff`]) rather
/// than holding the whole list, as layers made before card ids did.
pub fn is_card_diff(v: &serde_json::Value) -> bool {
    v.get(CARDS_CHANGED).is_some() || v.get(CARDS_REMOVED).is_some()
}

/// The same card, but for what only the UI keeps on it (`_…` fields).
fn same_card(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let strip = |v: &serde_json::Value| -> serde_json::Value {
        match v.as_object() {
            Some(o) => serde_json::Value::Object(o.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, v)| (k.clone(), v.clone())).collect()),
            None => v.clone(),
        }
    };
    strip(a) == strip(b)
}

/// What a card list `now` changes from the module's own `base`, card by card: the cards
/// changed or added, and the ids taken away - `None` when it changes nothing. The cards
/// turning layers on and off are left out: they are always the patch's. A list with a
/// card that has no id yet is kept whole.
pub fn card_diff(base: Option<&serde_json::Value>, now: &serde_json::Value) -> Option<serde_json::Value> {
    let empty = Vec::new();
    let base: &Vec<serde_json::Value> = base.and_then(|b| b.as_array()).unwrap_or(&empty);
    let now_cards = now.as_array()?;
    if now_cards.iter().chain(base).any(|c| card_id(c).is_none()) {
        return (base != now_cards).then(|| now.clone());
    }
    let changed: Vec<serde_json::Value> = now_cards.iter()
        .filter(|c| !card_switches_layer(c))
        .filter(|c| !base.iter().any(|b| card_id(b) == card_id(c) && same_card(b, c)))
        .cloned()
        .collect();
    let removed: Vec<serde_json::Value> = base.iter()
        .filter(|b| !card_switches_layer(b))
        .filter(|b| !now_cards.iter().any(|c| card_id(c) == card_id(b)))
        .filter_map(|b| card_id(b).map(|id| serde_json::Value::String(id.to_string())))
        .collect();
    (!changed.is_empty() || !removed.is_empty()).then(|| serde_json::json!({
        CARDS_CHANGED: changed,
        CARDS_REMOVED: removed,
    }))
}

/// The cards a module runs with a layer's value for its card list over its own (`base`).
/// A [`card_diff`]: the module's cards, the layer's changed ones in their place, its added
/// ones after them, its removed ones gone. A whole list (an older layer): the layer's
/// cards. Either way the cards turning layers on and off are always the patch's - the
/// card that turns a layer off must work with that layer on.
pub fn merge_cards(base: Option<&serde_json::Value>, layer: &serde_json::Value) -> serde_json::Value {
    let base_cards: Vec<serde_json::Value> = base.and_then(|b| b.as_array()).cloned().unwrap_or_default();
    if is_card_diff(layer) {
        let mut out = base_cards;
        let changed = layer.get(CARDS_CHANGED).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for card in changed.into_iter().filter(|c| !card_switches_layer(c)) {
            match out.iter_mut().find(|b| card_id(b).is_some() && card_id(b) == card_id(&card)) {
                Some(slot) if !card_switches_layer(slot) => *slot = card,
                Some(_) => {}
                None => out.push(card),
            }
        }
        let removed: Vec<&str> = layer.get(CARDS_REMOVED).and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        out.retain(|c| card_switches_layer(c) || !card_id(c).is_some_and(|id| removed.contains(&id)));
        return serde_json::Value::Array(out);
    }
    let Some(cards) = layer.as_array() else { return layer.clone() };
    let mut out: Vec<serde_json::Value> = cards.iter().filter(|c| !card_switches_layer(c)).cloned().collect();
    out.extend(base_cards.into_iter().filter(|c| card_switches_layer(c)));
    serde_json::Value::Array(out)
}

/// Give each card of a list an id of its own: one with none, or the second of two with
/// the same (a copied card). Returns whether any changed.
pub fn give_cards_ids(cards: &mut [serde_json::Value]) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut changed = false;
    for card in cards.iter_mut() {
        let Some(obj) = card.as_object_mut() else { continue };
        let id = obj.get(CARD_ID).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
        match id {
            Some(id) if !seen.contains(&id) => seen.push(id),
            _ => {
                let fresh = loop {
                    let f = crate::macros::new_port_id();
                    if !seen.contains(&f) { break f; }
                };
                obj.insert(CARD_ID.into(), serde_json::Value::String(fresh.clone()));
                seen.push(fresh);
                changed = true;
            }
        }
    }
    changed
}

/// A fresh layer id (8 hex characters, as a macro port's).
pub fn new_layer_id() -> String {
    crate::macros::new_port_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_round_trips_through_its_pin() {
        for action in LayerAction::ALL {
            let pin = layer_pin("1f3a9c2b", action);
            assert_eq!(parse_layer_pin(&pin), Some(("1f3a9c2b", action)), "{pin}");
        }
        assert_eq!(layer_pin("ab", LayerAction::Hold), "layer:ab");
        assert_eq!(parse_layer_pin("macro:ab"), None);
        assert_eq!(parse_layer_pin("layer:"), None);
        assert_eq!(parse_layer_pin("layer:_on"), None, "no id");
    }

    /// A layer names cards by id: changed in place, added after, removed gone - and the
    /// rest follow the patch.
    #[test]
    fn a_layer_changes_cards_one_by_one() {
        use serde_json::json;
        let base = json!([
            { "id": "a", "in": ["btn_south"], "out": ["btn_east"] },
            { "id": "b", "in": ["btn_north"], "out": ["btn_west"] },
            { "id": "s", "in": ["btn_guide", "btn_west"], "out": ["layer:aa_toggle"] },
        ]);
        let now = json!([
            { "id": "a", "in": ["btn_south"], "out": ["btn_north"], "_open": true },
            { "id": "s", "in": ["btn_guide", "btn_east"], "out": ["layer:aa_toggle"] },
            { "id": "c", "in": ["btn_lb"], "out": ["key_q"] },
        ]);
        let diff = card_diff(Some(&base), &now).expect("a change");
        assert_eq!(diff[CARDS_CHANGED].as_array().unwrap().len(), 2, "a changed, c added; the switch card is the patch's");
        assert_eq!(diff[CARDS_REMOVED], json!(["b"]));
        // The patch changes card b's neighbour later: the layer keeps following it.
        let mut later = base.clone();
        later.as_array_mut().unwrap().push(json!({ "id": "d", "in": ["btn_rb"], "out": ["key_e"] }));
        let merged = merge_cards(Some(&later), &diff);
        let ids: Vec<&str> = merged.as_array().unwrap().iter().filter_map(card_id).collect();
        assert_eq!(ids, ["a", "s", "d", "c"]);
        assert_eq!(merged[0]["out"], json!(["btn_north"]));
        assert_eq!(merged[1]["in"], json!(["btn_guide", "btn_west"]), "the patch's switch card");
        assert_eq!(card_diff(Some(&base), &base), None, "no change");
        // Ids for new and copied cards.
        let mut cards = vec![json!({ "in": [] }), json!({ "id": "a" }), json!({ "id": "a" })];
        assert!(give_cards_ids(&mut cards));
        let ids: Vec<&str> = cards.iter().filter_map(card_id).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[1], "a");
        assert_ne!(ids[2], "a");
        assert!(!give_cards_ids(&mut cards));
    }

    #[test]
    fn the_cards_switching_layers_are_always_the_patchs() {
        use serde_json::json;
        let base = json!([
            { "in": ["btn_south"], "out": ["btn_east"] },
            { "in": ["btn_guide", "btn_west"], "out": ["layer:aa_toggle"] },
        ]);
        let layer = json!([
            { "in": ["btn_south"], "out": ["btn_north"] },
            { "in": ["btn_guide", "btn_east"], "out": ["layer:aa_toggle"] },
        ]);
        assert_eq!(merge_cards(Some(&base), &layer), json!([
            { "in": ["btn_south"], "out": ["btn_north"] },
            { "in": ["btn_guide", "btn_west"], "out": ["layer:aa_toggle"] },
        ]));
    }
}
