//! Where Bluetooth Classic link keys live.
//!
//! ⭐ **A link key is the bond, and the bond belongs to the DONGLE.** Pairing a
//! controller establishes a shared secret between it and this radio's address —
//! not this PC. The controller reconnects by looking for that address, so
//! carrying the dongle to another machine carries the pairing with it.
//!
//! ❗ Except for one thing: both ends must hold the key. The controller keeps
//! its copy in its own flash; the host's copy is this file. Move the dongle
//! without it and the controller comes back to a host that no longer knows it,
//! which fails as an authentication error rather than as anything that explains
//! itself.
//!
//! That is why the location is configurable rather than fixed beside the
//! executable. Pointed at a cloud-synced folder, the key follows the dongle to
//! every machine automatically and the copy step disappears — which is the
//! difference between a dongle you can move and a dongle you can move *and then
//! remember to do something about*.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

/// File name inside the chosen directory.
///
/// ⭐ Plain text, one device per line — deliberately not JSON.
/// It holds a handful of pairs, it is something a person may reasonably want to
/// read, copy or hand-edit while diagnosing a controller that will not
/// reconnect, and every added format is another thing between the user and
/// their own pairing.
const FILE_NAME: &str = "bt-classic-keys.txt";

/// Environment override, for the probe binaries and for anyone who would rather
/// not go through the UI.
const ENV_DIR: &str = "FLEXINPUT_BT_KEY_DIR";

fn dir_override() -> &'static RwLock<Option<PathBuf>> {
    static DIR: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    DIR.get_or_init(|| RwLock::new(None))
}

/// Point the key store at a directory. `None` restores the default.
///
/// Called from the settings UI, and once at startup so a saved setting is in
/// force before any controller tries to reconnect.
pub fn set_dir(dir: Option<PathBuf>) {
    if let Ok(mut d) = dir_override().write() {
        *d = dir;
    }
}

/// The directory currently in use: the explicit setting, else the environment,
/// else beside the executable.
///
/// ⭐ **Beside the executable, not the working directory.** The working
/// directory is wherever the app happened to be launched FROM — a shortcut, a
/// game launcher, an IDE — so the key file lands somewhere different depending
/// on how FlexInput was started, and a user looking for it has nowhere obvious
/// to look. Next to the exe it is always in the same place, and it is a place
/// someone can find, back up, or drop a synced copy into.
///
/// Falls back to the working directory only if the executable's own path
/// cannot be determined, which should not happen.
pub fn dir() -> PathBuf {
    if let Some(d) = dir_override().read().ok().and_then(|d| d.clone()) {
        return d;
    }
    if let Ok(d) = std::env::var(ENV_DIR) {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Some(beside_exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
    {
        return beside_exe;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Full path of the key file.
pub fn path() -> PathBuf {
    dir().join(FILE_NAME)
}

/// One paired controller: its key and, if it was recorded, its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub key: [u8; 16],
    /// The controller's own friendly name, captured when it was paired.
    ///
    /// ⭐ Stored rather than asked for. A name request PAGES the device — a
    /// real radio conversation, several seconds, and impossible at all while
    /// the controller is switched off. A list of pairings has to be readable
    /// when nothing is connected, which is exactly when a user is trying to
    /// work out which entry to remove.
    pub name: Option<String>,
    /// The DONGLE this bond was made with, when it is known.
    ///
    /// ⛔ **A key is worthless to any other adapter.** The controller stores
    /// the address of the radio it paired with and pages THAT address; a key
    /// copied from a different dongle produces a controller which blinks
    /// forever while the host sits with page scan on, listening perfectly, for
    /// a call addressed to somebody else. Nothing fails, nothing is logged, and
    /// every layer looks correct — which is precisely why this is recorded and
    /// checked instead of left to be discovered by a day of packet traces.
    ///
    /// `None` for entries written before this was stored.
    pub adapter: Option<[u8; 6]>,
}

/// Every stored pairing, keyed by lower-case colon-separated address.
///
/// A missing or unreadable file is an empty map, not an error: no keys yet is
/// the ordinary state before the first pairing, and refusing to start over it
/// would be absurd.
///
/// ❗ The name is the REST of the line, spaces and all — "Pro Controller" has
/// one in it, and splitting on whitespace would have stored "Pro". Older
/// two- and three-column files still load, with no name and no adapter.
///
/// An `@`-prefixed address after the key names the adapter that made the bond.
/// It is optional and positional-but-tagged so that files written before it
/// existed keep loading, and so a friendly name can never be mistaken for one.
pub fn load() -> BTreeMap<String, Pairing> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path()) else {
        return out;
    };
    for line in text.lines() {
        let mut it = line.splitn(3, char::is_whitespace);
        let (Some(addr), Some(key)) = (it.next(), it.next()) else { continue };
        if let Some(k) = parse_key(key) {
            let rest = it.next().map(str::trim).unwrap_or_default();
            let (adapter, rest) = match rest.strip_prefix('@') {
                Some(tail) => {
                    let (tok, after) = match tail.split_once(char::is_whitespace) {
                        Some((t, a)) => (t, a),
                        None => (tail, ""),
                    };
                    match parse_addr(tok) {
                        Some(a) => (Some(a), after.trim()),
                        None => (None, rest),
                    }
                }
                None => (None, rest),
            };
            let name = Some(rest).filter(|n| !n.is_empty()).map(str::to_string);
            out.insert(addr.to_ascii_lowercase(), Pairing { key: k, name, adapter });
        }
    }
    out
}

/// Look one address up.
pub fn get(addr: [u8; 6]) -> Option<[u8; 16]> {
    load().get(&format_addr(addr)).map(|p| p.key)
}

/// Store one key, keeping every other entry.
///
/// Creates the directory if it does not exist — a cloud folder the user has
/// just typed in may well not, and failing at that point would look like the
/// pairing failed rather than the folder being new.
pub fn put(
    addr: [u8; 6],
    key: [u8; 16],
    name: Option<&str>,
    adapter: Option<[u8; 6]>,
) -> std::io::Result<PathBuf> {
    let mut all = load();
    // Keep an existing name if this call has none — re-pairing should not lose
    // a label the user can recognise.
    let name = name
        .map(str::to_string)
        .or_else(|| all.get(&format_addr(addr)).and_then(|p| p.name.clone()));
    // ❗ The adapter is NOT kept from the old entry when this call has one: a
    // re-pair on a different dongle replaces the bond, and carrying the
    // previous adapter forward would preserve exactly the wrong fact.
    let adapter = adapter.or_else(|| all.get(&format_addr(addr)).and_then(|p| p.adapter));
    all.insert(format_addr(addr), Pairing { key, name, adapter });
    write_all(&all)
}

/// Remove one device's key. Returns whether it was there.
///
/// ❗ This is only HALF the bond. The controller keeps its own copy and will
/// still try to reconnect to this dongle; forgetting here makes the host refuse
/// it, which is what you want when retiring a controller, but it is not the
/// same as unpairing on the device. Re-pairing is what puts both halves back.
pub fn forget(addr: [u8; 6]) -> std::io::Result<bool> {
    let mut all = load();
    if all.remove(&format_addr(addr)).is_none() {
        return Ok(false);
    }
    write_all(&all)?;
    Ok(true)
}

/// Serialise the whole store.
///
/// ⭐ Whole-file, not append: the store is small, and rewriting it is what makes
/// removal and update the same operation. Two hand-rolled serialisers — one for
/// adding and one for removing — is exactly how a file format acquires a
/// trailing-newline bug that only bites on the path nobody exercises.
///
/// Creates the directory if it does not exist: a cloud folder the user has just
/// pointed at may well not, and failing there would look like the pairing
/// failed rather than the folder being new.
fn write_all(all: &BTreeMap<String, Pairing>) -> std::io::Result<PathBuf> {
    let p = path();
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut body = String::new();
    for (a, p) in all {
        body.push_str(a);
        body.push(' ');
        for b in &p.key {
            body.push_str(&format!("{b:02x}"));
        }
        if let Some(a) = p.adapter {
            body.push_str(" @");
            body.push_str(&format_addr(a));
        }
        if let Some(n) = &p.name {
            body.push(' ');
            // Newlines would split one entry into two unparseable ones.
            body.push_str(&n.replace(['\n', '\r'], " "));
        }
        body.push('\n');
    }
    std::fs::write(&p, body)?;
    Ok(p)
}

/// `aa:bb:cc:dd:ee:ff`, lower case — the form a person reads off a device list.
pub fn format_addr(addr: [u8; 6]) -> String {
    addr.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Parse `aa:bb:cc:dd:ee:ff` back into an address.
pub fn parse_addr(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut a = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        a[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(a)
}

fn parse_key(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut k = [0u8; 16];
    for i in 0..16 {
        k[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(k)
}

/// Whether a directory can actually be written to, for the settings UI to say
/// so BEFORE a pairing depends on it.
///
/// ⭐ A cloud-synced folder is exactly the kind that can be unavailable — not
/// yet created, offline, or read-only while the client reconciles. Finding that
/// out during a pairing means losing the key for a bond that has already
/// replaced the controller's previous host, which is the worst possible moment.
pub fn check_writable(dir: &Path) -> Result<(), String> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(format!("cannot create: {e}"));
    }
    let probe = dir.join(".flexinput-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!("not writable: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⛔ The store's directory is process-global, and Rust runs tests in
    /// PARALLEL THREADS of one process — so two tests that both call `set_dir`
    /// race, and each sees the other's files.
    ///
    /// They failed exactly that way when the second was added: not because
    /// either was wrong, but because the global made them one test pretending
    /// to be two. Serialising them is the honest fix; the alternative is a
    /// per-call directory argument that no caller wants.
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn an_address_round_trips_through_its_text_form() {
        let a = [0xda, 0x2d, 0x16, 0x0f, 0x01, 0x69];
        assert_eq!(format_addr(a), "da:2d:16:0f:01:69");
        assert_eq!(parse_addr("da:2d:16:0f:01:69"), Some(a));
        assert_eq!(parse_addr("DA:2D:16:0F:01:69"), Some(a), "case must not matter");
        assert_eq!(parse_addr("da:2d:16:0f:01"), None, "short address accepted");
        assert_eq!(parse_addr("nonsense"), None);
    }

    /// ⛔ A key must survive the text form byte for byte.
    ///
    /// It is a shared secret: one wrong nibble and the controller comes back to
    /// a host that cannot authenticate it, which surfaces as a reconnect
    /// failure with nothing pointing at the file.
    #[test]
    fn a_key_round_trips_byte_for_byte() {
        let k: [u8; 16] = core::array::from_fn(|i| (i * 17) as u8);
        let text: String = k.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_key(&text), Some(k));
        // Length is checked, so a truncated line cannot yield a partial key.
        assert_eq!(parse_key(&text[..30]), None);
        assert_eq!(parse_key("zz"), None);
    }

    /// ⛔ Several controllers must coexist, and removing one must not disturb
    /// the others.
    ///
    /// One dongle serving a couch full of pads is the point of the store being
    /// a map rather than a single key. The failure worth guarding is the
    /// rewrite: `forget` reserialises the WHOLE file, so a bug there loses
    /// every other pairing rather than the one asked for.
    #[test]
    fn several_pairings_coexist_and_forget_removes_only_one() {
        let _guard = exclusive();
        let dir = std::env::temp_dir().join("flexinput-keystore-multi");
        let _ = std::fs::remove_dir_all(&dir);
        set_dir(Some(dir.clone()));

        let pads: [[u8; 6]; 3] = [[0x11; 6], [0x22; 6], [0x33; 6]];
        for (i, a) in pads.iter().enumerate() {
            put(*a, [i as u8 + 1; 16], None, None).expect("write");
        }
        assert_eq!(load().len(), 3, "three pairings must coexist");
        for (i, a) in pads.iter().enumerate() {
            assert_eq!(get(*a), Some([i as u8 + 1; 16]), "pad {i} key wrong");
        }

        assert_eq!(forget(pads[1]).expect("forget"), true);
        assert_eq!(get(pads[1]), None, "the removed pad is still there");
        assert_eq!(get(pads[0]), Some([1; 16]), "forget disturbed another pairing");
        assert_eq!(get(pads[2]), Some([3; 16]), "forget disturbed another pairing");
        assert_eq!(load().len(), 2);

        // Removing something absent is not an error and changes nothing.
        assert_eq!(forget([0xAB; 6]).expect("forget"), false);
        assert_eq!(load().len(), 2);

        set_dir(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⛔ The adapter round-trips, and an old two-or-three-column file still
    /// loads. A key belongs to the dongle that made it, so losing that column
    /// silently would restore the failure it exists to explain.
    #[test]
    fn the_adapter_survives_a_round_trip_and_old_files_still_load() {
        let _guard = exclusive();
        let dir = std::env::temp_dir().join("flexinput-keystore-adapter");
        let _ = std::fs::remove_dir_all(&dir);
        set_dir(Some(dir.clone()));

        let pad = [0x11u8; 6];
        let dongle = [0x8c, 0x68, 0x8b, 0x81, 0xe3, 0xc5];
        put(pad, [0x22; 16], Some("Pro Controller"), Some(dongle)).expect("write");
        let back = load();
        let p = back.get(&format_addr(pad)).expect("entry missing");
        assert_eq!(p.adapter, Some(dongle), "adapter lost");
        assert_eq!(p.name.as_deref(), Some("Pro Controller"), "name lost");
        assert_eq!(p.key, [0x22; 16]);

        // ❗ A name is never mistaken for an adapter, even one starting with @.
        std::fs::write(
            path(),
            "11:11:11:11:11:11 33333333333333333333333333333333 @home pad
",
        )
        .unwrap();
        let back = load();
        let p = back.get(&format_addr(pad)).expect("entry missing");
        assert_eq!(p.adapter, None, "'@home pad' is a name, not an address");
        assert_eq!(p.name.as_deref(), Some("@home pad"));

        // A file written before adapters existed loads with none.
        std::fs::write(
            path(),
            "11:11:11:11:11:11 44444444444444444444444444444444 Pro Controller
",
        )
        .unwrap();
        let p = load().get(&format_addr(pad)).cloned().expect("entry missing");
        assert_eq!(p.adapter, None);
        assert_eq!(p.name.as_deref(), Some("Pro Controller"));

        set_dir(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Junk lines are skipped rather than poisoning the whole store — the file
    /// is meant to be hand-editable, so a stray blank or comment must not cost
    /// someone every other pairing they have.
    #[test]
    fn a_malformed_line_does_not_discard_the_good_ones() {
        let _guard = exclusive();
        let dir = std::env::temp_dir().join("flexinput-keystore-test");
        // ❗ Cleared at the START, not only at the end. `put` MERGES with what
        // is on disk, so a file left behind by a previous run that panicked
        // before its cleanup silently joins this one's data — which is exactly
        // how this test first failed, reporting a bug that did not exist.
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        set_dir(Some(dir.clone()));
        let good = [0x11u8; 6];
        put(good, [0x22; 16], Some("Pro Controller"), None).expect("write");
        // Append rubbish of several shapes.
        let mut text = std::fs::read_to_string(path()).unwrap();
        text.push_str("\n# a comment\n\nnot-an-address zzzz\naa:bb short\n");
        std::fs::write(path(), text).unwrap();

        let all = load();
        assert_eq!(all.len(), 1, "good entry lost among the junk: {all:?}");
        assert_eq!(get(good), Some([0x22; 16]));
        // ⭐ And the NAME survives, spaces and all. Splitting the line on
        // whitespace instead of on the first two fields would have stored
        // "Pro" — which is the kind of wrong that looks right in a list.
        assert_eq!(
            all[&format_addr(good)].name.as_deref(),
            Some("Pro Controller"),
            "the recorded name did not round-trip",
        );
        set_dir(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
