//! The Sony vendor block's signs of life: the counters, the IMU clock and the
//! noise tail that a real DualSense writes into every input report, beside the
//! sticks and gyro we already encode.
//!
//! Measured from a real wired DualSense at rest (250 Hz, `hm_report_record`):
//!
//! * byte 7 and the `uint32` at bytes 12..15 each advance exactly once per
//!   report — two independent counters, not one mirrored;
//! * the `uint32`s at bytes 28..31 and 49..52 are a free-running **3 MHz** clock
//!   (+12 000 ticks per 4 ms report), the second stamp running ~2 ms ahead of
//!   the first — the pad's own time base for the samples in the report;
//! * bytes 56..63 are uncorrelated per-report noise (mean |Δ| 85 of 255, i.e.
//!   uniform, a byte repeats its previous value in ~0.5% of reports).
//!
//! This matters because motion consumers — Sony's own stack, SDL, a native
//! title's gyro-driven features — work out *what changed since the last frame*
//! from these fields rather than from the arrival of a report. Held at zero
//! they describe the same frame forever: raw gyro still steers, but anything
//! that integrates the motion never engages. So the counters are stamped once
//! per published report, never per state change, and an idle pad still streams
//! fresh frames.
//!
//! The fields are opt-in per profile: the encoder only writes what the profile
//! declares (see `ExtendedLayout::{in_sequence, in_counter32, in_ticks,
//! in_ticks2, in_noise}`), so a pad that has no such region is untouched.

use std::time::Instant;

use flexinput_hidmaestro::encode::GamepadState;

/// Ticks per microsecond of a real pad's IMU clock: 3 MHz.
const TICKS_PER_MICRO: u64 = 3;
/// How far ahead the pad's second clock stamp sits behind the first: measured
/// 1.7..2.6 ms apart, so 2 ms.
const TICKS_AHEAD: u32 = 6_000;

/// Per-device state for the vendor block's moving fields.
pub struct PadClock {
    start: Instant,
    sequence: u8,
    counter: u32,
    noise: u64,
}

impl PadClock {
    /// Start from the profile's `sequence_initial`. `seed` only keeps two
    /// deployed personas' noise streams apart; it never affects the counters.
    pub fn new(sequence_initial: u8, seed: u64) -> Self {
        Self {
            start: Instant::now(),
            sequence: sequence_initial,
            counter: 0,
            noise: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// Advance by one published report and stamp the vendor fields onto
    /// `state`. Call exactly once per report handed to the host, after the
    /// pins have been read.
    pub fn stamp(&mut self, state: &mut GamepadState) {
        self.sequence = self.sequence.wrapping_add(1);
        self.counter = self.counter.wrapping_add(1);

        // The noise tail: xorshift64 fed by the report counter, so consecutive
        // reports differ in every bit and no value ever needs storing. Uniform
        // bytes are what the hardware shows (mean |Δ| 85 ≈ the 85.5 expected of
        // two independent uniform bytes).
        let mut x = self.noise ^ (self.counter as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        if x == 0 {
            x = 0x2545_F491_4F6C_DD1D;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.noise = x;

        state.sequence = self.sequence;
        state.report_counter = self.counter;
        // A real clock, not a per-report increment: the pad's own time base
        // keeps running between our reports, so a persona that publishes at
        // something other than 250 Hz still tells the truth about elapsed time.
        let ticks = (self.start.elapsed().as_micros() as u64 * TICKS_PER_MICRO) as u32;
        state.sensor_ticks = ticks;
        state.sensor_ticks2 = ticks.wrapping_add(TICKS_AHEAD);
        state.vendor_noise = x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flexinput_hidmaestro::Profile;

    fn layout() -> Profile {
        Profile::from_json(include_str!("../../hidmaestro/profiles/dualsense.json")).unwrap()
    }

    #[test]
    fn every_moving_field_moves_per_report() {
        let mut clock = PadClock::new(0, 1);
        let mut prev = GamepadState::neutral();
        for i in 0..50 {
            let mut st = GamepadState::neutral();
            clock.stamp(&mut st);
            assert_eq!(st.sequence, (i + 1) as u8, "byte 7 once per report");
            assert_eq!(st.report_counter, i + 1, "the 32-bit counter likewise");
            if i > 0 {
                assert_ne!(st.vendor_noise, prev.vendor_noise, "nor the noise tail");
                assert!(st.sensor_ticks >= prev.sensor_ticks, "the clock never runs back");
            }
            prev = st;
        }
    }

    #[test]
    fn the_clock_runs_at_hardware_rate() {
        // 3 MHz: 20 ms after the device starts the stamp reads ~60 000 ticks,
        // and the second stamp sits ~2 ms (6 000 ticks) ahead of the first.
        let mut clock = PadClock::new(0, 7);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut st = GamepadState::neutral();
        clock.stamp(&mut st);
        let at_least = 20_000 * TICKS_PER_MICRO;
        assert!(
            st.sensor_ticks >= at_least as u32 && st.sensor_ticks < (at_least * 4) as u32,
            "20 ms at 3 MHz ≈ {at_least}, got {}",
            st.sensor_ticks
        );
        assert_eq!(st.sensor_ticks2, st.sensor_ticks.wrapping_add(TICKS_AHEAD));
    }

    #[test]
    fn the_profile_declares_the_fields_this_fills() {
        let p = layout();
        assert_eq!(p.extended.in_sequence, Some(7));
        assert_eq!(p.extended.in_counter32, Some(12), "the pad's 32-bit counter");
        assert_eq!(p.extended.in_ticks, Some(28), "the IMU clock");
        assert_eq!(p.extended.in_ticks2, Some(49), "its second stamp");
        assert_eq!(p.extended.in_noise, Some(56), "the noise tail");
    }

    #[test]
    fn noise_looks_like_hardware_not_zeros() {
        let mut clock = PadClock::new(0, 3);
        let mut all_zero = false;
        let mut repeats = 0;
        let mut prev = [0u8; 8];
        let mut abs_delta_sum = 0i64;
        let mut st = GamepadState::neutral();
        for i in 0..300 {
            clock.stamp(&mut st);
            if st.vendor_noise == [0u8; 8] {
                all_zero = true;
            }
            if i > 0 && st.vendor_noise == prev {
                repeats += 1;
            }
            if i > 0 {
                for b in 0..8 {
                    abs_delta_sum += (st.vendor_noise[b] as i16 - prev[b] as i16).abs() as i64;
                }
            }
            prev = st.vendor_noise;
        }
        assert!(!all_zero, "never a run of zeros");
        assert!(repeats <= 2, "a full tail repeat in {repeats}/299 reports");
        // Hardware: mean |Δ| per byte 80..88. Uniform noise expects ~85.5.
        let mean = abs_delta_sum as f64 / (299.0 * 8.0);
        assert!(mean > 60.0 && mean < 110.0, "mean |Δ| per byte was {mean:.1}");
    }

    #[test]
    fn a_stamped_report_carries_all_of_it_on_the_wire() {
        use flexinput_hidmaestro::encode::{encode_report, GamepadState};

        let p = layout();
        let mut clock = PadClock::new(0, 11);
        let mut st = GamepadState::neutral();
        clock.stamp(&mut st);
        let r = encode_report(&p, &st);
        assert_eq!(r[7], st.sequence, "byte 7");
        assert_eq!(&r[12..16], &st.report_counter.to_le_bytes(), "bytes 12..15");
        assert_eq!(&r[28..32], &st.sensor_ticks.to_le_bytes(), "bytes 28..31");
        assert_eq!(&r[49..53], &st.sensor_ticks2.to_le_bytes(), "bytes 49..52");
        assert_eq!(&r[56..64], &st.vendor_noise, "bytes 56..63");
        // The static vendor bytes the profile pins stay pinned: they never move
        // on hardware, so ours must not either.
        assert_eq!(r[32], 0x18, "byte 32 (measured 0x17-0x18)");
        assert_eq!(&r[42..44], &[0x09, 0x09], "bytes 42..43");
        assert_eq!(r[54], 0x08, "byte 54");
        assert_eq!(r[55], 0x00, "byte 55");

        // Two reports from one device differ ONLY in the moving fields.
        let mut st2 = GamepadState::neutral();
        clock.stamp(&mut st2);
        let r2 = encode_report(&p, &st2);
        for i in (8..12).chain(16..28).chain(32..49).chain(53..56) {
            assert_eq!(r[i], r2[i], "byte {i} held still between two idle reports");
        }
    }
}
