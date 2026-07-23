//! Phase-1 verification probe for the HIDMaestro shared-memory client.
//!
//! This is throwaway scaffolding for the Phase-1 gate — it deliberately does
//! NOT reimplement HIDMaestro's report encoder (that's Phase 2). Instead it
//! replays a **captured raw input report** (bytes you grab from HIDMaestro's own
//! C# app for a known controller state) and optionally ramps one byte, so the
//! gate exercises *only* the Rust SHM transport: does our seqlock writer deliver
//! frames the driver accepts as cleanly as the C# writer does?
//!
//! Workflow for the gate:
//!   1. Run HIDMaestro's C# test app to CREATE a device (it owns the section).
//!      e.g. `HIDMaestroTest emulate xbox-360-wired` (keep it running, or use a
//!      build that creates the device then idles without writing).
//!   2. Capture one report's bytes from the C# side (a tiny dump patch in
//!      WriteInputFrame, or known from the descriptor) for, say, sticks centered.
//!   3. Run this probe to OPEN that section and drive it:
//!      `hm_shm_probe input  --index 0 --report <hex> [--ramp-offset N]`
//!      `hm_shm_probe output --index 0`
//!   4. Watch a gamepad tester (joy.cpl / Gamepad Tester): the ramped axis must
//!      move smoothly with NO jitter/tearing over 60+ seconds.
//!
//! Build: `cargo run -p flexinput-hidmaestro --features probe-bin --bin hm_shm_probe -- ...`

use std::time::{Duration, Instant};

use flexinput_hidmaestro::encode::{encode_report, GamepadState};
use flexinput_hidmaestro::profile::presets::DUALSHOCK_4_V2_JSON;
use flexinput_hidmaestro::{
    create_device_node, remove_device_node, InputSection, OutputSection, Profile,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");

    match mode {
        "input" => run_input(&args[2..]),
        "output" => run_output(&args[2..]),
        "create-input" => run_create_input(&args[2..]),
        "dump" => run_dump(&args[2..]),
        "preset" => run_preset(&args[2..]),
        "create" => run_create(&args[2..]),
        "destroy" => run_destroy(&args[2..]),
        "info" => run_info(),
        "helper-call" => run_helper_call(&args[2..]),
        _ => {
            eprintln!(
                "usage:\n  \
                 hm_shm_probe input  --index <N> --report <hexbytes> [--ramp-offset <byte>] [--rate-hz <hz>]\n  \
                 hm_shm_probe output --index <N>\n  \
                 hm_shm_probe create-input --index <N> --report <hexbytes>   (elevated; creates the section itself)\n\n\
                 <hexbytes>: a captured raw input report, e.g. 00800080800000  (no 0x, no spaces or with).\n\
                 --ramp-offset: index into the report whose byte is swept 0..255 to make motion visible."
            );
            std::process::exit(2);
        }
    }
}

fn arg<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn parse_index(args: &[String]) -> u32 {
    arg(args, "--index").and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn parse_report(args: &[String]) -> Vec<u8> {
    let hex = arg(args, "--report").unwrap_or("00800080800000");
    let cleaned: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if !cleaned.len().is_multiple_of(2) {
        eprintln!("--report must have an even number of hex digits");
        std::process::exit(2);
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).unwrap())
        .collect()
}

fn run_input(args: &[String]) {
    let index = parse_index(args);
    let base = parse_report(args);
    let ramp_offset: Option<usize> = arg(args, "--ramp-offset").and_then(|s| s.parse().ok());
    let rate_hz: f64 = arg(args, "--rate-hz").and_then(|s| s.parse().ok()).unwrap_or(250.0);
    let period = Duration::from_secs_f64(1.0 / rate_hz);

    let mut section = match InputSection::open(index) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open input section {index} failed: {e}");
            eprintln!("(is HIDMaestro's C# app running and did it create this controller index?)");
            std::process::exit(1);
        }
    };

    println!(
        "driving input section {index} @ {rate_hz} Hz, {}-byte report{}. Ctrl-C to stop.",
        base.len(),
        match ramp_offset {
            Some(o) => format!(", ramping byte[{o}] 0..255"),
            None => " (static)".into(),
        }
    );

    let start = Instant::now();
    let mut report = base.clone();
    let mut last_log = Instant::now();
    let mut frames: u64 = 0;
    loop {
        if let Some(o) = ramp_offset {
            if o < report.len() {
                // Triangle ramp ~0.5 Hz so motion is obvious but not frantic.
                let t = start.elapsed().as_secs_f64() * 0.5;
                let tri = (t.fract() * 2.0 - 1.0).abs(); // 0..1..0
                report[o] = (tri * 255.0) as u8;
            }
        }
        section.write_frame(&report, None);
        frames += 1;

        if last_log.elapsed() >= Duration::from_secs(2) {
            println!("  {frames} frames written ({}s elapsed)", start.elapsed().as_secs());
            last_log = Instant::now();
        }
        std::thread::sleep(period);
    }
}

fn run_create_input(args: &[String]) {
    let index = parse_index(args);
    let base = parse_report(args);
    let mut section = match InputSection::create(index) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("create input section {index} failed: {e}");
            eprintln!("(creating Global\\ sections needs elevation — run as Administrator)");
            std::process::exit(1);
        }
    };
    println!("created input section {index}; writing static report. Ctrl-C to stop.");
    loop {
        section.write_frame(&base, None);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Phase-4 client: talk to an already-running elevated helper over the named
/// pipe. `--op ping|ensure|create|destroy`. For create, uses the DS4-v2 preset.
/// (Unelevated — the helper does the privileged work.)
fn run_helper_call(args: &[String]) {
    use flexinput_hidmaestro::helper_ipc::{HelperClient, Request, Response};
    let op = arg(args, "--op").unwrap_or("ping");
    let index = parse_index(args);
    let mut client = match HelperClient::connect(3000) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect to helper failed: {e} (is hidmaestro_helper running elevated?)");
            std::process::exit(1);
        }
    };
    let req = match op {
        "ping" => Request::Ping,
        "ensure" => Request::EnsureDriver,
        "create" => {
            let profile_json = match arg(args, "--profile").unwrap_or("ds4") {
                "dualsense" => flexinput_hidmaestro::profile::presets::DUALSENSE_JSON,
                _ => DUALSHOCK_4_V2_JSON,
            };
            let device_id = arg(args, "--device-id")
                .unwrap_or(if arg(args, "--profile") == Some("dualsense") {
                    "virtual.hm.dualsense"
                } else {
                    "virtual.hm.ds4"
                })
                .to_string();
            Request::Create { device_id, profile_json: profile_json.to_string(), index_hint: index, poll_interval_ms: 0 }
        }
        "destroy" => Request::Destroy {
            instance_id: arg(args, "--id").unwrap_or(r"ROOT\HIDClass\0000").to_string(),
        },
        "shutdown" => Request::Shutdown,
        other => {
            eprintln!("unknown --op {other}");
            std::process::exit(2);
        }
    };
    match client.call(&req) {
        Ok(resp) => {
            println!("RESP: {resp:?}");
            if let Response::Error { .. } = resp {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("call failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Report driver availability + discovered INF path (no elevation needed).
fn run_info() {
    let avail = flexinput_hidmaestro::hidmaestro_available();
    let inf = flexinput_hidmaestro::installed_inf_path();
    println!("hidmaestro_available = {avail}");
    // Distinguishes a half-installed DriverStore from a clean absence — both
    // report `available = false`, but only the latter is fixed by installing.
    println!("driver_state         = {:?}", flexinput_hidmaestro::driver_state());
    println!("packages_to_remove   = {:?}", flexinput_hidmaestro::installed_inf_names());
    match inf {
        Some(p) => println!("installed_inf_path   = {}", p.display()),
        None => println!("installed_inf_path   = (not found)"),
    }
}

/// Phase-3a gate: create a DS4-v2 device node from Rust (elevated). Pre-creates
/// the input section first (so the driver can open it), creates the devnode,
/// binds the driver, and prints the instance id for teardown.
fn run_create(args: &[String]) {
    let index = parse_index(args);
    // Prefer an explicit --inf; otherwise discover the published HIDMaestro INF.
    let discovered = flexinput_hidmaestro::installed_inf_path();
    let inf: String = match arg(args, "--inf") {
        Some(s) => s.to_string(),
        None => match &discovered {
            Some(p) => p.display().to_string(),
            None => {
                eprintln!("no HIDMaestro INF found in %SystemRoot%\\INF; pass --inf or install the driver");
                std::process::exit(1);
            }
        },
    };
    let profile = Profile::from_json(DUALSHOCK_4_V2_JSON).expect("DS4v2 profile");

    // Pre-create the Global\ sections (elevated) so the driver can OpenFileMapping
    // them once it binds.
    let _input = InputSection::create(index).unwrap_or_else(|e| {
        eprintln!("create input section failed: {e} (run elevated)");
        std::process::exit(1);
    });
    let _output = OutputSection::create(index);

    match create_device_node(&profile, &inf, index, "probe") {
        Ok(dev) => {
            println!("CREATED instance_id={} controller_index={}", dev.instance_id, dev.controller_index);
            println!("(keeping section handles alive; Ctrl-C after verifying. inf={inf})");
            // Keep the process (and section handles) alive so the device stays
            // bound and the section mapped while we verify externally.
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        Err(e) => {
            eprintln!("CREATE FAILED: {e}");
            std::process::exit(1);
        }
    }
}

/// Phase-3a teardown: remove a device node by instance id (elevated).
fn run_destroy(args: &[String]) {
    let id = match arg(args, "--id") {
        Some(s) => s,
        None => {
            eprintln!("usage: hm_shm_probe destroy --id <ROOT\\HIDClass\\NNNN>");
            std::process::exit(2);
        }
    };
    match remove_device_node(id) {
        Ok(gone) => println!("REMOVE {id} -> gone={gone}"),
        Err(e) => {
            eprintln!("REMOVE FAILED: {e}");
            std::process::exit(1);
        }
    }
}

/// Drive a DS4-v2 device using the REAL Phase-2 encoder, sweeping left-stick X.
/// Proves the descriptor parser + encoder produce bytes the driver accepts as a
/// correct DS4 report (plain-HID `Data[]` path — no XUSB companion).
fn run_preset(args: &[String]) {
    let index = parse_index(args);
    let what = arg(args, "--sweep").unwrap_or("lx"); // lx|ly|rx|ry|lt|rt
    let profile = Profile::from_json(DUALSHOCK_4_V2_JSON).expect("DS4v2 profile");
    let mut section = match InputSection::open(index) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open input section {index} failed: {e}");
            std::process::exit(1);
        }
    };
    // Print the neutral report once so it can be diffed against the C# capture.
    let neutral = encode_report(&profile, &GamepadState::neutral());
    println!("DS4v2 neutral report ({} bytes): {:02x?}", neutral.len(), neutral);
    println!("sweeping '{what}' on section {index}. Ctrl-C to stop.");

    let start = Instant::now();
    loop {
        let t = start.elapsed().as_secs_f64() * 0.5;
        let tri = (t.fract() * 2.0 - 1.0).abs() as f32; // 0..1..0
        let mut st = GamepadState::neutral();
        match what {
            "lx" => st.left_stick_x = tri,
            "ly" => st.left_stick_y = tri,
            "rx" => st.right_stick_x = tri,
            "ry" => st.right_stick_y = tri,
            "lt" => st.left_trigger = tri,
            "rt" => st.right_trigger = tri,
            _ => st.left_stick_x = tri,
        }
        let rep = encode_report(&profile, &st);
        section.write_frame(&rep, None);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Poll the input section and print the Data bytes whenever they change — used
/// to CAPTURE the exact report layout the C# app writes (run the C# emulate
/// pattern, watch what bytes correspond to a known stick position).
fn run_dump(args: &[String]) {
    let index = parse_index(args);
    let section = match InputSection::open(index) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open input section {index} failed: {e}");
            std::process::exit(1);
        }
    };
    println!("dumping input section {index} on change (run the C# emulate pattern). Ctrl-C to stop.");
    let mut last: Vec<u8> = Vec::new();
    loop {
        let (seq, len, data, ext) = section.debug_snapshot();
        if data != last {
            println!("  seq={seq} len={len} ext={ext} data={data:02x?}");
            last = data;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn run_output(args: &[String]) {
    let index = parse_index(args);
    // Replay mode: start at last_seq=0 so any frames the driver has EVER written
    // to this ring are shown, not just ones after we attached.
    let mut section = match OutputSection::open_replay(index) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open output section {index} failed: {e}");
            eprintln!("(is the virtual device deployed at THIS index? try --index 1)");
            std::process::exit(1);
        }
    };
    let (head0, _) = section.debug_head();
    println!("polling output ring {index} every 8ms. Trigger rumble in a tester. Ctrl-C to stop.");
    println!("  [diag] initial ring Head = {head0}  \
              (0 = driver has NEVER written output to this section)");
    let mut last_head = head0;
    let mut last_tick = Instant::now();
    let mut total = 0u64;
    loop {
        let mut drained = 0;
        while let Some(f) = section.try_read() {
            println!(
                "  output: source={} report_id={:#04x} len={} data={:02x?}",
                f.source,
                f.report_id,
                f.data.len(),
                &f.data[..f.data.len().min(16)]
            );
            total += 1;
            drained += 1;
            if drained > 256 {
                break;
            }
        }
        // Heartbeat: report Head movement even when try_read is conservative, so
        // silence is interpretable. Head climbing = driver IS writing.
        if last_tick.elapsed() >= Duration::from_secs(2) {
            let (head, _) = section.debug_head();
            if head != last_head {
                println!("  [diag] ring Head {last_head} -> {head} (driver wrote {} frame(s); {total} shown)",
                    head.wrapping_sub(last_head));
                last_head = head;
            } else {
                println!("  [diag] ring Head unchanged at {head} (no driver output in last 2s)");
            }
            last_tick = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}
