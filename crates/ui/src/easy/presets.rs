use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/generated_presets.rs"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetSource {
    Factory,
    User,
}

#[derive(Clone, Debug)]
pub struct PresetInfo {
    pub display_name: String,
    /// Filesystem path. For embedded factory presets this is a synthetic
    /// sentinel path (`factory://<name>`) — never read from disk.
    pub path: PathBuf,
    pub source: PresetSource,
    /// Present for embedded factory presets; `None` for user presets (read
    /// from disk via `path` at load time).
    pub embedded_bytes: Option<&'static [u8]>,
    /// Optional sibling `<name>.png` for the chip thumbnail.
    /// RESERVED: probed and populated at index time, but chip-thumbnail
    /// rendering hasn't been implemented yet — no reader exists.
    #[allow(dead_code)]
    pub icon_path: Option<PathBuf>,
    /// FNV-1a hash of the preset bytes. Used for dirty detection.
    pub content_hash: u64,
}

/// Build the full preset index: embedded factory presets first, then any
/// user presets from `user_folder`. User entries with the same display name
/// shadow factory ones.
pub fn load_index(user_folder: Option<&Path>) -> Vec<PresetInfo> {
    let mut out: Vec<PresetInfo> = Vec::new();

    for &(name, bytes) in FACTORY_PRESETS {
        let path = PathBuf::from(format!("factory://{name}"));
        let content_hash = hash_bytes(bytes);
        out.push(PresetInfo {
            display_name: name.to_string(),
            path,
            source: PresetSource::Factory,
            embedded_bytes: Some(bytes),
            icon_path: None,
            content_hash,
        });
    }

    if let Some(p) = user_folder {
        scan_into(&mut out, p);
    }

    out
}

fn scan_into(out: &mut Vec<PresetInfo>, dir: &Path) {
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
        // User preset shadows factory preset with same name.
        out.retain(|p| p.display_name != display_name);
        let icon_path = {
            let png = path.with_extension("png");
            if png.exists() { Some(png) } else { None }
        };
        let content_hash = std::fs::read(&path).map(|b| hash_bytes(&b)).unwrap_or(0);
        out.push(PresetInfo {
            display_name,
            path,
            source: PresetSource::User,
            embedded_bytes: None,
            icon_path,
            content_hash,
        });
    }
}

/// FNV-1a 64-bit hash — stable, dependency-free, sufficient for dirty detection.
pub(super) fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
