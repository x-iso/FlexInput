//! P2P transport tier (feature `p2p`): iroh endpoints carrying the same
//! protocol packets in unreliable QUIC datagrams.
//!
//! iroh gives us dial-by-`EndpointId` (the peer's public key IS the address),
//! NAT hole-punching with relay fallback, and DNS discovery — so a Send node
//! connects with just the Receive node's pairing code, no IP / port / port-
//! forward. Authentication and encryption come from iroh's own TLS keyed by the
//! endpoint keypair, so the inner protocol packets travel as plaintext
//! (`cipher = None`); `remote_id()` is the verified peer identity.
//!
//! When the `p2p` feature is disabled this compiles to a stub that surfaces a
//! clear status error instead of pulling the iroh dependency tree.

use std::sync::atomic::AtomicBool;

#[cfg(not(feature = "p2p"))]
mod stub {
    use super::*;
    use crate::{LinkState, NetStatus};

    fn unavailable(uid: usize) {
        crate::set_status(
            uid,
            NetStatus {
                state: LinkState::Error("P2P support not built".into()),
                ..Default::default()
            },
        );
    }

    pub fn run_send(_stop: &AtomicBool, uid: usize, _peer_code: String, _rate_hz: u32) {
        unavailable(uid);
    }
    pub fn run_recv(
        _stop: &AtomicBool,
        uid: usize,
        _secret_key: String,
        _stale_ms: u32,
        _fb_rate_hz: u32,
    ) {
        unavailable(uid);
    }
}

#[cfg(not(feature = "p2p"))]
pub use stub::{run_recv, run_send};

#[cfg(feature = "p2p")]
pub use imp::{run_recv, run_send};

#[cfg(feature = "p2p")]
mod imp {
    use super::*;
    use std::str::FromStr;
    use std::sync::atomic::Ordering;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use iroh::{Endpoint, EndpointId, SecretKey};

    use crate::protocol::{self, decode, Packet, SeqTracker};
    use crate::{LinkState, NetStatus};

    /// Protocol/version identifier negotiated on every iroh connection.
    const ALPN: &[u8] = b"flexinput/net/1";
    const RETRY: Duration = Duration::from_secs(2);
    const STATUS_EVERY: Duration = Duration::from_millis(200);

    /// One shared multi-threaded tokio runtime drives every iroh worker's
    /// endpoint + background tasks (relay, discovery, magicsock). Each worker
    /// thread blocks on its own async loop against this runtime.
    fn runtime() -> &'static tokio::runtime::Runtime {
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .thread_name("flexinput-iroh")
                .build()
                .expect("build shared iroh tokio runtime")
        })
    }

    fn set_err(uid: usize, msg: impl Into<String>) {
        crate::set_status(uid, NetStatus { state: LinkState::Error(msg.into()), ..Default::default() });
    }

    async fn sleep_stop(stop: &AtomicBool, total: Duration) {
        let deadline = Instant::now() + total;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 1-second sliding packet counters for the status pps fields.
    #[derive(Default)]
    struct Pps {
        tx: u32,
        rx: u32,
        tx_out: u32,
        rx_out: u32,
        start: Option<Instant>,
    }
    impl Pps {
        fn tick(&mut self) {
            let now = Instant::now();
            let s = *self.start.get_or_insert(now);
            if now.duration_since(s) >= Duration::from_secs(1) {
                self.tx_out = std::mem::take(&mut self.tx);
                self.rx_out = std::mem::take(&mut self.rx);
                self.start = Some(now);
            }
        }
    }

    // ── Send: dial the peer's EndpointId, stream input, drain feedback ────────
    pub fn run_send(stop: &AtomicBool, uid: usize, peer_code: String, rate_hz: u32) {
        let peer = match EndpointId::from_str(peer_code.trim()) {
            Ok(id) => id,
            Err(_) => return set_err(uid, "invalid peer code"),
        };
        let period = Duration::from_secs_f64(1.0 / rate_hz.clamp(1, 4000) as f64);

        runtime().block_on(async move {
            // Ephemeral identity for the dialer — the receiver doesn't pin it.
            let endpoint = match Endpoint::builder(iroh::endpoint::presets::N0)
                .secret_key(SecretKey::generate())
                .bind()
                .await
            {
                Ok(e) => e,
                Err(e) => return set_err(uid, format!("endpoint: {e}")),
            };

            while !stop.load(Ordering::Relaxed) {
                crate::set_status(uid, NetStatus { state: LinkState::Listening, ..Default::default() });
                let conn = match endpoint.connect(peer, ALPN).await {
                    Ok(c) => c,
                    Err(e) => {
                        set_err(uid, format!("connect: {e}"));
                        sleep_stop(stop, RETRY).await;
                        continue;
                    }
                };
                let remote = conn.remote_id().to_string();
                let session_id: u64 = rand::rng().random();
                let mut seq: u32 = 0;
                let mut fb_seq = SeqTracker::default();
                let mut pps = Pps::default();
                let mut last_tx: Option<Instant> = None;
                let mut next = Instant::now();
                let mut last_status = Instant::now() - STATUS_EVERY;

                loop {
                    if stop.load(Ordering::Relaxed) {
                        conn.close(0u32.into(), b"stop");
                        return;
                    }
                    // Drain inbound feedback datagrams without blocking the pace.
                    while let Ok(Ok(bytes)) =
                        tokio::time::timeout(Duration::from_micros(50), conn.read_datagram()).await
                    {
                        if let Ok(d) = decode(&bytes, None) {
                            if let Packet::Feedback(f) = d.packet {
                                if fb_seq.accept(d.header.session_id, d.header.seq) {
                                    crate::set_latest_feedback(uid, f);
                                    pps.rx += 1;
                                }
                            }
                        }
                    }

                    if let Some((frame, _)) = crate::latest_send_frame(uid) {
                        let pkt = protocol::encode_input(&frame, session_id, seq, None);
                        seq = seq.wrapping_add(1);
                        if seq == 0 {
                            break; // roll a fresh session on nonce exhaustion
                        }
                        if conn.send_datagram(pkt.into()).is_ok() {
                            pps.tx += 1;
                            last_tx = Some(Instant::now());
                        } else {
                            break; // connection lost → reconnect
                        }
                    }

                    if last_status.elapsed() >= STATUS_EVERY {
                        last_status = Instant::now();
                        pps.tick();
                        let connected = last_tx.map(|t| t.elapsed() < Duration::from_secs(1)).unwrap_or(false);
                        crate::set_status(uid, NetStatus {
                            state: if connected { LinkState::Connected } else { LinkState::Listening },
                            tx_pps: pps.tx_out, rx_pps: pps.rx_out,
                            remote: Some(remote.clone()),
                            ..Default::default()
                        });
                    }

                    next += period;
                    let now = Instant::now();
                    if next > now { tokio::time::sleep(next - now).await; } else { next = now; }
                }
            }
        });
    }

    // ── Recv: bind a stable identity, accept a sender, serve input + feedback ─
    pub fn run_recv(stop: &AtomicBool, uid: usize, secret_key: String, stale_ms: u32, fb_rate_hz: u32) {
        let secret = match SecretKey::from_str(secret_key.trim()) {
            Ok(s) => s,
            Err(_) => return set_err(uid, "invalid node key"),
        };
        let stale = Duration::from_millis(stale_ms.max(50) as u64);
        let fb_period = Duration::from_secs_f64(1.0 / fb_rate_hz.clamp(1, 2000) as f64);
        let code = secret.public().to_string();

        runtime().block_on(async move {
            let endpoint = match Endpoint::builder(iroh::endpoint::presets::N0)
                .secret_key(secret)
                .alpns(vec![ALPN.to_vec()])
                .bind()
                .await
            {
                Ok(e) => e,
                Err(e) => return set_err(uid, format!("bind failed: {e}")),
            };
            // Publish our pairing code immediately (available from the key, before
            // the relay is even online) so the UI can show it.
            let listening = |extra_rx: u32| NetStatus {
                state: LinkState::Listening,
                code: Some(code.clone()),
                rx_pps: extra_rx,
                ..Default::default()
            };
            crate::set_status(uid, listening(0));

            loop {
                if stop.load(Ordering::Relaxed) {
                    endpoint.close().await;
                    return;
                }
                let incoming = match tokio::time::timeout(Duration::from_millis(200), endpoint.accept()).await {
                    Ok(Some(inc)) => inc,
                    Ok(None) => return, // endpoint closed
                    Err(_) => { crate::set_status(uid, listening(0)); continue; }
                };
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let remote = conn.remote_id().to_string();
                let session_id: u64 = rand::rng().random();
                let mut fb_seq: u32 = 0;
                let mut in_seq = SeqTracker::default();
                let mut last_rx: Option<Instant> = None;
                let mut next_fb = Instant::now();
                let mut pps = Pps::default();
                let mut last_status = Instant::now() - STATUS_EVERY;

                loop {
                    if stop.load(Ordering::Relaxed) {
                        conn.close(0u32.into(), b"stop");
                        endpoint.close().await;
                        return;
                    }
                    match tokio::time::timeout(Duration::from_millis(1), conn.read_datagram()).await {
                        Ok(Ok(bytes)) => {
                            if let Ok(d) = decode(&bytes, None) {
                                if let Packet::Input(f) = d.packet {
                                    if in_seq.accept(d.header.session_id, d.header.seq) {
                                        crate::set_latest_input(uid, f);
                                        last_rx = Some(Instant::now());
                                        pps.rx += 1;
                                    }
                                }
                            }
                        }
                        Ok(Err(_)) => break, // connection closed → back to accept
                        Err(_) => {}          // read timeout
                    }

                    let fresh = last_rx.map(|t| t.elapsed() < stale).unwrap_or(false);
                    if fresh && Instant::now() >= next_fb {
                        next_fb += fb_period;
                        if next_fb < Instant::now() { next_fb = Instant::now() + fb_period; }
                        if let Some((frame, _)) = crate::latest_feedback_frame(uid) {
                            let pkt = protocol::encode_feedback(&frame, session_id, fb_seq, None);
                            fb_seq = fb_seq.wrapping_add(1);
                            if fb_seq == 0 { break; }
                            if conn.send_datagram(pkt.into()).is_ok() { pps.tx += 1; }
                        }
                    }

                    if last_status.elapsed() >= STATUS_EVERY {
                        last_status = Instant::now();
                        pps.tick();
                        crate::set_status(uid, NetStatus {
                            state: if fresh { LinkState::Connected } else { LinkState::Listening },
                            code: Some(code.clone()),
                            tx_pps: pps.tx_out, rx_pps: pps.rx_out,
                            remote: Some(remote.clone()),
                            ..Default::default()
                        });
                    }
                }
            }
        });
    }

    use rand::Rng as _;
}
