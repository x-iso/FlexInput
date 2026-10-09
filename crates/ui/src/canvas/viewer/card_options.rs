//! A mapping card's header options — time gap, Hold, Turbo: whether each does
//! anything for the card as it's set, and what it does, for its tooltip. One
//! place for both, so the card's greying, the gamepad nav's skipping and the
//! words can't disagree. Mirrors `flexinput_engine::eval::activation`.

use serde_json::Value;

/// What a card's options mean, as it's set now.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CardOptions {
    pub(crate) gap_applies: bool,
    pub(crate) hold_applies: bool,
    pub(crate) turbo_applies: bool,
    pub(crate) gap_tip: String,
    pub(crate) hold_tip: String,
    pub(crate) turbo_tip: String,
}

impl CardOptions {
    /// For a card (`mapping`); `allow_order` = its module has "in order"
    /// chords (the Remapper).
    pub(crate) fn of(mapping: &Value, allow_order: bool) -> Self {
        let flag = |k: &str| mapping.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let mode = mapping.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
        let in_order = allow_order && flexinput_engine::in_order_applies(mapping) && flag("in_order");
        let n_out = mapping.get("out").and_then(|v| v.as_array()).map_or(0, |a| a.len());
        let timed = crate::kbm_picker::TimedChord::from_params(
            vec![String::new(); n_out],
            mapping.get(flexinput_engine::eval::OUT_GAP_KEY),
            mapping.get(flexinput_engine::eval::OUT_DUR_KEY),
        ).is_timed();
        let threshold = mapping.get("threshold").and_then(|v| v.as_f64()).is_some();
        Self::from_parts(Parts {
            mode,
            in_order,
            turbo: flag("turbo"),
            hold: flag("sustain"),
            timed,
            threshold,
        })
    }

    fn from_parts(c: Parts) -> Self {
        let Parts { mode, in_order, turbo, hold, timed, threshold } = c;
        // An analog card with a manual threshold holds its output while the
        // input is past the line: Hold has no pulse to shape, and the time gap
        // is only Turbo's.
        let analog_thr = mode == "analog" && threshold;
        let order_tap = in_order && matches!(mode, "short" | "double");
        let gap_applies = match mode {
            "short" | "long" | "double" | "on_press" | "on_release" | "sequence" => true,
            "analog" => !threshold || turbo,
            _ => turbo,
        };
        let seq_hold = mode == "down" && timed && !in_order;
        let hold_applies = mode == "long" || (mode == "analog" && !threshold) || seq_hold
            || (in_order && matches!(mode, "down" | "short" | "double"));
        let turbo_applies = order_tap || !matches!(mode, "short" | "double" | "on_press" | "on_release");

        let tap_period = "\nWith Turbo on, it's also the tap period: on for half, off for half.";
        let gap_tip = match mode {
            "short" if in_order =>
                "Time gap: the longest the chord's last input can be held and still count as a \
                 short press. With Turbo, it times the whole chord, from its first input.".to_string(),
            "short" =>
                "Time gap: the longest a press can last and still count as short. Let go in time \
                 and the press is sent then, for as long as you held it; hold it longer and this \
                 card lets it go by.".to_string(),
            "long" => format!(
                "Time gap: how long to hold before the long press fires.{}",
                if turbo { tap_period } else { "" }),
            "double" if in_order =>
                "Time gap: the chord's last input has to be tapped twice within this time. \
                 With Turbo, the whole chord, from its first input, has to be done twice in it.".to_string(),
            "double" =>
                "Time gap: both taps have to land within this time of the first press. The \
                 second press is what's sent, cut off when the time runs out.".to_string(),
            "on_press" =>
                "Time gap: how long the output stays on once the input is pressed (at least \
                 10 ms), however long it's held.".to_string(),
            "on_release" =>
                "Time gap: how long the output stays on once the input is let go (at least \
                 10 ms).".to_string(),
            "sequence" => format!(
                "Time gap: the most time allowed from one step to the next. Take longer and the \
                 sequence starts over.{}",
                if turbo { tap_period } else { "" }),
            "analog" if threshold && turbo =>
                "Time gap: the turbo period. Past the threshold, the output taps on and off, \
                 one tap per this time.".to_string(),
            "analog" if threshold =>
                "Time gap: not used. With a threshold set, the output is held while the input is \
                 past it. Turn on Turbo and this becomes its tap period.".to_string(),
            "analog" if hold => format!(
                "Time gap: the pulse period. Each period the output is on for a share as large as \
                 the push: a light push gives short blips, a full push holds it.{}",
                if turbo { "\nWith Turbo, the period also shortens the further it's pushed." } else { "" }),
            "analog" => format!(
                "Time gap: the fastest the output can tap, one tap per this time. A lighter push \
                 taps slower; never faster than {} taps a second.",
                if turbo { 40 } else { 20 }),
            _ if turbo =>
                "Time gap: the turbo period. While the output is on, it taps: on for half of \
                 this, off for the other half.".to_string(),
            _ =>
                "Time gap: not used in Normal mode, where the output simply follows the input. \
                 Turn on Turbo and it becomes the tap period.\n(The waits between outputs are \
                 set per output, in Choose.)".to_string(),
        };

        let hold_tip = match mode {
            "down" if in_order =>
                "Hold: once the chord fires, its output stays on while the last input is held, \
                 even after you let go of the earlier ones. An output that's one of those \
                 earlier inputs lets go with it.".to_string(),
            "down" if timed =>
                "Hold: let go before the last output of the sequence has started, and the rest \
                 is cancelled.\nOff: a sequence, once started, always plays out.".to_string(),
            "down" =>
                "Hold: not used here, as the output already follows the input. Give the outputs \
                 a gap or their own duration and Hold lets you cancel the sequence by letting \
                 go early.".to_string(),
            "long" if in_order =>
                "Hold: the output stays on while held instead of a short pulse, and keeps going \
                 while the last input is held, even after you let go of the earlier ones.".to_string(),
            "long" =>
                "Hold: on, the output stays on from when the time gap is reached until you let \
                 go.\nOff: the long press sends one short pulse.".to_string(),
            "short" | "double" if in_order =>
                "Hold: the earlier inputs stay held in the game the whole time; only the last \
                 one is taken.".to_string(),
            "short" =>
                "Hold: not used in Short press: the press is sent when you let go, for as long \
                 as you held it.".to_string(),
            "double" =>
                "Hold: not used in Double tap: the second press is what's sent.".to_string(),
            "on_press" | "on_release" =>
                "Hold: not used here: the output is a pulse as long as the time gap.".to_string(),
            "sequence" =>
                "Hold: not used in Sequence: once the steps are done, the output stays on while \
                 the last one is held.".to_string(),
            "analog" if analog_thr =>
                "Hold: not used while a threshold is set: the output is held while the input \
                 is past it.".to_string(),
            "analog" =>
                "Hold: on, the output pulses once per time gap, staying on longer the further \
                 the input is pushed; a full push holds it.\nOff: it taps, faster the further \
                 it's pushed.".to_string(),
            _ => "Hold".to_string(),
        };

        let again = if timed { " Each tap plays the outputs' sequence again from the start." } else { "" };
        let turbo_tip = match mode {
            "short" if order_tap =>
                "Turbo (whole chord): the entire chord, from its first input, has to be completed \
                 and let go within the time gap.\nOff: the time gap only times the last input, \
                 so the earlier ones can be held as long as you like.".to_string(),
            "double" if order_tap =>
                "Turbo (whole chord): the entire chord, from its first input, has to be completed \
                 twice within the time gap.\nOff: the time gap only times the last input's double \
                 tap, so the earlier ones can be held as long as you like.".to_string(),
            "short" =>
                "Turbo: not used in Short press: the output is the one press, replayed.".to_string(),
            "double" =>
                "Turbo: not used in Double tap: the output is the second press.".to_string(),
            "on_press" | "on_release" =>
                "Turbo: not used here: the output is a single pulse.".to_string(),
            "long" if hold => format!(
                "Turbo: once the long press fires, the output taps on and off, one tap per time \
                 gap, until you let go.{again}"),
            "long" =>
                "Turbo: taps while the long press is on, but with Hold off that's one short \
                 pulse. Turn Hold on for a stream of taps.".to_string(),
            "sequence" => format!(
                "Turbo: once the steps are done, the output taps on and off, one tap per time \
                 gap, while the last step is held.{again}"),
            "analog" if threshold => format!(
                "Turbo: past the threshold, the output taps on and off, one tap per time gap, \
                 instead of holding.{again}"),
            "analog" if hold =>
                "Turbo: the pulses also come faster the further the input is pushed.".to_string(),
            "analog" =>
                "Turbo: doubles the fastest tap rate, to 40 a second.".to_string(),
            _ => format!(
                "Turbo: while the input is held, the output taps on and off instead of holding, \
                 one tap per time gap.{again}"),
        };

        CardOptions { gap_applies, hold_applies, turbo_applies, gap_tip, hold_tip, turbo_tip }
    }
}

struct Parts<'a> {
    mode: &'a str,
    in_order: bool,
    turbo: bool,
    hold: bool,
    timed: bool,
    threshold: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(v: serde_json::Value) -> CardOptions {
        CardOptions::of(&v, true)
    }

    #[test]
    fn normal_mode_uses_the_gap_only_for_turbo_and_hold_only_for_a_sequence() {
        let o = opts(serde_json::json!({ "in": ["btn_south"], "out": ["key_a"] }));
        assert!(!o.gap_applies && !o.hold_applies && o.turbo_applies);
        assert!(o.gap_tip.contains("not used"));
        let o = opts(serde_json::json!({ "in": ["btn_south"], "out": ["key_a"], "turbo": true }));
        assert!(o.gap_applies && o.gap_tip.contains("turbo period"));
        let o = opts(serde_json::json!({ "in": ["btn_south"], "out": ["key_a", "key_b"], "out_gap": [0, 50] }));
        assert!(o.hold_applies && o.hold_tip.contains("cancelled"));
        assert!(o.turbo_tip.contains("sequence again"));
    }

    // A manual threshold makes an analog card a plain hold: Hold has nothing
    // to shape, and the time gap is only Turbo's.
    #[test]
    fn an_analog_threshold_retires_hold_and_the_gap() {
        let o = opts(serde_json::json!({ "in": ["left_trigger"], "out": ["key_a"], "mode": "analog" }));
        assert!(o.gap_applies && o.hold_applies && o.gap_tip.contains("fastest"));
        let o = opts(serde_json::json!({ "in": ["left_trigger"], "out": ["key_a"], "mode": "analog", "threshold": 0.4 }));
        assert!(!o.gap_applies && !o.hold_applies && o.turbo_applies);
    }

    #[test]
    fn the_one_shot_modes_take_no_turbo() {
        for mode in ["short", "double", "on_press", "on_release"] {
            let o = opts(serde_json::json!({ "in": ["btn_south"], "out": ["key_a"], "mode": mode }));
            assert!(!o.turbo_applies && o.turbo_tip.contains("not used"), "{mode}");
            assert!(o.gap_applies, "{mode}");
        }
    }
}
