//! Rumble for raw pads that speak the USB force-feedback protocol (HID PID),
//! sent straight to the device — no vendor driver, no DirectInput.
//!
//! Some older DirectInput pads (a Saitek P2600 Rumble Force Pad, for one)
//! drive their motors through the HID "Physical Interface Device" page: the
//! host creates an effect in one of the device's slots, sets its parameters,
//! and starts it. Windows ships a generic driver for that (`pid.dll`), but
//! for many such pads it registers no effects and DirectInput reports no
//! force feedback at all, so SDL can't rumble them. Their report descriptor
//! still describes every report, so this module builds them itself:
//!
//! * [`parse`] finds the reports by their PID collections and usages, not by
//!   fixed bytes, so any PID device's own layout is used.
//! * Only pads whose effects have **no force axes** are driven: a wheel or a
//!   joystick with force feedback declares axes and a direction, and a
//!   constant force there pushes the stick (on a direct-drive wheel, hard).
//!   A rumble pad has nothing to push, so the sign of a constant force is
//!   what picks its motor — which one, the rumble test on the device node
//!   finds out ([`RumbleMap`](flexinput_core::raw_pad::RumbleMap)).
//! * Writes go through a worker thread ([`PidRumble`]): one output report
//!   takes a USB interval (~16 ms on the P2600), far too long for the I/O
//!   loop. The worker keeps only the latest strengths, so it never falls
//!   behind a game's rumble stream.
//!
//! Verified on the P2600: effect slots count from 0, an "endless" duration
//! (0xFFFF, just past the logical range) plays nothing — so effects run for
//! two seconds at a time and are restarted every second while they should
//! play, which also silences the pad soon after FlexInput stops — and two
//! constant forces of opposite sign play both motors at once.

use std::collections::HashMap;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use flexinput_core::raw_pad::RumbleChannel;

/// The HID Physical Interface Device usage page.
const PID: u16 = 0x0F;

// Collections.
const SET_EFFECT_REPORT: u16 = 0x21;
const EFFECT_TYPE: u16 = 0x25;
const SET_ENVELOPE_REPORT: u16 = 0x5A;
const SET_CONSTANT_FORCE_REPORT: u16 = 0x73;
const EFFECT_OPERATION_REPORT: u16 = 0x77;
const BLOCK_LOAD_REPORT: u16 = 0x89;
const BLOCK_FREE_REPORT: u16 = 0x90;
const CREATE_NEW_EFFECT_REPORT: u16 = 0xAB;
// Fields.
const EFFECT_BLOCK_INDEX: u16 = 0x22;
const ET_CONSTANT_FORCE: u16 = 0x26;
const DURATION: u16 = 0x50;
const SAMPLE_PERIOD: u16 = 0x51;
const GAIN: u16 = 0x52;
const TRIGGER_BUTTON: u16 = 0x53;
const TRIGGER_REPEAT_INTERVAL: u16 = 0x54;
const AXES_ENABLE: u16 = 0x55;
const DIRECTION_ENABLE: u16 = 0x56;
const DIRECTION: u16 = 0x57;
const ATTACK_LEVEL: u16 = 0x5B;
const ATTACK_TIME: u16 = 0x5C;
const FADE_LEVEL: u16 = 0x5D;
const FADE_TIME: u16 = 0x5E;
const MAGNITUDE: u16 = 0x70;
const OP_EFFECT_START: u16 = 0x79;
const OP_EFFECT_STOP: u16 = 0x7B;
const LOOP_COUNT: u16 = 0x7C;
const DEVICE_GAIN: u16 = 0x7E;
const BLOCK_LOAD_SUCCESS: u16 = 0x8C;
const DC_ENABLE_ACTUATORS: u16 = 0x97;
const DC_STOP_ALL_EFFECTS: u16 = 0x99;
const DC_DEVICE_RESET: u16 = 0x9A;
const DC_DEVICE_CONTINUE: u16 = 0x9C;
const START_DELAY: u16 = 0xA7;

/// How long the rumble test plays one channel.
pub const PROBE_MS: u64 = 1500;

/// How long one start of an effect plays. Effects are restarted at half this
/// while they should keep playing, so if FlexInput quits or crashes the pad
/// falls silent within it — with the field's longest duration (65 s on the
/// P2600) a crash mid-rumble would leave it buzzing for a minute.
const EFFECT_RUN: Duration = Duration::from_secs(2);

// ── Descriptor ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Input,
    Output,
    Feature,
}

/// One main item of a report descriptor, as far as PID needs it.
#[derive(Clone, Debug)]
struct Item {
    kind: Kind,
    report: u8,
    /// Bit offset into the report's payload (after the id byte).
    bit: u32,
    size: u32,
    min: i32,
    max: i32,
    /// The unit exponent in force for it (a power of ten).
    exp: i32,
    /// A variable: its one usage. An array: every usage it can select, in
    /// index order (the field holds `min + index`).
    usages: Vec<(u16, u16)>,
    array: bool,
    /// The collections it sits in, outermost first.
    colls: Vec<(u16, u16)>,
}

impl Item {
    fn in_coll(&self, usage: u16) -> bool {
        self.colls.contains(&(PID, usage))
    }
}

/// Every data item of a descriptor (constant padding skipped), and the
/// payload length in bytes of each `(kind, report id)`.
fn parse_items(desc: &[u8]) -> (Vec<Item>, HashMap<(Kind, u8), usize>, bool) {
    #[derive(Clone, Copy, Default)]
    struct Globals {
        page: u16,
        min: i32,
        max: i32,
        exp: i32,
        size: u32,
        count: u32,
        report: u8,
    }
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<(u16, u16)> = Vec::new();
    let mut umin: Option<(u16, u16)> = None;
    let mut umax: Option<u16> = None;
    let mut colls: Vec<(u16, u16)> = Vec::new();
    let mut bits: HashMap<(Kind, u8), u32> = HashMap::new();
    let mut numbered = false;
    let mut out = Vec::new();

    let mut i = 0usize;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            let len = desc.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + len;
            continue;
        }
        let len = match prefix & 3 { 0 => 0, 1 => 1, 2 => 2, _ => 4 };
        let Some(data) = desc.get(i + 1..i + 1 + len) else { break };
        i += 1 + len;
        let mut u: u32 = 0;
        for (k, b) in data.iter().enumerate() {
            u |= (*b as u32) << (8 * k);
        }
        let s: i32 = match len {
            1 => u as u8 as i8 as i32,
            2 => u as u16 as i16 as i32,
            4 => u as i32,
            _ => 0,
        };
        // The locals' usage list, ranges expanded (capped: a 0..0xFFFF button
        // range is no PID field).
        let local_usages = |usages: &Vec<(u16, u16)>, umin: Option<(u16, u16)>, umax: Option<u16>| {
            let mut list = usages.clone();
            if let Some((page, lo)) = umin {
                let hi = umax.unwrap_or(lo).max(lo);
                list.extend((lo..=hi).take(256).map(|x| (page, x)));
            }
            list
        };
        let (typ, tag) = ((prefix >> 2) & 3, prefix >> 4);
        match (typ, tag) {
            (0, 8) | (0, 9) | (0, 11) => {
                let kind = match tag { 8 => Kind::Input, 9 => Kind::Output, _ => Kind::Feature };
                let constant = u & 1 != 0;
                let variable = u & 2 != 0;
                let list = local_usages(&usages, umin, umax);
                let off = bits.entry((kind, g.report)).or_insert(0);
                if !constant && g.size > 0 && g.size <= 32 && !list.is_empty() {
                    if variable {
                        for n in 0..g.count {
                            out.push(Item {
                                kind,
                                report: g.report,
                                bit: *off + n * g.size,
                                size: g.size,
                                min: g.min,
                                max: g.max,
                                exp: g.exp,
                                usages: vec![list[(n as usize).min(list.len() - 1)]],
                                array: false,
                                colls: colls.clone(),
                            });
                        }
                    } else {
                        // An array: `count` slots, each selecting one usage.
                        // PID's selectors have one slot; the first is enough.
                        out.push(Item {
                            kind,
                            report: g.report,
                            bit: *off,
                            size: g.size,
                            min: g.min,
                            max: g.max,
                            exp: g.exp,
                            usages: list,
                            array: true,
                            colls: colls.clone(),
                        });
                    }
                }
                *off += g.count * g.size;
                usages.clear();
                umin = None;
                umax = None;
            }
            (0, 10) => {
                // Collection: named by the usage before it.
                let named = local_usages(&usages, umin, umax).first().copied().unwrap_or((g.page, 0));
                colls.push(named);
                usages.clear();
                umin = None;
                umax = None;
            }
            (0, 12) => {
                colls.pop();
                usages.clear();
                umin = None;
                umax = None;
            }
            (1, 0) => g.page = u as u16,
            (1, 1) => g.min = s,
            (1, 2) => g.max = if g.min >= 0 && s < g.min { u.min(i32::MAX as u32) as i32 } else { s },
            // Unit exponent: a 4-bit two's-complement nibble by the spec, but
            // descriptors (and Windows' reconstruction) also write a full
            // signed value.
            (1, 5) => g.exp = if (8..=15).contains(&s) { s - 16 } else { s },
            (1, 7) => g.size = u,
            (1, 8) => {
                g.report = u as u8;
                numbered = true;
            }
            (1, 9) => g.count = u,
            (1, 10) => stack.push(g),
            (1, 11) => {
                if let Some(p) = stack.pop() {
                    g = p;
                }
            }
            (2, 0) => usages.push(if len == 4 { ((u >> 16) as u16, u as u16) } else { (g.page, u as u16) }),
            (2, 1) => umin = Some((if len == 4 { (u >> 16) as u16 } else { g.page }, u as u16)),
            (2, 2) => umax = Some(u as u16),
            _ => {}
        }
    }
    let lens = bits.into_iter().map(|(k, b)| (k, b.div_ceil(8) as usize)).collect();
    (out, lens, numbered)
}

/// A variable field of one report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Var {
    report: u8,
    bit: u32,
    size: u32,
    min: i32,
    max: i32,
    exp: i32,
}

/// An array field: selects one of its usages.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sel {
    report: u8,
    bit: u32,
    size: u32,
    min: i32,
    usages: Vec<u16>,
}

impl Sel {
    fn value_of(&self, usage: u16) -> Option<i64> {
        self.usages.iter().position(|u| *u == usage).map(|i| self.min as i64 + i as i64)
    }

    fn usage_of(&self, value: i64) -> Option<u16> {
        usize::try_from(value - self.min as i64).ok().and_then(|i| self.usages.get(i).copied())
    }
}

#[derive(Clone, Debug)]
struct SetEffect {
    block: Var,
    duration: Option<Var>,
    trigger_button: Option<Var>,
    trigger_repeat: Option<Var>,
    start_delay: Option<Var>,
    sample_period: Option<Var>,
    gain: Option<Var>,
    effect_type: Option<Sel>,
}

#[derive(Clone, Debug)]
struct Envelope {
    block: Var,
    attack_level: Option<Var>,
    attack_time: Option<Var>,
    fade_level: Option<Var>,
    fade_time: Option<Var>,
}

/// Where a PID device keeps everything constant-force rumble needs.
#[derive(Clone, Debug)]
pub struct PidLayout {
    out_len: HashMap<u8, usize>,
    feat_len: HashMap<u8, usize>,
    control: Sel,
    device_gain: Option<Var>,
    create_type: Sel,
    load_block: Var,
    load_status: Option<Sel>,
    effect: SetEffect,
    envelope: Option<Envelope>,
    constant_block: Var,
    magnitude: Var,
    op_block: Var,
    op: Sel,
    loop_count: Option<Var>,
    free_block: Option<Var>,
    /// The device's effects have force axes or a direction: a wheel or a
    /// joystick, not a rumble pad. Never driven ([`parse`] keeps it so the
    /// log can say why).
    pub force_axes: bool,
}

/// The constant-force PID interface a descriptor declares, or `None` when it
/// declares none (or not enough of one to play an effect).
pub fn parse(desc: &[u8]) -> Option<PidLayout> {
    let (items, lens, numbered) = parse_items(desc);
    if !numbered {
        return None;
    }
    let var_in = |kind: Kind, coll: u16, usage: u16| {
        items.iter()
            .find(|it| it.kind == kind && !it.array && it.usages[0] == (PID, usage) && it.in_coll(coll))
            .map(to_var)
    };
    let var_any = |kind: Kind, usage: u16| {
        items.iter().find(|it| it.kind == kind && !it.array && it.usages[0] == (PID, usage)).map(to_var)
    };
    let sel_with = |kind: Kind, member: u16, coll: Option<u16>| {
        items.iter()
            .find(|it| {
                it.kind == kind && it.array && it.usages.contains(&(PID, member)) && coll.is_none_or(|c| it.in_coll(c))
            })
            .map(to_sel)
    };

    let effect = SetEffect {
        block: var_in(Kind::Output, SET_EFFECT_REPORT, EFFECT_BLOCK_INDEX)?,
        duration: var_in(Kind::Output, SET_EFFECT_REPORT, DURATION),
        trigger_button: var_in(Kind::Output, SET_EFFECT_REPORT, TRIGGER_BUTTON),
        trigger_repeat: var_in(Kind::Output, SET_EFFECT_REPORT, TRIGGER_REPEAT_INTERVAL),
        start_delay: var_in(Kind::Output, SET_EFFECT_REPORT, START_DELAY),
        sample_period: var_in(Kind::Output, SET_EFFECT_REPORT, SAMPLE_PERIOD),
        gain: var_in(Kind::Output, SET_EFFECT_REPORT, GAIN),
        effect_type: sel_with(Kind::Output, ET_CONSTANT_FORCE, Some(SET_EFFECT_REPORT)),
    };
    let force_axes = items.iter().any(|it| {
        it.kind == Kind::Output
            && it.in_coll(SET_EFFECT_REPORT)
            && (it.in_coll(AXES_ENABLE)
                || it.in_coll(DIRECTION)
                || it.usages.iter().any(|&(p, u)| p == PID && matches!(u, AXES_ENABLE | DIRECTION_ENABLE | DIRECTION)))
    });
    let envelope = var_in(Kind::Output, SET_ENVELOPE_REPORT, EFFECT_BLOCK_INDEX).map(|block| Envelope {
        block,
        attack_level: var_in(Kind::Output, SET_ENVELOPE_REPORT, ATTACK_LEVEL),
        attack_time: var_in(Kind::Output, SET_ENVELOPE_REPORT, ATTACK_TIME),
        fade_level: var_in(Kind::Output, SET_ENVELOPE_REPORT, FADE_LEVEL),
        fade_time: var_in(Kind::Output, SET_ENVELOPE_REPORT, FADE_TIME),
    });
    let split = |kind: Kind| {
        lens.iter().filter(|((k, _), _)| *k == kind).map(|((_, id), n)| (*id, *n)).collect::<HashMap<u8, usize>>()
    };
    Some(PidLayout {
        out_len: split(Kind::Output),
        feat_len: split(Kind::Feature),
        control: sel_with(Kind::Output, DC_DEVICE_RESET, None)
            .or_else(|| sel_with(Kind::Output, DC_ENABLE_ACTUATORS, None))?,
        device_gain: var_any(Kind::Output, DEVICE_GAIN),
        create_type: sel_with(Kind::Feature, ET_CONSTANT_FORCE, Some(CREATE_NEW_EFFECT_REPORT))
            .or_else(|| sel_with(Kind::Feature, ET_CONSTANT_FORCE, Some(EFFECT_TYPE)))?,
        load_block: var_in(Kind::Feature, BLOCK_LOAD_REPORT, EFFECT_BLOCK_INDEX)?,
        load_status: sel_with(Kind::Feature, BLOCK_LOAD_SUCCESS, None),
        effect,
        envelope,
        constant_block: var_in(Kind::Output, SET_CONSTANT_FORCE_REPORT, EFFECT_BLOCK_INDEX)?,
        magnitude: var_in(Kind::Output, SET_CONSTANT_FORCE_REPORT, MAGNITUDE)?,
        op_block: var_in(Kind::Output, EFFECT_OPERATION_REPORT, EFFECT_BLOCK_INDEX)?,
        op: sel_with(Kind::Output, OP_EFFECT_START, None)?,
        loop_count: var_in(Kind::Output, EFFECT_OPERATION_REPORT, LOOP_COUNT),
        free_block: var_in(Kind::Output, BLOCK_FREE_REPORT, EFFECT_BLOCK_INDEX),
        force_axes,
    })
}

fn to_var(it: &Item) -> Var {
    Var { report: it.report, bit: it.bit, size: it.size, min: it.min, max: it.max, exp: it.exp }
}

fn to_sel(it: &Item) -> Sel {
    Sel {
        report: it.report,
        bit: it.bit,
        size: it.size,
        min: it.min,
        usages: it.usages.iter().map(|(_, u)| *u).collect(),
    }
}

// ── Reports ───────────────────────────────────────────────────────────────────

/// Write `size` bits of `value` (two's complement) at `bit` of the payload.
fn put(buf: &mut [u8], bit: u32, size: u32, value: i64) {
    let v = value as u64;
    for i in 0..size {
        let b = (bit + i) as usize;
        let Some(byte) = buf.get_mut(1 + b / 8) else { return };
        if (v >> i) & 1 == 1 {
            *byte |= 1 << (b % 8);
        } else {
            *byte &= !(1 << (b % 8));
        }
    }
}

fn get(buf: &[u8], bit: u32, size: u32, signed: bool) -> Option<i64> {
    let mut v: u64 = 0;
    for i in 0..size {
        let b = (bit + i) as usize;
        if (buf.get(1 + b / 8)? >> (b % 8)) & 1 == 1 {
            v |= 1 << i;
        }
    }
    Some(if signed && size > 0 && (v >> (size - 1)) & 1 == 1 { v as i64 - (1i64 << size) } else { v as i64 })
}

impl PidLayout {
    /// The channels this device can play: both signs when its magnitude is
    /// signed, else only positive.
    pub fn channels(&self) -> Vec<RumbleChannel> {
        if self.magnitude.min < 0 {
            vec![RumbleChannel::Positive, RumbleChannel::Negative]
        } else {
            vec![RumbleChannel::Positive]
        }
    }

    fn out_report(&self, id: u8) -> Vec<u8> {
        let mut b = vec![0u8; 1 + self.out_len.get(&id).copied().unwrap_or(0)];
        b[0] = id;
        b
    }

    fn feat_report(&self, id: u8) -> Vec<u8> {
        let mut b = vec![0u8; 1 + self.feat_len.get(&id).copied().unwrap_or(0)];
        b[0] = id;
        b
    }

    /// A Device Control report, or `None` if the device lacks that command.
    fn control(&self, command: u16) -> Option<Vec<u8>> {
        let v = self.control.value_of(command)?;
        let mut b = self.out_report(self.control.report);
        put(&mut b, self.control.bit, self.control.size, v);
        Some(b)
    }

    fn device_gain_max(&self) -> Option<Vec<u8>> {
        let g = self.device_gain?;
        let mut b = self.out_report(g.report);
        put(&mut b, g.bit, g.size, g.max as i64);
        Some(b)
    }

    /// Create New Effect (a feature report): a constant force.
    fn create_constant(&self) -> Vec<u8> {
        let s = &self.create_type;
        let mut b = self.feat_report(s.report);
        put(&mut b, s.bit, s.size, s.value_of(ET_CONSTANT_FORCE).unwrap_or(s.min as i64));
        b
    }

    /// An empty Block Load feature report, to read the device's answer into.
    fn block_load_query(&self) -> Vec<u8> {
        let mut b = self.feat_report(self.load_block.report);
        // Room for whatever the device actually sends back.
        b.resize(b.len().max(64), 0);
        b
    }

    /// The slot a Block Load reply hands out; `None` if the device said the
    /// pool is full or the load failed.
    fn block_from_load(&self, reply: &[u8]) -> Option<u32> {
        if let Some(s) = &self.load_status {
            let status = get(reply, s.bit, s.size, false)?;
            if s.usage_of(status) != Some(BLOCK_LOAD_SUCCESS) {
                return None;
            }
        }
        let v = self.load_block;
        get(reply, v.bit, v.size, false).map(|x| x as u32)
    }

    /// How long one start of an effect plays, raw and in real time:
    /// [`EFFECT_RUN`], or the longest the field can say if that is shorter.
    /// No duration field: the device decides (taken as until stopped).
    fn effect_duration(&self) -> Option<(i64, Duration)> {
        let d = self.effect.duration?;
        // PID durations are seconds × 10^exp; an exponent of 0 in a
        // descriptor that set none means the conventional milliseconds.
        let exp = if d.exp == 0 { -3 } else { d.exp.clamp(-9, 9) };
        let unit = 10u128.pow((9 + exp) as u32); // nanoseconds per raw unit
        let want = EFFECT_RUN.as_nanos().div_ceil(unit).max(1);
        let raw = want.min(d.max.clamp(1, 0xFFFF) as u128);
        let real = Duration::from_nanos((raw * unit).min(3_600_000_000_000) as u64);
        Some((raw as i64, real.max(Duration::from_millis(50))))
    }

    /// Set Effect for a constant force in `block`: [`Self::effect_duration`],
    /// no trigger button, no delay, full gain.
    fn set_effect(&self, block: u32) -> Vec<u8> {
        let e = &self.effect;
        let mut b = self.out_report(e.block.report);
        put(&mut b, e.block.bit, e.block.size, block as i64);
        if let (Some(d), Some((raw, _))) = (e.duration, self.effect_duration()) {
            put(&mut b, d.bit, d.size, raw);
        }
        if let Some(t) = e.trigger_button {
            // "No button" is a value outside the button range.
            let none = if t.min > 0 {
                0
            } else if (t.max as i64) < (1i64 << t.size) - 1 {
                t.max as i64 + 1
            } else {
                (1i64 << t.size) - 1
            };
            put(&mut b, t.bit, t.size, none);
        }
        for v in [e.trigger_repeat, e.start_delay, e.sample_period].into_iter().flatten() {
            put(&mut b, v.bit, v.size, 0);
        }
        if let Some(g) = e.gain {
            put(&mut b, g.bit, g.size, g.max as i64);
        }
        if let Some(t) = &e.effect_type {
            put(&mut b, t.bit, t.size, t.value_of(ET_CONSTANT_FORCE).unwrap_or(t.min as i64));
        }
        b
    }

    /// A flat envelope: full levels, no attack or fade.
    fn set_envelope(&self, block: u32) -> Option<Vec<u8>> {
        let e = self.envelope.as_ref()?;
        let mut b = self.out_report(e.block.report);
        put(&mut b, e.block.bit, e.block.size, block as i64);
        for v in [e.attack_level, e.fade_level].into_iter().flatten() {
            put(&mut b, v.bit, v.size, v.max as i64);
        }
        for v in [e.attack_time, e.fade_time].into_iter().flatten() {
            put(&mut b, v.bit, v.size, 0);
        }
        Some(b)
    }

    /// The magnitude field's value for `level` (0..1) on `channel`.
    fn magnitude_raw(&self, channel: RumbleChannel, level: f32) -> i64 {
        let m = self.magnitude;
        let level = level.clamp(0.0, 1.0);
        match channel {
            RumbleChannel::Positive => (level * m.max.max(0) as f32).round() as i64,
            RumbleChannel::Negative => -((level * m.min.min(0).unsigned_abs() as f32).round() as i64),
        }
    }

    fn set_constant(&self, block: u32, raw: i64) -> Vec<u8> {
        let mut b = self.out_report(self.constant_block.report);
        put(&mut b, self.constant_block.bit, self.constant_block.size, block as i64);
        put(&mut b, self.magnitude.bit, self.magnitude.size, raw);
        b
    }

    fn operation(&self, block: u32, start: bool) -> Option<Vec<u8>> {
        let op = self.op.value_of(if start { OP_EFFECT_START } else { OP_EFFECT_STOP })?;
        let mut b = self.out_report(self.op_block.report);
        put(&mut b, self.op_block.bit, self.op_block.size, block as i64);
        put(&mut b, self.op.bit, self.op.size, op);
        if let Some(l) = self.loop_count {
            put(&mut b, l.bit, l.size, if start { 1 } else { 0 });
        }
        Some(b)
    }

    fn block_free(&self, block: u32) -> Option<Vec<u8>> {
        let f = self.free_block?;
        let mut b = self.out_report(f.report);
        put(&mut b, f.bit, f.size, block as i64);
        Some(b)
    }
}

// ── Driving ───────────────────────────────────────────────────────────────────

/// The device side of the driver: output reports and feature reports, each
/// buffer starting with its report id.
pub(crate) trait PidIo: Send {
    fn write(&mut self, report: &[u8]) -> Result<(), String>;
    fn send_feature(&mut self, report: &[u8]) -> Result<(), String>;
    /// `buf[0]` holds the report id; the reply lands in `buf`.
    fn get_feature(&mut self, buf: &mut [u8]) -> Result<usize, String>;
}

impl PidIo for hidapi::HidDevice {
    fn write(&mut self, report: &[u8]) -> Result<(), String> {
        hidapi::HidDevice::write(self, report).map(|_| ()).map_err(|e| e.to_string())
    }

    fn send_feature(&mut self, report: &[u8]) -> Result<(), String> {
        self.send_feature_report(report).map_err(|e| e.to_string())
    }

    fn get_feature(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        self.get_feature_report(buf).map_err(|e| e.to_string())
    }
}

fn channel_index(c: RumbleChannel) -> usize {
    match c {
        RumbleChannel::Positive => 0,
        RumbleChannel::Negative => 1,
    }
}

const CHANNELS: [RumbleChannel; 2] = [RumbleChannel::Positive, RumbleChannel::Negative];

#[derive(Clone, Copy, Debug)]
struct Slot {
    block: u32,
    /// The magnitude last written, raw.
    raw: i64,
    playing: bool,
    started: Instant,
}

/// After this many failed writes in a row the device is taken as gone.
const MAX_FAILURES: u32 = 20;

/// The PID state machine of one device: one constant-force effect per
/// channel, created the first time that channel plays.
struct Driver<I: PidIo> {
    io: I,
    layout: PidLayout,
    dev_id: String,
    ready: bool,
    slots: [Option<Slot>; 2],
    /// A channel whose effect couldn't be created stays silent.
    unavailable: [bool; 2],
    /// Restart a playing effect this often, well before its duration ends.
    rearm: Option<Duration>,
    failures: u32,
    logged_failure: bool,
}

impl<I: PidIo> Driver<I> {
    fn new(io: I, layout: PidLayout, dev_id: &str) -> Self {
        let rearm = layout.effect_duration().map(|(_, d)| d / 2);
        let available = layout.channels();
        let unavailable = CHANNELS.map(|c| !available.contains(&c));
        Driver {
            io,
            layout,
            dev_id: dev_id.to_string(),
            ready: false,
            slots: [None, None],
            unavailable,
            rearm,
            failures: 0,
            logged_failure: false,
        }
    }

    fn gone(&self) -> bool {
        self.failures >= MAX_FAILURES
    }

    fn note(&mut self, what: &str, r: Result<(), String>) -> bool {
        match r {
            Ok(()) => {
                self.failures = 0;
                true
            }
            Err(e) => {
                self.failures += 1;
                if !self.logged_failure {
                    eprintln!("[pid] {}: {what} failed ({e})", self.dev_id);
                    self.logged_failure = true;
                }
                false
            }
        }
    }

    fn write(&mut self, what: &str, report: Option<Vec<u8>>) -> bool {
        let Some(r) = report else { return false };
        let res = self.io.write(&r);
        self.note(what, res)
    }

    /// Reset the device and switch its actuators on, the first time anything
    /// plays.
    fn init(&mut self) {
        if self.ready {
            return;
        }
        self.ready = true;
        self.write("device reset", self.layout.control(DC_DEVICE_RESET));
        std::thread::sleep(Duration::from_millis(50));
        self.write("enable actuators", self.layout.control(DC_ENABLE_ACTUATORS));
        self.write("device continue", self.layout.control(DC_DEVICE_CONTINUE));
        self.write("device gain", self.layout.device_gain_max());
        eprintln!("[pid] {}: force-feedback rumble ready", self.dev_id);
    }

    /// The channel's effect, created and set up on first use.
    fn slot(&mut self, i: usize, now: Instant) -> Option<Slot> {
        if let Some(s) = self.slots[i] {
            return Some(s);
        }
        if self.unavailable[i] {
            return None;
        }
        let create = self.layout.create_constant();
        let res = self.io.send_feature(&create);
        if !self.note("create effect", res) {
            return None;
        }
        let mut reply = self.layout.block_load_query();
        let block = match self.io.get_feature(&mut reply) {
            Ok(_) => self.layout.block_from_load(&reply),
            Err(e) => {
                self.note("block load", Err(e));
                None
            }
        };
        let Some(block) = block else {
            eprintln!("[pid] {}: the device has no effect slot for {:?} — that channel stays silent", self.dev_id, CHANNELS[i]);
            self.unavailable[i] = true;
            return None;
        };
        self.write("set effect", Some(self.layout.set_effect(block)));
        let envelope = self.layout.set_envelope(block);
        if envelope.is_some() {
            self.write("set envelope", envelope);
        }
        let s = Slot { block, raw: 0, playing: false, started: now };
        self.slots[i] = Some(s);
        Some(s)
    }

    /// Play each channel at its level (0..1), index 0 positive, 1 negative.
    fn apply(&mut self, levels: [f32; 2], now: Instant) {
        if levels.iter().any(|l| *l > 0.0) {
            self.init();
        }
        for (i, &channel) in CHANNELS.iter().enumerate() {
            let raw = self.layout.magnitude_raw(channel, levels[i]);
            if raw == 0 {
                if let Some(mut s) = self.slots[i] {
                    if s.playing {
                        self.write("effect stop", self.layout.operation(s.block, false));
                        s.playing = false;
                    }
                    s.raw = 0;
                    self.slots[i] = Some(s);
                }
                continue;
            }
            let Some(mut s) = self.slot(i, now) else { continue };
            if s.raw != raw {
                self.write("set magnitude", Some(self.layout.set_constant(s.block, raw)));
                s.raw = raw;
            }
            let due = s.playing && self.rearm.is_some_and(|r| now.duration_since(s.started) >= r);
            if !s.playing || due {
                self.write("effect start", self.layout.operation(s.block, true));
                s.playing = true;
                s.started = now;
            }
            self.slots[i] = Some(s);
        }
    }

    /// Stop everything and give the slots back.
    fn shutdown(&mut self) {
        if !self.ready {
            return;
        }
        for s in self.slots.iter().flatten().copied().collect::<Vec<_>>() {
            if s.playing {
                self.write("effect stop", self.layout.operation(s.block, false));
            }
            self.write("block free", self.layout.block_free(s.block));
        }
        self.slots = [None, None];
        self.write("stop all effects", self.layout.control(DC_STOP_ALL_EFFECTS));
    }
}

enum Cmd {
    /// Channel levels, 0..1, index 0 positive and 1 negative.
    Drive([f32; 2]),
    /// The rumble test: play one channel at full strength for a moment,
    /// over whatever is being driven.
    Probe(RumbleChannel),
}

fn run<I: PidIo>(mut driver: Driver<I>, rx: mpsc::Receiver<Cmd>, gen: u64) {
    let mut levels = [0.0f32; 2];
    let mut probe: Option<(RumbleChannel, Instant)> = None;
    loop {
        let now = Instant::now();
        let wait = match probe {
            Some((_, until)) => until.saturating_duration_since(now).min(Duration::from_millis(100)),
            None => Duration::from_millis(250),
        };
        let first = match rx.recv_timeout(wait) {
            Ok(cmd) => Some(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // Only the latest strengths matter: catch up on everything queued.
        for cmd in first.into_iter().chain(rx.try_iter()) {
            match cmd {
                Cmd::Drive(l) => levels = l,
                Cmd::Probe(c) => probe = Some((c, Instant::now() + Duration::from_millis(PROBE_MS))),
            }
        }
        let now = Instant::now();
        if probe.is_some_and(|(_, until)| now >= until) {
            probe = None;
        }
        let target = match probe {
            Some((c, _)) => {
                let mut t = [0.0; 2];
                t[channel_index(c)] = 1.0;
                t
            }
            None => levels,
        };
        driver.apply(target, now);
        if driver.gone() {
            eprintln!("[pid] {}: the device stopped answering — rumble off", driver.dev_id);
            break;
        }
    }
    driver.shutdown();
    unregister(&driver.dev_id, gen);
}

// ── The worker, and what the UI can ask of it ─────────────────────────────────

struct Entry {
    gen: u64,
    tx: mpsc::Sender<Cmd>,
    channels: Vec<RumbleChannel>,
}

static REGISTRY: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);
static NEXT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn unregister(dev_id: &str, gen: u64) {
    if let Ok(mut g) = REGISTRY.lock() {
        if let Some(m) = g.as_mut() {
            if m.get(dev_id).is_some_and(|e| e.gen == gen) {
                m.remove(dev_id);
            }
        }
    }
}

/// The force-feedback channels a connected raw pad's rumble test can play;
/// empty when it has no PID rumble interface.
pub fn rumble_channels(dev_id: &str) -> Vec<RumbleChannel> {
    REGISTRY.lock().ok()
        .and_then(|g| g.as_ref()?.get(dev_id).map(|e| e.channels.clone()))
        .unwrap_or_default()
}

/// Play one channel at full strength for [`PROBE_MS`] (the rumble test).
/// False when the pad has no PID rumble interface.
pub fn probe(dev_id: &str, channel: RumbleChannel) -> bool {
    REGISTRY.lock().ok()
        .and_then(|g| g.as_ref()?.get(dev_id).map(|e| e.tx.send(Cmd::Probe(channel)).is_ok()))
        .unwrap_or(false)
}

/// A raw pad's PID rumble worker. Dropping it stops the motors and ends the
/// thread.
pub(crate) struct PidRumble {
    tx: mpsc::Sender<Cmd>,
    dev_id: String,
    gen: u64,
}

impl PidRumble {
    pub fn spawn(dev_id: &str, layout: PidLayout, dev: hidapi::HidDevice) -> Option<Self> {
        let gen = NEXT_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let channels = layout.channels();
        let driver = Driver::new(dev, layout, dev_id);
        std::thread::Builder::new()
            .name("pid-rumble".into())
            .spawn(move || run(driver, rx, gen))
            .ok()?;
        if let Ok(mut g) = REGISTRY.lock() {
            g.get_or_insert_with(HashMap::new)
                .insert(dev_id.to_string(), Entry { gen, tx: tx.clone(), channels: channels.clone() });
        }
        eprintln!("[pid] {dev_id}: USB force-feedback interface found — rumble test available ({channels:?})");
        Some(PidRumble { tx, dev_id: dev_id.to_string(), gen })
    }

    /// Play each channel at its level (0..1): index 0 positive, 1 negative.
    pub fn drive(&self, levels: [f32; 2]) {
        let _ = self.tx.send(Cmd::Drive(levels));
    }
}

impl Drop for PidRumble {
    fn drop(&mut self) {
        unregister(&self.dev_id, self.gen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Saitek P2600 Rumble Force Pad (06A3:5F0D) as hidapi hands it over on
    /// Windows (reconstructed from the preparsed data).
    const P2600: &[u8] = &[
        0x05, 0x01, 0x09, 0x04, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31,
        0x15, 0x00, 0x26, 0xFF, 0x00, 0x35, 0x00, 0x46, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
        0x05, 0x02, 0x09, 0xBA, 0x09, 0xBB, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81,
        0x02, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0C, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0C, 0x45,
        0x00, 0x81, 0x02, 0x05, 0x01, 0x09, 0x39, 0x15, 0x00, 0x25, 0x07, 0x35, 0x00, 0x46, 0x3B, 0x01,
        0x65, 0x44, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0xC0, 0x05, 0x0F, 0x09, 0x92, 0xA1, 0x02, 0x85,
        0x02, 0x09, 0xA0, 0x09, 0x9F, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x45, 0x00, 0x65,
        0x00, 0x81, 0x02, 0x75, 0x06, 0x95, 0x01, 0x81, 0x03, 0x09, 0x22, 0x15, 0x00, 0x25, 0x7F, 0x75,
        0x07, 0x95, 0x01, 0x81, 0x02, 0x09, 0x94, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81,
        0x02, 0xC0, 0x09, 0x21, 0xA1, 0x02, 0x85, 0x0B, 0x09, 0x22, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75,
        0x08, 0x95, 0x01, 0x91, 0x02, 0x09, 0x53, 0x15, 0x00, 0x25, 0x0A, 0x75, 0x08, 0x95, 0x01, 0x91,
        0x02, 0x09, 0x50, 0x09, 0x54, 0x09, 0xA7, 0x15, 0x00, 0x27, 0xFE, 0xFF, 0x00, 0x00, 0x35, 0x00,
        0x47, 0xFE, 0xFF, 0x00, 0x00, 0x57, 0xFD, 0xFF, 0xFF, 0xFF, 0x66, 0x01, 0x10, 0x75, 0x10, 0x95,
        0x03, 0x91, 0x02, 0xC0, 0x09, 0x5A, 0xA1, 0x02, 0x85, 0x0C, 0x09, 0x22, 0x15, 0x00, 0x26, 0xFF,
        0x00, 0x35, 0x00, 0x45, 0x00, 0x55, 0x00, 0x65, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0x09,
        0x5C, 0x15, 0x00, 0x26, 0x10, 0x27, 0x35, 0x00, 0x46, 0x10, 0x27, 0x57, 0xFD, 0xFF, 0xFF, 0xFF,
        0x66, 0x01, 0x10, 0x75, 0x10, 0x95, 0x01, 0x91, 0x02, 0x09, 0x5B, 0x15, 0x00, 0x25, 0x7F, 0x55,
        0x00, 0x65, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0x09, 0x5E, 0x15, 0x00, 0x26, 0x10, 0x27,
        0x57, 0xFD, 0xFF, 0xFF, 0xFF, 0x66, 0x01, 0x10, 0x75, 0x10, 0x95, 0x01, 0x91, 0x02, 0x09, 0x5D,
        0x15, 0x00, 0x25, 0x7F, 0x55, 0x00, 0x65, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0xC0, 0x09,
        0x73, 0xA1, 0x02, 0x85, 0x0D, 0x09, 0x22, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x35, 0x00, 0x45, 0x00,
        0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0x09, 0x70, 0x15, 0x81, 0x25, 0x7F, 0x36, 0xF0, 0xD8, 0x46,
        0x10, 0x27, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0xC0, 0x09, 0x6E, 0xA1, 0x02, 0x85, 0x0E, 0x09,
        0x22, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x35, 0x00, 0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02,
        0x09, 0x70, 0x15, 0x00, 0x25, 0x7F, 0x35, 0x00, 0x46, 0x10, 0x27, 0x75, 0x08, 0x95, 0x01, 0x91,
        0x02, 0x09, 0x6F, 0x15, 0x81, 0x25, 0x7F, 0x36, 0xF0, 0xD8, 0x46, 0x10, 0x27, 0x75, 0x08, 0x95,
        0x01, 0x91, 0x02, 0x09, 0x71, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x35, 0x00, 0x46, 0x68, 0x01, 0x75,
        0x08, 0x95, 0x01, 0x91, 0x02, 0x09, 0x72, 0x15, 0x00, 0x26, 0x10, 0x27, 0x35, 0x00, 0x46, 0x10,
        0x27, 0x57, 0xFD, 0xFF, 0xFF, 0xFF, 0x66, 0x01, 0x10, 0x75, 0x10, 0x95, 0x01, 0x91, 0x02, 0xC0,
        0x09, 0x77, 0xA1, 0x02, 0x85, 0x51, 0x09, 0x22, 0x15, 0x00, 0x25, 0x7F, 0x35, 0x00, 0x45, 0x00,
        0x55, 0x00, 0x65, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0x09, 0x78, 0xA1, 0x02, 0x09, 0x7B,
        0x09, 0x79, 0x09, 0x7A, 0x15, 0x01, 0x25, 0x03, 0x75, 0x08, 0x95, 0x01, 0x91, 0x00, 0xC0, 0x09,
        0x7C, 0x15, 0x00, 0x26, 0xFE, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0xC0, 0x09, 0x92, 0xA1,
        0x02, 0x09, 0x96, 0xA1, 0x02, 0x85, 0x52, 0x09, 0x9A, 0x09, 0x99, 0x09, 0x97, 0x09, 0x98, 0x09,
        0x9B, 0x09, 0x9C, 0x15, 0x01, 0x25, 0x06, 0x75, 0x08, 0x95, 0x01, 0x91, 0x00, 0xC0, 0xC0, 0x05,
        0xFF, 0x0A, 0x01, 0x03, 0xA1, 0x02, 0x0A, 0x02, 0x03, 0xA1, 0x02, 0x85, 0x40, 0x1A, 0x11, 0x03,
        0x2A, 0x20, 0x03, 0x15, 0x01, 0x25, 0x10, 0x75, 0x08, 0x95, 0x01, 0x91, 0x00, 0xC0, 0x0A, 0x03,
        0x03, 0x15, 0x00, 0x27, 0xFF, 0xFF, 0x00, 0x00, 0x75, 0x10, 0x95, 0x01, 0x91, 0x02, 0xC0, 0x05,
        0x0F, 0x09, 0x7D, 0xA1, 0x02, 0x85, 0x43, 0x09, 0x7E, 0x15, 0x00, 0x26, 0x80, 0x00, 0x35, 0x00,
        0x46, 0x10, 0x27, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0xC0, 0x09, 0x7F, 0xA1, 0x02, 0x85, 0x0B,
        0x09, 0x80, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x35, 0x00, 0x45, 0x00, 0x75, 0x0F, 0x95, 0x01, 0xB1,
        0x03, 0x09, 0xA9, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0xB1, 0x03, 0x09, 0x83, 0x15,
        0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x03, 0xC0, 0x09, 0xAB, 0xA1, 0x03, 0x09,
        0x25, 0xA1, 0x02, 0x85, 0x15, 0x09, 0x26, 0x09, 0x30, 0x09, 0x32, 0x09, 0x31, 0x09, 0x33, 0x09,
        0x34, 0x15, 0x01, 0x25, 0x06, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x00, 0xC0, 0xC0, 0x09, 0x89, 0xA1,
        0x03, 0x09, 0x8B, 0xA1, 0x02, 0x85, 0x16, 0x09, 0x8C, 0x09, 0x8D, 0x09, 0x8E, 0x15, 0x01, 0x25,
        0x03, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x00, 0xC0, 0x09, 0x22, 0x15, 0x00, 0x26, 0xFE, 0x00, 0x75,
        0x08, 0x95, 0x01, 0xB1, 0x02, 0xC0, 0x09, 0x90, 0xA1, 0x03, 0x85, 0x50, 0x09, 0x22, 0x15, 0x00,
        0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0xC0, 0xC0,
    ];

    /// Records what the driver sends; answers Block Load with the next slot.
    #[derive(Default)]
    struct Fake {
        sent: Vec<Vec<u8>>,
        next_block: u8,
        full: bool,
    }

    impl PidIo for Fake {
        fn write(&mut self, report: &[u8]) -> Result<(), String> {
            self.sent.push(report.to_vec());
            Ok(())
        }
        fn send_feature(&mut self, report: &[u8]) -> Result<(), String> {
            self.sent.push(report.to_vec());
            Ok(())
        }
        fn get_feature(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            // The P2600's Block Load: [0x16, status (1 = success, 2 = full), slot, …].
            buf[1] = if self.full { 2 } else { 1 };
            buf[2] = self.next_block;
            self.next_block += 1;
            Ok(4)
        }
    }

    fn hex(sent: &[Vec<u8>]) -> Vec<String> {
        sent.iter().map(|r| r.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")).collect()
    }

    #[test]
    fn the_p2600_descriptor_yields_a_rumble_pad_with_two_channels() {
        let l = parse(P2600).expect("a PID interface");
        assert!(!l.force_axes, "no axes or direction in its Set Effect report");
        assert_eq!(l.channels(), vec![RumbleChannel::Positive, RumbleChannel::Negative]);
        assert_eq!((l.magnitude.report, l.magnitude.min, l.magnitude.max), (0x0D, -127, 127));
        assert_eq!(l.create_type.report, 0x15);
        assert_eq!(l.load_block.report, 0x16);
        let (raw, real) = l.effect_duration().unwrap();
        assert_eq!(raw, 2000, "exponent −3 seconds: milliseconds");
        assert_eq!(real, EFFECT_RUN);
    }

    #[test]
    fn reports_match_what_played_on_the_real_p2600() {
        // Byte for byte what the hand-built HidP test sent (and felt), cut to
        // each report's own length (hidapi pads to the device's).
        let l = parse(P2600).unwrap();
        let h = |r: Option<Vec<u8>>| hex(&[r.unwrap()]).remove(0);
        assert_eq!(h(l.control(DC_DEVICE_RESET)), "52 01");
        assert_eq!(h(l.control(DC_STOP_ALL_EFFECTS)), "52 02");
        assert_eq!(h(l.control(DC_ENABLE_ACTUATORS)), "52 03");
        assert_eq!(h(l.control(DC_DEVICE_CONTINUE)), "52 06");
        assert_eq!(h(l.device_gain_max()), "43 80");
        assert_eq!(h(Some(l.create_constant())), "15 01");
        assert_eq!(h(l.set_envelope(0)), "0C 00 00 00 7F 00 00 7F");
        assert_eq!(h(Some(l.set_constant(0, l.magnitude_raw(RumbleChannel::Positive, 1.0)))), "0D 00 7F");
        assert_eq!(h(Some(l.set_constant(0, l.magnitude_raw(RumbleChannel::Negative, 1.0)))), "0D 00 81");
        assert_eq!(h(Some(l.set_constant(1, l.magnitude_raw(RumbleChannel::Negative, 0.5)))), "0D 01 C0");
        assert_eq!(h(l.operation(0, true)), "51 00 02 01");
        assert_eq!(h(l.operation(0, false)), "51 00 01 00");
        assert_eq!(h(l.block_free(1)), "50 01");
        // Set Effect: no trigger button (11 = past 0..10), and 2 s instead of
        // the test's 1.5 s: 2000 = 0x07D0.
        assert_eq!(h(Some(l.set_effect(0))), "0B 00 0B D0 07 00 00 00 00");
    }

    #[test]
    fn block_load_replies_give_the_slot_and_a_full_pool_gives_none() {
        let l = parse(P2600).unwrap();
        assert_eq!(l.block_from_load(&[0x16, 0x01, 0x00, 0x80]), Some(0), "slot 0 is a real slot");
        assert_eq!(l.block_from_load(&[0x16, 0x01, 0x01, 0x80]), Some(1));
        assert_eq!(l.block_from_load(&[0x16, 0x02, 0xFF, 0x80]), None, "status: full");
    }

    #[test]
    fn the_driver_creates_a_slot_per_channel_and_only_rewrites_what_changed() {
        let l = parse(P2600).unwrap();
        let mut d = Driver::new(Fake::default(), l, "test");
        let t0 = Instant::now();
        d.apply([0.0, 0.0], t0);
        assert!(d.io.sent.is_empty(), "silence touches nothing, not even the reset");

        d.apply([1.0, 0.0], t0);
        assert_eq!(hex(&d.io.sent), [
            "52 01", "52 03", "52 06", "43 80", // reset, actuators on, continue, gain
            "15 01", "0B 00 0B D0 07 00 00 00 00", "0C 00 00 00 7F 00 00 7F", // slot 0
            "0D 00 7F", "51 00 02 01", // full positive, start
        ]);
        d.io.sent.clear();

        d.apply([1.0, 0.0], t0 + Duration::from_millis(5));
        assert!(d.io.sent.is_empty(), "the same strength again sends nothing");

        d.apply([0.5, 0.5], t0 + Duration::from_millis(10));
        assert_eq!(hex(&d.io.sent), [
            "0D 00 40", // positive to half; already playing
            "15 01", "0B 01 0B D0 07 00 00 00 00", "0C 01 00 00 7F 00 00 7F", // slot 1
            "0D 01 C0", "51 01 02 01", // half negative, start
        ]);
        d.io.sent.clear();

        d.apply([0.0, 0.5], t0 + Duration::from_millis(15));
        assert_eq!(hex(&d.io.sent), ["51 00 01 00"], "positive stops, its slot kept");
        d.io.sent.clear();

        // The negative effect started at +10 ms and runs 2 s; it is restarted
        // a second in, and not before.
        d.apply([0.0, 0.5], t0 + Duration::from_millis(1000));
        assert!(d.io.sent.is_empty(), "not due yet");
        d.apply([0.0, 0.5], t0 + Duration::from_millis(1020));
        assert_eq!(hex(&d.io.sent), ["51 01 02 01"], "restarted before its 2 s run out");
        d.io.sent.clear();

        d.shutdown();
        assert_eq!(hex(&d.io.sent), ["50 00", "51 01 01 00", "50 01", "52 02"]);
    }

    #[test]
    fn a_full_effect_pool_leaves_that_channel_silent_without_retrying() {
        let l = parse(P2600).unwrap();
        let mut d = Driver::new(Fake { full: true, ..Default::default() }, l, "test");
        let t0 = Instant::now();
        d.apply([1.0, 0.0], t0);
        let n = d.io.sent.len();
        assert!(hex(&d.io.sent).last().is_some_and(|r| r == "15 01"), "asked for a slot, got none");
        d.apply([0.8, 0.0], t0);
        assert_eq!(d.io.sent.len(), n, "doesn't ask again");
    }

    #[test]
    fn a_force_feedback_joystick_with_axes_is_flagged_and_not_a_rumble_pad() {
        // A minimal PID Set Effect report with Axes Enable (X, Y) — what a
        // force-feedback stick or wheel declares.
        let desc = [
            0x05, 0x0F, 0x09, 0x21, 0xA1, 0x02, 0x85, 0x01, // Set Effect Report, id 1
            0x09, 0x22, 0x15, 0x00, 0x25, 0x28, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, // block index
            0x09, 0x55, 0xA1, 0x02, // Axes Enable
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x91, 0x02,
            0xC0, 0x05, 0x0F, 0x75, 0x06, 0x95, 0x01, 0x91, 0x03, // padding
            0xC0,
        ];
        let (items, _, _) = parse_items(&desc);
        assert!(items.iter().any(|it| it.in_coll(AXES_ENABLE)), "the axes sit in the Axes Enable collection");
        // Too little else for a full layout, so `parse` gives none — but a
        // full one with axes would carry `force_axes`; check the rule itself.
        assert!(parse(&desc).is_none());
        let mut full = P2600.to_vec();
        // Splice an Axes Enable collection into the P2600's Set Effect
        // report: after its three 16-bit times (…0x95, 0x03, 0x91, 0x02).
        let at = full.windows(4).position(|w| w == [0x95, 0x03, 0x91, 0x02]).unwrap() + 4;
        full.splice(at..at, [
            0x09, 0x55, 0xA1, 0x02, 0x05, 0x01, 0x09, 0x30, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01,
            0x91, 0x02, 0xC0, 0x05, 0x0F,
        ]);
        assert!(parse(&full).expect("still a layout").force_axes);
    }

    #[test]
    fn a_descriptor_without_pid_reports_has_no_layout() {
        // A plain gamepad: two axes and eight buttons.
        let desc = [
            0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26,
            0xFF, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x25,
            0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xC0,
        ];
        assert!(parse(&desc).is_none());
    }
}
