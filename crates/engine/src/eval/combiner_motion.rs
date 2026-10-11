//! The AutoMap Combiner's Motion section: gyro, accelerometer and orientation
//! resolved as ONE unit, never pin by pin.
//!
//! ❗ Per-pin SORT ("first non-zero value wins") is wrong for motion. A resting
//! pad's gyro deadzone writes exact zeros, so the next port's gyro leaked
//! through — and since every axis was decided on its own, gyro X could come
//! from one Joy-Con and gyro Y from the other. Two IMUs mixed axis by axis is
//! no rotation at all.
//!
//! Two modes (`combiner_motion` param):
//! * `SORT` (default) — the whole section from the top port that carries
//!   motion, zeros included. A port without an IMU never takes part.
//! * `CORRELATE` — every motion-carrying port fused. Only meaningful when the
//!   IMUs are rigidly joined: two Joy-Cons in a grip, or an extra IMU strapped
//!   to a pad that has its own. Each lower port's mounting rotation relative
//!   to the top one is learned by correlating their gyros (Wahba's problem,
//!   Davenport's q-method); once the fit is good the aligned readings are
//!   averaged. Until then — and whenever the fit degrades — the top port
//!   alone, exactly as SORT.

use std::collections::HashMap;

use flexinput_core::Signal;
use glam::{Quat, Vec3, Vec4};

/// The pins the section moves as one.
pub(crate) const MOTION_PINS: [&str; 7] =
    ["gyro_x", "gyro_y", "gyro_z", "accel_x", "accel_y", "accel_z", "orientation"];

pub(crate) fn is_motion_pin(pin: &str) -> bool {
    MOTION_PINS.contains(&pin)
}

/// One port's motion, as far as it carries it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PortMotion {
    pub gyro: Option<Vec3>,
    pub accel: Option<Vec3>,
    pub orientation: Option<Vec4>,
}

impl PortMotion {
    pub fn read(mut get: impl FnMut(&str) -> Option<Signal>) -> Option<PortMotion> {
        let mut v3 = |x: &str, y: &str, z: &str| -> Option<Vec3> {
            let (x, y, z) = (get(x), get(y), get(z));
            if x.is_none() && y.is_none() && z.is_none() {
                return None;
            }
            let f = |s: Option<Signal>| s.map(|s| s.as_float()).unwrap_or(0.0);
            Some(Vec3::new(f(x), f(y), f(z)))
        };
        let gyro = v3("gyro_x", "gyro_y", "gyro_z");
        let accel = v3("accel_x", "accel_y", "accel_z");
        let orientation = match get("orientation") {
            Some(Signal::Vec4(v)) => Some(v),
            _ => None,
        };
        let m = PortMotion { gyro, accel, orientation };
        (m != PortMotion::default()).then_some(m)
    }

    /// The section's pins, as written to the bus.
    pub fn signals(&self) -> Vec<(&'static str, Signal)> {
        let mut out = Vec::new();
        if let Some(g) = self.gyro {
            out.extend([("gyro_x", g.x), ("gyro_y", g.y), ("gyro_z", g.z)]
                .map(|(p, v)| (p, Signal::Float(v))));
        }
        if let Some(a) = self.accel {
            out.extend([("accel_x", a.x), ("accel_y", a.y), ("accel_z", a.z)]
                .map(|(p, v)| (p, Signal::Float(v))));
        }
        if let Some(o) = self.orientation {
            out.push(("orientation", Signal::Vec4(o)));
        }
        out
    }
}

/// Resolve the section. `ports[i]` is port i's motion (`None` = carries none),
/// `ids[i]` its identity, so a learned alignment is dropped when a different
/// device takes the port. Returns `None` when no port carries motion.
pub(crate) fn resolve(
    correlate: bool,
    ports: &[Option<PortMotion>],
    ids: &[String],
    state: &mut Option<Box<MotionCorrelator>>,
    dt: f32,
) -> Option<PortMotion> {
    let top = ports.iter().position(|p| p.is_some())?;
    let reference = ports[top]?;
    if !correlate {
        *state = None;
        return Some(reference);
    }
    let corr = state.get_or_insert_with(Default::default);
    corr.fuse(top, reference, ports, ids, dt)
}

/// Gyro magnitude (bus units, 1.0 = 2000 °/s) below which a sample says
/// nothing about alignment: deadzone, tremor, noise. ≈ 20 °/s.
const MIN_RATE: f32 = 0.01;
/// How long the alignment remembers, so re-gripping is picked up.
const FORGET_SECS: f32 = 30.0;
/// Seconds of shared motion before the fit is trusted at all.
const MIN_MOTION_SECS: f32 = 1.0;
/// How far the motion must have spread off one axis. A rotation about a
/// single axis fixes the alignment only up to a spin about that axis, and
/// still scores a perfect fit — so a perfect score alone proves nothing.
/// 0 = one axis only, 0.5 = two axes equally, 0.67 = all three.
const MIN_SPREAD: f32 = 0.15;
/// Fit quality to start (and to keep) fusing: 1 = the aligned gyros agree
/// exactly. Hysteresis so a borderline fit doesn't flicker.
const LOCK_QUALITY: f32 = 0.93;
const UNLOCK_QUALITY: f32 = 0.85;
/// Re-solve at most this often; accumulating is per tick, solving isn't.
const SOLVE_EVERY_SECS: f32 = 0.1;

/// Per-node state for `CORRELATE`: one alignment per lower port.
#[derive(Default)]
pub struct MotionCorrelator {
    ports: HashMap<usize, Alignment>,
}

impl MotionCorrelator {
    fn fuse(
        &mut self,
        top: usize,
        reference: PortMotion,
        ports: &[Option<PortMotion>],
        ids: &[String],
        dt: f32,
    ) -> Option<PortMotion> {
        let id = |i: usize| ids.get(i).cloned().unwrap_or_default();
        // A port whose device changed, or that stopped carrying motion, starts over.
        self.ports.retain(|&i, a| {
            i != top && ports.get(i).is_some_and(|p| p.is_some()) && a.device == id(i)
        });
        let mut gyro_sum = reference.gyro;
        let mut accel_sum = reference.accel;
        let mut n = 1.0f32;
        for (i, port) in ports.iter().enumerate() {
            let Some(port) = port else { continue };
            if i == top {
                continue;
            }
            let a = self.ports.entry(i).or_insert_with(|| Alignment::new(id(i)));
            if let (Some(r), Some(b)) = (reference.gyro, port.gyro) {
                a.observe(r, b, dt);
            }
            a.update_lock(i, top);
            if !a.locked {
                continue;
            }
            let (Some(rg), Some(bg)) = (gyro_sum, port.gyro) else { continue };
            gyro_sum = Some(rg + a.rot * bg);
            accel_sum = match (accel_sum, port.accel) {
                (Some(ra), Some(ba)) => Some(ra + a.rot * ba),
                (ra, _) => ra,
            };
            n += 1.0;
        }
        Some(PortMotion {
            gyro: gyro_sum.map(|g| g / n),
            accel: accel_sum.map(|a| a / n),
            // An absolute pose can't be averaged component-wise; the top
            // port's stands for the assembly.
            orientation: reference.orientation,
        })
    }
}

/// One lower port's learned mounting rotation into the top port's frame.
struct Alignment {
    device: String,
    /// Σ w · r bᵀ, row-major — the attitude profile matrix.
    b: [[f32; 3]; 3],
    /// Σ w · |r||b|: the best `b`'s top eigenvalue could reach.
    scale: f32,
    /// Seconds of shared motion (decayed with the rest).
    motion_secs: f32,
    /// Σ w · b̂ b̂ᵀ: which directions the motion has covered.
    spread: [[f32; 3]; 3],
    since_solve: f32,
    /// Davenport's eigenvector, kept between solves as the warm start.
    q: Vec4,
    rot: Quat,
    quality: f32,
    locked: bool,
}

impl Alignment {
    fn new(device: String) -> Self {
        Alignment {
            device,
            b: [[0.0; 3]; 3],
            scale: 0.0,
            motion_secs: 0.0,
            spread: [[0.0; 3]; 3],
            since_solve: SOLVE_EVERY_SECS,
            q: Vec4::new(0.0, 0.0, 0.0, 1.0),
            rot: Quat::IDENTITY,
            quality: 0.0,
            locked: false,
        }
    }

    fn observe(&mut self, r: Vec3, b: Vec3, dt: f32) {
        let keep = (-dt / FORGET_SECS).exp();
        for row in self.b.iter_mut().chain(self.spread.iter_mut()) {
            for v in row.iter_mut() {
                *v *= keep;
            }
        }
        self.scale *= keep;
        self.motion_secs *= keep;
        self.since_solve += dt;
        let (rl, bl) = (r.length(), b.length());
        if rl < MIN_RATE || bl < MIN_RATE {
            return;
        }
        let (ra, ba, bh) = (r.to_array(), b.to_array(), (b / bl).to_array());
        for i in 0..3 {
            for j in 0..3 {
                self.b[i][j] += dt * ra[i] * ba[j];
                self.spread[i][j] += dt * bh[i] * bh[j];
            }
        }
        self.scale += dt * rl * bl;
        self.motion_secs += dt;
    }

    /// 0 when the motion so far lies along one axis, up to 2/3 for all three
    /// alike: `1 − ‖M‖²_F / tr(M)²` of the direction covariance.
    fn spread(&self) -> f32 {
        let m = &self.spread;
        let tr = m[0][0] + m[1][1] + m[2][2];
        if tr <= f32::EPSILON {
            return 0.0;
        }
        let fro: f32 = m.iter().flatten().map(|v| v * v).sum();
        1.0 - fro / (tr * tr)
    }

    fn update_lock(&mut self, port: usize, top: usize) {
        if self.since_solve < SOLVE_EVERY_SECS {
            return;
        }
        self.since_solve = 0.0;
        if self.motion_secs < MIN_MOTION_SECS || self.spread() < MIN_SPREAD || self.scale <= 0.0 {
            if self.locked {
                self.locked = false;
                eprintln!("[combiner] motion correlation in_{port} → in_{top} released: not enough shared motion");
            }
            return;
        }
        let (q, lambda) = davenport(&self.b, self.scale, self.q);
        self.q = q;
        // Davenport's quaternion is the PASSIVE attitude; the active rotation
        // that takes this port's vectors into the top port's frame is its
        // conjugate (pinned by `recovers_a_known_mounting_rotation`).
        self.rot = Quat::from_xyzw(-q.x, -q.y, -q.z, q.w).normalize();
        self.quality = lambda / self.scale;
        let was = self.locked;
        self.locked = if was { self.quality >= UNLOCK_QUALITY } else { self.quality >= LOCK_QUALITY };
        if self.locked != was {
            let (axis, angle) = self.rot.to_axis_angle();
            eprintln!(
                "[combiner] motion correlation in_{port} → in_{top} {}: quality {:.3}, mounting {:.1}° about {:?}",
                if self.locked { "LOCKED" } else { "released" },
                self.quality,
                angle.to_degrees(),
                axis,
            );
        }
    }
}

/// Davenport's q-method: the unit quaternion (x, y, z, w) maximising
/// `qᵀ K q`, and that maximum. Power iteration on `K + c·I`, which shifts
/// every eigenvalue non-negative (|λ| ≤ c) so the top one dominates;
/// warm-started from the last answer, so a steady fit needs few steps.
fn davenport(b: &[[f32; 3]; 3], c: f32, warm: Vec4) -> (Vec4, f32) {
    let sigma = b[0][0] + b[1][1] + b[2][2];
    let z = [b[1][2] - b[2][1], b[2][0] - b[0][2], b[0][1] - b[1][0]];
    let mut k = [[0.0f32; 4]; 4];
    for i in 0..3 {
        for j in 0..3 {
            k[i][j] = b[i][j] + b[j][i] - if i == j { sigma } else { 0.0 };
        }
        k[i][3] = z[i];
        k[3][i] = z[i];
    }
    k[3][3] = sigma;
    let mul = |v: Vec4| -> Vec4 {
        let a = v.to_array();
        let row = |r: usize| (0..4).map(|j| k[r][j] * a[j]).sum::<f32>();
        Vec4::new(row(0), row(1), row(2), row(3))
    };
    let mut q = if warm.length_squared() > 0.5 { warm.normalize() } else { Vec4::W };
    for _ in 0..60 {
        let next = mul(q) + q * c;
        let len = next.length();
        if len <= f32::EPSILON {
            break;
        }
        q = next / len;
    }
    // Keep w ≥ 0 so warm starts and the logged angle stay on one hemisphere.
    if q.w < 0.0 {
        q = -q;
    }
    let lambda = q.dot(mul(q));
    (q, lambda)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(g: Vec3) -> Option<PortMotion> {
        Some(PortMotion { gyro: Some(g), accel: Some(Vec3::Z * 0.125), orientation: None })
    }

    #[test]
    fn sort_takes_the_whole_section_from_the_top_motion_port_zeros_included() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut st = None;
        // Port 0 has no IMU; port 1 rests at exact zero; port 2 is turning.
        let ports = [None, port(Vec3::ZERO), port(Vec3::new(0.3, 0.2, 0.1))];
        let m = resolve(false, &ports, &ids, &mut st, 0.001).unwrap();
        assert_eq!(m.gyro, Some(Vec3::ZERO), "the resting top port wins, not the moving one");
        assert!(resolve(false, &[None, None], &ids, &mut st, 0.001).is_none());
    }

    /// Drive two rigidly joined IMUs, the second mounted at `mount`, through
    /// motion about several axes; return the fused output and the correlator.
    fn drive(mount: Quat, secs: f32) -> (PortMotion, Option<Box<MotionCorrelator>>) {
        let ids = vec!["top".to_string(), "strapped".to_string()];
        let mut st = None;
        let dt = 0.002;
        let mut out = None;
        let steps = (secs / dt) as usize;
        for k in 0..steps {
            let t = k as f32 * dt;
            // A tumbling rate that keeps changing axis.
            let w = Vec3::new((t * 1.3).sin(), (t * 0.7).cos(), (t * 2.1).sin() * 0.6) * 0.2;
            // The strapped IMU sees the same body rate in its own frame.
            let b = mount.inverse() * w;
            out = resolve(true, &[port(w), port(b)], &ids, &mut st, dt);
            if k == steps - 1 {
                let fused = out.unwrap().gyro.unwrap();
                assert!((fused - w).length() < 1e-3 * w.length().max(1.0) + 2e-3,
                    "fused {fused} vs truth {w}");
            }
        }
        (out.unwrap(), st)
    }

    #[test]
    fn recovers_a_known_mounting_rotation() {
        let mount = Quat::from_euler(glam::EulerRot::YXZ, 1.1, -0.4, 2.3);
        let (_, st) = drive(mount, 6.0);
        let a = &st.unwrap().ports[&1];
        assert!(a.locked, "quality {}", a.quality);
        assert!(a.rot.angle_between(mount) < 0.02, "learned {:?} vs mounted {:?}", a.rot, mount);
    }

    #[test]
    fn joycons_in_a_grip_lock_near_identity() {
        let (_, st) = drive(Quat::IDENTITY, 4.0);
        let a = &st.unwrap().ports[&1];
        assert!(a.locked);
        assert!(a.rot.angle_between(Quat::IDENTITY) < 0.02);
    }

    #[test]
    fn a_single_axis_never_locks() {
        let ids = vec!["top".to_string(), "strapped".to_string()];
        let mut st = None;
        let mount = Quat::from_rotation_x(1.0);
        for k in 0..5000 {
            let w = Vec3::Z * (0.2 + 0.1 * (k as f32 * 0.01).sin());
            let fused = resolve(true, &[port(w), port(mount.inverse() * w)], &ids, &mut st, 0.002)
                .unwrap().gyro.unwrap();
            assert_eq!(fused, w, "unlocked: the top port alone");
        }
        assert!(!st.unwrap().ports[&1].locked, "one axis can't fix a 3D alignment");
    }

    #[test]
    fn uncorrelated_imus_never_lock() {
        let ids = vec!["top".to_string(), "other".to_string()];
        let mut st = None;
        for k in 0..5000 {
            let t = k as f32 * 0.002;
            let a = Vec3::new((t * 1.3).sin(), (t * 0.7).cos(), (t * 2.1).sin()) * 0.2;
            let b = Vec3::new((t * 3.7).cos(), (t * 0.3).sin(), (t * 5.1).cos()) * 0.2;
            resolve(true, &[port(a), port(b)], &ids, &mut st, 0.002);
        }
        assert!(!st.unwrap().ports[&1].locked);
    }
}
