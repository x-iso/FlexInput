//! `VirtualDevice` backend backed by HIDMaestro (plain-HID profiles).
//!
//! Bridges FlexInput's sink-pin signal graph to the `flexinput-hidmaestro`
//! crate: pin writes accumulate into a `PinState`, `flush()` encodes a HID
//! report through the profile and publishes it to the shared-memory input
//! section, and `poll_outputs()` drains the output ring for rumble.
//!
//! **Device lifetime / elevation:** creating the virtual device node and the
//! `Global\` shared sections requires elevation (see
//! `flexinput_hidmaestro::orchestrator`). That is owned by a separate elevated
//! helper process (Phase 4). This adapter therefore *opens* an already-created
//! input/output section by controller index; `is_connected()` reflects whether
//! the open succeeded. When the helper isn't running (or the device wasn't
//! created), the device is "disconnected" and silently drops writes — matching
//! how `VirtualXInput` behaves when ViGEmBus is absent.

use std::time::Instant;

use flexinput_core::Signal;
use flexinput_hidmaestro::encode::{encode_report_into, gip_from_state, PinState};
use flexinput_hidmaestro::{helper, InputSection, OutputSection, Profile};

use crate::{layouts, SinkPin, SourcePin};

/// Classification of one XUSB-companion output-ring frame for rumble purposes.
enum RumbleFrame {
    /// A real vibration command: latch these `(strong, weak)` amplitudes.
    Command(f32, f32),
    /// WGI's idle keep-alive heartbeat (`[00 00 00 00 02 01 01 00 00]`): the
    /// driver republishes it continuously between commands. It must NOT change
    /// the latched rumble (treating it as a stop made continuous vibration
    /// collapse to one short pulse per ~1 s command re-send).
    Idle,
    /// Not a vibration frame at all (e.g. the LED `…01` frame, or malformed).
    Other,
}

/// Classify an Xbox 360 vibration packet from the XUSB companion's output ring.
///
/// Layout confirmed empirically 2026-06-20 by sending known motor values through
/// BOTH rumble APIs and reading the forwarded bytes. **Motors are always at
/// `data[2]` (LEFT = strong/low-frequency) and `data[3]` (RIGHT = weak/high-
/// frequency), each already 0..255** (the high byte of the 16-bit
/// `wLeftMotorSpeed`/`wRightMotorSpeed`), with a `0x02` packet marker at
/// `data[4]`. Two frame lengths carry this same layout:
///   * **5-byte** `[00, 00, L, R, 02]` — the legacy XInput / `XInputSetState`
///     (and Steam) path via `IOCTL_XUSB_SET_STATE`. Always a real command.
///   * **9-byte** `[00, XX, L, R, 02, 01, 01, 00, 00]` — the WGI
///     `Gamepad.Vibration` (`put_Vibration`, browsers/UWP/web testers) path.
///     `XX` (data[1]) is a command marker (≠0 on a real set, incl. an explicit
///     stop like `00 F7 00 00 02…`), NOT a motor value. WGI also republishes an
///     **idle heartbeat** `[00 00 00 00 02 01 01 00 00]` (data[1]==0, motors==0)
///     continuously — that is keep-alive, not a stop.
///
/// So: a 9-byte frame with `data[1]==0` AND both motors 0 is [`RumbleFrame::Idle`]
/// (ignore); everything else with the `0x02` marker is a [`RumbleFrame::Command`]
/// to latch (a 5-byte frame is always a command — that path has no heartbeat).
fn classify_rumble_frame(data: &[u8]) -> RumbleFrame {
    if data.len() < 5 || data[4] != 0x02 {
        return RumbleFrame::Other;
    }
    let (strong, weak) = (data[2], data[3]);
    if data.len() >= 9 && data[1] == 0 && strong == 0 && weak == 0 {
        return RumbleFrame::Idle;
    }
    RumbleFrame::Command(strong as f32 / 255.0, weak as f32 / 255.0)
}

/// How long a non-zero XInput/WGI rumble level is held without a fresh non-zero
/// command before it decays to zero. WGI (`Gamepad.Vibration`) has NO explicit
/// stop — it conveys "off" by ceasing to re-send the command (~every 700 ms while
/// active) and reverting to an idle heartbeat. So we hold the level a bit longer
/// than that re-send gap, then decay. Long enough to ride out a missed re-send;
/// short enough that release feels responsive. (Steam's legacy path DOES send an
/// explicit `Command(0,0)`, which stops immediately regardless of this.)
const RUMBLE_HOLD: std::time::Duration = std::time::Duration::from_millis(900);

/// A HIDMaestro-backed virtual controller (plain-HID path: DS4 / DualSense).
pub struct HidMaestroDevice {
    id: String,
    display_name: String,
    profile: Profile,
    /// Original profile JSON, kept verbatim so the helper re-parses the exact
    /// same source (avoids any lossy reconstruction from parsed fields).
    profile_json: String,
    pins: PinState,
    /// Reused report buffer (profile.input_report_size bytes).
    report_buf: Vec<u8>,
    /// Open input section (write target). `None` when the device/helper isn't
    /// available — the adapter then no-ops, staying "disconnected".
    input: Option<InputSection>,
    /// Open output section (rumble/FFB ring). Best-effort; may be `None`.
    output: Option<OutputSection>,
    /// Latest decoded rumble (strong, weak) in 0.0..1.0.
    rumble: (f32, f32),
    /// When the last NON-ZERO XInput/WGI rumble command was seen. The XUSB path
    /// has no reliable explicit "stop" for WGI (it just stops re-sending the
    /// command and reverts to an idle heartbeat), so a non-zero `rumble` decays
    /// to zero if no fresh non-zero command arrives within `RUMBLE_HOLD`. `None`
    /// once rumble is zero. Companion (XInput) path only.
    last_rumble_cmd: Option<Instant>,
    /// Helper-managed device instance id (`ROOT\HIDClass\NNNN`). `Some` when
    /// this device was created via the helper and must be destroyed on drop.
    helper_instance_id: Option<String>,
    /// True when last tick published a peak that exceeded the real value; the
    /// next tick settles `rumble` to the latest actual frame value so a held
    /// peak doesn't buzz forever after a one-shot ping.
    rumble_settle_pending: bool,
    /// Latest actual (non-peak) rumble frame value, used to settle after a peak.
    rumble_latest: (f32, f32),
    /// Most recent FULL output frame from the game (kept so non-rumble feedback —
    /// lightbar/LEDs/triggers — can be decoded from a level-held report rather
    /// than peak-held like rumble). `None` until the game sends one.
    last_out_frame: Option<Vec<u8>>,
    /// Latest decoded non-rumble feedback (lightbar/LEDs/adaptive triggers).
    fb: DsFeedback,
}

/// Decoded non-rumble DualSense/DS4 feedback the game wrote to the virtual pad,
/// normalized to the 0–1 source-pin convention so it routes straight to a
/// physical pad's matching haptic input pins. All-zero = nothing driven.
#[derive(Clone, Copy, Default)]
struct DsFeedback {
    lightbar: (f32, f32, f32),
    player_led: f32,
    mic_led: f32,
    /// (mode, start, end, strength, freq) per trigger, each 0–1.
    trig_r: (f32, f32, f32, f32, f32),
    trig_l: (f32, f32, f32, f32, f32),
}

impl DsFeedback {
    /// Flatten to source-pin (id, Signal) pairs. Pins whose underlying field the
    /// profile doesn't declare still report 0.0 — harmless (a physical pad that
    /// lacks the pin drops it, and 0 = "off" otherwise).
    fn as_pins(&self) -> Vec<(&'static str, Signal)> {
        vec![
            ("lightbar_r", Signal::Float(self.lightbar.0)),
            ("lightbar_g", Signal::Float(self.lightbar.1)),
            ("lightbar_b", Signal::Float(self.lightbar.2)),
            ("player_led", Signal::Float(self.player_led)),
            ("mic_led", Signal::Float(self.mic_led)),
            ("trigger_r_mode", Signal::Float(self.trig_r.0)),
            ("trigger_r_start", Signal::Float(self.trig_r.1)),
            ("trigger_r_end", Signal::Float(self.trig_r.2)),
            ("trigger_r_strength", Signal::Float(self.trig_r.3)),
            ("trigger_r_freq", Signal::Float(self.trig_r.4)),
            ("trigger_l_mode", Signal::Float(self.trig_l.0)),
            ("trigger_l_start", Signal::Float(self.trig_l.1)),
            ("trigger_l_end", Signal::Float(self.trig_l.2)),
            ("trigger_l_strength", Signal::Float(self.trig_l.3)),
            ("trigger_l_freq", Signal::Float(self.trig_l.4)),
        ]
    }
}

impl HidMaestroDevice {
    /// Open a HIDMaestro device for `controller_index` from `profile_json`. The
    /// device node + sections must already exist (created by the elevated
    /// helper); this parses the profile and opens the sections to drive them.
    ///
    /// Returns `None` if `profile_json` is invalid. `id`/`display_name` follow
    /// FlexInput's virtual-device id scheme (e.g. `virtual.ds4`).
    pub fn open(
        id: impl Into<String>,
        display_name: impl Into<String>,
        profile_json: &str,
        controller_index: u32,
    ) -> Option<Self> {
        let profile = Profile::from_json(profile_json).ok()?;
        let report_buf = vec![0u8; profile.input_report_size];
        let input = InputSection::open(controller_index).ok();
        let output = OutputSection::open(controller_index).ok();
        Some(HidMaestroDevice {
            id: id.into(),
            display_name: display_name.into(),
            profile,
            profile_json: profile_json.to_string(),
            pins: PinState::new(),
            report_buf,
            input,
            output,
            rumble: (0.0, 0.0),
            helper_instance_id: None,
            rumble_settle_pending: false,
            rumble_latest: (0.0, 0.0),
            last_rumble_cmd: None,
            last_out_frame: None,
            fb: DsFeedback::default(),
        })
    }

    /// Create a HIDMaestro device end-to-end: ask the elevated helper to create
    /// the device node + `Global\` sections (spawning the helper on first use,
    /// one UAC), then open the sections here to drive it.
    ///
    /// Returns `None` if `profile_json` is invalid. On a helper/creation failure
    /// the device is returned "disconnected" (no input section) with the error
    /// logged — adding a device never panics the UI thread.
    pub fn create(
        id: impl Into<String>,
        display_name: impl Into<String>,
        profile_json: &str,
        index_hint: u32,
    ) -> Option<Self> {
        // open() with the hint first; the real index comes back from the helper
        // (it allocates a globally-unique one, or reclaims the existing device).
        let mut dev = Self::open(id, display_name, profile_json, index_hint)?;
        let device_id = dev.id.clone();
        // XUSB companion pump period from the app's polling-rate setting (0 =>
        // unset => helper leaves the driver's 125Hz default). Only the XInput
        // profile's companion uses it; harmless for plain-HID.
        let poll_interval_ms = crate::requested_poll_interval_ms();
        match helper::create(&device_id, &dev.profile_json, index_hint, poll_interval_ms) {
            Ok((instance_id, allocated_index)) => {
                // Open the sections at the index the helper actually used — NOT
                // the hint. (Opening the wrong index was the no-input bug: two
                // devices both guessed index 0 and collided.)
                dev.input = InputSection::open(allocated_index).ok();
                dev.output = OutputSection::open(allocated_index).ok();
                dev.helper_instance_id = Some(instance_id.clone());

                // First-launch readiness: on a fresh create the UMDF driver opens
                // its side of the section AFTER it binds. The helper now waits for
                // the HID child to reach the "Started" state before returning (see
                // orchestrator::wait_for_hid_child_started), so by here the driver
                // is listening. Prime a neutral frame so the driver's first
                // observed SeqNo transition is unambiguously ours.
                if let Some(input) = dev.input.as_mut() {
                    let state = dev.pins.state();
                    // Mirror flush(): for the XUSB companion send an empty HID
                    // Data[] (GIP is the sole source); otherwise encode the report.
                    let gip = dev
                        .profile
                        .requires_xusb_companion
                        .then(|| gip_from_state(&state));
                    let data: &[u8] = if gip.is_some() {
                        dev.report_buf.fill(0);
                        &dev.report_buf[..]
                    } else {
                        encode_report_into(&dev.profile, &state, &mut dev.report_buf);
                        if dev.profile.report.report_id != 0 {
                            &dev.report_buf[1..]
                        } else {
                            &dev.report_buf[..]
                        }
                    };
                    input.write_frame(data, gip.as_ref());
                }
            }
            Err(e) => {
                eprintln!("[hidmaestro] create via helper failed for {}: {e}", dev.id);
            }
        }
        Some(dev)
    }

    /// Publish a raw, pre-built input report to the SHM frame, bypassing the
    /// gamepad PinState/encode path. For profiles whose reports the gamepad
    /// model doesn't describe — the virtual keyboard/mouse (see
    /// `keymouse_hm`). `data` must be `input_report_size` bytes of RID-less
    /// payload, exactly as the profile descriptor lays it out. No-op while
    /// disconnected (no input section), matching `flush()`.
    pub fn write_raw_input(&mut self, data: &[u8]) {
        if let Some(input) = self.input.as_mut() {
            input.write_frame(data, None);
        }
    }

    /// Which static sink-pin layout to advertise for `profile`. Xbox360/XInput
    /// uses the XInput pin set (no gyro/touchpad/lightbar); DualSense uses its
    /// full Sony set; everything else falls back to DS4 pins.
    fn sink_pins_for(&self) -> &'static [SinkPin] {
        if self.profile.requires_xusb_companion {
            layouts::XINPUT_SINK_PINS
        } else if self.profile.id.contains("dualsense") {
            layouts::DUALSENSE_SINK_PINS
        } else {
            layouts::DS4_SINK_PINS
        }
    }
}

impl crate::VirtualDevice for HidMaestroDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn sink_pins(&self) -> &'static [SinkPin] {
        self.sink_pins_for()
    }

    fn send(&mut self, pin: &str, value: Signal) {
        // Vec2 pins decompose into the x/y accumulator entries; scalar pins use
        // the float/bool views and PinState picks the right one.
        match (pin, value) {
            ("left_stick", Signal::Vec2(v)) => {
                self.pins.set("left_stick_x", v.x, true);
                self.pins.set("left_stick_y", v.y, true);
            }
            ("right_stick", Signal::Vec2(v)) => {
                self.pins.set("right_stick_x", v.x, true);
                self.pins.set("right_stick_y", v.y, true);
            }
            ("dpad", Signal::Vec2(v)) => {
                self.pins.set("dpad_left", 0.0, v.x < -0.5);
                self.pins.set("dpad_right", 0.0, v.x > 0.5);
                self.pins.set("dpad_up", 0.0, v.y > 0.5);
                self.pins.set("dpad_down", 0.0, v.y < -0.5);
            }
            _ => {
                self.pins.set(pin, value.as_float(), value.as_bool());
            }
        }
    }

    fn flush(&mut self) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        let state = self.pins.state();
        // For XInput/Xbox360 pads the XUSB companion derives state SOLELY from the
        // GipData[14] region — and it ALSO reads the HID Data[] for its sibling
        // node. If we publish a populated HID report there too, the descriptor's
        // (different) byte layout bleeds into XInput state (symptom: only the first
        // axis tracks, everything else garbage, stick deflection clipped square).
        // So for the companion path send an EMPTY Data[] and let GIP be the only
        // source — exactly what the validated self-test does (`[0u8; N]` report).
        let gip = self
            .profile
            .requires_xusb_companion
            .then(|| gip_from_state(&state));
        let data: &[u8] = if gip.is_some() {
            // Neutral HID payload: zeros. (Length is cosmetic for the companion;
            // keep it the report size so the sibling node sees a sane frame.)
            self.report_buf.fill(0);
            &self.report_buf[..]
        } else {
            encode_report_into(&self.profile, &state, &mut self.report_buf);
            // The SHM frame strips the Report-ID byte (driver re-prepends it).
            if self.profile.report.report_id != 0 {
                &self.report_buf[1..]
            } else {
                &self.report_buf[..]
            }
        };
        input.write_frame(data, gip.as_ref());
    }

    fn reset_outputs(&mut self) {
        self.pins.reset();
        self.flush();
    }

    fn is_connected(&self) -> bool {
        self.input.is_some()
    }

    fn source_pins(&self) -> &'static [SourcePin] {
        if self.profile.requires_xusb_companion {
            // XInput feedback is rumble strong/weak only (no lightbar).
            layouts::XINPUT_SOURCE_PINS
        } else if self.profile.id.contains("dualsense") {
            // DualSense adds player/mic LEDs + both adaptive triggers on top of
            // the DS4 rumble+lightbar set.
            layouts::DUALSENSE_SOURCE_PINS
        } else {
            layouts::DS4_SOURCE_PINS
        }
    }

    fn persist_on_drop(&mut self) {
        // Forget the helper-created node so Drop won't call helper::destroy:
        // the node stays alive past app exit and is reclaimed (by device_id)
        // on next launch. Closing the section handles here is fine — the helper
        // keeps the node; we reopen on reclaim.
        self.helper_instance_id = None;
        self.input = None;
        self.output = None;
    }

    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> {
        if self.profile.requires_xusb_companion {
            // XInput (Xbox 360) rumble-in. The driver forwards the game's
            // vibration packet to the output ring as Source=XINPUT(2).
            // `classify_rumble_frame` parses it (layout confirmed empirically).
            //
            // Rumble is LEVEL-triggered, latched with a TIMEOUT, because the two
            // APIs signal "stop" differently:
            //   * A non-zero `Command` latches the level AND refreshes a hold
            //     timer. WGI re-sends the command ~every 700 ms while active, so
            //     the timer keeps refreshing → sustained vibration.
            //   * Steam's legacy path sends an explicit `Command(0,0)` on release
            //     → we zero immediately.
            //   * WGI has NO explicit stop: it just CEASES re-sending and reverts
            //     to the idle heartbeat. `Idle` is therefore ignored here (it must
            //     not refresh the timer), and a non-zero level that goes
            //     un-refreshed for `RUMBLE_HOLD` decays to zero — that's how WGI
            //     "off" is detected.
            if let Some(output) = self.output.as_mut() {
                while let Some(frame) = output.try_read() {
                    match classify_rumble_frame(&frame.data) {
                        RumbleFrame::Command(s, w) => {
                            self.rumble = (s, w);
                            self.last_rumble_cmd =
                                if s > 0.0 || w > 0.0 { Some(Instant::now()) } else { None };
                        }
                        RumbleFrame::Idle | RumbleFrame::Other => {}
                    }
                }
            }
            // Decay a stale non-zero level to zero once the hold window lapses
            // with no fresh non-zero command (covers WGI's implicit stop).
            if let Some(t) = self.last_rumble_cmd {
                if t.elapsed() >= RUMBLE_HOLD {
                    self.rumble = (0.0, 0.0);
                    self.last_rumble_cmd = None;
                }
            }
            return vec![
                ("rumble_strong", Signal::Float(self.rumble.0)),
                ("rumble_weak", Signal::Float(self.rumble.1)),
            ];
        }
        // Drain the output ring; keep the latest rumble report we recognize.
        // Motor byte offsets differ between DS4 (right@4/left@5) and DualSense
        // (right@3/left@4), so we read them from the profile's
        // `extendedOutputReport` rather than hardcoding. Those offsets are
        // RID-inclusive; the ring strips the report id, so within `data` a motor
        // at byte `o` sits at index `o-1`.
        let (left_idx, right_idx) = (
            self.profile.extended.out_left_motor.map(|o| o.saturating_sub(1)),
            self.profile.extended.out_right_motor.map(|o| o.saturating_sub(1)),
        );
        // Drain all frames queued since the last tick, keeping the PEAK rumble
        // seen this tick — not just the last frame. A short ping enqueues an ON
        // frame immediately followed by an OFF frame; if we kept only the last,
        // one poll drains both and publishes (0,0), so the consumer (a physical
        // pad via AutoMap feedback) never sees the pulse. This is the divergence
        // from the XInput backend, whose async notification lets the ON value
        // linger. We publish the peak for one tick, then settle to the latest
        // actual value so a held peak doesn't buzz forever once frames stop.
        let mut peak: Option<(f32, f32)> = None;
        if let Some(output) = self.output.as_mut() {
            while let Some(frame) = output.try_read() {
                let strong = left_idx.and_then(|i| frame.data.get(i)).map(|b| *b as f32 / 255.0);
                let weak = right_idx.and_then(|i| frame.data.get(i)).map(|b| *b as f32 / 255.0);
                if let (Some(s), Some(w)) = (strong, weak) {
                    let (ps, pw) = peak.unwrap_or((0.0, 0.0));
                    peak = Some((s.max(ps), w.max(pw)));
                    self.rumble_latest = (s, w);
                }
                // Keep the latest full frame for non-rumble feedback decode
                // (lightbar/LEDs/triggers are level-held, so the most recent
                // report is authoritative — no peak-holding needed).
                self.last_out_frame = Some(frame.data.clone());
            }
        }
        if let Some((pk_s, pk_w)) = peak {
            let (lt_s, lt_w) = self.rumble_latest;
            // Hold the peak one tick when it exceeds the settled value.
            self.rumble_settle_pending = pk_s > lt_s + 0.01 || pk_w > lt_w + 0.01;
            self.rumble = (pk_s, pk_w);
        } else if self.rumble_settle_pending {
            // No new frames, but we owe a settle from a prior peak.
            self.rumble_settle_pending = false;
            self.rumble = self.rumble_latest;
        }
        let mut out = vec![
            ("rumble_strong", Signal::Float(self.rumble.0)),
            ("rumble_weak", Signal::Float(self.rumble.1)),
        ];
        // Non-rumble feedback (lightbar / LEDs / adaptive triggers). The game's
        // last output report is the authoritative state for these LEVEL-held
        // fields (unlike rumble, which needs peak-holding because a ping's
        // on→off pair can drain in one tick). We keep the most recent decoded
        // values in `self.fb` and refresh them from `last_out_frame` here.
        self.decode_nonrumble_feedback();
        out.extend(self.fb.as_pins());
        out
    }

    fn inject_rumble_ping(&mut self, strong: f32, weak: f32) {
        // Set the level directly so the next poll_outputs() reports it. The ping
        // handler drives this each tick (level while active, 0 when done), so we
        // must NOT arm the auto-decay timer — that's only for real game commands
        // whose "stop" is implicit. Clearing last_rumble_cmd also prevents the
        // companion path's decay from fighting the ping. For an xinput-companion
        // device this feeds the same `self.rumble` poll_outputs returns; for a
        // plain-HID (DS4/DS) device it likewise overrides until the next real
        // frame, which is fine for a momentary ping.
        self.rumble = (strong, weak);
        self.last_rumble_cmd = None;
    }
}

impl HidMaestroDevice {
    /// Decode the game's latest output report into `self.fb` (lightbar / LEDs /
    /// adaptive triggers), using the profile's RID-inclusive output offsets. The
    /// ring strips the report id, so an offset `o` sits at `data[o-1]`. Fields the
    /// profile doesn't declare stay at their default (0 = off). Called from
    /// `poll_outputs` after the rumble drain so `last_out_frame` is fresh.
    fn decode_nonrumble_feedback(&mut self) {
        let Some(data) = self.last_out_frame.as_deref() else { return };
        let ext = &self.profile.extended;
        let at = |o: Option<usize>| o.and_then(|o| o.checked_sub(1)).filter(|&i| i < data.len());

        if let Some(i) = at(ext.out_lightbar_r) {
            // rgb24: R@i, G@i+1, B@i+2.
            let g = |k: usize| data.get(k).copied().unwrap_or(0) as f32 / 255.0;
            self.fb.lightbar = (g(i), g(i + 1), g(i + 2));
        }
        if let Some(i) = at(ext.out_player_led) {
            self.fb.player_led = decode_player_led(data[i]);
        }
        if let Some(i) = at(ext.out_mic_led) {
            // DualSense mute-LED byte: 0=off, 1=on(solid), 2=pulse → 0 / 0.5 / 1.
            self.fb.mic_led = (data[i] as f32 / 2.0).clamp(0.0, 1.0);
        }
        if let Some(i) = at(ext.out_trigger_r) {
            self.fb.trig_r = decode_trigger_effect(&data[i..]);
        }
        if let Some(i) = at(ext.out_trigger_l) {
            self.fb.trig_l = decode_trigger_effect(&data[i..]);
        }
    }
}

/// Map a DualSense player-indicator bitmask byte back to the 0–1 `player_led`
/// convention (0=off, 0.25=P1 … 1=P4). The pad lights 1–4 of its 5 front LEDs in
/// fixed symmetric patterns per player number; we recognise those, and fall back
/// to "scale by lit-count" for anything non-standard. Low 5 bits are the LEDs.
fn decode_player_led(b: u8) -> f32 {
    match b & 0x1F {
        0x00 => 0.0,        // off
        0x04 => 0.25,       // P1: centre
        0x0A => 0.5,        // P2: inner pair
        0x15 => 0.75,       // P3: centre + outer pair
        0x1B => 1.0,        // P4: two pairs
        other => {
            // Non-standard: approximate by how many LEDs are lit (1..5 → ~0.25..1).
            let n = other.count_ones() as f32;
            (n / 4.0).clamp(0.0, 1.0)
        }
    }
}

/// Decode one 11-byte DualSense adaptive-trigger effect block back into
/// `(mode, start, end, strength, freq)`, each normalized 0–1 — the inverse of
/// `flexinput_devices::gyro::encode_trigger_effect`. `mode` 0/0.33/0.66/1 maps to
/// Off/Feedback(0x21)/Weapon(0x25)/Vibration(0x26); `start`/`end` are zone 0–9
/// (→ /9); `strength` is force 0–7 (→ /7); `freq` is 0–255 (→ /255, Vibration).
/// A short/empty block decodes as Off. This is intentionally the rough inverse —
/// it recovers the parameters the encoder packs, enough to forward the effect to
/// a physical DualSense (which re-encodes them).
fn decode_trigger_effect(block: &[u8]) -> (f32, f32, f32, f32, f32) {
    let off = (0.0, 0.0, 0.0, 0.0, 0.0);
    if block.len() < 7 {
        return off;
    }
    // Lowest set bit of the 10-bit active-zone field (params byte 1..3) → start.
    let active = (block[1] as u16) | ((block[2] as u16) << 8);
    let start_zone: f32 = if active == 0 { 0.0 } else { active.trailing_zeros() as f32 };
    // Force is a 3-bit value repeated per active zone (params byte 3..7). Read the
    // 3 bits at the start zone's slot.
    let force_bits = (block[3] as u32)
        | ((block[4] as u32) << 8)
        | ((block[5] as u32) << 16)
        | ((block[6] as u32) << 24);
    let force_at = |zone: u32| ((force_bits >> (zone * 3)) & 0x7) as f32;

    match block[0] {
        0x21 => {
            // Feedback: constant resistance from start zone, force = strength.
            (0.33, (start_zone / 9.0).clamp(0.0, 1.0), 0.0, (force_at(start_zone as u32) / 7.0).clamp(0.0, 1.0), 0.0)
        }
        0x25 => {
            // Weapon: two set bits in start_end (byte1..3) = start, end; byte3 = strength.
            let lo = if active == 0 { 0 } else { active.trailing_zeros() };
            let hi = if active == 0 { 0 } else { 15 - active.leading_zeros() };
            let strength = block.get(3).copied().unwrap_or(0).min(7) as f32;
            ((0.66), (lo as f32 / 9.0).clamp(0.0, 1.0), (hi as f32 / 9.0).clamp(0.0, 1.0), (strength / 7.0).clamp(0.0, 1.0), 0.0)
        }
        0x26 => {
            // Vibration: like Feedback + frequency at params byte 9 (block[9]).
            let freq = block.get(9).copied().unwrap_or(0) as f32 / 255.0;
            (1.0, (start_zone / 9.0).clamp(0.0, 1.0), 0.0, (force_at(start_zone as u32) / 7.0).clamp(0.0, 1.0), freq.clamp(0.0, 1.0))
        }
        _ => off, // 0x00 / 0x05 (off) and anything unrecognized.
    }
}

impl Drop for HidMaestroDevice {
    fn drop(&mut self) {
        // If this device was created via the helper, ask it to tear the node
        // down. Drop the section handles first so the helper's removal isn't
        // blocked by our mapped views.
        if let Some(id) = self.helper_instance_id.take() {
            self.input = None;
            self.output = None;
            if let Err(e) = helper::destroy(&id) {
                eprintln!("[hidmaestro] destroy via helper failed for {id}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualDevice;
    use flexinput_core::Signal;
    use flexinput_hidmaestro::profile::presets::DUALSHOCK_4_V2_JSON;
    use glam::Vec2;

    fn ds4_device() -> HidMaestroDevice {
        // controller index that almost certainly has no live section in CI →
        // input/output open as None; we test the pin→encode path, not the SHM.
        // open() (not create()) so no helper is spawned.
        HidMaestroDevice::open("virtual.ds4", "Virtual DualShock 4", DUALSHOCK_4_V2_JSON, 250)
            .expect("valid DS4 profile")
    }

    #[test]
    fn disconnected_when_no_section() {
        let dev = ds4_device();
        assert!(!dev.is_connected(), "no live section → disconnected");
    }

    #[test]
    fn advertises_ds4_sink_pins() {
        let dev = ds4_device();
        let pins = dev.sink_pins();
        assert!(pins.iter().any(|p| p.id == "left_stick"));
        assert!(pins.iter().any(|p| p.id == "btn_south"));
    }

    fn dualsense_device() -> HidMaestroDevice {
        HidMaestroDevice::open(
            "virtual.dualsense", "Virtual DualSense",
            flexinput_hidmaestro::profile::presets::DUALSENSE_JSON, 251,
        ).expect("valid DualSense profile")
    }

    // The DualSense virtual exposes LED + adaptive-trigger feedback source pins
    // (so a game driving the virtual can route them to a physical pad). DS4 does
    // not (no such hardware).
    #[test]
    fn dualsense_exposes_led_and_trigger_source_pins() {
        let ds = dualsense_device();
        let ids: Vec<&str> = ds.source_pins().iter().map(|p| p.id).collect();
        for want in ["player_led", "mic_led", "trigger_r_mode", "trigger_l_freq", "lightbar_r"] {
            assert!(ids.contains(&want), "DualSense source pins missing {want}");
        }
        // DS4 stays rumble + lightbar only.
        let ds4_ids: Vec<&str> = ds4_device().source_pins().iter().map(|p| p.id).collect();
        assert!(!ds4_ids.contains(&"player_led"), "DS4 must not expose player_led");
        assert!(!ds4_ids.contains(&"trigger_r_mode"), "DS4 must not expose triggers");
    }

    #[test]
    fn dualsense_parses_output_feedback_offsets() {
        let ds = dualsense_device();
        let e = &ds.profile.extended;
        // From the DualSense output report: lightbar@45, playerIndicator@44,
        // muteLed@9, trigger effects @11 / @22.
        assert_eq!(e.out_lightbar_r, Some(45));
        assert_eq!(e.out_player_led, Some(44));
        assert_eq!(e.out_mic_led, Some(9));
        assert_eq!(e.out_trigger_r, Some(11));
        assert_eq!(e.out_trigger_l, Some(22));
    }

    // Replicate the physical-side `encode_trigger_effect` bit-packing exactly so
    // `decode_trigger_effect` is verified against the real wire format. (Encoder
    // lives in flexinput-devices, so we re-encode here rather than cross-call.)
    fn encode_feedback(start: u8, strength: u8) -> [u8; 11] {
        let mut p = [0u8; 11];
        p[0] = 0x21;
        let s = start.min(9) as u16;
        let f = strength.min(7) as u32;
        let active: u16 = if s < 10 { ((1u16 << (10 - s)) - 1) << s } else { 0 };
        let mut force = 0u32;
        for z in s..10 { force |= f << (z * 3); }
        p[1] = (active & 0xFF) as u8;
        p[2] = ((active >> 8) & 0xFF) as u8;
        p[3] = (force & 0xFF) as u8;
        p[4] = ((force >> 8) & 0xFF) as u8;
        p[5] = ((force >> 16) & 0xFF) as u8;
        p[6] = ((force >> 24) & 0xFF) as u8;
        p
    }

    #[test]
    fn decode_trigger_feedback_recovers_params() {
        // Feedback mode, start zone 3, strength 5.
        let block = encode_feedback(3, 5);
        let (mode, start, _end, strength, freq) = decode_trigger_effect(&block);
        assert!((mode - 0.33).abs() < 0.01, "mode = Feedback");
        assert!((start - 3.0 / 9.0).abs() < 0.02, "start zone 3, got {start}");
        assert!((strength - 5.0 / 7.0).abs() < 0.02, "strength 5, got {strength}");
        assert_eq!(freq, 0.0, "feedback has no freq");
        // Off block decodes to all-zero.
        assert_eq!(decode_trigger_effect(&[0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), (0.0, 0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn decode_player_led_known_patterns() {
        assert_eq!(decode_player_led(0x00), 0.0);
        assert_eq!(decode_player_led(0x04), 0.25); // P1
        assert_eq!(decode_player_led(0x1B), 1.0);  // P4
    }

    #[test]
    fn pin_writes_drive_the_encoder() {
        // Exercise send() → PinState → encoder, independent of the SHM section.
        let mut dev = ds4_device();
        dev.send("left_stick", Signal::Vec2(Vec2::new(1.0, 0.0))); // full right
        dev.send("btn_south", Signal::Bool(true));

        let state = dev.pins.state();
        let mut buf = vec![0u8; dev.profile.input_report_size];
        encode_report_into(&dev.profile, &state, &mut buf);
        // X (left_stick_x = +1 → 0xFF) at byte 1 (after RID byte 0x01).
        assert_eq!(buf[0], 0x01, "report id");
        assert_eq!(buf[1], 255, "left_stick full right → X max");
        // Cross (btn_south) → DS4 buttonMap[0]=1 → Btn2 at bit 37 → byte 5 bit 5.
        assert_eq!(buf[5] & (1 << 5), 1 << 5, "btn_south sets Cross");
    }

    #[test]
    fn flush_is_noop_when_disconnected() {
        // Must not panic when there is no input section.
        let mut dev = ds4_device();
        dev.send("left_stick", Signal::Vec2(Vec2::new(0.5, -0.5)));
        dev.flush();
        dev.reset_outputs();
    }

    // ── XInput rumble decode (layout verified live 2026-06-20) ────────────────

    fn cmd(data: &[u8]) -> Option<(f32, f32)> {
        match classify_rumble_frame(data) {
            RumbleFrame::Command(s, w) => Some((s, w)),
            _ => None,
        }
    }
    fn is_idle(data: &[u8]) -> bool {
        matches!(classify_rumble_frame(data), RumbleFrame::Idle)
    }
    fn is_other(data: &[u8]) -> bool {
        matches!(classify_rumble_frame(data), RumbleFrame::Other)
    }

    #[test]
    fn xinput_5byte_frame_is_command() {
        // 5-byte legacy XInput/XInputSetState (Steam) path: [00 00 L R 02].
        // Observed live for (L,R) = (0xFFFF,0), (0,0xFFFF), (0x1111,0x8888).
        assert_eq!(cmd(&[0, 0, 255, 0, 2]), Some((1.0, 0.0)));
        assert_eq!(cmd(&[0, 0, 0, 255, 2]), Some((0.0, 1.0)));
        let (s, w) = cmd(&[0, 0, 0x11, 0x88, 2]).unwrap();
        assert!((s - 17.0 / 255.0).abs() < 1e-6);
        assert!((w - 136.0 / 255.0).abs() < 1e-6);
        // A 5-byte all-zero frame IS a real stop command (no heartbeat on this path).
        assert_eq!(cmd(&[0, 0, 0, 0, 2]), Some((0.0, 0.0)));
    }

    #[test]
    fn wgi_9byte_command_vs_idle_heartbeat() {
        // Real WGI command: [00 XX L R 02 01 01 00 00], motors at [2]/[3], data[1]
        // is a command marker (≠0), NOT a motor. Observed `00 67 FF FF ...`.
        assert_eq!(cmd(&[0, 0x67, 0xFF, 0xFF, 2, 1, 1, 0, 0]), Some((1.0, 1.0)));
        // Explicit WGI stop (data[1]≠0, motors 0) IS a command (clears rumble).
        assert_eq!(cmd(&[0, 0xF7, 0, 0, 2, 1, 1, 0, 0]), Some((0.0, 0.0)));
        // Idle keep-alive heartbeat (data[1]==0, motors 0): IGNORED, not a stop.
        assert!(is_idle(&[0, 0, 0, 0, 2, 1, 1, 0, 0]));
    }

    #[test]
    fn rejects_non_vibration_frames() {
        // Gate is the 0x02 marker at data[4]. An LED/other frame (`…01` marker)
        // and malformed/short frames are Other (ignored).
        assert!(is_other(&[0, 6, 0, 0, 1])); // marker 0x01
        assert!(is_other(&[0, 0, 255])); // too short
        assert!(is_other(&[]));
    }
}
