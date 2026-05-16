//! Global panic-mode hotkey listener (Windows). Uses RegisterHotKey instead
//! of a low-level keyboard hook — simpler, reliable, and doesn't require
//! pumping a message queue for hook delivery to work. The tradeoff is that
//! the chord is not "eaten" — it still reaches whatever window has focus —
//! but for a toggle that's acceptable (and we filter the same chord out of
//! the Remapper's Learn mode so it can't be captured as a binding).
//!
//! Communication with the UI:
//!  - `shortcut`: the desired chord. Updated by the UI when the user changes
//!    the binding. A short pump-loop tick polls this and re-registers if it
//!    has changed.
//!  - `toggle_requested`: set true by the listener when the chord fires.
//!    The UI consumes it each frame and flips `panic_active`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::app::PanicShortcut;

/// Path to the JSON file where the configured shortcut is persisted.
fn settings_path() -> Option<std::path::PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let mut p = std::path::PathBuf::from(appdata);
    p.push("FlexInput");
    let _ = std::fs::create_dir_all(&p);
    p.push("panic_shortcut.json");
    Some(p)
}

pub fn load_panic_shortcut() -> Option<PanicShortcut> {
    let p = settings_path()?;
    let bytes = std::fs::read(&p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_panic_shortcut(s: &PanicShortcut) {
    let Some(p) = settings_path() else { return; };
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        let _ = std::fs::write(&p, json);
    }
}

/// Spawns the platform-specific global hotkey listener. On non-Windows
/// targets this is currently a no-op (the panic button in the UI still
/// works manually).
pub fn spawn_panic_hotkey_listener(
    shortcut: Arc<RwLock<PanicShortcut>>,
    toggle_requested: Arc<AtomicBool>,
) {
    #[cfg(windows)]
    windows_impl::spawn(shortcut, toggle_requested);
    #[cfg(not(windows))]
    {
        let _ = shortcut;
        let _ = toggle_requested;
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey,
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PeekMessageW, MSG, PM_REMOVE, WM_HOTKEY,
    };

    const HOTKEY_ID: i32 = 0xF1E1;

    pub fn spawn(
        shortcut: Arc<RwLock<PanicShortcut>>,
        toggle_requested: Arc<AtomicBool>,
    ) {
        std::thread::Builder::new()
            .name("panic-hotkey".into())
            .spawn(move || unsafe {
                // The thread must own the hotkey registration so WM_HOTKEY
                // is queued onto our queue (we pass hwnd=NULL).
                let mut current: Option<PanicShortcut> = None;
                let mut registered = false;

                // Try to install the initial shortcut.
                if let Ok(sc) = shortcut.read() {
                    let snapshot = sc.clone();
                    drop(sc);
                    registered = register(&snapshot);
                    current = Some(snapshot);
                }
                if let Some(c) = current.as_ref() {
                    eprintln!(
                        "[panic-hotkey] initial registration {} for {}",
                        if registered { "OK" } else { "FAILED" },
                        c.label()
                    );
                }

                let mut msg: MSG = std::mem::zeroed();
                let mut last_check = std::time::Instant::now();
                loop {
                    // Drain any messages — WM_HOTKEY is what we care about.
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        if msg.message == WM_HOTKEY && msg.wParam as i32 == HOTKEY_ID {
                            let label = current
                                .as_ref()
                                .map(|c| c.label())
                                .unwrap_or_else(|| "<unset>".into());
                            eprintln!("[panic-hotkey] chord fired: {}", label);
                            toggle_requested.store(true, Ordering::Relaxed);
                        }
                    }

                    // Every 250ms, check if the desired shortcut has changed.
                    if last_check.elapsed() >= std::time::Duration::from_millis(250) {
                        last_check = std::time::Instant::now();
                        if let Ok(sc) = shortcut.read() {
                            let want = sc.clone();
                            drop(sc);
                            let need_reregister = match &current {
                                Some(c) => c != &want,
                                None => true,
                            };
                            if need_reregister {
                                if registered {
                                    UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
                                }
                                registered = register(&want);
                                eprintln!(
                                    "[panic-hotkey] re-registered {} for {}",
                                    if registered { "OK" } else { "FAILED" },
                                    want.label()
                                );
                                current = Some(want);
                            }
                        }
                    }

                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
            })
            .expect("failed to spawn panic-hotkey thread");
    }

    /// Try to register the chord with Windows. Returns true on success.
    unsafe fn register(sc: &PanicShortcut) -> bool {
        let Some(ref name) = sc.key else { return false; };
        let Some(vk) = egui_name_to_vk(name) else { return false; };
        let mut mods: u32 = MOD_NOREPEAT;
        if sc.ctrl  { mods |= MOD_CONTROL; }
        if sc.shift { mods |= MOD_SHIFT; }
        if sc.alt   { mods |= MOD_ALT; }
        if sc.win   { mods |= MOD_WIN; }
        RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, mods, vk) != 0
    }

    /// Convert egui Debug key name to a Windows virtual-key code.
    fn egui_name_to_vk(name: &str) -> Option<u32> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        Some(match name {
            "Escape" => VK_ESCAPE as u32,
            "Tab"    => VK_TAB as u32,
            "Enter"  => VK_RETURN as u32,
            "Space"  => VK_SPACE as u32,
            "Backspace" => VK_BACK as u32,
            "Delete" => VK_DELETE as u32,
            "Insert" => VK_INSERT as u32,
            "Home"   => VK_HOME as u32,
            "End"    => VK_END as u32,
            "PageUp"   => VK_PRIOR as u32,
            "PageDown" => VK_NEXT as u32,
            "ArrowUp"    => VK_UP as u32,
            "ArrowDown"  => VK_DOWN as u32,
            "ArrowLeft"  => VK_LEFT as u32,
            "ArrowRight" => VK_RIGHT as u32,
            "Backtick"        => VK_OEM_3 as u32,
            "Minus"           => VK_OEM_MINUS as u32,
            "Plus" | "Equals" => VK_OEM_PLUS as u32,
            "Comma"           => VK_OEM_COMMA as u32,
            "Period"          => VK_OEM_PERIOD as u32,
            "Slash"           => VK_OEM_2 as u32,
            "Backslash"       => VK_OEM_5 as u32,
            "Semicolon"       => VK_OEM_1 as u32,
            "Quote"           => VK_OEM_7 as u32,
            "OpenBracket"     => VK_OEM_4 as u32,
            "CloseBracket"    => VK_OEM_6 as u32,
            "F1" => VK_F1 as u32, "F2" => VK_F2 as u32, "F3" => VK_F3 as u32, "F4" => VK_F4 as u32,
            "F5" => VK_F5 as u32, "F6" => VK_F6 as u32, "F7" => VK_F7 as u32, "F8" => VK_F8 as u32,
            "F9" => VK_F9 as u32, "F10" => VK_F10 as u32, "F11" => VK_F11 as u32, "F12" => VK_F12 as u32,
            n if n.len() == 1 => {
                let c = n.chars().next()?;
                if c.is_ascii_alphabetic() { c.to_ascii_uppercase() as u32 }
                else if c.is_ascii_digit() { c as u32 }
                else { return None; }
            }
            n if n.starts_with("Num") && n.len() == 4 => {
                let c = n.chars().nth(3)?;
                if c.is_ascii_digit() { c as u32 } else { return None; }
            }
            _ => return None,
        })
    }
}
