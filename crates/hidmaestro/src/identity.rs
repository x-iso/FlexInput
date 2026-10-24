//! HIDMaestro v2.0's version contract, FlexInput's half (upstream
//! `driver/hm_contract.h`, `DeviceIdentity.TagOf`,
//! `DeviceOrchestrator.WriteIdentityEntry`).
//!
//! Every devnode of one controller — the HID parent and, for an Xbox 360, its
//! XUSB companion — ends its instance ID in the same identity token. At add the
//! driver reads that token, looks up `HKLM\SOFTWARE\HIDMaestro\Identity\<token>`
//! for the controller index, and reads the controller's v2.0 values
//! ([`crate::answers`]) only when that index's version section
//! (`Global\HIDMaestroVersion<N>`, made by [`crate::InputSection::create`])
//! carries the token's tag. Otherwise it serves the devnode as a v1 program's:
//! no Sony feature answers, and no XInput interface on the companion.
//!
//! FlexInput's tokens are `FX` + uppercase hex, unique per create and never
//! reused: a new devnode never meets a record a previous life left (why the SWD
//! nodes already took a fresh suffix per create), and the startup sweep can tell
//! FlexInput's devnodes from another HIDMaestro program's.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::orchestrator::registry;

/// What every FlexInput identity token starts with.
pub const TOKEN_PREFIX: &str = "FX";
const IDENTITY_ROOT: &str = r"SOFTWARE\HIDMaestro\Identity";
const IDENTITY_VALUE: &str = "ControllerIndex";

/// A fresh identity token: `FX`, this helper's start time and process id, and a
/// per-create counter, in uppercase hex (Windows may store an instance ID in
/// uppercase, and the tag is taken over the token as the driver reads it back).
/// 22 characters, inside the driver's 39.
pub fn new_token() -> String {
    static START: OnceLock<u32> = OnceLock::new();
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let start = *START.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0)
    });
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) & 0xFFFF;
    format!("{TOKEN_PREFIX}{start:08X}{:08X}{seq:04X}", std::process::id())
}

/// The identity tag of a token: 64-bit FNV-1a over its UTF-16LE code units, no
/// terminator — `HmIdentityTag` in hm_contract.h.
pub fn tag(token: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for unit in token.encode_utf16() {
        for byte in unit.to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    h
}

/// The token of a devnode: the last segment of its instance ID, as the driver
/// reads it (`HmReadInstanceToken`).
pub fn token_of(instance_id: &str) -> &str {
    instance_id.rsplit('\\').next().unwrap_or(instance_id)
}

/// Whether a token is one of FlexInput's.
pub fn is_ours(token: &str) -> bool {
    token.len() > TOKEN_PREFIX.len()
        && token.as_bytes()[..TOKEN_PREFIX.len()].eq_ignore_ascii_case(TOKEN_PREFIX.as_bytes())
}

/// Write the identity entry naming `index` for `token`, before any PnP work, as
/// the driver reads it before its first start. Every other entry that names the
/// index goes first: the index is ours, so such an entry is only ever one a
/// program left behind when it ended without its teardown (upstream does the
/// same).
pub fn write_entry(token: &str, index: u32) -> Result<(), u32> {
    for other in registry::enum_subkeys(registry::HKLM, IDENTITY_ROOT).unwrap_or_default() {
        if !other.eq_ignore_ascii_case(token)
            && registry::read_dword(registry::HKLM, &entry_path(&other), IDENTITY_VALUE) == Some(index)
        {
            registry::delete_tree(registry::HKLM, &entry_path(&other));
        }
    }
    registry::write_dword(registry::HKLM, &entry_path(token), IDENTITY_VALUE, index)
}

/// Remove a controller's identity entry, once its devnodes are gone.
pub fn delete_entry(token: &str) {
    if !token.is_empty() {
        registry::delete_tree(registry::HKLM, &entry_path(token));
    }
}

/// Remove FlexInput's identity entries whose token no devnode in `live` carries:
/// what a helper that crashed or was killed left. Never touches another
/// program's entries. Returns how many went.
pub fn sweep_stale<'a>(live: impl IntoIterator<Item = &'a str>) -> usize {
    let live: HashSet<String> = live.into_iter().map(|t| t.to_ascii_uppercase()).collect();
    let mut removed = 0;
    for token in registry::enum_subkeys(registry::HKLM, IDENTITY_ROOT).unwrap_or_default() {
        if is_ours(&token) && !live.contains(&token.to_ascii_uppercase()) {
            registry::delete_tree(registry::HKLM, &entry_path(&token));
            removed += 1;
        }
    }
    removed
}

fn entry_path(token: &str) -> String {
    format!(r"{IDENTITY_ROOT}\{token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_match_hm_contract() {
        // The two vectors hm_contract.h gives.
        assert_eq!(tag("HM_0000"), 0x1FD0_4891_6B8D_461F);
        assert_eq!(tag("HM_0123456789ABCDEF"), 0x3B86_F363_4086_D831);
    }

    #[test]
    fn tokens_are_ours_unique_uppercase_and_fit_the_driver() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        for t in [&a, &b] {
            assert!(is_ours(t), "{t}");
            assert!(t.len() < 40, "{t}");
            assert!(t[2..].bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()), "{t}");
        }
        assert!(!is_ours("HM_0000"));
        assert!(!is_ours("0000"));
        assert!(!is_ours("FX"));
    }

    #[test]
    fn the_token_is_the_last_segment() {
        assert_eq!(token_of(r"ROOT\HIDCLASS\FX0123"), "FX0123");
        assert_eq!(token_of(r"SWD\HIDMAESTRO\FX0123"), "FX0123");
        assert_eq!(token_of("FX0123"), "FX0123");
    }
}
