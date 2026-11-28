//! Layers at run time: which are on, in the order they came on, and the graph a
//! tick runs with their settings in place.
//!
//! The UI builds the patch's graph only when something in it changes (and slowly
//! while its window is frozen behind a game), so a layer can't wait for it: the
//! engine thread keeps the patch's graph as the UI sent it and, while any layer
//! is on, runs a copy with the layers' settings written over it. Node state is
//! kept by node uid, so swapping that copy in or out restarts nothing.
//!
//! Stacking is Steam Input's: the layer turned on last wins a setting two layers
//! change, and turning it off shows the one beneath again.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use flexinput_core::layers::{parse_layer_pin, LayerAction};
use serde_json::Value;

use crate::graph::{NodeSnap, ProcessingGraph};

static REQUESTS: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());
static ACTIVE: RwLock<Option<Arc<Vec<String>>>> = RwLock::new(None);

/// Turn layer `id` on or off from outside the graph: a layer chip clicked in the
/// UI. Taken by the engine's next wakeup, as a press of its On / Off pin would be.
pub fn request_layer(id: &str, on: bool) {
    if let Ok(mut q) = REQUESTS.lock() {
        q.push((id.to_string(), on));
    }
}

/// The layers on right now, the one turned on last at the end.
pub fn active_layers() -> Arc<Vec<String>> {
    ACTIVE.read().ok().and_then(|a| a.clone()).unwrap_or_default()
}

fn take_requests() -> Vec<(String, bool)> {
    REQUESTS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

fn publish_active(order: &[String]) {
    if let Ok(mut a) = ACTIVE.write() {
        *a = Some(Arc::new(order.to_vec()));
    }
}

/// Which layers are on, and in what order they came on.
#[derive(Default, Debug)]
pub(crate) struct LayerStack {
    /// On, oldest first: the last one's settings win.
    order: Vec<String>,
    /// Turned on by a press (On, Toggle, the UI) until turned off again.
    latched: HashSet<String>,
    /// The layer pins cards held last tick, for telling a press from a hold.
    prev: HashSet<String>,
}

impl LayerStack {
    pub(crate) fn order(&self) -> &[String] {
        &self.order
    }

    /// The UI's requests (a chip clicked): latch each layer on or off, as a press
    /// of its On / Off pin would. The pins cards hold are left as they are, so a
    /// held layer stays on and a held Toggle isn't pressed again. Returns whether
    /// the layers on changed.
    pub(crate) fn request(&mut self, requests: &[(String, bool)]) -> bool {
        let held: Vec<&str> = self.prev.iter()
            .filter_map(|p| parse_layer_pin(p))
            .filter(|(_, a)| *a == LayerAction::Hold)
            .map(|(id, _)| id)
            .collect();
        let mut changed = false;
        for (id, on) in requests {
            if *on {
                self.latched.insert(id.clone());
                if !self.order.contains(id) {
                    self.order.push(id.clone());
                    changed = true;
                }
            } else {
                self.latched.remove(id);
                if !held.contains(&id.as_str()) {
                    let before = self.order.len();
                    self.order.retain(|o| o != id);
                    changed |= self.order.len() != before;
                }
            }
        }
        changed
    }

    /// Take this tick's layer pins (asserted: at least half way on). Returns
    /// whether the layers on changed.
    pub(crate) fn update(&mut self, pins: &[(String, f32)]) -> bool {
        let mut now: Vec<&str> = pins.iter()
            .filter(|(_, v)| v.abs() >= 0.5)
            .map(|(p, _)| p.as_str())
            .collect();
        // A stable order, so layers pressed on in the same tick stack the same
        // way every time.
        now.sort_unstable();
        now.dedup();
        let mut held: Vec<&str> = Vec::new();
        let mut came_on: Vec<&str> = Vec::new();
        for &pin in &now {
            let Some((id, action)) = parse_layer_pin(pin) else { continue };
            let pressed = !self.prev.contains(pin);
            match action {
                LayerAction::Hold => held.push(id),
                LayerAction::Toggle if pressed => {
                    if !self.latched.remove(id) {
                        self.latched.insert(id.to_string());
                        came_on.push(id);
                    }
                }
                LayerAction::On if pressed => {
                    self.latched.insert(id.to_string());
                    came_on.push(id);
                }
                LayerAction::Off if pressed => {
                    self.latched.remove(id);
                }
                _ => {}
            }
        }
        self.prev = now.iter().map(|p| p.to_string()).collect();

        let before = self.order.len();
        let latched = &self.latched;
        let is_on = |id: &str| latched.contains(id) || held.contains(&id);
        self.order.retain(|id| is_on(id));
        let mut changed = self.order.len() != before;
        // Held layers come on in pin order, then the presses in order.
        for id in held.iter().chain(came_on.iter()) {
            if is_on(id) && !self.order.iter().any(|o| o.as_str() == *id) {
                self.order.push(id.to_string());
                changed = true;
            }
        }
        changed
    }
}

/// `base` with the settings of the layers in `order` (oldest first) written over
/// it: the graph a tick runs while they are on. Layers the graph has no entry for
/// (a layer being edited, a tab's layer after switching tabs) change nothing.
pub(crate) fn apply(base: &ProcessingGraph, order: &[String]) -> ProcessingGraph {
    let mut nodes = base.nodes.clone();
    for id in order {
        let Some(layer) = base.layers.iter().find(|l| &l.id == id) else { continue };
        for o in &layer.overrides {
            set_param(&mut nodes, &o.path, &o.key, &o.value);
        }
    }
    // Fresh caches: a pad's settings (gyro speed, deadzone) are among what a
    // layer changes.
    ProcessingGraph { nodes, layers: base.layers.clone(), ..Default::default() }
}

fn set_param(nodes: &mut [NodeSnap], path: &[usize], key: &str, value: &Value) -> bool {
    let Some((&uid, rest)) = path.split_first() else { return false };
    let Some(node) = nodes.iter_mut().find(|n| n.node_uid == uid) else { return false };
    if rest.is_empty() {
        // A card list keeps the patch's cards that switch layers.
        let value = if flexinput_core::layers::is_card_list(key) {
            flexinput_core::layers::merge_cards(node.params.get(key), value)
        } else {
            value.clone()
        };
        node.params.insert(key.to_string(), value);
        return true;
    }
    let Some(sg) = node.inline_subgraph.as_mut() else { return false };
    let changed = set_param(&mut sg.graph.nodes, rest, key, value);
    if changed {
        let inner = std::mem::take(&mut sg.graph);
        sg.graph = ProcessingGraph { nodes: inner.nodes, layers: inner.layers, ..Default::default() };
    }
    changed
}

/// The engine thread's layers: the stack, and the graph with them applied for
/// the patch graph it was worked out from.
#[derive(Default)]
pub(crate) struct LayerRuntime {
    stack: LayerStack,
    /// The patch graph `effective` was made from.
    base: Option<Arc<ProcessingGraph>>,
    effective: Option<Arc<ProcessingGraph>>,
}

impl LayerRuntime {
    /// At a wakeup, with the patch graph the UI last sent: take the UI's requests
    /// and work the applied graph out again if either changed.
    pub(crate) fn wake(&mut self, base: &Arc<ProcessingGraph>) {
        let requests = take_requests();
        let changed = !requests.is_empty() && self.stack.request(&requests);
        if changed {
            publish_active(self.stack.order());
        }
        let rebased = !self.base.as_ref().is_some_and(|b| Arc::ptr_eq(b, base));
        if changed || rebased {
            self.rebuild(base);
        }
    }

    /// After a tick, with the layer pins its cards held. Returns whether the graph
    /// the next tick runs changed.
    pub(crate) fn after_tick(&mut self, pins: &[(String, f32)], base: &Arc<ProcessingGraph>) -> bool {
        if pins.is_empty() && self.stack.prev.is_empty() {
            return false; // no card has touched a layer: the common case, for free
        }
        if !self.stack.update(pins) {
            return false;
        }
        publish_active(self.stack.order());
        self.rebuild(base);
        true
    }

    fn rebuild(&mut self, base: &Arc<ProcessingGraph>) {
        self.base = Some(base.clone());
        let applies = self.stack.order().iter().any(|id| base.layers.iter().any(|l| &l.id == id));
        self.effective = applies.then(|| Arc::new(apply(base, self.stack.order())));
    }

    /// The graph the next tick runs.
    pub(crate) fn graph(&self, base: &Arc<ProcessingGraph>) -> Arc<ProcessingGraph> {
        self.effective.clone().unwrap_or_else(|| base.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::LayerSnap;
    use flexinput_core::layers::layer_pin;

    fn pins(list: &[&str]) -> Vec<(String, f32)> {
        list.iter().map(|p| (p.to_string(), 1.0)).collect()
    }

    #[test]
    fn hold_is_on_while_held() {
        let mut s = LayerStack::default();
        let hold = layer_pin("a", LayerAction::Hold);
        assert!(s.update(&pins(&[&hold])));
        assert_eq!(s.order(), ["a"]);
        assert!(!s.update(&pins(&[&hold])), "still held");
        assert!(s.update(&[]));
        assert!(s.order().is_empty());
    }

    #[test]
    fn toggle_flips_on_each_press_and_on_off_latch() {
        let mut s = LayerStack::default();
        let t = layer_pin("a", LayerAction::Toggle);
        s.update(&pins(&[&t]));
        s.update(&pins(&[&t])); // still the same press
        s.update(&[]);
        assert_eq!(s.order(), ["a"], "on after the first press");
        s.update(&pins(&[&t]));
        assert!(s.order().is_empty(), "off after the second");
        s.update(&pins(&[&layer_pin("b", LayerAction::On)]));
        s.update(&[]);
        assert_eq!(s.order(), ["b"]);
        s.update(&pins(&[&layer_pin("b", LayerAction::Off)]));
        assert!(s.order().is_empty());
    }

    /// Steam's stacking: the layer on last is at the end, and wins.
    #[test]
    fn the_layer_on_last_is_on_top() {
        let mut s = LayerStack::default();
        s.request(&[("a".into(), true)]);
        s.request(&[("b".into(), true)]);
        assert_eq!(s.order(), ["a", "b"]);
        s.request(&[("a".into(), false)]);
        s.request(&[("a".into(), true)]);
        assert_eq!(s.order(), ["b", "a"]);
    }

    /// A chip clicked while a card holds a Toggle: the press isn't seen again, and
    /// a layer a card holds stays on.
    #[test]
    fn a_chip_leaves_what_cards_hold_alone() {
        let mut s = LayerStack::default();
        let t = layer_pin("a", LayerAction::Toggle);
        s.update(&pins(&[&t]));
        assert_eq!(s.order(), ["a"]);
        s.request(&[("b".into(), true)]);
        s.update(&pins(&[&t]));
        assert_eq!(s.order(), ["a", "b"], "the held Toggle isn't a second press");
        let hold = layer_pin("c", LayerAction::Hold);
        s.update(&pins(&[&t, &hold]));
        s.request(&[("c".into(), false)]);
        assert!(s.order().contains(&"c".to_string()), "held by a card, it stays on");
    }

    fn node(uid: usize, gain: f64) -> NodeSnap {
        NodeSnap {
            node_uid: uid,
            module_id: "math.multiply".into(),
            params: [("gain".to_string(), Value::from(gain))].into_iter().collect(),
            n_outputs: 1,
            input_sources: Vec::new(),
            device_id: None,
            output_pin_ids: Vec::new(),
            aux_f32_override: None,
            sink_target: None,
            inline_subgraph: None,
        }
    }

    #[test]
    fn layers_write_their_settings_in_order_down_into_sub_patches() {
        let mut sub = node(7, 1.0);
        sub.module_id = "subpatch".into();
        sub.inline_subgraph = Some(Box::new(crate::graph::InlineSubgraph {
            graph: ProcessingGraph::new(vec![node(3, 1.0)]),
            outlet_locs: Vec::new(),
        }));
        let set = |path: Vec<usize>, v: f64| crate::graph::LayerOverride { path, key: "gain".into(), value: Value::from(v) };
        let base = ProcessingGraph {
            nodes: vec![node(1, 1.0), sub],
            layers: vec![
                LayerSnap { id: "a".into(), overrides: vec![set(vec![1], 0.5), set(vec![7, 3], 2.0)] },
                LayerSnap { id: "b".into(), overrides: vec![set(vec![1], 3.0)] },
            ],
            ..Default::default()
        };
        let gain = |g: &ProcessingGraph, path: &[usize]| {
            let mut nodes = &g.nodes;
            let mut n = None;
            for (i, uid) in path.iter().enumerate() {
                let found = nodes.iter().find(|n| n.node_uid == *uid).unwrap();
                if i + 1 < path.len() {
                    nodes = &found.inline_subgraph.as_ref().unwrap().graph.nodes;
                }
                n = Some(found);
            }
            n.unwrap().params["gain"].as_f64().unwrap()
        };
        let g = apply(&base, &["a".into(), "b".into()]);
        assert_eq!(gain(&g, &[1]), 3.0, "the later layer wins");
        assert_eq!(gain(&g, &[7, 3]), 2.0, "inside the sub-patch");
        let g = apply(&base, &["b".into(), "a".into()]);
        assert_eq!(gain(&g, &[1]), 0.5);
        assert_eq!(gain(&base, &[1]), 1.0, "the patch's own graph is untouched");
        let g = apply(&base, &["gone".into()]);
        assert_eq!(gain(&g, &[1]), 1.0);
    }
}
