//! Verification probe for per-machine driver signing (`signing.rs`).
//!
//! The approach was proven with PowerShell (`New-FileCatalog` + attribute
//! patching + `Set-AuthenticodeSignature`) against real `pnputil`. This probe
//! exercises the Rust implementation of the same steps before `deploy.rs`
//! depends on it.
//!
//! ```text
//! hm_sign_probe catalog <out.cat> <hwid[,hwid...]> <file>...
//!     Build an UNSIGNED catalog. No cert, no elevation. Diff its members and
//!     attributes against an Inf2Cat-built catalog.
//!
//! hm_sign_probe user-selftest <dir> <inf> <binary> [--keep]
//!     Whole pipeline in CurrentUser scope, no elevation, no machine state:
//!     create a probe cert, sign <binary>, build + sign <dir>\probe.cat, confirm
//!     both signatures name the probe cert. Deletes cert + key unless --keep.
//! hm_sign_probe user-cleanup
//!     Delete any CurrentUser probe cert + key left by --keep.
//!
//! hm_sign_probe machine-test <dir>      (ELEVATED)
//!     <dir> holds an isolated package: fxcatspike.inf (hardware id
//!     root\FXCATSPIKE, CatalogFile=fxcatspike.cat) + HIDMaestro.dll.
//!     Negative control first (unsigned catalog must be rejected), then sign
//!     with a LocalMachine probe cert, trust it, `pnputil /add-driver` without
//!     /install, check the DriverStore. Teardown always runs.
//!     Log: <dir>\..\hm_sign_probe.log
//! ```
//!
//! Build: `cargo build -p flexinput-hidmaestro --features sign-probe-bin --bin hm_sign_probe`

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use flexinput_hidmaestro::signing::{
    add_to_store, build_catalog, delete_cert_and_key, ensure_cert_named, hex, remove_from_store,
    sign_driver_package, signer_thumbprint, DriverPackage, Scope, Thumbprint,
};

/// Distinct from the production cert name, so the probe can never find, reuse
/// or delete the real per-machine cert.
const PROBE_CERT_NAME: &str = "FlexInput Signing Probe";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rest = &args[2.min(args.len())..];
    let code = match args.get(1).map(String::as_str) {
        Some("catalog") => run_catalog(rest),
        Some("user-selftest") => run_user_selftest(rest),
        Some("user-cleanup") => run_user_cleanup(),
        Some("machine-test") => run_machine_test(rest),
        _ => {
            eprintln!("usage: see the header of src/bin/hm_sign_probe.rs");
            2
        }
    };
    std::process::exit(code);
}

fn run_catalog(args: &[String]) -> i32 {
    if args.len() < 3 {
        eprintln!("catalog <out.cat> <hwid[,hwid...]> <file>...");
        return 2;
    }
    let out = PathBuf::from(&args[0]);
    let hwids: Vec<&str> = args[1].split(',').filter(|s| !s.is_empty()).collect();
    let files: Vec<PathBuf> = args[2..].iter().map(PathBuf::from).collect();
    match build_catalog(&out, &hwids, &files) {
        Ok(()) => {
            println!("OK: unsigned catalog written to {}", out.display());
            0
        }
        Err(e) => {
            println!("FAILED: {e}");
            1
        }
    }
}

fn run_user_selftest(args: &[String]) -> i32 {
    if args.len() < 3 {
        eprintln!("user-selftest <dir> <inf> <binary> [--keep]");
        return 2;
    }
    let dir = PathBuf::from(&args[0]);
    let (inf, binary) = (args[1].as_str(), args[2].as_str());
    let keep = args.iter().any(|a| a == "--keep");

    let cert = match ensure_cert_named(Scope::CurrentUser, PROBE_CERT_NAME) {
        Ok(c) => c,
        Err(e) => {
            println!("FAILED creating/finding CurrentUser probe cert: {e}");
            return 1;
        }
    };
    let thumb = match cert.thumbprint() {
        Ok(t) => t,
        Err(e) => {
            println!("FAILED reading thumbprint: {e}");
            return 1;
        }
    };
    println!("cert    : CN={PROBE_CERT_NAME}  {}", hex(&thumb));

    let pkg = DriverPackage { inf, binaries: &[binary], catalog: "probe.cat", hardware_ids: &["root\\HIDMaestro"] };
    let mut ok = true;
    match sign_driver_package(&dir, &pkg, &cert) {
        Ok(()) => println!("package : signed binary, built + signed probe.cat"),
        Err(e) => {
            println!("FAILED sign_driver_package: {e}");
            ok = false;
        }
    }
    if ok {
        for file in [binary, "probe.cat"] {
            let path = dir.join(file);
            match signer_thumbprint(&path) {
                Ok(Some(t)) if t == thumb => println!("verify  : {file} signed by the probe cert"),
                Ok(Some(t)) => {
                    println!("MISMATCH: {file} signed by {} (expected {})", hex(&t), hex(&thumb));
                    ok = false;
                }
                Ok(None) => {
                    println!("MISSING : {file} carries no signature");
                    ok = false;
                }
                Err(e) => {
                    println!("FAILED reading {file} signer: {e}");
                    ok = false;
                }
            }
        }
    }
    drop(cert);
    if keep {
        println!("kept    : cert + key left in CurrentUser\\My (run user-cleanup when done)");
    } else {
        report_delete(Scope::CurrentUser, &thumb);
    }
    println!("{}", if ok { "RESULT: PASS" } else { "RESULT: FAIL" });
    if ok { 0 } else { 1 }
}

fn run_user_cleanup() -> i32 {
    // ensure_cert_named would CREATE one if none exists, so find by enumeration
    // of what a previous run left: repeatedly look it up without creating.
    let mut removed = 0;
    while let Some(thumb) = find_probe_thumbprint(Scope::CurrentUser) {
        match delete_cert_and_key(Scope::CurrentUser, &thumb) {
            Ok(true) => removed += 1,
            _ => break,
        }
    }
    println!("removed {removed} CurrentUser probe cert(s)");
    0
}

/// Thumbprint of an existing probe cert in `scope`'s `My`, via PowerShell-free
/// lookup: `certutil` is in-box and read-only here.
fn find_probe_thumbprint(scope: Scope) -> Option<Thumbprint> {
    let mut cmd = Command::new("certutil");
    if scope == Scope::CurrentUser {
        cmd.arg("-user");
    }
    let out = cmd.args(["-store", "My", PROBE_CERT_NAME]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|l| {
        let (_, value) = l.split_once(':')?;
        if !l.to_ascii_lowercase().contains("sha1") {
            return None;
        }
        let digits: String = value.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        parse_thumbprint(&digits)
    })
}

fn parse_thumbprint(hex_digits: &str) -> Option<Thumbprint> {
    if hex_digits.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex_digits[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn report_delete(scope: Scope, thumb: &Thumbprint) {
    match delete_cert_and_key(scope, thumb) {
        Ok(true) => println!("cleanup : deleted probe cert + key from {scope:?}\\My"),
        Ok(false) => println!("cleanup : probe cert already gone"),
        Err(e) => println!("cleanup FAILED: {e}"),
    }
}

// ── Elevated pnputil test ───────────────────────────────────────────────────

const SPIKE_INF: &str = "fxcatspike.inf";
const SPIKE_CAT: &str = "fxcatspike.cat";
const SPIKE_BIN: &str = "HIDMaestro.dll";

struct Log(Option<std::fs::File>);

impl Log {
    fn line(&mut self, s: &str) {
        println!("{s}");
        if let Some(f) = self.0.as_mut() {
            let _ = writeln!(f, "{s}");
        }
    }
}

/// Undoes everything the machine test changed, on every exit path — including
/// a panic partway through.
struct Teardown<'a> {
    log: &'a mut Log,
    published: Option<String>,
    thumbprint: Option<Thumbprint>,
}

impl Drop for Teardown<'_> {
    fn drop(&mut self) {
        self.log.line("");
        self.log.line("=== TEARDOWN ===");
        if let Some(name) = self.published.take() {
            let (rc, out) = pnputil(&["/delete-driver", &name]);
            self.log.line(&format!("   delete-driver {name} exit={rc}"));
            for l in out.lines().filter(|l| !l.trim().is_empty()) {
                self.log.line(&format!("   | {}", l.trim()));
            }
        } else {
            self.log.line("   (nothing published)");
        }
        if let Some(thumb) = self.thumbprint.take() {
            for store in ["ROOT", "TrustedPublisher"] {
                match remove_from_store(Scope::LocalMachine, store, &thumb) {
                    Ok(true) => self.log.line(&format!("   removed probe cert from LocalMachine\\{store}")),
                    Ok(false) => self.log.line(&format!("   not in LocalMachine\\{store}")),
                    Err(e) => self.log.line(&format!("   FAILED removing from {store}: {e}")),
                }
            }
            match delete_cert_and_key(Scope::LocalMachine, &thumb) {
                Ok(true) => self.log.line("   deleted probe cert + key from LocalMachine\\My"),
                Ok(false) => self.log.line("   probe cert not in LocalMachine\\My"),
                Err(e) => self.log.line(&format!("   FAILED deleting cert/key: {e}")),
            }
        }
        self.log.line("");
        self.log.line("=== FINAL STATE (must equal baseline) ===");
        for name in driverstore_hidmaestro_like() {
            self.log.line(&format!("   {name}"));
        }
        let left = find_probe_thumbprint(Scope::LocalMachine).is_some();
        self.log.line(&format!("   probe cert remaining in LocalMachine\\My: {left}"));
    }
}

fn run_machine_test(args: &[String]) -> i32 {
    let Some(dir) = args.first().map(PathBuf::from) else {
        eprintln!("machine-test <dir>");
        return 2;
    };
    let log_path = dir.parent().unwrap_or(&dir).join("hm_sign_probe.log");
    let mut log = Log(std::fs::File::create(&log_path).ok());
    log.line(&format!("hm_sign_probe machine-test  dir={}", dir.display()));

    for f in [SPIKE_INF, SPIKE_BIN] {
        if !dir.join(f).exists() {
            log.line(&format!("ABORT: {} missing", dir.join(f).display()));
            return 2;
        }
    }

    log.line("=== PHASE 0: baseline ===");
    let before = driverstore_hidmaestro_like();
    for name in &before {
        log.line(&format!("   {name}"));
    }

    let outcome = {
        let mut td = Teardown { log: &mut log, published: None, thumbprint: None };
        machine_phases(&dir, &before, &mut td)
    };
    log.line(&format!("\nRESULT: {}", if outcome { "PASS" } else { "FAIL" }));
    if outcome { 0 } else { 1 }
}

fn machine_phases(dir: &Path, before: &[String], td: &mut Teardown) -> bool {
    let inf = dir.join(SPIKE_INF);
    let cat = dir.join(SPIKE_CAT);
    let binary = dir.join(SPIKE_BIN);

    td.log.line("");
    td.log.line("=== PHASE 1: create LocalMachine probe cert (fails here if not elevated) ===");
    let cert = match ensure_cert_named(Scope::LocalMachine, PROBE_CERT_NAME) {
        Ok(c) => c,
        Err(e) => {
            td.log.line(&format!("   FAILED: {e}  (run from an elevated prompt)"));
            return false;
        }
    };
    let thumb = match cert.thumbprint() {
        Ok(t) => t,
        Err(e) => {
            td.log.line(&format!("   FAILED thumbprint: {e}"));
            return false;
        }
    };
    td.thumbprint = Some(thumb);
    td.log.line(&format!("   cert CN={PROBE_CERT_NAME}  {}", hex(&thumb)));

    td.log.line("");
    td.log.line("=== PHASE 2: NEGATIVE CONTROL - unsigned catalog ===");
    if let Err(e) = build_catalog(&cat, &["root\\FXCATSPIKE"], &[inf.clone(), binary.clone()]) {
        td.log.line(&format!("   FAILED building control catalog: {e}"));
        return false;
    }
    let (rc, out) = pnputil(&["/add-driver", &inf.to_string_lossy()]);
    td.log.line(&format!("   exit={rc}"));
    for l in out.lines().filter(|l| !l.trim().is_empty()) {
        td.log.line(&format!("   | {}", l.trim()));
    }
    let control_published = published_name(&out);
    if let Some(name) = &control_published {
        td.log.line(&format!("   !! unsigned package WAS accepted as {name} - control failed"));
        td.published = Some(name.clone());
        return false;
    }

    td.log.line("");
    td.log.line("=== PHASE 3: sign binary, build + sign catalog, trust cert ===");
    let pkg = DriverPackage { inf: SPIKE_INF, binaries: &[SPIKE_BIN], catalog: SPIKE_CAT, hardware_ids: &["root\\FXCATSPIKE"] };
    if let Err(e) = sign_driver_package(dir, &pkg, &cert) {
        td.log.line(&format!("   FAILED sign_driver_package: {e}"));
        return false;
    }
    for file in [SPIKE_BIN, SPIKE_CAT] {
        let signer = signer_thumbprint(&dir.join(file));
        let good = matches!(signer, Ok(Some(t)) if t == thumb);
        td.log.line(&format!("   {file}: signed by probe cert = {good}"));
        if !good {
            return false;
        }
    }
    for store in ["ROOT", "TrustedPublisher"] {
        if let Err(e) = add_to_store(Scope::LocalMachine, store, cert.der()) {
            td.log.line(&format!("   FAILED trusting in {store}: {e}"));
            return false;
        }
        td.log.line(&format!("   trusted in LocalMachine\\{store}"));
    }

    td.log.line("");
    td.log.line("=== PHASE 4: add-driver with the Rust-signed catalog ===");
    let (rc, out) = pnputil(&["/add-driver", &inf.to_string_lossy()]);
    td.log.line(&format!("   exit={rc}"));
    for l in out.lines().filter(|l| !l.trim().is_empty()) {
        td.log.line(&format!("   | {}", l.trim()));
    }
    let Some(name) = published_name(&out) else {
        td.log.line("   not published");
        return false;
    };
    td.published = Some(name.clone());
    td.log.line(&format!("   published as {name}"));

    td.log.line("");
    td.log.line("=== PHASE 5: DriverStore ===");
    let after = driverstore_hidmaestro_like();
    for n in &after {
        td.log.line(&format!("   {n}"));
    }
    let staged: Vec<&String> = after.iter().filter(|n| !before.contains(n)).collect();
    td.log.line(&format!("   newly staged: {staged:?}"));
    staged.iter().any(|n| n.starts_with("fxcatspike.inf_"))
}

fn pnputil(args: &[&str]) -> (i32, String) {
    let exe = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("pnputil.exe");
    match Command::new(exe).args(args).output() {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        ),
        Err(e) => (-1, format!("could not run pnputil: {e}")),
    }
}

fn published_name(pnputil_output: &str) -> Option<String> {
    pnputil_output.split_whitespace().find_map(|w| {
        let lower = w.to_ascii_lowercase();
        (lower.starts_with("oem") && lower.ends_with(".inf") && lower[3..lower.len() - 4].chars().all(|c| c.is_ascii_digit()))
            .then_some(lower)
    })
}

fn driverstore_hidmaestro_like() -> Vec<String> {
    let repo = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join(r"System32\DriverStore\FileRepository");
    let mut names: Vec<String> = std::fs::read_dir(repo)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    let l = n.to_ascii_lowercase();
                    l.starts_with("hidmaestro") || l.starts_with("fxcatspike")
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
