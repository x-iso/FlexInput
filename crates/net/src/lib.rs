//! Network transport for the AutoMap bus: serialize the canonical gamepad bus
//! on one PC, carry it over UDP (plain / PSK-encrypted / QUIC datagrams), and
//! republish it on another. Feedback (rumble, lightbar, adaptive triggers)
//! rides the same link backwards.
//!
//! Crate layout mirrors the loopback-haptics design: this crate is a
//! dependency LEAF (core only). The engine's proc thread owns a [`NetManager`]
//! and reconciles it from the graph snapshot; per-node worker threads move
//! packets; eval and the UI talk to workers exclusively through the
//! process-global slots below (single-writer per slot, latest-value-wins).
//!
//! Slot dataflow per node uid:
//!
//! ```text
//! Send node:  eval --publish_send_frame--> [SEND_FRAMES] --worker--> socket
//!             socket --worker--> [LATEST_FEEDBACK] --latest_feedback--> eval
//! Recv node:  socket --worker--> [LATEST_INPUT] --latest_input--> eval
//!             eval --publish_feedback_frame--> [FEEDBACK_FRAMES] --worker--> socket
//! Both:       worker --set_status--> [STATUS] --status--> UI node body
//! ```

pub mod crypto;
pub mod frame;
pub mod manager;
pub mod protocol;
pub mod transport;

pub use frame::{BusFrame, FeedbackFrame};
pub use manager::{NetManager, NetNodeConfig, Transport};

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::Instant;

/// Link state summarized for the node body indicator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LinkState {
    /// No socket open (node just created, or reconcile hasn't run).
    #[default]
    Idle,
    /// Socket open, no fresh peer traffic (recv: no sender; send: no feedback).
    Listening,
    /// Fresh, valid traffic within the staleness window.
    Connected,
    /// Socket-level failure (bind/resolve error) — human-readable.
    Error(String),
}

/// Live per-node link status published by the worker, read by the UI each
/// frame. All counters are cheap approximations, not accounting.
#[derive(Clone, Debug, Default)]
pub struct NetStatus {
    pub state: LinkState,
    /// Age of the last valid inbound packet, if any ever arrived.
    pub last_rx_ms: Option<u64>,
    /// Sent / received packets per second (1s sliding window).
    pub tx_pps: u32,
    pub rx_pps: u32,
    /// Packets dropped by validation (bad magic, auth failure, stale seq…).
    pub drops: u64,
    /// Peer runs a different pin-table layout (version skew) — inputs beyond
    /// the common prefix are ignored.
    pub layout_warn: bool,
    /// Peer address as observed on the socket.
    pub remote: Option<String>,
    /// P2P tier only: this Receive node's own pairing code (its EndpointId), to
    /// be shared with the sender. Stable across restarts (derived from the
    /// node's persisted secret key). `None` on the UDP/PSK tiers.
    pub code: Option<String>,
}

/// Generate a fresh 32-byte node secret, hex-encoded (64 chars). Stored in the
/// Receive node's params so its P2P pairing code stays stable across restarts
/// and patch save/load. No iroh dependency — iroh's `SecretKey::from_str`
/// accepts this hex form.
pub fn generate_secret_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Derive the public pairing code (EndpointId) from a hex secret key, or `None`
/// if the P2P feature is disabled or the hex is invalid. Lets the UI show the
/// code immediately from params without waiting for the worker to bind.
#[cfg(feature = "p2p")]
pub fn endpoint_id_for_secret(secret_hex: &str) -> Option<String> {
    use std::str::FromStr;
    iroh::SecretKey::from_str(secret_hex).ok().map(|sk| sk.public().to_string())
}

/// Fallback when the P2P feature is off.
#[cfg(not(feature = "p2p"))]
pub fn endpoint_id_for_secret(_secret_hex: &str) -> Option<String> {
    None
}

// ── process-global latest-value slots ───────────────────────────────────────
//
// Same pattern as `loopback_manager`'s PARAMS/SCOPE/SPECTRUM: RwLock'd maps
// keyed by node uid, `retain`ed against the live node set on reconcile.
// Values are small (≤ ~500 B); clone-out keeps lock hold times trivial.

struct Stamped<T> {
    value: T,
    at: Instant,
}

static SEND_FRAMES: RwLock<Option<HashMap<usize, Stamped<BusFrame>>>> = RwLock::new(None);
static FEEDBACK_FRAMES: RwLock<Option<HashMap<usize, Stamped<FeedbackFrame>>>> = RwLock::new(None);
static LATEST_INPUT: RwLock<Option<HashMap<usize, Stamped<BusFrame>>>> = RwLock::new(None);
static LATEST_FEEDBACK: RwLock<Option<HashMap<usize, Stamped<FeedbackFrame>>>> = RwLock::new(None);
static STATUS: RwLock<Option<HashMap<usize, NetStatus>>> = RwLock::new(None);

fn put<T>(slot: &RwLock<Option<HashMap<usize, Stamped<T>>>>, uid: usize, value: T) {
    let mut guard = slot.write().unwrap();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(uid, Stamped { value, at: Instant::now() });
}

fn get<T: Clone>(
    slot: &RwLock<Option<HashMap<usize, Stamped<T>>>>,
    uid: usize,
) -> Option<(T, std::time::Duration)> {
    let guard = slot.read().unwrap();
    guard
        .as_ref()
        .and_then(|m| m.get(&uid))
        .map(|s| (s.value.clone(), s.at.elapsed()))
}

fn retain<T>(slot: &RwLock<Option<HashMap<usize, Stamped<T>>>>, live: &HashSet<usize>) {
    let mut guard = slot.write().unwrap();
    if let Some(map) = guard.as_mut() {
        map.retain(|uid, _| live.contains(uid));
    }
}

/// Eval (send node): publish this tick's outgoing bus frame.
pub fn publish_send_frame(uid: usize, frame: BusFrame) {
    put(&SEND_FRAMES, uid, frame);
}

/// Send worker: latest bus frame to transmit, with its age.
pub fn latest_send_frame(uid: usize) -> Option<(BusFrame, std::time::Duration)> {
    get(&SEND_FRAMES, uid)
}

/// Eval (recv node): publish the gathered feedback destined for the sender.
pub fn publish_feedback_frame(uid: usize, frame: FeedbackFrame) {
    put(&FEEDBACK_FRAMES, uid, frame);
}

/// Recv worker: latest feedback frame to transmit back, with its age.
pub fn latest_feedback_frame(uid: usize) -> Option<(FeedbackFrame, std::time::Duration)> {
    get(&FEEDBACK_FRAMES, uid)
}

/// Recv worker: publish a validated inbound bus frame.
pub fn set_latest_input(uid: usize, frame: BusFrame) {
    put(&LATEST_INPUT, uid, frame);
}

/// Eval (recv node): last valid inbound bus frame and how old it is.
pub fn latest_input(uid: usize) -> Option<(BusFrame, std::time::Duration)> {
    get(&LATEST_INPUT, uid)
}

/// Send worker: publish a validated inbound feedback frame.
pub fn set_latest_feedback(uid: usize, frame: FeedbackFrame) {
    put(&LATEST_FEEDBACK, uid, frame);
}

/// Eval (send node): last valid inbound feedback frame and how old it is.
pub fn latest_feedback(uid: usize) -> Option<(FeedbackFrame, std::time::Duration)> {
    get(&LATEST_FEEDBACK, uid)
}

/// Worker: publish link status for the node body.
pub fn set_status(uid: usize, status: NetStatus) {
    let mut guard = STATUS.write().unwrap();
    guard.get_or_insert_with(HashMap::new).insert(uid, status);
}

/// UI: latest link status for a node, or default (Idle) if none published.
pub fn status(uid: usize) -> NetStatus {
    STATUS
        .read()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&uid).cloned())
        .unwrap_or_default()
}

/// Manager: drop every slot for nodes not in `live` (call once per reconcile).
pub fn retain_all(live: &HashSet<usize>) {
    retain(&SEND_FRAMES, live);
    retain(&FEEDBACK_FRAMES, live);
    retain(&LATEST_INPUT, live);
    retain(&LATEST_FEEDBACK, live);
    let mut guard = STATUS.write().unwrap();
    if let Some(map) = guard.as_mut() {
        map.retain(|uid, _| live.contains(uid));
    }
}

/// Serializes tests that touch the process-global slots. In production there is
/// exactly ONE `NetManager`, so `retain_all` (which drops every uid not in the
/// reconciling manager's live set) is correct; but two concurrent test managers
/// — or the retain-all check below — would clobber each other's slots. Every
/// test that publishes/reads a slot or reconciles a manager must hold this.
#[cfg(test)]
pub(crate) static SLOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_roundtrip_and_retain() {
        let _guard = SLOT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Uids far outside any realistic node range to avoid collisions with
        // other tests sharing the process-global slots.
        let uid = 0xFFFF_0001;
        let mut fr = BusFrame::empty();
        fr.set("btn_south", flexinput_core::signal::Signal::Bool(true));
        publish_send_frame(uid, fr.clone());
        let (got, age) = latest_send_frame(uid).unwrap();
        assert_eq!(got, fr);
        assert!(age.as_secs() < 5);

        set_status(uid, NetStatus { state: LinkState::Connected, ..Default::default() });
        assert_eq!(status(uid).state, LinkState::Connected);
        assert_eq!(status(uid + 1).state, LinkState::Idle); // default for unknown

        retain_all(&HashSet::new());
        assert!(latest_send_frame(uid).is_none());
        assert_eq!(status(uid).state, LinkState::Idle);
    }
}
