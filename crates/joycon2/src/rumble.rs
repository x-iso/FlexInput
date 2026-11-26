//! HD rumble for a Joy-Con 2 half: the sample encoding, the frames, and the
//! pacing every transport shares.
//!
//! # The sample
//!
//! One LRA per half, driven by two oscillators. A sample is five bytes, four
//! 10-bit fields packed little endian (SDL 3.4 `SDL_hidapi_switch2.c`,
//! `EncodeHDRumble`; the field order is spelled out in libsdl-org/SDL#16680):
//!
//! ```text
//!   bits  0..10  high band frequency
//!   bits 10..20  high band amplitude
//!   bits 20..30  low band frequency
//!   bits 30..40  low band amplitude
//! ```
//!
//! **Frequency** is logarithmic, `code = round(log2(Hz / 10) * 96)`: measured
//! by the community with a Pro Controller 2's accelerometer, not documented by
//! Nintendo. SDL's defaults, `0x187` / `0x112`, are then ~168 Hz and ~72 Hz;
//! the two profiles the M12 Pro fork plays decode to 183 + 260 Hz and
//! 109 + 213 Hz, all in the actuator's band.
//!
//! **Amplitude** has no documented unit. SDL treats it as linear and caps it at
//! [`AMP_MAX`]: *"Rumble can get so strong that it might be dangerous to the
//! controller."* Every amplitude here is a share of that cap, so nothing a
//! patch sends can go past it.
//!
//! # The frame
//!
//! A 16-byte block: `0x50 | sequence` (low nibble rolls with every frame), one
//! sample, zeros. The same block opens the USB output report `0x01` (SDL sends
//! it exactly so) and the Bluetooth rumble characteristic, `0x0012`, behind a
//! leading zero byte (the M12 Pro fork's `SendRumble`, 42 bytes).
//!
//! ❗ Not tried on hardware yet. Over Bluetooth, `0x0012` is what both
//! references write rumble to, but the M12-S ignores the common command
//! channel `0x0014` and serves everything on `0x0016`. If it stays silent
//! here too, the same block in the rumble region of a `0x0016` frame is the
//! next thing to try.
//!
//! # Pacing
//!
//! [`Player`] sends one frame every [`FRAME_GAP`] while anything plays, SDL's
//! interval, carrying the latest target, then a stop frame twice when it
//! falls silent. A target that changes faster than that is sampled, not
//! queued, so a stream of changing haptics can never back the link up.

use std::time::{Duration, Instant};

/// Largest amplitude code sent: SDL's cap, `29000 >> 6` of 1023.
pub const AMP_MAX: u16 = 453;

/// Frequency code SDL plays its low band (a game's strong motor) at: ~72 Hz.
pub const CLASSIC_LOW_CODE: u16 = 0x112;
/// Frequency code SDL plays its high band (a game's weak motor) at: ~168 Hz.
pub const CLASSIC_HIGH_CODE: u16 = 0x187;

/// Time between frames while something plays.
pub const FRAME_GAP: Duration = Duration::from_millis(12);

/// Stop frames sent when the rumble ends. Over Bluetooth a write without
/// response can be lost, and a lost stop would leave the actuator running.
const STOP_FRAMES: u8 = 2;

/// Length of the rumble block.
pub const BLOCK_LEN: usize = 16;
/// Length of a frame for the Bluetooth rumble characteristic: a zero byte and
/// the block, padded as the M12 Pro fork sends it.
pub const BLE_FRAME_LEN: usize = 42;

/// One oscillator.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Band {
    /// Pitch in Hz.
    pub hz: f32,
    /// Strength, 0–1 of [`AMP_MAX`], linear.
    pub amp: f32,
}

/// What the half should play: both oscillators.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rumble {
    pub low: Band,
    pub high: Band,
}

impl Rumble {
    /// Nothing audible.
    pub const SILENT: Rumble = Rumble {
        low: Band { hz: 0.0, amp: 0.0 },
        high: Band { hz: 0.0, amp: 0.0 },
    };

    /// A game's classic rumble the way SDL plays it on these pads: the strong
    /// motor on the low band, the weak one on the high band, each at SDL's
    /// pitch.
    pub fn classic(strong: f32, weak: f32) -> Rumble {
        Rumble {
            low: Band { hz: hz_of_code(CLASSIC_LOW_CODE), amp: strong },
            high: Band { hz: hz_of_code(CLASSIC_HIGH_CODE), amp: weak },
        }
    }

    /// Whether anything would be felt: an amplitude that encodes above zero.
    pub fn is_active(&self) -> bool {
        amp_code(self.low.amp) > 0 || amp_code(self.high.amp) > 0
    }
}

/// Frequency code for a pitch: `round(log2(Hz / 10) * 96)`, held to 10 bits.
pub fn freq_code(hz: f32) -> u16 {
    if !(hz > 10.0) {
        return 0;
    }
    ((hz / 10.0).log2() * 96.0).round().clamp(0.0, 1023.0) as u16
}

/// The pitch a frequency code plays.
pub fn hz_of_code(code: u16) -> f32 {
    10.0 * 2f32.powf(code as f32 / 96.0)
}

/// Amplitude code for a 0–1 strength: linear up to [`AMP_MAX`]. Anything above
/// zero is at least 1, so a faint value does not round to silence.
pub fn amp_code(amp: f32) -> u16 {
    if !(amp > 0.0) {
        return 0;
    }
    ((amp.min(1.0) * AMP_MAX as f32).round() as u16).clamp(1, AMP_MAX)
}

/// The five-byte sample.
pub fn encode_sample(r: &Rumble) -> [u8; 5] {
    let bits = freq_code(r.high.hz) as u64
        | (amp_code(r.high.amp) as u64) << 10
        | (freq_code(r.low.hz) as u64) << 20
        | (amp_code(r.low.amp) as u64) << 30;
    let mut out = [0u8; 5];
    for (i, b) in out.iter_mut().enumerate() {
        *b = (bits >> (i * 8)) as u8;
    }
    out
}

/// The 16-byte rumble block: header with the sequence, one sample.
pub fn block(seq: u8, r: &Rumble) -> [u8; BLOCK_LEN] {
    let mut out = [0u8; BLOCK_LEN];
    out[0] = 0x50 | (seq & 0x0F);
    out[1..6].copy_from_slice(&encode_sample(r));
    out
}

/// A frame for the Bluetooth rumble characteristic (`0x0012`).
pub fn ble_frame(block: &[u8; BLOCK_LEN]) -> [u8; BLE_FRAME_LEN] {
    let mut out = [0u8; BLE_FRAME_LEN];
    out[1..1 + BLOCK_LEN].copy_from_slice(block);
    out
}

/// A USB output report `0x01` carrying only rumble, padded to the report
/// length the HID stack insists on.
pub fn usb_frame(block: &[u8; BLOCK_LEN]) -> Vec<u8> {
    let mut out = vec![0u8; crate::protocol::USB_REPORT_LEN + 1];
    out[0] = crate::protocol::USB_OUTPUT_REPORT_ID;
    out[1..1 + BLOCK_LEN].copy_from_slice(block);
    out
}

/// Paces frames for one half.
#[derive(Debug, Default)]
pub struct Player {
    target: Rumble,
    /// The last audible target sent, whose pitches the stop frames keep.
    played: Rumble,
    seq: u8,
    last: Option<Instant>,
    stops_left: u8,
}

impl Player {
    /// What to play from now on. A transition to silence schedules the stop
    /// frames.
    pub fn set(&mut self, r: Rumble) {
        if self.target.is_active() && !r.is_active() {
            self.stops_left = STOP_FRAMES;
        } else if r.is_active() {
            self.stops_left = 0;
        }
        self.target = r;
    }

    /// Whether a frame is still to come — for a transport that only wakes up
    /// for rumble when there is some.
    pub fn busy(&self) -> bool {
        self.target.is_active() || self.stops_left > 0
    }

    /// The block to send now, if one is due.
    pub fn poll(&mut self, now: Instant) -> Option<[u8; BLOCK_LEN]> {
        if !self.busy() {
            return None;
        }
        if let Some(t) = self.last {
            if now.saturating_duration_since(t) < FRAME_GAP {
                return None;
            }
        }
        let r = if self.target.is_active() {
            self.played = self.target;
            self.target
        } else {
            self.stops_left -= 1;
            // Silent amplitudes, the pitches kept: what SDL sends to stop.
            Rumble {
                low: Band { amp: 0.0, ..self.played.low },
                high: Band { amp: 0.0, ..self.played.high },
            }
        };
        let b = block(self.seq, &r);
        self.seq = self.seq.wrapping_add(1);
        self.last = Some(now);
        Some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packing is SDL's: the profile bytes the M12 Pro fork plays must
    /// decode to the fields it meant, and encoding those fields must give the
    /// bytes back.
    #[test]
    fn the_sample_packs_four_ten_bit_fields_like_sdl() {
        let fields = |b: [u8; 5]| {
            let mut bits = 0u64;
            for (i, v) in b.iter().enumerate() {
                bits |= (*v as u64) << (i * 8);
            }
            [bits & 0x3FF, (bits >> 10) & 0x3FF, (bits >> 20) & 0x3FF, (bits >> 30) & 0x3FF]
        };
        assert_eq!(fields([0x93, 0x35, 0x36, 0x1C, 0x0D]), [0x193, 397, 0x1C3, 52]);
        assert_eq!(fields([0x4B, 0x7D, 0x80, 0x5A, 0x02]), [0x14B, 31, 0x1A8, 9]);

        // SDL's own encoder, written out for the same inputs.
        let sdl = |hf: u16, ha16: u16, lf: u16, la16: u16| {
            [
                (hf & 0xFF) as u8,
                (((ha16 >> 4) & 0xFC) | ((hf >> 8) & 0x03)) as u8,
                ((ha16 >> 12) | (lf << 4)) as u8,
                ((la16 & 0xC0) | ((lf >> 4) & 0x3F)) as u8,
                (la16 >> 8) as u8,
            ]
        };
        let r = Rumble {
            low: Band { hz: hz_of_code(0x112), amp: 200.0 / AMP_MAX as f32 },
            high: Band { hz: hz_of_code(0x187), amp: 1.0 },
        };
        assert_eq!(encode_sample(&r), sdl(0x187, AMP_MAX << 6, 0x112, 200 << 6));
    }

    #[test]
    fn frequencies_are_log_scaled_and_round_trip() {
        assert_eq!(freq_code(160.0), 384);
        assert_eq!(freq_code(320.0), 480);
        for code in [0x112u16, 0x187, 300, 700] {
            assert_eq!(freq_code(hz_of_code(code)), code);
        }
        assert_eq!(freq_code(0.0), 0);
        assert_eq!(freq_code(f32::NAN), 0);
        assert_eq!(freq_code(1e9), 1023);
    }

    #[test]
    fn amplitudes_never_pass_the_cap_and_faint_ones_are_not_lost() {
        assert_eq!(amp_code(0.0), 0);
        assert_eq!(amp_code(-1.0), 0);
        assert_eq!(amp_code(f32::NAN), 0);
        assert_eq!(amp_code(1e-6), 1);
        assert_eq!(amp_code(1.0), AMP_MAX);
        assert_eq!(amp_code(7.0), AMP_MAX);
    }

    #[test]
    fn frames_put_the_block_where_each_transport_expects_it() {
        let b = block(0x13, &Rumble::classic(1.0, 0.0));
        assert_eq!(b[0], 0x53, "0x50 | the sequence's low nibble");
        let ble = ble_frame(&b);
        assert_eq!((ble.len(), ble[0], &ble[1..17]), (42, 0x00, &b[..]));
        assert!(ble[17..].iter().all(|v| *v == 0));
        let usb = usb_frame(&b);
        assert_eq!((usb.len(), usb[0], &usb[1..17]), (64, 0x01, &b[..]));
    }

    /// While playing: one frame per gap with the latest target, the sequence
    /// rolling. On release: two stop frames, then nothing.
    #[test]
    fn the_player_paces_frames_and_stops_twice() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut p = Player::default();
        assert_eq!(p.poll(at(0)), None, "nothing to play");

        p.set(Rumble::classic(0.5, 0.0));
        let first = p.poll(at(0)).expect("first frame at once");
        assert_eq!(first[0], 0x50);
        assert_eq!(p.poll(at(5)), None, "inside the gap");
        p.set(Rumble::classic(1.0, 0.0));
        let second = p.poll(at(12)).expect("next frame after the gap");
        assert_eq!(second[0], 0x51);
        assert_eq!(&second[1..6], &encode_sample(&Rumble::classic(1.0, 0.0)));

        p.set(Rumble::SILENT);
        assert!(p.busy());
        assert_eq!(p.poll(at(13)), None, "the stop waits for the gap too");
        let stop = p.poll(at(24)).expect("stop frame");
        assert_eq!(&stop[1..6], &encode_sample(&Rumble::classic(0.0, 0.0)), "pitches kept, amplitudes zero");
        assert!(p.poll(at(36)).is_some(), "second stop frame");
        assert!(!p.busy());
        assert_eq!(p.poll(at(100)), None, "silent afterwards");
    }

    /// A target that starts again during the stop frames cancels them.
    #[test]
    fn playing_again_cancels_the_pending_stops() {
        let t0 = Instant::now();
        let mut p = Player::default();
        p.set(Rumble::classic(1.0, 0.0));
        p.poll(t0);
        p.set(Rumble::SILENT);
        p.set(Rumble::classic(0.3, 0.0));
        let b = p.poll(t0 + FRAME_GAP).expect("frame");
        assert_eq!(&b[1..6], &encode_sample(&Rumble::classic(0.3, 0.0)));
        p.set(Rumble::classic(0.0, 0.0));
        assert!(p.busy(), "stop frames scheduled again");
    }
}
