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
/// Format per part block (16 lines after filename):
///   Line 1: filename (`left_trigger.obj`)
///   Lines 2-4: `[pos_x, pos_y, pos_z]` — position offset in model space
///   Lines 5-7: axis-angle rotation vector `[ax, ay, az]` (mostly zeros)
///   Line 8: padding zero
///   Lines 9-10: scale factors (always 0 → use default 1.0)
///   Line 11: rotation angle in radians
///     • If axis (lines 5-7) is non-zero: rotate around that axis by this angle
///     • If axis is all zeros but angle ≠ 0: single-axis X rotation
///   Lines 12-16: padding zeros
pub fn parse_info_txt(text: &str) -> Vec<PartTransform> {
    let mut parts = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() {
            i += 1;
            continue;
        }
        // Start of a part block: filename on this line.
        let name = Path::new(line).file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| line.trim_end_matches(".obj").to_string());

        // Collect the 16 lines of transform data (skip empty lines between blocks).
        let mut vals: Vec<f32> = Vec::with_capacity(16);
        i += 1;
        while i < lines.len() && vals.len() < 16 {
            if let Ok(v) = lines[i].trim().parse::<f32>() {
                vals.push(v);
            }
            // Empty line after we've started collecting → end of this block.
            if lines[i].trim().is_empty() && !vals.is_empty() {
                i += 1;
                break;
            }
            i += 1;
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

        parts.push(PartTransform { name, pos, rot });
    }

    parts
}

#[derive(Debug, Clone)]
pub struct PartTransform {
    pub name: String,
    pub pos: Vec3,
    pub rot: Quat,
}

// ── Model assembly ────────────────────────────────────────────────────────────

/// Load a complete controller model from an assets directory.
/// `base_path` points to the folder containing `.obj` files and `info.txt`.
pub fn load_controller_model(base_path: &Path) -> Result<ControllerModel, ObjError> {
    let info_path = base_path.join("info.txt");
    if !info_path.exists() {
        return Err(ObjError::InfoFileMissing);
    }

    let info_text = std::fs::read_to_string(&info_path).map_err(|e| ObjError::IO(e))?;
    let parts_transforms = parse_info_txt(&info_text);

    // Determine display name from the folder name.
    let display_name = base_path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut assembled_parts: Vec<Part> = Vec::new();

    for pt in &parts_transforms {
        let obj_path = base_path.join(format!("{}.obj", pt.name));
        if !obj_path.exists() {
            // Some parts listed in info.txt may not have corresponding .obj files.
            continue;
        }

        let obj_text = std::fs::read_to_string(&obj_path).map_err(|e| ObjError::IO(e))?;
        let mesh = parse_obj(&obj_text)?;

        assembled_parts.push(Part {
            name: pt.name.clone(),
            mesh,
            pos: pt.pos,
            rot: pt.rot,
        });
    }

    Ok(ControllerModel {
        parts: assembled_parts,
        display_name,
    })
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
}
