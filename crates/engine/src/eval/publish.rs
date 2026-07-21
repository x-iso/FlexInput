//! Publishing into the shared signal bus, and the reserved key namespaces
//! that keep publishers from colliding: consumed markers, source blocks,
//! and the macro carry uid.
//!
//! One publisher per producer kind — collectors, feedback control, audio
//! stream haptics, the network transports, and the AutoMap
//! fork/combiner/selector nodes.

use super::*;

pub(crate) const CONSUMED_PREFIX: &str = "__consumed__:";

/// Reserved `state` key holding the cross-tick menu carry state
/// (`NodeState::macro_prev` / `source_block` / `unblocked_src`). No real node
/// ever has this uid.
pub(crate) const MACRO_CARRY_UID: usize = usize::MAX;

/// `collector_sigs` key prefix a Virtual Menu writes its SOURCE-BLOCK request
/// under: `("{SRC_BLOCK_PREFIX}{device_id}", pin_id) = Bool(true)`. Drained at
/// tick end into `NodeState::source_block` and applied to `dev_sigs` next tick.
pub(crate) const SRC_BLOCK_PREFIX: &str = "__src_block__:";

/// Write `__consumed__:{pin}` markers into `collector_sigs` under `key` for
/// every pin a Remapper claimed — both the claimed cardinals/buttons and the
/// underlying stick axes of any claimed cardinal (so a Combiner suppresses the
/// raw axis too, not just the synthetic cardinal).
pub(crate) fn publish_consumed_markers(
    key: &str,
    claimed_digital: &HashSet<String>,
    claimed_analog: &HashSet<String>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let mark = |pin: &str, collector_sigs: &mut HashMap<(String, String), Signal>| {
        collector_sigs.insert((key.to_string(), format!("{CONSUMED_PREFIX}{pin}")), Signal::Int(1));
    };
    for pin in claimed_digital.iter().chain(claimed_analog.iter()) {
        mark(pin, collector_sigs);
        // Suppress the underlying axis AND bundled Vec2 too (covers sticks and
        // D-pad), otherwise the virtual device regenerates the consumed
        // direction from the still-raw axis / Vec2.
        if let Some((axis_pin, _)) = cardinal_axis_for_suppression(pin) {
            mark(axis_pin, collector_sigs);
            if let Some(v) = vec2_pin_for_axis(axis_pin) { mark(v, collector_sigs); }
        }
    }
}

pub(crate) fn combine_signals(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(x + y),
        (Signal::Vec2(x),  Signal::Vec2(y))  => Signal::Vec2(x + y),
        (Signal::Bool(x),  Signal::Bool(y))  => Signal::Bool(x || y),
        (Signal::Int(x),   Signal::Int(y))   => Signal::Int(x + y),
        (_, b) => b,
    }
}

// ── AutoMap consumer publishing helpers (shared by top-level + subgraph) ──────
//
// These three modules (Fork, Combiner, Selector) write into `collector_sigs`
// under "<kind>:{key_uid}" so downstream consumers can resolve them via the
// AutoMap routing scheme. `key_uid` is the publishing UID:
//   - top-level: `snap.node_uid` (raw)
//   - subgraph:  `namespaced_uid(outer_uid, snap.node_uid)`
//
// The subgraph form must use the namespaced UID so the keys match what
// `find_automap_device_rec` in the UI produces when it walks the wire chain
// across the sub-patch boundary.

/// Feedback Control node: inject wired inlet values into the physical source
/// pad's haptic channel and read outlet taps from the virtual destination's
/// feedback. Shared by the top-level loop and `eval_subgraph`.
///
/// Injection key: `("feedback_inject:{_fb_source_dev}", inlet_pin_id)`. The
/// physical `device.source` sink drains this in its feedback pass, keyed by its
/// own device id — so the bridge needs no per-uid plumbing and works at any
/// sub-patch depth. Multiple injectors targeting one pad combine additively.
///
/// Returns the node's full output vector: output[0] = AutoMap pass-through
/// (None placeholder), outputs[1..] = outlet taps (per `_fb_outlet_ids`).
pub(crate) fn feedback_control_publish(
    snap: &NodeSnap,
    computed: &[Vec<Option<Signal>>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) -> Vec<Option<Signal>> {
    // Resolve wired inputs for this node (input[0] = AutoMap bus, ignored as a
    // value; inputs[1..] = inlets parallel to `_fb_inlet_ids`).
    let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
        .map(|src| src.and_then(|(si, op)| {
            computed.get(si).and_then(|v| v.get(op)).copied().flatten()
        }))
        .collect();

    // ── Inlet injection ──────────────────────────────────────────────────────
    let source_dev = snap.params.get("_fb_source_dev").and_then(|v| v.as_str()).unwrap_or("");
    if !source_dev.is_empty() {
        let inlet_ids = snap.params.get("_fb_inlet_ids").and_then(|v| v.as_array());
        if let Some(inlet_ids) = inlet_ids {
            let key = format!("feedback_inject:{source_dev}");
            for (i, pin_v) in inlet_ids.iter().enumerate() {
                let Some(pin_id) = pin_v.as_str() else { continue; };
                if pin_id.is_empty() { continue; }
                // inputs[i + 1] — skip the AutoMap bus at input[0].
                if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
                    // Additive combine when several injectors hit the same pin
                    // on the same device (last-writer-wins would silently drop).
                    let entry = collector_sigs.entry((key.clone(), pin_id.to_string()));
                    use std::collections::hash_map::Entry;
                    match entry {
                        Entry::Occupied(mut o) => { *o.get_mut() = combine_signals(*o.get(), sig); }
                        Entry::Vacant(v)       => { v.insert(sig); }
                    }
                }
            }
        }
    }

    // ── Outlet taps ──────────────────────────────────────────────────────────
    let dest_dev = snap.params.get("_fb_dest_dev").and_then(|v| v.as_str()).unwrap_or("");
    let outlet_ids = snap.params.get("_fb_outlet_ids").and_then(|v| v.as_array());
    let mut out: Vec<Option<Signal>> = vec![None; snap.n_outputs];
    if let Some(outlet_ids) = outlet_ids {
        for (i, pin_v) in outlet_ids.iter().enumerate() {
            let Some(pin_id) = pin_v.as_str() else { continue; };
            // output[i + 1] — skip the AutoMap pass-through at output[0].
            let out_idx = i + 1;
            if out_idx >= out.len() { break; }
            if dest_dev.is_empty() { continue; }
            if let Some(&sig) = dev_sigs.get(&(dest_dev.to_string(), pin_id.to_string())) {
                out[out_idx] = Some(sig);
            }
        }
    }
    out
}

/// Audio Stream Haptics: pass the AutoMap bus through (so the gamepad's forward
/// signals continue downstream), then derive HD rumble from the node's WASAPI
/// loopback capture, blend it with any standard rumble already on the bus per the
/// `asth_modulator` slider, and inject the result into the target pad's feedback
/// channel (`feedback_inject:{_asth_dest_dev}`), drained by the feedback post-pass.
///
/// Modulator (`asth_modulator`, 0..1):
///   1.0  → audio amplitude REPLACES standard rumble (pure audio haptics).
///   0.0  → audio is GATED by standard-rumble amplitude (rumble decides *when*,
///          audio decides the *texture*): out = audio_amp * std_rumble.
///   0.5  → lighter audio, BOOSTED by standard-rumble events:
///          out = audio_amp * (base + (1-base) * std_rumble).
/// Linearly interpolated between those anchors.
/// Mirror the upstream AutoMap bus into this node's own `collector:{uid}` key,
/// so a downstream sink (which resolves the node as a `collector:` source) sees
/// the forward signals passing through. Reads the node's stamped upstream
/// references: `_automap_collector_id` (an upstream collector-style producer)
/// takes priority, with `_automap_device_id` (a raw physical device) filling
/// any pins the collector didn't carry. Shared by Audio Stream Haptics and
/// Network Send — both are pass-through AutoMap nodes.
pub(crate) fn republish_bus_as_collector(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let uid_key = format!("collector:{}", uid);
    let upstream_dev = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let upstream_collector = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !upstream_collector.is_empty() {
        let copies: Vec<(String, Signal)> = collector_sigs.iter()
            .filter(|((dev, _), _)| dev == &upstream_collector)
            .map(|((_, pin), sig)| (pin.clone(), *sig))
            .collect();
        for (pin, sig) in copies {
            collector_sigs.insert((uid_key.clone(), pin), sig);
        }
        if !upstream_dev.is_empty() {
            for pin in flexinput_core::automap::ALL_PINS {
                let key = (uid_key.clone(), pin.id.to_string());
                if collector_sigs.contains_key(&key) { continue; }
                if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                    collector_sigs.insert(key, sig);
                }
            }
        }
    } else if !upstream_dev.is_empty() {
        for pin in flexinput_core::automap::ALL_PINS {
            if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                collector_sigs.insert((uid_key.clone(), pin.id.to_string()), sig);
            }
        }
    }
}

pub(crate) fn audio_stream_haptics_publish(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) -> Vec<Option<Signal>> {
    // `uid` is this node's effective publishing id: `snap.node_uid` at the top level,
    // the namespaced uid inside a sub-patch. It keys the collector pass-through AND
    // the loopback capture lookup (both must match what the capture manager + the
    // downstream sink resolver use), so ASTH works identically nested or not.
    // ── 1. AutoMap pass-through (mirror the Collector's phase-1 copy). ─────────
    let uid_key = format!("collector:{}", uid);
    republish_bus_as_collector(snap, uid, dev_sigs, collector_sigs);

    // ── 2. Latest audio-derived haptics for this node. ────────────────────────
    // (l_amp, l_freq, r_amp, r_freq) — zeros on non-Windows (no WASAPI loopback).
    #[cfg(windows)]
    let audio = {
        let p = flexinput_devices::loopback_manager::latest_params(uid);
        (p.l_amp, p.l_freq, p.r_amp, p.r_freq)
    };
    #[cfg(not(windows))]
    let audio = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (audio_l_amp, mut audio_l_freq, audio_r_amp, mut audio_r_freq) = audio;

    // ── 2b. Two-band engine (HF + LF carriers from the EQ-gained spectrum). ───
    // The Switch Pro / DualSense LRAs play TWO carriers at once (LF + HF) that mix
    // on the actuator. We split the spectrum at a crossover and collapse each side
    // to its own (carrier_freq, energy): LF from the sub-crossover bins, HF from the
    // super-crossover bins. The per-band ENERGY fractions weight how the per-side
    // loudness splits across the two carriers (so a bass-heavy moment drives mostly
    // the LF carrier, a sizzly one the HF). `lf_carrier`/`hf_carrier` are 0..1 freqs;
    // `lf_frac`/`hf_frac` sum to ≤1. Defaults (no EQ / no spectrum): single LF
    // carrier from the autocorrelation pitch, HF silent.
    let mut lf_carrier = audio_l_freq; // both sides share the mono spectrum carrier
    let mut hf_carrier = 0.0f32;
    let mut lf_frac = 1.0f32;
    let mut hf_frac = 0.0f32;
    let crossover_hz = snap.params.get("asth_crossover").and_then(|v| v.as_f64()).unwrap_or(250.0) as f32;
    #[cfg(windows)]
    {
        // Flat unity EQ if none configured, so the two-band split still applies.
        let eq_pts = curve_points_from_params_keyed(&snap.params, "asth_eq_points")
            .unwrap_or_else(|| vec![[0.0, 0.5], [1.0, 0.5]]);
        let spectrum = flexinput_devices::loopback_manager::latest_spectrum(uid);
        let xpos = crossover_hz_to_pos(crossover_hz);
        let lf = multiband_collapse_band(&spectrum, &eq_pts, 0.0, xpos);
        let hf = multiband_collapse_band(&spectrum, &eq_pts, xpos, 1.0);
        let lf_e = lf.map(|(_, e)| e).unwrap_or(0.0);
        let hf_e = hf.map(|(_, e)| e).unwrap_or(0.0);
        let total = lf_e + hf_e;
        if total > 1.0e-4 {
            lf_frac = lf_e / total;
            hf_frac = hf_e / total;
            if let Some((c, _)) = lf { lf_carrier = c; }
            if let Some((c, _)) = hf { hf_carrier = c; }
        }
    }
    let _ = &mut audio_l_freq; let _ = &mut audio_r_freq; // superseded by lf/hf_carrier

    // ── 3. Standard rumble already on the bus (for the modulator). ────────────
    let bus_f = |pin: &str| -> f32 {
        sig_to_f32(collector_sigs.get(&(uid_key.clone(), pin.to_string())).copied()).unwrap_or(0.0)
    };
    // A tiny floor so residual/quantization noise on the rumble bus doesn't keep
    // the gate open when the game isn't actually rumbling.
    const STD_GATE_FLOOR: f32 = 0.02;
    let gate_std = |v: f32| if v <= STD_GATE_FLOOR { 0.0 } else { v };
    let std_l = gate_std(bus_f("rumble_strong").max(bus_f("hd_l_amp")));
    let std_r = gate_std(bus_f("rumble_weak").max(bus_f("hd_r_amp")));

    // ── 4. Amplitude calibration + frequency-bias, then the modulator blend. ──
    // (Volume is applied as INPUT GAIN in the capture thread, before detection, so
    //  it's already baked into the loudness here — lowering it restores headroom on
    //  a hot source instead of squashing.)
    // Curve  (asth_curve, 0.3..3, default 1): response exponent — >1 expands the
    //        quiet range (more dynamics), <1 compresses it (everything strong).
    // Amp min/max (asth_amp_min/max, 0..1): remap the shaped loudness onto a usable
    //        slice of the Switch Pro range — lift `min` above the actuator's dead
    //        zone (so weak audio is still felt) and cap `max`. Applied only when a
    //        side actually has signal, so silence stays silent (no floor on zero).
    // Band balance (asth_freq_bias, -1..1, default 0): tilts how the loudness splits
    //        across the two carriers. -1 = all energy to the LF carrier, +1 = all to
    //        HF, 0 = the natural spectral split. Visibly reshapes the LF/HF envelope.
    let curve     = (snap.params.get("asth_curve").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32).clamp(0.3, 3.0);
    let amp_min   = (snap.params.get("asth_amp_min").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32).clamp(0.0, 1.0);
    let amp_max   = (snap.params.get("asth_amp_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32).clamp(0.0, 1.0);
    let amp_lo = amp_min.min(amp_max);
    let amp_hi = amp_min.max(amp_max);
    let band_balance = (snap.params.get("asth_freq_bias").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32)
        .clamp(-1.0, 1.0);

    // Apply the band balance. This is a CREATIVE control, not a passive spectral
    // reweight: a pure energy reweight can't move amplitude into a band the source
    // has no content in (bass-heavy audio → hf_frac≈0 → balance→HF did nothing, the
    // reported "only LF ever applies"). So balance does two things that always have
    // an audible effect regardless of source spectrum:
    //   1. Migrates the felt-amplitude SPLIT toward the chosen band by mixing the
    //      natural spectral fraction with a forced target (all-LF at -1, all-HF at
    //      +1). At the extremes the split is fully forced, so HF gets amplitude even
    //      from bass-only audio.
    //   2. Biases each carrier's FREQUENCY toward the band edge so the felt pitch
    //      actually rises/falls with the slider (the Switch path collapses to one
    //      carrier, so this frequency shift IS the felt "texture").
    // NOTE: Balance is applied LATER, as the modulation DEPTH only — it must NOT
    // touch the carrier's amplitude (an earlier version reweighted lf_frac/hf_frac by
    // Balance, which drained the carrier band to silence at the modulator extreme).
    // Here lf_frac/hf_frac stay the NATURAL spectral fractions; the carrier always
    // plays at the full felt loudness regardless of Balance.

    // Curve only (Volume already applied pre-detection); range remap comes after the
    // blend so the floor is applied to the final felt amplitude, not pre-modulation.
    let shape_amp = |a: f32| a.clamp(0.0, 1.0).powf(curve);
    let audio_l_amp = shape_amp(audio_l_amp);
    let audio_r_amp = shape_amp(audio_r_amp);

    // ── Raw band envelope followers (exposed output pins). ────────────────────
    // These are the per-band share of the curve-shaped loudness BEFORE the
    // carrier/modulator (AM/RM) blend, the range remap, and the Balance depth
    // mapping — i.e. the "clean" two-band decomposition of the audio analysis.
    // The felt-output path below derives its own EFs from `l_amp` (post-blend);
    // these stay independent so a scope/readout on these pins shows the source.
    let raw_l_lf_ef = (audio_l_amp * lf_frac).clamp(0.0, 1.0);
    let raw_l_hf_ef = (audio_l_amp * hf_frac).clamp(0.0, 1.0);
    let raw_r_lf_ef = (audio_r_amp * lf_frac).clamp(0.0, 1.0);
    let raw_r_hf_ef = (audio_r_amp * hf_frac).clamp(0.0, 1.0);
    // Band carrier frequencies, converted from the engine's 0..1 spectral position
    // to Hz (log scale 40–1253, matching the spectrum/crossover mapping).
    let raw_lf_hz = band_pos_to_hz(lf_carrier);
    let raw_hf_hz = if hf_frac > 0.0 { band_pos_to_hz(hf_carrier) } else { 0.0 };

    let modulator = snap.params.get("asth_modulator").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let blend = |audio_amp: f32, std: f32| -> f32 {
        // anchors: gate(0) = audio*std ; boost(0.5) = audio*(0.5 + 0.5*std) ;
        // replace(1) = audio. Lerp between the two relevant anchors.
        let gate  = audio_amp * std;
        let boost = audio_amp * (0.5 + 0.5 * std);
        let out = if modulator <= 0.5 {
            let t = modulator / 0.5;          // 0..1 across gate→boost
            gate + (boost - gate) * t
        } else {
            let t = (modulator - 0.5) / 0.5;  // 0..1 across boost→replace
            boost + (audio_amp - boost) * t
        };
        out.clamp(0.0, 1.0)
    };
    // Remap a non-zero blended amplitude onto [amp_lo, amp_hi]; pass zero through
    // untouched so silence never gets lifted to the floor.
    let range_remap = |a: f32| if a <= 0.0 { 0.0 } else { (amp_lo + a * (amp_hi - amp_lo)).clamp(0.0, 1.0) };
    let l_amp = range_remap(blend(audio_l_amp, std_l));
    let r_amp = range_remap(blend(audio_r_amp, std_r));

    // Two independent envelope followers, one per band: each band's share of the
    // felt loudness = the side amplitude times that band's NATURAL spectral fraction.
    // These are the LF EF and HF EF — both keep their own amplitude (scope traces).
    let l_lf_ef = l_amp * lf_frac;
    let l_hf_ef = l_amp * hf_frac;
    let r_lf_ef = r_amp * lf_frac;
    let r_hf_ef = r_amp * hf_frac;

    // Carrier vs modulator. `asth_swap` flips which band is the felt carrier vs the
    // texture modulator. Default: LF carrier, HF modulator.
    //
    // CRITICAL (the "extreme of Balance goes silent" fix): the CARRIER amplitude is
    // the FULL felt loudness `l_amp` — it does NOT depend on Balance or on the band
    // split, so the rumble never drops out as you sweep Balance. Balance maps ONLY to
    // the modulation DEPTH:
    //   * at the CARRIER end of Balance → depth 0 (pure carrier, no flutter),
    //   * at the MODULATOR end → depth = the modulator band's EF (max texture).
    // So one extreme = clean carrier (unaffected), the other = fully-textured carrier
    // — exactly the expected behaviour, with no amplitude loss at either end.
    let swap = snap.params.get("asth_swap").and_then(|v| v.as_bool()).unwrap_or(false);
    // Balance −1..+1 → 0..1 "toward the modulator". Default (LF carrier, HF modulator):
    // +1 (HF) is the modulator end. Swapped (HF carrier, LF modulator): −1 (LF) is the
    // modulator end. `toward_mod` is 0 at the carrier end, 1 at the modulator end.
    let toward_mod = if swap {
        (-band_balance).clamp(0.0, 1.0) // LF end (−1) drives the LF modulator
    } else {
        band_balance.clamp(0.0, 1.0)    // HF end (+1) drives the HF modulator
    };
    let (l_carrier_amp, l_carrier_freq, l_mod_ef, l_mod_freq,
         r_carrier_amp, r_carrier_freq, r_mod_ef, r_mod_freq) = if swap {
        (l_amp, hf_carrier, l_lf_ef, lf_carrier,
         r_amp, hf_carrier, r_lf_ef, lf_carrier)
    } else {
        (l_amp, lf_carrier, l_hf_ef, hf_carrier,
         r_amp, lf_carrier, r_hf_ef, hf_carrier)
    };
    // Carrier amplitude = full felt loudness (Balance-independent). Modulation depth =
    // modulator-band EF scaled by how far Balance is toward the modulator end; gated
    // so a silent carrier stays silent.
    let l_lf_amp = l_carrier_amp;
    let r_lf_amp = r_carrier_amp;
    let l_hf_amp = if l_carrier_amp > 0.0 { (l_mod_ef * toward_mod).clamp(0.0, 1.0) } else { 0.0 };
    let r_hf_amp = if r_carrier_amp > 0.0 { (r_mod_ef * toward_mod).clamp(0.0, 1.0) } else { 0.0 };

    // ── Scalar output pins: raw band EFs + band carrier freqs (Hz). ──────────
    // Built BEFORE injection so the analysis outputs are still produced even when
    // no feedback destination is configured (early return below). Order MUST match
    // the descriptor's outputs: [AutoMap, LF EF L, HF EF L, LF EF R, HF EF R,
    // LF Hz, HF Hz]. output[0] (AutoMap) carries no scalar.
    let mut out: Vec<Option<Signal>> = vec![None; snap.n_outputs.max(1)];
    {
        let mut set = |i: usize, v: f32| { if let Some(slot) = out.get_mut(i) { *slot = Some(Signal::Float(v)); } };
        set(1, raw_l_lf_ef);
        set(2, raw_l_hf_ef);
        set(3, raw_r_lf_ef);
        set(4, raw_r_hf_ef);
        set(5, raw_lf_hz);
        set(6, raw_hf_hz);
    }

    // ── 5. Inject into the target pad's feedback channel. ──
    let dest_dev = snap.params.get("_asth_dest_dev").and_then(|v| v.as_str()).unwrap_or("");
    if dest_dev.is_empty() { return out; }
    let key = format!("feedback_inject:{dest_dev}");
    // `force` distinguishes the amplitude pins (always written, even at 0.0, so the
    // feedback post-pass actively drives the pad's rumble back to zero on silence —
    // otherwise a skipped injection leaves the pad holding its last value and it
    // buzzes forever) from the frequency pins (only meaningful when amp > 0).
    let mut put = |pin: &str, v: f32, force: bool| {
        if v <= 0.0 && !force { return; }
        use std::collections::hash_map::Entry;
        match collector_sigs.entry((key.clone(), pin.to_string())) {
            Entry::Occupied(mut o) => { *o.get_mut() = combine_signals(*o.get(), Signal::Float(v)); }
            Entry::Vacant(e)       => { e.insert(Signal::Float(v)); }
        }
    };
    // hd_* = carrier amplitude (always written so the pad zeroes on silence);
    // hd2_* = modulator depth.
    put("hd_l_amp", l_lf_amp, true);
    put("hd_r_amp", r_lf_amp, true);
    put("hd2_l_amp", l_hf_amp, true);
    put("hd2_r_amp", r_hf_amp, true);
    // hd_*_freq = carrier pitch (the felt frequency); hd2_*_freq = modulator pitch =
    // AM mod rate (Switch) / second-sine pitch (DualSense). Both follow the swap.
    if l_lf_amp > 0.0 { put("hd_l_freq", l_carrier_freq, false); }
    if r_lf_amp > 0.0 { put("hd_r_freq", r_carrier_freq, false); }
    if l_hf_amp > 0.0 { put("hd2_l_freq", l_mod_freq, false); }
    if r_hf_amp > 0.0 { put("hd2_r_freq", r_mod_freq, false); }

    out
}

/// Network Send: transmit the upstream AutoMap bus to a peer and inject any
/// feedback received from that peer back into the upstream physical pad.
///
/// `uid` is the node's effective publishing id (raw at top level, namespaced in
/// a sub-patch) — it keys BOTH the collector pass-through AND the network
/// worker's frame slots, so it must match what the UI's collector resolver and
/// the NetManager use. output[0] is the AutoMap pass-through (no scalar).
pub(crate) fn net_send_publish(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) -> Vec<Option<Signal>> {
    // ── 1. Forward pass-through: mirror the upstream bus into collector:{uid}
    //    so a locally-wired sink downstream still receives the pad's signals. ──
    let uid_key = format!("collector:{}", uid);
    republish_bus_as_collector(snap, uid, dev_sigs, collector_sigs);

    // ── 2. Pack the mirrored bus into a frame and hand it to the send worker. ──
    let mut frame = flexinput_net::BusFrame::empty();
    let prefix = format!("collector:{}", uid);
    for ((dev, pin), sig) in collector_sigs.iter() {
        if dev == &prefix {
            frame.set(pin, *sig);
        }
    }
    let _ = &uid_key; // (kept for symmetry with ASTH; prefix is the same string)
    flexinput_net::publish_send_frame(uid, frame);

    // ── 3. Feedback intake: values the peer's game requested, injected into the
    //    upstream physical pad's feedback channel (drained by the post-pass). ──
    let physical_dev = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
    if !physical_dev.is_empty() {
        if let Some((fb, age)) = flexinput_net::latest_feedback(uid) {
            // Match the send worker's status window: ignore feedback older than
            // ~1 s so a dead peer can't leave the pad buzzing forever.
            if age.as_millis() < 1000 {
                let key = format!("feedback_inject:{physical_dev}");
                for (pin, v) in fb.iter_present() {
                    collector_sigs
                        .entry((key.clone(), pin.to_string()))
                        .and_modify(|e| *e = combine_signals(*e, Signal::Float(v)))
                        .or_insert(Signal::Float(v));
                }
            }
        }
    }

    vec![None; snap.n_outputs.max(1)]
}

/// Network Receive: publish a peer's AutoMap bus (received over the network)
/// into collector:{uid} for downstream sinks. output[0] = AutoMap pass-through,
/// output[1] = Bool "Connected". The outgoing feedback frame is assembled later
/// by [`publish_recv_feedback_frames`] (a post-pass), not here — see the note
/// at the end of this function.
pub(crate) fn net_recv_publish(
    snap: &NodeSnap,
    uid: usize,
    _dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) -> Vec<Option<Signal>> {
    let uid_key = format!("collector:{}", uid);
    let stale_ms = snap.params.get("net_stale_ms").and_then(|v| v.as_u64()).unwrap_or(200) as u128;

    // ── 1. Publish the received bus, or a neutral fail-safe frame. ────────────
    let connected = match flexinput_net::latest_input(uid) {
        Some((frame, age)) if age.as_millis() < stale_ms => {
            for (pin, sig) in frame.iter_present() {
                collector_sigs.insert((uid_key.clone(), pin.to_string()), sig);
            }
            for extra in &frame.extras {
                collector_sigs.insert((uid_key.clone(), extra.name.clone()), extra.value);
            }
            true
        }
        // Stale or never received: actively center everything. Downstream holds
        // last value, so we MUST write neutral, not just stop publishing.
        _ => {
            for (pin, sig) in flexinput_net::BusFrame::neutral().iter_present() {
                collector_sigs.insert((uid_key.clone(), pin.to_string()), sig);
            }
            false
        }
    };

    // NOTE: the outgoing feedback frame is NOT built here. It's assembled by
    // `publish_recv_feedback_frames` in a post-pass, AFTER the whole graph has
    // run — otherwise this node (an AutoMap *source*, so it evaluates upstream of
    // the virtual sinks and any ASTH / Feedback Control node that targets it)
    // would only ever see last-tick's feedback.

    let mut out = vec![None; snap.n_outputs.max(2)];
    out[1] = Some(Signal::Bool(connected));
    out
}

/// Scan every sink in the graph (all sub-patch levels) and index the virtual
/// sink device ids by the AutoMap SOURCE they map from. Because `automap_source`
/// is resolved at build time by `find_automap_device_rec` — which traces across
/// sub-patch inlet/outlet boundaries and yields a network recv node's effective
/// `collector:{uid}` id — this correctly links a recv node to its downstream
/// virtual sinks regardless of which level either one lives on. That's the piece
/// the build-time `_net_fb_devs` stamp couldn't do (it only saw its own level).
pub(crate) fn collect_sink_sources(nodes: &[NodeSnap], out: &mut HashMap<String, Vec<String>>) {
    for node in nodes {
        if let Some(ref st) = node.sink_target {
            if st.device_id.starts_with("virtual.") {
                if let Some((src_id, _)) = &st.automap_source {
                    out.entry(src_id.clone()).or_default().push(st.device_id.clone());
                }
            }
        }
        if let Some(ref sg) = node.inline_subgraph {
            collect_sink_sources(&sg.graph.nodes, out);
        }
    }
}

/// Build and publish the outgoing feedback frame for every network_recv node,
/// descending into inline sub-patches. Keyed by EFFECTIVE uid (raw at top level,
/// namespaced inside a sub-patch) so it matches the socket worker, the recv
/// node's forward publish, and any `feedback_inject:collector:{uid}` an ASTH /
/// Feedback Control node on the receiver wrote while targeting this node.
///
/// Two feedback sources are max-combined per haptic pin:
///   (a) game-driven output the downstream virtual sinks report (classic rumble,
///       lightbar) — from `dev_sigs`, via `sink_sources` (the global source→sinks
///       index, so cross-level wiring is covered).
///   (b) HD/LED/trigger effects injected on the receiver — from `collector_sigs`
///       under `feedback_inject:collector:{uid}`.
///
/// Runs after the feedback_inject post-pass, so (b) is fully populated.
pub(crate) fn publish_recv_feedback_frames(
    nodes: &[NodeSnap],
    outer_uid: usize,
    nested: bool,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    sink_sources: &HashMap<String, Vec<String>>,
) {
    for node in nodes {
        let uid = if nested { namespaced_uid(outer_uid, node.node_uid) } else { node.node_uid };
        if node.module_id == NET_RECV_ID {
            let empty = Vec::new();
            let fb_devs = sink_sources.get(&format!("collector:{}", uid)).unwrap_or(&empty);
            let inject_key = format!("feedback_inject:collector:{}", uid);
            let mut fb = flexinput_net::FeedbackFrame::empty();
            let mut any = false;
            for pin in flexinput_core::automap::FEEDBACK_INLET_PINS {
                let mut best: Option<f32> = None;
                for dev in fb_devs {
                    if let Some(&sig) = dev_sigs.get(&(dev.clone(), pin.id.to_string())) {
                        let v = sig.as_float();
                        best = Some(best.map_or(v, |b| b.max(v)));
                    }
                }
                if let Some(&sig) = collector_sigs.get(&(inject_key.clone(), pin.id.to_string())) {
                    let v = sig.as_float();
                    best = Some(best.map_or(v, |b| b.max(v)));
                }
                if let Some(v) = best {
                    fb.set(pin.id, v);
                    any = true;
                }
            }
            if any {
                flexinput_net::publish_feedback_frame(uid, fb);
            }
        }
        if let Some(ref sg) = node.inline_subgraph {
            publish_recv_feedback_frames(&sg.graph.nodes, uid, true, dev_sigs, collector_sigs, sink_sources);
        }
    }
}

/// Inverse of [`crossover_hz_to_pos`]: map a 0..1 spectral band position back to Hz
/// on the same log scale (40 Hz–1253 Hz). Used to expose the band carrier frequencies
/// as Hz on the Audio Stream Haptics output pins.
pub(crate) fn band_pos_to_hz(pos: f32) -> f32 {
    const MIN: f32 = 40.0;
    const MAX: f32 = 1253.0;
    let pos = pos.clamp(0.0, 1.0);
    MIN * (MAX / MIN).powf(pos)
}

pub(crate) fn automap_fork_publish(
    snap: &NodeSnap,
    key_uid: usize,
    computed: &[Vec<Option<Signal>>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
    let collector_id_upstream = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
    // inputs[0] = AutoMap (ignored as value), inputs[1] = select
    let select = match snap.input_sources.get(1)
        .and_then(|src| src.and_then(|(si, op)| computed.get(si).and_then(|v| v.get(op)).copied().flatten()))
    {
        Some(Signal::Float(f)) => {
            let n = snap.n_outputs.max(1);
            ((f.clamp(0.0, 1.0) * (n as f32 - 1.0 + 0.5)).floor() as usize).min(n - 1)
        }
        Some(Signal::Bool(b)) => if b { 1 } else { 0 },
        _ => 0,
    };
    for out_idx in 0..snap.n_outputs {
        if out_idx != select { continue; }
        let key = format!("forksel:{}:{}", key_uid, out_idx);
        for pin in flexinput_core::automap::ALL_PINS {
            let sig = if !collector_id_upstream.is_empty() {
                collector_sigs.get(&(collector_id_upstream.to_string(), pin.id.to_string())).copied()
                    .or_else(|| dev_sigs.get(&(dev_id.to_string(), pin.id.to_string())).copied())
            } else {
                dev_sigs.get(&(dev_id.to_string(), pin.id.to_string())).copied()
            };
            if let Some(sig) = sig {
                collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
            }
        }
        if !collector_id_upstream.is_empty() {
            let copies: Vec<(String, Signal)> = collector_sigs.iter()
                .filter(|((d, p), _)| {
                    d == collector_id_upstream
                        && !automap::ALL_PINS.iter().any(|ap| ap.id == p.as_str())
                })
                .map(|((_, p), s)| (p.clone(), *s))
                .collect();
            for (pin, sig) in copies {
                collector_sigs.insert((key.clone(), pin), sig);
            }
        }
    }
}

pub(crate) fn automap_combiner_publish(
    snap: &NodeSnap,
    key_uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let input_devs = snap.params.get("_automap_input_devs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let input_collectors = snap.params.get("_automap_input_collectors")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let policy_map = snap.params.get("combiner_pin_policy")
        .and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let port_map = snap.params.get("combiner_pin_port")
        .and_then(|v| v.as_object()).cloned().unwrap_or_default();
    // Per-PORT default policy: `{ "0": "ADD", "1": "SORT", … }`. Applies to any
    // pin offered by that port that has no per-pin override. When several ports
    // offer a pin, the lowest-index (highest-priority) port that actually
    // carries the pin this tick wins the default. Falls back to global SORT.
    let port_default_map = snap.params.get("combiner_port_default")
        .and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let key = format!("combiner:{}", key_uid);

    fn clamp_for_pin(pin_id: &str, v: f32) -> f32 {
        match pin_id {
            "left_trigger" | "right_trigger" => v.clamp(0.0, 1.0),
            "left_stick_x" | "left_stick_y"
            | "right_stick_x" | "right_stick_y"
            | "dpad_x" | "dpad_y" => v.clamp(-1.0, 1.0),
            _ => v,
        }
    }
    fn clamp_vec2_for_pin(pin_id: &str, v: glam::Vec2) -> glam::Vec2 {
        if matches!(pin_id, "left_stick" | "right_stick" | "dpad") {
            glam::Vec2::new(v.x.clamp(-1.0, 1.0), v.y.clamp(-1.0, 1.0))
        } else { v }
    }

    fn read_pin_at(
        i: usize, pin_id: &str,
        input_devs: &[String], input_collectors: &[String],
        collector_sigs: &HashMap<(String, String), Signal>,
        dev_sigs: &HashMap<(String, String), Signal>,
    ) -> Option<Signal> {
        let collector_id = input_collectors.get(i).map(|s| s.as_str()).unwrap_or("");
        let dev_id       = input_devs.get(i).map(|s| s.as_str()).unwrap_or("");
        if !collector_id.is_empty() {
            collector_sigs.get(&(collector_id.to_string(), pin_id.to_string())).copied()
                .or_else(|| {
                    if !dev_id.is_empty() {
                        dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
                    } else { None }
                })
        } else if !dev_id.is_empty() {
            dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
        } else {
            None
        }
    }

    for pin in flexinput_core::automap::ALL_PINS {
        if let Some(port_v) = port_map.get(pin.id) {
            if let Some(port_u) = port_v.as_u64() {
                let n_inputs = input_devs.len();
                if n_inputs == 0 { continue; }
                let port = (port_u as usize).min(n_inputs - 1);
                if let Some(sig) = read_pin_at(port, pin.id,
                    &input_devs, &input_collectors, &collector_sigs, dev_sigs)
                {
                    collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
                }
                continue;
            }
        }

        // Effective policy: per-pin override > per-port default (from the
        // lowest-index port that actually carries this pin) > global SORT.
        let policy: &str = if let Some(p) = policy_map.get(pin.id).and_then(|v| v.as_str()) {
            p
        } else {
            let mut from_port = "SORT";
            if !port_default_map.is_empty() {
                for i in 0..input_devs.len() {
                    let offers = read_pin_at(i, pin.id,
                        &input_devs, &input_collectors, &collector_sigs, dev_sigs).is_some();
                    if !offers { continue; }
                    if let Some(p) = port_default_map.get(&i.to_string()).and_then(|v| v.as_str()) {
                        from_port = p;
                        break;
                    }
                }
            }
            from_port
        };

        // Hierarchy suppression: if an upstream Remapper on ANY input collector
        // CONSUMED this pin (mapped it away), the higher-level decision wins for
        // every port — the pin is dropped from all inputs, including raw-device
        // ports that still carry it. EXCEPTION: ADD explicitly opts into mixing,
        // so a Combiner port set to ADD keeps its value.
        let marker = format!("{CONSUMED_PREFIX}{}", pin.id);
        let any_consumed = input_collectors.iter().any(|cid| {
            !cid.is_empty() && collector_sigs.contains_key(&(cid.to_string(), marker.clone()))
        });
        if any_consumed && policy != "ADD" {
            // Hierarchy: a pin an upstream Remapper claimed is owned by that
            // Remapper. Take ITS (already per-side-suppressed) value — which
            // clears only the mapped direction, leaving e.g. dpad_right intact —
            // and ignore the raw ports entirely. The consuming collector is the
            // FIRST input collector that carries the marker.
            //
            // We must WRITE the value (even when it's off/zero), never drop it:
            // a virtual device latches the last value for any pin it stops
            // receiving, so a dropped consumed D-pad/button would stay stuck.
            let consuming = input_collectors.iter().find(|cid| {
                !cid.is_empty()
                    && collector_sigs.contains_key(&((*cid).clone(), marker.clone()))
            }).cloned();
            let owned = consuming
                .and_then(|cid| collector_sigs.get(&(cid, pin.id.to_string())).copied());
            let sig = owned.unwrap_or(match pin.signal_type {
                flexinput_core::SignalType::Bool => Signal::Bool(false),
                flexinput_core::SignalType::Vec2 => Signal::Vec2(glam::Vec2::ZERO),
                flexinput_core::SignalType::Int  => Signal::Int(0),
                _ => Signal::Float(0.0),
            });
            collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
            // Re-publish the marker so a Combiner-of-Combiners keeps honouring
            // the hierarchy.
            collector_sigs.insert((key.clone(), marker.clone()), Signal::Int(1));
            continue;
        }

        let mut raw: Vec<Signal> = Vec::with_capacity(input_devs.len());
        for i in 0..input_devs.len() {
            if let Some(s) = read_pin_at(i, pin.id,
                &input_devs, &input_collectors, &collector_sigs, dev_sigs)
            {
                raw.push(s);
            }
        }
        if raw.is_empty() { continue; }
        let resolved: Option<Signal> = match policy {
            // Priority merge: the first ASSERTED (non-default) value wins,
            // falling back to the highest-priority port when none is asserted.
            // A port that explicitly carries an "off" value (Bool(false), 0.0,
            // zero Vec2) must NOT mask a lower-priority port that is actually
            // contributing — otherwise a raw passthrough port (which reports
            // every button as false each tick) clobbers an upstream Remapper's
            // mapped OUTPUT pin (the output side carries no `consumed` marker,
            // so it doesn't take the hierarchy-suppression branch above).
            "SORT" => {
                let asserted = raw.iter().copied().find(|s| match s {
                    Signal::Bool(b) => *b,
                    Signal::Int(i)  => *i != 0,
                    Signal::Float(f) => *f != 0.0,
                    Signal::Vec2(v) => *v != glam::Vec2::ZERO,
                    Signal::Vec4(v) => *v != glam::Vec4::ZERO,
                });
                asserted.or_else(|| raw.into_iter().next())
            }
            "OR" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let any = raw.iter().any(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(any))
                }
                flexinput_core::SignalType::Vec2 => {
                    let pick = |sel: fn(&glam::Vec2) -> f32| {
                        raw.iter().filter_map(|s| match s {
                            Signal::Vec2(v) => Some(sel(v)), _ => None
                        }).fold(0.0_f32, |acc, x|
                            if x.abs() > acc.abs() { x } else { acc })
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id,
                        glam::Vec2::new(pick(|v| v.x), pick(|v| v.y)))))
                }
                _ => {
                    let f = raw.iter().filter_map(|s| sig_to_f32(Some(*s))).fold(0.0_f32, |acc, x|
                        if x.abs() > acc.abs() { x } else { acc });
                    Some(Signal::Float(clamp_for_pin(pin.id, f)))
                }
            },
            "AND" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let all = raw.iter().all(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(all))
                }
                flexinput_core::SignalType::Vec2 => {
                    let pick = |sel: fn(&glam::Vec2) -> f32| {
                        let mut it = raw.iter().filter_map(|s| match s {
                            Signal::Vec2(v) => Some(sel(v)), _ => None
                        });
                        let mut best = it.next().unwrap_or(0.0);
                        for x in it {
                            if x.abs() < best.abs() { best = x; }
                        }
                        best
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id,
                        glam::Vec2::new(pick(|v| v.x), pick(|v| v.y)))))
                }
                _ => {
                    let mut it = raw.iter().filter_map(|s| sig_to_f32(Some(*s)));
                    let mut best = it.next().unwrap_or(0.0);
                    for x in it {
                        if x.abs() < best.abs() { best = x; }
                    }
                    Some(Signal::Float(clamp_for_pin(pin.id, best)))
                }
            },
            "XOR" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let parity = raw.iter()
                        .filter(|s| matches!(s, Signal::Bool(true))).count() % 2 == 1;
                    Some(Signal::Bool(parity))
                }
                flexinput_core::SignalType::Vec2 => {
                    let fold = |sel: fn(&glam::Vec2) -> f32| -> f32 {
                        let xs: Vec<f32> = raw.iter().filter_map(|s| match s {
                            Signal::Vec2(v) => Some(sel(v)), _ => None
                        }).collect();
                        if xs.is_empty() { return 0.0; }
                        xs.iter().skip(1).fold(xs[0], |acc, &x| (acc - x).abs())
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id,
                        glam::Vec2::new(fold(|v| v.x), fold(|v| v.y)))))
                }
                _ => {
                    let xs: Vec<f32> = raw.iter().filter_map(|s| sig_to_f32(Some(*s))).collect();
                    let v = if xs.is_empty() { 0.0 }
                        else { xs.iter().skip(1).fold(xs[0], |acc, &x| (acc - x).abs()) };
                    Some(Signal::Float(clamp_for_pin(pin.id, v)))
                }
            },
            "ADD" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let any = raw.iter().any(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(any))
                }
                flexinput_core::SignalType::Vec2 => {
                    let sum = raw.iter().fold(glam::Vec2::ZERO, |acc, s| match s {
                        Signal::Vec2(v) => acc + *v, _ => acc
                    });
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id, sum)))
                }
                _ => {
                    let s: f32 = raw.iter().filter_map(|s| sig_to_f32(Some(*s))).sum();
                    Some(Signal::Float(clamp_for_pin(pin.id, s)))
                }
            },
            "MULT" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let all = raw.iter().all(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(all))
                }
                flexinput_core::SignalType::Vec2 => {
                    let first = match raw.first() {
                        Some(Signal::Vec2(v)) => *v,
                        _ => glam::Vec2::ZERO,
                    };
                    let mag_product = raw.iter().fold(1.0_f32, |acc, s| match s {
                        Signal::Vec2(v) => acc * v.length(),
                        _ => acc,
                    });
                    let dir = if first.length() > 0.0 {
                        first.normalize()
                    } else {
                        glam::Vec2::ZERO
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id, dir * mag_product)))
                }
                _ => {
                    let is_signed = !matches!(pin.id,
                        "left_trigger" | "right_trigger");
                    let nums: Vec<f32> = raw.iter()
                        .filter_map(|s| sig_to_f32(Some(*s))).collect();
                    let v = if is_signed {
                        let sign = nums.first().copied().unwrap_or(0.0);
                        let mag = nums.iter().fold(1.0_f32, |a, b| a * b.abs());
                        if sign < 0.0 { -mag } else { mag }
                    } else {
                        nums.iter().fold(1.0_f32, |a, b| a * b)
                    };
                    Some(Signal::Float(clamp_for_pin(pin.id, v)))
                }
            },
            _ => None,
        };
        if let Some(s) = resolved {
            collector_sigs.insert((key.clone(), pin.id.to_string()), s);
        }
    }

    // Off-spec pass-through (Remapper's keyboard/mouse pins etc.).
    {
        let mut extras: HashMap<String, Signal> = HashMap::new();
        for collector_id in input_collectors.iter().rev() {
            if collector_id.is_empty() { continue; }
            for ((dev, pin), &sig) in collector_sigs.iter() {
                if dev != collector_id { continue; }
                if automap::ALL_PINS.iter().any(|p| p.id == pin.as_str()) { continue; }
                extras.insert(pin.clone(), sig);
            }
        }
        for (pin, sig) in extras {
            let dest_key = (key.clone(), pin);
            if !collector_sigs.contains_key(&dest_key) {
                collector_sigs.insert(dest_key, sig);
            }
        }
    }
}

pub(crate) fn automap_selector_publish(
    snap: &NodeSnap,
    key_uid: usize,
    computed: &[Vec<Option<Signal>>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    fb_routes: &mut HashMap<String, String>,
) {
    let n_inputs = snap.input_sources.len().saturating_sub(1).max(1);
    let select = match snap.input_sources.get(0)
        .and_then(|src| src.and_then(|(si, op)| computed.get(si).and_then(|v| v.get(op)).copied().flatten()))
    {
        Some(Signal::Float(f)) => {
            let n = n_inputs as f32;
            ((f.clamp(0.0, 1.0) * n).floor() as usize).min(n_inputs - 1)
        }
        Some(Signal::Bool(b)) => if b { 1 } else { 0 },
        _ => 0,
    };
    let input_devs = snap.params.get("_automap_input_devs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let input_collectors = snap.params.get("_automap_input_collectors")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let selected_dev = input_devs.get(select).map(|s| s.as_str()).unwrap_or("").to_string();
    let selected_collector = input_collectors.get(select).map(|s| s.as_str()).unwrap_or("").to_string();
    let key = format!("forksel:{}:0", key_uid);
    // Record the reverse-feedback route: feedback injected at our OUTPUT id flows
    // back to whichever input we're currently gating from (its collector id if it
    // is one, else its raw device id). Empty when nothing is selected/wired.
    let route_to = if !selected_collector.is_empty() { &selected_collector } else { &selected_dev };
    if !route_to.is_empty() {
        fb_routes.insert(key.clone(), route_to.clone());
    }
    for pin in flexinput_core::automap::ALL_PINS {
        let sig = if !selected_collector.is_empty() {
            collector_sigs.get(&(selected_collector.clone(), pin.id.to_string())).copied()
                .or_else(|| {
                    if !selected_dev.is_empty() {
                        dev_sigs.get(&(selected_dev.clone(), pin.id.to_string())).copied()
                    } else { None }
                })
        } else if !selected_dev.is_empty() {
            dev_sigs.get(&(selected_dev.clone(), pin.id.to_string())).copied()
        } else {
            None
        };
        if let Some(sig) = sig {
            collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
        }
    }
    if !selected_collector.is_empty() {
        let copies: Vec<(String, Signal)> = collector_sigs.iter()
            .filter(|((d, p), _)| {
                d == &selected_collector
                    && !automap::ALL_PINS.iter().any(|ap| ap.id == p.as_str())
            })
            .map(|((_, p), s)| (p.clone(), *s))
            .collect();
        for (pin, sig) in copies {
            collector_sigs.insert((key.clone(), pin), sig);
        }
    }
}

// ── Sub-patch inner evaluation ────────────────────────────────────────────────
