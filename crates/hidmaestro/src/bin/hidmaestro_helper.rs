//! Standalone elevated HIDMaestro helper binary.
//!
//! Thin wrapper around [`flexinput_hidmaestro::run_helper_server`]. The shipping
//! app no longer uses this exe — it re-execs *itself* with `--hidmaestro-helper`
//! and calls `run_helper_server` directly (one binary, see `app/src/main.rs`).
//! This bin is kept for the Phase-4 gate and manual testing.
//!
//! Usage: `hidmaestro_helper [--parent-pid N] [--persist]`
//!
//! Build: `cargo build -p flexinput-hidmaestro --features helper-bin --bin hidmaestro_helper`

#![cfg(windows)]

fn main() {
    let mut parent_pid: Option<u32> = None;
    let mut persist = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--parent-pid" => parent_pid = args.next().and_then(|s| s.parse().ok()),
            "--persist" => persist = true,
            _ => {}
        }
    }
    flexinput_hidmaestro::run_helper_server(parent_pid, persist);
}
