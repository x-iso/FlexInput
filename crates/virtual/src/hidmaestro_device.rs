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

use flexinput_core::Signal;
use flexinput_hidmaestro::encode::{encode_report_into, gip_from_state, PinState};
use flexinput_hidmaestro::{helper, InputSection, OutputSection, Profile};

use crate::{layouts, SinkPin, SourcePin};

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
    /// Helper-managed device instance id (`ROOT\HIDClass\NNNN`). `Some` when
    /// this device was created via the helper and must be destroyed on drop.
    helper_instance_id: Option<String>,
    /// True when last tick published a peak that exceeded the real value; the
    /// next tick settles `rumble` to the latest actual frame value so a held
    /// peak doesn't buzz forever after a one-shot ping.
    rumble_settle_pending: bool,
    /// Latest actual (non-peak) rumble frame value, used to settle after a peak.
    rumble_latest: (f32, f32),
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
        // XInput/Xbox360 rumble-in is NOT YET MAPPED. The XUSB companion's output
        // ring layout is unknown (the offsets tried were a guess), and reading
        // unmapped bytes produced a STUCK nonzero rumble that AutoMap forwarded to
        // the physical pad (constant buzz, x360 ports never lit because it was a
        // synthesized garbage value, not a real frame). Until the ring layout is
        // mapped empirically (same treatment as the input GIP), report NO rumble
        // for the companion path — silence beats a phantom constant buzz.
        if self.profile.requires_xusb_companion {
            return vec![
                ("rumble_strong", Signal::Float(0.0)),
                ("rumble_weak", Signal::Float(0.0)),
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
        vec![
            ("rumble_strong", Signal::Float(self.rumble.0)),
            ("rumble_weak", Signal::Float(self.rumble.1)),
        ]
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
}
