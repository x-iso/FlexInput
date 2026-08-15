//! `DeviceBackend` adapter for Joy-Con 2 controllers reached over Bluetooth LE.
//!
//! All the protocol work lives in `flexinput-joycon2`; this file is the seam
//! that turns its `PadSnapshot`s into FlexInput `Signal`s and gives each half a
//! device id. The hub runs on its own thread, so `poll` and `enumerate` here
//! only ever touch an in-memory snapshot and never block on Bluetooth.

use flexinput_core::Signal;
use flexinput_joycon2::{Joycon2DongleHub, Joycon2Hub, Joycon2UsbHub, PadKey, PadState, Side};

use crate::gyro::{ACCEL_REF_G, GYRO_REF_DPS};
use crate::{layouts, ControllerKind, DeviceBackend, PhysicalDevice};

/// Accelerometer scale, now measured from hardware rather than assumed: the
/// at-rest gravity vector has magnitude ≈4096 LSB, so 1 g = 4096 LSB.
///
/// That works out to the same number as the `8.0 / 32767.0` guessed from the
/// Switch Pro, because ±8 g across 4096 LSB/g fills the full i16 range — the
/// sensor really is ±8 g over 16 bits. The scale was never the problem; the
/// field OFFSET was. Expressed via the measured constant so it stays anchored
/// to evidence instead of a coincidence.
const JC2_ACCEL_G_PER_LSB: f32 = 1.0 / flexinput_joycon2::reports::ACCEL_LSB_PER_G;
/// Gyro scale. Still a guess — the gyro field has not been located in the
/// report yet, so `pad.gyro` is always zero and this constant is unexercised.
const JC2_GYRO_DPS_PER_LSB: f32 = 2000.0 / 32767.0;

pub struct Joycon2Backend {
    hub: Joycon2Hub,
    usb: Joycon2UsbHub,
    dongle: Joycon2DongleHub,
}

impl Joycon2Backend {
    /// Pads from every transport, best first.
    ///
    /// A controller reachable over both would appear twice, but the two get
    /// different `PadKey` addresses (USB synthesises one, tagged `0xFE`/`0xFF`
    /// so it can never collide with a real BD_ADDR) and therefore different
    /// device ids, so nothing is silently overwritten — the user just sees two
    /// entries and can wire the one they want.
    fn all_pads(&self) -> Vec<PadState> {
        // Dongle first: it is the only transport that holds a link
        // indefinitely, so when a controller is reachable both ways its dongle
        // entry is the one a user should reach for.
        let mut pads = self.dongle.pads();
        pads.extend(self.usb.pads());
        pads.extend(self.hub.pads());
        pads
    }
}

impl Joycon2Backend {
    /// Start the BLE hub. `pairing_enabled` gates the LTK handshake, which
    /// writes to controller flash — see `flexinput_joycon2::pairing`.
    pub fn new(pairing_enabled: bool) -> Self {
        Self {
            hub: Joycon2Hub::start(pairing_enabled),
            // Started unconditionally: it costs one idle thread polling hidapi
            // every 2 s, and a wired controller is strictly better than a
            // Bluetooth one here — Windows reclaims unpaired BLE links every
            // ~30 s and nothing we can do from a GATT client prevents it.
            usb: Joycon2UsbHub::new(),
            // Costs one thread that exits immediately when no WinUSB-bound
            // dongle is present, which is the common case.
            dongle: Joycon2DongleHub::new(),
        }
    }

    pub fn set_pairing_enabled(&self, on: bool) {
        self.hub.set_pairing_enabled(on);
    }
}

/// Device id for one half.
///
/// The BLE address is folded in because two same-side halves can be connected
/// at once (two left Joy-Cons for two players), and an index would shuffle
/// between sessions and silently repoint a user's saved wiring at the other
/// controller. The address is stable for the life of the hardware.
fn device_id(key: &PadKey) -> String {
    format!("jc2:{}:{}", kind_for(key.side).id_slug(), key.address_slug())
}

fn kind_for(side: Side) -> ControllerKind {
    match side {
        Side::Left => ControllerKind::JoyCon2L,
        Side::Right => ControllerKind::JoyCon2R,
    }
}

/// Map raw IMU counts into FlexInput's canonical frame: gyro `(roll, pitch,
/// yaw)`, accel `(forward, side, vertical)`.
///
/// This mirrors the Switch Pro parser in `gyro.rs`, which is the canonical
/// reference — accel passes straight through and gyro Y/Z are negated. That is
/// the right starting point because both are Nintendo IMUs, but a detached
/// Joy-Con is held rotated 90° from a Pro Controller, so the axes almost
/// certainly need a per-orientation rotation on top. Left deliberately
/// un-rotated until it can be checked against real hardware rather than
/// guessed — a wrong rotation here is much harder to spot than none.
/// `gyro` arrives already zero-rate corrected (see `flexinput_joycon2::GyroBias`)
/// and so is `f32` LSB rather than raw counts. Passing the uncorrected
/// `snapshot.motion.gyro` here instead is what makes an aim mapping slide
/// across the screen with the controller sitting still.
fn canonical_imu(accel: [i32; 3], gyro: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let gs = JC2_GYRO_DPS_PER_LSB / GYRO_REF_DPS;
    let as_ = JC2_ACCEL_G_PER_LSB / ACCEL_REF_G;
    (
        [
            gyro[0] * gs,
            -gyro[1] * gs,
            -gyro[2] * gs,
        ],
        [
            accel[0] as f32 * as_,
            accel[1] as f32 * as_,
            accel[2] as f32 * as_,
        ],
    )
}

fn push_common(out: &mut Vec<(String, String, Signal)>, dev: &str, pad: &PadState) {
    let b = pad.snapshot.buttons;
    let mut f = |pin: &str, v: f32| out.push((dev.into(), pin.into(), Signal::Float(v)));

    // Mouse deltas are relative and intentionally unnormalised (see layouts.rs).
    f("mouse_dx", pad.snapshot.mouse.delta_x as f32);
    f("mouse_dy", pad.snapshot.mouse.delta_y as f32);
    f("mouse_liftoff", pad.snapshot.mouse.liftoff as f32);
    f("battery", pad.snapshot.power.fraction());

    let (gyro, accel) = canonical_imu(pad.snapshot.motion.accel, pad.gyro);
    f("gyro_x", gyro[0]);
    f("gyro_y", gyro[1]);
    f("gyro_z", gyro[2]);
    f("accel_x", accel[0]);
    f("accel_y", accel[1]);
    f("accel_z", accel[2]);

    let mut bo = |pin: &str, v: bool| out.push((dev.into(), pin.into(), Signal::Bool(v)));
    bo("charging", pad.snapshot.power.charging);
    bo("btn_sl", b.sl);
    bo("btn_sr", b.sr);
}

fn push_left(out: &mut Vec<(String, String, Signal)>, dev: &str, pad: &PadState) {
    let b = pad.snapshot.buttons;
    let (x, y) = pad.stick;

    out.push((dev.into(), "left_stick".into(), Signal::Vec2(glam::Vec2::new(x, y))));
    out.push((dev.into(), "left_stick_x".into(), Signal::Float(x)));
    out.push((dev.into(), "left_stick_y".into(), Signal::Float(y)));

    // D-pad axes are derived from the discrete buttons, matching how the gilrs
    // backend builds them (see the note there about not using
    // axis_dpad_to_button).
    let dx = (b.dpad_right as i8 - b.dpad_left as i8) as f32;
    let dy = (b.dpad_up as i8 - b.dpad_down as i8) as f32;
    out.push((dev.into(), "dpad".into(), Signal::Vec2(glam::Vec2::new(dx, dy))));
    out.push((dev.into(), "dpad_x".into(), Signal::Float(dx)));
    out.push((dev.into(), "dpad_y".into(), Signal::Float(dy)));

    let mut bo = |pin: &str, v: bool| out.push((dev.into(), pin.into(), Signal::Bool(v)));
    bo("btn_lb", b.shoulder);
    bo("btn_lt_dig", b.z);
    bo("btn_ls", b.stick);
    bo("btn_back", b.minus);
    bo("btn_capture", b.capture);
    bo("dpad_up", b.dpad_up);
    bo("dpad_down", b.dpad_down);
    bo("dpad_left", b.dpad_left);
    bo("dpad_right", b.dpad_right);
}

fn push_right(out: &mut Vec<(String, String, Signal)>, dev: &str, pad: &PadState) {
    let b = pad.snapshot.buttons;
    let (x, y) = pad.stick;

    out.push((dev.into(), "right_stick".into(), Signal::Vec2(glam::Vec2::new(x, y))));
    out.push((dev.into(), "right_stick_x".into(), Signal::Float(x)));
    out.push((dev.into(), "right_stick_y".into(), Signal::Float(y)));

    let mut bo = |pin: &str, v: bool| out.push((dev.into(), pin.into(), Signal::Bool(v)));
    // Positional ids, Nintendo labels: A is East, B is South, X is North, Y is West.
    bo("btn_south", b.b);
    bo("btn_east", b.a);
    bo("btn_west", b.y);
    bo("btn_north", b.x);
    bo("btn_rb", b.shoulder);
    bo("btn_rt_dig", b.z);
    bo("btn_rs", b.stick);
    bo("btn_start", b.plus);
    bo("btn_guide", b.home);
    bo("btn_c", b.c);
}

impl DeviceBackend for Joycon2Backend {
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        self.all_pads()
            .into_iter()
            // A pad mid-initialisation has no usable signals yet; surfacing it
            // would make pins appear and then read zero for a second or two.
            .filter(|pad| pad.streaming)
            .map(|pad| {
                let kind = kind_for(pad.key.side);
                PhysicalDevice {
                    id: device_id(&pad.key),
                    display_name: pad.display_name.clone(),
                    kind,
                    outputs: layouts::outputs_for(kind),
                    inputs: layouts::inputs_for(kind),
                    // BLE controllers have no HID instance path and Windows
                    // binds no driver to them, so there is no phantom gamepad
                    // for HidHide to mask — and nothing for it to act on.
                    instance_path: None,
                    vid: Some(flexinput_joycon2::NINTENDO_VID),
                    pid: Some(match pad.key.side {
                        Side::Left => flexinput_joycon2::PID_JOYCON2_L,
                        Side::Right => flexinput_joycon2::PID_JOYCON2_R,
                    }),
                }
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        puffin::profile_function!();
        let pads = self.all_pads();
        let mut out = Vec::with_capacity(pads.len() * 32);
        for pad in pads.iter().filter(|p| p.streaming) {
            let dev = device_id(&pad.key);
            push_common(&mut out, &dev, pad);
            match pad.key.side {
                Side::Left => push_left(&mut out, &dev, pad),
                Side::Right => push_right(&mut out, &dev, pad),
            }
        }
        out
    }

    fn set_joycon2_pairing(&mut self, on: bool) {
        // Cheap: the hub stores this in an atomic and only reads it when a
        // controller is mid-initialisation, so pushing it every tick is free
        // and a live toggle applies to the next controller that connects.
        self.hub.set_pairing_enabled(on);
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        // Both transports, or a wired pad would always display 0 Hz.
        let mut counts = self.dongle.take_event_counts();
        counts.extend(self.usb.take_event_counts());
        counts.extend(self.hub.take_event_counts());
        counts
            .into_iter()
            .map(|(key, n)| (device_id(&key), n))
            .collect()
    }

    fn send(&mut self, device_id_in: &str, pin_id: &str, signal: Signal) {
        let Some(pad) = self
            .hub
            .pads()
            .into_iter()
            .find(|p| device_id(&p.key) == device_id_in)
        else {
            return;
        };
        if pin_id == "player_led" {
            let Signal::Float(v) = signal else { return };
            // 0=off, 0.25=P1 … 1.0=P4, matching the DualSense pin encoding.
            let mask = match (v * 4.0).round() as i32 {
                n @ 1..=4 => 1u8 << (n - 1),
                _ => 0,
            };
            self.hub.set_player_led(pad.key, mask);
        }
        // HD rumble is declared in the layout but not encoded yet — the
        // 16-byte LRA packing is a separate reverse-engineering job from the
        // Switch Pro's, and sending a malformed frame is worse than silence.
    }
}

/// Pin ids the backend actually drives, used by the test below to keep the
/// declared layout and the emitted signals from drifting apart.
#[cfg(test)]
fn emitted_pins(side: Side) -> Vec<(String, String, Signal)> {
    let pad = PadState {
        key: PadKey {
            side,
            address: [0; 6],
        },
        display_name: String::new(),
        connected: true,
        streaming: true,
        snapshot: Default::default(),
        stick: (0.0, 0.0),
        gyro: [0.0; 3],
        events: 0,
    };
    let mut out = Vec::new();
    push_common(&mut out, "dev", &pad);
    match side {
        Side::Left => push_left(&mut out, "dev", &pad),
        Side::Right => push_right(&mut out, "dev", &pad),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every pin the backend emits must be declared in the layout, or a sink
    /// can never bind it and the signal is silently discarded. This is the
    /// failure mode that a new device layout gets wrong most often.
    #[test]
    fn every_emitted_pin_is_declared_in_the_layout() {
        for side in [Side::Left, Side::Right] {
            let kind = kind_for(side);
            let declared: HashSet<String> =
                layouts::outputs_for(kind).into_iter().map(|p| p.id).collect();
            for (_, pin, _) in emitted_pins(side) {
                assert!(
                    declared.contains(&pin),
                    "{kind:?} emits `{pin}` but does not declare it",
                );
            }
        }
    }

    /// The reverse direction: a declared pin nothing ever drives shows up in
    /// the UI as a dead output. `automap_out` is the documented exception —
    /// the bus port is appended by `outputs_for` and driven elsewhere.
    #[test]
    fn every_declared_pin_is_driven() {
        for side in [Side::Left, Side::Right] {
            let kind = kind_for(side);
            let emitted: HashSet<String> =
                emitted_pins(side).into_iter().map(|(_, p, _)| p).collect();
            for pin in layouts::outputs_for(kind).into_iter().map(|p| p.id) {
                if pin == "automap_out" {
                    continue;
                }
                assert!(emitted.contains(&pin), "{kind:?} declares `{pin}` but never drives it");
            }
        }
    }

    /// Two same-side halves must not collide. An index-based id would, which is
    /// why the BLE address is in there.
    #[test]
    fn two_left_joycons_get_distinct_ids() {
        let a = PadKey { side: Side::Left, address: [0xAA; 6] };
        let b = PadKey { side: Side::Left, address: [0xBB; 6] };
        assert_ne!(device_id(&a), device_id(&b));
        assert!(device_id(&a).starts_with("jc2:joycon2_l:"));
    }

    #[test]
    fn sides_map_to_distinct_kinds_and_slugs() {
        assert_eq!(kind_for(Side::Left), ControllerKind::JoyCon2L);
        assert_eq!(kind_for(Side::Right), ControllerKind::JoyCon2R);
        assert_ne!(
            ControllerKind::JoyCon2L.id_slug(),
            ControllerKind::JoyCon2R.id_slug()
        );
    }

    /// A resting IMU must read zero on every axis, whatever the axis mapping
    /// ends up being — this catches a sign or index slip that leaves a constant
    /// bias on the gyro, which would make a gyro-aim patch drift forever.
    #[test]
    fn zero_imu_counts_produce_zero_signal() {
        let (gyro, accel) = canonical_imu([0; 3], [0.0; 3]);
        assert_eq!(gyro, [0.0, 0.0, 0.0]);
        assert_eq!(accel, [0.0, 0.0, 0.0]);
    }

    /// Full-scale counts land at ±1.0, matching every other pad's normalisation
    /// so gyro-aim sensitivity carries across controllers.
    #[test]
    fn full_scale_imu_counts_normalise_to_one() {
        let (gyro, _) = canonical_imu([0; 3], [i16::MAX as f32; 3]);
        assert!((gyro[0] - 1.0).abs() < 1e-4, "gyro roll {}", gyro[0]);
        // Y/Z are negated relative to raw, matching the Switch Pro reference.
        assert!((gyro[1] + 1.0).abs() < 1e-4);
        assert!((gyro[2] + 1.0).abs() < 1e-4);
    }

    /// One g of gravity must read as 1/8 of full scale, since the graph's
    /// accel reference is ±8 g. Anchors the measured 4096 LSB/g against the
    /// shared normalisation so a change to either is caught.
    #[test]
    fn one_g_reads_as_an_eighth_of_full_scale() {
        let lsb = flexinput_joycon2::reports::ACCEL_LSB_PER_G as i32;
        let (_, accel) = canonical_imu([lsb, 0, 0], [0.0; 3]);
        assert!((accel[0] - 0.125).abs() < 1e-4, "1 g read as {}", accel[0]);

        // And a full-scale ±8 g excursion saturates at ±1.0.
        let (_, accel) = canonical_imu([lsb * 8, 0, 0], [0.0; 3]);
        assert!((accel[0] - 1.0).abs() < 1e-3, "8 g read as {}", accel[0]);
    }

    #[test]
    fn player_led_float_maps_to_a_single_lamp_bit() {
        // Encoding shared with the DualSense pin: 0=off, 0.25=P1 … 1.0=P4.
        let mask = |v: f32| match (v * 4.0).round() as i32 {
            n @ 1..=4 => 1u8 << (n - 1),
            _ => 0,
        };
        assert_eq!(mask(0.0), 0b0000);
        assert_eq!(mask(0.25), 0b0001);
        assert_eq!(mask(0.5), 0b0010);
        assert_eq!(mask(0.75), 0b0100);
        assert_eq!(mask(1.0), 0b1000);
    }
}
