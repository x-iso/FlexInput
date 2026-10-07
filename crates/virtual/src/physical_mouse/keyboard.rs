//! Which keys the user is holding on a PHYSICAL keyboard, for a mapping card's
//! Learn on its output stage.
//!
//! egui only hears keys while a FlexInput window has keyboard focus, which the
//! config overlay never takes (it floats over the game unfocused) — so Learn
//! there couldn't capture a key at all. Raw Input with INPUTSINK hears them
//! whatever has focus, and, like the mouse listener in the parent module, says
//! which DEVICE each key came from: our HIDMaestro virtual keyboard (same
//! VID/PID family as the virtual mouse) and our own SendInput are left out, so
//! a key a mapping is injecting can't be learned back as the output.
//!
//! This takes the KEYBOARD raw-input registration from winit, which uses it
//! only for `DeviceEvent::Key` — nothing in FlexInput or egui reads that; egui's
//! keys come from the window messages, which Raw Input leaves alone. It is only
//! taken the first time something asks, i.e. the first Learn that listens for
//! keys.

use super::*;
use std::sync::Mutex;

/// A key reported down by Raw Input whose release we never heard (the secure
/// desktop, a lock screen) would stay held for good. Past this age, a held key
/// also has to be down by `GetAsyncKeyState` to count.
const STALE_AFTER: Duration = Duration::from_secs(1);

static KB_STARTED: AtomicBool = AtomicBool::new(false);
/// The listener couldn't start or register; callers fall back to egui.
static KB_FAILED: AtomicBool = AtomicBool::new(false);
/// Held keys: pin id, the virtual-key code `GetAsyncKeyState` knows it by, and
/// when it last went down (or auto-repeated).
static HELD: Mutex<Vec<(&'static str, u16, Instant)>> = Mutex::new(Vec::new());

/// The pins of the physical keys held right now, or `None` when there is no
/// listener and the caller should fall back to egui. Starts the listener the
/// first time it's asked; a key already down then is unknown until it is
/// pressed again, so the first frames of the first Learn read empty.
pub fn physical_keys_held() -> Option<Vec<&'static str>> {
    ensure_kb_listening();
    if KB_FAILED.load(Ordering::Relaxed) {
        return None;
    }
    let now = Instant::now();
    let mut held = HELD.lock().ok()?;
    held.retain(|&(_, vk, since)| {
        now.duration_since(since) < STALE_AFTER || unsafe { GetAsyncKeyState(vk as i32) } < 0
    });
    Some(held.iter().map(|&(pin, _, _)| pin).collect())
}

fn ensure_kb_listening() {
    if KB_STARTED.swap(true, Ordering::Relaxed) { return; }
    let spawned = std::thread::Builder::new()
        .name("physical-keyboard".into())
        .spawn(listen_keyboard);
    if let Err(e) = spawned {
        eprintln!("[physical-keyboard] couldn't start the listener: {e}");
        KB_FAILED.store(true, Ordering::Relaxed);
    }
}

fn press(pin: &'static str, vk: u16) {
    let Ok(mut held) = HELD.lock() else { return };
    if !held.iter().any(|&(p, _, _)| p == pin) {
        held.push((pin, vk, Instant::now()));
    } else {
        // Auto-repeat: the key is still down, so it isn't stale.
        for h in held.iter_mut().filter(|h| h.0 == pin) {
            h.2 = Instant::now();
        }
    }
}

fn release(pin: &'static str) {
    if let Ok(mut held) = HELD.lock() {
        held.retain(|&(p, _, _)| p != pin);
    }
}

// ── key names ────────────────────────────────────────────────────────────────

const RI_KEY_BREAK: u16 = 0x01;
const RI_KEY_E0: u16 = 0x02;

const LETTERS: [&str; 26] = [
    "key_a", "key_b", "key_c", "key_d", "key_e", "key_f", "key_g", "key_h", "key_i",
    "key_j", "key_k", "key_l", "key_m", "key_n", "key_o", "key_p", "key_q", "key_r",
    "key_s", "key_t", "key_u", "key_v", "key_w", "key_x", "key_y", "key_z",
];
/// The digit row, spelled the way egui names it (`Num7`) — the spelling Learn
/// has always captured and the picker's keys carry.
const DIGITS: [&str; 10] = [
    "key_num0", "key_num1", "key_num2", "key_num3", "key_num4",
    "key_num5", "key_num6", "key_num7", "key_num8", "key_num9",
];
const FKEYS: [&str; 20] = [
    "key_f1", "key_f2", "key_f3", "key_f4", "key_f5", "key_f6", "key_f7", "key_f8",
    "key_f9", "key_f10", "key_f11", "key_f12", "key_f13", "key_f14", "key_f15",
    "key_f16", "key_f17", "key_f18", "key_f19", "key_f20",
];

/// The pin a raw key event names, and the virtual-key code its held state can
/// be checked by. `None` for a key the virtual keyboard can't send back (the
/// numpad, media keys) and for the fake Shifts Windows wraps round some
/// navigation keys. Modifiers come out SIDED, as the picker offers them.
fn key_pin(vkey: u16, make: u16, flags: u16) -> Option<(&'static str, u16)> {
    let e0 = flags & RI_KEY_E0 != 0;
    Some(match vkey {
        // A Shift with E0 is one Windows made up around Insert/Home/arrows.
        0x10 if e0 => return None,
        0x10 if make == 0x36 => ("key_rshift", 0xA1),
        0x10 => ("key_lshift", 0xA0),
        0x11 if e0 => ("key_rctrl", 0xA3),
        0x11 => ("key_lctrl", 0xA2),
        0x12 if e0 => ("key_ralt", 0xA5),
        0x12 => ("key_lalt", 0xA4),
        0x5B => ("key_lwin", 0x5B),
        0x5C => ("key_rwin", 0x5C),
        0x41..=0x5A => (LETTERS[(vkey - 0x41) as usize], vkey),
        0x30..=0x39 => (DIGITS[(vkey - 0x30) as usize], vkey),
        0x70..=0x83 => (FKEYS[(vkey - 0x70) as usize], vkey),
        0x08 => ("key_backspace", vkey),
        0x09 => ("key_tab", vkey),
        0x0D => ("key_enter", vkey),
        0x13 => ("key_pause", vkey),
        0x14 => ("key_capslock", vkey),
        0x1B => ("key_escape", vkey),
        0x20 => ("key_space", vkey),
        0x21 => ("key_pageup", vkey),
        0x22 => ("key_pagedown", vkey),
        0x23 => ("key_end", vkey),
        0x24 => ("key_home", vkey),
        0x25 => ("key_arrowleft", vkey),
        0x26 => ("key_arrowup", vkey),
        0x27 => ("key_arrowright", vkey),
        0x28 => ("key_arrowdown", vkey),
        0x2C => ("key_printscreen", vkey),
        0x2D => ("key_insert", vkey),
        0x2E => ("key_delete", vkey),
        0xBA => ("key_semicolon", vkey),
        0xBB => ("key_equals", vkey),
        0xBC => ("key_comma", vkey),
        0xBD => ("key_minus", vkey),
        0xBE => ("key_period", vkey),
        0xBF => ("key_slash", vkey),
        0xC0 => ("key_backtick", vkey),
        0xDB => ("key_openbracket", vkey),
        0xDC => ("key_backslash", vkey),
        0xDD => ("key_closebracket", vkey),
        0xDE => ("key_quote", vkey),
        _ => return None,
    })
}

// ── Win32 ────────────────────────────────────────────────────────────────────

/// RAWKEYBOARD.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawKeyboard {
    make_code: u16,
    flags: u16,
    _reserved: u16,
    vkey: u16,
    message: u32,
    extra_info: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputKeyboard {
    header: RawInputHeader,
    keyboard: RawKeyboard,
}

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(vkey: i32) -> i16;
}

const RIM_TYPEKEYBOARD: u32 = 1;

fn listen_keyboard() {
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("FlexInputPhysicalKeyboard");
        let class = WndClassExW {
            cb_size: std::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            wnd_proc,
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
            icon_sm: 0,
        };
        RegisterClassExW(&class);
        let hwnd = CreateWindowExW(0, class_name.as_ptr(), class_name.as_ptr(), 0,
            0, 0, 0, 0, HWND_MESSAGE, 0, instance, std::ptr::null());
        if hwnd == 0 {
            eprintln!("[physical-keyboard] couldn't create the listener window");
            KB_FAILED.store(true, Ordering::Relaxed);
            return;
        }
        let device = RawInputDevice {
            usage_page: 0x01,
            usage: 0x06,
            flags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            target: hwnd,
        };
        if RegisterRawInputDevices(&device, 1, std::mem::size_of::<RawInputDevice>() as u32) == 0 {
            eprintln!("[physical-keyboard] couldn't register for raw keyboard input");
            KB_FAILED.store(true, Ordering::Relaxed);
            return;
        }

        // hDevice → "is our virtual keyboard", looked up once per device.
        let mut ours: HashMap<isize, bool> = HashMap::new();
        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            match msg.message {
                WM_INPUT => {
                    let mut raw: RawInputKeyboard = std::mem::zeroed();
                    let mut size = std::mem::size_of::<RawInputKeyboard>() as u32;
                    let got = GetRawInputData(msg.lparam, RID_INPUT,
                        (&mut raw as *mut RawInputKeyboard).cast(), &mut size,
                        std::mem::size_of::<RawInputHeader>() as u32);
                    if got != u32::MAX && raw.header.kind == RIM_TYPEKEYBOARD {
                        let k = raw.keyboard;
                        let device = raw.header.device;
                        let is_ours = if device == 0 {
                            is_our_injection(k.extra_info)
                        } else {
                            *ours.entry(device).or_insert_with(|| {
                                device_vid_pid(device).is_some_and(|(v, p)| is_our_virtual_mouse(v, p))
                            })
                        };
                        if !is_ours {
                            if let Some((pin, vk)) = key_pin(k.vkey, k.make_code, k.flags) {
                                if k.flags & RI_KEY_BREAK != 0 {
                                    release(pin);
                                } else {
                                    press(pin, vk);
                                }
                            }
                        }
                    }
                }
                WM_INPUT_DEVICE_CHANGE if msg.wparam == GIDC_REMOVAL => {
                    ours.remove(&msg.lparam);
                }
                _ => {}
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        KB_FAILED.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The virtual keyboard is in the virtual mouse's VID/PID family, so the
    /// one check leaves both out.
    #[test]
    fn our_virtual_keyboard_is_recognised() {
        assert!(is_our_virtual_mouse(0x1209, 0xF1E0), "keyboard.json's PID");
    }

    #[test]
    fn keys_are_named_the_way_learn_and_the_picker_name_them() {
        assert_eq!(key_pin(0x41, 0x1E, 0), Some(("key_a", 0x41)));
        assert_eq!(key_pin(0x37, 0x08, 0).map(|k| k.0), Some("key_num7"));
        assert_eq!(key_pin(0x7B, 0x58, 0).map(|k| k.0), Some("key_f12"));
        assert_eq!(key_pin(0x26, 0x48, RI_KEY_E0).map(|k| k.0), Some("key_arrowup"));
        // Modifiers come out sided, by make code or E0.
        assert_eq!(key_pin(0x10, 0x2A, 0).map(|k| k.0), Some("key_lshift"));
        assert_eq!(key_pin(0x10, 0x36, 0).map(|k| k.0), Some("key_rshift"));
        assert_eq!(key_pin(0x11, 0x1D, 0).map(|k| k.0), Some("key_lctrl"));
        assert_eq!(key_pin(0x11, 0x1D, RI_KEY_E0).map(|k| k.0), Some("key_rctrl"));
        assert_eq!(key_pin(0x12, 0x38, RI_KEY_E0).map(|k| k.0), Some("key_ralt"));
        // The Shift Windows invents round the navigation keys is no key.
        assert_eq!(key_pin(0x10, 0x2A, RI_KEY_E0), None);
        // Nor is anything the virtual keyboard couldn't send back.
        assert_eq!(key_pin(0x60, 0x52, 0), None, "numpad 0");
    }

    #[test]
    fn win32_layouts() {
        assert_eq!(std::mem::size_of::<RawKeyboard>(), 16);
        assert_eq!(std::mem::size_of::<RawInputKeyboard>(), 40);
    }
}
