//! Streaming audio spectrum analyzer for the Audio Stream Haptics module.
//!
//! Feeds on the same captured render-mix samples as [`crate::haptic_pcm`], but
//! instead of one envelope+carrier it produces a **log-spaced magnitude band
//! spectrum** over the Switch Pro HD-rumble frequency range (~40–1250 Hz). That
//! spectrum is what the node's spectrum view draws and what the multi-band engine
//! maps onto Switch Pro carriers (one carrier per side after collapse).
//!
//! Pure DSP, no I/O. The FFT (`rustfft`) is pure Rust — it carries no `windows`
//! crate so it can't reintroduce the COM-major conflict the wasapi pin guards.

use std::sync::Arc;

use rustfft::{num_complex::Complex, Fft, FftPlanner};

/// FFT window length. 2048 @ 48 kHz ≈ 43 ms / ~23 Hz bin resolution — fine enough
/// to separate the low Switch Pro band (40–80 Hz) while staying cheap to run a few
/// hundred times a second.
pub const FFT_SIZE: usize = 2048;

/// Assumed capture sample rate (the loopback client is initialized at 48 kHz, and
/// the system-loopback render mix is 48 kHz on virtually all modern endpoints).
pub const SAMPLE_RATE_HZ: f32 = 48_000.0;

/// Number of log-spaced output bands across the haptic range. Doubles as the bar
/// count in the spectrum view and the band count the multi-band engine maps.
pub const N_BANDS: usize = 32;

/// Haptic frequency range the bands span — the Switch Pro HD-rumble usable band.
pub const BAND_MIN_HZ: f32 = 40.0;
pub const BAND_MAX_HZ: f32 = 1253.0;

/// A streaming spectrum analyzer: push samples, read the latest log-band spectrum.
pub struct SpectrumAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    /// Hann window, precomputed.
    window: Vec<f32>,
    /// Sliding input ring of the most recent `FFT_SIZE` mono samples.
    ring: Vec<f32>,
    pos: usize,
    filled: usize,
    /// Scratch FFT buffer (reused each transform).
    scratch: Vec<Complex<f32>>,
    /// Lower/upper FFT-bin index for each output band (precomputed from the log
    /// edges), so binning is a couple of slice sums per band.
    band_bins: Vec<(usize, usize)>,
    /// Latest computed band magnitudes (0..1-ish, normalized), held between reads.
    bands: [f32; N_BANDS],
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|n| {
                // Hann window.
                let x = std::f32::consts::PI * 2.0 * n as f32 / FFT_SIZE as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        let band_bins = compute_band_bins();
        Self {
            fft,
            window,
            ring: vec![0.0; FFT_SIZE],
            pos: 0,
            filled: 0,
            scratch: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            band_bins,
            bands: [0.0; N_BANDS],
        }
    }

    /// Push one mono sample (e.g. `(l+r)/2`) into the sliding window.
    #[inline]
    pub fn push(&mut self, s: f32) {
        self.ring[self.pos] = s;
        self.pos = (self.pos + 1) % FFT_SIZE;
        if self.filled < FFT_SIZE {
            self.filled += 1;
        }
    }

    /// Recompute the band spectrum from the current window and return it. Cheap to
    /// call a few hundred Hz; returns the last spectrum unchanged until the window
    /// has filled once.
    pub fn compute(&mut self) -> [f32; N_BANDS] {
        if self.filled < FFT_SIZE {
            return self.bands;
        }
        // Linearize the ring (oldest → newest) into the complex scratch, windowed.
        for k in 0..FFT_SIZE {
            let s = self.ring[(self.pos + k) % FFT_SIZE] * self.window[k];
            self.scratch[k] = Complex::new(s, 0.0);
        }
        self.fft.process(&mut self.scratch);

        // Magnitude per FFT bin (only the first half is meaningful for real input).
        // Bucket into log bands by summing bin magnitudes, then normalize.
        let norm = 2.0 / FFT_SIZE as f32; // single-sided amplitude scale
        let mut raw = [0.0f32; N_BANDS];
        for (b, &(lo, hi)) in self.band_bins.iter().enumerate() {
            let mut acc = 0.0f32;
            for bin in lo..=hi {
                acc += self.scratch[bin].norm() * norm;
            }
            // Average over the bins in the band so wide high bands aren't inflated.
            let count = (hi - lo + 1).max(1) as f32;
            raw[b] = acc / count;
        }
        self.bands = raw;
        self.bands
    }

    /// The most recently computed band spectrum without recomputing.
    pub fn latest(&self) -> [f32; N_BANDS] {
        self.bands
    }
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Center frequency (Hz) of output band `b`, log-spaced across the haptic range.
pub fn band_center_hz(b: usize) -> f32 {
    let t = (b as f32 + 0.5) / N_BANDS as f32;
    BAND_MIN_HZ * (BAND_MAX_HZ / BAND_MIN_HZ).powf(t)
}

/// Normalized position (0..1) of band `b`'s center, matching the `*_freq` pin /
/// Switch Pro carrier convention so the engine can map a band straight to a carrier.
pub fn band_center_norm(b: usize) -> f32 {
    (b as f32 + 0.5) / N_BANDS as f32
}

/// Precompute the FFT-bin span [lo, hi] for each log-spaced output band.
fn compute_band_bins() -> Vec<(usize, usize)> {
    let hz_per_bin = SAMPLE_RATE_HZ / FFT_SIZE as f32;
    let nyquist_bin = FFT_SIZE / 2;
    let edge_hz = |i: usize| -> f32 {
        let t = i as f32 / N_BANDS as f32;
        BAND_MIN_HZ * (BAND_MAX_HZ / BAND_MIN_HZ).powf(t)
    };
    (0..N_BANDS)
        .map(|b| {
            let lo_hz = edge_hz(b);
            let hi_hz = edge_hz(b + 1);
            let lo = ((lo_hz / hz_per_bin).floor() as usize).max(1).min(nyquist_bin);
            let hi = ((hi_hz / hz_per_bin).ceil() as usize).max(lo).min(nyquist_bin);
            (lo, hi)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Push `cycles` worth of a pure sine at `hz` and return the band spectrum.
    fn analyze_sine(hz: f32, amp: f32) -> [f32; N_BANDS] {
        let mut a = SpectrumAnalyzer::new();
        // Fill several windows so the sliding buffer is fully primed.
        for n in 0..(FFT_SIZE * 3) {
            let t = n as f32 / SAMPLE_RATE_HZ;
            a.push((TAU * hz * t).sin() * amp);
        }
        a.compute()
    }

    fn dominant_band(bands: &[f32; N_BANDS]) -> usize {
        bands.iter().enumerate().max_by(|x, y| x.1.partial_cmp(y.1).unwrap()).unwrap().0
    }

    #[test]
    fn band_edges_are_monotonic_and_in_range() {
        for b in 0..N_BANDS {
            let c = band_center_hz(b);
            assert!(c >= BAND_MIN_HZ && c <= BAND_MAX_HZ, "band {b} center {c} out of range");
            if b > 0 {
                assert!(band_center_hz(b) > band_center_hz(b - 1), "bands must ascend");
            }
        }
    }

    #[test]
    fn pure_tone_peaks_in_the_right_band() {
        // A 160 Hz tone should peak in the band whose center is nearest 160 Hz.
        let bands = analyze_sine(160.0, 0.8);
        let dom = dominant_band(&bands);
        let dom_hz = band_center_hz(dom);
        assert!((dom_hz - 160.0).abs() < 60.0, "160 Hz peaked at band center {dom_hz} Hz");
    }

    #[test]
    fn higher_tone_peaks_in_a_higher_band() {
        let lo = dominant_band(&analyze_sine(120.0, 0.8));
        let hi = dominant_band(&analyze_sine(600.0, 0.8));
        assert!(hi > lo, "600 Hz (band {hi}) should sit above 120 Hz (band {lo})");
    }

    #[test]
    fn silence_is_flat_zero() {
        let mut a = SpectrumAnalyzer::new();
        for _ in 0..(FFT_SIZE * 2) {
            a.push(0.0);
        }
        let bands = a.compute();
        assert!(bands.iter().all(|&m| m < 1e-4), "silence should give ~0 across bands");
    }
}
