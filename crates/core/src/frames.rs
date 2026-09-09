//! The two coordinate frames orientation quaternions live in, and the single
//! rotation between them.
//!
//! ⭐ **This exists because the conversion was defined in one place and needed
//! in three.** FlexInput's canonical IMU frame is Z-UP — the contract on
//! `flexinput_devices::gyro::HidReading` defines `z` as vertical, positive with
//! the controller lying flat and face up. The 3D controller renderer builds its
//! camera with `Mat4::look_at_rh(.., Vec3::Y)`, so its world is Y-UP.
//!
//! ❗ A quaternion is meaningless without knowing which of those it is in, and
//! applying the wrong one is not a subtle error: it permutes the axes, so yaw
//! renders as pitch, pitch as roll and roll as yaw. That has now happened twice
//! in opposite directions — once when a device's canonical quaternion was drawn
//! unconverted, and once when a module's already-viewer-frame quaternion was
//! converted as though it were canonical.
//!
//! **The contract: every `Vec4` orientation signal on a pin is CANONICAL.**
//! Producers convert on the way out, renderers convert on the way in, and
//! neither guesses.

use glam::{Mat3, Quat, Vec3};

/// Rotation taking canonical axes onto renderer axes.
///
/// ```text
///   canonical +x (forward)  -> viewer -z (into the screen)
///   canonical +y (side)     -> viewer -x
///   canonical +z (up)       -> viewer +y (up)
/// ```
///
/// ❗ Signs are chosen so this is a proper rotation (determinant +1) rather
/// than a reflection. A mirrored basis looks entirely plausible while inverting
/// every rotation direction, which is a far harder fault to spot than an
/// obviously wrong axis.
pub fn canonical_to_viewer() -> Quat {
    // Columns are the images of the canonical basis vectors.
    Quat::from_mat3(&Mat3::from_cols(
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(-1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
    ))
}

/// Re-express a canonical-frame orientation in the renderer's frame.
///
/// A change of basis, `q' = R q R⁻¹` — not a multiplication by `R`. Composing
/// instead of conjugating adds a fixed rotation to the pose, which reads as a
/// model that starts out facing the wrong way and is easy to mistake for a
/// modelling error.
pub fn canonical_to_viewer_quat(q: Quat) -> Quat {
    let r = canonical_to_viewer();
    r * q * r.conjugate()
}

/// The inverse: re-express a renderer-frame orientation as canonical.
///
/// For a producer that integrates in the renderer's frame and has to publish on
/// a pin, which is canonical by contract.
pub fn viewer_to_canonical_quat(q: Quat) -> Quat {
    let r = canonical_to_viewer();
    r.conjugate() * q * r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn the_basis_map_is_the_documented_one() {
        let r = canonical_to_viewer();
        assert!(close(r * Vec3::X, Vec3::new(0.0, 0.0, -1.0)), "forward -> into the screen");
        assert!(close(r * Vec3::Y, Vec3::new(-1.0, 0.0, 0.0)), "side -> viewer -x");
        assert!(close(r * Vec3::Z, Vec3::Y), "up -> up");
    }

    #[test]
    fn it_is_a_rotation_and_not_a_reflection() {
        // ⭐ The failure this guards against does not look like a failure. A
        // reflection maps the axes just as convincingly and silently inverts
        // every rotation direction.
        let m = Mat3::from_quat(canonical_to_viewer());
        assert!((m.determinant() - 1.0).abs() < 1e-5, "det {}", m.determinant());
    }

    #[test]
    fn the_two_conversions_are_inverses() {
        let q = Quat::from_axis_angle(Vec3::new(0.3, -0.5, 0.81).normalize(), 1.1);
        let round_trip = viewer_to_canonical_quat(canonical_to_viewer_quat(q));
        assert!(round_trip.abs_diff_eq(q, 1e-5), "{round_trip:?} vs {q:?}");
    }

    #[test]
    fn a_canonical_yaw_becomes_a_rotation_about_the_viewers_up_axis() {
        // ❗ The exact symptom that motivated this: a yaw drawn without
        // conversion spins about the axis pointing at the camera, which is
        // roll. Reported from hardware as "when I do yaw rotation, it rolls
        // both handles".
        let yaw = Quat::from_axis_angle(Vec3::Z, 0.7);
        let (axis, angle) = canonical_to_viewer_quat(yaw).to_axis_angle();
        assert!(close(axis, Vec3::Y), "axis {axis:?}");
        assert!((angle - 0.7).abs() < 1e-5, "angle {angle}");
    }

    #[test]
    fn conjugation_leaves_the_identity_alone() {
        // Composing with `R` instead of conjugating would put a fixed rotation
        // here, and a model that starts out facing sideways reads as a broken
        // mesh rather than a broken transform.
        assert!(canonical_to_viewer_quat(Quat::IDENTITY).abs_diff_eq(Quat::IDENTITY, 1e-6));
    }
}
