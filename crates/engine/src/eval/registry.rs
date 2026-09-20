//! Per-module evaluator hooks — the engine layer of the module-registry seam
//! (Phase C).
//!
//! Consulted BEFORE the hardcoded `module_id` match in `eval_graph_tick` /
//! `eval_subgraph`. A module that registers a `publish` hook is dispatched from
//! the registry; the hardcoded arms stay in place as fallback so the remaining
//! modules migrate onto the registry one at a time rather than in a big bang.
//! Keyed by `module_id` string to avoid a crate dependency cycle (the engine
//! can't depend on `crates/modules`). Pilot module: Audio Stream Haptics.

use std::collections::HashMap;

use flexinput_core::Signal;

use crate::graph::NodeSnap;

use crate::state::NodeState;

use super::{audio_stream_haptics_publish, jsm_publish, AUDIO_STREAM_HAPTICS_ID, JSM_ID};

/// An "injector" publisher (the feedback / network / ASTH shape): it publishes
/// the node's outputs into the collector + device signal maps under `uid` and
/// returns its output vector (output[0] = AutoMap pass-through, output[1..] =
/// module-specific data). The returned vector becomes the node's `computed[idx]`
/// and its `last_outputs[uid]`, then dispatch `continue`s. `uid` is the node's
/// effective publishing id (`node_uid` at top level, the namespaced uid nested).
pub(crate) type PublishFn = fn(
    &NodeSnap,
    usize,
    &HashMap<(String, String), Signal>,
    &mut HashMap<(String, String), Signal>,
) -> Vec<Option<Signal>>;

/// The same shape as [`PublishFn`], for a module that also needs its own
/// per-node state (and the tick's `dt`) — a state machine rather than a filter.
pub(crate) type PublishStatefulFn = fn(
    &NodeSnap,
    usize,
    &HashMap<(String, String), Signal>,
    &mut HashMap<(String, String), Signal>,
    &mut HashMap<usize, NodeState>,
    f32,
) -> Vec<Option<Signal>>;

/// Engine-side hooks for one module id. Grows as more of a module's engine
/// behaviour migrates off the hardcoded dispatch.
pub(crate) struct EvalHooks {
    /// Replaces the default `compute_node` path with an injector publisher.
    pub(crate) publish: Option<PublishFn>,
    /// As `publish`, but handed the graph's node state and the tick's `dt`.
    pub(crate) publish_stateful: Option<PublishStatefulFn>,
}

/// Look up a module's engine hooks, or `None` to fall through to the hardcoded
/// dispatch. This runs once per node per tick, so keep it a cheap string match.
pub(crate) fn eval_hooks(module_id: &str) -> Option<&'static EvalHooks> {
    match module_id {
        AUDIO_STREAM_HAPTICS_ID => Some(&ASTH_HOOKS),
        JSM_ID => Some(&JSM_HOOKS),
        _ => None,
    }
}

static ASTH_HOOKS: EvalHooks = EvalHooks {
    publish: Some(audio_stream_haptics_publish),
    publish_stateful: None,
};

static JSM_HOOKS: EvalHooks = EvalHooks {
    publish: None,
    publish_stateful: Some(jsm_publish),
};
