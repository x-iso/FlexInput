//! Guided IMU field-mapping probe — both halves, one sweep per axis.
//!
//! The motion block's packing is documented as "unknown", the published layout
//! for reports 0x07/0x08 is wrong over BLE, and the offsets currently in
//! `flexinput-joycon2` came from an AT-REST capture. That can locate the
//! accelerometer, because gravity is a constant-magnitude vector, but it can
//! never locate the gyro: a stationary gyro reads zero and is indistinguishable
//! from padding.
//!
//! # Method
//!
//! One continuous sweep per axis, and the two sensors are separated
//! afterwards by analysis rather than by asking the operator to hold still at
//! precise moments:
//!
//! * **Accelerometer** is found by physics. Gravity has constant magnitude, so
//!   the accel triple is the one set of three fields whose vector length stays
//!   at 1 g (**4096 LSB**) through every orientation. Every candidate triple is
//!   tested and scored on how constant its magnitude is; nothing else in the
//!   report can fake that.
//! * **Gyro** is then whatever responds strongly to exactly ONE sweep and is
//!   quiet in the others — angular rate about one axis, by definition.
//!
//! Because a sweep goes one way and then the other, each gyro field's minimum
//! and maximum are the two rotation directions, which recovers its **sign**.
//!
//! Both halves are recorded during the same physical motion. Clipped into the
//! grip they are rigidly coupled and cannot rotate at different rates, so a
//! genuine gyro axis must respond on both — and their neutral accelerometer
//! readings differ only by the fixed mounting rotation, which is exactly the
//! pivot offset a shared frame has to correct for.
//!
//! Usage:
//!   cargo run -p flexinput-btle --bin jc2_imu
//!   cargo run -p flexinput-btle --bin jc2_imu -- --save out.bin
//!   cargo run -p flexinput-btle --bin jc2_imu -- --load out.bin   (no hardware)
//!
//! Flags that change what is done to the controller:
//!   --led           blink the player LEDs through the common command channel,
//!                   which is the only unambiguous "is this channel alive" test
//!   --common-only   skip the per-side init, to see whether it is what locks
//!                   the common stream out
//!   --cmd-per-side  send the modern init down 0x0016 instead of 0x0014, since
//!                   the LED test proved 0x0014 inert on this hardware.
//!                   ⭐ THIS IS WHAT MADE THE CONTROLLER ANSWER — 15 command
//!                   acknowledgements on 0x001a, after months of silence.
//!   --mask-sweep    try each feature mask and report which report bytes exist
//!   --dump-flash    read the controller's flash regions and hex-dump them.
//!                   Factory IMU calibration lives there, and it is the one
//!                   source of device state never actually read.
//!   --flash-scan    with --dump-flash, also walk beyond the known regions
//!   --force         record the sweep even when the preflight says nothing new
//!                   will come of it
//!
//! There is no `--mag`: the magnetometer is one bit of [`FEATURE_MASK`], not a
//! separate gate. See that constant for why the flag was removed rather than
//! rewired.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon as jc, Dongle, Event};

const NINTENDO_COMPANY_ID: u16 = 0x0553;

/// Byte range searched, as overlapping little-endian `i16`s. The real alignment
/// is unknown; assuming it would beg the question.
const SEARCH_START: usize = 8;
const SEARCH_END: usize = 61;

/// 1 g in accelerometer LSB, established from an earlier hardware capture.
const ONE_G: f64 = 4096.0;

struct Phase {
    name: &'static str,
    instruction: &'static str,
    secs: u64,
}

/// Index 0 is the reference; 1..=3 are the axis sweeps, in roll/pitch/yaw order.
const PHASES: &[Phase] = &[
    Phase {
        name: "neutral",
        instruction: "Lay the grip FLAT on the table, both halves attached. DO NOT TOUCH IT.",
        secs: 5,
    },
    Phase {
        name: "roll",
        instruction: "ROLL — like turning a STEERING WHEEL. Hold the grip up, face buttons                       toward you. Turn it CLOCKWISE (as you look at it) one FULL 360 deg,                       SLOWLY and steadily, ending exactly where it started.",
        secs: 12,
    },
    Phase {
        name: "pitch",
        instruction: "PITCH — tumble it END OVER END. From flat, tip the grip so the SHOULDER                       BUTTONS RISE, keep going up and over the top, one FULL 360 deg, SLOWLY,                       ending flat again exactly where it started.",
        secs: 12,
    },
    Phase {
        name: "yaw",
        instruction: "YAW — spin it like a record. Keep the grip FLAT on the table and spin it                       CLOCKWISE (seen from above) one FULL 360 deg, SLOWLY, ending exactly                       where it started.",
        secs: 12,
    },
];

/// Sweep phases for ONE DETACHED half (`--detached`).
///
/// ⭐ **The grip is what invalidated the previous searches.** Each half is
/// mounted at an angle inside it, so a grip rotation splits across two or three
/// DEVICE axes and no physical motion can isolate one. Every test that assumed
/// "one field owns one phase" — including the rate integral this exists to
/// re-run — was measuring a mixture and reporting a null.
///
/// Detached, the half's own axes line up with the motions below, so a single
/// axis really is isolated and the assumption finally holds.
///
/// ⭐ **FAST turns, with STILL ends.** The old instruction said "SLOWLY", which
/// was correct at ~67 Hz where a quick turn aliased — and wrong now that the
/// link runs at ~201 Hz. A 360 deg turn taken over 12 s is only 30 deg/s, about
/// 430 LSB at the controller's stated scale: a small signal sitting on a bias
/// that drifts for the whole phase. The same turn in one second is six times
/// the amplitude with a sixth of the drift, and 200 samples still cover it.
///
/// The rotation is still exactly 360 deg, so every scale calculation is
/// unchanged — only the signal-to-noise improves. Holding still at both ends
/// also gives `phase_bias` a clean zero to work from, which is what made byte 26
/// legible in the first place.
const PHASES_DETACHED: &[Phase] = &[
    Phase {
        name: "neutral",
        instruction: "ONE half only, OUT of the grip. Lay it FLAT on the table, buttons up.
    DO NOT TOUCH IT.",
        secs: 5,
    },
    Phase {
        name: "roll",
        instruction: "ROLL about its LONG axis.
    Hold STILL for 2 s → then ONE FAST 360 deg twist (about a second) →
    then hold STILL again until the phase ends. Keep the long axis pointing
    the same way throughout.",
        secs: 8,
    },
    Phase {
        name: "pitch",
        instruction: "PITCH end over end.
    Hold STILL for 2 s → then ONE FAST 360 deg tumble, top end going up and
    over → then hold STILL. Do not let it twist or spin while you do it.",
        secs: 8,
    },
    Phase {
        name: "yaw",
        instruction: "YAW like a record, flat on the table.
    Hold STILL for 2 s → then ONE FAST 360 deg spin clockwise seen from
    above → then hold STILL.",
        secs: 8,
    },
];

const AXES: [&str; 3] = ["roll", "pitch", "yaw"];

/// One recorded notification stream: a controller half and one of its
/// notifying characteristics.
///
/// A half can expose SEVERAL notifying characteristics, and until now the probe
/// recorded exactly one of them — handle `0x000e`, taken from an HCI capture of
/// the Windows stack. That is where every gyro search has looked. Making the
/// stream part of the record's identity is what lets the others be searched at
/// all.
struct Link {
    conn: u16,
    side: &'static str,
    /// Attribute handle the notifications arrive on.
    att: u16,
}

impl Link {
    /// Display label. The handle is included because two entries can now differ
    /// only by which stream they came from.
    fn label(&self) -> String {
        format!("{}@{:#06x}", self.side, self.att)
    }
}

/// Records are keyed by `(connection handle, attribute handle)`.
type Key = (u16, u16);

/// Raw reports kept per phase per link.
///
/// Whole frames rather than running statistics: the accelerometer test needs
/// three fields *from the same sample* to compute a vector magnitude, which
/// per-offset summaries throw away. ~3000 frames per half is a few hundred KB.
type Frames = Vec<Vec<u8>>;

/// Serialise every captured frame so analysis can be re-run without hardware.
///
/// Each decoding idea otherwise costs a fresh 45-second physical sweep, which
/// is the real bottleneck now that the accelerometer is settled: the gyro
/// encoding will take many attempts, and they should be cheap.
///
/// Marks a v2 capture. A v1 file began with the phase count, which is small, so
/// a value no plausible phase count can reach distinguishes the two without a
/// separate file extension — and old captures stay loadable, which matters
/// because re-recording one costs a 45-second physical sweep.
const CAPTURE_V2: u8 = 0xFE;

/// Serialise every captured frame so analysis can be re-run without hardware.
///
/// Each decoding idea otherwise costs a fresh 45-second physical sweep, which
/// is the real bottleneck now that the accelerometer is settled: the gyro
/// encoding will take many attempts, and they should be cheap.
///
/// v2 format: `[0xFE][2][phases u8]` then per phase `[links u8]`, per link
/// `[side u8][conn u16][att u16][frames u32]` then each frame as
/// `[len u8][bytes]`. The connection handle is stored rather than re-derived:
/// several streams now share one half, so position in the list no longer says
/// which controller a record came from.
fn save_capture(path: &str, links: &[Link], rec: &[HashMap<Key, Frames>]) -> std::io::Result<()> {
    let mut out: Vec<u8> = vec![CAPTURE_V2, 2, rec.len() as u8];
    for phase in rec {
        out.push(links.len() as u8);
        for l in links {
            out.push(if l.side == "RIGHT" { 1 } else { 0 });
            out.extend_from_slice(&l.conn.to_le_bytes());
            out.extend_from_slice(&l.att.to_le_bytes());
            let frames = phase.get(&(l.conn, l.att)).cloned().unwrap_or_default();
            out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
            for f in frames {
                out.push(f.len() as u8);
                out.extend_from_slice(&f);
            }
        }
    }
    std::fs::write(path, out)
}

/// Reload a capture written by [`save_capture`], v1 or v2.
fn load_capture(path: &str) -> std::io::Result<(Vec<Link>, Vec<HashMap<Key, Frames>>)> {
    let buf = std::fs::read(path)?;
    let mut i = 0usize;
    let take = |i: &mut usize, n: usize| -> &[u8] { let s = &buf[*i..*i + n]; *i += n; s };
    let v2 = buf[i] == CAPTURE_V2;
    if v2 {
        i += 2; // magic + version
    }
    let phases = buf[i]; i += 1;
    let mut links: Vec<Link> = Vec::new();
    let mut rec: Vec<HashMap<Key, Frames>> = Vec::new();
    for p in 0..phases {
        let nlinks = buf[i]; i += 1;
        let mut map: HashMap<Key, Frames> = HashMap::new();
        for l in 0..nlinks {
            let side = if buf[i] == 1 { "RIGHT" } else { "LEFT" }; i += 1;
            // v1 predates per-stream recording, so everything in it came from
            // the one handle the probe subscribed to back then, and its
            // connection handles are synthesised from list position.
            let (conn, att) = if v2 {
                let c = u16::from_le_bytes(take(&mut i, 2).try_into().unwrap());
                let a = u16::from_le_bytes(take(&mut i, 2).try_into().unwrap());
                (c, a)
            } else {
                (l as u16, jc::HANDLE_INPUT_VALUE)
            };
            let count = u32::from_le_bytes(take(&mut i, 4).try_into().unwrap()) as usize;
            if p == 0 {
                links.push(Link { conn, side, att });
            }
            let mut frames = Frames::new();
            for _ in 0..count {
                let len = buf[i] as usize; i += 1;
                frames.push(take(&mut i, len).to_vec());
            }
            map.insert((conn, att), frames);
        }
        rec.push(map);
    }
    Ok((links, rec))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg_after = |flag: &str| -> Option<String> {
        args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
    };

    // Print a byte range as raw numbers instead of scanning it.
    //
    // Every automated scan so far has answered "does this field behave like a
    // gyro" and returned no. Reading the actual values answers a different and
    // more basic question — what IS in there — which no amount of scoring can,
    // because a scan only ever confirms or denies the shape it was told to look
    // for. Byte order is printed both ways: an LSB-first bit reader cannot
    // recognise a big-endian field, and everything so far has assumed
    // little-endian.
    if let Some(path) = arg_after("--dump") {
        let lo: usize = arg_after("--from").and_then(|s| s.parse().ok()).unwrap_or(19);
        let hi: usize = arg_after("--to").and_then(|s| s.parse().ok()).unwrap_or(31);
        match load_capture(&path) {
            Ok((links, rec)) => dump_region(&links, &rec, lo, hi),
            Err(e) => eprintln!("[imu] cannot read {path}: {e}"),
        }
        return;
    }

    // Live watch: print the motion block continuously so the encoding can be
    // read off by hand instead of inferred.
    //
    // Every automated attempt has fitted a MODEL to the data and scored it, and
    // each time a wrong model scored well enough to look right. Watching raw
    // counts while turning the controller a known amount answers the two open
    // questions directly — how many counts per degree, and where the value
    // actually wraps — with no model in the way.
    if args.iter().any(|a| a == "--watch") {
        let dongle = match flexinput_btle::open_preferred() {
            Ok(d) => d,
            Err(e) => { eprintln!("[imu] cannot open dongle: {e}"); std::process::exit(1); }
        };
        if let Err(e) = dongle.reset_and_init() {
            eprintln!("[imu] init failed: {e}");
            std::process::exit(1);
        }
        let watch_opts = Opts {
            common_only: args.iter().any(|a| a == "--common-only"),
            cmd_per_side: args.iter().any(|a| a == "--cmd-per-side"),
            detached: args.iter().any(|a| a == "--detached"),
            late_subscribe: args.iter().any(|a| a == "--late-subscribe"),
            led_test: args.iter().any(|a| a == "--led"),
            force: true,
        };
        let links = connect_both(&dongle, &watch_opts);
        if links.is_empty() {
            eprintln!("[imu] no controllers connected");
            std::process::exit(1);
        }
        watch(&dongle, &links);
        return;
    }

    // Re-analyse a saved capture: no dongle, no controllers, no sweep.
    if let Some(path) = arg_after("--load") {
        match load_capture(&path) {
            Ok((links, rec)) => {
                println!("[imu] loaded {path}: {} phases, {} halves", rec.len(), links.len());
                for link in &links {
                    analyse(link, &rec);
                }
                cross_check(&links, &rec);
            }
            Err(e) => eprintln!("[imu] cannot read {path}: {e}"),
        }
        return;
    }
    let save_path = arg_after("--save");

    let opts = Opts {
        common_only: std::env::args().any(|a| a == "--common-only"),
        // ⭐ The flash dump reads replies on 0x001a, and 0x001a only ever answers
        // when the init went down 0x0016. Implying the flag removes the one way
        // to run this mode in the configuration that cannot possibly work.
        cmd_per_side: std::env::args().any(|a| {
            a == "--cmd-per-side"
                || a == "--dump-flash"
                || a == "--mask-sweep"
                || a == "--rate-sweep"
                || a == "--cmd-sweep"
                || a == "--late-subscribe"
                || a == "--read-all"
                || a == "--encrypt"
        }),
        detached: std::env::args().any(|a| a == "--detached"),
        late_subscribe: std::env::args().any(|a| a == "--late-subscribe"),
        led_test: std::env::args().any(|a| a == "--led"),
        force: std::env::args().any(|a| a == "--force"),
    };
    // Echo the RESOLVED setting, not the raw argument. Three separate rounds of
    // this investigation were wasted on flags that silently never reached the
    // process, each time producing confident-looking but meaningless results.
    //
    // ❗ This line had become an instance of exactly that. It printed 0x37/0xB7
    // — the OLD mask — long after `init` started sending 0x94, so the run log
    // stated a feature mask the code did not use.
    //
    // There is no separate magnetometer flag any more: `FEATURE_MAGNOMETER` is
    // one bit of the single IMU mask, and 0x94 is the known-good set the
    // reference uses. Enabling more is actively harmful (phantom ZL/ZR), and
    // enabling less has never been something we wanted to test.
    println!("[imu] feature mask = {FEATURE_MASK:#04x}  (motion|mouse|magnetometer — the whole IMU, one gate)");

    // ⭐ RUN CONFIGURATION, printed before anything can go wrong.
    //
    // This exists because a stale binary and an unapplied edit look identical
    // from the output, and both have wasted a physical test run here. Every
    // decision that changes what the controller is told is stated up front, so
    // a log can be checked against the flags that were meant to be active
    // WITHOUT reading the source.
    println!("[imu] ── run configuration ──────────────────────────────");
    println!(
        "[imu]   command channel : {:#06x} {}",
        if opts.cmd_per_side { jc::HANDLE_CMD_WRITE } else { jc::HANDLE_CMD_WRITE_COMMON },
        if opts.cmd_per_side { "(per-side — executes commands here)" } else { "(common — INERT on this pad)" },
    );
    println!(
        "[imu]   input subscribe : {}",
        if opts.late_subscribe { "AFTER init (reference order)" } else { "before init (our historical order)" },
    );
    println!(
        "[imu]   legacy 0x37 init: {}",
        if opts.common_only || opts.cmd_per_side { "SKIPPED (cannot overwrite the mask under test)" } else { "WILL RUN and will overwrite the mask" },
    );
    println!(
        "[imu]   sweep phases    : {}",
        if opts.detached { "DETACHED (one half, own axes)" } else { "grip (both halves)" },
    );
    println!("[imu] ───────────────────────────────────────────────────");

    let dongle = match flexinput_btle::open_preferred() {
        Ok(d) => d,
        Err(e) => { eprintln!("[imu] cannot open dongle: {e}"); std::process::exit(1); }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[imu] init failed: {e}");
        std::process::exit(1);
    }

    let links = connect_both(&dongle, &opts);
    if links.is_empty() {
        eprintln!("[imu] no controllers connected");
        std::process::exit(1);
    }
    // Count HALVES, not streams: each half now contributes several streams, so
    // `links.len()` no longer answers "did both controllers connect".
    let mut halves: Vec<u16> = links.iter().map(|l| l.conn).collect();
    halves.sort_unstable();
    halves.dedup();
    if halves.len() < 2 && !opts.detached {
        println!("\n[imu] NOTE: only one half connected — no cross-check between halves.\n");
    }
    println!("[imu] recording {} stream(s) across {} half/halves", links.len(), halves.len());

    // ⭐ FAIL FAST. Whether a stream is alive is knowable in two seconds, and
    // asking for a 45 s physical sweep before saying so wastes the one resource
    // this investigation actually runs on: the user's time rotating a grip.
    //
    // Every question worth asking here is about which streams woke up, and none
    // of them need the sweep to answer.
    if std::env::args().any(|a| a == "--mask-sweep") {
        mask_sweep(&dongle, &links);
        return;
    }

    if std::env::args().any(|a| a == "--encrypt") {
        try_encrypt(&dongle, &links, &opts);
        return;
    }

    if std::env::args().any(|a| a == "--read-all") {
        read_all(&dongle, &links);
        return;
    }

    if std::env::args().any(|a| a == "--cmd-sweep") {
        cmd_sweep(&dongle, &links);
        return;
    }

    if std::env::args().any(|a| a == "--rate-sweep") {
        rate_sweep(&dongle, &links);
        return;
    }

    if std::env::args().any(|a| a == "--dump-flash") {
        dump_flash(&dongle, &links, std::env::args().any(|a| a == "--flash-scan"));
        return;
    }

    if !preflight(&dongle, &links, &opts) {
        eprintln!("\n[imu] ABORTED before the sweep — nothing new would be captured.");
        eprintln!("[imu] Pass --force to record anyway.");
        std::process::exit(1);
    }

    // ⭐ The phase list is chosen HERE, not baked in. `--detached` needs its own
    // instructions: they describe one half's own axes, which is the entire
    // reason the mode exists.
    let phases: &[Phase] = if opts.detached { PHASES_DETACHED } else { PHASES };
    println!("\n[imu] === {} phases, about 45 s total ===", phases.len());
    if opts.detached {
        println!("[imu] ONE half, OUT of the grip. Isolating a single device axis per");
        println!("[imu] phase is the whole point — the grip makes that impossible.\n");
    } else {
        println!("[imu] Keep BOTH halves clipped into the grip throughout.\n");
    }

    // phase -> (conn, att) -> frames
    let mut rec: Vec<HashMap<Key, Frames>> = Vec::new();
    for phase in phases {
        println!("\n>>> [{}] {}", phase.name, phase.instruction);
        for n in (1..=3).rev() {
            println!("    {n}…");
            std::thread::sleep(Duration::from_secs(1));
        }
        println!("    GO — {} s", phase.secs);
        let frames = record(&dongle, &links, Duration::from_secs(phase.secs));
        for l in &links {
            let f = frames.get(&(l.conn, l.att));
            // Report LENGTH is printed too: if the magnetometer bit is honoured,
            // the controller has to put those readings somewhere, so a longer
            // report is the most direct evidence that the bit did anything at
            // all. Identical lengths with and without --mag means it did not.
            let len = f.and_then(|v| v.first()).map(|r| r.len()).unwrap_or(0);
            println!("    {:>14}: {} frames, report len {len}", l.label(), f.map(|v| v.len()).unwrap_or(0));
        }
        rec.push(frames);
    }

    for conn in &halves {
        let _ = dongle.disconnect(*conn);
    }
    if let Some(path) = &save_path {
        match save_capture(path, &links, &rec) {
            Ok(()) => println!("
[imu] capture saved to {path} — re-analyse with --load {path}"),
            Err(e) => eprintln!("[imu] could not save {path}: {e}"),
        }
    }
    for link in &links {
        analyse(link, &rec);
    }
    cross_check(&links, &rec);
}

/// Print bytes `lo..hi` as raw values across each phase, decimated.
fn dump_region(links: &[Link], rec: &[HashMap<Key, Frames>], lo: usize, hi: usize) {
    for link in links {
        println!("\n\n========== {} bytes {lo}..{hi} ==========", link.label());
        for (pi, phase) in PHASES.iter().enumerate() {
            let Some(frames) = rec[pi].get(&(link.conn, link.att)) else { continue };
            if frames.is_empty() {
                continue;
            }
            println!("\n-- {} ({} frames, every 25th) --", phase.name, frames.len());
            println!("  {:>5}  {:<40}  {:<26}  {}", "n", "hex", "i16 LE", "i16 BE");
            // Step is settable: a decimated view cannot tell a smooth sensor
            // from one whose consecutive samples alternate, and that is exactly
            // the distinction between a real reading and packed sub-samples.
            let step: usize = std::env::args()
                .position(|a| a == "--step")
                .and_then(|i| std::env::args().nth(i + 1))
                .and_then(|v| v.parse().ok())
                .unwrap_or(25);
            let skip: usize = std::env::args()
                .position(|a| a == "--skip")
                .and_then(|i| std::env::args().nth(i + 1))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            for (n, f) in frames.iter().enumerate().skip(skip).step_by(step.max(1)).take(40) {
                let end = hi.min(f.len());
                if lo >= end {
                    continue;
                }
                let sl = &f[lo..end];
                let hex: String = sl.iter().map(|b| format!("{b:02x} ", )).collect();
                let le: String = sl
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| format!("{:>6} ", i16::from_le_bytes([c[0], c[1]])))
                    .collect();
                let be: String = sl
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| format!("{:>6} ", i16::from_be_bytes([c[0], c[1]])))
                    .collect();
                println!("  {n:>5}  {hex:<40}  {le:<26}  {be}");
            }
        }
    }
}

/// Integer encodings a sensor field might use.
///
/// Byte order and width were both fixed assumptions until now — little-endian
/// `i16` throughout — and neither was ever checked. The saved capture shows
/// smoothly-varying 24-bit little-endian quantities in the motion block, which
/// a 16-bit reader chops in half and renders as noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    I16Le,
    I16Be,
    I24Le,
    I24Be,
}

impl Codec {
    const ALL: [Codec; 4] = [Codec::I16Le, Codec::I16Be, Codec::I24Le, Codec::I24Be];

    fn width(self) -> usize {
        match self {
            Codec::I16Le | Codec::I16Be => 2,
            Codec::I24Le | Codec::I24Be => 3,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Codec::I16Le => "i16le",
            Codec::I16Be => "i16be",
            Codec::I24Le => "i24le",
            Codec::I24Be => "i24be",
        }
    }

    /// Read a signed value at `off`, or `None` if it does not fit.
    fn read(self, frame: &[u8], off: usize) -> Option<i64> {
        let w = self.width();
        let b = frame.get(off..off + w)?;
        let raw: u32 = match self {
            Codec::I16Le => u16::from_le_bytes([b[0], b[1]]) as u32,
            Codec::I16Be => u16::from_be_bytes([b[0], b[1]]) as u32,
            Codec::I24Le => b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16,
            Codec::I24Be => b[2] as u32 | (b[1] as u32) << 8 | (b[0] as u32) << 16,
        };
        let bits = w * 8;
        let sign = 1u32 << (bits - 1);
        Some(if raw & sign != 0 {
            raw as i64 - (1i64 << bits)
        } else {
            raw as i64
        })
    }
}

/// Print the motion block live, a few times a second.
///
/// Shows each 4-byte group as raw hex plus several readings of the same bytes,
/// so a wrap is visible as it happens and counts-per-degree can be measured by
/// turning the controller a known amount between two readings.
fn watch(dongle: &Dongle, links: &[Link]) {
    println!("
[watch] Turn a controller slowly by a KNOWN angle and read the counts.");
    println!("[watch] Columns per group: hex | u24le | i24le | u32le");
    println!("[watch] Ctrl-C to stop.
");
    // Per-link throttle. A single shared timer let whichever half reported
    // first consume every window, so the second half printed only occasionally
    // and looked half-broken — the same starvation that made one controller
    // read 0 Hz during scanning.
    let mut last: HashMap<u16, Instant> = HashMap::new();
    loop {
        for pkt in dongle.drain_acl(256) {
            if pkt.cid != acl::CID_ATT { continue; }
            let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
            if n.handle != jc::HANDLE_INPUT_VALUE { continue; }
            let Some(link) = links.iter().find(|l| l.conn == pkt.conn_handle) else { continue };
            let t = last.entry(pkt.conn_handle).or_insert_with(|| Instant::now() - Duration::from_secs(1));
            if t.elapsed() < Duration::from_millis(250) { continue; }
            *t = Instant::now();

            // Anchor to the accelerometer, the one field that is certain.
            let accel_base = if link.side == "RIGHT" { 34 } else { 33 };
            let base = accel_base - 14;
            let f = &n.value;
            let mut line = format!("{:>5} ", link.side);
            for g in 0..3 {
                let o = base + g * 4;
                let Some(b) = f.get(o..o + 4) else { continue };
                let u24 = b[1] as u32 | (b[2] as u32) << 8 | (b[3] as u32) << 16;
                let i24 = if u24 & 0x80_0000 != 0 { u24 as i32 - (1 << 24) } else { u24 as i32 };
                let u32v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                line.push_str(&format!(
                    "| {:02x}{:02x}{:02x}{:02x} {:>8} {:>9} {:>11} ",
                    b[0], b[1], b[2], b[3], u24, i24, u32v
                ));
            }
            let ax = field(f, accel_base).unwrap_or(0);
            let ay = field(f, accel_base + 4).unwrap_or(0);
            let az = field(f, accel_base + 8).unwrap_or(0);
            line.push_str(&format!("| accel {ax:>6} {ay:>6} {az:>6}"));
            println!("{line}");
        }
    }
}

/// Read an `i16` field from a frame, or `None` if it does not fit.
fn field(frame: &[u8], off: usize) -> Option<i16> {
    if off + 1 < frame.len() {
        Some(i16::from_le_bytes([frame[off], frame[off + 1]]))
    } else {
        None
    }
}

/// Range of an offset across a set of frames.
fn range_of(frames: &Frames, off: usize) -> i32 {
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for f in frames {
        if let Some(v) = field(f, off) {
            lo = lo.min(v as i32);
            hi = hi.max(v as i32);
        }
    }
    if lo > hi { 0 } else { hi - lo }
}

/// Mean of an i16 field across frames.
fn mean_of(frames: &Frames, off: usize) -> i32 {
    let (mut sum, mut n) = (0i64, 0i64);
    for f in frames {
        if let Some(v) = field(f, off) {
            sum += v as i64;
            n += 1;
        }
    }
    if n == 0 { 0 } else { (sum / n) as i32 }
}

/// Score a consecutive 3-field i32 block as an accelerometer: how constant is
/// its vector magnitude, relative to 1 g?
///
/// This is the identification that cannot be faked. Gravity's magnitude is
/// fixed, so only the real accel triple holds a constant length through every
/// orientation; nothing else in the report has a physically pinned magnitude.
fn accel_error(all: &[Vec<u8>], base: usize, stride: usize) -> Option<f64> {
    let (mut err, mut n) = (0.0f64, 0.0f64);
    for f in all {
        let (Some(x), Some(y), Some(z)) = (
            field(f, base),
            field(f, base + stride),
            field(f, base + 2 * stride),
        ) else {
            continue;
        };
        let m = ((x as f64).powi(2) + (y as f64).powi(2) + (z as f64).powi(2)).sqrt();
        err += ((m - ONE_G) / ONE_G).powi(2);
        n += 1.0;
    }
    if n < 50.0 {
        return None;
    }
    Some((err / n).sqrt())
}

fn analyse(link: &Link, rec: &[HashMap<Key, Frames>]) {
    let empty: Frames = Vec::new();
    let ph = |i: usize| rec[i].get(&(link.conn, link.att)).unwrap_or(&empty);

    println!("

========== {} ==========", link.label());

    let mut all: Vec<Vec<u8>> = Vec::new();
    for i in 0..PHASES.len() {
        all.extend(ph(i).iter().cloned());
    }
    // A subscribed-but-silent characteristic is a RESULT, not a stream to
    // analyse. Say so in one line instead of printing a full set of empty
    // scans that read like failures of the analysis.
    if all.is_empty() {
        println!("  subscribed successfully but the controller never sent a notification here.");
        println!("  The characteristic exists and is silent — that is a real negative.");
        return;
    }

    // A raw frame, so the block structure can be read directly instead of
    // inferred. Cheap, and it settles layout arguments that statistics cannot.
    if let Some(f) = ph(0).last() {
        println!("
-- one NEUTRAL frame (flat on the table) --");
        for row in 0..(f.len() + 15) / 16 {
            let lo = row * 16;
            let hi = (lo + 16).min(f.len());
            let hex: Vec<String> = f[lo..hi].iter().map(|b| format!("{b:02x}")).collect();
            println!("  {lo:>3}: {}", hex.join(" "));
        }
    }

    // Which bytes carry anything at all.
    //
    // Assumption-free, and it frames every decoding question that follows: a
    // byte that never changes across the whole sweep cannot hold a gyro axis,
    // whatever the packing. For a newly-discovered stream this is also the
    // first thing worth knowing — a stream of constant bytes is not a place to
    // go looking for motion data.
    if !all.is_empty() {
        let width = all.iter().map(|f| f.len()).max().unwrap_or(0);
        let mut live = vec![false; width];
        let first = &all[0];
        for f in &all {
            for i in 0..width {
                if f.get(i) != first.get(i) {
                    live[i] = true;
                }
            }
        }
        let map: String = live
            .iter()
            .map(|l| if *l { '#' } else { '.' })
            .collect();
        println!("
-- BYTE LIVENESS over all {} frames ('#' varies, '.' constant) --", all.len());
        for row in 0..(width + 31) / 32 {
            let lo = row * 32;
            let hi = (lo + 32).min(width);
            println!("  {lo:>3}: {}", &map[lo..hi]);
        }
        let n_live = live.iter().filter(|l| **l).count();
        println!("  {n_live} of {width} bytes carry any variation at all");
    }

    format3_check(&all);
    magnitude_scan(&all);

    // Accelerometer: the 3 i16 fields whose vector magnitude stays at 1 g.
    // Strides are searched rather than assumed — the block could be packed
    // (stride 2) or interleaved with another sensor (stride 4 or 6), and the
    // earlier i32 reading was simply wrong: it produced values in the hundreds
    // of millions, which is what reading across packed 16-bit fields looks like.
    let mut accel: Vec<(usize, usize, f64)> = Vec::new();
    for stride in [2usize, 4, 6] {
        for b in SEARCH_START..SEARCH_END.saturating_sub(2 * stride + 1) {
            if let Some(e) = accel_error(&all, b, stride) {
                accel.push((b, stride, e));
            }
        }
    }
    accel.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());

    println!("
-- ACCELEROMETER (3 x i16; magnitude must hold at 4096 = 1 g) --");
    for (b, st, e) in accel.iter().take(5) {
        let verdict = if *e < 0.15 { "  <== ACCEL BLOCK" } else { "" };
        println!("  offsets [{} {} {}] stride {st}  magnitude error {:>6.1}%{}",
            b, b + st, b + 2 * st, e * 100.0, verdict);
    }
    match accel.first().filter(|(_, _, e)| *e < 0.15) {
        Some((b, st, _)) => {
            println!("  neutral (flat) means: {}={} {}={} {}={}",
                b, mean_of(ph(0), *b), b + st, mean_of(ph(0), *b + st),
                b + 2 * st, mean_of(ph(0), *b + 2 * st));
            println!("  flat on a table, one axis should read about +/-4096 and the others ~0");
        }
        None => println!("  no triple held a constant magnitude at any stride"),
    }

    // Gyro: a field that moves far more during rotation than at rest.
    //
    // Scored PURELY on the ratio between motion and rest. The previous version
    // required an absolute range of at least 1000 and rejected anything above
    // 60000, and both bounds were guesses that threw away real candidates: the
    // gyro axis at bytes 22-23 spans about 108 counts during a sweep against 4
    // at rest — a 27x ratio, and far below the floor. An absolute threshold
    // cannot know a sensor's scale factor in advance; a ratio does not need to.
    println!("
-- GYRO (moves under rotation, still at rest; scored by rest-vs-motion ratio) --");
    println!("{:>5} {:>7} {:>9} {:>9} {:>9} {:>8} {:>7}  {:>16}",
        "off", "codec", "roll", "pitch", "yaw", "rest", "ratio", "min..max on axis");
    let accel_span = accel.first().filter(|(_, _, e)| *e < 0.15).map(|(b, st, _)| (*b, *st));
    let span_of = |frames: &Frames, off: usize, c: Codec| -> i64 {
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for f in frames {
            if let Some(v) = c.read(f, off) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if lo > hi { 0 } else { hi - lo }
    };

    let mut rows: Vec<(usize, Codec, [i64; 3], i64, f64)> = Vec::new();
    for c in Codec::ALL {
        for off in SEARCH_START..SEARCH_END.saturating_sub(c.width()) {
            // Skip the accel fields themselves — gravity makes them respond to
            // rotation too, and they outranked real gyro axes until excluded.
            if let Some((b, st)) = accel_span {
                if (0..3).any(|i| {
                    let a = b + i * st;
                    off + c.width() > a && a + 2 > off
                }) {
                    continue;
                }
            }
            let rest = span_of(ph(0), off, c);
            let r = [
                span_of(ph(1), off, c),
                span_of(ph(2), off, c),
                span_of(ph(3), off, c),
            ];
            let best = r.iter().copied().max().unwrap_or(0);
            // A field must actually move, and must move far more under rotation
            // than at rest. Nothing else is assumed about its scale.
            if best < 20 || best < rest * 6 {
                continue;
            }
            let others = r.iter().copied().sum::<i64>() - best;
            rows.push((off, c, r, rest, best as f64 / others.max(1) as f64));
        }
    }
    rows.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap());
    for (off, c, r, rest, sel) in rows.iter().take(12) {
        let axis = (0..3).max_by_key(|i| r[*i]).unwrap();
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for f in ph(axis + 1) {
            if let Some(v) = c.read(f, *off) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        let tag = if *sel > 2.0 { format!("  <== GYRO {}", AXES[axis]) } else { String::new() };
        println!("{off:>5} {:>7} {:>9} {:>9} {:>9} {rest:>8} {sel:>7.1}  {lo:>7}..{hi:<7}{tag}",
            c.name(), r[0], r[1], r[2]);
    }
    if rows.is_empty() {
        println!("  nothing moved selectively with rotation, at any offset or codec");
    }
    println!("
  A gyro axis has min and max of OPPOSITE sign about its bias (the sweep went both ways),");
    println!("  and its three axes sit at a regular spacing. 'rest' is the range while stationary.");

    if let Some((ab, _, _)) = accel.first().filter(|(_, _, e)| *e < 0.15) {
        let phases = rec_phases(rec, (link.conn, link.att));
        omega_scan(&phases, *ab);
        block_scan(&phases, *ab);
        rate_scan_bytes(&phases, *ab);
        bit_scan(&phases, *ab);
    } else {
        // Without an accelerometer in THIS stream there is no angular-rate
        // reference to score against, so say so rather than running a scan
        // whose null result would mean nothing. A stream carrying gyro but no
        // accel still gets cross-referenced below.
        println!("
  no accelerometer in this stream — rate correlation needs one, skipped");
    }
}

/// Frames for one recorded stream, grouped by phase.
fn rec_phases(rec: &[HashMap<Key, Frames>], key: Key) -> Vec<Frames> {
    rec.iter()
        .map(|m| m.get(&key).cloned().unwrap_or_default())
        .collect()
}

/// Extract a signed field of `width` bits starting at `bit_off`, LSB-first.
fn bit_field(frame: &[u8], bit_off: usize, width: usize) -> Option<i64> {
    if (bit_off + width + 7) / 8 > frame.len() {
        return None;
    }
    let mut v: u64 = 0;
    for i in 0..width {
        let b = bit_off + i;
        let bit = (frame[b / 8] >> (b % 8)) & 1;
        v |= (bit as u64) << i;
    }
    // Sign-extend from `width` bits.
    let sign = 1u64 << (width - 1);
    Some(if v & sign != 0 {
        (v as i64) - (1i64 << width)
    } else {
        v as i64
    })
}

/// Full angular-velocity vector implied by consecutive gravity readings.
///
/// **This supersedes the `atan2` references.** Those took two accel axes and
/// differentiated the tilt angle between them, which has two serious faults:
/// it sees rotation in ONE plane only, and it amplifies noise wherever the
/// denominator passes through zero — which a 90 degree sweep guarantees. The
/// symptom was a reference peaking at 0.56 rad/frame where a hand sweep should
/// produce about 0.008, i.e. spikes seventy times larger than the signal they
/// were supposed to measure. Correlating anything against that finds nothing,
/// however correct the candidate decoding.
///
/// For a small rotation between two unit gravity vectors, the angular velocity
/// is their cross product over |g|². It is singularity-free, needs no
/// unwrapping, and yields all three axes at once — so each candidate field can
/// be scored against the axis it actually belongs to.
///
/// The component ABOUT gravity (yaw) stays unobservable: gravity is invariant
/// under it. That is physics, not a limitation of this method.
fn gravity_rates(frames: &Frames, base: usize) -> Vec<[f64; 3]> {
    let read = |f: &[u8], i: usize| -> f64 { field(f, base + i * 4).unwrap_or(0) as f64 };
    let mut out: Vec<[f64; 3]> = vec![[0.0; 3]];
    for w in frames.windows(2) {
        let a = [read(&w[0], 0), read(&w[0], 1), read(&w[0], 2)];
        let b = [read(&w[1], 0), read(&w[1], 1), read(&w[1], 2)];
        let n = a[0] * a[0] + a[1] * a[1] + a[2] * a[2];
        out.push(if n > 0.0 {
            [
                (a[1] * b[2] - a[2] * b[1]) / n,
                (a[2] * b[0] - a[0] * b[2]) / n,
                (a[0] * b[1] - a[1] * b[0]) / n,
            ]
        } else {
            [0.0; 3]
        });
    }
    out
}

/// Absolute tilt implied by the accelerometer, in radians.
///
/// Unwrapped so the series is continuous across the branch cut: a 180 degree
/// sweep crosses it, and a raw `atan2` would jump a full turn there. A
/// discontinuity like that destroys correlation against a smoothly-integrated
/// angle, which is exactly what this reference exists to detect.
fn accel_angles(frames: &Frames, base: usize, axis_a: usize, axis_b: usize) -> Vec<f64> {
    let read = |f: &[u8], i: usize| -> f64 { field(f, base + i * 4).unwrap_or(0) as f64 };
    let mut out: Vec<f64> = Vec::with_capacity(frames.len());
    let mut turns = 0.0f64;
    let mut prev: Option<f64> = None;
    for f in frames {
        let a = read(f, axis_a).atan2(read(f, axis_b));
        if let Some(p) = prev {
            let d = a - p;
            if d > std::f64::consts::PI {
                turns -= 2.0 * std::f64::consts::PI;
            } else if d < -std::f64::consts::PI {
                turns += 2.0 * std::f64::consts::PI;
            }
        }
        prev = Some(a);
        out.push(a + turns);
    }
    out
}

/// Angular rate implied by the accelerometer, in radians per frame.
///
/// Gravity's direction in the sensor frame gives absolute tilt, and the gyro is
/// the DERIVATIVE of that tilt — so differentiating it produces a genuine
/// angular-rate reference to score candidate decodings against. This is the
/// discriminator that range-based scoring could never be: range found the
/// accelerometer because gravity is large and slow, but it cannot characterise
/// a rate signal at all, which is why five scans came up empty.
///
/// Only ROLL and PITCH can be recovered this way. Yaw rotates about the gravity
/// vector, so gravity is invariant under it and the accelerometer is blind to
/// it — the yaw axis has to be inferred afterwards as the remaining field at
/// the same bit spacing.
fn accel_rates(frames: &Frames, base: usize, axis_a: usize, axis_b: usize) -> Vec<f64> {
    let angles = accel_angles(frames, base, axis_a, axis_b);
    let mut rates = Vec::with_capacity(angles.len());
    rates.push(0.0);
    for i in 1..angles.len() {
        let mut d = angles[i] - angles[i - 1];
        // Unwrap: a jump of more than half a turn is the branch cut, not motion.
        if d > std::f64::consts::PI {
            d -= 2.0 * std::f64::consts::PI;
        } else if d < -std::f64::consts::PI {
            d += 2.0 * std::f64::consts::PI;
        }
        rates.push(d);
    }
    rates
}

/// Moving average, to make a differenced signal comparable at all.
///
/// Differencing a quantised angle once per frame at ~67 Hz produces a reference
/// dominated by quantisation noise, which drags every correlation toward zero
/// however correct the candidate decoding is. Smoothing both sides equally
/// leaves any real relationship intact while removing the noise that hides it.
fn smooth(v: &[f64], w: usize) -> Vec<f64> {
    if v.len() < w || w < 2 {
        return v.to_vec();
    }
    let mut out = Vec::with_capacity(v.len() - w + 1);
    let mut acc: f64 = v[..w].iter().sum();
    out.push(acc / w as f64);
    for i in w..v.len() {
        acc += v[i] - v[i - w];
        out.push(acc / w as f64);
    }
    out
}

/// Running sum with the mean removed first.
///
/// Demeaning is essential, not cosmetic: the running sum of a biased series is
/// a straight ramp, and a ramp correlates near 1.0 with any other ramp. Without
/// this every candidate with a non-zero bias would score as a discovery.
fn integrate(v: &[f64]) -> Vec<f64> {
    if v.is_empty() {
        return Vec::new();
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let mut acc = 0.0;
    v.iter()
        .map(|x| {
            acc += x - m;
            acc
        })
        .collect()
}

/// First difference of a WRAPPING field, length-matched to its input.
///
/// ❗ This is the transform every earlier scan was missing. The motion block
/// holds an angle in a `width`-bit wrapping representation: during a roll sweep
/// it runs up to +8,225,146, just under 2^23, then reappears at -5,022,652.
/// Read as a plain integer that is a jump of 13 million between adjacent
/// frames, and a handful of those is enough to bury any correlation — which is
/// exactly what happened, with a reference that was otherwise sound.
///
/// Interpreting the difference modulo 2^width recovers the true step, and the
/// step of an angle per frame IS the angular rate. So this turns the field into
/// the gyro reading the device never sends directly.
fn wrapped_delta(v: &[f64], width: usize) -> Vec<f64> {
    if v.is_empty() || width == 0 || width >= 63 {
        return v.to_vec();
    }
    let modulus = (1u64 << width) as f64;
    let half = modulus / 2.0;
    let mut out = Vec::with_capacity(v.len());
    out.push(0.0);
    for i in 1..v.len() {
        let mut d = v[i] - v[i - 1];
        if d > half {
            d -= modulus;
        } else if d < -half {
            d += modulus;
        }
        out.push(d);
    }
    out
}

/// First difference, length-matched to its input.
fn differentiate(v: &[f64]) -> Vec<f64> {
    if v.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(v.len());
    out.push(0.0);
    for i in 1..v.len() {
        out.push(v[i] - v[i - 1]);
    }
    out
}

/// Pearson correlation. Returns 0 when either series is constant.
fn correlate(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 20 {
        return 0.0;
    }
    let (ma, mb) = (
        a[..n].iter().sum::<f64>() / n as f64,
        b[..n].iter().sum::<f64>() / n as f64,
    );
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        return 0.0;
    }
    num / (da * db).sqrt()
}

/// Score every candidate bit-field by how well it tracks the accelerometer's
/// angular rate, and report the best per axis.
fn rate_scan(phases: &[Frames], accel_base: usize, _start_byte: usize, _region_bits: usize) {
    // Search the WHOLE report, not just the region before the accelerometer.
    // Confining it there assumed the gyro sits next to the accel block, and
    // that assumption has now produced five empty scans — it costs little to
    // stop making it.
    let start_byte = 8usize;
    let region_bits = (accel_base.max(48) - start_byte) * 8;
    const SMOOTH: usize = 7;
    println!("
-- RATE CORRELATION (vs accelerometer-derived angular rate) --");
    println!("   Only roll and pitch can be scored: yaw rotates about gravity, so the");
    println!("   accelerometer cannot see it. |r| near 1.0 is a real gyro axis.");
    println!("{:>6} {:>6} {:>9} {:>9}", "bit", "width", "r(roll)", "r(pitch)");

    // Roll tilts axis 1 against vertical; pitch tilts axis 0 against vertical.
    let roll_ref = smooth(&accel_rates(&phases[1], accel_base, 1, 2), SMOOTH);
    let pitch_ref = smooth(&accel_rates(&phases[2], accel_base, 0, 2), SMOOTH);

    // Validate the REFERENCE before trusting a null result. If the derived
    // angular rate is flat or absurd, no candidate can correlate with it and
    // "nothing found" would say nothing about the encoding at all.
    let stats = |v: &[f64]| -> (f64, f64, f64) {
        if v.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let m = v.iter().sum::<f64>() / v.len() as f64;
        let sd = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        let peak = v.iter().cloned().fold(0.0f64, |a, b| a.max(b.abs()));
        (m, sd, peak)
    };
    let (rm, rsd, rpk) = stats(&roll_ref);
    let (pm, psd, ppk) = stats(&pitch_ref);
    println!("   reference roll : n={} mean={rm:.5} sd={rsd:.5} peak={rpk:.5} rad/frame",
        roll_ref.len());
    println!("   reference pitch: n={} mean={pm:.5} sd={psd:.5} peak={ppk:.5} rad/frame",
        pitch_ref.len());
    println!("   (a 90 deg sweep over ~4 s at 67 Hz is about 0.006 rad/frame; a peak near");
    println!("    zero means the reference is broken and the null result is meaningless)");

    let series = |frames: &Frames, bit: usize, w: usize| -> Vec<f64> {
        let raw: Vec<f64> = frames
            .iter()
            .map(|f| {
                f.get(start_byte..)
                    .and_then(|sl| bit_field(sl, bit, w))
                    .unwrap_or(0) as f64
            })
            .collect();
        smooth(&raw, SMOOTH)
    };

    let mut rows: Vec<(usize, usize, f64, f64)> = Vec::new();
    for width in [10usize, 11, 12, 14, 16, 20, 21] {
        for bit in 0..region_bits.saturating_sub(width) {
            let r_roll = correlate(&series(&phases[1], bit, width), &roll_ref);
            let r_pitch = correlate(&series(&phases[2], bit, width), &pitch_ref);
            if r_roll.abs() > 0.45 || r_pitch.abs() > 0.45 {
                rows.push((bit, width, r_roll, r_pitch));
            }
        }
    }
    rows.sort_by(|a, b| {
        b.2.abs().max(b.3.abs()).partial_cmp(&a.2.abs().max(a.3.abs())).unwrap()
    });
    if rows.is_empty() {
        println!("   nothing correlated above 0.5 — the encoding is not a plain packed");
        println!("   signed field, or the rate is differenced/scaled between samples");
        return;
    }
    for (bit, width, rr, rp) in rows.iter().take(12) {
        let tag = if rr.abs() > 0.8 {
            "  <== GYRO roll"
        } else if rp.abs() > 0.8 {
            "  <== GYRO pitch"
        } else {
            ""
        };
        println!("{bit:>6} {width:>6} {rr:>9.3} {rp:>9.3}{tag}");
    }
    println!("   A negative r means the axis is inverted, which is information, not noise.");
}

/// Score every `(byte offset, codec)` against the accelerometer-derived
/// angular rate.
///
/// The existing [`rate_scan`] only ever tried packed bit-fields read LSB-first
/// from byte 8. That covers one family of encodings; this covers the other, and
/// the two together are what the "nothing correlated" result actually needed to
/// have tested before it could mean anything.
fn rate_scan_bytes(phases: &[Frames], accel_base: usize) {
    const SMOOTH: usize = 7;
    let roll_ref = smooth(&accel_rates(&phases[1], accel_base, 1, 2), SMOOTH);
    let pitch_ref = smooth(&accel_rates(&phases[2], accel_base, 0, 2), SMOOTH);

    println!("
-- RATE CORRELATION, BYTE-ALIGNED (vs accelerometer angular rate) --");
    println!("{:>5} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "off", "codec", "rate(rl)", "rate(pt)", "ang(rl)", "ang(pt)");

    // Also score against the accelerometer's ABSOLUTE tilt, not just its
    // derivative.
    //
    // A rate gyro sits at a constant bias when still. These fields instead
    // drift smoothly and monotonically at rest — bytes 21-23 slide about 1000
    // counts over five motionless seconds — which is the signature of an
    // INTEGRATED angle accumulating gyro bias, not of a rate. If that is what
    // this is, it can never correlate with a rate reference however correct the
    // decoding, and every null result so far would be explained by comparing
    // against the wrong physical quantity.
    let roll_angle = smooth(&accel_angles(&phases[1], accel_base, 1, 2), SMOOTH);
    let pitch_angle = smooth(&accel_angles(&phases[2], accel_base, 0, 2), SMOOTH);

    let series = |frames: &Frames, off: usize, c: Codec| -> Vec<f64> {
        let raw: Vec<f64> = frames.iter().map(|f| c.read(f, off).unwrap_or(0) as f64).collect();
        smooth(&raw, SMOOTH)
    };

    // Two guards, both learned from false positives this scan produced:
    //
    // 1. Skip anything overlapping the accelerometer. Correlating the accel
    //    against an angle DERIVED from the accel is circular, and it scored
    //    0.95 — the strongest "result" in the table, and meaningless.
    // 2. Require the field to carry real variation. Byte 18 is the constant
    //    length byte 0x0c with one rare step in it, and smoothing a nearly
    //    constant series against a monotonic ramp manufactured r = 0.94 from
    //    nothing.
    let overlaps_accel = |off: usize, w: usize| -> bool {
        (0..3).any(|i| {
            let a = accel_base + i * 4;
            off + w > a && a + 2 > off
        })
    };
    let varies = |v: &[f64]| -> bool {
        if v.len() < 20 {
            return false;
        }
        let m = v.iter().sum::<f64>() / v.len() as f64;
        let sd = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        // Distinct values matter as much as spread: a two-level step function
        // has a healthy standard deviation and still carries no waveform.
        let mut seen: Vec<i64> = v.iter().map(|x| *x as i64).collect();
        seen.sort_unstable();
        seen.dedup();
        sd > 0.0 && seen.len() >= 8
    };

    let mut rows: Vec<(usize, Codec, f64, f64, f64, f64)> = Vec::new();
    for c in Codec::ALL {
        for off in SEARCH_START..SEARCH_END.saturating_sub(c.width()) {
            if overlaps_accel(off, c.width()) {
                continue;
            }
            let s_roll = series(&phases[1], off, c);
            let s_pitch = series(&phases[2], off, c);
            if !varies(&s_roll) || !varies(&s_pitch) {
                continue;
            }
            let rr = correlate(&s_roll, &roll_ref);
            let rp = correlate(&s_pitch, &pitch_ref);
            let ar = correlate(&s_roll, &roll_angle);
            let ap = correlate(&s_pitch, &pitch_angle);
            if [rr, rp, ar, ap].iter().any(|v| v.abs() > 0.4) {
                rows.push((off, c, rr, rp, ar, ap));
            }
        }
    }
    rows.sort_by(|a, b| {
        let k = |r: &(usize, Codec, f64, f64, f64, f64)| {
            r.2.abs().max(r.3.abs()).max(r.4.abs()).max(r.5.abs())
        };
        k(b).partial_cmp(&k(a)).unwrap()
    });
    if rows.is_empty() {
        println!("   nothing byte-aligned tracked either reference");
        return;
    }
    for (off, c, rr, rp, ar, ap) in rows.iter().take(14) {
        let tag = if ar.abs() > 0.8 {
            "  <== ANGLE roll"
        } else if ap.abs() > 0.8 {
            "  <== ANGLE pitch"
        } else if rr.abs() > 0.8 {
            "  <== RATE roll"
        } else if rp.abs() > 0.8 {
            "  <== RATE pitch"
        } else {
            ""
        };
        println!("{off:>5} {:>7} {rr:>9.3} {rp:>9.3} {ar:>9.3} {ap:>9.3}{tag}", c.name());
    }
    println!("   A negative r means the axis is inverted, which is information, not noise.");
}

/// Decode the report against the DOCUMENTED Format-3 layout and check it.
///
/// Layout from TommyWabg/Switch2Connect (`ControllerInputData`), which drives
/// these controllers successfully:
///
/// | bytes | field |
/// |---|---|
/// | 0–3 | timestamp u32 |
/// | 4–7 | buttons u32 |
/// | 10–15 | sticks (3 bytes each, 12-bit packed) |
/// | 16–23 | mouse x/y, roughness, distance |
/// | 25–30 | **magnetometer** x,y,z i16 |
/// | 31–34 | battery voltage/current |
/// | 46–47 | temperature |
/// | 48–53 | **accelerometer** x,y,z i16 |
/// | 54–59 | **gyroscope** x,y,z i16 |
///
/// All contiguous little-endian i16 — nothing like the sparse strided block on
/// the per-side stream. The accelerometer magnitude is the check that says
/// whether this layout is actually in effect: 4096 LSB = 1 g, independently
/// matching our own hardware measurement.
fn format3_check(all: &[Vec<u8>]) {
    println!("
-- FORMAT-3 LAYOUT CHECK (documented offsets) --");
    if all.iter().all(|f| f.len() < 60) {
        println!("   reports are shorter than 60 bytes — Format 3 is NOT in effect");
        return;
    }
    let i16at = |f: &[u8], o: usize| -> f64 {
        f.get(o..o + 2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f64)
            .unwrap_or(0.0)
    };
    let (mut sum, mut n) = (0.0f64, 0.0f64);
    for f in all {
        if f.len() < 60 {
            continue;
        }
        let m = (i16at(f, 48).powi(2) + i16at(f, 50).powi(2) + i16at(f, 52).powi(2)).sqrt();
        sum += m;
        n += 1.0;
    }
    if n < 10.0 {
        println!("   too few full-length reports");
        return;
    }
    let mean = sum / n;
    if let Some(f) = all.iter().find(|f| f.len() >= 60) {
        println!("   accel  {:>8.0} {:>8.0} {:>8.0}   |a| mean = {mean:.0} LSB ({:.2} g)",
            i16at(f, 48), i16at(f, 50), i16at(f, 52), mean / 4096.0);
        println!("   gyro   {:>8.0} {:>8.0} {:>8.0}   (16.334 LSB per deg/s on a Joy-Con)",
            i16at(f, 54), i16at(f, 56), i16at(f, 58));
        println!("   mag    {:>8.0} {:>8.0} {:>8.0}",
            i16at(f, 25), i16at(f, 27), i16at(f, 29));
    }
    let off = (mean / 4096.0 - 1.0).abs();
    if off < 0.1 {
        println!("   ✅ |a| within {:.0}% of 1 g — Format 3 confirmed, offsets are correct.", off * 100.0);
    } else {
        println!("   ⛔ |a| is {:.0}% off 1 g — this is NOT the Format-3 layout.", off * 100.0);
    }
}

/// Find every field triple whose vector magnitude stays constant, at any codec,
/// stride and scale.
///
/// The accelerometer was found this way but with the scale HARD-CODED to
/// 4096 LSB = 1 g, which can only ever find an accelerometer. Learning the
/// scale from the data instead makes the same physics test find any normalised
/// vector — an orientation or gravity-direction output has constant magnitude
/// too, and at a completely different scale.
///
/// Motivated by the raw dump: the motion block reads as three 24-bit values
/// which, taken signed at 2^23 = 1.0, give (-0.065, 0.003, 0.995) with the grip
/// flat — magnitude 0.997, pointing straight up.
fn magnitude_scan(all: &[Vec<u8>]) {
    println!("
-- CONSTANT-MAGNITUDE TRIPLES (scale learned, not assumed) --");
    let mut rows: Vec<(usize, usize, Codec, f64, f64)> = Vec::new();
    for c in Codec::ALL {
        for stride in [c.width(), c.width() + 1, 4usize, 6] {
            if stride < c.width() {
                continue;
            }
            for b in SEARCH_START..SEARCH_END.saturating_sub(2 * stride + c.width()) {
                let mags: Vec<f64> = all
                    .iter()
                    .filter_map(|f| {
                        let x = c.read(f, b)? as f64;
                        let y = c.read(f, b + stride)? as f64;
                        let z = c.read(f, b + 2 * stride)? as f64;
                        Some((x * x + y * y + z * z).sqrt())
                    })
                    .collect();
                if mags.len() < 200 {
                    continue;
                }
                let mean = mags.iter().sum::<f64>() / mags.len() as f64;
                if mean <= 0.0 {
                    continue;
                }
                // ❗ The components must MOVE. A triple of constant zeros has a
                // perfectly constant magnitude and is the single most common
                // thing in this report — 41 of 63 bytes never change — so
                // without this the table fills with dead padding scoring 0.00%.
                let moving = (0..3).all(|i| {
                    let vals: Vec<f64> = all
                        .iter()
                        .filter_map(|f| c.read(f, b + i * stride).map(|v| v as f64))
                        .collect();
                    let m = vals.iter().sum::<f64>() / vals.len().max(1) as f64;
                    let sd = (vals.iter().map(|x| (x - m).powi(2)).sum::<f64>()
                        / vals.len().max(1) as f64)
                        .sqrt();
                    sd > mean * 0.01
                });
                if !moving {
                    continue;
                }
                let sd = (mags.iter().map(|m| (m - mean).powi(2)).sum::<f64>()
                    / mags.len() as f64)
                    .sqrt();
                let rel = sd / mean;
                if rel < 0.05 {
                    rows.push((b, stride, c, mean, rel));
                }
            }
        }
    }
    rows.sort_by(|a, b| a.4.partial_cmp(&b.4).unwrap());
    if rows.is_empty() {
        println!("   no triple held a constant magnitude at any codec or stride");
        return;
    }
    println!("{:>6} {:>7} {:>7} {:>14} {:>9}  {}",
        "base", "stride", "codec", "mean |v|", "rel sd", "scale it implies");
    for (b, st, c, mean, rel) in rows.iter().take(10) {
        // A magnitude sitting on a power of two is a normalised vector with
        // that power as its 1.0; anything else is probably a physical unit.
        let p2 = (mean.log2()).round();
        let hint = if (mean / 2f64.powf(p2) - 1.0).abs() < 0.02 {
            format!("~2^{p2:.0} = 1.0 (normalised)")
        } else if (mean - 4096.0).abs() < 200.0 {
            "4096 = 1 g (accelerometer)".to_string()
        } else {
            String::new()
        };
        println!("{b:>6} {st:>7} {:>7} {mean:>14.0} {:>8.2}%  {hint}",
            c.name(), rel * 100.0);
    }
}

/// Solve the full 3x3 map from field rates to angular velocity.
///
/// ❗ **This supersedes trying to isolate one axis per phase.** The halves sit
/// at an ANGLE in the charging grip, so a grip-pitch is not a device-pitch: one
/// grip rotation splits across two or three device fields. No physical motion
/// can isolate a device axis while the halves are in the grip, which is why
/// "pitch" never appeared as a clean single-field response however the sweep
/// was designed.
///
/// A fixed mount rotation is a linear map, so fit it rather than fight it:
/// find M minimising |P(M·r − ω)| over every sample, where `r` is the three
/// fields' wrapped per-frame deltas and `ω` is angular velocity from
/// consecutive gravity readings.
///
/// `P` projects out the gravity direction, because rotation ABOUT gravity is
/// unobservable at any instant. It is still recoverable overall: as the device
/// tumbles, gravity moves through the device frame and a different component
/// becomes unobservable each time, so varied motion constrains all nine terms.
/// This is why a rich tumbling capture beats three tidy single-axis sweeps.
fn axis_solve(phases: &[Frames], accel_base: usize) {
    println!("
-- AXIS MAP SOLVE (3x3 field-rate -> angular velocity) --");
    // Search the field TRIPLE as well as the map.
    //
    // `accel_base - 13` with a 24-bit codec was itself inferred, and an
    // otherwise-correct solve cannot rescue the wrong three fields. The
    // accelerometer's own structure (i16 on a 4-byte stride) is the strongest
    // hint available for what a sensor triple looks like in this report, so
    // that shape is tried too.
    // ❗ TWO GAPS THIS SEARCH HAD, both of which excluded the most likely shape.
    //
    // 1. **Stride was hardcoded to 4.** Every candidate triple was read as
    //    `base, base+4, base+8` — the accelerometer's shape. A CONTIGUOUS
    //    3 x i16 triple (stride 2) was never fitted, and that is both the
    //    Format-3 shape and exactly what the liveness map shows at LEFT bytes
    //    20-25: six consecutive live bytes bounded by dead ones at 19 and 26.
    //
    // 2. **The field was always DIFFERENCED first** (`wrapped_delta`), which
    //    assumes it is an absolute angle. The controller states its gyro scale
    //    in LSB per deg/s — a RATE — so a raw rate field would be destroyed by
    //    differencing it. Both interpretations are now fitted.
    let mut best: Option<Candidate> = None;
    for codec in [Codec::I16Le, Codec::I24Le, Codec::I16Be, Codec::I24Be] {
        for stride in [2usize, 3, 4, 6] {
            if stride < codec.width() {
                continue;
            }
            for deriv in [false, true] {
                for back in 4..=16usize {
                    let base = accel_base.saturating_sub(back);
                    let Some((frac, txt, m)) =
                        axis_solve_at(phases, accel_base, base, stride, codec, deriv)
                    else {
                        continue;
                    };
                    if best.as_ref().is_none_or(|b| frac > b.frac) {
                        best = Some(Candidate { frac, base, stride, codec, deriv, txt, m });
                    }
                }
            }
        }
    }
    let Some(b) = best else {
        println!("   no usable triple");
        return;
    };
    let kind = if b.deriv { "differenced (angle)" } else { "raw (rate)" };
    if b.frac > 0.4 {
        println!(
            "   ✅ best triple: base {}, stride {}, {}, {kind} — {:.0}% explained",
            b.base, b.stride, b.codec.name(), b.frac * 100.0
        );
        print!("{}", b.txt);
    } else {
        println!("   ⛔ best of ALL candidate triples explains only {:.0}%", b.frac * 100.0);
        println!("      (base {}, stride {}, {}, {kind})", b.base, b.stride, b.codec.name());
        println!("      Searched bases accel_base-16..-4, strides 2/3/4/6, i16 and i24 in");
        println!("      both byte orders, raw AND differenced, with the full 3x3 mixing that");
        println!("      any fixed mount angle would require.");
    }

    // ⭐ THE SCALE CHECK THE DEVICE HANDED US.
    //
    // A fit quality alone has misled this search three times. The controller
    // now states its own gyro scale, so a genuine rate triple must ALSO produce
    // a map whose rows are that scale: the fitted M converts field counts to
    // rad/frame, so each row norm should be the stated rad/s per LSB divided by
    // the report rate. That is an independent check with no free parameter, and
    // a fit that lands on the wrong scale is wrong however well it scores.
    // ❗ The MEASURED rate, not the constant: the fitted map is per FRAME, so a
    // capture taken at 200 Hz compared against a 67 Hz constant is wrong by 3x
    // and would reject a correct decode.
    let hz = measured_report_hz(phases);
    let expected = GYRO_RAD_PER_LSB / hz;
    println!("      scale check: rows should be ~{expected:.3e} rad/frame per LSB");
    println!("      (controller states {GYRO_RAD_PER_LSB:.7} rad/s per LSB; capture ran at {hz:.1} Hz)");
    for i in 0..3 {
        let n = (b.m[i * 3].powi(2) + b.m[i * 3 + 1].powi(2) + b.m[i * 3 + 2].powi(2)).sqrt();
        let ratio = n / expected;
        println!(
            "         row {i}: {n:.3e}  = {ratio:>8.2} x expected{}",
            if (0.5..2.0).contains(&ratio) { "   <== right order of magnitude" } else { "" },
        );
    }
}

/// The controller's stated gyro scale, from its own `0x11/0x03` reply.
///
/// 0.0012217 rad/s per LSB = 0.07 deg/s per LSB = 14.286 LSB per deg/s. See
/// [`sensor_scales`] — the accelerometer figure in the same block reproduces
/// the independently measured 4096 LSB/g, which is why this one is trusted.
const GYRO_RAD_PER_LSB: f64 = 0.0012217;
/// Input reports per second, part of the scale since rates arrive per report.
const REPORT_HZ: f32 = 67.0;

/// The report rate this capture was ACTUALLY taken at, measured from it.
///
/// ⛔ **`REPORT_HZ` is a hardcoded 67 and must not be trusted for a new
/// capture.** It was measured on links negotiated at ~15 ms; pinning the
/// connection interval to 7.5 ms yields ~200 Hz. Every rate and scale in this
/// file divides by the report rate, so using the constant against a 200 Hz
/// capture makes every answer three times wrong — the exact shape of mistake
/// that has already produced several confident, false conclusions here.
///
/// Frames divided by the phase's known wall-clock duration needs no assumption
/// about intervals or timestamps.
fn measured_report_hz(phases: &[Frames]) -> f64 {
    let (mut frames, mut secs) = (0usize, 0f64);
    // Phase durations come from whichever list produced the capture; both lists
    // use the same durations, so either is safe here.
    for (i, f) in phases.iter().enumerate() {
        if let Some(p) = PHASES.get(i) {
            frames += f.len();
            secs += p.secs as f64;
        }
    }
    if secs <= 0.0 || frames == 0 {
        return REPORT_HZ as f64;
    }
    frames as f64 / secs
}

struct Candidate {
    frac: f64,
    base: usize,
    stride: usize,
    codec: Codec,
    deriv: bool,
    txt: String,
    m: [f64; 9],
}

/// One (base, codec) candidate: fit the map and return the fraction explained.
fn axis_solve_at(
    phases: &[Frames],
    accel_base: usize,
    base: usize,
    stride: usize,
    codec: Codec,
    deriv: bool,
) -> Option<(f64, String, [f64; 9])> {

    // Per-sample rate vector and observed angular velocity.
    let mut rs: Vec<[f64; 3]> = Vec::new();
    let mut ws: Vec<[f64; 3]> = Vec::new();
    let mut gs: Vec<[f64; 3]> = Vec::new();
    for frames in phases.iter() {
        if frames.len() < 20 { continue; }
        let raw: Vec<[f64; 3]> = frames.iter().map(|f| {
            [
                codec.read(f, base).unwrap_or(0) as f64,
                codec.read(f, base + stride).unwrap_or(0) as f64,
                codec.read(f, base + 2 * stride).unwrap_or(0) as f64,
            ]
        }).collect();
        // Smooth BOTH sides equally before fitting.
        //
        // A per-frame difference of a quantised field against a per-frame
        // gravity cross-product is noise on both sides, and least squares
        // responds by shrinking the fit toward zero — which reads as "no
        // relationship" even when the integrals plainly track each other, as
        // `turn_scale` shows they do. Smoothing removes the noise that hides
        // the relation without inventing one: an unrelated pair stays
        // unrelated however much it is averaged.
        const SM: usize = 9;
        let d: Vec<Vec<f64>> = (0..3)
            .map(|i| {
                let col: Vec<f64> = raw.iter().map(|r| r[i]).collect();
                // A RATE field is used as it stands; only an ANGLE field is
                // differenced. Differencing a rate turns angular acceleration
                // into the thing being fitted, which correlates with nothing.
                let series = if deriv {
                    wrapped_delta(&col, codec.width() * 8)
                } else {
                    // Remove the resting bias: a gyro at rest sits at a
                    // constant offset, and an uncentred column makes the fit
                    // spend its freedom cancelling a DC term.
                    let mean = col.iter().sum::<f64>() / col.len().max(1) as f64;
                    col.iter().map(|v| v - mean).collect()
                };
                smooth(&series, SM)
            })
            .collect();
        let omega_raw = gravity_rates(frames, accel_base);
        let omega: Vec<[f64; 3]> = {
            let cols: Vec<Vec<f64>> = (0..3)
                .map(|i| smooth(&omega_raw.iter().map(|w| w[i]).collect::<Vec<_>>(), SM))
                .collect();
            (0..cols[0].len()).map(|t| [cols[0][t], cols[1][t], cols[2][t]]).collect()
        };
        let n_ok = frames.len().min(omega.len()).min(d[0].len());
        for t in 1..n_ok {
            let f = &frames[t];
            let g = [
                field(f, accel_base).unwrap_or(0) as f64,
                field(f, accel_base + 4).unwrap_or(0) as f64,
                field(f, accel_base + 8).unwrap_or(0) as f64,
            ];
            let n = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            if n < 1.0 { continue; }
            rs.push([d[0][t], d[1][t], d[2][t]]);
            ws.push([omega[t][0], omega[t][1], omega[t][2]]);
            gs.push([g[0] / n, g[1] / n, g[2] / n]);
        }
    }
    if rs.len() < 300 {
        return None;
    }

    // Normal equations for the 9 unknowns of M.
    let mut ata = [[0.0f64; 9]; 9];
    let mut atb = [0.0f64; 9];
    for ((r, w), g) in rs.iter().zip(ws.iter()).zip(gs.iter()) {
        // P = I - g g^T
        let mut pm = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for k in 0..3 {
                pm[i][k] = if i == k { 1.0 } else { 0.0 } - g[i] * g[k];
            }
        }
        for i in 0..3 {
            // Row i of A: A[i, k*3+j] = P[i][k] * r[j]
            let mut arow = [0.0f64; 9];
            for k in 0..3 {
                for j in 0..3 {
                    arow[k * 3 + j] = pm[i][k] * r[j];
                }
            }
            let b: f64 = (0..3).map(|k| pm[i][k] * w[k]).sum();
            for a in 0..9 {
                atb[a] += arow[a] * b;
                for c in 0..9 {
                    ata[a][c] += arow[a] * arow[c];
                }
            }
        }
    }
    // Ridge term: the solve is rank-deficient when motion is not varied enough,
    // and without this it returns huge fitted values that explain nothing.
    for i in 0..9 { ata[i][i] += 1e-9; }

    let m = solve9(ata, atb)?;

    // Residual, against the do-nothing baseline.
    let (mut res, mut base_res) = (0.0f64, 0.0f64);
    for ((r, w), g) in rs.iter().zip(ws.iter()).zip(gs.iter()) {
        let mr = [
            m[0] * r[0] + m[1] * r[1] + m[2] * r[2],
            m[3] * r[0] + m[4] * r[1] + m[5] * r[2],
            m[6] * r[0] + m[7] * r[1] + m[8] * r[2],
        ];
        for i in 0..3 {
            let mut e = mr[i] - w[i];
            let mut b = -w[i];
            let dot: f64 = (0..3).map(|k| g[k] * (mr[k] - w[k])).sum();
            let dotb: f64 = (0..3).map(|k| g[k] * (-w[k])).sum();
            e -= g[i] * dot;
            b -= g[i] * dotb;
            res += e * e;
            base_res += b * b;
        }
    }
    let rms = (res / rs.len() as f64).sqrt();
    let rms0 = (base_res / rs.len() as f64).sqrt();

    let frac = 1.0 - rms / rms0.max(1e-12);
    let mut txt = String::from("      map rows = angular velocity axes, cols = fields 0,1,2:
");
    for i in 0..3 {
        txt.push_str(&format!("      [{:>12.3e} {:>12.3e} {:>12.3e}]
",
            m[i * 3], m[i * 3 + 1], m[i * 3 + 2]));
    }
    for i in 0..3 {
        let n = (m[i * 3].powi(2) + m[i * 3 + 1].powi(2) + m[i * 3 + 2].powi(2)).sqrt();
        if n > 0.0 {
            txt.push_str(&format!("      axis {i}: {:.0} counts/turn
", std::f64::consts::TAU / n));
        }
    }
    Some((frac, txt, m))
}

/// Solve a 9x9 system by Gaussian elimination with partial pivoting.
fn solve9(mut a: [[f64; 9]; 9], mut b: [f64; 9]) -> Option<[f64; 9]> {
    for col in 0..9 {
        let piv = (col..9).max_by(|x, y| a[*x][col].abs().partial_cmp(&a[*y][col].abs()).unwrap())?;
        if a[piv][col].abs() < 1e-18 { return None; }
        a.swap(col, piv);
        b.swap(col, piv);
        for row in 0..9 {
            if row == col { continue; }
            let f = a[row][col] / a[col][col];
            if f == 0.0 { continue; }
            for k in col..9 { a[row][k] -= f * a[col][k]; }
            b[row] -= f * b[col];
        }
    }
    let mut out = [0.0f64; 9];
    for i in 0..9 { out[i] = b[i] / a[i][i]; }
    Some(out)
}

/// Read counts-per-revolution straight off a known 360 degree turn.
///
/// This is the measurement every earlier attempt was missing. Each motion phase
/// is exactly ONE full turn about one axis, ending where it began, so:
///
/// * the field owning that axis advances by exactly one revolution's worth of
///   counts — its total unwrapped change IS counts per revolution, with no
///   model, no fitted scale and no accelerometer geometry involved;
/// * it must return to its starting value, so `residual` near zero confirms the
///   unwrapping was right rather than merely plausible;
/// * yaw is measurable too, which gravity could never do.
///
/// ❗ Unwrapping assumes no true step exceeds half the modulus between samples.
/// That is why the instruction says SLOWLY: a fast turn aliases, and the result
/// silently comes out as a fraction of the real value.
/// Integrate every candidate field as a RATE at the scale the device states.
///
/// ⭐ **The first test in this whole search with no free parameter.**
///
/// Everything before it either fitted a scale and scored the fit, or scored a
/// correlation — and a wrong model scored well enough to look right three
/// separate times. This asks a question with one arithmetic answer:
///
/// > The controller says 0.07 deg/s per LSB. Each sweep phase is a known 360
/// > degree turn. So for the true gyro axis, summing (value - rest bias) over
/// > the phase, divided by the report rate and multiplied by 0.07, MUST come
/// > out at 360 — and near 0 in the phases that did not turn about that axis.
///
/// A field that lands on 360 by accident, at a scale fixed by the hardware and
/// not by us, is a coincidence worth taking seriously. Nothing else here has
/// been able to say that.
///
/// The flash dump also proves there is something to find: `0x013040` holds a
/// per-unit factory gyro bias (0.03-0.31 deg/s across the two halves), so this
/// controller has a real calibrated three-axis gyro, not just fused output.
fn rate_integral(phases: &[Frames]) {
    // Stated by the controller in its 0x11/0x03 reply. NOT fitted — used only
    // as a yardstick to compare a MEASURED scale against.
    const DPS_PER_LSB: f64 = 0.07;

    println!("
-- RATE INTEGRAL: measured scale per axis (no pass/fail threshold) --");
    println!("   Each sweep phase is a known 360 deg turn. For the field that owns an");
    println!("   axis, sum(value - rest bias) / report_rate is the rotation in LSB-seconds,");
    println!("   so 360 / that IS the field's deg/s per LSB — measured, not assumed.");
    println!("   The controller states {DPS_PER_LSB} deg/s per LSB, so a real gyro axis lands there.");

    let Some(rest) = phases.first() else { return };
    if rest.len() < 20 || phases.len() < 2 {
        println!("   not enough phases");
        return;
    }
    let hz = measured_report_hz(phases);
    println!("   report rate MEASURED from this capture: {hz:.1} Hz (constant says {REPORT_HZ})");

    // Per (codec, offset): the integral in each phase.
    let mut rows: Vec<(Codec, usize, Vec<f64>)> = Vec::new();
    for c in [Codec::I16Le, Codec::I16Be, Codec::I24Le, Codec::I24Be] {
        for off in SEARCH_START..SEARCH_END.saturating_sub(c.width()) {
            let bias = {
                let v: Vec<f64> = rest.iter().filter_map(|f| c.read(f, off).map(|x| x as f64)).collect();
                if v.len() < 10 { continue }
                v.iter().sum::<f64>() / v.len() as f64
            };
            // The rest phase must actually integrate to ~nothing. A field that
            // accumulates while the controller sits still is not a rate.
            let rest_i = rest.iter().filter_map(|f| c.read(f, off).map(|x| x as f64 - bias)).sum::<f64>()
                / hz;
            if (rest_i * DPS_PER_LSB).abs() > 20.0 {
                continue;
            }
            // ❗ Each phase is zeroed on ITS OWN stationary ends, not on the
            // rest phase — see `phase_bias`. Using one global bias made drift
            // masquerade as rotation and produced several confident nulls.
            let integ: Vec<f64> = phases[1..]
                .iter()
                .map(|frames| {
                    let v: Vec<f64> = frames.iter().filter_map(|f| c.read(f, off).map(|x| x as f64)).collect();
                    let (b, _) = phase_bias(&v);
                    v.iter().map(|x| x - b).sum::<f64>() / hz
                })
                .collect();
            rows.push((c, off, integ));
        }
    }
    if rows.is_empty() {
        println!("   no field held still during the rest phase");
        return;
    }

    // Report per PHASE: which field responded most selectively, and what scale
    // that response implies. Printing the measurement rather than a verdict is
    // the point — a wrong scale is information, a bare "not found" is not.
    for (pi, phase) in PHASES.iter().skip(1).enumerate() {
        println!("
   [{}] — a gyro axis for this phase implies ~{DPS_PER_LSB} deg/s per LSB", phase.name);
        println!("     off  codec     LSB-s here   vs other phases   implied deg/s per LSB");
        let mut scored: Vec<(f64, &(Codec, usize, Vec<f64>))> = rows
            .iter()
            .filter_map(|r| {
                let here = r.2[pi].abs();
                let other = r.2.iter().enumerate()
                    .filter(|(i, _)| *i != pi)
                    .map(|(_, v)| v.abs())
                    .fold(0.0f64, f64::max);
                // Selectivity: how much this phase dominates the others.
                (here > 1.0).then_some((here / other.max(1.0), r))
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        for (sel, (c, off, integ)) in scored.iter().take(6) {
            let implied = 360.0 / integ[pi].abs().max(1e-9);
            let hit = (implied / DPS_PER_LSB).clamp(0.0, 1e9);
            println!(
                "   {off:>5}  {:<7} {:>10.0}   {sel:>8.1}x        {implied:>10.5}{}",
                c.name(),
                integ[pi].abs(),
                if (0.8..1.25).contains(&hit) && *sel > 3.0 {
                    "   <== MATCHES the stated scale"
                } else { "" },
            );
        }
    }
    println!("
   A row that is BOTH strongly selective and near {DPS_PER_LSB} is the gyro axis.");
    println!("   Selective but at a different scale means a real rotation signal that is");
    println!("   not the raw gyro — worth knowing, and previously reported as nothing.");
}

/// Zero for one phase, taken from its own stationary ends.
///
/// ⭐ **Every integral test here used the REST phase's bias, and the bias
/// drifts.** On byte 26 that made roll, pitch and yaw all imply the same scale
/// — a beautifully consistent result that was pure arithmetic: the field's mean
/// shifted between phases, and mean-shift times duration IS the integral. The
/// tests were integrating drift and reporting it as rotation.
///
/// Every sweep phase begins and ends stationary by instruction, so its own ends
/// are the honest zero. Returns `(bias, end_disagreement)`; a large
/// disagreement means the zero moved DURING the phase and the integral is still
/// not to be trusted.
fn phase_bias(vals: &[f64]) -> (f64, f64) {
    if vals.len() < 20 {
        let m = vals.iter().sum::<f64>() / vals.len().max(1) as f64;
        return (m, 0.0);
    }
    let tenth = (vals.len() / 10).max(4);
    let head = vals[..tenth].iter().sum::<f64>() / tenth as f64;
    let tail = vals[vals.len() - tenth..].iter().sum::<f64>() / tenth as f64;
    ((head + tail) / 2.0, (tail - head).abs())
}

/// Print one field's actual shape over every phase, downsampled.
///
/// ⭐ Every tool here so far reduces a field to ONE NUMBER — a correlation, an
/// integral, a net change — and numbers that disagree cannot be reconciled
/// without seeing the signal. Byte 26 is selective in `turn_scale` (net change)
/// and absent from `rate_integral` (integral), which is the signature of an
/// angle rather than a rate; but "ramp", "sawtooth" and "bipolar swing" all
/// produce plausible-looking summary statistics and only one of them is an
/// angle.
///
/// So: look at it. A ramp that resets is a wrapping angle. A symmetric swing
/// about a bias is a rate. A staircase is a counter.
fn trace_field(phases: &[Frames], off: usize, codec: Codec) {
    println!("
-- FIELD TRACE: offset {off}, {} --", codec.name());
    let hz = measured_report_hz(phases);
    for (pi, frames) in phases.iter().enumerate() {
        let name = PHASES.get(pi).map(|p| p.name).unwrap_or("?");
        let vals: Vec<f64> = frames.iter().filter_map(|f| codec.read(f, off).map(|v| v as f64)).collect();
        if vals.len() < 8 {
            continue;
        }
        let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        // Largest single-sample step: separates a smooth ramp from a field that
        // jumps, without needing to guess why it jumps.
        let step = vals.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f64, f64::max);
        println!("
   [{name}] n={} over {:.1}s   min {lo:.0}  max {hi:.0}  mean {mean:.0}  largest step {step:.0}",
            vals.len(), vals.len() as f64 / hz);

        // 24 evenly spaced samples, enough to read the shape in one line.
        const COLS: usize = 24;
        let pick: Vec<f64> = (0..COLS)
            .map(|i| vals[i * (vals.len() - 1) / (COLS - 1)])
            .collect();
        print!("      ");
        for v in &pick {
            print!("{:>8.0}", v);
            if (pick.iter().position(|x| x == v).unwrap_or(0) + 1) % 8 == 0 {
                print!("
      ");
            }
        }
        println!();
        // A crude sparkline so the shape is visible without reading numbers.
        let span = (hi - lo).max(1.0);
        let bars = ["_", ".", "-", "=", "*", "#"];
        let spark: String = pick
            .iter()
            .map(|v| bars[(((v - lo) / span) * 5.0).round().clamp(0.0, 5.0) as usize])
            .collect();
        println!("      shape: {spark}");

        // ⭐ How often does it actually CHANGE?
        //
        // Reports arrive at ~201 Hz once the connection interval is pinned to
        // 7.5 ms, but the IMU behind them need not sample that fast. If a value
        // is repeated across N reports, integrating every report counts each
        // sample N times — the integral comes out N times too large and the
        // implied scale N times too SMALL. That is a silent factor-of-N error in
        // every rate measurement in this file.
        let changes = vals.windows(2).filter(|w| w[0] != w[1]).count();
        let update_hz = hz * changes as f64 / (vals.len() - 1) as f64;
        println!(
            "      changes on {}/{} samples → field updates at ~{update_hz:.1} Hz (reports {hz:.1} Hz)",
            changes,
            vals.len() - 1,
        );

        // ⭐ The number that decides whether this is a gyro: integrate it with
        // the resting bias removed. If the field is angular rate, the integral
        // over a known 360 degree turn IS 360 degrees, so `360 / integral` is
        // the field's deg/s per LSB — measured, with the phase's own rotation as
        // the only input.
        if pi > 0 {
            // ❗ Bias from THIS phase's own stationary ends, not from the rest
            // phase.
            //
            // Using the rest phase's bias made byte 26 imply the same scale for
            // roll, pitch AND yaw — which looked like a beautifully consistent
            // result and was arithmetic: its mean shifted from 3135 at rest to
            // 732 during motion, and that difference times the phase duration
            // IS the whole "integral". The test was integrating a bias drift.
            //
            // Every sweep phase begins and ends stationary by instruction, so
            // the first and last tenth of each phase are the honest zero for
            // that phase.
            let tenth = (vals.len() / 10).max(4);
            let ends: Vec<f64> = vals[..tenth]
                .iter()
                .chain(vals[vals.len() - tenth..].iter())
                .copied()
                .collect();
            let bias = ends.iter().sum::<f64>() / ends.len() as f64;
            let integ: f64 = vals.iter().map(|v| v - bias).sum::<f64>() / hz;
            // How much the two ends disagree: if the field drifts within the
            // phase the zero is not stable and the integral is still suspect.
            let head = vals[..tenth].iter().sum::<f64>() / tenth as f64;
            let tail = vals[vals.len() - tenth..].iter().sum::<f64>() / tenth as f64;
            println!(
                "      per-phase bias {bias:.0} (ends differ by {:.0})  integral {integ:.0} LSB-s  →  360/it = {:.5} deg/s per LSB",
                (tail - head).abs(),
                360.0 / integ.abs().max(1e-9),
            );
        }
    }
    println!("
   ramp that resets = wrapping angle | symmetric swing about a bias = rate");
    println!("   staircase = counter | flat with spikes = status or noise");
}

/// Test whether the report carries three FUSED ANGLES, measuring each axis
/// scale from the data before testing whether they compose.
///
/// GAP THIS CLOSES: `gravity_solve` never tested the fields the data points at.
/// It is hardcoded to `base = accel_base - 13`, stride 4, `i24le` — offsets
/// 21/25/29 on the right half. But `turn_scale` on a clean fast capture finds
/// the axis-selective fields at 25 (i24le, roll), 27 (20-bit be, pitch) and
/// 29 (i24le, yaw), plus 16-bit ones at 26/28/30. Different stride, widths and
/// byte orders. Every "these are not three angles" verdict was reached about a
/// triple nobody had evidence for.
///
/// This searches the triple AND takes each axis counts-per-turn from its own
/// sweep phase rather than fitting it, leaving only order and signs free.
///
/// Why it matters even though no raw rate exists: the heading field is plainly
/// a FUSED output (absolute, magnetometer-corrected, drift-free), and fusion
/// requires a gyro. If roll and pitch are exposed the same way, the controller
/// gives a complete orientation with none of the linear-acceleration
/// contamination that makes accel-derived tilt unusable while actually aiming.
fn fused_angle_solve(phases: &[Frames], accel_base: usize) {
    use glam::{Quat, Vec3};
    println!("\n-- FUSED ANGLE SOLVE (scales measured per axis, then composed) --");
    if phases.len() < 4 {
        println!("   needs all four phases");
        return;
    }

    let grav = |f: &Vec<u8>| -> Option<Vec3> {
        let a = Vec3::new(
            field(f, accel_base)? as f32,
            field(f, accel_base + 4)? as f32,
            field(f, accel_base + 8)? as f32,
        );
        (a.length() > 1.0).then(|| a.normalize())
    };

    let mut identity = 0.0f64;
    let mut n_id = 0.0f64;
    for frames in phases.iter() {
        for f in frames.iter().step_by(4) {
            if let Some(g) = grav(f) {
                identity += (g.z.clamp(-1.0, 1.0) as f64).acos();
                n_id += 1.0;
            }
        }
    }
    let identity = (identity / n_id.max(1.0)).to_degrees();

    let mut best: Option<(f64, String)> = None;
    for codec in [Codec::I16Le, Codec::I24Le, Codec::I16Be, Codec::I24Be] {
        for stride in [2usize, 3, 4] {
            if stride < codec.width() {
                continue;
            }
            for base in accel_base.saturating_sub(16)..accel_base.saturating_sub(3) {
                // Measure each axis scale from ITS OWN phase: phase i+1 is a
                // known 360 degree turn about axis i, so the field total
                // unwrapped change across it IS its counts-per-turn.
                let mut turns = [0.0f64; 3];
                let mut ok = true;
                for i in 0..3 {
                    let off = base + i * stride;
                    let Some(frames) = phases.get(i + 1) else { ok = false; break };
                    let raw: Vec<f64> = frames
                        .iter()
                        .filter_map(|f| codec.read(f, off).map(|v| v as f64))
                        .collect();
                    if raw.len() < 100 {
                        ok = false;
                        break;
                    }
                    let net: f64 = wrapped_delta(&raw, codec.width() * 8).iter().sum();
                    if net.abs() < 256.0 {
                        ok = false;
                        break;
                    }
                    turns[i] = net.abs();
                }
                if !ok {
                    continue;
                }

                for signs in 0..8u32 {
                    let sg = [
                        if signs & 1 != 0 { -1.0 } else { 1.0 },
                        if signs & 2 != 0 { -1.0 } else { 1.0 },
                        if signs & 4 != 0 { -1.0 } else { 1.0 },
                    ];
                    for order in 0..6usize {
                        let mut err = 0.0f64;
                        let mut n = 0.0f64;
                        for frames in phases.iter() {
                            for f in frames.iter().step_by(4) {
                                let Some(g) = grav(f) else { continue };
                                let mut ang = [0.0f32; 3];
                                let mut good = true;
                                for i in 0..3 {
                                    let Some(v) = codec.read(f, base + i * stride) else {
                                        good = false;
                                        break;
                                    };
                                    let t = (v as f64 / turns[i]).rem_euclid(1.0);
                                    let t = if t > 0.5 { t - 1.0 } else { t };
                                    ang[i] = (t * std::f64::consts::TAU * sg[i]) as f32;
                                }
                                if !good {
                                    continue;
                                }
                                let idx = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]][order];
                                let rot = |k: usize, a: f32| match k {
                                    0 => Quat::from_rotation_x(a),
                                    1 => Quat::from_rotation_y(a),
                                    _ => Quat::from_rotation_z(a),
                                };
                                let q = rot(idx[0], ang[idx[0]])
                                    * rot(idx[1], ang[idx[1]])
                                    * rot(idx[2], ang[idx[2]]);
                                let up = (q * g).normalize();
                                err += (up.z.clamp(-1.0, 1.0) as f64).acos();
                                n += 1.0;
                            }
                        }
                        if n < 200.0 {
                            continue;
                        }
                        let deg = (err / n).to_degrees();
                        if best.as_ref().is_none_or(|(b, _)| deg < *b) {
                            let sc = [
                                if sg[0] < 0.0 { "-" } else { "+" },
                                if sg[1] < 0.0 { "-" } else { "+" },
                                if sg[2] < 0.0 { "-" } else { "+" },
                            ];
                            best = Some((
                                deg,
                                format!(
                                    "base {base} stride {stride} {} turns[{:.0} {:.0} {:.0}] order {order} signs[{}{}{}]",
                                    codec.name(), turns[0], turns[1], turns[2], sc[0], sc[1], sc[2],
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }

    match best {
        Some((deg, what)) if deg < identity * 0.6 => {
            println!("   OK {deg:.1} deg residual vs {identity:.1} for doing nothing");
            println!("      {what}");
            println!("   Three fused angles ARE present — this is a full orientation.");
        }
        Some((deg, what)) => {
            println!("   best {deg:.1} deg vs {identity:.1} deg for applying NO rotation.");
            println!("      ({what})");
            println!("   Even with each axis scaled from its own 360 deg turn, no triple");
            println!("   composes into a rotation that explains gravity.");
        }
        None => println!("   no candidate triple had enough usable samples"),
    }
}

/// Measure the gyro scale from PATH LENGTH, which does not care about axes.
///
/// ⭐ **This removes the axis-isolation assumption entirely**, and with it the
/// flaw that invalidated every previous version of this test. The magnitude of
/// an angular-velocity vector is rotation-invariant: integrating `|w| dt` over
/// a 360 degree turn gives 360 degrees no matter WHICH axis the turn was about,
/// how the sensor is mounted, or how much the other two axes wobbled.
///
/// So a human turning a controller roughly about one axis is good enough, and
/// the charging grip's mounting angle stops mattering. Imperfect isolation only
/// lengthens the path slightly — it cannot move the answer to a different axis.
///
/// If a triple really is the gyro at the stated scale, `360 / integral` lands on
/// 0.07 deg/s per LSB **in all three phases independently**. Three agreeing
/// measurements from one capture is a far stronger result than one field
/// dominating one phase.
fn magnitude_integral(phases: &[Frames]) {
    const DPS_PER_LSB: f64 = 0.07;
    println!("
-- ANGULAR PATH LENGTH (axis-independent scale measurement) --");
    println!("   |w| integrated over a 360 deg turn is 360 deg WHATEVER the axis, so this");
    println!("   needs no axis isolation and no mount-angle correction. A real gyro triple");
    println!("   implies ~{DPS_PER_LSB} deg/s per LSB in EVERY phase at once.");

    let Some(rest) = phases.first() else { return };
    if phases.len() < 2 || rest.len() < 20 {
        println!("   not enough phases");
        return;
    }
    let hz = measured_report_hz(phases);
    println!("   report rate MEASURED from this capture: {hz:.1} Hz");

    struct Cand { base: usize, stride: usize, codec: Codec, implied: Vec<f64>, spread: f64 }
    let mut out: Vec<Cand> = Vec::new();

    for codec in [Codec::I16Le, Codec::I16Be, Codec::I24Le, Codec::I24Be] {
        for stride in [2usize, 3, 4, 6] {
            if stride < codec.width() { continue }
            for base in SEARCH_START..SEARCH_END.saturating_sub(2 * stride + codec.width()) {
                let read3 = |f: &Vec<u8>| -> Option<[f64; 3]> {
                    Some([
                        codec.read(f, base)? as f64,
                        codec.read(f, base + stride)? as f64,
                        codec.read(f, base + 2 * stride)? as f64,
                    ])
                };
                // Zero-rate bias from the stationary phase.
                let mut bias = [0.0f64; 3];
                let mut n = 0.0;
                for f in rest.iter() {
                    if let Some(v) = read3(f) {
                        for i in 0..3 { bias[i] += v[i]; }
                        n += 1.0;
                    }
                }
                if n < 10.0 { continue }
                for b in bias.iter_mut() { *b /= n; }

                // Per-phase zero, per component. A drifting bias inflates
                // |w| every sample and lengthens the path, which reads as a
                // smaller implied scale for every candidate at once.
                let path = |frames: &Frames| -> f64 {
                    let rows: Vec<[f64; 3]> = frames.iter().filter_map(read3).collect();
                    if rows.is_empty() {
                        return 0.0;
                    }
                    let mut b = [0.0f64; 3];
                    for i in 0..3 {
                        let col: Vec<f64> = rows.iter().map(|r| r[i]).collect();
                        b[i] = phase_bias(&col).0;
                    }
                    rows.iter().map(|v| {
                        let d = [v[0] - b[0], v[1] - b[1], v[2] - b[2]];
                        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
                    }).sum::<f64>() / hz
                };

                // A resting controller must accumulate almost no path. Without
                // this, a noisy field integrates its own noise into a large
                // "rotation" and lands near any scale you like.
                let rest_raw = path(rest);
                if rest_raw * DPS_PER_LSB > 40.0 { continue }

                let raw: Vec<f64> = phases[1..].iter().map(path).collect();
                // ❗ A field that never changes has zero path, and `360 / 0` is a
                // huge "implied scale" that sorts to the top with a perfect 1.00x
                // spread — the report's all-zero tail bytes did exactly that.
                // Require the triple to MOVE, and to move much more under
                // rotation than at rest.
                if raw.iter().any(|p| *p < 36.0) { continue }
                if raw.iter().fold(f64::MAX, |a, b| a.min(*b)) < rest_raw * 3.0 { continue }

                let implied: Vec<f64> = raw.iter().map(|p| 360.0 / p).collect();
                // Anything outside this range is not a plausible sensor scale;
                // it is a counter, a constant, or noise.
                if implied.iter().any(|v| !v.is_finite() || !(0.0005..5.0).contains(v)) { continue }
                let (lo, hi) = implied.iter().fold((f64::MAX, 0.0f64), |(l, h), v| (l.min(*v), h.max(*v)));
                out.push(Cand { base, stride, codec, implied, spread: hi / lo.max(1e-9) });
            }
        }
    }

    if out.is_empty() {
        println!("   no triple stayed still during the rest phase");
        return;
    }
    // Rank by AGREEMENT across phases first: a triple that implies the same
    // scale three times is meaningful even if that scale is not 0.07, whereas
    // one that implies three different scales is not a rate at all.
    out.sort_by(|a, b| a.spread.partial_cmp(&b.spread).unwrap());
    println!("   base  stride  codec     implied deg/s per LSB per phase        spread");
    for c in out.iter().take(12) {
        let cols: Vec<String> = c.implied.iter().map(|v| format!("{v:>9.5}")).collect();
        let mean = c.implied.iter().sum::<f64>() / c.implied.len() as f64;
        println!(
            "   {:>4}  {:>6}  {:<7} {}   {:>5.2}x{}",
            c.base, c.stride, c.codec.name(), cols.join(" "), c.spread,
            if c.spread < 1.35 && (0.7..1.45).contains(&(mean / DPS_PER_LSB)) {
                "   <== GYRO at the stated scale"
            } else if c.spread < 1.35 {
                "   <- consistent, but not 0.07"
            } else { "" },
        );
    }
    println!("   Spread is max/min across phases; 1.00x means every phase agreed.");

    // Second view: closest to the STATED scale, whatever its consistency. The
    // ranking above answers "is anything a rate?"; this one answers "is
    // anything the gyro?", and a null here is the stronger statement.
    let mut by_scale: Vec<&Cand> = out.iter().collect();
    let mean_of = |c: &Cand| c.implied.iter().sum::<f64>() / c.implied.len() as f64;
    by_scale.sort_by(|a, b| {
        ((mean_of(a) / DPS_PER_LSB).ln().abs())
            .partial_cmp(&((mean_of(b) / DPS_PER_LSB).ln().abs()))
            .unwrap()
    });
    println!("
   Closest to the stated {DPS_PER_LSB} deg/s per LSB, ignoring consistency:");
    for c in by_scale.iter().take(6) {
        let m = mean_of(c);
        println!(
            "   base {:>3} stride {} {:<7} mean {:>9.5}  = {:>6.2}x the stated scale  spread {:>5.2}x",
            c.base, c.stride, c.codec.name(), m, m / DPS_PER_LSB, c.spread,
        );
    }
}

fn turn_scale(phases: &[Frames], accel_base: usize) {
    let lo = accel_base.saturating_sub(16);
    let hi = accel_base.saturating_sub(1);
    println!("
-- COUNTS PER REVOLUTION (measured from a known 360 deg turn) --");
    println!("   Each row: the field's total unwrapped change during each full turn.");
    println!("   The field owning an axis shows a large value in ITS phase and ~0 in others.");
    println!("   'alias' is the largest single-sample step as a fraction of half the");
    println!("   modulus. Unwrapping is only valid below 1.0 — at or above it a step is");
    println!("   indistinguishable from a wrap the other way and the count is WRONG.");
    println!("   {:>5} {:>6} {:>5} {:>12} {:>12} {:>12} {:>8} {:>7}",
        "off", "width", "ord", "roll", "pitch", "yaw", "alias", "score");

    struct Row {
        off: usize, width: usize, be: bool,
        nets: [f64; 3], alias: f64, score: f64,
    }
    let mut rows: Vec<Row> = Vec::new();

    for width in [8usize, 12, 16, 20, 24, 32] {
        for be in [false, true] {
            if width == 8 && be { continue; }
            for off in lo..hi {
                let mut nets = [0.0f64; 3];
                let mut alias = 0.0f64;
                let mut ok = true;
                let half = (1u64 << width) as f64 / 2.0;
                for (i, p) in (1..=3).enumerate() {
                    let Some(frames) = phases.get(p) else { ok = false; break };
                    let raw: Vec<u64> = frames.iter()
                        .filter_map(|f| read_masked(f, off, width, be))
                        .collect();
                    if raw.len() < 100 { ok = false; break; }
                    let series = unwrap_mod(&raw, width);
                    nets[i] = *series.last().unwrap_or(&0.0);
                    // Modular step size, which is what decides whether the
                    // unwrap can be trusted.
                    //
                    // ❗ A raw jump of 65279 on a 16-bit field is NOT a large
                    // step — it is a step of -257 taken the short way round,
                    // and that is exactly what the unwrapper assumes. Computing
                    // this from the unwrapped series gives the same answer for
                    // the same reason. Both forms are correct; the raw
                    // difference is the misleading one.
                    for w in raw.windows(2) {
                        let d = (w[1] as f64 - w[0] as f64).abs();
                        let d = d.min((1u64 << width) as f64 - d);
                        alias = alias.max(d / half);
                    }
                }
                if !ok { continue; }
                let best = nets.iter().cloned().fold(0.0f64, |a, b| if b.abs() > a.abs() { b } else { a });
                let others: f64 = nets.iter().map(|n| n.abs()).sum::<f64>() - best.abs();
                if best.abs() < 64.0 { continue; }
                // Selectivity: one axis should dominate. A field responding
                // equally to all three is not an axis.
                let score = best.abs() / others.max(1.0);
                rows.push(Row { off, width, be, nets, alias, score });
            }
        }
    }

    rows.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut seen: Vec<usize> = Vec::new();
    let mut shown = 0;
    for r in &rows {
        // One row per byte offset, so a single field does not fill the table
        // once at every width that happens to contain it.
        if seen.contains(&r.off) { continue; }
        seen.push(r.off);
        let best = r.nets.iter().cloned().fold(0.0f64, |a, b| if b.abs() > a.abs() { b } else { a });
        let axis = r.nets.iter().position(|n| *n == best).unwrap_or(0);
        let turn = best.abs();
        let p = if turn > 1.0 { turn.log2().round() } else { 0.0 };
        // ❗ Do not call this "aliased". Aliasing is a CAUSE, and asserting it
        // claims a real signal sampled too slowly — which at 200 Hz a
        // hand-turned controller cannot produce. A single-sample step of half
        // the modulus at this rate means the FIELD IS BEING READ WRONG (wrong
        // width, offset or byte order), and labelling that as undersampling
        // sends the next reader looking for a faster capture instead of a
        // better decode.
        //
        // The honest statement is what was measured: the step is too large for
        // the unwrap to be meaningful, whatever the reason.
        let flag = if r.alias >= 1.0 {
            "  step >= half modulus — unwrap MEANINGLESS (likely a wrong decode, not undersampling)"
        } else {
            ""
        };
        println!("   {:>5} {:>6} {:>5} {:>12.0} {:>12.0} {:>12.0} {:>8.2} {:>7.1}   {} -> {:.0} counts/turn (2^{p:.0} {:+.0}%){flag}",
            r.off, r.width, if r.be { "be" } else { "le" },
            r.nets[0], r.nets[1], r.nets[2], r.alias, r.score,
            AXES[axis], turn, (turn / 2f64.powf(p) - 1.0) * 100.0);
        shown += 1;
        if shown >= 12 { break; }
    }
    if shown == 0 {
        println!("   nothing advanced by a usable amount on any axis");
    }
}

/// Solve the orientation encoding by REPROJECTING GRAVITY.
///
/// Every earlier test compared a field against a tilt angle derived from two
/// chosen accelerometer axes, which bakes in assumptions about which axis the
/// operator rotated and in which plane. This one has none: whatever the
/// orientation means, rotating the measured gravity direction by it must land
/// on world "up" in EVERY sample. The residual is an angle in degrees, so a
/// correct model reads near zero and a wrong one cannot hide behind a
/// correlation coefficient that merely looks large.
///
/// Searched jointly, because these are not separable — a wrong scale can be
/// partly absorbed by a wrong axis order, which is how a 0.73 correlation got
/// mistaken for a decode:
/// * counts per revolution (powers of two, 2^12 … 2^32)
/// * how the three values compose (Euler orders, rotation vector, quaternion)
/// * per-axis sign
fn gravity_solve(phases: &[Frames], accel_base: usize) {
    use glam::{Quat, Vec3};
    let base = accel_base.saturating_sub(13);

    // Pool every phase: a model that only fits one sweep is fitting that sweep.
    let mut samples: Vec<([f64; 3], Vec3)> = Vec::new();
    for frames in phases.iter() {
        for f in frames.iter().step_by(3) {
            let a = Vec3::new(
                field(f, accel_base).unwrap_or(0) as f32,
                field(f, accel_base + 4).unwrap_or(0) as f32,
                field(f, accel_base + 8).unwrap_or(0) as f32,
            );
            if a.length() < 1.0 {
                continue;
            }
            let raw = [
                Codec::I24Le.read(f, base).unwrap_or(0) as f64,
                Codec::I24Le.read(f, base + 4).unwrap_or(0) as f64,
                Codec::I24Le.read(f, base + 8).unwrap_or(0) as f64,
            ];
            samples.push((raw, a.normalize()));
        }
    }
    println!("
-- GRAVITY REPROJECTION SOLVE ({} samples) --", samples.len());
    if samples.len() < 200 {
        println!("   too few samples");
        return;
    }

    let orders: [(&str, [usize; 3]); 6] = [
        ("xyz", [0, 1, 2]), ("xzy", [0, 2, 1]), ("yxz", [1, 0, 2]),
        ("yzx", [1, 2, 0]), ("zxy", [2, 0, 1]), ("zyx", [2, 1, 0]),
    ];

    let mut best: Option<(f64, String)> = None;
    let mut report: Vec<(f64, String)> = Vec::new();

    // ⭐ PER-AXIS SCALES. This search assumed ONE counts/turn for all three
    // fields, and the resting frame says that is wrong.
    //
    // At rest — the grip flat on a table, so the decoded angles must be near
    // zero — the three fields read:
    //
    //   LEFT  128912, 65763, 32437  =  2^16.98, 2^16.00, 2^14.99
    //   RIGHT 130445, 65371, 32930  =  2^17.00, 2^15.997, 2^15.01
    //
    // Both halves, three fields, each sitting at exactly HALF the scale of the
    // one before it. Offset-binary fields resting at mid-scale, of three
    // DIFFERENT widths — so a single `turn` applied to all three is wrong by
    // 2x on one axis and 4x on another no matter what value it takes.
    //
    // That is precisely the error that makes a solver report "identity wins":
    // two of the three angles come out at the wrong magnitude, the composed
    // rotation is nonsense, and every candidate scores near the do-nothing
    // baseline. Search a base scale plus a per-axis offset instead.
    for pow in 12..=32u32 {
        for d1 in -3i32..=3 {
        for d2 in -3i32..=3 {
        let turns = [
            (1u64 << pow) as f64,
            (2f64).powi(pow as i32 + d1),
            (2f64).powi(pow as i32 + d2),
        ];
        for signs in 0..8u32 {
            let sg = [
                if signs & 1 != 0 { -1.0 } else { 1.0 },
                if signs & 2 != 0 { -1.0 } else { 1.0 },
                if signs & 4 != 0 { -1.0 } else { 1.0 },
            ];
            for model in 0..8usize {
                let mut err = 0.0f64;
                let mut n = 0.0f64;
                for (raw, a) in &samples {
                    // Wrapped into -0.5..0.5 turns, then radians.
                    let ang: Vec<f32> = (0..3)
                        .map(|i| {
                            let t = (raw[i] / turns[i]).rem_euclid(1.0);
                            let t = if t > 0.5 { t - 1.0 } else { t };
                            (t * std::f64::consts::TAU * sg[i]) as f32
                        })
                        .collect();
                    let q = match model {
                        0..=5 => {
                            let (_, o) = orders[model];
                            let axis = |i: usize| match o[i] {
                                0 => Quat::from_rotation_x(ang[o[i]]),
                                1 => Quat::from_rotation_y(ang[o[i]]),
                                _ => Quat::from_rotation_z(ang[o[i]]),
                            };
                            axis(0) * axis(1) * axis(2)
                        }
                        6 => Quat::from_scaled_axis(Vec3::new(ang[0], ang[1], ang[2])),
                        _ => {
                            // Quaternion imaginary parts, w recovered.
                            let v = Vec3::new(
                                ang[0] / std::f32::consts::PI,
                                ang[1] / std::f32::consts::PI,
                                ang[2] / std::f32::consts::PI,
                            );
                            let l2 = v.length_squared();
                            if l2 > 1.0 { Quat::from_xyzw(v.x, v.y, v.z, 0.0).normalize() }
                            else { Quat::from_xyzw(v.x, v.y, v.z, (1.0f32 - l2).sqrt()) }
                        }
                    };
                    let up = (q * *a).normalize();
                    err += (up.z.clamp(-1.0, 1.0) as f64).acos();
                    n += 1.0;
                }
                let mean_deg = (err / n).to_degrees();
                let name = format!(
                    "2^[{pow},{},{}] {:<4} signs[{}{}{}]",
                    pow as i32 + d1,
                    pow as i32 + d2,
                    if model <= 5 { orders[model].0 } else if model == 6 { "rvec" } else { "quat" },
                    if sg[0] < 0.0 { '-' } else { '+' },
                    if sg[1] < 0.0 { '-' } else { '+' },
                    if sg[2] < 0.0 { '-' } else { '+' },
                );
                report.push((mean_deg, name.clone()));
                if best.as_ref().is_none_or(|(b, _)| mean_deg < *b) {
                    best = Some((mean_deg, name));
                }
            }
        }
        }
        }
    }

    // ❗ THE CONTROL: what does doing NOTHING score?
    //
    // At a large assumed scale every decoded angle collapses toward zero and
    // the quaternion becomes identity, so the search happily reports a "best"
    // model that applies no rotation at all. Without this line that reads as a
    // solved decode; with it, a model has to actually beat leaving the data
    // alone. This is the same trap as the earlier 0.73 correlation.
    let identity_err = {
        let mut e = 0.0;
        for (_, a) in &samples {
            e += (a.z.clamp(-1.0, 1.0) as f64).acos();
        }
        (e / samples.len() as f64).to_degrees()
    };

    report.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    println!("   {:>9}  model", "residual");
    for (e, name) in report.iter().take(8) {
        println!("   {e:>7.1} deg  {name}");
    }
    // A random orientation averages ~90 deg against a fixed axis, so anything
    // near that is no better than guessing.
    let (b, name) = best.unwrap();
    println!("   {identity_err:>7.1} deg  IDENTITY (apply no rotation at all) — the control");
    if b < identity_err * 0.5 {
        println!("   ✅ best model beats identity by {:.0}x: {name}", identity_err / b.max(1e-6));
    } else {
        println!("   ⛔ best residual {b:.1} deg vs {identity_err:.1} deg for doing NOTHING.");
        println!("      No scale/order/sign combination explains the data: these three values");
        println!("      are not three angles composing a rotation. The decode is UNSOLVED.");
    }
}

/// Read `width` bits at a byte offset as an unsigned value, LE or BE.
///
/// Deliberately unsigned and masked: for unwrapping, only the value modulo
/// 2^width matters, and committing to a sign convention before the width is
/// even known is what produced the last wrong answer.
fn read_masked(frame: &[u8], off: usize, width: usize, be: bool) -> Option<u64> {
    let nbytes = width.div_ceil(8);
    let b = frame.get(off..off + nbytes)?;
    let mut v: u64 = 0;
    if be {
        for x in b { v = (v << 8) | *x as u64; }
    } else {
        for (i, x) in b.iter().enumerate() { v |= (*x as u64) << (8 * i); }
    }
    Some(v & ((1u64 << width) - 1))
}

/// Unwrap a modular series into a continuous one.
fn unwrap_mod(v: &[u64], width: usize) -> Vec<f64> {
    let m = (1u64 << width) as f64;
    let half = m / 2.0;
    let mut out = Vec::with_capacity(v.len());
    let mut acc = 0.0;
    let mut prev = v.first().copied().unwrap_or(0) as f64;
    for x in v {
        let mut d = *x as f64 - prev;
        if d > half { d -= m; } else if d < -half { d += m; }
        acc += d;
        prev = *x as f64;
        out.push(acc);
    }
    out
}

/// Search offset x width x byte-order for the encoding that best tracks the
/// accelerometer's absolute tilt.
///
/// Assuming the answer is what went wrong before: "24-bit little-endian at a
/// 4-byte stride" was inferred from a hex dump and a wrap point, then scored
/// only against itself. A decoding that is actually right correlates ~0.99 with
/// a sound reference; the 0.73 that was accepted as confirmation should have
/// been read as "close to something, but not this".
fn encoding_search(phases: &[Frames], accel_base: usize) {
    println!("
-- ENCODING SEARCH (unwrapped field vs accelerometer tilt) --");
    println!("   Roll and pitch only — gravity cannot see yaw.");
    println!("   {:>5} {:>6} {:>6} {:>7} {:>9} {:>13}", "off", "width", "order", "phase", "r", "counts/turn");

    let lo = accel_base.saturating_sub(16);
    let hi = accel_base.saturating_sub(1);

    let mut rows: Vec<(f64, usize, usize, bool, &'static str, f64)> = Vec::new();
    for (phase, ax_a, ax_b, name) in [(1usize, 1usize, 2usize, "roll"), (2, 0, 2, "pitch")] {
        let frames = &phases[phase];
        if frames.len() < 100 { continue; }
        let truth = accel_angles(frames, accel_base, ax_a, ax_b);

        for width in [12usize, 14, 16, 18, 20, 21, 22, 24, 28, 32] {
            for be in [false, true] {
                for off in lo..hi {
                    let raw: Vec<u64> = frames.iter()
                        .filter_map(|f| read_masked(f, off, width, be))
                        .collect();
                    if raw.len() < 100 { continue; }
                    let series = unwrap_mod(&raw, width);
                    let n = truth.len().min(series.len());
                    let r = correlate(&truth[..n], &series[..n]);
                    if r.abs() < 0.9 { continue; }
                    // ❗ Reject anything that merely rises with TIME.
                    //
                    // Over one full turn the accelerometer tilt is monotonic,
                    // and so is the timestamp — so the timestamp correlates
                    // 0.98 with it while having nothing to do with rotation.
                    // Byte 17/18 (the timestamp high byte and the constant
                    // length byte) topped the "pitch" results for exactly this
                    // reason. A real axis tracks the tilt, not the clock.
                    let time: Vec<f64> = (0..n).map(|i| i as f64).collect();
                    if correlate(&series[..n], &time).abs() > 0.9 { continue; }
                    // A field must have the RESOLUTION to track a smooth turn.
                    // Byte 18 is the constant length byte plus two bits of its
                    // neighbour — four distinct values total — and a four-step
                    // staircase still scored 0.984 against a smooth 360° sweep.
                    // Correlation does not care how coarse the signal is; this
                    // does.
                    let mut distinct: Vec<i64> = series[..n].iter().map(|v| *v as i64).collect();
                    distinct.sort_unstable();
                    distinct.dedup();
                    if distinct.len() < 64 { continue; }
                    // Slope -> counts per turn, so a plausible encoding can be
                    // recognised by landing on a round number.
                    let mt = truth[..n].iter().sum::<f64>() / n as f64;
                    let mc = series[..n].iter().sum::<f64>() / n as f64;
                    let (mut num, mut den) = (0.0, 0.0);
                    for i in 0..n {
                        let (t, c) = (truth[i] - mt, series[i] - mc);
                        num += t * c; den += t * t;
                    }
                    if den <= 0.0 { continue; }
                    let turn = (num / den).abs() * std::f64::consts::TAU;
                    rows.push((r.abs(), off, width, be, name, turn));
                }
            }
        }
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    if rows.is_empty() {
        println!("   NOTHING reached |r| = 0.9 — the wrapping-angle model is wrong, not just its scale.");
        return;
    }
    let mut seen: Vec<(usize, usize, bool)> = Vec::new();
    for (r, off, width, be, name, turn) in rows.iter() {
        if seen.contains(&(*off, *width, *be)) { continue; }
        seen.push((*off, *width, *be));
        println!("   {off:>5} {width:>6} {:>6} {name:>7} {r:>9.3} {turn:>13.0}",
            if *be { "be" } else { "le" });
        if seen.len() >= 14 { break; }
    }
}

/// Measure how many angle counts make one revolution, from the data.
///
/// ❗ This exists because of a real error: the field was seen to WRAP at
/// +/-2^23 and that was taken to mean 2^24 counts per revolution. Field width
/// is not angular scale. A 24-bit field wrapping tells you the field is 24 bits
/// and nothing whatever about how much rotation fills it, and assuming they
/// matched made the app over-rotate — a small turn spun the 3D model through
/// several full revolutions, and the wrap correction fired at the wrong
/// threshold so fast rotations decoded as garbage.
///
/// The accelerometer measures absolute tilt, so roll and pitch give a
/// ground-truth angle in radians to fit the counts against. Slope of
/// counts-vs-radians is counts per radian; times 2*pi is counts per turn.
///
/// Yaw is excluded: gravity cannot see it.
fn scale_probe(phases: &[Frames], accel_base: usize) {
    let base = accel_base.saturating_sub(13);
    println!("
-- ANGLE SCALE (counts per revolution, MEASURED not assumed) --");
    println!("   Fitting angle-field counts against accelerometer tilt in radians.");
    println!("   {:>6} {:>7} {:>12} {:>14} {:>10}", "field", "phase", "counts/rad", "counts/turn", "fit r");

    // (phase index, accel axes forming the tilt plane, which angle field)
    for (phase, ax_a, ax_b, field) in [(1usize, 1usize, 2usize, 0usize), (2, 0, 2, 1)] {
        let frames = &phases[phase];
        if frames.len() < 100 {
            continue;
        }
        // Ground truth: unwrapped accelerometer tilt, radians.
        let truth = accel_angles(frames, accel_base, ax_a, ax_b);

        // Candidate: the angle field, unwrapped by accumulating wrapped deltas
        // so its own 24-bit wraps do not appear as jumps.
        let off = base + field * 4;
        let raw: Vec<f64> = frames
            .iter()
            .map(|f| Codec::I24Le.read(f, off).unwrap_or(0) as f64)
            .collect();
        let mut counts = Vec::with_capacity(raw.len());
        let mut acc = 0.0;
        for d in wrapped_delta(&raw, 24) {
            acc += d;
            counts.push(acc);
        }

        // Least-squares slope through the demeaned data.
        let n = truth.len().min(counts.len());
        if n < 100 {
            continue;
        }
        let mt = truth[..n].iter().sum::<f64>() / n as f64;
        let mc = counts[..n].iter().sum::<f64>() / n as f64;
        let (mut num, mut den) = (0.0, 0.0);
        for i in 0..n {
            let (t, c) = (truth[i] - mt, counts[i] - mc);
            num += t * c;
            den += t * t;
        }
        if den <= 0.0 {
            continue;
        }
        let slope = num / den;
        let r = correlate(&truth[..n], &counts[..n]);
        println!("   {:>6} {:>7} {slope:>12.0} {:>14.0} {r:>10.3}",
            off, PHASES[phase].name, slope.abs() * std::f64::consts::TAU);
        // Powers of two are the likely intent; naming the nearest one makes a
        // measured 16.7M vs 65536 impossible to misread.
        let turn = slope.abs() * std::f64::consts::TAU;
        if turn > 1.0 {
            let p = turn.log2().round();
            println!("          nearest power of two: 2^{p:.0} = {} ({:+.1}% off)",
                2f64.powf(p), (turn / 2f64.powf(p) - 1.0) * 100.0);
        }
    }
    println!("   A low |r| means the fit is meaningless — check the reference before believing the scale.");
}

/// Is the magnetometer feeding these angles?
///
/// The three fields are wrapping ABSOLUTE angles, not rates, so they are a
/// fused output — and that is where a magnetometer would show up, since the 12
/// bytes are fully accounted for as three tags plus three angles and have no
/// room for a separate raw magnetometer block.
///
/// The discriminator is drift. Roll and pitch can always be held absolute by
/// gravity, which the accelerometer measures directly. **Yaw cannot**: nothing
/// in an accel+gyro system observes rotation about the gravity vector, so a
/// 6-axis fusion must integrate the gyro and accumulate error without bound. A
/// 9-axis fusion corrects yaw against magnetic north and returns to the same
/// reading at the same physical heading.
///
/// So: return the grip to its starting orientation and compare. Yaw coming back
/// to where it started is a magnetometer; yaw ending somewhere else is
/// integrated gyro drift. Each phase of the sweep ends back at neutral, which
/// makes every phase boundary a free instance of this test.
fn magnetometer_test(phases: &[Frames], base: usize) {
    const TURN: f64 = 16777216.0; // 2^24 spans a full revolution
    let deg = |v: f64| v * 360.0 / TURN;

    println!("
-- MAGNETOMETER TEST (does yaw hold absolute, or drift?) --");
    println!("   Every phase starts and ends with the grip flat in the SAME orientation,");
    println!("   so an absolute angle must read the same at both ends. Roll and pitch are");
    println!("   held by gravity either way; only YAW distinguishes 6-axis from 9-axis.");
    // ❗ `travel` is what makes the drift column mean anything.
    //
    // A field that never moves has a net drift of zero and looks perfectly
    // "absolute", which is the easiest possible way to mistake a constant for a
    // compass. Total path length separates the two: a real heading accumulates
    // hundreds of degrees during the yaw sweep and still comes back to where it
    // started, while a constant accumulates nothing.
    println!("   {:>7} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "field", "phase", "start", "end", "net drift", "travel");

    for i in 0..3 {
        let off = base + i * 4;
        let name = ["roll", "pitch", "YAW", "?"][i.min(3)];
        for (p, frames) in phases.iter().enumerate() {
            if frames.len() < 50 {
                continue;
            }
            let read = |f: &Vec<u8>| Codec::I24Le.read(f, off).unwrap_or(0) as f64;
            // Average a few frames at each end so a single noisy sample cannot
            // masquerade as drift.
            let n = 20.min(frames.len() / 4);
            let s: f64 = frames[..n].iter().map(read).sum::<f64>() / n as f64;
            let e: f64 = frames[frames.len() - n..].iter().map(read).sum::<f64>() / n as f64;
            let mut d = e - s;
            if d > TURN / 2.0 {
                d -= TURN;
            } else if d < -TURN / 2.0 {
                d += TURN;
            }
            let vals: Vec<f64> = frames.iter().map(read).collect();
            let travel: f64 = wrapped_delta(&vals, 24).iter().map(|x| x.abs()).sum();
            println!("   {:>7} {:>8} {:>9.1}° {:>9.1}° {:>9.1}° {:>9.0}°",
                format!("{off}/{name}"), PHASES[p].name, deg(s), deg(e), deg(d), deg(travel));
        }
    }
    println!("   A yaw drift of a few degrees per phase = 9-axis (magnetometer active).");
    println!("   Tens of degrees, growing with each phase = 6-axis integration, no mag.");
}

/// Apply a named transform, matching `omega_scan`'s scoring exactly.
fn apply(raw: &[f64], tf: &str, width: usize, sm: usize) -> Vec<f64> {
    match tf {
        "d/dt" => differentiate(&smooth(raw, sm)),
        "integral" => integrate(&smooth(raw, sm)),
        "wrapDelta" => smooth(&wrapped_delta(raw, width), sm),
        _ => smooth(raw, sm),
    }
}

/// Rebuild a candidate's RAW series and its bit width from the printed label.
///
/// Reparsing the label keeps one definition of what each candidate means; a
/// second construction path is how a follow-up check ends up silently scoring a
/// different field than the one it claims to.
fn candidate_raw(frames: &Frames, label: &str, lo: usize, hi: usize) -> (Vec<f64>, usize) {
    let tok: Vec<&str> = label.split_whitespace().collect();
    let width;
    let raw: Vec<f64> = if tok[0] == "byte" {
        let off: usize = tok[1].parse().unwrap_or(0);
        let c = Codec::ALL
            .iter()
            .find(|c| c.name() == tok[2])
            .copied()
            .unwrap_or(Codec::I16Le);
        width = c.width() * 8;
        frames.iter().map(|f| c.read(f, off).unwrap_or(0) as f64).collect()
    } else {
        let bit: usize = tok[1].parse().unwrap_or(0);
        let w: usize = tok[2].trim_start_matches('w').parse().unwrap_or(16);
        let msb = tok[3] == "msb";
        width = w;
        frames
            .iter()
            .map(|f| {
                f.get(lo..hi)
                    .and_then(|sl| if msb { bit_field_msb(sl, bit, w) } else { bit_field(sl, bit, w) })
                    .unwrap_or(0) as f64
            })
            .collect()
    };
    (raw, width)
}

/// Score every candidate field against the gravity-derived angular velocity,
/// over ALL motion phases at once.
///
/// Two changes from every earlier scan, both of which matter:
///
/// * The reference is [`gravity_rates`], not a differenced `atan2` — see there
///   for why the old one could not have worked.
/// * Phases are CONCATENATED. A single sweep gives ~800 frames of one rotation;
///   all three give ~2400 covering every direction. A real gyro axis tracks its
///   reference through the whole thing, while a field that merely happens to
///   drift during one sweep cannot. Scoring per-phase let coincidences win.
fn omega_scan(phases: &[Frames], accel_base: usize) {
    const SMOOTH: usize = 5;
    let lo = accel_base.saturating_sub(14);
    let hi = accel_base.saturating_sub(2);

    // Concatenate the motion phases, keeping the reference aligned frame for
    // frame with the candidate series.
    let mut frames: Frames = Vec::new();
    let mut omega: Vec<[f64; 3]> = Vec::new();
    for p in 1..phases.len() {
        omega.extend(gravity_rates(&phases[p], accel_base));
        frames.extend(phases[p].iter().cloned());
    }
    if frames.len() < 100 {
        return;
    }
    // Six references: angular velocity per axis, and its integral (the angle).
    // A field holding an integrated angle correlates with the second set, not
    // the first — which is the case the block's smooth monotonic drift at rest
    // keeps pointing at.
    let mut refs: Vec<Vec<f64>> = (0..3)
        .map(|i| smooth(&omega.iter().map(|w| w[i]).collect::<Vec<_>>(), SMOOTH))
        .collect();
    for i in 0..3 {
        refs.push(integrate(&refs[i]));
    }

    println!("
-- OMEGA SCAN (gravity cross-product angular velocity, all phases, bytes {lo}..{hi}) --");
    for (i, r) in refs.iter().enumerate() {
        let m = r.iter().sum::<f64>() / r.len() as f64;
        let sd = (r.iter().map(|x| (x - m).powi(2)).sum::<f64>() / r.len() as f64).sqrt();
        let peak = r.iter().cloned().fold(0.0f64, |a, b| a.max(b.abs()));
        println!("   ref omega[{i}]: n={} sd={sd:.5} peak={peak:.5} rad/frame", r.len());
    }
    println!("   (a hand sweep is ~0.008 rad/frame; a peak far above that means the");
    println!("    reference is noise-dominated and any null result is meaningless)");

    let varies = |v: &[f64]| -> bool {
        let mut s: Vec<i64> = v.iter().map(|x| *x as i64).collect();
        s.sort_unstable();
        s.dedup();
        s.len() >= 8
    };
    // Three readings of every candidate, because "responds to motion but not
    // linearly to rate" has only a few plausible causes and each shows up under
    // a different transform:
    //   raw     — a plain rate gyro
    //   d/dt    — the field is an integrated ANGLE, so its derivative is rate
    //   ∫dt     — the field is DELTA-coded, so its running sum is the rate
    // Testing only `raw` is what every previous scan did.
    // `raw` here is UNSMOOTHED. The wrapped difference must be taken before any
    // smoothing: averaging across a wrap blends two ends of the range and
    // destroys the very discontinuity this transform exists to undo.
    let score = |raw: &[f64], width: usize| -> (&'static str, usize, f64) {
        let s = smooth(raw, SMOOTH);
        let variants: [(&'static str, Vec<f64>); 4] = [
            ("raw", s.clone()),
            ("d/dt", differentiate(&s)),
            ("integral", integrate(&s)),
            ("wrapDelta", smooth(&wrapped_delta(raw, width), SMOOTH)),
        ];
        let mut best = ("raw", 0usize, 0.0f64);
        for (name, v) in &variants {
            for (i, r) in refs.iter().enumerate() {
                let c = correlate(v, r);
                if c.abs() > best.2.abs() {
                    best = (name, i, c);
                }
            }
        }
        best
    };

    // (label, transform, ref index, r)
    let mut rows: Vec<(String, &'static str, usize, f64)> = Vec::new();

    // Byte-aligned candidates across the whole report, so nothing is assumed
    // about where the block is.
    for c in Codec::ALL {
        for off in SEARCH_START..SEARCH_END.saturating_sub(c.width()) {
            if (0..3).any(|i| {
                let a = accel_base + i * 4;
                off + c.width() > a && a + 2 > off
            }) {
                continue;
            }
            let s: Vec<f64> = frames.iter().map(|f| c.read(f, off).unwrap_or(0) as f64).collect();
            if !varies(&smooth(&s, SMOOTH)) {
                continue;
            }
            let (tf, axis, r) = score(&s, c.width() * 8);
            if r.abs() > 0.5 {
                rows.push((format!("byte {off} {}", c.name()), tf, axis, r));
            }
        }
    }
    // Packed candidates inside the motion block, both bit orders.
    for msb in [false, true] {
        for w in 8usize..=24 {
            for bit in 0..(12 * 8usize).saturating_sub(w) {
                let s: Vec<f64> = frames
                    .iter()
                    .map(|f| {
                        f.get(lo..hi)
                            .and_then(|sl| if msb { bit_field_msb(sl, bit, w) } else { bit_field(sl, bit, w) })
                            .unwrap_or(0) as f64
                    })
                    .collect();
                if !varies(&smooth(&s, SMOOTH)) {
                    continue;
                }
                let (tf, axis, r) = score(&s, w);
                if r.abs() > 0.5 {
                    rows.push((
                        format!("bit {bit} w{w} {} (byte {}+{})", if msb { "msb" } else { "lsb" }, lo + bit / 8, bit % 8),
                        tf,
                        axis,
                        r,
                    ));
                }
            }
        }
    }

    // ── Structural hypothesis, tested directly rather than by best-fit ──
    //
    // The raw dump shows the block as three repeats of [tag byte][3-byte LE
    // value]: tags at block offsets 0/4/8 (the near-constant 0x00, 0x01,
    // 0x01/0x81), values at offsets 1/5/9. Relative to the accelerometer that
    // puts the first value at `accel_base-13` on both halves.
    //
    // Scanning finds ragged partial reads that clip the field at odd bit
    // boundaries; naming the structure and testing it gives the whole field,
    // and a 3x3 matrix says which axis maps to which rather than assuming the
    // order matches the accelerometer's.
    let base = accel_base.saturating_sub(13);
    println!("
   Structural test — 3 x i24le at {base}/{}/{} (wrapped delta):",
        base + 4, base + 8);
    println!("      {:>10} {:>9} {:>9} {:>9}", "field", "omega[0]", "omega[1]", "omega[2]");
    let mut matched = 0;
    for i in 0..3 {
        let off = base + i * 4;
        let raw: Vec<f64> = frames
            .iter()
            .map(|f| Codec::I24Le.read(f, off).unwrap_or(0) as f64)
            .collect();
        let d = smooth(&wrapped_delta(&raw, 24), SMOOTH);
        let cs: Vec<f64> = refs[..3].iter().map(|r| correlate(&d, r)).collect();
        let best = cs.iter().cloned().fold(0.0f64, |a, b| a.max(b.abs()));
        if best > 0.7 {
            matched += 1;
        }
        let tag = match cs.iter().position(|c| c.abs() == best).filter(|_| best > 0.7) {
            Some(a) => format!("  <== gyro axis {a}"),
            None => String::new(),
        };
        println!("      byte {off:>5} {:>9.3} {:>9.3} {:>9.3}{tag}", cs[0], cs[1], cs[2]);
    }
    println!("      ({matched} of 3 matched an angular-velocity axis; the third is yaw,");
    println!("       which gravity cannot see, so 2 of 3 is the expected best case)");

    rate_integral(phases);
    magnitude_integral(phases);
    if let Some(off) = std::env::args()
        .position(|a| a == "--trace")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
    {
        for codec in [Codec::I16Le, Codec::I24Le] {
            trace_field(phases, off, codec);
        }
    }
    axis_solve(phases, accel_base);
    turn_scale(phases, accel_base);
    gravity_solve(phases, accel_base);
    fused_angle_solve(phases, accel_base);
    encoding_search(phases, accel_base);
    scale_probe(phases, accel_base);
    magnetometer_test(phases, base);

    rows.sort_by(|a, b| b.3.abs().partial_cmp(&a.3.abs()).unwrap());
    if rows.is_empty() {
        println!("   nothing tracked the angular velocity on any axis, under any transform");
        return;
    }

    // Collapse candidates that score identically. A field's high bits are
    // constant and its low bits are noise, so dozens of (offset, width) pairs
    // read the SAME underlying value and fill the table with one finding
    // repeated twenty times.
    let mut seen: Vec<f64> = Vec::new();
    let unique: Vec<_> = rows
        .iter()
        .filter(|(_, _, _, r)| {
            if seen.iter().any(|s| (s - r).abs() < 1e-6) {
                false
            } else {
                seen.push(*r);
                true
            }
        })
        .take(8)
        .collect();

    // ❗ THE CONTROL: correlation against a plain linear ramp.
    //
    // `angle[..]` references are large, slow and roughly monotonic (sd 0.42
    // against 0.011 for the rate), so ANY slowly-drifting quantity — a
    // temperature, a battery reading, a free-running counter — correlates with
    // them at 0.95 while having nothing to do with rotation. A real gyro axis
    // tracks its own axis and NOT the clock; a drift tracks the clock. Without
    // this column the two are indistinguishable, and the drift is the more
    // likely thing to find by accident.
    let time: Vec<f64> = (0..frames.len()).map(|i| i as f64).collect();

    // Best candidate for EACH reference axis, not just the global winner.
    //
    // An orientation or gyro triple has three fields at a regular spacing, and
    // reporting only the strongest hides the other two — which are the evidence
    // that the first one is part of a sensor rather than a coincidence.
    println!("
   Best candidate per reference axis (a real triple sits at regular spacing):");
    for (ri, rname) in ["omega[0]", "omega[1]", "omega[2]", "angle[0]", "angle[1]", "angle[2]"]
        .iter()
        .enumerate()
    {
        let mut best: Option<(String, &'static str, f64)> = None;
        for (label, tf, _, _) in rows.iter() {
            let (raw, width) = candidate_raw(&frames, label, lo, hi);
            let v = apply(&raw, tf, width, SMOOTH);
            let c = correlate(&v, &refs[ri]);
            if best.as_ref().map_or(true, |b| c.abs() > b.2.abs()) {
                best = Some((label.clone(), tf, c));
            }
        }
        match best {
            Some((l, tf, c)) if c.abs() > 0.6 => {
                println!("     {rname:>9}: r={c:>7.3}  {l} [{tf}]")
            }
            _ => println!("     {rname:>9}: nothing above 0.6"),
        }
    }

    println!("
{:>30} {:>9} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}  {:>7}",
        "candidate", "transform", "om[0]", "om[1]", "om[2]", "ang[0]", "ang[1]", "ang[2]", "TIME");
    for (label, tf, _, _) in unique {
        let (raw, width) = candidate_raw(&frames, label, lo, hi);
        let v = apply(&raw, tf, width, SMOOTH);
        let cs: Vec<f64> = refs.iter().map(|r| correlate(&v, r)).collect();
        let ct = correlate(&v, &time);
        // Axis-specific AND not just drift is the bar. Anything that matches
        // the clock as well as it matches an axis is a clock.
        let best_ang = cs[3..].iter().cloned().fold(0.0f64, |a, b| a.max(b.abs()));
        let verdict = if cs[..3].iter().any(|c| c.abs() > 0.7) {
            "  <== TRACKS RATE"
        } else if best_ang > 0.85 && ct.abs() < 0.5 {
            "  <== angle-like, NOT drift"
        } else if ct.abs() > 0.85 {
            "  (drift — tracks the clock)"
        } else {
            ""
        };
        println!("{label:>30} {tf:>9} {:>7.3} {:>7.3} {:>7.3} {:>7.3} {:>7.3} {:>7.3}  {ct:>7.3}{verdict}",
            cs[0], cs[1], cs[2], cs[3], cs[4], cs[5]);
    }
}

/// Exhaustively scan the 12-byte motion block, both bit orders.
///
/// Every other notifying characteristic on the device is subscribed and silent,
/// so the gyro must be inside this report; the liveness map puts all remaining
/// unexplained variation in exactly 12 bytes at `accel_base-14 .. accel_base-3`
/// — the same size and 3x4 shape as the accelerometer block. That makes the
/// search finite and worth doing exhaustively rather than by hypothesis.
///
/// ❗ **MSB-first extraction is included and has never been tried.** Every
/// packed scan so far read bit-fields LSB-first, which is one arbitrary choice
/// out of two; a field packed the other way is unreadable by the wrong one and
/// looks exactly like noise.
fn block_scan(phases: &[Frames], accel_base: usize) {
    const SMOOTH: usize = 7;
    let lo = accel_base.saturating_sub(14);
    let hi = accel_base.saturating_sub(2);
    println!("
-- MOTION BLOCK, EXHAUSTIVE (bytes {lo}..{hi}, both bit orders) --");

    let roll_rate = smooth(&accel_rates(&phases[1], accel_base, 1, 2), SMOOTH);
    let pitch_rate = smooth(&accel_rates(&phases[2], accel_base, 0, 2), SMOOTH);
    let roll_ang = smooth(&accel_angles(&phases[1], accel_base, 1, 2), SMOOTH);
    let pitch_ang = smooth(&accel_angles(&phases[2], accel_base, 0, 2), SMOOTH);

    let series = |frames: &Frames, bit: usize, w: usize, msb: bool| -> Vec<f64> {
        let raw: Vec<f64> = frames
            .iter()
            .map(|f| {
                f.get(lo..hi)
                    .and_then(|sl| if msb { bit_field_msb(sl, bit, w) } else { bit_field(sl, bit, w) })
                    .unwrap_or(0) as f64
            })
            .collect();
        smooth(&raw, SMOOTH)
    };
    // Same guard as the byte scan: a field with almost no distinct values
    // correlates beautifully with any monotonic ramp and means nothing.
    let varies = |v: &[f64]| -> bool {
        let mut s: Vec<i64> = v.iter().map(|x| *x as i64).collect();
        s.sort_unstable();
        s.dedup();
        s.len() >= 8
    };

    let mut rows: Vec<(usize, usize, bool, f64, f64, f64, f64)> = Vec::new();
    for msb in [false, true] {
        for w in 8usize..=24 {
            for bit in 0..(12 * 8usize).saturating_sub(w) {
                let sr = series(&phases[1], bit, w, msb);
                let sp = series(&phases[2], bit, w, msb);
                if !varies(&sr) || !varies(&sp) {
                    continue;
                }
                let (rr, rp) = (correlate(&sr, &roll_rate), correlate(&sp, &pitch_rate));
                let (ar, ap) = (correlate(&sr, &roll_ang), correlate(&sp, &pitch_ang));
                if [rr, rp, ar, ap].iter().any(|v| v.abs() > 0.75) {
                    rows.push((bit, w, msb, rr, rp, ar, ap));
                }
            }
        }
    }
    rows.sort_by(|a, b| {
        let k = |r: &(usize, usize, bool, f64, f64, f64, f64)| {
            r.3.abs().max(r.4.abs()).max(r.5.abs()).max(r.6.abs())
        };
        k(b).partial_cmp(&k(a)).unwrap()
    });
    if rows.is_empty() {
        println!("   nothing in the block tracked rate or angle, either bit order");
        return;
    }
    println!("{:>5} {:>5} {:>5} {:>9} {:>9} {:>9} {:>9}",
        "bit", "width", "order", "rate(rl)", "rate(pt)", "ang(rl)", "ang(pt)");
    for (bit, w, msb, rr, rp, ar, ap) in rows.iter().take(16) {
        // Byte and bit within the block, so a hit can be located by hand.
        println!("{bit:>5} {w:>5} {:>5} {rr:>9.3} {rp:>9.3} {ar:>9.3} {ap:>9.3}   byte {}+{}",
            if *msb { "msb" } else { "lsb" }, lo + bit / 8, bit % 8);
    }
    println!("   Cross-check the two halves: a real axis appears in BOTH at the same");
    println!("   block-relative bit, since the block is located relative to the accel.");
}

/// Extract a signed field of `width` bits starting at `bit_off`, MSB-first.
fn bit_field_msb(frame: &[u8], bit_off: usize, width: usize) -> Option<i64> {
    if (bit_off + width).div_ceil(8) > frame.len() {
        return None;
    }
    let mut v: u64 = 0;
    for i in 0..width {
        let b = bit_off + i;
        let bit = (frame[b / 8] >> (7 - (b % 8))) & 1;
        v = (v << 1) | bit as u64;
    }
    let sign = 1u64 << (width - 1);
    Some(if v & sign != 0 { (v as i64) - (1i64 << width) } else { v as i64 })
}

/// Search the packed region between the timestamp and the accelerometer for
/// bit-aligned signed fields that behave like gyro axes.
///
/// Byte-aligned scans found nothing there but saturating garbage, while the
/// same bytes read as u32 sit within a fraction of a percent of 2^24 and 2^23.
/// Values centred on power-of-two biases mean a PACKED encoding — fields that
/// do not start on byte boundaries — so the search has to move a bit at a time.
///
/// Six sensor axes (3 gyro + 3 magnetometer) in this region would be about
/// 10-11 bits each, which also matches the persistent ~1023-range field the
/// byte-aligned scan kept flagging.
fn bit_scan(phases: &[Frames], accel_base: usize) {
    // The region runs from the end of the button/stick fields to the accel
    // block. It used to start at `accel_base - 9`, which for an accel at byte
    // 33 meant bytes 24..33 — and the report's live motion bytes begin at 19,
    // so five of them were never scanned at all. The window is now derived from
    // where data actually is rather than from a fixed back-off.
    let start_byte = SEARCH_START;
    let region_bits = (accel_base.saturating_sub(start_byte)) * 8;

    println!("
-- PACKED BIT-FIELD SCAN (bytes {start_byte}..{accel_base}) --");
    println!("{:>6} {:>6} {:>10} {:>10} {:>10} {:>7}", "bit", "width", "roll", "pitch", "yaw", "sel");

    let sub = |frames: &Frames, bit: usize, w: usize| -> (i64, i64) {
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for f in frames {
            if let Some(v) = bit_field(&f[start_byte..], bit, w) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if lo > hi { (0, 0) } else { (lo, hi) }
    };

    let mut rows: Vec<(usize, usize, [i64; 3], f64)> = Vec::new();
    for width in [10usize, 11, 12, 14, 16, 20, 21] {
        for bit in 0..region_bits.saturating_sub(width) {
            let rest = { let (l, h) = sub(&phases[0], bit, width); h - l };
            let r = [
                { let (l, h) = sub(&phases[1], bit, width); h - l },
                { let (l, h) = sub(&phases[2], bit, width); h - l },
                { let (l, h) = sub(&phases[3], bit, width); h - l },
            ];
            let best = r.iter().copied().max().unwrap_or(0);
            // Must move a lot more during one sweep than at rest, and must not
            // saturate its own width — a saturating field is a bad decoding.
            let full = 1i64 << width;
            if best < full / 8 || best > full * 9 / 10 || best < rest * 4 {
                continue;
            }
            let others = r.iter().copied().sum::<i64>() - best;
            rows.push((bit, width, r, best as f64 / others.max(1) as f64));
        }
    }
    rows.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap());
    if rows.is_empty() {
        println!("  nothing in this region behaved like a rotation-selective field");
        return;
    }
    for (bit, width, r, sel) in rows.iter().take(12) {
        let axis = (0..3).max_by_key(|i| r[*i]).unwrap();
        let (lo, hi) = sub(&phases[axis + 1], *bit, *width);
        let tag = if *sel > 2.0 { format!("  <== {} ({lo}..{hi})", AXES[axis]) } else { String::new() };
        println!("{bit:>6} {width:>6} {:>10} {:>10} {:>10} {sel:>7.1}{tag}", r[0], r[1], r[2]);
    }
    println!("  bit is relative to byte {start_byte}; a real field repeats at a regular");
    println!("  spacing for its three axes, so look for three strong rows evenly spaced.");

    rate_scan(phases, accel_base, start_byte, region_bits);
}

/// Compare the halves. They are bolted together and cannot rotate at different
/// rates, so any axis identified on only one of them is suspect; and their
/// neutral accelerometer readings differ purely by the mounting rotation.
fn cross_check(links: &[Link], rec: &[HashMap<Key, Frames>]) {
    if links.len() < 2 {
        return;
    }
    println!("\n\n========== CROSS-CHECK ==========");
    println!("Both halves saw the SAME motion, so gyro magnitudes should match per axis,");
    println!("and any difference in the neutral accel vector IS the fixed mounting offset.");
    let empty: Frames = Vec::new();
    for (ai, axis) in AXES.iter().enumerate() {
        println!("\n-- {axis} --");
        for link in links {
            let ph = |i: usize| rec[i].get(&(link.conn, link.att)).unwrap_or(&empty);
            let best = (SEARCH_START..SEARCH_END)
                .map(|off| (off, range_of(ph(ai + 1), off), range_of(ph(0), off)))
                .filter(|(_, act, rest)| *act > 400 && *act > rest * 4)
                .max_by_key(|(_, act, _)| *act);
            match best {
                Some((off, act, _)) => println!("  {:>14}: strongest offset {off:>3}, range {act}", link.label()),
                None => println!("  {:>14}: nothing responded", link.label()),
            }
        }
    }
}

fn connect_both(dongle: &Dongle, opts: &Opts) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    let mut conns: Vec<u16> = Vec::new();
    let mut addrs: Vec<[u8; 6]> = Vec::new();
    // ⭐ `--detached` uses ONE half by design. Demanding two left the probe
    // waiting 40 s for a controller the user had deliberately set aside, with
    // an instruction on screen telling them to use only one.
    let want = if opts.detached { 1 } else { 2 };
    if want == 1 {
        println!("[imu] --detached: waiting for ONE half — wake it now (any button)");
    } else {
        println!("[imu] waiting for BOTH halves — wake them now (any button)");
    }

    let deadline = Instant::now() + Duration::from_secs(40);
    while conns.len() < want && Instant::now() < deadline {
        let Some((addr, addr_type, pid)) = scan_once(dongle, &addrs) else { continue };
        let side = if pid == 0x2066 { "RIGHT" } else { "LEFT" };
        // ⭐ Fixed 7.5 ms, not `le_connect`'s 5 ms-first attempt.
        //
        // `le_connect` asks for interval 4 — BELOW the BLE spec minimum — and
        // falls back only if refused. Pinning 6, as the reference ESP32 firmware
        // does ("values below 6 are rejected"), produced ~200 Hz on `0x000e`
        // against the ~67 Hz every previous capture was taken at.
        //
        // For a gyro hunt that is three times the samples per rotation, which
        // is exactly the resolution a rate signal needs.
        match dongle
            .le_connect_params(addr, addr_type, 6, 6)
            .map(|p| {
                println!(
                    "[imu] {side} link: interval {:.2} ms (~{:.0} Hz ceiling)",
                    p.interval_ms(),
                    1000.0 / p.interval_ms(),
                );
                p.conn_handle
            }) {
            // A handle already in use means the connect returned a STALE
            // Connection Complete belonging to the first controller rather than
            // a new link. Accepting it silently makes both halves read the same
            // stream — byte-identical frames, identical counters, identical
            // everything — which looks like working data and is not.
            Ok(conn) if conns.contains(&conn) => {
                eprintln!("[imu] {side} returned handle {conn:#06x}, already in use — retrying");
                dongle.cancel_pending_connect();
            }
            Ok(conn) => {
                println!("[imu] {side} connected as handle {conn:#06x} ({} of 2)", conns.len() + 1);
                let found = subscribe_all(dongle, conn, side, opts);
                init(dongle, conn, opts, &found);
                if opts.late_subscribe {
                    subscribe_inputs(dongle, conn, &found);
                }
                for att in found.streams {
                    links.push(Link { conn, side, att });
                }
                addrs.push(addr);
                conns.push(conn);
            }
            Err(e) => eprintln!("[imu] {side} connect failed: {e}"),
        }
    }
    links
}

/// Discover the attribute table and subscribe to EVERY notifying
/// characteristic, returning the value handles to record.
///
/// This exists because of one unexamined assumption. `HANDLE_INPUT_VALUE`
/// (`0x000e`) came from an HCI capture of the Windows stack, and every gyro
/// search so far has scanned that stream alone — including a rate-correlation
/// scan with a validated reference that still found nothing. But `protocol.rs`
/// documents a second input characteristic at `0x000a`, the common report 0x05,
/// with the note "we do not subscribe to it", and the published HID descriptor
/// puts report 0x05 at 63 bytes while the per-side reports 0x07/0x08 are only
/// 2 bytes. If the motion data lives there, no amount of scanning `0x000e`
/// could ever have found it.
///
/// Discovering rather than hard-coding `0x000a` is deliberate: hard-coding a
/// guessed handle is the same move that produced the assumption being tested.
/// The one IMU gate: `motion(0x04) | mouse(0x10) | magnetometer(0x80)`.
///
/// ❗ **There is no separate magnetometer switch, and there was never a reason
/// for one.** The magnetometer is a single bit of this mask, and 0x94 is the
/// set the reference uses. A `--mag` flag survived here long after the mask
/// became a constant, doing nothing but printing a note — and while it existed
/// the startup banner reported 0x37/0xB7, the mask from before this was known,
/// so every run log stated a feature mask the code did not send.
///
/// Widening it is actively harmful rather than merely wasteful: the reference
/// records that enabling everything makes the Joy-Con emit phantom ZL/ZR.
/// ⭐ **Now 0x2f, and the probe must never disagree with the shipped path.**
///
/// 0x94 was wrong, and so was the 0x37 it replaced. Neither carries bit 0x08
/// ([`flexinput_joycon2::protocol::feature::IMU_RAW`]), and without that bit the
/// controller leaves the standard accel/gyro block at 0x30..0x3C entirely zero.
/// Every capture this probe ever saved was taken with the real gyro switched
/// off, which is why none of them contained an angular rate to find.
///
/// ⚠️ The mask sweep in this file tests `[0x37, 0x3F, 0x77, 0xB7, 0xBF, 0xF7,
/// 0x7F, 0xFF]` — supersets of 0x37 — and reported no change. `0x3F` does carry
/// bit 0x08, so that looks like counter-evidence, but those runs are the ones
/// the legacy 0x37 fallback overwrote on the same channel before the report was
/// read. They are void, not negative.
/// Spelled as a literal because `btle` sits BELOW `joycon2` in the dependency
/// graph and cannot import its constant. Keep the two in step by hand:
/// `flexinput_joycon2::protocol::feature::JOYCON2_DEFAULT`.
const FEATURE_MASK: u8 = 0x2f;

/// Command-line switches that change what the probe does to the controller.
#[derive(Clone, Copy)]
struct Opts {
    /// Skip the per-side init on `0x0016` entirely.
    ///
    /// ⭐ That "safety net" is now the prime suspect for the common stream
    /// never starting. The rate descriptors tell the story: `0x000c` and
    /// `0x0028` accept a write BEFORE init and are refused
    /// `Write Not Permitted` AFTER it, while `0x0010` — the per-side one —
    /// stays writable throughout. Something in init makes the controller commit
    /// to the per-side path and write-protect the other two, and the per-side
    /// init is the only part of init known to be reaching the device at all.
    common_only: bool,
    /// Send the modern init down `0x0016` — the channel that DEMONSTRABLY works
    /// — instead of `0x0014`, which hardware has now shown to be inert.
    ///
    /// ⭐ The LED test settled it: four alternating player-LED patterns on
    /// `0x0014` moved nothing, while the same command on `0x0016` visibly drives
    /// the LEDs (the shipped dongle path does it, and the user has driven LED
    /// position from a Knob through it). So this controller answers commands on
    /// the per-side channel only.
    ///
    /// Which makes one thing untested rather than impossible: **Set Input Mode
    /// 0x30 has only ever been sent to the dead channel.** If Format 3 is
    /// reachable on this hardware at all, it is reachable here.
    cmd_per_side: bool,
    /// Sweep with ONE half out of the grip, so each phase isolates a real
    /// device axis — see [`PHASES_DETACHED`].
    detached: bool,
    /// Subscribe to the INPUT characteristics only AFTER the init sequence.
    ///
    /// ⭐ Taken from reading the reference implementation, which subscribes to
    /// the command-response channel, runs the whole init, sets input mode 0x30,
    /// and only THEN calls `enable_input_notify_callback()` for the common
    /// input. We have always done the opposite — every CCCD written up front,
    /// before a single command. A device that re-initialises its notification
    /// state during configuration would drop that early subscription, and the
    /// characteristic would read as "exists, subscribes fine, never notifies":
    /// exactly what `0x000a` has done on every run.
    late_subscribe: bool,
    /// Blink the player LEDs through the common command channel and stop.
    led_test: bool,
    /// Record the sweep even when the preflight says nothing new will come of it.
    force: bool,
}

/// Check what actually woke up, in seconds, before asking for any physical motion.
///
/// Returns false when the run has nothing left to learn. Two distinct failures
/// are worth telling apart here and the sweep hides both:
/// - no stream at all is alive, so the capture would be 63 zero bytes
/// - only `0x000e` is alive, which is the same data as every previous capture
fn preflight(dongle: &Dongle, links: &[Link], opts: &Opts) -> bool {
    println!("\n[imu] preflight — 2 s, no motion needed");
    let frames = record(dongle, links, Duration::from_secs(2));
    let mut alive: Vec<&Link> = Vec::new();
    let mut format3 = false;
    for l in links {
        let rows = frames.get(&(l.conn, l.att));
        let n = rows.map(|f| f.len()).unwrap_or(0);
        if n == 0 {
            println!("      {}: 0 frames", l.label());
            continue;
        }
        alive.push(l);
        let rows = rows.expect("non-empty");

        // ⭐ The Format-3 signature, checkable without any motion.
        //
        // In every capture of the current layout, bytes 45..62 are ALL ZERO —
        // and that tail is exactly where Format 3 puts accelerometer (48–53)
        // and gyroscope (54–59). So "the tail is dead" is a one-frame test for
        // which layout is streaming, and it needs no sweep, no reference and no
        // correlation to read.
        let width = rows[0].len();
        let live = |i: usize| rows.iter().any(|r| r.get(i) != rows[0].get(i));
        let live_count = (0..width).filter(|i| live(*i)).count();
        let tail_live = (45..width).filter(|i| live(*i)).count();
        let tail_nonzero = rows.iter().any(|r| r.iter().skip(45).any(|b| *b != 0));
        println!(
            "      {}: {n} frames, len {width}, {live_count} live bytes, tail(45+) {} live / {}",
            l.label(),
            tail_live,
            if tail_nonzero { "NON-ZERO" } else { "all zero" },
        );
        if tail_nonzero {
            format3 = true;
        }
    }
    if format3 {
        println!("\n⭐⭐ A report tail past byte 45 is NON-ZERO — the layout CHANGED.");
        println!("   That is the Format-3 signature. Recording.");
        return true;
    }
    if alive.is_empty() {
        eprintln!("\n⛔ NO stream is notifying. The controller is connected but sending nothing.");
        if opts.common_only {
            eprintln!("   --common-only skipped the per-side init, and the common init did");
            eprintln!("   not start anything either. That is the answer this run was for:");
            eprintln!("   commands to {:#06x} do not reach the controller.", jc::HANDLE_CMD_WRITE_COMMON);
        }
        return opts.force;
    }

    let common = alive.iter().any(|l| l.att == jc::HANDLE_INPUT_COMMON);
    let per_side = alive.iter().any(|l| l.att == jc::HANDLE_INPUT_VALUE);
    if common {
        println!("\n⭐ {:#06x} IS STREAMING — this is new. Recording.", jc::HANDLE_INPUT_COMMON);
        return true;
    }
    // ❗ This guard reasons about STREAM DISCOVERY — "we already have captures of
    // 0x000e, don't make the user sweep for nothing". That is wrong for
    // `--detached`, where the novelty is the MOTION, not the stream: the whole
    // point is to re-record the same characteristic with one device axis
    // actually isolated. Aborting there refused to run the experiment it was
    // asked for.
    if opts.detached {
        println!("\n[imu] --detached: recording {:#06x} again — the NEW thing here is the", jc::HANDLE_INPUT_VALUE);
        println!("[imu] motion (one isolated device axis per phase), not the stream.");
        return true;
    }
    if per_side && !opts.force {
        eprintln!(
            "\n⛔ Only {:#06x} is alive — the same stream every previous capture already holds.",
            jc::HANDLE_INPUT_VALUE
        );
        eprintln!("   A sweep would produce a byte-for-byte equivalent of `imu360.bin`.");
        eprintln!("   Re-analyse an existing capture with --load instead.");
        return false;
    }
    true
}

/// Send one command and return its reply payload from the response channel.
fn cmd_reply(dongle: &Dongle, conn: u16, cmd: u8, sub: u8, data: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut frame = vec![0u8; 17];
    frame.extend_from_slice(&cmd_frame(cmd, sub, data));
    dongle
        .write_attribute(conn, jc::HANDLE_CMD_WRITE, &frame, acl::ATT_WRITE_COMMAND)
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(900);
    while Instant::now() < deadline {
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        if pkt.cid != acl::CID_ATT || pkt.conn_handle != conn {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        if n.handle == jc::HANDLE_CMD_RESPONSE && n.value.len() >= 8 && n.value[0] == cmd {
            return Some((n.value[1], n.value[8..].to_vec()));
        }
    }
    None
}

/// Try to bring the link up ENCRYPTED, then see whether `0x000a` starts talking.
///
/// ⭐ **The one structural difference between us and the reference that has
/// never been tested.** The reference drives these controllers through WinRT,
/// which encrypts a bonded link automatically; our dongle stack implements
/// `le_enable_encryption` and has NEVER used it, because the per-side stream
/// works fine unencrypted. So every observation we have of `0x000a` was made on
/// a plaintext link.
///
/// If the combined report is the Nintendo-protocol channel and the plaintext
/// one is a vendor/app channel, this is exactly what we would see: subscribes
/// accepted, reads refused, notifications never sent — because the controller
/// will not serve console data to an unbonded host.
///
/// ❗ **Deliberately stops short of `0x15/0x03`.** That subcommand FINALISES the
/// bond and commits the host address and key to controller flash. Everything
/// here is staged and reversible by power-cycling; nothing is written
/// permanently. If encryption succeeds without it, the finalise was never
/// needed — and if it fails, we have learned that without having rewritten a
/// bond the controller may still need for its console.
fn try_encrypt(dongle: &Dongle, links: &[Link], opts: &Opts) {
    // The reference's fixed host key (`ltk1` in its `pair()`), minus the leading
    // framing byte. It is a CONSTANT there — the host dictates the key rather
    // than negotiating one.
    const HOST_KEY: [u8; 16] = [
        0xea, 0xbd, 0x47, 0x13, 0x89, 0x35, 0x42, 0xc6,
        0x79, 0xee, 0x07, 0xf2, 0x53, 0x2c, 0x6c, 0x31,
    ];

    let host = match dongle.read_bd_addr() {
        Ok(a) => {
            println!("[imu] dongle BD_ADDR {:02x?}", a);
            a
        }
        Err(e) => {
            eprintln!("[imu] cannot read dongle address: {e}");
            return;
        }
    };

    let mut seen: Vec<(u16, &str)> = Vec::new();
    for l in links {
        if !seen.iter().any(|(c, _)| *c == l.conn) {
            seen.push((l.conn, l.side));
        }
    }

    for (conn, side) in seen {
        println!("\n[imu] === ENCRYPTION ATTEMPT: {side} ===");
        println!("[imu] NOTE: staged only — 0x15/0x03 (finalise/flash commit) is NOT sent.");

        // 1. Tell the controller which host it is talking to. Little-endian,
        //    twice, exactly as the reference sends it.
        let mut mac = vec![0x00, 0x02];
        let mut le = host;
        le.reverse();
        mac.extend_from_slice(&le);
        mac.extend_from_slice(&le);
        match cmd_reply(dongle, conn, 0x15, 0x01, &mac) {
            Some((st, d)) => println!("   0x15/0x01 set-host  status {st:#04x} data {d:02x?}"),
            None => println!("   0x15/0x01 set-host  no reply"),
        }

        // 2. Key exchange. Our own protocol notes model this as "host sends A1,
        //    device replies B1, LTK = A1 xor B1"; the reference just sends a
        //    constant. Sending the constant and READING the reply tests both
        //    readings at once, and the reply is data we have never seen.
        let mut key_payload = vec![0x00];
        key_payload.extend_from_slice(&HOST_KEY);
        let device_key = match cmd_reply(dongle, conn, 0x15, 0x04, &key_payload) {
            Some((st, d)) => {
                println!("   0x15/0x04 key-xchg  status {st:#04x} data {d:02x?}");
                d
            }
            None => {
                println!("   0x15/0x04 key-xchg  NO REPLY — key exchange is not answered");
                Vec::new()
            }
        };

        // ❗ The reply is `[framing 0x01][16-byte device key]`. XOR-ing against
        // the whole reply shifts every byte of the result by one and produces a
        // key that is wrong in all 16 positions — which is what dropped the
        // link on the first attempt.
        //
        // The derived-key model is the right one: our own `pairing.rs` test
        // asserts `ltk[0] == 0x35 ^ 0x5c`, and `0x5c` is exactly the first byte
        // of this controller's reply.
        let mut candidates: Vec<(&str, [u8; 16])> = Vec::new();
        if device_key.len() >= 17 {
            let mut xor = HOST_KEY;
            for (i, b) in xor.iter_mut().enumerate() {
                *b ^= device_key[i + 1];
            }
            candidates.push(("host XOR device key (framing byte skipped)", xor));
            // Key endianness is a real ambiguity here: `register_link_key_data`
            // stores the LTK byte-reversed, and `le_enable_encryption` reverses
            // again on the way to the wire. Trying both costs one attempt.
            let mut rev = xor;
            rev.reverse();
            candidates.push(("the same, byte-reversed", rev));
        }
        candidates.push(("host key as-is (host-dictates model)", HOST_KEY));

        for (what, ltk) in candidates {
            println!("\n   → encrypting with {what}: {ltk:02x?}");
            if let Err(e) = dongle.le_enable_encryption(conn, 0, 0, &ltk) {
                println!("     LE_Enable_Encryption failed to send: {e}");
                continue;
            }
            // The outcome arrives as an Encryption Change event, not a command
            // completion.
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut verdict = None;
            while Instant::now() < deadline {
                match dongle.read_event_timeout(Duration::from_millis(200)) {
                    Ok(Some(Event::EncryptionChange { status, enabled, .. })) => {
                        verdict = Some((status, enabled));
                        break;
                    }
                    Ok(Some(Event::DisconnectionComplete { reason, .. })) => {
                        println!("     ⛔ LINK DROPPED, reason {reason:#04x} — wrong key.");
                        verdict = Some((0xFF, 0));
                        break;
                    }
                    _ => continue,
                }
            }
            match verdict {
                Some((0x00, 1)) => {
                    println!("     ⭐⭐ ENCRYPTION ENABLED. This is the first encrypted link we");
                    println!("        have ever had with these controllers.");
                    std::thread::sleep(Duration::from_millis(400));
                    let frames = record(dongle, links, Duration::from_secs(2));
                    for l in links.iter().filter(|l| l.conn == conn) {
                        let n = frames.get(&(l.conn, l.att)).map(|f| f.len()).unwrap_or(0);
                        println!("        {}: {n} frames", l.label());
                    }
                    let common = links.iter().any(|l| {
                        l.conn == conn
                            && l.att == jc::HANDLE_INPUT_COMMON
                            && frames.get(&(l.conn, l.att)).map(|f| !f.is_empty()).unwrap_or(false)
                    });
                    if common {
                        println!("        ⭐⭐⭐ {:#06x} IS STREAMING ON THE ENCRYPTED LINK.", jc::HANDLE_INPUT_COMMON);
                    } else {
                        println!("        {:#06x} still silent even encrypted.", jc::HANDLE_INPUT_COMMON);
                    }
                    return;
                }
                Some((0xFF, _)) => {
                    // ❗ Once the link is gone every later candidate is
                    // meaningless — the writes go nowhere and the results read
                    // as "no response", which looks like evidence and is not.
                    println!("     stopping: the link is gone, so no further key can be tested.");
                    println!("     Wake the controller to reconnect and re-run to try the next one.");
                    break;
                }
                Some((st, en)) => println!("     refused: status {st:#04x} enabled {en}"),
                None => println!("     no Encryption Change event within 3 s"),
            }
        }
    }
    let _ = opts;
}

/// Read one region of controller flash and return `(status, data)`.
///
/// Command `0x02/0x04`, payload `[size][0x7E][0][0][addr u32 LE]`, sent on the
/// per-side channel with its 17-byte prefix — the only combination this pad
/// executes. The reply arrives on the COMMON response characteristic `0x001a`.
///
/// ⭐ These reads have been in the init since the dongle link first worked, and
/// their replies were invisible for the whole investigation because nothing was
/// listening on a channel that answered. They are the one source of controller
/// state we have never actually read.
fn read_flash(dongle: &Dongle, conn: u16, addr: u32, size: u8) -> Option<(u8, Vec<u8>)> {
    let mut payload = vec![size, 0x7E, 0x00, 0x00];
    payload.extend_from_slice(&addr.to_le_bytes());
    let mut frame = vec![0u8; 17];
    frame.extend_from_slice(&cmd_frame(0x02, 0x04, &payload));
    dongle
        .write_attribute(conn, jc::HANDLE_CMD_WRITE, &frame, acl::ATT_WRITE_COMMAND)
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(900);
    while Instant::now() < deadline {
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        if pkt.cid != acl::CID_ATT || pkt.conn_handle != conn {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        // Input reports pour in at 67 Hz on 0x000e; only the response channel
        // matters here, and only a reply to the command we just sent.
        if n.handle != jc::HANDLE_CMD_RESPONSE || n.value.len() < 8 || n.value[0] != 0x02 {
            continue;
        }
        return Some((n.value[1], n.value[8..].to_vec()));
    }
    None
}

/// Hex + ASCII, because a calibration blob is often half strings.
fn hexdump(addr: u32, data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' })
            .collect();
        println!(
            "      {:08x}  {:<47}  |{ascii}|",
            addr as usize + i * 16,
            hex.join(" "),
        );
    }
}

/// Flash regions the official init reads, with the sizes it asks for.
///
/// Addresses and sizes are verbatim from the captured Windows-stack init; the
/// labels are inference from context and are marked as such.
const FLASH_REGIONS: &[(u32, u8, &str)] = &[
    (0x013000, 0x40, "read first by official init"),
    (0x013040, 0x10, ""),
    (0x013060, 0x20, ""),
    (0x013080, 0x40, "stick factory calibration, per earlier notes"),
    (0x013100, 0x18, ""),
    (0x1FC040, 0x40, "separate region, likely device identity"),
];

/// Dump controller flash and print it.
///
/// Worth doing now for one reason: **factory IMU calibration and sensor
/// configuration live in this flash**, and the report layout we cannot decode
/// may simply be described there. Every other approach has been to guess a
/// layout and score it; this asks the device.
fn dump_flash(dongle: &Dongle, links: &[Link], scan: bool) {
    let mut seen: Vec<(u16, &str)> = Vec::new();
    for l in links {
        if !seen.iter().any(|(c, _)| *c == l.conn) {
            seen.push((l.conn, l.side));
        }
    }

    for (conn, side) in seen {
        println!("\n[imu] === FLASH DUMP: {side} ===");
        for (addr, size, note) in FLASH_REGIONS {
            match read_flash(dongle, conn, *addr, *size) {
                Some((0x01, data)) if !data.is_empty() => {
                    println!(
                        "\n   {addr:#08x} ({size} bytes) OK{}",
                        if note.is_empty() { String::new() } else { format!("  — {note}") },
                    );
                    hexdump(*addr, &data);
                }
                Some((0x01, _)) => println!("\n   {addr:#08x} OK but returned no data"),
                Some((st, _)) => println!("\n   {addr:#08x} REJECTED, status {st:#04x}"),
                None => println!("\n   {addr:#08x} no reply within 900 ms"),
            }
            std::thread::sleep(Duration::from_millis(60));
        }

        if !scan {
            continue;
        }
        // A wider walk, for finding regions the official init never touches.
        // 0x40 per read is the largest size the init uses, so it is known to be
        // accepted; anything larger is a guess.
        println!("\n[imu] --- {side}: WIDER SCAN (silent regions are skipped) ---");
        for base in [0x013000u32, 0x01_3200, 0x1FC000] {
            for off in (0..0x200u32).step_by(0x40) {
                let addr = base + off;
                let Some((0x01, data)) = read_flash(dongle, conn, addr, 0x40) else { continue };
                // A region of pure 0x00 or 0xFF is erased flash, not content.
                if data.iter().all(|b| *b == 0x00) || data.iter().all(|b| *b == 0xFF) {
                    continue;
                }
                println!("\n   {addr:#08x}");
                hexdump(addr, &data);
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    }
    println!("\n[imu] flash dump complete.");
}

/// Read every readable characteristic directly, and re-read the input ones to
/// see whether they are LIVE.
///
/// ⭐ **`0x000a` is `READ|NOTIFY`. The whole investigation has tried to make it
/// NOTIFY and never once tried to READ it.**
///
/// The distinction matters enormously here. A characteristic can hold a
/// perfectly good, continuously-updated value and simply never push it — if the
/// clone implements the report buffer but not the notification path, that is
/// exactly what we would see, and it is indistinguishable from "empty" until
/// somebody reads it.
///
/// If `0x000a` reads back a populated report that CHANGES between reads, then:
/// * the combined report exists on this pad after all
/// * the reference's layout applies (accel at 48, gyro at 54)
/// * and polling by ATT Read is a usable delivery path even if notifications
///   never work
///
/// Reads only — nothing here can change controller state.
fn read_all(dongle: &Dongle, links: &[Link]) {
    let mut seen: Vec<(u16, &str)> = Vec::new();
    for l in links {
        if !seen.iter().any(|(c, _)| *c == l.conn) {
            seen.push((l.conn, l.side));
        }
    }

    for (conn, side) in seen {
        println!("
[imu] === READ EVERY READABLE CHARACTERISTIC: {side} ===");
        // Value handles worth reading: the inputs, the vendor service nobody
        // has ever touched, and the GAP strings for orientation.
        const HANDLES: &[(u16, &str)] = &[
            (0x0002, "vendor 00c5af5d…281 (service never touched)"),
            (0x0006, "vendor 00c5af5d…283"),
            (0x000a, "⭐ COMMON INPUT — the report the reference reads"),
            (0x000e, "per-side input (the stream we decode today)"),
            (0x0026, "ab7de9be…7fde (input-shaped, always silent)"),
            (0x002d, "GAP device name"),
            (0x002f, "GAP appearance"),
        ];
        for (h, what) in HANDLES {
            match dongle.read_attribute_detail(conn, *h).map(|r| r.map_err(|c| (c, acl::att_error_name(c)))) {
                Err(e) => println!("
   {h:#06x}  {what}
   transport error: {e}"),
                Ok(Err((c, name))) => println!("
   {h:#06x}  {what}
   REFUSED: ATT {c:#04x} {name}"),
                Ok(Ok(Some(v))) if !v.is_empty() => {
                    println!("
   {h:#06x}  {what}
   {} bytes", v.len());
                    hexdump(0, &v);
                    let ascii: String = v.iter()
                        .map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' })
                        .collect();
                    if ascii.chars().filter(|c| *c != '.').count() > 3 {
                        println!("   as text: {ascii}");
                    }
                }
                Ok(Ok(Some(_))) => println!("
   {h:#06x}  {what}
   read OK but EMPTY"),
                Ok(Ok(None)) => println!("
   {h:#06x}  {what}
   no response at all"),
            }
        }

        // ⭐ The decisive part: is the common input LIVE?
        //
        // A static buffer read twice is identical. A live report has a counter
        // or timestamp that moves. This separates "the clone allocates the
        // characteristic but never fills it" from "it fills it and only fails
        // to push it" — and only the second is useful.
        println!("
   --- is {:#06x} LIVE? four reads, 300 ms apart ---", jc::HANDLE_INPUT_COMMON);
        let mut prev: Option<Vec<u8>> = None;
        let mut changed_bytes = std::collections::BTreeSet::new();
        for i in 0..4 {
            std::thread::sleep(Duration::from_millis(300));
            match dongle.read_attribute_detail(conn, jc::HANDLE_INPUT_COMMON) {
                Ok(Err(c)) => {
                    println!("   read {i}: REFUSED ATT {c:#04x} {}", acl::att_error_name(c));
                    continue;
                }
                Ok(Ok(Some(v))) if !v.is_empty() => {
                    if let Some(p) = &prev {
                        for (b, (a, c)) in p.iter().zip(v.iter()).enumerate() {
                            if a != c {
                                changed_bytes.insert(b);
                            }
                        }
                    }
                    println!("   read {i}: {:02x?}", &v[..v.len().min(32)]);
                    prev = Some(v);
                }
                other => println!("   read {i}: {other:?}"),
            }
        }
        if changed_bytes.is_empty() {
            println!("   ⛔ identical every time — a dead buffer, not a live report.");
        } else {
            println!("   ⭐⭐ {} BYTE(S) CHANGED between reads: {:?}", changed_bytes.len(), changed_bytes);
            println!("   The common report IS live on this pad. It just never notifies.");
            println!("   Move the controller and re-run to confirm the motion bytes track.");
        }
    }
}

/// Sweep the command space on the channel that WORKS, watching `motion_len`.
///
/// ⭐ **Every previous search for the real gyro path went through `0x0014`**,
/// the handle hardware later proved inert: the feature-mask sweep, the input
/// mode `0x30` write, every attempt to wake `0x000a`. Those nulls say nothing —
/// the commands were never delivered. This is the first search of that space
/// with commands the controller actually executes and a response channel that
/// answers.
///
/// ⭐ **`motion_len` is the target, not the liveness map.** It is the
/// controller's own statement of how many motion bytes it is sending: 30 today,
/// with report bytes 45-62 permanently zero. A Switch 2 gets real angular rate
/// from this hardware, so some command must widen that block — and a command
/// that changes `motion_len` has found the path even if we cannot yet decode
/// what arrives.
///
/// ❗ Deliberately conservative about what it sends:
/// * only subcommands of opcodes the known-good init already uses, so nothing
///   here is a wholly unknown operation
/// * empty payloads — a bare subcommand is far less likely to WRITE anything
///   than one carrying data
/// * `0x02` (controller memory) excluded entirely. It is the one opcode known
///   to touch flash, and factory stick/IMU calibration lives there. A malformed
///   write would be unrecoverable.
fn cmd_sweep(dongle: &Dongle, links: &[Link]) {
    // Opcodes the official init exercises, so each is a known-real operation.
    const OPCODES: &[u8] = &[0x01, 0x03, 0x07, 0x09, 0x0A, 0x0C, 0x10, 0x11, 0x15, 0x16];

    let mut conns: Vec<u16> = links.iter().map(|l| l.conn).collect();
    conns.sort_unstable();
    conns.dedup();

    println!("
[imu] === COMMAND SWEEP on {:#06x} — no motion needed ===", jc::HANDLE_CMD_WRITE);
    println!("[imu] Watching motion_len (30 = today's block) and any byte past 45.");
    println!("[imu] A row where motion_len CHANGES is the answer.
");

    let baseline = shape_of(dongle, links);
    println!("  cmd/sub  status   {}", baseline.header());
    println!("  baseline    -     {}", baseline.line());

    for &cmd in OPCODES {
        for sub in 1u8..=0x10 {
            for &conn in &conns {
                let mut frame = vec![0u8; 17];
                frame.extend_from_slice(&cmd_frame(cmd, sub, &[]));
                let _ = dongle.write_attribute(
                    conn, jc::HANDLE_CMD_WRITE, &frame, acl::ATT_WRITE_COMMAND,
                );
            }
            let status = collect_status(dongle, &conns, cmd);
            let shape = shape_of(dongle, links);
            // Only print rows that DIFFER from the baseline, plus accepted
            // commands. A full 160-row dump of identical lines hides the one
            // row that matters.
            let changed = shape.motion_len != baseline.motion_len || shape.tail_live > 0;
            if changed || status == Some(0x01) {
                println!(
                    "  {cmd:#04x}/{sub:#04x}  {}  {}{}",
                    match status {
                        Some(0x01) => "OK   ".to_string(),
                        Some(st) => format!("{st:#04x} "),
                        None => "-    ".to_string(),
                    },
                    shape.line(),
                    if changed { "   <== CHANGED" } else { "" },
                );
            }
        }
    }
    println!("
[imu] sweep done. Reconnect the controllers to clear any state this left.");
}

/// One measurement of what the report currently looks like.
struct ReportShape {
    per_link: Vec<(String, usize, u8, usize, usize)>,
    motion_len: u8,
    tail_live: usize,
}

impl ReportShape {
    fn header(&self) -> String {
        "stream          len  mlen  live  tail".into()
    }
    fn line(&self) -> String {
        self.per_link
            .iter()
            .map(|(l, len, ml, live, tail)| format!("{l:<14} {len:>3}  {ml:>4}  {live:>4}  {tail:>4}"))
            .collect::<Vec<_>>()
            .join("  |  ")
    }
}

fn shape_of(dongle: &Dongle, links: &[Link]) -> ReportShape {
    let frames = record(dongle, links, Duration::from_millis(700));
    let mut per_link = Vec::new();
    let (mut motion_len, mut tail_live) = (0u8, 0usize);
    for l in links {
        let Some(rows) = frames.get(&(l.conn, l.att)) else { continue };
        if rows.is_empty() || l.att != jc::HANDLE_INPUT_VALUE {
            continue;
        }
        let width = rows[0].len();
        // The motion-length byte sits one earlier on the left half, the same
        // one-byte shift the whole report carries.
        let off = if l.side == "LEFT" { 14 } else { 15 };
        let ml = rows[0].get(off).copied().unwrap_or(0);
        let live = |i: usize| rows.iter().any(|r| r.get(i) != rows[0].get(i));
        let live_n = (0..width).filter(|i| live(*i)).count();
        let tail = (45..width).filter(|i| live(*i)).count();
        motion_len = motion_len.max(ml);
        tail_live += tail;
        per_link.push((l.label(), width, ml, live_n, tail));
    }
    ReportShape { per_link, motion_len, tail_live }
}

/// Wait briefly for a command reply and return its status byte.
fn collect_status(dongle: &Dongle, conns: &[u16], cmd: u8) -> Option<u8> {
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        if pkt.cid != acl::CID_ATT || !conns.contains(&pkt.conn_handle) {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        if n.handle == jc::HANDLE_CMD_RESPONSE && n.value.len() >= 8 && n.value[0] == cmd {
            return Some(n.value[1]);
        }
    }
    None
}

/// Try different values in the "report rate" descriptor and see what changes.
///
/// ⭐ **`0x85 0x00` is the only value ever written, and "report rate" is a
/// GUESS.** The research doc labels it "Set Report Rate?" with the question
/// mark; nothing has ever confirmed what it means.
///
/// What IS confirmed is that this descriptor is the single most powerful write
/// on the device: without it the stream is STUB — counter incrementing, every
/// field zero — and with it real data appears. A write that turns the payload
/// on is exactly the kind of write that might also select WHICH payload, and
/// the byte is 8 bits of unexplored configuration sitting in plain sight.
///
/// The feature mask turned out to change nothing about the layout. This is the
/// remaining candidate for the same job.
fn rate_sweep(dongle: &Dongle, links: &[Link]) {
    // Around the known-good 0x85, plus the obvious structural guesses: low
    // values, single bits, and the byte with each high bit cleared.
    const VALUES: &[[u8; 2]] = &[
        [0x85, 0x00], // the known-good control
        [0x00, 0x00], [0x01, 0x00], [0x05, 0x00], [0x0F, 0x00],
        [0x30, 0x00], [0x40, 0x00], [0x80, 0x00], [0x81, 0x00],
        [0x83, 0x00], [0x87, 0x00], [0x8F, 0x00], [0xC5, 0x00],
        [0xFF, 0x00], [0x85, 0x01], [0x85, 0xFF],
    ];

    let mut conns: Vec<u16> = links.iter().map(|l| l.conn).collect();
    conns.sort_unstable();
    conns.dedup();

    println!("\n[imu] === REPORT-RATE DESCRIPTOR SWEEP — no motion needed ===");
    println!("[imu] 0x85 0x00 is the control. Watch for a changed length, a changed");
    println!("[imu] live-byte count, or anything past byte 45.\n");
    println!("  value      stream          frames  len  live  tail  liveness map");

    for v in VALUES {
        for &conn in &conns {
            // Written to the PER-SIDE input's descriptor: that is the stream
            // that exists, and the common one is refused after init anyway.
            let _ = dongle.write_attribute(
                conn,
                jc::HANDLE_INPUT_REPORT_RATE,
                v,
                acl::ATT_WRITE_REQUEST,
            );
        }
        std::thread::sleep(Duration::from_millis(250));
        let frames = record(dongle, links, Duration::from_millis(1500));
        for l in links {
            let Some(rows) = frames.get(&(l.conn, l.att)) else { continue };
            if rows.is_empty() {
                // "stopped" would be a lie and a misleading one: these streams
                // never started in the first place, so their silence here says
                // nothing about the value being tested.
                println!("  {:02x} {:02x}      {:<14}       -  (never started)", v[0], v[1], l.label());
                continue;
            }
            let width = rows[0].len();
            let live = |i: usize| rows.iter().any(|r| r.get(i) != rows[0].get(i));
            let live_count = (0..width).filter(|i| live(*i)).count();
            let tail = (45..width).filter(|i| live(*i)).count();
            let map: String = (0..width).map(|i| if live(i) { '#' } else { '.' }).collect();
            println!(
                "  {:02x} {:02x}      {:<14}  {:>6}  {width:>3}  {live_count:>4}  {tail:>4}  {map}",
                v[0], v[1], l.label(), rows.len(),
            );
        }
    }
    // Leave the controller in the state that works, whatever the sweep did.
    for &conn in &conns {
        let _ = dongle.write_attribute(
            conn,
            jc::HANDLE_INPUT_REPORT_RATE,
            &jc::REPORT_RATE_PAYLOAD,
            acl::ATT_WRITE_REQUEST,
        );
    }
    println!("\n[imu] restored {:02x?}. A row differing from the 0x85 0x00 control is the lead.",
        jc::REPORT_RATE_PAYLOAD);
}

/// Try a range of feature masks and report what each does to the report.
///
/// ⭐ Worth doing only now, and for a specific reason: the mask demonstrably
/// REACHES the controller. Under `0x37` the report carries 22 live bytes; under
/// `0x94` it carries 16. A setting that changes the payload is a setting worth
/// scanning, and until the per-side command channel started acknowledging
/// commands there was no way to know it was landing at all.
///
/// `0x94` is a *subset* of what we want here — it drops bits 0, 1 and 5 that
/// `0x37` sets, which is why it removed live bytes rather than adding them. The
/// interesting candidates are supersets of the known-good `0x37`.
///
/// No physical motion is needed: the question is which bytes exist, not what
/// they contain.
fn mask_sweep(dongle: &Dongle, links: &[Link]) {
    // Every mask that adds a bit to the known-good 0x37, plus 0x37 itself as
    // the control and 0xFF as the upper bound. The reference warns 0xFF makes
    // the pad emit phantom ZL/ZR, which is a reason to measure it, not skip it.
    const MASKS: &[u8] = &[0x37, 0x3F, 0x77, 0xB7, 0xBF, 0xF7, 0x7F, 0xFF];

    let mut conns: Vec<u16> = links.iter().map(|l| l.conn).collect();
    conns.sort_unstable();
    conns.dedup();

    println!("\n[imu] === FEATURE MASK SWEEP — no motion needed ===");
    println!("[imu] Watching for live bytes appearing past 45, which is where");
    println!("[imu] Format 3 puts accel (48-53) and gyro (54-59).\n");
    println!("  mask   stream          frames  live  tail(45+)  liveness map");

    for &mask in MASKS {
        for &conn in &conns {
            for (cmd, sub) in [(0x0Cu8, 0x02u8), (0x0C, 0x04)] {
                let mut frame = vec![0u8; 17];
                frame.extend_from_slice(&cmd_frame(cmd, sub, &[mask, 0, 0, 0]));
                let _ = dongle.write_attribute(
                    conn,
                    jc::HANDLE_CMD_WRITE,
                    &frame,
                    acl::ATT_WRITE_COMMAND,
                );
                std::thread::sleep(Duration::from_millis(40));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
        let frames = record(dongle, links, Duration::from_millis(1500));
        for l in links {
            let Some(rows) = frames.get(&(l.conn, l.att)) else { continue };
            if rows.is_empty() {
                continue;
            }
            let width = rows[0].len();
            let live = |i: usize| rows.iter().any(|r| r.get(i) != rows[0].get(i));
            let live_count = (0..width).filter(|i| live(*i)).count();
            let tail = (45..width).filter(|i| live(*i)).count();
            let map: String = (0..width).map(|i| if live(i) { '#' } else { '.' }).collect();
            println!(
                "  {mask:#04x}   {:<14}  {:>6}  {live_count:>4}  {tail:>9}  {map}",
                l.label(),
                rows.len(),
            );
        }
    }
    println!("\n[imu] A mask that lights up bytes past 45 is the answer.");
    println!("[imu] If none do, Format 3 is not reachable on this controller.");
}

/// What discovery learned about one link.
struct Discovered {
    /// Value handles to record.
    streams: Vec<u16>,
    /// Characteristic declarations, carrying the properties that decide which
    /// write opcode each handle accepts.
    chars: Vec<acl::CharDecl>,
    attrs: Vec<acl::AttrInfo>,
}

impl Discovered {
    /// The write opcode for a handle, from its declared properties.
    ///
    /// Falls back to Write Command when properties could not be recovered,
    /// because that is what hardware proved the command channel wants: an
    /// acknowledged write to `0x0014` is answered `Write Not Permitted`.
    fn write_opcode(&self, value_handle: u16) -> u8 {
        self.chars
            .iter()
            .find(|c| c.value_handle == value_handle)
            .and_then(|c| c.write_opcode())
            .unwrap_or(acl::ATT_WRITE_COMMAND)
    }
}

/// Write the input CCCDs and their report-rate descriptors.
///
/// Split out of [`subscribe_all`] so `--late-subscribe` can run it AFTER the
/// init instead of before — see [`Opts::late_subscribe`].
fn subscribe_inputs(dongle: &Dongle, conn: u16, found: &Discovered) {
    println!("    -> --late-subscribe: subscribing to inputs NOW, after init");
    let cccds: Vec<u16> = found
        .attrs
        .iter()
        .filter(|a| a.uuid == acl::AttUuid::Short(acl::GATT_CCCD))
        .map(|a| a.handle)
        .collect();
    for h in cccds {
        // Skip the command-response CCCD: it was subscribed before the init on
        // purpose, because the init's own replies are the point of it.
        if h == jc::HANDLE_CMD_RESPONSE_CCCD {
            continue;
        }
        match dongle.write_attribute(conn, h, &acl::CCCD_NOTIFY, acl::ATT_WRITE_REQUEST) {
            Ok(()) => println!("       CCCD {h:#06x} <- 01 00  ok"),
            Err(e) => println!("       CCCD {h:#06x} <- 01 00  {e}"),
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    write_report_rates(dongle, conn, &found.attrs, &found.chars);
    std::thread::sleep(Duration::from_millis(150));
}

fn subscribe_all(dongle: &Dongle, conn: u16, side: &str, opts: &Opts) -> Discovered {
    // MTU first. The default 23 bytes would fragment both the discovery
    // responses and any 63-byte report, and a fragmented Find Information
    // Response would silently truncate the attribute table.
    let _ = dongle.send_att(conn, &acl::exchange_mtu_request(jc::DESIRED_MTU));
    std::thread::sleep(Duration::from_millis(100));

    // Both walks are attempted and both outcomes are reported. They use
    // DIFFERENT ATT opcodes (Read By Type vs Find Information), so one can work
    // where the other is refused, and the reasons they fail are not the same
    // reason. Collapsing that into one "discovery returned nothing" line is how
    // a diagnosable refusal becomes a dead end.
    let mut chars = match dongle.discover_characteristics(conn) {
        Ok(c) if !c.is_empty() => {
            println!("[imu] {side}: Read By Type walk found {} characteristic(s)", c.len());
            c
        }
        Ok(_) => {
            eprintln!("[imu] {side}: Read By Type walk ended immediately (Attribute Not Found at handle 1)");
            Vec::new()
        }
        Err(e) => {
            eprintln!("[imu] {side}: Read By Type walk failed — {e}");
            Vec::new()
        }
    };
    let attrs = match dongle.discover_attributes(conn) {
        Ok(a) if !a.is_empty() => {
            println!("[imu] {side}: Find Information walk found {} attribute(s)", a.len());
            // The raw table, printed whenever it is available. This is the
            // ground truth the whole `0x000a` question turns on, and it costs
            // one screen of output.
            for a in &a {
                println!("      {:#06x}  {}", a.handle, a.uuid);
            }
            a
        }
        Ok(_) => {
            eprintln!("[imu] {side}: Find Information walk ended immediately");
            Vec::new()
        }
        Err(e) => {
            eprintln!("[imu] {side}: Find Information walk failed — {e}");
            Vec::new()
        }
    };

    // Read By Type is refused by this controller, every run, on both halves —
    // but plain Read is not. Reading each 0x2803 declaration one at a time
    // recovers exactly what the failed walk would have returned: properties,
    // value handle and UUID.
    //
    // ⭐ Properties are the whole point. Without them the write opcode for the
    // command channel is a guess, and that guess has now been wrong in BOTH
    // directions: Write Command (0x52) was ignored, then Write Request (0x12)
    // earned `Write Not Permitted` nine times in a row. The device knows which
    // it accepts; asking it is cheaper than another physical test run.
    if chars.is_empty() && !attrs.is_empty() {
        chars = dongle.read_characteristics(conn, &attrs);
        if chars.is_empty() {
            eprintln!("[imu] {side}: reading declarations recovered nothing either");
        } else {
            println!(
                "[imu] {side}: read {} characteristic declaration(s) directly",
                chars.len()
            );
        }
    }
    if !chars.is_empty() {
        println!("[imu] {side}: characteristic properties");
        for c in &chars {
            let write = match c.write_opcode() {
                Some(acl::ATT_WRITE_COMMAND) => "  write as 0x52",
                Some(acl::ATT_WRITE_REQUEST) => "  write as 0x12",
                _ => "",
            };
            println!(
                "      value {:#06x}  {:<28}{write}",
                c.value_handle,
                c.properties_text(),
            );
        }
    }

    if !attrs.is_empty() {
        // EVERY handle typed 0x2902 is a CCCD, and a characteristic's value
        // handle is the one immediately before its CCCD. A CCCD only exists on
        // a characteristic that can notify or indicate, so its presence is the
        // permission — this works whether or not properties were recovered.
        //
        // The first version of this fell back to the captured handles and threw
        // the discovered table away — which discovered five notifying
        // characteristics and then subscribed to two of them.
        let cccds: Vec<u16> = attrs
            .iter()
            .filter(|a| a.uuid == acl::AttUuid::Short(acl::GATT_CCCD))
            .map(|a| a.handle)
            .collect();
        if opts.late_subscribe {
            println!("[imu] {side}: --late-subscribe — command response ONLY for now");
        } else {
            println!("[imu] {side}: subscribing to all {} discovered CCCD(s)", cccds.len());
        }
        let mut streams = Vec::new();
        for h in &cccds {
            // With `--late-subscribe`, only the command-response channel is
            // enabled here; the inputs are subscribed after the init instead,
            // which is the order the reference implementation uses.
            let now = !opts.late_subscribe || *h == jc::HANDLE_CMD_RESPONSE_CCCD;
            let value = h - 1;
            let uuid = attrs
                .iter()
                .find(|a| a.handle == value)
                .map(|a| a.uuid.to_string())
                .unwrap_or_else(|| "?".into());
            if now {
                // ❗ ACKNOWLEDGED, and the result REPORTED. This was
                // fire-and-forget: `send_att` with the response discarded, so a
                // REFUSED subscribe was indistinguishable from a successful one
                // that simply never produced data.
                //
                // That is precisely the shape of the `0x000a` mystery. If the
                // combined report is gated behind an encrypted link — a
                // Nintendo-protocol channel the controller only opens to a
                // properly bonded host — the CCCD write returns
                // `Insufficient Authentication/Encryption`, and we have been
                // throwing that answer away on every single run.
                match dongle.write_attribute(conn, *h, &acl::CCCD_NOTIFY, acl::ATT_WRITE_REQUEST) {
                    Ok(()) => println!("      CCCD {h:#06x} -> value {value:#06x}  {uuid}"),
                    Err(e) => {
                        println!("      CCCD {h:#06x} -> value {value:#06x}  {uuid}");
                        println!("         ⛔ SUBSCRIBE REFUSED: {e}");
                    }
                }
            }
            streams.push(value);
            if now {
                std::thread::sleep(Duration::from_millis(60));
            }
        }
        // Recording the command-response channel as a data stream would fill
        // the capture with init replies; it is subscribed for
        // `drain_responses` but is not an IMU candidate.
        streams.retain(|h| *h != jc::HANDLE_CMD_RESPONSE);
        if !opts.late_subscribe {
            write_report_rates(dongle, conn, &attrs, &chars);
        }
        println!();
        return Discovered { streams, chars, attrs };
    }

    eprintln!("[imu] {side}: no attribute table at all — falling back to the captured handles");
    for h in [jc::HANDLE_INPUT_CCCD, jc::HANDLE_CMD_RESPONSE_CCCD, jc::HANDLE_INPUT_COMMON_CCCD] {
        if let Err(e) = dongle.write_attribute(conn, h, &acl::CCCD_NOTIFY, acl::ATT_WRITE_REQUEST) {
            println!("      ⛔ CCCD {h:#06x} SUBSCRIBE REFUSED: {e}");
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    Discovered {
        streams: vec![jc::HANDLE_INPUT_VALUE, jc::HANDLE_INPUT_COMMON],
        chars,
        attrs,
    }
}

/// Vendor "report rate" descriptor UUID, one per input characteristic.
const UUID_REPORT_RATE: &str = "679d5510-5a24-4dee-9557-95df80486ecb";

/// Write the report-rate payload to EVERY input characteristic that has one.
///
/// ⭐ This is the difference between the stream that works and the streams that
/// do not, and it went unnoticed for the whole search.
///
/// The per-side input `0x000e` has a rate descriptor at `0x0010`, and the init
/// has always written `REPORT_RATE_PAYLOAD` to it — the comment on that
/// constant records that without it the controller emits stub reports, counter
/// incrementing and every field zero. The common input `0x000a` has the *same*
/// descriptor at `0x000c`, and nothing has ever written to it. So `0x000a` was
/// subscribed, enabled, and then measured as "exists but never notifies" —
/// which is precisely the stub behaviour the per-side channel shows when its
/// own rate descriptor is missed, except a stream that never starts produces no
/// frames at all rather than empty ones.
///
/// `0x0026` has one too, at `0x0028`, and is equally silent.
fn write_report_rates(
    dongle: &Dongle,
    conn: u16,
    attrs: &[acl::AttrInfo],
    chars: &[acl::CharDecl],
) {
    let rates: Vec<u16> = attrs
        .iter()
        .filter(|a| a.uuid.to_string() == UUID_REPORT_RATE)
        .map(|a| a.handle)
        .collect();
    if rates.is_empty() {
        return;
    }
    println!("      report-rate descriptors: {} found", rates.len());
    for h in rates {
        // A descriptor has no characteristic declaration of its own, so the
        // opcode is taken from the characteristic it belongs to: the nearest
        // one at or below it. Descriptors on this controller are writable
        // acknowledged, but deriving it beats assuming it again.
        let opcode = chars
            .iter()
            .filter(|c| c.value_handle < h)
            .max_by_key(|c| c.value_handle)
            .and_then(|c| c.write_opcode())
            .unwrap_or(acl::ATT_WRITE_REQUEST);
        match dongle.write_attribute(conn, h, &jc::REPORT_RATE_PAYLOAD, opcode) {
            Ok(()) => println!("      rate {h:#06x} <- {:02x?}  ok", jc::REPORT_RATE_PAYLOAD),
            Err(e) => println!("      rate {h:#06x} <- {:02x?}  {e}", jc::REPORT_RATE_PAYLOAD),
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn scan_once(dongle: &Dongle, known: &[[u8; 6]]) -> Option<([u8; 6], u8, u16)> {
    // Report the failure rather than swallowing it. Silently returning None
    // here turned a refused scan-enable into an endless "waiting for BOTH
    // halves" with nothing to explain it.
    if let Err(e) = dongle.start_le_scan() {
        eprintln!("[imu] scan enable failed: {e}");
        std::thread::sleep(Duration::from_millis(500));
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = None;
    while Instant::now() < deadline {
        if let Ok(Some(Event::LeAdvertisingReport(r))) =
            dongle.read_event_timeout(Duration::from_millis(100))
        {
            let Some(md) = r.manufacturer_data() else { continue };
            if md.len() < 9 || u16::from_le_bytes([md[0], md[1]]) != NINTENDO_COMPANY_ID {
                continue;
            }
            if known.contains(&r.address) {
                continue;
            }
            found = Some((r.address, r.address_type, u16::from_le_bytes([md[7], md[8]])));
            break;
        }
    }
    let _ = dongle.stop_le_scan();
    found
}

fn record(dongle: &Dongle, links: &[Link], dur: Duration) -> HashMap<Key, Frames> {
    let mut out: HashMap<Key, Frames> = links.iter().map(|l| ((l.conn, l.att), Vec::new())).collect();
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if let Ok(Some(Event::DisconnectionComplete { reason, .. })) =
            dongle.read_event_timeout(Duration::from_millis(1))
        {
            eprintln!("[imu] a link dropped mid-phase (reason {reason:#04x})");
        }
        // Greedy drain: one packet per iteration starved whichever half was
        // serviced second, badly enough to make its statistics meaningless.
        for pkt in dongle.drain_acl(256) {
            if pkt.cid != acl::CID_ATT {
                continue;
            }
            let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
            // Demultiplex by BOTH handles: connection handle separates the two
            // halves sharing one pipe, attribute handle separates the several
            // notification streams now subscribed on each of them.
            if let Some(v) = out.get_mut(&(pkt.conn_handle, n.handle)) {
                v.push(n.value);
            }
        }
    }
    out
}

/// Build a vendor command.
///
/// ❗ **No leading rumble region.** An earlier version prefixed 17 zero bytes
/// (report id + rumble block), copied from the per-side channel that shares its
/// characteristic with rumble. The command channel is command-only and expects
/// the header at offset 0, so every command we ever sent was 17 bytes of
/// padding followed by a header the controller never looked at.
fn cmd_frame(cmd: u8, sub: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![cmd, 0x91, 0x01, sub, 0x00, data.len() as u8, 0x00, 0x00];
    out.extend_from_slice(data);
    out
}

/// Run the vendor init sequence. MTU and subscriptions are already done by
/// [`subscribe_all`], which has to run first so the command channel exists.
///
/// The order and payloads are taken from TommyWabg/Switch2Connect, which drives
/// these controllers successfully. Two things in here were previously wrong in
/// ways that produced a silently degraded stream rather than an error.
fn init(dongle: &Dongle, conn: u16, opts: &Opts, found: &Discovered) {
    // ❗ The write opcode is READ FROM THE DEVICE, not chosen.
    //
    // Both guesses have now been tried and both were wrong. Write Command
    // (0x52) went unacknowledged and appeared to do nothing; Write Request
    // (0x12) was answered `ATT 0x03 Write Not Permitted` for every command in
    // this sequence, on both halves. The command characteristic at 0x0014
    // declares WRITE_NO_RESPONSE and nothing else, so 0x52 is the only opcode
    // it will accept — and an attribute that takes 0x52 gives no
    // acknowledgement by definition, so "no reply" here is not evidence of
    // anything.
    //
    // The reference reaches the same place from the other direction: bleak's
    // `write_gatt_char` with no explicit `response` picks write-without-
    // response whenever the characteristic offers it. It never chose the
    // acknowledged form for these commands either.
    // Which channel, and therefore which framing. The per-side channel shares
    // its characteristic with rumble, so a command there carries the 17-byte
    // prefix (report id + rumble region); the common channel is command-only
    // and expects the header at offset 0.
    let cmd_handle = if opts.cmd_per_side {
        jc::HANDLE_CMD_WRITE
    } else {
        jc::HANDLE_CMD_WRITE_COMMON
    };
    let opcode = found.write_opcode(cmd_handle);
    println!(
        "    -> command channel {cmd_handle:#06x} accepts {}{}",
        if opcode == acl::ATT_WRITE_COMMAND { "0x52 (unacknowledged)" } else { "0x12 (acknowledged)" },
        if opts.cmd_per_side { "  [--cmd-per-side: the channel known to work]" } else { "" },
    );

    let send = |c: u8, s: u8, d: &[u8]| {
        let mut frame = Vec::new();
        if opts.cmd_per_side {
            frame.resize(17, 0);
        }
        frame.extend_from_slice(&cmd_frame(c, s, d));
        if let Err(e) = dongle.write_attribute(conn, cmd_handle, &frame, opcode) {
            eprintln!("       {c:#04x}/{s:#04x} refused: {e}");
        }
        // Pacing, not acknowledgement. An unacknowledged write has no reply to
        // wait for, and firing thirteen of them into one connection interval
        // would overrun the controller's buffer.
        std::thread::sleep(Duration::from_millis(40));
    };

    let features = FEATURE_MASK;
    println!("    -> feature mask {features:#04x}");

    // Verbatim from Switch2Connect's `sw2_init_commands`, in order.
    for (cmd, sub, data) in [
        (0x03u8, 0x0du8, &[0x01, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..]),
        (0x07, 0x01, &[][..]),
        (0x16, 0x01, &[][..]),
        (0x15, 0x03, &[0x00][..]),
        (0x0c, 0x02, &[features, 0, 0, 0][..]),
        (0x11, 0x03, &[][..]),
        (0x0a, 0x08, &[
            0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0x35, 0x00, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ][..]),
        (0x0c, 0x04, &[features, 0, 0, 0][..]),
        (0x03, 0x0a, &[0x09, 0x00, 0x00, 0x00][..]),
        (0x10, 0x01, &[][..]),
        (0x01, 0x0c, &[][..]),
        (0x01, 0x01, &[0x00, 0x00, 0x00, 0x00][..]),
        (0x09, 0x07, &[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00][..]),
    ] {
        send(cmd, sub, data);
    }

    // ⭐ GROUND TRUTH: does this channel do ANYTHING?
    //
    // Every other signal we have is inferential — a stream that does not start,
    // a response that never arrives, both of which have a dozen possible
    // causes. The player LEDs are a physical output with no parsing between us
    // and the answer: if they change, commands on 0x0014 are being executed,
    // and the problem is elsewhere. If they never change across four alternating
    // patterns, this channel is inert and no amount of framing or feature-mask
    // work on it will matter.
    //
    // The per-side channel is known to drive these LEDs (recorded when the
    // dongle link first worked), so the command itself is not in question —
    // only the channel it is sent on.
    if opts.led_test {
        println!("\n    === LED TEST on {cmd_handle:#06x} — WATCH THE CONTROLLER ===");
        println!("    Four alternating patterns, 1.2 s apart.");
        for (i, pattern) in [0x01u8, 0x08, 0x01, 0x08].iter().enumerate() {
            println!("    {}. player LED pattern {pattern:#04x}", i + 1);
            send(0x09, 0x07, &[*pattern, 0, 0, 0, 0, 0, 0, 0]);
            std::thread::sleep(Duration::from_millis(1200));
        }
        println!("    === Did the LEDs change? That is the whole result. ===\n");
    }

    // ⭐ SET INPUT MODE 0x30 ("Format 3"). This is the step that changes the
    // report layout entirely, and without it the controller streams the sparse,
    // strided format every decode attempt was fighting. Sent as a RAW write, not
    // through the command framing above.
    let mut mode_frame = Vec::new();
    if opts.cmd_per_side {
        mode_frame.resize(17, 0);
    }
    mode_frame.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x30]);
    let mode = dongle.write_attribute(conn, cmd_handle, &mode_frame, opcode);
    println!(
        "    -> input mode 0x30 (Format 3): {}",
        match &mode {
            Ok(()) if opcode == acl::ATT_WRITE_COMMAND => "sent (unacknowledged)".to_string(),
            Ok(()) => "acknowledged".to_string(),
            Err(e) => e.to_string(),
        }
    );
    std::thread::sleep(Duration::from_millis(150));

    send(0x0C, 0x01, &[]);
    drain_responses(dongle, conn, Duration::from_millis(600));

    // Safety net: if the common channel did not take, restore the per-side
    // stream so a failed experiment still leaves a usable capture instead of
    // 63 zero bytes.
    //
    // ❗ **It is also the prime suspect for why the common channel never
    // takes.** The rate descriptors record a state change across init: `0x000c`
    // and `0x0028` accept a write BEFORE it and are refused `Write Not
    // Permitted` AFTER, while the per-side `0x0010` stays writable throughout.
    // Something in init makes the controller commit to the per-side path and
    // write-protect the other two — and this block is the only part of init
    // known for certain to be reaching the device.
    //
    // So the safety net may be sabotaging the experiment it protects.
    // `--common-only` runs without it; that is the test.
    // ⛔⛔ **THIS BLOCK STOPPED BEING A FALLBACK AND BECAME INTERFERENCE.**
    //
    // It was written when `HANDLE_CMD_WRITE` pointed at `0x0014`: the modern
    // init went to a handle that turned out to be inert, so re-running a known
    // -good sequence on the per-side channel afterwards was a genuine safety
    // net. `HANDLE_CMD_WRITE` is now `0x0016` — the SAME channel the modern
    // init uses — so this ran as a second, contradictory init that always went
    // last and therefore always won:
    //
    // * it overwrote the `0x94` feature mask with `0x37`
    // * it re-sent the handshake AFTER the input-mode `0x30` write
    // * it rewrote the rate descriptors a third time
    //
    // Every experiment that depended on the modern init's state — the mask
    // baseline, `--cmd-per-side`, `--late-subscribe`, the Format-3 attempt —
    // was measured with the mask back at `0x37`. Those results are void.
    //
    // It is now confined to the case it was actually written for: commands
    // going to the dead common channel, where nothing else has reached the
    // controller at all.
    if opts.common_only || opts.cmd_per_side {
        if opts.common_only {
            println!("    -> --common-only: per-side init SKIPPED");
        } else {
            println!("    -> init went to {:#06x}; legacy 0x37 fallback SKIPPED so it", jc::HANDLE_CMD_WRITE);
            println!("       cannot overwrite the mask this run is testing");
        }
        // ❗ With `--late-subscribe` the rate descriptors are written after the
        // CCCDs instead, by `subscribe_inputs`. Writing them here would put a
        // descriptor write BEFORE the subscribe — and the reference writes no
        // rate descriptor at all (`679d5510` appears nowhere in it), so keeping
        // one here would be our own step contaminating a test of THEIR order.
        if !opts.late_subscribe {
            write_report_rates(dongle, conn, &found.attrs, &found.chars);
        }
        std::thread::sleep(Duration::from_millis(100));
        return;
    }
    let legacy = |c: u8, s: u8, d: &[u8]| {
        let mut framed = vec![0u8; 17];
        framed.extend_from_slice(&cmd_frame(c, s, d));
        let _ = dongle.send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE, &framed));
        std::thread::sleep(Duration::from_millis(30));
    };
    legacy(0x07, 0x01, &[]);
    legacy(0x10, 0x01, &[]);
    legacy(0x16, 0x01, &[]);
    legacy(0x0C, 0x02, &[0x37, 0, 0, 0]);
    legacy(0x0C, 0x04, &[0x37, 0, 0, 0]);
    // Rate descriptors again, now that the features are set. They are written
    // once before init as well: the per-side channel has always had this
    // written LAST and streams correctly, so neither ordering is assumed to be
    // the required one until hardware says which.
    write_report_rates(dongle, conn, &found.attrs, &found.chars);
    std::thread::sleep(Duration::from_millis(100));
}

/// Print whatever the controller sends back on the command-response channel.
///
/// Responses on this characteristic carry their 8-byte header at offset 0x0F
/// (the leading bytes are the rumble region echoed back), so the header is
/// decoded from there: `[cmd][dir 0x01][transport][subcmd][..][len]`.
/// Decode the `0x11/0x03` reply: the controller's own IMU scale factors.
///
/// ⭐ **The device states its sensor scales, and one of them is the gyro scale
/// this whole search has been trying to measure.** Six little-endian f32 at
/// offset 5, verified against the reply captured from both halves:
///
/// | offset | value | meaning |
/// |---|---|---|
/// | 5 | 0.002393 | accel, m/s² per LSB |
/// | 9 | 0.0012218 | gyro, rad/s per LSB |
/// | 13 | 78.45 | accel full scale, m/s² (= 8 g) |
/// | 17 | 34.91 | gyro full scale, rad/s (= 2000 °/s) |
/// | 21 | 19.61 | second accel range (= 2 g) |
/// | 25 | 8.73 | second gyro range (= 500 °/s) |
///
/// The accel figure is the strong check: 0.002393 m/s² per LSB is
/// **4096.3 LSB/g**, independently reproducing the 4096 measured from gravity
/// magnitude. A scale block that gets the known sensor right is worth believing
/// about the unknown one.
///
/// The gyro figure is **0.07 °/s per LSB = 14.286 LSB per °/s** — the *Pro
/// controller* scale from `gyro.py`, not the 16.384 quoted for a Joy-Con.
fn sensor_scales(data: &[u8]) -> Option<SensorScales> {
    let f32_at = |i: usize| -> Option<f32> {
        data.get(i..i + 4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (accel, gyro) = (f32_at(5)?, f32_at(9)?);
    if !(accel.is_finite() && gyro.is_finite() && accel > 0.0 && gyro > 0.0) {
        return None;
    }
    Some(SensorScales {
        accel_per_lsb: accel,
        gyro_rad_per_lsb: gyro,
        accel_range: f32_at(13),
        gyro_range: f32_at(17),
    })
}

/// Standard gravity, for turning m/s² per LSB into LSB per g.
const G: f32 = 9.80665;

struct SensorScales {
    accel_per_lsb: f32,
    gyro_rad_per_lsb: f32,
    accel_range: Option<f32>,
    gyro_range: Option<f32>,
}

impl SensorScales {
    fn lsb_per_g(&self) -> f32 {
        G / self.accel_per_lsb
    }
    fn lsb_per_dps(&self) -> f32 {
        1.0 / self.gyro_rad_per_lsb.to_degrees()
    }
}

fn print_sensor_scales(data: &[u8]) {
    let Some(s) = sensor_scales(data) else { return };
    let (accel, gyro) = (s.accel_per_lsb, s.gyro_rad_per_lsb);
    let (lsb_per_g, lsb_per_dps) = (s.lsb_per_g(), s.lsb_per_dps());
    println!("       ⭐ IMU SCALES, stated by the controller:");
    println!(
        "          accel {accel:.6} m/s2 per LSB  ->  {lsb_per_g:.1} LSB/g{}",
        if (lsb_per_g - 4096.0).abs() < 64.0 { "   ✅ matches the 4096 we measured" } else { "" },
    );
    println!(
        "          gyro  {gyro:.7} rad/s per LSB ->  {lsb_per_dps:.3} LSB per deg/s  ({:.4} deg/s per LSB)",
        gyro.to_degrees(),
    );
    if let (Some(ar), Some(gr)) = (s.accel_range, s.gyro_range) {
        println!(
            "          ranges: accel +/-{:.2} g, gyro +/-{:.0} deg/s",
            ar / G,
            gr.to_degrees(),
        );
    }
}

fn drain_responses(dongle: &Dongle, conn: u16, dur: Duration) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        if pkt.cid != acl::CID_ATT || pkt.conn_handle != conn {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        // Listen on BOTH response channels. Which one answers is itself the
        // answer to which command channel the controller accepted — and no
        // reply has ever been seen on either, so narrowing to one would just
        // hide half the evidence.
        let which = match n.handle {
            h if h == jc::HANDLE_CMD_RESPONSE => "common 0x001a",
            0x001E => "per-side 0x001e",
            _ => continue,
        };
        // ❗ Header at offset 0, not 0x0F. `[cmd][status][..][data from 8]`,
        // and status 0x01 means accepted. The old 0x0F came from the same
        // mistaken rumble-prefix model as the command framing.
        if n.value.len() >= 8 {
            let h = &n.value[..8];
            let data = &n.value[8..(8 + h[5] as usize).min(n.value.len())];
            println!(
                "    <- reply on {which}: cmd={:#04x} status={:#04x}{} len={} data={:02x?}",
                h[0], h[1], if h[1] == 0x01 { " OK" } else { " ***REJECTED***" }, h[5], data
            );
            if h[0] == 0x11 && h[1] == 0x01 {
                print_sensor_scales(data);
            }
        } else {
            println!("    <- reply on {which} (short) {:02x?}", n.value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `0x11/0x03` reply captured from BOTH halves, byte for byte.
    ///
    /// Identical on left and right, which is itself worth pinning: a scale
    /// block that differed per half would mean it was something else.
    const SCALE_REPLY: [u8; 29] = [
        0x01, 0xc0, 0x03, 0x00, 0x00,
        0xe7, 0xd0, 0x1c, 0x3b, // accel m/s^2 per LSB
        0x79, 0x22, 0xa0, 0x3a, // gyro rad/s per LSB
        0x0a, 0xe8, 0x9c, 0x42, // accel full scale
        0x58, 0xa0, 0x0b, 0x42, // gyro full scale
        0x0a, 0xe8, 0x9c, 0x41, // second accel range
        0x58, 0xa0, 0x0b, 0x41, // second gyro range
    ];

    #[test]
    fn the_controller_states_the_accelerometer_scale_we_measured() {
        let s = sensor_scales(&SCALE_REPLY).expect("decodes");
        // 4096 LSB/g was measured independently from gravity magnitude long
        // before this reply could be read. The device agreeing to within a
        // fraction of a percent is what makes the block trustworthy about the
        // gyro, which no measurement has ever pinned down.
        assert!(
            (s.lsb_per_g() - 4096.0).abs() < 8.0,
            "accel scale {} LSB/g, expected ~4096",
            s.lsb_per_g()
        );
        assert!((s.accel_range.unwrap() / G - 8.0).abs() < 0.05);
    }

    #[test]
    fn the_gyro_scale_is_the_pro_controller_figure_not_the_joycon_one() {
        let s = sensor_scales(&SCALE_REPLY).expect("decodes");
        // 14.2857 LSB per deg/s (0.07 deg/s per LSB) is what `gyro.py` quotes
        // for a Pro controller; a Joy-Con is documented as 16.384. This pad
        // reports the Pro figure, so decoding it with the Joy-Con constant
        // would read every rate ~15% low.
        assert!(
            (s.lsb_per_dps() - 14.2857).abs() < 0.05,
            "gyro scale {} LSB per deg/s",
            s.lsb_per_dps()
        );
        assert!((s.gyro_range.unwrap().to_degrees() - 2000.0).abs() < 10.0);
    }

    #[test]
    fn a_truncated_or_absurd_scale_block_is_rejected_rather_than_decoded() {
        assert!(sensor_scales(&SCALE_REPLY[..8]).is_none());
        // All-zero payload: a zero scale would divide to infinity and print a
        // confident-looking nonsense figure.
        assert!(sensor_scales(&[0u8; 29]).is_none());
    }
}
