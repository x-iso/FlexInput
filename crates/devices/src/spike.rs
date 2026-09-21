//! Outlier spike suppression for raw IMU streams.
//!
//! Sits at the device-poll boundary, between a backend's packet decode and
//! the signals it pushes to the engine. Every backend with a raw IMU stream
//! owns one filter per device; the UI pushes settings down each I/O tick via
//! `DeviceBackend::set_spike_filter`.
//!
//! # What it catches
//!
//! Single- and multi-packet excursions — a sample (or short run of samples)
//! that leaves the local trajectory and snaps straight back. These come from
//! transport glitches and sensor readout faults, not from motion, and they
//! are indistinguishable from a real flick to anything downstream: an aim
//! mapping integrates one and the view jumps.
//!
//! # Two rules, both of which must fire
//!
//! A sample is rejected only when it is BOTH
//!
//!   1. far from where its neighbours say it should be — farther than
//!      `sensitivity`-scaled multiple of the axis's own measured noise, and
//!   2. not corroborated by its neighbours — the neighbours are mutually
//!      consistent, so the excursion is the sample's alone.
//!
//! Rule 2 is what keeps real motion intact. During a genuine fast sweep every
//! sample is far from the window median, but so are its neighbours, so the
//! spread is wide and nothing is rejected. During a spike at rest the
//! neighbours agree with each other and only the centre sample disagrees.
//!
//! # Why the noise estimate is measured, not assumed
//!
//! The threshold in rule 1 is a multiple of a *per-axis, continuously
//! measured* noise level, not of a constant. Quiescent jitter spans well over
//! an order of magnitude across controllers — a pad whose real floor is 3e-4
//! and one whose floor is 5e-3 are both ordinary — so any fixed constant is
//! simultaneously far too aggressive for one and completely blind on the
//! other. A previous version of this filter used a hardcoded 0.003, which
//! put every spike below that amplitude out of reach at *every* sensitivity
//! setting, making the slider inert on quiet devices.
//!
//! The estimator tracks the LOW envelope of the per-sample residual: it falls
//! quickly and rises slowly, so motion and spikes barely move it while the
//! resting noise floor pulls it down. It is also clamped against its own
//! current value before updating, so a spike can never inflate it, and it
//! updates on every sample (accepted or not) so it can never deadlock at a
//! value that rejects everything.

use std::collections::VecDeque;

/// Axis count: gyro x/y/z then accel x/y/z.
pub const AXES: usize = 6;

/// Smallest threshold base, in normalized units. Roughly one LSB of a 16-bit
/// sensor at full scale (1/32767), so a perfectly silent stream can't drive
/// the threshold to zero and start rejecting quantization steps.
const ABS_MIN: f32 = 3.1e-5;

/// Initial noise estimate before anything is measured. Deliberately on the
/// high side — over-filtering a fresh stream is worse than under-filtering
/// it, and the fast fall rate reaches a quiet device's true floor within a
/// few dozen samples.
const NOISE_SEED: f32 = 3e-3;

/// EMA rate when the residual is BELOW the current estimate. Fast, so the
/// estimate tracks down to the resting floor promptly on connect.
const FALL: f32 = 0.05;
/// EMA rate when the residual is ABOVE it. Deliberately 10x slower, so the
/// estimate follows the low envelope of the residual rather than its mean —
/// motion bursts and spikes lift it only slightly.
const RISE: f32 = 0.005;

/// How far above the current estimate a single residual is allowed to count
/// when updating it. Bounds the influence of any one sample, so a spike
/// cannot teach the filter that spikes are normal.
const UPDATE_CLAMP: f32 = 4.0;

/// Neighbour-consistency factor for rule 2: the centre sample's deviation
/// must exceed this multiple of the neighbours' robust spread.
///
/// 2.0 is not arbitrary. At `window == 3` the two neighbours' median absolute
/// deviation is exactly half the gap between them, so `deviation > 2 * mad`
/// reduces to `deviation > |n - a|` — the original hand-tuned 3-tap rule,
/// preserved exactly as the degenerate case of the general one.
const GATE_K: f32 = 2.0;

/// Widest supported window. Bounds the fixed-size scratch buffers below.
pub const MAX_WINDOW: usize = 7;

/// Clamp an arbitrary window request to a supported odd width in 3..=7.
pub fn sanitize_window(w: u8) -> u8 {
    let w = w.clamp(3, MAX_WINDOW as u8);
    if w % 2 == 0 { w + 1 } else { w }
}

/// Map 0..100 % sensitivity to a multiple of the measured per-axis noise
/// floor. Log-spaced so the slider's feel is even across its travel:
///
///   0 %   → 24x  (only gross excursions)
///   25 %  → 13x
///   50 %  → 8.5x
///   75 %  → 4.8x
///   100 % → 3x   (just clear of ordinary noise peaks)
///
/// The floor of 3 is chosen against the estimator's own units: it tracks mean
/// absolute residual, which for Gaussian noise is ~0.8σ, so 3x lands near
/// 3.75σ — above essentially every real noise peak, below any true spike.
pub fn sensitivity_to_multiplier(sensitivity_pct: f32) -> f32 {
    let s = (sensitivity_pct / 100.0).clamp(0.0, 1.0);
    let log_hi = 3.0_f32.ln();
    let log_lo = 24.0_f32.ln();
    (log_lo + (log_hi - log_lo) * s).exp()
}

/// Median of a small slice. Sorts in place; `buf` is caller-owned scratch.
/// For an even count this averages the two middle values, which is what makes
/// the 3-tap case collapse to the midpoint of the two neighbours.
fn median_of(buf: &mut [f32]) -> f32 {
    buf.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = buf.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        buf[n / 2]
    } else {
        0.5 * (buf[n / 2 - 1] + buf[n / 2])
    }
}

/// Per-device outlier filter over the six IMU axes.
///
/// Holds a window of the last `window` raw samples and judges the CENTRE one
/// against the median of the rest, which is why output lags input by
/// `window / 2` samples. A rejected sample is replaced by that median and
/// written back into the window, so a burst can't poison the windows that
/// follow it.
///
/// Width sets how long a burst can be repaired: a window of `W` tolerates
/// `(W - 1) / 2` contaminated samples, so 3 catches isolated spikes only,
/// 5 catches bursts up to 2 samples, 7 up to 3. Wider costs latency in
/// direct proportion, which is why it is a user-facing setting rather than a
/// constant — on a 250 Hz pad the difference between 3 and 7 is 4 ms of
/// added aim delay.
#[derive(Debug, Clone)]
pub struct SpikeFilter {
    enabled: bool,
    sensitivity: f32,
    window: usize,
    /// Raw sample history, newest at the back. Never longer than `window`.
    ring: VecDeque<[f32; AXES]>,
    /// Per-axis running noise floor, in normalized units.
    noise: [f32; AXES],
}

impl Default for SpikeFilter {
    fn default() -> Self {
        Self {
            enabled: true,
            sensitivity: 50.0,
            window: 3,
            ring: VecDeque::with_capacity(MAX_WINDOW),
            noise: [NOISE_SEED; AXES],
        }
    }
}

impl SpikeFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current settings, for change detection by callers that want to avoid
    /// touching the filter when nothing moved.
    pub fn config(&self) -> (bool, f32, u8) {
        (self.enabled, self.sensitivity, self.window as u8)
    }

    /// Apply UI settings. Cheap and idempotent: returns without touching
    /// state when nothing changed, so callers can call it every I/O tick.
    ///
    /// Changing the window or disabling the filter clears the history — a
    /// window that straddles a width change would judge a centre sample
    /// against the wrong neighbours. The noise estimate is deliberately
    /// KEPT across such a reset, since it describes the device, not the
    /// window, and re-converging it on every slider nudge would make the
    /// filter briefly blind each time.
    pub fn set_config(&mut self, enabled: bool, sensitivity_pct: f32, window: u8) {
        let s = sensitivity_pct.clamp(0.0, 100.0);
        let w = sanitize_window(window) as usize;
        let changed = self.enabled != enabled
            || (self.sensitivity - s).abs() > f32::EPSILON
            || self.window != w;
        if !changed {
            return;
        }
        self.enabled = enabled;
        self.sensitivity = s;
        if self.window != w || !enabled {
            self.window = w;
            self.ring.clear();
        }
    }

    /// Drop all history. Call when a device's stream breaks (reconnect,
    /// transport stall) so stale samples can't be judged against fresh ones.
    pub fn reset(&mut self) {
        self.ring.clear();
    }

    /// Feed one raw sample; get back the sample to publish.
    ///
    /// Exactly one output per input, so the stream's rate is unchanged — but
    /// the value returned is the one from `window / 2` samples ago. While the
    /// window is still filling, the oldest buffered sample is repeated, which
    /// costs a brief hold at connect and nothing after.
    pub fn push(&mut self, sample: [f32; AXES]) -> [f32; AXES] {
        if !self.enabled {
            if !self.ring.is_empty() {
                self.ring.clear();
            }
            return sample;
        }

        self.ring.push_back(sample);
        if self.ring.len() < self.window {
            // Warm-up: hold the oldest sample until we can judge a centre.
            return self.ring[0];
        }

        let centre = self.window / 2;
        let multiplier = sensitivity_to_multiplier(self.sensitivity);
        let mut out = self.ring[centre];

        for axis in 0..AXES {
            // Neighbours of the centre sample, in window order.
            let mut neigh = [0.0_f32; MAX_WINDOW - 1];
            let mut n = 0;
            for (i, s) in self.ring.iter().enumerate() {
                if i != centre {
                    neigh[n] = s[axis];
                    n += 1;
                }
            }
            let neigh = &mut neigh[..n];

            // Rule 1 reference: where the neighbours say the centre should be.
            let mut scratch = [0.0_f32; MAX_WINDOW - 1];
            scratch[..n].copy_from_slice(neigh);
            let reference = median_of(&mut scratch[..n]);
            let deviation = (self.ring[centre][axis] - reference).abs();

            // Rule 2 reference: how much the neighbours disagree among
            // THEMSELVES. Median-absolute-deviation rather than peak spread,
            // so one contaminated neighbour inside a burst doesn't mask the
            // burst it belongs to.
            let mut devs = [0.0_f32; MAX_WINDOW - 1];
            for (i, v) in neigh.iter().enumerate() {
                devs[i] = (v - reference).abs();
            }
            let spread = median_of(&mut devs[..n]);

            let thresh = multiplier * self.noise[axis].max(ABS_MIN);
            let is_spike = deviation > thresh && deviation > GATE_K * spread;

            if is_spike {
                out[axis] = reference;
                // Write the repair back so this sample can't drag the
                // windows that follow it.
                self.ring[centre][axis] = reference;
            }

            // Update the noise estimate from this sample's residual, bounded
            // so a spike can't teach it that spikes are normal. Runs whether
            // or not the sample was rejected, so the estimate can always
            // recover even if the threshold is momentarily far too low.
            let cur = self.noise[axis];
            let d = deviation.min(cur * UPDATE_CLAMP + ABS_MIN);
            let rate = if d < cur { FALL } else { RISE };
            self.noise[axis] = (cur + rate * (d - cur)).max(ABS_MIN);
        }

        self.ring.pop_front();
        out
    }

    /// Current measured noise floor per axis, in normalized units. Exposed
    /// for diagnostics — it is the number the threshold is actually built
    /// from, so it's the one worth showing when a filter looks wrong.
    pub fn noise_floor(&self) -> [f32; AXES] {
        self.noise
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a filter with a scalar signal on gyro_x and collect gyro_x out.
    fn run(samples: &[f32], sensitivity: f32, window: u8) -> Vec<f32> {
        let mut f = SpikeFilter::new();
        f.set_config(true, sensitivity, window);
        samples
            .iter()
            .map(|&v| {
                let mut s = [0.0; AXES];
                s[0] = v;
                f.push(s)[0]
            })
            .collect()
    }

    /// Settle the noise estimate on a quiet signal, then add a spike run.
    fn quiet_with_burst(width: usize, amp: f32) -> Vec<f32> {
        let mut s = vec![0.0_f32; 400];
        for (i, v) in s.iter_mut().enumerate() {
            // Deterministic sub-LSB dither so the estimate has something to
            // measure without any RNG dependency in the test.
            *v = if i % 2 == 0 { 1e-5 } else { -1e-5 };
        }
        for v in s.iter_mut().skip(300).take(width) {
            *v = amp;
        }
        s
    }

    /// The regression this filter was rewritten for: a spike well below the
    /// old hardcoded 0.003 floor, on a pad whose real noise is ~1e-5.
    #[test]
    fn catches_spike_far_below_the_old_hardcoded_floor() {
        let out = run(&quiet_with_burst(1, 0.0028), 100.0, 3);
        let peak = out[290..].iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(peak < 1e-3, "spike survived at 100% sensitivity: peak {peak}");
    }

    /// And the slider must actually do something across its travel. Sized
    /// deliberately: on this near-silent signal the estimate sits at
    /// `ABS_MIN`, so 100 % admits excursions past ~3x it and 0 % past ~24x.
    /// A spike between those two is the only kind the slider can decide —
    /// a grosser one (like the 0.0028 above) is caught at every setting,
    /// which is the point of the range, not a failure of it.
    #[test]
    fn sensitivity_spans_a_useful_range() {
        let sig = quiet_with_burst(1, 3e-4);
        let peak = |s: f32| run(&sig, s, 3)[290..].iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        let aggressive = peak(100.0);
        let permissive = peak(0.0);
        assert!(aggressive < 1e-4, "100% should suppress: {aggressive}");
        assert!(permissive > 2e-4, "0% should let it through: {permissive}");
    }

    /// A 3-tap window structurally cannot repair a 2-wide burst; a 5-tap can.
    #[test]
    fn wider_windows_repair_wider_bursts() {
        let sig = quiet_with_burst(2, 0.0028);
        let w3 = run(&sig, 100.0, 3)[290..].iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        let w5 = run(&sig, 100.0, 5)[290..].iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(w3 > 1e-3, "3-tap should miss a 2-wide burst: {w3}");
        assert!(w5 < 1e-3, "5-tap should catch a 2-wide burst: {w5}");
    }

    /// Real motion must survive. A steady ramp is far from any fixed point
    /// but its neighbours agree with it, so rule 2 must let every sample pass.
    #[test]
    fn steady_motion_passes_through_untouched() {
        let sig: Vec<f32> = (0..400).map(|i| i as f32 * 1e-3).collect();
        for w in [3_u8, 5, 7] {
            let out = run(&sig, 100.0, w);
            let lag = (w / 2) as usize;
            for i in 200..sig.len() - lag {
                let expected = sig[i];
                let got = out[i + lag];
                assert!(
                    (got - expected).abs() < 1e-6,
                    "window {w} altered motion at {i}: {got} vs {expected}"
                );
            }
        }
    }

    /// Deterministic LCG in 0..1, so the noise tests carry no rng dependency.
    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1 << 24) as f32
    }

    /// Ordinary resting noise must pass through essentially intact. A filter
    /// that quietly smooths the noise floor is also eating fine motion detail,
    /// which is worse than the spikes it was added to remove — so this pins
    /// the energy of a pure-noise stream, not just its peak.
    #[test]
    fn resting_noise_is_not_smoothed_away() {
        let mut seed = 12_345_u32;
        // Sum of 4 uniforms ≈ Gaussian, scaled to a realistic 3e-4 floor.
        let sig: Vec<f32> = (0..2000)
            .map(|_| {
                let s: f32 = (0..4).map(|_| lcg(&mut seed) - 0.5).sum();
                s * 3e-4
            })
            .collect();
        let out = run(&sig, 100.0, 3);
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        // Compare only the settled tail, past the estimator's convergence.
        let a = rms(&sig[1000..]);
        let b = rms(&out[1000..]);
        assert!(
            b > a * 0.8,
            "filter smoothed the noise floor: rms {b:.3e} vs input {a:.3e}"
        );
    }

    /// The accelerometer sits at ~1.0 on whichever axis gravity loads, so the
    /// filter must judge residuals, not magnitudes — a large DC offset must
    /// not make an axis look permanently out of range.
    #[test]
    fn dc_offset_does_not_confuse_the_estimator() {
        let mut seed = 999_u32;
        let mut sig: Vec<f32> = (0..600)
            .map(|_| 1.0 + (lcg(&mut seed) - 0.5) * 6e-5)
            .collect();
        sig[500] = 1.0 + 3e-3; // a spike riding on top of gravity
        let out = run(&sig, 100.0, 3);
        assert!(
            (out[501] - 1.0).abs() < 5e-4,
            "spike on a DC-offset axis survived: {}",
            out[501]
        );
        // And the quiet samples around it are still ~1.0, not dragged off.
        assert!((out[400] - 1.0).abs() < 1e-3, "DC level drifted: {}", out[400]);
    }

    /// Disabled means bit-exact pass-through, not "a very permissive filter".
    #[test]
    fn disabled_is_transparent() {
        let sig = quiet_with_burst(1, 0.0028);
        let mut f = SpikeFilter::new();
        f.set_config(false, 100.0, 5);
        for &v in &sig {
            let mut s = [0.0; AXES];
            s[0] = v;
            assert_eq!(f.push(s)[0], v);
        }
    }

    /// Output count must equal input count, or the stream's rate changes.
    #[test]
    fn one_output_per_input() {
        for w in [3_u8, 5, 7] {
            let sig = quiet_with_burst(1, 0.0028);
            assert_eq!(run(&sig, 50.0, w).len(), sig.len());
        }
    }

    /// The 3-tap case must still be the original hand-tuned rule: deviation
    /// beyond the gap between the two neighbours.
    #[test]
    fn three_tap_reduces_to_the_original_neighbour_rule() {
        // Neighbours 0.0 and 0.2 (gap 0.2, so the rule 2 bar is 0.2 above the
        // 0.1 midpoint). 0.25 is only 0.15 off the midpoint — corroborated
        // motion, must pass. 0.5 is 0.4 off — must be rejected.
        let mut f = SpikeFilter::new();
        f.set_config(true, 100.0, 3);
        let feed = |f: &mut SpikeFilter, v: f32| {
            let mut s = [0.0; AXES];
            s[0] = v;
            f.push(s)[0]
        };
        feed(&mut f, 0.0);
        feed(&mut f, 0.25);
        assert!((feed(&mut f, 0.2) - 0.25).abs() < 1e-6, "corroborated motion was rejected");

        let mut f = SpikeFilter::new();
        f.set_config(true, 100.0, 3);
        feed(&mut f, 0.0);
        feed(&mut f, 0.5);
        assert!((feed(&mut f, 0.2) - 0.1).abs() < 1e-6, "uncorroborated spike was kept");
    }
}
