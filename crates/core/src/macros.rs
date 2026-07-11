//! Shared schema + pin-id scheme for `module.macro` (Macro Output).
//!
//! A Macro node defines named, typed output ports that mappings elsewhere in
//! the patch (Remapper output chords, Touch Zones zone cards, 3DOF-Lean
//! mappings) can target by id — no wires between the mapping module and the
//! macro node. Both the engine (`eval.rs`) and the UI (`viewer.rs` body,
//! KB/M picker) depend on this module so the port schema and pin ids stay in
//! lock-step.
//!
//! Persisted on the node (`node.params`):
//!   macro_ports:    [{ "id": "1f3a9c2b", "name": "Ping", "icon": "gyro",
//!                      "type": "bool" }, …]   — the port definitions
//!   output_pin_ids: ["macro:1f3a9c2b", …]     — one per output slot, same
//!                                               order as `macro_ports`
//!
//! A mapping targets a port by putting its pin id (`macro:{id}`) into the
//! mapping's `out` array — the same place keyboard keys / bus pins go. The
//! id is a random stable token, NOT the display name, so renaming a port
//! never breaks existing bindings.
//!
//! Engine data flow (see `eval.rs`): mapping evaluators write asserted macro
//! values into `collector_sigs` under the reserved [`SIGS_NS`] /
//! [`SIGS_NS_VEC2`] namespaces; the macro node's compute reads them back and
//! coerces to each port's declared type.

use std::collections::HashMap;

use serde_json::Value;

use crate::SignalType;

/// `node.params` key holding the port-definition array.
pub const MACRO_PORTS_PARAM: &str = "macro_ports";

/// `collector_sigs` namespace key for scalar/bool macro writes (Remapper
/// digital + analog, Lean, Touch Zones button gate).
pub const SIGS_NS: &str = "macro";

/// `collector_sigs` namespace key for the Vec2 aspect of a macro write
/// (Touch Zones zone-local deflection). Kept separate from [`SIGS_NS`] so a
/// write site never needs to know the port's declared type: it publishes
/// whichever aspects it has, and the macro node picks by type on read.
pub const SIGS_NS_VEC2: &str = "macro#v2";

/// One user-defined macro output port.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroPortDef {
    /// Stable random token; the bus pin id is `macro:{id}`. Never regenerated
    /// on rename/retype so existing mappings keep resolving.
    pub id: String,
    /// User-facing display name (shown in the picker and on mapping chips).
    pub name: String,
    /// Icon key into the UI's embedded macro icon set; `""` = no icon
    /// (the picker cell falls back to the name).
    pub icon: String,
    /// Custom icon: raw SVG text loaded from a user file, embedded in the
    /// patch so it travels with it (same convention as `module.svg` /
    /// layout decorations). Non-empty wins over `icon`.
    pub icon_svg: String,
    pub signal_type: SignalType,
}

/// Bus pin id for a port id: `macro_pin_id("1f3a") == "macro:1f3a"`.
pub fn macro_pin_id(port_id: &str) -> String {
    format!("macro:{port_id}")
}

/// Parse a bus pin id back to the port id, or `None` if it isn't a macro pin.
pub fn parse_macro_pin(pin: &str) -> Option<&str> {
    pin.strip_prefix("macro:")
}

/// Fresh random port id (8 hex chars — short enough for params, unique enough
/// for a per-patch namespace).
pub fn new_port_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    id[..8].to_string()
}

/// Persisted string for a port type. Only the four user-selectable types are
/// meaningful; anything else round-trips to Bool.
pub fn type_to_str(ty: SignalType) -> &'static str {
    match ty {
        SignalType::Float => "float",
        SignalType::Vec2 => "vec2",
        SignalType::Any => "any",
        _ => "bool",
    }
}

pub fn type_from_str(s: &str) -> SignalType {
    match s {
        "float" => SignalType::Float,
        "vec2" => SignalType::Vec2,
        "any" => SignalType::Any,
        _ => SignalType::Bool,
    }
}

/// Parse the `macro_ports` param value. Entries missing an id or name are
/// skipped (malformed hand-edits) rather than defaulted, so the port list and
/// `output_pin_ids` can't silently drift apart.
pub fn ports_from_value(v: Option<&Value>) -> Vec<MacroPortDef> {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let id = p.get("id")?.as_str()?.to_string();
            let name = p.get("name")?.as_str()?.to_string();
            if id.is_empty() {
                return None;
            }
            Some(MacroPortDef {
                id,
                name,
                icon: p.get("icon").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                icon_svg: p.get("icon_svg").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                signal_type: type_from_str(p.get("type").and_then(|v| v.as_str()).unwrap_or("bool")),
            })
        })
        .collect()
}

/// Convenience: parse ports straight from a node's params map.
pub fn ports_from_params(params: &HashMap<String, Value>) -> Vec<MacroPortDef> {
    ports_from_value(params.get(MACRO_PORTS_PARAM))
}

/// Serialize ports back to the `macro_ports` param value. `icon_svg` is
/// written only when set, keeping patches lean for the common embedded-icon
/// case.
pub fn ports_to_value(ports: &[MacroPortDef]) -> Value {
    Value::Array(
        ports
            .iter()
            .map(|p| {
                let mut o = serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "icon": p.icon,
                    "type": type_to_str(p.signal_type),
                });
                if !p.icon_svg.is_empty() {
                    o["icon_svg"] = Value::String(p.icon_svg.clone());
                }
                o
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_id_roundtrip() {
        let id = new_port_id();
        assert_eq!(id.len(), 8);
        let pin = macro_pin_id(&id);
        assert_eq!(parse_macro_pin(&pin), Some(id.as_str()));
        assert_eq!(parse_macro_pin("key_a"), None);
        assert_eq!(parse_macro_pin("macros:x"), None);
    }

    #[test]
    fn ports_value_roundtrip() {
        let ports = vec![
            MacroPortDef { id: "aabbccdd".into(), name: "Ping".into(), icon: "gyro".into(), icon_svg: String::new(), signal_type: SignalType::Bool },
            MacroPortDef { id: "11223344".into(), name: "Aim".into(), icon: "".into(), icon_svg: "<svg>custom</svg>".into(), signal_type: SignalType::Vec2 },
        ];
        let v = ports_to_value(&ports);
        assert_eq!(ports_from_value(Some(&v)), ports);
        // Lean encoding: icon_svg key absent when empty.
        assert!(v[0].get("icon_svg").is_none());
        assert!(v[1].get("icon_svg").is_some());
    }

    #[test]
    fn malformed_entries_skipped() {
        let v: Value = serde_json::json!([
            { "name": "no id" },
            { "id": "", "name": "empty id" },
            { "id": "ok111111", "name": "Good", "type": "float" },
        ]);
        let ports = ports_from_value(Some(&v));
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].signal_type, SignalType::Float);
    }

    #[test]
    fn unknown_type_defaults_to_bool() {
        assert_eq!(type_from_str("weird"), SignalType::Bool);
        // AutoMap/Int are not user-selectable port types — they persist as bool.
        assert_eq!(type_from_str(type_to_str(SignalType::AutoMap)), SignalType::Bool);
        assert_eq!(type_from_str(type_to_str(SignalType::Int)), SignalType::Bool);
    }
}
