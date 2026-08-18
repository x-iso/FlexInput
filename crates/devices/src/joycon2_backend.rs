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
        // Dongle FIRST, so its readiness flag exists before the hub can scan.
        // Constructing the hub first leaves a window in which the Windows stack
        // scans, spots a remembered controller and auto-connects it — after
        // which the dongle cannot see the pad at all.
        let dongle = Joycon2DongleHub::new();
        // ❗ The flag goes in at construction. Handing it over afterwards left
        // a window in which this hub scanned with no flag set, which Windows
        // used to claim a remembered controller before the dongle was ready —
        // see `Joycon2Hub::start`.
        let hub = Joycon2Hub::start(pairing_enabled, Some(dongle.state_flag()));
        Self {
            hub,
            // Started unconditionally: it costs one idle thread polling hidapi
            // every 2 s, and a wired controller is strictly better than a
            // Bluetooth one here — Windows reclaims unpaired BLE links every
            // ~30 s and nothing we can do from a GATT client prevents it.
            usb: Joycon2UsbHub::new(),
            dongle,
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
///
/// ❗ `gyro` now arrives already in **degrees per second**, from
/// [`flexinput_joycon2::reports::OrientationTracker`], so the only conversion
/// left is into the shared normalised unit. It used to arrive as angle counts
/// per report from differencing three fields, two of which are not angles.
///
/// The Y/Z negation is unchanged and still unverified — it is a convention
/// question, and a per-device baseline in calibration cancels a fixed frame
/// offset either way.
fn canonical_imu(accel: [i32; 3], gyro_dps: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let gs = 1.0 / GYRO_REF_DPS;
    let as_ = JC2_ACCEL_G_PER_LSB / ACCEL_REF_G;
    let a = flexinput_joycon2::reports::to_canonical_accel(accel);
    (
        // ❗ NO per-axis negation here any more. It was inherited from the
        // Switch Pro parser, which signs RAW SENSOR axes; these rates are not
        // raw sensor output — they are differences of angles already computed
        // in the canonical frame, so negating them again mirrors two axes.
        [gyro_dps[0] * gs, gyro_dps[1] * gs, gyro_dps[2] * gs],
        [a[0] as f32 * as_, a[1] as f32 * as_, a[2] as f32 * as_],
    )
}

/// Build an absolute orientation quaternion: gravity for roll and pitch, the
/// heading field for yaw.
///
/// ⭐ **This used to compose three `Motion::angle` fields as Euler angles, and
/// that is why the 3D model tumbled** — spinning through several revolutions on
/// a small turn, with the calibration baseline unable to help. Only one of those
/// three fields is an angle. The other two were exhaustively disproven (see
/// [`flexinput_joycon2::reports::Motion::angle`]), so no composition order,
/// handedness or per-axis scale could ever have made them work: the inputs were
/// wrong, not the arithmetic.
///
/// What replaces them needs no reverse engineering at all:
///
/// * **roll and pitch from gravity.** The accelerometer is the best-established
///   thing on this controller — 4096 LSB/g, confirmed by measurement and then
///   independently by the device's own scale block.
/// * **yaw from the heading field**, which is absolute and magnetometer-
///   corrected: ≤4.5° drift across a full 360° turn.
///
/// So the composition is no longer a guess either. Roll and pitch are defined
/// as rotations that bring the measured gravity vector back to vertical, and
/// yaw is applied about that vertical — which is exactly `Rz · Ry · Rx`.
///
/// A per-device baseline captured during calibration still cancels any fixed
/// mounting offset on top of this.
fn canonical_orientation(euler: [f32; 3]) -> glam::Quat {
    let [roll, pitch, yaw] = euler;
    // ❗ Takes the euler the TRANSPORT already computed, rather than
    // recomputing from the raw report. Yaw needs unwrapping and unwrapping is
    // stateful: the heading field spans half a turn and wraps twice per
    // revolution, so anything recomputing it without wrap history flips 180
    // degrees at every seam. That showed up as instant full-scale jumps on an
    // otherwise noisy yaw trace.
    //
    // The rotations are INVERTED relative to the angles because this quaternion
    // must take the body frame to the world frame: pitch and yaw are stored
    // with the canonical sign convention (pitch + nose-up, yaw + clockwise),
    // and undoing a nose-up tilt is a nose-DOWN rotation. Pinned by the test
    // that rotates measured gravity onto world up.
    glam::Quat::from_rotation_z(yaw)
        * glam::Quat::from_rotation_y(-pitch)
        * glam::Quat::from_rotation_x(roll)
}

fn push_common(out: &mut Vec<(String, String, Signal)>, dev: &str, pad: &PadState) {
    let b = pad.snapshot.buttons;
    let mut f = |pin: &str, v: f32| out.push((dev.into(), pin.into(), Signal::Float(v)));

    // Mouse deltas are relative and intentionally unnormalised (see layouts.rs).
    f("mouse_dx", pad.snapshot.mouse.delta_x as f32);
    f("mouse_dy", pad.snapshot.mouse.delta_y as f32);
    f("mouse_liftoff", pad.snapshot.mouse.liftoff as f32);
    f("battery", pad.snapshot.power.fraction());

    // ⭐ The gyro pins now carry the FIELD-derived rate, not the accel-derived
    // one.
    //
    // `pad.gyro` differentiates tilt read from the accelerometer, which is the
    // worst available rate source at exactly the moment it matters: during a
    // flick the accel measures centripetal load on top of gravity, so the tilt
    // it implies is wrong precisely when the rate is largest. Captured sweep
    // frames read 2627/649/3088 against a 4096 magnitude — the "gravity"
    // direction there is fiction.
    //
    // The field-derived rate has none of that. It is the controller's own
    // integrated motion, differenced back into a rate, and on hardware it is
    // the difference between motion controls that are unusable and ones that
    // are not. Putting it on `gyro_x/y/z` means the existing curve, filter and
    // 3DOF tooling applies to it without anyone having to wire a probe pin.
    let (gyro, accel) = canonical_imu(
        pad.snapshot.motion.accel,
        flexinput_joycon2::reports::canonical_field_rate(pad.field_rate),
    );
    f("gyro_x", gyro[0]);
    f("gyro_y", gyro[1]);
    f("gyro_z", gyro[2]);
    f("accel_x", accel[0]);
    f("accel_y", accel[1]);
    f("accel_z", accel[2]);

    // Absolute orientation. Pushed here rather than beside the accel pins
    // because `f` above holds a unique borrow of `out` for its whole scope.
    let q = canonical_orientation(pad.orientation);
    out.push((
        dev.into(),
        "orientation".into(),
        Signal::Vec4(glam::Vec4::new(q.x, q.y, q.z, q.w)),
    ));

    push_probe(out, dev, &pad.snapshot.motion);
    // Normalised by the same reference as `gyro_x/y/z`, so the two are
    // interchangeable in a patch and a difference in feel is a difference in
    // SENSOR, not in scaling.
    for (i, v) in pad.field_rate.iter().enumerate() {
        out.push((
            dev.into(),
            format!("probe_rate_{i}"),
            Signal::Float(v / GYRO_REF_DPS),
        ));
    }

    let mut bo = |pin: &str, v: bool| out.push((dev.into(), pin.into(), Signal::Bool(v)));
    bo("charging", pad.snapshot.power.charging);
    bo("btn_sl", b.sl);
    bo("btn_sr", b.sr);
}

/// Full scale of each probe reading, used only to bring the pins into the
/// ±1 range every other Float pin lives in.
const PROBE_I16_FULL_SCALE: f32 = 32768.0;
const PROBE_I24_FULL_SCALE: f32 = (1u32 << 23) as f32;

/// Push the undecoded motion bytes out as pins — see `joycon2_probe_outputs`.
///
/// Scaled, never re-interpreted. Dividing by full scale changes no shape and
/// loses no information; it just puts the value where FlexInput's displays,
/// curves and AutoMap already work. Anything more (bias removal, differencing,
/// axis permutation) would be another decode, and a decode is exactly what
/// these pins exist to avoid — the reader is the judge here, not the code.
fn push_probe(out: &mut Vec<(String, String, Signal)>, dev: &str, m: &flexinput_joycon2::reports::Motion) {
    for (i, v) in m.probe.iter().enumerate() {
        out.push((
            dev.into(),
            format!("probe_i16_{i}"),
            Signal::Float(*v as f32 / PROBE_I16_FULL_SCALE),
        ));
    }
    for (i, v) in m.angle.iter().enumerate() {
        out.push((
            dev.into(),
            format!("probe_i24_{i}"),
            Signal::Float(*v as f32 / PROBE_I24_FULL_SCALE),
        ));
    }
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

    // Absolute orientation. Pushed here rather than beside the accel pins
    // because `f` above holds a unique borrow of `out` for its whole scope.
    let q = canonical_orientation(pad.orientation);
    out.push((
        dev.into(),
        "orientation".into(),
        Signal::Vec4(glam::Vec4::new(q.x, q.y, q.z, q.w)),
    ));

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

    // Absolute orientation. Pushed here rather than beside the accel pins
    // because `f` above holds a unique borrow of `out` for its whole scope.
    let q = canonical_orientation(pad.orientation);
    out.push((
        dev.into(),
        "orientation".into(),
        Signal::Vec4(glam::Vec4::new(q.x, q.y, q.z, q.w)),
    ));

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
        field_rate: [0.0; 3],
        orientation: [0.0; 3],
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

    /// A full-scale ROTATION RATE lands at ±1.0, matching every other pad's
    /// normalisation so gyro-aim sensitivity carries across controllers.
    ///
    /// ❗ This has now been rewritten twice, each time because the UNIT of the
    /// gyro input changed under it. First it fed `i16::MAX` raw counts; then
    /// angle counts per report; now degrees per second, which is what
    /// `OrientationTracker` produces. Each rewrite is a reminder that a test
    /// asserting a number without asserting its unit is barely a test — so the
    /// input here is spelled as a physical rate and nothing else.
    #[test]
    fn full_scale_rotation_rate_normalises_to_one() {
        let (gyro, _) = canonical_imu([0; 3], [GYRO_REF_DPS; 3]);
        // ❗ All three POSITIVE. The old version expected Y and Z negated,
        // inherited from the Switch Pro parser — but that parser signs RAW
        // SENSOR axes, and these rates are differences of angles already in the
        // canonical frame. Negating them again mirrored two axes, which is
        // exactly the "tumbling in all sorts of wrong ways" symptom.
        for (i, g) in gyro.iter().enumerate() {
            assert!((g - 1.0).abs() < 1e-4, "gyro axis {i} = {g}, expected 1.0");
        }
    }

    /// A realistic hand rotation must produce a usable, non-tiny signal.
    ///
    /// Catches "the gyro technically works but the cursor barely moves", which
    /// is how a scale error shows up on hardware rather than as an obvious
    /// failure.
    #[test]
    fn a_brisk_hand_rotation_produces_a_usable_signal() {
        let (gyro, _) = canonical_imu([0; 3], [180.0, 0.0, 0.0]);
        let expect = 180.0 / GYRO_REF_DPS;
        assert!(
            (gyro[0] - expect).abs() < 1e-3,
            "180 deg/s gave {}, expected {expect}",
            gyro[0]
        );
        assert!(gyro[0] > 0.05, "signal too small to aim with: {}", gyro[0]);
    }

    /// The orientation must actually undo the measured tilt.
    ///
    /// This is the property the old three-Euler-angle version could not hold,
    /// and the reason the 3D model tumbled: rotating the measured gravity
    /// vector by the orientation has to land on world up, in every pose. It is
    /// the same geometric check that `gravity_solve` used to reject the old
    /// decode, applied here as a regression test.
    ///
    /// It also pins the SIGN of pitch, which has been wrong in both directions
    /// now — once making the model tumble, once making `gyro_y` read backwards
    /// against every other pad.
    #[test]
    fn the_orientation_brings_measured_gravity_back_to_vertical() {
        use flexinput_joycon2::reports::{
            tilt_from_accel, to_canonical_accel, ACCEL_LSB_PER_G,
        };
        let g = ACCEL_LSB_PER_G as i32;
        let h = (g as f32 / 2f32.sqrt()) as i32;
        for raw in [[0, 0, g], [0, g, 0], [g, 0, 0], [0, h, h], [h, 0, h], [-h, h, 0]] {
            let c = to_canonical_accel(raw);
            let (roll, pitch) = tilt_from_accel(c);
            // Yaw rotates ABOUT gravity, so it cannot affect this — varied
            // deliberately to prove it does not.
            for yaw in [0.0f32, 1.3, -2.6] {
                let q = canonical_orientation([roll, pitch, yaw]);
                let a = glam::Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32).normalize();
                let up = (q * a).normalize();
                assert!(
                    up.z > 0.999,
                    "raw {raw:?} yaw {yaw} left gravity at {up:?}"
                );
            }
        }
    }

    /// Pitch must be POSITIVE NOSE-UP, matching the canonical contract.
    ///
    /// The contract says `accel_x > 0` when the nose tilts up and `gyro_y` is
    /// pitch positive nose-up, so the angle has to rise with canonical x.
    /// Getting this backwards inverts `gyro_y` against every other pad while
    /// still producing a perfectly self-consistent orientation — which is why
    /// it needs its own test rather than being implied by the gravity check.
    #[test]
    fn pitch_is_positive_nose_up_and_roll_positive_right_grip_down() {
        use flexinput_joycon2::reports::{tilt_from_accel, ACCEL_LSB_PER_G};
        let g = ACCEL_LSB_PER_G as i32;
        // Canonical accel: nose up puts gravity on +x.
        let (_, pitch) = tilt_from_accel([g, 0, 0]);
        assert!(pitch > 1.0, "nose-up pitch should be strongly positive, got {pitch}");
        // Right grip down puts gravity on +y.
        let (roll, _) = tilt_from_accel([0, g, 0]);
        assert!(roll > 1.0, "right-grip-down roll should be positive, got {roll}");
    }

    /// One g of gravity must read as 1/8 of full scale, since the graph's
    /// accel reference is ±8 g. Anchors the measured 4096 LSB/g against the
    /// shared normalisation so a change to either is caught.
    #[test]
    fn one_g_reads_as_an_eighth_of_full_scale() {
        let lsb = flexinput_joycon2::reports::ACCEL_LSB_PER_G as i32;
        // Raw axis 1 is the canonical FORWARD axis — see `to_canonical_accel`.
        let (_, accel) = canonical_imu([0, lsb, 0], [0.0; 3]);
        assert!((accel[0] - 0.125).abs() < 1e-4, "1 g read as {}", accel[0]);

        // And a full-scale +/-8 g excursion saturates at +/-1.0.
        let (_, accel) = canonical_imu([0, lsb * 8, 0], [0.0; 3]);
        assert!((accel[0] - 1.0).abs() < 1e-3, "8 g read as {}", accel[0]);
    }

    /// The canonical permutation itself, stated as the contract rather than as
    /// three index swaps — so a future change has to disagree with the CONTRACT
    /// to break it, not merely with the previous implementation.
    #[test]
    fn accel_lands_in_the_canonical_forward_side_vertical_frame() {
        use flexinput_joycon2::reports::ACCEL_LSB_PER_G;
        let lsb = ACCEL_LSB_PER_G as i32;
        let axis_of = |raw: [i32; 3]| {
            let (_, a) = canonical_imu(raw, [0.0; 3]);
            a
        };
        // Grip flat, face up: gravity on canonical +z and nothing else.
        let flat = axis_of([0, 0, lsb]);
        assert!(flat[2] > 0.12 && flat[0].abs() < 1e-3 && flat[1].abs() < 1e-3, "flat {flat:?}");
        // Raw 1 is forward; raw 0 is side, INVERTED. Both were passed straight
        // through before, which put this pad's accel in a different order and
        // sign from the DualSense and Switch Pro on the same motion.
        assert!(axis_of([0, lsb, 0])[0] > 0.12, "raw axis 1 must be canonical +x");
        assert!(axis_of([lsb, 0, 0])[1] < -0.12, "raw axis 0 must be canonical -y");
    }

    /// The probe pins must be a pure rescale of the report bytes.
    ///
    /// This is the one property that makes them worth having: the moment
    /// anything here filters, biases or permutes, the pins stop answering "what
    /// does the hardware send" and start answering "what does this code think
    /// the hardware sends" — which is the question that has already been
    /// answered wrongly several times.
    #[test]
    fn probe_pins_are_a_pure_rescale_of_the_raw_bytes() {
        use flexinput_joycon2::reports::Motion;
        let m = Motion {
            probe: [i16::MIN, -1, 0, 1, 16384, i16::MAX],
            angle: [-(1 << 23), 0, (1 << 23) - 1],
            ..Default::default()
        };
        let mut out = Vec::new();
        push_probe(&mut out, "dev", &m);

        let get = |pin: &str| {
            out.iter()
                .find(|(_, p, _)| p == pin)
                .map(|(_, _, s)| match s {
                    Signal::Float(v) => *v,
                    other => panic!("{pin} is {other:?}, not a Float"),
                })
                .unwrap_or_else(|| panic!("{pin} was never pushed"))
        };

        // Full scale lands at -1, and no value can exceed it.
        assert_eq!(get("probe_i16_0"), -1.0);
        assert_eq!(get("probe_i16_2"), 0.0);
        assert_eq!(get("probe_i16_4"), 0.5);
        assert!((get("probe_i16_5") - 1.0).abs() < 1e-4);
        assert_eq!(get("probe_i24_0"), -1.0);
        assert_eq!(get("probe_i24_1"), 0.0);
        assert!((get("probe_i24_2") - 1.0).abs() < 1e-6);

        // Sign and ORDER survive: #1 and #3 differ only in sign, and neither is
        // silently zeroed the way a "clean up the noise" filter would.
        assert!(get("probe_i16_1") < 0.0 && get("probe_i16_3") > 0.0);
        assert_eq!(get("probe_i16_1"), -get("probe_i16_3"));
    }

    /// A resting controller must NOT read zero on the probe pins.
    ///
    /// Sounds backwards, but it is the point: these pins exist to show what is
    /// actually in the bytes, and the real at-rest capture has two fields near
    /// full scale. If a future change makes them quietly read zero at rest —
    /// the shape someone would "fix" them into, expecting a gyro — the pins
    /// have stopped reporting and started asserting.
    #[test]
    fn the_probe_pins_report_the_resting_values_hardware_actually_sends() {
        use flexinput_joycon2::reports::Motion;
        let m = Motion {
            // From the pinned at-rest capture in `reports.rs`.
            probe: [30720, -153, -767, -24, -1536, 32752],
            ..Default::default()
        };
        let mut out = Vec::new();
        push_probe(&mut out, "dev", &m);
        let v = |pin: &str| match out.iter().find(|(_, p, _)| p == pin).unwrap().2 {
            Signal::Float(v) => v,
            _ => unreachable!(),
        };
        assert!(v("probe_i16_0") > 0.9, "resting #0 should be near full scale");
        assert!(v("probe_i16_5") > 0.9, "resting #5 should be near full scale");
        assert!(v("probe_i16_1").abs() < 0.01);
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
