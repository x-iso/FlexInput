use std::collections::HashMap;
use std::time::{Duration, Instant};

use glam::Vec2;
use gilrs::{Axis, Button, Gilrs, GilrsBuilder};

use flexinput_core::Signal;

use crate::{
    gyro::GyroManager,
    identification::ControllerKind,
    layouts,
    DeviceBackend, PhysicalDevice,
};

// ── XInput force-feedback FFI (no windows-sys dependency) ────────────────────
// Pattern mirrors crates/devices/src/hidhide.rs win32 module.
#[cfg(windows)]
mod xinput_ffi {
    #[repr(C)]
    pub struct XINPUT_VIBRATION {
        pub w_left_motor_speed:  u16,
        pub w_right_motor_speed: u16,
    }

    #[link(name = "xinput")]
    extern "system" {
        /// Sets vibration state for an XInput controller slot (0-3).
        /// Returns 0 on success, ERROR_DEVICE_NOT_CONNECTED (1167) if the
        /// controller is disconnected.
        pub fn XInputSetState(dw_user_index: u32, p_vibration: *const XINPUT_VIBRATION) -> u32;
    }
}

pub struct GilrsBackend {
    gilrs: Gilrs,
    /// Cached count of non-ViGEmBus physical instances per (VID, PID).
    /// Rebuilt at most once every 2 s to avoid calling SetupAPI every frame.
    phys_counts: HashMap<(u16, u16), usize>,
    /// Whether ViGEmBus has at least one virtual device for each (VID, PID).
    /// Only pairs with vigem_present=true need the physical-count dedup filter.
    vigem_present: HashMap<(u16, u16), bool>,
    phys_counts_at: Instant,
    gyro: GyroManager,
    /// XInput user index (0-3) for each `gilrs:<kind>:<inst>` device_id string.
    /// Rebuilt at the start of each poll() call from the same kind_seen counter
    /// used for the dev-string, so indices never drift on reconnect.
    xinput_idx: HashMap<String, u32>,
    /// Last-written rumble state per XInput slot to avoid redundant XInputSetState calls.
    /// (left_motor_byte, right_motor_byte) in 0-255 range.
    xinput_rumble: HashMap<u32, (u8, u8)>,
    /// Per-gilrs-gamepad-id raw event count since the last `take_event_counts`.
    /// Used by the I/O thread to compute live per-device polling rates.
    event_counts: HashMap<usize, u32>,
    /// Maps gilrs-internal gamepad id → device_id string. Refreshed each
    /// `poll()` so the I/O thread can convert event counts to per-device
    /// rates.
    id_to_dev: HashMap<usize, String>,
}

impl GilrsBackend {
    pub fn try_new() -> Option<Self> {
        GilrsBuilder::new().with_default_filters(false).build().ok().map(|gilrs| Self {
            gilrs,
            phys_counts: HashMap::new(),
            vigem_present: HashMap::new(),
            // force refresh on first enumerate()
            phys_counts_at: Instant::now() - Duration::from_secs(10),
            gyro: GyroManager::new(),
            xinput_idx: HashMap::new(),
            xinput_rumble: HashMap::new(),
            event_counts: HashMap::new(),
            id_to_dev: HashMap::new(),
        })
    }

    fn refresh_phys_counts(&mut self) {
        self.phys_counts.clear();
        self.vigem_present.clear();
        for (_, pad) in self.gilrs.gamepads() {
            if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                self.vigem_present.entry(vp).or_insert_with(|| {
                    crate::hidhide::has_vigem_for_vid_pid(vp.0, vp.1)
                });
                self.phys_counts.entry(vp).or_insert_with(|| {
                    crate::hidhide::physical_count_for_vid_pid(vp.0, vp.1)
                });
            }
        }
        self.phys_counts_at = Instant::now();
    }
}

impl DeviceBackend for GilrsBackend {
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        puffin::profile_function!();
        if self.phys_counts_at.elapsed() > Duration::from_secs(2) {
            self.refresh_phys_counts();
        }

        let mut gilrs_seen: HashMap<(u16, u16), usize> = HashMap::new();
        let mut kind_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut virt_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut result = Vec::new();

        for (_id, pad) in self.gilrs.gamepads() {
            // Our OWN emulated HIDMaestro devices are identified by name marker and
            // handled by gilrs_device_id (distinct `vN` instance) — skip the
            // count-based ViGEm dedup for them (it would otherwise drop them as
            // "extra beyond physical count", since has_vigem_for_vid_pid is true
            // whenever any HIDMaestro device for that VID/PID exists).
            if !is_own_virtual(&pad) {
                if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                    // Only dedup when ViGEmBus is confirmed to have a virtual for this
                    // VID/PID. Without that, SetupAPI's USB-format HW-ID search would
                    // return 0 for Bluetooth devices and incorrectly drop them.
                    if *self.vigem_present.get(&vp).unwrap_or(&false) {
                        let phys = *self.phys_counts.get(&vp).unwrap_or(&0);
                        let seen = gilrs_seen.entry(vp).or_insert(0);
                        if *seen >= phys {
                            continue; // extra beyond physical count → ViGEmBus virtual
                        }
                        *seen += 1;
                    }
                }
            }

            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());
            let (dev_id, _inst, is_virt) =
                gilrs_device_id(&pad, kind, &mut kind_seen, &mut virt_seen);

            let display_name = if kind == ControllerKind::Generic {
                pad.name().to_string()
            } else if is_virt {
                format!("{} (virtual)", kind.display_name())
            } else {
                kind.display_name().to_string()
            };

            result.push(PhysicalDevice {
                id: dev_id,
                display_name,
                kind,
                outputs: layouts::outputs_for(kind),
                inputs: layouts::inputs_for(kind),
                instance_path: None,
            });
        }
        result
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        puffin::profile_function!();
        // Rebuild XInput slot map at the start of each poll so it stays in sync
        // with kind_seen even after device reconnects.
        self.xinput_idx.clear();

        // Drain events. Raw events auto-update axis and button state.
        // axis_dpad_to_button is intentionally NOT applied: on BT Switch Pro it creates
        // conflicting synthetic button releases that fight the native WGI DPad button events,
        // causing flicker and broken diagonals. DPad discrete outputs are derived manually
        // below from both axis_data (HAT/USB path) and button_data (BT/WGI path).
        let mut ev_count = 0u32;
        {
            puffin::profile_scope!("gilrs_next_event_drain");
            while let Some(ev) = self.gilrs.next_event() {
                // Count raw events per gilrs gamepad id so the I/O thread can
                // compute live per-device polling rates. We bump on every event
                // (Axis/Button/Connected/Disconnected/Dropped) — a single iter of
                // device data typically produces several events together, which
                // matches what users see as "device polled this frame".
                *self.event_counts.entry(usize::from(ev.id)).or_insert(0) += 1;
                self.gilrs.update(&ev);
                ev_count += 1;
            }
        }
        // event count is captured below as a data field on a dedicated scope
        // to avoid the function-body profile_scope trap (RAII guard would
        // otherwise span to function exit and falsely include later work).
        let _ = ev_count;

        // Flush staged rumble / lightbar outputs *before* reading inputs so any
        // device.sink writes from the previous frame land on the controller.
        {
            puffin::profile_scope!("gyro_flush_outputs");
            self.gyro.flush_outputs();
        }

        let mut out = Vec::new();
        let mut gilrs_seen: HashMap<(u16, u16), usize> = HashMap::new();
        let mut kind_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut virt_seen: HashMap<ControllerKind, usize> = HashMap::new();
        // Track per-(VID,PID) instance index for gyro correlation.
        let mut gyro_idx: HashMap<(u16, u16), usize> = HashMap::new();
        // Rebuild the gilrs-id → device-id map this frame so event counts
        // can be resolved to per-device polling rates.
        self.id_to_dev.clear();

        // Wrap the gamepads() walk in an explicit block so the profile
        // scope's RAII guard ends with the for loop — NOT with the function.
        // Bare profile_scope! at this depth would falsely include the
        // post-walk `out` return at function exit (the misleading-scope
        // trap we've hit twice now). Matching `}` is at the original
        // for-loop closing brace, with an extra `}` to close this wrapper.
        {
        puffin::profile_scope!("gilrs_gamepads_walk");
        for (gilrs_id, pad) in self.gilrs.gamepads() {
            // Apply the same ViGEmBus filter as enumerate() so IDs stay in sync —
            // but never to our own marked emulated devices (handled below).
            if !is_own_virtual(&pad) {
                if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                    if *self.vigem_present.get(&vp).unwrap_or(&false) {
                        let phys = *self.phys_counts.get(&vp).unwrap_or(&0);
                        let seen = gilrs_seen.entry(vp).or_insert(0);
                        if *seen >= phys {
                            continue;
                        }
                        *seen += 1;
                    }
                }
            }

            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());
            let (dev, inst, is_virt) =
                gilrs_device_id(&pad, kind, &mut kind_seen, &mut virt_seen);
            self.id_to_dev.insert(usize::from(gilrs_id), dev.clone());

            // Record XInput slot: inst is the 0-based kind_seen index, which matches
            // the XInput user index on Windows (gilrs enumerates XInput controllers
            // in slot order 0-3). Only real XInput pads (ViGEm virtuals are filtered
            // and our HIDMaestro pads aren't XInput), so the marker case never hits.
            if kind == ControllerKind::XInput && !is_virt {
                self.xinput_idx.insert(dev.clone(), inst as u32);
            }

            for (axis, pin_id) in axis_map(kind) {
                let v = pad.axis_data(*axis).map_or(0.0, |d| d.value());
                out.push((dev.clone(), pin_id.to_string(), Signal::Float(v)));
            }

            // In BT mode gilrs maps Switch Pro buttons by WGI label order (Nintendo's A/B/X/Y
            // labels → gilrs South/East/West/North), reversing physical positions. Use a
            // corrective map when the pad name doesn't say "Pro Controller" (USB mode).
            let is_switch_bt = kind == ControllerKind::SwitchPro
                && !pad.name().to_ascii_lowercase().contains("pro controller");
            let btn_map: &[(Button, &str)] = if is_switch_bt {
                BUTTON_MAP_SWITCH_BT
            } else {
                button_map(kind)
            };
            for (button, pin_id) in btn_map {
                let pressed = pad.button_data(*button).map_or(false, |d| d.is_pressed());
                out.push((dev.clone(), pin_id.to_string(), Signal::Bool(pressed)));
            }

            // Universal DPad discrete outputs: combine axis_data (HAT/USB path) and
            // button_data (WGI/BT path).  Since axis_dpad_to_button is no longer applied,
            // button_data only reflects native button events (WGI flags etc.) while
            // axis_data reflects HAT-switch events.  OR-combining both handles all paths:
            //   • USB HAT (DS4, Switch Pro USB): axis_data non-zero, button_data zero.
            //   • BT WGI (Switch Pro BT): axis_data zero, button_data non-zero.
            //   • XInput / WGI gamepad: button_data set by WGI DPad flags.
            {
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                let du = dy >  0.5 || pad.button_data(Button::DPadUp).map_or(false,    |d| d.is_pressed());
                let dd = dy < -0.5 || pad.button_data(Button::DPadDown).map_or(false,  |d| d.is_pressed());
                let dr = dx >  0.5 || pad.button_data(Button::DPadRight).map_or(false, |d| d.is_pressed());
                let dl = dx < -0.5 || pad.button_data(Button::DPadLeft).map_or(false,  |d| d.is_pressed());
                out.push((dev.clone(), "dpad_up".into(),    Signal::Bool(du)));
                out.push((dev.clone(), "dpad_down".into(),  Signal::Bool(dd)));
                out.push((dev.clone(), "dpad_right".into(), Signal::Bool(dr)));
                out.push((dev.clone(), "dpad_left".into(),  Signal::Bool(dl)));

                // Reconstruct dpad Vec2 and axis pins from button states when axis_data is zero
                // (BT WGI path has no HAT axis events).  Diagonal magnitude is normalised.
                if dx == 0.0 && dy == 0.0 && (du || dd || dr || dl) {
                    let bx = if dr { 1.0f32 } else if dl { -1.0 } else { 0.0 };
                    let by = if du { 1.0f32 } else if dd { -1.0 } else { 0.0 };
                    let (nx, ny) = if bx != 0.0 && by != 0.0 {
                        (bx * std::f32::consts::FRAC_1_SQRT_2, by * std::f32::consts::FRAC_1_SQRT_2)
                    } else { (bx, by) };
                    out.push((dev.clone(), "dpad_x".into(), Signal::Float(nx)));
                    out.push((dev.clone(), "dpad_y".into(), Signal::Float(ny)));
                    out.push((dev.clone(), "dpad".into(), Signal::Vec2(glam::Vec2::new(nx, ny))));
                }
            }

            let lx = axis_val(&pad, Axis::LeftStickX);
            let ly = axis_val(&pad, Axis::LeftStickY);
            out.push((dev.clone(), "left_stick".into(), Signal::Vec2(Vec2::new(lx, ly))));

            let rx = axis_val(&pad, Axis::RightStickX);
            let ry = axis_val(&pad, Axis::RightStickY);
            out.push((dev.clone(), "right_stick".into(), Signal::Vec2(Vec2::new(rx, ry))));

            if matches!(kind, ControllerKind::XInput | ControllerKind::DualShock4 | ControllerKind::DualSense | ControllerKind::Generic) {
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(dx, dy))));
            } else if kind == ControllerKind::SwitchPro {
                // DPad comes as Axis::DPadX/Y (HAT switch) — already emitted by axis_map;
                // just build the bundled Vec2 from the same values.
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(dx, dy))));
            }

            // Analog triggers: ButtonData::value() works for both XInput and native HID.
            // Switch Pro ZL/ZR are digital-only and handled by button_map already.
            if !matches!(kind, ControllerKind::SwitchPro) {
                let lt = pad.button_data(Button::LeftTrigger2).map_or(0.0, |d| d.value());
                let rt = pad.button_data(Button::RightTrigger2).map_or(0.0, |d| d.value());
                let (l_pin, r_pin) = ("left_trigger", "right_trigger");
                out.push((dev.clone(), l_pin.into(), Signal::Float(lt)));
                out.push((dev.clone(), r_pin.into(), Signal::Float(rt)));
            }

            // Gyro via raw HID for DS4 / DualSense.
            if let Some((vid, pid)) = pad.vendor_id().zip(pad.product_id()) {
                let vp = (vid, pid);
                let idx = *gyro_idx.entry(vp).or_insert(0);
                gyro_idx.insert(vp, idx + 1);

                if let Some(g) = self.gyro.read(vid, pid, idx) {
                    // Attribute HID IMU/state reports to the per-device polling
                    // rate. gilrs's event stream only fires on axis/button state
                    // changes, so a still gyro-only device would otherwise read 0 Hz.
                    let hid_n = self.gyro.take_event_count(vid, pid, idx);
                    if hid_n > 0 {
                        *self.event_counts.entry(usize::from(gilrs_id)).or_insert(0) += hid_n;
                    }
                    out.push((dev.clone(), "gyro_x".into(),  Signal::Float(g.gyro_x)));
                    out.push((dev.clone(), "gyro_y".into(),  Signal::Float(g.gyro_y)));
                    out.push((dev.clone(), "gyro_z".into(),  Signal::Float(g.gyro_z)));
                    out.push((dev.clone(), "accel_x".into(), Signal::Float(g.accel_x)));
                    out.push((dev.clone(), "accel_y".into(), Signal::Float(g.accel_y)));
                    out.push((dev.clone(), "accel_z".into(), Signal::Float(g.accel_z)));
                    if g.has_touchpad {
                        out.push((dev.clone(), "touch1_x".into(),      Signal::Float(g.touch1.x)));
                        out.push((dev.clone(), "touch1_y".into(),      Signal::Float(g.touch1.y)));
                        out.push((dev.clone(), "touch1_active".into(), Signal::Bool(g.touch1.active)));
                        out.push((dev.clone(), "touch2_x".into(),      Signal::Float(g.touch2.x)));
                        out.push((dev.clone(), "touch2_y".into(),      Signal::Float(g.touch2.y)));
                        out.push((dev.clone(), "touch2_active".into(), Signal::Bool(g.touch2.active)));
                        // gilrs's Windows backend doesn't fire Button::C for the touchpad
                        // click on DS4/DualSense, and DualSense's mute button isn't mapped
                        // at all — so we override with the values parsed from the raw HID
                        // report. Pushed last so the per-frame HashMap lookup wins.
                        out.push((dev.clone(), "btn_touchpad".into(),  Signal::Bool(g.touchpad_click)));
                    }
                    if matches!(kind, ControllerKind::DualSense) {
                        out.push((dev.clone(), "btn_mute".into(), Signal::Bool(g.mic_button)));
                    }
                    // DualSense: override all axes and buttons with raw HID values.
                    // gilrs on Windows HID maps the 6 axes by USB Usage ID order rather than
                    // logical gamepad position, placing L2/R2 where RightStickX/Y should be
                    // and RX/RY under LeftZ/RightZ. Parsing the report directly avoids this.
                    if let Some(ds) = g.dualsense {
                        out.push((dev.clone(), "left_stick_x".into(),  Signal::Float(ds.lx)));
                        out.push((dev.clone(), "left_stick_y".into(),  Signal::Float(ds.ly)));
                        out.push((dev.clone(), "right_stick_x".into(), Signal::Float(ds.rx)));
                        out.push((dev.clone(), "right_stick_y".into(), Signal::Float(ds.ry)));
                        out.push((dev.clone(), "left_stick".into(),    Signal::Vec2(Vec2::new(ds.lx, ds.ly))));
                        out.push((dev.clone(), "right_stick".into(),   Signal::Vec2(Vec2::new(ds.rx, ds.ry))));
                        out.push((dev.clone(), "left_trigger".into(),  Signal::Float(ds.l2)));
                        out.push((dev.clone(), "right_trigger".into(), Signal::Float(ds.r2)));
                        out.push((dev.clone(), "btn_south".into(),   Signal::Bool(ds.btn_south)));
                        out.push((dev.clone(), "btn_east".into(),    Signal::Bool(ds.btn_east)));
                        out.push((dev.clone(), "btn_west".into(),    Signal::Bool(ds.btn_west)));
                        out.push((dev.clone(), "btn_north".into(),   Signal::Bool(ds.btn_north)));
                        out.push((dev.clone(), "btn_lb".into(),      Signal::Bool(ds.btn_l1)));
                        out.push((dev.clone(), "btn_rb".into(),      Signal::Bool(ds.btn_r1)));
                        out.push((dev.clone(), "btn_lt_dig".into(),  Signal::Bool(ds.btn_l2)));
                        out.push((dev.clone(), "btn_rt_dig".into(),  Signal::Bool(ds.btn_r2)));
                        out.push((dev.clone(), "btn_ls".into(),      Signal::Bool(ds.btn_ls)));
                        out.push((dev.clone(), "btn_rs".into(),      Signal::Bool(ds.btn_rs)));
                        out.push((dev.clone(), "btn_start".into(),   Signal::Bool(ds.btn_options)));
                        out.push((dev.clone(), "btn_back".into(),    Signal::Bool(ds.btn_create)));
                        out.push((dev.clone(), "btn_guide".into(),   Signal::Bool(ds.btn_ps)));
                        out.push((dev.clone(), "dpad_up".into(),     Signal::Bool(ds.dpad_up)));
                        out.push((dev.clone(), "dpad_down".into(),   Signal::Bool(ds.dpad_down)));
                        out.push((dev.clone(), "dpad_left".into(),   Signal::Bool(ds.dpad_left)));
                        out.push((dev.clone(), "dpad_right".into(),  Signal::Bool(ds.dpad_right)));
                        let dx = if ds.dpad_right { 1.0f32 } else if ds.dpad_left { -1.0 } else { 0.0 };
                        let dy = if ds.dpad_up    { 1.0f32 } else if ds.dpad_down { -1.0 } else { 0.0 };
                        let (ndx, ndy) = if dx != 0.0 && dy != 0.0 {
                            (dx * std::f32::consts::FRAC_1_SQRT_2, dy * std::f32::consts::FRAC_1_SQRT_2)
                        } else { (dx, dy) };
                        out.push((dev.clone(), "dpad_x".into(),  Signal::Float(ndx)));
                        out.push((dev.clone(), "dpad_y".into(),  Signal::Float(ndy)));
                        out.push((dev.clone(), "dpad".into(),    Signal::Vec2(Vec2::new(ndx, ndy))));
                    }
                    // Switch Pro: override gilrs button output with raw-HID button data.
                    // This bypasses gilrs's WGI backend which loses D-Pad diagonals (8-position
                    // switch only emits cardinal events), garbles A/B/X/Y vs South/East/West/North
                    // by Nintendo label vs physical position, and mis-routes Plus/Minus/Home/Capture.
                    // Pushing last makes these the authoritative values in the IO-thread HashMap.
                    if let Some(sb) = g.switch_buttons {
                        // Sticks: raw-HID values calibrated from SPI flash are authoritative.
                        // gilrs's WGI stick mapping can land on the wrong axes when the HID
                        // device tree shifts (e.g. another driver install changes enumeration),
                        // so we override unconditionally for Switch Pro.
                        out.push((dev.clone(), "left_stick_x".into(),  Signal::Float(sb.lstick_x)));
                        out.push((dev.clone(), "left_stick_y".into(),  Signal::Float(sb.lstick_y)));
                        out.push((dev.clone(), "right_stick_x".into(), Signal::Float(sb.rstick_x)));
                        out.push((dev.clone(), "right_stick_y".into(), Signal::Float(sb.rstick_y)));
                        out.push((dev.clone(), "left_stick".into(),    Signal::Vec2(Vec2::new(sb.lstick_x, sb.lstick_y))));
                        out.push((dev.clone(), "right_stick".into(),   Signal::Vec2(Vec2::new(sb.rstick_x, sb.rstick_y))));
                        // Face buttons by physical position (Nintendo's labels are weird):
                        out.push((dev.clone(), "btn_south".into(), Signal::Bool(sb.btn_b))); // B = south
                        out.push((dev.clone(), "btn_east".into(),  Signal::Bool(sb.btn_a))); // A = east
                        out.push((dev.clone(), "btn_west".into(),  Signal::Bool(sb.btn_y))); // Y = west
                        out.push((dev.clone(), "btn_north".into(), Signal::Bool(sb.btn_x))); // X = north
                        out.push((dev.clone(), "btn_lb".into(),    Signal::Bool(sb.btn_l)));
                        out.push((dev.clone(), "btn_rb".into(),    Signal::Bool(sb.btn_r)));
                        out.push((dev.clone(), "btn_lt_dig".into(),Signal::Bool(sb.btn_zl)));
                        out.push((dev.clone(), "btn_rt_dig".into(),Signal::Bool(sb.btn_zr)));
                        out.push((dev.clone(), "btn_ls".into(),    Signal::Bool(sb.btn_lstick)));
                        out.push((dev.clone(), "btn_rs".into(),    Signal::Bool(sb.btn_rstick)));
                        out.push((dev.clone(), "btn_start".into(), Signal::Bool(sb.btn_plus)));
                        out.push((dev.clone(), "btn_back".into(),  Signal::Bool(sb.btn_minus)));
                        out.push((dev.clone(), "btn_guide".into(), Signal::Bool(sb.btn_home)));
                        out.push((dev.clone(), "btn_capture".into(),Signal::Bool(sb.btn_capture)));
                        out.push((dev.clone(), "dpad_up".into(),   Signal::Bool(sb.dpad_up)));
                        out.push((dev.clone(), "dpad_down".into(), Signal::Bool(sb.dpad_down)));
                        out.push((dev.clone(), "dpad_left".into(), Signal::Bool(sb.dpad_left)));
                        out.push((dev.clone(), "dpad_right".into(),Signal::Bool(sb.dpad_right)));
                        // Reconstruct DPad axis/Vec2 from authoritative button bits (BT path
                        // sends no axis events at all; diagonals get √2/2 magnitude).
                        let bx = if sb.dpad_right { 1.0f32 } else if sb.dpad_left { -1.0 } else { 0.0 };
                        let by = if sb.dpad_up    { 1.0f32 } else if sb.dpad_down { -1.0 } else { 0.0 };
                        let (nx, ny) = if bx != 0.0 && by != 0.0 {
                            (bx * std::f32::consts::FRAC_1_SQRT_2, by * std::f32::consts::FRAC_1_SQRT_2)
                        } else { (bx, by) };
                        out.push((dev.clone(), "dpad_x".into(), Signal::Float(nx)));
                        out.push((dev.clone(), "dpad_y".into(), Signal::Float(ny)));
                        out.push((dev.clone(), "dpad".into(),   Signal::Vec2(Vec2::new(nx, ny))));
                    }
                }
            }
        }
        } // end gilrs_gamepads_walk profile_scope block

        out
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        // ── XInput path: dispatch via XInputSetState ──────────────────────────
        if let Some(&xinput_slot) = self.xinput_idx.get(device_id) {
            let byte = match signal {
                Signal::Float(f) => (f.clamp(0.0, 1.0) * 255.0) as u8,
                Signal::Bool(b)  => if b { 255 } else { 0 },
                _ => return,
            };
            let entry = self.xinput_rumble.entry(xinput_slot).or_insert((0, 0));
            match pin_id {
                "rumble_strong" => entry.0 = byte,
                "rumble_weak"   => entry.1 = byte,
                _ => return, // other pin names are not XInput rumble
            }
            #[cfg(windows)]
            unsafe {
                use xinput_ffi::*;
                let vib = XINPUT_VIBRATION {
                    // 0-255 byte mapped to 0-65535 motor speed: multiply by 257 (= 0xFFFF / 0xFF).
                    w_left_motor_speed:  (entry.0 as u16).saturating_mul(257),
                    w_right_motor_speed: (entry.1 as u16).saturating_mul(257),
                };
                XInputSetState(xinput_slot, &vib);
            }
            return;
        }

        // ── PS/Switch HID path via GyroManager ───────────────────────────────
        let (vid, pid, idx) = match self.lookup_phys(device_id) {
            Some(t) => t,
            None => return,
        };
        // Scale Float 0–1 to the semantic range for each pin.
        // Pins with custom ranges are listed explicitly; everything else uses 0–255.
        let f = match signal {
            Signal::Float(f) => f.clamp(0.0, 1.0),
            Signal::Bool(b)  => if b { 1.0 } else { 0.0 },
            _ => return,
        };
        let byte = match pin_id {
            // Trigger mode: 0=Off, 1=Feedback, 2=Weapon, 3=Vibration → scale to 0–3
            "trigger_r_mode" | "trigger_l_mode" => (f * 3.0).round() as u8,
            // Trigger zones: 0–9 along trigger travel → scale to 0–9
            "trigger_r_start" | "trigger_r_end" |
            "trigger_l_start" | "trigger_l_end" => (f * 9.0).round() as u8,
            // Trigger force: 0–7 → scale to 0–7
            "trigger_r_strength" | "trigger_l_strength" => (f * 7.0).round() as u8,
            // Player LED: 0=off, 1=P1, 2=P2, 3=P3, 4=P4 → scale to 0–4
            "player_led" => (f * 4.0).round() as u8,
            // Mic LED: 0=off, 1=on, 2=pulsing → scale to 0–2
            "mic_led" => (f * 2.0).round() as u8,
            // Everything else (rumble 0–255, lightbar 0–255, trigger freq 0–255): linear
            _ => (f * 255.0) as u8,
        };
        self.gyro.set_output_byte(vid, pid, idx, pin_id, byte);
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        // Map gilrs-id counts to device-id, summing in case multiple gilrs ids
        // somehow resolved to the same device id (shouldn't happen, but safe).
        let mut acc: HashMap<String, u32> = HashMap::new();
        for (gilrs_id, count) in self.event_counts.drain() {
            if let Some(dev) = self.id_to_dev.get(&gilrs_id) {
                *acc.entry(dev.clone()).or_insert(0) += count;
            }
        }
        for (dev, count) in acc {
            out.push((dev, count));
        }
        out
    }

    fn set_spike_filter(&mut self, device_id: &str, enabled: bool, sensitivity_pct: f32) {
        if let Some((vid, pid, idx)) = self.lookup_phys(device_id) {
            self.gyro.set_spike_filter(vid, pid, idx, enabled, sensitivity_pct);
        }
    }
}

impl GilrsBackend {
    /// Resolve a `gilrs:<kind>:<inst>` device id to the (vid, pid, instance index) tuple
    /// that GyroManager keys its open HID handles by. Applies the same ViGEmBus filter
    /// and kind-based instance counting as `enumerate()` / `poll()`.
    fn lookup_phys(&self, device_id: &str) -> Option<(u16, u16, usize)> {
        let mut gilrs_seen: HashMap<(u16, u16), usize> = HashMap::new();
        let mut kind_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut virt_seen: HashMap<ControllerKind, usize> = HashMap::new();
        let mut vp_seen: HashMap<(u16, u16), usize> = HashMap::new();
        for (_id, pad) in self.gilrs.gamepads() {
            if !is_own_virtual(&pad) {
                if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                    if *self.vigem_present.get(&vp).unwrap_or(&false) {
                        let phys = *self.phys_counts.get(&vp).unwrap_or(&0);
                        let seen = gilrs_seen.entry(vp).or_insert(0);
                        if *seen >= phys {
                            continue;
                        }
                        *seen += 1;
                    }
                }
            }
            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());
            let (dev, _inst, _is_virt) =
                gilrs_device_id(&pad, kind, &mut kind_seen, &mut virt_seen);
            let vp = pad.vendor_id().zip(pad.product_id());
            if dev == device_id {
                let (v, p) = vp?;
                let idx = *vp_seen.get(&(v, p)).unwrap_or(&0);
                return Some((v, p, idx));
            }
            if let Some((v, p)) = vp {
                *vp_seen.entry((v, p)).or_insert(0) += 1;
            }
        }
        None
    }
}

fn axis_val(pad: &gilrs::Gamepad, axis: Axis) -> f32 {
    pad.axis_data(axis).map_or(0.0, |d| d.value())
}

/// True if `pad` is one of FlexInput's OWN emulated HIDMaestro devices, detected
/// by the marker we append to its Windows naming (see
/// `flexinput_hidmaestro::orchestrator::NAME_MARKER`). gilrs reports the marked
/// FriendlyName, so a real same-VID/PID controller (clean name) is excluded.
/// This is the only reliable discriminator: a real and an emulated DualSense
/// share VID/PID 054C:0CE6 and WGI exposes no per-instance device path.
fn is_own_virtual(pad: &gilrs::Gamepad) -> bool {
    pad.name().contains(flexinput_core::VIRTUAL_DEVICE_NAME_MARKER)
}

/// Compute the stable `gilrs:` device id for a pad. Own emulated devices get a
/// `v`-prefixed instance (`gilrs:dualsense:v0`) counted independently of real
/// devices, so a real controller keeps a contiguous `gilrs:dualsense:0` index
/// regardless of plug order — fixing the collision where the dedup dropped the
/// real device when a virtual of the same model already existed. `kind_seen`
/// tracks real-device instances per kind; `virt_seen` tracks emulated ones.
fn gilrs_device_id(
    pad: &gilrs::Gamepad,
    kind: ControllerKind,
    kind_seen: &mut std::collections::HashMap<ControllerKind, usize>,
    virt_seen: &mut std::collections::HashMap<ControllerKind, usize>,
) -> (String, usize, bool) {
    if is_own_virtual(pad) {
        let inst = *virt_seen.get(&kind).unwrap_or(&0);
        virt_seen.insert(kind, inst + 1);
        (format!("gilrs:{}:v{}", kind.id_slug(), inst), inst, true)
    } else {
        let inst = *kind_seen.get(&kind).unwrap_or(&0);
        kind_seen.insert(kind, inst + 1);
        (format!("gilrs:{}:{}", kind.id_slug(), inst), inst, false)
    }
}

fn axis_map(kind: ControllerKind) -> &'static [(Axis, &'static str)] {
    match kind {
        ControllerKind::DualShock4 | ControllerKind::DualSense => AXIS_MAP_DS4,
        ControllerKind::SwitchPro                              => AXIS_MAP_SWITCH,
        _                                                      => AXIS_MAP_STANDARD,
    }
}

const AXIS_MAP_STANDARD: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // Triggers emitted separately via ButtonData::value() — Axis::LeftZ/RightZ
    // is not reliably populated on all Windows gilrs backends.
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

const AXIS_MAP_DS4: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // Triggers emitted separately via ButtonData::value().
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

const AXIS_MAP_SWITCH: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // DPad is a HAT switch reported as axes on Windows; read directly.
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

fn button_map(kind: ControllerKind) -> &'static [(Button, &'static str)] {
    match kind {
        ControllerKind::DualShock4 | ControllerKind::DualSense => BUTTON_MAP_PLAYSTATION,
        ControllerKind::SwitchPro  => BUTTON_MAP_SWITCH,
        _                          => BUTTON_MAP_XINPUT,
    }
}

// DPad discrete outputs are emitted via the unified axis+button path in poll(); omitted here.
const BUTTON_MAP_XINPUT: &[(Button, &str)] = &[
    (Button::South,         "btn_south"),
    (Button::East,          "btn_east"),
    (Button::West,          "btn_west"),
    (Button::North,         "btn_north"),
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_start"),
    (Button::Select,        "btn_back"),
    (Button::Mode,          "btn_guide"),
];

const BUTTON_MAP_PLAYSTATION: &[(Button, &str)] = &[
    (Button::South,         "btn_south"),
    (Button::East,          "btn_east"),
    (Button::West,          "btn_west"),
    (Button::North,         "btn_north"),
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_start"),
    (Button::Select,        "btn_back"),
    (Button::Mode,          "btn_guide"),
    (Button::C,             "btn_touchpad"),
];

// USB / wired Switch Pro: gilrs Button::South = physical south (B), East = A, West = Y, North = X.
const BUTTON_MAP_SWITCH: &[(Button, &str)] = &[
    (Button::South,         "btn_south"),
    (Button::East,          "btn_east"),
    (Button::West,          "btn_west"),
    (Button::North,         "btn_north"),
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_start"),   // Plus
    (Button::Select,        "btn_back"),    // Minus
    (Button::Mode,          "btn_guide"),   // Home
    (Button::C,             "btn_capture"), // Capture
    // DPad omitted: emitted separately via axis+button combo in poll() to avoid BT flicker.
];

// Bluetooth Switch Pro: gilrs is driven by Windows Gaming Input which maps Nintendo labels
// rather than physical positions (Nintendo's A label → WGI A → gilrs South).
// Additionally Home/Capture appear at gilrs Select/Start in BT mode.
const BUTTON_MAP_SWITCH_BT: &[(Button, &str)] = &[
    (Button::South, "btn_east"),    // WGI A = Nintendo A (east physical position)
    (Button::East,  "btn_south"),   // WGI B = Nintendo B (south physical position)
    (Button::West,  "btn_north"),   // WGI X = Nintendo X (north physical position)
    (Button::North, "btn_west"),    // WGI Y = Nintendo Y (west physical position)
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    // In BT mode: Start fires for Home, Select fires for Capture (opposite of USB).
    (Button::Start,  "btn_guide"),   // Home → gilrs Start in BT
    (Button::Select, "btn_capture"), // Capture → gilrs Select in BT
    (Button::Mode,   "btn_start"),   // Plus (if it fires Mode in BT)
    (Button::C,      "btn_back"),    // Minus (if it fires C in BT)
    // DPad omitted: emitted separately via axis+button combo in poll().
];
