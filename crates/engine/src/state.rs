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
    /// Per-mapping progress for Remapper cards whose input ORDER matters ("in
    /// order" chords and Sequence mode). Indexed like `gesture_state`; unused
    /// entries for order-agnostic cards stay default.
    pub order_state: Vec<OrderTrack>,
    /// Per-mapping move timing for one-input analog Remapper cards in Short or
    /// Sequence mode, which time a move from where the input leaves zero.
    pub move_state: Vec<MoveTrack>,
    /// Per-pin hold-back state for a Remapper: inputs kept from the rest of the
    /// node while a card is still deciding on them ("in order", Sequence, or a
    /// Short / Long / Double press), and the late replay of a press it gave up on.
    pub hold_back: HashMap<String, HoldBackPin>,
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

/// Progress of one order-aware Remapper card (see `NodeState::order_state`).
#[derive(Default, Clone)]
pub struct OrderTrack {
    /// Each `in` pin's held state last tick, for edge detection.
    pub prev_held: Vec<bool>,
    /// How many leading `in` pins have matched, in order. Equal to the pin
    /// count while the card is matched.
    pub progress: usize,
    /// Seconds since the last step matched.
    pub since_step: f32,
}

/// Where a one-input analog card's current move stands (see
/// `NodeState::move_state`).
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub enum MovePhase {
    /// At rest (zero).
    #[default]
    Idle,
    /// Moving, not yet past the threshold, still inside the time gap.
    Moving,
    /// Reached the threshold inside the time gap and is still past it.
    Fast,
    /// Too slow, or dropped back under the threshold: nothing more from this
    /// move until the input returns to zero.
    Spent,
}

/// Timing of a one-input analog card's current move.
#[derive(Default, Clone, Copy, Debug)]
pub struct MoveTrack {
    pub phase: MovePhase,
    /// Seconds since the input left zero.
    pub since_start: f32,
    /// Seconds spent past the threshold during this fast move.
    pub above_s: f32,
    /// Short mode: seconds of output left to play back after a qualifying flick.
    pub replay_s: f32,
}

/// Hold-back state of one input pin (see `NodeState::hold_back`).
///
/// `cap` only matters for a stick direction: how far the rest of the node
/// still sees it deflect while hidden (0 = not at all).
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub enum HoldBackPin {
    #[default]
    Idle,
    /// Hidden while a card is still deciding on it. `held_s` is the current
    /// press's length so far; `released` means that press already ended (so
    /// it's replayed if the card gives up).
    Withheld { held_s: f32, released: bool, cap: f32 },
    /// Used by a card that fired; stays hidden until the pin is released.
    Consumed { cap: f32 },
    /// The card gave up after the press ended: the press plays back late.
    Replaying { remaining_s: f32 },
}
