use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use glam::Vec2;
use flexinput_core::Signal;

/// Per-node computation state owned by the processing thread.
/// Replaces the computation fields that were previously in NodeExtra.
#[derive(Default)]
pub struct NodeState {
    /// Latest computed outputs for stateful nodes (oscillator, delay, etc.).
    pub last_signals: Vec<Option<Signal>>,
    /// Per-channel ring buffers of (timestamp, value) pairs for the delay module.
    pub delay_bufs: Vec<VecDeque<(Instant, f32)>>,
    /// Per-channel sample ring buffers for the moving-average module (float inputs).
    pub avg_bufs: Vec<VecDeque<f32>>,
    /// Per-channel sample ring buffers for the moving-average module (Vec2 inputs).
    pub avg_bufs_v2: Vec<VecDeque<Vec2>>,
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
    /// Per-mapping state for Remapper / Map Action press modes (4 floats per
    /// mapping — see `eval::press_state_get`). Kept separate from `aux_f32`
    /// so it can grow with the mapping list without clashing with the touchpad
    /// zone accumulators already living at `aux_f32[0..3]`.
    pub press_state: Vec<f32>,
    /// Per-mapping stick-gesture progress for Remapper / Map Action mappings
    /// whose `in` set is entirely stick cardinals. Two `u8` bitmaps per
    /// mapping (left_stick, right_stick), each bit indexed in `CARDINAL_BITS`
    /// order. A bit becomes 1 when the matching cardinal has been visited
    /// during the active gesture; the bitmap resets when the stick returns
    /// to neutral (mag < 0.3). Gesture-mappings fire when every required
    /// cardinal across both sticks has been visited at least once.
    pub gesture_state: Vec<[u8; 2]>,
    // ── Two-way Response Curve ────────────────────────────────────────────────
    /// Per-channel current lane: +1 = rising, -1 = falling.
    pub twoway_lane: Vec<i8>,
    /// Per-channel ring buffer of recent per-tick input deltas for hysteresis.
    pub twoway_dir_buf: Vec<VecDeque<f32>>,
    /// Per-channel interpolation blend factor [0, 1] (0 = fully on old lane output).
    pub twoway_blend: Vec<f32>,
    /// Per-channel previous tick input value for delta computation.
    pub twoway_prev_input: Vec<f32>,
    /// Per-channel blended output frozen at the moment a lane switch begins.
    pub twoway_old_output: Vec<f32>,
    // ── Macro-namespace carry-over (stored on a single reserved sentinel uid) ──
    /// Previous tick's macro-namespace values (`macro:` / `menu:` control pins).
    /// `collector_sigs` is rebuilt from empty every tick, so a macro READER that
    /// is forced to evaluate BEFORE its producer — e.g. a Virtual Menu that sits
    /// upstream of the Remapper mapping to its Select target, a genuine feedback
    /// cycle — would never observe the value this tick. This snapshot lets such a
    /// reader fall back to the last published value (one tick stale, imperceptible
    /// at kHz rates). Only the reserved `MACRO_CARRY_UID` entry uses this field.
    pub macro_prev: HashMap<(String, String), Signal>,
    /// Virtual Menu SOURCE-BLOCK request, `(device_id, pin_id)` pairs an open
    /// menu asked to zero at the source so those analog inputs reach ONLY the
    /// menu's navigation (not a mouse mapping, another module, or the pad).
    /// Rebuilt each tick from `collector_sigs` and applied to `dev_sigs` at the
    /// START of the NEXT tick — one tick stale, imperceptible. Only the reserved
    /// `MACRO_CARRY_UID` entry uses this field.
    pub source_block: HashSet<(String, String)>,
    /// Pre-block values of the pins in `source_block`, snapshotted when the block
    /// is applied so the menu can still READ its navigation inputs (it's the
    /// reason they're blocked for everyone else). Only the reserved
    /// `MACRO_CARRY_UID` entry uses this field.
    pub unblocked_src: HashMap<(String, String), Signal>,
}
