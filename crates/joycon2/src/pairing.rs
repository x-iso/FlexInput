//! Nintendo's pseudo-out-of-band Bluetooth pairing for Switch 2 controllers.
//!
//! Switch 2 controllers do not implement SMP at all — a host that tries the
//! standard LE pairing flow gets disconnected. Instead the key exchange runs as
//! four `0x15` subcommands over the ordinary command channel:
//!
//! 1. `0x01` host sends its Bluetooth address(es); controller replies with its own.
//! 2. `0x04` host sends a 16-byte key `A1`; controller replies with `B1`.
//!    Both sides then hold `LTK = A1 ^ B1`.
//! 3. `0x02` host sends a 16-byte challenge `A2`; controller replies with
//!    `B2 = AES128-ECB(LTK, A2)`, proving it derived the same key.
//! 4. `0x03` finalises, committing the host address and LTK to controller flash.
//!
//! **Step 4 writes to persistent controller memory (`0x1FA000`), which holds
//! only two host slots.** Pairing a Joy-Con 2 to a PC can therefore evict a
//! console's entry and require re-syncing on the Switch 2. Nothing here runs
//! unless pairing is explicitly enabled.

use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use aes::Aes128;

use crate::protocol;

/// The controller's half of the key exchange is not actually random — every
/// unit observed so far returns this same constant. Kept as a sanity check:
/// a controller that returns something else is either a different product or
/// our framing is off, and both are worth a log line.
pub const KNOWN_DEVICE_KEY: [u8; 16] = [
    0x5c, 0xf6, 0xee, 0x79, 0x2c, 0xdf, 0x05, 0xe1,
    0xba, 0x2b, 0x63, 0x25, 0xc4, 0x1a, 0x5f, 0x10,
];

/// `LTK = A1 ^ B1`, computed identically on both sides.
pub fn derive_ltk(host_key: &[u8; 16], device_key: &[u8; 16]) -> [u8; 16] {
    let mut ltk = [0u8; 16];
    for i in 0..16 {
        ltk[i] = host_key[i] ^ device_key[i];
    }
    ltk
}

/// Compute the confirmation the controller is expected to return for `challenge`.
///
/// Both the key and the plaintext are byte-reversed before the AES block, which
/// is the one detail that makes this fail silently if transcribed from the spec
/// tables rather than the reference implementation.
pub fn expected_confirmation(ltk: &[u8; 16], challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = *ltk;
    key.reverse();
    let mut block = *challenge;
    block.reverse();

    let cipher = Aes128::new(GenericArray::from_slice(&key));
    let mut block = GenericArray::clone_from_slice(&block);
    cipher.encrypt_block(&mut block);
    block.into()
}

/// How the controller's confirmation compared against ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// Matched exactly — the controller derived the same LTK.
    Match,
    /// Matched once reversed. The research doc marks several of these fields
    /// "(reverse byte-order)" inconsistently, so this is treated as a pass and
    /// logged rather than a failure; it tells us which convention real hardware
    /// actually uses.
    MatchReversed,
    Mismatch,
}

pub fn check_confirmation(expected: &[u8; 16], received: &[u8; 16]) -> Confirmation {
    if expected == received {
        return Confirmation::Match;
    }
    let mut rev = *received;
    rev.reverse();
    if *expected == rev {
        return Confirmation::MatchReversed;
    }
    Confirmation::Mismatch
}

// ── Request payload builders ──────────────────────────────────────────────────

/// Subcommand `0x01` data: `[0x00][count][addr × count]`, each address in
/// reverse byte order (little-endian BD_ADDR, as it goes on the wire).
pub fn exchange_addresses_data(hosts: &[[u8; 6]]) -> Vec<u8> {
    let mut data = Vec::with_capacity(2 + hosts.len() * 6);
    data.push(0x00);
    data.push(hosts.len() as u8);
    for addr in hosts {
        let mut wire = *addr;
        wire.reverse();
        data.extend_from_slice(&wire);
    }
    data
}

/// Pull the controller's own address out of a subcommand `0x01` response.
/// Response data is `[0x01][unknown][count][addr 6]`; returned in natural
/// (big-endian, display) order.
pub fn parse_controller_address(data: &[u8]) -> Option<[u8; 6]> {
    if data.len() < 9 {
        return None;
    }
    let mut addr = [0u8; 6];
    addr.copy_from_slice(&data[3..9]);
    addr.reverse();
    Some(addr)
}

/// Subcommand `0x04` data: `[0x00][A1 16]`.
pub fn exchange_keys_data(host_key: &[u8; 16]) -> Vec<u8> {
    let mut data = Vec::with_capacity(17);
    data.push(0x00);
    data.extend_from_slice(host_key);
    data
}

/// Subcommand `0x02` data: `[0x00][A2 16]`.
pub fn confirm_ltk_data(challenge: &[u8; 16]) -> Vec<u8> {
    let mut data = Vec::with_capacity(17);
    data.push(0x00);
    data.extend_from_slice(challenge);
    data
}

/// Subcommand `0x03` data: a single `0x00`.
pub fn finalise_data() -> Vec<u8> {
    vec![0x00]
}

/// Data for command `0x03` subcommand `0x07`: hand the controller the host
/// address and the link key the connection should use.
///
/// `[host address 6, wire order][LTK 16, byte-reversed]` — 22 bytes, matching
/// the `0x16` length field in the captured init.
///
/// This step is sent immediately after pairing is finalised and is **not**
/// optional: without it the controller has completed a key exchange but has
/// never been told which key the live link is using. Omitting it was almost
/// certainly why connections were dropped a fixed ~30 s after connecting,
/// across two different Bluetooth adapters.
pub fn register_link_key_data(host: &[u8; 6], ltk: &[u8; 16]) -> Vec<u8> {
    let mut data = Vec::with_capacity(22);
    let mut addr = *host;
    addr.reverse();
    data.extend_from_slice(&addr);
    // Reversed, exactly like the key material in subcommands 0x02 and 0x04.
    let mut key = *ltk;
    key.reverse();
    data.extend_from_slice(&key);
    data
}

pub fn cmd_register_link_key(host: &[u8; 6], ltk: &[u8; 16]) -> Vec<u8> {
    protocol::command(
        protocol::CMD_PAIRING_EXTRA,
        SUB_REGISTER_LINK_KEY,
        &register_link_key_data(host, ltk),
    )
}

/// `0x03/0x07` — register host address + link key.
pub const SUB_REGISTER_LINK_KEY: u8 = 0x07;
/// `0x03/0x09` — no data; closes out the sequence above.
pub const SUB_LINK_KEY_COMMIT: u8 = 0x09;

/// Extract the 16-byte payload from a `0x04` or `0x02` response, which is
/// `[0x01][key-or-response 16]`.
pub fn parse_key_response(data: &[u8]) -> Option<[u8; 16]> {
    if data.len() < 17 {
        return None;
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&data[1..17]);
    Some(out)
}

/// Convenience wrappers so callers don't hand-roll the command ids.
pub fn cmd_exchange_addresses(hosts: &[[u8; 6]]) -> Vec<u8> {
    protocol::command(
        protocol::CMD_PAIRING,
        protocol::SUB_PAIR_EXCHANGE_ADDRS,
        &exchange_addresses_data(hosts),
    )
}

pub fn cmd_exchange_keys(host_key: &[u8; 16]) -> Vec<u8> {
    protocol::command(
        protocol::CMD_PAIRING,
        protocol::SUB_PAIR_EXCHANGE_KEYS,
        &exchange_keys_data(host_key),
    )
}

pub fn cmd_confirm_ltk(challenge: &[u8; 16]) -> Vec<u8> {
    protocol::command(
        protocol::CMD_PAIRING,
        protocol::SUB_PAIR_CONFIRM_LTK,
        &confirm_ltk_data(challenge),
    )
}

pub fn cmd_finalise() -> Vec<u8> {
    protocol::command(
        protocol::CMD_PAIRING,
        protocol::SUB_PAIR_FINALISE,
        &finalise_data(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The worked example from `bluetooth_interface.md`.
    const A1: [u8; 16] = [
        0x35, 0x03, 0xe9, 0x29, 0x82, 0x87, 0x71, 0x24,
        0xbe, 0xa8, 0x0c, 0x66, 0x46, 0x15, 0x83, 0x4b,
    ];
    const A2: [u8; 16] = [
        0x6f, 0xc6, 0xdf, 0x8a, 0xd8, 0xfe, 0xdf, 0x15,
        0xbb, 0x8c, 0x15, 0xe9, 0x1f, 0x32, 0x05, 0x44,
    ];
    /// The `0x15/0x02` response captured on the wire in `commands.md`.
    const WIRE_B2: [u8; 16] = [
        0x13, 0x4c, 0x97, 0xf5, 0x11, 0xb9, 0xb6, 0xdd,
        0x4d, 0x86, 0xfd, 0x40, 0xf5, 0x36, 0xe9, 0xed,
    ];

    #[test]
    fn ltk_is_the_xor_of_both_keys() {
        let ltk = derive_ltk(&A1, &KNOWN_DEVICE_KEY);
        assert_eq!(ltk[0], 0x35 ^ 0x5c);
        assert_eq!(ltk[15], 0x4b ^ 0x10);
        // XOR is an involution: recovering either input from the LTK proves the
        // derivation is symmetric, which is what lets both sides compute it.
        assert_eq!(derive_ltk(&ltk, &KNOWN_DEVICE_KEY), A1);
    }

    /// Known-answer test against real captured traffic. If this fails, our
    /// byte-order handling is wrong and pairing would be rejected by hardware —
    /// far better to learn that here than on the controller.
    #[test]
    fn confirmation_matches_captured_hardware_response() {
        let ltk = derive_ltk(&A1, &KNOWN_DEVICE_KEY);
        let expected = expected_confirmation(&ltk, &A2);
        assert_eq!(
            check_confirmation(&expected, &WIRE_B2),
            Confirmation::Match,
            "computed {expected:02x?} vs captured {WIRE_B2:02x?}",
        );
    }

    #[test]
    fn confirmation_detects_a_wrong_response() {
        let ltk = derive_ltk(&A1, &KNOWN_DEVICE_KEY);
        let expected = expected_confirmation(&ltk, &A2);
        assert_eq!(check_confirmation(&expected, &[0u8; 16]), Confirmation::Mismatch);

        let mut reversed = expected;
        reversed.reverse();
        assert_eq!(check_confirmation(&expected, &reversed), Confirmation::MatchReversed);
    }

    #[test]
    fn address_exchange_round_trips_through_the_wire_order() {
        let host = [0x48, 0xf1, 0xeb, 0x85, 0x11, 0x5f];
        let data = exchange_addresses_data(&[host]);
        assert_eq!(data[0], 0x00);
        assert_eq!(data[1], 1, "count");
        assert_eq!(&data[2..8], &[0x5f, 0x11, 0x85, 0xeb, 0xf1, 0x48]);

        // A response echoes an address back in the same wire order.
        let mut resp = vec![0x01, 0x04, 0x01];
        resp.extend_from_slice(&[0x5f, 0x11, 0x85, 0xeb, 0xf1, 0x48]);
        assert_eq!(parse_controller_address(&resp), Some(host));
    }

    /// Matches the captured `15 91 01 01 00 0e …` — two addresses, 14 data bytes.
    #[test]
    fn two_host_addresses_produce_the_captured_length() {
        let data = exchange_addresses_data(&[[0; 6], [0; 6]]);
        assert_eq!(data.len(), 0x0e);
        let framed = cmd_exchange_addresses(&[[0; 6], [0; 6]]);
        assert_eq!(framed[0], protocol::CMD_PAIRING);
        assert_eq!(framed[3], protocol::SUB_PAIR_EXCHANGE_ADDRS);
        assert_eq!(framed[5], 0x0e, "header length field");
    }

    #[test]
    fn key_payloads_are_seventeen_bytes_and_round_trip() {
        let data = exchange_keys_data(&A1);
        assert_eq!(data.len(), 17);
        assert_eq!(parse_key_response(&data), Some(A1));

        let data = confirm_ltk_data(&A2);
        assert_eq!(data.len(), 17);
        assert_eq!(parse_key_response(&data), Some(A2));
    }

    /// Known-answer test decoding the `0x03/0x07` command straight out of the
    /// captured Joy-Con 2 init, which is the whole basis for this step:
    ///
    /// ```text
    /// 03 91 01 07 00 16 00 00 | 5e 11 85 eb f1 48 c1 27 80 67 1a fd 29 b8 00 e1 dd c5 19 b4 f0 54
    /// ```
    ///
    /// The trailing 16 bytes are the session LTK reversed. Reproducing them
    /// from that session's `A1` proves the derivation rather than assuming it.
    #[test]
    fn link_key_registration_matches_the_captured_init() {
        // `15 91 01 04 … 00 08 06 5a …` from the same capture.
        let a1: [u8; 16] = [
            0x08, 0x06, 0x5a, 0x60, 0xe9, 0x02, 0xe4, 0xe1,
            0x02, 0x02, 0x9e, 0x3f, 0xa3, 0x9a, 0x78, 0xd1,
        ];
        let ltk = derive_ltk(&a1, &KNOWN_DEVICE_KEY);
        // Host address as it appears on the wire in the address exchange.
        let host = [0x48, 0xf1, 0xeb, 0x85, 0x11, 0x5e];

        let data = register_link_key_data(&host, &ltk);
        assert_eq!(data.len(), 0x16, "must match the captured length field");
        assert_eq!(
            data,
            vec![
                0x5e, 0x11, 0x85, 0xeb, 0xf1, 0x48, // host address, wire order
                0xc1, 0x27, 0x80, 0x67, 0x1a, 0xfd, 0x29, 0xb8, // LTK reversed
                0x00, 0xe1, 0xdd, 0xc5, 0x19, 0xb4, 0xf0, 0x54,
            ],
        );

        let framed = cmd_register_link_key(&host, &ltk);
        assert_eq!(
            &framed[..protocol::CMD_HEADER_LEN],
            &[0x03, 0x91, 0x01, 0x07, 0x00, 0x16, 0x00, 0x00],
        );
    }

    #[test]
    fn short_responses_are_rejected_rather_than_panicking() {
        assert_eq!(parse_key_response(&[0x01; 8]), None);
        assert_eq!(parse_controller_address(&[0x01; 4]), None);
    }
}
