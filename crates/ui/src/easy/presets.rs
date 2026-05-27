//! Preset (`.fxsp` sub-patch) discovery for Easy mode.
//!
//! See PLAN §8. Scans the factory folder `app/assets/sub-patches/` and
//! the optional user-selected folder; produces a flat index used by
//! the center-panel chip strip.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetSource {
    Factory,
    User,
}

#[derive(Clone, Debug)]
pub struct PresetInfo {
    pub display_name: String,
    pub path: PathBuf,
    pub source: PresetSource,
    /// Optional sibling `<name>.png` for the chip thumbnail.
    pub icon_path: Option<PathBuf>,
    /// Canonical-JSON SHA-style hash for dirty detection. 0 if unreadable.
    pub content_hash: u64,
}

/// Best-effort scan of factory + user preset folders. Returns the
/// union, deduped by display_name (user folder wins on collision so
/// user customizations override shipped defaults).
pub fn load_index(user_folder: Option<&Path>) -> Vec<PresetInfo> {
    let mut out: Vec<PresetInfo> = Vec::new();
    if let Some(p) = factory_dir() {
        scan_into(&mut out, &p, PresetSource::Factory);
    }
    if let Some(p) = user_folder {
        scan_into(&mut out, p, PresetSource::User);
    }
    out
}

/// Locate the factory presets dir relative to the running binary so
/// the path works from both `cargo run` and a packaged build. We look
/// upward from the exe for an `app/assets/sub-patches/` folder.
fn factory_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..6 {
        let candidate = dir.join("app").join("assets").join("sub-patches");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn scan_into(out: &mut Vec<PresetInfo>, dir: &Path, source: PresetSource) {
    let Ok(read) = std::fs::read_dir(dir) else { return; };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("fxsp") {
            continue;
        }
        let display_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let icon_path = {
            let png = path.with_extension("png");
            if png.exists() { Some(png) } else { None }
        };
        let content_hash = std::fs::read(&path).map(|b| hash_bytes(&b)).unwrap_or(0);
        out.push(PresetInfo { display_name, path, source, icon_path, content_hash });
    }
}

/// Stable, dependency-free 64-bit hash (FNV-1a). Sufficient for
/// dirty detection — we only need "did this byte stream change."
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
