//! UDP workers for the LAN (plaintext) and PSK (ChaCha20-Poly1305) tiers.
//! One OS thread per node; all socket waits use short read timeouts so the
//! `stop` flag is honored within a few milliseconds.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rand::Rng;

use crate::crypto::Cipher;
use crate::protocol::{self, decode, DecodeError, Packet, SeqTracker};
use crate::transport::should_stop;
use crate::{LinkState, NetStatus};

/// Retry cadence for bind/resolve failures.
const RETRY: Duration = Duration::from_secs(2);

/// Status publish cadence (the UI reads at frame rate; no need for more).
const STATUS_EVERY: Duration = Duration::from_millis(150);

/// Sliding 1-second window packet counters for the status pps fields.
#[derive(Default)]
struct PpsWindow {
    tx: u32,
    rx: u32,
    tx_out: u32,
    rx_out: u32,
    window_start: Option<Instant>,
}

impl PpsWindow {
    fn tick(&mut self) {
        let now = Instant::now();
        let start = *self.window_start.get_or_insert(now);
        if now.duration_since(start) >= Duration::from_secs(1) {
            self.tx_out = self.tx;
            self.rx_out = self.rx;
            self.tx = 0;
            self.rx = 0;
            self.window_start = Some(now);
        }
    }
}

struct LinkStats {
    pps: PpsWindow,
    drops: u64,
    layout_warn: bool,
    last_valid_rx: Option<Instant>,
    last_tx: Option<Instant>,
    remote: Option<SocketAddr>,
    last_status: Instant,
}

impl LinkStats {
    fn new() -> Self {
        Self {
            pps: PpsWindow::default(),
            drops: 0,
            layout_warn: false,
            last_valid_rx: None,
            last_tx: None,
            remote: None,
            last_status: Instant::now() - STATUS_EVERY,
        }
    }

    /// Publish status. `connected` is computed by the caller per role: the SEND
    /// worker is connected whenever it is actively transmitting to its peer
    /// (UDP is fire-and-forget, so a returning feedback stream isn't required to
    /// call the forward link healthy); the RECV worker is connected while it has
    /// fresh inbound input.
    fn publish(&mut self, uid: usize, connected: bool, force: bool) {
        if !force && self.last_status.elapsed() < STATUS_EVERY {
            return;
        }
        self.last_status = Instant::now();
        self.pps.tick();
        crate::set_status(
            uid,
            NetStatus {
                state: if connected { LinkState::Connected } else { LinkState::Listening },
                last_rx_ms: self.last_valid_rx.map(|t| t.elapsed().as_millis() as u64),
                tx_pps: self.pps.tx_out,
                rx_pps: self.pps.rx_out,
                drops: self.drops,
                layout_warn: self.layout_warn,
                remote: self.remote.map(|a| a.to_string()),
            },
        );
    }
}

fn publish_error(uid: usize, msg: impl Into<String>) {
    crate::set_status(uid, NetStatus { state: LinkState::Error(msg.into()), ..Default::default() });
}

/// Sleep in small slices so `stop` stays responsive during error backoff.
fn backoff(stop: &AtomicBool, total: Duration) {
    let deadline = Instant::now() + total;
    while !should_stop(stop) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn make_cipher(encrypted: bool, psk: &str) -> Option<Cipher> {
    encrypted.then(|| Cipher::from_passphrase(psk))
}

/// Send-node worker: transmit the latest published bus frame at `rate_hz`,
/// drain feedback packets coming back on the same socket.
pub fn run_send(
    stop: &AtomicBool,
    uid: usize,
    host: String,
    port: u16,
    rate_hz: u32,
    encrypted: bool,
    psk: String,
) {
    let cipher = make_cipher(encrypted, &psk);
    let period = Duration::from_secs_f64(1.0 / rate_hz.clamp(1, 4000) as f64);

    'outer: while !should_stop(stop) {
        // Resolve + connect. Both can block (DNS) — that's why we're on a
        // dedicated thread and not the proc thread.
        let addr = match (host.as_str(), port).to_socket_addrs().map(|mut a| a.next()) {
            Ok(Some(a)) => a,
            _ => {
                publish_error(uid, format!("cannot resolve {host}"));
                backoff(stop, RETRY);
                continue;
            }
        };
        let socket = match UdpSocket::bind(("0.0.0.0", 0)).and_then(|s| {
            s.connect(addr)?;
            Ok(s)
        }) {
            Ok(s) => s,
            Err(e) => {
                publish_error(uid, format!("socket error: {e}"));
                backoff(stop, RETRY);
                continue;
            }
        };

        let session_id: u64 = rand::rng().random();
        let mut seq: u32 = 0;
        let mut fb_seq = SeqTracker::default();
        let mut stats = LinkStats::new();
        stats.remote = Some(addr);
        let mut buf = [0u8; protocol::MAX_PACKET];
        let mut next_send = Instant::now();

        while !should_stop(stop) {
            // Wait for inbound feedback until the next pace tick is due.
            let now = Instant::now();
            let wait = next_send.saturating_duration_since(now).max(Duration::from_micros(100));
            let _ = socket.set_read_timeout(Some(wait));
            match socket.recv(&mut buf) {
                Ok(n) => match decode(&buf[..n], cipher.as_ref()) {
                    Ok(d) if matches!(d.packet, Packet::Feedback(_)) => {
                        if fb_seq.accept(d.header.session_id, d.header.seq) {
                            if let Packet::Feedback(f) = d.packet {
                                crate::set_latest_feedback(uid, f);
                            }
                            stats.pps.rx += 1;
                            stats.last_valid_rx = Some(Instant::now());
                            stats.layout_warn = !d.layout_match;
                        } else {
                            stats.drops += 1;
                        }
                    }
                    Ok(_) => stats.drops += 1, // input-direction packet at the send end
                    Err(DecodeError::NotFlexInput) => stats.drops += 1,
                    Err(_) => stats.drops += 1,
                },
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                // 10054 (ICMP port unreachable reflected onto a connected UDP
                // socket) and friends: keep sending, peer may come up later.
                Err(_) => {}
            }

            if Instant::now() >= next_send {
                next_send += period;
                // If we fell far behind (debugger pause, laptop sleep), resync
                // rather than bursting a backlog of stale frames.
                if next_send < Instant::now() {
                    next_send = Instant::now() + period;
                }
                if let Some((frame, _age)) = crate::latest_send_frame(uid) {
                    let pkt = protocol::encode_input(&frame, session_id, seq, cipher.as_ref());
                    seq = seq.wrapping_add(1);
                    if seq == 0 {
                        // Nonce space for this session exhausted → new session.
                        break; // reconnect loop rolls a fresh session_id
                    }
                    if socket.send(&pkt).is_ok() {
                        stats.pps.tx += 1;
                        stats.last_tx = Some(Instant::now());
                    }
                }
                // The forward link is healthy whenever we're actively sending —
                // the returning feedback stream (rx_pps) is shown separately.
                let connected = stats.last_tx.map(|t| t.elapsed() < Duration::from_secs(1)).unwrap_or(false);
                stats.publish(uid, connected, false);
            }
        }

        if should_stop(stop) {
            break 'outer;
        }
    }
}

/// Recv-node worker: accept input frames on `bind_port`, return feedback
/// frames to the last valid sender at `fb_rate_hz`.
pub fn run_recv(
    stop: &AtomicBool,
    uid: usize,
    bind_port: u16,
    stale_ms: u32,
    fb_rate_hz: u32,
    encrypted: bool,
    psk: String,
) {
    let cipher = make_cipher(encrypted, &psk);
    let stale = Duration::from_millis(stale_ms.max(50) as u64);
    let fb_period = Duration::from_secs_f64(1.0 / fb_rate_hz.clamp(1, 2000) as f64);

    while !should_stop(stop) {
        let socket = match UdpSocket::bind(("0.0.0.0", bind_port)) {
            Ok(s) => s,
            Err(e) => {
                publish_error(uid, format!("bind :{bind_port} failed: {e}"));
                backoff(stop, RETRY);
                continue;
            }
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(1)));

        let session_id: u64 = rand::rng().random();
        let mut fb_seq: u32 = 0;
        let mut in_seq = SeqTracker::default();
        let mut stats = LinkStats::new();
        let mut buf = [0u8; protocol::MAX_PACKET];
        let mut next_fb = Instant::now();
        let mut sender: Option<SocketAddr> = None;

        while !should_stop(stop) {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) => match decode(&buf[..n], cipher.as_ref()) {
                    Ok(d) if matches!(d.packet, Packet::Input(_)) => {
                        if in_seq.accept(d.header.session_id, d.header.seq) {
                            if let Packet::Input(f) = d.packet {
                                crate::set_latest_input(uid, f);
                            }
                            sender = Some(from);
                            stats.remote = Some(from);
                            stats.pps.rx += 1;
                            stats.last_valid_rx = Some(Instant::now());
                            stats.layout_warn = !d.layout_match;
                        } else {
                            stats.drops += 1;
                        }
                    }
                    Ok(_) => stats.drops += 1,
                    Err(_) => stats.drops += 1,
                },
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => {}
            }

            // Feedback return leg — only while the forward link is live, so a
            // vanished sender doesn't accumulate ICMP errors forever.
            let input_fresh = stats
                .last_valid_rx
                .map(|t| t.elapsed() < stale)
                .unwrap_or(false);
            if input_fresh && Instant::now() >= next_fb {
                next_fb += fb_period;
                if next_fb < Instant::now() {
                    next_fb = Instant::now() + fb_period;
                }
                if let (Some(to), Some((frame, _age))) = (sender, crate::latest_feedback_frame(uid))
                {
                    let pkt = protocol::encode_feedback(&frame, session_id, fb_seq, cipher.as_ref());
                    fb_seq = fb_seq.wrapping_add(1);
                    if fb_seq == 0 {
                        break; // roll a fresh session_id for nonce safety
                    }
                    if socket.send_to(&pkt, to).is_ok() {
                        stats.pps.tx += 1;
                    }
                }
            }

            let connected = stats.last_valid_rx.map(|t| t.elapsed() < stale).unwrap_or(false);
            stats.publish(uid, connected, false);
        }
        return;
    }
}
