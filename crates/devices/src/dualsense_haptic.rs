//! DualSense HD haptics over the pad's USB audio: WASAPI on Windows, ALSA on
//! Linux.
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
//! The render thread pulls fresh amplitude/frequency targets from an atomic
//! each period. The atomic shape lets the gyro I/O thread call `set_targets()`
//! without blocking. On Windows it runs WASAPI in shared mode (event-driven);
//! on Linux it writes the pad's own sound card, found through sysfs from its
//! hidraw node, which it then holds (no other program can play to the pad
//! meanwhile).

#![cfg(any(windows, target_os = "linux"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

#[cfg(windows)]
use wasapi::{
    initialize_mta, AudioClient, AudioRenderClient, DeviceCollection, Direction, SampleType,
    StreamMode, WaveFormat,
};

#[cfg(windows)]
use crate::devnode::container_of_endpoint;
#[cfg(windows)]
pub use crate::devnode::{container_of_interface, ContainerId};
#[cfg(target_os = "linux")]
pub use linux::{container_of_interface, ContainerId};

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
    /// Start the render thread on the audio endpoint of the pad in `container`
    /// (see [`container_of_interface`]). With `None` — the pad's container
    /// couldn't be read — Windows takes the first DualSense-named endpoint,
    /// which can be the wrong pad's or a virtual one's, and Linux gives up.
    /// The thread exits if no endpoint matches (controller not on USB, or its
    /// audio driver hasn't enumerated yet).
    pub fn open(container: Option<&ContainerId>) -> Option<Self> {
        let container = container.cloned();
        let shared = Arc::new(Shared {
            targets: AtomicU32::new(0),
            targets2: AtomicU32::new(0),
            running: AtomicBool::new(true),
        });
        let shared_for_thread = shared.clone();

        let thread = thread::Builder::new()
            .name("dualsense-haptic".into())
            .spawn(move || {
                #[cfg(windows)]
                let result = render_loop(shared_for_thread, container);
                #[cfg(target_os = "linux")]
                let result = linux::render_loop(shared_for_thread, container);
                if let Err(e) = result {
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

// ── synthesis ─────────────────────────────────────────────────────────────────

/// Both carriers per LRA as continuous-phase sines, retuned each period from
/// the latest targets. The two carriers are summed per actuator and kept
/// within the drive ceiling, so two loud ones don't clip past the safe
/// amplitude.
#[derive(Default)]
struct Synth {
    /// Per sine — left LF, right LF, left HF, right HF — its phase, its phase
    /// step per sample and its drive, the last two for the current period.
    phase: [f32; 4],
    step: [f32; 4],
    drive: [f32; 4],
}

impl Synth {
    /// Take the latest amp/freq snapshot, both carriers.
    fn retune(&mut self, shared: &Shared, sample_rate: f32) {
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
        self.step = [l1_freq, r1_freq, l2_freq, r2_freq].map(|f| std::f32::consts::TAU * hz(f) / sample_rate);
        self.drive = [l1_amp, r1_amp, l2_amp, r2_amp].map(|a| a * DRIVE_CEILING);
    }

    /// The next sample for the left and right LRA, in -1..1.
    fn next(&mut self) -> (f32, f32) {
        let s = |i: usize| self.drive[i] * self.phase[i].sin();
        let l = (s(0) + s(2)).clamp(-1.0, 1.0);
        let r = (s(1) + s(3)).clamp(-1.0, 1.0);
        for (phase, step) in self.phase.iter_mut().zip(self.step) {
            *phase += step;
            if *phase > std::f32::consts::TAU {
                *phase -= std::f32::consts::TAU;
            }
        }
        (l, r)
    }
}

// ── render thread ─────────────────────────────────────────────────────────────

#[cfg(windows)]
fn render_loop(shared: Arc<Shared>, container: Option<ContainerId>) -> Result<(), String> {
    // COM init for this thread (MTA — matches what the wasapi crate documents).
    // Ignored if already initialized.
    let _ = initialize_mta();

    let (client, render, fmt, event) = open_dualsense_stream(container)
        .ok_or_else(|| "no DualSense audio endpoint found".to_string())?;

    let sample_rate = fmt.get_samplespersec() as f32;
    let channels = fmt.get_nchannels() as usize;
    let bytes_per_frame = fmt.get_blockalign() as usize;
    let sample_type = fmt.get_subformat().map_err(|e| format!("subformat: {e}"))?;

    client.start_stream().map_err(|e| format!("start: {e}"))?;

    let mut synth = Synth::default();
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

        synth.retune(&shared, sample_rate);
        for f in 0..frames {
            let (sl, sr) = synth.next();
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
#[cfg(windows)]
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

/// Walk active render endpoints and return the pad's as a fully-initialized
/// event-driven shared-mode stream: the 4-channel endpoint in `container`.
///
/// Without a container, the first 4-channel endpoint whose friendly name
/// contains "Wireless Controller" (Sony's USB Audio product name, shared by
/// DS4 and DualSense) or "DualSense" (some Windows drivers).
#[cfg(windows)]
fn open_dualsense_stream(
    container: Option<ContainerId>,
) -> Option<(AudioClient, AudioRenderClient, WaveFormat, wasapi::Handle)> {
    let collection = DeviceCollection::new(&Direction::Render).ok()?;
    let count = collection.get_nbr_devices().ok()?;

    for i in 0..count {
        let Ok(device) = collection.get_device_at_index(i) else { continue; };
        let ours = match container {
            Some(want) => device.get_id().ok()
                .and_then(|id| container_of_endpoint(&id))
                .is_some_and(|c| c == want),
            None => {
                let name = device.get_friendlyname().unwrap_or_default();
                name.contains("Wireless Controller") || name.contains("DualSense")
            }
        };
        if !ours {
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

// ── Linux: ALSA ───────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Shared, Synth};

    /// The pad's USB device, as its sysfs directory: its sound card hangs off
    /// the same one.
    pub type ContainerId = PathBuf;

    /// The USB device behind a hidraw node (hidapi's `/dev/hidrawN`, or the
    /// private twin of a hidden pad, `/dev/flexinput/hidrawN`). `None` off USB
    /// (Bluetooth).
    pub fn container_of_interface(path: &str) -> Option<ContainerId> {
        let name = path.rsplit('/').next()?;
        let mut dir = std::fs::canonicalize(format!("/sys/class/hidraw/{name}/device")).ok()?;
        loop {
            if dir.join("idVendor").exists() {
                return Some(dir);
            }
            if !dir.pop() || dir.as_os_str() == "/sys/devices" {
                return None;
            }
        }
    }

    /// The number of the sound card on the USB device `usb`.
    fn sound_card_of(usb: &Path) -> Option<u32> {
        std::fs::read_dir("/sys/class/sound").ok()?.flatten().find_map(|e| {
            let n: u32 = e.file_name().to_str()?.strip_prefix("card")?.parse().ok()?;
            // A card's device is the USB interface it was made for.
            let interface = std::fs::canonicalize(e.path().join("device")).ok()?;
            (interface.parent()? == usb).then_some(n)
        })
    }

    const RATE: u32 = 48_000;
    const CHANNELS: usize = 4;
    /// 5 ms periods, four buffered: about the Windows path's 10 ms
    /// shared-mode buffer.
    const PERIOD: usize = 240;
    /// How soon a card that's busy or not there yet is tried again.
    const RETRY: Duration = Duration::from_millis(500);

    pub(super) fn render_loop(shared: Arc<Shared>, container: Option<ContainerId>) -> Result<(), String> {
        use alsa::pcm::{Access, Format, Frames, HwParams, PCM};
        use alsa::{Direction, ValueOr};

        let usb = container.ok_or("the pad isn't on USB")?;
        // Opened non-blocking: a card another program holds (PipeWire, while
        // something plays to the pad) fails at once instead of parking this
        // thread past `Drop`, and is tried again.
        let pcm = loop {
            if !shared.running.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Some(card) = sound_card_of(&usb) {
                if let Ok(pcm) = PCM::new(&format!("hw:{card},0"), Direction::Playback, true) {
                    break pcm;
                }
            }
            if !usb.exists() {
                return Err("the pad went away".into());
            }
            std::thread::sleep(RETRY);
        };
        {
            let e = |what: &str, e: alsa::Error| format!("{what}: {e}");
            let hwp = HwParams::any(&pcm).map_err(|x| e("hw params", x))?;
            hwp.set_access(Access::RWInterleaved).map_err(|x| e("access", x))?;
            hwp.set_format(Format::s16()).map_err(|x| e("format", x))?;
            hwp.set_channels(CHANNELS as u32).map_err(|x| e("channels", x))?;
            hwp.set_rate(RATE, ValueOr::Nearest).map_err(|x| e("rate", x))?;
            hwp.set_period_size_near(PERIOD as Frames, ValueOr::Nearest).map_err(|x| e("period", x))?;
            hwp.set_buffer_size_near(4 * PERIOD as Frames).map_err(|x| e("buffer", x))?;
            pcm.hw_params(&hwp).map_err(|x| e("hw params", x))?;
        }
        let rate = pcm.hw_params_current().and_then(|h| h.get_rate()).map_err(|e| format!("rate: {e}"))? as f32;
        let io = pcm.io_i16().map_err(|e| format!("io: {e}"))?;

        let mut synth = Synth::default();
        let mut buf = vec![0i16; PERIOD * CHANNELS];
        while shared.running.load(Ordering::Acquire) {
            // 100 ms ceiling, so unplugging the pad can't trap the thread.
            if let Err(e) = pcm.wait(Some(100)) {
                pcm.try_recover(e, true).map_err(|e| format!("wait: {e}"))?;
                continue;
            }
            let frames = match pcm.avail_update() {
                Ok(n) => (n.max(0) as usize).min(PERIOD),
                Err(e) => {
                    pcm.try_recover(e, true).map_err(|e| format!("avail: {e}"))?;
                    continue;
                }
            };
            if frames == 0 {
                continue;
            }
            synth.retune(&shared, rate);
            for frame in buf.chunks_exact_mut(CHANNELS).take(frames) {
                let (l, r) = synth.next();
                // Headphones silent, the actuators on channels 3 and 4.
                frame.copy_from_slice(&[0, 0, (l * 32767.0) as i16, (r * 32767.0) as i16]);
            }
            if let Err(e) = io.writei(&buf[..frames * CHANNELS]) {
                pcm.try_recover(e, true).map_err(|e| format!("write: {e}"))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synth_drives_only_the_targeted_actuator() {
        // Left LF carrier at full amplitude, mid band; everything else off.
        let shared = Shared {
            targets: AtomicU32::new(0xFF | (0x80 << 8)),
            targets2: AtomicU32::new(0),
            running: AtomicBool::new(true),
        };
        let mut synth = Synth::default();
        synth.retune(&shared, 48_000.0);
        let samples: Vec<(f32, f32)> = (0..480).map(|_| synth.next()).collect();
        let peak = samples.iter().map(|s| s.0.abs()).fold(0.0, f32::max);
        assert!((peak - DRIVE_CEILING).abs() < 0.01, "left peaks at the drive ceiling: {peak}");
        assert!(samples.iter().all(|s| s.1 == 0.0), "right silent");
    }
}
