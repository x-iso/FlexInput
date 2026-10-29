//! Hiding remapped controllers from games on Linux: the counterpart of HidHide.
//!
//! Linux has no per-application device whitelist. A game runs as the same user
//! as FlexInput, so whatever FlexInput may open, the game may too. What Linux
//! does have is device nodes, which can be made unreadable. So a small helper,
//! this binary run as root through pkexec (`--linux-hide-helper`, one password
//! prompt per session), takes the public nodes of each hidden controller
//! (hidraw, evdev, js) away from the user, and makes FlexInput a private node
//! for the same device in [`PRIVATE_DIR`], owned by the user. Games find the
//! public nodes (udev, SDL, Steam), can't open them, and skip the controller.
//! FlexInput's own opens fall back to the private node: [`reachable`] for its
//! hidraw readers, a patch in the vendored gilrs-core for evdev, and for SDL,
//! [`open_or_private`], which the linker puts behind every C `open` call in
//! the binary.
//!
//! - Nodes that already exist are taken away directly. Another program that
//!   already has one open (Steam keeps every controller it supports open) is
//!   cut off by having the kernel re-attach the controller, once per
//!   controller: every open handle dies with the old nodes.
//! - Nodes made later (a reconnect, that re-attach) are born hidden: a runtime
//!   udev rule ([`RULES_FILE`]) drops their `uaccess` grant and makes the
//!   private node before udev tells anyone the node exists. Nobody can race it.
//! - When FlexInput closes (the helper's stdin closes), or on the helper's next
//!   start if it was killed, the rule goes and udev gives every node back.
//!
//! The helper makes a private node only for a device the user could already
//! open, and only for hidraw and evdev nodes.

use std::collections::HashSet;
use std::ffi::CString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Where the private nodes live. On devtmpfs: the user's runtime dir and /run
/// are mounted `nodev`, so a device node there can't be opened. Root-owned,
/// so only the helper (and its udev rule) adds or removes nodes.
pub const PRIVATE_DIR: &str = "/dev/flexinput";

/// The command-line flag that makes this binary the helper.
pub const HELPER_FLAG: &str = "--linux-hide-helper";

/// The runtime udev rule that makes a hidden controller's new nodes hidden. In
/// /run, so a reboot clears it whatever happens. Between udev's `uaccess`
/// tagging (70) and the seat rules that apply it (73).
const RULES_FILE: &str = "/run/udev/rules.d/72-flexinput-hide.rules";

/// The POSIX ACL xattr udev's `uaccess` grants the user through.
const ACL_XATTR: &str = "system.posix_acl_access";

/// How long a controller that leaves the set stays hidden. FlexInput drops a
/// controller from the set while it's disconnected, which the eviction below
/// does to it for a second or two: showing it right away handed it to Steam
/// as it came back.
const RELEASE_GRACE: Duration = Duration::from_secs(5);

/// Where the helper looks for the system tools it runs: pkexec gives it a PATH
/// of the first four, which NixOS doesn't use.
const TOOL_DIRS: [&str; 7] = [
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/run/current-system/sw/bin",
];

fn tool(name: &str) -> Option<String> {
    TOOL_DIRS.iter().map(|d| format!("{d}/{name}")).find(|p| Path::new(p).is_file())
}

/// The node FlexInput should open for `path`: the public one while it can, else
/// the private one the helper made for it. For FlexInput's hidraw opens.
pub fn reachable(path: &std::ffi::CStr) -> CString {
    // SAFETY: a valid NUL-terminated path; access() only reads it.
    if unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::W_OK) } == 0 {
        return path.to_owned();
    }
    let bytes = path.to_bytes();
    let name = bytes.rsplit(|&b| b == b'/').next().unwrap_or(bytes);
    let mut private = format!("{PRIVATE_DIR}/").into_bytes();
    private.extend_from_slice(name);
    match CString::new(private) {
        // SAFETY: as above.
        Ok(p) if unsafe { libc::access(p.as_ptr(), libc::R_OK | libc::W_OK) } == 0 => p,
        _ => path.to_owned(),
    }
}

/// The private twin of a public controller node, written NUL-terminated into
/// `buf`: `/dev/hidrawN` or `/dev/input/eventN` → `/dev/flexinput/<name>`, the
/// two kinds the helper makes private nodes for. `None` for any other path.
fn private_twin<'a>(path: &[u8], buf: &'a mut [u8; 64]) -> Option<&'a std::ffi::CStr> {
    let rest = path.strip_prefix(b"/dev/")?;
    let name = if rest.starts_with(b"hidraw") {
        rest
    } else {
        rest.strip_prefix(b"input/").filter(|n| n.starts_with(b"event"))?
    };
    if name.contains(&b'/') {
        return None;
    }
    let dir = PRIVATE_DIR.as_bytes();
    let len = dir.len() + 1 + name.len();
    if len >= buf.len() {
        return None;
    }
    buf[..dir.len()].copy_from_slice(dir);
    buf[dir.len()] = b'/';
    buf[dir.len() + 1..len].copy_from_slice(name);
    buf[len] = 0;
    std::ffi::CStr::from_bytes_with_nul(&buf[..=len]).ok()
}

/// `open(2)` for every C caller in the `flexinput` binary on Linux: the linker
/// wraps their `open` into this (`-Wl,--wrap=open`, app/build.rs). Reading a
/// hidden controller is the point — SDL's joystick and HID code open the
/// public node, which hiding refuses, and SDL has no way to be told about the
/// private one. So a refused open of a controller node is retried on its
/// private twin; every other open behaves exactly as `open` does. SDL keeps the
/// public path, so its udev lookups and the own-virtual test still work.
///
/// No allocation and nothing but syscalls: callers may be anywhere, a forked
/// child included.
///
/// # Safety
/// `path` is a valid NUL-terminated string, as for `open`. `mode` is read only
/// with `O_CREAT` / `O_TMPFILE`, as for `open`.
pub unsafe fn open_or_private(path: *const libc::c_char, flags: libc::c_int, mode: libc::c_uint) -> libc::c_int {
    // glibc's open is this same openat(AT_FDCWD, …), which isn't wrapped.
    let fd = libc::openat(libc::AT_FDCWD, path, flags, mode);
    if fd >= 0 || path.is_null() {
        return fd;
    }
    let err = *libc::__errno_location();
    if err != libc::EACCES && err != libc::EPERM {
        return fd;
    }
    let mut buf = [0u8; 64];
    if let Some(private) = private_twin(std::ffi::CStr::from_ptr(path).to_bytes(), &mut buf) {
        let fd = libc::openat(libc::AT_FDCWD, private.as_ptr(), flags, mode);
        if fd >= 0 {
            return fd;
        }
    }
    // The caller sees the public node's refusal, not the twin's absence.
    *libc::__errno_location() = err;
    -1
}

// ── Client: FlexInput's side ────────────────────────────────────────────────

/// FlexInput's handle on the helper: starts it on the first non-empty set and
/// sends it each new set. Dropping it (FlexInput closing) closes the helper's
/// stdin, which puts everything back.
#[derive(Default)]
pub struct HideClient {
    child: Option<std::process::Child>,
    /// The password prompt was dismissed (or pkexec failed): don't prompt again
    /// until [`HideClient::allow_prompt`].
    declined: bool,
}

impl HideClient {
    /// Whether hiding can work here: pkexec is installed.
    pub fn available() -> bool {
        find_in_path("pkexec").is_some()
    }

    /// The user changed what to hide: a dismissed prompt may be shown again.
    pub fn allow_prompt(&mut self) {
        self.declined = false;
    }

    /// Hide exactly these controllers (by USB vendor and product id), and show
    /// every other one the helper hid. Starts the helper — the password prompt —
    /// only when there is something to hide.
    pub fn apply(&mut self, set: &[(u16, u16)]) {
        self.reap();
        if self.child.is_none() {
            if set.is_empty() || self.declined {
                return;
            }
            match spawn_helper() {
                Ok(child) => self.child = Some(child),
                Err(e) => {
                    eprintln!("[linux-hide] couldn't start the helper: {e}");
                    self.declined = true;
                    return;
                }
            }
        }
        let mut line = String::from("SET");
        for (vid, pid) in set {
            line.push_str(&format!(" {vid:04x}:{pid:04x}"));
        }
        line.push('\n');
        let sent = self
            .child
            .as_mut()
            .and_then(|c| c.stdin.as_mut())
            .map(|stdin| stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()));
        if let Some(Err(e)) = sent {
            eprintln!("[linux-hide] helper gone ({e})");
            self.child = None;
        }
    }

    /// Notice a helper that exited: a dismissed prompt, or a failure.
    fn reap(&mut self) {
        let Some(child) = self.child.as_mut() else { return };
        if let Ok(Some(status)) = child.try_wait() {
            eprintln!("[linux-hide] helper exited ({status})");
            self.child = None;
            // pkexec: 126 = the prompt was dismissed, 127 = not authorized.
            self.declined = true;
        }
    }
}

fn spawn_helper() -> std::io::Result<std::process::Child> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    eprintln!("[linux-hide] starting the helper (asks for your password)");
    let mut child = Command::new("pkexec")
        .arg(exe)
        .arg(HELPER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    // Relay what it says to our log.
    if let Some(out) = child.stdout.take() {
        let _ = std::thread::Builder::new().name("hide-helper-log".into()).spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                eprintln!("[linux-hide] {line}");
            }
        });
    }
    Ok(child)
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.is_file())
}

// ── Helper: the root side ───────────────────────────────────────────────────

/// Set by SIGTERM / SIGHUP / SIGINT (a logout, Ctrl+C in FlexInput's
/// terminal): stop, giving everything back first.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// Run as the helper until stdin closes. The process exit code.
pub fn run_helper() -> i32 {
    // SAFETY: geteuid / getppid have no preconditions.
    let (euid, parent) = unsafe { (libc::geteuid(), libc::getppid()) };
    if euid != 0 {
        eprintln!("[linux-hide] the helper must run as root (through pkexec)");
        return 1;
    }
    let Some(uid) = ["PKEXEC_UID", "SUDO_UID"]
        .iter()
        .find_map(|v| std::env::var(v).ok()?.parse::<u32>().ok())
    else {
        eprintln!("[linux-hide] no PKEXEC_UID: start the helper through pkexec");
        return 1;
    };
    recover();
    if let Err(e) = ensure_private_dir() {
        println!("ERROR {PRIVATE_DIR}: {e}");
        return 1;
    }
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            libc::signal(sig, on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    // pkexec execs the helper in its own place, so its parent is FlexInput: its
    // handles on a controller aren't someone else's.
    let mut helper = Helper {
        uid,
        parent,
        set: Vec::new(),
        releasing: Vec::new(),
        evicted: HashSet::new(),
        rule_since: None,
        reported: HashSet::new(),
    };
    let netlink = open_uevent_socket();
    println!("READY");
    let _ = std::io::stdout().flush();

    let mut pending = Vec::<u8>::new();
    let mut buf = [0u8; 8192];
    'serve: while !STOP.load(Ordering::Relaxed) {
        let mut fds = vec![libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 }];
        if let Some(nl) = netlink {
            fds.push(libc::pollfd { fd: nl, events: libc::POLLIN, revents: 0 });
        }
        // SAFETY: valid pollfds. A second's timeout re-checks the hidden nodes,
        // in case something (a session switch) granted one to the user again.
        // A signal interrupts it, and the loop condition sees STOP.
        let timeout = if helper.releasing.is_empty() { 1000 } else { 250 };
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
            continue;
        }
        if fds[0].revents != 0 {
            // SAFETY: reading into our buffer.
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break 'serve;
            }
            pending.extend_from_slice(&buf[..n as usize]);
            while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                helper.command(String::from_utf8_lossy(&line).trim());
            }
        }
        if let Some(nl) = netlink {
            if fds.get(1).is_some_and(|f| f.revents != 0) {
                // Only the wake-up matters; drain what's queued.
                // SAFETY: reading into our buffer from our socket.
                while unsafe { libc::recv(nl, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) } > 0 {}
            }
        }
        helper.expire_releases();
        helper.rescan();
    }
    helper.release_all();
    let _ = std::fs::remove_dir(PRIVATE_DIR);
    println!("stopped; every controller is visible again");
    0
}

struct Helper {
    uid: u32,
    /// FlexInput's pid.
    parent: i32,
    /// What FlexInput asked to hide.
    set: Vec<(u16, u16)>,
    /// Controllers that left the set, hidden until their deadline
    /// ([`RELEASE_GRACE`]).
    releasing: Vec<((u16, u16), Instant)>,
    /// HID devices already re-attached to cut another program off: once each,
    /// or a program that reopens through other means would loop it.
    evicted: HashSet<PathBuf>,
    /// When the rule was last written for a non-empty set: a node that turns
    /// up visible after that slipped past it, which the log says.
    rule_since: Option<Instant>,
    /// Nodes (path, device number) already reported as slipping past the rule.
    reported: HashSet<(String, u64)>,
}

/// A controller node found in sysfs.
struct Node {
    path: String,
    name: String,
    vid: u16,
    pid: u16,
    /// Its sysfs device (the node's `device` link), for udev and eviction.
    sysdev: PathBuf,
    /// FlexInput reads it (hidraw, evdev); js nodes nobody needs back.
    private: bool,
}

impl Helper {
    fn command(&mut self, line: &str) {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("SET") => {
                let set: Vec<(u16, u16)> = words.filter_map(parse_vid_pid).collect();
                println!("SET {} controller(s)", set.len());
                let _ = std::io::stdout().flush();
                self.set_to(set);
            }
            Some(other) => println!("ERROR unknown command {other}"),
            None => {}
        }
    }

    /// What is hidden: the set, and what left it within the grace period.
    fn hidden_set(&self) -> Vec<(u16, u16)> {
        let mut all = self.set.clone();
        all.extend(self.releasing.iter().map(|(v, _)| *v).filter(|v| !self.set.contains(v)));
        all.sort_unstable();
        all
    }

    /// Hide `set`. What leaves it stays hidden for [`RELEASE_GRACE`].
    fn set_to(&mut self, set: Vec<(u16, u16)>) {
        let before = self.hidden_set();
        let deadline = Instant::now() + RELEASE_GRACE;
        for v in &self.set {
            if !set.contains(v) && !self.releasing.iter().any(|(r, _)| r == v) {
                self.releasing.push((*v, deadline));
            }
        }
        // Back in the set: no longer on its way out.
        self.releasing.retain(|(v, _)| !set.contains(v));
        self.set = set;
        self.apply(before);
    }

    /// Show what's past its grace period.
    fn expire_releases(&mut self) {
        let now = Instant::now();
        if self.releasing.iter().all(|(_, until)| *until > now) {
            return;
        }
        let before = self.hidden_set();
        self.releasing.retain(|(_, until)| *until > now);
        self.apply(before);
    }

    /// Show everything, now: the helper is stopping.
    fn release_all(&mut self) {
        let before = self.hidden_set();
        self.set.clear();
        self.releasing.clear();
        self.apply(before);
    }

    /// Bring the rule and the nodes from hiding `before` to hiding
    /// [`Self::hidden_set`]: the rule for new nodes, the nodes that exist now,
    /// and give back what left.
    fn apply(&mut self, before: Vec<(u16, u16)>) {
        let after = self.hidden_set();
        if after == before {
            return;
        }
        match write_rules(&after, self.uid) {
            Ok(()) => {
                self.rule_since = (!after.is_empty()).then(Instant::now);
                println!("RULE {RULES_FILE} for {} controller(s)", after.len());
            }
            Err(e) => println!("ERROR writing {RULES_FILE}: {e}"),
        }
        udevadm(&["control", "--reload"]);
        let dropped: Vec<(u16, u16)> = before.iter().filter(|v| !after.contains(v)).copied().collect();
        give_back(&dropped);
        let fresh = self.rescan();
        self.evict_holders(&fresh);
        let _ = std::io::stdout().flush();
    }

    /// Take away every node of the set the user can still open — made before
    /// the rule, or granted again since — and drop private nodes whose device
    /// is gone. The nodes it took just now.
    fn rescan(&mut self) -> Vec<Node> {
        let set = self.hidden_set();
        let nodes: Vec<Node> = controller_nodes().into_iter().filter(|n| set.contains(&(n.vid, n.pid))).collect();
        let mut fresh: Vec<String> = Vec::new();
        for n in &nodes {
            if revoked(&n.path) {
                // Born hidden by the rule, whose RUN made the private node. If
                // udev's sandbox kept it from that, make it here.
                if n.private && !Path::new(&private_path(&n.name)).exists() {
                    if let Ok(rdev) = node_rdev(&n.path) {
                        if let Err(e) = make_private_node(&n.name, rdev, self.uid) {
                            println!("ERROR private node for {}: {e}", n.name);
                        }
                    }
                }
                continue;
            }
            // Visible though the rule had a moment to apply: say why, once.
            if self.rule_since.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                let key = (n.path.clone(), node_rdev(&n.path).unwrap_or(0));
                if self.reported.insert(key) {
                    println!("SLIPPED {}", slipped_report(n));
                }
            }
            match hide(n, self.uid) {
                Ok(()) => fresh.push(n.path.clone()),
                Err(e) => println!("ERROR hiding {}: {e}", n.path),
            }
        }
        clean_private_dir(&nodes);
        let fresh: Vec<Node> = nodes.into_iter().filter(|n| fresh.contains(&n.path)).collect();
        if !fresh.is_empty() {
            let names: Vec<&str> = fresh.iter().map(|n| n.name.as_str()).collect();
            println!("HIDDEN {}", names.join(" "));
            let _ = std::io::stdout().flush();
        }
        fresh
    }

    /// Cut off any other program that had one of these nodes open before it was
    /// hidden: re-attach its HID device, so every open handle dies and the new
    /// nodes are born hidden. Only joystick and hidraw nodes count: the
    /// desktop holds a DualSense's touchpad, and keeps it.
    fn evict_holders(&mut self, fresh: &[Node]) {
        let watched: Vec<&Node> = fresh.iter().filter(|n| n.name.starts_with("hidraw") || is_joystick(n)).collect();
        if watched.is_empty() {
            return;
        }
        let paths: Vec<&str> = watched.iter().map(|n| n.path.as_str()).collect();
        let holders = holders_of(&paths, self.parent);
        for n in watched {
            let Some((pid, comm)) = holders.iter().find(|(p, _)| *p == n.path).map(|(_, who)| who) else { continue };
            let Some(hid) = hid_device_of(&n.sysdev) else {
                println!("HELD {} by {comm} ({pid}), which can't be cut off (no HID device)", n.name);
                continue;
            };
            if !self.evicted.insert(hid.clone()) {
                continue;
            }
            match reattach(&hid) {
                Ok(()) => println!("EVICTED {comm} ({pid}) from {}", n.name),
                Err(e) => println!("ERROR re-attaching {}: {e}", hid.display()),
            }
        }
        let _ = std::io::stdout().flush();
    }
}

/// Take a node away from the user: a private node for FlexInput first (when
/// the user could open it), then the public one root's alone.
fn hide(n: &Node, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(&n.path)?;
    if !meta.file_type().is_char_device() {
        return Ok(());
    }
    if n.private && user_can_open(uid, meta.uid(), meta.mode(), get_xattr(&n.path, ACL_XATTR).as_deref()) {
        make_private_node(&n.name, meta.rdev(), uid)?;
    }
    revoke(&n.path);
    Ok(())
}

fn private_path(name: &str) -> String {
    format!("{PRIVATE_DIR}/{name}")
}

fn node_rdev(path: &str) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::symlink_metadata(path)?.rdev())
}

/// Whether a public node is root's alone.
fn revoked(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.permissions().mode() & 0o777 == 0o600)
        && get_xattr(path, ACL_XATTR).is_none()
}

/// Make a public node root's alone: no ACL, owner-only mode.
fn revoke(path: &str) {
    let Ok(c) = CString::new(path) else { return };
    let name = CString::new(ACL_XATTR).expect("no NUL");
    // SAFETY: valid paths; ENODATA (no ACL) is fine.
    unsafe {
        libc::removexattr(c.as_ptr(), name.as_ptr());
        libc::chmod(c.as_ptr(), 0o600);
    }
}

/// Give these controllers' nodes back: with the rule gone, a change event makes
/// udev apply its own mode and `uaccess` again.
fn give_back(set: &[(u16, u16)]) {
    if set.is_empty() {
        return;
    }
    let nodes: Vec<String> = controller_nodes()
        .into_iter()
        .filter(|n| set.contains(&(n.vid, n.pid)))
        .map(|n| n.path)
        .collect();
    if nodes.is_empty() {
        return;
    }
    let mut args = vec!["trigger", "--action=change"];
    args.extend(nodes.iter().map(String::as_str));
    udevadm(&args);
    udevadm(&["settle", "--timeout=3"]);
}

/// A helper that was killed left its rule: give back what it hid.
fn recover() {
    let Ok(text) = std::fs::read_to_string(RULES_FILE) else { return };
    let set: Vec<(u16, u16)> = text
        .lines()
        .find_map(|l| l.strip_prefix("# set:"))
        .map(|s| s.split_whitespace().filter_map(parse_vid_pid).collect())
        .unwrap_or_default();
    let _ = std::fs::remove_file(RULES_FILE);
    udevadm(&["control", "--reload"]);
    give_back(&set);
    clean_private_dir(&[]);
    println!("gave back what an earlier run left hidden");
}

/// The rule for `set`, or none for an empty one. It matches the hidraw node by
/// its HID device's name (`0005:057E:2009.0003`) and the evdev / js nodes by
/// their input id, never FlexInput's own UHID devices (bus 0003 under
/// /devices/virtual/misc/uhid; a Bluetooth LE pad is bus 0005 there) nor
/// anyone's uinput devices.
fn write_rules(set: &[(u16, u16)], uid: u32) -> std::io::Result<()> {
    if set.is_empty() {
        return match std::fs::remove_file(RULES_FILE) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(Path::new(RULES_FILE).parent().expect("has a parent"))?;
    let tools = RunTools::find();
    if tools.is_none() {
        println!("ERROR no sh / mkdir / rm / mknod / chown in {TOOL_DIRS:?}: new nodes get FlexInput's copy late");
    }
    std::fs::write(RULES_FILE, rules(set, uid, tools.as_ref()))
}

/// The tools the rule's RUN makes the private node with, by full path: udev
/// runs it with a PATH of its own, which on NixOS has none of them.
struct RunTools {
    sh: String,
    mkdir: String,
    rm: String,
    mknod: String,
    chown: String,
}

impl RunTools {
    fn find() -> Option<Self> {
        Some(RunTools {
            sh: tool("sh")?,
            mkdir: tool("mkdir")?,
            rm: tool("rm")?,
            mknod: tool("mknod")?,
            chown: tool("chown")?,
        })
    }
}

/// `tools: None` leaves the private node to the helper's own rescan.
fn rules(set: &[(u16, u16)], uid: u32, tools: Option<&RunTools>) -> String {
    let ids: String = set.iter().map(|(v, p)| format!(" {v:04x}:{p:04x}")).collect();
    let mut r = format!(
        "# Written by FlexInput's controller-hiding helper; removed when it exits.\n# set:{ids}\n"
    );
    // Never FlexInput's own pads of the same model: UHID ones, and the ones
    // with USB audio, which hang off the kernel's USB/IP host controller.
    for (v, p) in set {
        r.push_str(&format!(
            "SUBSYSTEM==\"hidraw\", KERNELS==\"*:{v:04X}:{p:04X}.*\", \
             DEVPATH!=\"/devices/virtual/misc/uhid/0003:*\", DEVPATH!=\"/devices/platform/vhci_hcd.*\", \
             GOTO=\"flexinput_hide\"\n"
        ));
        r.push_str(&format!(
            "SUBSYSTEM==\"input\", KERNEL==\"event*|js*\", DEVPATH!=\"/devices/virtual/input/*\", \
             DEVPATH!=\"/devices/virtual/misc/uhid/0003:*\", DEVPATH!=\"/devices/platform/vhci_hcd.*\", \
             ATTRS{{id/vendor}}==\"{v:04x}\", ATTRS{{id/product}}==\"{p:04x}\", GOTO=\"flexinput_hide\"\n"
        ));
    }
    r.push_str("GOTO=\"flexinput_hide_end\"\nLABEL=\"flexinput_hide\"\n");
    r.push_str("ACTION!=\"remove\", TAG-=\"uaccess\", MODE=\"0600\"\n");
    if let Some(RunTools { sh, mkdir, rm, mknod, chown }) = tools {
        r.push_str(&format!(
            "ACTION!=\"remove\", KERNEL==\"hidraw*|event*\", RUN+=\"{sh} -c '{mkdir} -p {PRIVATE_DIR} \
             && {rm} -f {PRIVATE_DIR}/%k && {mknod} -m 600 {PRIVATE_DIR}/%k c $major $minor \
             && {chown} {uid} {PRIVATE_DIR}/%k'\"\n"
        ));
    }
    r.push_str("LABEL=\"flexinput_hide_end\"\n");
    r
}

/// Why a node of a hidden controller was visible after the rule had time to
/// apply: its mode, who its ACL grants, and the tags udev left it.
fn slipped_report(n: &Node) -> String {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::symlink_metadata(&n.path) else { return format!("{}: gone", n.name) };
    let users: Vec<u32> = get_xattr(&n.path, ACL_XATTR)
        .map(|acl| {
            acl.get(4..)
                .unwrap_or_default()
                .chunks_exact(8)
                .filter(|e| u16::from_le_bytes([e[0], e[1]]) == 0x02)
                .map(|e| u32::from_le_bytes([e[4], e[5], e[6], e[7]]))
                .collect()
        })
        .unwrap_or_default();
    let (major, minor) = (libc::major(m.rdev() as libc::dev_t), libc::minor(m.rdev() as libc::dev_t));
    let tags: Vec<String> = std::fs::read_to_string(format!("/run/udev/data/c{major}:{minor}"))
        .map(|db| db.lines().filter(|l| l.starts_with("G:") || l.starts_with("Q:")).map(str::to_string).collect())
        .unwrap_or_default();
    format!(
        "{}: visible after the rule (mode {:o}, ACL users {users:?}, udev tags {tags:?}, rule file {})",
        n.name,
        m.mode() & 0o777,
        if Path::new(RULES_FILE).exists() { "present" } else { "missing" },
    )
}

fn udevadm(args: &[&str]) {
    let Some(udevadm) = tool("udevadm") else {
        println!("ERROR udevadm not found in {TOOL_DIRS:?}");
        return;
    };
    match std::process::Command::new(udevadm).args(args).status() {
        Ok(s) if s.success() => {}
        Ok(s) => println!("ERROR udevadm {}: {s}", args.first().copied().unwrap_or("")),
        Err(e) => println!("ERROR udevadm: {e}"),
    }
}

/// `/dev/flexinput` as a root-owned directory, and nothing else (a symlink or
/// someone else's directory refuses).
fn ensure_private_dir() -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    match std::fs::symlink_metadata(PRIVATE_DIR) {
        Ok(m) if m.is_dir() && m.uid() == 0 => {}
        Ok(_) => return Err(std::io::Error::other("exists and isn't root's directory")),
        Err(_) => std::fs::create_dir(PRIVATE_DIR)?,
    }
    std::fs::set_permissions(PRIVATE_DIR, std::fs::Permissions::from_mode(0o755))
}

/// A character node for `rdev` in [`PRIVATE_DIR`], the user's alone.
fn make_private_node(name: &str, rdev: u64, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let path = private_path(name);
    if let Ok(m) = std::fs::symlink_metadata(&path) {
        if m.file_type().is_char_device() && m.rdev() == rdev && m.uid() == uid {
            return Ok(());
        }
        std::fs::remove_file(&path)?;
    }
    let c = CString::new(path).map_err(std::io::Error::other)?;
    // SAFETY: valid path; a character device node for a device the user may
    // already open, owned by them.
    unsafe {
        if libc::mknod(c.as_ptr(), libc::S_IFCHR | 0o600, rdev as libc::dev_t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::lchown(c.as_ptr(), uid, 0) != 0 {
            let e = std::io::Error::last_os_error();
            libc::unlink(c.as_ptr());
            return Err(e);
        }
    }
    Ok(())
}

/// Remove the private nodes no hidden controller node backs any more: its
/// device went away (and its number may go to another device), or it left the
/// set.
fn clean_private_dir(hidden: &[Node]) {
    use std::os::unix::fs::MetadataExt;
    let Ok(dir) = std::fs::read_dir(PRIVATE_DIR) else { return };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let rdev = e.metadata().map(|m| m.rdev()).ok();
        let backed = hidden.iter().any(|n| {
            n.name == name && std::fs::symlink_metadata(&n.path).is_ok_and(|m| Some(m.rdev()) == rdev)
        });
        if !backed {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Every hidraw, evdev and js node of a real controller, with its vendor and
/// product id. Left out: FlexInput's own virtual devices (their phys says so,
/// or for a pad with USB audio its USB serial) and any program's uinput
/// devices (Steam's, other mappers').
fn controller_nodes() -> Vec<Node> {
    use flexinput_core::own_virtual::{sysfs_usb_is_own, PHYS_PREFIX};
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/sys/class/hidraw") {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(sysdev) = std::fs::canonicalize(e.path().join("device")) else { continue };
            let Ok(uevent) = std::fs::read_to_string(sysdev.join("uevent")) else { continue };
            let field = |k: &str| uevent.lines().find_map(|l| l.strip_prefix(k)).map(str::to_string);
            if field("HID_PHYS=").is_some_and(|p| p.starts_with(PHYS_PREFIX)) || sysfs_usb_is_own(&sysdev) {
                continue;
            }
            // HID_ID=0005:0000057E:00002009
            let Some((vid, pid)) = field("HID_ID=").and_then(|id| {
                let mut p = id.split(':').skip(1);
                Some((u16::from_str_radix(p.next()?, 16).ok()?, u16::from_str_radix(p.next()?, 16).ok()?))
            }) else {
                continue;
            };
            out.push(Node { path: format!("/dev/{name}"), name, vid, pid, sysdev, private: true });
        }
    }
    if let Ok(dir) = std::fs::read_dir("/sys/class/input") {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let private = name.starts_with("event");
            if !private && !name.starts_with("js") {
                continue;
            }
            let Ok(sysdev) = std::fs::canonicalize(e.path().join("device")) else { continue };
            if sysdev.to_string_lossy().contains("/devices/virtual/input/") {
                continue;
            }
            let read = |f: &str| std::fs::read_to_string(sysdev.join(f)).map(|s| s.trim().to_string()).ok();
            if read("phys").is_some_and(|p| p.starts_with(PHYS_PREFIX)) || sysfs_usb_is_own(&sysdev) {
                continue;
            }
            let hex = |f: &str| read(f).and_then(|s| u16::from_str_radix(&s, 16).ok());
            let (Some(vid), Some(pid)) = (hex("id/vendor"), hex("id/product")) else { continue };
            out.push(Node { path: format!("/dev/input/{name}"), name, vid, pid, sysdev, private });
        }
    }
    out
}

/// Whether udev took an evdev or js node for a joystick (what games open, as
/// opposed to a pad's touchpad or motion sensors).
fn is_joystick(n: &Node) -> bool {
    use std::os::unix::fs::MetadataExt;
    if n.name.starts_with("js") {
        return true;
    }
    let Ok(m) = std::fs::symlink_metadata(&n.path) else { return false };
    let (major, minor) = (libc::major(m.rdev() as libc::dev_t), libc::minor(m.rdev() as libc::dev_t));
    std::fs::read_to_string(format!("/run/udev/data/c{major}:{minor}"))
        .is_ok_and(|db| db.lines().any(|l| l == "E:ID_INPUT_JOYSTICK=1"))
}

/// Which of `paths` some process other than `parent` (FlexInput) has open:
/// `(path, (pid, process name))`.
fn holders_of(paths: &[&str], parent: i32) -> Vec<(String, (i32, String))> {
    let me = std::process::id() as i32;
    let mut out = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else { return out };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        if pid == parent || pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let target = target.to_string_lossy();
            if let Some(path) = paths.iter().find(|&&path| path == target) {
                let comm = std::fs::read_to_string(p.path().join("comm")).unwrap_or_default();
                out.push((path.to_string(), (pid, comm.trim().to_string())));
            }
        }
    }
    out
}

/// The HID device a node belongs to: its sysfs device, or the closest one
/// above it named like `0005:057E:2009.0003`.
fn hid_device_of(sysdev: &Path) -> Option<PathBuf> {
    sysdev.ancestors().find(|d| d.file_name().and_then(|n| n.to_str()).is_some_and(is_hid_device_name)).map(Path::to_path_buf)
}

fn is_hid_device_name(name: &str) -> bool {
    // BBBB:VVVV:PPPP.NNNN
    let b = name.as_bytes();
    b.len() == 19
        && b[4] == b':'
        && b[9] == b':'
        && b[14] == b'.'
        && b.iter().enumerate().all(|(i, c)| matches!(i, 4 | 9 | 14) || c.is_ascii_hexdigit())
}

/// Detach a HID device from its driver and probe it again: the kernel tears
/// its nodes down (every open handle dies) and makes new ones, which the rule
/// makes hidden.
fn reattach(hid: &Path) -> std::io::Result<()> {
    let name = hid.file_name().and_then(|n| n.to_str()).ok_or_else(|| std::io::Error::other("no name"))?;
    std::fs::write(hid.join("driver/unbind"), name)?;
    std::fs::write("/sys/bus/hid/drivers_probe", name)
}

/// Whether `uid` may read and write a node with this owner, mode and ACL.
fn user_can_open(uid: u32, owner: u32, mode: u32, acl: Option<&[u8]>) -> bool {
    const RW: u16 = 0o6;
    if mode & 0o006 == 0o006 || (owner == uid && mode & 0o600 == 0o600) {
        return true;
    }
    // posix_acl_xattr: a u32 version, then (u16 tag, u16 perm, u32 id) entries.
    let Some(acl) = acl.filter(|a| a.len() >= 4) else { return false };
    const USER: u16 = 0x02;
    const MASK: u16 = 0x10;
    let entries = acl[4..].chunks_exact(8).map(|e| {
        (u16::from_le_bytes([e[0], e[1]]), u16::from_le_bytes([e[2], e[3]]), u32::from_le_bytes([e[4], e[5], e[6], e[7]]))
    });
    let mut user = false;
    let mut mask_ok = true;
    for (tag, perm, id) in entries {
        match tag {
            USER if id == uid => user = perm & RW == RW,
            MASK => mask_ok = perm & RW == RW,
            _ => {}
        }
    }
    user && mask_ok
}

/// A netlink socket that wakes on udev's events (group 2: after its rules ran,
/// so after it granted the node), or `None` (the second's re-check covers it).
fn open_uevent_socket() -> Option<libc::c_int> {
    // SAFETY: plain socket calls on a zeroed sockaddr_nl.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_KOBJECT_UEVENT,
        );
        if fd < 0 {
            return None;
        }
        let mut addr: libc::sockaddr_nl = std::mem::zeroed();
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        addr.nl_groups = 2;
        let len = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        if libc::bind(fd, (&addr as *const libc::sockaddr_nl).cast(), len) != 0 {
            libc::close(fd);
            return None;
        }
        Some(fd)
    }
}

fn get_xattr(path: &str, name: &str) -> Option<Vec<u8>> {
    let (c, n) = (CString::new(path).ok()?, CString::new(name).ok()?);
    let mut buf = vec![0u8; 512];
    // SAFETY: valid path and name; the buffer is ours and its length given.
    let len = unsafe { libc::getxattr(c.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    (len > 0).then(|| {
        buf.truncate(len as usize);
        buf
    })
}

fn parse_vid_pid(s: &str) -> Option<(u16, u16)> {
    let (v, p) = s.split_once(':')?;
    Some((u16::from_str_radix(v, 16).ok()?, u16::from_str_radix(p, 16).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
        let mut v = 2u32.to_le_bytes().to_vec();
        for &(tag, perm, id) in entries {
            v.extend_from_slice(&tag.to_le_bytes());
            v.extend_from_slice(&perm.to_le_bytes());
            v.extend_from_slice(&id.to_le_bytes());
        }
        v
    }

    #[test]
    fn only_a_user_who_could_open_the_node_gets_it() {
        // udev's uaccess: root:root 0600 plus a user entry for the seat's user.
        let granted = acl(&[(0x01, 6, u32::MAX), (0x02, 6, 1000), (0x04, 0, u32::MAX), (0x10, 6, u32::MAX), (0x20, 0, u32::MAX)]);
        assert!(user_can_open(1000, 0, 0o600, Some(&granted)));
        assert!(!user_can_open(1001, 0, 0o600, Some(&granted)), "someone else's grant");
        let masked = acl(&[(0x02, 6, 1000), (0x10, 4, u32::MAX)]);
        assert!(!user_can_open(1000, 0, 0o600, Some(&masked)), "the mask takes write away");
        assert!(!user_can_open(1000, 0, 0o600, None), "root's alone");
        assert!(user_can_open(1000, 1000, 0o600, None), "the user's own");
        assert!(user_can_open(1000, 0, 0o666, None), "anyone's");
    }

    #[test]
    fn the_rule_names_each_controller_both_ways_and_records_the_set() {
        let tools = RunTools {
            sh: "/bin/sh".into(),
            mkdir: "/usr/bin/mkdir".into(),
            rm: "/usr/bin/rm".into(),
            mknod: "/usr/bin/mknod".into(),
            chown: "/usr/bin/chown".into(),
        };
        let r = rules(&[(0x057E, 0x2009)], 1000, Some(&tools));
        // FXI_DUMP_RULES=<path> writes it out, for `udevadm verify <path>`.
        if let Ok(out) = std::env::var("FXI_DUMP_RULES") {
            std::fs::write(out, &r).unwrap();
        }
        assert!(r.contains("# set: 057e:2009\n"), "{r}");
        assert!(r.contains("KERNELS==\"*:057E:2009.*\""), "hidraw by HID device name, upper case");
        assert!(r.contains("ATTRS{id/vendor}==\"057e\", ATTRS{id/product}==\"2009\""), "evdev by input id");
        assert!(r.contains("/usr/bin/chown 1000 /dev/flexinput/%k"), "tools by full path");
        assert!(!rules(&[(0x057E, 0x2009)], 1000, None).contains("RUN+="), "no tools, no RUN");
        assert!(r.contains("DEVPATH!=\"/devices/virtual/misc/uhid/0003:*\""), "never our own UHID pads");
        assert!(r.contains("DEVPATH!=\"/devices/platform/vhci_hcd.*\""), "nor the ones with USB audio");
        // The set line reads back.
        let back: Vec<(u16, u16)> = r
            .lines()
            .find_map(|l| l.strip_prefix("# set:"))
            .map(|s| s.split_whitespace().filter_map(parse_vid_pid).collect())
            .unwrap();
        assert_eq!(back, [(0x057E, 0x2009)]);
    }

    #[test]
    fn hid_devices_are_found_above_their_nodes() {
        assert!(is_hid_device_name("0005:057E:2009.0003"));
        assert!(!is_hid_device_name("input12"));
        let ev = Path::new("/sys/devices/pci0000:00/usb1/1-1/1-1:1.0/0003:054C:0CE6.0007/input/input31");
        assert_eq!(
            hid_device_of(ev).as_deref(),
            Some(Path::new("/sys/devices/pci0000:00/usb1/1-1/1-1:1.0/0003:054C:0CE6.0007"))
        );
        assert_eq!(hid_device_of(Path::new("/sys/devices/pci0000:00/usb1/1-2/input/input4")), None, "xpad: no HID");
    }

    #[test]
    fn a_public_node_stays_put_while_it_opens() {
        let null = CString::new("/dev/null").unwrap();
        assert_eq!(reachable(&null), null);
    }

    #[test]
    fn only_controller_nodes_have_a_private_twin() {
        let twin = |p: &str| {
            let mut buf = [0u8; 64];
            private_twin(p.as_bytes(), &mut buf).map(|c| c.to_str().unwrap().to_string())
        };
        assert_eq!(twin("/dev/hidraw3").as_deref(), Some("/dev/flexinput/hidraw3"));
        assert_eq!(twin("/dev/input/event17").as_deref(), Some("/dev/flexinput/event17"));
        assert_eq!(twin("/dev/input/js0"), None, "the helper makes none for js");
        assert_eq!(twin("/dev/input/by-id/usb-pad-event-joystick"), None);
        assert_eq!(twin("/dev/null"), None);
        assert_eq!(twin("/dev/hidraw3/../../etc/shadow"), None);
        assert_eq!(twin("/home/u/hidraw3"), None);
        assert_eq!(twin(&format!("/dev/hidraw{}", "9".repeat(60))), None, "too long for the buffer");
    }

    #[test]
    fn open_or_private_is_open_for_everything_else() {
        let null = CString::new("/dev/null").unwrap();
        // SAFETY: valid paths; the fds are closed below.
        let fd = unsafe { open_or_private(null.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC, 0) };
        assert!(fd >= 0);
        unsafe { libc::close(fd) };
        let missing = CString::new("/dev/hidraw-flexinput-test-missing").unwrap();
        let fd = unsafe { open_or_private(missing.as_ptr(), libc::O_RDONLY, 0) };
        assert_eq!(fd, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ENOENT), "not retried, errno kept");
    }

    #[test]
    fn ids_parse() {
        assert_eq!(parse_vid_pid("057e:2009"), Some((0x057E, 0x2009)));
        assert_eq!(parse_vid_pid("nope"), None);
    }
}
