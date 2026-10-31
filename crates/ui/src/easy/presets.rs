use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

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

/// How long the preset index stands before the folder is looked at again. The
/// panel asks for it every frame.
const RESCAN_EVERY: Duration = Duration::from_secs(2);

/// A preset file as last read, to know it needn't be read again.
type KnownFiles = HashMap<PathBuf, (Option<SystemTime>, u64, u64)>;

struct IndexCache {
    folder: Option<PathBuf>,
    built: Instant,
    index: Arc<Vec<PresetInfo>>,
    files: KnownFiles,
}

static INDEX_CACHE: Mutex<Option<IndexCache>> = Mutex::new(None);

fn factory_presets() -> &'static [PresetInfo] {
    static FACTORY: OnceLock<Vec<PresetInfo>> = OnceLock::new();
    FACTORY.get_or_init(|| FACTORY_PRESETS.iter().map(|&(name, bytes)| PresetInfo {
        display_name: name.to_string(),
        path: PathBuf::from(format!("factory://{name}")),
        source: PresetSource::Factory,
        embedded_bytes: Some(bytes),
        icon_path: None,
        content_hash: hash_bytes(bytes),
    }).collect())
}

/// The full preset index: embedded factory presets first, then any user presets
/// from `user_folder`. User entries with the same display name shadow factory
/// ones. Cached: a new or edited file shows up within [`RESCAN_EVERY`], or at once
/// after [`invalidate_index`], and only a changed file is read.
pub fn load_index(user_folder: Option<&Path>) -> Arc<Vec<PresetInfo>> {
    let mut cache = INDEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.as_ref() {
        if c.folder.as_deref() == user_folder && c.built.elapsed() < RESCAN_EVERY {
            return Arc::clone(&c.index);
        }
    }
    let known = cache.take().map(|c| c.files).unwrap_or_default();
    let mut files = KnownFiles::new();
    let mut out = factory_presets().to_vec();
    if let Some(p) = user_folder {
        scan_into(&mut out, p, &known, &mut files);
    }
    let index = Arc::new(out);
    *cache = Some(IndexCache {
        folder: user_folder.map(Path::to_path_buf),
        built: Instant::now(),
        index: Arc::clone(&index),
        files,
    });
    index
}

/// Look at the folder again next time: a preset was just saved.
pub fn invalidate_index() {
    if let Some(c) = INDEX_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        c.built = Instant::now() - RESCAN_EVERY;
    }
}

fn scan_into(out: &mut Vec<PresetInfo>, dir: &Path, known: &KnownFiles, files: &mut KnownFiles) {
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
        let meta = entry.metadata().ok();
        let stamp = (meta.as_ref().and_then(|m| m.modified().ok()), meta.as_ref().map_or(0, |m| m.len()));
        let content_hash = match known.get(&path) {
            Some(&(modified, len, hash)) if (modified, len) == stamp => hash,
            _ => std::fs::read(&path).map(|b| hash_bytes(&b)).unwrap_or(0),
        };
        files.insert(path.clone(), (stamp.0, stamp.1, content_hash));
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

#[cfg(test)]
mod tests {
    use super::*;

    // One test: they share the process's cache.
    #[test]
    fn the_index_is_cached_and_sees_changes_when_told() {
        let dir = std::env::temp_dir().join(format!("fxi_presets_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mine.fxsp");
        std::fs::write(&file, b"one").unwrap();

        let first = load_index(Some(&dir));
        let mine = |i: &[PresetInfo]| i.iter().find(|p| p.display_name == "mine").map(|p| p.content_hash);
        assert_eq!(mine(&first), Some(hash_bytes(b"one")));
        assert_eq!(first.iter().filter(|p| p.source == PresetSource::Factory).count(), FACTORY_PRESETS.len());

        // Within the window it's the same list, whatever happened on disk.
        std::fs::write(&file, b"two!").unwrap();
        assert!(Arc::ptr_eq(&first, &load_index(Some(&dir))));

        // Told to look: the changed file is read again.
        invalidate_index();
        assert_eq!(mine(&load_index(Some(&dir))), Some(hash_bytes(b"two!")));

        // Another folder is another index, at once.
        assert_eq!(mine(&load_index(None)), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
