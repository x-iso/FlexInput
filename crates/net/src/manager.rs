//! Lifecycle manager for per-node network workers.
//!
//! Mirrors `flexinput_devices::loopback_manager`: the socket-owning worker
//! threads are long-lived and must NOT be created/destroyed by the engine's
//! stateless per-tick eval. Instead the processing thread owns one
//! [`NetManager`] and calls [`NetManager::reconcile`] once per wakeup with the
//! current set of network nodes (keyed by effective uid). The manager spawns /
//! replaces / drops workers as configs change and prunes the global slots.
//!
//! Reconcile takes only primitives ([`NetNodeConfig`]), so this crate stays a
//! dependency leaf — the engine extracts the config list from its graph
//! snapshot and hands it over.

use std::collections::{HashMap, HashSet};

use crate::transport::{udp, Worker};

/// Which transport tier a node is configured for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Plaintext UDP (LAN, or WAN with a static/forwarded endpoint).
    Udp,
    /// ChaCha20-Poly1305 over UDP, keyed by a shared passphrase.
    Psk,
    /// QUIC unreliable datagrams, PSK-authenticated payloads.
    Quic,
}

impl Transport {
    pub fn from_str(s: &str) -> Self {
        match s {
            "psk" => Transport::Psk,
            "quic" => Transport::Quic,
            _ => Transport::Udp,
        }
    }

    fn encrypted(self) -> bool {
        matches!(self, Transport::Psk | Transport::Quic)
    }
}

/// A network node's full runtime configuration, extracted from its params.
/// `PartialEq` drives reconcile: an unchanged config leaves the worker running,
/// so the UI's every-frame graph republish never flaps sockets.
#[derive(Clone, Debug, PartialEq)]
pub enum NetNodeConfig {
    Send {
        transport: Transport,
        host: String,
        port: u16,
        rate_hz: u32,
        psk: String,
    },
    Recv {
        transport: Transport,
        bind_port: u16,
        stale_ms: u32,
        fb_rate_hz: u32,
        psk: String,
    },
}

struct Entry {
    config: NetNodeConfig,
    _worker: Worker,
}

#[derive(Default)]
pub struct NetManager {
    entries: HashMap<usize, Entry>,
}

impl NetManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile live workers against `configs` (the current network nodes,
    /// keyed by effective uid). Call once per proc-thread wakeup.
    pub fn reconcile(&mut self, configs: &[(usize, NetNodeConfig)]) {
        let live: HashSet<usize> = configs.iter().map(|(uid, _)| *uid).collect();

        // Drop workers whose node vanished or whose config changed; the Worker
        // Drop impl stops + joins the thread and releases the socket.
        self.entries
            .retain(|uid, e| live.contains(uid) && configs.iter().any(|(u, c)| u == uid && c == &e.config));

        for (uid, config) in configs {
            if self.entries.contains_key(uid) {
                continue; // unchanged — leave the running worker alone
            }
            let worker = spawn_worker(*uid, config.clone());
            self.entries.insert(*uid, Entry { config: config.clone(), _worker: worker });
        }

        // Clear published slots for nodes no longer present.
        crate::retain_all(&live);
    }
}

fn spawn_worker(uid: usize, config: NetNodeConfig) -> Worker {
    match config {
        NetNodeConfig::Send { transport, host, port, rate_hz, psk } => {
            let name = format!("net-send-{uid}");
            match transport {
                Transport::Quic => Worker::spawn(name, move |stop| {
                    crate::transport::quic::run_send(&stop, uid, host, port, rate_hz, &psk);
                }),
                _ => Worker::spawn(name, move |stop| {
                    udp::run_send(&stop, uid, host, port, rate_hz, transport.encrypted(), psk);
                }),
            }
        }
        NetNodeConfig::Recv { transport, bind_port, stale_ms, fb_rate_hz, psk } => {
            let name = format!("net-recv-{uid}");
            match transport {
                Transport::Quic => Worker::spawn(name, move |stop| {
                    crate::transport::quic::run_recv(&stop, uid, bind_port, stale_ms, fb_rate_hz, &psk);
                }),
                _ => Worker::spawn(name, move |stop| {
                    udp::run_recv(&stop, uid, bind_port, stale_ms, fb_rate_hz, transport.encrypted(), psk);
                }),
            }
        }
    }
}
