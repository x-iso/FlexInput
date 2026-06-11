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
use flexinput_hidmaestro::{InputSection, OutputSection, Profile};

use crate::{layouts, SinkPin, SourcePin};

/// A HIDMaestro-backed virtual controller (plain-HID path: DS4 / DualSense).
pub struct HidMaestroDevice {
    id: String,
    display_name: String,
    profile: Profile,
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
}

impl HidMaestroDevice {
    /// Open a HIDMaestro device for `controller_index` using `profile`. The
    /// device node + sections must already exist (created by the elevated
    /// helper); this opens the input/output sections to drive them.
    ///
    /// `id`/`display_name` follow FlexInput's virtual-device id scheme
    /// (e.g. `virtual.ds4`, "Virtual DualShock 4").
    pub fn open(
        id: impl Into<String>,
        display_name: impl Into<String>,
        profile: Profile,
        controller_index: u32,
    ) -> Self {
        let report_buf = vec![0u8; profile.input_report_size];
        let input = InputSection::open(controller_index).ok();
        let output = OutputSection::open(controller_index).ok();
        HidMaestroDevice {
            id: id.into(),
            display_name: display_name.into(),
            profile,
            pins: PinState::new(),
            report_buf,
            input,
            output,
            rumble: (0.0, 0.0),
        }
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
        if let Some(output) = self.output.as_mut() {
            while let Some(frame) = output.try_read() {
                // DS4 USB output report (id 0x05): byte[3] = weak (right/small),
                // byte[4] = strong (left/large) motor. The ring strips the
                // report-id, so within `data` the motors sit at [2]/[3]. Be
                // defensive about length; ignore unrecognized reports.
                if frame.data.len() >= 4 {
                    let weak = frame.data[2] as f32 / 255.0;
                    let strong = frame.data[3] as f32 / 255.0;
                    self.rumble = (strong, weak);
                }
            }
        }
        vec![
            ("rumble_strong", Signal::Float(self.rumble.0)),
            ("rumble_weak", Signal::Float(self.rumble.1)),
        ]
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
        let profile = Profile::from_json(DUALSHOCK_4_V2_JSON).unwrap();
        // controller index that almost certainly has no live section in CI →
        // input/output open as None; we test the pin→encode path, not the SHM.
        HidMaestroDevice::open("virtual.ds4", "Virtual DualShock 4", profile, 250)
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
