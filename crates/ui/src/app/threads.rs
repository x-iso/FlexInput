//! Background threads owned by the app: the device I/O polling loop and
//! the MIDI enumeration watcher.

use super::*;

// ── Device I/O thread (polling-rate setting, default 500 Hz) ──────────────────

pub(crate) fn spawn_io_thread(
    mut backends: Vec<Box<dyn DeviceBackend>>,
    midi: Arc<Mutex<Option<MidiBackend>>>,
    proc_device_signals: flexinput_engine::ArcSignals,
    sink_bus: SinkBus,
    // App-level shared pool of virtual output devices. Membership is
    // managed by the UI thread (reconcile on patch load, prune on tab
    // close); the I/O thread only reads it.
    shared_virtual_devices: SharedDevicePool,
    // IDs referenced by the active tab's canvas. Devices in the pool
    // whose id is NOT in this set are silenced (`reset_outputs()`)
    // every tick — background tabs don't drive output.
    active_tab_device_ids: Arc<RwLock<HashSet<String>>>,
    io_bypass: Arc<AtomicBool>,
    // Gamepad-UI-nav suppression — treated identically to `io_bypass`.
    ui_nav_suppress: Arc<AtomicBool>,
    shared_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    shared_midi_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    polling_hz: Arc<AtomicU32>,
    device_rates: flexinput_engine::DeviceRates,
    scope_taps: flexinput_engine::ScopeTaps,
    spike_filter_settings: Arc<RwLock<HashMap<String, (bool, f32)>>>,
    // Global "route every pad through SDL" switch — pushed to every backend
    // each iteration so a live toggle re-arbitrates without a restart.
    sdl_all_pads: Arc<AtomicBool>,
    ping_requests: Arc<Mutex<Vec<String>>>,
) {
    use std::time::{Duration, Instant};

    std::thread::Builder::new()
        .name("device-io".into())
        .spawn(move || {
            // Bump the Windows system timer resolution to 1 ms so
            // `thread::sleep(Duration::from_millis(1))` actually sleeps
            // ~1 ms instead of the default ~15.6 ms. Without this, the
            // requested polling rate is capped at ~64 Hz regardless of
            // setting. Process-wide effect; matches what game-input
            // libraries do internally.
            #[cfg(windows)]
            unsafe {
                let r = windows_sys::Win32::Media::timeBeginPeriod(1);
                eprintln!("[device-io] timeBeginPeriod(1) -> {} (0 == TIMERR_NOERROR)", r);

                // Input must win over UI rendering. This thread polls physical
                // inputs and flushes the virtual-device outputs — the hard
                // real-time leg of the input→output path. Pin it above the UI
                // and render threads so a busy frame can never delay an input
                // flush. TIME_CRITICAL (not just ABOVE_NORMAL) because the loop
                // is a tight bounded poll-and-sleep: it yields the CPU every
                // iteration via `thread::sleep`, so it can't starve other
                // threads, but while runnable it should preempt them.
                use windows_sys::Win32::System::Threading::{
                    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
                };
                let ok = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
                eprintln!("[device-io] SetThreadPriority(TIME_CRITICAL) -> {} (nonzero == ok)", ok);
            }
            let mut last_enum = Instant::now() - Duration::from_secs(10);
            let mut last_midi_out: HashMap<(String, String), Signal> = HashMap::new();
            // Physical-pad haptic outputs we drove last tick (rumble, HD amp,
            // lightbar…). Used to actively send a single 0 when a pin's feedback
            // producer vanishes (network link dropped, wire disconnected, a game
            // closed) — otherwise the motor/coil sticks on its last value because
            // nothing writes it into sink_outputs anymore. See the zero-on-drop
            // pass after the physical feedback send below.
            let mut last_phys_haptics: HashMap<(String, String), Signal> = HashMap::new();
            // Latest virtual-device feedback (rumble/FFB from poll_outputs),
            // carried ACROSS ticks. Merged into the published signal map at the
            // START of each tick so the engine — which reads proc_device_signals
            // on its own thread, asynchronously — always sees the current
            // feedback. Without this, the per-tick "publish physical, then
            // re-merge virtual later" sequence left a window where the engine
            // sampled physical-only signals and read a virtual pad's rumble as 0,
            // so a game's rumble never routed back to the physical pad.
            let mut last_virt_sigs: HashMap<(String, String), Signal> = HashMap::new();
            // Measured I/O rate EMA. Updated each iteration; published via
            // `flexinput_engine::set_io_rate` so the UI can show the actual
            // poll rate (separate from the engine's sample rate).
            let mut last_loop_t = Instant::now();
            let mut measured_hz_ema: f32 = 0.0;
            // Per-device event accumulator. We sample the device backends'
            // raw event counts and convert to Hz on a fixed 500 ms cadence
            // (smooths short-term spikes while still feeling "live").
            let mut dev_event_acc: HashMap<String, u32> = HashMap::new();
            let mut dev_rate_ema: HashMap<String, f32> = HashMap::new();
            let mut last_rate_publish = Instant::now();

            // Active rumble-ping pulses: device_id → instant the pulse should stop.
            // Started when the UI pushes a ping request; cleared once expired (sending
            // a single rumble-off so the motors don't latch).
            let mut ping_until: HashMap<String, Instant> = HashMap::new();
            const PING_RUMBLE_MS: u64 = 200;

            // ── I/O stall diagnostics ─────────────────────────────────────
            // Cheap per-iteration section timing (a handful of Instant::now
            // calls, no allocation in steady state); prints ONE stderr line
            // when an iteration's BUSY time (sleep excluded) overruns
            // STALL_LOG_MS. Purpose: a periodic input freeze in the field
            // names its guilty section without attaching a profiler — the
            // every-few-seconds gap class of bug has now hidden in three
            // different sections (enumerate classification, gyro open retry,
            // and counting), so the instrumentation stays permanently. The
            // stderr report is opt-in (FLEXINPUT_IO_STALL_LOG=1): with the
            // known causes fixed, remaining hits are benign one-shots
            // (hotplug arrivals) that would only add log noise.
            const STALL_LOG_MS: u128 = 25;
            let stall_log = std::env::var("FLEXINPUT_IO_STALL_LOG").map_or(false, |v| v == "1");
            let io_started = Instant::now();
            let mut sect_marks: Vec<(&'static str, Duration)> = Vec::with_capacity(12);
            let mut backend_marks: Vec<Duration> = Vec::with_capacity(4);

            loop {
                puffin::GlobalProfiler::lock().new_frame();
                puffin::profile_scope!("io_thread_iter");
                let t0 = Instant::now();
                sect_marks.clear();
                backend_marks.clear();
                let mut sect_t = t0;
                macro_rules! mark {
                    ($name:literal) => {{
                        let now = Instant::now();
                        sect_marks.push(($name, now - sect_t));
                        // The write after the LAST mark of an iteration is
                        // intentionally dead (next iteration resets from t0).
                        #[allow(unused_assignments)]
                        {
                            sect_t = now;
                        }
                    }};
                }
                // Re-read polling rate each iteration so live retunes apply.
                let hz = polling_hz.load(Ordering::Relaxed).clamp(60, 4000);
                let interval = Duration::from_nanos(1_000_000_000 / hz as u64);

                // Push per-device snap-back spike-filter settings to backends.
                // Cheap: each backend's `set_spike_filter` early-returns when
                // the value is unchanged. We do this BEFORE polling so the
                // filter applies to this iteration's samples.
                {
                    puffin::profile_scope!("push_spike_filter");
                    let settings = spike_filter_settings.read().unwrap();
                    for (dev_id, (on, sens)) in settings.iter() {
                        for backend in &mut backends {
                            backend.set_spike_filter(dev_id, *on, *sens);
                        }
                    }
                }
                mark!("spike_filter");

                // Push the global-SDL switch before enumerate/poll so a live
                // toggle takes effect this iteration.
                {
                    let on = sdl_all_pads.load(Ordering::Relaxed);
                    for backend in &mut backends {
                        backend.set_sdl_all_pads(on);
                    }
                }

                // ── Poll physical inputs ──────────────────────────────────────
                let mut signals: HashMap<(String, String), Signal> = HashMap::new();
                {
                    puffin::profile_scope!("backends_poll");
                    for backend in &mut backends {
                        let bt = Instant::now();
                        for (dev, pin, sig) in backend.poll() {
                            signals.insert((dev, pin), sig);
                        }
                        for (dev, n) in backend.take_event_counts() {
                            *dev_event_acc.entry(dev).or_insert(0) += n;
                        }
                        backend_marks.push(bt.elapsed());
                    }
                }
                mark!("backends_poll");
                {
                    puffin::profile_scope!("midi_poll");
                    if let Ok(mut mg) = midi.try_lock() {
                        if let Some(m) = mg.as_mut() {
                            for (dev, pin, sig) in m.poll() {
                                signals.insert((dev, pin), sig);
                            }
                            for (dev, n) in m.take_event_counts() {
                                *dev_event_acc.entry(dev).or_insert(0) += n;
                            }
                        }
                    }
                }
                mark!("midi_poll");
                // Tap gyro/accel samples into the per-pin scope rings so the
                // calibration window can render at true polling Hz rather than
                // UI repaint Hz. We do this BEFORE moving `signals` into the
                // shared map.
                //
                // Skip the write-lock acquisition entirely when no taped pin
                // names appear in this iteration's signals — the common case
                // when no gyro-capable device is connected. Saves a contended
                // RwLock write per loop iteration (polling rate) on idle setups.
                let has_taped_pin = signals.keys()
                    .any(|(_, pin)| flexinput_engine::SCOPE_TAP_PINS.iter().any(|p| *p == pin.as_str()));
                if has_taped_pin {
                    puffin::profile_scope!("scope_taps_write");
                    let now = Instant::now();
                    let mut taps = scope_taps.write().unwrap();
                    let retain = Duration::from_millis(flexinput_engine::SCOPE_TAP_RETAIN_MS);
                    for ((dev, pin), sig) in &signals {
                        if !flexinput_engine::SCOPE_TAP_PINS.iter().any(|p| *p == pin.as_str()) {
                            continue;
                        }
                        let v = match sig {
                            Signal::Float(f) => *f,
                            Signal::Bool(b)  => if *b { 1.0 } else { 0.0 },
                            _ => continue,
                        };
                        let ring = taps.entry((dev.clone(), pin.clone()))
                            .or_insert_with(flexinput_engine::ScopeTapRing::new);
                        ring.push_back((now, v));
                        while let Some(&(t, _)) = ring.front() {
                            if now.duration_since(t) > retain
                                || ring.len() > flexinput_engine::SCOPE_TAP_MAX_LEN
                            {
                                ring.pop_front();
                            } else {
                                break;
                            }
                        }
                    }
                }
                mark!("scope_taps");

                {
                    puffin::profile_scope!("publish_signals");
                    // Fold the most recent virtual-device feedback into this
                    // tick's physical signals BEFORE publishing, so the engine
                    // (async reader on its own thread) never sees a window with
                    // the feedback missing. Physical signals for the same key win
                    // (a real device's live value overrides stale feedback); the
                    // current tick's fresh virtual poll is merged again below and
                    // refreshes last_virt_sigs for the next tick.
                    for (k, v) in &last_virt_sigs {
                        signals.entry(k.clone()).or_insert(*v);
                    }
                    // ArcSwap publish — consumers (proc thread, UI) read
                    // via `load_full()`, a refcount bump rather than a
                    // map clone under a RwLock.
                    proc_device_signals.store(std::sync::Arc::new(signals));
                }
                mark!("publish");

                // ── Enumerate gilrs devices periodically ──────────────────────
                // MIDI enumeration is handled by spawn_midi_watch_thread() so
                // the slow Win32 MIDI calls (60–70 ms with loopMIDI loaded)
                // don't stall this I/O loop.
                if last_enum.elapsed() > Duration::from_secs(2) {
                    puffin::profile_scope!("enumerate_devices");
                    let mut devs: Vec<PhysicalDevice> = Vec::new();
                    // Per-backend rows in the stall report (absolute durations,
                    // alongside the "enumerate" section total): a stall here has
                    // hidden in both backends already (gilrs classification,
                    // SDL's open-probe), so name the culprit directly.
                    for (bi, backend) in backends.iter_mut().enumerate() {
                        let bt = Instant::now();
                        devs.extend(backend.enumerate());
                        let name: &'static str =
                            match bi { 0 => "enum_b0", 1 => "enum_b1", _ => "enum_bN" };
                        sect_marks.push((name, bt.elapsed()));
                    }
                    // Append MIDI device list maintained by the MIDI watch thread.
                    devs.extend(shared_midi_devices.read().unwrap().iter().cloned());
                    *shared_devices.write().unwrap() = devs;
                    last_enum = Instant::now();
                }
                mark!("enumerate");

                // ── Get latest sink outputs from processing thread ─────────────
                // Uses a separate RwLock so this read never contends on proc_outputs.
                let sink_outputs: HashMap<(String, String), Signal> = {
                    puffin::profile_scope!("read_sink_bus");
                    sink_bus.read().unwrap().clone()
                };
                mark!("sink_bus");

                // ── Drive virtual & physical devices ──────────────────────────
                // Shared pool holds ALL virtual devices across every open
                // tab. The active-tab id filter decides which devices
                // actually route signals this tick; devices outside the
                // filter receive `reset_outputs()` so a background tab's
                // device idles instead of holding its last state.
                let bypass = io_bypass.load(Ordering::Relaxed)
                    || ui_nav_suppress.load(Ordering::Relaxed);
                let active_ids = active_tab_device_ids.read().unwrap().clone();
                {
                    puffin::profile_scope!("route_virtual_devices");
                    let mut devs = shared_virtual_devices.lock().unwrap();
                    // Mixed mode: a virtual gamepad active alongside the
                    // keyboard/mouse device. Forces physical-mouse suppression
                    // off so stick-driven aim stays smooth (the suppression
                    // heuristic misfires on games that warp the cursor). Recomputed
                    // each tick; cleared under bypass (no output flowing).
                    let mut km_active = false;
                    let mut pad_active = false;
                    for dev in devs.iter() {
                        if !active_ids.contains(dev.id()) { continue; }
                        if dev.id() == "virtual.keymouse" { km_active = true; }
                        else { pad_active = true; }
                    }
                    let mixed = !bypass && km_active && pad_active;
                    flexinput_virtual::set_mouse_mixed_mode_active(mixed);

                    // ── Mixed-output braiding (experimental) ───────────────────
                    // When a virtual gamepad AND keyboard/mouse are both active,
                    // braiding makes the gamepad and mouse SUBMIT in strict
                    // alternation (shared turn token in flexinput-virtual) so a
                    // gamepad HID flush never coincides with a mouse SendInput. We
                    // always `send` the latest gamepad state every tick (send sets
                    // the report buffer; flush submits it), so holding the flush
                    // until the gamepad's turn never drops input — it just controls
                    // submit timing. An idle mouse passes its turn, so it can't
                    // chop the pad. The keymouse thread reads the SAME token, so
                    // the two interleave. When braiding is off `braid_try_gamepad`
                    // returns true every tick → flush every tick as before.
                    let flush_gamepad = if mixed {
                        flexinput_virtual::braid_try_gamepad()
                    } else {
                        true
                    };

                    if bypass {
                        for dev in devs.iter_mut() { dev.reset_outputs(); }
                    } else {
                        // Silence devices not referenced by the active tab.
                        for dev in devs.iter_mut() {
                            if !active_ids.contains(dev.id()) {
                                dev.reset_outputs();
                            }
                        }
                        // Route signals to active-tab devices only (every tick —
                        // braiding gates the FLUSH timing below, not the state).
                        for ((device_id, pin_id), &signal) in &sink_outputs {
                            if !active_ids.contains(device_id) { continue; }
                            if let Some(dev) = devs.iter_mut().find(|d| d.id() == device_id) {
                                dev.send(pin_id, signal);
                            }
                        }
                        // Flush: background + keymouse devices every tick; the
                        // active-tab gamepad is braid-gated (see flush_gamepad).
                        // Background devices must flush to commit reset_outputs;
                        // the keymouse flush only updates its shared velocity (the
                        // OS mouse packet timing is braided on the keymouse thread).
                        for dev in devs.iter_mut() {
                            let active = active_ids.contains(dev.id());
                            let is_km = dev.id() == "virtual.keymouse";
                            if !active || is_km || flush_gamepad {
                                dev.flush();
                            }
                        }
                    }

                    // Poll rumble/feedback signals back from virtual devices —
                    // ALWAYS, even under bypass. Bypass suppresses *outgoing*
                    // mapped input to the virtual pad; it must NOT stop us
                    // draining *incoming* rumble/FFB the game sends back. The
                    // HIDMaestro backend decodes rumble *inside* poll_outputs (it
                    // drains the SHM output ring), so gating poll_outputs on
                    // !bypass meant a game's rumble never reached the physical pad
                    // whenever FlexInput was unfocused (bypass true). The ViGEm
                    // backends update rumble on a separate notification thread, so
                    // they were unaffected — which is why XInput rumble worked but
                    // HIDMaestro's didn't.
                    //
                    // CRITICAL: poll_outputs() must run on EVERY device every
                    // tick, NOT just active-tab ones. For HIDMaestro it *drains
                    // the SHM output ring* as a side effect — gating it on
                    // active_ids let frames pile up until the 64-slot ring was
                    // full, then one eventual poll dumped a 64-frame backlog with
                    // ~minute latency (the rumble never reached the physical pad
                    // in time). Draining is cleanup and must be continuous;
                    // routing is what gets gated. So we always drain, but only
                    // *publish* an active-tab device's feedback into the signal
                    // map so background-tab feedback can't leak into the wrong
                    // graph. (active_ids isn't reliably populated for a pure
                    // feedback-source device anyway — it's refreshed only on
                    // canvas events, not per frame.)
                    // Always drain every device's output ring (cleanup, prevents
                    // the 64-slot backlog), but only RECORD active-tab devices'
                    // feedback into `last_virt_sigs`. The actual publish into
                    // proc_device_signals happens at the TOP of the next tick
                    // (single merge), so we do NOT clone+store the whole signal
                    // map here. Previously this block load_full+cloned+stored the
                    // entire map EVERY tick (poll_outputs always returns the two
                    // rumble pins, so it never short-circuited) — a full-map clone
                    // + ArcSwap store at up to 4 kHz that added real I/O-thread
                    // load. Recording into last_virt_sigs and letting the
                    // top-of-tick merge publish is equivalent (sub-ms latency at
                    // these rates) and far cheaper.
                    for dev in devs.iter_mut() {
                        let id = dev.id().to_string();
                        let sigs = dev.poll_outputs(); // always drain the ring
                        if !active_ids.contains(&id) { continue; } // gate routing only
                        for (pin_id, sig) in sigs {
                            last_virt_sigs.insert((id.clone(), pin_id.to_string()), sig);
                        }
                    }
                }
                mark!("virtual_route");

                // Physical device outputs (rumble, lightbar) to gilrs pads run
                // regardless of bypass: these carry *incoming* feedback the game
                // sends back to a virtual pad (rumble/FFB), routed to a physical
                // pad via AutoMap — not FlexInput's own mapped input, which is
                // what bypass suppresses. Gating these on !bypass meant a game's
                // rumble never reached the physical pad whenever FlexInput was
                // unfocused (bypass stale-true). HD-rumble amplitude pins get a
                // default frequency injected below so they're audible on Switch
                // Pro (whose HD motors need a non-zero frequency).
                for ((device_id, pin_id), &signal) in &sink_outputs {
                    // Forward feedback to any PHYSICAL-backend pad — gilrs: (native)
                    // OR sdl: (route-all-through-SDL). Gating on gilrs: alone meant
                    // no mapped rumble/lightbar/HD-rumble ever reached an SDL pad
                    // (the "ping" worked only because it sends to backends directly,
                    // bypassing this loop). Each backend's send() ignores ids that
                    // aren't its own prefix, so fanning out to all is safe.
                    if device_id.starts_with("gilrs:") || device_id.starts_with("sdl:") {
                        for backend in &mut backends {
                            backend.send(device_id, pin_id, signal);
                        }
                    }
                }
                // Default HD-rumble frequency: when AutoMap feedback drives an
                // hd_*_amp pin (amplitude only) without a paired hd_*_freq, the
                // Switch Pro stays silent (its voice-coil needs a frequency). If
                // a side's amp is non-zero and no explicit freq was routed this
                // tick, inject ~320 Hz (0.6) so the rumble is audible — matching
                // what the manual ping pulse does.
                for (amp_pin, freq_pin) in [("hd_l_amp", "hd_l_freq"), ("hd_r_amp", "hd_r_freq")] {
                    for ((device_id, pin_id), &signal) in &sink_outputs {
                        let is_phys = device_id.starts_with("gilrs:") || device_id.starts_with("sdl:");
                        if !is_phys || pin_id != amp_pin { continue; }
                        let amp = signal.as_float();
                        let has_freq = sink_outputs
                            .get(&(device_id.clone(), freq_pin.to_string()))
                            .map(|s| s.as_float() > 0.0)
                            .unwrap_or(false);
                        if amp > 0.01 && !has_freq {
                            for backend in &mut backends {
                                backend.send(device_id, freq_pin, Signal::Float(0.6));
                            }
                        }
                    }
                }
                // Zero-on-drop: if a physical-pad haptic pin we drove last tick is
                // absent from sink_outputs this tick, its feedback producer went
                // away (network link dropped, wire disconnected, game closed).
                // Actively send 0 once so the motor/coil turns off instead of
                // sticking on its last value. Producers that are still live write
                // their pin every tick (0.0 when idle), so this only fires on a
                // genuine disappearance — no flicker in normal operation.
                for (key, &last) in &last_phys_haptics {
                    if sink_outputs.contains_key(key) { continue; }
                    if last.as_float() == 0.0 { continue; } // already silent
                    for backend in &mut backends {
                        backend.send(&key.0, &key.1, Signal::Float(0.0));
                    }
                }
                // Snapshot this tick's physical-pad outputs for next-tick drop
                // detection (gilrs: and sdl: feedback pins).
                last_phys_haptics.clear();
                for ((device_id, pin_id), &signal) in &sink_outputs {
                    if device_id.starts_with("gilrs:") || device_id.starts_with("sdl:") {
                        last_phys_haptics.insert((device_id.clone(), pin_id.clone()), signal);
                    }
                }
                if !bypass {
                    // MIDI output — only send on change to avoid flooding the bus.
                    if let Ok(mut mg) = midi.try_lock() {
                        if let Some(m) = mg.as_mut() {
                            for ((device_id, pin_id), &signal) in &sink_outputs {
                                if device_id.starts_with("midi_out:") {
                                    let key = (device_id.clone(), pin_id.clone());
                                    if last_midi_out.get(&key) != Some(&signal) {
                                        m.send(device_id, pin_id, signal);
                                        last_midi_out.insert(key, signal);
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Rumble ping ──────────────────────────────────────────────
                // Diagnostic pulse so the user can confirm which physical pad a
                // card maps to. Runs regardless of bypass — it's a deliberate,
                // user-initiated action, not patch output. New requests start a
                // 200 ms pulse; we drive both motors while active and send a
                // single rumble-off when the deadline passes.
                {
                    let now = Instant::now();
                    if let Ok(mut reqs) = ping_requests.try_lock() {
                        for dev_id in reqs.drain(..) {
                            ping_until.insert(dev_id, now + Duration::from_millis(PING_RUMBLE_MS));
                        }
                    }
                    if !ping_until.is_empty() {
                        let mut expired: Vec<String> = Vec::new();
                        for (dev_id, deadline) in ping_until.iter() {
                            let active = now < *deadline;
                            let amp = if active { Signal::Float(1.0) } else { Signal::Float(0.0) };
                            let amp_f = if active { 1.0 } else { 0.0 };
                            // Pinging an OWN-VIRTUAL pad (its loopback shown as a
                            // physical `gilrs:<kind>:v<N>`) can't go through a
                            // backend — those rumble a real device, and our virtual
                            // has no motor. Instead inject the rumble into the
                            // matching virtual device's feedback, so it flows
                            // poll_outputs → AutoMap → the mapped physical pad,
                            // exactly as a game's rumble would. (Restores the ViGEm
                            // behavior where pinging the virtual buzzed whatever it
                            // mapped to.)
                            if let Some((kind_prefix, vidx)) = own_virtual_pool_target(dev_id) {
                                let mut pool = shared_virtual_devices.lock().unwrap();
                                if let Some(dev) = pool
                                    .iter_mut()
                                    .filter(|d| {
                                        flexinput_virtual::kind_prefix(d.id()).as_str() == kind_prefix
                                    })
                                    .nth(vidx)
                                {
                                    dev.inject_rumble_ping(amp_f, amp_f);
                                }
                                if !active { expired.push(dev_id.clone()); }
                                continue;
                            }
                            // Drive every rumble pin family so the ping works
                            // regardless of controller type — each backend ignores
                            // pins it doesn't recognise:
                            //   rumble_strong/weak → XInput motors, DS4/DualSense
                            //                        classic motors
                            //   hd_l_amp/hd_r_amp  → Switch Pro HD rumble (needs a
                            //                        non-zero freq to be audible, so
                            //                        we set ~320 Hz too)
                            let freq = if active { Signal::Float(0.6) } else { Signal::Float(0.0) };
                            for backend in &mut backends {
                                backend.send(dev_id, "rumble_strong", amp);
                                backend.send(dev_id, "rumble_weak", amp);
                                backend.send(dev_id, "hd_l_amp", amp);
                                backend.send(dev_id, "hd_r_amp", amp);
                                backend.send(dev_id, "hd_l_freq", freq);
                                backend.send(dev_id, "hd_r_freq", freq);
                            }
                            if !active { expired.push(dev_id.clone()); }
                        }
                        for dev_id in expired { ping_until.remove(&dev_id); }
                    }
                }
                mark!("outputs_misc");

                // ── Stall report (see I/O stall diagnostics above) ────────────
                // Checked BEFORE the sleep so only busy time counts. Formats
                // lazily: nothing allocates unless a stall actually happened.
                let busy = t0.elapsed();
                if stall_log && busy.as_millis() >= STALL_LOG_MS {
                    let mut line = format!(
                        "[io-stall] +{:.1}s busy {:.1}ms:",
                        io_started.elapsed().as_secs_f32(),
                        busy.as_secs_f32() * 1e3,
                    );
                    for (name, d) in &sect_marks {
                        if d.as_micros() >= 500 {
                            line.push_str(&format!(" {}={:.1}ms", name, d.as_secs_f32() * 1e3));
                        }
                    }
                    for (i, d) in backend_marks.iter().enumerate() {
                        if d.as_micros() >= 500 {
                            line.push_str(&format!(" backend{}={:.1}ms", i, d.as_secs_f32() * 1e3));
                        }
                    }
                    eprintln!("{line}");
                }

                let elapsed = t0.elapsed();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }

                // Measured per-loop Hz via inter-iteration interval. EMA
                // smooths it so the UI label doesn't strobe at frame rate.
                let now = Instant::now();
                let dt = now.duration_since(last_loop_t).as_secs_f32().max(1e-4);
                last_loop_t = now;
                let inst_hz = 1.0 / dt;
                // EMA time constant scales with the loop rate: alpha 0.02
                // ≈ 0.1 s at the default 500 Hz, ≈ 0.4 s at the 125 Hz floor.
                let alpha = 0.02_f32;
                if measured_hz_ema == 0.0 {
                    measured_hz_ema = inst_hz;
                } else {
                    measured_hz_ema = measured_hz_ema * (1.0 - alpha) + inst_hz * alpha;
                }
                flexinput_engine::set_io_rate(measured_hz_ema.round() as u32);

                // Publish per-device rates every ~150 ms. Hz is computed from
                // raw event counts accumulated since the last publish, EMA-
                // smoothed across publishes for stability.
                let rate_dt = last_rate_publish.elapsed();
                if rate_dt >= Duration::from_millis(150) {
                    let rate_dt_s = rate_dt.as_secs_f32().max(1e-3);
                    // Compute new per-device instantaneous Hz, lerp into EMA.
                    let alpha = 0.6_f32;
                    let seen_devs: Vec<String> = dev_event_acc.keys().cloned().collect();
                    for dev in &seen_devs {
                        let count = dev_event_acc.get(dev).copied().unwrap_or(0) as f32;
                        let inst = count / rate_dt_s;
                        let prev = dev_rate_ema.get(dev).copied().unwrap_or(0.0);
                        let new = prev * (1.0 - alpha) + inst * alpha;
                        dev_rate_ema.insert(dev.clone(), new);
                    }
                    // Devices without recent events decay toward zero.
                    let known: Vec<String> = dev_rate_ema.keys().cloned().collect();
                    for dev in known {
                        if !dev_event_acc.contains_key(&dev) {
                            let prev = dev_rate_ema.get(&dev).copied().unwrap_or(0.0);
                            let new = prev * (1.0 - alpha);
                            if new < 0.5 {
                                dev_rate_ema.remove(&dev);
                            } else {
                                dev_rate_ema.insert(dev, new);
                            }
                        }
                    }
                    dev_event_acc.clear();
                    last_rate_publish = Instant::now();

                    // Publish to shared map.
                    if let Ok(mut rates) = device_rates.write() {
                        rates.clear();
                        for (dev, hz) in &dev_rate_ema {
                            rates.insert(dev.clone(), hz.round() as u32);
                        }
                    }
                }
            }
        })
        .expect("failed to spawn device I/O thread");
}

// ── MIDI watch thread ────────────────────────────────────────────────────────
//
// Runs the (slow, Windows-blocking) MIDI port enumeration off the I/O
// loop so it never stalls device polling. Cycle every 2 s:
//
//  1. Read pinned_midi_ids (set of midi_in:N / midi_out:N the canvas uses).
//  2. Lock MidiBackend briefly to drop any open OS handles that aren't
//     pinned — this lets the Windows MIDI subsystem report removed ports as
//     gone (otherwise an open handle keeps loopMIDI ports alive even after
//     the user deletes them in loopMIDI's UI).
//  3. Without the lock, call list_live_ports() — the slow Win32 call.
//  4. Lock MidiBackend again briefly to apply the diff (open connections for
//     pinned ports that came back, drop entries for vanished ports) and
//     rebuild the shared MIDI device list for the UI panel.
pub(crate) fn spawn_midi_watch_thread(
    midi: Arc<Mutex<Option<MidiBackend>>>,
    pinned_midi_ids: Arc<RwLock<HashSet<String>>>,
    shared_midi_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    refresh_requested: Arc<AtomicBool>,
) {
    use std::time::Duration;
    std::thread::Builder::new()
        .name("midi-watch".into())
        .spawn(move || {
            // MANUAL REFRESH (no periodic poll). MIDI port enumeration goes through
            // Windows' legacy `wdmaud.drv` (WinMM), which is part of the audio stack.
            // Calling it on a timer (we used to, every 2s) periodically disturbs the
            // WHOLE audio engine: listeners on a Discord/desktop stream hear a skip,
            // and Bluetooth headsets audibly pulse their noise floor, every couple of
            // seconds — even with no game and no virtual device. It can also hang
            // (>1.5s) or fault inside wdmaud when a composite USB-audio device (our
            // virtual DualSense) is present. So we DON'T poll: enumerate once at
            // startup, then only when the user explicitly asks (a "Refresh MIDI"
            // button sets `refresh_requested`). The thread otherwise just sleeps,
            // never touching wdmaud, so it can't disturb audio.
            let do_enumerate = |label: &str| {
                // Release non-canvas-pinned handles so the OS can free them, then the
                // slow enum without the lock, then apply the diff + publish.
                {
                    let pinned = pinned_midi_ids.read().unwrap().clone();
                    if let Ok(mut mg) = midi.lock() {
                        if let Some(m) = mg.as_mut() {
                            m.release_unpinned(&pinned);
                        }
                    }
                }
                let t0 = std::time::Instant::now();
                let (live_in, live_out) = MidiBackend::list_live_ports();
                let took = t0.elapsed();
                if took > Duration::from_millis(200) {
                    eprintln!("[midi] {label} enumerate took {took:?} (wdmaud)");
                }
                if let Ok(mut mg) = midi.lock() {
                    if let Some(m) = mg.as_mut() {
                        m.apply_port_diff(&live_in, &live_out);
                        let devs = m.enumerate();
                        *shared_midi_devices.write().unwrap() = devs;
                    }
                }
            };

            // One enumeration at startup so existing MIDI ports show up.
            do_enumerate("startup");

            // Then wait for explicit refresh requests. Cheap idle wakeups (250ms) just
            // to poll the flag — these do NOT touch wdmaud/audio.
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if refresh_requested.swap(false, Ordering::AcqRel) {
                    do_enumerate("manual-refresh");
                }
            }
        })
        .expect("failed to spawn MIDI watch thread");
}
