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
//!   fused. Until then — and whenever the fit degrades — the top port
//!   alone, exactly as SORT.
//!
//! ⭐ **How CORRELATE removes noise, not just averages it.** Rigidly joined
//! IMUs see the SAME rotation and INDEPENDENT noise, so once aligned:
//! * each sensor's own noise power is measured by CORRELATING it with the
//!   disagreement `e = r - R b`: motion cancels out of `e`, so `E[r_hp . e]`
//!   is the top sensor's noise alone and `-E[b_hp . e]` the other's — no
//!   matter how much the pad is turning. The fusion weights each sensor by the
//!   inverse of its noise power, the optimal linear combination: a plain
//!   average is worse than the better sensor alone once one is ~3x noisier;
//!   this never is.
//! * the aligned sensors' DISAGREEMENT is noise alone — motion cancels out of
//!   it — so it measures, in the output's own units, how much noise the fused
//!   rate carries right now. That sizes a soft gate (`combiner_motion_gate`):
//!   fused rates indistinguishable from that noise are removed in quadrature,
//!   real motion passes nearly whole. A deadzone that measures itself, so it
//!   tracks warm-up, a noisier half, or a pad on a vibrating desk without
//!   anyone recalibrating.

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
///
/// `gate` scales the noise gate (0 = off, 1 = default); CORRELATE only.
pub(crate) fn resolve(
    correlate: bool,
    gate: f32,
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
    corr.fuse(top, reference, ports, ids, gate, dt)
}

/// Noise power `kappa` times the measured floor that the gate removes in
/// quadrature, at gate 1. The floor being the fused rate's own expected noise
/// power, 3x leaves a resting pad's output at zero ~97% of the time (chi-square,
/// 3 degrees of freedom) while a turn at 3x the noise amplitude keeps ~84% of
/// its speed.
const GATE_KAPPA: f32 = 3.0;
/// High-pass corner that strips the slow motion out of a sensor's reading
/// before it is correlated with the disagreement.
const HP_TAU: f32 = 0.03;
/// Memory of the per-sensor noise correlations.
const CROSS_TAU: f32 = 3.0;
/// "Calm": the fused rate within twice the noise amplitude. The correlations
/// only learn then — two pads' reports arrive up to a report interval apart,
/// and while turning that skew puts motion into the disagreement, correlated
/// with the turn itself. At rest and in fine aim it is negligible, and those
/// are exactly where the noise matters.
const CALM_K: f32 = 4.0;
/// Calm seconds of correlation before the weights leave "equal".
const MIN_CROSS_SECS: f32 = 0.5;
/// The minimum-statistics floor's low bias, undone while it stands in for the
/// calm mean: the minimum of a power averaged over ~16 reports sits ~1/3 under
/// its mean.
const FLOOR_BIAS: f32 = 1.5;
/// Smoothing of the disagreement power before its floor follows it.
const POWER_TAU: f32 = 0.25;
/// How fast a floor may RISE (per second, relative): slowly, so a burst of
/// timing skew while turning can't drag it up; it falls at once to any calmer
/// reading.
const FLOOR_RISE_PER_SEC: f32 = 0.15;

/// The minimum-statistics floor of a power signal: the noise level, read off
/// the calm stretches between bursts of real motion.
#[derive(Clone, Copy, Default, Debug)]
struct NoiseFloor {
    power: f32,
    floor: f32,
    primed: bool,
}

impl NoiseFloor {
    fn push_power(&mut self, p: f32, dt: f32) {
        if !self.primed {
            *self = NoiseFloor { power: p, floor: p, primed: true };
            return;
        }
        self.power += (p - self.power) * (1.0 - (-dt / POWER_TAU).exp());
        self.floor = if self.power < self.floor {
            self.power
        } else {
            (self.floor * (1.0 + FLOOR_RISE_PER_SEC * dt)).min(self.power)
        };
    }
}

/// What a short low-pass leaves of a vector.
#[derive(Clone, Copy, Default, Debug)]
struct HighPass {
    lp: Vec3,
    primed: bool,
}

impl HighPass {
    fn push(&mut self, v: Vec3, dt: f32) -> Vec3 {
        if !self.primed {
            *self = HighPass { lp: v, primed: true };
        }
        self.lp += (v - self.lp) * (1.0 - (-dt / HP_TAU).exp());
        v - self.lp
    }
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

/// Per-node state for `CORRELATE`: one alignment per lower port, and the top
/// port's own noise floor.
#[derive(Default)]
pub struct MotionCorrelator {
    ports: HashMap<usize, Alignment>,
    top: Option<(usize, String)>,
    top_hp: HighPass,
}

impl MotionCorrelator {
    fn fuse(
        &mut self,
        top: usize,
        reference: PortMotion,
        ports: &[Option<PortMotion>],
        ids: &[String],
        gate: f32,
        dt: f32,
    ) -> Option<PortMotion> {
        let id = |i: usize| ids.get(i).cloned().unwrap_or_default();
        // A new top device is a different reference: every alignment was
        // learned against the old one, so all of it starts over.
        let top_key = (top, id(top));
        if self.top.as_ref() != Some(&top_key) {
            self.top = Some(top_key);
            self.top_hp = HighPass::default();
            self.ports.clear();
        }
        // A port whose device changed, or that stopped carrying motion, starts over.
        self.ports.retain(|&i, a| {
            i != top && ports.get(i).is_some_and(|p| p.is_some()) && a.device == id(i)
        });
        let r_hp = reference.gyro.map(|r| self.top_hp.push(r, dt));

        // Every locked port's aligned reading, with its noise tracked.
        let mut fused: Vec<(Vec3, Option<Vec3>, usize)> = Vec::new();
        for (i, port) in ports.iter().enumerate() {
            let Some(port) = port else { continue };
            if i == top {
                continue;
            }
            let a = self.ports.entry(i).or_insert_with(|| Alignment::new(id(i)));
            if let (Some(r), Some(r_hp), Some(b)) = (reference.gyro, r_hp, port.gyro) {
                a.observe(r, b, dt);
                let bg = a.rot * b;
                let b_hp = a.hp.push(bg, dt);
                let e = r - bg;
                a.disagreement.push_power(e.length_squared(), dt);
                // Only once aligned — before that, `e` is mostly misalignment.
                let m = (r + bg) * 0.5;
                if a.locked && m.length_squared() < CALM_K * a.disagreement.floor {
                    let k = 1.0 - (-dt / CROSS_TAU).exp();
                    a.cross_top += (r_hp.dot(e) - a.cross_top) * k;
                    a.cross_self += ((-b_hp).dot(e) - a.cross_self) * k;
                    a.calm_power += (e.length_squared() - a.calm_power) * k;
                    a.cross_secs += dt;
                }
            }
            a.update_lock(i, top);
            if a.locked {
                if let Some(b) = port.gyro {
                    fused.push((a.rot * b, port.accel.map(|x| a.rot * x), i));
                }
            }
        }
        let (Some(rg), false) = (reference.gyro, fused.is_empty()) else {
            return Some(reference);
        };

        // Absolute noise powers. The correlations give each sensor's noise only
        // up to a common factor (what the high-pass lets through), but their
        // RATIO holds; each disagreement supplies the scale — it is the top's
        // noise power plus that port's, exactly. Unmeasured yet: equal.
        let eps = 1e-12;
        let rho: Vec<f32> = fused
            .iter()
            .map(|&(_, _, i)| {
                let a = &self.ports[&i];
                if a.cross_secs >= MIN_CROSS_SECS && a.cross_top > eps && a.cross_self > eps {
                    (a.cross_self / a.cross_top).clamp(0.02, 50.0)
                } else {
                    1.0
                }
            })
            .collect();
        let sigma_top = fused
            .iter()
            .zip(&rho)
            .map(|(&(_, _, i), r)| {
                let a = &self.ports[&i];
                let d = if a.cross_secs >= MIN_CROSS_SECS {
                    // The average starts from zero: divide out the weight it
                    // has gathered so far, or it reads low for seconds.
                    a.calm_power / (1.0 - (-a.cross_secs / CROSS_TAU).exp())
                } else {
                    a.disagreement.floor * FLOOR_BIAS
                };
                d / (1.0 + r)
            })
            .sum::<f32>()
            / fused.len() as f32;
        let sigmas: Vec<f32> = std::iter::once(sigma_top).chain(rho.iter().map(|r| r * sigma_top)).collect();
        // Inverse-variance weights; equal while nothing has been measured (both
        // floors at zero — a deadzoned pair at rest, or warm-up).
        let weights: Vec<f32> = if sigma_top > eps {
            sigmas.iter().map(|s| 1.0 / s.max(eps)).collect()
        } else {
            vec![1.0; sigmas.len()]
        };
        let wsum: f32 = weights.iter().sum();
        let mut gyro = rg * weights[0];
        for (k, &(g, _, _)) in fused.iter().enumerate() {
            gyro += g * weights[k + 1];
        }
        gyro /= wsum;
        // The fused rate's expected noise power, for the gate.
        let fused_noise: f32 =
            weights.iter().zip(&sigmas).map(|(w, s)| w * w * s).sum::<f32>() / (wsum * wsum);
        if gate > 0.0 && fused_noise > eps {
            let m2 = gyro.length_squared();
            let keep = if m2 > eps {
                (1.0 - GATE_KAPPA * gate * fused_noise / m2).max(0.0).sqrt()
            } else {
                0.0
            };
            gyro *= keep;
        }

        // Accelerometers average plainly: their noise is a different sensor's,
        // and the gate is about rotation.
        let accel = reference.accel.map(|ra| {
            let (sum, n) = fused.iter().fold((ra, 1.0f32), |(s, n), &(_, a, _)| match a {
                Some(a) => (s + a, n + 1.0),
                None => (s, n),
            });
            sum / n
        });
        Some(PortMotion {
            gyro: Some(gyro),
            accel,
            // An absolute pose can't be averaged component-wise; the top port's
            // stands for the assembly.
            orientation: reference.orientation,
        })
    }
}

/// One lower port's learned mounting rotation into the top port's frame.
struct Alignment {
    device: String,
    /// High-pass of its aligned reading, for the noise correlation.
    hp: HighPass,
    /// Power of its aligned disagreement with the top port: both noises, no motion.
    disagreement: NoiseFloor,
    /// `E[r_hp . e]` and `E[-b_hp . e]`: the top's and this sensor's noise
    /// power, up to a common factor. See the module doc.
    cross_top: f32,
    cross_self: f32,
    cross_secs: f32,
    /// Mean disagreement power over the calm stretches: the scale, unbiased.
    /// (The floor is the minimum of a fluctuating estimate, so it sits in the
    /// low tail — about a third under — and only stands in until this exists.)
    calm_power: f32,
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
            hp: HighPass::default(),
            disagreement: NoiseFloor::default(),
            cross_top: 0.0,
            cross_self: 0.0,
            cross_secs: 0.0,
            calm_power: 0.0,
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
        let m = resolve(false, 1.0, &ports, &ids, &mut st, 0.001).unwrap();
        assert_eq!(m.gyro, Some(Vec3::ZERO), "the resting top port wins, not the moving one");
        assert!(resolve(false, 1.0, &[None, None], &ids, &mut st, 0.001).is_none());
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
            out = resolve(true, 1.0, &[port(w), port(b)], &ids, &mut st, dt);
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
            let fused = resolve(true, 1.0, &[port(w), port(mount.inverse() * w)], &ids, &mut st, 0.002)
                .unwrap().gyro.unwrap();
            assert_eq!(fused, w, "unlocked: the top port alone");
        }
        assert!(!st.unwrap().ports[&1].locked, "one axis can't fix a 3D alignment");
    }

    /// Deterministic unit-variance noise (sum of four uniforms, xorshift).
    struct Noise(u32);
    impl Noise {
        fn next(&mut self) -> f32 {
            let mut u = || {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 17;
                self.0 ^= self.0 << 5;
                self.0 as f32 / u32::MAX as f32
            };
            (u() + u() + u() + u() - 2.0) * 3f32.sqrt()
        }
        fn vec(&mut self, sigma: f32) -> Vec3 {
            Vec3::new(self.next(), self.next(), self.next()) * sigma
        }
    }

    /// Two rigidly joined IMUs with independent noise, sampled at a pad's 66 Hz
    /// and read by the engine every millisecond (samples held in between, as on
    /// the bus), through `phases` of (seconds, turning?). Returns (fused, top
    /// alone, truth) for every tick of the LAST phase.
    fn noisy_pair(sigma: (f32, f32), gate: f32, phases: &[(f32, bool)]) -> Vec<(Vec3, Vec3, Vec3)> {
        let ids = vec!["top".to_string(), "other".to_string()];
        let (mut st, mut na, mut nb) = (None, Noise(0x1234_5678), Noise(0x9abc_def1));
        let dt = 0.001;
        let (mut a, mut b, mut truth) = (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
        let mut out = Vec::new();
        let total: f32 = phases.iter().map(|p| p.0).sum();
        let last_from = total - phases.last().unwrap().0;
        let turning_at = |t: f32| {
            let mut end = 0.0;
            phases.iter().find(|p| { end += p.0; t < end }).is_some_and(|p| p.1)
        };
        let steps = (total / dt) as usize;
        for k in 0..steps {
            let t = k as f32 * dt;
            if k % 15 == 0 {
                truth = if turning_at(t) {
                    Vec3::new((t * 1.3).sin(), (t * 0.7).cos(), (t * 2.1).sin() * 0.6) * 0.2
                } else {
                    Vec3::ZERO
                };
                a = truth + na.vec(sigma.0);
                b = truth + nb.vec(sigma.1);
            }
            let fused = resolve(true, gate, &[port(a), port(b)], &ids, &mut st, dt).unwrap().gyro.unwrap();
            if t >= last_from {
                out.push((fused, a, truth));
            }
        }
        out
    }

    fn rms(v: impl Iterator<Item = f32>) -> f32 {
        let (s, n) = v.fold((0.0, 0.0), |(s, n), x| (s + x * x, n + 1.0));
        (s / n).sqrt()
    }

    /// Equal noise: fusing two sensors cuts the error by ~1/√2 (gate off,
    /// so this measures the weighting alone).
    #[test]
    fn fusing_two_equal_imus_cuts_the_noise() {
        let run = noisy_pair((0.003, 0.003), 0.0, &[(5.0, true), (3.0, false), (5.0, true)]);
        let fused = rms(run.iter().map(|(f, _, t)| (*f - *t).length()));
        let top = rms(run.iter().map(|(_, a, t)| (*a - *t).length()));
        eprintln!("[equal noise] fused/top rms error = {:.3}", fused / top);
        assert!(fused < top * 0.8, "fused {fused} vs top alone {top}");
    }

    /// A 3× noisier second sensor: an equal average would be ~1.6× WORSE than
    /// the top alone; inverse-variance weighting is never worse.
    #[test]
    fn a_noisier_imu_never_makes_the_fusion_worse() {
        let run = noisy_pair((0.002, 0.006), 0.0, &[(5.0, true), (3.0, false), (5.0, true)]);
        let fused = rms(run.iter().map(|(f, _, t)| (*f - *t).length()));
        let top = rms(run.iter().map(|(_, a, t)| (*a - *t).length()));
        eprintln!("[3x noisier second] fused/top rms error = {:.3}", fused / top);
        assert!(fused <= top, "fused {fused} vs top alone {top}");
    }

    /// At rest the gate removes what can't be told from noise; in motion it
    /// keeps nearly all of the turn.
    #[test]
    fn the_gate_silences_rest_and_keeps_motion() {
        let resting = noisy_pair((0.003, 0.003), 1.0, &[(5.0, true), (4.0, false)]);
        let turning = noisy_pair((0.003, 0.003), 1.0, &[(5.0, true), (3.0, false), (5.0, true)]);
        let zero = resting.iter().filter(|(f, _, _)| *f == Vec3::ZERO).count() as f32 / resting.len() as f32;
        eprintln!("[gate] resting output silent {:.1}%", zero * 100.0);
        assert!(zero > 0.8, "only {zero:.2} of resting output is silent");
        // The last phase's first ticks still hold the rest's zero sample.
        let turning: Vec<_> = turning.iter().filter(|(_, _, t)| *t != Vec3::ZERO).collect();
        let kept = turning.iter().map(|(f, _, t)| f.length() / t.length()).sum::<f32>() / turning.len() as f32;
        eprintln!("[gate] turning speed kept {:.1}%", kept * 100.0);
        assert!(kept > 0.95, "the gate ate motion: kept {kept:.3}");
    }

    #[test]
    fn uncorrelated_imus_never_lock() {
        let ids = vec!["top".to_string(), "other".to_string()];
        let mut st = None;
        for k in 0..5000 {
            let t = k as f32 * 0.002;
            let a = Vec3::new((t * 1.3).sin(), (t * 0.7).cos(), (t * 2.1).sin()) * 0.2;
            let b = Vec3::new((t * 3.7).cos(), (t * 0.3).sin(), (t * 5.1).cos()) * 0.2;
            resolve(true, 1.0, &[port(a), port(b)], &ids, &mut st, 0.002);
        }
        assert!(!st.unwrap().ports[&1].locked);
    }
}
