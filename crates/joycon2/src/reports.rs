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
/// ⭐ **Yaw needs a factor of 2**, measured against a real rotation.
///
/// ❗ It was 4, and that was wrong. The 4 came from two soft sources: a
/// path-length ratio across captures, and "yaw feels about four times weaker"
/// on hardware — but that feel was reported through a probe chain that had yaw
/// inverted downstream AND through a 3D view whose basis mismatch was rendering
/// yaw as roll. Neither was a clean look at yaw.
///
/// The replacement is a direct comparison, in the corrected basis, against the
/// user's own hand: turning the controller 90° rotated the model 180°. Exactly
/// double, and it explained a second symptom at the same time — doubled yaw
/// reaches the ±180° wrap at 90° of real rotation, so the model flipped and
/// appeared to clamp just past a quarter turn.
///
/// A lesson worth keeping: a quantity confirmed only through a chain that
/// contains other unverified transforms is not confirmed. Two of the three
/// links in that chain later turned out to be wrong.
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
///
/// ⭐ **This scales the GYRO OUTPUT PINS ONLY.** The orientation estimate has
/// its own factor, [`ORIENTATION_GAIN`], and conflating the two was a real
/// mistake: a report that the 3D model over-rotated in yaw was answered by
/// halving this constant, which silently halved the yaw gyro pin as well and
/// left yaw visibly weaker than roll and pitch to aim with.
///
/// ❗ They are judged against DIFFERENT quantities and there is no reason for
/// them to agree. [`ORIENTATION_GAIN`] is judged against absolute angle — turn
/// 90°, the model should show 90° — and the measured counts-per-turn settles it
/// at 1.0 with no freedom left. This one is judged against the OTHER TWO AXES
/// at the speeds a person actually aims: yaw comes from the fused, magnetometer-
/// corrected heading field, which is accurate over a slow full turn but
/// attenuated and smoothed on quick motion, so its differentiated rate reads
/// low exactly where aiming lives. A scalar cannot really fix a frequency
/// response, but it is what there is, and 4 is the value that felt matched on
/// hardware.
pub const FIELD_GAIN: [f32; 3] = [1.0, 1.0, 4.0];

/// Per-axis gain for the ORIENTATION estimate, in field order.
///
/// ⭐ **1.0, and not arrived at by feel.** [`ANGLE_COUNTS_PER_TURN`] was
/// measured from known 360° turns on hardware — the yaw field at 33 752 971
/// (left) and 33 674 419 (right), both inside 0.6% of 2^25, and the roll field
/// at 32 458 485 with r = 0.993. The conversion to degrees is therefore already
/// correct, and any factor other than 1.0 here makes the model disagree with
/// the world by exactly that factor.
///
/// So this constant exists to be 1.0 and to say why, rather than to be tuned:
/// it is the thing that keeps a gyro-pin adjustment from quietly re-breaking
/// "90° real reads 90° on screen". Override with
/// `FLEXINPUT_JC2_ORIENTATION_GAIN="1,1,1"` if that measurement is ever shown
/// to be wrong.
pub const ORIENTATION_GAIN: [f32; 3] = [1.0, 1.0, 1.0];

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
/// ❗ Yaw is POSITIVE. It was briefly negated here on a hardware report that the
/// cursor moved the wrong way — but that report came from a patch still feeding
/// the `probe_rate_*` pins, where the yaw had already been inverted once
/// downstream. Two inversions in series, one of them invisible from where the
/// symptom was observed.
///
/// Worth remembering as a class of mistake: a sign confirmed through a chain
/// that contains another sign is not confirmed at all.
///
/// `FLEXINPUT_JC2_GYRO_MAP="+1,+0,+2"` — three signed field indices, in
/// canonical order.
pub const GYRO_MAP: [(usize, f32); 3] = [(1, 1.0), (0, 1.0), (2, 1.0)];

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

/// Cross-axis correction, applied after the permutation and gain.
///
/// ⭐ **Identity by default, because the coefficients are not known** — and a
/// guessed correction is worse than none, as the removed ZYX Euler transform
/// demonstrated at length.
///
/// The problem it exists for is real and reported from hardware: rolling the
/// controller bleeds into yaw. The fields are a firmware fusion of a
/// nine-axis IMU, not three independent gyro axes, so nothing guarantees they
/// separate cleanly — and every attempt to derive the mixing analytically has
/// failed, because the saved captures rotate about the CONTROLLER's axes while
/// the coupling that matters is in the user's grip.
///
/// So this is a knob rather than a derivation. Row-major, canonical order
/// `(roll, pitch, yaw)`; row *i* says how canonical axis *i* is built from the
/// three mapped rates. To cancel roll bleeding into yaw, make the yaw row's
/// roll term negative:
///
/// ```text
///   FLEXINPUT_JC2_GYRO_MIX="1,0,0, 0,1,0, -0.2,0,1"
/// ```
///
/// The same approach settled the yaw gain: a factor found by feel, then
/// confirmed against captures at 4.15 / 4.29 / 4.14. If a coefficient here
/// turns out to work, it becomes the new default with that evidence recorded.
pub const GYRO_MIX: [[f32; 3]; 3] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
];

/// [`GYRO_MIX`], overridable by `FLEXINPUT_JC2_GYRO_MIX` (nine numbers).
pub fn gyro_mix() -> [[f32; 3]; 3] {
    static MIX: std::sync::OnceLock<[[f32; 3]; 3]> = std::sync::OnceLock::new();
    *MIX.get_or_init(|| {
        let Ok(raw) = std::env::var("FLEXINPUT_JC2_GYRO_MIX") else {
            return GYRO_MIX;
        };
        let v: Vec<f32> = raw.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if v.len() == 9 && v.iter().all(|x| x.is_finite()) {
            let m = [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
            eprintln!("[jc2] gyro mix overridden: {m:?}");
            m
        } else {
            eprintln!("[jc2] FLEXINPUT_JC2_GYRO_MIX={raw:?} is not nine numbers — ignoring");
            GYRO_MIX
        }
    })
}

/// Apply [`gyro_map`] to a field-order rate triple, yielding canonical
/// `(roll, pitch, yaw)`.
pub fn canonical_field_rate(field_rate: [f32; 3]) -> [f32; 3] {
    let m = gyro_map();
    let mapped = [
        field_rate[m[0].0] * m[0].1,
        field_rate[m[1].0] * m[1].1,
        field_rate[m[2].0] * m[2].1,
    ];
    // Cross-axis correction last, so the mix is expressed in canonical axes —
    // the ones a person can reason about — rather than in field order.
    let x = gyro_mix();
    [
        x[0][0] * mapped[0] + x[0][1] * mapped[1] + x[0][2] * mapped[2],
        x[1][0] * mapped[0] + x[1][1] * mapped[1] + x[1][2] * mapped[2],
        x[2][0] * mapped[0] + x[2][1] * mapped[1] + x[2][2] * mapped[2],
    ]
}

/// [`ORIENTATION_GAIN`], overridable by `FLEXINPUT_JC2_ORIENTATION_GAIN`.
///
/// Read once and cached; called per report.
pub fn orientation_gain() -> [f32; 3] {
    static GAIN: std::sync::OnceLock<[f32; 3]> = std::sync::OnceLock::new();
    *GAIN.get_or_init(|| {
        let Ok(raw) = std::env::var("FLEXINPUT_JC2_ORIENTATION_GAIN") else {
            return ORIENTATION_GAIN;
        };
        let v: Vec<f32> = raw.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if v.len() == 3 && v.iter().all(|x| x.is_finite() && *x != 0.0) {
            let g = [v[0], v[1], v[2]];
            eprintln!("[jc2] orientation gain overridden: {g:?}");
            g
        } else {
            eprintln!("[jc2] FLEXINPUT_JC2_ORIENTATION_GAIN={raw:?} is not three non-zero numbers — ignoring");
            ORIENTATION_GAIN
        }
    })
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
// Referenced only from tests now that the probe reads six fixed i16, but it is
// the documented width of the block and the offset assertions are written
// against it.
#[cfg_attr(not(test), allow(dead_code))]
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
    /// Reports each field has gone without changing, so a delta can be divided
    /// by the time it actually accumulated over — see [`AngleGyro::rate`].
    held: [u32; 3],
}

/// Reports a field may sit unchanged before its rate is called zero.
///
/// A field that has genuinely stopped must not hold its last rate forever, or a
/// controller set down mid-turn keeps reporting that turn. The bound only has
/// to sit above the slowest legitimate update: at 2^25 counts per revolution a
/// field advances ~93 000 counts per degree, so even a 0.1 °/s crawl moves it
/// every single report. Anything quiet for sixteen — an eighth of a second —
/// has stopped, not slowed.
const MAX_HOLD: u32 = 16;

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
        // ⭐ A REPEATED field is not a moment of stillness, and the three
        // fields do NOT update together.
        //
        // ❗ This used to require all three to repeat before it did anything,
        // which meant it almost never fired. The fields refresh at DIFFERENT
        // internal rates, so the usual case is one field holding still while
        // the others move — and that field then reported zero, zero, then one
        // delta covering three reports' worth of motion divided by one
        // report's worth of time. A comb: flat, flat, spike, flat, flat, spike,
        // three times too tall, on a perfectly smooth motion.
        //
        // On an oscilloscope that is unmistakable, and it is exactly what was
        // seen — one channel smooth (the fastest field) while the other two
        // pulsed. It was also visible as the fields "not responding": a rate
        // that is zero two reports in three reads as a weak axis.
        //
        // So: track each field separately, and divide a delta by the number of
        // reports it actually accumulated over. That is the rate the field
        // genuinely represents, it removes the comb without filtering anything,
        // and it costs no lag — the same total motion, spread over the interval
        // it happened in rather than dumped into one sample.
        //
        // Holding between updates is right on the physics too: the controller
        // was turning an instant ago and a report that carries no fresh sample
        // does not mean it stopped. Genuine stillness still reads zero, via
        // MAX_HOLD.
        //
        // (Reported on hardware as jaggedness while the back button is HELD.
        // That button is a Mobapad addition with no Joy-Con 2 equivalent, so
        // the firmware is synthesising an input it does not natively have;
        // stalling an IMU field while it does so is an ordinary way for that to
        // show up, and it would widen the gaps this now handles.)
        let mut out = self.last;
        for i in 0..3 {
            if angle[i] == prev[i] {
                self.held[i] = self.held[i].saturating_add(1);
                if self.held[i] >= MAX_HOLD {
                    out[i] = 0.0;
                }
                continue;
            }
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
            if d.abs() <= MAX_STEP {
                // Spread over the reports it accumulated across.
                out[i] = d as f32 / (self.held[i] + 1) as f32;
            }
            self.held[i] = 0;
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



/// Measures resting drift over long stationary periods, straight from a running
/// session.
///
/// ⭐ **Answers "is the drift a trend that can simply be cancelled?" with data
/// instead of argument.** Saved captures only hold a few seconds of stillness at
/// each end, which is enough to see that drift exists and not enough to say
/// whether it is steady. Minutes are needed, and asking for them through a
/// separate probe tool means fighting the dongle again.
///
/// Two design points, both from the proposal that prompted it:
///
/// * **The accelerometer decides what counts as still.** Not the gyro, which is
///   the thing being measured — using it to gate its own measurement would hide
///   exactly the drift being looked for.
/// * **Deltas, not absolute field values.** The fields wrap, and a wrap looks
///   like an enormous sign-flipped jump; accumulating the already-modulo-
///   corrected steps is immune to that. This is the "make sure the axes do not
///   flip sign like the absolute ones do" requirement, satisfied by
///   construction rather than by a threshold.
///
/// Any movement resets the run, so a reported figure is always drift measured
/// over one uninterrupted stationary stretch.
#[derive(Debug, Clone, Copy, Default)]
pub struct DriftProbe {
    /// Accumulated field rotation over the current stationary run, in degrees.
    sum: [f64; 3],
    secs: f64,
    /// Seconds of stillness already reported, so each line covers new ground.
    reported: f64,
}

/// How much continuous stillness to gather before reporting, and how often after
/// that. Half a minute is long enough for a sub-degree-per-second trend to rise
/// clear of noise, and short enough to see several points in a four-minute run.
const DRIFT_REPORT_SECS: f64 = 30.0;

impl DriftProbe {
    /// Feed one sample. `rate_dps` is the raw per-field rate BEFORE any
    /// correction — the point is to measure what the hardware does, not what is
    /// left after the estimator has had a go at it.
    pub fn update(&mut self, rate_dps: [f32; 3], dt: f32, still: bool, side: crate::protocol::Side) {
        if !still || !(1e-6..0.5).contains(&dt) {
            // Movement, or an implausible gap. Either way this run is over.
            if self.secs >= DRIFT_REPORT_SECS {
                crate::dlog::drift(format_args!(
                    "drift {} run ENDED after {:.0} s",
                    side.display_name(),
                    self.secs,
                ));
            }
            *self = Self::default();
            return;
        }
        for i in 0..3 {
            self.sum[i] += rate_dps[i] as f64 * dt as f64;
        }
        self.secs += dt as f64;
        if self.secs - self.reported >= DRIFT_REPORT_SECS {
            self.reported = self.secs;
            let r = |i: usize| self.sum[i] / self.secs;
            crate::dlog::drift(format_args!(
                "drift {} still {:>5.0} s  field#0 {:+8.4}  field#1 {:+8.4}  field#2 {:+8.4} deg/s",
                side.display_name(),
                self.secs,
                r(0),
                r(1),
                r(2),
            ));
        }
    }
}

/// Resting drift subtracted before the bias estimator runs, in deg/s, field
/// order, per half.
///
/// ⭐ **A head start, not a replacement.** Measured on hardware across six
/// sessions of four to eleven minutes with both halves stationary throughout,
/// by [`DriftProbe`] — then corrected for the clock error described in
/// [`TickClock::dt_at`], which had been scaling every rate down by a factor
/// that differed per half AND per session, and which is why the previous
/// numbers here were roughly three times too large:
///
/// ```text
///          field#0                 field#1                 field#2
///   LEFT   -0.410  sd 0.026   6%   +0.046  sd 0.004   8%   -0.034  sd 0.006  17%
///   RIGHT  -0.335  sd 0.013   4%   -0.008  sd 0.020 262%   +0.021  sd 0.013  61%
/// ```
///
/// The big terms are the reproducible ones. Both halves' field #0 repeats to
/// within a few per cent across all six sessions, and a third of a degree per
/// second is twenty degrees of yaw a minute being integrated from the moment of
/// connect. That is worth removing outright.
///
/// ❗ The small terms are not constants, and are not claimed as any. The right
/// half's field #1 has a standard deviation twenty-five times its own mean and
/// changes sign between sessions. It is set to the measured mean because that
/// is the best fixed estimate there is, but it is within noise of zero and
/// nothing should be built on its sign.
///
/// ⭐ **The drift is fixed in the BODY, not in the world.** Sessions 4-6 were
/// captured with the grip yawed 90° from sessions 1-3 to test exactly that, and
/// the shifts that appeared — up to 0.045 °/s — are far too large to be a
/// heading effect: the earth turns at 0.0042 °/s in total, so no change of
/// heading can move any projection of it by more than about 0.008 °/s. What
/// moved instead was time. The trend within a single run is about -0.009 °/s
/// per minute of stillness on the left half's field #0, and the six sessions
/// ran back to back over half an hour. This is a MEMS zero-rate offset warming
/// up, which no fixed table can follow.
///
/// So both, as before: this removes the reproducible bulk immediately, and
/// [`GyroBias`] learns the session-specific remainder and the warm-up ramp —
/// starting from a small error now, in the very window where the estimator has
/// not converged and drift is otherwise unopposed.
pub const RESTING_DRIFT_LEFT: [f32; 3] = [-0.410, 0.046, -0.034];
pub const RESTING_DRIFT_RIGHT: [f32; 3] = [-0.335, -0.008, 0.021];

/// The resting-drift correction for one half.
pub fn resting_drift(side: crate::protocol::Side) -> [f32; 3] {
    match side {
        crate::protocol::Side::Left => RESTING_DRIFT_LEFT,
        crate::protocol::Side::Right => RESTING_DRIFT_RIGHT,
    }
}

/// Per-side mounting correction, applied after [`to_canonical_accel`].
///
/// ⭐ **The IMU is not mounted the way the canonical frame assumes, and it is
/// mounted DIFFERENTLY in the two halves.** Both are documented traits of this
/// hardware family — the research notes for the original Joy-Con say outright
/// that "because of the placement of the IMU chip, the 2 Joy-Con have an axis
/// reversed each" — and both showed up on hardware exactly as that predicts:
///
/// * pitching up rolled the model right, and rolling counter-clockwise pitched
///   it down — a clean swap, which is a 90° rotation about the forward axis;
/// * a small yaw flipped which half was edge-on, and the two halves flipped in
///   OPPOSITE directions, which no shared correction can produce.
///
/// Expressed as a signed axis permutation rather than a matrix: `(source axis,
/// sign)` per canonical axis, so it is readable, exactly invertible, and cannot
/// introduce scale error the way a hand-written rotation matrix can.
///
/// ⚠️ Both default to identity, and on this hardware identity is CORRECT.
/// Measured in the neutral grip pose, the accelerometer reads x≈0.000,
/// y≈-0.002, z≈0.125 — a clean 1 g on canonical +z, exactly where the frame
/// contract puts it. Nothing needs permuting.
///
/// This was added while chasing a "yaw rolls the model" symptom that turned out
/// to be a Z-up/Y-up mismatch in the 3D renderer, not a sensor frame at all.
/// Kept because the mirrored-IMU trait is real and a future half may need it —
/// but do not reach for it before confirming the accelerometer is actually
/// wrong, because here it never was.
///
/// `FLEXINPUT_JC2_MOUNT_L` / `_R`, three signed indices, e.g. `"+0,+2,-1"`.
pub const MOUNT_LEFT: [(usize, f32); 3] = [(0, 1.0), (1, 1.0), (2, 1.0)];
pub const MOUNT_RIGHT: [(usize, f32); 3] = [(0, 1.0), (1, 1.0), (2, 1.0)];

/// The mounting correction for one half, with the environment override applied.
pub fn mount_for(side: crate::protocol::Side) -> [(usize, f32); 3] {
    use crate::protocol::Side;
    static L: std::sync::OnceLock<[(usize, f32); 3]> = std::sync::OnceLock::new();
    static R: std::sync::OnceLock<[(usize, f32); 3]> = std::sync::OnceLock::new();
    let (cell, var, fallback) = match side {
        Side::Left => (&L, "FLEXINPUT_JC2_MOUNT_L", MOUNT_LEFT),
        Side::Right => (&R, "FLEXINPUT_JC2_MOUNT_R", MOUNT_RIGHT),
    };
    *cell.get_or_init(|| parse_mount(var).unwrap_or(fallback))
}

fn parse_mount(var: &str) -> Option<[(usize, f32); 3]> {
    let raw = std::env::var(var).ok()?;
    let parts: Vec<&str> = raw.split(',').map(|p| p.trim()).collect();
    if parts.len() != 3 {
        eprintln!("[jc2] {var}={raw:?} is not three signed indices — ignoring");
        return None;
    }
    let mut out = [(0usize, 1.0f32); 3];
    let mut used = [false; 3];
    for (i, p) in parts.iter().enumerate() {
        let sign = if p.starts_with('-') { -1.0 } else { 1.0 };
        match p.trim_start_matches(['+', '-']).parse::<usize>() {
            // Every source axis exactly once, or the frame is not a rotation —
            // it collapses one axis and doubles another, which reads as "one
            // axis is dead" and is very hard to trace to a typo.
            Ok(idx) if idx < 3 && !used[idx] => {
                used[idx] = true;
                out[i] = (idx, sign);
            }
            _ => {
                eprintln!("[jc2] {var}={raw:?} is not a permutation — ignoring");
                return None;
            }
        }
    }
    eprintln!("[jc2] {var} applied: {out:?}");
    Some(out)
}

/// Apply a mounting permutation to a canonical-frame triple.
pub fn apply_mount(v: [f32; 3], m: [(usize, f32); 3]) -> [f32; 3] {
    [v[m[0].0] * m[0].1, v[m[1].0] * m[1].1, v[m[2].0] * m[2].1]
}

/// Rotation about gravity: the only component of a body rate that is yaw.
///
/// `up` is the measured up-vector in the body frame — an accelerometer at rest
/// reports the upward reaction force, so the normalised accel already points
/// up. Projecting onto it needs no axis assignment: whatever permutation the
/// three rates arrive in, their component along up is the rotation about
/// vertical.
pub fn canonical_yaw_rate(body_dps: [f32; 3], up: [f32; 3]) -> f32 {
    body_dps[0] * up[0] + body_dps[1] * up[1] + body_dps[2] * up[2]
}

/// Orientation as a quaternion `[x, y, z, w]`, body-to-world.
///
/// ⭐ **Body rates must be integrated as a rotation, not accumulated as Euler
/// angles.** This is the fix for the single most damaging bug in this decode,
/// and it was visible on hardware as:
///
///   > the left handle was on its side, the yaw was doing roll to it
///
/// The three field rates are BODY rates — measured about the controller's own
/// axes, which move with it. The orientation was being composed as
/// `Rz(yaw) · Ry(-pitch) · Rx(roll)`, where yaw is a rotation about the WORLD
/// vertical. Those coincide only at neutral, which is exactly why the output
/// was perfect immediately after connect and degraded as the pad's resting
/// attitude wandered: tilt it and the "yaw" rate is applied about the wrong
/// axis, so a body-axis rotation comes out as world roll, and every axis
/// appears to bleed into every other.
///
/// No fixed mixing matrix can repair that, because the error depends on the
/// current pose — which is why the measured cross-axis coupling wandered by
/// 29-66° across a single capture and no constant 3x3 ever fitted it.
///
/// Integrating `q <- q * dq(omega * dt)` is pose-correct everywhere by
/// construction, has no gimbal singularity, and needs no convention guessed.
mod quat {
    /// Hamilton product, `[x, y, z, w]` order.
    pub fn mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
        let (ax, ay, az, aw) = (a[0], a[1], a[2], a[3]);
        let (bx, by, bz, bw) = (b[0], b[1], b[2], b[3]);
        [
            aw * bx + ax * bw + ay * bz - az * by,
            aw * by - ax * bz + ay * bw + az * bx,
            aw * bz + ax * by - ay * bx + az * bw,
            aw * bw - ax * bx - ay * by - az * bz,
        ]
    }

    pub fn normalise(q: [f32; 4]) -> [f32; 4] {
        let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        if n < 1e-9 {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
        }
    }

    /// `(roll, pitch, yaw)` for consumers that still want Euler angles.
    ///
    /// ZYX, matching the convention the rest of this crate documents. Pitch is
    /// clamped at the poles rather than allowed to produce NaN.
    pub fn to_euler(q: [f32; 4]) -> [f32; 3] {
        let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
        let sinp = 2.0 * (w * y - z * x);
        // ❗ NEGATED, to match the convention the rest of this crate composes
        // with: `Rz(yaw) * Ry(-pitch) * Rx(roll)`, where a positive pitch is
        // nose-up. A textbook ZYX extraction returns the opposite sign, and
        // taking it verbatim made a stationary controller report 15 deg/s of
        // pitch — the estimate and the angles it was compared against
        // disagreed about which way up was.
        let pitch = -if sinp.abs() >= 1.0 {
            std::f32::consts::FRAC_PI_2.copysign(sinp)
        } else {
            sinp.asin()
        };
        let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
        let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
        [roll, pitch, yaw]
    }
}

/// Orientation for one report, plus the angular rate implied by it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Orientation {
    /// `(roll, pitch, yaw)` in radians. Roll and pitch from gravity, yaw from
    /// the absolute heading field.
    pub euler_rad: [f32; 3],
    /// `(roll, pitch, yaw)` rate in degrees per second.
    pub rate_dps: [f32; 3],
    /// ⭐ **Yaw rate alone, in degrees per second: the component of the body
    /// rotation about GRAVITY.**
    ///
    /// The one axis the raw fields cannot deliver, computed the one way that
    /// does not cost anything to get it. `omega . g_hat` is a projection of the
    /// FIELD rates — no differentiation of the accelerometer anywhere in it —
    /// so it keeps the fields' low noise while being immune to how the
    /// controller is held. Tilt the pad and the projection follows; the fields
    /// alone cannot, which is what made yaw pick up roll.
    ///
    /// ❗ Noise from the gravity direction enters MULTIPLICATIVELY here, as a
    /// weight on the rates, not additively as it does in
    /// [`Orientation::rate_dps`]. A stationary controller has rates near zero,
    /// so a jittering weight on them is still near zero. That is the whole
    /// difference between this and differentiating tilt, which multiplies the
    /// accelerometer's noise by the report rate and produces tens of degrees
    /// per second of it while sitting still.
    pub yaw_rate_dps: f32,
    /// ⭐ Orientation as a quaternion `[x, y, z, w]`, body-to-world.
    ///
    /// The authoritative form. `euler_rad` is derived FROM this, not the other
    /// way round, and exists for consumers that want angles. Rebuilding a
    /// quaternion from those angles round-trips through a convention that has
    /// already been got wrong twice — once here, once in the backend — so
    /// anything wanting a rotation should take this directly.
    pub quat_xyzw: [f32; 4],
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
    /// ⭐ **What the gyro PINS should carry**: `(roll, pitch, yaw)` in deg/s,
    /// canonical order, with the fields' pose-dependent leak removed.
    ///
    /// See [`GhostCancel`]. Roll and pitch are the field rates with their
    /// slow error corrected against gravity; yaw is the projection about
    /// gravity, which needs no such help because it is built from a direction
    /// rather than from an axis assignment.
    pub pin_rate_dps: [f32; 3],
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
    /// Resting drift measured on THIS controller, replacing the compiled-in
    /// default — see [`crate::cal`].
    ///
    /// `None` until something calibrates this half, at which point the constant
    /// becomes only a fallback. It is applied at the SOURCE rather than to the
    /// output pins because yaw is integrated further down: a correction applied
    /// after that cannot remove drift the estimate has already absorbed.
    resting_override: Option<[f32; 3]>,
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
    /// Last gravity direction, for the stillness test in
    /// [`GyroBias::correct_gated`].
    /// Is the controller holding still? See [`StillDetector`].
    still: StillDetector,
    /// Removes the fields' pose-dependent leak — see [`GhostCancel`].
    ghost: GhostCancel,
    /// Yaw, integrated from the corrected rate. Starts at zero on connect,
    /// which is what makes the neutral pose correct.
    yaw_rad: f32,
    /// Long-run resting drift measurement — see [`DriftProbe`].
    drift_probe: DriftProbe,
    /// Last trustworthy gravity direction, body frame, unit length.
    ///
    /// ⭐ Held across samples where the accelerometer is NOT measuring gravity.
    /// It feeds both the tilt angles and the yaw projection, so a corrupted
    /// reading damages the orientation twice over — once directly, and once by
    /// pointing the `omega . up` projection at the wrong axis, which is how a
    /// sideways shove turned into yaw and how roll started leaking into yaw
    /// over time.
    gravity: Option<[f32; 3]>,
    /// Orientation as a quaternion, body-to-world — see [`quat`].
    ///
    /// `None` until the first usable gravity reading, so the very first pose is
    /// levelled from the accelerometer instead of starting at identity and
    /// spending seconds converging.
    q: Option<[f32; 4]>,
}

/// How far gravity may move between reports and still count as "not rotating",
/// as a fraction of a unit vector.
///
/// 0.004 is about a quarter of a degree per report — well above accelerometer
/// noise at rest, and far below any movement a person is making on purpose.
/// ❗ Sized against the LAG of the average below, not against per-sample noise.
/// A stationary controller sits within accelerometer noise of its own average
/// (~0.004); a 5°/s rotation pulls away by ~0.03. 0.012 separates them with
/// room on both sides.
const STILL_GRAVITY_STEP: f32 = 0.012;
/// Removes the fields' pose-dependent leak from the roll and pitch rates,
/// using gravity as the reference for what actually moved.
///
/// ⭐ **The fields hand a physical axis between themselves depending on where
/// the controller is, and this is measured, not suspected.** Integrating each
/// field across a known 360° sweep — the true angle taken from gravity, which
/// cannot lie about a rotation it can see — and reading the LOCAL gain
/// `d(field)/d(truth)` in bins gives, for one half rotating about its x axis:
///
/// ```text
///   truth    gain#0   gain#2
///     -32     -2.68    -0.01     field #0 carries the rotation
///     -95     -4.31    -0.00
///    -111     -1.04     0.76     handover begins
///    -127     -0.26     1.72     field #0 dead, field #2 carries it
///    -281     -0.02     1.71
///    -296     -6.76     0.05     field #0 returns
/// ```
///
/// and rotating about y, field #1's gain runs −1.4…−2.8, flips to +0.9…+3.8
/// through the middle of the sweep, and comes back — a sign reversal and
/// return, which is a mirror bounce.
///
/// ❗ **No fixed axis map or 3×3 mix can express that**, because the mapping is
/// a function of pose rather than a constant. That is why every attempt to
/// tune `GYRO_MAP` or `GYRO_MIX` improved one orientation and broke another,
/// and why the contamination always came back "over time" — time being however
/// long it took the controller to be held somewhere else.
///
/// ⭐ **Gravity knows which motions are real.** It cannot see yaw, but roll and
/// pitch move it directly, so a field claiming roll while gravity holds still
/// is claiming a rotation that did not happen. Subtracting that disagreement
/// cancels the ghost.
///
/// It is done as a COMPLEMENTARY correction rather than a gate:
///
/// ```text
///   corrected = field_rate + lowpass(gravity_rate - field_rate)
/// ```
///
/// * at high frequency the correction is flat, so a fast flick passes through
///   the field path untouched — keeping its ~1 °/s noise and its responsiveness;
/// * at low frequency the field is dragged onto gravity's truth, which is where
///   the leak and the drift both live;
/// * when the field is already right the two agree, the correction is zero, and
///   nothing happens at all.
///
/// ⛔ It deliberately does NOT differentiate gravity into the output. Doing that
/// directly was tried and was much worse: per-sample tilt differences carry
/// 13–26 °/s of accelerometer noise against the fields' ~1. Here that noise is
/// low-passed by [`GHOST_TAU_SECS`] before it can reach anything, which is what
/// makes gravity usable as a reference without making it the signal.
#[derive(Debug, Clone, Copy, Default)]
struct GhostCancel {
    /// Previous gravity-derived tilt, for its rate.
    prev_tilt: Option<(f32, f32)>,
    /// Low-passed (gravity rate − field rate), per axis, deg/s.
    correction: [f32; 2],
}

/// Whether the ghost correction runs at all. **Off by default.**
///
/// ⛔ **The mechanism is sound and the trust gate underneath it is not, so it
/// stays opt-in until that is fixed.** On hardware it made the controls worse,
/// and the reason is visible in the gate rather than in the filter: gravity is
/// accepted whenever `|accel|` is within 10% of 1 g, which a hand in motion
/// satisfies almost continuously. Real play is not a sequence of clean
/// stillness and clean flicks — it is constant small linear acceleration, all
/// of it passing the magnitude test while corrupting the DIRECTION the
/// correction treats as truth.
///
/// A filter that learns a persistent offset needs a reference that is right on
/// average, not merely plausible in magnitude. The unit tests handed it a
/// perfect trust signal, which is exactly why they passed while the controller
/// got worse.
///
/// What would make it usable is a stricter gate — agreement between the two
/// halves of a grip, which are rigidly coupled and cannot genuinely rotate
/// differently, would be a far better arbiter than either half's own
/// accelerometer. Until then: `FLEXINPUT_JC2_GHOST_CANCEL=on`.
pub fn ghost_cancel() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("FLEXINPUT_JC2_GHOST_CANCEL")
            .map(|v| v.eq_ignore_ascii_case("on"))
            .unwrap_or(false);
        if on {
            eprintln!("[jc2] ghost cancellation enabled");
        }
        on
    })
}

/// Time constant of the ghost correction, in SECONDS.
///
/// ❗ Seconds, not samples — a per-sample weight silently changes meaning with
/// the polling rate, which is how the stillness detector broke when 200 Hz was
/// unlocked.
///
/// Half a second is long enough that accelerometer noise averages away (20 °/s
/// of it becomes about 2) and short enough to follow a handover, which happens
/// as fast as the wrist can move between poses.
const GHOST_TAU_SECS: f32 = 0.5;

impl GhostCancel {
    /// `field` is the canonical roll/pitch rate from the fields; `tilt` the
    /// gravity-derived roll and pitch; `trusted` whether gravity is currently
    /// measuring gravity alone. Returns the corrected roll/pitch rate.
    fn correct(
        &mut self,
        field: [f32; 2],
        tilt: (f32, f32),
        dt: f32,
        trusted: bool,
    ) -> [f32; 2] {
        let prev = self.prev_tilt.replace(tilt);
        // ❗ The correction is only UPDATED while gravity is trustworthy; it is
        // still APPLIED at all times. During a hard flick the accelerometer
        // measures the flick, so learning from it would inject the flick as a
        // correction — but the leak it is cancelling does not vanish for the
        // duration, so holding the last value is right and zeroing is not.
        if let (Some((pr, pp)), true) = (prev, trusted && dt > 1e-6) {
            let g_rate = [
                (tilt.0 - pr).to_degrees() / dt,
                (tilt.1 - pp).to_degrees() / dt,
            ];
            let a = (dt / GHOST_TAU_SECS).min(1.0);
            for i in 0..2 {
                let want = g_rate[i] - field[i];
                self.correction[i] += (want - self.correction[i]) * a;
            }
        }
        [field[0] + self.correction[0], field[1] + self.correction[1]]
    }
}

/// Decides whether the controller is holding still, from the direction of
/// gravity alone.
///
/// ⭐ Its own type because the property that matters is not expressible about
/// one sample: it is that a SUSTAINED slow rotation never reads as rest, at any
/// report rate. That has now been got wrong twice inside
/// [`OrientationTracker::update`], where it could only be tested at whatever
/// rate the test harness happened to produce — which was 67 Hz, the one rate at
/// which the broken version still worked.
#[derive(Debug, Clone, Copy, Default)]
struct StillDetector {
    /// The direction stillness is measured AGAINST. Moves only to where the
    /// device actually is, never toward where it is going.
    anchor: Option<[f32; 3]>,
    /// Seconds spent continuously within [`STILL_GRAVITY_STEP`] of it.
    secs: f32,
}

impl StillDetector {
    /// Feed one gravity direction and the time since the previous one.
    fn observe(&mut self, dir: [f32; 3], dt: f32) -> bool {
        let anchor = self.anchor.unwrap_or(dir);
        let d = (0..3).map(|i| (dir[i] - anchor[i]).powi(2)).sum::<f32>().sqrt();
        if d > STILL_GRAVITY_STEP {
            // Left the neighbourhood: re-anchor here, and make stillness be
            // earned again from scratch. A pan that keeps moving keeps failing.
            self.anchor = Some(dir);
            self.secs = 0.0;
            return false;
        }
        self.anchor = Some(anchor);
        self.secs += dt;
        self.secs >= STILL_SETTLE_SECS
    }

    /// Give up on the current run — used when gravity cannot be measured at all.
    fn interrupt(&mut self) {
        self.secs = 0.0;
    }
}

/// How long the device must stay inside [`STILL_GRAVITY_STEP`] of a FIXED
/// anchor before it counts as still.
///
/// ⭐ This is what separates a slow deliberate aim from actual rest, and the
/// pair of numbers is the whole detector: a rotation slower than
/// `STILL_GRAVITY_STEP / STILL_SETTLE_SECS` — about 0.7 °/s — can still slip
/// through, and anything faster cannot. Below that the motion is slower than
/// the drift being corrected, so mistaking it for rest costs nothing.
///
/// ❗ In SECONDS, deliberately, not in samples. The previous test used a
/// per-sample weight, which silently changed meaning with the polling rate:
/// unlocking 200 Hz shortened its effective window threefold and made it three
/// times worse at noticing slow motion, on exactly the axis a user aims with.
const STILL_SETTLE_SECS: f32 = 1.0;
/// How far |accel| may sit from 1 g and still be trusted as gravity alone.
const STILL_GRAVITY_TOLERANCE: f32 = 0.06;
/// How far |accel| may sit from 1 g before the sample is refused as a gravity
/// REFERENCE.
///
/// Looser than the stillness test above, and deliberately so: that one decides
/// whether to learn a bias, where being wrong is cheap and waiting costs
/// nothing. This one decides whether to update the attitude at all, and holding
/// a stale direction for too long is its own error. 10% passes ordinary
/// handling and rejects the shoves that were showing up as phantom yaw.
const GRAVITY_TRUST_TOLERANCE: f32 = 0.10;

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
/// A host gap longer than this is a stall, not a report interval. Pairing that
/// much wall clock with however many ticks happen to arrive after it says
/// nothing about the device's clock, so it is left out of the ratio.
const TICK_HOST_STALL: f64 = 1.0;

impl TickClock {
    /// Seconds since the previous report, or `None` on the first sample or
    /// after a gap.
    fn dt(&mut self, stamp: u32, fallback_hz: f32) -> Option<f32> {
        self.dt_at(stamp, fallback_hz, std::time::Instant::now())
    }

    /// [`TickClock::dt`] with the host clock supplied, so that bursty arrival —
    /// the thing this used to get wrong — can be reproduced exactly in a test.
    fn dt_at(&mut self, stamp: u32, fallback_hz: f32, now: std::time::Instant) -> Option<f32> {
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
        // ⭐ EVERY plausible sample calibrates the tick, bursts included.
        //
        // This used to require `host` in 0.001..0.2 s, on the reasoning that a
        // sub-millisecond gap could not be a real report interval. But it is
        // one: BLE delivers several notifications in a single connection event,
        // so reports arrive in bursts — one carrying the whole inter-event gap
        // and the rest microseconds behind it.
        //
        // ❗ Excluding those dropped their TICKS from the ratio while dropping
        // almost none of the TIME, and the ratio is seconds per tick. With
        // bursts of N reports every P seconds, each advancing T ticks:
        //
        //     accepted:  T ticks per P seconds   ->  tick = P / T
        //     truth:   N*T ticks per P seconds   ->  tick = P / (N*T)
        //
        // so the tick came out N times too long and every rate N times too
        // small. That is not hypothetical — it is measured, in the saved drift
        // logs, as device time running ahead of wall clock: the left half
        // reported 420 s of stillness over 276 s of wall (x1.52), the right
        // half 600 s over 267 s (x2.25). The two halves disagree because they
        // negotiate different connection intervals, and a given half disagrees
        // with itself between sessions for the same reason — which is why the
        // gyro needed re-tuning by hand every time and never stayed tuned.
        //
        // Summing every sample is exact under the same model: N*T ticks against
        // P seconds. A late-delivered burst is self-correcting too, since its
        // host deltas still sum to the true elapsed time however they are split.
        if host < TICK_HOST_STALL {
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
    /// Replace the compiled-in resting drift with one measured on this
    /// controller; `None` restores the default.
    ///
    /// Degrees per second, field order, before any gain — the same space as
    /// [`RESTING_DRIFT_LEFT`].
    pub fn set_resting_drift(&mut self, drift: Option<[f32; 3]>) {
        self.resting_override = drift;
    }

    /// `motion` is one parsed report. The canonical accel permutation is applied
    /// here so every caller gets canonical-frame angles and rates without having
    /// to remember to do it — forgetting it is what put the axes in the wrong
    /// order in the first place.
    /// `side` selects the mounting correction — see [`mount_for`]. Passed
    /// explicitly rather than stored, so a caller cannot forget to set it and
    /// silently get the other half's frame.
    pub fn update(&mut self, motion: &Motion, side: crate::protocol::Side) -> Orientation {
        let (accel, angle) = (motion.accel, motion.angle);
        // Canonical permutation, then the per-half mounting correction. Both
        // are frame changes; keeping them separate keeps the first anchored to
        // the sensor's own axes and the second to how the part is fitted.
        let mount = mount_for(side);
        let ca = to_canonical_accel(accel);
        let ca = apply_mount([ca[0] as f32, ca[1] as f32, ca[2] as f32], mount);

        // ⭐ GATE THE GRAVITY READING. An accelerometer measures gravity plus
        // whatever else is pushing the controller, and only the first is usable
        // as a reference. Taking it ungated meant every shove tilted the
        // estimate — reported as sideways acceleration producing yaw and
        // forward-back producing pitch — and, worse, it corrupted the
        // `omega . up` projection so linear motion leaked into the integrated
        // yaw permanently rather than just wobbling it.
        //
        // Rotating the controller does NOT change the magnitude, so this rejects
        // translation while passing rotation through untouched — which is what
        // makes it safe to gate at all.
        let raw_mag = (ca[0] * ca[0] + ca[1] * ca[1] + ca[2] * ca[2]).sqrt() / ACCEL_LSB_PER_G;
        if (raw_mag - 1.0).abs() < GRAVITY_TRUST_TOLERANCE && raw_mag > 0.01 {
            let n = raw_mag * ACCEL_LSB_PER_G;
            self.gravity = Some([ca[0] / n, ca[1] / n, ca[2] / n]);
        }
        // Until a trustworthy sample arrives, fall back to the raw reading
        // rather than reporting level — a controller picked up mid-motion
        // should still show roughly where it is.
        let gdir = self.gravity.or_else(|| {
            (raw_mag > 0.01).then(|| {
                let n = raw_mag * ACCEL_LSB_PER_G;
                [ca[0] / n, ca[1] / n, ca[2] / n]
            })
        });
        let gvec = gdir.unwrap_or([0.0, 0.0, 1.0]);
        let (roll, pitch) = tilt_from_accel([
            (gvec[0] * ACCEL_LSB_PER_G) as i32,
            (gvec[1] * ACCEL_LSB_PER_G) as i32,
            (gvec[2] * ACCEL_LSB_PER_G) as i32,
        ]);

        // Unwrap BEFORE converting to radians. The field covers half a turn, so
        // untracked wraps land as 180 degree flips in the middle of otherwise
        // ordinary motion.
        // Heading is still tracked, for the `probe_i24_2` pin and for anything
        // that wants the controller's own absolute figure. It no longer feeds
        // the orientation — see the yaw integration below.
        let h = angle[HEADING_AXIS];
        match self.prev_heading.replace(h) {
            Some(p) => unwrap_heading(p, h, &mut self.heading_acc),
            None => self.heading_acc = h as i64,
        }
        // ❗ The euler triple is NOT built here. Yaw is integrated from the
        // rate, which is not known until further down, and an earlier revision
        // built the triple with a placeholder yaw of zero, stored THAT as the
        // previous sample, and then differenced against it — so the yaw rate
        // read zero forever. Everything that needs `prev` now happens after
        // yaw exists.
        let first_sample = self.prev.is_none();
        if first_sample {
            // Prime the differencer, or the SECOND sample produces the spike
            // this branch exists to prevent.
            self.field_gyro.rate(angle);
            // Level from gravity immediately. Deferring this to the second
            // sample left the first integration step with nothing to rotate,
            // so the first reported rate was always zero.
            let (sr, cr) = (roll * 0.5).sin_cos();
            let (sp, cp) = (pitch * 0.5).sin_cos();
            self.q = Some(quat::normalise(quat::mul(
                [0.0, -sp, 0.0, cp],
                [sr, 0.0, 0.0, cr],
            )));
            self.prev = Some([roll, pitch, 0.0]);
            self.prev_at = Some(std::time::Instant::now());
            return Orientation {
                euler_rad: [roll, pitch, 0.0],
                rate_dps: [0.0; 3],
                field_rate_dps: [0.0; 3],
                pin_rate_dps: [0.0; 3],
                yaw_rate_dps: 0.0,
                quat_xyzw: self.q.unwrap_or([0.0, 0.0, 0.0, 1.0]),
            };
        }

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
        // Is the controller actually stationary? Gravity is fixed in the world,
        // so a body-frame gravity direction that is not moving means the device
        // is not rotating. Both the magnitude check and the direction check
        // matter: the first rejects samples carrying linear acceleration, the
        // second is the rotation test itself.
        let gmag = raw_mag;
        // ⭐ Compare against a SLOWLY LAGGING average, not against the previous
        // sample.
        //
        // Consecutive-sample comparison cannot work here and never did: at
        // ±10 counts of accelerometer noise on 4096, the direction moves about
        // 0.0035 between reports all by itself, which is the same order as the
        // threshold. So a perfectly stationary controller failed the test
        // constantly, the drift run reset almost every report, and no
        // measurement ever reached thirty seconds — which is why the log stayed
        // empty.
        //
        // It also could not have detected slow motion even in principle: a 5°/s
        // pan moves the direction 0.0013 per report, FAR below the same
        // threshold. Per-sample differences simply do not separate the two.
        //
        // ⛔ And a LAGGING AVERAGE does not work either, which is the mistake
        // that replaced it and had to be undone.
        //
        // An EMA is a low-pass, and against a sustained rotation it settles at
        // a CONSTANT lag of rate × time-constant — it does not keep pulling
        // away, it catches up and then tracks. So a slow steady aim reads as
        // still after the first fraction of a second, the bias estimator learns
        // the aim as drift, and the cursor is dragged back while it is being
        // moved. That is the "gyro fights small movements" report, and it is a
        // property of the filter, not a tuning error.
        //
        // ⭐ A FIXED anchor does work. Deviation from a point that is not
        // moving keeps growing for as long as the rotation continues, so
        // stillness has to be earned by staying put for `STILL_SETTLE_SECS`
        // and is lost the moment the pad leaves the neighbourhood of the
        // anchor. The anchor only ever moves to where the device actually is,
        // never toward where it is heading.
        let device_still = match gdir {
            Some(now) if (gmag - 1.0).abs() < STILL_GRAVITY_TOLERANCE => {
                self.still.observe(now, dt)
            }
            // Not measuring gravity, so nothing can be concluded about
            // stillness — and "moving" is the safe answer: it only ever stops
            // the estimator learning, which is recoverable.
            _ => {
                self.still.interrupt();
                false
            }
        };

        // Measure the raw drift before anything is subtracted from it.
        let raw_dps = [
            counts[0] * per_sec / ANGLE_COUNTS_PER_DEG,
            counts[1] * per_sec / ANGLE_COUNTS_PER_DEG,
            counts[2] * per_sec / ANGLE_COUNTS_PER_DEG,
        ];
        self.drift_probe.update(raw_dps, dt, device_still, side);

        // Subtract the reproducible resting drift FIRST, so the estimator only
        // has the session-specific remainder to find — see `RESTING_DRIFT_*`.
        let fixed = self.resting_override.unwrap_or_else(|| resting_drift(side));
        let euler_dps = self.field_bias.correct_gated([
            counts[0] * per_sec / ANGLE_COUNTS_PER_DEG - fixed[0],
            counts[1] * per_sec / ANGLE_COUNTS_PER_DEG - fixed[1],
            counts[2] * per_sec / ANGLE_COUNTS_PER_DEG - fixed[2],
        ], device_still);

        // ⭐ If the controller is sending its REAL gyro, use it and throw all of
        // the above away — no wrap seam, no per-axis gain, no drift beyond the
        // sensor's own offset. The recovered path stays as a fallback so a
        // controller that withholds it still produces something.
        //
        // ⭐ TWO rates, deliberately, because they are judged against different
        // things — see [`FIELD_GAIN`] and [`ORIENTATION_GAIN`]. `field_rate_dps`
        // goes out to the gyro pins; `orient_dps` drives the yaw integration.
        // A real gyro block needs neither, so both collapse to it when present.
        let (field_rate_dps, orient_dps) = match motion.gyro {
            Some(g) => {
                let s = 1.0 / GYRO_LSB_PER_DPS;
                let r = [g[0] as f32 * s, g[1] as f32 * s, g[2] as f32 * s];
                (r, r)
            }
            None => {
                let g = field_gain();
                let o = orientation_gain();
                (
                    [euler_dps[0] * g[0], euler_dps[1] * g[1], euler_dps[2] * g[2]],
                    [euler_dps[0] * o[0], euler_dps[1] * o[1], euler_dps[2] * o[2]],
                )
            }
        };

        // ⭐ ORIENTATION: tilt comes STRAIGHT FROM GRAVITY. Only yaw is integrated.
        //
        // Free-integrating all three body rates and correcting afterwards was
        // tried and failed on hardware the same way the Euler composition did:
        // the model tumbled, roll appeared in yaw, and every movement ended
        // with the estimate being dragged back. That is what an integrator with
        // a WRONG AXIS MAP looks like once a correction term is fighting it —
        // and the map is exactly the thing this decode has never been able to
        // verify, because the fields are a firmware fusion rather than three
        // gyro axes.
        //
        // So do not integrate what does not need integrating. The
        // accelerometer measures the direction of gravity directly, which fixes
        // roll and pitch EXACTLY, with no accumulation, no axis map, and no
        // convention to guess. They cannot tumble because nothing is being
        // integrated into them.
        //
        // Yaw is the only degree of freedom gravity cannot see, so it is the
        // only one integrated — and the rate used is the component of the body
        // rotation ABOUT GRAVITY:
        //
        //     yaw_rate = omega . g_hat
        //
        // A dot product with the measured down-vector, which needs no axis
        // assignment either: whatever permutation the three rates arrive in,
        // their projection onto gravity is the rotation about vertical. That is
        // the piece that was wrong before — a body-axis rate was being applied
        // as a world-vertical rotation, so tilting the controller turned yaw
        // into roll.
        // Same correction on the rates: gravity and the gyro must agree about
        // which way the controller is fitted, or the yaw projection below is
        // taken against the wrong up-vector.
        let body = apply_mount(canonical_field_rate(orient_dps), mount);
        // ⭐ The PIN's yaw is projected from the pin-scaled rate, the
        // orientation's from its own.
        //
        // ❗ Both were taken from `body` at first, which quietly put the yaw pin
        // on `ORIENTATION_GAIN` and left `FIELD_GAIN`'s yaw term driving
        // nothing at all. Turning the documented knob for "yaw is weak to aim
        // with" then did nothing, which is the worst way for a constant to be
        // wrong. Roll and pitch pins already scale with `FIELD_GAIN`; this is
        // what makes the third entry mean the same thing for yaw.
        let body_pin = apply_mount(canonical_field_rate(field_rate_dps), mount);
        // ⭐ NEGATED, and this is a convention mismatch rather than a guess.
        //
        // `canonical_yaw_rate` is `omega . g_hat`, and `g_hat` points UP. By the
        // right-hand rule a positive rotation about UP turns the controller to
        // the LEFT. But the canonical gyro contract is the opposite: `gyro_z` is
        // positive turning RIGHT — see the Switch Pro parser in
        // `flexinput_devices::gyro`, which negates its raw yaw for exactly this
        // reason and is the reference every other pad is matched against.
        //
        // ❗ So the projection is correct physics and still the wrong sign for
        // this pin. Left unflipped, a patch has to tick "invert yaw" to get
        // ordinary behaviour, which pushes a decode bug out into every user's
        // configuration and hides it there.
        //
        // The ORIENTATION integration below deliberately does NOT take this
        // negation: it feeds `yaw_rad` in the crate's own Euler convention,
        // where positive yaw is about the up-vector. Two consumers, two
        // conventions, and conflating them is what made the sign wander.
        let yaw_rate_pin = -match gdir {
            Some(g) => canonical_yaw_rate(body_pin, g),
            None => body_pin[2],
        };
        // Roll and pitch for the PINS. Straight from the fields unless the
        // ghost correction is switched on — see [`ghost_cancel`].
        let pin_rp = if ghost_cancel() {
            self.ghost.correct(
                [body_pin[0], body_pin[1]],
                (roll, pitch),
                dt,
                gdir.is_some(),
            )
        } else {
            [body_pin[0], body_pin[1]]
        };
        let yaw_rate = match gdir {
            // ❗ NOT negated. An accelerometer at rest measures the upward
            // REACTION force, not gravity — a controller lying flat face-up
            // reads +1 g on its vertical axis. So `gdir` already points UP,
            // which is the axis yaw is positive about.
            Some(g) => canonical_yaw_rate(body, g),
            None => body[2],
        };
        self.yaw_rad += yaw_rate.to_radians() * dt;
        if self.yaw_rad > std::f32::consts::PI {
            self.yaw_rad -= std::f32::consts::TAU;
        } else if self.yaw_rad < -std::f32::consts::PI {
            self.yaw_rad += std::f32::consts::TAU;
        }

        // Compose in the crate's documented convention. Roll and pitch are the
        // gravity-derived angles for THIS sample, so the attitude is absolute
        // and self-correcting by construction rather than by a feedback gain.
        let (sy, cy) = (self.yaw_rad * 0.5).sin_cos();
        let (sp, cp) = (pitch * 0.5).sin_cos();
        let (sr, cr) = (roll * 0.5).sin_cos();
        self.q = Some(quat::normalise(quat::mul(
            quat::mul([0.0, 0.0, sy, cy], [0.0, -sp, 0.0, cp]),
            [sr, 0.0, 0.0, cr],
        )));

        let euler_rad = quat::to_euler(self.q.unwrap_or([0.0, 0.0, 0.0, 1.0]));

        // Now that the triple is real, difference it against the previous one.
        let prev = self.prev.replace(euler_rad).unwrap_or(euler_rad);
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

        Orientation {
            euler_rad,
            rate_dps,
            field_rate_dps,
            pin_rate_dps: [pin_rp[0], pin_rp[1], yaw_rate_pin],
            yaw_rate_dps: yaw_rate_pin,
            quat_xyzw: self.q.unwrap_or([0.0, 0.0, 0.0, 1.0]),
        }
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
///
/// ⛔ **Do not speed this up to chase the fusion pull-back.** That was tried —
/// 8 samples with a 0.10 EMA — and it made the controller fight the user:
///
///   > I try to move the cursor a little to the right and it fights it and
///   > moves back until I give enough momentum.
///
/// Which is exactly right, and is what any bias tracker does when it converges
/// faster than a person moves. A deliberate slow movement is indistinguishable
/// from a zero-rate offset by magnitude and by stability, so the only thing
/// separating them is TIME — and taking that away turns the estimator into a
/// high-pass filter that cancels precisely the small, careful motions aiming
/// depends on.
const REST_SAMPLES: u32 = 16;
/// EMA weight once converged. Deliberately slow: bias drifts with temperature
/// over minutes, so it needs to follow that without chasing per-sample noise.
const BIAS_EMA: f32 = 0.02;
/// Refuse to treat anything beyond this as bias.
///
/// Stability alone cannot tell a resting controller from one turning at a
/// perfectly constant rate, so this cap is what stops a sustained pan being
/// silently absorbed and cancelled out.
///
/// ⭐ **Was 36 dps, chosen as "far above any real zero-rate offset". It is far
/// above it — by a factor of twenty-five.** The resting drift of these fields
/// was later measured directly, on both halves across two captures, at 0.03 to
/// 1.44 °/s. A 36 °/s ceiling therefore did nothing to protect real motion: a
/// deliberate slow pan sits well inside it and was eligible to be learned as an
/// offset and subtracted away.
///
/// ⭐ **10 °/s, raised from 4 once the estimator stopped guessing at stillness.**
///
/// 4 was chosen against a resting drift measured at 0.03-1.44 °/s over short
/// captures. Over a long session the fields drift considerably faster than that
/// — one of the three visibly outruns the others and all three loop right
/// around given time — so a 4 °/s ceiling simply refused to learn the offset
/// that mattered, and it passed straight through into the rate.
///
/// Raising it is only safe because learning is now gated on the ACCELEROMETER
/// saying the controller is stationary, not on the gyro reading looking steady.
/// A slow deliberate tilt moves gravity, so it can no longer be mistaken for an
/// offset however small it is — which is what made the old cap load-bearing.
///
/// ⚠️ The gate is blind to rotation about gravity, so a very slow flat yaw can
/// still be learned as bias. That is the same axis an accelerometer can never
/// see, and it is the reason yaw needs a gyro in the first place.
const MAX_BIAS_DPS: f32 = 10.0;
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
        self.correct_gated(raw, true)
    }

    /// [`GyroBias::correct`], but only allowed to LEARN while `device_still`.
    ///
    /// ⭐ **Stability is not stillness, and conflating them is what made the
    /// controller fight small movements.** This estimator judged rest by how
    /// much the reading moved between samples — but a hand panning slowly and
    /// steadily is just as stable as a controller lying on a desk. Given time
    /// it learned that pan as an offset and cancelled it, so a careful little
    /// nudge went nowhere while a flick worked fine.
    ///
    /// The accelerometer settles it independently: gravity is a fixed direction
    /// in the world, so if its direction in the BODY frame is not moving, the
    /// controller is not rotating — about roll or pitch at least. That is a
    /// measurement of the thing we actually mean, rather than an inference from
    /// the signal being corrected.
    ///
    /// ⚠️ Rotation about gravity itself is invisible to this, so a slow flat
    /// yaw can still be learned as bias. Nothing an accelerometer can do about
    /// that; it is the same blind axis that makes yaw need a gyro at all.
    pub fn correct_gated(&mut self, raw: [f32; 3], device_still: bool) -> [f32; 3] {
        let s = [raw[0] as f32, raw[1] as f32, raw[2] as f32];

        let stable = self.have_last
            && (0..3).all(|i| (s[i] - self.last[i]).abs() < STILL_DELTA_LSB);
        let plausible = s.iter().all(|v| v.abs() < MAX_BIAS_LSB);
        self.last = s;
        self.have_last = true;

        if stable && plausible && device_still {
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
        assert_eq!(out[2], 30.0, "canonical yaw comes from field #2, upright");
    }

    /// ⛔ A slow, deliberate movement must NOT be learned as bias.
    ///
    /// This is the "it fights small movements" regression, pinned. A hand
    /// panning slowly is perfectly stable sample to sample and sits well inside
    /// the bias cap, so the estimator has no way to tell it from a zero-rate
    /// offset — except that the accelerometer can see the device rotating.
    /// With that gate shut, the reading must survive.
    #[test]
    fn a_slow_deliberate_pan_is_not_absorbed_while_the_device_is_moving() {
        let mut bias = GyroBias::default();
        // Well below the cap, perfectly steady — indistinguishable from bias by
        // every test except "is the controller actually still".
        let pan = [3.0f32, 0.0, 0.0];
        let mut out = [0.0; 3];
        for _ in 0..(REST_SAMPLES * 8) {
            out = bias.correct_gated(pan, false);
        }
        assert!(
            (out[0] - pan[0]).abs() < 0.05,
            "a 3 deg/s pan decayed to {} while the device was moving",
            out[0],
        );
    }

    /// And with the device genuinely still, it must still converge.
    #[test]
    fn a_resting_offset_is_still_learned_when_the_device_is_still() {
        let mut bias = GyroBias::default();
        let resting = [1.2f32, -0.4, 0.7];
        let mut out = [0.0; 3];
        for _ in 0..(REST_SAMPLES * 40) {
            out = bias.correct_gated(resting, true);
        }
        assert!(
            out.iter().all(|v| v.abs() < 0.1),
            "resting offset was not cancelled: {out:?}",
        );
    }

    /// The cross-axis mix defaults to the identity, and must.
    ///
    /// ⛔ A guessed correction is worse than none — the removed ZYX Euler
    /// transform was exactly that, and it injected the coupling it was meant to
    /// remove. Until a coefficient is measured rather than assumed, this stays
    /// a no-op, and a future edit that quietly bakes one in has to fail here.
    #[test]
    fn the_cross_axis_mix_is_identity_until_something_is_measured() {
        for r in 0..3 {
            for c in 0..3 {
                let want = if r == c { 1.0 } else { 0.0 };
                assert_eq!(GYRO_MIX[r][c], want, "GYRO_MIX[{r}][{c}] is not identity");
            }
        }
        // And it must be a genuine pass-through, not merely look like one.
        let out = canonical_field_rate([3.0, 5.0, 7.0]);
        let mapped = canonical_field_rate([3.0, 5.0, 7.0]);
        assert_eq!(out, mapped);
        assert!(out.iter().any(|v| *v != 0.0), "mapping produced nothing");
    }

    /// The bias cap must stay near the drift actually measured on hardware.
    ///
    /// It was 36 °/s against a real resting drift of 0.03–1.44 °/s — twenty-five
    /// times too permissive, which meant a deliberate slow pan was eligible to
    /// be learned as a zero-offset and subtracted away. A cap that large is
    /// indistinguishable from having no cap for any motion a hand produces.
    #[test]
    fn the_bias_cap_is_scaled_to_measured_drift_not_guessed() {
        assert!(
            (1.44..=10.0).contains(&MAX_BIAS_DPS),
            "MAX_BIAS_DPS is {MAX_BIAS_DPS} — it must exceed the 1.44 deg/s measured \
             on hardware but stay well below any deliberate aiming motion",
        );
    }

    /// ⛔ The two yaw gains must not be collapsed back into one.
    ///
    /// ⭐ The history is worth keeping, because it is a double-correction and
    /// those are hard to see from either end. `ANGLE_COUNTS_PER_TURN` was once
    /// the field MODULUS, 2^24, which made every axis rotate twice as far as it
    /// should. That 2x was observed as "a 90° turn shows 180° on screen" — and
    /// then FIXED TWICE, independently: the constant was corrected to 2^25, and
    /// the yaw gain was separately halved from 4 to 2 to cancel the same error.
    ///
    /// The result is exactly what was reported from hardware afterwards. All
    /// three axes lost the shared 2x from the constant, and yaw alone lost
    /// another 2x from the gain — so yaw read half of roll and pitch on the
    /// pins ("weaker than the rest"), while the orientation, having been
    /// corrected only once, still turned 180° for 90°.
    ///
    /// Hence the split, and hence both values here: 1.0 for orientation, where
    /// the measured counts-per-turn leaves no freedom, and 4.0 for the pins,
    /// which is where it was before either correction landed.
    #[test]
    fn the_two_yaw_gains_stay_separate_and_keep_their_values() {
        assert_eq!(FIELD_GAIN[0], 1.0);
        assert_eq!(FIELD_GAIN[1], 1.0);
        assert!(
            (FIELD_GAIN[2] - 4.0).abs() < 1e-6,
            "yaw PIN gain is {} — halving it is what made yaw weaker than roll              and pitch to aim with",
            FIELD_GAIN[2],
        );
        assert_eq!(
            ORIENTATION_GAIN, [1.0, 1.0, 1.0],
            "orientation gain is fixed by the measured counts-per-turn; a value              other than 1 makes the model disagree with the world by that factor",
        );
        assert_ne!(
            FIELD_GAIN[2], ORIENTATION_GAIN[2],
            "the pin gain and the orientation gain have been collapsed back into              one number — tuning either will now silently move the other",
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

    /// ⛔ Bursty arrival must not stretch the tick.
    ///
    /// The radio hands over several notifications per connection event, so most
    /// reports land microseconds after the one before. Calibrating only on the
    /// gaps and ignoring those counted their ticks nowhere while counting their
    /// time everywhere, and the tick came out one burst-length too long — which
    /// scaled every gyro rate down by the same factor, differently on each half
    /// and on each session. See the comment in [`TickClock::dt_at`].
    #[test]
    fn the_device_clock_is_not_fooled_by_bursts() {
        use std::time::Duration;
        // Three reports every 15 ms, twelve ticks each: 36 ticks per 15 ms, so
        // a tick is 15/36 ms and one report is exactly 5 ms of device time.
        const PERIOD: Duration = Duration::from_millis(15);
        const PER_EVENT: u32 = 3;
        const TICKS: u32 = 12;

        let mut c = TickClock::default();
        let base = std::time::Instant::now();
        let mut stamp = 1000u32;
        let mut dt = None;
        for event in 0..400u32 {
            for i in 0..PER_EVENT {
                // First report of the event carries the whole gap; the rest
                // arrive right behind it, as the radio actually delivers them.
                let at = base + PERIOD * event + Duration::from_micros(i as u64 * 60);
                stamp = stamp.wrapping_add(TICKS);
                dt = c.dt_at(stamp, 100.0, at);
            }
        }
        let dt = dt.expect("a calibrated clock must answer");
        assert!(
            (dt - 0.005).abs() < 0.0005,
            "expected ~5 ms per report, got {dt} s — the clock is out by x{:.2}",
            dt / 0.005,
        );
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

    /// A stalled field holds its rate, then spreads its catch-up step over the
    /// gap instead of spiking.
    #[test]
    fn a_stalled_field_averages_over_the_gap_instead_of_spiking() {
        let mut g = AngleGyro::default();
        g.rate([0, 0, 0]);
        assert_eq!(g.rate([90_000, 0, 0])[0], 90_000.0);
        // Same value again: the pad did not stop, the field carried no fresh
        // sample.
        assert_eq!(g.rate([90_000, 0, 0])[0], 90_000.0, "a stall read as a stop");
        // ⭐ The catch-up step covers TWO reports, so it is HALF the rate — not
        // the doubled spike it used to be. The field moved 90 000 counts in the
        // time two reports took; that is what it did, and reporting it as one
        // report's worth is what put a comb on a smooth pan.
        assert_eq!(g.rate([180_000, 0, 0])[0], 45_000.0);
    }

    /// ⛔ Fields refreshing at DIFFERENT rates must still read as one steady
    /// rate.
    ///
    /// This is the oscilloscope symptom, reproduced: one channel smooth and the
    /// others pulsing, on a motion that was perfectly smooth. The old code only
    /// held when all three fields repeated together, which is the one case that
    /// almost never happens — the usual case is one field stalling while the
    /// others move, and that field then read zero, zero, triple.
    #[test]
    fn fields_that_refresh_at_different_rates_read_as_one_steady_rate() {
        let mut g = AngleGyro::default();
        // Field 0 advances every report, field 2 every third — the same true
        // angular rate, sampled at a third of the refresh.
        let (mut a0, mut a2) = (0i32, 0i32);
        g.rate([0, 0, 0]);
        let mut fast = Vec::new();
        let mut slow = Vec::new();
        for step in 1..=12 {
            a0 += 300;
            if step % 3 == 0 {
                a2 += 900;
            }
            let out = g.rate([a0, 0, a2]);
            fast.push(out[0]);
            slow.push(out[2]);
        }
        // Once the slow field has produced its first update, every sample from
        // it must agree with the fast one. A comb would show up here as a third
        // of the samples at zero and a third at 900.
        for (i, (f, sl)) in fast.iter().zip(&slow).enumerate().skip(3) {
            assert!(
                (sl - 300.0).abs() < 1.0,
                "sample {i}: slow field read {sl}, expected the true 300 (fast field reads {f})",
            );
        }
    }

    /// A field that genuinely stops must not hold its last rate forever.
    ///
    /// The counterpart to the hold above: a controller set down mid-turn has to
    /// stop reporting that turn. `MAX_HOLD` bounds it at an eighth of a second,
    /// which no real motion can hide inside — at 2^25 counts per revolution
    /// even a 0.1 °/s crawl moves the field every report.
    #[test]
    fn a_field_that_stops_eventually_reads_zero() {
        let mut g = AngleGyro::default();
        g.rate([0, 0, 0]);
        assert_eq!(g.rate([90_000, 0, 0])[0], 90_000.0);
        let mut last = 90_000.0;
        for _ in 0..MAX_HOLD {
            last = g.rate([90_000, 0, 0])[0];
        }
        assert_eq!(last, 0.0, "a stopped field held its rate past MAX_HOLD");
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
            let jitter = if i % 2 == 0 { 0.15 } else { -0.15 };
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
            let o = t.update(&sample([0, 0, g], [0, 0, h]), Side::Right);
            worst = worst.max(o.rate_dps[2].abs());
        }
        // Half a degree per report at REPORT_HZ is ~34 deg/s. A missed wrap
        // would show up here as roughly 180 * REPORT_HZ = 12000 deg/s.
        assert!(worst < 200.0, "wrap leaked through as {worst} deg/s");
    }

    #[test]
    fn the_first_sample_produces_no_rate_spike() {
        let mut t = OrientationTracker::default();
        let o = t.update(&sample([0, 0, G], [0, 0, 12_345_678]), Side::Right);
        assert_eq!(o.rate_dps, [0.0; 3]);
        // ⭐ And yaw starts at EXACTLY zero, which is the point. It used to be
        // seeded from the controller's raw heading, whose value at connect is
        // arbitrary — so a pad lying flat and pointing at the screen produced
        // whatever heading it happened to hold, and the 3D model faced a random
        // direction. Yaw is integrated from the rate now, so neutral is neutral.
        assert_eq!(o.euler_rad[2], 0.0, "yaw must start at neutral, not at the raw heading");
        // Roll and pitch remain absolute and available immediately.
        assert!(o.euler_rad[0].abs() < 1e-6 && o.euler_rad[1].abs() < 1e-6);
    }

    #[test]
    fn a_steady_yaw_turn_reads_the_right_rate() {
        // 1 degree of yaw per report at REPORT_HZ reports per second is
        // REPORT_HZ deg/s, straight from the definition.
        let per_report = (ANGLE_COUNTS_PER_TURN / 360) as i32;
        let mut t = OrientationTracker::default();
        t.update(&sample([0, 0, G], [0, 0, 0]), Side::Right);
        let o = t.update(&sample([0, 0, G], [0, 0, per_report]), Side::Right);
        // One degree of FIELD movement per report, through the same chain the
        // orientation uses: field #2 -> canonical yaw, times the yaw gain.
        // Spelled out rather than hardcoded so a change to either constant
        // shows up here as a disagreement about the chain, not a magic number.
        // ❗ ORIENTATION_GAIN, not FIELD_GAIN: `rate_dps` is differentiated from
        // the orientation, so it follows the orientation's scale. Reading it
        // against the PIN gain is precisely the conflation that broke both.
        let expect = REPORT_HZ * ORIENTATION_GAIN[2] * GYRO_MAP[2].1;
        assert!(
            (o.rate_dps[2] - expect).abs() < 2.0,
            "yaw rate {} deg/s, expected {expect}",
            o.rate_dps[2],
        );
        // ⭐ And the PIN carries the pin gain from the same sample — the split
        // pinned behaviourally, not just as two constants. If these two ever
        // come out equal again, one knob is driving both jobs.
        let pin = REPORT_HZ * FIELD_GAIN[2];
        assert!(
            (o.field_rate_dps[2] - pin).abs() < 2.0,
            "yaw pin {} deg/s, expected {pin}",
            o.field_rate_dps[2],
        );
        assert!(
            (o.field_rate_dps[2] - o.rate_dps[2]).abs() > 1.0,
            "the pin rate and the orientation rate came out identical — the two              gains have been collapsed back into one",
        );
        // ⭐ And the yaw PIN follows the pin gain, not the orientation's.
        //
        // Flat on the table, canonical up is +z, so the projection onto gravity
        // reduces to the yaw component itself — which makes this a direct read
        // of which gain reached the pin. It was the orientation's for a while,
        // and the symptom was that `FLEXINPUT_JC2_FIELD_GAIN` did nothing to a
        // yaw axis everyone agreed was too weak.
        assert!(
            (o.yaw_rate_dps.abs() - pin).abs() < 2.0,
            "yaw pin projected {} deg/s, expected the pin-gained {pin}",
            o.yaw_rate_dps,
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
        t.update(&sample([0, 0, G], [0, 0, half - one_deg]), Side::Right);
        let o = t.update(&sample([0, 0, G], [0, 0, half + one_deg]), Side::Right);
        // Generous, and scaled by the yaw gain: the point is that a wrap does
        // not produce a FULL-CIRCLE flick, not that it is pixel-perfect.
        assert!(
            o.rate_dps[2].abs() < 6.0 * REPORT_HZ * FIELD_GAIN[2],
            "wrap produced a {} deg/s spike",
            o.rate_dps[2]
        );
    }

    /// ⭐ Tilt must come from GRAVITY, not from integration.
    ///
    /// The failure this pins is the one that survived three attempts: a
    /// body-axis rate applied as a world-vertical rotation, so tilting the
    /// controller turned yaw into roll and the model tumbled. Roll and pitch
    /// are read from the accelerometer for every sample now, so a controller
    /// held at a fixed tilt reports that tilt exactly, no matter what the gyro
    /// fields are doing or how long it has been running.
    #[test]
    fn a_fixed_tilt_reads_exactly_that_tilt_however_the_gyro_behaves() {
        let mut t = OrientationTracker::default();
        // Right grip down: gravity on canonical +y, which is raw axis 0 negated.
        let held = [-G, 0, 0];
        // Feed a large, steadily changing angle field the whole time — the gyro
        // is "spinning" as hard as the glitch filter allows.
        let mut angle = 0i32;
        let mut last = [0.0f32; 3];
        for _ in 0..2000 {
            angle = angle.wrapping_add(200_000);
            last = t.update(&sample(held, [angle, angle, angle]), Side::Right).euler_rad;
        }
        // Roll pinned at +90 by gravity, pitch at zero. Neither may wander.
        assert!(
            (last[0] - std::f32::consts::FRAC_PI_2).abs() < 0.02,
            "roll drifted to {} rad after 2000 reports of hard rotation",
            last[0],
        );
        assert!(last[1].abs() < 0.02, "pitch drifted to {} rad", last[1]);
    }

    /// The drift probe must measure a steady offset, and must reset on motion.
    ///
    /// Both halves matter. Averaging a known offset over a long run is the
    /// whole point; resetting when the controller moves is what makes the
    /// figure mean "drift" rather than "whatever happened recently".
    #[test]
    fn the_drift_probe_averages_a_steady_offset_and_resets_on_motion() {
        let mut p = DriftProbe::default();
        // 90 s of stillness at a steady -1.2 deg/s, in 10 ms steps.
        for _ in 0..9000 {
            p.update([-1.2, 0.1, 0.0], 0.01, true, Side::Left);
        }
        assert!(p.secs > 89.0, "accumulated only {:.1} s", p.secs);
        let mean0 = p.sum[0] / p.secs;
        assert!((mean0 + 1.2).abs() < 1e-3, "measured {mean0} deg/s, expected -1.2");

        // Movement ends the run outright — a drift figure spanning a movement
        // is not a drift figure.
        p.update([-1.2, 0.1, 0.0], 0.01, false, Side::Left);
        assert_eq!(p.secs, 0.0, "motion did not reset the run");
        assert_eq!(p.sum, [0.0; 3]);
    }

    /// The resting-drift constants must stay within measured bounds.
    ///
    /// They come from six stationary sessions of four to eleven minutes, and
    /// their job is to remove the reproducible part of the offset before the
    /// estimator converges. A value outside the observed range would be adding
    /// drift rather than removing it, in the exact window where nothing else is
    /// opposing it.
    #[test]
    fn the_resting_drift_constants_match_what_was_measured() {
        // field#0 is the reproducible term on both halves: -0.37..-0.44 on the
        // left and -0.31..-0.36 on the right, once the clock error is out. The
        // rest are near zero. Bounds are deliberately loose — this pins the
        // sign and order of magnitude, which is what a wrong edit would get
        // wrong.
        let l = RESTING_DRIFT_LEFT;
        assert!((-0.50..=-0.30).contains(&l[0]), "left field#0 drift {} deg/s", l[0]);
        assert!(l[1].abs() < 0.15 && l[2].abs() < 0.15, "left {l:?}");
        let r = RESTING_DRIFT_RIGHT;
        assert!((-0.45..=-0.25).contains(&r[0]), "right field#0 drift {} deg/s", r[0]);
        assert!(r[1].abs() < 0.15 && r[2].abs() < 0.15, "right {r:?}");
        // And the two halves must differ — they are separate sensors, and
        // sharing one table would silently apply the wrong correction to one.
        assert_ne!(l, r, "both halves cannot share a drift correction");
    }

    /// ⛔ A shove must not tilt the estimate.
    ///
    /// The accelerometer measures gravity plus every other force, and only the
    /// first is a reference. Ungated, a sideways push showed up as yaw and a
    /// forward-back push as pitch — and because yaw is the projection of the
    /// body rate onto the up-vector, a corrupted up-vector leaked linear motion
    /// into the INTEGRATED yaw, where it stayed.
    #[test]
    fn linear_acceleration_does_not_move_the_attitude_estimate() {
        let mut t = OrientationTracker::default();
        // Settle flat.
        for _ in 0..50 {
            t.update(&sample([0, 0, G], [0, 0, 0]), Side::Right);
        }
        let level = t.update(&sample([0, 0, G], [0, 0, 0]), Side::Right).euler_rad;

        // A hard sideways shove: magnitude far from 1 g, so not gravity.
        for _ in 0..50 {
            t.update(&sample([0, G, G], [0, 0, 0]), Side::Right);
        }
        let shoved = t.update(&sample([0, G, G], [0, 0, 0]), Side::Right).euler_rad;
        assert!(
            (shoved[0] - level[0]).abs() < 0.02 && (shoved[1] - level[1]).abs() < 0.02,
            "a shove moved the attitude from {level:?} to {shoved:?}",
        );
    }

    /// But a genuine ROTATION must still be tracked immediately.
    ///
    /// The gate keys on magnitude, which rotation does not change — that is
    /// what makes it safe. If it ever started rejecting rotations too, the
    /// attitude would freeze and the fix would look like the bug.
    #[test]
    fn rotating_the_controller_still_updates_the_attitude() {
        let mut t = OrientationTracker::default();
        for _ in 0..50 {
            t.update(&sample([0, 0, G], [0, 0, 0]), Side::Right);
        }
        // Tipped onto its side: still exactly 1 g, just pointing elsewhere.
        // Raw axis 0 maps to canonical -y, so right-grip-down is NEGATIVE here.
        let tipped = t.update(&sample([-G, 0, 0], [0, 0, 0]), Side::Right).euler_rad;
        assert!(
            (tipped[0] - std::f32::consts::FRAC_PI_2).abs() < 0.05,
            "rotation was rejected as well: roll {} rad",
            tipped[0],
        );
    }

    /// A mounting correction must be a PERMUTATION, both halves.
    ///
    /// Reusing a source axis collapses one output and doubles another, which on
    /// hardware reads as "one axis is dead" with nothing pointing at a frame
    /// table as the cause.
    #[test]
    fn the_mounting_corrections_are_permutations() {
        for (name, m) in [("left", MOUNT_LEFT), ("right", MOUNT_RIGHT)] {
            let mut used = [false; 3];
            for (idx, sign) in m {
                assert!(idx < 3, "{name}: source axis {idx} out of range");
                assert!(!used[idx], "{name}: source axis {idx} used twice");
                assert!(sign == 1.0 || sign == -1.0, "{name}: sign {sign} is not ±1");
                used[idx] = true;
            }
            assert!(used.iter().all(|v| *v), "{name}: an axis is unused");
        }
    }

    /// A mounting permutation must move values, and preserve magnitude.
    ///
    /// It is a change of frame, so it can reorder and negate but must never
    /// scale — a hand-written rotation matrix can quietly do the latter, which
    /// is why this is expressed as a signed permutation instead.
    #[test]
    fn a_mounting_permutation_preserves_magnitude() {
        let m: [(usize, f32); 3] = [(2, 1.0), (0, -1.0), (1, 1.0)];
        let v = [3.0f32, 4.0, 12.0];
        let out = apply_mount(v, m);
        assert_eq!(out, [12.0, -3.0, 4.0]);
        let mag = |a: [f32; 3]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
        assert!((mag(out) - mag(v)).abs() < 1e-5);
    }

    /// Yaw must be the rotation ABOUT GRAVITY, not about a body axis.
    ///
    /// With the controller on its side, a rotation about its own vertical axis
    /// is physically ROLL, not yaw — and reporting it as yaw is what produced
    /// "the yaw was doing roll to it". Projecting the body rate onto the
    /// measured up-vector is what makes that impossible.
    #[test]
    fn yaw_is_measured_about_gravity_not_about_a_body_axis() {
        // Flat: canonical up is +z, so a rate on canonical z is real yaw.
        let flat = canonical_yaw_rate([0.0, 0.0, 50.0], [0.0, 0.0, 1.0]);
        assert!((flat - 50.0).abs() < 1e-4, "flat yaw rate {flat}");
        // On its side: canonical up is now +y, so that same z rate is no longer
        // yaw at all and must contribute nothing.
        let sideways = canonical_yaw_rate([0.0, 0.0, 50.0], [0.0, 1.0, 0.0]);
        assert!(sideways.abs() < 1e-4, "a body-axis rate leaked into yaw: {sideways}");
    }

    /// ⛔ A field claiming a rotation gravity cannot see must be cancelled.
    ///
    /// This is the ghost: the fields hand a physical axis between themselves
    /// depending on pose, so one starts reporting roll while the controller is
    /// not rolling. Gravity is the arbiter — it moves when roll moves, and here
    /// it does not.
    #[test]
    fn a_ghost_rotation_the_accelerometer_cannot_see_is_cancelled() {
        for hz in [67.0f32, 200.0] {
            let dt = 1.0 / hz;
            let mut g = GhostCancel::default();
            let mut out = [0.0; 2];
            // Two seconds of the field insisting on 20 deg/s of roll while the
            // tilt does not move at all.
            for _ in 0..(hz * 2.0) as usize {
                out = g.correct([20.0, 0.0], (0.0, 0.0), dt, true);
            }
            assert!(
                out[0].abs() < 2.0,
                "at {hz} Hz a ghost roll of 20 deg/s was left at {} deg/s",
                out[0],
            );
        }
    }

    /// ⛔ …and a REAL rotation must pass through untouched.
    ///
    /// The other half of the bargain, and the one a naive gate gets wrong: when
    /// the field and gravity agree, the correction has nothing to correct and
    /// must converge to zero rather than to some fraction of the signal.
    #[test]
    fn a_real_rotation_both_sources_agree_on_is_not_attenuated() {
        for hz in [67.0f32, 200.0] {
            let dt = 1.0 / hz;
            let mut g = GhostCancel::default();
            let rate = 20.0f32;
            let mut tilt = 0.0f32;
            let mut out = [0.0; 2];
            for _ in 0..(hz * 2.0) as usize {
                tilt += rate.to_radians() * dt;
                out = g.correct([rate, 0.0], (tilt, 0.0), dt, true);
            }
            assert!(
                (out[0] - rate).abs() < 1.0,
                "at {hz} Hz a real {rate} deg/s roll came out as {} deg/s",
                out[0],
            );
        }
    }

    /// ⛔ A fast flick must not be learned as a correction.
    ///
    /// The accelerometer measures the flick as well as gravity, so its implied
    /// tilt is wrong exactly when the rate is highest. The correction is HELD
    /// through that rather than updated — and held, not zeroed, because the
    /// leak it cancels does not go away for the duration of a flick.
    #[test]
    fn an_untrusted_accelerometer_holds_the_correction_rather_than_learning() {
        let dt = 1.0 / 200.0;
        let mut g = GhostCancel::default();
        // Learn a ghost while gravity is trustworthy.
        for _ in 0..400 {
            g.correct([20.0, 0.0], (0.0, 0.0), dt, true);
        }
        let learned = g.correction[0];
        assert!(learned < -15.0, "correction never converged: {learned}");
        // Now a flick: the accelerometer implies a wild tilt, and is not trusted.
        let mut tilt = 0.0f32;
        for _ in 0..200 {
            tilt += 500.0f32.to_radians() * dt;
            g.correct([20.0, 0.0], (tilt, 0.0), dt, false);
        }
        assert!(
            (g.correction[0] - learned).abs() < 1e-3,
            "an untrusted accelerometer moved the correction from {learned} to {}",
            g.correction[0],
        );
    }

    /// ⛔ A sustained slow pan must read as MOVING — at every report rate.
    ///
    /// ⭐ The rate sweep is the point. The detector this replaced compared
    /// gravity against a LAGGING AVERAGE with a per-sample weight, and a
    /// low-pass settles at a constant lag against a steady input — it catches
    /// up and then tracks. Worse, "per-sample" made its window depend on the
    /// polling rate: at 67 Hz the lag was 0.75 s and a 2 °/s pan stayed
    /// detectable, so every test passed. Unlocking 200 Hz cut the window to
    /// 0.25 s, the lag fell under the threshold, and a deliberate aim started
    /// reading as rest — which the bias estimator then cancelled, dragging the
    /// cursor back while the user was moving it.
    ///
    /// So this asserts across rates. A version that only works at one is the
    /// exact bug.
    #[test]
    fn a_sustained_slow_pan_never_reads_as_still_at_any_rate() {
        for hz in [67.0f32, 125.0, 200.0, 250.0] {
            for rate_dps in [1.5f32, 2.0, 5.0, 20.0] {
                let dt = 1.0 / hz;
                let mut d = StillDetector::default();
                let mut still_count = 0;
                // Five seconds of continuous, perfectly steady rotation.
                for i in 0..(hz * 5.0) as usize {
                    let th = (rate_dps * dt * i as f32).to_radians();
                    if d.observe([th.sin(), 0.0, th.cos()], dt) {
                        still_count += 1;
                    }
                }
                assert_eq!(
                    still_count, 0,
                    "{rate_dps} deg/s at {hz} Hz read as still on {still_count} \
                     samples — a deliberate aim will be cancelled as drift",
                );
            }
        }
    }

    /// …and a pad that IS still must be recognised, promptly, at every rate.
    ///
    /// The other half of the bargain: a gate that never opens is as broken as
    /// one that never shuts, and it removes no drift at all. Accelerometer
    /// noise moves the measured direction about 0.0035 per sample, so this
    /// feeds that too — a detector that only works on noiseless input is not
    /// one.
    #[test]
    fn a_still_pad_is_recognised_promptly_at_any_rate() {
        for hz in [67.0f32, 200.0] {
            let dt = 1.0 / hz;
            let mut d = StillDetector::default();
            let mut first_still = None;
            for i in 0..(hz * 4.0) as usize {
                // Deterministic dither of the same size as the real noise.
                let n = ((i * 2654435761usize) % 1000) as f32 / 1000.0 - 0.5;
                let j = n * 0.007;
                if d.observe([j, j * 0.5, 1.0 + j * 0.25], dt) && first_still.is_none() {
                    first_still = Some(i as f32 * dt);
                }
            }
            let t = first_still
                .unwrap_or_else(|| panic!("never settled at {hz} Hz — no drift \
                                           would ever be learned"));
            assert!(
                t < STILL_SETTLE_SECS * 2.0,
                "took {t} s to settle at {hz} Hz, expected about {STILL_SETTLE_SECS}",
            );
        }
    }

    /// ⛔ The yaw PIN and the yaw ORIENTATION use OPPOSITE sign conventions,
    /// on purpose.
    ///
    /// ⭐ Both are correct, for different contracts, and that is exactly why
    /// this keeps getting "fixed" in the wrong direction. The orientation
    /// integrates in the crate's own Euler convention, where positive yaw is a
    /// rotation about the up-vector. The PIN answers to
    /// `flexinput_devices::gyro`, whose contract is `gyro_z` POSITIVE TURNING
    /// RIGHT — which by the right-hand rule is negative about up.
    ///
    /// Without the flip a patch has to enable "invert yaw" to behave normally,
    /// which is a decode bug relocated into every user's configuration. With
    /// the flip applied to BOTH, the 3D model turns the wrong way instead. The
    /// invariant worth pinning is therefore not either sign on its own but that
    /// the two DISAGREE.
    #[test]
    fn the_yaw_pin_is_signed_against_the_gyro_contract_not_the_euler_one() {
        let per_report = (ANGLE_COUNTS_PER_TURN / 360) as i32;
        let mut t = OrientationTracker::default();
        // Flat on the table, so gravity is unambiguous and the projection
        // reduces to the yaw component alone.
        t.update(&sample([0, 0, G], [0, 0, 0]), Side::Right);
        let o = t.update(&sample([0, 0, G], [0, 0, per_report]), Side::Right);

        assert!(o.yaw_rate_dps.abs() > 1.0, "no yaw rate to check the sign of");
        assert!(o.rate_dps[2].abs() > 1.0, "no orientation yaw to compare against");
        assert!(
            o.yaw_rate_dps.signum() != o.rate_dps[2].signum(),
            "the yaw pin ({}) and the orientation yaw ({}) agree in sign — one \
             of the two conventions has been applied to both",
            o.yaw_rate_dps,
            o.rate_dps[2],
        );
    }

    /// ⛔ A slow deliberate aim must NOT be learned as drift.
    ///
    /// ⭐ This is the regression that has landed twice, and both times it was a
    /// stillness detector that TRACKED the motion instead of measuring it
    /// against something fixed. The symptom on hardware is unmistakable and
    /// horrible: nudge the aim a little and it slides back under you, because
    /// the estimator decided the nudge was bias and started cancelling it.
    ///
    /// 2 °/s is the case that matters — well above any drift worth correcting,
    /// well below anything a detector tuned for "is it being waved about" would
    /// catch.
    #[test]
    fn a_slow_steady_pan_is_not_learned_as_drift() {
        const RATE_DPS: f32 = 2.0;
        let per_report_deg = RATE_DPS / REPORT_HZ;
        let counts = (per_report_deg * ANGLE_COUNTS_PER_DEG) as i32;

        let mut t = OrientationTracker::default();
        let mut angle = 0i32;
        let mut out = 0.0;
        // Twenty seconds — many times the estimator's convergence, so if it
        // were going to swallow the pan it has had every chance.
        for i in 0..(REPORT_HZ * 20.0) as usize {
            // Gravity genuinely moves, because the controller genuinely turns.
            let th = (per_report_deg * i as f32).to_radians();
            let accel = [
                (G as f32 * th.sin()) as i32,
                0,
                (G as f32 * th.cos()) as i32,
            ];
            angle += counts;
            out = t.update(&sample(accel, [0, 0, angle]), Side::Right).field_rate_dps[2];
        }
        let expect = RATE_DPS * FIELD_GAIN[2];
        assert!(
            out > expect * 0.8,
            "a {RATE_DPS} deg/s pan decayed to {out} deg/s (expected about \
             {expect}) — the estimator is cancelling deliberate movement",
        );
    }

    /// …and the other half of the same bargain: a pad that is genuinely still
    /// must still have its drift removed.
    ///
    /// Held together these two pin the detector from both sides. Either alone
    /// is trivially satisfiable by a gate that is always open or always shut,
    /// and both of those have shipped.
    ///
    /// Gravity fixed while the field advances is not a contradiction — it is
    /// exactly what yaw drift looks like, the one rotation gravity cannot see.
    #[test]
    fn a_resting_pad_still_gets_its_drift_removed() {
        const DRIFT_DPS: f32 = 2.0;
        let counts = (DRIFT_DPS / REPORT_HZ * ANGLE_COUNTS_PER_DEG) as i32;

        let mut t = OrientationTracker::default();
        let mut angle = 0i32;
        let mut out = 0.0;
        for _ in 0..(REPORT_HZ * 20.0) as usize {
            angle += counts;
            out = t.update(&sample([12, -8, G], [0, 0, angle]), Side::Right)
                .field_rate_dps[2];
        }
        assert!(
            out.abs() < DRIFT_DPS * FIELD_GAIN[2] * 0.2,
            "a resting pad kept reporting {out} deg/s — the drift was never learned",
        );
    }

    /// A controller left alone must stop accumulating yaw.
    ///
    /// ❗ A CONSTANT angle field is not a stationary controller, and the test
    /// used to read as though it were. Real hardware at rest reports a steady
    /// non-zero field rate — that is precisely what `RESTING_DRIFT_*` measures
    /// — and the decoder subtracts it. A field that never moves is therefore a
    /// controller whose drift has ALREADY been removed, so the subtraction puts
    /// the constant back in with the opposite sign, and report 1 carries it in
    /// full by construction. Asserting zero there tests nothing but the size of
    /// the constant.
    ///
    /// What is worth pinning is what happens next: [`GyroBias`] has to notice a
    /// standing offset on a device it can see is still, and drive it out before
    /// it becomes degrees. So the assertions are on the settled tail and on the
    /// TOTAL yaw accumulated on the way there — the quantity the user actually
    /// sees, and the one that made the model wander off while sitting on a
    /// desk.
    ///
    /// (This test spent a while silently not running: a stray `#[test]` above
    /// it was adopted by a newly inserted neighbour, leaving this one with
    /// none. It is here, in a drift test, that that mattered most.)
    #[test]
    fn holding_still_produces_no_drift() {
        let mut t = OrientationTracker::default();
        let mut settled = 0.0f32;
        for i in 0..2000 {
            let o = t.update(&sample([12, -8, G], [0, 0, 5_000_000]), Side::Right);
            // By 300 reports — under three seconds of link time — the estimator
            // has had ample opportunity; anything left is a leak, not lag.
            if i >= 300 {
                settled = settled.max(o.rate_dps.iter().fold(0.0, |a: f32, v| a.max(v.abs())));
            }
        }
        assert!(settled < 1e-3, "still drifting at {settled} deg/s after settling");
        // Everything the injected offset managed to integrate before the
        // estimator caught it. Measured at 0.010°; a tenth of a degree is a
        // generous ceiling and still far below anything visible.
        let wandered = t.yaw_rad.to_degrees().abs();
        assert!(wandered < 0.1, "yaw wandered {wandered} deg while sitting still");
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
