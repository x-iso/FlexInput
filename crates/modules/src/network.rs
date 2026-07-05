use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![reg::<NetworkSendModule>(), reg::<NetworkRecvModule>()]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Network Send ──────────────────────────────────────────────────────────────

/// Transmits the AutoMap bus to a peer FlexInput instance over the network and
/// injects feedback (rumble/lightbar) received from that peer back into the
/// upstream physical device — the "pad PC" end of a network link.
///
/// The AutoMap output is a local passthrough (like Audio Stream Haptics), so
/// one pad can drive a local sink and the network simultaneously.
///
/// Transport tiers (param `net_transport`): "udp" (plaintext LAN), "psk"
/// (ChaCha20-Poly1305 over UDP, WAN-safe with a shared passphrase), "quic"
/// (QUIC unreliable datagrams, PSK-authenticated). Sockets live in
/// `flexinput-net`'s NetManager, reconciled by the proc thread; the eval block
/// only moves frames through the crate's global slots.
#[derive(Default)]
pub struct NetworkSendModule;

impl Module for NetworkSendModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.network_send",
            display_name: "Network Send",
            category: "Network",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Behavior lives in the engine eval block (net_send_publish).
        SmallVec::new()
    }
}

// ── Network Receive ───────────────────────────────────────────────────────────

/// Publishes a peer's AutoMap bus received over the network — the "game PC"
/// end of a network link. Wire its AutoMap output into a virtual device sink
/// (or any AutoMap consumer) exactly like a local gamepad source. Feedback the
/// game requests on downstream virtual sinks is gathered and sent back to the
/// peer automatically.
///
/// Safety: when no valid packet has arrived within `net_stale_ms`, the node
/// publishes a full neutral frame (sticks centered, buttons released) so a
/// dead link can never leave inputs stuck.
#[derive(Default)]
pub struct NetworkRecvModule;

impl Module for NetworkRecvModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.network_recv",
            display_name: "Network Receive",
            category: "Network",
            inputs: vec![],
            outputs: vec![
                PinDescriptor::new("AutoMap", SignalType::AutoMap),
                PinDescriptor::new("Connected", SignalType::Bool),
            ],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Behavior lives in the engine eval block (net_recv_publish).
        SmallVec::new()
    }
}
