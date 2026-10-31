//! Offline CPU benchmark for the two hot paths a patch drives continuously:
//! the per-UI-frame `build_processing_graph` and the engine's per-tick
//! `eval_graph_tick` (the sample rate, default 2 kHz). Loads every tab of a
//! workspace file (point it at a COPY — this only reads), synthesizes moving
//! device signals for each source device, and reports time and heap
//! allocations per call. Ignored by default:
//!
//! ```text
//! set FXI_BENCH_WORKSPACE=C:\path\to\workspace_copy.json
//! cargo test --release -p flexinput-ui perf_bench -- --ignored --nocapture
//! ```

use std::alloc::{GlobalAlloc, Layout};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use flexinput_core::{automap, Signal, SignalType};
use flexinput_engine::eval::TickOutput;
use flexinput_engine::{eval_graph_tick, NodeState, ProcessingGraph};
use glam::Vec2;

// Counts every allocation in the test binary, and the bytes live on the heap.
// A few relaxed atomic adds per allocation — negligible next to the allocation.
struct CountingAlloc;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        sampler::on_alloc(ALLOCS.fetch_add(1, Ordering::Relaxed));
        LIVE_BYTES.fetch_add(layout.size() as i64, Ordering::Relaxed);
        mimalloc::MiMalloc.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        mimalloc::MiMalloc.dealloc(ptr, layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        sampler::on_alloc(ALLOCS.fetch_add(1, Ordering::Relaxed));
        LIVE_BYTES.fetch_add(layout.size() as i64, Ordering::Relaxed);
        mimalloc::MiMalloc.alloc_zeroed(layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        sampler::on_alloc(ALLOCS.fetch_add(1, Ordering::Relaxed));
        LIVE_BYTES.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
        mimalloc::MiMalloc.realloc(ptr, layout, new_size)
    }
}

/// Heap bytes held by what `f` builds and returns (measured on this thread's
/// view of the global counter; the bench is single-threaded at that point).
fn heap_bytes_of<T>(f: impl FnOnce() -> T) -> (T, i64) {
    let before = LIVE_BYTES.load(Ordering::Relaxed);
    let v = f();
    (v, LIVE_BYTES.load(Ordering::Relaxed) - before)
}

/// What the extra ticks buy: the same 500 Hz input through two engines, one at
/// 2 kHz (four ticks per input frame) and one at 500 Hz (one tick per frame),
/// comparing what reaches the sinks after every frame. Displacement pins
/// (banked by the real thread) are compared as a per-frame sum. Per tab.
#[test]
#[ignore]
fn perf_rate_equiv() {
    use flexinput_core::Signal;
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses");
    // One input frame: 2 ms by default, FXI_FRAME_MS to match a polling rate.
    // A runs FXI_TICKS_A ticks per frame (default 4, so 2 kHz), B FXI_TICKS_B.
    let frame_s: f32 = std::env::var("FXI_FRAME_MS").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(2.0) / 1000.0;
    let ticks_a: u32 = std::env::var("FXI_TICKS_A").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    let frames: u32 = (8.0 / frame_s) as u32; // 8 s of input
    let ticks_b: u32 = std::env::var("FXI_TICKS_B").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let warm: u32 = frames / 8;
    let frame_sigs = pad_signals_at;
    eprintln!("{:<26} | {:>6} {:>9} {:>11} | {:>9} {:>7} | {:>9} | worst float pin",
        "tab", "pins", "rms Δ", "max Δ", "bool miss", "longest", "disp sum");
    for (ti, mut tab) in ws.tabs.into_iter().enumerate() {
        crate::canvas::viewer::refresh_motion_units(&mut tab.snarl);
        let (graph, _) = crate::app::graph::build_processing_graph(&tab.snarl, Default::default());
        let devs = source_devices(&graph);
        let (mut st_a, mut st_b) = (HashMap::new(), HashMap::new());
        let (mut out_a, mut out_b) = (TickOutput::default(), TickOutput::default());
        let (mut sq, mut n_float, mut max_d, mut worst) = (0f64, 0u64, 0f32, String::new());
        let (mut miss, mut n_bool) = (0u64, 0u64);
        let mut run: HashMap<(String, String), u32> = HashMap::new();
        let (mut longest, mut worst_b) = (0u32, String::new());
        let (mut disp_a, mut disp_b) = (0f64, 0f64);
        for f in 0..frames {
            let sigs = frame_sigs(&devs, f as f32 * frame_s);
            // A: its ticks, displacement summed across them.
            let mut sum_a: HashMap<(String, String), Vec2> = HashMap::new();
            for _ in 0..ticks_a {
                eval_graph_tick(&graph, &mut st_a, &sigs, frame_s / ticks_a as f32, &mut out_a);
                for (k, v) in &flexinput_core::SigMap::from(&out_a.sink_outputs) {
                    if flexinput_engine::is_displacement_pin(&k.1) {
                        let d = match v { Signal::Vec2(p) => *p, o => Vec2::new(o.as_float(), 0.0) };
                        *sum_a.entry(k.clone()).or_default() += d;
                    }
                }
            }
            let mut sum_b: HashMap<(String, String), Vec2> = HashMap::new();
            for _ in 0..ticks_b {
                eval_graph_tick(&graph, &mut st_b, &sigs, frame_s / ticks_b as f32, &mut out_b);
                for (k, v) in &flexinput_core::SigMap::from(&out_b.sink_outputs) {
                    if flexinput_engine::is_displacement_pin(&k.1) {
                        let d = match v { Signal::Vec2(p) => *p, o => Vec2::new(o.as_float(), 0.0) };
                        *sum_b.entry(k.clone()).or_default() += d;
                    }
                }
            }
            if f < warm { continue; }
            let sinks_b = flexinput_core::SigMap::from(&out_b.sink_outputs);
            for (k, a) in &flexinput_core::SigMap::from(&out_a.sink_outputs) {
                if flexinput_engine::is_displacement_pin(&k.1) {
                    disp_a += sum_a.get(k).map_or(0.0, |v| v.length() as f64);
                    disp_b += sum_b.get(k).map_or(0.0, |v| v.length() as f64);
                    continue;
                }
                let Some(b) = sinks_b.get(k) else { continue };
                match (a, b) {
                    (Signal::Bool(x), Signal::Bool(y)) => {
                        n_bool += 1;
                        let r = run.entry(k.clone()).or_default();
                        if x != y { miss += 1; *r += 1; if *r > longest { longest = *r; worst_b = k.1.clone(); } } else { *r = 0; }
                    }
                    _ => {
                        let (va, vb) = match (a, b) {
                            (Signal::Vec2(p), Signal::Vec2(q)) => (*p, *q),
                            _ => (Vec2::new(a.as_float(), 0.0), Vec2::new(b.as_float(), 0.0)),
                        };
                        let d = (va - vb).length();
                        sq += (d * d) as f64;
                        n_float += 1;
                        if d > max_d { max_d = d; worst = k.1.clone(); }
                    }
                }
            }
        }
        let rms = (sq / n_float.max(1) as f64).sqrt();
        let title: String = tab.title.chars().take(21).collect();
        eprintln!("{:<26} | {:>6} {:>9.5} {:>11.5} | {:>8.3}% {:>7} | {:>14} | {worst}  {worst_b}",
            format!("{ti} {title}"), out_a.sink_outputs.len(), rms, max_d,
            miss as f64 * 100.0 / n_bool.max(1) as f64, longest,
            if disp_a > 0.0 { format!("{:.3}x/{:.0}", disp_b / disp_a, disp_a) } else { "-".into() });
    }
}

const PIN_SKIP: &[&str] = &["mouse", "scroll", "key_", "trackpad", "rumble", "hd_", "lightbar", "media_", "trigger_", "touch"];

/// A pad being played: sticks sweep, triggers ride, gyro wobbles, and buttons
/// press and release several times a second. Only what a real pad publishes, so
/// no mouse / key / feedback pins.
fn busy_signals_at(devs: &HashMap<String, Vec<(String, SignalType)>>, t: f32) -> HashMap<(String, String), Signal> {
    let mut m = HashMap::new();
    for (dev, pins) in devs {
        for (i, (pin, ty)) in pins.iter().enumerate() {
            let p = pin.as_str();
            if PIN_SKIP.iter().any(|x| p.starts_with(x)) { continue; }
            let ph = t * 7.0 * (1.0 + (i % 7) as f32 * 0.37);
            let sig = match ty {
                SignalType::Vec2 => Signal::Vec2(Vec2::new(ph.sin() * 0.8, ph.cos() * 0.8)),
                SignalType::Bool => Signal::Bool(((t * 9.0 + i as f32 * 0.13) as i32) % 3 == 0),
                SignalType::Int => Signal::Int(0),
                // Gyro is normalized to +-2000 deg/s: wobble around 100 deg/s.
                _ if p.starts_with("gyro") => Signal::Float(ph.sin() * 0.05),
                _ if p == "accel_z" => Signal::Float(1.0),
                _ if p.starts_with("accel") => Signal::Float(ph.sin() * 0.02),
                _ => Signal::Float(ph.sin() * 0.5),
            };
            m.insert((dev.clone(), pin.clone()), sig);
        }
    }
    m
}

/// [`busy_signals_at`] with each stick's `_x` / `_y` pins agreeing with its Vec2
/// pin, as a real pad's do. RWS Aim reads one when its source is blocked and the
/// other when not, so disagreeing ones make its block flip every tick.
fn pad_signals_at(devs: &HashMap<String, Vec<(String, SignalType)>>, t: f32) -> HashMap<(String, String), Signal> {
    let mut m = busy_signals_at(devs, t);
    for dev in devs.keys() {
        for stick in ["left_stick", "right_stick"] {
            let Some(&Signal::Vec2(v)) = m.get(&(dev.clone(), stick.to_string())) else { continue };
            for (axis, val) in [("x", v.x), ("y", v.y)] {
                let key = (dev.clone(), format!("{stick}_{axis}"));
                if m.contains_key(&key) {
                    m.insert(key, Signal::Float(val));
                }
            }
        }
    }
    m
}

/// Everything the engine produces, tick by tick, on every tab of a workspace, as
/// one hash per frame in the file `FXI_GOLDEN_OUT`. For proving a change to the
/// evaluator changes nothing: run it before and after, and compare the files.
/// `FXI_GOLDEN_DUMP=tab:frame` prints that frame in full, to find what differs.
#[test]
#[ignore]
fn perf_golden() {
    use std::fmt::Write as _;
    use std::hash::{Hash, Hasher};
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses");
    let out_path = std::env::var("FXI_GOLDEN_OUT").expect("FXI_GOLDEN_OUT");
    let dump: Option<(usize, u32)> = std::env::var("FXI_GOLDEN_DUMP").ok().and_then(|s| {
        let (a, b) = s.split_once(':')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    });
    const FRAMES: u32 = 1500; // 3 s at 500 Hz
    const TICKS: u32 = 4;
    let mut file = String::new();
    for (ti, mut tab) in ws.tabs.into_iter().enumerate() {
        crate::canvas::viewer::refresh_motion_units(&mut tab.snarl);
        let (graph, _) = crate::app::graph::build_processing_graph(&tab.snarl, Default::default());
        let devs = source_devices(&graph);
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        for f in 0..FRAMES {
            let sigs = busy_signals_at(&devs, f as f32 * 0.002);
            let mut lines: Vec<String> = Vec::new();
            for t in 0..TICKS {
                out.same_input = t != 0;
                eval_graph_tick(&graph, &mut state, &sigs, 0.0005, &mut out);
                for (k, v) in &out.sink_outputs { lines.push(format!("{t} sink {k:?} {v:?}")); }
                for (k, v) in &out.outputs { lines.push(format!("{t} out {k:?} {v:?}")); }
                for (k, v) in &out.last_inputs { lines.push(format!("{t} in {k:?} {v:?}")); }
                for (k, v) in &out.last_outputs { lines.push(format!("{t} lo {k:?} {v:?}")); }
                for (k, v) in &out.scope_samples { lines.push(format!("{t} sc {k:?} {v:?}")); }
                for v in &out.gyro_cal_requests { lines.push(format!("{t} cal {v:?}")); }
            }
            lines.sort_unstable();
            if dump == Some((ti, f)) {
                for l in &lines { eprintln!("{l}"); }
            }
            let mut h = std::collections::hash_map::DefaultHasher::new();
            lines.hash(&mut h);
            let _ = writeln!(file, "{ti} {f} {} {:016x}", lines.len(), h.finish());
        }
        eprintln!("tab {ti} done");
    }
    std::fs::write(out_path, file).expect("write golden");
}

/// One UI frame of Easy mode's center panel on a workspace tab, headless: egui's
/// layout and tessellation, no GPU. `FXI_BENCH_TAB` (default 1). With
/// `FXI_BENCH_SAMPLE` set, profiles it as `perf_sample` does.
#[test]
#[ignore]
fn perf_ui_frame() {
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let tab_ix: usize = std::env::var("FXI_BENCH_TAB").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let presets_dir = std::env::var("FXI_BENCH_PRESETS").ok().map(std::path::PathBuf::from);
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses");
    let tab = ws.tabs.into_iter().nth(tab_ix).expect("tab exists");
    let mut canvas = crate::canvas::Canvas::new();
    canvas.snarl = tab.snarl;
    crate::canvas::viewer::refresh_motion_units(&mut canvas.snarl);
    let (graph, _) = crate::app::graph::build_processing_graph(&canvas.snarl, Default::default());
    let devs = source_devices(&graph);
    let descriptors: Vec<_> = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor).collect();
    let mut easy_state = crate::app::EasyState::default();
    let mut overlay = crate::canvas::OverlayLayout::default();
    let mut config = crate::canvas::OverlayLayout::default();
    let mut nav = crate::gamepad_nav::GamepadNav::default();
    let mut favorites = Vec::new();
    let panic = crate::app::PanicShortcut::default();
    let rates = HashMap::new();
    let live_ids = std::collections::HashSet::new();
    let defaults = crate::canvas::DeviceParamDefaults::default();
    let ctx = egui::Context::default();
    let mut frame = |i: u32, canvas: &mut crate::canvas::Canvas| {
        let live = signals_at(&devs, i as f32 / 60.0);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(i as f64 / 60.0),
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                crate::easy::center_panel::show(
                    ui, canvas, &mut easy_state, &mut overlay, &mut config,
                    presets_dir.as_deref(), defaults, &descriptors, &[], &live_ids, &live,
                    &panic, &rates, &mut favorites, &mut nav,
                );
            });
        });
        std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
    };
    for i in 0..30 { frame(i, &mut canvas); }
    if std::env::var("FXI_BENCH_SAMPLE").is_ok() {
        let sampler = sampler::Sampler::start();
        let t0 = Instant::now();
        let mut i = 30;
        while t0.elapsed().as_secs_f32() < 6.0 { frame(i, &mut canvas); i += 1; }
        sampler.stop_and_report(40);
        return;
    }
    let mut best = f64::MAX;
    let a0 = ALLOCS.load(Ordering::Relaxed);
    for b in 0..6 {
        let t0 = Instant::now();
        for i in 0..20 { frame(30 + b * 20 + i, &mut canvas); }
        best = best.min(t0.elapsed().as_secs_f64() * 1e3 / 20.0);
    }
    eprintln!("{:.2} ms per frame, {} allocations", best, (ALLOCS.load(Ordering::Relaxed) - a0) / 120);
}

/// What a freshly published graph costs the engine: ticks against one graph, then
/// against a new snapshot every `n` ticks (the UI at 60 Hz gives 2 at 125 Hz, 8 at
/// 500 Hz), with the old one dropped on the engine's side as it is in the app.
/// Building the snapshot isn't timed: that's the UI's. `FXI_BENCH_TAB` (default 1).
#[test]
#[ignore]
fn perf_republish() {
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let tab_ix: usize = std::env::var("FXI_BENCH_TAB").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses");
    let mut tab = ws.tabs.into_iter().nth(tab_ix).expect("tab exists");
    crate::canvas::viewer::refresh_motion_units(&mut tab.snarl);
    let build = || crate::app::graph::build_processing_graph(&tab.snarl, Default::default()).0;
    let devs = source_devices(&build());
    let sigs = pad_signals_at(&devs, 1.0);
    for every in [u32::MAX, 8, 2, 1] {
        let mut best = f64::MAX;
        for _ in 0..8 {
            let mut state: HashMap<usize, NodeState> = HashMap::new();
            let mut out = TickOutput::default();
            let mut graph = build();
            for _ in 0..200 { eval_graph_tick(&graph, &mut state, &sigs, 0.002, &mut out); }
            let mut ns = 0u128;
            const TICKS: u32 = 400;
            for k in 0..TICKS {
                let fresh = (every != u32::MAX && k % every == 0).then(&build);
                let t = Instant::now();
                if let Some(g) = fresh { graph = g; }
                eval_graph_tick(&graph, &mut state, &sigs, 0.002, &mut out);
                ns += t.elapsed().as_nanos();
            }
            best = best.min(ns as f64 / 1e3 / TICKS as f64);
        }
        let label = if every == u32::MAX { "one graph".to_string() } else { format!("new graph every {every} ticks") };
        eprintln!("{label:<30} {best:>8.1} us per tick");
    }
}

/// CPU seconds this process has used (user + kernel).
fn process_cpu() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let z = || FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z(), z(), z(), z());
    // SAFETY: FFI with valid out-pointers.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    let secs = |f: FILETIME| (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) as f64 * 1e-7;
    secs(k) + secs(u)
}

/// End-to-end check of the engine ↔ I/O hand-off on a real tab: a 500 Hz
/// emulated I/O loop toggles `btn_south` every iteration, publishes, kicks the
/// engine, waits like the I/O thread does, and drains the sink bus. Reports how
/// often the virtual pad's `btn_south` matches the button polled in the SAME
/// iteration (fresh) vs the previous one (one polling period late).
/// `FXI_BENCH_WORKSPACE`, `FXI_BENCH_TAB` (default 1).
#[test]
#[ignore]
fn perf_handoff() {
    use flexinput_engine::{EngineSync, HrWaiter};
    use std::sync::{Arc, Mutex, RwLock};
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let tab_ix: usize = std::env::var("FXI_BENCH_TAB").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses");
    let mut tab = ws.tabs.into_iter().nth(tab_ix).expect("tab exists");
    crate::canvas::viewer::refresh_motion_units(&mut tab.snarl);
    let (graph, _) = crate::app::graph::build_processing_graph(&tab.snarl, Default::default());
    let devs = source_devices(&graph);

    let arc_graph = flexinput_engine::new_arc_graph();
    arc_graph.store(Arc::new(graph));
    let signals = flexinput_engine::new_arc_signals();
    let bus: flexinput_engine::SinkBus = Arc::new(RwLock::new(HashMap::new()));
    let sync = EngineSync::new();
    flexinput_engine::spawn_processing_thread(
        arc_graph, Arc::clone(&signals),
        Arc::new(Mutex::new(flexinput_engine::ProcessingOutput::default())),
        Arc::clone(&bus), Arc::new(std::sync::atomic::AtomicU32::new(2000)),
        flexinput_engine::new_ui_source_block(), flexinput_engine::new_ui_source_block(),
        flexinput_engine::new_ui_source_block(), Arc::clone(&sync),
    );

    let waiter = HrWaiter::new();
    let pace = HrWaiter::new();
    // FXI_KICK_HZ=2000 wakes the engine once per tick, as the old timer did,
    // to compare CPU against the default 500 (one wakeup per poll).
    let kick_hz: u32 = std::env::var("FXI_KICK_HZ").ok().and_then(|s| s.parse().ok()).unwrap_or(500);
    let interval = std::time::Duration::from_secs_f64(1.0 / kick_hz as f64);
    let polls_per_input = (kick_hz / 500).max(1);
    let budget_pct: f32 = std::env::var("FXI_BUDGET_PCT").ok().and_then(|s| s.parse().ok()).unwrap_or(85.0);
    let cpu0 = process_cpu();
    let wall0 = Instant::now();
    let mut waits_us: Vec<u128> = Vec::new();
    let (mut fresh, mut late, mut other, mut timeouts) = (0, 0, 0, 0);
    let mut out: HashMap<(String, String), flexinput_core::Signal> = HashMap::new();
    for i in 0..1500 * polls_per_input {
        let t0 = Instant::now();
        let held = (i / polls_per_input) % 2 == 1;
        // The input itself changes at 500 Hz whatever the kick rate.
        if i % polls_per_input == 0 {
            let mut s = signals_at(&devs, i as f32 * 0.002);
            for (k, v) in s.iter_mut() {
                if matches!(v, flexinput_core::Signal::Bool(_)) {
                    *v = flexinput_core::Signal::Bool(k.1 == "btn_south" && held);
                }
            }
            signals.store(Arc::new(s));
        }
        let frame = sync.input_published();
        let tw = Instant::now();
        if !sync.wait_consumed(frame, interval.mul_f32(budget_pct / 100.0), &waiter) {
            timeouts += 1;
        } else if i > 50 {
            waits_us.push(tw.elapsed().as_micros());
        }
        flexinput_engine::drain_sink_bus_into(&bus, &mut out);
        if i > 50 {
            let v = out.iter()
                .find(|(k, _)| k.0.starts_with("virtual.") && k.1 == "btn_south")
                .map(|(_, v)| v.as_bool());
            match v {
                Some(b) if b == held => fresh += 1,
                Some(_) => late += 1,
                None => other += 1,
            }
        }
        let el = t0.elapsed();
        if el < interval { pace.wait(interval - el); }
    }
    let cpu_ms_per_s = (process_cpu() - cpu0) * 1e3 / wall0.elapsed().as_secs_f64();
    eprintln!("kicks at {kick_hz} Hz: process CPU {cpu_ms_per_s:.0} ms per second");
    waits_us.sort();
    let pct = |p: usize| waits_us.get(waits_us.len() * p / 100).copied().unwrap_or(0);
    eprintln!("virtual btn_south: fresh (same poll) {fresh}, one poll late {late}, absent {other}; engine waits timed out {timeouts}");
    eprintln!("kick→consumed µs (successful waits): p50 {} p90 {} p99 {} max {}",
        pct(50), pct(90), pct(99), waits_us.last().copied().unwrap_or(0));
}

/// How egui's font atlas grows when text is drawn at many distinct sizes (as a
/// continuously scaled pinned widget or a rect-sized label does), at the
/// atlas width the app runs with (`FXI_ATLAS_SIDE`, default 8192).
#[test]
#[ignore]
fn perf_font_atlas() {
    let side: usize = std::env::var("FXI_ATLAS_SIDE").ok().and_then(|s| s.parse().ok()).unwrap_or(8192);
    let ctx = egui::Context::default();
    let text = "Remapper  left_stick_x  Right Trigger (analog)  0123456789 +-.,%";
    let mut sizes_drawn = 0;
    for step in 0..=2000 {
        // 10.0 .. 40.0 pt in steps of 0.015 — what a slow drag-resize produces.
        let size = 10.0 + step as f32 * 0.015;
        let mut input = egui::RawInput::default();
        input.max_texture_side = Some(side);
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label(egui::RichText::new(text).size(size));
            });
        });
        sizes_drawn += 1;
        if step % 250 == 0 {
            let [w, h] = ctx.fonts(|f| f.font_image_size());
            eprintln!("{sizes_drawn:>5} sizes: atlas {w}x{h} = {:.0} MB CPU (+ same on GPU)", (w * h * 4) as f64 / 1e6);
        }
    }
}

/// Memory picture of each tab: what the loaded patch holds on the heap, what
/// one undo snapshot (a full snarl clone) costs, and what the per-frame graph
/// snapshot holds. `FXI_BENCH_WORKSPACE` as for `perf_bench`.
#[test]
#[ignore]
fn perf_memory() {
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let (ws, ws_bytes) = heap_bytes_of(|| {
        crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses")
    });
    eprintln!("whole workspace in memory: {:.1} MB ({} tabs)", ws_bytes as f64 / 1e6, ws.tabs.len());
    eprintln!("{:<28} | {:>12} {:>14} {:>12}", "tab", "snarl clone", "x50 undo", "graph snap");
    let mut total_undo = 0i64;
    for (ti, tab) in ws.tabs.iter().enumerate() {
        let (clone, snarl_b) = heap_bytes_of(|| tab.snarl.clone());
        drop(clone);
        let (g, graph_b) = heap_bytes_of(|| crate::app::graph::build_processing_graph(&tab.snarl, Default::default()));
        drop(g);
        total_undo += snarl_b * 50;
        let title: String = tab.title.chars().take(24).collect();
        eprintln!("{:<28} | {:>9.2} MB {:>11.1} MB {:>9.2} MB",
            format!("{ti} {title}"), snarl_b as f64 / 1e6, snarl_b as f64 * 50.0 / 1e6, graph_b as f64 / 1e6);
    }
    eprintln!("full undo stacks on every tab: {:.0} MB", total_undo as f64 / 1e6);
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Every device a top-level `device.source` reads, with the pins it publishes:
/// the full canonical AutoMap set (what a real pad's backend publishes) plus the
/// node's own output pins.
fn source_devices(graph: &ProcessingGraph) -> HashMap<String, Vec<(String, SignalType)>> {
    let mut devs: HashMap<String, Vec<(String, SignalType)>> = HashMap::new();
    for n in &graph.nodes {
        if n.module_id != "device.source" { continue; }
        let Some(dev) = n.device_id.clone() else { continue; };
        let pins = devs.entry(dev).or_default();
        for p in automap::ALL_PINS {
            if !pins.iter().any(|(id, _)| id == p.id) {
                pins.push((p.id.to_string(), p.signal_type));
            }
        }
        for id in &n.output_pin_ids {
            if !id.is_empty() && !pins.iter().any(|(p, _)| p == id) {
                pins.push((id.clone(), SignalType::Float));
            }
        }
    }
    devs
}

/// A moving input frame at time `t`: sticks circle, gyro/accel wobble, buttons
/// toggle at a few Hz — so filters, gestures and state machines all do work.
fn signals_at(devs: &HashMap<String, Vec<(String, SignalType)>>, t: f32) -> HashMap<(String, String), Signal> {
    let mut m = HashMap::new();
    for (dev, pins) in devs {
        for (i, (pin, ty)) in pins.iter().enumerate() {
            let ph = t * (1.0 + (i % 7) as f32 * 0.37);
            let sig = match ty {
                SignalType::Vec2 => Signal::Vec2(Vec2::new(ph.sin() * 0.8, ph.cos() * 0.8)),
                SignalType::Bool => Signal::Bool(((t * 3.0 + i as f32 * 0.13) as i32) % 4 == 0),
                SignalType::Int => Signal::Int(0),
                _ => Signal::Float(ph.sin() * 0.5),
            };
            m.insert((dev.clone(), pin.clone()), sig);
        }
    }
    m
}

#[test]
#[ignore]
fn perf_bench() {
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path))
        .expect("workspace parses");
    let only: Option<usize> = std::env::var("FXI_BENCH_TAB").ok().and_then(|s| s.parse().ok());
    let ticks: u32 = std::env::var("FXI_BENCH_TICKS").ok().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    const SR: f32 = 2000.0;

    eprintln!("{:<28} {:>5} | {:>9} {:>9} | {:>9} {:>9} {:>9} | {:>7}",
        "tab", "nodes", "build µs", "build al", "tick µs", "(median)", "tick al", "sinks");
    for (ti, mut tab) in ws.tabs.into_iter().enumerate() {
        if only.is_some_and(|o| o != ti) { continue; }
        crate::canvas::viewer::refresh_motion_units(&mut tab.snarl);

        // UI side: the graph rebuild that runs every frame.
        // Timings are the FASTEST of several blocks: this machine shares its
        // cores with OneDrive and friends, and the minimum is what the code
        // costs when nothing else interferes.
        let (graph, _) = crate::app::graph::build_processing_graph(&tab.snarl, Default::default());
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let mut build_blocks = Vec::new();
        for _ in 0..10 {
            let t0 = Instant::now();
            for _ in 0..30 {
                let g = crate::app::graph::build_processing_graph(&tab.snarl, Default::default());
                std::hint::black_box(g);
            }
            build_blocks.push(t0.elapsed().as_secs_f64() * 1e6 / 30.0);
        }
        let build_us = build_blocks.iter().cloned().fold(f64::MAX, f64::min);
        let build_al = (ALLOCS.load(Ordering::Relaxed) - a0) as f64 / 300.0;

        // Engine side: the sample-rate tick. Inputs change every 4 ticks — the
        // I/O thread publishes at 500 Hz against the 2 kHz engine.
        let devs = source_devices(&graph);
        let mut state: HashMap<usize, NodeState> = HashMap::new();
        let mut out = TickOutput::default();
        let dt = 1.0 / SR;
        let mut sigs = signals_at(&devs, 0.0);
        for k in 0..2000u32 { // warm-up: state maps, capacities
            if k % 4 == 0 { sigs = signals_at(&devs, k as f32 * dt); }
            eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
        }
        let mut eval_al = 0u64;
        let block = 500u32;
        let mut tick_blocks = Vec::new();
        let mut k = 0u32;
        while k < ticks {
            let mut ns = 0u128;
            for _ in 0..block {
                if k % 4 == 0 { sigs = signals_at(&devs, (2000 + k) as f32 * dt); }
                out.skip_display = k % 4 != 3; // as the engine does: only a burst's last tick is read
                out.same_input = k % 4 != 0;
                let a = ALLOCS.load(Ordering::Relaxed);
                let t = Instant::now();
                eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
                ns += t.elapsed().as_nanos();
                eval_al += ALLOCS.load(Ordering::Relaxed) - a;
                k += 1;
            }
            tick_blocks.push(ns as f64 / 1e3 / block as f64);
        }
        tick_blocks.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let tick_us = tick_blocks[0];
        let tick_med = tick_blocks[tick_blocks.len() / 2];
        let tick_al = eval_al as f64 / k as f64;


        let total_nodes = count_nodes(&graph);
        let title: String = tab.title.chars().take(24).collect();
        eprintln!("{:<28} {:>5} | {:>9.1} {:>9.0} | {:>9.2} {:>9.2} {:>9.0} | {:>7}",
            format!("{ti} {title}"), total_nodes, build_us, build_al, tick_us, tick_med, tick_al, out.sink_outputs.len());
    }
}

fn count_nodes(g: &ProcessingGraph) -> usize {
    g.nodes.iter().map(|n| 1 + n.inline_subgraph.as_ref().map_or(0, |s| count_nodes(&s.graph))).sum()
}

/// Statistical profile of the engine tick (or the graph build) for one tab:
/// `FXI_BENCH_SAMPLE=tick|build FXI_BENCH_TAB=n`. Prints the hottest functions
/// by self and inclusive samples.
#[test]
#[ignore]
fn perf_sample() {
    let (Ok(path), Ok(what)) = (std::env::var("FXI_BENCH_WORKSPACE"), std::env::var("FXI_BENCH_SAMPLE")) else {
        eprintln!("FXI_BENCH_WORKSPACE / FXI_BENCH_SAMPLE not set — nothing to sample");
        return;
    };
    let tab_ix: usize = std::env::var("FXI_BENCH_TAB").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let secs: f32 = std::env::var("FXI_BENCH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses");
    let mut tab = ws.tabs.into_iter().nth(tab_ix).expect("tab exists");
    crate::canvas::viewer::refresh_motion_units(&mut tab.snarl);
    let (graph, _) = crate::app::graph::build_processing_graph(&tab.snarl, Default::default());
    let devs = source_devices(&graph);
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut out = TickOutput::default();
    let dt = 1.0 / 2000.0;
    let mut sigs = signals_at(&devs, 0.0);
    for k in 0..2000u32 {
        if k % 4 == 0 { sigs = signals_at(&devs, k as f32 * dt); }
        eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
    }
    let sampler = sampler::Sampler::start();
    let t0 = Instant::now();
    let mut k = 0u32;
    while t0.elapsed().as_secs_f32() < secs {
        if what == "build" {
            std::hint::black_box(crate::app::graph::build_processing_graph(&tab.snarl, Default::default()));
        } else {
            if k % 4 == 0 { sigs = signals_at(&devs, (2000 + k) as f32 * dt); }
            eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
        }
        k += 1;
    }
    sampler.stop_and_report(30);
}

/// Where a tick allocates: every 4th allocation's call stack, as a share of them.
/// `FXI_BENCH_TAB` (default 0), with the same profile build as `perf_sample`.
#[test]
#[ignore]
fn perf_alloc_sites() {
    let Ok(path) = std::env::var("FXI_BENCH_WORKSPACE") else {
        eprintln!("FXI_BENCH_WORKSPACE not set — nothing to measure");
        return;
    };
    let tab_ix: usize = std::env::var("FXI_BENCH_TAB").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ws = crate::settings::load_workspace_from(std::path::Path::new(&path)).expect("workspace parses");
    let mut tab = ws.tabs.into_iter().nth(tab_ix).expect("tab exists");
    crate::canvas::viewer::refresh_motion_units(&mut tab.snarl);
    let (graph, _) = crate::app::graph::build_processing_graph(&tab.snarl, Default::default());
    let devs = source_devices(&graph);
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut out = TickOutput::default();
    let dt = 1.0 / 2000.0;
    let sigs = signals_at(&devs, 0.0);
    for _ in 0..2000u32 {
        eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
    }
    let before = ALLOCS.load(Ordering::Relaxed);
    sampler::trace_allocs(true);
    const TICKS: u64 = 500;
    for _ in 0..TICKS {
        eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
    }
    sampler::trace_allocs(false);
    eprintln!("{} allocations per tick", (ALLOCS.load(Ordering::Relaxed) - before) / TICKS);
    sampler::report_traced_allocs(60);
}

#[cfg(windows)]
mod sampler {
    //! Minimal in-process sampling profiler: a second thread suspends the
    //! target every ~1 ms, unwinds its stack with the OS unwinder
    //! (RtlVirtualUnwind — works without frame pointers), and resumes it.
    //! Nothing allocates while the target is suspended (it may hold the heap
    //! lock). Symbolized with dbghelp at the end.
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use std::sync::Arc;

    type Handle = *mut c_void;
    const MAX_FRAMES: usize = 48;
    const CTX_SIZE: usize = 1232;
    const CONTEXT_FULL: u32 = 0x0010_000B;

    #[repr(C, align(16))]
    struct Ctx([u8; CTX_SIZE]);
    impl Ctx {
        fn rip(&self) -> u64 { u64::from_le_bytes(self.0[0xF8..0x100].try_into().unwrap()) }
        fn rsp(&self) -> u64 { u64::from_le_bytes(self.0[0x98..0xA0].try_into().unwrap()) }
        fn set_rip(&mut self, v: u64) { self.0[0xF8..0x100].copy_from_slice(&v.to_le_bytes()); }
        fn set_rsp(&mut self, v: u64) { self.0[0x98..0xA0].copy_from_slice(&v.to_le_bytes()); }
    }

    #[repr(C)]
    struct SymbolInfoW {
        size_of_struct: u32, type_index: u32, reserved: [u64; 2], index: u32, size: u32,
        mod_base: u64, flags: u32, value: u64, address: u64, register: u32, scope: u32,
        tag: u32, name_len: u32, max_name_len: u32, name: [u16; 512],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut *mut c_void, hash: *mut u32) -> u16;
        fn GetCurrentProcess() -> Handle;
        fn OpenThread(access: u32, inherit: i32, tid: u32) -> Handle;
        fn SuspendThread(h: Handle) -> u32;
        fn ResumeThread(h: Handle) -> u32;
        fn GetThreadContext(h: Handle, ctx: *mut Ctx) -> i32;
        fn CloseHandle(h: Handle) -> i32;
        fn RtlLookupFunctionEntry(pc: u64, image_base: *mut u64, history: *mut c_void) -> *mut c_void;
        fn RtlVirtualUnwind(kind: u32, image_base: u64, pc: u64, entry: *mut c_void, ctx: *mut Ctx,
            handler_data: *mut *mut c_void, establisher: *mut u64, ctx_ptrs: *mut c_void) -> *mut c_void;
    }
    #[link(name = "dbghelp")]
    extern "system" {
        fn SymInitializeW(h: Handle, path: *const u16, invade: i32) -> i32;
        fn SymSetOptions(opts: u32) -> u32;
        fn SymFromAddrW(h: Handle, addr: u64, disp: *mut u64, sym: *mut SymbolInfoW) -> i32;
    }

    /// Rust legacy symbol (`_ZN<len><id>...17h<hash>E`) → `a::b::c`.
    fn demangle(s: &str) -> String {
        let Some(rest) = s.trim_start_matches('_').strip_prefix("ZN") else { return s.to_string() };
        let b = rest.as_bytes();
        let (mut i, mut parts) = (0usize, Vec::new());
        while i < b.len() && b[i] != b'E' {
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() { i += 1; }
            let Ok(len) = rest[st..i].parse::<usize>() else { return s.to_string() };
            if i + len > b.len() { return s.to_string(); }
            parts.push(&rest[i..i + len]);
            i += len;
        }
        if parts.last().is_some_and(|p| p.len() == 17 && p.starts_with('h')) { parts.pop(); }
        let mut out = parts.join("::");
        for (from, to) in [("$LT$", "<"), ("$GT$", ">"), ("$RF$", "&"), ("$BP$", "*"), ("$C$", ","),
            ("$u20$", " "), ("$u27$", "'"), ("$u5b$", "["), ("$u5d$", "]"), ("$u7b$", "{"),
            ("$u7d$", "}"), ("$u7e$", "~"), ("$LP$", "("), ("$RP$", ")"), ("..", "::")] {
            out = out.replace(from, to);
        }
        out
    }

    /// Stacks of traced allocations; capacity reserved up front, so tracing
    /// doesn't allocate. Only one test traces, and only from its own thread.
    static ALLOC_TRACE: AtomicPtr<Vec<([u64; MAX_FRAMES], usize)>> = AtomicPtr::new(std::ptr::null_mut());

    #[inline]
    pub fn on_alloc(n: u64) {
        let trace = ALLOC_TRACE.load(Ordering::Relaxed);
        if trace.is_null() { return; }
        let _ = n;
        // SAFETY: set only by `trace_allocs`, which keeps it alive until cleared.
        let trace = unsafe { &mut *trace };
        if trace.len() == trace.capacity() { return; }
        let mut frames = [0u64; MAX_FRAMES];
        let got = unsafe {
            RtlCaptureStackBackTrace(2, MAX_FRAMES as u32, frames.as_mut_ptr() as *mut *mut c_void, std::ptr::null_mut())
        };
        trace.push((frames, got as usize));
    }

    pub fn trace_allocs(on: bool) {
        if on {
            let v = Box::new(Vec::with_capacity(200_000));
            ALLOC_TRACE.store(Box::into_raw(v), Ordering::Relaxed);
        } else {
            TRACED.store(ALLOC_TRACE.swap(std::ptr::null_mut(), Ordering::Relaxed), Ordering::Relaxed);
        }
    }

    static TRACED: AtomicPtr<Vec<([u64; MAX_FRAMES], usize)>> = AtomicPtr::new(std::ptr::null_mut());

    pub fn report_traced_allocs(top: usize) {
        // SAFETY: the pointer came from `Box::into_raw` in `trace_allocs`.
        let samples = unsafe { Box::from_raw(TRACED.swap(std::ptr::null_mut(), Ordering::Relaxed)) };
        report(*samples, top);
    }

    pub struct Sampler {
        stop: Arc<AtomicBool>,
        join: std::thread::JoinHandle<Vec<([u64; MAX_FRAMES], usize)>>,
    }

    impl Sampler {
        pub fn start() -> Self {
            let tid = unsafe { GetCurrentThreadId() };
            let stop = Arc::new(AtomicBool::new(false));
            let stop2 = stop.clone();
            let join = std::thread::spawn(move || {
                // THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION
                let h = unsafe { OpenThread(0x0002 | 0x0008 | 0x0040, 0, tid) };
                assert!(!h.is_null(), "OpenThread failed");
                let mut samples: Vec<([u64; MAX_FRAMES], usize)> = Vec::with_capacity(200_000);
                let mut ctx = Box::new(Ctx([0; CTX_SIZE]));
                while !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_micros(500));
                    if samples.len() == samples.capacity() { break; }
                    let mut frames = [0u64; MAX_FRAMES];
                    let mut n = 0usize;
                    unsafe {
                        if SuspendThread(h) == u32::MAX { break; }
                        ctx.0.fill(0);
                        ctx.0[0x30..0x34].copy_from_slice(&CONTEXT_FULL.to_le_bytes());
                        if GetThreadContext(h, &mut *ctx) != 0 {
                            while n < MAX_FRAMES {
                                let pc = ctx.rip();
                                if pc == 0 { break; }
                                frames[n] = pc;
                                n += 1;
                                let mut base = 0u64;
                                let entry = RtlLookupFunctionEntry(pc, &mut base, std::ptr::null_mut());
                                if entry.is_null() {
                                    // Leaf without unwind info: return address on top of stack.
                                    let sp = ctx.rsp();
                                    if sp == 0 { break; }
                                    ctx.set_rip(*(sp as *const u64));
                                    ctx.set_rsp(sp + 8);
                                } else {
                                    let mut hd: *mut c_void = std::ptr::null_mut();
                                    let mut est = 0u64;
                                    RtlVirtualUnwind(0, base, pc, entry, &mut *ctx, &mut hd, &mut est, std::ptr::null_mut());
                                }
                            }
                        }
                        ResumeThread(h);
                    }
                    samples.push((frames, n));
                }
                unsafe { CloseHandle(h) };
                samples
            });
            Sampler { stop, join }
        }

        pub fn stop_and_report(self, top: usize) {
            self.stop.store(true, Ordering::Relaxed);
            report(self.join.join().unwrap(), top);
        }
    }

    fn report(samples: Vec<([u64; MAX_FRAMES], usize)>, top: usize) {
        {
            let total = samples.len().max(1);
            let proc = unsafe { GetCurrentProcess() };
            // Search next to the test exe, where cargo leaves its PDB.
            let dir: Vec<u16> = std::env::current_exe().ok()
                .and_then(|p| p.parent().map(|d| d.as_os_str().to_owned()))
                .map(|s| { use std::os::windows::ffi::OsStrExt; s.encode_wide().chain([0]).collect() })
                .unwrap_or_else(|| vec![0]);
            unsafe {
                SymSetOptions(0x2 | 0x10 | 0x4); // UNDNAME | DEFERRED_LOADS | LOAD_LINES
                if SymInitializeW(proc, dir.as_ptr(), 1) == 0 {
                    eprintln!("SymInitializeW failed");
                }
            }
            let mut names: HashMap<u64, String> = HashMap::new();
            let mut name_of = |addr: u64| -> String {
                if let Some(n) = names.get(&addr) { return n.clone(); }
                let mut si: Box<SymbolInfoW> = Box::new(unsafe { std::mem::zeroed() });
                si.size_of_struct = 88;
                si.max_name_len = 511;
                let mut disp = 0u64;
                let n = if unsafe { SymFromAddrW(proc, addr, &mut disp, &mut *si) } != 0 {
                    demangle(&String::from_utf16_lossy(&si.name[..si.name_len.min(511) as usize]))
                } else {
                    format!("?{addr:#x}")
                };
                names.insert(addr, n.clone());
                n
            };
            let mut self_c: HashMap<String, usize> = HashMap::new();
            let mut incl_c: HashMap<String, usize> = HashMap::new();
            for (frames, n) in &samples {
                if *n == 0 { continue; }
                *self_c.entry(name_of(frames[0])).or_default() += 1;
                let mut seen: Vec<String> = Vec::new();
                for &pc in &frames[..*n] {
                    // Return addresses point past the call; step back into it.
                    let f = name_of(if pc == frames[0] { pc } else { pc - 1 });
                    if !seen.contains(&f) { seen.push(f); }
                }
                for f in seen { *incl_c.entry(f).or_default() += 1; }
            }
            let shorten = |s: &str| -> String {
                let s = s.replace("flexinput_engine::eval::", "eval::").replace("hashbrown::", "hb::");
                if s.chars().count() > 110 { format!("{}…", s.chars().take(110).collect::<String>()) } else { s }
            };
            let mut v: Vec<_> = self_c.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            eprintln!("== SELF ({} samples) ==", total);
            for (f, c) in v.iter().take(top) {
                eprintln!("{:6.2}%  {}", *c as f64 * 100.0 / total as f64, shorten(f));
            }
            let mut v: Vec<_> = incl_c.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            eprintln!("== INCLUSIVE ==");
            for (f, c) in v.iter().take(top + 15) {
                eprintln!("{:6.2}%  {}", *c as f64 * 100.0 / total as f64, shorten(f));
            }
            // FXI_BENCH_CALLERS=substr: who calls the leaf functions matching
            // `substr` (first frame up that doesn't match it).
            if let Ok(pat) = std::env::var("FXI_BENCH_CALLERS") {
                let mut callers: HashMap<String, usize> = HashMap::new();
                for (frames, n) in &samples {
                    if *n == 0 || !name_of(frames[0]).contains(&pat) { continue; }
                    let caller = frames[1..*n].iter()
                        .map(|&pc| name_of(pc - 1))
                        .find(|f| !f.contains(&pat) && !f.starts_with("alloc::") && !f.starts_with("core::"))
                        .unwrap_or_else(|| "?".into());
                    *callers.entry(caller).or_default() += 1;
                }
                let mut v: Vec<_> = callers.into_iter().collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                eprintln!("== CALLERS of leaf ~ {pat:?} ==");
                for (f, c) in v.iter().take(top) {
                    eprintln!("{:6.2}%  {}", *c as f64 * 100.0 / total as f64, shorten(f));
                }
            }
        }
    }
}
