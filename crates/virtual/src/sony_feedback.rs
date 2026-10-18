//! The feedback a game writes to a virtual Sony pad: rumble from the output
//! reports' motor bytes, and the lightbar, player / mic LEDs and adaptive
//! triggers, normalized to the source-pin convention so each routes straight
//! to a physical pad's matching haptic input. Shared by the UMDF backend
//! (output ring frames) and the composite one (interrupt-OUT reports), which
//! see the same report with its id stripped.

use flexinput_core::Signal;
use flexinput_hidmaestro::profile::ExtendedLayout;
use flexinput_hidmaestro::Profile;

/// One poll's worth of a Sony pad's output reports folded into feedback pins.
#[derive(Default)]
pub(crate) struct SonyFeedback {
    /// True when the last poll published a peak above the settled value; the
    /// next settles to the latest actual frame, so a held peak doesn't buzz
    /// forever after a one-shot ping.
    rumble_settle_pending: bool,
    /// The latest actual (non-peak) rumble frame value.
    rumble_latest: (f32, f32),
    /// The game's most recent full output report: the LEDs are level-held, so
    /// the newest report is authoritative.
    last_out_frame: Option<Vec<u8>>,
    pub(crate) fb: DsFeedback,
}

impl SonyFeedback {
    /// Fold this poll's output reports (report id stripped, oldest first) into
    /// `rumble` and the non-rumble feedback, and return the source pins.
    ///
    /// Rumble keeps the PEAK seen this poll, not the last frame: a short ping
    /// enqueues an ON frame right before an OFF one, and if one poll drained
    /// both and kept the last, the physical pad would never feel the pulse.
    /// The peak is published for one poll, then settles to the latest value.
    pub(crate) fn poll<'a>(
        &mut self,
        profile: &Profile,
        rumble: &mut (f32, f32),
        frames: impl IntoIterator<Item = &'a [u8]>,
    ) -> Vec<(&'static str, Signal)> {
        // Motor offsets differ between DS4 (right@4/left@5) and DualSense
        // (right@3/left@4), so they come from the profile. They're
        // RID-inclusive; the frames aren't, so a motor at `o` sits at `o-1`.
        let (left_idx, right_idx) = (
            profile.extended.out_left_motor.map(|o| o.saturating_sub(1)),
            profile.extended.out_right_motor.map(|o| o.saturating_sub(1)),
        );
        let mut peak: Option<(f32, f32)> = None;
        for data in frames {
            let strong = left_idx.and_then(|i| data.get(i)).map(|b| *b as f32 / 255.0);
            let weak = right_idx.and_then(|i| data.get(i)).map(|b| *b as f32 / 255.0);
            if let (Some(s), Some(w)) = (strong, weak) {
                let (ps, pw) = peak.unwrap_or((0.0, 0.0));
                peak = Some((s.max(ps), w.max(pw)));
                self.rumble_latest = (s, w);
            }
            // Trigger effects and the lightbar from EVERY frame: each says
            // whether it updates them, and a later frame that doesn't must not
            // hide an earlier one that did.
            self.fb.update_triggers(&profile.extended, data);
            self.fb.update_lightbar(profile, data);
            self.last_out_frame = Some(data.to_vec());
        }
        if let Some((pk_s, pk_w)) = peak {
            let (lt_s, lt_w) = self.rumble_latest;
            self.rumble_settle_pending = pk_s > lt_s + 0.01 || pk_w > lt_w + 0.01;
            *rumble = (pk_s, pk_w);
        } else if self.rumble_settle_pending {
            self.rumble_settle_pending = false;
            *rumble = self.rumble_latest;
        }
        self.decode_leds(&profile.extended);
        let mut out = vec![
            ("rumble_strong", Signal::Float(rumble.0)),
            ("rumble_weak", Signal::Float(rumble.1)),
        ];
        out.extend(self.fb.as_pins());
        out
    }

    /// The player / mic LEDs from the latest report. Fields the profile doesn't
    /// declare stay at their default (0 = off).
    fn decode_leds(&mut self, ext: &ExtendedLayout) {
        let Some(data) = self.last_out_frame.as_deref() else { return };
        let at = |o: Option<usize>| o.and_then(|o| o.checked_sub(1)).filter(|&i| i < data.len());
        if let Some(i) = at(ext.out_player_led) {
            self.fb.player_led = decode_player_led(data[i]);
        }
        if let Some(i) = at(ext.out_mic_led) {
            // DualSense mute-LED byte: 0=off, 1=on(solid), 2=pulse → 0 / 0.5 / 1.
            self.fb.mic_led = (data[i] as f32 / 2.0).clamp(0.0, 1.0);
        }
    }
}

/// Decoded non-rumble DualSense/DS4 feedback the game wrote to the virtual pad,
/// normalized to the 0–1 source-pin convention so it routes straight to a
/// physical pad's matching haptic input pins. All-zero = nothing driven.
#[derive(Clone, Copy, Default)]
pub(crate) struct DsFeedback {
    /// `None` until the game sends a report that sets the lightbar (its valid
    /// flag), so a game that never touches it leaves the physical pad on its
    /// own default colour instead of forcing it black.
    pub(crate) lightbar: Option<(f32, f32, f32)>,
    pub(crate) player_led: f32,
    pub(crate) mic_led: f32,
    /// A trigger's effect is `None` until the game first sets it (its valid-flag
    /// bit): a game that never drives the adaptive triggers publishes no trigger
    /// pins at all, so a pad's default trigger effect can tell "the game says
    /// nothing" from "the game says Off".
    pub(crate) trig_r: Option<Trig>,
    pub(crate) trig_l: Option<Trig>,
}

/// One trigger's effect as the `trigger_*` pins carry it, each 0–1 (see
/// `flexinput_core::automap::TriggerMode` for the mode's values).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(crate) struct Trig {
    pub(crate) mode: f32,
    pub(crate) start: f32,
    pub(crate) end: f32,
    pub(crate) strength: f32,
    pub(crate) freq: f32,
    pub(crate) strength2: f32,
    pub(crate) period: f32,
}

impl DsFeedback {
    /// Flatten to source-pin (id, Signal) pairs. Pins whose underlying field the
    /// profile doesn't declare still report 0.0 — harmless (a physical pad that
    /// lacks the pin drops it, and 0 = "off" otherwise).
    pub(crate) fn as_pins(&self) -> Vec<(&'static str, Signal)> {
        let mut out = vec![
            ("player_led", Signal::Float(self.player_led)),
            ("mic_led", Signal::Float(self.mic_led)),
        ];
        // Absent (not 0) until the game sets it: no pin = nothing drives the
        // physical lightbar, so its device-card default colour applies.
        if let Some((r, g, b)) = self.lightbar {
            out.extend([
                ("lightbar_r", Signal::Float(r)),
                ("lightbar_g", Signal::Float(g)),
                ("lightbar_b", Signal::Float(b)),
            ]);
        }
        for (t, pins) in [
            (self.trig_r, [
                "trigger_r_mode", "trigger_r_start", "trigger_r_end", "trigger_r_strength",
                "trigger_r_freq", "trigger_r_strength2", "trigger_r_period",
            ]),
            (self.trig_l, [
                "trigger_l_mode", "trigger_l_start", "trigger_l_end", "trigger_l_strength",
                "trigger_l_freq", "trigger_l_strength2", "trigger_l_period",
            ]),
        ] {
            let Some(t) = t else { continue };
            let v = [t.mode, t.start, t.end, t.strength, t.freq, t.strength2, t.period];
            out.extend(pins.into_iter().zip(v).map(|(p, v)| (p, Signal::Float(v))));
        }
        out
    }
}

impl DsFeedback {
    /// Update the adaptive-trigger effects from one output frame — only the
    /// sides whose `valid_flag0` bit the frame sets, as a real DualSense does:
    /// a rumble- or lightbar-only report leaves the triggers' effects alone
    /// (its trigger bytes are zeros that mean nothing).
    pub(crate) fn update_triggers(&mut self, ext: &ExtendedLayout, data: &[u8]) {
        // `valid_flag0` sits 10 bytes before the right trigger block in both the
        // USB (0x02) and Bluetooth (0x31) output reports. Offsets are
        // RID-inclusive; the ring strips the report id.
        let Some(flags) = ext.out_trigger_r
            .and_then(|r| r.checked_sub(11))
            .and_then(|i| data.get(i).copied())
        else { return };
        let at = |o: Option<usize>| o.and_then(|o| o.checked_sub(1)).filter(|&i| i < data.len());
        if flags & DS_FLAG0_RIGHT_TRIGGER != 0 {
            if let Some(i) = at(ext.out_trigger_r) {
                self.trig_r = Some(decode_trigger_effect(&data[i..]));
            }
        }
        if flags & DS_FLAG0_LEFT_TRIGGER != 0 {
            if let Some(i) = at(ext.out_trigger_l) {
                self.trig_l = Some(decode_trigger_effect(&data[i..]));
            }
        }
    }
}

impl DsFeedback {
    /// Take the lightbar colour from one output frame, if that frame says it
    /// sets the lightbar (DualSense `valid_flag1` bit 2, DS4 flag byte bit 1).
    /// A profile that declares no flag byte takes every frame's colour.
    pub(crate) fn update_lightbar(&mut self, profile: &Profile, data: &[u8]) {
        let ext = &profile.extended;
        let at = |o: Option<usize>| o.and_then(|o| o.checked_sub(1)).filter(|&i| i < data.len());
        let Some(i) = at(ext.out_lightbar_r) else { return };
        if i + 2 >= data.len() {
            return;
        }
        let (flag, bit) = if profile.id.contains("dualsense") {
            (ext.out_valid_flag1, DS_FLAG1_LIGHTBAR)
        } else {
            (ext.out_valid_flag0, DS4_FLAG_LIGHTBAR)
        };
        if at(flag).is_some_and(|f| data[f] & bit == 0) {
            return;
        }
        let c = |k: usize| data[k] as f32 / 255.0;
        self.lightbar = Some((c(i), c(i + 1), c(i + 2)));
    }
}

/// DualSense output-report `valid_flag0` bits saying the report carries a new
/// right / left adaptive-trigger effect.
pub(crate) const DS_FLAG0_RIGHT_TRIGGER: u8 = 0x04;
const DS_FLAG0_LEFT_TRIGGER: u8 = 0x08;
/// DualSense `valid_flag1` bit: LIGHTBAR_CONTROL_ENABLE.
const DS_FLAG1_LIGHTBAR: u8 = 0x04;
/// DS4 output report (0x05) flag-byte bit: the report sets the lightbar.
const DS4_FLAG_LIGHTBAR: u8 = 0x02;

/// Map a DualSense player-indicator bitmask byte back to the 0–1 `player_led`
/// convention (0=off, 0.25=P1 … 1=P4). The pad lights 1–4 of its 5 front LEDs in
/// fixed symmetric patterns per player number; we recognise those, and fall back
/// to "scale by lit-count" for anything non-standard. Low 5 bits are the LEDs.
pub(crate) fn decode_player_led(b: u8) -> f32 {
    match b & 0x1F {
        0x00 => 0.0,        // off
        0x04 => 0.25,       // P1: centre
        0x0A => 0.5,        // P2: inner pair
        0x15 => 0.75,       // P3: centre + outer pair
        0x1B => 1.0,        // P4: two pairs
        other => {
            // Non-standard: approximate by how many LEDs are lit (1..5 → ~0.25..1).
            let n = other.count_ones() as f32;
            (n / 4.0).clamp(0.0, 1.0)
        }
    }
}

/// Decode one 11-byte DualSense adaptive-trigger effect block back into the
/// `trigger_*` pins, each normalized 0–1 — the inverse of
/// `flexinput_devices::gyro::encode_trigger_effect`, for all seven effect types
/// that encoder writes (Off 0x05, Feedback 0x21, Bow 0x22, Galloping 0x23,
/// Weapon 0x25, Vibration 0x26, Machine 0x27; byte layouts per Nielk1's
/// `TriggerEffectGenerator`). Zones are 0–9 (→ /9), forces 0–7 (→ /7), freq and
/// period raw bytes (→ /255). A short/empty block, and the "simple" effect types
/// whose positions are raw bytes rather than zones, decode as Off. Enough to
/// forward a game's effect to a physical DualSense, which re-encodes it.
pub(crate) fn decode_trigger_effect(block: &[u8]) -> Trig {
    use flexinput_core::automap::TriggerMode;
    if block.len() < 10 {
        return Trig::default();
    }
    let zone = |z: u32| (z as f32 / 9.0).clamp(0.0, 1.0);
    let force = |f: u8| (f.min(7) as f32 / 7.0).clamp(0.0, 1.0);
    let byte = |b: u8| b as f32 / 255.0;
    // The 10-bit zone field (params bytes 1..3): every active zone for the
    // positional effects, just the start and end bits for the two-zone ones.
    let zones = (block[1] as u16) | ((block[2] as u16) << 8);
    let lo = if zones == 0 { 0 } else { zones.trailing_zeros() };
    let hi = if zones == 0 { 0 } else { 15 - zones.leading_zeros() };
    // Positional effects repeat a 3-bit force per active zone (bytes 3..7); read
    // the one at the start zone.
    let forces = u32::from_le_bytes([block[3], block[4], block[5], block[6]]);
    let force_at = |z: u32| ((forces >> (z * 3)) & 0x7) as u8;
    // Two-zone effects pack a pair of 3-bit values into byte 3.
    let (low3, high3) = (block[3] & 0x7, (block[3] >> 3) & 0x7);

    let t = |mode: TriggerMode| Trig { mode: mode.pin(), ..Trig::default() };
    match block[0] {
        0x21 => Trig { start: zone(lo), strength: force(force_at(lo)), ..t(TriggerMode::Feedback) },
        0x22 => Trig {
            start: zone(lo),
            end: zone(hi),
            strength: force(low3),
            strength2: force(high3),
            ..t(TriggerMode::Bow)
        },
        // Galloping packs the SECOND foot low and the first above it.
        0x23 => Trig {
            start: zone(lo),
            end: zone(hi),
            strength: force(high3),
            strength2: force(low3),
            freq: byte(block[4]),
            ..t(TriggerMode::Galloping)
        },
        0x25 => Trig {
            start: zone(lo),
            end: zone(hi),
            strength: force(block[3]),
            ..t(TriggerMode::Weapon)
        },
        0x26 => Trig {
            start: zone(lo),
            strength: force(force_at(lo)),
            freq: byte(block[9]),
            ..t(TriggerMode::Vibration)
        },
        0x27 => Trig {
            start: zone(lo),
            end: zone(hi),
            strength: force(low3),
            strength2: force(high3),
            freq: byte(block[4]),
            period: byte(block[5]),
            ..t(TriggerMode::Machine)
        },
        _ => Trig::default(), // 0x00 / 0x05 (off) and anything unrecognized.
    }
}

