# FlexInput Network Reference

## Overview

FlexInput's network subsystem enables bidirectional gamepad state sharing between two FlexInput instances over a LAN, internet (with encryption), or NAT traversal (P2P). It carries the complete AutoMap bus including all canonical pins and haptic feedback signals.

**Key Characteristics:**
- Three transport tiers: LAN (raw UDP), PSK (encrypted UDP), P2P (iroh QUIC)
- Bidirectional haptics - rumble, lightbar, adaptive triggers travel back
- Fail-safe neutral frame on connection loss
- Configurable staleness window
- Integrated with AutoMap system via Network Send/Receive modules

> ⚠️ **Accuracy caveat.** This document is the most aspirational of the set: several
> code blocks below (frame structs like `UdpFrame`/`PskFrame`/`CompactFrame`,
> `encode_signal`, `ConnectionState`, the P2P `generate_code`/`dial` snippet) are
> ILLUSTRATIVE pseudo-code, not real APIs, and the "Future Enhancements" section is
> unbuilt. Ground truth for the real API:
> - `crates/net/src/`: `frame.rs`, `crypto.rs` (ChaCha20-Poly1305), `protocol.rs`,
>   `manager.rs`, `transport/{mod,udp,p2p}.rs`. There is **no `transport/psk.rs`** —
>   PSK is a mode of the UDP transport plus `crypto.rs`, not a separate file.
> - The transport enum is `Transport { Lan, Psk, P2p }` (`manager.rs`); the string
>   `"quic"` is a back-compat alias for `P2p` (raw-QUIC was replaced by the iroh tier).
> - P2P uses **iroh**: you dial a peer by its `EndpointId` (the peer's public key IS the
>   address); iroh's own TLS provides auth+encryption, with relay fallback for NAT
>   traversal. It is gated behind the `p2p` cargo feature.
> - Engine publish hooks: `net_send_publish` / `net_recv_publish` in `eval/publish.rs`.
> - `module.network_send` passes the bus through; `module.network_recv` injects received
>   signals into `collector_sigs` under a synthetic device id.

---

## Transport Tiers

### 1. LAN Transport (`transport/udp.rs`)

**Protocol:** Raw UDP, no encryption

**Use Case:** Same local network, trusted environment

**Frame Format:**
```rust
pub struct UdpFrame {
    pub timestamp: f64,                    // Server time in seconds (double)
    pub device_id: String,                 // Sender's physical device ID
    pub signals: HashMap<String, Signal>,  // All AutoMap pins
    pub haptics: HapticFeedback,           // Rumble/lightbar state
}
```

**Implementation:**
- `UdpTransport` struct manages socket lifecycle
- Non-blocking send/recv with configurable buffer sizes
- Automatic reconnection on timeout

### 2. PSK Transport (`transport/psk.rs`)

**Protocol:** UDP + ChaCha20-Poly1305 encryption

**Use Case:** Internet, untrusted networks

**Key Derivation:**
```rust
// From user-provided passphrase:
let key = derive_key(passphrase.as_bytes(), salt);  // PBKDF2 or similar
```

**Frame Format:**
```rust
pub struct PskFrame {
    pub nonce: [u8; 12],           // ChaCha20 nonce (12 bytes)
    pub ciphertext: Vec<u8>,       // Encrypted UdpFrame
    pub tag: [u8; 16],             // Poly1305 authentication tag
}
```

**Security Properties:**
- Confidentiality via ChaCha20 stream cipher
- Integrity via Poly1305 MAC
- Replay protection via monotonic nonce counter

### 3. P2P Transport (`transport/p2p.rs`)

**Protocol:** iroh network (libp2p-based)

**Use Case:** NAT traversal, CGNAT environments, VPNs

**Connection Establishment (iroh):** a peer is addressed by its `EndpointId` — the
public key IS the address. The receiver publishes its EndpointId; the sender dials it.
iroh handles NAT hole-punching with relay fallback and provides TLS auth/encryption
keyed by the endpoint identity (so P2P frames need no separate PSK layer). See
`crates/net/src/transport/p2p.rs` for the real `Endpoint`/`EndpointId` usage (illustrative
`generate_code`/`dial` calls are not the actual API).

**Frame Format:**
Same as LAN transport but wrapped in iroh's reliable delivery layer.

---

## Network Module Implementation

### Network Send (`module.network_send`)

**Purpose:** Transmit local AutoMap bus to remote instance

**Inputs:**
- Input 0: Source AutoMap bus (AutoMap type)

**Outputs:**
- Output 0: Passthrough AutoMap bus (unmodified)

**Parameters:**
```rust
pub struct NetworkSendParams {
    pub target_ip: String,           // Remote IP address
    pub target_port: u16,            // Remote UDP port
    pub transport: TransportType,    // "lan", "psk", or "p2p"
    pub passphrase: Option<String>,  // PSK encryption key
    pub peer_code: Option<String>,   // P2P connection code
}

// Real enum name is `Transport` (crates/net/src/manager.rs), not `TransportType`:
pub enum Transport {
    Lan,   // raw UDP
    Psk,   // UDP + ChaCha20-Poly1305 (crypto.rs)
    P2p,   // iroh QUIC (feature `p2p`); "quic" is a back-compat string alias
}
```

**Evaluation Flow:**
1. Read AutoMap bus from input (via `_automap_device_id` param)
2. Serialize signals into `NetworkFrame`
3. Encrypt if PSK transport
4. Send via UDP socket or iroh relay
5. Pass through original bus to output[0]

**Code Location:** `crates/modules/src/network.rs`, `network_send_publish()` in `eval/publish.rs`

### Network Receive (`module.network_recv`)

**Purpose:** Inject remote AutoMap bus into local graph

**Inputs:** None (listens on configured port)

**Outputs:** Signals injected into `collector_sigs` map under synthetic device ID

**Parameters:**
```rust
pub struct NetworkRecvParams {
    pub listen_port: u16,            // Local UDP port to bind
    pub transport: TransportType,    // Must match sender
    pub passphrase: Option<String>,  // PSK decryption key
    pub peer_code: Option<String>,   // P2P inbound code
}
```

**Evaluation Flow:**
1. Bind to `listen_port` on first evaluation
2. Receive frames in post-evaluation step (not during tick)
3. Decrypt if PSK transport
4. Inject signals into `collector_sigs[(recv_device_id, pin)]`
5. Publish feedback signals from local virtual devices

**Code Location:** `crates/modules/src/network.rs`, `net_recv_publish()` in `eval/publish.rs`

---

## Frame Serialization

### Signal Encoding

```rust
pub fn encode_signal(sig: Signal) -> Vec<u8> {
    match sig {
        Signal::Float(f) => {
            let mut buf = vec![0x01];  // Type tag
            buf.extend_from_slice(&f.to_le_bytes());
            buf
        }
        Signal::Bool(b) => vec![0x02, if b { 1 } else { 0 }],
        Signal::Vec2(v) => {
            let mut buf = vec![0x03];
            buf.extend_from_slice(&v.x.to_le_bytes());
            buf.extend_from_slice(&v.y.to_le_bytes());
            buf
        }
        // ... etc
    }
}

pub fn decode_signal(data: &[u8]) -> Option<Signal> {
    match data[0] {
        0x01 => Some(Signal::Float(f32::from_le_bytes([data[1..5].try_into().unwrap()]))),
        0x02 => Some(Signal::Bool(data[1] != 0)),
        // ... etc
    }
}
```

### Compact Format

For bandwidth efficiency, only non-default signals are transmitted:

```rust
pub struct CompactFrame {
    pub timestamp: f64,
    pub device_id: String,
    pub changes: Vec<(String, Signal)>,  // Only pins that changed since last frame
}
```

**Change Detection:**
- Compare current signal map against previous frame's map
- Serialize only delta entries
- Receiver applies deltas to local state

---

## Haptic Feedback Routing

### Bidirectional Flow

Feedback signals flow **backward** along the network wire:

```
Local Physical Pad ←── Network Receive ──→ Remote Virtual Pad
       ↑                                        ↓
       │          Feedback Signals              │
       └──────── Network Send ←────────────────┘
```

**Implementation:**
1. Local pad's haptic inputs (rumble, lightbar) are read by `NetworkSend`
2. Sent to remote instance as part of frame
3. Remote `NetworkReceive` injects into local virtual pad's feedback channel
4. Virtual pad applies rumble shaping and sends to hardware

### Feedback Pin Mapping

Uses same `FEEDBACK_PAIRS` logic as AutoMap:

```rust
// In network_recv_publish():
for (virt_out_pin, physical_pins) in FEEDBACK_PAIRS {
    if let Some(sig) = collector_sigs.get(&(recv_device_id, virt_out_pin)) {
        for &phys_pin in physical_pins {
            if let Some(dst_pin) = resolve_feedback_pin(virt_out_pin, &local_pins) {
                sink_outputs.insert((local_dev_id, dst_pin), *sig);
            }
        }
    }
}
```

---

## Connection Management

### Staleness Detection

Frames are considered stale if older than `staleness_window` (default 200 ms):

```rust
pub const DEFAULT_STALENESS_WINDOW: f64 = 0.2;  // 200 ms

fn is_stale(frame: &NetworkFrame, now: f64) -> bool {
    now - frame.timestamp > DEFAULT_STALENESS_WINDOW
}
```

**On Stale Frame:**
- Receiver emits neutral signals (zeros for analog, false for digital)
- Prevents "frozen" gamepad state if sender crashes

### Reconnection Logic

```rust
pub struct ConnectionState {
    pub connected: bool,
    pub last_success: Option<Instant>,
    pub consecutive_failures: u32,
    pub backoff_ms: u64,  // Exponential backoff
}

impl ConnectionState {
    pub fn on_send_success(&mut self) {
        self.connected = true;
        self.consecutive_failures = 0;
        self.backoff_ms = 100;  // Reset to minimum
    }
    
    pub fn on_send_failure(&mut self) {
        self.connected = false;
        self.consecutive_failures += 1;
        self.backoff_ms = (self.backoff_ms * 2).min(5000);  // Cap at 5s
    }
}
```

### Keep-Alive Messages

Periodic empty frames prevent NAT timeout:

```rust
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

// In network_send evaluation:
if now - last_keepalive > KEEPALIVE_INTERVAL {
    send_empty_frame();  // Just timestamp, no signals
    last_keepalive = now;
}
```

---

## Performance Considerations

### Bandwidth Optimization

**Typical frame size:** ~200-500 bytes (with change detection)

**Without optimization:** All 40+ pins × average 8 bytes = ~320-400 bytes per frame

**With change detection:** Only modified pins transmitted, typically 5-15% of total

### Latency Requirements

**Target end-to-end latency:** < 16 ms (one frame at 60 Hz)

**Bottlenecks:**
1. Network round-trip time (RTT)
2. Frame serialization/deserialization
3. Haptic feedback shaping delay

**Mitigation:**
- Use UDP (no TCP retransmission delays)
- Compact binary format (no JSON overhead)
- Async evaluation (send/recv don't block graph tick)

### Thread Safety

Network operations run on the **processing thread** to avoid UI blocking:

```rust
// In eval/publish.rs
pub fn net_send_publish(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) -> Vec<Option<Signal>> {
    // Serialize and send asynchronously
    let frame = build_frame(snap, dev_sigs);
    std::thread::spawn(move || {
        transport.send(&frame).unwrap_or_else(|e| eprintln!("Network send failed: {}", e));
    });
    
    vec![Some(Signal::AutoMap(bus))]  // Pass through
}
```

---

## Testing & Debugging

### Loopback Test (`tests/loopback.rs`)

Verifies send/receive cycle without network:

```rust
#[test]
fn test_loopback_frame_roundtrip() {
    let sender = NetworkSend::new("127.0.0.1", 12345, TransportType::Lan);
    let receiver = NetworkRecv::new(12346, TransportType::Lan);
    
    // Send a frame
    let signals = create_test_signals();
    sender.send(signals.clone());
    
    // Receive it
    let received = receiver.receive();
    assert_eq!(received.signals, signals);
}
```

### Wireshark Capture

For LAN/PSK transports, frames are raw UDP and can be captured with Wireshark:

1. Filter by port: `udp.port == 12345`
2. Decode as custom protocol (write Wireshark dissector)
3. Inspect signal values and timestamps

### Debug Logging

Enable verbose logging via environment variable:

```bash
FLEXINPUT_NET_DEBUG=1 cargo run
```

Logs include:
- Frame send/receive timestamps
- Encryption/decryption success/failure
- Connection state transitions
- Staleness warnings

---

## Configuration in Patches

### Network Send Node Setup

When user adds a `Network Send` module to canvas:

```rust
// In canvas/node.rs, when creating network_send node:
let params = HashMap::from([
    ("target_ip".to_string(), Value::String("192.168.1.100".into())),
    ("target_port".to_string(), Value::Number(12345.into())),
    ("transport".to_string(), Value::String("lan".into())),
]);

snarl.insert_node(pos, NodeData {
    module_id: "module.network_send".into(),
    params,
    // ...
});
```

### Auto-Configuration

The UI can auto-fill parameters based on context:
- If user is in P2P mode, prompt for code exchange
- If PSK selected, require passphrase input
- Validate IP address format on blur

---

## Security Considerations

### PSK Passphrase Storage

**Never store passphrases in patch files (.fxp).**

Passphrases are UI-only parameters:
```rust
// In NodeData serialization:
#[serde(skip)]
pub passphrase: Option<String>,  // Not persisted
```

Users must re-enter passphrase after patch reload.

### P2P Code Expiration

Connection codes should be time-limited:
```rust
pub const CODE_EXPIRY: Duration = Duration::from_secs(300);  // 5 minutes

// When generating code:
let code = iroh::Node::generate_code();
let expiry = Instant::now() + CODE_EXPIRY;
println!("Connect within {} seconds: {}", CODE_EXPIRY.as_secs(), code);
```

### Network Address Validation

Sanitize IP addresses to prevent SSRF:
```rust
fn validate_ip(ip: &str) -> Result<IpAddr, String> {
    let addr: Ipv4Addr = ip.parse().map_err(|e| format!("Invalid IP: {}", e))?;
    if addr.is_loopback() || addr.is_private() {
        Ok(IpAddr::V4(addr))
    } else {
        Err("Only localhost and private ranges allowed".into())
    }
}
```

---

## Future Enhancements

### Multi-Receiver Support

Allow one sender to broadcast to multiple receivers:
```rust
pub struct NetworkSendParams {
    pub targets: Vec<NetworkTarget>,  // Multiple destinations
}

pub struct NetworkTarget {
    pub ip: String,
    pub port: u16,
    pub transport: TransportType,
}
```

### Latency Compensation

Adjust for network delay in haptic feedback:
```rust
// Predictive rumble: send future frames based on game state
pub struct PredictiveHaptics {
    pub lookahead_ms: f64,  // How far ahead to predict
    pub game_state_estimator: Box<dyn Fn(&GameFrame) -> &HapticState>,
}
```

### Bandwidth Adaptation

Dynamically reduce frame rate on congested networks:
```rust
pub struct AdaptiveBandwidth {
    pub min_frame_interval_ms: u64,  // Maximum frame rate
    pub max_frame_interval_ms: u64,  // Minimum frame rate
    pub loss_threshold: f32,         // Packet loss % to trigger adaptation
}
```

---

## References

- Transport trait + tiers: `crates/net/src/transport/mod.rs`
- LAN + PSK (encrypted UDP): `crates/net/src/transport/udp.rs` + `crates/net/src/crypto.rs`
  (there is NO `transport/psk.rs`)
- P2P (iroh QUIC, feature `p2p`): `crates/net/src/transport/p2p.rs`
- Transport enum + staleness: `crates/net/src/manager.rs`, `crates/net/src/lib.rs`
- Network modules: `crates/modules/src/network.rs`
- Publish hooks: `crates/engine/src/eval/publish.rs` (`net_send_publish`, `net_recv_publish`)
- Frame format / protocol: `crates/net/src/frame.rs`, `crates/net/src/protocol.rs`
