//! Wire protocol: a 24-byte plaintext header followed by a payload that is
//! either plaintext (LAN tier) or ChaCha20-Poly1305 sealed with the header as
//! AAD (PSK / QUIC tiers).
//!
//! ```text
//! offset  field
//! 0       u32  magic          "FXIN"
//! 4       u8   version        1
//! 5       u8   flags          bit0: direction (0=input, 1=feedback)
//!                             bit1: payload encrypted
//! 6       u16  reserved       0
//! 8       u32  layout_hash    FNV-1a of the direction's pin-id table
//! 12      u64  session_id     random per sender-socket lifetime
//! 20      u32  seq            monotonic per direction
//! 24      …    payload        (+16B Poly1305 tag when encrypted)
//! ```
//!
//! Input payload (little-endian):
//! `u16 n_pins; u16 n_slots; u8 bitmap[⌈n_pins/8⌉]; f32 slots[n_slots];
//!  u8 n_extra; n_extra × { u8 name_len; name; u8 kind(0=f32,1=bool); f32 }`
//!
//! Feedback payload: `u16 n_pins; u8 bitmap[⌈n_pins/8⌉]; f32 vals[n_pins]`.
//!
//! Version tolerance: the receiver decodes `min(remote n_pins, local n_pins)`
//! using its own layout for the common prefix — `ALL_PINS` growth is
//! append-only by contract, and a true mid-list divergence is surfaced by the
//! layout-hash mismatch (`Decoded::layout_match == false`) while still
//! decoding the prefix.

use flexinput_core::signal::Signal;

use crate::crypto::{Cipher, TAG_LEN};
use crate::frame::{feedback_layout, input_layout, BusFrame, Extra, FeedbackFrame};

pub const MAGIC: &[u8; 4] = b"FXIN";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 24;

const FLAG_FEEDBACK: u8 = 1 << 0;
const FLAG_ENCRYPTED: u8 = 1 << 1;

/// Hard cap on datagram size we will build or accept. Generous vs the ~500 B
/// steady state; prevents a hostile length field from allocating gigabytes.
pub const MAX_PACKET: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Forward gamepad bus, pad PC → game PC.
    Input,
    /// Haptics/LED back-channel, game PC → pad PC.
    Feedback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub dir: Direction,
    pub encrypted: bool,
    pub layout_hash: u32,
    pub session_id: u64,
    pub seq: u32,
}

#[derive(Debug, PartialEq)]
pub enum Packet {
    Input(BusFrame),
    Feedback(FeedbackFrame),
}

#[derive(Debug, PartialEq)]
pub struct Decoded {
    pub header: Header,
    pub packet: Packet,
    /// False when the peer's pin table hash differs from ours (mixed
    /// versions). The common prefix was still decoded; surface as a warning.
    pub layout_match: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Not our protocol at all (bad magic / truncated header) — silently drop.
    NotFlexInput,
    /// Future major version we can't parse.
    UnknownVersion(u8),
    /// Payload shorter than its own length fields claim.
    Truncated,
    /// Length field exceeds MAX_PACKET bounds.
    Oversized,
    /// Packet is encrypted but we have no PSK configured.
    EncryptedWithoutKey,
    /// We require encryption (PSK configured) but the packet is plaintext.
    /// Rejected to prevent a downgrade path.
    PlaintextRejected,
    /// AEAD authentication failed (wrong PSK, tamper, or replayed nonce).
    AuthFailed,
}

fn write_header(buf: &mut Vec<u8>, h: &Header) {
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
    let mut flags = 0u8;
    if h.dir == Direction::Feedback {
        flags |= FLAG_FEEDBACK;
    }
    if h.encrypted {
        flags |= FLAG_ENCRYPTED;
    }
    buf.push(flags);
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&h.layout_hash.to_le_bytes());
    buf.extend_from_slice(&h.session_id.to_le_bytes());
    buf.extend_from_slice(&h.seq.to_le_bytes());
}

fn parse_header(b: &[u8]) -> Result<Header, DecodeError> {
    if b.len() < HEADER_LEN || &b[0..4] != MAGIC {
        return Err(DecodeError::NotFlexInput);
    }
    if b[4] != VERSION {
        return Err(DecodeError::UnknownVersion(b[4]));
    }
    let flags = b[5];
    Ok(Header {
        dir: if flags & FLAG_FEEDBACK != 0 { Direction::Feedback } else { Direction::Input },
        encrypted: flags & FLAG_ENCRYPTED != 0,
        layout_hash: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        session_id: u64::from_le_bytes(b[12..20].try_into().unwrap()),
        seq: u32::from_le_bytes(b[20..24].try_into().unwrap()),
    })
}

fn encode_input_payload(frame: &BusFrame) -> Vec<u8> {
    let layout = input_layout();
    let n_pins = layout.pins.len();
    let mut p = Vec::with_capacity(4 + n_pins.div_ceil(8) + layout.n_slots * 4 + 1);
    p.extend_from_slice(&(n_pins as u16).to_le_bytes());
    p.extend_from_slice(&(layout.n_slots as u16).to_le_bytes());
    let mut bitmap = vec![0u8; n_pins.div_ceil(8)];
    for (i, &present) in frame.present.iter().enumerate().take(n_pins) {
        if present {
            bitmap[i / 8] |= 1 << (i % 8);
        }
    }
    p.extend_from_slice(&bitmap);
    for &s in frame.slots.iter().take(layout.n_slots) {
        p.extend_from_slice(&s.to_le_bytes());
    }
    let n_extra = frame.extras.len().min(u8::MAX as usize);
    p.push(n_extra as u8);
    for e in frame.extras.iter().take(n_extra) {
        let name = e.name.as_bytes();
        let len = name.len().min(u8::MAX as usize);
        p.push(len as u8);
        p.extend_from_slice(&name[..len]);
        match e.value {
            Signal::Bool(b) => {
                p.push(1);
                p.extend_from_slice(&(if b { 1.0f32 } else { 0.0f32 }).to_le_bytes());
            }
            v => {
                p.push(0);
                p.extend_from_slice(&v.as_float().to_le_bytes());
            }
        }
    }
    p
}

fn decode_input_payload(p: &[u8]) -> Result<(BusFrame, bool), DecodeError> {
    let layout = input_layout();
    if p.len() < 4 {
        return Err(DecodeError::Truncated);
    }
    let remote_pins = u16::from_le_bytes(p[0..2].try_into().unwrap()) as usize;
    let remote_slots = u16::from_le_bytes(p[2..4].try_into().unwrap()) as usize;
    if remote_pins > 4096 || remote_slots > 8192 {
        return Err(DecodeError::Oversized);
    }
    let bitmap_len = remote_pins.div_ceil(8);
    let slots_start = 4 + bitmap_len;
    let extras_start = slots_start + remote_slots * 4;
    if p.len() < extras_start + 1 {
        return Err(DecodeError::Truncated);
    }
    let bitmap = &p[4..slots_start];
    let mut frame = BusFrame::empty();

    // Common-prefix decode: pin i means the same thing on both peers for the
    // shared prefix (append-only table contract), so our local offsets apply
    // as long as the remote actually shipped those slots.
    let n = remote_pins.min(layout.pins.len());
    for (i, slot) in layout.pins.iter().enumerate().take(n) {
        if bitmap[i / 8] & (1 << (i % 8)) == 0 {
            continue;
        }
        if slot.offset + slot.width > remote_slots {
            break; // remote is an older build with fewer slots
        }
        frame.present[i] = true;
        for w in 0..slot.width {
            let at = slots_start + (slot.offset + w) * 4;
            frame.slots[slot.offset + w] = f32::from_le_bytes(p[at..at + 4].try_into().unwrap());
        }
    }

    let n_extra = p[extras_start] as usize;
    let mut at = extras_start + 1;
    for _ in 0..n_extra {
        if p.len() < at + 1 {
            return Err(DecodeError::Truncated);
        }
        let name_len = p[at] as usize;
        at += 1;
        if p.len() < at + name_len + 5 {
            return Err(DecodeError::Truncated);
        }
        let name = String::from_utf8_lossy(&p[at..at + name_len]).into_owned();
        at += name_len;
        let kind = p[at];
        at += 1;
        let v = f32::from_le_bytes(p[at..at + 4].try_into().unwrap());
        at += 4;
        let value = if kind == 1 { Signal::Bool(v >= 0.5) } else { Signal::Float(v) };
        frame.extras.push(Extra { name, value });
    }

    Ok((frame, remote_pins == layout.pins.len()))
}

fn encode_feedback_payload(frame: &FeedbackFrame) -> Vec<u8> {
    let layout = feedback_layout();
    let n_pins = layout.pins.len();
    let mut p = Vec::with_capacity(2 + n_pins.div_ceil(8) + n_pins * 4);
    p.extend_from_slice(&(n_pins as u16).to_le_bytes());
    let mut bitmap = vec![0u8; n_pins.div_ceil(8)];
    for (i, &present) in frame.present.iter().enumerate().take(n_pins) {
        if present {
            bitmap[i / 8] |= 1 << (i % 8);
        }
    }
    p.extend_from_slice(&bitmap);
    for &v in frame.vals.iter().take(n_pins) {
        p.extend_from_slice(&v.to_le_bytes());
    }
    p
}

fn decode_feedback_payload(p: &[u8]) -> Result<(FeedbackFrame, bool), DecodeError> {
    let layout = feedback_layout();
    if p.len() < 2 {
        return Err(DecodeError::Truncated);
    }
    let remote_pins = u16::from_le_bytes(p[0..2].try_into().unwrap()) as usize;
    if remote_pins > 4096 {
        return Err(DecodeError::Oversized);
    }
    let bitmap_len = remote_pins.div_ceil(8);
    let vals_start = 2 + bitmap_len;
    if p.len() < vals_start + remote_pins * 4 {
        return Err(DecodeError::Truncated);
    }
    let bitmap = &p[2..vals_start];
    let mut frame = FeedbackFrame::empty();
    let n = remote_pins.min(layout.pins.len());
    for i in 0..n {
        if bitmap[i / 8] & (1 << (i % 8)) == 0 {
            continue;
        }
        let at = vals_start + i * 4;
        frame.present[i] = true;
        frame.vals[i] = f32::from_le_bytes(p[at..at + 4].try_into().unwrap());
    }
    Ok((frame, remote_pins == layout.pins.len()))
}

/// Build one input-direction datagram.
pub fn encode_input(
    frame: &BusFrame,
    session_id: u64,
    seq: u32,
    cipher: Option<&Cipher>,
) -> Vec<u8> {
    encode(Direction::Input, encode_input_payload(frame), session_id, seq, cipher)
}

/// Build one feedback-direction datagram.
pub fn encode_feedback(
    frame: &FeedbackFrame,
    session_id: u64,
    seq: u32,
    cipher: Option<&Cipher>,
) -> Vec<u8> {
    encode(Direction::Feedback, encode_feedback_payload(frame), session_id, seq, cipher)
}

fn encode(
    dir: Direction,
    payload: Vec<u8>,
    session_id: u64,
    seq: u32,
    cipher: Option<&Cipher>,
) -> Vec<u8> {
    let layout_hash = match dir {
        Direction::Input => input_layout().layout_hash,
        Direction::Feedback => feedback_layout().layout_hash,
    };
    let header = Header { dir, encrypted: cipher.is_some(), layout_hash, session_id, seq };
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len() + TAG_LEN);
    write_header(&mut buf, &header);
    match cipher {
        Some(c) => {
            let aad = buf[..HEADER_LEN].to_vec();
            let sealed = c.seal(dir, session_id, seq, &aad, &payload);
            buf.extend_from_slice(&sealed);
        }
        None => buf.extend_from_slice(&payload),
    }
    buf
}

/// Parse + (when `cipher` is set) authenticate one datagram.
///
/// Security invariant: when a PSK is configured, plaintext packets are
/// REJECTED — otherwise an attacker could bypass the AEAD entirely by just
/// clearing the encrypted flag.
pub fn decode(bytes: &[u8], cipher: Option<&Cipher>) -> Result<Decoded, DecodeError> {
    if bytes.len() > MAX_PACKET {
        return Err(DecodeError::Oversized);
    }
    let header = parse_header(bytes)?;
    let body = &bytes[HEADER_LEN..];
    let plain: Vec<u8> = match (header.encrypted, cipher) {
        (true, Some(c)) => c
            .open(header.dir, header.session_id, header.seq, &bytes[..HEADER_LEN], body)
            .ok_or(DecodeError::AuthFailed)?,
        (true, None) => return Err(DecodeError::EncryptedWithoutKey),
        (false, Some(_)) => return Err(DecodeError::PlaintextRejected),
        (false, None) => body.to_vec(),
    };
    let (packet, layout_match) = match header.dir {
        Direction::Input => {
            let (f, m) = decode_input_payload(&plain)?;
            (Packet::Input(f), m && header.layout_hash == input_layout().layout_hash)
        }
        Direction::Feedback => {
            let (f, m) = decode_feedback_payload(&plain)?;
            (Packet::Feedback(f), m && header.layout_hash == feedback_layout().layout_hash)
        }
    };
    Ok(Decoded { header, packet, layout_match })
}

/// Per-peer, per-direction replay/reorder gate. UDP may duplicate or reorder;
/// we keep only forward progress within a session and adopt any new session
/// (peer restart) unconditionally.
#[derive(Default, Debug)]
pub struct SeqTracker {
    state: Option<(u64, u32)>,
}

impl SeqTracker {
    /// True if the packet should be applied; updates internal state.
    pub fn accept(&mut self, session_id: u64, seq: u32) -> bool {
        match self.state {
            None => {
                self.state = Some((session_id, seq));
                true
            }
            Some((sess, _)) if sess != session_id => {
                self.state = Some((session_id, seq));
                true
            }
            Some((_, last)) => {
                let delta = seq.wrapping_sub(last) as i32;
                if delta > 0 {
                    self.state = Some((session_id, seq));
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flexinput_core::signal::Signal;

    fn sample_frame() -> BusFrame {
        let mut fr = BusFrame::empty();
        fr.set("left_stick", Signal::Vec2(glam::Vec2::new(-0.25, 0.75)));
        fr.set("right_trigger", Signal::Float(1.0));
        fr.set("btn_south", Signal::Bool(true));
        fr.set("btn_east", Signal::Bool(false)); // present-but-false is meaningful
        fr.set("macro_key_7", Signal::Bool(true)); // extra
        fr.set("blend_amt", Signal::Float(0.5)); // extra
        fr
    }

    #[test]
    fn plaintext_input_roundtrip() {
        let fr = sample_frame();
        let pkt = encode_input(&fr, 0xABCD, 5, None);
        let d = decode(&pkt, None).unwrap();
        assert!(d.layout_match);
        assert_eq!(d.header.dir, Direction::Input);
        assert_eq!(d.header.session_id, 0xABCD);
        assert_eq!(d.header.seq, 5);
        assert_eq!(d.packet, Packet::Input(fr));
    }

    #[test]
    fn encrypted_input_roundtrip() {
        let c = Cipher::from_passphrase("secret");
        let fr = sample_frame();
        let pkt = encode_input(&fr, 1, 2, Some(&c));
        let d = decode(&pkt, Some(&c)).unwrap();
        assert_eq!(d.packet, Packet::Input(fr));
    }

    #[test]
    fn feedback_roundtrip() {
        let mut fb = FeedbackFrame::empty();
        fb.set("rumble_strong", 0.8);
        fb.set("rumble_weak", 0.0); // active zero
        fb.set("lightbar_r", 0.33);
        let pkt = encode_feedback(&fb, 9, 100, None);
        let d = decode(&pkt, None).unwrap();
        assert_eq!(d.header.dir, Direction::Feedback);
        assert_eq!(d.packet, Packet::Feedback(fb));
    }

    #[test]
    fn downgrade_and_key_mismatch_rejected() {
        let c = Cipher::from_passphrase("secret");
        let fr = sample_frame();
        // Plaintext packet against a keyed receiver → rejected (no downgrade).
        let plain = encode_input(&fr, 1, 1, None);
        assert_eq!(decode(&plain, Some(&c)).unwrap_err(), DecodeError::PlaintextRejected);
        // Encrypted packet against a keyless receiver → rejected.
        let sealed = encode_input(&fr, 1, 1, Some(&c));
        assert_eq!(decode(&sealed, None).unwrap_err(), DecodeError::EncryptedWithoutKey);
        // Wrong passphrase → auth failure.
        let other = Cipher::from_passphrase("other");
        assert_eq!(decode(&sealed, Some(&other)).unwrap_err(), DecodeError::AuthFailed);
    }

    #[test]
    fn garbage_and_truncation_rejected() {
        assert_eq!(decode(b"nope", None).unwrap_err(), DecodeError::NotFlexInput);
        let fr = sample_frame();
        let pkt = encode_input(&fr, 1, 1, None);
        for cut in [HEADER_LEN, HEADER_LEN + 3, pkt.len() - 5] {
            assert_eq!(decode(&pkt[..cut], None).unwrap_err(), DecodeError::Truncated);
        }
        let mut wrong_ver = pkt.clone();
        wrong_ver[4] = 99;
        assert_eq!(decode(&wrong_ver, None).unwrap_err(), DecodeError::UnknownVersion(99));
    }

    // A peer with a shorter (older) pin table: decode the common prefix.
    #[test]
    fn min_prefix_decode_tolerates_older_peer() {
        let layout = input_layout();
        let fr = sample_frame();
        let mut pkt = encode_input(&fr, 1, 1, None);
        // Rewrite the payload's n_pins/n_slots to pretend the sender knew only
        // the first 10 pins / their slots (still append-only compatible).
        let short_pins = 10usize;
        let short_slots: usize =
            layout.pins.iter().take(short_pins).map(|p| p.width).sum();
        pkt[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&(short_pins as u16).to_le_bytes());
        pkt[HEADER_LEN + 2..HEADER_LEN + 4].copy_from_slice(&(short_slots as u16).to_le_bytes());
        // Rebuild the packet body to the shorter claim: bitmap + slots + no extras.
        let mut body = Vec::new();
        body.extend_from_slice(&(short_pins as u16).to_le_bytes());
        body.extend_from_slice(&(short_slots as u16).to_le_bytes());
        let mut bitmap = vec![0u8; short_pins.div_ceil(8)];
        for i in 0..short_pins {
            if fr.present[i] {
                bitmap[i / 8] |= 1 << (i % 8);
            }
        }
        body.extend_from_slice(&bitmap);
        for &s in fr.slots.iter().take(short_slots) {
            body.extend_from_slice(&s.to_le_bytes());
        }
        body.push(0); // n_extra
        let mut short_pkt = pkt[..HEADER_LEN].to_vec();
        short_pkt.extend_from_slice(&body);

        let d = decode(&short_pkt, None).unwrap();
        assert!(!d.layout_match, "shorter table must be flagged");
        let Packet::Input(got) = d.packet else { panic!() };
        // left_stick (idx 0) survived; pins beyond the short table are absent.
        assert_eq!(got.get_idx(0), fr.get_idx(0));
        for i in short_pins..layout.pins.len() {
            assert_eq!(got.get_idx(i), None);
        }
    }

    #[test]
    fn seq_tracker_matrix() {
        let mut t = SeqTracker::default();
        assert!(t.accept(1, 10)); // first
        assert!(t.accept(1, 11)); // forward
        assert!(!t.accept(1, 11)); // duplicate
        assert!(!t.accept(1, 9)); // reorder
        assert!(t.accept(2, 3)); // new session adopted
        assert!(!t.accept(2, 3)); // dup in new session
        // wrap-around: near u32::MAX then past it counts as forward.
        let mut w = SeqTracker::default();
        assert!(w.accept(7, u32::MAX - 1));
        assert!(w.accept(7, u32::MAX));
        assert!(w.accept(7, 0)); // wrapped delta = +1
        assert!(!w.accept(7, u32::MAX)); // now behind
    }
}
