//! What the 0-1 on a feedback pin means in the world: a rumble motor's share, a haptic
//! actuator's pitch in Hz and strength in dB, a light bar's 0-255, an adaptive trigger's
//! zone, force and effect. The pins all take 0-1 (the device layouts say so in their names,
//! "0=80Hz 1=500Hz"), and a Knob, a Response Curve's Out axis or a Readout on one read in
//! these units instead, once the wiring says where the value ends up
//! (`curve_support::refresh_motion_units`).

use flexinput_core::automap::PitchScale;

/// The bottom of the dB scale a reading is put in: quieter is silence.
pub(crate) const DB_FLOOR: f32 = -60.0;

/// What a pin's value reads as.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PinUnit {
    /// A pitch, on the scale its pad reads the pin on.
    Hz(PitchScale),
    /// An amplitude, as dB of full strength.
    Decibels,
    /// A share, as a percentage.
    Percent,
    /// A byte, 0-255.
    Byte,
    /// One of `n` + 1 steps, 0..n (a trigger's zones 0-9, its forces 0-7).
    Steps(u8),
    /// An adaptive trigger's effect, in sixths.
    TriggerMode,
    /// A DualSense's player LEDs: off, then P1-P4 in quarters.
    PlayerLed,
    /// A DualSense's mic LED: off, on, pulsing.
    MicLed,
}

/// A pin's unit, with the name it shows - what a Knob driving it is called until it is named
/// by hand.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PinTarget {
    pub unit: PinUnit,
    pub name: String,
}

impl PinUnit {
    /// The unit of feedback pin `pin`. `hint` is the pad's own name for the pin, which says
    /// how it maps a frequency ("0=82Hz … 1=1253Hz" is the Switch table); `switch` says the
    /// pad behind a Feedback Control is a Switch pad, for when there is no hint.
    pub(crate) fn of_feedback_pin(pin: &str, hint: Option<&str>, switch: bool) -> Option<Self> {
        let hd_scale = || match hint {
            Some(h) if h.contains("1253") => PitchScale::Switch,
            Some(h) if h.contains("2000Hz") => PitchScale::Wide,
            Some(h) if h.contains("500Hz") => PitchScale::DualSense,
            _ if switch => PitchScale::Switch,
            _ => PitchScale::DualSense,
        };
        Some(match pin {
            "rumble_strong" | "rumble_weak" | "hd_rumble_l" | "hd_rumble_r" => PinUnit::Percent,
            "lightbar_r" | "lightbar_g" | "lightbar_b" => PinUnit::Byte,
            "player_led" => PinUnit::PlayerLed,
            "mic_led" => PinUnit::MicLed,
            p if p.ends_with("_amp") => PinUnit::Decibels,
            p if p.starts_with("hd") && p.ends_with("_freq") => PinUnit::Hz(hd_scale()),
            p if p.starts_with("ds_") || p.starts_with("tp_") || p.starts_with("grip") => {
                // Each its own pad's, whatever pad is behind.
                PinUnit::Hz(PitchScale::of_pin(p, [])?)
            }
            p if p.starts_with("trigger_") => {
                if p.ends_with("_mode") {
                    PinUnit::TriggerMode
                } else if p.ends_with("_start") || p.ends_with("_end") {
                    PinUnit::Steps(9)
                } else if p.ends_with("_strength") || p.ends_with("_strength2") {
                    PinUnit::Steps(7)
                } else if p.ends_with("_freq") || p.ends_with("_period") {
                    PinUnit::Byte
                } else {
                    return None;
                }
            }
            _ => return None,
        })
    }

    /// Value `v` (the pin's 0-1) as it reads.
    pub(crate) fn text(self, v: f32) -> String {
        let v = v.clamp(0.0, 1.0);
        match self {
            PinUnit::Hz(scale) => format!("{:.0} Hz", scale.hz(v)),
            PinUnit::Decibels if v < 1e-4 => "-∞ dB".into(),
            PinUnit::Decibels => format!("{:.1} dB", 20.0 * v.log10()),
            PinUnit::Percent => format!("{:.0}%", v * 100.0),
            PinUnit::Byte => format!("{:.0}", v * 255.0),
            PinUnit::Steps(n) => format!("{:.0}", (v * n as f32).round()),
            PinUnit::TriggerMode => ["off", "bow", "resist", "gallop", "click", "machine", "vibrate"][(v * 6.0).round() as usize].into(),
            PinUnit::PlayerLed => ["off", "P1", "P2", "P3", "P4"][(v * 4.0).round() as usize].into(),
            PinUnit::MicLed => ["off", "on", "pulse"][(v * 2.0).round() as usize].into(),
        }
    }

    /// Value `v` (the pin's 0-1) as the number it reads as: Hz, dB (floored at
    /// [`DB_FLOOR`]), a percentage, a byte, a step.
    pub(crate) fn reading(self, v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        match self {
            PinUnit::Hz(scale) => scale.hz(v),
            PinUnit::Decibels => (20.0 * v.max(1e-6).log10()).max(DB_FLOOR),
            PinUnit::Percent => v * 100.0,
            PinUnit::Byte => v * 255.0,
            PinUnit::Steps(n) => v * n as f32,
            PinUnit::TriggerMode => v * 6.0,
            PinUnit::PlayerLed => v * 4.0,
            PinUnit::MicLed => v * 2.0,
        }
    }

    /// The pin's 0-1 for reading `x`: [`Self::reading`] the other way round. The floor of
    /// the dB scale is silence.
    pub(crate) fn from_reading(self, x: f32) -> f32 {
        let v = match self {
            PinUnit::Hz(scale) => scale.value(x),
            PinUnit::Decibels if x <= DB_FLOOR => 0.0,
            PinUnit::Decibels => 10f32.powf(x / 20.0),
            PinUnit::Percent => x / 100.0,
            PinUnit::Byte => x / 255.0,
            PinUnit::Steps(n) => x / (n.max(1) as f32),
            PinUnit::TriggerMode => x / 6.0,
            PinUnit::PlayerLed => x / 4.0,
            PinUnit::MicLed => x / 2.0,
        };
        v.clamp(0.0, 1.0)
    }

    /// How far a pixel of drag moves a reading.
    pub(crate) fn per_px(self) -> f32 {
        match self {
            PinUnit::Hz(_) => 2.0,
            PinUnit::Decibels => 0.2,
            PinUnit::Percent => 0.5,
            PinUnit::Byte => 1.0,
            _ => 0.05,
        }
    }

    /// The widest it ever reads, for sizing what shows it.
    pub(crate) fn widest(self) -> String {
        match self {
            PinUnit::Hz(_) => "2000 Hz".into(),
            PinUnit::Decibels => "-80.0 dB".into(),
            PinUnit::Percent => "100%".into(),
            PinUnit::Byte => "255".into(),
            PinUnit::Steps(_) => "9".into(),
            PinUnit::TriggerMode => "vibrate".into(),
            PinUnit::PlayerLed => "off".into(),
            PinUnit::MicLed => "pulse".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The pins read in the units their pads' own names give them.
    #[test]
    fn feedback_pins_read_in_their_pads_units() {
        let ds = PinUnit::of_feedback_pin("ds_l_freq", None, false).unwrap();
        assert_eq!(ds.text(0.0), "80 Hz");
        assert_eq!(ds.text(1.0), "500 Hz");
        let sw = PinUnit::of_feedback_pin("hd_l_freq", Some("HD Rumble: Frequency (0=82Hz 0.5=320Hz 1=1253Hz)"), false).unwrap();
        assert_eq!(sw.text(0.0), "82 Hz");
        assert_eq!(sw.text(0.5), "320 Hz");
        assert_eq!(sw.text(1.0), "1253 Hz");
        let named = PinUnit::of_feedback_pin("hd_l_freq", Some("HD Haptic L: Frequency (0=80Hz 1=500Hz)"), true).unwrap();
        assert_eq!(named.text(1.0), "500 Hz", "the pad's own name wins over the guess");
        for pin in ["grip_l_freq", "grip2_r_freq", "tp_l_freq"] {
            let steam = PinUnit::of_feedback_pin(pin, None, true).unwrap();
            assert_eq!((steam.text(0.0), steam.text(0.5), steam.text(1.0)),
                ("40 Hz".into(), "283 Hz".into(), "2000 Hz".into()), "{pin}");
        }
        let amp = PinUnit::of_feedback_pin("hd2_r_amp", None, false).unwrap();
        assert_eq!(amp.text(1.0), "0.0 dB");
        assert_eq!(amp.text(0.5), "-6.0 dB");
        assert_eq!(amp.text(0.0), "-∞ dB");
        assert_eq!(PinUnit::of_feedback_pin("trigger_r_mode", None, false).unwrap().text(4.0 / 6.0), "click");
        assert_eq!(PinUnit::of_feedback_pin("trigger_l_start", None, false).unwrap().text(1.0), "9");
        assert_eq!(PinUnit::of_feedback_pin("lightbar_g", None, false).unwrap().text(1.0), "255");
        assert!(PinUnit::of_feedback_pin("left_stick_x", None, false).is_none());
    }

    // A reading goes back to the pin's value it came from, so a range written in Hz or dB
    // keeps what was typed.
    #[test]
    fn a_reading_goes_back_to_its_value() {
        let units = [
            PinUnit::Hz(PitchScale::DualSense),
            PinUnit::Hz(PitchScale::Switch),
            PinUnit::Hz(PitchScale::Wide),
            PinUnit::Decibels,
            PinUnit::Percent,
            PinUnit::Byte,
            PinUnit::Steps(9),
        ];
        for u in units {
            for v in [0.05f32, 0.25, 0.5, 0.75, 1.0] {
                let back = u.from_reading(u.reading(v));
                assert!((back - v).abs() < 1e-3, "{u:?}: {v} -> {} -> {back}", u.reading(v));
            }
        }
        assert_eq!(PinUnit::Decibels.from_reading(DB_FLOOR), 0.0, "the floor is silence");
    }
}
