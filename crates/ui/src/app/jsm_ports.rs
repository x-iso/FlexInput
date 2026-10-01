//! Macro Output ports made for the `@Name`s a JSM config uses.
//!
//! A config binds `S = @Jump` and reads `@Jump,E = SPACE`; both need a Macro
//! Output port called Jump somewhere in the tab. Rather than make you go and
//! add one, the app makes it: every `@Name` a JSM node's text mentions that
//! names no port (or Virtual Menu entry) in the tab becomes an Any port on a
//! Macro Output node beside that JSM node — the first one in its (sub-)patch,
//! or a new one when it has none.
//!
//! When it does so matters more than how:
//!
//! * never while a JSM editor has the keyboard — `@J`, `@Ju`, `@Jum` would each
//!   become a port on the way to `@Jump`;
//! * only for text it hasn't seen before. A config is synced once per change, so
//!   a port you delete on purpose stays deleted until the config is edited
//!   again, and a patch loaded with `@Name`s gets its ports the moment it opens.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use super::*;

impl FlexInputApp {
    /// Make the Macro Output ports the active tab's JSM configs name but the tab
    /// hasn't got. Run once a frame, before the macro registry is published, so
    /// the configs resolve against the new ports on the very next compile.
    pub(crate) fn sync_jsm_macro_ports(&mut self, ctx: &egui::Context) {
        if flexinput_engine::eval::jsm_editor_focus() {
            return;
        }
        let seen_id = egui::Id::new("jsm_port_sync_seen");
        let mut seen: HashSet<u64> = ctx.data(|d| d.get_temp(seen_id)).unwrap_or_default();
        let mut known: Vec<String> =
            self.macro_display_entries().into_iter().map(|e| e.name).collect();
        let Some(macro_desc) = self.descriptors.iter().find(|d| d.id == "module.macro") else {
            return;
        };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        if make_missing_ports(&mut canvas.snarl, &mut known, &mut seen, macro_desc) {
            // A direct write into the tab (and maybe a sub-patch in it): bump the
            // generation so an open sub-patch editor re-pulls instead of writing
            // its stale copy back.
            canvas.mutation_gen = canvas.mutation_gen.wrapping_add(1);
            ctx.request_repaint();
        }
        // A config edited back and forth leaves a hash per version; they are
        // tiny, but not worth keeping for ever.
        if seen.len() > 4096 {
            seen.clear();
        }
        ctx.data_mut(|d| d.insert_temp(seen_id, seen));
    }
}

/// Every config text of one JSM node, all tabs.
fn jsm_texts(node: &NodeData) -> Vec<&str> {
    node.params
        .get("jsm_tabs")
        .and_then(|v| v.as_array())
        .map(|tabs| tabs.iter().filter_map(|t| t.get("text").and_then(|s| s.as_str())).collect())
        .unwrap_or_default()
}

/// Walk `snarl` and its sub-patches, making a port for each unknown `@Name` in a
/// JSM node whose text is new. `known` holds the names that already resolve,
/// and gains the ones made, so two configs naming the same port make it once.
/// Returns whether anything was made.
pub(crate) fn make_missing_ports(
    snarl: &mut Snarl<NodeData>,
    known: &mut Vec<String>,
    seen: &mut HashSet<u64>,
    macro_desc: &flexinput_core::ModuleDescriptor,
) -> bool {
    let mut made = false;
    // This patch's own JSM nodes first: what they need lands in this patch.
    let mut wanted: Vec<String> = Vec::new();
    let mut anchor: Option<egui_snarl::NodeId> = None;
    for (id, node) in snarl.node_ids() {
        if node.module_id != "module.jsm" {
            continue;
        }
        let texts = jsm_texts(node);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        texts.hash(&mut h);
        if !seen.insert(h.finish()) {
            continue;
        }
        for text in texts {
            for name in flexinput_engine::eval::jsm_at_names(text) {
                let known_already = known
                    .iter()
                    .chain(wanted.iter())
                    .any(|k| k.trim().eq_ignore_ascii_case(name.trim()));
                if !known_already {
                    wanted.push(name);
                    anchor.get_or_insert(id);
                }
            }
        }
    }
    if let Some(jsm_id) = anchor {
        add_ports(snarl, jsm_id, &wanted, macro_desc);
        known.extend(wanted);
        made = true;
    }
    // Then every sub-patch, which keeps its own Macro Output.
    let subs: Vec<egui_snarl::NodeId> = snarl
        .node_ids()
        .filter(|(_, n)| n.subpatch.is_some())
        .map(|(id, _)| id)
        .collect();
    for id in subs {
        if let Some(sp) = snarl.get_node_mut(id).and_then(|n| n.subpatch.as_mut()) {
            made |= make_missing_ports(&mut sp.snarl, known, seen, macro_desc);
        }
    }
    made
}

/// Append Any ports called `names` to the first Macro Output node in `snarl`,
/// creating one to the right of `near` when there is none. The port list, the
/// output pins and `output_pin_ids` are written together, as the Macro Output's
/// own body does, so the three can't drift.
fn add_ports(
    snarl: &mut Snarl<NodeData>,
    near: egui_snarl::NodeId,
    names: &[String],
    macro_desc: &flexinput_core::ModuleDescriptor,
) {
    use flexinput_core::macros as mac;
    let existing = snarl
        .node_ids()
        .find(|(_, n)| n.module_id == "module.macro")
        .map(|(id, _)| id);
    let macro_id = existing.unwrap_or_else(|| {
        let pos = snarl
            .get_node_info(near)
            .map(|n| n.pos + egui::vec2(320.0, 0.0))
            .unwrap_or_default();
        snarl.insert_node(pos, NodeData::from(macro_desc))
    });
    let Some(node) = snarl.get_node_mut(macro_id) else { return };
    let mut ports = mac::ports_from_params(&node.params);
    for name in names {
        ports.push(mac::MacroPortDef {
            id: mac::new_port_id(),
            name: name.clone(),
            icon: String::new(),
            icon_svg: String::new(),
            // Any, because a JSM port carries whatever drives it — a press, a
            // trigger's pull — and whatever reads it decides: a JSM button
            // thresholds the amount, a wire takes the value as it is.
            signal_type: SignalType::Any,
        });
    }
    node.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(&ports));
    node.params.insert(
        "output_pin_ids".to_string(),
        serde_json::Value::Array(
            ports.iter().map(|p| serde_json::Value::String(mac::macro_pin_id(&p.id))).collect(),
        ),
    );
    node.outputs = ports
        .iter()
        .map(|p| PinDescriptor::new(p.name.clone(), p.signal_type))
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jsm_node(text: &str) -> NodeData {
        let mut n = NodeData::from(&flexinput_core::ModuleDescriptor {
            id: "module.jsm",
            display_name: "JSM Config",
            category: "Mapping",
            inputs: vec![],
            outputs: vec![],
        });
        n.params.insert(
            "jsm_tabs".into(),
            serde_json::json!([{ "name": "main", "text": text }]),
        );
        n
    }

    fn macro_desc() -> flexinput_core::ModuleDescriptor {
        flexinput_core::ModuleDescriptor {
            id: "module.macro",
            display_name: "Macro Output",
            category: "Utility",
            inputs: vec![],
            outputs: vec![],
        }
    }

    fn port_names(snarl: &Snarl<NodeData>) -> Vec<String> {
        snarl
            .nodes()
            .filter(|n| n.module_id == "module.macro")
            .flat_map(|n| flexinput_core::macros::ports_from_params(&n.params))
            .map(|p| p.name)
            .collect()
    }

    /// The `@Name`s a config uses become ports — bound to or read, once each —
    /// on a Macro Output made beside it, with pins and ids to match.
    #[test]
    fn unknown_at_names_become_ports_on_a_new_macro_output() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        snarl.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Jump\n@Jump,E = @Dash\nN = @jump"));
        let (mut known, mut seen) = (Vec::new(), HashSet::new());
        assert!(make_missing_ports(&mut snarl, &mut known, &mut seen, &macro_desc()));
        assert_eq!(port_names(&snarl), ["Jump", "Dash"], "once each, as first written");
        let mac = snarl.nodes().find(|n| n.module_id == "module.macro").unwrap();
        assert_eq!(mac.outputs.len(), 2);
        assert!(
            flexinput_core::macros::ports_from_params(&mac.params)
                .iter()
                .all(|p| p.signal_type == SignalType::Any),
            "a JSM port carries whatever drives it"
        );
        assert_eq!(mac.params["output_pin_ids"].as_array().unwrap().len(), 2);
        // Seen once, so nothing more is made for the same text.
        assert!(!make_missing_ports(&mut snarl, &mut known, &mut seen, &macro_desc()));
    }

    /// An existing Macro Output gains the ports, and a name it already has — or
    /// a Virtual Menu entry the tab knows — is left alone.
    #[test]
    fn ports_join_the_existing_macro_output_and_known_names_are_skipped() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let mut mac = NodeData::from(&macro_desc());
        let first = flexinput_core::macros::MacroPortDef {
            id: "aabbccdd".into(),
            name: "Reload".into(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: SignalType::Bool,
        };
        mac.params.insert(
            flexinput_core::macros::MACRO_PORTS_PARAM.into(),
            flexinput_core::macros::ports_to_value(&[first]),
        );
        snarl.insert_node(egui::pos2(0.0, 0.0), mac);
        snarl.insert_node(egui::pos2(0.0, 100.0), jsm_node("S = @reload\nE = @Crouch\nW = @\"Menu — Show\""));
        let mut known = vec!["Reload".to_string(), "Menu — Show".to_string()];
        assert!(make_missing_ports(&mut snarl, &mut known, &mut HashSet::new(), &macro_desc()));
        assert_eq!(port_names(&snarl), ["Reload", "Crouch"]);
        assert_eq!(snarl.nodes().filter(|n| n.module_id == "module.macro").count(), 1);
    }

    /// A JSM node inside a sub-patch gets its ports inside that sub-patch.
    #[test]
    fn a_sub_patch_config_gets_its_ports_in_the_sub_patch() {
        let mut inner: Snarl<NodeData> = Snarl::new();
        inner.insert_node(egui::pos2(0.0, 0.0), jsm_node("S = @Fire"));
        let mut outer: Snarl<NodeData> = Snarl::new();
        let mut sp = jsm_node("");
        sp.module_id = "subpatch".into();
        sp.params.clear();
        sp.subpatch = Some(Box::new(crate::canvas::node::UiSubPatch {
            snarl: Box::new(inner),
            ..Default::default()
        }));
        let sp_id = outer.insert_node(egui::pos2(0.0, 0.0), sp);
        assert!(make_missing_ports(&mut outer, &mut Vec::new(), &mut HashSet::new(), &macro_desc()));
        assert!(port_names(&outer).is_empty(), "nothing in the outer patch");
        let inner = &outer.get_node(sp_id).unwrap().subpatch.as_ref().unwrap().snarl;
        assert_eq!(port_names(inner), ["Fire"]);
    }
}
