//! HIDMaestro-backed Virtual Keyboard & Mouse.
//!
//! Same device id / pin contract as the SendInput (`enigo`) implementation in
//! `windows::VirtualKeyMouse`, but the output lands as **real HID input
//! reports** from two root-enumerated HIDMaestro nodes (profiles
//! `hm-keyboard` / `hm-mouse`) instead of `SendInput` injection. Why this
//! exists: user-mode injected input can be silently swallowed system-wide —
//! groundtruthed on a Win11 26H1 ASUS machine where every `SendInput`
//! (keyboard, relative and absolute mouse) was "accepted" by the API and eaten
//! before reaching the desktop, while physical input flowed normally (vendor
//! low-level-hook stack filtering `LLKHF_INJECTED`-style events). A kernel-side
//! HID device is upstream of all of that — and games additionally see a
//! genuine raw-input mouse.
//!
//! Differences from the enigo backend, on purpose:
//! * **No typematic re-send** — the report holds the key down and the HOST
//!   (kbdhid) generates auto-repeat, like a physical keyboard.
//! * **Motion is emitted from `flush()`** (the I/O thread rate, 500 Hz
//!   default) rather than a dedicated 1 kHz thread. Sub-pixel carry works the
//!   same; at 500 Hz the int16 mickey deltas are simply 2× the 1 kHz ones. If
//!   slow-aim smoothness ever needs the finer quantization back, raise the
//!   polling rate or move the mouse writer onto its own thread — the report
//!   builder is already self-contained.
//! * **Braiding IS consulted** (`braid_try_mouse` gates the motion emit in
//!   `flush()`), same as the enigo backend — NOT because HID frames need the
//!   anti-coincidence property (they don't), but because the I/O loop gates
//!   the virtual GAMEPAD's flush on `braid_try_gamepad()` whenever mixed mode
//!   is on and braiding is enabled. The token must keep alternating: an
//!   earlier version of this backend skipped braiding entirely, the token
//!   parked on "mouse" after the first gamepad flush, and the virtual pad's
//!   flush starved FOREVER (SHM SeqNo frozen on the create seed, games saw an
//!   eternal neutral pad) while keyboard/mouse kept working.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use flexinput_core::Signal;

use crate::hidmaestro_device::HidMaestroDevice;
use crate::windows::cursor_pos;
use crate::{layouts, SinkPin, VirtualDevice};

/// Modifier bits of report byte 0 (left-hand variants).
mod kbd_mod {
    pub const CTRL: u8 = 0x01;
    pub const SHIFT: u8 = 0x02;
    pub const ALT: u8 = 0x04;
    pub const WIN: u8 = 0x08;
}

/// Map a learned-key pin name (egui key name, with or without the `key_`
/// prefix — the same universe `windows::egui_key_name_to_enigo` accepts) to a
/// HID Keyboard/Keypad usage (page 0x07). `None` = unmappable, pin ignored
/// (mirrors the enigo backend returning `None`).
fn key_name_to_hid_usage(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("key_").unwrap_or(name);
    let n = rest.to_ascii_lowercase();
    // Single character: letter or top-row digit.
    if n.len() == 1 {
        let c = n.chars().next()?;
        if c.is_ascii_alphabetic() {
            return Some(0x04 + (c as u8 - b'a'));
        }
        if c.is_ascii_digit() {
            // '1'..'9' → 0x1E..0x26, '0' → 0x27.
            return Some(if c == '0' { 0x27 } else { 0x1E + (c as u8 - b'1') });
        }
        return None;
    }
    // egui names its top-row digits Num0..Num9 (it has no numpad variants), so
    // "numN" is the digit row too — matching what the enigo backend sends.
    if let Some(d) = n.strip_prefix("num") {
        if d.len() == 1 {
            let c = d.chars().next()?;
            if c.is_ascii_digit() {
                return Some(if c == '0' { 0x27 } else { 0x1E + (c as u8 - b'1') });
            }
        }
    }
    if let Some(f) = n.strip_prefix('f') {
        if let Ok(num) = f.parse::<u8>() {
            return match num {
                1..=12 => Some(0x3A + (num - 1)),  // F1..F12
                13..=20 => Some(0x68 + (num - 13)), // F13..F20
                _ => None,
            };
        }
    }
    Some(match n.as_str() {
        "enter" | "return" => 0x28,
        "escape" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,
        "minus" => 0x2D,
        "plus" | "equals" => 0x2E,
        "openbracket" => 0x2F,
        "closebracket" => 0x30,
        "backslash" => 0x31,
        "semicolon" => 0x33,
        "quote" => 0x34,
        "backtick" => 0x35,
        "comma" => 0x36,
        "period" => 0x37,
        "slash" => 0x38,
        "capslock" => 0x39,
        "printscreen" => 0x46,
        "pause" => 0x48,
        "insert" => 0x49,
        "home" => 0x4A,
        "pageup" => 0x4B,
        "delete" => 0x4C,
        "end" => 0x4D,
        "pagedown" => 0x4E,
        "arrowright" => 0x4F,
        "arrowleft" => 0x50,
        "arrowdown" => 0x51,
        "arrowup" => 0x52,
        _ => return None,
    })
}

#[derive(Default, Clone, Copy, PartialEq)]
struct MouseButtons {
    lmb: bool,
    rmb: bool,
    mmb: bool,
    mb4: bool,
    mb5: bool,
}

impl MouseButtons {
    fn bits(self) -> u8 {
        (self.lmb as u8)
            | (self.rmb as u8) << 1
            | (self.mmb as u8) << 2
            | (self.mb4 as u8) << 3
            | (self.mb5 as u8) << 4
    }
}

#[derive(Default, Clone, Copy)]
struct KeysHeld {
    escape: bool,
    shift: bool,
    ctrl: bool,
    alt: bool,
    win: bool,
}

/// Ticks (well, wall time) after our own emitted move during which a cursor
/// mismatch is attributed to us rather than a physical mouse — GetCursorPos can
/// lag the actual injection. Same 16 ms the enigo thread used.
const SELF_MOVE_WINDOW: Duration = Duration::from_millis(16);
/// Clamp on the dt applied per flush, so a scheduler gap moves at most this
/// much worth of distance in one report instead of lurching (see the enigo
/// thread's MAX_DT rationale). 4× the default 500 Hz tick — note this equals
/// ONE tick at the 125 Hz minimum polling setting, where the clamp is a no-op.
const MAX_DT: f32 = 0.008;
/// Velocity unit: pixels per 60 Hz reference frame (unchanged pin semantics).
const REF: f32 = 60.0;
/// Analog scroll rate unit: notches per second at |rate| = 1.0. A full-deflection
/// stick / touch-zone maps to a firm-but-controllable continuous scroll.
const SCROLL_REF: f32 = 18.0;

pub struct VirtualKeyMouseHm {
    pub muted: bool,

    // Desired state accumulated by send(), consumed by flush().
    mouse_vel_x: f32,
    mouse_vel_y: f32,
    scroll_delta: i32,   // vertical digital scroll clicks (scroll_up/down)
    hscroll_delta: i32,  // horizontal digital scroll clicks (scroll_left/right)
    scroll_vel_v: f32,   // analog vertical scroll rate (scroll_y), +up
    scroll_vel_h: f32,   // analog horizontal scroll rate (scroll_x), +right
    buttons: MouseButtons,
    keys: KeysHeld,
    learned_keys: HashMap<String, bool>,

    // The two HIDMaestro nodes. Held as plain structs (not dyn) — their
    // gamepad-oriented trait methods are unused; we drive them via
    // write_raw_input. Drop tears both nodes down through the helper;
    // persist_on_drop() forwards for keep-alive.
    kbd: HidMaestroDevice,
    mouse: HidMaestroDevice,

    // Mouse emission state (port of the enigo thread's logic, at flush rate).
    carry_x: f32,
    carry_y: f32,
    scroll_carry_v: f32, // sub-click remainder for analog vertical scroll
    scroll_carry_h: f32, // sub-click remainder for analog horizontal scroll
    last_emit: Instant,
    last_mouse_report: [u8; 7],
    mouse_report_dirty: bool,

    // Physical-mouse suppression (same semantics as the enigo backend).
    last_cursor: Option<(i32, i32)>,
    blocked_until: Option<Instant>,
    self_move_until: Option<Instant>,

    // Keyboard emission state.
    last_kbd_report: [u8; 8],
}

impl VirtualKeyMouseHm {
    /// Create both HIDMaestro nodes via the elevated helper. Returns `None`
    /// when either node comes up without an input section (driver not
    /// installed / helper refused) so the factory can fall back to the
    /// SendInput backend instead of presenting a dead device.
    pub fn create() -> Option<Self> {
        let kbd = HidMaestroDevice::create(
            "virtual.keymouse.kbd",
            "FlexInput Virtual Keyboard",
            flexinput_hidmaestro::profile::presets::KEYBOARD_JSON,
            0,
        )?;
        let mouse = HidMaestroDevice::create(
            "virtual.keymouse.mouse",
            "FlexInput Virtual Mouse",
            flexinput_hidmaestro::profile::presets::MOUSE_JSON,
            0,
        )?;
        if !kbd.is_connected() || !mouse.is_connected() {
            // Dropping tears down whichever node DID come up.
            return None;
        }
        Some(Self {
            muted: false,
            mouse_vel_x: 0.0,
            mouse_vel_y: 0.0,
            scroll_delta: 0,
            hscroll_delta: 0,
            scroll_vel_v: 0.0,
            scroll_vel_h: 0.0,
            buttons: MouseButtons::default(),
            keys: KeysHeld::default(),
            learned_keys: HashMap::new(),
            kbd,
            mouse,
            carry_x: 0.0,
            carry_y: 0.0,
            scroll_carry_v: 0.0,
            scroll_carry_h: 0.0,
            last_emit: Instant::now(),
            last_mouse_report: [0; 7],
            mouse_report_dirty: false,
            last_cursor: None,
            blocked_until: None,
            self_move_until: None,
            last_kbd_report: [0; 8],
        })
    }

    /// Build the 8-byte keyboard report from held modifiers + learned keys.
    fn build_kbd_report(&self) -> [u8; 8] {
        let mut rep = [0u8; 8];
        if !self.muted {
            let mut mods = 0u8;
            if self.keys.shift { mods |= kbd_mod::SHIFT; }
            if self.keys.ctrl  { mods |= kbd_mod::CTRL; }
            if self.keys.alt   { mods |= kbd_mod::ALT; }
            if self.keys.win   { mods |= kbd_mod::WIN; }
            rep[0] = mods;
            let mut slot = 2;
            if self.keys.escape && slot < 8 {
                rep[slot] = 0x29;
                slot += 1;
            }
            // 6KRO: keys beyond the 6 slots are dropped (a real keyboard would
            // report ErrorRollOver; for graph-driven input, dropping the
            // overflow is friendlier than blanking everything).
            for (name, held) in &self.learned_keys {
                if slot >= 8 { break; }
                if !held { continue; }
                if let Some(u) = key_name_to_hid_usage(name) {
                    // The fixed escape pin and a learned "Escape" can coexist.
                    if !rep[2..slot].contains(&u) {
                        rep[slot] = u;
                        slot += 1;
                    }
                }
            }
        }
        rep
    }
}

impl VirtualDevice for VirtualKeyMouseHm {
    fn id(&self) -> &str { "virtual.keymouse" }
    fn display_name(&self) -> &str { "Virtual Keyboard & Mouse" }
    fn sink_pins(&self) -> &'static [SinkPin] { layouts::KEYMOUSE_DEFAULT_PINS }
    fn is_connected(&self) -> bool { self.kbd.is_connected() && self.mouse.is_connected() }

    fn send(&mut self, pin: &str, value: Signal) {
        // Pin semantics identical to windows::VirtualKeyMouse::send.
        match pin {
            "mouse" => { if let Signal::Vec2(v) = value { self.mouse_vel_x += v.x; self.mouse_vel_y += -v.y; } }
            "mouse_x"       => { if let Signal::Float(f) = value { self.mouse_vel_x += f; } }
            "mouse_y"       => { if let Signal::Float(f) = value { self.mouse_vel_y += -f; } }
            "scroll_up"     => { if matches!(value, Signal::Bool(true)) { self.scroll_delta += 1; } }
            "scroll_down"   => { if matches!(value, Signal::Bool(true)) { self.scroll_delta -= 1; } }
            "scroll_right"  => { if matches!(value, Signal::Bool(true)) { self.hscroll_delta += 1; } }
            "scroll_left"   => { if matches!(value, Signal::Bool(true)) { self.hscroll_delta -= 1; } }
            "scroll_y"      => { if let Signal::Float(f) = value { self.scroll_vel_v += f; } }
            "scroll_x"      => { if let Signal::Float(f) = value { self.scroll_vel_h += f; } }
            "mouse_left"    => { if let Signal::Bool(b) = value { self.buttons.lmb = b; } }
            "mouse_right"   => { if let Signal::Bool(b) = value { self.buttons.rmb = b; } }
            "mouse_middle"  => { if let Signal::Bool(b) = value { self.buttons.mmb = b; } }
            "mouse_back"    => { if let Signal::Bool(b) = value { self.buttons.mb4 = b; } }
            "mouse_forward" => { if let Signal::Bool(b) = value { self.buttons.mb5 = b; } }
            "key_escape"    => { if let Signal::Bool(b) = value { self.keys.escape = b; } }
            "key_shift"     => { if let Signal::Bool(b) = value { self.keys.shift  = b; } }
            "key_ctrl"      => { if let Signal::Bool(b) = value { self.keys.ctrl   = b; } }
            "key_alt"       => { if let Signal::Bool(b) = value { self.keys.alt    = b; } }
            "key_win"       => { if let Signal::Bool(b) = value { self.keys.win    = b; } }
            _ => { if let Signal::Bool(b) = value { self.learned_keys.insert(pin.to_string(), b); } }
        }
    }

    fn flush(&mut self) {
        let now = Instant::now();

        // ── Keyboard ─────────────────────────────────────────────────────────
        // Level-triggered: write only when the report actually changes; the
        // host does typematic repeat on its own.
        let kbd_rep = self.build_kbd_report();
        if kbd_rep != self.last_kbd_report {
            self.kbd.write_raw_input(&kbd_rep);
            self.last_kbd_report = kbd_rep;
        }

        // ── Mouse ────────────────────────────────────────────────────────────
        // dt-scaled velocity → sub-pixel carry → int16 mickeys (same math as
        // the enigo thread, at flush cadence).
        let dt = (now - self.last_emit).as_secs_f32().min(MAX_DT);
        self.last_emit = now;

        // Physical-mouse suppression: a cursor move we didn't cause blocks
        // virtual motion for the configured window.
        let (suppression_on, release_ms) = crate::mouse_suppression_effective();
        let cur = cursor_pos();
        let suppressed = if suppression_on {
            if let (Some(pos), Some(last)) = (cur, self.last_cursor) {
                if pos != last && self.self_move_until.map_or(true, |t| now > t) {
                    self.blocked_until = Some(now + Duration::from_millis(release_ms as u64));
                }
            }
            self.last_cursor = cur;
            self.blocked_until.map_or(false, |t| now < t)
        } else {
            self.last_cursor = cur;
            self.blocked_until = None;
            false
        };

        let vel_x = std::mem::take(&mut self.mouse_vel_x);
        let vel_y = std::mem::take(&mut self.mouse_vel_y);
        let scroll = std::mem::take(&mut self.scroll_delta);
        let hscroll = std::mem::take(&mut self.hscroll_delta);
        let scroll_vel_v = std::mem::take(&mut self.scroll_vel_v);
        let scroll_vel_h = std::mem::take(&mut self.scroll_vel_h);

        // Mixed-output braiding: keep the shared turn token alternating (see
        // module docs — the gamepad flush is gated on the other side of this
        // token). Between our turns the carry keeps accumulating, so no motion
        // is lost; buttons/scroll are unaffected, mirroring the enigo backend.
        let emit_move = if crate::MOUSE_MIXED_MODE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
            crate::braid_try_mouse()
        } else {
            true
        };

        let mut rep = [0u8; 7];
        if !self.muted && !suppressed {
            self.carry_x += vel_x * REF * dt;
            self.carry_y += vel_y * REF * dt;
            let (mut dx, mut dy) = (0i32, 0i32);
            if emit_move {
                dx = (self.carry_x.trunc() as i32).clamp(-32767, 32767);
                dy = (self.carry_y.trunc() as i32).clamp(-32767, 32767);
                self.carry_x -= dx as f32;
                self.carry_y -= dy as f32;
            }
            // Analog scroll: dt-scaled rate → sub-click carry → int8 clicks, then
            // fold in the digital clicks. Scroll isn't braid-gated (like the
            // wheel already wasn't), so it emits every flush.
            self.scroll_carry_v += scroll_vel_v * SCROLL_REF * dt;
            self.scroll_carry_h += scroll_vel_h * SCROLL_REF * dt;
            let vclicks = self.scroll_carry_v.trunc() as i32;
            let hclicks = self.scroll_carry_h.trunc() as i32;
            self.scroll_carry_v -= vclicks as f32;
            self.scroll_carry_h -= hclicks as f32;
            rep[0] = self.buttons.bits();
            rep[1..3].copy_from_slice(&(dx as i16).to_le_bytes());
            rep[3..5].copy_from_slice(&(dy as i16).to_le_bytes());
            rep[5] = (scroll + vclicks).clamp(-127, 127) as i8 as u8;   // vertical wheel
            rep[6] = (hscroll + hclicks).clamp(-127, 127) as i8 as u8;  // horizontal (AC Pan)
            if dx != 0 || dy != 0 {
                self.self_move_until = Some(now + SELF_MOVE_WINDOW);
                if let Some(ref mut last) = self.last_cursor {
                    last.0 += dx;
                    last.1 += dy;
                }
            }
        } else {
            // Muted/suppressed: zero deltas, all buttons released, and the
            // banked carry (motion + scroll) is discarded so it can't discharge
            // as a jump later.
            self.carry_x = 0.0;
            self.carry_y = 0.0;
            self.scroll_carry_v = 0.0;
            self.scroll_carry_h = 0.0;
        }

        // Relative device: identical all-zero frames carry no information, so
        // only write when something moved/changed vs the last frame (buttons
        // level, motion, or wheel). A button release IS a change and gets its
        // report.
        let has_motion = rep[1..7] != [0; 6];
        if has_motion || rep != self.last_mouse_report || self.mouse_report_dirty {
            self.mouse.write_raw_input(&rep);
            self.mouse_report_dirty = false;
            self.last_mouse_report = rep;
        }
    }

    fn reset_outputs(&mut self) {
        self.mouse_vel_x = 0.0;
        self.mouse_vel_y = 0.0;
        self.scroll_delta = 0;
        self.hscroll_delta = 0;
        self.scroll_vel_v = 0.0;
        self.scroll_vel_h = 0.0;
        self.buttons = MouseButtons::default();
        self.keys = KeysHeld::default();
        for v in self.learned_keys.values_mut() { *v = false; }
        self.carry_x = 0.0;
        self.carry_y = 0.0;
        self.scroll_carry_v = 0.0;
        self.scroll_carry_h = 0.0;
        // Force a neutral frame out even if the last report was already
        // neutral-equal (e.g. first reset after reconnect).
        self.mouse_report_dirty = true;
        self.flush();
    }

    fn persist_on_drop(&mut self) {
        self.kbd.persist_on_drop();
        self.mouse.persist_on_drop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The learned-key universe must map to the correct HID usages — same
    /// names `windows::egui_key_name_to_enigo` accepts, both bare ("A") and
    /// Remapper-prefixed ("key_a") forms.
    #[test]
    fn key_names_map_to_hid_usages() {
        // Letters (bare + prefixed + case).
        assert_eq!(key_name_to_hid_usage("A"), Some(0x04));
        assert_eq!(key_name_to_hid_usage("key_a"), Some(0x04));
        assert_eq!(key_name_to_hid_usage("z"), Some(0x1D));
        // Top-row digits, both bare and egui's NumN spelling.
        assert_eq!(key_name_to_hid_usage("1"), Some(0x1E));
        assert_eq!(key_name_to_hid_usage("9"), Some(0x26));
        assert_eq!(key_name_to_hid_usage("0"), Some(0x27));
        assert_eq!(key_name_to_hid_usage("num0"), Some(0x27));
        assert_eq!(key_name_to_hid_usage("key_num5"), Some(0x22));
        // F-keys: contiguous F1..F12 block, then the F13.. block.
        assert_eq!(key_name_to_hid_usage("f1"), Some(0x3A));
        assert_eq!(key_name_to_hid_usage("F12"), Some(0x45));
        assert_eq!(key_name_to_hid_usage("f13"), Some(0x68));
        assert_eq!(key_name_to_hid_usage("f20"), Some(0x6F));
        assert_eq!(key_name_to_hid_usage("f21"), None);
        // Named keys.
        assert_eq!(key_name_to_hid_usage("Enter"), Some(0x28));
        assert_eq!(key_name_to_hid_usage("Return"), Some(0x28));
        assert_eq!(key_name_to_hid_usage("Space"), Some(0x2C));
        assert_eq!(key_name_to_hid_usage("key_arrowleft"), Some(0x50));
        assert_eq!(key_name_to_hid_usage("ArrowUp"), Some(0x52));
        assert_eq!(key_name_to_hid_usage("PageDown"), Some(0x4E));
        assert_eq!(key_name_to_hid_usage("PrintScreen"), Some(0x46));
        // Unmappable stays unmapped (pin silently ignored, like enigo path).
        assert_eq!(key_name_to_hid_usage("mediaplay"), None);
        assert_eq!(key_name_to_hid_usage("?"), None);
    }

    #[test]
    fn mouse_button_bits_pack_in_descriptor_order() {
        let b = MouseButtons { lmb: true, rmb: false, mmb: true, mb4: false, mb5: true };
        assert_eq!(b.bits(), 0b10101);
        assert_eq!(MouseButtons::default().bits(), 0);
    }
}
