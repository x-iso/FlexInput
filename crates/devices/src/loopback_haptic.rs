//! Audio-to-haptics via WASAPI process loopback.
//!
//! The driver-based HD-haptics path (a virtual DualSense USB audio endpoint that
//! games stream their authored haptic track to) needs a kernel driver, which
//! can't ship without test-signing / paid attestation. This is the shippable
//! alternative: capture a *specific application's* audio output with WASAPI
//! **process loopback** — pure user-mode, no driver — and derive rumble from it.
//!
//! It's lower fidelity than the authored DualSense LRA track (we get the audible
//! game mix: gunfire, explosions, engine, music — not dedicated haptic channels),
//! but it ships to anyone and drives genuinely useful "feel the action" rumble.
//!
//! Pipeline:
//!   process loopback PCM (the target app's render mix, shared-mode format)
//!     → per-side envelope + pitch detection ([`crate::haptic_pcm::ChannelDetector`])
//!     → `(amplitude, frequency)` per side, published lock-free
//!
//! Downstream (a separate step) maps that amp/freq onto the Switch Pro HD-rumble
//! encoder / virtual-pad feedback. This module is capture + DSP only — no I/O out.
//!
//! Mirrors the threading model of [`crate::dualsense_haptic`]: one named worker
//! thread runs the event-driven WASAPI client; consumers read a lock-free atomic.

#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};

use wasapi::{initialize_mta, Direction, SampleType, WaveFormat};

use crate::haptic_pcm::ChannelDetector;
use crate::spectrum::{SpectrumAnalyzer, N_BANDS};

/// Number of downsampled time-domain points retained for the node's EF
/// oscilloscope. ~2 s at the ~120 Hz publish rate; cheap to clone for the UI.
pub const SCOPE_POINTS: usize = 256;

/// One downsampled oscilloscope point: per-side audio peak (rectified, 0–1), the
/// envelope-follower / loudness output (0–1), and the loudness split into the LF
/// and HF carriers by their instantaneous spectral energy fraction (0–1 each,
/// `env_lf + env_hf ≈ max(env_l, env_r)`). The split is mono (the spectrum is
/// mono-summed) so it's the same for both sides.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScopePoint {
    pub audio_l: f32,
    pub audio_r: f32,
    pub env_l: f32,
    pub env_r: f32,
    pub env_lf: f32,
    pub env_hf: f32,
}

/// Latest detected per-side haptic targets, packed into 32 bits for lock-free
/// publish/consume: `[l_amp, l_freq, r_amp, r_freq]`, each quantized to 0–255.
struct Shared {
    targets: AtomicU32,
    running: AtomicBool,
    /// Loudness-follower release time in milliseconds × 10 (fixed-point), set live
    /// by the node's Release slider and read by the capture loop each iteration.
    release_ms_x10: AtomicU32,
    /// Input gain × 1000 (fixed-point), set live by the node's Volume slider.
    /// Applied to the raw samples BEFORE envelope/RMS + FFT, so lowering it truly
    /// reduces the signal (restoring headroom on a hot/clipping source) rather than
    /// squashing an already-detected loudness.
    volume_x1000: AtomicU32,
    /// LF/HF crossover band position × 1000 (0..1 on the log spectrum range), set by
    /// the node's Crossover slider. Used only to split the scope envelope into its
    /// LF/HF carrier traces; the haptic split is done in the engine.
    crossover_pos_x1000: AtomicU32,
    /// Rolling oscilloscope ring (newest pushed at the back). Mutex-guarded — only
    /// touched once per publish from the capture thread and read by the UI, so
    /// contention is negligible.
    scope: Mutex<std::collections::VecDeque<ScopePoint>>,
    /// Latest log-band audio spectrum (0..1-ish per band) for the node's spectrum
    /// view and the multi-band engine. Mutex-guarded; written once per publish.
    spectrum: Mutex<[f32; N_BANDS]>,
}

/// Per-side amplitude (0–1) and normalized carrier frequency (0–1) recovered
/// from the captured audio. Frequency normalization matches the `*_freq` pin
/// convention used elsewhere (0 = [`crate::haptic_pcm::FREQ_MIN_HZ`], 1 = MAX).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoopbackParams {
    pub l_amp: f32,
    pub l_freq: f32,
    pub r_amp: f32,
    pub r_freq: f32,
}

/// What audio to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopbackTarget {
    /// A specific process (and optionally its child tree) via process loopback.
    Process { pid: u32, include_tree: bool },
    /// The whole system mix on the default render endpoint (classic loopback).
    System,
}

/// A running loopback haptic capture (one process, or the system mix).
pub struct LoopbackHaptic {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl LoopbackHaptic {
    /// Start a capture for `target`, deriving haptics from its audio. Returns
    /// `None` only if the worker thread can't be spawned; the WASAPI client is
    /// opened on the thread, so a not-yet-playing target is fine.
    pub fn open_target(target: LoopbackTarget) -> Option<Self> {
        let shared = Arc::new(Shared {
            targets: AtomicU32::new(0),
            running: AtomicBool::new(true),
            release_ms_x10: AtomicU32::new(300), // 30.0 ms default
            volume_x1000: AtomicU32::new(1000),  // 1.0× default
            crossover_pos_x1000: AtomicU32::new(500), // mid by default
            scope: Mutex::new(std::collections::VecDeque::with_capacity(SCOPE_POINTS)),
            spectrum: Mutex::new([0.0; N_BANDS]),
        });
        let shared_for_thread = shared.clone();

        let thread = thread::Builder::new()
            .name("loopback-haptic".into())
            .spawn(move || {
                if let Err(e) = capture_loop(shared_for_thread, target) {
                    #[cfg(debug_assertions)]
                    eprintln!("[loopback-haptic] capture loop exited: {e}");
                    let _ = e;
                }
            })
            .ok()?;

        Some(Self { shared, thread: Some(thread) })
    }

    /// Capture a specific process (and optionally its child tree).
    pub fn open(process_id: u32, include_tree: bool) -> Option<Self> {
        Self::open_target(LoopbackTarget::Process { pid: process_id, include_tree })
    }

    /// Capture the whole system audio mix (default render endpoint loopback).
    pub fn open_system() -> Option<Self> {
        Self::open_target(LoopbackTarget::System)
    }

    /// Set the loudness-follower release time (ms). Lock-free; applied by the
    /// capture loop on its next iteration. Cheap to call every reconcile tick.
    pub fn set_release_ms(&self, ms: f32) {
        let x10 = (ms.clamp(1.0, 1000.0) * 10.0).round() as u32;
        self.shared.release_ms_x10.store(x10, Ordering::Release);
    }

    /// Set the input gain (Volume). Applied to raw samples before detection, so
    /// lowering it genuinely reduces the signal. Lock-free.
    pub fn set_volume(&self, v: f32) {
        let x1000 = (v.clamp(0.0, 4.0) * 1000.0).round() as u32;
        self.shared.volume_x1000.store(x1000, Ordering::Release);
    }

    /// Set the LF/HF crossover band position (0..1 on the log spectrum range) used
    /// to split the scope envelope into LF/HF traces. Lock-free.
    pub fn set_crossover_pos(&self, pos: f32) {
        let x1000 = (pos.clamp(0.0, 1.0) * 1000.0).round() as u32;
        self.shared.crossover_pos_x1000.store(x1000, Ordering::Release);
    }

    /// Latest per-side haptic targets. Lock-free; safe from any thread.
    pub fn params(&self) -> LoopbackParams {
        let packed = self.shared.targets.load(Ordering::Acquire);
        let unpack = |shift: u32| ((packed >> shift) & 0xFF) as f32 / 255.0;
        LoopbackParams {
            l_amp: unpack(0),
            l_freq: unpack(8),
            r_amp: unpack(16),
            r_freq: unpack(24),
        }
    }

    /// Snapshot the EF oscilloscope ring (oldest → newest). Cheap clone for the UI.
    pub fn scope_snapshot(&self) -> Vec<ScopePoint> {
        self.shared.scope.lock().map(|q| q.iter().copied().collect()).unwrap_or_default()
    }

    /// Latest log-band audio spectrum (per-band magnitude). Cheap copy for the UI
    /// and the multi-band engine.
    pub fn spectrum_snapshot(&self) -> [f32; N_BANDS] {
        self.shared.spectrum.lock().map(|s| *s).unwrap_or([0.0; N_BANDS])
    }
}

impl Drop for LoopbackHaptic {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        if let Some(h) = self.thread.take() {
            // The loop wakes at least every 100 ms, so the join is brief.
            let _ = h.join();
        }
    }
}

fn pack_targets(p: LoopbackParams) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    q(p.l_amp) | (q(p.l_freq) << 8) | (q(p.r_amp) << 16) | (q(p.r_freq) << 24)
}

// ── capture thread ────────────────────────────────────────────────────────────

fn capture_loop(shared: Arc<Shared>, target: LoopbackTarget) -> Result<(), String> {
    // WASAPI lives in the MTA (matches the wasapi crate + dualsense_haptic path).
    let _ = initialize_mta();

    let (client, channels, sample_type, bytes_per_frame) = open_client(target)?;
    let bytes_per_sample = if channels > 0 { bytes_per_frame / channels } else { bytes_per_frame };

    let event = client.set_get_eventhandle().map_err(|e| format!("event handle: {e}"))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| format!("capture client: {e}"))?;

    client.start_stream().map_err(|e| format!("start: {e}"))?;

    // One envelope/pitch detector per haptic side. We map the captured render mix
    // onto L/R: stereo → channels 0/1; mono → both sides share the same signal;
    // multichannel → fronts (0/1). The detectors run at the capture sample rate;
    // haptic_pcm's detector logic is sample-rate-agnostic for our purposes.
    let mut det_l = ChannelDetector::new();
    let mut det_r = ChannelDetector::new();
    // Streaming spectrum over the mono-summed mix (for the spectrum view + bands).
    let mut spec = SpectrumAnalyzer::new();

    let mut buf: Vec<u8> = Vec::new();
    // How many samples to bleed in per side when the stream goes idle (no events),
    // so the loudness follower keeps decaying toward 0 instead of freezing at its
    // last value (the "stuck buzz" on a hard audio cut). One wait period of silence
    // at 48 kHz: 100 ms ≈ 4800 samples.
    let idle_decay_samples = 4_800usize;

    while shared.running.load(Ordering::Acquire) {
        // Apply the latest Release + Volume settings (cheap; atomic loads per wake).
        let release_ms = shared.release_ms_x10.load(Ordering::Acquire) as f32 / 10.0;
        det_l.set_release_ms(release_ms);
        det_r.set_release_ms(release_ms);
        let volume = shared.volume_x1000.load(Ordering::Acquire) as f32 / 1000.0;

        // 100 ms ceiling so a stopped/exited target doesn't trap the thread.
        let woke = event.wait_for_event(100).is_ok();
        let mut got_audio = false;

        if woke {
            // Drain all currently-available packets.
            loop {
                if !shared.running.load(Ordering::Acquire) {
                    break;
                }
                let frames = match capture.get_next_packet_size() {
                    Ok(Some(n)) if n > 0 => n as usize,
                    Ok(_) => break, // no more packets ready
                    Err(_) => break,
                };

                let needed = frames * bytes_per_frame;
                if buf.len() < needed {
                    buf.resize(needed, 0);
                }
                // read_from_device fills our buffer and reports flags (silence etc.).
                let (_frames_read, flags) = match capture.read_from_device(&mut buf[..needed]) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                got_audio = true;

                // On a silent packet WASAPI may not fill the buffer; treat as zeros so
                // the envelope decays naturally rather than holding the last value.
                let silent = flags.silent;

                // Peak audio over this packet, per side, for the EF scope trace.
                let (mut peak_l, mut peak_r) = (0.0f32, 0.0f32);
                for f in 0..frames {
                    let base = f * bytes_per_frame;
                    let (mut sl, mut sr) = (0.0f32, 0.0f32);
                    if !silent {
                        sl = read_sample(&buf[base..], 0, bytes_per_sample, &sample_type);
                        sr = if channels >= 2 {
                            read_sample(&buf[base..], 1, bytes_per_sample, &sample_type)
                        } else {
                            sl // mono → drive both sides equally
                        };
                        // Input gain BEFORE detection — this is what lets lowering
                        // Volume restore headroom on a hot source instead of just
                        // squashing the detected loudness.
                        sl *= volume;
                        sr *= volume;
                    }
                    det_l.push(sl);
                    det_r.push(sr);
                    spec.push((sl + sr) * 0.5);
                    peak_l = peak_l.max(sl.abs());
                    peak_r = peak_r.max(sr.abs());
                }

                publish(&shared, &mut det_l, &mut det_r, &mut spec, peak_l, peak_r);
            }
        }

        // No event (timeout) or the wake delivered no packets ⇒ the source went
        // idle. WASAPI stops firing events on a stopped render stream, so without
        // this the followers would freeze at their last (non-zero) value and the
        // pad would buzz forever. Bleed in silence so loudness decays to 0, and
        // republish so the zeroed amplitude actually reaches the gamepad.
        if !got_audio {
            for _ in 0..idle_decay_samples {
                det_l.push(0.0);
                det_r.push(0.0);
                spec.push(0.0);
            }
            publish(&shared, &mut det_l, &mut det_r, &mut spec, 0.0, 0.0);
        }
    }

    let _ = client.stop_stream();
    Ok(())
}

/// Read the detectors' current per-side haptics and publish them lock-free.
/// Amplitude uses the RMS-based **loudness** (wide dynamic range, decays to 0 on
/// silence) rather than the peak envelope, so the volume control has headroom and
/// a hard audio cut doesn't leave a stuck buzz.
fn publish(
    shared: &Arc<Shared>,
    det_l: &mut ChannelDetector,
    det_r: &mut ChannelDetector,
    spec: &mut SpectrumAnalyzer,
    peak_l: f32,
    peak_r: f32,
) {
    let params = LoopbackParams {
        l_amp: det_l.loudness(),
        l_freq: det_l.frequency_norm(),
        r_amp: det_r.loudness(),
        r_freq: det_r.frequency_norm(),
    };
    shared.targets.store(pack_targets(params), Ordering::Release);

    // Recompute the log-band spectrum, then split the envelope into LF/HF carriers
    // by the spectral energy fraction at the crossover (mirrors the engine's split,
    // so the two scope traces match what the pad plays).
    let bands = spec.compute();
    let xpos = shared.crossover_pos_x1000.load(Ordering::Acquire) as f32 / 1000.0;
    let n = bands.len();
    let (mut lf_e, mut hf_e) = (0.0f32, 0.0f32);
    for (i, &m) in bands.iter().enumerate() {
        let pos = (i as f32 + 0.5) / n as f32;
        let e = m.max(0.0).sqrt();
        if pos < xpos { lf_e += e; } else { hf_e += e; }
    }
    let total = lf_e + hf_e;
    let env = params.l_amp.max(params.r_amp);
    let (env_lf, env_hf) = if total > 1.0e-4 {
        (env * lf_e / total, env * hf_e / total)
    } else {
        (0.0, 0.0)
    };

    // Push one downsampled point onto the EF scope ring.
    if let Ok(mut q) = shared.scope.lock() {
        if q.len() >= SCOPE_POINTS {
            q.pop_front();
        }
        q.push_back(ScopePoint {
            audio_l: peak_l.clamp(0.0, 1.0),
            audio_r: peak_r.clamp(0.0, 1.0),
            env_l: params.l_amp,
            env_r: params.r_amp,
            env_lf,
            env_hf,
        });
    }

    if let Ok(mut s) = shared.spectrum.lock() {
        *s = bands;
    }
}

/// Open + initialize the WASAPI capture client for `target`, returning it plus
/// the frame layout (channels, sample type, bytes-per-frame) the loop reads.
fn open_client(
    target: LoopbackTarget,
) -> Result<(wasapi::AudioClient, usize, SampleType, usize), String> {
    match target {
        LoopbackTarget::Process { pid, include_tree } => {
            // Process-loopback client wraps the ActivateAudioInterfaceAsync +
            // completion-handler dance (VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK).
            let mut client = wasapi::AudioClient::new_application_loopback_client(pid, include_tree)
                .map_err(|e| format!("process loopback client: {e}"))?;
            // Process-loopback clients return E_NOTIMPL from GetMixFormat — the
            // format must be SPECIFIED. Canonical loopback format: 32f / 48k / stereo
            // (the engine resamples the target into it).
            let channels = 2usize;
            let sample_type = SampleType::Float;
            let fmt = WaveFormat::new(32, 32, &sample_type, 48_000, channels, None);
            client
                .initialize_client(
                    &fmt,
                    &Direction::Capture,
                    &wasapi::StreamMode::EventsShared { autoconvert: false, buffer_duration_hns: 0 },
                )
                .map_err(|e| format!("init process loopback: {e}"))?;
            Ok((client, channels, sample_type, fmt.get_blockalign() as usize))
        }
        LoopbackTarget::System => {
            // System loopback = capture the DEFAULT RENDER endpoint. The wasapi
            // crate sets AUDCLNT_STREAMFLAGS_LOOPBACK automatically when a Render
            // device is initialized for Capture in shared mode. Unlike process
            // loopback, the render device DOES support GetMixFormat, so query it.
            let device = wasapi::get_default_device(&Direction::Render)
                .map_err(|e| format!("default render device: {e}"))?;
            let mut client = device
                .get_iaudioclient()
                .map_err(|e| format!("render audio client: {e}"))?;
            let fmt = client
                .get_mixformat()
                .map_err(|e| format!("render mix format: {e}"))?;
            let channels = fmt.get_nchannels() as usize;
            let sample_type = fmt.get_subformat().map_err(|e| format!("subformat: {e}"))?;
            let bytes_per_frame = fmt.get_blockalign() as usize;
            client
                .initialize_client(
                    &fmt,
                    &Direction::Capture, // Render device + Capture + Shared ⇒ loopback
                    &wasapi::StreamMode::EventsShared { autoconvert: false, buffer_duration_hns: 0 },
                )
                .map_err(|e| format!("init system loopback: {e}"))?;
            Ok((client, channels, sample_type, bytes_per_frame))
        }
    }
}

/// Read interleaved sample `ch` from one frame slice as a normalized `f32`
/// in roughly [-1, 1]. Supports float32 and signed-int (16/24/32) PCM.
fn read_sample(frame: &[u8], ch: usize, bytes_per_sample: usize, sample_type: &SampleType) -> f32 {
    let off = ch * bytes_per_sample;
    if off + bytes_per_sample > frame.len() {
        return 0.0;
    }
    let s = &frame[off..off + bytes_per_sample];
    match sample_type {
        SampleType::Float => match bytes_per_sample {
            4 => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            8 => f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32,
            _ => 0.0,
        },
        SampleType::Int => match bytes_per_sample {
            2 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32_768.0,
            3 => {
                // 24-bit signed, little-endian → sign-extend to i32.
                let raw = (s[0] as i32) | ((s[1] as i32) << 8) | ((s[2] as i32) << 16);
                let signed = (raw << 8) >> 8; // sign-extend from bit 23
                signed as f32 / 8_388_608.0
            }
            4 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            _ => 0.0,
        },
    }
}

// ── process enumeration (for the target picker) ────────────────────────────────

/// Running-process discovery used by the Audio Stream Haptics target picker.
/// Names are exe file names (e.g. "Game.exe"); PIDs change every launch, so the
/// module persists the NAME and re-resolves it to a live PID via [`pid_for_name`].
pub mod process {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    /// One running process: exe name + PID.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ProcInfo {
        pub pid: u32,
        pub name: String,
    }

    fn entry_name(entry: &PROCESSENTRY32W) -> String {
        let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&entry.szExeFile[..end])
    }

    /// Snapshot of all running processes that have a non-empty exe name, sorted
    /// case-insensitively by name. Best-effort: returns empty on snapshot failure.
    pub fn list() -> Vec<ProcInfo> {
        let mut out = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap.is_null() {
                return out;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut entry) != 0 {
                loop {
                    let name = entry_name(&entry);
                    if !name.is_empty() {
                        out.push(ProcInfo { pid: entry.th32ProcessID, name });
                    }
                    if Process32NextW(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    /// First live PID whose exe name equals `name` (case-insensitive). Used to
    /// re-resolve a persisted target name to a current PID each session.
    pub fn pid_for_name(name: &str) -> Option<u32> {
        let want = name.to_lowercase();
        list().into_iter().find(|p| p.name.to_lowercase() == want).map(|p| p.pid)
    }

    /// PID owning the current foreground window (for "focused app" mode).
    pub fn foreground_pid() -> Option<u32> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            (pid != 0).then_some(pid)
        }
    }

    /// Exe name for a PID, if it's still running (for showing the focused app's name).
    pub fn name_for_pid(pid: u32) -> Option<String> {
        list().into_iter().find(|p| p.pid == pid).map(|p| p.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_roundtrip_quantizes_each_side() {
        let p = LoopbackParams { l_amp: 1.0, l_freq: 0.0, r_amp: 0.5, r_freq: 0.25 };
        let packed = pack_targets(p);
        let unpack = |shift: u32| ((packed >> shift) & 0xFF) as f32 / 255.0;
        assert!((unpack(0) - 1.0).abs() < 0.01);
        assert!(unpack(8) < 0.01);
        assert!((unpack(16) - 0.5).abs() < 0.01);
        assert!((unpack(24) - 0.25).abs() < 0.01);
    }

    #[test]
    fn read_sample_float_and_int() {
        // float32 full-scale
        let f = 0.75f32.to_le_bytes();
        assert!((read_sample(&f, 0, 4, &SampleType::Float) - 0.75).abs() < 1e-6);
        // i16 mid
        let h = (16_384i16).to_le_bytes();
        assert!((read_sample(&h, 0, 2, &SampleType::Int) - 0.5).abs() < 0.01);
        // out-of-range channel returns 0
        assert_eq!(read_sample(&f, 5, 4, &SampleType::Float), 0.0);
    }
}
