//! Per-controller calibration measured at runtime, shared with the decoder.
//!
//! ⭐ **Why a registry and not a parameter.** Resting gyro drift is a property
//! of one physical sensor: the two halves of a grip measure meaningfully
//! different offsets, and those offsets move with temperature and with each
//! power cycle. A constant compiled into [`crate::reports`] can only ever be an
//! average of the units the author happened to own, so the useful value is the
//! one measured on the user's own hardware — which means it arrives from the
//! UI, at runtime, long after the decoder was built.
//!
//! ❗ It has to reach the DECODER, not just the output pins. Orientation is
//! integrated inside [`crate::reports::OrientationTracker`], so a correction
//! applied downstream cannot undo drift that has already been integrated into
//! the estimate — the pins would read clean while the 3D model still wandered.
//! Subtracting at the source fixes both at once, because the pins are derived
//! from the same rate.
//!
//! A process-global map rather than a field threaded through each transport:
//! there are THREE independent drive loops (BLE via the OS, BLE via the
//! dongle, and USB), each owning its own tracker, and each would otherwise need
//! its own plumbing for the same value. This also matches how the rest of the
//! crate already handles runtime configuration ([`crate::reports::field_gain`]
//! and friends).

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::hub::PadKey;

fn registry() -> &'static RwLock<HashMap<PadKey, [f32; 3]>> {
    static MAP: OnceLock<RwLock<HashMap<PadKey, [f32; 3]>>> = OnceLock::new();
    MAP.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Record a measured resting drift for one half, in **degrees per second, field
/// order, before any gain** — the same space as
/// [`crate::reports::RESTING_DRIFT_LEFT`], so the decoder can use it as a
/// straight replacement.
///
/// `None` clears it and restores the compiled-in default.
///
/// Cheap to call repeatedly with an unchanged value: the caller pushes this
/// every tick so that a controller which reconnects picks its calibration back
/// up, and an unchanged value takes only a read lock.
pub fn set_field_drift(key: PadKey, drift: Option<[f32; 3]>) {
    let same = {
        let map = registry().read().unwrap();
        match (map.get(&key), drift) {
            (Some(cur), Some(new)) => *cur == new,
            (None, None) => true,
            _ => false,
        }
    };
    if same {
        return;
    }
    let mut map = registry().write().unwrap();
    match drift {
        Some(d) => {
            log::info!(
                "joycon2: measured resting drift for {} {} = [{:+.4}, {:+.4}, {:+.4}] deg/s",
                key.side.display_name(),
                key.address_slug(),
                d[0],
                d[1],
                d[2],
            );
            map.insert(key, d);
        }
        None => {
            map.remove(&key);
        }
    }
}

/// The measured drift for one half, or `None` if it has never been calibrated.
///
/// Callers fall back to [`crate::reports::resting_drift`] — the compiled
/// default — rather than to zero, so an uncalibrated controller is no worse off
/// than it was before this existed.
pub fn field_drift(key: &PadKey) -> Option<[f32; 3]> {
    registry().read().unwrap().get(key).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Side;

    fn key(side: Side, last: u8) -> PadKey {
        PadKey { side, address: [0, 0, 0, 0, 0, last] }
    }

    /// The two halves must not share an entry.
    ///
    /// They are separate sensors with separate offsets — measured at −0.410 and
    /// −0.335 °/s on the dominant axis of one grip — so a registry that keyed
    /// on anything coarser than the individual controller would apply one
    /// half's correction to the other and make the worse half worse.
    #[test]
    fn each_half_keeps_its_own_measurement() {
        let l = key(Side::Left, 1);
        let r = key(Side::Right, 2);
        set_field_drift(l, Some([-0.410, 0.046, -0.034]));
        set_field_drift(r, Some([-0.335, -0.008, 0.021]));
        assert_eq!(field_drift(&l), Some([-0.410, 0.046, -0.034]));
        assert_eq!(field_drift(&r), Some([-0.335, -0.008, 0.021]));
        set_field_drift(l, None);
        assert_eq!(field_drift(&l), None, "clearing one must not touch the other");
        assert_eq!(field_drift(&r), Some([-0.335, -0.008, 0.021]));
        set_field_drift(r, None);
    }

    /// Two same-side halves (two left Joy-Cons, two players) are different
    /// controllers and must not collide.
    #[test]
    fn same_side_halves_are_distinct() {
        let a = key(Side::Left, 0xaa);
        let b = key(Side::Left, 0xbb);
        set_field_drift(a, Some([1.0, 0.0, 0.0]));
        assert_eq!(field_drift(&b), None);
        set_field_drift(a, None);
    }
}
