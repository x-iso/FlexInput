//! Tuning the spike filter from a recording of a pad lying still.
//!
//! A pad at rest reads a flat line plus noise, so a spike is easy to name: a
//! sample far from the rolling median of its neighbours, by much more than
//! that axis's own noise ([`detect`]). [`tune`] then replays the real
//! [`SpikeFilter`] over the same recording to find the least it takes to
//! remove them all — the narrowest window first (each step costs a sample of
//! latency), then the lowest sensitivity (the less it rejects, the less it
//! can mistake real motion for a spike).
//!
//! The recording is what the filter itself is fed (`spike::capture_*`), so
//! the replay sees exactly what the live filter would.

use crate::spike::{SpikeFilter, AXES};

/// Rolling-median radius for the at-rest baseline: 17 samples, so a burst of
/// up to 8 can't drag it.
pub const BASE_R: usize = 8;
/// How far past the noise a sample must sit to count as a spike, in robust
/// standard deviations (1.4826 × the median absolute residual).
const SPIKE_SIGMA: f32 = 8.0;
/// ...and never less than this, normalized: 2 °/s on the gyro (±2000 °/s
/// full scale), 0.032 G on the accelerometer (±8 G). Smaller excursions
/// aren't worth a sample of filter latency.
const MIN_EXCURSION: [f32; AXES] = [0.001, 0.001, 0.001, 0.004, 0.004, 0.004];
/// Fewest samples worth tuning from.
pub const MIN_SAMPLES: usize = 64;
/// Sensitivity search step, %.
const SENS_STEP: f32 = 5.0;

/// The spikes in a recording at rest.
#[derive(Debug, Clone)]
pub struct Detection {
    /// Per sample: does any axis leave its baseline by more than `thresh`?
    pub flags: Vec<bool>,
    /// Per-axis spike threshold, normalized.
    pub thresh: [f32; AXES],
    /// Per-sample rolling-median baseline.
    pub baseline: Vec<[f32; AXES]>,
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Find the spikes in `samples`, a pad lying still.
pub fn detect(samples: &[[f32; AXES]]) -> Detection {
    let n = samples.len();
    let mut baseline = vec![[0.0f32; AXES]; n];
    let mut win: Vec<f32> = Vec::with_capacity(2 * BASE_R + 1);
    for i in 0..n {
        let span = &samples[i.saturating_sub(BASE_R)..(i + BASE_R + 1).min(n)];
        for a in 0..AXES {
            win.clear();
            win.extend(span.iter().map(|s| s[a]));
            baseline[i][a] = median(&mut win);
        }
    }
    let mut thresh = MIN_EXCURSION;
    let mut resid: Vec<f32> = Vec::with_capacity(n);
    for a in 0..AXES {
        resid.clear();
        resid.extend((0..n).map(|i| (samples[i][a] - baseline[i][a]).abs()));
        thresh[a] = (SPIKE_SIGMA * 1.4826 * median(&mut resid)).max(MIN_EXCURSION[a]);
    }
    let flags = (0..n)
        .map(|i| (0..AXES).any(|a| (samples[i][a] - baseline[i][a]).abs() > thresh[a]))
        .collect();
    Detection { flags, thresh, baseline }
}

/// What a recording held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpikeReport {
    pub samples: usize,
    /// Samples that are spikes.
    pub spikes: usize,
    /// Runs of consecutive spike samples.
    pub bursts: usize,
    /// The longest run.
    pub longest: usize,
}

impl SpikeReport {
    fn of(flags: &[bool]) -> Self {
        let (mut bursts, mut longest, mut run) = (0, 0, 0);
        for &f in flags {
            if f {
                run += 1;
                if run == 1 {
                    bursts += 1;
                }
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        Self { samples: flags.len(), spikes: flags.iter().filter(|f| **f).count(), bursts, longest }
    }
}

/// The outcome of [`tune`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub report: SpikeReport,
    /// `(window, sensitivity %)` to set, `None` when there is nothing to
    /// remove or no setting removes any of it.
    pub setting: Option<(u8, f32)>,
    /// Spike samples the setting leaves (0 = all removed).
    pub left: usize,
}

/// Spike samples left in `samples` after the filter at `(window, sens)`.
/// `warm` carries a noise estimate already settled on this recording, as the
/// live filter's is.
fn left_after(samples: &[[f32; AXES]], det: &Detection, warm: &SpikeFilter, window: u8, sens: f32) -> usize {
    let mut f = warm.clone();
    f.set_config(true, sens, window);
    f.reset();
    // Output k is input k − window/2 once the window has filled; before
    // that the filter holds the first sample.
    let w = window as usize;
    let mut left = 0;
    for (k, x) in samples.iter().enumerate() {
        let y = f.judge(*x);
        let j = if k + 1 >= w { k - w / 2 } else { 0 };
        if (0..AXES).any(|a| (y[a] - det.baseline[j][a]).abs() > det.thresh[a]) {
            left += 1;
        }
    }
    left
}

/// The least filter that removes the spikes in `samples`, a pad lying still:
/// for each window from 3 up, the lowest sensitivity that leaves none. When
/// no setting gets them all, the one that leaves fewest. `None` when the
/// recording is too short to judge.
pub fn tune(samples: &[[f32; AXES]]) -> Option<Tuning> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let det = detect(samples);
    let report = SpikeReport::of(&det.flags);
    if report.spikes == 0 {
        return Some(Tuning { report, setting: None, left: 0 });
    }
    let mut warm = SpikeFilter::new();
    warm.set_config(true, 50.0, 3);
    for x in samples {
        warm.judge(*x);
    }
    let mut best: Option<((u8, f32), usize)> = None;
    for window in [3u8, 5, 7] {
        let mut sens = 0.0;
        while sens <= 100.0 {
            let left = left_after(samples, &det, &warm, window, sens);
            if left == 0 {
                return Some(Tuning { report, setting: Some((window, sens)), left: 0 });
            }
            if best.is_none_or(|(_, b)| left < b) {
                best = Some(((window, sens), left));
            }
            sens += SENS_STEP;
        }
    }
    let (setting, left) = best?;
    if left >= report.spikes {
        return Some(Tuning { report, setting: None, left: report.spikes });
    }
    Some(Tuning { report, setting: Some(setting), left })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, roughly normal noise (σ = 1) for sample `i`, axis `a`.
    fn gauss(i: usize, a: usize) -> f32 {
        let mut x = ((i * AXES + a) as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mut sum = 0.0;
        for _ in 0..4 {
            x ^= x >> 33;
            x = x.wrapping_mul(0xff51_afd7_ed55_8ccd).wrapping_add(0x1234_5678);
            x ^= x >> 29;
            sum += (x >> 11) as f32 / (1u64 << 53) as f32;
        }
        (sum - 2.0) * 3.0f32.sqrt()
    }

    /// A pad at rest: a resting offset on every axis, and noise of σ =
    /// `gyro_sigma` on the gyro (0.06 °/s for the quiet pad).
    fn rest_with(n: usize, gyro_sigma: f32) -> Vec<[f32; AXES]> {
        (0..n)
            .map(|i| {
                let mut s = [0.0002, -0.0001, 0.0003, 0.0, 0.0, 0.125];
                for (a, v) in s.iter_mut().enumerate() {
                    *v += gauss(i, a) * if a < 3 { gyro_sigma } else { 0.0006 };
                }
                s
            })
            .collect()
    }

    fn rest(n: usize) -> Vec<[f32; AXES]> {
        rest_with(n, 0.00003)
    }

    fn spike(s: &mut [[f32; AXES]], at: usize, len: usize, axis: usize, amp: f32) {
        for x in s.iter_mut().skip(at).take(len) {
            x[axis] += amp;
        }
    }

    #[test]
    fn a_quiet_pad_has_nothing_to_tune() {
        let t = tune(&rest(1500)).unwrap();
        assert_eq!((t.report.spikes, t.setting), (0, None));
    }

    #[test]
    fn single_sample_spikes_take_the_narrowest_window() {
        let mut s = rest(1500);
        for at in [200, 520, 900, 1310] {
            spike(&mut s, at, 1, 2, 0.08); // 160 °/s for one sample
        }
        let t = tune(&s).unwrap();
        assert_eq!((t.report.spikes, t.report.longest), (4, 1));
        assert_eq!(t.left, 0);
        assert_eq!(t.setting.map(|(w, _)| w), Some(3));
    }

    #[test]
    fn two_sample_bursts_need_window_five() {
        let mut s = rest(1500);
        for at in [300, 700, 1100] {
            spike(&mut s, at, 2, 0, -0.05);
        }
        let t = tune(&s).unwrap();
        assert_eq!(t.report.longest, 2);
        assert_eq!((t.setting.map(|(w, _)| w), t.left), (Some(5), 0));
    }

    #[test]
    fn smaller_spikes_need_more_sensitivity() {
        // A noisier pad (σ 0.8 °/s): spikes count from ~6 °/s, and the
        // filter's least sensitive setting only takes them from ~7.5 °/s.
        let mut big = rest_with(1500, 0.0004);
        let mut small = rest_with(1500, 0.0004);
        for at in [250, 600, 1000] {
            spike(&mut big, at, 1, 1, 0.05);
            spike(&mut small, at, 1, 1, 0.0034); // 6.8 °/s
        }
        let (b, s) = (tune(&big).unwrap(), tune(&small).unwrap());
        assert_eq!((b.left, s.left), (0, 0));
        let sens = |t: Tuning| t.setting.unwrap().1;
        assert!(sens(s) > sens(b), "{s:?} vs {b:?}");
    }

    #[test]
    fn a_burst_too_long_to_repair_changes_nothing() {
        let mut s = rest(1500);
        spike(&mut s, 700, 6, 2, 0.05);
        let t = tune(&s).unwrap();
        assert_eq!(t.report.longest, 6);
        assert!(t.setting.is_none() || t.left < t.report.spikes, "{t:?}");
    }

    #[test]
    fn too_short_a_recording_is_not_judged() {
        assert!(tune(&rest(MIN_SAMPLES - 1)).is_none());
    }
}

