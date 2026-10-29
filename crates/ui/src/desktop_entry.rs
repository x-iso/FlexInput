//! The desktop entry a Linux desktop finds FlexInput's icon through.
//!
//! Wayland has no window icons: the compositor shows the icon of the desktop
//! entry named after the window's app id ([`crate::APP_ID`]), and a window with none
//! gets a generic one (Plasma's "W"). FlexInput is one binary with no
//! installer, so it writes its own entry and icon into the user's data folder
//! (`~/.local/share`) on every start, with `Exec` pointing at the binary that
//! ran. That also lists it in the application menu.
//!
//! KWin also reads the entry whose `Exec` is the calling binary to decide
//! whether it may use KWin's screenshot interface, which the RWS calibration
//! reference frame does (`capture.rs`); the entry asks for it. KWin looks
//! entries up in KDE's application cache, which is rebuilt only when a
//! folder's modification time moves, so a changed entry is renamed into
//! place rather than rewritten (`write_if_changed`).

use std::io;
use std::path::{Path, PathBuf};

use crate::APP_ID;

/// Write the entry and icon where they're missing or out of date. Before the
/// first window opens, so the desktop has them when it maps it.
pub fn install() {
    if let Err(e) = try_install() {
        eprintln!("[desktop-entry] not installed: {e}");
    }
}

fn try_install() -> io::Result<()> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| io::Error::other("no HOME"))?;
    let exe = std::env::current_exe()?;
    let entry = entry(&exe).ok_or_else(|| io::Error::other("the binary's path has a line break"))?;
    write_if_changed(
        &data.join("icons/hicolor/256x256/apps").join(format!("{APP_ID}.png")),
        crate::app::APP_ICON_PNG,
    )?;
    write_if_changed(&data.join("applications").join(format!("{APP_ID}.desktop")), entry.as_bytes())
}

/// Put `bytes` at `path` unless it already holds them, through a sibling temp
/// file renamed over it, so the folder's modification time moves and KDE's
/// application cache notices (a file rewritten in place goes unnoticed). A
/// file newer than its folder was rewritten in place, as earlier builds did,
/// and is renamed into place again for the same reason.
fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent folder"))?;
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let newer_than_dir = matches!((modified(path), modified(dir)), (Some(f), Some(d)) if f > d);
    if !newer_than_dir && std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?;
    let temp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

fn entry(exe: &Path) -> Option<String> {
    Some(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=FlexInput\n\
         Comment=Remap and process game controllers\n\
         Exec={}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Game;Utility;\n\
         StartupWMClass={APP_ID}\n\
         X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2\n",
        exec_arg(exe)?
    ))
}

/// `exe` as the `Exec` key's program: quoted, with the characters the spec
/// reserves inside quotes backslash-escaped and `%` doubled (field codes), then
/// every backslash doubled again for the key's own string escaping. `None` for
/// a path with a line break, which no key can hold.
fn exec_arg(exe: &Path) -> Option<String> {
    let path = exe.to_string_lossy();
    if path.contains(['\n', '\r']) {
        return None;
    }
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    Some(quoted.replace('\\', "\\\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quotes_and_escapes_the_path() {
        assert_eq!(exec_arg(Path::new("/home/me/Games/flexinput")).unwrap(), "\"/home/me/Games/flexinput\"");
        assert_eq!(exec_arg(Path::new("/home/me/100% $ure/fi")).unwrap(), "\"/home/me/100%% \\\\$ure/fi\"");
        assert_eq!(exec_arg(Path::new("/a\\b")).unwrap(), "\"/a\\\\\\\\b\"");
        assert!(exec_arg(Path::new("/a\nb")).is_none());
        let e = entry(Path::new("/opt/fi")).unwrap();
        assert!(e.contains("Exec=\"/opt/fi\"\nIcon=flexinput\n"), "{e}");
    }

    #[test]
    fn writes_by_rename_and_redoes_an_in_place_rewrite() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!("fi-desktop-entry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("a.desktop");
        let modified = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
        let set_modified = |t: SystemTime| {
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(t).unwrap();
        };

        write_if_changed(&path, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "the temp file is gone");

        // Same bytes, older than the folder: left alone.
        let old = SystemTime::now() - Duration::from_secs(3600);
        set_modified(old);
        write_if_changed(&path, b"one").unwrap();
        assert_eq!(modified(&path), old);

        // Same bytes, newer than the folder (rewritten in place): renamed in
        // again, which brings the folder level with it.
        set_modified(SystemTime::now() + Duration::from_secs(3600));
        write_if_changed(&path, b"one").unwrap();
        assert!(modified(&path) <= modified(&dir));
        assert_eq!(std::fs::read(&path).unwrap(), b"one");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
