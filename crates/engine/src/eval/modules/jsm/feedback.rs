//! What a config sends back to the pad: rumble, the light bar, and the adaptive
//! triggers.
//!
//! These are the one thing this module writes *backwards* — everything else goes
//! forward on the bus. They are published as `feedback_override:{pad}`, the layer
//! that takes a kind of feedback over from the game (see `eval/feedback.rs`), which
//! is exactly what JSM does: while it owns the pad, the game's rumble reaches it
//! only if `RUMBLE` is on.
//!
//! ## The adaptive triggers do not line up, and that is worth saying out loud
//!
//! JSM carries the DualSense's full effect vocabulary — seven usable modes with up
//! to six parameters. Our bus carries the four the DualSense encoder here
//! implements: off, feedback (constant resistance), weapon (a click between two
//! zones), and vibration. Four of JSM's seven land exactly:
//!
//! | JSM | ours | parameters JSM gives it |
//! | --- | --- | --- |
//! | `OFF` | off | — |
//! | `RESISTANCE` | feedback | start, force |
//! | `SEMI_AUTOMATIC` | weapon | start, end, force |
//! | `AUTOMATIC` | vibration | start, force, frequency |
//!
//! `BOW`, `GALLOPING` and `MACHINE` have no home in that model — they need two
//! forces, or a second frequency, and there is nowhere on the bus to put them. A
//! config asking for one is told so on its line, with the nearest thing named,
//! rather than being quietly given something that feels wrong. Extending the bus
//! to carry them is a deliberate change across several modules (the pin list, the
//! DualSense encoder, every feedback-producing module's vocabulary) and belongs on
//! its own, not smuggled in here.
//!
//! ## Units
//!
//! JSM speaks the DualSense's own numbers: zones 0-9 along the trigger, force
//! 0-7, frequency 0-255. Our pins are all `Float` 0..1 and the device layer scales
//! each back to its own range, so every value is divided by its JSM maximum on the
//! way out. A config's numbers therefore mean what they meant in JSM.

/// One trigger's effect (`LEFT_TRIGGER_EFFECT` / `RIGHT_TRIGGER_EFFECT`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Effect {
    /// JSM's `ON`, and its default: no effect of its own — JSM shapes resistance
    /// from whatever dual-stage trigger mode is set. We leave the trigger alone,
    /// so the game keeps whatever it was asking for.
    #[default]
    Auto,
    Off,
    /// Constant resistance from `start` onwards.
    Resistance { start: u8, force: u8 },
    /// A click at `start`, releasing at `end`.
    SemiAutomatic { start: u8, end: u8, force: u8 },
    /// Vibration from `start` onwards.
    Automatic { start: u8, force: u8, frequency: u8 },
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

    // The five pins of each trigger's group, left then right.
    const LEFT: [&str; 5] = [
        "trigger_l_mode",
        "trigger_l_start",
        "trigger_l_end",
        "trigger_l_strength",
        "trigger_l_freq",
    ];
    const RIGHT: [&str; 5] = [
        "trigger_r_mode",
        "trigger_r_start",
        "trigger_r_end",
        "trigger_r_strength",
        "trigger_r_freq",
    ];
    // Zones are 0-9, force 0-7, frequency 0-255 — JSM's own scales; the pins are
    // fractions of each.
    let zone = |v: u8| (v.min(9) as f32) / 9.0;
    let force = |v: u8| (v.min(7) as f32) / 7.0;
    let freq = |v: u8| v as f32 / 255.0;

    for (side, effect) in s.trigger.iter().enumerate() {
        let pins = if side == 0 { LEFT } else { RIGHT };
        // `ADAPTIVE_TRIGGER = OFF` wins over whatever effect is set, which is how
        // JSM uses it: one switch to stop the triggers fighting you.
        let effect = if s.adaptive { *effect } else { Effect::Off };
        // (mode, start, end, strength, freq) — every pin of the group gets a value
        // whenever any of them does, or a leftover from a previous effect would
        // shape this one.
        let values = match effect {
            // Nothing of our own: leave the group alone, so the game keeps whatever
            // it was asking for.
            Effect::Auto => continue,
            Effect::Off => [0.0; 5],
            Effect::Resistance { start, force: f } => {
                [1.0 / 3.0, zone(start), 0.0, force(f), 0.0]
            }
            Effect::SemiAutomatic { start, end, force: f } => {
                [2.0 / 3.0, zone(start), zone(end), force(f), 0.0]
            }
            Effect::Automatic { start, force: f, frequency } => {
                [1.0, zone(start), 0.0, force(f), freq(frequency)]
            }
        };
        out.extend(pins.iter().copied().zip(values));
    }
    out
}
