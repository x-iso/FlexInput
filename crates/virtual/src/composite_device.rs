//! `VirtualDevice` backend for the composite USB personas: a virtual DualSense
//! (or Edge) with its USB audio interfaces beside the HID one, served by
//! `flexinput-usbip` and attached through a USB/IP host controller:
//! usbip-win2's on Windows, the kernel's vhci-hcd on Linux. To a game it is a
//! wired pad with a speaker / haptics output and a microphone, so the haptic
//! waveforms it authors stream to us.
//!
//! The HID side is the plain profile's: the same encoder builds the input
//! report and the same decode reads the game's output report. What differs is
//! the transport — an in-process USB/IP server instead of HIDMaestro's UMDF
//! driver and shared memory (Windows) or UHID (Linux) — and that nothing
//! outlives the app: the device is served from this process, so it can't
//! persist past it.

use std::sync::{Arc, Mutex};

use flexinput_core::haptic_pcm::HapticConverter;
use flexinput_core::Signal;
use flexinput_hidmaestro::encode::{encode_report_into, PinState};
use flexinput_hidmaestro::Profile;
use flexinput_usbip::{vhci, EmulatedDevice, OutputKind};

use crate::apply_pin;
use crate::sony_feedback::SonyFeedback;
use crate::{layouts, SinkPin, SourcePin};

/// The DualSense's USB output report.
const OUTPUT_REPORT_ID: u8 = 0x02;

pub struct CompositeDevice {
    id: String,
    display_name: String,
    profile: Profile,
    pins: PinState,
    report_buf: Vec<u8>,
    usb: Option<EmulatedDevice>,
    /// The host controller port the device is attached on.
    vport: Option<i32>,
    rumble: (f32, f32),
    sony: SonyFeedback,
    /// The haptic waveform the game streams (output channels 3/4), measured
    /// into per-side amplitude and frequency on the pacing thread.
    haptics: Arc<Mutex<HapticConverter>>,
}

impl CompositeDevice {
    /// Build the persona `profile_json` describes and attach it, setting the
    /// host controller up first if it isn't (see [`ensure_host_controller`]).
    /// Blocking: call from the device-ops worker. `None` only for an invalid
    /// bundled profile; a failed attach returns a disconnected device, with
    /// the reason logged.
    pub fn create(
        id: impl Into<String>,
        display_name: impl Into<String>,
        profile_json: &str,
        index: u8,
    ) -> Option<Self> {
        let mut dev = Self::detached(id, display_name, profile_json)?;
        if let Err(e) = dev.connect(profile_json, index) {
            eprintln!("[usbip] {} not attached: {e}", dev.id);
        }
        Some(dev)
    }

    /// The device, not yet served or attached.
    fn detached(id: impl Into<String>, display_name: impl Into<String>, profile_json: &str) -> Option<Self> {
        let profile = Profile::from_json(profile_json).ok()?;
        Some(CompositeDevice {
            id: id.into(),
            display_name: display_name.into(),
            report_buf: vec![0u8; profile.report.byte_size()],
            profile,
            pins: PinState::new(),
            usb: None,
            vport: None,
            rumble: (0.0, 0.0),
            sony: SonyFeedback::default(),
            haptics: Arc::new(Mutex::new(HapticConverter::new())),
        })
    }

    fn connect(&mut self, profile_json: &str, index: u8) -> Result<(), String> {
        ensure_host_controller()?;
        // Before the server exports anything, or the driver's re-attach loop
        // for a dead session's device could import this one first.
        flexinput_usbip::sweep_stale_once();
        let usb = EmulatedDevice::new(profile_json, &self.id, index).map_err(|e| e.to_string())?;
        let haptics = Arc::clone(&self.haptics);
        usb.set_pcm_sink(Some(Box::new(move |pcm: &[u8]| haptics.lock().unwrap().push_pcm(pcm))));
        // A neutral report waits for the host's first read.
        encode_report_into(&self.profile, &self.pins.state(), &mut self.report_buf);
        usb.submit_input(&self.report_buf);
        let vport = vhci::attach(flexinput_usbip::HOST, usb.port(), usb.busid()).map_err(|e| e.to_string())?;
        self.usb = Some(usb);
        self.vport = Some(vport);
        Ok(())
    }

    /// The HID-side pins: the Edge adds its back and Fn buttons.
    fn is_edge(&self) -> bool {
        self.profile.id.contains("dualsense-edge")
    }
}

/// A host controller this process can attach to. Windows: usbip-win2,
/// installed through the elevated helper when missing (one UAC, and the USB
/// root hubs re-enumerate once). Linux: the kernel's vhci-hcd, loaded and
/// handed to the user through pkexec the first time (see `linux::access`).
fn ensure_host_controller() -> Result<(), String> {
    #[cfg(windows)]
    if !vhci::is_available() {
        flexinput_hidmaestro::helper::ensure_usbip().map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    crate::linux::access::ensure_vhci()?;
    Ok(())
}

impl crate::VirtualDevice for CompositeDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn sink_pins(&self) -> &'static [SinkPin] {
        if self.is_edge() { layouts::DUALSENSE_EDGE_SINK_PINS } else { layouts::DUALSENSE_SINK_PINS }
    }

    fn send(&mut self, pin: &str, value: Signal) {
        apply_pin(&mut self.pins, pin, value);
    }

    fn flush(&mut self) {
        let Some(usb) = self.usb.as_ref() else { return };
        encode_report_into(&self.profile, &self.pins.state(), &mut self.report_buf);
        usb.submit_input(&self.report_buf);
    }

    fn reset_outputs(&mut self) {
        self.pins.reset();
        self.flush();
    }

    fn is_connected(&self) -> bool {
        self.vport.is_some()
    }

    fn source_pins(&self) -> &'static [SourcePin] {
        layouts::DUALSENSE_USB_SOURCE_PINS
    }

    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> {
        let frames: Vec<Vec<u8>> = self
            .usb
            .as_ref()
            .map(|u| u.take_outputs())
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f.kind == OutputKind::Output && f.report_id == OUTPUT_REPORT_ID)
            .map(|f| f.data)
            .collect();
        let mut out = self.sony.poll(&self.profile, &mut self.rumble, frames.iter().map(Vec::as_slice));
        let hd = self.haptics.lock().unwrap().params();
        out.extend([
            ("hd_l_amp", Signal::Float(hd.l_amp)),
            ("hd_l_freq", Signal::Float(hd.l_freq)),
            ("hd_r_amp", Signal::Float(hd.r_amp)),
            ("hd_r_freq", Signal::Float(hd.r_freq)),
        ]);
        out
    }

    fn inject_rumble_ping(&mut self, strong: f32, weak: f32) {
        self.rumble = (strong, weak);
    }
}

impl Drop for CompositeDevice {
    fn drop(&mut self) {
        // Plug out first: the driver closes the socket before it unplugs the
        // device, so the server sees the connection end cleanly.
        if let Some(p) = self.vport.take() {
            if let Err(e) = vhci::detach(p) {
                eprintln!("[usbip] detach of {} failed: {e}", self.id);
            }
        }
        self.usb = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualDevice;
    use flexinput_usbip::profiles::{DUALSENSE_COMPOSITE_JSON, DUALSENSE_EDGE_COMPOSITE_JSON};

    // A composite pad carries the plain pad's pins, so switching a node between
    // the two keeps every wire.
    #[test]
    fn composite_pins_match_the_plain_pads() {
        for (plain, usb) in crate::USB_AUDIO_VARIANTS {
            let (ps, po, pn) = crate::kind_pin_metadata(plain, 0).unwrap();
            let (us, uo, un) = crate::kind_pin_metadata(usb, 0).unwrap();
            let ids = |p: &[SinkPin]| p.iter().map(|p| p.id).collect::<Vec<_>>();
            let src = |p: &[SourcePin]| p.iter().map(|p| p.id).collect::<Vec<_>>();
            assert_eq!(ids(ps), ids(us), "{usb} sink pins");
            assert_eq!(src(uo)[..po.len()], src(po)[..], "{usb} keeps the plain pad's source pins in order");
            assert_eq!(pn, un, "same display name");
        }
        let edge = CompositeDevice::detached("virtual.usb.dualsense_edge", "E", DUALSENSE_EDGE_COMPOSITE_JSON).unwrap();
        let ds = CompositeDevice::detached("virtual.usb.dualsense", "D", DUALSENSE_COMPOSITE_JSON).unwrap();
        assert!(edge.sink_pins().iter().any(|p| p.id == "btn_paddle_r1"));
        assert!(!ds.sink_pins().iter().any(|p| p.id == "btn_paddle_r1"));
    }

    #[test]
    fn a_detached_device_is_disconnected_and_quiet() {
        let mut d = CompositeDevice::detached("virtual.usb.dualsense", "D", DUALSENSE_COMPOSITE_JSON).unwrap();
        assert!(!d.is_connected());
        d.send("btn_south", Signal::Bool(true));
        d.flush(); // no server: a no-op, not a panic
        let out = d.poll_outputs();
        assert!(out.contains(&("rumble_strong", Signal::Float(0.0))));
        assert_eq!(d.report_buf.len(), 64, "RID-inclusive USB report");
    }

    // The haptic channels of the game's PCM come out as the HD carrier pins.
    #[test]
    fn haptic_pcm_becomes_hd_pins() {
        let mut d = CompositeDevice::detached("virtual.usb.dualsense", "D", DUALSENSE_COMPOSITE_JSON).unwrap();
        // 20 ms of a 160 Hz sine on the left actuator (channel 3) only.
        let mut pcm = Vec::new();
        for n in 0..960 {
            let s = ((n as f32 * 160.0 / 48_000.0) * std::f32::consts::TAU).sin();
            for ch in 0..4 {
                let v = if ch == 2 { (s * 20_000.0) as i16 } else { 0 };
                pcm.extend_from_slice(&v.to_le_bytes());
            }
        }
        d.haptics.lock().unwrap().push_pcm(&pcm);
        let out = d.poll_outputs();
        let get = |p: &str| out.iter().find(|(id, _)| *id == p).map(|(_, s)| s.as_float()).unwrap();
        assert!(get("hd_l_amp") > 0.3, "left actuator driven: {}", get("hd_l_amp"));
        assert!(get("hd_r_amp") < 0.01, "right actuator silent");
        let hz = 80.0 + get("hd_l_freq") * 420.0;
        assert!((hz - 160.0).abs() < 25.0, "carrier near 160 Hz, got {hz}");
    }
}
