//! Parsing for Joy-Con 2 input reports 0x07 (left) and 0x08 (right).
//!
//! Layout from `hid_reports.md`. Over BLE the report id byte is omitted, so a
//! notification payload is 63 bytes and offset 0 is the report counter.

use crate::protocol::Side;

/// Full length of a BLE input notification. Shorter payloads are still parsed
/// as far as they go — the motion and mouse blocks are optional features and a
/// controller with them disabled has been observed to truncate.
pub const INPUT_REPORT_LEN: usize = 63;

const OFF_COUNTER: usize = 0x00;
const OFF_POWER: usize = 0x01;
const OFF_BUTTONS: usize = 0x02;
const OFF_STICK: usize = 0x05;
const OFF_MOUSE: usize = 0x09;
const OFF_MOTION_LEN: usize = 0x0F;
const OFF_MOTION: usize = 0x10;

/// Timestamp, first field of the motion block. Increments by 12 per report.
const OFF_MOTION_TIMESTAMP: usize = OFF_MOTION;
/// Accelerometer: three little-endian **i32** at `0x22`, `0x26`, `0x2A` — the
/// last 12 bytes of a 30-byte motion block.
///
/// Established from a hardware capture, not from the spec. At rest the three
/// values form a vector of essentially constant magnitude (≈4123, 4099, 4110 …
/// across samples) — gravity, giving 1 g ≈ 4096 LSB. Nothing else in the block
/// behaves like an accelerometer.
///
/// `hid_reports.md` documents the block as
/// `[timestamp u32][temperature u16][accel xyz i16][gyro xyz i16]`, which put
/// accel at `0x16`. That is wrong for report 0x07/0x08 over BLE: reading there
/// produced one smooth axis and two channels of noise, and the field the doc
/// calls `gyro y` was a slowly incrementing counter.
/// Offsets below are for the RIGHT half. The LEFT half's report is identical
/// but shifted one byte EARLIER — see [`left_shift`].
const OFF_MOTION_ACCEL: usize = 0x22;

/// The left half's report is offset one byte earlier than the right's.
///
/// Measured, not assumed: a guided motion sweep found the accelerometer at
/// 33/37/41 on the left and 34/38/42 on the right, and every other responsive
/// field showed the same one-byte difference. The right half carries one extra
/// byte ahead of the motion block.
///
/// This was silently breaking the left half entirely. `motion_len` was read one
/// byte late — 0x0b instead of 0x1e — which failed the length guard, so the
/// left half's motion was never parsed at all.
fn left_shift(side: Side) -> usize {
    match side {
        Side::Left => 1,
        Side::Right => 0,
    }
}

/// One IMU sample.
///
/// The block is 30 bytes (`motion_len` = 30 in every steady-state report, 4 in
/// the first). The 12 bytes at [`OFF_MOTION_ACCEL`] are the accelerometer; the
/// 14 bytes between the timestamp and it are **not yet identified**, and the
/// gyro is somewhere in them. It cannot be located from an at-rest capture,
/// because at rest the gyro reads ~0 and is indistinguishable from padding —
/// it needs a capture taken while the controller is deliberately rotated.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Motion {
    /// Increments by 12 per report.
    pub timestamp: u32,
    /// Accelerometer, raw signed LSB. **1 g = [`ACCEL_LSB_PER_G`]**, established
    /// from hardware: the three axes form a vector of constant magnitude ≈4096
    /// in every orientation (measured error 2–5% across a full sweep).
    ///
    /// Each axis is an **i16 followed by two zero bytes**, on a 4-byte stride —
    /// NOT an i32, despite the padding making it look like one. Reading it as
    /// i32 works only for positive values: `aa ff 00 00` is −86 as i16 but
    /// +65450 as i32, so every negative reading came out as a large positive
    /// one. That is what made two axes look like "a mess".
    pub accel: [i32; 3],
    /// Gyro, raw signed LSB. **Not yet located in the report** — see
    /// [`parse_input`]. Always zero for now, deliberately: feeding the bytes we
    /// used to read here sent garbage into any aim mapping.
    pub gyro: [i16; 3],
}

/// Accelerometer counts per g, measured from hardware (see [`Motion::accel`]).
pub const ACCEL_LSB_PER_G: f32 = 4096.0;

/// Relative motion from the Joy-Con 2's optical mouse sensor (feature bit 4).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Mouse {
    pub delta_x: i16,
    pub delta_y: i16,
    /// The research doc labels this "Lift-off distance?" — surfaced raw.
    pub liftoff: u8,
}

/// Battery / charge state from the power-info bitfield.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Power {
    pub external_power: bool,
    pub charging: bool,
    /// 0–9 as reported by the controller.
    pub level: u8,
}

impl Power {
    /// Charge as a 0.0–1.0 fraction, matching how the devices crate surfaces
    /// battery for every other pad.
    pub fn fraction(self) -> f32 {
        (self.level as f32 / 9.0).clamp(0.0, 1.0)
    }
}

/// Button state, named positionally where the two halves agree and by physical
/// label where they don't. Only the fields relevant to a given side are ever
/// set; the rest stay false.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons {
    // Shared across both halves.
    pub stick: bool,
    /// ZL on the left half, ZR on the right.
    pub z: bool,
    /// L on the left half, R on the right.
    pub shoulder: bool,
    pub sl: bool,
    pub sr: bool,

    // Left half only.
    pub minus: bool,
    pub capture: bool,
    pub dpad_up: bool,
    pub dpad_down: bool,
    pub dpad_left: bool,
    pub dpad_right: bool,

    // Right half only.
    pub plus: bool,
    pub home: bool,
    /// The Switch 2's new "C" (GameChat) button.
    pub c: bool,
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
}

/// Everything one input notification carries.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadSnapshot {
    pub counter: u8,
    pub power: Power,
    pub buttons: Buttons,
    /// Raw uncalibrated 12-bit stick, 0–4095 per axis.
    pub stick_raw: (u16, u16),
    pub mouse: Mouse,
    pub motion: Motion,
    /// Raw value of the motion-length field. Zero means the IMU block was
    /// absent and [`PadSnapshot::motion`] is stale/default.
    pub motion_len: u8,
}

fn i16le(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

/// Unpack the 3-byte, two-channel 12-bit stick encoding into `(x, y)`, each
/// 0–4095.
///
/// `[x_lo][y_lo << 4 | x_hi][y_hi]`
pub fn unpack_stick(b: &[u8]) -> (u16, u16) {
    let x = b[0] as u16 | ((b[1] as u16 & 0x0F) << 8);
    let y = (b[1] as u16 >> 4) | ((b[2] as u16) << 4);
    (x, y)
}

fn parse_buttons(side: Side, b0: u8, b1: u8) -> Buttons {
    let mut btn = Buttons {
        stick: b0 & 0x80 != 0,
        z: b0 & 0x20 != 0,
        shoulder: b0 & 0x10 != 0,
        sl: b1 & 0x80 != 0,
        sr: b1 & 0x40 != 0,
        ..Default::default()
    };
    match side {
        // byte0: Stick Minus ZL L Up Left Right Down
        // byte1: SL SR - - - - - Capture
        Side::Left => {
            btn.minus = b0 & 0x40 != 0;
            btn.dpad_up = b0 & 0x08 != 0;
            btn.dpad_left = b0 & 0x04 != 0;
            btn.dpad_right = b0 & 0x02 != 0;
            btn.dpad_down = b0 & 0x01 != 0;
            btn.capture = b1 & 0x01 != 0;
        }
        // byte0: Stick Plus ZR R X Y A B
        // byte1: SL SR - C - - - Home
        Side::Right => {
            btn.plus = b0 & 0x40 != 0;
            btn.x = b0 & 0x08 != 0;
            btn.y = b0 & 0x04 != 0;
            btn.a = b0 & 0x02 != 0;
            btn.b = b0 & 0x01 != 0;
            btn.c = b1 & 0x10 != 0;
            btn.home = b1 & 0x01 != 0;
        }
    }
    btn
}

/// Parse an input notification. Returns `None` if the payload is too short to
/// contain even buttons and a stick.
pub fn parse_input(side: Side, payload: &[u8]) -> Option<PadSnapshot> {
    if payload.len() < OFF_STICK + 3 {
        return None;
    }

    let power_bits = payload[OFF_POWER];
    let mut snap = PadSnapshot {
        counter: payload[OFF_COUNTER],
        power: Power {
            external_power: power_bits & 0x01 != 0,
            charging: power_bits & 0x02 != 0,
            level: (power_bits >> 2) & 0x0F,
        },
        buttons: parse_buttons(side, payload[OFF_BUTTONS], payload[OFF_BUTTONS + 1]),
        stick_raw: unpack_stick(&payload[OFF_STICK..OFF_STICK + 3]),
        ..Default::default()
    };

    if payload.len() >= OFF_MOUSE + 5 {
        snap.mouse = Mouse {
            delta_x: i16le(payload, OFF_MOUSE),
            delta_y: i16le(payload, OFF_MOUSE + 2),
            liftoff: payload[OFF_MOUSE + 4],
        };
    }

    let sh = left_shift(side);
    if payload.len() > OFF_MOTION_LEN - sh {
        snap.motion_len = payload[OFF_MOTION_LEN - sh];
        // A full block is 30 bytes; the very first report after init carries 4
        // and no sensor data. Require enough length for the accel field rather
        // than for the (wrong) 18-byte layout the spec describes.
        let need = OFF_MOTION_ACCEL + 12 - OFF_MOTION;
        if snap.motion_len as usize >= need && payload.len() >= OFF_MOTION_ACCEL + 12 {
            let t = OFF_MOTION_TIMESTAMP - sh;
            let a = OFF_MOTION_ACCEL - sh;
            snap.motion = Motion {
                timestamp: u32::from_le_bytes([
                    payload[t], payload[t + 1], payload[t + 2], payload[t + 3],
                ]),
                // i16 on a 4-byte stride, widened for the caller. The two
                // bytes after each axis are padding, not part of the value.
                accel: [
                    i16le(payload, a) as i32,
                    i16le(payload, a + 4) as i32,
                    i16le(payload, a + 8) as i32,
                ],
                // Not located yet — see the note on `OFF_MOTION_ACCEL`.
                // Deliberately left at zero rather than pointed at a guess: the
                // previous offsets fed noise straight into gyro aim, and
                // FlexInput's gamepad nav drives the cursor from `gyro_*`
                // whether or not the user mapped anything.
                gyro: [0; 3],
            };
        }
    }

    Some(snap)
}

/// Per-axis stick normalisation.
///
/// Factory calibration lives in controller memory at `0x13080` and is not read
/// yet, so the range is learned at runtime instead: centre is taken from the
/// first sample (the stick is at rest when a controller connects) and the
/// extents widen as the user moves it. That converges within one full circle
/// and never reports beyond ±1.
#[derive(Debug, Clone, Copy)]
pub struct StickCalib {
    centre: (u16, u16),
    min: (u16, u16),
    max: (u16, u16),
    seeded: bool,
}

/// Nominal half-travel for a 12-bit stick, used until the real extents are
/// observed. Deliberately conservative: too small and the axis saturates early
/// (which the user notices), too large and full deflection never reaches 1.0.
const NOMINAL_HALF_RANGE: u16 = 1400;

impl Default for StickCalib {
    fn default() -> Self {
        Self {
            centre: (2048, 2048),
            min: (2048 - NOMINAL_HALF_RANGE, 2048 - NOMINAL_HALF_RANGE),
            max: (2048 + NOMINAL_HALF_RANGE, 2048 + NOMINAL_HALF_RANGE),
            seeded: false,
        }
    }
}

impl StickCalib {
    /// Feed a raw sample and return the normalised `(x, y)` in −1..=1, with y
    /// flipped so +Y is up (the report counts downward).
    pub fn normalize(&mut self, raw: (u16, u16)) -> (f32, f32) {
        if !self.seeded {
            self.centre = raw;
            self.min = (
                raw.0.saturating_sub(NOMINAL_HALF_RANGE),
                raw.1.saturating_sub(NOMINAL_HALF_RANGE),
            );
            self.max = (
                raw.0.saturating_add(NOMINAL_HALF_RANGE),
                raw.1.saturating_add(NOMINAL_HALF_RANGE),
            );
            self.seeded = true;
        }
        self.min = (self.min.0.min(raw.0), self.min.1.min(raw.1));
        self.max = (self.max.0.max(raw.0), self.max.1.max(raw.1));

        let axis = |v: u16, centre: u16, min: u16, max: u16| -> f32 {
            if v >= centre {
                let span = (max - centre).max(1) as f32;
                ((v - centre) as f32 / span).clamp(0.0, 1.0)
            } else {
                let span = (centre - min).max(1) as f32;
                -((centre - v) as f32 / span).clamp(0.0, 1.0)
            }
        };

        let x = axis(raw.0, self.centre.0, self.min.0, self.max.0);
        let y = axis(raw.1, self.centre.1, self.min.1, self.max.1);
        (x, y)
    }
}

/// Zero-rate offset tracking for the gyro.
///
/// Every MEMS gyro reads non-zero when perfectly still, by tens to hundreds of
/// LSB. Feeding that straight into an aim mapping integrates it into constant
/// cursor drift — the bias, not noise, is what makes a gyro cursor slide across
/// the screen on its own.
///
/// The estimate is learned automatically: while the controller is judged to be
/// at rest, the bias converges on the average reading. Motion is never used to
/// update it, so putting the controller down re-converges but waving it around
/// cannot corrupt the estimate.
#[derive(Debug, Clone, Copy)]
pub struct GyroBias {
    bias: [f32; 3],
    last: [f32; 3],
    /// Consecutive samples that have looked like rest.
    still_count: u32,
    seeded: bool,
    have_last: bool,
}

/// Rest is judged on STABILITY — how much the reading moves between consecutive
/// samples — not on how close it is to zero.
///
/// Judging closeness to zero cannot work: an un-seeded estimate is zero, so a
/// controller whose bias exceeds the threshold never looks still and its bias
/// can never be learned. That is precisely the controller that needs correcting
/// most, and it fails silently. Stability is independent of the offset, so a
/// large bias converges just as readily as a small one.
const STILL_DELTA_LSB: f32 = 30.0;
/// Consecutive stable samples before the bias starts tracking. At ~60 Hz this is
/// about a quarter second, long enough that a momentary pause mid-motion does
/// not get mistaken for rest.
const REST_SAMPLES: u32 = 16;
/// EMA weight once converged. Deliberately slow: bias drifts with temperature
/// over minutes, so it needs to follow that without chasing per-sample noise.
const BIAS_EMA: f32 = 0.02;
/// Refuse to treat anything beyond this as bias. Stability alone cannot tell a
/// resting controller from one turning at a perfectly constant rate; this cap
/// means a sustained fast pan can never be silently absorbed into the estimate
/// and cancelled out. Roughly 36 dps — far above any real zero-rate offset,
/// well below a deliberate flick.
const MAX_BIAS_LSB: f32 = 600.0;

impl Default for GyroBias {
    fn default() -> Self {
        Self {
            bias: [0.0; 3],
            last: [0.0; 3],
            still_count: 0,
            seeded: false,
            have_last: false,
        }
    }
}

impl GyroBias {
    /// Feed a raw gyro sample; returns it with the current bias removed.
    pub fn correct(&mut self, raw: [i16; 3]) -> [f32; 3] {
        let s = [raw[0] as f32, raw[1] as f32, raw[2] as f32];

        let stable = self.have_last
            && (0..3).all(|i| (s[i] - self.last[i]).abs() < STILL_DELTA_LSB);
        let plausible = s.iter().all(|v| v.abs() < MAX_BIAS_LSB);
        self.last = s;
        self.have_last = true;

        if stable && plausible {
            self.still_count = self.still_count.saturating_add(1);
            if self.still_count >= REST_SAMPLES {
                if self.seeded {
                    for i in 0..3 {
                        self.bias[i] += (s[i] - self.bias[i]) * BIAS_EMA;
                    }
                } else {
                    // First convergence snaps straight to the observed reading
                    // rather than easing in, so drift is gone within a second of
                    // connecting instead of a minute.
                    self.bias = s;
                    self.seeded = true;
                }
            }
        } else {
            self.still_count = 0;
        }

        [s[0] - self.bias[0], s[1] - self.bias[1], s[2] - self.bias[2]]
    }

    /// Current estimate, in raw LSB. Exposed for diagnostics.
    pub fn bias(&self) -> [f32; 3] {
        self.bias
    }

    /// Whether a rest estimate has been established yet.
    pub fn is_seeded(&self) -> bool {
        self.seeded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips the packing rather than asserting one magic triple, so a
    /// nibble-order mistake can't hide behind a hand-picked example.
    #[test]
    fn stick_unpack_recovers_both_channels() {
        for &(x, y) in &[(0u16, 0u16), (4095, 0), (0, 4095), (2048, 2048), (1234, 3210)] {
            let packed = [
                (x & 0xFF) as u8,
                (((y & 0x0F) << 4) | (x >> 8)) as u8,
                (y >> 4) as u8,
            ];
            assert_eq!(unpack_stick(&packed), (x, y), "packed {packed:02x?}");
        }
    }

    fn report(side: Side, b0: u8, b1: u8) -> PadSnapshot {
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_BUTTONS] = b0;
        buf[OFF_BUTTONS + 1] = b1;
        parse_input(side, &buf).expect("full-length report parses")
    }

    #[test]
    fn left_buttons_map_to_the_left_half() {
        let b = report(Side::Left, 0x40, 0x01); // Minus + Capture
        assert!(b.buttons.minus && b.buttons.capture);
        assert!(!b.buttons.plus && !b.buttons.home);

        let d = report(Side::Left, 0x0F, 0x00); // Up | Left | Right | Down
        assert!(d.buttons.dpad_up && d.buttons.dpad_left);
        assert!(d.buttons.dpad_right && d.buttons.dpad_down);
    }

    #[test]
    fn right_buttons_map_to_the_right_half() {
        let b = report(Side::Right, 0x40, 0x01); // Plus + Home
        assert!(b.buttons.plus && b.buttons.home);
        assert!(!b.buttons.minus && !b.buttons.capture);

        let f = report(Side::Right, 0x0F, 0x10); // X | Y | A | B, plus C
        assert!(f.buttons.x && f.buttons.y && f.buttons.a && f.buttons.b);
        assert!(f.buttons.c);
    }

    /// Bit 0x40 is Minus on the left and Plus on the right — the same bit, two
    /// different buttons. This is the mapping most likely to get copy-pasted
    /// wrong between the two parsers.
    #[test]
    fn shared_bits_resolve_per_side() {
        assert!(report(Side::Left, 0x40, 0).buttons.minus);
        assert!(report(Side::Right, 0x40, 0).buttons.plus);

        // 0x20/0x10 are ZL/L on the left and ZR/R on the right, but both land
        // on the positional `z`/`shoulder` fields.
        for side in [Side::Left, Side::Right] {
            let s = report(side, 0x30, 0);
            assert!(s.buttons.z && s.buttons.shoulder);
        }
    }

    #[test]
    fn power_info_decodes_level_and_charge_flags() {
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_POWER] = 0b0010_0111; // level 9, charging, external power
        let s = parse_input(Side::Left, &buf).unwrap();
        assert!(s.power.external_power);
        assert!(s.power.charging);
        assert_eq!(s.power.level, 9);
        assert_eq!(s.power.fraction(), 1.0);
    }

    #[test]
    fn mouse_deltas_are_signed() {
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_MOUSE..OFF_MOUSE + 5].copy_from_slice(&[0xFF, 0xFF, 0x0A, 0x00, 0x42]);
        let s = parse_input(Side::Right, &buf).unwrap();
        assert_eq!(s.mouse.delta_x, -1);
        assert_eq!(s.mouse.delta_y, 10);
        assert_eq!(s.mouse.liftoff, 0x42);
    }

    #[test]
    fn motion_is_only_parsed_when_the_controller_reports_a_block() {
        // Side::Right, because the module constants are the RIGHT half's
        // offsets; the left half's report sits one byte earlier.
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_MOTION_LEN] = 0; // IMU feature off
        buf[OFF_MOTION_ACCEL] = 0xFF; // stale bytes that must NOT be read
        let s = parse_input(Side::Right, &buf).unwrap();
        assert_eq!(s.motion, Motion::default());

        buf[OFF_MOTION_LEN] = 30;
        buf[OFF_MOTION..OFF_MOTION + 4].copy_from_slice(&1234u32.to_le_bytes());
        buf[OFF_MOTION_ACCEL..OFF_MOTION_ACCEL + 2].copy_from_slice(&(-500i16).to_le_bytes());
        let s = parse_input(Side::Right, &buf).unwrap();
        assert_eq!(s.motion_len, 30);
        assert_eq!(s.motion.timestamp, 1234);
        assert_eq!(s.motion.accel[0], -500);
    }

    /// The left half's whole report is one byte earlier than the right's.
    ///
    /// Pinned because getting it wrong is SILENT and total: `motion_len` reads
    /// the neighbouring byte, fails the length guard, and the left half's
    /// motion is simply never parsed — no error, no warning, just a permanently
    /// still accelerometer. That is exactly what shipped until a guided sweep
    /// measured the accel at 33/37/41 on the left and 34/38/42 on the right.
    #[test]
    fn the_left_half_report_is_shifted_one_byte_earlier() {
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_MOTION_LEN - 1] = 30;
        buf[OFF_MOTION_ACCEL - 1..OFF_MOTION_ACCEL + 1]
            .copy_from_slice(&(-86i16).to_le_bytes());
        buf[OFF_MOTION_ACCEL + 7..OFF_MOTION_ACCEL + 9]
            .copy_from_slice(&4136i16.to_le_bytes());

        let l = parse_input(Side::Left, &buf).unwrap();
        assert_eq!(l.motion_len, 30, "left reads motion_len one byte earlier");
        assert_eq!(l.motion.accel[0], -86);
        assert_eq!(l.motion.accel[2], 4136, "1 g on the vertical axis");

        // The same bytes read as a RIGHT half must NOT produce that block.
        let r = parse_input(Side::Right, &buf).unwrap();
        assert_ne!(r.motion.accel[0], -86);
    }

    /// Negative accelerometer readings must survive.
    ///
    /// Each axis is an i16 followed by two ZERO bytes, so reading the field as
    /// an i32 turns every negative value into a large positive one — `aa ff 00
    /// 00` is −86 as i16 but +65450 as i32. That single mistake is what made
    /// two of the three axes look like noise on hardware.
    #[test]
    fn negative_accel_axes_are_not_read_as_huge_positives() {
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_MOTION_LEN] = 30;
        // Exactly the bytes a controller sends: i16, then two zero pad bytes.
        buf[OFF_MOTION_ACCEL..OFF_MOTION_ACCEL + 4].copy_from_slice(&[0xaa, 0xff, 0x00, 0x00]);
        let s = parse_input(Side::Right, &buf).unwrap();
        assert_eq!(s.motion.accel[0], -86);
    }

    /// Real capture (report #3 from a Joy-Con 2 (R) lying still on a desk).
    ///
    /// This is the evidence the accel offset rests on: whatever the block's
    /// full layout turns out to be, the three values at 0x22/0x26/0x2A form a
    /// gravity vector. A regression that moves the offset back to the
    /// spec-documented 0x16 fails here.
    #[test]
    fn captured_at_rest_report_yields_a_one_g_gravity_vector() {
        let raw: [u8; INPUT_REPORT_LEN] = [
            0x03, 0x18, 0x00, 0x00, 0x07, 0x00, 0xf8, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x1e, 0x61, 0xc0, 0x00, 0x0c, 0x00, 0x78, 0x67, 0xff, 0x01, 0xfd, 0xe8, 0xff,
            0x00, 0xfa, 0xf0, 0x7f, 0x00, 0x00, 0x26, 0x09, 0x00, 0x00, 0xac, 0x00, 0x00, 0x00,
            0x3c, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let s = parse_input(Side::Right, &raw).expect("captured report parses");

        assert_eq!(s.counter, 3);
        assert_eq!(s.motion_len, 30);
        assert_eq!(s.motion.accel, [2342, 172, 3388]);

        let g = (s.motion.accel.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()).sqrt();
        assert!(
            (g - ACCEL_LSB_PER_G as f64).abs() < 100.0,
            "gravity magnitude {g} is not ~{ACCEL_LSB_PER_G} LSB — accel offset is wrong",
        );

        // The stick reads centred and the pad reports no buttons, confirming
        // the rest of the offsets against the same capture.
        assert_eq!(s.stick_raw, (2048, 2047));
        assert_eq!(s.buttons, Buttons::default());
        assert_eq!(s.power.level, 6);
    }

    /// A short payload must degrade rather than panic — the optional feature
    /// blocks are exactly the bytes a truncated report is missing.
    #[test]
    fn truncated_reports_parse_what_they_can() {
        let buf = vec![0u8; OFF_STICK + 3];
        let s = parse_input(Side::Left, &buf).expect("buttons + stick is enough");
        assert_eq!(s.mouse, Mouse::default());
        assert_eq!(s.motion_len, 0);

        assert!(parse_input(Side::Left, &[0u8; 4]).is_none());
    }

    /// The headline case: a controller sitting still with a constant offset
    /// must end up reading zero, or that offset integrates into cursor drift.
    #[test]
    fn resting_gyro_bias_converges_to_zero_output() {
        let mut bias = GyroBias::default();
        let resting = [40i16, -25, 12];

        // Before convergence the offset still comes through.
        let first = bias.correct(resting);
        assert_eq!(first, [40.0, -25.0, 12.0]);

        for _ in 0..REST_SAMPLES + 4 {
            bias.correct(resting);
        }
        assert!(bias.is_seeded());
        let out = bias.correct(resting);
        for (i, v) in out.iter().enumerate() {
            assert!(v.abs() < 1.0, "axis {i} still drifting: {v}");
        }
    }

    /// A large bias must still be learnable. An earlier version judged rest by
    /// closeness to zero, which meant a controller with a big offset never
    /// looked still and never converged — silently, and worst for the hardware
    /// that needed correcting most.
    #[test]
    fn a_large_bias_still_converges() {
        let mut bias = GyroBias::default();
        let resting = [(MAX_BIAS_LSB as i16) - 100, 0, 0];
        for _ in 0..REST_SAMPLES + 4 {
            bias.correct(resting);
        }
        assert!(bias.is_seeded(), "never judged the controller still");
        assert!(bias.correct(resting)[0].abs() < 1.0);
    }

    /// A sustained pan at a perfectly constant rate is stable sample-to-sample,
    /// so stability alone would call it rest and cancel the motion out. The
    /// magnitude cap is what stops that.
    #[test]
    fn a_fast_constant_pan_is_never_absorbed_as_bias() {
        let mut bias = GyroBias::default();
        let panning = [(MAX_BIAS_LSB as i16) + 2000, 0, 0];
        for _ in 0..200 {
            bias.correct(panning);
        }
        assert!(!bias.is_seeded(), "a fast pan was mistaken for rest");
        let out = bias.correct(panning);
        assert!(out[0] > MAX_BIAS_LSB, "pan was cancelled out: {}", out[0]);
    }

    /// Noise around a resting offset must not reset convergence — real sensors
    /// never report the exact same counts twice.
    #[test]
    fn small_noise_around_rest_still_counts_as_still() {
        let mut bias = GyroBias::default();
        for i in 0..REST_SAMPLES + 10 {
            let jitter = if i % 2 == 0 { 3 } else { -3 };
            bias.correct([40 + jitter, -25, 12]);
        }
        assert!(bias.is_seeded());
        let out = bias.correct([40, -25, 12]);
        assert!(out.iter().all(|v| v.abs() < 10.0), "{out:?}");
    }

    /// Real motion must pass through untouched, and must not be absorbed into
    /// the bias estimate — otherwise a slow steady turn would fade to zero.
    #[test]
    fn motion_passes_through_and_does_not_corrupt_the_estimate() {
        let mut bias = GyroBias::default();
        for _ in 0..REST_SAMPLES + 4 {
            bias.correct([10, 0, 0]);
        }
        let before = bias.bias();

        let moving = [8000i16, 0, 0];
        for _ in 0..50 {
            let out = bias.correct(moving);
            assert!(out[0] > 7000.0, "motion was swallowed: {}", out[0]);
        }
        let after = bias.bias();
        assert!(
            (after[0] - before[0]).abs() < 1.0,
            "sustained motion moved the bias estimate {before:?} → {after:?}",
        );
    }

    #[test]
    fn stick_centres_on_the_first_sample_and_widens_with_travel() {
        let mut cal = StickCalib::default();
        // Resting off-centre: the first sample defines zero, so a pad with an
        // offset centre does not drift.
        let (x, y) = cal.normalize((1900, 2100));
        assert_eq!((x, y), (0.0, 0.0));

        // Beyond the nominal range the extents widen and the axis saturates
        // at exactly 1.0 rather than overshooting.
        let (x, _) = cal.normalize((4095, 2100));
        assert_eq!(x, 1.0);
        let (x, _) = cal.normalize((0, 2100));
        assert_eq!(x, -1.0);
        // Having seen both extremes, mid-travel is proportional.
        let (x, _) = cal.normalize((1900 + (4095 - 1900) / 2, 2100));
        assert!((x - 0.5).abs() < 0.01, "got {x}");
    }
}
