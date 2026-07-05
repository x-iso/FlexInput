//! PSK encryption for the secure transport tiers.
//!
//! Both peers derive per-direction ChaCha20-Poly1305 keys from a shared
//! passphrase via HKDF-SHA256. Directions use SEPARATE keys ("input" vs
//! "feedback" info strings) so the two sides of a bidirectional link can never
//! reuse a (key, nonce) pair even though both derive nonces the same way.
//!
//! Nonce (96-bit) = session_id (8 bytes LE) ‖ seq (4 bytes LE). session_id is
//! random per socket lifetime and seq is monotonic per direction, so nonces
//! never repeat under one key within a session; a peer restart rolls a new
//! session_id. The full 24-byte plaintext header is bound as AAD, so a
//! tampered header (direction flip, seq splice) fails authentication.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::protocol::Direction;

/// Poly1305 tag length appended to every sealed payload.
pub const TAG_LEN: usize = 16;

const HKDF_SALT: &[u8] = b"flexinput-net-v1";

/// Per-direction AEAD cipher pair derived from one passphrase.
pub struct Cipher {
    input: ChaCha20Poly1305,
    feedback: ChaCha20Poly1305,
}

impl Cipher {
    pub fn from_passphrase(psk: &str) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), psk.as_bytes());
        let mut key = [0u8; 32];
        hk.expand(b"input", &mut key).expect("32B is a valid HKDF-SHA256 length");
        let input = ChaCha20Poly1305::new(Key::from_slice(&key));
        hk.expand(b"feedback", &mut key).expect("32B is a valid HKDF-SHA256 length");
        let feedback = ChaCha20Poly1305::new(Key::from_slice(&key));
        Self { input, feedback }
    }

    fn for_dir(&self, dir: Direction) -> &ChaCha20Poly1305 {
        match dir {
            Direction::Input => &self.input,
            Direction::Feedback => &self.feedback,
        }
    }

    /// Encrypt `plaintext`, binding `aad` (the packet header). Returns
    /// ciphertext with the 16-byte tag appended.
    pub fn seal(
        &self,
        dir: Direction,
        session_id: u64,
        seq: u32,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Vec<u8> {
        let nonce = nonce_bytes(session_id, seq);
        self.for_dir(dir)
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
            .expect("ChaCha20Poly1305 encrypt is infallible for in-memory buffers")
    }

    /// Decrypt + authenticate. `None` on any tamper/key mismatch.
    pub fn open(
        &self,
        dir: Direction,
        session_id: u64,
        seq: u32,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Option<Vec<u8>> {
        let nonce = nonce_bytes(session_id, seq);
        self.for_dir(dir)
            .decrypt(Nonce::from_slice(&nonce), Payload { msg: ciphertext, aad })
            .ok()
    }
}

fn nonce_bytes(session_id: u64, seq: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(&session_id.to_le_bytes());
    n[8..].copy_from_slice(&seq.to_le_bytes());
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let c = Cipher::from_passphrase("hunter2");
        let aad = b"header-bytes";
        let sealed = c.seal(Direction::Input, 7, 42, aad, b"payload");
        assert_eq!(sealed.len(), 7 + TAG_LEN);
        let opened = c.open(Direction::Input, 7, 42, aad, &sealed).unwrap();
        assert_eq!(opened, b"payload");
    }

    #[test]
    fn wrong_passphrase_fails() {
        let a = Cipher::from_passphrase("alpha");
        let b = Cipher::from_passphrase("bravo");
        let sealed = a.seal(Direction::Input, 1, 1, b"aad", b"data");
        assert!(b.open(Direction::Input, 1, 1, b"aad", &sealed).is_none());
    }

    #[test]
    fn tampered_aad_or_body_fails() {
        let c = Cipher::from_passphrase("psk");
        let sealed = c.seal(Direction::Feedback, 3, 9, b"aad", b"data");
        assert!(c.open(Direction::Feedback, 3, 9, b"AAD", &sealed).is_none());
        let mut bad = sealed.clone();
        bad[0] ^= 1;
        assert!(c.open(Direction::Feedback, 3, 9, b"aad", &bad).is_none());
    }

    #[test]
    fn directions_use_distinct_keys() {
        let c = Cipher::from_passphrase("psk");
        let sealed = c.seal(Direction::Input, 3, 9, b"aad", b"data");
        // Same nonce inputs, other direction key → must not open.
        assert!(c.open(Direction::Feedback, 3, 9, b"aad", &sealed).is_none());
    }

    #[test]
    fn nonce_depends_on_session_and_seq() {
        let c = Cipher::from_passphrase("psk");
        let sealed = c.seal(Direction::Input, 3, 9, b"aad", b"data");
        assert!(c.open(Direction::Input, 3, 10, b"aad", &sealed).is_none());
        assert!(c.open(Direction::Input, 4, 9, b"aad", &sealed).is_none());
    }
}
