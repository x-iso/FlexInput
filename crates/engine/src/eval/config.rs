//! Module ids the evaluator dispatches on, the network-node param
//! readers, and [`TickOutput`] — the per-tick result the app thread
//! consumes. Plus the haptic-feedback shaping the transports share.

use super::*;

/// The `prefix:` namespaces a signal source id carries when a NODE produced it
/// rather than a physical device — collectors, fork/selector, Remapper,
/// Combiner, gyro Lean, Touch Zones and Menu.
///
/// The engine reads them to route a source through `collector_sigs`, and the UI
/// reads them to decide whether a node needs an `_automap_collector_id`. Both
/// used to spell the list out inline, seven `starts_with` arms each; adding an
/// eighth producer meant finding both, and missing one would have left the
/// graph builder and the evaluator disagreeing about what a source IS.
pub const NAMESPACED_SOURCE_PREFIXES: &[&str] = &[
    "collector:",
    "forksel:",
    "remap:",
    "combiner:",
    "lean:",
    "touchmap:",
    "menumap:",
];

/// Is this source id produced by a node (see [`NAMESPACED_SOURCE_PREFIXES`])
/// rather than by a physical device?
pub fn is_namespaced_source(id: &str) -> bool {
    NAMESPACED_SOURCE_PREFIXES.iter().any(|p| id.starts_with(p))
}

/// Stable module id for the Audio Stream Haptics node (audio-loopback → rumble).
pub const AUDIO_STREAM_HAPTICS_ID: &str = "module.audio_stream_haptics";

/// Stable module ids for the network transport nodes.
pub const NET_SEND_ID: &str = "module.network_send";
pub const NET_RECV_ID: &str = "module.network_recv";

/// Build a [`NetNodeConfig`](flexinput_net::NetNodeConfig) from a network node's
/// params, or `None` if the module id isn't a network node. Shared param keys:
/// `net_transport` ("udp"|"psk"|"quic"), `net_psk`. Send adds `net_host`,
/// `net_port`, `net_rate_hz`; recv adds `net_bind_port`, `net_stale_ms`,
/// `net_fb_rate_hz`. See the node body UI in `crates/ui` for defaults.
pub fn net_config_from_params(
    module_id: &str,
    params: &HashMap<String, Value>,
) -> Option<flexinput_net::NetNodeConfig> {
    use flexinput_net::{NetNodeConfig, Transport};
    let transport = Transport::from_str(
        params.get("net_transport").and_then(|v| v.as_str()).unwrap_or("udp"),
    );
    let psk = params.get("net_psk").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let u16p = |k: &str, d: u16| {
        params.get(k).and_then(|v| v.as_u64()).unwrap_or(d as u64).clamp(1, 65535) as u16
    };
    let u32p = |k: &str, d: u32| params.get(k).and_then(|v| v.as_u64()).unwrap_or(d as u64) as u32;
    let str_param = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match module_id {
        NET_SEND_ID => Some(NetNodeConfig::Send {
            transport,
            host: params.get("net_host").and_then(|v| v.as_str()).unwrap_or("127.0.0.1").to_string(),
            port: u16p("net_port", 46700),
            rate_hz: u32p("net_rate_hz", 500),
            psk,
            peer_code: str_param("net_peer"),
        }),
        NET_RECV_ID => Some(NetNodeConfig::Recv {
            transport,
            bind_port: u16p("net_bind_port", 46700),
            stale_ms: u32p("net_stale_ms", 200),
            fb_rate_hz: u32p("net_fb_rate_hz", 200),
            psk,
            secret_key: str_param("net_secret"),
        }),
        _ => None,
    }
}

/// Build a loopback [`CaptureRequest`](flexinput_devices::loopback_manager::CaptureRequest)
/// from an Audio Stream Haptics node's params. Schema:
///   `asth_mode`         = "process" | "focused" | "system" (default "system")
///   `asth_target_name`  = exe name (process mode)
///   `asth_include_tree` = bool (default true)
/// Returns `None` for process mode with no target name set yet.
#[cfg(windows)]
pub fn loopback_request_from_params(
    params: &HashMap<String, Value>,
) -> Option<flexinput_devices::loopback_manager::CaptureRequest> {
    use flexinput_devices::loopback_manager::CaptureRequest;
    let mode = params.get("asth_mode").and_then(|v| v.as_str()).unwrap_or("system");
    let include_tree = params.get("asth_include_tree").and_then(|v| v.as_bool()).unwrap_or(true);
    match mode {
        "process" => {
            let name = params.get("asth_target_name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                None
            } else {
                Some(CaptureRequest::ProcessName { name: name.to_string(), include_tree })
            }
        }
        "focused" => Some(CaptureRequest::Focused { include_tree }),
        _ => Some(CaptureRequest::System),
    }
}

/// Perceptual shaping for HD voice-coil amplitude on the AutoMap feedback path,
/// using the source virtual device's per-device floor/max/exp.
///
/// Maps a 0..1 input to 0 (when zero) or `floor + (max-floor) * v^exp` (when
/// non-zero). A game's classic rumble is often weak (0.1–0.3); mapped onto an HD
/// coil and run through the encoder's power-law amp curve it's below the felt
/// threshold. `floor` lifts any non-zero rumble to a perceptible level, `max`
/// caps the ceiling, and `exp < 1` boosts the low (felt) end. Exactly 0 stays 0
/// (silent). With defaults (floor 0.35, max 1.0, exp 0.6): 0.09 -> ~0.49,
/// 0.21 -> ~0.60, 1.0 -> 1.0. floor <= 0 means pass-through (no shaping).
pub(crate) fn shape_hd_feedback(sig: Signal, floor: f32, max: f32, exp: f32) -> Signal {
    let v = match sig {
        Signal::Float(f) => f,
        Signal::Bool(b) => if b { 1.0 } else { 0.0 },
        _ => return sig,
    };
    if v <= 0.0 {
        return Signal::Float(0.0);
    }
    if floor <= 0.0 {
        // Pass-through, but still honor a ceiling below 1.0.
        return Signal::Float(v.clamp(0.0, max.clamp(0.0, 1.0)));
    }
    let floor = floor.clamp(0.0, 1.0);
    let max = max.clamp(floor, 1.0);
    let exp = exp.max(0.01);
    let shaped = floor + (max - floor) * v.clamp(0.0, 1.0).powf(exp);
    Signal::Float(shaped.clamp(0.0, 1.0))
}

/// Combine two feedback values targeting the same physical haptic pin from
/// different virtual sources. Haptics are level-triggered, so "any source active
/// wins" = take the larger magnitude (Float) / logical-or (Bool). Used so two
/// virtual pads fed by one physical device both reach its rumble/LED, instead of
/// the first-seen silently winning. Mixed/other signal types keep the existing
/// value (no meaningful combine).
pub(crate) fn combine_feedback_max(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(if x.abs() >= y.abs() { x } else { y }),
        (Signal::Bool(x), Signal::Bool(y)) => Signal::Bool(x || y),
        // Float vs Bool: coerce the bool to 0/1 and compare magnitudes.
        (Signal::Float(x), Signal::Bool(y)) | (Signal::Bool(y), Signal::Float(x)) => {
            let yb = if y { 1.0 } else { 0.0 };
            Signal::Float(if x.abs() >= yb { x } else { yb })
        }
        (a, _) => a,
    }
}

// ── Public output type ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct TickOutput {
    /// Latest output per (node_uid, output_pin). Excludes device.source (UI evaluates fresh).
    pub outputs: HashMap<(usize, usize), Option<Signal>>,
    /// Per display node: one scope sample for this tick (uid, per-channel values).
    pub scope_samples: Vec<(usize, Vec<Option<f32>>)>,
    /// Latest inputs per display/response_curve node for UI readout rendering.
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Latest outputs per twoway_response_curve node (blended engine output for UI arrow).
    pub last_outputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Latest signals destined for each (device_id, pin_id) sink slot.
    pub sink_outputs: HashMap<(String, String), Signal>,
}

impl TickOutput {
    /// Clear all containers in-place. Preserves allocated capacity so the
    /// proc thread can reuse the same `TickOutput` across ticks instead of
    /// dropping and reallocating five HashMaps per call (was hot at the default 2 kHz rate).
    pub fn clear(&mut self) {
        self.outputs.clear();
        self.scope_samples.clear();
        self.last_inputs.clear();
        self.last_outputs.clear();
        self.sink_outputs.clear();
    }
}
