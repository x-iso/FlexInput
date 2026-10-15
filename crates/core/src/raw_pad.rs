//! Learned layouts for controllers no mapping database knows.
//!
//! A DirectInput pad that SDL has no gamepad mapping for reaches FlexInput as
//! a bare joystick: numbered axes, buttons and hats that mean nothing yet (the
//! `sdl:raw:` devices). Where its HID report descriptor can be read, every
//! field SDL skips comes along too ([`RawSrc::Field`]) — vendor fields, sensor
//! fields, bytes of a vendor blob. The user teaches FlexInput what each one is
//! with Learn on the device node, and the answers become a [`RawPadProfile`]
//! kept in settings under [`profile_key`] — so it follows the MODEL across
//! reconnects, tabs and presets instead of living on one node. The device
//! backend compiles it ([`RawPadProfile::compile`]) and applies it on every
//! poll, so the pad publishes the same canonical pins a known pad does and
//! AutoMap, the Remapper, calibration and the rest work on it unchanged.
//!
//! Every element the backend reads per poll is ALSO published under its raw
//! pin (`raw:a0`, `raw:b3`, `raw:h0`, `raw:f…` — see [`RawSrc::pin_id`]);
//! Learn watches those. An element kept as a port of its own is published
//! under its EXTRA pin (`extra:b3`, [`RawSrc::extra_pin_id`]), which — unlike
//! the raw pins — rides the AutoMap bus, so a Remapper downstream can map it.
//!
//! A motion sensor is learned whole rather than axis by axis: [`solve_motion`]
//! turns a short guided pose sequence into an [`ImuMap`] per sensor, which
//! carries the axis choice, frame and scale onto the canonical `gyro_*` /
//! `accel_*` pins.

use std::collections::HashMap;

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::{automap, imu, Signal, SignalType};

/// Prefix of the per-element pins a raw joystick publishes.
pub const RAW_PIN_PREFIX: &str = "raw:";

/// Prefix of an element kept as a port of its own. These ride the bus.
pub const EXTRA_PIN_PREFIX: &str = "extra:";

/// Device-id prefix of a raw joystick (`sdl:raw:<inst>`).
pub const RAW_DEVICE_PREFIX: &str = "sdl:raw:";

/// The pin a hat bind drives: the whole D-pad (Vec2 plus its four buttons and
/// two axes).
pub const HAT_DPAD_PIN: &str = "dpad";

/// A raw axis reads −1..1, so a bind whose rest and full travel sit closer
/// than this is a capture that never really moved — refused rather than
/// turned into a near-infinite gain.
const MIN_SPAN: f32 = 0.2;

/// XInput's digital-trigger threshold (~30/255), as the SDL gamepad path uses.
const TRIGGER_DIGITAL_THRESHOLD: f32 = 0.117;

pub fn is_raw_device_id(id: &str) -> bool {
    id.starts_with(RAW_DEVICE_PREFIX)
}

pub fn is_raw_pin(pin: &str) -> bool {
    pin.starts_with(RAW_PIN_PREFIX)
}

pub fn is_extra_pin(pin: &str) -> bool {
    pin.starts_with(EXTRA_PIN_PREFIX)
}

/// The settings key a raw pad's profile is stored under: the model, not the
/// connection. Two identical pads share one layout.
pub fn profile_key(vid: Option<u16>, pid: Option<u16>, name: &str) -> String {
    format!("{:04x}:{:04x}:{}", vid.unwrap_or(0), pid.unwrap_or(0), name)
}

// ── Elements ──────────────────────────────────────────────────────────────────

/// A bit field of one HID input report: where it sits and how to read it.
/// Values are normalised over `min..=max` to −1..1 like an SDL axis (a 1-bit
/// field reads as a button); a negative `min` makes the field signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HidField {
    /// Report id; 0 for a device that doesn't number its reports.
    pub report: u8,
    /// Bit offset into the report's payload (after the id byte).
    pub bit: u16,
    /// Width in bits, 1..=32.
    pub size: u8,
    pub min: i32,
    pub max: i32,
}

impl HidField {
    /// A little-endian 16-bit signed word at `byte` — how a motion sensor
    /// inside a vendor blob is read.
    pub fn word(report: u8, byte: u16) -> Self {
        HidField { report, bit: byte * 8, size: 16, min: i16::MIN as i32, max: i16::MAX as i32 }
    }

    /// One bit of `byte`, as a button.
    pub fn flag(report: u8, byte: u16, bit: u8) -> Self {
        HidField { report, bit: byte * 8 + bit as u16, size: 1, min: 0, max: 1 }
    }

    /// An unsigned byte, as an axis.
    pub fn byte(report: u8, byte: u16) -> Self {
        HidField { report, bit: byte * 8, size: 8, min: 0, max: 255 }
    }

    /// The field's integer value in `payload`, sign-extended when signed.
    pub fn raw(&self, payload: &[u8]) -> Option<i64> {
        let size = self.size as usize;
        if size == 0 || size > 32 {
            return None;
        }
        let start = self.bit as usize;
        if start + size > payload.len() * 8 {
            return None;
        }
        let mut v: u64 = 0;
        for i in 0..size {
            let b = start + i;
            if (payload[b / 8] >> (b % 8)) & 1 == 1 {
                v |= 1 << i;
            }
        }
        let signed = self.min < 0;
        Some(if signed && (v >> (size - 1)) & 1 == 1 {
            v as i64 - (1i64 << size)
        } else {
            v as i64
        })
    }

    /// The field read as an element: a button for one bit, else an axis.
    pub fn value(&self, payload: &[u8]) -> Option<RawValue> {
        let v = self.raw(payload)?;
        if self.size == 1 {
            return Some(RawValue::Button(v != 0));
        }
        let (lo, hi) = (self.min as f64, self.max as f64);
        if hi <= lo {
            return Some(RawValue::Axis(0.0));
        }
        let t = 2.0 * (v as f64 - lo) / (hi - lo) - 1.0;
        Some(RawValue::Axis(t.clamp(-1.0, 1.0) as f32))
    }
}

/// One element of a raw joystick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawSrc {
    /// By index in SDL's axis list.
    Axis(u8),
    Button(u8),
    Hat(u8),
    /// Read from the HID input report directly, past SDL.
    Field(HidField),
}

/// What kind of reading an element gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElemKind {
    Axis,
    Button,
    Hat,
}

impl RawSrc {
    fn suffix(self) -> String {
        match self {
            RawSrc::Axis(i) => format!("a{i}"),
            RawSrc::Button(i) => format!("b{i}"),
            RawSrc::Hat(i) => format!("h{i}"),
            RawSrc::Field(f) => format!("f{}.{}.{}.{}.{}", f.report, f.bit, f.size, f.min, f.max),
        }
    }

    fn from_suffix(rest: &str) -> Option<Self> {
        let (kind, tail) = rest.split_at_checked(1)?;
        match kind {
            "a" => tail.parse().ok().map(RawSrc::Axis),
            "b" => tail.parse().ok().map(RawSrc::Button),
            "h" => tail.parse().ok().map(RawSrc::Hat),
            "f" => {
                let mut it = tail.split('.');
                let f = HidField {
                    report: it.next()?.parse().ok()?,
                    bit: it.next()?.parse().ok()?,
                    size: it.next()?.parse().ok()?,
                    min: it.next()?.parse().ok()?,
                    max: it.next()?.parse().ok()?,
                };
                (it.next().is_none() && (1..=32).contains(&f.size)).then_some(RawSrc::Field(f))
            }
            _ => None,
        }
    }

    /// The pin the backend publishes this element's reading on.
    pub fn pin_id(self) -> String {
        format!("{RAW_PIN_PREFIX}{}", self.suffix())
    }

    pub fn from_pin_id(pin: &str) -> Option<Self> {
        Self::from_suffix(pin.strip_prefix(RAW_PIN_PREFIX)?)
    }

    /// The pin it is published on when kept as a port of its own.
    pub fn extra_pin_id(self) -> String {
        format!("{EXTRA_PIN_PREFIX}{}", self.suffix())
    }

    pub fn from_extra_pin_id(pin: &str) -> Option<Self> {
        Self::from_suffix(pin.strip_prefix(EXTRA_PIN_PREFIX)?)
    }

    pub fn kind(self) -> ElemKind {
        match self {
            RawSrc::Axis(_) => ElemKind::Axis,
            RawSrc::Button(_) => ElemKind::Button,
            RawSrc::Hat(_) => ElemKind::Hat,
            RawSrc::Field(f) if f.size == 1 => ElemKind::Button,
            RawSrc::Field(_) => ElemKind::Axis,
        }
    }

    /// What the user sees. One-based, the way the Windows game-controller
    /// panel and every DirectInput tool number them. A field's descriptor
    /// usage name, where there is one, is the UI's to put first.
    pub fn label(self) -> String {
        match self {
            RawSrc::Axis(i) => format!("Axis {}", i as u16 + 1),
            RawSrc::Button(i) => format!("Button {}", i as u16 + 1),
            RawSrc::Hat(i) => format!("Hat {}", i as u16 + 1),
            RawSrc::Field(f) => {
                let report = if f.report == 0 { String::new() } else { format!("Report {} · ", f.report) };
                let (byte, bit) = (f.bit / 8, f.bit % 8);
                match f.size {
                    1 => format!("{report}byte {byte} bit {bit}"),
                    s if bit == 0 && s % 8 == 0 => format!("{report}byte {byte} ({s}-bit)"),
                    s => format!("{report}bit {} ({s}-bit)", f.bit),
                }
            }
        }
    }

    pub fn signal_type(self) -> SignalType {
        match self.kind() {
            ElemKind::Axis => SignalType::Float,
            ElemKind::Button => SignalType::Bool,
            ElemKind::Hat => SignalType::Vec2,
        }
    }
}

/// A raw element's reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RawValue {
    Axis(f32),
    Button(bool),
    Hat(Vec2),
}

impl RawValue {
    pub fn from_signal(src: RawSrc, s: Signal) -> Self {
        match src.kind() {
            ElemKind::Axis => RawValue::Axis(s.as_float()),
            ElemKind::Button => RawValue::Button(s.as_bool()),
            ElemKind::Hat => RawValue::Hat(match s {
                Signal::Vec2(v) => v,
                _ => Vec2::ZERO,
            }),
        }
    }

    pub fn signal(self) -> Signal {
        match self {
            RawValue::Axis(v) => Signal::Float(v),
            RawValue::Button(b) => Signal::Bool(b),
            RawValue::Hat(h) => Signal::Vec2(h),
        }
    }

    /// As a number: an axis' value, 1/0 for a button, a hat's length.
    pub fn as_f32(self) -> f32 {
        match self {
            RawValue::Axis(v) => v,
            RawValue::Button(b) => if b { 1.0 } else { 0.0 },
            RawValue::Hat(h) => h.length(),
        }
    }
}

// ── Profile ───────────────────────────────────────────────────────────────────

fn default_full() -> f32 {
    1.0
}

/// One learned element: what it drives and, for an axis, how.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawBind {
    pub src: RawSrc,
    /// What the source drives: a canonical pin (`automap::ALL_PINS`),
    /// [`HAT_DPAD_PIN`] for a hat, or the source's extra pin
    /// ([`RawSrc::extra_pin_id`]) to keep it as a port of its own.
    pub pin: String,
    /// Axis sources only: the raw reading at rest, and at full travel the way
    /// that drives `pin` positive. The pin reads `(v - rest) / (full - rest)`,
    /// so a stick axis bound the other way round has `full = -1`, and a
    /// trigger that rests at the bottom of its axis has `rest = -1`.
    #[serde(default)]
    pub rest: f32,
    #[serde(default = "default_full")]
    pub full: f32,
    /// The port's name when the source is kept as its own pin; empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl RawBind {
    pub fn is_own_port(&self) -> bool {
        is_extra_pin(&self.pin)
    }

    /// The direction of travel that drives the pin positive, for an axis.
    fn direction(&self) -> f32 {
        (self.full - self.rest).signum()
    }
}

/// Pins whose value runs both ways from rest (−1..1). Every other analog pin
/// an axis can drive is one-sided (0..1).
pub fn is_two_sided(pin: &str) -> bool {
    matches!(
        pin,
        "left_stick_x" | "left_stick_y" | "right_stick_x" | "right_stick_y" | "dpad_x" | "dpad_y"
    )
}

/// A motion sensor's three axes mapped onto the canonical ones:
/// `bus = matrix · (reading − bias)`, row-major, already in bus units
/// (±1 = `imu::BUS_GYRO_DPS` / `imu::BUS_ACCEL_G`) and in the canonical frame,
/// so nothing downstream can tell it from a known pad's sensor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImuMap {
    pub src: [RawSrc; 3],
    #[serde(default)]
    pub bias: [f32; 3],
    pub matrix: [f32; 9],
}

impl ImuMap {
    fn read(&self, r: &RawReading) -> Option<[f32; 3]> {
        let mut v = [0.0f32; 3];
        for (i, src) in self.src.iter().enumerate() {
            v[i] = r.value(*src)?.as_f32() - self.bias[i];
        }
        let m = &self.matrix;
        Some([
            (m[0] * v[0] + m[1] * v[1] + m[2] * v[2]).clamp(-1.0, 1.0),
            (m[3] * v[0] + m[4] * v[1] + m[5] * v[2]).clamp(-1.0, 1.0),
            (m[6] * v[0] + m[7] * v[1] + m[8] * v[2]).clamp(-1.0, 1.0),
        ])
    }
}

/// A raw pad's learned layout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RawPadProfile {
    /// The device name it was learned on, for display.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub binds: Vec<RawBind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gyro: Option<ImuMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accel: Option<ImuMap>,
    /// Started from the community controller database (`sdl_db`) rather
    /// than from nothing. Kept even once every entry is forgotten, so the
    /// database isn't offered again for a pad the user cleared on purpose.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_db: bool,
}

/// All learned layouts, by [`profile_key`].
pub type RawPadProfiles = HashMap<String, RawPadProfile>;

const GYRO_PINS: [&str; 3] = ["gyro_x", "gyro_y", "gyro_z"];
const ACCEL_PINS: [&str; 3] = ["accel_x", "accel_y", "accel_z"];

impl RawPadProfile {
    pub fn is_empty(&self) -> bool {
        self.binds.is_empty() && self.gyro.is_none() && self.accel.is_none()
    }

    /// Add `bind`, replacing whatever it supersedes: any bind driving the same
    /// pin (one source per pin keeps a re-learn from leaving the old element
    /// still driving it), and any earlier bind of the same source — except
    /// that an axis can drive two one-sided pins with its two halves, which is
    /// how a pad with both triggers on one Z axis gets mapped.
    pub fn set(&mut self, bind: RawBind) {
        self.binds.retain(|b| {
            if b.pin == bind.pin {
                return false;
            }
            if b.src != bind.src {
                return true;
            }
            b.src.kind() == ElemKind::Axis
                && !is_two_sided(&b.pin)
                && !is_two_sided(&bind.pin)
                && !b.is_own_port()
                && !bind.is_own_port()
                && b.direction() != bind.direction()
        });
        self.binds.push(bind);
    }

    /// Forget whatever drives `pin` — a gyro or accel pin forgets that whole
    /// sensor. True if anything did.
    pub fn remove_pin(&mut self, pin: &str) -> bool {
        if GYRO_PINS.contains(&pin) {
            return self.gyro.take().is_some();
        }
        if ACCEL_PINS.contains(&pin) {
            return self.accel.take().is_some();
        }
        let before = self.binds.len();
        self.binds.retain(|b| b.pin != pin);
        self.binds.len() != before
    }

    /// Install a learned motion sensor, freeing its axes from any bind.
    pub fn set_motion(&mut self, gyro: Option<ImuMap>, accel: Option<ImuMap>) {
        if gyro.is_some() {
            self.gyro = gyro;
        }
        if accel.is_some() {
            self.accel = accel;
        }
        let used: Vec<RawSrc> = self.motion_srcs().collect();
        self.binds.retain(|b| !used.contains(&b.src));
    }

    /// The elements feeding a motion sensor.
    pub fn motion_srcs(&self) -> impl Iterator<Item = RawSrc> + '_ {
        self.gyro.iter().chain(self.accel.iter()).flat_map(|m| m.src)
    }

    /// Every element the layout reads.
    pub fn srcs_in_use(&self) -> Vec<RawSrc> {
        self.binds.iter().map(|b| b.src).chain(self.motion_srcs()).collect()
    }

    /// The device-node ports this layout implies, in order: each bind's pin,
    /// then a stick's Vec2 once either of its axes is bound, then the motion
    /// pins. `(id, name, type)`.
    pub fn ports(&self) -> Vec<(String, String, SignalType)> {
        fn push(out: &mut Vec<(String, String, SignalType)>, id: String, name: String, ty: SignalType) {
            if !out.iter().any(|(i, _, _)| *i == id) {
                out.push((id, name, ty));
            }
        }
        fn push_canonical(out: &mut Vec<(String, String, SignalType)>, pin: &str) {
            if let Some(p) = canonical(pin) {
                push(out, p.id.to_string(), p.display_name.to_string(), p.signal_type);
            }
        }
        let mut out = Vec::new();
        for b in &self.binds {
            if b.is_own_port() {
                let name = if b.label.is_empty() { b.src.label() } else { b.label.clone() };
                push(&mut out, b.pin.clone(), name, b.src.signal_type());
            } else {
                push_canonical(&mut out, &b.pin);
            }
        }
        for (stick, x, y) in [
            ("left_stick", "left_stick_x", "left_stick_y"),
            ("right_stick", "right_stick_x", "right_stick_y"),
        ] {
            if self.binds.iter().any(|b| b.pin == x || b.pin == y) {
                push_canonical(&mut out, stick);
            }
        }
        for (map, pins) in [(&self.gyro, GYRO_PINS), (&self.accel, ACCEL_PINS)] {
            if map.is_some() {
                for pin in pins {
                    push_canonical(&mut out, pin);
                }
            }
        }
        out
    }

    /// Resolve the pins once, so the per-poll [`RawLayout::apply`] does no
    /// string matching.
    pub fn compile(&self) -> RawLayout {
        let mut binds = Vec::new();
        for b in &self.binds {
            if b.src.kind() == ElemKind::Axis && (b.full - b.rest).abs() < MIN_SPAN {
                continue;
            }
            let target = if b.is_own_port() {
                Target::Extra(b.pin.clone())
            } else {
                let Some(p) = canonical(&b.pin) else { continue };
                match (b.src.kind(), p.id) {
                    (ElemKind::Hat, HAT_DPAD_PIN) => Target::Dpad,
                    (ElemKind::Hat, _) => continue,
                    (_, id) => match p.signal_type {
                        SignalType::Bool => Target::Bool(id),
                        SignalType::Float => Target::Float { id, two_sided: is_two_sided(id) },
                        _ => continue,
                    },
                }
            };
            binds.push(CompiledBind { src: b.src, target, rest: b.rest, full: b.full });
        }
        RawLayout { binds, gyro: self.gyro.clone(), accel: self.accel.clone() }
    }
}

fn canonical(pin: &str) -> Option<&'static automap::AutoMapPin> {
    automap::ALL_PINS.iter().find(|p| p.id == pin)
}

#[derive(Clone, Debug)]
enum Target {
    Bool(&'static str),
    Float { id: &'static str, two_sided: bool },
    Dpad,
    /// An own port: the element's reading, as is, on its extra pin.
    Extra(String),
}

#[derive(Clone, Debug)]
struct CompiledBind {
    src: RawSrc,
    target: Target,
    rest: f32,
    full: f32,
}

/// A [`RawPadProfile`] resolved for the per-poll path.
#[derive(Clone, Debug, Default)]
pub struct RawLayout {
    binds: Vec<CompiledBind>,
    gyro: Option<ImuMap>,
    accel: Option<ImuMap>,
}

/// Where [`RawSrc::Field`]s are read from: the latest payload (after the id
/// byte) of each HID input report, by report id.
pub trait ReportSource {
    fn report(&self, id: u8) -> Option<&[u8]>;
}

/// No HID tier: every field reads as absent.
impl ReportSource for () {
    fn report(&self, _: u8) -> Option<&[u8]> {
        None
    }
}

impl ReportSource for HashMap<u8, Vec<u8>> {
    fn report(&self, id: u8) -> Option<&[u8]> {
        self.get(&id).map(|v| v.as_slice())
    }
}

/// One poll of a raw joystick: SDL's axes (−1..1), buttons and hats (+x right,
/// +y up), and the latest HID input reports for [`RawSrc::Field`]s.
pub struct RawReading<'a> {
    pub axes: &'a [f32],
    pub buttons: &'a [bool],
    pub hats: &'a [Vec2],
    pub reports: &'a dyn ReportSource,
}

impl RawReading<'_> {
    pub fn value(&self, src: RawSrc) -> Option<RawValue> {
        match src {
            RawSrc::Axis(i) => self.axes.get(i as usize).map(|v| RawValue::Axis(*v)),
            RawSrc::Button(i) => self.buttons.get(i as usize).map(|b| RawValue::Button(*b)),
            RawSrc::Hat(i) => self.hats.get(i as usize).map(|h| RawValue::Hat(*h)),
            RawSrc::Field(f) => f.value(self.reports.report(f.report)?),
        }
    }
}

impl RawLayout {
    pub fn is_empty(&self) -> bool {
        self.binds.is_empty() && self.gyro.is_none() && self.accel.is_none()
    }

    /// The motion sensors' readings, in bus units: `(gyro, accel)`.
    pub fn motion(&self, r: &RawReading) -> (Option<[f32; 3]>, Option<[f32; 3]>) {
        (
            self.gyro.as_ref().and_then(|m| m.read(r)),
            self.accel.as_ref().and_then(|m| m.read(r)),
        )
    }

    /// The pins this poll's readings drive, plus the composites they imply (a
    /// stick's Vec2, the D-pad's axes and Vec2, the digital trigger next to an
    /// analog one and the other way round). Pins nothing drives are not
    /// emitted at all. Motion is separate ([`Self::motion`]): the backend runs
    /// its spike filter over it first.
    pub fn apply(&self, r: &RawReading, mut emit: impl FnMut(&str, Signal)) {
        // A handful of pins each poll: linear lists beat a map here.
        let mut floats: Vec<(&'static str, f32)> = Vec::with_capacity(8);
        let mut bools: Vec<(&'static str, bool)> = Vec::with_capacity(16);
        fn or_bool(v: &mut Vec<(&'static str, bool)>, id: &'static str, on: bool) {
            match v.iter_mut().find(|(i, _)| *i == id) {
                Some((_, b)) => *b |= on,
                None => v.push((id, on)),
            }
        }
        // Two sources on one analog pin: the one pushed further wins.
        fn max_float(v: &mut Vec<(&'static str, f32)>, id: &'static str, x: f32) {
            match v.iter_mut().find(|(i, _)| *i == id) {
                Some((_, f)) => {
                    if x.abs() > f.abs() {
                        *f = x;
                    }
                }
                None => v.push((id, x)),
            }
        }

        for b in &self.binds {
            let Some(value) = r.value(b.src) else { continue };
            let t = match (&b.target, value) {
                (Target::Extra(pin), v) => {
                    emit(pin, v.signal());
                    continue;
                }
                (Target::Dpad, RawValue::Hat(h)) => {
                    or_bool(&mut bools, "dpad_up", h.y > 0.5);
                    or_bool(&mut bools, "dpad_down", h.y < -0.5);
                    or_bool(&mut bools, "dpad_right", h.x > 0.5);
                    or_bool(&mut bools, "dpad_left", h.x < -0.5);
                    continue;
                }
                (_, RawValue::Button(on)) => if on { 1.0 } else { 0.0 },
                (_, RawValue::Axis(v)) => {
                    let span = b.full - b.rest;
                    if span.abs() < f32::EPSILON {
                        continue;
                    }
                    (v - b.rest) / span
                }
                _ => continue,
            };
            match b.target {
                Target::Bool(id) => or_bool(&mut bools, id, t > 0.5),
                Target::Float { id, two_sided } => {
                    let v = if two_sided { t.clamp(-1.0, 1.0) } else { t.clamp(0.0, 1.0) };
                    max_float(&mut floats, id, v);
                }
                Target::Dpad | Target::Extra(_) => {}
            }
        }

        let float = |v: &Vec<(&'static str, f32)>, id: &str| {
            v.iter().find(|(i, _)| *i == id).map(|(_, f)| *f)
        };
        let boolean = |v: &Vec<(&'static str, bool)>, id: &str| {
            v.iter().find(|(i, _)| *i == id).map(|(_, b)| *b)
        };

        // Sticks: the Vec2 from whichever axes are bound.
        for (stick, x, y) in [
            ("left_stick", "left_stick_x", "left_stick_y"),
            ("right_stick", "right_stick_x", "right_stick_y"),
        ] {
            let (fx, fy) = (float(&floats, x), float(&floats, y));
            if fx.is_some() || fy.is_some() {
                emit(stick, Signal::Vec2(Vec2::new(fx.unwrap_or(0.0), fy.unwrap_or(0.0))));
            }
        }

        // D-pad: from its axes when an axis drives them, else from its buttons.
        let (ax, ay) = (float(&floats, "dpad_x"), float(&floats, "dpad_y"));
        let dirs = ["dpad_up", "dpad_down", "dpad_left", "dpad_right"];
        let any_btn = dirs.iter().any(|d| boolean(&bools, d).is_some());
        if ax.is_some() || ay.is_some() {
            let (x, y) = (ax.unwrap_or(0.0), ay.unwrap_or(0.0));
            for (d, on) in [("dpad_up", y > 0.5), ("dpad_down", y < -0.5),
                            ("dpad_left", x < -0.5), ("dpad_right", x > 0.5)] {
                if boolean(&bools, d).is_none() {
                    or_bool(&mut bools, d, on);
                }
            }
            if ax.is_none() { floats.push(("dpad_x", 0.0)); }
            if ay.is_none() { floats.push(("dpad_y", 0.0)); }
            emit("dpad", Signal::Vec2(Vec2::new(x, y)));
        } else if any_btn {
            let on = |d: &str| boolean(&bools, d).unwrap_or(false);
            let x = if on("dpad_right") { 1.0 } else if on("dpad_left") { -1.0 } else { 0.0 };
            let y = if on("dpad_up") { 1.0 } else if on("dpad_down") { -1.0 } else { 0.0 };
            let (x, y) = if x != 0.0 && y != 0.0 {
                (x * std::f32::consts::FRAC_1_SQRT_2, y * std::f32::consts::FRAC_1_SQRT_2)
            } else {
                (x, y)
            };
            floats.push(("dpad_x", x));
            floats.push(("dpad_y", y));
            for d in dirs {
                if boolean(&bools, d).is_none() {
                    bools.push((d, false));
                }
            }
            emit("dpad", Signal::Vec2(Vec2::new(x, y)));
        }

        // Triggers: analog and digital stand in for each other.
        for (analog, digital) in [("left_trigger", "btn_lt_dig"), ("right_trigger", "btn_rt_dig")] {
            match (float(&floats, analog), boolean(&bools, digital)) {
                (Some(a), None) => bools.push((digital, a > TRIGGER_DIGITAL_THRESHOLD)),
                (None, Some(d)) => floats.push((analog, if d { 1.0 } else { 0.0 })),
                _ => {}
            }
        }

        for (id, v) in floats {
            emit(id, Signal::Float(v));
        }
        for (id, v) in bools {
            emit(id, Signal::Bool(v));
        }
    }
}

// ── Learn ─────────────────────────────────────────────────────────────────────

/// How far an axis must travel from where it rested for Learn to take it.
/// Half the range: well clear of stick drift and of the jitter an
/// accelerometer exposed as an axis shows while the pad is held.
pub const LEARN_AXIS_THRESHOLD: f32 = 0.5;

/// How close to its resting value an axis must come back before Learn
/// listens for the next element.
pub const LEARN_REST_TOLERANCE: f32 = 0.25;

/// What Learn saw move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detected {
    pub src: RawSrc,
    /// Axis: where it rested when Learn started. 0 otherwise.
    pub rest: f32,
    /// Axis: the reading when it crossed the threshold — its sign relative
    /// to `rest` is the direction the user moved it. 0 otherwise.
    pub moved_to: f32,
}

impl Detected {
    /// The direction the user moved an axis, ±1.
    pub fn direction(&self) -> f32 {
        if self.moved_to >= self.rest { 1.0 } else { -1.0 }
    }

    /// An axis that rested near an end of its travel: a trigger, a throttle.
    pub fn rests_at_end(&self) -> bool {
        self.rest.abs() > 0.5
    }
}

/// The first element that has left its resting value: a pressed button, a
/// hat off centre, or an axis moved past [`LEARN_AXIS_THRESHOLD`]. Buttons
/// first, then hats, then axes, so a pad that reports a trigger as both a
/// button and an axis is caught on the button the moment it clicks; the
/// caller decides whether to wait for the axis (see Learn's settle window).
pub fn detect(baseline: &[(RawSrc, RawValue)], now: &[(RawSrc, RawValue)]) -> Option<Detected> {
    let base_of = |src: RawSrc| baseline.iter().find(|(s, _)| *s == src).map(|(_, v)| *v);
    let mut hat = None;
    let mut axis: Option<(f32, Detected)> = None;
    for &(src, v) in now {
        match (v, base_of(src)) {
            (RawValue::Button(true), Some(RawValue::Button(false)) | None) => {
                return Some(Detected { src, rest: 0.0, moved_to: 0.0 });
            }
            (RawValue::Hat(h), Some(RawValue::Hat(b))) if b.length() < 0.5 && h.length() > 0.5 => {
                hat.get_or_insert(Detected { src, rest: 0.0, moved_to: 0.0 });
            }
            (RawValue::Axis(a), Some(RawValue::Axis(b))) => {
                let d = (a - b).abs();
                if d > LEARN_AXIS_THRESHOLD && axis.is_none_or(|(best, _)| d > best) {
                    axis = Some((d, Detected { src, rest: b, moved_to: a }));
                }
            }
            _ => {}
        }
    }
    hat.or(axis.map(|(_, d)| d))
}

/// True once every element is back where it rested (buttons up, hats
/// centred, axes within [`LEARN_REST_TOLERANCE`]) — Learn's cue that the
/// last element was let go and the next one can be listened for.
pub fn all_at_rest(baseline: &[(RawSrc, RawValue)], now: &[(RawSrc, RawValue)]) -> bool {
    now.iter().all(|&(src, v)| {
        let base = baseline.iter().find(|(s, _)| *s == src).map(|(_, b)| *b);
        match (v, base) {
            (RawValue::Button(on), _) => !on,
            (RawValue::Hat(h), _) => h.length() < 0.5,
            (RawValue::Axis(a), Some(RawValue::Axis(b))) => (a - b).abs() < LEARN_REST_TOLERANCE,
            _ => true,
        }
    })
}

/// What a byte of a vendor blob did between Learn's baseline and now: one
/// bit flipped (a button), or the whole byte swung (an 8-bit axis). `None`
/// when it is unchanged or changed in some other way.
pub fn detect_blob_byte(report: u8, byte: u16, base: u8, now: u8) -> Option<Detected> {
    let diff = base ^ now;
    if diff == 0 {
        return None;
    }
    if diff.count_ones() == 1 {
        let bit = diff.trailing_zeros() as u8;
        // A set bit is a press; a cleared one is a press on an active-low flag.
        // Either way the element is the bit; which way round is the bind's.
        return Some(Detected { src: RawSrc::Field(HidField::flag(report, byte, bit)), rest: 0.0, moved_to: 0.0 });
    }
    if (now as i16 - base as i16).abs() >= 96 {
        let f = HidField::byte(report, byte);
        let norm = |b: u8| b as f32 / 127.5 - 1.0;
        return Some(Detected { src: RawSrc::Field(f), rest: norm(base), moved_to: norm(now) });
    }
    None
}

// ── Motion ────────────────────────────────────────────────────────────────────

/// What the guided motion setup recorded, over every candidate element at
/// once: the resting reading in each pose, and each turn's integral
/// `∫(reading − rest before it) dt` (reading × seconds).
///
/// The poses and turns, in order: flat face up · tilt the nose up to stand on
/// the grips (+pitch) · flat again · roll onto the right side (+roll) · flat
/// again · turn right on the table (+yaw).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MotionCapture {
    pub srcs: Vec<RawSrc>,
    pub flat: Vec<f32>,
    pub nose_up: Vec<f32>,
    pub flat_again: Vec<f32>,
    pub right_down: Vec<f32>,
    pub flat_yaw: Vec<f32>,
    pub pitch: Vec<f32>,
    pub roll: Vec<f32>,
    pub yaw: Vec<f32>,
}

/// The sensors [`solve_motion`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MotionResult {
    pub gyro: Option<ImuMap>,
    pub accel: Option<ImuMap>,
}

fn v3(v: &[f32], idx: [usize; 3]) -> Vec3 {
    Vec3::new(v[idx[0]], v[idx[1]], v[idx[2]])
}

/// The three largest by `score`, if they stand out: each at least `frac` of
/// the largest, which itself must exceed `floor`.
fn top_three(score: &[f32], exclude: &[usize], frac: f32, floor: f32) -> Option<[usize; 3]> {
    let mut idx: Vec<usize> = (0..score.len()).filter(|i| !exclude.contains(i)).collect();
    idx.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    let best = *score.get(*idx.first()?)?;
    if best <= floor || idx.len() < 3 {
        return None;
    }
    let three = [idx[0], idx[1], idx[2]];
    three.iter().all(|&i| score[i] >= frac * best).then_some(three)
}

/// Gram-Schmidt `v` against `against`, normalised; `None` when nothing of it
/// is left (the pose or turn repeated an earlier one).
fn orthonormal(v: Vec3, against: &[Vec3]) -> Option<Vec3> {
    let len = v.length();
    let mut w = v;
    for a in against {
        w -= w.dot(*a) * *a;
    }
    (w.length() > 0.35 * len && len > 0.0).then(|| w.normalize())
}

fn rows(r: [Vec3; 3], scale: f32) -> [f32; 9] {
    let [a, b, c] = r.map(|v| v * scale);
    [a.x, a.y, a.z, b.x, b.y, b.z, c.x, c.y, c.z]
}

/// Work out the motion sensors from a [`MotionCapture`].
///
/// * **Accelerometer**: the three elements whose RESTING reading changes with
///   the pose. Gravity reads "up" in the pad's frame, so flat gives the
///   vertical axis, nose-up the forward one and right-side-down the side one
///   — orthogonalised, so a pose held a few degrees off still gives a clean
///   frame — and the length of gravity gives the scale.
/// * **Gyro**: of the rest, the three most active while turning — a gyro
///   returns to its bias when the pad stops. Each turn's integral points along
///   its rotation axis; the angle the accelerometer saw the pitch and roll
///   turns sweep, over the integral's length, gives the scale (a gyro-only
///   pad assumes the quarter turns the steps ask for).
///
/// Counters and timestamps must be kept out of the candidates: they change
/// in every pose and every turn alike.
pub fn solve_motion(c: &MotionCapture) -> Result<MotionResult, String> {
    let n = c.srcs.len();
    let all = [&c.flat, &c.nose_up, &c.flat_again, &c.right_down, &c.flat_yaw, &c.pitch, &c.roll, &c.yaw];
    if n < 3 || all.iter().any(|v| v.len() != n) {
        return Err("Not enough sensor-like axes to work with.".into());
    }
    // ── Accelerometer ──
    let change: Vec<f32> = (0..n)
        .map(|i| (c.nose_up[i] - c.flat[i]).abs().max((c.right_down[i] - c.flat_again[i]).abs()))
        .collect();
    let mut result = MotionResult::default();
    let mut accel_idx: Option<[usize; 3]> = None;
    let mut angles: Option<(f32, f32)> = None;
    if let Some(idx) = top_three(&change, &[], 0.35, 0.02) {
        let g_flat = v3(&c.flat, idx);
        let g_nose = v3(&c.nose_up, idx);
        let g_flat2 = v3(&c.flat_again, idx);
        let g_right = v3(&c.right_down, idx);
        let mags = [g_flat.length(), g_nose.length(), g_flat2.length(), g_right.length()];
        let g = mags.iter().sum::<f32>() / 4.0;
        if mags.iter().any(|m| (m - g).abs() > 0.3 * g) {
            return Err("Gravity didn't read the same in every pose — hold each one still \
                        until it turns green, and try again."
                .into());
        }
        let up = g_flat.normalize();
        let fwd = orthonormal(g_nose, &[up])
            .ok_or("Standing it on its grips looked like lying flat — tilt the nose all the way up.")?;
        let side = orthonormal(g_right, &[up, fwd])
            .ok_or("Lying on its right side looked like another pose — roll it all the way over.")?;
        result.accel = Some(ImuMap {
            src: idx.map(|i| c.srcs[i]),
            bias: [0.0; 3],
            // Rows: canonical x (forward), y (side), z (vertical), in g / bus range.
            matrix: rows([fwd, side, up], 1.0 / (g * imu::BUS_ACCEL_G)),
        });
        accel_idx = Some(idx);
        let angle = |a: Vec3, b: Vec3| a.angle_between(b).to_degrees();
        angles = Some((angle(g_flat, g_nose), angle(g_flat2, g_right)));
    }
    // ── Gyro ──
    let exclude: Vec<usize> = accel_idx.map(|a| a.to_vec()).unwrap_or_default();
    let activity: Vec<f32> = (0..n).map(|i| c.pitch[i].abs() + c.roll[i].abs() + c.yaw[i].abs()).collect();
    if let Some(idx) = top_three(&activity, &exclude, 0.25, 1e-3) {
        let (p, r, w) = (v3(&c.pitch, idx), v3(&c.roll, idx), v3(&c.yaw, idx));
        if p.length() < 1e-4 || r.length() < 1e-4 {
            return Err("The gyro didn't see the tilts — make them in one smooth movement.".into());
        }
        let (pitch_deg, roll_deg) = angles.unwrap_or((90.0, 90.0));
        let k = (pitch_deg / p.length() + roll_deg / r.length()) / 2.0; // degrees per reading·s
        let pitch = p.normalize();
        let roll = orthonormal(r, &[pitch]).ok_or("The roll turned about the same axis as the tilt.")?;
        let yaw = orthonormal(w, &[pitch, roll])
            .ok_or("The gyro didn't see the turn on the table — turn it right, flat, a quarter turn.")?;
        result.gyro = Some(ImuMap {
            src: idx.map(|i| c.srcs[i]),
            // A gyro rests at its bias: the flat pose before the turns.
            bias: idx.map(|i| c.flat[i]),
            // Rows: canonical x (roll), y (pitch), z (yaw), in °/s / bus range.
            matrix: rows([roll, pitch, yaw], k / imu::BUS_GYRO_DPS),
        });
    }
    if result.gyro.is_none() && result.accel.is_none() {
        return Err("No motion sensor showed up — none of these axes followed the poses.".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(src: RawSrc, pin: &str, rest: f32, full: f32) -> RawBind {
        RawBind { src, pin: pin.to_string(), rest, full, label: String::new() }
    }

    fn run(p: &RawPadProfile, axes: &[f32], buttons: &[bool], hats: &[Vec2]) -> HashMap<String, Signal> {
        let mut out = HashMap::new();
        p.compile().apply(&RawReading { axes, buttons, hats, reports: &() }, |id, s| {
            assert!(out.insert(id.to_string(), s).is_none(), "{id} emitted twice");
        });
        out
    }

    #[test]
    fn raw_pin_ids_round_trip() {
        let field = RawSrc::Field(HidField { report: 3, bit: 97, size: 12, min: -2048, max: 2047 });
        for src in [RawSrc::Axis(0), RawSrc::Button(127), RawSrc::Hat(3), field] {
            assert_eq!(RawSrc::from_pin_id(&src.pin_id()), Some(src));
            assert_eq!(RawSrc::from_extra_pin_id(&src.extra_pin_id()), Some(src));
        }
        assert_eq!(RawSrc::from_pin_id("raw:x1"), None);
        assert_eq!(RawSrc::from_pin_id("raw:b"), None);
        assert_eq!(RawSrc::from_pin_id("raw:f1.2.40.0.1"), None, "wider than 32 bits");
        assert_eq!(RawSrc::from_pin_id("midi:cc1"), None);
        assert_eq!(RawSrc::Button(0).label(), "Button 1");
        assert_eq!(RawSrc::Field(HidField::word(1, 12)).label(), "Report 1 · byte 12 (16-bit)");
    }

    #[test]
    fn hid_fields_read_little_endian_and_sign_extend() {
        let payload = [0x00, 0x34, 0x12, 0xFE, 0xFF, 0b0000_0100];
        assert_eq!(HidField::word(0, 1).raw(&payload), Some(0x1234));
        assert_eq!(HidField::word(0, 3).raw(&payload), Some(-2));
        assert_eq!(HidField::flag(0, 5, 2).value(&payload), Some(RawValue::Button(true)));
        assert_eq!(HidField::flag(0, 5, 3).value(&payload), Some(RawValue::Button(false)));
        // A 12-bit field straddling bytes, logical 0..4095.
        let f = HidField { report: 0, bit: 12, size: 12, min: 0, max: 4095 };
        assert_eq!(f.raw(&[0x00, 0xF0, 0xFF]), Some(4095));
        assert_eq!(f.value(&[0x00, 0xF0, 0xFF]), Some(RawValue::Axis(1.0)));
        assert_eq!(HidField::word(0, 5).raw(&payload), None, "past the end");
    }

    #[test]
    fn every_pin_a_bind_can_drive_is_canonical() {
        for pin in ["left_stick_x", "left_stick_y", "right_stick_x", "right_stick_y",
                    "dpad_x", "dpad_y", HAT_DPAD_PIN, "left_trigger", "right_trigger"]
            .into_iter().chain(GYRO_PINS).chain(ACCEL_PINS)
        {
            assert!(canonical(pin).is_some(), "{pin}");
        }
    }

    #[test]
    fn a_button_drives_its_pin() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Button(2), "btn_south", 0.0, 1.0));
        let out = run(&p, &[], &[false, false, true], &[]);
        assert_eq!(out.get("btn_south"), Some(&Signal::Bool(true)));
        assert_eq!(out.len(), 1, "nothing unbound is emitted: {out:?}");
    }

    #[test]
    fn a_stick_axis_bound_the_other_way_is_inverted_and_builds_the_vec2() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Axis(0), "left_stick_x", 0.0, 1.0));
        // DirectInput Y grows downward: learned by pushing up, which read −1.
        p.set(bind(RawSrc::Axis(1), "left_stick_y", 0.0, -1.0));
        let out = run(&p, &[0.5, -0.8], &[], &[]);
        assert_eq!(out.get("left_stick_x"), Some(&Signal::Float(0.5)));
        assert_eq!(out.get("left_stick_y"), Some(&Signal::Float(0.8)));
        assert_eq!(out.get("left_stick"), Some(&Signal::Vec2(Vec2::new(0.5, 0.8))));
        assert!(!out.contains_key("right_stick"));
    }

    #[test]
    fn a_trigger_resting_at_minus_one_reads_zero_to_one_and_fires_its_digital_twin() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Axis(4), "left_trigger", -1.0, 1.0));
        let rest = run(&p, &[0.0, 0.0, 0.0, 0.0, -1.0], &[], &[]);
        assert_eq!(rest.get("left_trigger"), Some(&Signal::Float(0.0)));
        assert_eq!(rest.get("btn_lt_dig"), Some(&Signal::Bool(false)));
        let half = run(&p, &[0.0, 0.0, 0.0, 0.0, 0.0], &[], &[]);
        assert_eq!(half.get("left_trigger"), Some(&Signal::Float(0.5)));
        assert_eq!(half.get("btn_lt_dig"), Some(&Signal::Bool(true)));
    }

    #[test]
    fn a_shared_z_axis_drives_both_triggers_with_its_halves() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Axis(2), "left_trigger", 0.0, 1.0));
        p.set(bind(RawSrc::Axis(2), "right_trigger", 0.0, -1.0));
        assert_eq!(p.binds.len(), 2, "opposite halves of one axis coexist");
        let out = run(&p, &[0.0, 0.0, -0.6], &[], &[]);
        assert_eq!(out.get("left_trigger"), Some(&Signal::Float(0.0)));
        assert_eq!(out.get("right_trigger"), Some(&Signal::Float(0.6)));
    }

    #[test]
    fn relearning_replaces_rather_than_stacks() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Button(0), "btn_south", 0.0, 1.0));
        p.set(bind(RawSrc::Button(1), "btn_south", 0.0, 1.0));
        assert_eq!(p.binds.len(), 1, "one source per pin");
        assert_eq!(p.binds[0].src, RawSrc::Button(1));
        p.set(bind(RawSrc::Button(1), "btn_east", 0.0, 1.0));
        assert_eq!(p.binds.len(), 1, "one pin per button");
        // An axis re-learned onto another stick replaces even the other way.
        p.set(bind(RawSrc::Axis(0), "left_stick_x", 0.0, 1.0));
        p.set(bind(RawSrc::Axis(0), "right_stick_x", 0.0, -1.0));
        assert_eq!(p.binds.iter().filter(|b| b.src == RawSrc::Axis(0)).count(), 1);
    }

    #[test]
    fn a_hat_is_the_whole_dpad() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Hat(0), HAT_DPAD_PIN, 0.0, 1.0));
        let out = run(&p, &[], &[], &[Vec2::new(1.0, 1.0)]);
        assert_eq!(out.get("dpad_up"), Some(&Signal::Bool(true)));
        assert_eq!(out.get("dpad_right"), Some(&Signal::Bool(true)));
        assert_eq!(out.get("dpad_down"), Some(&Signal::Bool(false)));
        let d = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(out.get("dpad"), Some(&Signal::Vec2(Vec2::new(d, d))));
        assert_eq!(out.get("dpad_x"), Some(&Signal::Float(d)));
    }

    #[test]
    fn a_dpad_on_axes_learned_as_buttons_builds_the_dpad() {
        // Cheap DirectInput pads report the D-pad as an X/Y axis pair.
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Axis(1), "dpad_up", 0.0, -1.0));
        p.set(bind(RawSrc::Axis(1), "dpad_down", 0.0, 1.0));
        assert_eq!(p.binds.len(), 2);
        let out = run(&p, &[0.0, -1.0], &[], &[]);
        assert_eq!(out.get("dpad_up"), Some(&Signal::Bool(true)));
        assert_eq!(out.get("dpad_down"), Some(&Signal::Bool(false)));
        assert_eq!(out.get("dpad"), Some(&Signal::Vec2(Vec2::new(0.0, 1.0))));
    }

    #[test]
    fn an_own_port_publishes_its_reading_on_its_extra_pin() {
        let mut p = RawPadProfile::default();
        p.set(RawBind {
            src: RawSrc::Button(9),
            pin: RawSrc::Button(9).extra_pin_id(),
            rest: 0.0,
            full: 1.0,
            label: "Turbo".into(),
        });
        let out = run(&p, &[], &[true; 10], &[]);
        assert_eq!(out.get("extra:b9"), Some(&Signal::Bool(true)));
        assert_eq!(out.len(), 1);
        assert_eq!(p.ports(), vec![("extra:b9".to_string(), "Turbo".to_string(), SignalType::Bool)]);
    }

    #[test]
    fn a_field_bind_reads_the_report() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Field(HidField::flag(2, 1, 4)), "btn_paddle_l1", 0.0, 1.0));
        p.set(bind(RawSrc::Field(HidField::byte(2, 2)), "right_trigger", -1.0, 1.0));
        let reports: HashMap<u8, Vec<u8>> = [(2, vec![0u8, 0b1_0000, 255])].into();
        let mut out = HashMap::new();
        p.compile().apply(&RawReading { axes: &[], buttons: &[], hats: &[], reports: &reports },
            |id, s| { out.insert(id.to_string(), s); });
        assert_eq!(out.get("btn_paddle_l1"), Some(&Signal::Bool(true)));
        assert_eq!(out.get("right_trigger"), Some(&Signal::Float(1.0)));
    }

    #[test]
    fn ports_add_the_stick_vec2_after_its_axes() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Axis(0), "left_stick_x", 0.0, 1.0));
        p.set(bind(RawSrc::Button(0), "btn_south", 0.0, 1.0));
        let ids: Vec<String> = p.ports().into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(ids, ["left_stick_x", "btn_south", "left_stick"]);
    }

    #[test]
    fn a_capture_that_never_moved_is_refused() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Axis(0), "left_trigger", 0.95, 1.0));
        assert!(p.compile().is_empty());
    }

    #[test]
    fn profiles_round_trip_through_json() {
        let mut p = RawPadProfile { name: "USB Gamepad".into(), ..Default::default() };
        p.set(bind(RawSrc::Axis(3), "right_stick_y", 0.0, -1.0));
        p.set(bind(RawSrc::Hat(0), HAT_DPAD_PIN, 0.0, 1.0));
        p.set(bind(RawSrc::Field(HidField::word(1, 12)), "left_trigger", 0.0, 1.0));
        p.set_motion(None, Some(ImuMap {
            src: [RawSrc::Axis(5), RawSrc::Axis(6), RawSrc::Axis(7)],
            bias: [0.0; 3],
            matrix: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }));
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<RawPadProfile>(&json).unwrap(), p);
    }

    #[test]
    fn learning_motion_frees_its_axes_and_forgetting_a_motion_pin_drops_the_sensor() {
        let mut p = RawPadProfile::default();
        p.set(bind(RawSrc::Axis(5), "left_trigger", 0.0, 1.0));
        let map = ImuMap {
            src: [RawSrc::Axis(5), RawSrc::Axis(6), RawSrc::Axis(7)],
            bias: [0.0; 3],
            matrix: [0.0; 9],
        };
        p.set_motion(Some(map), None);
        assert!(p.binds.is_empty(), "axis 5 moved over to the gyro");
        assert!(p.ports().iter().any(|(id, _, _)| id == "gyro_z"));
        assert!(p.remove_pin("gyro_y"));
        assert!(p.is_empty());
    }

    fn vals(v: &[(RawSrc, RawValue)]) -> Vec<(RawSrc, RawValue)> {
        v.to_vec()
    }

    #[test]
    fn learn_detects_the_first_element_to_leave_rest() {
        let base = vals(&[
            (RawSrc::Axis(0), RawValue::Axis(0.0)),
            (RawSrc::Axis(4), RawValue::Axis(-1.0)),
            (RawSrc::Button(0), RawValue::Button(false)),
            (RawSrc::Hat(0), RawValue::Hat(Vec2::ZERO)),
        ]);
        assert_eq!(detect(&base, &base), None);
        assert!(all_at_rest(&base, &base));

        // A small wobble is not a move.
        let mut now = base.clone();
        now[0].1 = RawValue::Axis(0.3);
        assert_eq!(detect(&base, &now), None);
        assert!(!all_at_rest(&base, &now), "0.3 is past the rest tolerance");

        // A trigger pulled from −1: taken with its rest and direction.
        now[0].1 = RawValue::Axis(0.0);
        now[1].1 = RawValue::Axis(0.2);
        let d = detect(&base, &now).unwrap();
        assert_eq!(d.src, RawSrc::Axis(4));
        assert_eq!(d.rest, -1.0);
        assert_eq!(d.direction(), 1.0);
        assert!(d.rests_at_end());

        // A button beats an axis moving in the same frame.
        now[2].1 = RawValue::Button(true);
        assert_eq!(detect(&base, &now).unwrap().src, RawSrc::Button(0));
    }

    #[test]
    fn a_button_held_when_learn_started_is_not_taken() {
        let base = vals(&[(RawSrc::Button(0), RawValue::Button(true))]);
        assert_eq!(detect(&base, &base), None);
        assert!(!all_at_rest(&base, &base), "still held: not at rest");
    }

    #[test]
    fn a_blob_byte_is_a_flag_or_an_axis() {
        let d = detect_blob_byte(1, 7, 0b0100_0000, 0b0110_0000).unwrap();
        assert_eq!(d.src, RawSrc::Field(HidField::flag(1, 7, 5)));
        assert_eq!(d.src.kind(), ElemKind::Button);
        let d = detect_blob_byte(1, 8, 0, 200).unwrap();
        assert_eq!(d.src.kind(), ElemKind::Axis);
        assert!(d.rests_at_end());
        assert_eq!(detect_blob_byte(1, 8, 10, 13), None, "two bits and a small swing: neither");
        assert_eq!(detect_blob_byte(1, 8, 10, 10), None);
    }

    /// A made-up pad: accel on raw axes 2, 5, 0 (permuted, signs flipped,
    /// 1 g = 0.25 — a ±4 g sensor), gyro on 3, 1, 6 at 0.002 per °/s with a
    /// bias, plus a stick that is never touched and a slider at rest.
    #[test]
    fn solve_motion_recovers_a_scrambled_sensor() {
        let srcs: Vec<RawSrc> = (0..8).map(RawSrc::Axis).collect();
        let g = 0.25;
        // Canonical accel (g) → raw: raw[2] = −x, raw[5] = z, raw[0] = y.
        let accel = |x: f32, y: f32, z: f32| {
            let mut v = vec![0.0f32; 8];
            v[2] = -x * g;
            v[5] = z * g;
            v[0] = y * g;
            v[3] = 0.01; v[1] = -0.02; v[6] = 0.005; // gyro bias
            v[7] = -1.0; // a slider resting at an end
            v
        };
        let (flat, nose_up, right_down) = (accel(0.0, 0.0, 1.0), accel(1.0, 0.0, 0.0), accel(0.0, 1.0, 0.0));
        // Canonical gyro (°·s over a turn) → raw integral: raw[3] = pitch,
        // raw[1] = −roll, raw[6] = yaw, at 0.002 per °/s.
        let k = 0.002;
        let turn = |roll: f32, pitch: f32, yaw: f32| {
            let mut v = vec![0.0f32; 8];
            v[3] = pitch * k;
            v[1] = -roll * k;
            v[6] = yaw * k;
            // The accelerometer swings during the tilts too; the solver must
            // not take it for a gyro.
            v[2] = 0.1;
            v
        };
        let cap = MotionCapture {
            srcs: srcs.clone(),
            flat: flat.clone(),
            nose_up,
            flat_again: flat.clone(),
            right_down,
            flat_yaw: flat.clone(),
            pitch: turn(0.0, 90.0, 0.0),
            roll: turn(90.0, 0.0, 0.0),
            yaw: turn(0.0, 0.0, 85.0),
        };
        let r = solve_motion(&cap).unwrap();

        let reading = |axes: &[f32]| -> (Option<[f32; 3]>, Option<[f32; 3]>) {
            let mut p = RawPadProfile::default();
            p.set_motion(r.gyro.clone(), r.accel.clone());
            p.compile().motion(&RawReading { axes, buttons: &[], hats: &[], reports: &() })
        };
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3);

        // Accel: flat reads +z, nose up +x, right side down +y — in bus units.
        let b = imu::BUS_ACCEL_G;
        assert!(close(reading(&accel(0.0, 0.0, 1.0)).1.unwrap(), [0.0, 0.0, 1.0 / b]));
        assert!(close(reading(&accel(1.0, 0.0, 0.0)).1.unwrap(), [1.0 / b, 0.0, 0.0]));
        assert!(close(reading(&accel(0.0, 1.0, 0.0)).1.unwrap(), [0.0, 1.0 / b, 0.0]));

        // Gyro: 100 °/s of roll, pitch and yaw land on x, y, z with bias removed.
        let rate = |roll: f32, pitch: f32, yaw: f32| {
            let mut v = accel(0.0, 0.0, 1.0);
            v[3] += pitch * k;
            v[1] += -roll * k;
            v[6] += yaw * k;
            v
        };
        let d = imu::BUS_GYRO_DPS;
        assert!(close(reading(&rate(100.0, 0.0, 0.0)).0.unwrap(), [100.0 / d, 0.0, 0.0]));
        assert!(close(reading(&rate(0.0, 100.0, 0.0)).0.unwrap(), [0.0, 100.0 / d, 0.0]));
        assert!(close(reading(&rate(0.0, 0.0, 100.0)).0.unwrap(), [0.0, 0.0, 100.0 / d]));
        let gyro_src = r.gyro.unwrap().src;
        for a in [1, 3, 6] {
            assert!(gyro_src.contains(&RawSrc::Axis(a)), "gyro on axis {a}: {gyro_src:?}");
        }
        let accel_src = r.accel.unwrap().src;
        for a in [0, 2, 5] {
            assert!(accel_src.contains(&RawSrc::Axis(a)), "accel on axis {a}: {accel_src:?}");
        }
    }

    #[test]
    fn solve_motion_says_why_when_nothing_moved() {
        let srcs: Vec<RawSrc> = (0..4).map(RawSrc::Axis).collect();
        let still = vec![0.1f32; 4];
        let zero = vec![0.0f32; 4];
        let cap = MotionCapture {
            srcs, flat: still.clone(), nose_up: still.clone(), flat_again: still.clone(),
            right_down: still.clone(), flat_yaw: still, pitch: zero.clone(), roll: zero.clone(), yaw: zero,
        };
        assert!(solve_motion(&cap).is_err());
    }
}
