//! DualSense HD haptics over WASAPI.
//!
//! The PS5 firmware exposes the controller as a 4-channel USB audio render
//! endpoint:
//!
//!   channel 1 → headphone L
//!   channel 2 → headphone R
//!   channel 3 → left  LRA (HD haptic actuator)
//!   channel 4 → right LRA (HD haptic actuator)
//!
//! Writing PCM samples on ch3/ch4 vibrates the actuators at whatever frequency
//! the waveform encodes. Bluetooth does not expose the audio endpoint, so this
//! path is USB-only — the caller filters by `Connection::Usb` before opening.
//!
//! The render thread runs WASAPI in shared mode (event-driven) and pulls fresh
//! amplitude/frequency targets from an atomic each period. The atomic shape
//! lets the gyro I/O thread call `set_targets()` without blocking.

#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use wasapi::{
    initialize_mta, AudioClient, AudioRenderClient, DeviceCollection, Direction, SampleType,
    StreamMode, WaveFormat,
};

/// Frequency range exposed by the `ds_l_freq` / `ds_r_freq` pins.
/// The DualSense LRAs resonate around ~160 Hz; useful band is ~80–500 Hz.
const FREQ_MIN_HZ: f32 = 80.0;
const FREQ_MAX_HZ: f32 = 500.0;

/// Drive headroom — going past ~0.7 saturates the controller's speaker amp
/// without making the actuator shake harder, and leaks audible buzz.
const DRIVE_CEILING: f32 = 0.7;

/// Shared atomic state between the I/O thread (writer) and the audio render
/// thread (reader). Packed into 32 bits so updates are lock-free.
struct Shared {
    /// Carrier 1 (LF) bytes: [l_amp, l_freq, r_amp, r_freq], each 0–255.
    targets: AtomicU32,
    /// Carrier 2 (HF) bytes: [l_amp, l_freq, r_amp, r_freq], each 0–255. The two
    /// carriers are summed per LRA so the actuator plays both bands at once.
    targets2: AtomicU32,
    /// Cleared from `Drop` to ask the render thread to exit.
    running: AtomicBool,
}

pub struct HapticStream {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl HapticStream {
    /// Locate the DualSense audio endpoint and start the render thread.
    /// Returns `None` if no matching device exists (controller not plugged in
    /// over USB, or audio driver hasn't enumerated it yet).
    pub fn open() -> Option<Self> {
        let shared = Arc::new(Shared {
            targets: AtomicU32::new(0),
            targets2: AtomicU32::new(0),
            running: AtomicBool::new(true),
        });
        let shared_for_thread = shared.clone();

        let thread = thread::Builder::new()
            .name("dualsense-haptic".into())
            .spawn(move || {
                if let Err(e) = render_loop(shared_for_thread) {
                    #[cfg(debug_assertions)]
                    eprintln!("[ds-haptic] render loop exited: {e}");
                    let _ = e;
                }
            })
            .ok()?;

        Some(Self { shared, thread: Some(thread) })
    }

    /// Update BOTH carriers per side: carrier 1 (LF) and carrier 2 (HF), each an
    /// (amp, freq) pair in 0–1. The render thread sums the two sines per LRA so the
    /// actuator plays both bands simultaneously (matching the Switch Pro's HF+LF
    /// capability). Lock-free; safe from any thread.
    #[allow(clippy::too_many_arguments)]
    pub fn set_targets_dual(
        &self,
        l_lf_amp: f32, l_lf_freq: f32, l_hf_amp: f32, l_hf_freq: f32,
        r_lf_amp: f32, r_lf_freq: f32, r_hf_amp: f32, r_hf_freq: f32,
    ) {
        let pack = |a: f32, af: f32, b: f32, bf: f32| -> u32 {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            q(a) | (q(af) << 8) | (q(b) << 16) | (q(bf) << 24)
        };
        self.shared.targets.store(pack(l_lf_amp, l_lf_freq, r_lf_amp, r_lf_freq), Ordering::Release);
        self.shared.targets2.store(pack(l_hf_amp, l_hf_freq, r_hf_amp, r_hf_freq), Ordering::Release);
    }
}

impl Drop for HapticStream {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        if let Some(h) = self.thread.take() {
            // Render loop polls `running` every period (~10 ms) so this is brief.
            let _ = h.join();
        }
    }
}

// ── render thread ─────────────────────────────────────────────────────────────

fn render_loop(shared: Arc<Shared>) -> Result<(), String> {
    // COM init for this thread (MTA — matches what the wasapi crate documents).
    // Ignored if already initialized.
    let _ = initialize_mta();

    let (client, render, fmt, event) = open_dualsense_stream()
        .ok_or_else(|| "no DualSense audio endpoint found".to_string())?;

    let sample_rate = fmt.get_samplespersec() as f32;
    let channels = fmt.get_nchannels() as usize;
    let bytes_per_frame = fmt.get_blockalign() as usize;
    let sample_type = fmt.get_subformat().map_err(|e| format!("subformat: {e}"))?;

    client.start_stream().map_err(|e| format!("start: {e}"))?;

    // Continuous-phase sine oscillators per side, per carrier (LF + HF).
    let mut phase_l: f32 = 0.0;   // carrier 1 (LF) left
    let mut phase_r: f32 = 0.0;   // carrier 1 (LF) right
    let mut phase_l2: f32 = 0.0;  // carrier 2 (HF) left
    let mut phase_r2: f32 = 0.0;  // carrier 2 (HF) right
    let two_pi = std::f32::consts::TAU;

    let mut scratch: Vec<u8> = Vec::new();

    while shared.running.load(Ordering::Acquire) {
        // 100 ms ceiling so unplugging the controller doesn't trap the thread.
        if event.wait_for_event(100).is_err() {
            // Timeout or device error — re-check `running` and loop.
            continue;
        }

        let frames = match client.get_available_space_in_frames() {
            Ok(n) => n as usize,
            Err(_) => break,
        };
        if frames == 0 { continue; }

        let needed = frames * bytes_per_frame;
        if scratch.len() != needed { scratch.resize(needed, 0); }

        // Latest amp/freq snapshot for this period — both carriers.
        let unpack = |packed: u32| {
            (
                (packed         & 0xFF) as f32 / 255.0, // l_amp
                ((packed >>  8) & 0xFF) as f32 / 255.0, // l_freq
                ((packed >> 16) & 0xFF) as f32 / 255.0, // r_amp
                ((packed >> 24) & 0xFF) as f32 / 255.0, // r_freq
            )
        };
        let (l1_amp, l1_freq, r1_amp, r1_freq) = unpack(shared.targets.load(Ordering::Acquire));
        let (l2_amp, l2_freq, r2_amp, r2_freq) = unpack(shared.targets2.load(Ordering::Acquire));

        let hz = |f: f32| FREQ_MIN_HZ + f * (FREQ_MAX_HZ - FREQ_MIN_HZ);
        let step = |f: f32| two_pi * hz(f) / sample_rate;
        let l1_step = step(l1_freq); let r1_step = step(r1_freq);
        let l2_step = step(l2_freq); let r2_step = step(r2_freq);
        // Each carrier is driven independently; we sum then keep the LRA within the
        // drive ceiling (so two loud carriers don't clip past the safe amplitude).
        let l1_d = l1_amp * DRIVE_CEILING; let r1_d = r1_amp * DRIVE_CEILING;
        let l2_d = l2_amp * DRIVE_CEILING; let r2_d = r2_amp * DRIVE_CEILING;

        for f in 0..frames {
            let sl = (l1_d * phase_l.sin() + l2_d * phase_l2.sin()).clamp(-1.0, 1.0);
            let sr = (r1_d * phase_r.sin() + r2_d * phase_r2.sin()).clamp(-1.0, 1.0);
            phase_l  += l1_step; phase_r  += r1_step;
            phase_l2 += l2_step; phase_r2 += r2_step;
            if phase_l  > two_pi { phase_l  -= two_pi; }
            if phase_r  > two_pi { phase_r  -= two_pi; }
            if phase_l2 > two_pi { phase_l2 -= two_pi; }
            if phase_r2 > two_pi { phase_r2 -= two_pi; }

            let off = f * bytes_per_frame;
            write_frame(&mut scratch[off..off + bytes_per_frame], channels, &sample_type, sl, sr);
        }

        if render.write_to_device(frames, &scratch, None).is_err() {
            break;
        }
    }

    let _ = client.stop_stream();
    Ok(())
}

/// Write one interleaved frame: silence on channels 1/2 (headphone L/R) and
/// the haptic sine on channels 3/4 (left/right LRA). Extra channels (5.1 / 7.1
/// pseudo-layouts) are zeroed defensively, though DualSense always reports 4.
fn write_frame(frame: &mut [u8], channels: usize, sample_type: &SampleType, hl: f32, hr: f32) {
    // For each channel position, the sample value we want.
    let pick = |ch: usize| -> f32 {
        match ch {
            0 | 1 => 0.0,         // headphone L / R — silent
            2 => hl,              // left LRA
            3 => hr,              // right LRA
            _ => 0.0,
        }
    };
    match sample_type {
        SampleType::Float => {
            let bps = 4;
            for ch in 0..channels {
                let v = pick(ch);
                let off = ch * bps;
                frame[off..off + bps].copy_from_slice(&v.to_le_bytes());
            }
        }
        SampleType::Int => {
            // Block-align tells us bytes per sample (block_align / channels).
            let bps = frame.len() / channels;
            for ch in 0..channels {
                let v = pick(ch).clamp(-1.0, 1.0);
                let off = ch * bps;
                match bps {
                    2 => {
                        let s = (v * 32767.0) as i16;
                        frame[off..off + 2].copy_from_slice(&s.to_le_bytes());
                    }
                    3 => {
                        let s = (v * 8_388_607.0) as i32;
                        let b = s.to_le_bytes();
                        frame[off..off + 3].copy_from_slice(&b[..3]);
                    }
                    4 => {
                        let s = (v * 2_147_483_647.0) as i32;
                        frame[off..off + 4].copy_from_slice(&s.to_le_bytes());
                    }
                    _ => for b in &mut frame[off..off + bps] { *b = 0; },
                }
            }
        }
    }
}

// ── device matching ───────────────────────────────────────────────────────────

/// Walk active render endpoints and return the first DualSense-looking one as
/// a fully-initialized event-driven shared-mode stream.
///
/// Heuristic: friendly name contains "Wireless Controller" (Sony's USB Audio
/// product name, shared by DS4 and DualSense — both expose 4 channels when
/// they have HD haptic actuators) or "DualSense" (some Windows drivers).
fn open_dualsense_stream(
) -> Option<(AudioClient, AudioRenderClient, WaveFormat, wasapi::Handle)> {
    let collection = DeviceCollection::new(&Direction::Render).ok()?;
    let count = collection.get_nbr_devices().ok()?;

    for i in 0..count {
        let Ok(device) = collection.get_device_at_index(i) else { continue; };
        let name = device.get_friendlyname().unwrap_or_default();
        if !(name.contains("Wireless Controller") || name.contains("DualSense")) {
            continue;
        }

        let Ok(mut client) = device.get_iaudioclient() else { continue; };
        let Ok(fmt) = client.get_mixformat() else { continue; };
        // Only proceed if this endpoint actually has the 4-channel layout that
        // exposes haptic actuators — otherwise we'd just write silent ch3/ch4
        // into nothing, or worse leak the empty channels into the headphone mix.
        if fmt.get_nchannels() < 4 { continue; }

        let stream_mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 10 * 10_000, // 10 ms (100ns units)
        };
        if client.initialize_client(&fmt, &Direction::Render, &stream_mode).is_err() {
            continue;
        }
        let Ok(event) = client.set_get_eventhandle() else { continue; };
        let Ok(render) = client.get_audiorenderclient() else { continue; };

        return Some((client, render, fmt, event));
    }
    None
}
