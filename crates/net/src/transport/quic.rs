//! QUIC transport tier (feature `quic`): the same PSK-sealed packets carried in
//! quinn unreliable datagrams. The self-signed cert is deliberately
//! unauthenticated — security rests entirely on the inner ChaCha20-Poly1305
//! AEAD, exactly like the PSK/UDP tier.
//!
//! When the `quic` feature is disabled this compiles to a stub that surfaces a
//! clear status error instead of pulling the quinn/tokio dependency tree.

use std::sync::atomic::AtomicBool;

#[cfg(not(feature = "quic"))]
mod stub {
    use super::*;
    use crate::{LinkState, NetStatus};

    fn unavailable(uid: usize) {
        crate::set_status(
            uid,
            NetStatus {
                state: LinkState::Error("QUIC support not built".into()),
                ..Default::default()
            },
        );
    }

    pub fn run_send(
        _stop: &AtomicBool,
        uid: usize,
        _host: String,
        _port: u16,
        _rate_hz: u32,
        _psk: &str,
    ) {
        unavailable(uid);
    }

    pub fn run_recv(
        _stop: &AtomicBool,
        uid: usize,
        _bind_port: u16,
        _stale_ms: u32,
        _fb_rate_hz: u32,
        _psk: &str,
    ) {
        unavailable(uid);
    }
}

#[cfg(not(feature = "quic"))]
pub use stub::{run_recv, run_send};

#[cfg(feature = "quic")]
pub use imp::{run_recv, run_send};

#[cfg(feature = "quic")]
mod imp {
    use super::*;
    use std::net::{SocketAddr, ToSocketAddrs};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::crypto::Cipher;
    use crate::protocol::{self, decode, Packet, SeqTracker};
    use crate::{LinkState, NetStatus};

    const ALPN: &[u8] = b"flexinput-net";
    const RETRY: Duration = Duration::from_secs(2);

    /// QUIC uses PSK-sealed packets only when a passphrase is set; otherwise the
    /// TLS tunnel alone protects the plaintext-encoded packets. `decode` enforces
    /// the matching expectation on the other end.
    fn cipher_for(psk: &str) -> Option<Cipher> {
        (!psk.is_empty()).then(|| Cipher::from_passphrase(psk))
    }

    fn set_err(uid: usize, msg: impl Into<String>) {
        crate::set_status(uid, NetStatus { state: LinkState::Error(msg.into()), ..Default::default() });
    }

    fn set_link(uid: usize, connected: bool, remote: Option<SocketAddr>, tx: u32, rx: u32) {
        crate::set_status(
            uid,
            NetStatus {
                state: if connected { LinkState::Connected } else { LinkState::Listening },
                last_rx_ms: None,
                tx_pps: tx,
                rx_pps: rx,
                drops: 0,
                layout_warn: false,
                remote: remote.map(|a| a.to_string()),
            },
        );
    }

    // ── TLS: a self-signed server cert + a client that skips verification.
    //    Security rests on the inner PSK AEAD (when set) + the point-to-point
    //    nature of the link, exactly as documented at the top of this file. ────

    #[derive(Debug)]
    struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end: &rustls::pki_types::CertificateDer<'_>,
            _inter: &[rustls::pki_types::CertificateDer<'_>],
            _server: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            m: &[u8],
            c: &rustls::pki_types::CertificateDer<'_>,
            d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &rustls::pki_types::CertificateDer<'_>,
            d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    fn client_config() -> Result<quinn::ClientConfig, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth();
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let qc = quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(|e| e.to_string())?;
        Ok(quinn::ClientConfig::new(Arc::new(qc)))
    }

    fn server_config() -> Result<quinn::ServerConfig, String> {
        let cert = rcgen::generate_simple_self_signed(vec!["flexinput".to_string()])
            .map_err(|e| e.to_string())?;
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert);
        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der.into())
            .map_err(|e| e.to_string())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let qc = quinn::crypto::rustls::QuicServerConfig::try_from(tls).map_err(|e| e.to_string())?;
        Ok(quinn::ServerConfig::with_crypto(Arc::new(qc)))
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread tokio runtime for quic worker")
    }

    pub fn run_send(stop: &AtomicBool, uid: usize, host: String, port: u16, rate_hz: u32, psk: &str) {
        // rustls needs a process-wide default crypto provider for some paths;
        // installing ring here is idempotent (ignore "already set").
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cfg = match client_config() {
            Ok(c) => c,
            Err(e) => return set_err(uid, format!("tls config: {e}")),
        };
        let cipher = cipher_for(psk);
        let period = Duration::from_secs_f64(1.0 / rate_hz.clamp(1, 4000) as f64);
        let rt = runtime();

        rt.block_on(async move {
            while !stop.load(Ordering::Relaxed) {
                let addr = match (host.as_str(), port).to_socket_addrs().ok().and_then(|mut a| a.next()) {
                    Some(a) => a,
                    None => {
                        set_err(uid, format!("cannot resolve {host}"));
                        sleep_stop(stop, RETRY).await;
                        continue;
                    }
                };
                let mut endpoint = match quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()) {
                    Ok(e) => e,
                    Err(e) => {
                        set_err(uid, format!("endpoint: {e}"));
                        sleep_stop(stop, RETRY).await;
                        continue;
                    }
                };
                endpoint.set_default_client_config(cfg.clone());
                let conn = match endpoint.connect(addr, "flexinput").map(|c| c) {
                    Ok(connecting) => match connecting.await {
                        Ok(c) => c,
                        Err(e) => {
                            set_err(uid, format!("connect: {e}"));
                            sleep_stop(stop, RETRY).await;
                            continue;
                        }
                    },
                    Err(e) => {
                        set_err(uid, format!("connect: {e}"));
                        sleep_stop(stop, RETRY).await;
                        continue;
                    }
                };

                let session_id: u64 = rand::rng().random();
                let mut seq: u32 = 0;
                let mut fb_seq = SeqTracker::default();
                let remote = conn.remote_address();
                let mut tx_ct = 0u32;
                let mut rx_ct = 0u32;
                let mut window = Instant::now();
                let mut next = Instant::now();

                loop {
                    if stop.load(Ordering::Relaxed) {
                        conn.close(0u32.into(), b"stop");
                        return;
                    }
                    // Drain any inbound feedback datagrams without blocking the pace.
                    while let Ok(Some(bytes)) = try_recv_datagram(&conn).await {
                        if let Ok(d) = decode(&bytes, cipher.as_ref()) {
                            if let Packet::Feedback(f) = d.packet {
                                if fb_seq.accept(d.header.session_id, d.header.seq) {
                                    crate::set_latest_feedback(uid, f);
                                    rx_ct += 1;
                                }
                            }
                        }
                    }

                    if let Some((frame, _)) = crate::latest_send_frame(uid) {
                        let pkt = protocol::encode_input(&frame, session_id, seq, cipher.as_ref());
                        seq = seq.wrapping_add(1);
                        if seq == 0 {
                            break; // roll a fresh session on nonce exhaustion
                        }
                        if conn.send_datagram(pkt.into()).is_ok() {
                            tx_ct += 1;
                        } else {
                            break; // connection lost → reconnect
                        }
                    }

                    if window.elapsed() >= Duration::from_secs(1) {
                        set_link(uid, true, Some(remote), tx_ct, rx_ct);
                        tx_ct = 0;
                        rx_ct = 0;
                        window = Instant::now();
                    }

                    next += period;
                    let now = Instant::now();
                    if next > now {
                        tokio::time::sleep(next - now).await;
                    } else {
                        next = now;
                    }
                }
            }
        });
    }

    pub fn run_recv(stop: &AtomicBool, uid: usize, bind_port: u16, stale_ms: u32, fb_rate_hz: u32, psk: &str) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cfg = match server_config() {
            Ok(c) => c,
            Err(e) => return set_err(uid, format!("tls config: {e}")),
        };
        let cipher = Arc::new(cipher_for(psk));
        let stale = Duration::from_millis(stale_ms.max(50) as u64);
        let fb_period = Duration::from_secs_f64(1.0 / fb_rate_hz.clamp(1, 2000) as f64);
        let rt = runtime();

        rt.block_on(async move {
            let bind: SocketAddr = ([0, 0, 0, 0], bind_port).into();
            let endpoint = match quinn::Endpoint::server(cfg, bind) {
                Ok(e) => e,
                Err(e) => return set_err(uid, format!("bind :{bind_port} failed: {e}")),
            };
            set_link(uid, false, None, 0, 0);

            loop {
                if stop.load(Ordering::Relaxed) {
                    endpoint.close(0u32.into(), b"stop");
                    return;
                }
                let incoming = match tokio::time::timeout(Duration::from_millis(200), endpoint.accept()).await {
                    Ok(Some(i)) => i,
                    Ok(None) => return, // endpoint closed
                    Err(_) => continue, // timeout → re-check stop
                };
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let remote = conn.remote_address();
                let cipher = cipher.clone();
                let mut in_seq = SeqTracker::default();
                let mut fb_seq: u32 = 0;
                let session_id: u64 = rand::rng().random();
                let mut last_rx = Instant::now();
                let mut have_rx = false;
                let mut next_fb = Instant::now();
                let mut tx_ct = 0u32;
                let mut rx_ct = 0u32;
                let mut window = Instant::now();

                loop {
                    if stop.load(Ordering::Relaxed) {
                        conn.close(0u32.into(), b"stop");
                        endpoint.close(0u32.into(), b"stop");
                        return;
                    }
                    // Receive input datagrams with a short timeout so the feedback
                    // pacing + stop check still run when the peer goes quiet.
                    match tokio::time::timeout(Duration::from_millis(1), conn.read_datagram()).await {
                        Ok(Ok(bytes)) => {
                            if let Ok(d) = decode(&bytes, cipher.as_ref().as_ref()) {
                                if let Packet::Input(f) = d.packet {
                                    if in_seq.accept(d.header.session_id, d.header.seq) {
                                        crate::set_latest_input(uid, f);
                                        last_rx = Instant::now();
                                        have_rx = true;
                                        rx_ct += 1;
                                    }
                                }
                            }
                        }
                        Ok(Err(_)) => break, // connection closed → back to accept()
                        Err(_) => {}          // read timeout
                    }

                    let fresh = have_rx && last_rx.elapsed() < stale;
                    if fresh && Instant::now() >= next_fb {
                        next_fb += fb_period;
                        if next_fb < Instant::now() {
                            next_fb = Instant::now() + fb_period;
                        }
                        if let Some((frame, _)) = crate::latest_feedback_frame(uid) {
                            let pkt = protocol::encode_feedback(&frame, session_id, fb_seq, cipher.as_ref().as_ref());
                            fb_seq = fb_seq.wrapping_add(1);
                            if fb_seq == 0 {
                                break;
                            }
                            if conn.send_datagram(pkt.into()).is_ok() {
                                tx_ct += 1;
                            }
                        }
                    }

                    if window.elapsed() >= Duration::from_secs(1) {
                        set_link(uid, fresh, Some(remote), tx_ct, rx_ct);
                        tx_ct = 0;
                        rx_ct = 0;
                        window = Instant::now();
                    }
                }
            }
        });
    }

    /// Non-blocking-ish datagram read: returns Ok(None) when none is ready.
    async fn try_recv_datagram(conn: &quinn::Connection) -> Result<Option<bytes::Bytes>, ()> {
        match tokio::time::timeout(Duration::from_micros(50), conn.read_datagram()).await {
            Ok(Ok(b)) => Ok(Some(b)),
            Ok(Err(_)) => Err(()),
            Err(_) => Ok(None),
        }
    }

    async fn sleep_stop(stop: &AtomicBool, total: Duration) {
        let deadline = Instant::now() + total;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    use rand::Rng as _;
}

#[cfg(all(test, feature = "quic"))]
mod tests {
    use crate::{BusFrame, FeedbackFrame, NetManager, NetNodeConfig, Transport};
    use flexinput_core::signal::Signal;
    use std::time::{Duration, Instant};

    fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(v) = f() {
                return Some(v);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    // Full QUIC loopback: handshake, encrypted input datagrams, and the feedback
    // return leg — all over a real quinn endpoint pair on 127.0.0.1.
    #[test]
    fn quic_loopback_bidirectional() {
        let (send_uid, recv_uid, port) = (0xE2E_00101, 0xE2E_00102, 47821u16);
        let mut mgr = NetManager::new();
        mgr.reconcile(&[
            (
                recv_uid,
                NetNodeConfig::Recv {
                    transport: Transport::Quic,
                    bind_port: port,
                    stale_ms: 1000,
                    fb_rate_hz: 200,
                    psk: "quic-secret".to_string(),
                },
            ),
            (
                send_uid,
                NetNodeConfig::Send {
                    transport: Transport::Quic,
                    host: "127.0.0.1".to_string(),
                    port,
                    rate_hz: 500,
                    psk: "quic-secret".to_string(),
                },
            ),
        ]);

        let mut tx = BusFrame::empty();
        tx.set("right_stick", Signal::Vec2(glam::Vec2::new(-0.5, 0.5)));
        tx.set("btn_west", Signal::Bool(true));
        crate::publish_send_frame(send_uid, tx);

        let got = wait_for(Duration::from_secs(8), || {
            crate::latest_input(recv_uid).map(|(f, _)| f)
        });
        let got = got.expect("recv never got a QUIC input frame");
        let li = crate::frame::input_layout();
        assert_eq!(got.get_idx(li.pin_index("btn_west").unwrap()), Some(Signal::Bool(true)));

        let mut fb = FeedbackFrame::empty();
        fb.set("rumble_weak", 0.6);
        crate::publish_feedback_frame(recv_uid, fb);

        let got_fb = wait_for(Duration::from_secs(8), || {
            crate::latest_feedback(send_uid).map(|(f, _)| f)
        });
        let got_fb = got_fb.expect("send never got a QUIC feedback frame");
        let present: std::collections::HashMap<&str, f32> = got_fb.iter_present().collect();
        assert_eq!(present.get("rumble_weak").copied(), Some(0.6));

        drop(mgr);
    }
}
