//! Where FlexInput keeps its per-user files: settings, workspace, crash log
//! and the `logs/` folder, all under one directory.
//!
//! Windows: `%APPDATA%\FlexInput`. Elsewhere: `$XDG_CONFIG_HOME/FlexInput`,
//! falling back to `~/.config/FlexInput`. One folder on every platform, so a
//! user backing up or attaching files to a bug report has one place to look.
//!
//! `crates/joycon2/src/dlog.rs` repeats [`log_dir`] because that crate has no
//! core dependency by design. Change both together.

use std::path::PathBuf;

/// The per-user FlexInput directory, created if missing. `None` only when the
/// platform's base directory can't be determined.
pub fn app_data_dir() -> Option<PathBuf> {
    let dir = base_dir()?.join("FlexInput");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// `logs/` under [`app_data_dir`], or `None` if it can't be created.
pub fn log_dir() -> Option<PathBuf> {
    let dir = base_dir()?.join("FlexInput").join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(windows)]
fn base_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(not(windows))]
fn base_dir() -> Option<PathBuf> {
    // The XDG spec says a relative XDG_CONFIG_HOME is invalid and is ignored.
    if let Some(p) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        if p.is_absolute() {
            return Some(p);
        }
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config"))
}
