//! Global toggle-hotkey listener (Windows). Companion to
//! [`crate::panic_hotkey`]: same RegisterHotKey pattern, caller-supplied
//! HOTKEY_ID so multiple chords can be registered simultaneously without
//! colliding (always-on-top pin, info overlay, …).
//!
//! Each chord raises its `toggle_requested` flag; the UI thread polls it
//! once per frame and applies the state flip there (so viewport commands,
//! focus flip-flops, and any visual feedback happen on the main thread).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::settings::PinShortcut;

/// HOTKEY_ID for the always-on-top pin chord. Distinct from
/// `panic_hotkey`'s id and `HOTKEY_ID_OVERLAY` so all registrations coexist.
pub const HOTKEY_ID_PIN: i32 = 0xF1E2;
/// HOTKEY_ID for the info-overlay visibility chord.
pub const HOTKEY_ID_OVERLAY: i32 = 0xF1E3;
/// HOTKEY_ID for the config-overlay visibility chord (M3).
pub const HOTKEY_ID_CONFIG: i32 = 0xF1E4;
/// HOTKEY_ID for the info-overlay EDIT-mode chord.
pub const HOTKEY_ID_OVERLAY_EDIT: i32 = 0xF1E5;

pub fn spawn_pin_hotkey_listener(
    hotkey_id: i32,
    thread_label: &'static str,
    shortcut: Arc<RwLock<PinShortcut>>,
    toggle_requested: Arc<AtomicBool>,
) {
    #[cfg(windows)]
    windows_impl::spawn(hotkey_id, thread_label, shortcut, toggle_requested);
    #[cfg(not(windows))]
    {
        let _ = (hotkey_id, thread_label);
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

    pub fn spawn(
        hotkey_id: i32,
        thread_label: &'static str,
        shortcut: Arc<RwLock<PinShortcut>>,
        toggle_requested: Arc<AtomicBool>,
    ) {
        std::thread::Builder::new()
            .name(thread_label.into())
            .spawn(move || unsafe {
                let mut current: Option<PinShortcut> = None;
                let mut registered = false;

                if let Ok(sc) = shortcut.read() {
                    let snapshot = sc.clone();
                    drop(sc);
                    registered = register(hotkey_id, &snapshot);
                    current = Some(snapshot);
                }
                if let Some(c) = current.as_ref() {
                    eprintln!(
                        "[{thread_label}] initial registration {} for {}",
                        if registered { "OK" } else { "FAILED" },
                        c.label()
                    );
                }

                let mut msg: MSG = std::mem::zeroed();
                let mut last_check = std::time::Instant::now();
                loop {
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        if msg.message == WM_HOTKEY && msg.wParam as i32 == hotkey_id {
                            toggle_requested.store(true, Ordering::Relaxed);
                        }
                    }

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
                                    UnregisterHotKey(std::ptr::null_mut(), hotkey_id);
                                }
                                registered = register(hotkey_id, &want);
                                eprintln!(
                                    "[{thread_label}] re-registered {} for {}",
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
            .expect("failed to spawn hotkey thread");
    }

    unsafe fn register(hotkey_id: i32, sc: &PinShortcut) -> bool {
        let Some(ref name) = sc.key else { return false; };
        let Some(vk) = egui_name_to_vk(name) else { return false; };
        let mut mods: u32 = MOD_NOREPEAT;
        if sc.ctrl  { mods |= MOD_CONTROL; }
        if sc.shift { mods |= MOD_SHIFT; }
        if sc.alt   { mods |= MOD_ALT; }
        if sc.win   { mods |= MOD_WIN; }
        // Empty chord (no modifiers AND a plain letter) would steal that key
        // system-wide — reject it. At least one of: a modifier, a function
        // key, or a non-letter named key.
        let is_letter_or_digit = matches!(name.len(), 1) || (name.starts_with("Num") && name.len() == 4);
        let no_mods = !sc.ctrl && !sc.shift && !sc.alt && !sc.win;
        if no_mods && is_letter_or_digit { return false; }
        RegisterHotKey(std::ptr::null_mut(), hotkey_id, mods, vk) != 0
    }

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
