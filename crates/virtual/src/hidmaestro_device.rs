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
use flexinput_hidmaestro::encode::{encode_report_into, PinState};
use flexinput_hidmaestro::{helper, InputSection, OutputSection, Profile};

use crate::{layouts, SinkPin, SourcePin};

/// Append a one-line diagnostic to `flexinput-hidmaestro.log` next to the exe.
/// Temporary instrumentation for the input-path investigation; works in the
/// console-less release build where `eprintln!` goes nowhere.
fn diag_log(line: &str) {
    use std::io::Write;
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("flexinput-hidmaestro.log")));
    if let Some(path) = path {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
    eprintln!("{line}");
}

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
    /// Controller index this device opened (for diagnostics).
    controller_index: u32,
    /// One-shot diagnostic: log the first non-neutral frame we write.
    diag_logged: bool,
    /// Throttle timestamp for the rumble diagnostic.
    diag_last_rumble: Option<std::time::Instant>,
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
            controller_index,
            diag_logged: false,
            diag_last_rumble: None,
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
        match helper::create(&device_id, &dev.profile_json, index_hint) {
            Ok((instance_id, allocated_index)) => {
                // Open the sections at the index the helper actually used — NOT
                // the hint. (Opening the wrong index was the no-input bug: two
                // devices both guessed index 0 and collided.)
                dev.controller_index = allocated_index;
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
                    encode_report_into(&dev.profile, &state, &mut dev.report_buf);
                    let data = if dev.profile.report.report_id != 0 {
                        &dev.report_buf[1..]
                    } else {
                        &dev.report_buf[..]
                    };
                    input.write_frame(data, None);
                }
                diag_log(&format!(
                    "[hidmaestro] create id={} hint={} alloc_idx={} instance={} input_open={} output_open={}",
                    dev.id, index_hint, allocated_index, instance_id,
                    dev.input.is_some(), dev.output.is_some()
                ));
            }
            Err(e) => {
                diag_log(&format!(
                    "[hidmaestro] create via helper FAILED id={} hint={}: {e}",
                    dev.id, index_hint
                ));
            }
        }
        Some(dev)
    }

    /// Which static sink-pin layout to advertise for `profile`. DS4 / DualSense
    /// share the DS4 pin set (sticks, triggers, face/shoulder buttons, d-pad,
    /// touchpad). Falls back to DS4 pins for any plain-HID gamepad.
    fn sink_pins_for(&self) -> &'static [SinkPin] {
        if self.profile.id.contains("dualsense") {
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
            if !self.diag_logged {
                diag_log(&format!(
                    "[hidmaestro] flush NO-OP (input section not open) id={} idx={}",
                    self.id, self.controller_index
                ));
                self.diag_logged = true;
            }
            return;
        };
        let state = self.pins.state();
        encode_report_into(&self.profile, &state, &mut self.report_buf);
        // The SHM input frame carries the report with its Report-ID byte
        // stripped (the driver re-prepends it). DS4 has Report ID 0x01 at byte 0.
        let data = if self.profile.report.report_id != 0 {
            &self.report_buf[1..]
        } else {
            &self.report_buf[..]
        };
        if !self.diag_logged {
            diag_log(&format!(
                "[hidmaestro] first flush id={} idx={} data[0..6]={:02x?}",
                self.id, self.controller_index, &data[..data.len().min(6)]
            ));
            self.diag_logged = true;
        }
        input.write_frame(data, None);
    }

    fn reset_outputs(&mut self) {
        self.pins.reset();
        self.flush();
    }

    fn is_connected(&self) -> bool {
        self.input.is_some()
    }

    fn source_pins(&self) -> &'static [SourcePin] {
        layouts::DS4_SOURCE_PINS
    }

    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> {
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
        let mut got_frame = false;
        if let Some(output) = self.output.as_mut() {
            while let Some(frame) = output.try_read() {
                let strong = left_idx.and_then(|i| frame.data.get(i)).map(|b| *b as f32 / 255.0);
                let weak = right_idx.and_then(|i| frame.data.get(i)).map(|b| *b as f32 / 255.0);
                if let (Some(s), Some(w)) = (strong, weak) {
                    let (ps, pw) = peak.unwrap_or((0.0, 0.0));
                    peak = Some((s.max(ps), w.max(pw)));
                    self.rumble_latest = (s, w);
                    got_frame = true;
                }
            }
        }
        let publish = if let Some((pk_s, pk_w)) = peak {
            let (lt_s, lt_w) = self.rumble_latest;
            // Hold the peak one tick when it exceeds the settled value.
            self.rumble_settle_pending = pk_s > lt_s + 0.01 || pk_w > lt_w + 0.01;
            Some((pk_s, pk_w))
        } else if self.rumble_settle_pending {
            // No new frames, but we owe a settle from a prior peak.
            self.rumble_settle_pending = false;
            Some(self.rumble_latest)
        } else {
            None
        };
        if let Some((strong, weak)) = publish {
            let changed = (strong - self.rumble.0).abs() > 0.01 || (weak - self.rumble.1).abs() > 0.01;
            if changed && (strong > 0.0 || weak > 0.0 || self.rumble.0 > 0.0 || self.rumble.1 > 0.0) {
                let now = std::time::Instant::now();
                if self.diag_last_rumble.map(|t| now.duration_since(t).as_millis() >= 300).unwrap_or(true) {
                    diag_log(&format!(
                        "[hidmaestro] rumble id={} strong={strong:.2} weak={weak:.2} got_frame={got_frame}",
                        self.id
                    ));
                    self.diag_last_rumble = Some(now);
                }
            }
            self.rumble = (strong, weak);
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
