//! The desktop entry a Linux desktop finds FlexInput's icon through.
//!
//! Wayland has no window icons: the compositor shows the icon of the desktop
//! entry named after the window's app id ([`crate::APP_ID`]), and a window with none
//! gets a generic one (Plasma's "W"). FlexInput is one binary with no
//! installer, so it writes its own entry and icon into the user's data folder
//! (`~/.local/share`) on every start, with `Exec` pointing at the binary that
//! ran. That also lists it in the application menu.

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

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
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
         StartupWMClass={APP_ID}\n",
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
}
