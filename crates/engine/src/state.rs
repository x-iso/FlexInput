use std::collections::VecDeque;
use std::time::Instant;

use flexinput_core::Signal;

/// Per-node computation state owned by the processing thread.
/// Replaces the computation fields that were previously in NodeExtra.
#[derive(Default)]
pub struct NodeState {
    /// Latest computed outputs for stateful nodes (oscillator, delay, etc.).
    pub last_signals: Vec<Option<Signal>>,
    /// Per-channel ring buffers of (timestamp, value) pairs for the delay module.
    pub delay_bufs: Vec<VecDeque<(Instant, f32)>>,
    /// Per-channel sample ring buffers for the moving-average module.
    pub avg_bufs: Vec<VecDeque<f32>>,
    /// Per-channel fast EMA for the DC filter.
    pub dc_fast: Vec<f64>,
    /// Per-channel slow EMA for the DC filter — estimates DC level.
    pub dc_estimates: Vec<f64>,
    /// Per-channel correction being applied for the DC filter module.
    pub dc_corrections: Vec<f64>,
    /// Per-channel time (seconds) signal has been stable AND non-zero.
    pub dc_timers: Vec<f32>,
    /// Corrected output frozen at the moment input starts moving.
    pub dc_frozen: Vec<f64>,
    /// Crossfade blend factor 0→1, advances while input is moving.
    pub dc_blend: Vec<f64>,
    /// Previous-frame signal snapshot for edge-detection modules.
    pub prev_signals: Vec<Option<Signal>>,
    /// Generic f32 scratch space for stateful modules (timers, accumulators).
    pub aux_f32: Vec<f32>,
}
