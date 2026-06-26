//! DualSense HD-haptic PCM → (amplitude, frequency) conversion.
//!
//! A game drives DualSense HD haptics by streaming PCM to the controller's USB
//! audio render endpoint: **4 channels, 48 kHz, S16 LE, interleaved**, where
//! `ch0/1` = headphone L/R and **`ch2`/`ch3` = left/right LRA (haptic) waveforms**
//! (verified against awalol/DS5Dongle `src/audio.cpp`, MIT). The actuator signal
//! is therefore a time-domain waveform — NOT an `(amp, freq)` pair.
//!
//! FlexInput's haptic pins (`ds_l/r_amp` + `ds_l/r_freq`, and the Switch Pro HD
//! Rumble sink `hd_l/r_amp` + `hd_l/r_freq`) are *parametric*. This module bridges
//! the two: it runs **envelope detection** (RMS → amplitude) and **pitch detection**
//! (autocorrelation → carrier frequency) over each LRA channel's samples, so a
//! game's HD-haptic waveform can drive the parametric sinks (Switch Pro HD Rumble,
//! or a physical DualSense's LRAs).
//!
//! Pure DSP, no I/O — the audio bytes arrive from the virtual-DualSense driver's
//! audio-OUT ring (see the UDE driver path). Unit-tested in isolation.

/// Source PCM format constants for the DualSense audio endpoint.
pub const CHANNELS: usize = 4;
pub const SAMPLE_RATE_HZ: f32 = 48_000.0;
/// Index of the left / right LRA (haptic) channels within each interleaved frame.
pub const LRA_L_CH: usize = 2;
pub const LRA_R_CH: usize = 3;

/// Frequency band the parametric pins map onto. The DualSense LRAs resonate around
/// ~160–200 Hz; the useful band is ~80–500 Hz. We report the detected carrier
/// normalized into `[0,1]` over this band so it lines up with the `ds_*_freq` pin
/// convention (0 = 80 Hz, 1 = 500 Hz; see `dualsense_haptic.rs`).
pub const FREQ_MIN_HZ: f32 = 80.0;
pub const FREQ_MAX_HZ: f32 = 500.0;

/// LRA resonance, normalized into the band above. Used as the default carrier when
/// the signal is a transient/impulse with no resolvable pitch (a menu click). A
/// DualSense impulse physically plays at the actuator's resonance (~180 Hz), so
/// that's the natural frequency to fall back to rather than 0 (80 Hz) or a spurious
/// max. (180-80)/(500-80) ≈ 0.238.
const RESONANCE_NORM: f32 = 0.238;

/// One channel's running pitch/envelope detector. Holds a small sample history so
/// autocorrelation has enough span to resolve the lowest frequency in-band even
/// when the driver delivers tiny ISO packets (a 48 kHz / 80 Hz period is 600
/// samples, so we keep a window at least that long).
#[derive(Debug, Clone)]
pub struct ChannelDetector {
    /// Ring of recent normalized samples (`-1.0..1.0`). Used for *pitch* detection
    /// only — amplitude comes from the peak-envelope follower below.
    history: Vec<f32>,
    /// Write cursor into `history`.
    pos: usize,
    /// How many valid samples have been written (saturates at capacity).
    filled: usize,
    /// Last detected normalized frequency (held when amplitude is below the
    /// noise floor so a silent gap doesn't snap frequency to 0 and click).
    last_freq_norm: f32,
    /// Peak-envelope follower (fast attack, slow release). HD-haptic effects are
    /// dominated by short transients (a menu click, an impact) only a few ms long;
    /// a plain window-RMS averages those down to near-nothing so they never get
    /// felt. The envelope instead jumps to each transient's peak and decays
    /// smoothly, so a 2 ms click still produces a meaningful amplitude.
    env: f32,
    /// Mean-square follower (smoothed `s²`). Used by the **audio-loopback** path's
    /// [`loudness`](Self::loudness): a full game mix peak-pins the peak envelope at
    /// ~1.0 constantly (every loud frame hits full scale), leaving amplitude with no
    /// dynamic range. Tracking mean-square instead follows *intensity* (quiet dialog
    /// vs an explosion) with a wide range that the volume control can actually scale.
    /// Both attack and release are slow-ish so it reads loudness, not transient peaks.
    ms: f32,
    /// Per-sample smoothing coefficient for `ms` when the level is *rising* (attack).
    /// Fixed: loudness should react reasonably quickly to a new sound.
    ms_attack: f32,
    /// Per-sample smoothing coefficient for `ms` when *falling* (release). Runtime
    /// adjustable (the node's Release slider) so the user can dial how fast rumble
    /// fades after a sound. Larger = faster decay.
    ms_release: f32,
}

/// Window length: enough to cover ~1.5 periods of the lowest in-band tone at
/// 48 kHz (600 samples / period @ 80 Hz). 1024 keeps autocorrelation cheap and
/// resolves the whole band.
const WINDOW: usize = 1024;

/// Below this envelope the channel is treated as silent: amplitude reports 0 and
/// pitch is frozen (not recomputed from noise). ~ -54 dBFS.
const SILENCE_RMS: f32 = 0.002;

/// Noise-floor expander knee. Any constant background (game music, ambience,
/// the render-mix dither) sits at a low but non-zero envelope; reported raw it
/// becomes a steady idle buzz on the actuator. We subtract a floor and rescale
/// so only content *above* the floor produces amplitude — quiet steady mixes map
/// to ~0, transients/loud passages still reach full scale. ~ -34 dBFS knee.
const NOISE_FLOOR: f32 = 0.02;

/// Envelope attack coefficient per sample (toward a louder instantaneous peak).
/// Near-instant so a sharp transient is captured at its true height: at 48 kHz a
/// coefficient of 0.5 reaches ~99% of a step in ~7 samples (~0.15 ms).
const ENV_ATTACK: f32 = 0.5;
/// Envelope release coefficient per sample (decay when the signal falls). Chosen so
/// the envelope falls ~one e-fold in ~12 ms (48k * 12ms ≈ 576 samples → 1 - 1/576),
/// long enough that a transient stays felt but short enough to track the effect's
/// own shape rather than smearing distinct events together.
const ENV_RELEASE: f32 = 0.00174;

/// Mean-square ATTACK coefficient (per sample) for [`ChannelDetector::loudness`].
/// ~12 ms time constant: loudness rises fairly quickly to a new sound so impacts
/// are felt promptly, without tracking individual sample peaks.
const MS_ATTACK: f32 = 0.0017;
/// Default mean-square RELEASE coefficient (per sample). ~30 ms time constant
/// (48k * 30ms ≈ 1440 → 1/1440). Overridden at runtime by the node's Release
/// slider via [`ChannelDetector::set_release_ms`]; this is the cold-start value.
const MS_RELEASE_DEFAULT: f32 = 0.0007;
/// Convert a release time in milliseconds to a per-sample one-pole coefficient at
/// the 48 kHz capture rate: `coeff = 1 - exp(-1 / (ms * 48))`, clamped sane.
pub fn release_ms_to_coeff(ms: f32) -> f32 {
    let ms = ms.clamp(1.0, 1000.0);
    let samples = ms * (SAMPLE_RATE_HZ / 1000.0);
    (1.0 - (-1.0 / samples).exp()).clamp(1.0e-5, 1.0)
}

/// Reference RMS that maps to full haptic scale before the volume control. A game
/// mix mastered near full scale sits around -12…-18 dBFS RMS in loud passages; we
/// map RMS ≈ 0.25 (-12 dBFS) to 1.0 so loud action reaches full rumble at volume 1,
/// and quiet passages spread across the lower range (giving volume real headroom).
const LOUDNESS_REF_RMS: f32 = 0.25;

impl Default for ChannelDetector {
    fn default() -> Self {
        Self {
            history: vec![0.0; WINDOW],
            pos: 0,
            filled: 0,
            // Default to the LRA resonance so an impulse with no resolvable carrier
            // plays at a natural haptic frequency instead of 80 Hz / a spurious max.
            last_freq_norm: RESONANCE_NORM,
            env: 0.0,
            ms: 0.0,
            ms_attack: MS_ATTACK,
            ms_release: MS_RELEASE_DEFAULT,
        }
    }
}

impl ChannelDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one normalized sample (`-1.0..1.0`) into the window and advance the
    /// peak-envelope follower.
    #[inline]
    pub fn push(&mut self, s: f32) {
        self.history[self.pos] = s;
        self.pos = (self.pos + 1) % WINDOW;
        if self.filled < WINDOW {
            self.filled += 1;
        }
        // Fast-attack / slow-release peak envelope on the rectified sample.
        let mag = s.abs();
        let coeff = if mag > self.env { ENV_ATTACK } else { ENV_RELEASE };
        self.env += (mag - self.env) * coeff;
        // Smoothed mean-square for the loudness (audio-loopback) path, with a fixed
        // attack and a runtime-adjustable release (the node's Release slider).
        let sq = s * s;
        let coeff = if sq > self.ms { self.ms_attack } else { self.ms_release };
        self.ms += (sq - self.ms) * coeff;
    }

    /// Set the loudness follower's release time in milliseconds (audio-loopback
    /// path). Cheap; safe to call every tick from the capture loop.
    #[inline]
    pub fn set_release_ms(&mut self, ms: f32) {
        self.ms_release = release_ms_to_coeff(ms);
    }

    /// Current amplitude — the peak envelope, noise-floor expanded and clamped to
    /// `[0,1]`. Unlike a window RMS this preserves short transients (HD-haptic
    /// clicks/impacts) at their true height instead of averaging them toward zero.
    ///
    /// A noise-floor expander removes the steady idle buzz a constant low-level
    /// mix would otherwise produce: anything at or below [`NOISE_FLOOR`] reports a
    /// hard 0, and the remaining range is rescaled back to full `[0,1]` so loud
    /// content still reaches the top of the range.
    pub fn amplitude(&self) -> f32 {
        let e = self.env;
        if e <= NOISE_FLOOR {
            return 0.0;
        }
        ((e - NOISE_FLOOR) / (1.0 - NOISE_FLOOR)).clamp(0.0, 1.0)
    }

    /// Perceptual loudness in `[0,1]` for the **audio-loopback** path. Derived from
    /// the smoothed mean-square (RMS) rather than the peak envelope, so a full game
    /// mix — which pins the peak follower at ~1.0 — instead spreads across the range
    /// by intensity (quiet → loud) and *decays toward 0* on an audio cut. The noise
    /// floor still hard-zeros a silent/idle mix so there's no constant buzz.
    pub fn loudness(&self) -> f32 {
        let rms = self.ms.max(0.0).sqrt();
        if rms <= NOISE_FLOOR {
            return 0.0;
        }
        // Map [floor, REF] → [0, 1] linearly; passages above the reference saturate.
        ((rms - NOISE_FLOOR) / (LOUDNESS_REF_RMS - NOISE_FLOOR)).clamp(0.0, 1.0)
    }

    /// Current carrier frequency normalized into `[0,1]` over `[FREQ_MIN,FREQ_MAX]`.
    /// Returns the held value when the channel is below the silence floor.
    pub fn frequency_norm(&mut self) -> f32 {
        if self.amplitude() < SILENCE_RMS || self.filled < WINDOW {
            return self.last_freq_norm;
        }
        if let Some(hz) = self.detect_pitch_hz() {
            let norm = ((hz - FREQ_MIN_HZ) / (FREQ_MAX_HZ - FREQ_MIN_HZ)).clamp(0.0, 1.0);
            self.last_freq_norm = norm;
        }
        self.last_freq_norm
    }

    /// Autocorrelation pitch detector over the linear window. Searches lag range
    /// corresponding to `[FREQ_MIN,FREQ_MAX]` and returns the frequency at the
    /// strongest correlation peak, or `None` if no clear period is found.
    fn detect_pitch_hz(&self) -> Option<f32> {
        // Linearize the ring into a temporary contiguous buffer (oldest → newest).
        let mut buf = [0.0f32; WINDOW];
        for k in 0..WINDOW {
            buf[k] = self.history[(self.pos + k) % WINDOW];
        }
        // Remove DC so correlation isn't dominated by any offset.
        let mean = buf.iter().sum::<f32>() / WINDOW as f32;
        for v in buf.iter_mut() {
            *v -= mean;
        }

        let min_lag = (SAMPLE_RATE_HZ / FREQ_MAX_HZ).floor() as usize; // highest freq → shortest lag
        let max_lag = (SAMPLE_RATE_HZ / FREQ_MIN_HZ).ceil() as usize; // lowest freq → longest lag
        let max_lag = max_lag.min(WINDOW - 1);
        if min_lag >= max_lag {
            return None;
        }

        let total_energy: f32 = buf.iter().map(|v| v * v).sum();
        if total_energy <= f32::EPSILON {
            return None;
        }

        let mut best_lag = 0usize;
        let mut best_corr = 0.0f32;
        for lag in min_lag..=max_lag {
            let mut corr = 0.0f32;
            let mut e0 = 0.0f32; // energy of buf[0 .. WINDOW-lag]
            let mut e1 = 0.0f32; // energy of buf[lag .. WINDOW]
            for i in 0..(WINDOW - lag) {
                corr += buf[i] * buf[i + lag];
                e0 += buf[i] * buf[i];
                e1 += buf[i + lag] * buf[i + lag];
            }
            // Normalized cross-correlation (Pearson-style) over the OVERLAP region,
            // not the full window. Dividing by the full-window energy systematically
            // favors short lags — for a sharp transient (a menu click, which has no
            // real carrier) the autocorrelation falls monotonically from lag 0, so
            // the shortest in-band lag always "won" and we reported the max frequency
            // (~626 Hz). Per-overlap normalization removes that bias so an impulse no
            // longer masquerades as a high tone.
            let denom = (e0 * e1).sqrt();
            let norm_corr = if denom > f32::EPSILON { corr / denom } else { 0.0 };
            if norm_corr > best_corr {
                best_corr = norm_corr;
                best_lag = lag;
            }
        }

        // Require a reasonably periodic signal — reject noise / impulses that have no
        // real carrier (otherwise we'd report a jittery or maxed-out frequency).
        if best_lag == 0 || best_corr < 0.5 {
            return None;
        }
        Some(SAMPLE_RATE_HZ / best_lag as f32)
    }
}

/// Per-side result of converting a block of haptic PCM.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HapticParams {
    pub l_amp: f32,
    pub l_freq: f32,
    pub r_amp: f32,
    pub r_freq: f32,
}

/// Stateful converter for the virtual DualSense's audio-OUT haptic stream.
///
/// Feed it raw interleaved S16 LE bytes (4-channel, 48 kHz) as they arrive from
/// the driver's audio ring via [`push_pcm`](Self::push_pcm); read the latest
/// parametric values with [`params`](Self::params). The two LRA channels (`ch2`,
/// `ch3`) drive the L/R sides; the headphone channels (`ch0`, `ch1`) are ignored.
#[derive(Debug, Clone, Default)]
pub struct HapticConverter {
    left: ChannelDetector,
    right: ChannelDetector,
}

impl HapticConverter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a block of interleaved S16 LE PCM (4 ch). A trailing partial frame
    /// (fewer than `2*CHANNELS` bytes) is ignored. Safe to call with any length.
    pub fn push_pcm(&mut self, bytes: &[u8]) {
        let frame_bytes = CHANNELS * 2;
        let frames = bytes.len() / frame_bytes;
        for f in 0..frames {
            let base = f * frame_bytes;
            let l = read_s16(bytes, base + LRA_L_CH * 2);
            let r = read_s16(bytes, base + LRA_R_CH * 2);
            self.left.push(l as f32 / 32768.0);
            self.right.push(r as f32 / 32768.0);
        }
    }

    /// Latest parametric haptics: per-side amplitude (`0..1`) and normalized
    /// frequency (`0..1` over 80–500 Hz). Cheap; recomputes from the live windows.
    pub fn params(&mut self) -> HapticParams {
        HapticParams {
            l_amp: self.left.amplitude(),
            l_freq: self.left.frequency_norm(),
            r_amp: self.right.amplitude(),
            r_freq: self.right.frequency_norm(),
        }
    }
}

#[inline]
fn read_s16(bytes: &[u8], off: usize) -> i16 {
    if off + 1 < bytes.len() {
        i16::from_le_bytes([bytes[off], bytes[off + 1]])
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Build N frames of interleaved 4-ch S16 with the LRA channels carrying a
    /// sine of `hz` at `amp` (0..1); headphone channels filled with a loud
    /// *different* tone to prove they're ignored.
    fn synth(hz_l: f32, amp_l: f32, hz_r: f32, amp_r: f32, frames: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(frames * CHANNELS * 2);
        for n in 0..frames {
            let t = n as f32 / SAMPLE_RATE_HZ;
            let hp = (TAU * 1000.0 * t).sin(); // 1 kHz on headphones — must be ignored
            let l = (TAU * hz_l * t).sin() * amp_l;
            let r = (TAU * hz_r * t).sin() * amp_r;
            for (ch, v) in [hp, hp, l, r].into_iter().enumerate() {
                let _ = ch;
                let s = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn silence_reports_zero_amplitude() {
        let mut c = HapticConverter::new();
        c.push_pcm(&synth(160.0, 0.0, 160.0, 0.0, WINDOW * 2));
        let p = c.params();
        assert!(p.l_amp < SILENCE_RMS, "silent L amp ~0, got {}", p.l_amp);
        assert!(p.r_amp < SILENCE_RMS, "silent R amp ~0, got {}", p.r_amp);
    }

    #[test]
    fn full_scale_sine_reports_high_amplitude() {
        let mut c = HapticConverter::new();
        c.push_pcm(&synth(160.0, 1.0, 160.0, 1.0, WINDOW * 2));
        let p = c.params();
        // Peak envelope of a full-scale sine settles near its peak ≈ 1.0.
        assert!(p.l_amp > 0.9, "L amp ≈1.0, got {}", p.l_amp);
        assert!(p.r_amp > 0.9, "R amp ≈1.0, got {}", p.r_amp);
    }

    #[test]
    fn short_transient_is_not_averaged_away() {
        // A brief loud burst (a few ms) followed by silence — the case the old
        // window-RMS crushed to near-zero. The peak envelope must still register a
        // clearly-felt amplitude shortly after the burst.
        let mut c = HapticConverter::new();
        // ~3 ms of full-scale 200 Hz on the LRA channels (≈144 frames @ 48 kHz).
        c.push_pcm(&synth(200.0, 1.0, 200.0, 1.0, 144));
        let p = c.params();
        assert!(p.l_amp > 0.5, "transient L should be strongly felt, got {}", p.l_amp);
        assert!(p.r_amp > 0.5, "transient R should be strongly felt, got {}", p.r_amp);
    }

    #[test]
    fn detects_carrier_frequency_in_band() {
        let mut c = HapticConverter::new();
        // 160 Hz left, 320 Hz right.
        c.push_pcm(&synth(160.0, 0.8, 320.0, 0.8, WINDOW * 3));
        let p = c.params();
        let l_hz = FREQ_MIN_HZ + p.l_freq * (FREQ_MAX_HZ - FREQ_MIN_HZ);
        let r_hz = FREQ_MIN_HZ + p.r_freq * (FREQ_MAX_HZ - FREQ_MIN_HZ);
        assert!((l_hz - 160.0).abs() < 12.0, "L carrier ≈160Hz, got {l_hz}");
        assert!((r_hz - 320.0).abs() < 18.0, "R carrier ≈320Hz, got {r_hz}");
    }

    #[test]
    fn headphone_channels_do_not_leak_into_haptics() {
        // LRA channels silent, headphones loud at 1 kHz → haptics must stay ~0.
        let mut c = HapticConverter::new();
        c.push_pcm(&synth(1000.0, 0.0, 1000.0, 0.0, WINDOW * 2));
        let p = c.params();
        assert!(p.l_amp < SILENCE_RMS && p.r_amp < SILENCE_RMS,
            "headphone tone leaked: L={} R={}", p.l_amp, p.r_amp);
    }

    #[test]
    fn frequency_holds_through_silence_gap() {
        let mut c = HapticConverter::new();
        c.push_pcm(&synth(200.0, 0.8, 200.0, 0.8, WINDOW * 3));
        let p1 = c.params();
        // Now feed enough silence for the slow-release envelope to fully decay —
        // frequency should hold (not snap to 0), amplitude should fall to silence.
        c.push_pcm(&synth(200.0, 0.0, 200.0, 0.0, WINDOW * 6));
        let p2 = c.params();
        assert_eq!(p2.l_freq, p1.l_freq, "freq held across silence");
        assert!(p2.l_amp < SILENCE_RMS, "amp dropped in silence, got {}", p2.l_amp);
    }

    #[test]
    fn partial_trailing_frame_is_ignored_safely() {
        let mut c = HapticConverter::new();
        let mut pcm = synth(160.0, 0.5, 160.0, 0.5, 10);
        pcm.push(0x12); // dangling odd byte
        pcm.push(0x34);
        pcm.push(0x56); // not a full 8-byte frame
        c.push_pcm(&pcm); // must not panic
        let _ = c.params();
    }

    #[test]
    fn independent_left_right_amplitude() {
        let mut c = HapticConverter::new();
        c.push_pcm(&synth(160.0, 1.0, 160.0, 0.0, WINDOW * 2));
        let p = c.params();
        assert!(p.l_amp > 0.5, "L loud");
        assert!(p.r_amp < SILENCE_RMS, "R silent");
    }
}
