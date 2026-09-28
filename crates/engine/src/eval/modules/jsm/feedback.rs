//! What a config sends back to the pad: rumble, the light bar, and the adaptive
//! triggers.
//!
//! These are the one thing this module writes *backwards* — everything else goes
//! forward on the bus. They are published as `feedback_override:{pad}`, the layer
//! that takes a kind of feedback over from the game (see `eval/feedback.rs`), which
//! is exactly what JSM does: while it owns the pad, the game's rumble reaches it
//! only if `RUMBLE` is on.
//!
//! ## The adaptive triggers
//!
//! All seven of JSM's effects are carried, each to the DualSense effect JSM's
//! own encoder (Nielk1's `TriggerEffectGenerator`) builds for it:
//!
//! | JSM | pad effect | parameters, in JSM's order |
//! | --- | --- | --- |
//! | `OFF` | off | — |
//! | `RESISTANCE` | feedback | start 0-9, force 0-8 |
//! | `BOW` | bow | start 0-8, end 0-8, force 0-8, snap force 0-8 |
//! | `GALLOPING` | galloping | start 0-8, end 0-9, first foot 0-6, second foot 0-7, frequency |
//! | `SEMI_AUTOMATIC` | weapon | start 2-7, end 0-8, force 0-8 |
//! | `AUTOMATIC` | vibration | start 0-9, force 0-8, frequency |
//! | `MACHINE` | machine | start 0-8, end 0-9, force A 0-7, force B 0-7, frequency, period |
//!
//! JSM sends `SEMI_AUTOMATIC` and `AUTOMATIC` through the generator's "simple"
//! effects, which take raw 0-255 positions — so its documented zones (start 2-7)
//! land in the first few percent of the pull there. Here they mean what JSM's
//! documentation says they mean, through the zone-based weapon and vibration
//! effects its own mode codes (0x25, 0x26) name.
//!
//! ## Units
//!
//! JSM speaks the DualSense's own numbers, and so do the pins, as fractions: zones
//! 0-9 (/9), 3-bit forces 0-7 (/7), frequency and period 0-255 (/255). A 0-8
//! force is JSM's "0 = none, 1-8 = weakest to strongest", which the pad stores as
//! 0-7 — so it goes out as `force - 1`, and a 0 turns the effect off, exactly as
//! the generator does. The feet and machine amplitudes are already 3-bit values
//! and go out as written.

/// One trigger's effect (`LEFT_TRIGGER_EFFECT` / `RIGHT_TRIGGER_EFFECT`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Effect {
    /// JSM's `ON`, and its default: no effect of its own — JSM shapes resistance
    /// from whatever dual-stage trigger mode is set. We leave the trigger alone,
    /// so the game keeps whatever it was asking for.
    #[default]
    Auto,
    Off,
    /// Constant resistance from `start` onwards. `force` 0-8.
    Resistance { start: u8, force: u8 },
    /// Resistance from `start` that snaps back at `end`. Both forces 0-8.
    Bow { start: u8, end: u8, force: u8, snap: u8 },
    /// Two pulses between `start` and `end`, repeated at `frequency`. Feet 0-6
    /// and 0-7, the first below the second.
    Galloping { start: u8, end: u8, first_foot: u8, second_foot: u8, frequency: u8 },
    /// A click at `start`, releasing at `end`. `force` 0-8.
    SemiAutomatic { start: u8, end: u8, force: u8 },
    /// Vibration from `start` onwards. `force` 0-8.
    Automatic { start: u8, force: u8, frequency: u8 },
    /// Vibration between `start` and `end`, alternating amplitudes A and B (0-7
    /// each) at `frequency`, every `period`.
    Machine { start: u8, end: u8, force_a: u8, force_b: u8, frequency: u8, period: u8 },
}

/// One number a trigger effect takes: what it is, JSM's range for it, and what
/// the editor writes when the number has to be made up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EffectParam {
    /// What the number does, as the fader and the line's errors name it.
    pub name: &'static str,
    pub lo: u8,
    pub hi: u8,
    /// A value that gives a clearly felt effect, for filling in a number the
    /// line doesn't have yet.
    pub default: u8,
}

const fn p(name: &'static str, lo: u8, hi: u8, default: u8) -> EffectParam {
    EffectParam { name, lo, hi, default }
}

/// The numbers each trigger effect takes, in the order JSM reads them and with
/// JSM's documented ranges — the one table the parser, the tune panel's faders
/// and the editor's auto-fill all read, so they cannot disagree about what a
/// number means. `None` for a word that isn't an effect; `ON` and `OFF` take no
/// numbers.
///
/// Two ranges are a notch narrower than JSM's help text, to the limit its
/// encoder actually accepts: `MACHINE`'s start stops at 8 (the generator refuses
/// 9 and sends nothing usable).
pub fn effect_params(mode: &str) -> Option<&'static [EffectParam]> {
    const START_9: EffectParam = p("start zone", 0, 9, 2);
    const START_8: EffectParam = p("start zone", 0, 8, 2);
    const FORCE: EffectParam = p("force", 0, 8, 5);
    const FREQ: EffectParam = p("frequency", 0, 255, 20);
    const RESISTANCE: &[EffectParam] = &[START_9, FORCE];
    const BOW: &[EffectParam] =
        &[START_8, p("end zone", 0, 8, 6), FORCE, p("snap force", 0, 8, 7)];
    const GALLOPING: &[EffectParam] = &[
        START_8,
        p("end zone", 0, 9, 8),
        p("first foot", 0, 6, 3),
        p("second foot", 0, 7, 5),
        FREQ,
    ];
    const SEMI_AUTOMATIC: &[EffectParam] =
        &[p("start zone", 2, 7, 3), p("end zone", 0, 8, 6), FORCE];
    const AUTOMATIC: &[EffectParam] = &[START_9, FORCE, FREQ];
    const MACHINE: &[EffectParam] = &[
        START_8,
        p("end zone", 0, 9, 8),
        p("force A", 0, 7, 3),
        p("force B", 0, 7, 6),
        FREQ,
        p("period", 0, 255, 10),
    ];
    Some(match mode.to_ascii_uppercase().as_str() {
        "ON" | "OFF" => &[],
        "RESISTANCE" => RESISTANCE,
        "BOW" => BOW,
        "GALLOPING" => GALLOPING,
        "SEMI_AUTOMATIC" => SEMI_AUTOMATIC,
        "AUTOMATIC" => AUTOMATIC,
        "MACHINE" => MACHINE,
        _ => return None,
    })
}

/// The effect a mode word and its numbers (already held to `effect_params`)
/// describe. `None` for a word that isn't an effect.
pub(crate) fn effect_of(mode: &str, v: &[u8]) -> Option<Effect> {
    let n = |i: usize| v.get(i).copied().unwrap_or(0);
    Some(match mode.to_ascii_uppercase().as_str() {
        "ON" => Effect::Auto,
        "OFF" => Effect::Off,
        "RESISTANCE" => Effect::Resistance { start: n(0), force: n(1) },
        "BOW" => Effect::Bow { start: n(0), end: n(1), force: n(2), snap: n(3) },
        "GALLOPING" => Effect::Galloping {
            start: n(0),
            end: n(1),
            first_foot: n(2),
            second_foot: n(3),
            frequency: n(4),
        },
        "SEMI_AUTOMATIC" => Effect::SemiAutomatic { start: n(0), end: n(1), force: n(2) },
        "AUTOMATIC" => Effect::Automatic { start: n(0), force: n(1), frequency: n(2) },
        "MACHINE" => Effect::Machine {
            start: n(0),
            end: n(1),
            force_a: n(2),
            force_b: n(3),
            frequency: n(4),
            period: n(5),
        },
        _ => return None,
    })
}

/// Where a mode's numbers have to be in order — an end zone past its start, a
/// second foot after the first — as `(earlier, later)` indices into its
/// `effect_params`.
pub(crate) fn effect_orderings(mode: &str) -> &'static [(usize, usize)] {
    match mode.to_ascii_uppercase().as_str() {
        "BOW" | "SEMI_AUTOMATIC" | "MACHINE" => &[(0, 1)],
        "GALLOPING" => &[(0, 1), (2, 3)],
        _ => &[],
    }
}

/// What the feedback side of a config is configured with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// `RUMBLE`: whether the game's rumble still reaches the pad.
    pub rumble: bool,
    /// `LIGHT_BAR`, as 0-255 per channel.
    pub light_bar: (u8, u8, u8),
    /// `ADAPTIVE_TRIGGER`: switches both trigger effects off at a stroke.
    pub adaptive: bool,
    /// Indexed 0 left, 1 right.
    pub trigger: [Effect; 2],
}

impl Default for Settings {
    fn default() -> Self {
        // JSM's defaults: rumble on, light bar white, adaptive triggers on with no
        // effect of their own.
        Settings {
            rumble: true,
            light_bar: (0xFF, 0xFF, 0xFF),
            adaptive: true,
            trigger: [Effect::Auto; 2],
        }
    }
}

/// One pin to write, as a fraction of its own range.
pub type Pin = (&'static str, f32);

/// Everything the feedback side wants written this tick.
///
/// `rumble` is the amplitude a *binding* asked for (`SMALL_RUMBLE`, `BIG_RUMBLE`,
/// `Rhhhh`), which is separate from `RUMBLE = ON|OFF`: the setting decides whether
/// the game's rumble passes, while a binding rumbles on its own account.
pub fn pins(s: &Settings, rumble: Option<(f32, f32)>) -> Vec<Pin> {
    let mut out: Vec<Pin> = Vec::new();

    // The light bar is always ours while the config runs — JSM sets it on connect
    // and keeps it there, so there is no "leave it alone" value.
    let (r, g, b) = s.light_bar;
    out.push(("lightbar_r", r as f32 / 255.0));
    out.push(("lightbar_g", g as f32 / 255.0));
    out.push(("lightbar_b", b as f32 / 255.0));

    // Rumble: a binding's amplitude if one is asking, otherwise silence — and
    // silence is the point of publishing it at all. Overriding the group means the
    // game's rumble is dropped, so something has to say "nothing", or the last
    // value a binding sent would be held for ever.
    //
    // `RUMBLE = OFF` is what drives the override: with it on, the game's rumble is
    // wanted, so the group is left alone unless a binding is actually rumbling.
    let (strong, weak) = rumble.unwrap_or((0.0, 0.0));
    if !s.rumble || rumble.is_some() {
        out.push(("rumble_strong", strong));
        out.push(("rumble_weak", weak));
    }

    // The seven pins of each trigger's group, left then right.
    const LEFT: [&str; 7] = [
        "trigger_l_mode",
        "trigger_l_start",
        "trigger_l_end",
        "trigger_l_strength",
        "trigger_l_freq",
        "trigger_l_strength2",
        "trigger_l_period",
    ];
    const RIGHT: [&str; 7] = [
        "trigger_r_mode",
        "trigger_r_start",
        "trigger_r_end",
        "trigger_r_strength",
        "trigger_r_freq",
        "trigger_r_strength2",
        "trigger_r_period",
    ];

    for (side, effect) in s.trigger.iter().enumerate() {
        let pins = if side == 0 { LEFT } else { RIGHT };
        // `ADAPTIVE_TRIGGER = OFF` wins over whatever effect is set, which is how
        // JSM uses it: one switch to stop the triggers fighting you.
        let effect = if s.adaptive { *effect } else { Effect::Off };
        // Nothing of our own: leave the group alone, so the game keeps whatever
        // it was asking for.
        let Some(values) = trigger_pins(effect) else { continue };
        // Every pin of the group gets a value whenever any of them does, or a
        // leftover from a previous effect would shape this one.
        out.extend(pins.iter().copied().zip(values));
    }
    out
}

/// Is this an effect whose numbers make it no effect at all — a zero force or
/// frequency, which the generator turns into "off"? (`OFF` itself says so on
/// purpose, so it doesn't count.)
pub(crate) fn does_nothing(effect: Effect) -> bool {
    !matches!(effect, Effect::Off | Effect::Auto) && trigger_pins(effect) == Some([0.0; 7])
}

/// One trigger's effect as its seven pins — (mode, start, end, strength, freq,
/// strength2, period), each a fraction of its range — or `None` for `ON`,
/// which leaves the trigger to the game.
///
/// The conditions under which an effect turns into "off" are the generator's
/// own: a 0-8 force of 0, or a frequency of 0, is no effect at all.
fn trigger_pins(effect: Effect) -> Option<[f32; 7]> {
    use flexinput_core::automap::TriggerMode;
    let zone = |v: u8| (v.min(9) as f32) / 9.0;
    // A 3-bit force as the pad stores it.
    let bits = |v: u8| (v.min(7) as f32) / 7.0;
    // A 0-8 force: 1-8 is 0-7 on the pad, and 0 is no effect.
    let force = |v: u8| bits(v.saturating_sub(1));
    let byte = |v: u8| v as f32 / 255.0;
    let off = [0.0; 7];
    let m = |mode: TriggerMode| mode.pin();
    Some(match effect {
        Effect::Auto => return None,
        Effect::Off => off,
        Effect::Resistance { start, force: f } if f > 0 => {
            [m(TriggerMode::Feedback), zone(start), 0.0, force(f), 0.0, 0.0, 0.0]
        }
        Effect::Bow { start, end, force: f, snap } if end > 0 && f > 0 && snap > 0 => {
            [m(TriggerMode::Bow), zone(start), zone(end), force(f), 0.0, force(snap), 0.0]
        }
        Effect::Galloping { start, end, first_foot, second_foot, frequency } if frequency > 0 => [
            m(TriggerMode::Galloping),
            zone(start),
            zone(end),
            bits(first_foot),
            byte(frequency),
            bits(second_foot),
            0.0,
        ],
        Effect::SemiAutomatic { start, end, force: f } if f > 0 => {
            [m(TriggerMode::Weapon), zone(start), zone(end), force(f), 0.0, 0.0, 0.0]
        }
        Effect::Automatic { start, force: f, frequency } if f > 0 && frequency > 0 => {
            [m(TriggerMode::Vibration), zone(start), 0.0, force(f), byte(frequency), 0.0, 0.0]
        }
        Effect::Machine { start, end, force_a, force_b, frequency, period } if frequency > 0 => [
            m(TriggerMode::Machine),
            zone(start),
            zone(end),
            bits(force_a),
            byte(frequency),
            bits(force_b),
            byte(period),
        ],
        // A zero force or frequency: the generator's "no effect".
        _ => off,
    })
}
