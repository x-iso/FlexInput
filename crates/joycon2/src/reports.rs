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

/// Start of the orientation-angle block, 13 bytes ahead of the accelerometer.
///
/// Three 24-bit little-endian angles on a 4-byte stride, each preceded by a tag
/// byte — see [`Motion::angle`]. Anchored to [`OFF_MOTION_ACCEL`] rather than
/// written as a literal because the two were measured together, from the same
/// captures, and a future correction to one applies to the other.
const OFF_MOTION_ANGLE: usize = OFF_MOTION_ACCEL - 13;

/// ⭐ The STANDARD accelerometer/gyroscope block: six contiguous `i16`.
///
/// `[accel xyz][gyro xyz]`, little-endian, stride 2, at `0x30..0x3C`. This is
/// the documented Joy-Con 2 layout, and it is the one a working implementation
/// reads. It appears only when the feature mask carries
/// [`crate::protocol::feature::IMU_RAW`]; without that bit these twelve bytes
/// are zero, which is what every capture in this project contained.
///
/// Verified against the reference's own example notification before being
/// trusted: `0x34` reads 4078 — one g on the vertical axis, against the 4096
/// LSB/g this project measured independently — and the three gyro words read
/// −2, 4 and 2 with the controller at rest. Both are exactly what a correct
/// offset looks like and neither could hold by accident.
const OFF_STD_ACCEL: usize = 0x30;
const OFF_STD_GYRO: usize = 0x36;

/// Per-axis gain on the recovered field rates, applied last.
///
/// ⭐ **The yaw field reads about a quarter of what the other two do**, and this
/// is a CALIBRATION for that, not a decode of it. Two independent measurements
/// agree:
///
/// * On hardware, the yaw rate feels roughly four times weaker than roll or
///   pitch during equivalent motion.
/// * On saved captures, field #2's total path length is 0.23–0.24 of the
///   dominant field's — 4.15, 4.29 and 4.14 across three phases, on two
///   captures and both halves. Those phases rotate about DIFFERENT axes, so a
///   ratio that stays put is not geometry.
///
/// Why it is a calibration and not a fix: three axes of one gyro share a
/// full-scale setting, so they cannot genuinely differ in counts per degree.
/// The fields are therefore not three body axes, and the constant absorbs
/// whatever the encoding really is well enough to aim with. It is honest about
/// being empirical rather than derived.
///
/// ❗ This REPLACED a ZYX Euler-rate-to-body-rate transform, which was removed
/// rather than kept alongside. That transform assumed the fields compose into
/// an orientation, and they do not: Euler in all six orders, the rotation
/// vector and the vector part of a quaternion were each scored against measured
/// gravity over 288 hypotheses, and the best managed 40.0° against 49.2° for
/// applying no rotation at all. Leaving it in was actively harmful — it mixed
/// yaw into pitch as a function of pose, which is exactly the coupling it was
/// added to remove.
///
/// Retune with `FLEXINPUT_JC2_FIELD_GAIN="1,1,4"` — see [`field_gain`].
pub const FIELD_GAIN: [f32; 3] = [1.0, 1.0, 4.0];

/// Which field drives which canonical gyro axis, and with what sign.
///
/// Canonical order is `(roll, pitch, yaw)` — the contract on
/// `flexinput_devices::gyro::HidReading`. Entries are `(field index, sign)`.
///
/// ⭐ **From hands-on observation, not from a fit.** Waving the controller on
/// one axis at a time against the raw field rates gave: field #0 responds to
/// PITCH, field #1 to ROLL, field #2 to YAW. Saved captures agree that #2 is
/// the yaw-dominant field and that #0 and #1 are the other two, but they cannot
/// separate roll from pitch cleanly because the guided sweeps rotate about the
/// controller's axes rather than the user's grip — and the grip is what decides
/// which physical motion a person calls pitch.
///
/// That is also why this is overridable: the correct answer depends on how the
/// controller is held, and the person holding it can settle in ten seconds what
/// a capture cannot settle at all.
///
/// ❗ **Yaw is NEGATED.** Reported from hardware: with the sign positive the
/// cursor moved the wrong way, and it had to be inverted downstream in the
/// 3DOF-to-2D module to be usable. Fixing it there hides it from every other
/// consumer — the `orientation` pin, any other aim module, a future patch — so
/// it belongs in the one place that defines the canonical frame. The canonical
/// contract is yaw POSITIVE CLOCKWISE seen from above, and this field counts
/// the other way, which is the same handedness quirk `heading_rad` already
/// corrects for the absolute angle.
///
/// `FLEXINPUT_JC2_GYRO_MAP="+1,+0,-2"` — three signed field indices, in
/// canonical order.
pub const GYRO_MAP: [(usize, f32); 3] = [(1, 1.0), (0, 1.0), (2, -1.0)];

/// [`GYRO_MAP`], overridable at runtime by `FLEXINPUT_JC2_GYRO_MAP`.
///
/// Read once and cached; called per report.
pub fn gyro_map() -> [(usize, f32); 3] {
    static MAP: std::sync::OnceLock<[(usize, f32); 3]> = std::sync::OnceLock::new();
    *MAP.get_or_init(|| {
        let Ok(raw) = std::env::var("FLEXINPUT_JC2_GYRO_MAP") else {
            return GYRO_MAP;
        };
        let mut out = [(0usize, 1.0f32); 3];
        let parts: Vec<&str> = raw.split(',').map(|p| p.trim()).collect();
        if parts.len() == 3 {
            let mut ok = true;
            for (i, p) in parts.iter().enumerate() {
                let sign = if p.starts_with('-') { -1.0 } else { 1.0 };
                match p.trim_start_matches(['+', '-']).parse::<usize>() {
                    Ok(idx) if idx < 3 => out[i] = (idx, sign),
                    _ => ok = false,
                }
            }
            // Every field used exactly once, or the map silently drops an axis
            // and doubles another — which reads as "one axis is dead" and is
            // very hard to trace back to a typo in an environment variable.
            let mut used = [false; 3];
            for (idx, _) in out {
                if used[idx] {
                    ok = false;
                }
                used[idx] = true;
            }
            if ok {
                eprintln!("[jc2] gyro map overridden: {out:?}");
                return out;
            }
        }
        eprintln!(
            "[jc2] FLEXINPUT_JC2_GYRO_MAP={raw:?} is not three distinct signed \
             indices 0-2 — ignoring"
        );
        GYRO_MAP
    })
}

/// Apply [`gyro_map`] to a field-order rate triple, yielding canonical
/// `(roll, pitch, yaw)`.
pub fn canonical_field_rate(field_rate: [f32; 3]) -> [f32; 3] {
    let m = gyro_map();
    [
        field_rate[m[0].0] * m[0].1,
        field_rate[m[1].0] * m[1].1,
        field_rate[m[2].0] * m[2].1,
    ]
}

/// [`FIELD_GAIN`], overridable at runtime by `FLEXINPUT_JC2_FIELD_GAIN`.
///
/// Three comma-separated numbers, in field order. Exists because the default is
/// a number arrived at by feel and by a path-length ratio, not by derivation —
/// so the person holding the controller is a better judge of it than a constant
/// compiled a month earlier, and should not need a Rust toolchain to disagree.
///
/// Read once and cached: this is called per report.
pub fn field_gain() -> [f32; 3] {
    static GAIN: std::sync::OnceLock<[f32; 3]> = std::sync::OnceLock::new();
    *GAIN.get_or_init(|| {
        let Ok(raw) = std::env::var("FLEXINPUT_JC2_FIELD_GAIN") else {
            return FIELD_GAIN;
        };
        let parts: Vec<f32> = raw.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        // All three or none: a partial parse would silently mix a user value
        // with a default and be very hard to explain afterwards.
        if parts.len() == 3 && parts.iter().all(|v| v.is_finite() && *v != 0.0) {
            let g = [parts[0], parts[1], parts[2]];
            eprintln!("[jc2] field gain overridden: {g:?}");
            g
        } else {
            eprintln!("[jc2] FLEXINPUT_JC2_FIELD_GAIN={raw:?} is not three non-zero numbers — ignoring");
            FIELD_GAIN
        }
    })
}

/// Gyro counts per degree per second: 48000 counts = 360 °/s.
///
/// Works out to a full scale of ±245 °/s over `i16`, which is a standard
/// LSM6DS-family range rather than an arbitrary figure — a good sign the
/// documented scale is the real one.
pub const GYRO_LSB_PER_DPS: f32 = 48000.0 / 360.0;

/// Start of the 12 bytes whose meaning is still open — see [`Motion::probe`].
///
/// The motion block is `[timestamp u32][THESE 12][2 bytes, always zero][accel
/// 12]`, so this is simply "everything between the timestamp and the padding".
/// Written as an offset from the timestamp rather than from the angle base
/// because it is the same 12 bytes under either reading, and anchoring it to
/// one reading would make the other look derived from it.
const OFF_MOTION_PROBE: usize = OFF_MOTION_TIMESTAMP + 4;

/// Length of that block. Exactly `3 x i16 + 3 x i16`, which is what a raw
/// gyro-plus-magnetometer pair would occupy.
const MOTION_PROBE_LEN: usize = 12;

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
/// the first), laid out as
/// `[timestamp u32][12 unresolved bytes][2 zero bytes][accel 12]`.
///
/// The accelerometer at [`OFF_MOTION_ACCEL`] is solid. The 12 bytes before the
/// padding are read two ways here — as [`Motion::angle`] and as
/// [`Motion::probe`] — because which reading is correct is still open.
///
/// ❗ This doc used to assert "there is no raw gyro or magnetometer field
/// anywhere in the report; both are fused into the angles". That conclusion
/// rested on the reference PC implementation getting motion out of a channel
/// this controller does not implement, and an HCI capture of that
/// implementation talking to this hardware showed it receives **zero**
/// notifications — it never got motion here either. So it was never evidence
/// for anything, and the raw bytes are exposed again rather than hidden behind
/// a decode nobody has confirmed.
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
    /// The 12 bytes before the accelerometer, read as three 24-bit LE fields on
    /// a 4-byte stride (each preceded by a tag byte).
    ///
    /// ⭐ **Index [`HEADING_AXIS`] (2) is an absolute, magnetometer-corrected
    /// heading, and it is the only one of the three that is trustworthy.**
    /// Six independent measurements across three captures and both halves put
    /// it at 33.53–33.77 million counts per turn — all within +0.6% of
    /// [`ANGLE_COUNTS_PER_TURN`]. Over a full 360° sweep it drifts −4.5° (left)
    /// and −1.7° (right), and it holds its absolute value for 45 s. Six-axis
    /// integration cannot do that, so the 9-axis fusion is real and is exposed
    /// here rather than as raw magnetometer counts — which is why enabling the
    /// magnetometer feature bit never changed anything observable.
    ///
    /// ⛔ **Indices 0 and 1 are NOT angles. Do not use them.** They were once
    /// believed to be roll and pitch on the strength of an |r| ≈ 0.73
    /// correlation; that was too weak and it was wrong. Exhaustively disproven
    /// since, on saved captures:
    ///
    /// * not angular velocity — a full 3x3 fit over strides 2/3/4/6, i16 and
    ///   i24 in both byte orders, raw and differenced, explains at most 10%,
    ///   and the fitted scale misses the controller's own stated gyro scale by
    ///   5-8x
    /// * not three composing Euler angles — reprojecting gravity through them
    ///   loses to applying NO rotation at all (18.9° vs 19.3° residual), even
    ///   allowing a separate scale per axis
    /// * not raw gyro counts — integrating any field at the scale the device
    ///   states (0.07 °/s per LSB) over a known 360° turn never reaches 360°,
    ///   at any offset, in any codec
    ///
    /// Kept in the struct because they are real report content and useful for
    /// diagnostics, not because their meaning is known.
    ///
    /// Roll and pitch now come from gravity instead — see [`tilt_from_accel`].
    pub angle: [i32; 3],
    /// **The same 12 bytes as [`Motion::angle`], read instead as six i16.**
    ///
    /// Kept as a DIAGNOSTIC, not as a live decode — the hypothesis it was added
    /// to test has since been answered, in the negative, by hand.
    ///
    /// It was put on pins because every automated attempt on this block had
    /// been a scripted decode judged by a fit statistic, and those tests could
    /// only ever reject the layouts someone thought to write. Watching six
    /// numbers while turning the controller settled in one session what months
    /// of scans had not:
    ///
    /// * `probe[0]`, `[2]`, `[4]` are NOISE — no response to any motion. They
    ///   straddle a field boundary, pairing a status byte with the bottom 8
    ///   bits of a slowly-moving value, so noise is exactly right.
    /// * `probe[1]`, `[3]`, `[5]` are the top 16 bits of the three 24-bit
    ///   fields, and each tracks one rotation axis absolutely.
    ///
    /// So the block is three 4-byte groups, not six i16, and this reading is
    /// wrong. Confirmed independently on saved captures: binning by the tilt
    /// the accelerometer already measures, `probe[0]/[2]/[4]` explain 1-4% of
    /// their own variance while the accel axes explain 99%.
    ///
    /// ⭐ The same captures answer the bigger question. Binning against angular
    /// RATE — differentiated from accel-derived tilt, so no assumption about
    /// any encoding — every field in the block scores ≤0.31, and the ones that
    /// score at all are the same ones that score higher against POSE, which is
    /// pose leaking through during a smooth sweep. The measurement resolves
    /// true rate at 0.97 and even catches the accelerometer's own tangential
    /// leakage at 0.59, so it has the sensitivity. **There is no angular rate
    /// anywhere in this report, in any encoding.** Not undecoded — absent.
    ///
    /// Index order is byte order — `probe[0]` is the first i16 of the block.
    /// No permutation, no sign flip, no bias removal: the point is to see what
    /// the hardware sends, so anything applied here could only hide it.
    pub probe: [i16; 6],
    /// ⭐ Raw gyro from the STANDARD block, in counts. `None` when the block is
    /// absent — see [`OFF_STD_GYRO`].
    ///
    /// This is a genuine angular rate straight from the sensor: no integration,
    /// no differencing, no wrap seam, no pose-dependent coupling, and nothing
    /// to unwrap at ±90°. Every one of those problems came from trying to
    /// recover a rate from the other block, which never contained one.
    ///
    /// Scale is [`GYRO_LSB_PER_DPS`].
    pub gyro: Option<[i16; 3]>,
    /// Accelerometer from the standard block, in counts, `None` when absent.
    ///
    /// Same units as [`Motion::accel`] ([`ACCEL_LSB_PER_G`]) but read from the
    /// documented offsets rather than the ones measured by hand. Kept separate
    /// so the two can be compared on hardware instead of one silently replacing
    /// the other.
    pub std_accel: Option<[i16; 3]>,
}

/// Modulus of the [`Motion::angle`] field — its WIDTH, 24 bits.
///
/// Used only for wrap detection.
pub const ANGLE_FIELD_MODULUS: i64 = 1 << 24;

/// Counts per full revolution — the angular SCALE, measured on hardware.
///
/// ❗ **Not the same number as [`ANGLE_FIELD_MODULUS`], and conflating them was
/// a real bug.** One constant did both jobs on the assumption that a full field
/// range equalled a full turn. It does not: the field's range spans 180°, so a
/// revolution takes TWO full ranges. The result was everything rotating twice
/// as far as it should, and the wrap threshold sitting at the wrong place.
///
/// Measured from known 360° turns, agreeing across both halves and two
/// independent axes:
/// * yaw field — LEFT 33 752 971, RIGHT 33 674 419 (+0.6% / +0.4% of 2^25)
/// * roll field — LEFT 32 458 485 at r = 0.993 (−3.3% of 2^25)
///
/// The spread is hand-rotation accuracy, not disagreement.
pub const ANGLE_COUNTS_PER_TURN: i64 = 1 << 25;

/// Fallback report rate, used only until the device's own clock is calibrated.
///
/// ❗ **No longer a scaling constant for anything physical.** It used to convert
/// rates and thresholds between counts-per-report and deg/s, on links that ran
/// at ~15 ms. Pinning the connection interval to 7.5 ms put the real rate at
/// 133-200 Hz, so everything routed through it came out two to three times off
/// — silently, because nothing in a count is labelled with its cadence.
///
/// Timing is now taken from the controller's own timestamp (see [`TickClock`]),
/// and every threshold is written in degrees per second. This survives purely as
/// the seed value for the first fraction of a second after connect.
pub const REPORT_HZ: f32 = 67.0;

/// Angle counts per degree of rotation.
pub const ANGLE_COUNTS_PER_DEG: f32 = ANGLE_COUNTS_PER_TURN as f32 / 360.0;

/// Turns absolute [`Motion::angle`] readings into angular rate.
///
/// Stateful by necessity: a rate is a difference between reports.
///
/// ❗ The subtraction must be taken modulo the FIELD modulus (2^24). The angle
/// wraps, so a plain difference across the wrap point yields ±16.7 million
/// instead of a small step — a violent spike in the aim mapping, twice per
/// revolution.
#[derive(Debug, Clone, Copy, Default)]
pub struct AngleGyro {
    prev: Option<[i32; 3]>,
    /// Last rate emitted, held across a rejected sample — see [`MAX_STEP`].
    last: [f32; 3],
}

/// Largest believable change in one report, in angle counts.
///
/// ⭐ **This is what made motion feel broken, and it is not a wrap.** The fields
/// carry a handful of genuine discontinuities — measured at 0.04% to 1% of
/// frames, jumping around 8.18 million counts. The wrap correction above cannot
/// catch them, because it triggers at half the field modulus (8 388 608) and
/// they land just BELOW that. So they survive as an eight-million-count step
/// where a normal one is a few hundred, and differentiating that puts a spike
/// four decades past any real motion straight into the aim mapping.
///
/// 2^22 counts is 45° inside a single report. The sensor's own full scale is
/// 2000 °/s, which at the slowest report rate this link runs (67 Hz) is 29.9°
/// per report — so the threshold sits comfortably above anything the hardware
/// can legitimately produce, and comfortably below the glitches, which are
/// twice as large again.
///
/// Measured cost of the rejection on saved captures: **0 to 2 samples per
/// phase, out of 800 to 2400**. That number is the whole justification. A
/// filter that discarded a meaningful share of real motion to hide a few
/// glitches would be trading one bad feel for another, so it is stated here
/// rather than assumed, and pinned by a test that a full-scale rotation
/// survives untouched.
const MAX_STEP: i64 = 1 << 22;

impl AngleGyro {
    /// Angular rate in angle counts per report, or zeros for the first sample.
    ///
    /// Returning zeros initially is deliberate: the alternative is treating the
    /// first angle as a delta from zero, which injects one enormous spike at
    /// connect time — exactly when a gyro cursor is most visible.
    ///
    /// ❗ **`f32`, not `i16`.** At 2^24 counts per revolution and ~67 reports
    /// per second, `i16::MAX` counts per report is only 47 deg/s — slower than
    /// an ordinary aim flick, so an `i16` here clips during any normal use and
    /// leaves the gyro technically working and practically useless. 180 deg/s
    /// alone needs about 125 000 counts per report.
    pub fn rate(&mut self, angle: [i32; 3]) -> [f32; 3] {
        let prev = match self.prev.replace(angle) {
            Some(p) => p,
            None => return [0.0; 3],
        };
        // ⭐ A REPEATED sample is not a moment of stillness.
        //
        // All three fields identical to the previous report means the
        // controller sent the same motion sample twice, not that it stopped
        // dead for one frame and resumed. Differentiating a duplicate gives
        // zero, then a double step on the next real one — a staircase, which is
        // exactly what a rate looks like when it is jagged despite the motion
        // being smooth.
        //
        // Reported on hardware: the jaggedness appears while the back button is
        // HELD. That button is a Mobapad addition with no Joy-Con 2 equivalent,
        // remapped in their app to a stick click, so the firmware is
        // synthesising an input it does not natively have — and repeating the
        // last IMU sample while it does so is a very ordinary way for that to
        // show up. It is not the user pressing hard enough to shake the sensor.
        //
        // Holding the previous rate is right on the physics too: the controller
        // was turning an instant ago and one duplicated report does not stop it.
        // Genuine stillness still reads as zero, because a resting IMU's low
        // bits dither and never repeat exactly.
        if angle == prev {
            return self.last;
        }
        let mut out = [0.0f32; 3];
        for i in 0..3 {
            // Wrapping is a property of the FIELD, so it uses the field
            // modulus — not the counts-per-turn scale. Using the scale here is
            // what made fast rotations decode as garbage: the threshold sat at
            // a full field range, so a genuine wrap was never corrected.
            let mut d = angle[i] as i64 - prev[i] as i64;
            if d > ANGLE_FIELD_MODULUS / 2 {
                d -= ANGLE_FIELD_MODULUS;
            } else if d < -ANGLE_FIELD_MODULUS / 2 {
                d += ANGLE_FIELD_MODULUS;
            }
            // A step no hand can produce is a glitch in the field, not motion.
            // HOLD the previous rate rather than emitting zero: the controller
            // was turning an instant ago and one dropped report does not stop
            // it, so holding keeps a fast pan smooth where a zero would punch a
            // one-frame notch into it.
            out[i] = if d.abs() > MAX_STEP {
                self.last[i]
            } else {
                d as f32
            };
        }
        self.last = out;
        out
    }
}

/// Accelerometer counts per g, measured from hardware (see [`Motion::accel`]).
///
/// ✅ Confirmed TWICE, independently: measured from the constant magnitude of
/// the accel vector across a full sweep, and later stated by the controller
/// itself in its `0x11/0x03` reply (0.002393 m/s² per LSB = 4098 LSB/g). Two
/// unrelated routes to the same number.
pub const ACCEL_LSB_PER_G: f32 = 4096.0;

/// Permute and sign raw Joy-Con 2 accel into FlexInput's canonical IMU frame.
///
/// ⭐ **The canonical frame is a CONTRACT, not a per-device choice**, spelled
/// out on `flexinput_devices::gyro::HidReading`: `x` = forward (positive when
/// the nose tilts up), `y` = side (positive when the right grip drops), `z` =
/// vertical (positive lying flat, face up). Every parser must land in that one
/// body frame so AutoMap, the 3DOF module and the canvas never have to know
/// which pad produced a value.
///
/// ❗ This controller does NOT arrive in that frame, and for a while it was
/// passed straight through as though it did — which put its accel axes in a
/// different order and sign from the DualSense and Switch Pro reading the same
/// physical motion. The Switch Pro happens to need no permutation, and that
/// made "no permutation" look like the neutral choice; it is not, it is a claim
/// about the sensor mounting.
///
/// Observed against those two pads on the same motion:
/// * raw axis 0 is the SIDE axis, and its sign is inverted
/// * raw axis 1 is the FORWARD axis
/// * raw axis 2 is vertical, matching (gravity sits here with the grip flat)
///
/// Applied as integer counts so the scale factor stays in one place.
pub fn to_canonical_accel(raw: [i32; 3]) -> [i32; 3] {
    [raw[1], -raw[0], raw[2]]
}

/// Which of the three [`Motion::angle`] fields is the real one.
///
/// ⛔ **Only index 2 survived testing.** Indices 0 and 1 are not angles — see
/// the note on [`Motion::angle`].
pub const HEADING_AXIS: usize = 2;

/// Convert heading counts to radians.
///
/// The field wraps, and that is fine here: a rotation is periodic too, so a
/// wrapped angle produces exactly the right orientation without unwrapping.
pub fn heading_rad(counts: i64) -> f32 {
    // ❗ Sign. The canonical contract says `gyro_z` is yaw POSITIVE CLOCKWISE
    // (seen from above); this field counts the other way, so it is negated
    // exactly once, here, for every consumer.
    //
    // It used to be negated in the quaternion builder but NOT in the rate, so
    // the `orientation` pin and the `gyro_z` pin disagreed about which way yaw
    // went — one of them was always wrong whichever way you turned.
    //
    // ⚠️ [`FIELD_GAIN`]'s yaw factor is deliberately NOT applied here, and the
    // two are in genuine tension. The rate gain rests on a path-length ratio
    // across captures plus how it feels in the hand; this scale rests on the
    // heading field's unwrapped travel across a known 360° turn, measured six
    // times on two halves and agreeing to within 0.6%. Applying the gain here
    // breaks those measurements outright.
    //
    // Both cannot be right about the same field, so one of them is measuring
    // something other than what it thinks. Until that is resolved the direct
    // measurement wins, and the discrepancy is left visible rather than papered
    // over by scaling this to match.
    -(counts as f64 / ANGLE_COUNTS_PER_TURN as f64 * std::f64::consts::TAU) as f32
}

/// Accumulate the wrapping heading field into a continuous count.
///
/// ⭐ **The field is 2^24 wide but a full turn is 2^25, so it covers only HALF
/// a revolution and wraps TWICE per turn.** Reading it directly therefore
/// produces a clean 180 degree flip at every wrap — the instant jumps visible
/// on a yaw trace, sitting in otherwise ordinary noise.
///
/// Two physical headings map to the same field value, so this cannot be fixed
/// by folding the value; the wraps have to be counted as they happen, which
/// makes it stateful. The step threshold is half the FIELD modulus, not half a
/// turn — conflating those two constants is an error this code has already made
/// once.
fn unwrap_heading(prev: i32, next: i32, acc: &mut i64) {
    let mut d = next as i64 - prev as i64;
    if d > ANGLE_FIELD_MODULUS / 2 {
        d -= ANGLE_FIELD_MODULUS;
    } else if d < -ANGLE_FIELD_MODULUS / 2 {
        d += ANGLE_FIELD_MODULUS;
    }
    *acc += d;
}

/// Roll and pitch in radians from the gravity direction, in that order.
///
/// ⭐ **This is where roll and pitch come from now**, because the report does
/// not contain them in any decodable form. Gravity is a known vector, the
/// accelerometer measures it to 2-3%, and two of the three orientation angles
/// follow from it directly — no fitting, no reverse engineering.
///
/// Canonical frame is `(forward, side, vertical)`, so at rest gravity lies on
/// the vertical axis and both angles are zero.
///
/// ❗ **Valid only while the controller is not being linearly accelerated.**
/// The accelerometer cannot separate gravity from a shove, so a hard flick
/// briefly tilts these. That is a real limitation of doing it this way and the
/// reason the aim modules smooth it — but it is bounded and self-correcting,
/// unlike an integrated rate, which drifts without bound.
pub fn tilt_from_accel(accel: [i32; 3]) -> (f32, f32) {
    let (x, y, z) = (accel[0] as f32, accel[1] as f32, accel[2] as f32);
    let n = (x * x + y * y + z * z).sqrt();
    if n < 1.0 {
        // No usable gravity vector — free fall, or the IMU is off. Report level
        // rather than inventing an angle from noise.
        return (0.0, 0.0);
    }
    let (x, y, z) = (x / n, y / n, z / n);
    // ❗ Pitch is `+x`, NOT `-x`. The canonical contract says `gyro_y` is pitch
    // POSITIVE NOSE-UP, and `accel_x > 0` is nose-up — so the angle has to rise
    // with x. The negated form put `gyro_y` the wrong way round against every
    // other pad, which is the reported inverted pitch.
    (y.atan2(z), x.atan2((y * y + z * z).sqrt()))
}

/// Orientation for one report, plus the angular rate implied by it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Orientation {
    /// `(roll, pitch, yaw)` in radians. Roll and pitch from gravity, yaw from
    /// the absolute heading field.
    pub euler_rad: [f32; 3],
    /// `(roll, pitch, yaw)` rate in degrees per second.
    pub rate_dps: [f32; 3],
    /// ⭐ **Angular rate from differencing the three [`Motion::angle`] fields**,
    /// in degrees per second, in FIELD ORDER — not yet mapped to canonical axes.
    ///
    /// This is the real gyro, and until now it was being thrown away.
    ///
    /// [`Orientation::rate_dps`] above derives roll and pitch by differentiating
    /// tilt read from the ACCELEROMETER, which is the worst available rate
    /// source at exactly the moment it matters most: during a hard flick the
    /// accel carries centripetal and tangential load on top of gravity, so the
    /// tilt it implies is wrong precisely when the rate is highest. Captured
    /// frames from a fast sweep read 2627/649/3088 against a 4096 magnitude —
    /// the "gravity" direction is fiction there.
    ///
    /// The angle fields have no such problem. Saved captures show them stepping
    /// smoothly by 24 000 to 630 000 counts per report through a fast rotation
    /// and by about 200 at rest — a hundred- to three-thousand-fold signal to
    /// noise — and one nets −358.0° across a hand-made 360° turn, which is both
    /// the scale ([`ANGLE_COUNTS_PER_TURN`]) and the proof that they integrate
    /// rotation.
    ///
    /// That they drift while the controller sits still (0.03–1.44 °/s measured,
    /// per axis, per half) is not a defect in the reading — it is the signature
    /// that identified them. A magnetometer-corrected fusion cannot drift; an
    /// integrated gyro must. The bias is small, constant, and exactly what
    /// [`GyroBias`] removes.
    ///
    /// ❗ **Field order, deliberately.** Which field is roll, pitch or yaw is
    /// not yet established — the obvious way to measure it, correlating against
    /// accel-derived rates, fails for the same reason the accel is a bad rate
    /// source. Rather than ship a guessed permutation, these go out on their own
    /// pins so the mapping can be read off hardware in one session.
    pub field_rate_dps: [f32; 3],
}

/// Tracks orientation across reports and differences it into an angular rate.
///
/// ⭐ **Replaces the `AngleGyro` + `GyroBias` pair for the roll and pitch axes**,
/// which differenced [`Motion::angle`] fields 0 and 1 — fields now known not to
/// be angles at all. Every `gyro_x` / `gyro_y` value this controller has ever
/// produced came from differencing those.
///
/// A pleasant consequence: **there is no zero-rate bias to correct any more.**
/// Both sources are absolute — gravity does not drift and the heading is
/// magnetometer-corrected — so nothing accumulates, and `GyroBias`'s whole job
/// disappears rather than being reimplemented.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrientationTracker {
    prev: Option<[f32; 3]>,
    /// Last raw heading field value, for wrap detection.
    prev_heading: Option<i32>,
    /// Heading accumulated across wraps, in field counts. `i64` because this
    /// grows without bound while the controller keeps turning the same way.
    heading_acc: i64,
    /// When the previous sample arrived.
    ///
    /// The rate used to be `delta * REPORT_HZ` with `REPORT_HZ` a hardcoded 67.
    /// That was measured on links negotiated at ~15 ms; pinning the connection
    /// interval to 7.5 ms yields 140-200 Hz, at which point every gyro value
    /// would come out two to three times too small. Timing the samples removes
    /// the assumption entirely and is correct on any transport, including USB.
    prev_at: Option<std::time::Instant>,
    /// Differences the raw angle fields, with the glitch rejection — the source
    /// of [`Orientation::field_rate_dps`].
    field_gyro: AngleGyro,
    /// Zero-rate offset for the same, because these fields genuinely drift.
    field_bias: GyroBias,
    /// Previous DEVICE timestamp, for a jitter-free `dt` — see [`TickClock`].
    clock: TickClock,
}

/// Converts the report's own timestamp into elapsed seconds.
///
/// ⭐ **This is what made the rate pins jagged in proportion to how hard the
/// controller was being swung.** `rate = delta_angle / dt`, and `delta_angle`
/// comes from the device's own accumulator, so it is right. `dt` was measured
/// on the host with `Instant::now()`, and BLE does not hand a notification to
/// the reader thread once per connection event: the stack batches, the dongle
/// drains a queue, and the OS schedules when it feels like it. The resulting
/// jitter is a MULTIPLICATIVE error — it scales with the signal — which is
/// exactly the "the faster I wave it, the more it zig-zags" symptom, and it is
/// why the noise looked like it belonged to the sensor.
///
/// The controller solves this for us. Its timestamp advances by a FIXED amount
/// per report — measured on saved captures at 12 units per report in 97.7% of
/// samples on one link and 4 units in 99.3% on another, with the exceptions
/// landing exactly where a report was dropped.
///
/// The tick DURATION is unknown and differs between links, so it is measured
/// rather than assumed: total host seconds divided by total device ticks over
/// the session. Jitter cancels in that ratio because it is zero-mean. Until
/// enough ticks accumulate to trust it, the host clock is used as before.
#[derive(Debug, Clone, Copy, Default)]
struct TickClock {
    prev: Option<u32>,
    prev_at: Option<std::time::Instant>,
    ticks: f64,
    secs: f64,
}

/// Ticks per report stay in this range in normal operation; anything outside is
/// a dropped run of reports or a counter wrap, and must not poison the ratio.
const TICK_MIN: u32 = 1;
const TICK_MAX: u32 = 4096;
/// Ticks to accumulate before the measured tick duration is trusted.
const TICK_WARMUP: f64 = 2048.0;

impl TickClock {
    /// Seconds since the previous report, or `None` on the first sample or
    /// after a gap.
    fn dt(&mut self, stamp: u32, fallback_hz: f32) -> Option<f32> {
        let now = std::time::Instant::now();
        let host = self
            .prev_at
            .replace(now)
            .map(|t| now.duration_since(t).as_secs_f64());
        let ticks = self.prev.replace(stamp).map(|p| stamp.wrapping_sub(p));

        let (ticks, host) = match (ticks, host) {
            (Some(t), Some(h)) => (t, h),
            _ => return None,
        };
        if !(TICK_MIN..=TICK_MAX).contains(&ticks) {
            return None;
        }
        // Only well-behaved host samples calibrate the tick. A scheduling stall
        // is not evidence about the device's clock.
        if (0.001..0.2).contains(&host) {
            self.ticks += ticks as f64;
            self.secs += host;
        }
        if self.ticks >= TICK_WARMUP {
            Some((ticks as f64 * (self.secs / self.ticks)) as f32)
        } else {
            Some(1.0 / fallback_hz)
        }
    }
}

impl OrientationTracker {
    /// `motion` is one parsed report. The canonical accel permutation is applied
    /// here so every caller gets canonical-frame angles and rates without having
    /// to remember to do it — forgetting it is what put the axes in the wrong
    /// order in the first place.
    pub fn update(&mut self, motion: &Motion) -> Orientation {
        let (accel, angle) = (motion.accel, motion.angle);
        let (roll, pitch) = tilt_from_accel(to_canonical_accel(accel));

        // Unwrap BEFORE converting to radians. The field covers half a turn, so
        // untracked wraps land as 180 degree flips in the middle of otherwise
        // ordinary motion.
        let h = angle[HEADING_AXIS];
        match self.prev_heading.replace(h) {
            Some(p) => unwrap_heading(p, h, &mut self.heading_acc),
            // Seed from the first reading so the absolute heading is preserved
            // rather than starting from zero at connect.
            None => self.heading_acc = h as i64,
        }
        let yaw = heading_rad(self.heading_acc);
        let euler_rad = [roll, pitch, yaw];

        let prev = match self.prev.replace(euler_rad) {
            Some(p) => p,
            None => {
                // Prime the field differencer too, or the SECOND sample
                // produces the spike this branch exists to prevent.
                self.field_gyro.rate(angle);
                return Orientation {
                    euler_rad,
                    rate_dps: [0.0; 3],
                    field_rate_dps: [0.0; 3],
                };
            }
        };

        // Measured sample rate, not an assumed one. Implausible gaps (a stalled
        // link, a paused debugger, the very first sample) fall back to the
        // nominal rate rather than producing an absurd spike.
        let now = std::time::Instant::now();
        let hz = match self.prev_at.replace(now) {
            Some(t) => {
                let dt = now.duration_since(t).as_secs_f32();
                if (0.001..0.2).contains(&dt) {
                    1.0 / dt
                } else {
                    REPORT_HZ
                }
            }
            None => REPORT_HZ,
        };

        let mut rate_dps = [0.0f32; 3];
        for i in 0..3 {
            let mut d = euler_rad[i] - prev[i];
            // Roll still wraps — `atan2` returns (-pi, pi] and rolling past
            // upside-down crosses that seam.
            if d > std::f32::consts::PI {
                d -= std::f32::consts::TAU;
            } else if d < -std::f32::consts::PI {
                d += std::f32::consts::TAU;
            }
            rate_dps[i] = d.to_degrees() * hz;
        }

        // The real gyro: difference the angle fields, drop the glitches, divide
        // by the DEVICE's elapsed time, and only then remove the resting bias.
        //
        // ❗ Order matters. Converting to deg/s BEFORE the bias step is what
        // makes the bias thresholds mean what they say: they are written in
        // deg/s, and feeding them counts-per-report made them depend on the
        // report rate.
        let counts = self.field_gyro.rate(angle);
        let dt = self.clock.dt(motion.timestamp, hz).unwrap_or(1.0 / hz);
        let per_sec = if dt > 1e-6 { 1.0 / dt } else { hz };
        let euler_dps = self.field_bias.correct([
            counts[0] * per_sec / ANGLE_COUNTS_PER_DEG,
            counts[1] * per_sec / ANGLE_COUNTS_PER_DEG,
            counts[2] * per_sec / ANGLE_COUNTS_PER_DEG,
        ]);

        // ⭐ If the controller is sending its REAL gyro, use it and throw all of
        // the above away — no wrap seam, no per-axis gain, no drift beyond the
        // sensor's own offset. The recovered path stays as a fallback so a
        // controller that withholds it still produces something.
        let field_rate_dps = match motion.gyro {
            Some(g) => {
                let s = 1.0 / GYRO_LSB_PER_DPS;
                [g[0] as f32 * s, g[1] as f32 * s, g[2] as f32 * s]
            }
            // Per-axis gain and nothing else — see `FIELD_GAIN`.
            None => {
                let g = field_gain();
                [euler_dps[0] * g[0], euler_dps[1] * g[1], euler_dps[2] * g[2]]
            }
        };

        Orientation { euler_rad, rate_dps, field_rate_dps }
    }
}

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

/// Read a 24-bit little-endian SIGNED value.
///
/// Sign-extended from bit 23, not zero-extended: the orientation angles use the
/// full range and read as large positives either side of the wrap point if the
/// top bit is ignored, which turns a small rotation into an apparent 16-million
/// count jump.
fn i24le(b: &[u8], off: usize) -> i32 {
    let raw = b[off] as u32 | (b[off + 1] as u32) << 8 | (b[off + 2] as u32) << 16;
    if raw & 0x80_0000 != 0 {
        (raw | 0xFF00_0000) as i32
    } else {
        raw as i32
    }
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
            let g = OFF_MOTION_ANGLE - sh;
            let p = OFF_MOTION_PROBE - sh;
            let mut probe = [0i16; 6];
            for (i, v) in probe.iter_mut().enumerate() {
                *v = i16le(payload, p + i * 2);
            }
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
                angle: [
                    i24le(payload, g),
                    i24le(payload, g + 4),
                    i24le(payload, g + 8),
                ],
                probe,
                ..Default::default()
            };

            // ⭐ The standard block, when the controller is sending it.
            //
            // ❗ NOT shifted by `sh`. The left/right one-byte offset was
            // measured on the OTHER block, and there is no evidence it applies
            // here — the documented layout quotes absolute offsets for both
            // halves. Applying an unverified shift would land one byte out,
            // which swaps every value's high and low halves and reads as
            // garbage rather than as an error.
            //
            // All-zero means the feature bit is off, not that the pad is
            // perfectly still: a real accelerometer always sees 1 g somewhere,
            // so an exactly-zero accel triple is proof of absence.
            if payload.len() >= OFF_STD_GYRO + 6 {
                let read3 = |base: usize| {
                    let v = [
                        i16le(payload, base),
                        i16le(payload, base + 2),
                        i16le(payload, base + 4),
                    ];
                    (v != [0, 0, 0]).then_some(v)
                };
                snap.motion.std_accel = read3(OFF_STD_ACCEL);
                // Gyro is only meaningful alongside a live accel block; on its
                // own an all-zero gyro is indistinguishable from a still pad.
                if snap.motion.std_accel.is_some() {
                    snap.motion.gyro = Some([
                        i16le(payload, OFF_STD_GYRO),
                        i16le(payload, OFF_STD_GYRO + 2),
                        i16le(payload, OFF_STD_GYRO + 4),
                    ]);
                }
            }
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
/// ⚠️ **CURRENTLY UNUSED — nothing calls this any more.** It corrected drift in
/// rates differenced out of the [`Motion::angle`] fields, and two of those three
/// fields turned out not to be angles. [`OrientationTracker`] replaced that
/// path, and its two sources are both absolute — gravity does not drift, and the
/// heading is magnetometer-corrected — so there is no bias to estimate.
///
/// Kept, rather than deleted, for the case where a genuine raw-rate gyro is
/// found on other hardware: a real MEMS rate WOULD need this, and the logic and
/// its tests are sound. Do not wire it back in without a raw rate to feed it.
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
///
/// ❗ Expressed in DEGREES PER SECOND and converted, not written as a raw count.
/// These were tuned as literal LSB values against a gyro scale that no longer
/// exists — the unit is now angle counts per report, where the same physical
/// rate is about 42x more counts. Left as literals they would have silently
/// become ~1 dps and ~0.9 dps, so every real motion would have looked like a
/// fast pan and the bias would never have converged.
const STILL_DELTA_DPS: f32 = 1.8;
const STILL_DELTA_LSB: f32 = STILL_DELTA_DPS;
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
const MAX_BIAS_DPS: f32 = 36.0;
const MAX_BIAS_LSB: f32 = MAX_BIAS_DPS;

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
    pub fn correct(&mut self, raw: [f32; 3]) -> [f32; 3] {
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

    /// The angle block sits 13 bytes ahead of the accel block, on both halves.
    #[test]
    fn orientation_angles_are_read_from_the_block_before_the_accelerometer() {
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_MOTION_LEN] = 30;
        // Three 24-bit LE angles on a 4-byte stride; byte 3 of each group is
        // the NEXT field's tag and must not leak into the value.
        buf[OFF_MOTION_ANGLE..OFF_MOTION_ANGLE + 3].copy_from_slice(&[0x78, 0x56, 0x34]);
        buf[OFF_MOTION_ANGLE + 3] = 0xFF;
        buf[OFF_MOTION_ANGLE + 4..OFF_MOTION_ANGLE + 7].copy_from_slice(&[0x00, 0x00, 0x80]);
        let s = parse_input(Side::Right, &buf).unwrap();
        assert_eq!(s.motion.angle[0], 0x345678);
        // 0x800000 is the most negative 24-bit value, not +8388608. Getting
        // this wrong puts a full half-turn of error either side of the wrap.
        assert_eq!(s.motion.angle[1], -8388608);
    }

    /// A wrap must read as a small step, not a 16-million-count jump.
    ///
    /// This is the whole reason the gyro went unfound for so long: read as a
    /// plain difference, one wrap per revolution produces a spike thousands of
    /// times larger than any real rotation.
    #[test]
    fn angle_wraparound_yields_a_small_rate_not_a_spike() {
        let mut g = AngleGyro::default();
        // First sample primes the state and must not itself be a rate.
        assert_eq!(g.rate([8_388_000, 0, 0]), [0.0; 3]);
        // Step forward across the +2^23 / -2^23 boundary. The raw difference
        // is -16_776_000; modulo 2^24 that is +1216.
        let r = g.rate([-8_388_000, 0, 0]);
        assert_eq!(r[0], 1216.0, "wrap read as {} instead of a small step", r[0]);

        // And the same going the other way.
        let mut g = AngleGyro::default();
        g.rate([-8_388_000, 0, 0]);
        assert_eq!(g.rate([8_388_000, 0, 0])[0], -1216.0);
    }

    /// The gyro map must be a PERMUTATION — every field used exactly once.
    ///
    /// A map that reuses a field silently drops an axis and doubles another,
    /// which on hardware reads as "one axis is dead" and gives no hint that the
    /// cause is a mapping table. Cheap to assert, miserable to debug.
    #[test]
    fn the_gyro_map_uses_every_field_exactly_once() {
        let mut seen = [false; 3];
        for (idx, sign) in GYRO_MAP {
            assert!(idx < 3, "field index {idx} out of range");
            assert!(!seen[idx], "field #{idx} is mapped to two canonical axes");
            assert!(sign == 1.0 || sign == -1.0, "sign must be ±1, got {sign}");
            seen[idx] = true;
        }
        assert!(seen.iter().all(|v| *v), "some field drives no axis");
    }

    /// The mapping actually moves the values it says it does.
    ///
    /// Pins the observed assignment — field #0 is PITCH, #1 is ROLL, #2 is YAW —
    /// against the canonical `(roll, pitch, yaw)` order, so a future edit has to
    /// disagree with the hands-on measurement rather than with a table.
    #[test]
    fn canonical_field_rate_places_each_field_on_its_observed_axis() {
        // Distinct values so a swap cannot hide behind symmetry.
        let out = canonical_field_rate([10.0, 20.0, 30.0]);
        assert_eq!(out[0], 20.0, "canonical roll should come from field #1");
        assert_eq!(out[1], 10.0, "canonical pitch should come from field #0");
        // ❗ Negated. Positive yaw sent the cursor the wrong way on hardware,
        // and correcting it downstream would leave every other consumer of the
        // canonical frame still wrong.
        assert_eq!(out[2], -30.0, "canonical yaw comes from field #2, INVERTED");
    }

    /// ⛔ The yaw gain must not be silently dropped.
    ///
    /// It is the difference between yaw reading a quarter of the other axes and
    /// reading the same — measured both on hardware and as a 4.15/4.29/4.14
    /// path-length ratio across captures. A default that drifts back to 1.0
    /// would restore the original complaint with no visible cause.
    #[test]
    fn the_yaw_field_keeps_its_measured_gain() {
        assert_eq!(FIELD_GAIN[0], 1.0);
        assert_eq!(FIELD_GAIN[1], 1.0);
        assert!(
            (FIELD_GAIN[2] - 4.0).abs() < 1e-6,
            "yaw gain is {} — hardware and captures both put it near 4",
            FIELD_GAIN[2],
        );
    }

    /// The device clock must refuse to answer where it has no evidence.
    ///
    /// Each of these returning a number instead of `None` is a silent wrong
    /// `dt`, and a wrong `dt` scales the whole rate — the failure is a gyro
    /// that reads plausibly but at the wrong sensitivity, which is far harder
    /// to notice than one that reads zero.
    #[test]
    fn the_device_clock_declines_when_it_cannot_know() {
        let mut c = TickClock::default();
        assert_eq!(c.dt(1000, 100.0), None, "first sample has no predecessor");
        // A normal advance answers, using the fallback until calibrated.
        let d = c.dt(1012, 100.0).expect("a normal step must produce a dt");
        assert!((d - 0.01).abs() < 1e-6, "expected the fallback 1/100 s, got {d}");
        // A repeated timestamp is not time passing.
        assert_eq!(c.dt(1012, 100.0), None, "zero ticks is not a duration");
        // A gap far beyond a dropped report or two: a counter wrap or a stall.
        assert_eq!(c.dt(1012 + TICK_MAX + 1, 100.0), None, "implausible gap");
    }

    /// Calibration must survive the timestamp counter wrapping at 2^32.
    ///
    /// Wrapping arithmetic makes this a non-event; plain subtraction would
    /// produce a four-billion-tick "gap" and, without the range guard, a `dt`
    /// of several hours that would flatten the gyro to zero for one sample.
    #[test]
    fn the_device_clock_survives_a_timestamp_wrap() {
        let mut c = TickClock::default();
        assert_eq!(c.dt(u32::MAX - 5, 100.0), None);
        // Crosses the wrap: MAX-5 -> 6 is 12 ticks, not -4294967284.
        let d = c.dt(6, 100.0).expect("a wrap is an ordinary 12-tick step");
        assert!(d > 0.0 && d < 1.0, "dt {d} is not a plausible report interval");
    }

    /// An ordinary rotation is passed through unchanged.
    #[test]
    fn a_normal_step_is_the_plain_difference() {
        let mut g = AngleGyro::default();
        g.rate([1000, -1000, 0]);
        assert_eq!(g.rate([1500, -1700, 250]), [500.0, -700.0, 250.0]);
    }

    /// The glitch that made motion unusable: a step too large to be real, and
    /// too SMALL to trip the wrap correction, which sits at half the modulus.
    ///
    /// Measured on hardware at around 8.18 million counts — just under the
    /// 8 388 608 wrap threshold, so it used to sail through as a genuine rate
    /// four decades past any hand motion.
    #[test]
    fn an_impossible_step_holds_the_previous_rate_instead_of_spiking() {
        let mut g = AngleGyro::default();
        g.rate([0, 0, 0]);
        // Establish a real pan.
        assert_eq!(g.rate([120_000, 0, 0])[0], 120_000.0);
        // Now the glitch, sized from the capture.
        let out = g.rate([120_000 + 8_176_435, 0, 0]);
        assert_eq!(out[0], 120_000.0, "glitch leaked through as a rate spike");
        // And the field carries on from its NEW value, so the next ordinary
        // step is an ordinary step rather than a second spike back.
        assert_eq!(g.rate([120_000 + 8_176_435 + 5_000, 0, 0])[0], 5_000.0);
    }

    /// A duplicated sample holds the rate instead of punching a zero into it.
    ///
    /// The controller repeats its last IMU sample under some conditions —
    /// observed while the added back button is held — and a duplicate
    /// differentiates to zero followed by a double step. That staircase is the
    /// jaggedness seen on an otherwise smooth pan.
    #[test]
    fn a_repeated_sample_holds_the_rate_rather_than_reading_as_a_stop() {
        let mut g = AngleGyro::default();
        g.rate([0, 0, 0]);
        assert_eq!(g.rate([90_000, 0, 0])[0], 90_000.0);
        // Same sample again: the pad did not stop, the report repeated.
        assert_eq!(g.rate([90_000, 0, 0])[0], 90_000.0, "duplicate read as a stop");
        // And the next genuine step is measured from the CURRENT value, so it
        // is an ordinary step rather than a doubled one.
        assert_eq!(g.rate([180_000, 0, 0])[0], 90_000.0);
    }

    /// ⛔ The rejection must not clip real motion.
    ///
    /// This is the test that makes the filter honest. The sensor's full scale
    /// is 2000 °/s, and at the slowest rate this link runs (67 Hz) that is 29.9°
    /// in one report. If the threshold ever creeps below that, hard flicks
    /// silently flatten and the gyro feels "capped" — which is far harder to
    /// diagnose than the spikes it was added to remove.
    #[test]
    fn a_full_scale_flick_is_not_mistaken_for_a_glitch() {
        let full_scale_step = (2000.0 / REPORT_HZ * ANGLE_COUNTS_PER_DEG) as i32;
        assert!(
            (full_scale_step as i64) < MAX_STEP,
            "full-scale rotation ({full_scale_step} counts/report) exceeds the \
             glitch threshold ({MAX_STEP}) — real flicks would be discarded",
        );
        let mut g = AngleGyro::default();
        g.rate([0; 3]);
        let out = g.rate([full_scale_step, -full_scale_step, 0]);
        assert_eq!(out[0], full_scale_step as f32);
        assert_eq!(out[1], -(full_scale_step as f32));
    }

    /// A wrap is still a wrap: the glitch filter must not swallow the legitimate
    /// modulus correction, which produces a RAW difference far larger than
    /// `MAX_STEP` and only becomes small after wrapping.
    #[test]
    fn the_glitch_filter_runs_after_the_wrap_correction() {
        let mut g = AngleGyro::default();
        g.rate([8_388_000, 0, 0]);
        assert_eq!(g.rate([-8_388_000, 0, 0])[0], 1216.0);
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

        // Same 12 bytes, six-i16 reading — see `Motion::probe`. Pinned from the
        // capture so the numbers on the probe pins have a written-down
        // provenance, and so a future offset change has to disagree with real
        // hardware rather than merely with the previous code.
        //
        // ⚠ Two of the six sit near i16 full scale WHILE THE PAD IS STILL.
        // Under a raw-gyro reading that is 2000+ deg/s on a controller lying on
        // a desk, so `probe[0]` and `probe[5]` are certainly not resting gyro
        // axes — which is a real, if unwelcome, thing for this test to say out
        // loud rather than leave for someone to rediscover on hardware.
        assert_eq!(s.motion.probe, [30720, -153, -767, -24, -1536, 32752]);
    }

    /// The probe block is the twelve bytes between the timestamp and the two
    /// zero bytes before the accelerometer, read little-endian in byte order.
    ///
    /// Also pins the left-half shift, because reading this block one byte out
    /// is not a small error: it swaps every value's high and low halves.
    #[test]
    fn the_probe_block_is_six_little_endian_i16_in_byte_order() {
        let mut buf = vec![0u8; INPUT_REPORT_LEN];
        buf[OFF_MOTION_LEN] = 30;
        let bytes: [u8; MOTION_PROBE_LEN] =
            [0x01, 0x00, 0x00, 0x80, 0xff, 0xff, 0x34, 0x12, 0x00, 0x00, 0xd0, 0x07];
        buf[OFF_MOTION_PROBE..OFF_MOTION_PROBE + MOTION_PROBE_LEN].copy_from_slice(&bytes);

        let s = parse_input(Side::Right, &buf).unwrap();
        assert_eq!(s.motion.probe, [1, -32768, -1, 0x1234, 0, 2000]);

        // The left half's block starts one byte earlier, so the same buffer
        // read as a left half must NOT yield the same values.
        let l = parse_input(Side::Left, &buf).unwrap();
        assert_ne!(l.motion.probe, s.motion.probe);

        let mut lbuf = vec![0u8; INPUT_REPORT_LEN];
        lbuf[OFF_MOTION_LEN - 1] = 30;
        lbuf[OFF_MOTION_PROBE - 1..OFF_MOTION_PROBE - 1 + MOTION_PROBE_LEN]
            .copy_from_slice(&bytes);
        assert_eq!(parse_input(Side::Left, &lbuf).unwrap().motion.probe, s.motion.probe);
    }

    /// The probe and the angle fields must read the SAME twelve bytes.
    ///
    /// They are two hypotheses about one block, so if a future offset edit
    /// moves one and not the other they stop being comparable and the whole
    /// point of exposing both is lost.
    #[test]
    fn the_probe_and_the_angles_cover_the_same_twelve_bytes() {
        assert_eq!(OFF_MOTION_ANGLE, OFF_MOTION_PROBE + 1, "angles start on the first tag byte");
        assert_eq!(
            OFF_MOTION_PROBE + MOTION_PROBE_LEN,
            OFF_MOTION_ANGLE + 11,
            "both readings must end on the same byte",
        );
        // And the block sits strictly between the timestamp and the accel.
        assert!(OFF_MOTION_PROBE >= OFF_MOTION_TIMESTAMP + 4);
        assert!(OFF_MOTION_PROBE + MOTION_PROBE_LEN <= OFF_MOTION_ACCEL);
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
        let resting = [(2.0), (-1.2), (0.6)];

        // Before convergence the offset still comes through.
        let first = bias.correct(resting);
        assert_eq!(first, resting);

        for _ in 0..REST_SAMPLES + 4 {
            bias.correct(resting);
        }
        assert!(bias.is_seeded());
        let out = bias.correct(resting);
        for (i, v) in out.iter().enumerate() {
            assert!(v.abs() < (0.1), "axis {i} still drifting: {v}");
        }
    }

    /// A large bias must still be learnable. An earlier version judged rest by
    /// closeness to zero, which meant a controller with a big offset never
    /// looked still and never converged — silently, and worst for the hardware
    /// that needed correcting most.
    #[test]
    fn a_large_bias_still_converges() {
        let mut bias = GyroBias::default();
        let resting = [MAX_BIAS_LSB - (5.0), 0.0, 0.0];
        for _ in 0..REST_SAMPLES + 4 {
            bias.correct(resting);
        }
        assert!(bias.is_seeded(), "never judged the controller still");
        assert!(bias.correct(resting)[0].abs() < (0.1));
    }

    /// A sustained pan at a perfectly constant rate is stable sample-to-sample,
    /// so stability alone would call it rest and cancel the motion out. The
    /// magnitude cap is what stops that.
    #[test]
    fn a_fast_constant_pan_is_never_absorbed_as_bias() {
        let mut bias = GyroBias::default();
        let panning = [MAX_BIAS_LSB + (100.0), 0.0, 0.0];
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
            let jitter = if i % 2 == 0 { (0.15) } else { (-0.15) };
            bias.correct([(2.0) + jitter, (-1.2), (0.6)]);
        }
        assert!(bias.is_seeded());
        let out = bias.correct([(2.0), (-1.2), (0.6)]);
        assert!(out.iter().all(|v| v.abs() < (0.5)), "{out:?}");
    }

    /// Real motion must pass through untouched, and must not be absorbed into
    /// the bias estimate — otherwise a slow steady turn would fade to zero.
    #[test]
    fn motion_passes_through_and_does_not_corrupt_the_estimate() {
        let mut bias = GyroBias::default();
        for _ in 0..REST_SAMPLES + 4 {
            bias.correct([(0.5), 0.0, 0.0]);
        }
        let before = bias.bias();

        // A deliberate 400 deg/s flick — far above the bias cap.
        let moving = [(400.0), 0.0, 0.0];
        for _ in 0..50 {
            let out = bias.correct(moving);
            assert!(out[0] > (350.0), "motion was swallowed: {}", out[0]);
        }
        let after = bias.bias();
        assert!(
            (after[0] - before[0]).abs() < (0.1),
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

#[cfg(test)]
mod orientation_tests {
    use super::*;

    const G: i32 = ACCEL_LSB_PER_G as i32;

    /// A motion block carrying just the two fields these tests exercise.
    ///
    /// The timestamp is left at zero deliberately. [`TickClock`] needs it to
    /// ADVANCE to derive a `dt`, so a constant one exercises the fallback path
    /// — which is the behaviour these tests were written against and which must
    /// keep working, since it is what runs for the first fraction of a second
    /// after every connect.
    fn sample(accel: [i32; 3], angle: [i32; 3]) -> Motion {
        Motion { accel, angle, ..Default::default() }
    }

    #[test]
    fn flat_on_a_table_reads_level() {
        // Canonical frame is (forward, side, vertical), so gravity at rest sits
        // entirely on the vertical axis.
        let (roll, pitch) = tilt_from_accel([0, 0, G]);
        assert!(roll.abs() < 1e-6, "roll {roll}");
        assert!(pitch.abs() < 1e-6, "pitch {pitch}");
    }

    #[test]
    fn tilting_each_way_moves_the_expected_angle_in_the_expected_direction() {
        // Rolled onto its side: gravity moves off vertical into the side axis,
        // and roll goes to +90 with pitch untouched.
        let (roll, pitch) = tilt_from_accel([0, G, 0]);
        assert!((roll.to_degrees() - 90.0).abs() < 0.5, "roll {}", roll.to_degrees());
        assert!(pitch.abs() < 1e-6);

        // ❗ Gravity on canonical +x means the NOSE IS UP, and the contract
        // says pitch is positive nose-up — so this is +90, not -90. The
        // negated form here was self-consistent and still inverted `gyro_y`
        // against every other pad.
        let (roll, pitch) = tilt_from_accel([G, 0, 0]);
        assert!((pitch.to_degrees() - 90.0).abs() < 0.5, "pitch {}", pitch.to_degrees());
        assert!(roll.abs() < 1e-6);

        // A 45 degree roll, checked against the arithmetic rather than a
        // hand-picked constant.
        let h = (G as f32 / 2f32.sqrt()) as i32;
        let (roll, _) = tilt_from_accel([0, h, h]);
        assert!((roll.to_degrees() - 45.0).abs() < 0.5, "roll {}", roll.to_degrees());
    }

    #[test]
    fn free_fall_reports_level_rather_than_noise() {
        // With no gravity vector there is no defensible angle; inventing one
        // from sensor noise would swing the orientation wildly.
        assert_eq!(tilt_from_accel([0, 0, 0]), (0.0, 0.0));
    }

    #[test]
    fn a_full_turn_of_heading_counts_is_a_full_turn_of_yaw() {
        let full = ANGLE_COUNTS_PER_TURN;
        // Negative: the field counts opposite to the canonical yaw convention
        // (+ clockwise), and the negation lives in `heading_rad` so exactly one
        // place owns it.
        assert!((heading_rad(full) + std::f32::consts::TAU).abs() < 1e-3);
        assert!((heading_rad(full / 4) + std::f32::consts::FRAC_PI_2).abs() < 1e-3);
    }

    #[test]
    fn the_heading_wrap_does_not_appear_as_a_180_degree_flip() {
        // ⭐ The regression this exists for. The field is 2^24 wide but a turn
        // is 2^25, so it wraps TWICE per revolution, and reading it directly
        // produced an instant half-turn flip at each seam — clearly visible as
        // full-scale spikes on an otherwise noisy yaw trace.
        let g = ACCEL_LSB_PER_G as i32;
        let step = (ANGLE_COUNTS_PER_TURN / 720) as i32; // half a degree
        let mut t = OrientationTracker::default();
        let mut h = (ANGLE_FIELD_MODULUS / 2) as i32 - 3 * step;

        let mut worst: f32 = 0.0;
        for _ in 0..12 {
            // Step steadily across the wrap point in the raw field.
            h = h.wrapping_add(step);
            if h as i64 >= ANGLE_FIELD_MODULUS / 2 {
                h -= ANGLE_FIELD_MODULUS as i32;
            }
            let o = t.update(&sample([0, 0, g], [0, 0, h]));
            worst = worst.max(o.rate_dps[2].abs());
        }
        // Half a degree per report at REPORT_HZ is ~34 deg/s. A missed wrap
        // would show up here as roughly 180 * REPORT_HZ = 12000 deg/s.
        assert!(worst < 200.0, "wrap leaked through as {worst} deg/s");
    }

    #[test]
    fn the_first_sample_produces_no_rate_spike() {
        let mut t = OrientationTracker::default();
        let o = t.update(&sample([0, 0, G], [0, 0, 12_345_678]));
        assert_eq!(o.rate_dps, [0.0; 3]);
        // ...but the orientation itself is available immediately.
        assert!(o.euler_rad[2] != 0.0);
    }

    #[test]
    fn a_steady_yaw_turn_reads_the_right_rate() {
        // 1 degree of yaw per report at REPORT_HZ reports per second is
        // REPORT_HZ deg/s, straight from the definition.
        let per_report = (ANGLE_COUNTS_PER_TURN / 360) as i32;
        let mut t = OrientationTracker::default();
        t.update(&sample([0, 0, G], [0, 0, 0]));
        let o = t.update(&sample([0, 0, G], [0, 0, per_report]));
        // Negative: the field counts opposite to the canonical yaw convention,
        // and `heading_rad` owns that negation.
        assert!(
            (o.rate_dps[2] + REPORT_HZ).abs() < 1.0,
            "yaw rate {} deg/s, expected {}",
            o.rate_dps[2],
            -REPORT_HZ
        );
    }

    #[test]
    fn wrapping_the_heading_does_not_produce_a_spike() {
        // The field wraps at 2^24 while a turn is 2^25, so this is not
        // hypothetical — it happens twice per revolution. A plain subtraction
        // here yields a full-circle jump and a violent flick in the aim mapping.
        let one_deg = (ANGLE_COUNTS_PER_TURN / 360) as i32;
        let half = (ANGLE_COUNTS_PER_TURN / 2) as i32;
        let mut t = OrientationTracker::default();
        t.update(&sample([0, 0, G], [0, 0, half - one_deg]));
        let o = t.update(&sample([0, 0, G], [0, 0, half + one_deg]));
        assert!(
            o.rate_dps[2].abs() < 3.0 * REPORT_HZ,
            "wrap produced a {} deg/s spike",
            o.rate_dps[2]
        );
    }

    #[test]
    fn holding_still_produces_no_drift() {
        // The point of sourcing both angles from absolute references: there is
        // no bias to accumulate, so a stationary controller reports exactly
        // zero rate forever rather than needing a GyroBias estimator.
        let mut t = OrientationTracker::default();
        for _ in 0..500 {
            let o = t.update(&sample([12, -8, G], [0, 0, 5_000_000]));
            assert_eq!(o.rate_dps, [0.0; 3]);
        }
    }
}

#[cfg(test)]
mod real_frame_tests {
    use super::*;

    /// A real resting frame from each half, byte for byte out of a hardware
    /// capture (`jc2_imu`, grip flat on the table).
    ///
    /// ⭐ These exist because "the accel pins read zero" is ambiguous between a
    /// parser bug and the controller not sending motion at all, and the two
    /// have completely different fixes. Running the REAL parse path over REAL
    /// captured bytes decides it offline, with no hardware and no guessing.
    const LEFT_RESTING: [u8; 63] = [
        0x29, 0x18, 0x00, 0x00, 0x07, 0x00, 0xf8, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1e, 0x55,
        0xc4, 0x00, 0x0c, 0x00, 0x1a, 0x90, 0xf7, 0x01, 0x0f, 0xe3, 0x00, 0x01, 0xd4, 0xb5, 0x7e, 0x00,
        0x00, 0x9f, 0xff, 0x00, 0x00, 0xf5, 0xff, 0x00, 0x00, 0x11, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    const RIGHT_RESTING: [u8; 63] = [
        0x9c, 0x18, 0x00, 0x00, 0x07, 0xb2, 0xa7, 0x7e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1e,
        0xef, 0xc0, 0x00, 0x0c, 0x00, 0x94, 0x8d, 0xfd, 0x01, 0x1d, 0x5b, 0xff, 0x80, 0x5f, 0xa2, 0x80,
        0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0xe3, 0x0f, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn a_real_captured_frame_yields_one_g_of_gravity() {
        for (side, frame) in [(Side::Left, LEFT_RESTING), (Side::Right, RIGHT_RESTING)] {
            let snap = parse_input(side, &frame).expect("parses");
            assert_eq!(snap.motion_len, 30, "{side:?} motion_len");
            let a = snap.motion.accel;
            let mag = ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]) as f32).sqrt();
            assert!(
                (mag - ACCEL_LSB_PER_G).abs() / ACCEL_LSB_PER_G < 0.05,
                "{side:?} accel {a:?} has magnitude {mag}, expected ~{ACCEL_LSB_PER_G}"
            );
            // Flat on a table: gravity is on the vertical axis and the other
            // two are near zero. Catches an offset slip that still happens to
            // produce a plausible magnitude.
            assert!(a[2].abs() > 3500, "{side:?} vertical axis {a:?}");
            assert!(a[0].abs() < 400 && a[1].abs() < 400, "{side:?} level axes {a:?}");
        }
    }

    #[test]
    fn a_real_captured_frame_yields_a_level_orientation() {
        for (side, frame) in [(Side::Left, LEFT_RESTING), (Side::Right, RIGHT_RESTING)] {
            let snap = parse_input(side, &frame).expect("parses");
            let (roll, pitch) = tilt_from_accel(snap.motion.accel);
            assert!(
                roll.to_degrees().abs() < 8.0 && pitch.to_degrees().abs() < 8.0,
                "{side:?} resting flat but reads roll {:.1} pitch {:.1}",
                roll.to_degrees(),
                pitch.to_degrees()
            );
        }
    }

    #[test]
    fn the_heading_field_is_non_zero_in_a_real_frame() {
        // If this ever reads zero the orientation collapses to identity and the
        // pin looks "dead but lit" — the exact symptom to tell apart from a
        // controller that is not sending motion at all.
        for (side, frame) in [(Side::Left, LEFT_RESTING), (Side::Right, RIGHT_RESTING)] {
            let snap = parse_input(side, &frame).expect("parses");
            assert_ne!(snap.motion.angle[HEADING_AXIS], 0, "{side:?} heading");
        }
    }
}
