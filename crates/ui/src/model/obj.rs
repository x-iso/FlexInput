/// Minimal Wavefront OBJ loader for FlexInput controller models.
/// Parses `.obj` vertex/normal/face data + `info.txt` assembly metadata
/// into a ready-to-render 3D controller model.

use glam::{Vec3, Quat};
use std::path::Path;

// ── Public types ──────────────────────────────────────────────────────────────

/// A single mesh with vertices packed as interleaved (pos + normal):
/// `[x0,y0,z0,nx0,ny0,nz0, x1,y1,z1,nx1,ny1,nz1, ...]`
#[derive(Debug, Clone)]
pub struct Mesh {
    pub vertices: Vec<f32>,
    /// Triangle count = `vertices.len() / 6`.
    pub tri_count: usize,
}

/// A loaded controller part with its mesh and the static transform from info.txt.
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub mesh: Mesh,
    /// Position offset in model space [x, y, z].
    pub pos: Vec3,
    /// Rotation as quaternion (from axis-angle or single-axis angle stored in info.txt).
    pub rot: Quat,
    /// Optional touch-area half-extents (from the touch width / depth values
    /// in info.txt): used by `touch_point*` parts as the touchpad's
    /// `(half_x, half_z)` so normalized touch input can be mapped onto the
    /// model. `None` for parts whose values are zero.
    pub extent: Option<glam::Vec2>,
    /// Tilt of that touch area — the plane the finger dot slides in. Identity
    /// for a flat pad and for every block in the original 16-value layout.
    pub touch_rot: Quat,
}

/// A complete controller model assembled from parts with their transforms.
#[derive(Debug, Clone)]
pub struct ControllerModel {
    /// Parts in draw order (shells first, then smaller components on top).
    pub parts: Vec<Part>,
    /// Display name for UI labels (e.g. `"DualSense"`).
    pub display_name: String,
}

// ── OBJ parsing ───────────────────────────────────────────────────────────────

/// Parse a single `.obj` file into a `Mesh`. Returns error on malformed data.
pub fn parse_obj(text: &str) -> Result<Mesh, ObjError> {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();

    // First pass: collect all `v` and `vn` lines.
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#')
            || trimmed.starts_with("o ") || trimmed.starts_with("mtllib")
            || trimmed.starts_with("usemtl") || trimmed.starts_with("s ")
        {
            continue;
        }
        match trimmed.split_once(char::is_whitespace) {
            Some(("v", rest)) => {
                let vals: Vec<f32> = rest.split_whitespace()
                    .take(3).filter_map(|s| s.parse().ok()).collect();
                if vals.len() == 3 {
                    positions.push(Vec3::new(vals[0], vals[1], vals[2]));
                }
            }
            Some(("vn", rest)) => {
                let vals: Vec<f32> = rest.split_whitespace()
                    .take(3).filter_map(|s| s.parse().ok()).collect();
                if vals.len() == 3 {
                    normals.push(Vec3::new(vals[0], vals[1], vals[2]));
                }
            }
            _ => {} // ignore vt, f (done in second pass), etc.
        }
    }

    let position_count = positions.len();
    let normal_count = normals.len();

    if position_count == 0 {
        return Err(ObjError::NoVertices);
    }

    // Second pass: collect faces and triangulate them. Each corner keeps its
    // OWN position AND normal index — in OBJ these can differ (`f v/vt/vn`),
    // and reusing the position index for the normal panicked whenever a mesh
    // had a different number of positions vs. normals. Indices are 1-based and
    // may be negative (relative to the end); every index is bounds-checked and
    // an out-of-range corner is dropped rather than crashing the whole load.
    let resolve = |raw: i64, count: usize| -> Option<usize> {
        if raw > 0 {
            let i = (raw - 1) as usize;
            (i < count).then_some(i)
        } else if raw < 0 {
            let i = count as i64 + raw;
            (i >= 0 && (i as usize) < count).then_some(i as usize)
        } else {
            None
        }
    };
    // `v/vt/vn` → (position index, optional normal index), 0-based.
    let parse_ref = |s: &str| -> Option<(usize, Option<usize>)> {
        let mut it = s.split('/');
        let p = resolve(it.next()?.parse::<i64>().ok()?, position_count)?;
        let _vt = it.next(); // texcoord — ignored
        let n = it
            .next()
            .filter(|x| !x.is_empty())
            .and_then(|x| x.parse::<i64>().ok())
            .and_then(|r| resolve(r, normal_count));
        Some((p, n))
    };

    // (position index, normal index) per triangle corner.
    let mut triangles: Vec<[(usize, Option<usize>); 3]> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("f ") { continue; }

        let verts: Vec<(usize, Option<usize>)> =
            trimmed[1..].split_whitespace().filter_map(parse_ref).collect();
        // Fan-triangulate any polygon with ≥ 3 corners (tri, quad, or n-gon).
        for k in 1..verts.len().saturating_sub(1) {
            triangles.push([verts[0], verts[k], verts[k + 1]]);
        }
    }

    if triangles.is_empty() {
        return Err(ObjError::NoFaces);
    }

    // Build interleaved vertex array: (pos.xyz, normal.xyz) per corner.
    let mut vertices: Vec<f32> = Vec::with_capacity(triangles.len() * 3 * 6);
    for tri in &triangles {
        for &(p, n) in tri {
            let pos = positions[p]; // bounds-checked by `resolve`
            let nrm = n.map(|ni| normals[ni]).unwrap_or(Vec3::Z);
            vertices.extend_from_slice(&[pos.x, pos.y, pos.z, nrm.x, nrm.y, nrm.z]);
        }
    }

    Ok(Mesh {
        vertices,
        tri_count: triangles.len(),
    })
}

// ── info.txt parsing ──────────────────────────────────────────────────────────

/// Parse `info.txt` for a controller model folder. Returns the ordered list of
/// parts with their transforms.
///
/// Each part is its filename line (`left_trigger.obj`) followed by one number
/// per line, in either of two layouts: the original 3d-controller-overlay's 16
/// values, or the 19 of ceski's fork (the same 16 plus the touch area's tilt).
/// A block runs until the next line that isn't a number, so both parse.
///
/// What is read, by value index after the filename (0-based):
///   0-2: `[pos_x, pos_y, pos_z]` — position offset in model space
///   3-5: axis-angle rotation vector `[ax, ay, az]` (mostly zeros)
///   10: rotation angle in radians
///     • If the axis is non-zero: rotate around that axis by this angle
///     • If the axis is all zeros but angle ≠ 0: single-axis X rotation
///   14-15: touch area width and depth — non-zero only on `touch_point*`
///     parts; the finger dot ranges over half of each either side of its
///     rest point.
///   16-18: touch area tilt in radians, about X, then Y, then Z (the fork's
///     layout only; the Steam Controller's angled pads set it).
/// Values 6-9 and 11-13 are unused (upstream's pop-up offsets and angles,
/// trigger and stick tilt limits): this viewer animates parts its own way.
pub fn parse_info_txt(text: &str) -> Vec<PartTransform> {
    let mut parts = Vec::new();
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty()).peekable();

    while let Some(line) = lines.next() {
        // A number outside any block has no part to belong to.
        if line.parse::<f32>().is_ok() {
            continue;
        }
        // Start of a part block: filename on this line.
        let name = Path::new(line).file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| line.trim_end_matches(".obj").to_string());

        // Its values: every numeric line up to the next filename.
        let mut vals: Vec<f32> = Vec::with_capacity(19);
        while let Some(v) = lines.peek().and_then(|l| l.parse::<f32>().ok()) {
            vals.push(v);
            lines.next();
        }

        // Interpret the transform data.
        let (pos, rot) = match vals.len() {
            n if n >= 11 => {
                // Full block: pos[3] + axis_angle[3] + pad + scale[2] + angle
                let pos = Vec3::new(vals[0], vals[1], vals[2]);

                let ax = vals[3];
                let ay = vals[4];
                let az = vals[5];
                let axis_raw = Vec3::new(ax, ay, az);
                let angle = vals[10]; // line 11 (0-indexed)

                let rot = if angle.abs() > 1e-6 {
                    if axis_raw.length_squared() > 1e-8 {
                        // Standard axis-angle rotation.
                        Quat::from_axis_angle(axis_raw.normalize(), angle)
                    } else {
                        // Axis is all zeros → single-axis X rotation (sticks, caps).
                        Quat::from_rotation_x(angle)
                    }
                } else {
                    Quat::IDENTITY
                };

                (pos, rot)
            }
            n if n >= 3 => {
                // Minimal block: just position.
                let pos = Vec3::new(vals[0], vals[1], vals[2]);
                (pos, Quat::IDENTITY)
            }
            _ => (Vec3::ZERO, Quat::IDENTITY),
        };

        // Touch area size (values 14-15); non-zero only for touch_point
        // parts. Both upstreams store the FULL width and depth — the DualSense
        // block's 0.55 sits inside its 0.81-wide pad mesh, and its overlay
        // offsets the dot by `width * (x - 0.5)` — so halve them.
        let extent = if vals.len() >= 16 && (vals[14].abs() > 1e-6 || vals[15].abs() > 1e-6) {
            Some(glam::Vec2::new(vals[14], vals[15]) * 0.5)
        } else {
            None
        };

        // Touch area tilt (values 16-18), composed in the order the fork's
        // renderer applies it: X, then Y, then Z.
        let touch_rot = if vals.len() >= 19 {
            Quat::from_rotation_x(vals[16])
                * Quat::from_rotation_y(vals[17])
                * Quat::from_rotation_z(vals[18])
        } else {
            Quat::IDENTITY
        };

        parts.push(PartTransform { name, pos, rot, extent, touch_rot });
    }

    parts
}

#[derive(Debug, Clone)]
pub struct PartTransform {
    pub name: String,
    pub pos: Vec3,
    pub rot: Quat,
    pub extent: Option<glam::Vec2>,
    pub touch_rot: Quat,
}

// ── Part names ────────────────────────────────────────────────────────────────

/// Part names in ceski's fork of 3d-controller-overlay, which renamed the
/// buttons after SDL's gamepad layout, mapped onto the names this viewer
/// animates and colours by. Applied only to a model that uses the fork's names
/// (see [`SDL_ONLY_PART_NAMES`]), because two of them mean something else in
/// the original layout: there `paddle1`-`paddle4` count by grip-rail position
/// (the Joy-Con SL/SR), while the fork numbers them in SDL's button order, and
/// the fork's `misc2` is a two-pad controller's right trackpad.
const SDL_PART_NAMES: &[(&str, &str)] = &[
    ("south_button", "a_button"),
    ("east_button", "b_button"),
    ("west_button", "x_button"),
    ("north_button", "y_button"),
    ("left_shoulder", "left_bumper"),
    ("right_shoulder", "right_bumper"),
    ("left_stick_base", "left_stick"),
    ("right_stick_base", "right_stick"),
    ("left_stick_ring", "left_ring"),
    ("right_stick_ring", "right_ring"),
    ("left_stick_cap", "left_cap"),
    ("right_stick_cap", "right_cap"),
    // SDL order: right paddle 1, left paddle 1, right paddle 2, left paddle 2.
    ("paddle1", "paddle_r1"),
    ("paddle2", "paddle_l1"),
    ("paddle3", "paddle_r2"),
    ("paddle4", "paddle_l2"),
    // The right trackpad (its click is SDL's Misc 2).
    ("misc2", "touchpad2"),
];

/// Names only the fork's layout has; a model with any of them uses it.
const SDL_ONLY_PART_NAMES: &[&str] = &[
    "south_button", "east_button", "west_button", "north_button",
    "left_shoulder", "right_shoulder", "left_stick_base", "right_stick_base",
];

// ── Model assembly ────────────────────────────────────────────────────────────

/// Load a complete controller model through a file-reading closure — the same
/// assembly path serves on-disk folders AND the compiled-in (embedded) copies.
/// `read_file("info.txt")` / `read_file("left_stick.obj")` return the file's
/// text or `None` when absent. Parts come out under this viewer's names
/// whichever layout the folder uses (see [`SDL_PART_NAMES`]).
pub fn load_controller_model_with(
    read_file: &dyn Fn(&str) -> Option<String>,
    display_name: String,
) -> Result<ControllerModel, ObjError> {
    let info_text = read_file("info.txt").ok_or(ObjError::InfoFileMissing)?;
    let parts_transforms = parse_info_txt(&info_text);
    let sdl_names = parts_transforms
        .iter()
        .any(|pt| SDL_ONLY_PART_NAMES.contains(&pt.name.as_str()));

    let mut assembled_parts: Vec<Part> = Vec::new();
    for pt in &parts_transforms {
        // Some parts listed in info.txt may not have corresponding .obj files.
        let Some(obj_text) = read_file(&format!("{}.obj", pt.name)) else {
            continue;
        };
        let mesh = parse_obj(&obj_text)?;
        let name = SDL_PART_NAMES
            .iter()
            .find(|(sdl, _)| sdl_names && *sdl == pt.name)
            .map_or_else(|| pt.name.clone(), |(_, ours)| ours.to_string());
        assembled_parts.push(Part {
            name,
            mesh,
            pos: pt.pos,
            rot: pt.rot,
            extent: pt.extent,
            touch_rot: pt.touch_rot,
        });
    }

    Ok(ControllerModel {
        parts: assembled_parts,
        display_name,
    })
}

/// Load a complete controller model from an assets directory.
/// `base_path` points to the folder containing `.obj` files and `info.txt`.
pub fn load_controller_model(base_path: &Path) -> Result<ControllerModel, ObjError> {
    let display_name = base_path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let read = |f: &str| std::fs::read_to_string(base_path.join(f)).ok();
    load_controller_model_with(&read, display_name)
}

// ── Transform helpers ─────────────────────────────────────────────────────────

/// Build a model-space transform matrix from position + rotation.
pub fn part_transform(pos: Vec3, rot: Quat) -> glam::Mat4 {
    // Scale is 1.0 for all current models (reserved for future use).
    let scale = glam::Mat4::from_scale(Vec3::ONE);
    let translation = glam::Mat4::from_translation(pos);
    let rotation = glam::Mat4::from_quat(rot);

    // Apply in order: rotate, then translate.
    translation * rotation * scale
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ObjError {
    #[error("no vertices in OBJ file")]
    NoVertices,
    #[error("no faces in OBJ file")]
    NoFaces,
    #[error("invalid face with {} vertices (expected 3 or 4)", _0)]
    InvalidFace(usize),
    #[error("invalid vertex index in face")]
    InvalidFaceIndex,
    #[error("info.txt not found")]
    InfoFileMissing,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_uses_separate_normal_index() {
        // 3 positions but only 1 normal; every corner references vn 1. The old
        // parser indexed `normals` with the POSITION index and panicked once a
        // mesh had more positions than normals (the real-model crash).
        let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1\n";
        let m = parse_obj(obj).expect("parses");
        assert_eq!(m.tri_count, 1);
        assert_eq!(m.vertices.len(), 3 * 6);
        // Each corner's normal is (0,0,1) — floats [3..6] of the first corner.
        assert_eq!(&m.vertices[3..6], &[0.0, 0.0, 1.0]);
    }

    #[test]
    fn quad_fans_into_two_triangles() {
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
        let m = parse_obj(obj).expect("parses");
        assert_eq!(m.tri_count, 2);
    }

    #[test]
    fn ngon_fans_and_bad_index_is_skipped() {
        // A pentagon (5 corners → 3 tris) plus a face with an out-of-range
        // index that must not panic (its bad corner drops out).
        let obj = "v 0 0 0\nv 1 0 0\nv 2 1 0\nv 1 2 0\nv 0 2 0\nf 1 2 3 4 5\nf 1 2 99\n";
        let m = parse_obj(obj).expect("parses without panic");
        // Pentagon = 3 tris; the second face loses its 99 corner so it has only
        // 2 valid corners and contributes nothing.
        assert_eq!(m.tri_count, 3);
    }

    /// One `info.txt` block in the original 16-value layout, then one in the
    /// fork's 19 with a tilt. Reading a fixed 16 used to take the fork's three
    /// extra values for the next part's filename.
    #[test]
    fn info_txt_reads_both_block_layouts() {
        let legacy = "left_stick.obj\n-0.3\n0.2\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0.35\n0\n0\n";
        let fork = "touch_point1.obj\n-0.4\n0.2\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0.4\n0.2\n0.3\n0\n0\n";
        let parts = parse_info_txt(&format!("{legacy}{fork}top_shell.obj\n0\n0\n0\n"));
        let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["left_stick", "touch_point1", "top_shell"]);

        assert_eq!(parts[0].pos, Vec3::new(-0.3, 0.2, 0.0));
        assert_eq!(parts[0].extent, None);
        assert_eq!(parts[0].touch_rot, Quat::IDENTITY);

        let tp = &parts[1];
        assert_eq!(tp.pos, Vec3::new(-0.4, 0.2, 0.0));
        // Width and depth are stored whole; the dot ranges over half of each.
        assert_eq!(tp.extent, Some(glam::Vec2::new(0.2, 0.1)));
        let up = tp.touch_rot * Vec3::Y;
        assert!((up - Vec3::new(0.0, 0.3f32.cos(), 0.3f32.sin())).length() < 1e-5, "{up}");
    }

    /// A model in the fork's SDL naming loads under this viewer's part names,
    /// paddles by SDL order; one in the original naming keeps its own.
    #[test]
    fn sdl_part_names_load_as_ours() {
        let tri = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let load = |info: &'static str| {
            let read = move |f: &str| (f == "info.txt").then(|| info.to_string())
                .or_else(|| f.ends_with(".obj").then(|| tri.to_string()));
            let m = load_controller_model_with(&read, "t".into()).expect("loads");
            m.parts.into_iter().map(|p| p.name).collect::<Vec<_>>()
        };
        assert_eq!(
            load("south_button.obj\n0\nleft_stick_cap.obj\n0\npaddle1.obj\n0\npaddle2.obj\n0\nmisc2.obj\n0\nmisc1.obj\n0\n"),
            ["a_button", "left_cap", "paddle_r1", "paddle_l1", "touchpad2", "misc1"],
        );
        assert_eq!(load("a_button.obj\n0\npaddle1.obj\n0\n"), ["a_button", "paddle1"]);
    }
}
