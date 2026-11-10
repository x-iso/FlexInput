//! Asset map for the Remapper module. SVGs are embedded at compile time so the
//! binary is self-contained and chip rendering is fast (no disk I/O per frame).
//!
//! KBM coverage: every standalone letter / digit / F-key, modifiers, common
//! punctuation, navigation, and the named mouse + scroll glyphs.
//! Gamepad coverage: face buttons, bumpers, triggers, start/back/guide, D-pad
//! arrows, and stick clicks for Xbox / PlayStation / Switch Pro families.
//! Anything not mapped falls back to the textual pin display.

/// Controller skin family. `Auto` means "detect from upstream device". `Kbm` is
/// internal-only; it is never offered in the skin dropdown because skin governs
/// only gamepad chip rendering — keyboard/mouse pins have no controller
/// equivalent and resolve from the KBM folder regardless of the chosen skin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skin {
    Auto,
    Xbox,
    Playstation,
    SwitchPro,
    /// A Steam Controller: its own glyphs where it has buttons no other pad
    /// has (grips, stick touch, the aux button, two trackpads), the Xbox ones
    /// for the rest, as its face buttons are lettered the same.
    Steam,
    Kbm,
}

impl Skin {
    pub fn from_str(s: &str) -> Self {
        match s {
            "xbox"        => Skin::Xbox,
            "playstation" => Skin::Playstation,
            "switchpro"   => Skin::SwitchPro,
            "steam"       => Skin::Steam,
            "kbm"         => Skin::Kbm,
            _             => Skin::Auto,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Skin::Auto        => "auto",
            Skin::Xbox        => "xbox",
            Skin::Playstation => "playstation",
            Skin::SwitchPro   => "switchpro",
            Skin::Steam       => "steam",
            Skin::Kbm         => "kbm",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Skin::Auto        => "auto",
            Skin::Xbox        => "Xbox",
            Skin::Playstation => "PlayStation",
            Skin::SwitchPro   => "Switch Pro",
            Skin::Steam       => "Steam Controller",
            Skin::Kbm         => "Keyboard",
        }
    }
}

/// Best-effort skin inference from a `device_id` string. Skin governs only
/// gamepad chip rendering — KBM has no controller equivalent, so a keyboard
/// upstream falls back to Xbox icons (which are the visual default).
/// The kind slug of a PHYSICAL gamepad device id, whether it came from gilrs
/// (`gilrs:<slug>:<inst>`) or SDL (`sdl:<slug>:<inst>`). `None` for anything
/// else (MIDI, virtual, unknown). Both backends use the same
/// `ControllerKind::id_slug` values, so downstream code (icon, calibration
/// caps) treats a pad the same regardless of which backend surfaced it.
pub fn phys_pad_slug(dev_id: &str) -> Option<&str> {
    dev_id
        .strip_prefix("gilrs:")
        .or_else(|| dev_id.strip_prefix("sdl:"))
        // Joy-Con 2 over BLE. Its ids are `jc2:<slug>:<bt-address>` — the third
        // field is an address rather than an index, but only the slug is read
        // here so the shape matches.
        .or_else(|| dev_id.strip_prefix("jc2:"))
        // Bluetooth Classic pads: `btc:<slug>:<bt-address>`, same shape.
        .or_else(|| dev_id.strip_prefix("btc:"))
        .and_then(|rest| rest.split(':').next())
}

/// One half of a Joy-Con pair.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoyConHalf {
    Left,
    Right,
}

/// Which Joy-Con half a pad is, of either generation (`joycon_l`, read through
/// SDL, or `joycon2_l`), or `None` for a whole pad.
pub fn joycon_half(dev_id: &str) -> Option<JoyConHalf> {
    match phys_pad_slug(dev_id)? {
        "joycon_l" | "joycon2_l" => Some(JoyConHalf::Left),
        "joycon_r" | "joycon2_r" => Some(JoyConHalf::Right),
        _ => None,
    }
}

/// Whether a pad's ZL/ZR (or LT/RT) are buttons with no travel, by its pad
/// slug — so the digital-trigger bridge is always on for it.
pub fn slug_has_digital_triggers(slug: &str) -> bool {
    matches!(slug, "switch_pro" | "joycon_l" | "joycon_r" | "joycon_pair")
}

pub fn skin_from_device_id(dev_id: &str) -> Skin {
    // Its id says only `sdl:generic:<serial>`; its USB ids say what it is.
    if crate::model::is_steam_controller(dev_id) {
        return Skin::Steam;
    }
    // A Joy-Con's id (`jc2:joycon2_l:…`, `sdl:joycon_l:…`, `sdl:joycon_pair:…`)
    // names no "switch", so it wore Xbox glyphs.
    if joycon_half(dev_id).is_some() || phys_pad_slug(dev_id) == Some("joycon_pair") {
        return Skin::SwitchPro;
    }
    let d = dev_id.to_ascii_lowercase();
    if d.contains("switch")               { return Skin::SwitchPro; }
    if d.contains("dualshock") || d.contains("dualsense") || d.contains("ds4")
        || d.contains("playstation") || d.contains("ps4") || d.contains("ps5") {
        return Skin::Playstation;
    }
    Skin::Xbox
}

// ── Embedded SVG bytes ────────────────────────────────────────────────────────

macro_rules! a { ($p:literal) => { include_bytes!(concat!("../../../../app/assets/", $p)) }; }

// Xbox face + shoulder + sticks + start/back/guide + dpad
const XB_A:       &[u8] = a!("Xbox/xbox_button_a.svg");
const XB_B:       &[u8] = a!("Xbox/xbox_button_b.svg");
const XB_X:       &[u8] = a!("Xbox/xbox_button_x.svg");
const XB_Y:       &[u8] = a!("Xbox/xbox_button_y.svg");
const XB_LB:      &[u8] = a!("Xbox/xbox_lb.svg");
const XB_RB:      &[u8] = a!("Xbox/xbox_rb.svg");
const XB_LT:      &[u8] = a!("Xbox/xbox_lt.svg");
const XB_RT:      &[u8] = a!("Xbox/xbox_rt.svg");
const XB_START:   &[u8] = a!("Xbox/xbox_button_start.svg");
const XB_BACK:    &[u8] = a!("Xbox/xbox_button_back.svg");
const XB_SHARE:   &[u8] = a!("Xbox/xbox_button_share.svg");
const XB_LOGO:    &[u8] = a!("Xbox/xbox_button_logo.svg");
const XB_DPAD_U:  &[u8] = a!("Xbox/xbox_dpad_up.svg");
const XB_DPAD_D:  &[u8] = a!("Xbox/xbox_dpad_down.svg");
const XB_DPAD_L:  &[u8] = a!("Xbox/xbox_dpad_left.svg");
const XB_DPAD_R:  &[u8] = a!("Xbox/xbox_dpad_right.svg");
const XB_DPAD:    &[u8] = a!("Xbox/xbox_dpad.svg");
const XB_DPAD_H:  &[u8] = a!("Xbox/xbox_dpad_horizontal.svg");
const XB_DPAD_V:  &[u8] = a!("Xbox/xbox_dpad_vertical.svg");
const XB_LSTICK_H: &[u8] = a!("Xbox/xbox_stick_l_horizontal.svg");
const XB_LSTICK_V: &[u8] = a!("Xbox/xbox_stick_l_vertical.svg");
const XB_LS:      &[u8] = a!("Xbox/xbox_stick_l_press.svg");
const XB_RS:      &[u8] = a!("Xbox/xbox_stick_r_press.svg");
const XB_LSTICK:  &[u8] = a!("Xbox/xbox_stick_l.svg");
const XB_RSTICK:  &[u8] = a!("Xbox/xbox_stick_r.svg");
const XB_LSTICK_U: &[u8] = a!("Xbox/xbox_stick_l_up.svg");
const XB_LSTICK_D: &[u8] = a!("Xbox/xbox_stick_l_down.svg");
const XB_LSTICK_L: &[u8] = a!("Xbox/xbox_stick_l_left.svg");
const XB_LSTICK_R: &[u8] = a!("Xbox/xbox_stick_l_right.svg");
const XB_RSTICK_U: &[u8] = a!("Xbox/xbox_stick_r_up.svg");
const XB_RSTICK_D: &[u8] = a!("Xbox/xbox_stick_r_down.svg");
const XB_RSTICK_L: &[u8] = a!("Xbox/xbox_stick_r_left.svg");
const XB_RSTICK_R: &[u8] = a!("Xbox/xbox_stick_r_right.svg");

// PlayStation
const PS_CROSS:    &[u8] = a!("Playstation/playstation_button_cross.svg");
const PS_CIRCLE:   &[u8] = a!("Playstation/playstation_button_circle.svg");
const PS_SQUARE:   &[u8] = a!("Playstation/playstation_button_square.svg");
const PS_TRIANGLE: &[u8] = a!("Playstation/playstation_button_triangle.svg");
const PS_L1:       &[u8] = a!("Playstation/playstation_trigger_l1.svg");
const PS_R1:       &[u8] = a!("Playstation/playstation_trigger_r1.svg");
const PS_L2:       &[u8] = a!("Playstation/playstation_trigger_l2.svg");
const PS_R2:       &[u8] = a!("Playstation/playstation_trigger_r2.svg");
const PS_L3:       &[u8] = a!("Playstation/playstation_button_l3.svg");
const PS_R3:       &[u8] = a!("Playstation/playstation_button_r3.svg");
const PS_OPTIONS:  &[u8] = a!("Playstation/playstation4_button_options.svg");
const PS_SHARE:    &[u8] = a!("Playstation/playstation4_button_share.svg");
const PS_MUTE:     &[u8] = a!("Playstation/playstation5_button_mute.svg");
const PS_LOGO:        &[u8] = a!("Playstation/ps4_button_logo.svg");
// Touchpad icons (PS5 set). The plain "click" pin uses the swipe_down asset
// because it carries the clearest tap affordance among the available
// touchpad SVGs.
const PS_TP_CLICK:    &[u8] = a!("Playstation/playstation5_touchpad_swipe_down.svg");
const PS_TP_LEFT:     &[u8] = a!("Playstation/playstation5_touchpad_press_left.svg");
const PS_TP_CENTER:   &[u8] = a!("Playstation/playstation5_touchpad_press_center.svg");
const PS_TP_RIGHT:    &[u8] = a!("Playstation/playstation5_touchpad_press_right.svg");
// Analog swipe gestures (touchpad), used by the Remapper/Lean swipe outputs.
const PS_TP_SWIPE_H:  &[u8] = a!("Playstation/playstation5_touchpad_swipe_horizontal.svg");
const PS_TP_SWIPE_V:  &[u8] = a!("Playstation/playstation5_touchpad_swipe_vertical.svg");
// Directional touchpad swipes — Touch Zones swipe triggers.
const PS_TP_SWIPE_UP:    &[u8] = a!("Playstation/playstation5_touchpad_swipe_up.svg");
const PS_TP_SWIPE_DOWN:  &[u8] = a!("Playstation/playstation5_touchpad_swipe_down.svg");
const PS_TP_SWIPE_LEFT:  &[u8] = a!("Playstation/playstation5_touchpad_swipe_left.svg");
const PS_TP_SWIPE_RIGHT: &[u8] = a!("Playstation/playstation5_touchpad_swipe_right.svg");
const PS_DPAD_U:   &[u8] = a!("Playstation/playstation_dpad_up.svg");
const PS_DPAD_D:   &[u8] = a!("Playstation/playstation_dpad_down.svg");
const PS_DPAD_L:   &[u8] = a!("Playstation/playstation_dpad_left.svg");
const PS_DPAD_R:   &[u8] = a!("Playstation/playstation_dpad_right.svg");
const PS_DPAD:     &[u8] = a!("Playstation/playstation_dpad.svg");
const PS_DPAD_H:   &[u8] = a!("Playstation/playstation_dpad_horizontal.svg");
const PS_DPAD_V:   &[u8] = a!("Playstation/playstation_dpad_vertical.svg");
const PS_LSTICK_H: &[u8] = a!("Playstation/playstation_stick_l_horizontal.svg");
const PS_LSTICK_V: &[u8] = a!("Playstation/playstation_stick_l_vertical.svg");
const PS_LSTICK:   &[u8] = a!("Playstation/playstation_stick_l.svg");
const PS_RSTICK:   &[u8] = a!("Playstation/playstation_stick_r.svg");
const PS_LSTICK_U: &[u8] = a!("Playstation/playstation_stick_l_up.svg");
const PS_LSTICK_D: &[u8] = a!("Playstation/playstation_stick_l_down.svg");
const PS_LSTICK_L: &[u8] = a!("Playstation/playstation_stick_l_left.svg");
const PS_LSTICK_R: &[u8] = a!("Playstation/playstation_stick_l_right.svg");
const PS_RSTICK_U: &[u8] = a!("Playstation/playstation_stick_r_up.svg");
const PS_RSTICK_D: &[u8] = a!("Playstation/playstation_stick_r_down.svg");
const PS_RSTICK_L: &[u8] = a!("Playstation/playstation_stick_r_left.svg");
const PS_RSTICK_R: &[u8] = a!("Playstation/playstation_stick_r_right.svg");

// Switch Pro
const SW_A:       &[u8] = a!("SwitchPro/switch_button_a.svg");
const SW_B:       &[u8] = a!("SwitchPro/switch_button_b.svg");
const SW_X:       &[u8] = a!("SwitchPro/switch_button_x.svg");
const SW_Y:       &[u8] = a!("SwitchPro/switch_button_y.svg");
const SW_L:       &[u8] = a!("SwitchPro/switch_button_l.svg");
const SW_R:       &[u8] = a!("SwitchPro/switch_button_r.svg");
const SW_ZL:      &[u8] = a!("SwitchPro/switch_button_zl.svg");
const SW_ZR:      &[u8] = a!("SwitchPro/switch_button_zr.svg");
const SW_PLUS:    &[u8] = a!("SwitchPro/switch_button_plus.svg");
const SW_MINUS:   &[u8] = a!("SwitchPro/switch_button_minus.svg");
const SW_HOME:    &[u8] = a!("SwitchPro/switchpro_button_home.svg");
const SW_SYNC:    &[u8] = a!("SwitchPro/switch_button_sync.svg");
// A Joy-Con's rail buttons. Both halves have an SL and an SR, so a chip also
// carries which half (`pin_pad_side`).
const SW_SL:      &[u8] = a!("SwitchPro/switch_button_sl.svg");
const SW_SR:      &[u8] = a!("SwitchPro/switch_button_sr.svg");
const SW_DPAD_U:  &[u8] = a!("SwitchPro/switch_dpad_up.svg");
const SW_DPAD_D:  &[u8] = a!("SwitchPro/switch_dpad_down.svg");
const SW_DPAD_L:  &[u8] = a!("SwitchPro/switch_dpad_left.svg");
const SW_DPAD_R:  &[u8] = a!("SwitchPro/switch_dpad_right.svg");
const SW_DPAD:    &[u8] = a!("SwitchPro/switch_dpad.svg");
const SW_DPAD_H:  &[u8] = a!("SwitchPro/switch_dpad_horizontal.svg");
const SW_DPAD_V:  &[u8] = a!("SwitchPro/switch_dpad_vertical.svg");
const SW_LSTICK_H: &[u8] = a!("SwitchPro/switch_stick_l_horizontal.svg");
const SW_LSTICK_V: &[u8] = a!("SwitchPro/switch_stick_l_vertical.svg");
const SW_LSTICK:  &[u8] = a!("SwitchPro/switch_stick_l.svg");
const SW_RSTICK:  &[u8] = a!("SwitchPro/switch_stick_r.svg");
const SW_LSTICK_U: &[u8] = a!("SwitchPro/switch_stick_l_up.svg");
const SW_LSTICK_D: &[u8] = a!("SwitchPro/switch_stick_l_down.svg");
const SW_LSTICK_L: &[u8] = a!("SwitchPro/switch_stick_l_left.svg");
const SW_LSTICK_R: &[u8] = a!("SwitchPro/switch_stick_l_right.svg");
const SW_RSTICK_U: &[u8] = a!("SwitchPro/switch_stick_r_up.svg");
const SW_RSTICK_D: &[u8] = a!("SwitchPro/switch_stick_r_down.svg");
const SW_RSTICK_L: &[u8] = a!("SwitchPro/switch_stick_r_left.svg");
const SW_RSTICK_R: &[u8] = a!("SwitchPro/switch_stick_r_right.svg");
const SW_LS:      &[u8] = a!("SwitchPro/switch_stick_l_press.svg");
const SW_RS:      &[u8] = a!("SwitchPro/switch_stick_r_press.svg");

// Steam Controller: what it has that no other pad does. Its two trackpads wear
// the PS4 touchpad, a side's letter cut out of it (`extra_button_label`).
const SC_GRIP_L:   &[u8] = a!("SC/controller_lg_gradient.svg");
const SC_GRIP_R:   &[u8] = a!("SC/controller_rg_gradient.svg");
const SC_CAP_L:    &[u8] = a!("SC/cap_sens_L.svg");
const SC_CAP_R:    &[u8] = a!("SC/cap_sens_R.svg");
const SC_AUX:      &[u8] = a!("SC/button_aux.svg");
/// The pad's own home glyph, for the Guide button: the Xbox logo it fell back to
/// is not what the pad has a button for.
const SC_HOME:     &[u8] = a!("SC/controller_icon_outline.svg");
const SC_MENU:     &[u8] = a!("SC/sd_button_menu.svg");
const SC_VIEW:     &[u8] = a!("SC/sd_button_view.svg");
const SC_L4:       &[u8] = a!("SC/controller_button_l4.svg");
const SC_L5:       &[u8] = a!("SC/controller_button_l5.svg");
const SC_R4:       &[u8] = a!("SC/controller_button_r4.svg");
const SC_R5:       &[u8] = a!("SC/controller_button_r5.svg");
// The click is the pad with its press notch; a touch (and how hard it's
// pressed), the pad's surface. Both keep a cut-out letter legible, as the
// arrowed glyphs don't.
const PS4_TP_CLICK:  &[u8] = a!("Playstation/playstation4_touchpad_press.svg");
const PS4_TP_SURFACE: &[u8] = a!("Playstation/playstation4_touchpad.svg");
const PS4_TP_LEFT:   &[u8] = a!("Playstation/playstation4_touchpad_press_left.svg");
const PS4_TP_CENTER: &[u8] = a!("Playstation/playstation4_touchpad_press_center.svg");
const PS4_TP_RIGHT:  &[u8] = a!("Playstation/playstation4_touchpad_press_right.svg");
// A Steam Controller's pads are rounded squares, like the PS4's touchpad and
// nothing like PS5's wide bar, so its swipes — which way, and how far along each
// axis — are drawn on that same surface.
const PS4_SWIPE_UP:    &[u8] = a!("Playstation/playstation4_touchpad_swipe_up.svg");
const PS4_SWIPE_DOWN:  &[u8] = a!("Playstation/playstation4_touchpad_swipe_down.svg");
const PS4_SWIPE_LEFT:  &[u8] = a!("Playstation/playstation4_touchpad_swipe_left.svg");
const PS4_SWIPE_RIGHT: &[u8] = a!("Playstation/playstation4_touchpad_swipe_right.svg");
const PS4_SWIPE_H:     &[u8] = a!("Playstation/playstation4_touchpad_swipe_horizontal.svg");
const PS4_SWIPE_V:     &[u8] = a!("Playstation/playstation4_touchpad_swipe_vertical.svg");

/// A Steam Controller's glyph for `pin_id`, where it has one of its own. Both
/// namings: the controls the pad names itself (read off its own reports) and the
/// Misc slots SDL hands out for them, which is what a pad read through SDL
/// carries.
fn steam_pin_svg(pin_id: &str) -> Option<&'static [u8]> {
    Some(match pin_id {
        // The three-dot button: Capture on the pad's own naming, Misc 1 on SDL's.
        "btn_capture" | "btn_misc1" => SC_AUX,
        "btn_cap_l" | "btn_misc3" => SC_CAP_L,
        "btn_cap_r" | "btn_misc4" => SC_CAP_R,
        "btn_grip_l" | "btn_misc5" => SC_GRIP_L,
        "btn_grip_r" | "btn_misc6" => SC_GRIP_R,
        "btn_back" => SC_VIEW,
        "btn_start" => SC_MENU,
        "btn_guide" => SC_HOME,
        // SDL's Triton mapping: the upper back buttons on the first paddle pair.
        "btn_paddle_l1" => SC_L4,
        "btn_paddle_r1" => SC_R4,
        "btn_paddle_l2" => SC_L5,
        "btn_paddle_r2" => SC_R5,
        "btn_touchpad" | "btn_touchpad2" | "btn_misc2" | "touchpad_any" => PS4_TP_CLICK,
        // The analog swipe OUT too: a pad of this shape driving a mouse.
        "touch_swipe_x" => PS4_SWIPE_H,
        "touch_swipe_y" => PS4_SWIPE_V,
        "touch1_active" | "touch2_active" | "touch1_pressure" | "touch2_pressure" => PS4_TP_SURFACE,
        "touch_left" | "touchpad_left" => PS4_TP_LEFT,
        "touch_center" | "touchpad_center" => PS4_TP_CENTER,
        "touch_right" | "touchpad_right" => PS4_TP_RIGHT,
        _ => return None,
    })
}

// KB/M — modifiers + special keys
const KB_SHIFT:    &[u8] = a!("KBM/keyboard_shift.svg");
const KB_CTRL:     &[u8] = a!("KBM/keyboard_ctrl.svg");
const KB_ALT:      &[u8] = a!("KBM/keyboard_alt.svg");
const KB_WIN:      &[u8] = a!("KBM/keyboard_win.svg");
const KB_ESCAPE:   &[u8] = a!("KBM/keyboard_escape.svg");
const KB_SPACE:    &[u8] = a!("KBM/keyboard_space.svg");
const KB_ENTER:    &[u8] = a!("KBM/keyboard_enter.svg");
const KB_TAB:      &[u8] = a!("KBM/keyboard_tab.svg");
const KB_BACKSPACE:&[u8] = a!("KBM/keyboard_backspace.svg");
const KB_DELETE:   &[u8] = a!("KBM/keyboard_delete.svg");
const KB_INSERT:   &[u8] = a!("KBM/keyboard_insert.svg");
const KB_HOME:     &[u8] = a!("KBM/keyboard_home.svg");
const KB_END:      &[u8] = a!("KBM/keyboard_end.svg");
const KB_PAGE_UP:  &[u8] = a!("KBM/keyboard_page_up.svg");
const KB_PAGE_DOWN:&[u8] = a!("KBM/keyboard_page_down.svg");
const KB_PRINTSCR: &[u8] = a!("KBM/keyboard_printscreen.svg");
const KB_PAUSE:    &[u8] = a!("KBM/keyboard_pause_break.svg");
const KB_CAPS:     &[u8] = a!("KBM/keyboard_capslock.svg");
const KB_NUMLOCK:  &[u8] = a!("KBM/keyboard_numlock.svg");
const KB_SCROLL_LK:&[u8] = a!("KBM/keyboard_scroll_lock.svg");
const KB_ARROW_U:  &[u8] = a!("KBM/keyboard_arrow_up.svg");
const KB_ARROW_D:  &[u8] = a!("KBM/keyboard_arrow_down.svg");
const KB_ARROW_L:  &[u8] = a!("KBM/keyboard_arrow_left.svg");
const KB_ARROW_R:  &[u8] = a!("KBM/keyboard_arrow_right.svg");

// KB/M — letters
const KB_A: &[u8] = a!("KBM/keyboard_a.svg");
const KB_B: &[u8] = a!("KBM/keyboard_b.svg");
const KB_C: &[u8] = a!("KBM/keyboard_c.svg");
const KB_D: &[u8] = a!("KBM/keyboard_d.svg");
const KB_E: &[u8] = a!("KBM/keyboard_e.svg");
const KB_F: &[u8] = a!("KBM/keyboard_f.svg");
const KB_G: &[u8] = a!("KBM/keyboard_g.svg");
const KB_H: &[u8] = a!("KBM/keyboard_h.svg");
const KB_I: &[u8] = a!("KBM/keyboard_i.svg");
const KB_J: &[u8] = a!("KBM/keyboard_j.svg");
const KB_K: &[u8] = a!("KBM/keyboard_k.svg");
const KB_L: &[u8] = a!("KBM/keyboard_l.svg");
const KB_M: &[u8] = a!("KBM/keyboard_m.svg");
const KB_N: &[u8] = a!("KBM/keyboard_n.svg");
const KB_O: &[u8] = a!("KBM/keyboard_o.svg");
const KB_P: &[u8] = a!("KBM/keyboard_p.svg");
const KB_Q: &[u8] = a!("KBM/keyboard_q.svg");
const KB_R: &[u8] = a!("KBM/keyboard_r.svg");
const KB_S: &[u8] = a!("KBM/keyboard_s.svg");
const KB_T: &[u8] = a!("KBM/keyboard_t.svg");
const KB_U: &[u8] = a!("KBM/keyboard_u.svg");
const KB_V: &[u8] = a!("KBM/keyboard_v.svg");
const KB_W: &[u8] = a!("KBM/keyboard_w.svg");
const KB_X: &[u8] = a!("KBM/keyboard_x.svg");
const KB_Y: &[u8] = a!("KBM/keyboard_y.svg");
const KB_Z: &[u8] = a!("KBM/keyboard_z.svg");

// KB/M — digits
const KB_0: &[u8] = a!("KBM/keyboard_0.svg");
const KB_1: &[u8] = a!("KBM/keyboard_1.svg");
const KB_2: &[u8] = a!("KBM/keyboard_2.svg");
const KB_3: &[u8] = a!("KBM/keyboard_3.svg");
const KB_4: &[u8] = a!("KBM/keyboard_4.svg");
const KB_5: &[u8] = a!("KBM/keyboard_5.svg");
const KB_6: &[u8] = a!("KBM/keyboard_6.svg");
const KB_7: &[u8] = a!("KBM/keyboard_7.svg");
const KB_8: &[u8] = a!("KBM/keyboard_8.svg");
const KB_9: &[u8] = a!("KBM/keyboard_9.svg");

// KB/M — F-keys
const KB_F1:  &[u8] = a!("KBM/keyboard_f1.svg");
const KB_F2:  &[u8] = a!("KBM/keyboard_f2.svg");
const KB_F3:  &[u8] = a!("KBM/keyboard_f3.svg");
const KB_F4:  &[u8] = a!("KBM/keyboard_f4.svg");
const KB_F5:  &[u8] = a!("KBM/keyboard_f5.svg");
const KB_F6:  &[u8] = a!("KBM/keyboard_f6.svg");
const KB_F7:  &[u8] = a!("KBM/keyboard_f7.svg");
const KB_F8:  &[u8] = a!("KBM/keyboard_f8.svg");
const KB_F9:  &[u8] = a!("KBM/keyboard_f9.svg");
const KB_F10: &[u8] = a!("KBM/keyboard_f10.svg");
const KB_F11: &[u8] = a!("KBM/keyboard_f11.svg");
const KB_F12: &[u8] = a!("KBM/keyboard_f12.svg");

// KB/M — punctuation
const KB_COMMA:      &[u8] = a!("KBM/keyboard_comma.svg");
const KB_PERIOD:     &[u8] = a!("KBM/keyboard_period.svg");
const KB_SEMICOLON:  &[u8] = a!("KBM/keyboard_semicolon.svg");
const KB_QUOTE:      &[u8] = a!("KBM/keyboard_quote.svg");
const KB_APOSTROPHE: &[u8] = a!("KBM/keyboard_apostrophe.svg");
const KB_MINUS:      &[u8] = a!("KBM/keyboard_minus.svg");
const KB_PLUS:       &[u8] = a!("KBM/keyboard_plus.svg");
const KB_EQUALS:     &[u8] = a!("KBM/keyboard_equals.svg");
const KB_SLASH_F:    &[u8] = a!("KBM/keyboard_slash_forward.svg");
const KB_SLASH_B:    &[u8] = a!("KBM/keyboard_slash_back.svg");
const KB_BRK_OPEN:   &[u8] = a!("KBM/keyboard_bracket_open.svg");
const KB_BRK_CLOSE:  &[u8] = a!("KBM/keyboard_bracket_close.svg");
const KB_TILDE:      &[u8] = a!("KBM/keyboard_tilde.svg");
const KB_COLON:      &[u8] = a!("KBM/keyboard_colon.svg");
const KB_QUESTION:   &[u8] = a!("KBM/keyboard_question.svg");
const KB_EXCLAIM:    &[u8] = a!("KBM/keyboard_exclamation.svg");
const KB_CARET:      &[u8] = a!("KBM/keyboard_caret.svg");
const KB_ASTERISK:   &[u8] = a!("KBM/keyboard_asterisk.svg");
const KB_UNDERSCORE: &[u8] = a!("KBM/keyboard_underscore.svg");
const KB_LESS:       &[u8] = a!("KBM/keyboard_bracket_less.svg");
const KB_GREATER:    &[u8] = a!("KBM/keyboard_bracket_greater.svg");
/// A key with no symbol on it, for a character with no icon of its own: the
/// renderer draws the character over it. Added so the shifted half of the number
/// row can be SHOWN at all.
const KB_BLANK:      &[u8] = a!("KBM/keyboard_blank.svg");


/// The icon for a CHARACTER, rather than for a pin.
///
/// A virtual keyboard that is typing has to show what each key would actually
/// add, and the caps latch changes half of them — a number row that doesn't
/// change is a caps key you can't tell you pressed. `None` means no asset has
/// that glyph, and the caller draws it over [`kb_blank`] instead.
pub fn char_svg(c: char) -> Option<&'static [u8]> {
    Some(match c {
        '0' => KB_0, '1' => KB_1, '2' => KB_2, '3' => KB_3, '4' => KB_4,
        '5' => KB_5, '6' => KB_6, '7' => KB_7, '8' => KB_8, '9' => KB_9,
        '!' => KB_EXCLAIM,
        '^' => KB_CARET,
        '*' => KB_ASTERISK,
        '-' => KB_MINUS,
        '_' => KB_UNDERSCORE,
        '=' => KB_EQUALS,
        '+' => KB_PLUS,
        '[' => KB_BRK_OPEN,
        ']' => KB_BRK_CLOSE,
        '/' => KB_SLASH_F,
        '\\' => KB_SLASH_B,
        ';' => KB_SEMICOLON,
        ':' => KB_COLON,
        '\'' => KB_APOSTROPHE,
        '\"' => KB_QUOTE,
        ',' => KB_COMMA,
        '<' => KB_LESS,
        '.' => KB_PERIOD,
        '>' => KB_GREATER,
        '?' => KB_QUESTION,
        // The backtick has no glyph of its own in the set, and wears the tilde's
        // — as it already does when addressed by pin.
        '`' | '~' => KB_TILDE,
        // @ # $ % & ( ) { } | have no asset in the set. They come back as None
        // and are drawn over the blank key.
        _ => return None,
    })
}

/// The blank key shape, for drawing a character over.
pub fn kb_blank() -> &'static [u8] {
    KB_BLANK
}

// Mouse
const M_LEFT:     &[u8] = a!("KBM/mouse_left.svg");
const M_RIGHT:    &[u8] = a!("KBM/mouse_right.svg");
// MMB = the scroll wheel; mouse_scroll.svg highlights the wheel (mouse.svg is
// the plain body).
const M_MIDDLE:   &[u8] = a!("KBM/mouse_scroll.svg");
const M_SIDE_BACK:    &[u8] = a!("KBM/mouse_side_back.svg");
const M_SIDE_FORWARD: &[u8] = a!("KBM/mouse_side_forward.svg");
const M_SCROLL_U: &[u8] = a!("KBM/mouse_scroll_up.svg");
const M_SCROLL_D: &[u8] = a!("KBM/mouse_scroll_down.svg");
// Horizontal scroll: bool left/right + the analog (variable-speed) axis glyph.
const M_SCROLL_L:  &[u8] = a!("KBM/mouse_horizontal_scroll_down.svg");
const M_SCROLL_R:  &[u8] = a!("KBM/mouse_horizontal_scroll_up.svg");
const M_HSCROLL:   &[u8] = a!("KBM/mouse_horizontal_scroll.svg");
// Vertical analog (variable-speed) scroll axis glyph.
const M_VSCROLL:   &[u8] = a!("KBM/mouse_scroll_vertical.svg");
// Mouse-movement delta outputs (Touch Zones relative-mouse mapping).
const M_MOVE:     &[u8] = a!("KBM/mouse_move.svg");
const M_MOVE_H:   &[u8] = a!("KBM/mouse_horizontal.svg");
const M_MOVE_V:   &[u8] = a!("KBM/mouse_vertical.svg");

/// Long-arrow glyph used in place of the textual "→" between a mapping's
/// input and output chips. Rasterized once and cached as an egui texture.
pub const ARROW_LONG_SVG: &[u8] = a!("flair_arrow_long.svg");

// ── Device-card icons ─────────────────────────────────────────────────────────
// Larger per-device images shown in the Physical/Virtual device panels —
// separate from the per-pin chip glyphs above.

const DEV_XBOX:     &[u8] = a!("Xbox/controller_xboxone.svg");
const DEV_PS4:      &[u8] = a!("Playstation/controller_playstation4.svg");
const DEV_PS5:      &[u8] = a!("Playstation/controller_playstation5.svg");
const DEV_SWITCH:   &[u8] = a!("SwitchPro/controller_switch_pro.svg");
// One Joy-Con, either generation, and a pair as one pad (the console with both
// attached).
const DEV_JOYCON_L:    &[u8] = a!("SwitchPro/switch_joycon_left.svg");
const DEV_JOYCON_R:    &[u8] = a!("SwitchPro/switch_joycon_right.svg");
const DEV_JOYCON_PAIR: &[u8] = a!("SwitchPro/controller_switch.svg");
const DEV_STEAM:    &[u8] = a!("SC/sc2.svg");
const DEV_MIDI_IN:  &[u8] = a!("MIDI_in.svg");
const DEV_MIDI_OUT: &[u8] = a!("MIDI_out.svg");
/// Combined keyboard + mouse glyph — used wherever Virtual Keyboard
/// & Mouse needs a single, compact icon (Easy mode card, virtual
/// device panel chip, sub-patch node header).
const DEV_KBM: &[u8] = a!("kbm.svg");

// Generic (family-neutral) button glyphs for extra buttons SDL reports on pads
// FlexInput doesn't skin natively (rear paddles). The same left/right glyph
// serves both paddle rows (L1/L2 share GEN_PADDLE_L); the on-icon text label
// (see `extra_button_label`) is what distinguishes them, painted over the icon
// at the render site. Misc buttons have no dedicated glyph and fall through to
// the text pill.
const GEN_PADDLE_L: &[u8] = a!("general/generic_button_gl.svg");
const GEN_PADDLE_R: &[u8] = a!("general/generic_button_gr.svg");

// The pad's motion, as a Remapper input (`flexinput_core::imu`).
const IMU_GYRO_PITCH:      &[u8] = a!("Motion/gyro_pitch.svg");
/// Set Neutral Forward: a pad turned at an angle.
const ORIENT_NEUTRAL:      &[u8] = a!("angled_pad.svg");
/// Calibrate Gyro: the pad with its motion glow.
const GYRO_CAL:            &[u8] = a!("Motion/gyro.svg");
const IMU_GYRO_PITCH_UP:   &[u8] = a!("Motion/gyro_pitch_up.svg");
const IMU_GYRO_PITCH_DOWN: &[u8] = a!("Motion/gyro_pitch_down.svg");
const IMU_GYRO_YAW:        &[u8] = a!("Motion/gyro_yaw.svg");
const IMU_GYRO_YAW_LEFT:   &[u8] = a!("Motion/gyro_yaw_left.svg");
const IMU_GYRO_YAW_RIGHT:  &[u8] = a!("Motion/gyro_yaw_right.svg");
const IMU_GYRO_ROLL:       &[u8] = a!("Motion/gyro_roll.svg");
const IMU_GYRO_ROLL_LEFT:  &[u8] = a!("Motion/gyro_roll_left.svg");
const IMU_GYRO_ROLL_RIGHT: &[u8] = a!("Motion/gyro_roll_right.svg");
const IMU_ACCEL_UD:        &[u8] = a!("Motion/accel_y.svg");
const IMU_ACCEL_UP:        &[u8] = a!("Motion/accel_y_up.svg");
const IMU_ACCEL_DOWN:      &[u8] = a!("Motion/accel_y_down.svg");
const IMU_ACCEL_LR:        &[u8] = a!("Motion/accel_x.svg");
const IMU_ACCEL_LEFT:      &[u8] = a!("Motion/accel_x_left.svg");
const IMU_ACCEL_RIGHT:     &[u8] = a!("Motion/accel_x_right.svg");
const IMU_ACCEL_FB:        &[u8] = a!("Motion/accel_z.svg");
const IMU_ACCEL_FORWARD:   &[u8] = a!("Motion/accel_z_forward.svg");
const IMU_ACCEL_BACKWARD:  &[u8] = a!("Motion/accel_z_back.svg");
const IMU_TILT_PITCH:      &[u8] = a!("Motion/tilt-noseUD.svg");
const IMU_TILT_UP:         &[u8] = a!("Motion/tilt-noseUp.svg");
const IMU_TILT_DOWN:       &[u8] = a!("Motion/tilt-noseDown.svg");
const IMU_TILT_ROLL:       &[u8] = a!("Motion/tilt-leftright.svg");
const IMU_TILT_LEFT:       &[u8] = a!("Motion/tilt-left.svg");
const IMU_TILT_RIGHT:      &[u8] = a!("Motion/tilt-right.svg");

/// Generic action icons used by panel chrome (add button, close button, …).
pub const ADD_SVG:   &[u8] = a!("add.svg");
pub const CLOSE_SVG: &[u8] = a!("close.svg");

/// Resolve a device-card icon from a `ControllerKind`, and the USB ids that
/// tell a Steam Controller from another Generic pad.
pub fn device_card_svg(kind: flexinput_devices::ControllerKind, vid: Option<u16>, pid: Option<u16>) -> &'static [u8] {
    use flexinput_devices::ControllerKind as K;
    if flexinput_devices::steam_haptics::is_steam_controller(vid, pid) {
        return DEV_STEAM;
    }
    match kind {
        K::XInput     => DEV_XBOX,
        K::DualShock4 => DEV_PS4,
        K::DualSense  => DEV_PS5,
        K::SwitchPro  => DEV_SWITCH,
        // ❗ Not `controller_switch_joycon_up/down.svg`: those are rail
        // attach/detach glyphs (a body with an arrow), not a side's art.
        K::JoyCon2L | K::JoyConL => DEV_JOYCON_L,
        K::JoyCon2R | K::JoyConR => DEV_JOYCON_R,
        K::JoyConPair => DEV_JOYCON_PAIR,
        K::Generic    => DEV_XBOX,
        K::MidiIn     => DEV_MIDI_IN,
        K::MidiOut    => DEV_MIDI_OUT,
    }
}

/// Resolve a virtual-device card icon by `kind_prefix` (e.g. `"virtual.xinput"`).
pub fn virtual_device_card_svg(kind_prefix: &str) -> &'static [u8] {
    match kind_prefix {
        "virtual.xinput"       => DEV_XBOX,
        "virtual.ds4"          => DEV_PS4,
        "virtual.keymouse"     => DEV_KBM,
        // HIDMaestro Xbox 360 uses the Xbox glyph; the Sony pads use PlayStation.
        "virtual.hm.xinput"    => DEV_XBOX,
        "virtual.hm.ds4"       => DEV_PS4,
        "virtual.hm.dualsense" => DEV_PS5,
        "virtual.hm.dualsense_edge" => DEV_PS5,
        "virtual.usb.dualsense" | "virtual.usb.dualsense_edge" => DEV_PS5,
        _ if kind_prefix.starts_with("virtual.hm") => DEV_PS4,
        _                      => DEV_XBOX,
    }
}

/// Single combined keyboard + mouse glyph (replaces the prior
/// `keymouse_pair_svgs` pair). Used by Easy mode + the virtual
/// device chip + any other single-icon call site.
pub fn keymouse_svg() -> &'static [u8] { DEV_KBM }

/// The SVG to render in a canvas device-node header. (Historically also
/// carried a two-glyph `Pair` for `virtual.keymouse`, now a combined glyph.)
pub enum NodeIconSpec {
    Single(&'static [u8]),
}

/// Resolve the canvas-node icon for any device id used by `device.source`
/// or `device.sink` nodes. Recognised id shapes:
/// - `gilrs:<slug>:<inst>` physical gamepad
/// - `midi_in:<N>` / `midi_out:<N>` physical MIDI port
/// - `virtual.xinput:<inst>` / `virtual.ds4:<inst>` virtual gamepad
/// - `virtual.keymouse[:<inst>]` virtual keyboard + mouse (pair)
pub fn device_node_icon_for_id(dev_id: &str) -> Option<NodeIconSpec> {
    if crate::model::is_steam_controller(dev_id) {
        return Some(NodeIconSpec::Single(DEV_STEAM));
    }
    if let Some(slug) = phys_pad_slug(dev_id) {
        return Some(NodeIconSpec::Single(match (slug, joycon_half(dev_id)) {
            (_, Some(JoyConHalf::Left))  => DEV_JOYCON_L,
            (_, Some(JoyConHalf::Right)) => DEV_JOYCON_R,
            ("joycon_pair", _) => DEV_JOYCON_PAIR,
            ("xinput", _)     => DEV_XBOX,
            ("ds4", _)        => DEV_PS4,
            ("dualsense", _)  => DEV_PS5,
            ("switch_pro", _) => DEV_SWITCH,
            _                 => DEV_XBOX,
        }));
    }
    if dev_id.starts_with("midi_in")  { return Some(NodeIconSpec::Single(DEV_MIDI_IN));  }
    if dev_id.starts_with("midi_out") { return Some(NodeIconSpec::Single(DEV_MIDI_OUT)); }
    if let Some(rest) = dev_id.strip_prefix("virtual.") {
        // `rest` is e.g. "xinput", "ds4.1", "hm.dualsense", "hm.xinput.1".
        let kind = rest.split(':').next().unwrap_or(rest);
        return match kind {
            "xinput"          => Some(NodeIconSpec::Single(DEV_XBOX)),
            "ds4"             => Some(NodeIconSpec::Single(DEV_PS4)),
            "keymouse"        => Some(NodeIconSpec::Single(DEV_KBM)),
            _ if kind.starts_with("hm.xinput")    => Some(NodeIconSpec::Single(DEV_XBOX)),
            _ if kind.starts_with("hm.dualsense") => Some(NodeIconSpec::Single(DEV_PS5)),
            _ if kind.starts_with("usb.dualsense") => Some(NodeIconSpec::Single(DEV_PS5)),
            _ if kind.starts_with("hm.ds4")       => Some(NodeIconSpec::Single(DEV_PS4)),
            _                 => None,
        };
    }
    None
}

/// Resolve the SVG bytes for a canonical AutoMap pin under a given skin.
/// Returns None when no icon is mapped (caller renders the text label).
/// KBM-family pins (`key_*`, `mouse_*`, `scroll_*`) always resolve from the
/// KBM folder regardless of the requested skin.
pub fn pin_svg(skin: Skin, pin_id: &str) -> Option<&'static [u8]> {
    if skin == Skin::Steam {
        if let Some(b) = steam_pin_svg(pin_id) {
            return Some(b);
        }
    }
    // On a Nintendo pad the paddle pins are a Joy-Con's rail buttons (SL first,
    // then SR, on either half); no other Nintendo pad has paddles.
    if skin == Skin::SwitchPro {
        match pin_id {
            "btn_paddle_l1" | "btn_paddle_r1" => return Some(SW_SL),
            "btn_paddle_l2" | "btn_paddle_r2" => return Some(SW_SR),
            _ => {}
        }
    }
    // KBM pins are family-agnostic.
    match pin_id {
        // Modifiers + named special keys
        // Sided and unsided share a face: which Shift it is belongs on the
        // label under it, not in a second drawing of the same key.
        "key_shift" | "key_lshift" | "key_rshift" => return Some(KB_SHIFT),
        "key_ctrl" | "key_lctrl" | "key_rctrl"    => return Some(KB_CTRL),
        "key_alt" | "key_lalt" | "key_ralt"       => return Some(KB_ALT),
        "key_win" | "key_lwin" | "key_rwin"       => return Some(KB_WIN),
        "key_escape"      => return Some(KB_ESCAPE),
        "key_space"       => return Some(KB_SPACE),
        "key_enter"       => return Some(KB_ENTER),
        "key_tab"         => return Some(KB_TAB),
        "key_backspace"   => return Some(KB_BACKSPACE),
        "key_delete"      => return Some(KB_DELETE),
        "key_insert"      => return Some(KB_INSERT),
        "key_home"        => return Some(KB_HOME),
        "key_end"         => return Some(KB_END),
        "key_pageup"      => return Some(KB_PAGE_UP),
        "key_pagedown"    => return Some(KB_PAGE_DOWN),
        "key_printscreen" => return Some(KB_PRINTSCR),
        "key_pause"       => return Some(KB_PAUSE),
        "key_capslock"    => return Some(KB_CAPS),
        "key_numlock"     => return Some(KB_NUMLOCK),
        "key_scrolllock"  => return Some(KB_SCROLL_LK),
        "key_arrowup"     => return Some(KB_ARROW_U),
        "key_arrowdown"   => return Some(KB_ARROW_D),
        "key_arrowleft"   => return Some(KB_ARROW_L),
        "key_arrowright"  => return Some(KB_ARROW_R),

        // Letters (single-letter ids match Key::A..Key::Z Debug lower-cased).
        "key_a" => return Some(KB_A), "key_b" => return Some(KB_B), "key_c" => return Some(KB_C),
        "key_d" => return Some(KB_D), "key_e" => return Some(KB_E), "key_f" => return Some(KB_F),
        "key_g" => return Some(KB_G), "key_h" => return Some(KB_H), "key_i" => return Some(KB_I),
        "key_j" => return Some(KB_J), "key_k" => return Some(KB_K), "key_l" => return Some(KB_L),
        "key_m" => return Some(KB_M), "key_n" => return Some(KB_N), "key_o" => return Some(KB_O),
        "key_p" => return Some(KB_P), "key_q" => return Some(KB_Q), "key_r" => return Some(KB_R),
        "key_s" => return Some(KB_S), "key_t" => return Some(KB_T), "key_u" => return Some(KB_U),
        "key_v" => return Some(KB_V), "key_w" => return Some(KB_W), "key_x" => return Some(KB_X),
        "key_y" => return Some(KB_Y), "key_z" => return Some(KB_Z),

        // Digits: egui::Key::Num0 → Debug "Num0" → our id `key_num0`.
        "key_num0" => return Some(KB_0), "key_num1" => return Some(KB_1),
        "key_num2" => return Some(KB_2), "key_num3" => return Some(KB_3),
        "key_num4" => return Some(KB_4), "key_num5" => return Some(KB_5),
        "key_num6" => return Some(KB_6), "key_num7" => return Some(KB_7),
        "key_num8" => return Some(KB_8), "key_num9" => return Some(KB_9),

        // F-keys
        "key_f1" => return Some(KB_F1),   "key_f2" => return Some(KB_F2),
        "key_f3" => return Some(KB_F3),   "key_f4" => return Some(KB_F4),
        "key_f5" => return Some(KB_F5),   "key_f6" => return Some(KB_F6),
        "key_f7" => return Some(KB_F7),   "key_f8" => return Some(KB_F8),
        "key_f9" => return Some(KB_F9),   "key_f10" => return Some(KB_F10),
        "key_f11" => return Some(KB_F11), "key_f12" => return Some(KB_F12),

        // Punctuation (egui Debug names round-tripped to lowercase).
        "key_comma"           => return Some(KB_COMMA),
        "key_period"          => return Some(KB_PERIOD),
        "key_semicolon"       => return Some(KB_SEMICOLON),
        "key_quote"           => return Some(KB_QUOTE),
        "key_apostrophe"      => return Some(KB_APOSTROPHE),
        "key_minus"           => return Some(KB_MINUS),
        "key_plus"            => return Some(KB_PLUS),
        "key_equals"          => return Some(KB_EQUALS),
        "key_slash"           => return Some(KB_SLASH_F),
        "key_backslash"       => return Some(KB_SLASH_B),
        "key_openbracket"     => return Some(KB_BRK_OPEN),
        "key_closebracket"    => return Some(KB_BRK_CLOSE),
        "key_backtick"        => return Some(KB_TILDE),
        "key_colon"           => return Some(KB_COLON),
        "key_questionmark"    => return Some(KB_QUESTION),
        "key_exclamationmark" => return Some(KB_EXCLAIM),

        "mouse_left"    => return Some(M_LEFT),
        "mouse_right"   => return Some(M_RIGHT),
        "mouse_middle"  => return Some(M_MIDDLE),
        "mouse_back"    => return Some(M_SIDE_BACK),
        "mouse_forward" => return Some(M_SIDE_FORWARD),
        "scroll_up"     => return Some(M_SCROLL_U),
        "scroll_down"   => return Some(M_SCROLL_D),
        "scroll_left"   => return Some(M_SCROLL_L),
        "scroll_right"  => return Some(M_SCROLL_R),
        "scroll_x" | "trackpad_scroll_x" | "scroll_move_x" => return Some(M_HSCROLL),
        "scroll_y" | "trackpad_scroll_y" | "scroll_move_y" => return Some(M_VSCROLL),
        // Mouse-movement delta (Touch Zones relative-mouse outputs).
        "mouse"         => return Some(M_MOVE),
        "mouse_x"       => return Some(M_MOVE_H),
        "mouse_y"       => return Some(M_MOVE_V),

        // Touchpad + DualSense-specific pins are family-agnostic (no Xbox
        // equivalent): always resolve to the PS asset so they render in the
        // KB/M picker (Skin::Kbm) and on non-PS skins too.
        "touch_swipe_x" => return Some(PS_TP_SWIPE_H),
        "touch_swipe_y" => return Some(PS_TP_SWIPE_V),
        // Touch Zones trigger tokens (family-agnostic touchpad glyphs).
        "tz_touch"       => return Some(PS_TP_CENTER),
        "tz_click"       => return Some(PS_TP_CLICK),
        "tz_swipe_up"    => return Some(PS_TP_SWIPE_UP),
        "tz_swipe_down"  => return Some(PS_TP_SWIPE_DOWN),
        "tz_swipe_left"  => return Some(PS_TP_SWIPE_LEFT),
        "tz_swipe_right" => return Some(PS_TP_SWIPE_RIGHT),
        // A pad's own swipe, per touch field: which way it went, and how far
        // along each axis. These are the pins of a pad with trackpads of its own
        // (a Steam Controller), so they wear the square-pad glyphs of the PS4 set,
        // as its touch, click and press cells do — PS5's touchpad is a wide bar
        // nothing like either.
        "touch1_swipe_up" | "touch2_swipe_up"       => return Some(PS4_SWIPE_UP),
        "touch1_swipe_down" | "touch2_swipe_down"   => return Some(PS4_SWIPE_DOWN),
        "touch1_swipe_left" | "touch2_swipe_left"   => return Some(PS4_SWIPE_LEFT),
        "touch1_swipe_right" | "touch2_swipe_right" => return Some(PS4_SWIPE_RIGHT),
        "touch1_swipe_x" | "touch2_swipe_x"         => return Some(PS4_SWIPE_H),
        "touch1_swipe_y" | "touch2_swipe_y"         => return Some(PS4_SWIPE_V),
        "touch_left"   | "touchpad_left"   => return Some(PS_TP_LEFT),
        "touch_center" | "touchpad_center" => return Some(PS_TP_CENTER),
        "touch_right"  | "touchpad_right"  => return Some(PS_TP_RIGHT),
        "btn_touchpad" | "touchpad_any"    => return Some(PS_TP_CLICK),
        "touch1_pressure" | "touch2_pressure" => return Some(PS4_TP_SURFACE),
        "btn_mute"                         => return Some(PS_MUTE),
        // Extra rear paddles (family-neutral generic glyph; left vs right by side).
        // Both paddle rows on a side share one glyph — the label overlay tells
        // L1/L2 apart. Misc buttons intentionally have no glyph → text pill.
        "btn_paddle_l1" | "btn_paddle_l2" => return Some(GEN_PADDLE_L),
        "btn_paddle_r1" | "btn_paddle_r2" => return Some(GEN_PADDLE_R),
        // The pad's motion: family-agnostic, like the touchpad.
        flexinput_core::imu::ORIENT_NEUTRAL_PIN => return Some(ORIENT_NEUTRAL),
        flexinput_core::imu::GYRO_CAL_PIN => return Some(GYRO_CAL),
        "imu_gyro_pitch"      => return Some(IMU_GYRO_PITCH),
        "imu_gyro_pitch_up"   => return Some(IMU_GYRO_PITCH_UP),
        "imu_gyro_pitch_down" => return Some(IMU_GYRO_PITCH_DOWN),
        "imu_gyro_yaw"        => return Some(IMU_GYRO_YAW),
        "imu_gyro_yaw_left"   => return Some(IMU_GYRO_YAW_LEFT),
        "imu_gyro_yaw_right"  => return Some(IMU_GYRO_YAW_RIGHT),
        "imu_gyro_roll"       => return Some(IMU_GYRO_ROLL),
        "imu_gyro_roll_left"  => return Some(IMU_GYRO_ROLL_LEFT),
        "imu_gyro_roll_right" => return Some(IMU_GYRO_ROLL_RIGHT),
        "imu_accel_ud"        => return Some(IMU_ACCEL_UD),
        "imu_accel_up"        => return Some(IMU_ACCEL_UP),
        "imu_accel_down"      => return Some(IMU_ACCEL_DOWN),
        "imu_accel_lr"        => return Some(IMU_ACCEL_LR),
        "imu_accel_left"      => return Some(IMU_ACCEL_LEFT),
        "imu_accel_right"     => return Some(IMU_ACCEL_RIGHT),
        "imu_accel_fb"        => return Some(IMU_ACCEL_FB),
        "imu_accel_forward"   => return Some(IMU_ACCEL_FORWARD),
        "imu_accel_backward"  => return Some(IMU_ACCEL_BACKWARD),
        "imu_tilt_pitch"      => return Some(IMU_TILT_PITCH),
        "imu_tilt_up"         => return Some(IMU_TILT_UP),
        "imu_tilt_down"       => return Some(IMU_TILT_DOWN),
        "imu_tilt_roll"       => return Some(IMU_TILT_ROLL),
        "imu_tilt_left"       => return Some(IMU_TILT_LEFT),
        "imu_tilt_right"      => return Some(IMU_TILT_RIGHT),
        _ => {}
    }
    // Gamepad pins. Auto falls back to Xbox; Kbm has no gamepad equivalents,
    // and a Steam Controller's face is an Xbox one.
    let skin = match skin {
        Skin::Auto | Skin::Kbm | Skin::Steam => Skin::Xbox,
        s => s,
    };
    match (skin, pin_id) {
        // Xbox
        (Skin::Xbox, "btn_south")     => Some(XB_A),
        (Skin::Xbox, "btn_east")      => Some(XB_B),
        (Skin::Xbox, "btn_west")      => Some(XB_X),
        (Skin::Xbox, "btn_north")     => Some(XB_Y),
        (Skin::Xbox, "btn_lb")        => Some(XB_LB),
        (Skin::Xbox, "btn_rb")        => Some(XB_RB),
        (Skin::Xbox, "btn_lt_dig")    => Some(XB_LT),
        (Skin::Xbox, "btn_rt_dig")    => Some(XB_RT),
        (Skin::Xbox, "left_trigger")  => Some(XB_LT),
        (Skin::Xbox, "right_trigger") => Some(XB_RT),
        (Skin::Xbox, "btn_start")     => Some(XB_START),
        (Skin::Xbox, "btn_back")      => Some(XB_BACK),
        (Skin::Xbox, "btn_guide")     => Some(XB_LOGO),
        (Skin::Xbox, "btn_capture")   => Some(XB_SHARE),
        (Skin::Xbox, "dpad_up")       => Some(XB_DPAD_U),
        (Skin::Xbox, "dpad_down")     => Some(XB_DPAD_D),
        (Skin::Xbox, "dpad_left")     => Some(XB_DPAD_L),
        (Skin::Xbox, "dpad_right")    => Some(XB_DPAD_R),
        (Skin::Xbox, "dpad")              => Some(XB_DPAD),
        (Skin::Xbox, "dpad_horizontal")   => Some(XB_DPAD_H),
        (Skin::Xbox, "dpad_vertical")     => Some(XB_DPAD_V),
        (Skin::Xbox, "left_stick_horizontal") => Some(XB_LSTICK_H),
        (Skin::Xbox, "left_stick_vertical")   => Some(XB_LSTICK_V),
        (Skin::Xbox, "btn_ls")        => Some(XB_LS),
        (Skin::Xbox, "btn_rs")        => Some(XB_RS),
        (Skin::Xbox, "left_stick")    => Some(XB_LSTICK),
        (Skin::Xbox, "right_stick")   => Some(XB_RSTICK),
        (Skin::Xbox, "left_stick_up")     => Some(XB_LSTICK_U),
        (Skin::Xbox, "left_stick_down")   => Some(XB_LSTICK_D),
        (Skin::Xbox, "left_stick_left")   => Some(XB_LSTICK_L),
        (Skin::Xbox, "left_stick_right")  => Some(XB_LSTICK_R),
        (Skin::Xbox, "right_stick_up")    => Some(XB_RSTICK_U),
        (Skin::Xbox, "right_stick_down")  => Some(XB_RSTICK_D),
        (Skin::Xbox, "right_stick_left")  => Some(XB_RSTICK_L),
        (Skin::Xbox, "right_stick_right") => Some(XB_RSTICK_R),
        // PlayStation
        (Skin::Playstation, "btn_south")     => Some(PS_CROSS),
        (Skin::Playstation, "btn_east")      => Some(PS_CIRCLE),
        (Skin::Playstation, "btn_west")      => Some(PS_SQUARE),
        (Skin::Playstation, "btn_north")     => Some(PS_TRIANGLE),
        (Skin::Playstation, "btn_lb")        => Some(PS_L1),
        (Skin::Playstation, "btn_rb")        => Some(PS_R1),
        (Skin::Playstation, "btn_lt_dig")    => Some(PS_L2),
        (Skin::Playstation, "btn_rt_dig")    => Some(PS_R2),
        (Skin::Playstation, "left_trigger")  => Some(PS_L2),
        (Skin::Playstation, "right_trigger") => Some(PS_R2),
        (Skin::Playstation, "btn_ls")        => Some(PS_L3),
        (Skin::Playstation, "btn_rs")        => Some(PS_R3),
        (Skin::Playstation, "btn_start")     => Some(PS_OPTIONS),
        (Skin::Playstation, "btn_back")      => Some(PS_SHARE),
        (Skin::Playstation, "btn_mute")      => Some(PS_MUTE),
        (Skin::Playstation, "btn_guide")     => Some(PS_LOGO),
        // The plain "touchpad click" pin shows the swipe_down icon — it has
        // the clearest "tap" affordance among the available touchpad assets.
        (Skin::Playstation, "btn_touchpad")  => Some(PS_TP_CLICK),
        (Skin::Playstation, "touchpad_any")  => Some(PS_TP_CLICK),
        // touchpad_swipe_down is rendered separately as a click overlay
        // via click_overlay_svg, not via pin_svg.
        // Touch-zone synthetic pins. Click required for any of these to fire
        // (see derive_touchpad_zones in eval.rs).
        (Skin::Playstation, "touchpad_left")   => Some(PS_TP_LEFT),
        (Skin::Playstation, "touchpad_center") => Some(PS_TP_CENTER),
        (Skin::Playstation, "touchpad_right")  => Some(PS_TP_RIGHT),
        (Skin::Playstation, "touch_left")      => Some(PS_TP_LEFT),
        (Skin::Playstation, "touch_center")    => Some(PS_TP_CENTER),
        (Skin::Playstation, "touch_right")     => Some(PS_TP_RIGHT),
        (Skin::Playstation, "dpad_up")       => Some(PS_DPAD_U),
        (Skin::Playstation, "dpad_down")     => Some(PS_DPAD_D),
        (Skin::Playstation, "dpad_left")     => Some(PS_DPAD_L),
        (Skin::Playstation, "dpad_right")    => Some(PS_DPAD_R),
        (Skin::Playstation, "dpad")              => Some(PS_DPAD),
        (Skin::Playstation, "dpad_horizontal")   => Some(PS_DPAD_H),
        (Skin::Playstation, "dpad_vertical")     => Some(PS_DPAD_V),
        (Skin::Playstation, "left_stick_horizontal") => Some(PS_LSTICK_H),
        (Skin::Playstation, "left_stick_vertical")   => Some(PS_LSTICK_V),
        (Skin::Playstation, "left_stick")    => Some(PS_LSTICK),
        (Skin::Playstation, "right_stick")   => Some(PS_RSTICK),
        (Skin::Playstation, "left_stick_up")     => Some(PS_LSTICK_U),
        (Skin::Playstation, "left_stick_down")   => Some(PS_LSTICK_D),
        (Skin::Playstation, "left_stick_left")   => Some(PS_LSTICK_L),
        (Skin::Playstation, "left_stick_right")  => Some(PS_LSTICK_R),
        (Skin::Playstation, "right_stick_up")    => Some(PS_RSTICK_U),
        (Skin::Playstation, "right_stick_down")  => Some(PS_RSTICK_D),
        (Skin::Playstation, "right_stick_left")  => Some(PS_RSTICK_L),
        (Skin::Playstation, "right_stick_right") => Some(PS_RSTICK_R),
        // Switch Pro (Nintendo layout: south position = B, east = A).
        (Skin::SwitchPro, "btn_south")     => Some(SW_B),
        (Skin::SwitchPro, "btn_east")      => Some(SW_A),
        (Skin::SwitchPro, "btn_west")      => Some(SW_Y),
        (Skin::SwitchPro, "btn_north")     => Some(SW_X),
        (Skin::SwitchPro, "btn_lb")        => Some(SW_L),
        (Skin::SwitchPro, "btn_rb")        => Some(SW_R),
        (Skin::SwitchPro, "btn_lt_dig")    => Some(SW_ZL),
        (Skin::SwitchPro, "btn_rt_dig")    => Some(SW_ZR),
        (Skin::SwitchPro, "left_trigger")  => Some(SW_ZL),
        (Skin::SwitchPro, "right_trigger") => Some(SW_ZR),
        (Skin::SwitchPro, "btn_start")     => Some(SW_PLUS),
        (Skin::SwitchPro, "btn_back")      => Some(SW_MINUS),
        (Skin::SwitchPro, "btn_guide")     => Some(SW_HOME),
        (Skin::SwitchPro, "btn_capture")   => Some(SW_SYNC),
        (Skin::SwitchPro, "dpad_up")       => Some(SW_DPAD_U),
        (Skin::SwitchPro, "dpad_down")     => Some(SW_DPAD_D),
        (Skin::SwitchPro, "dpad_left")     => Some(SW_DPAD_L),
        (Skin::SwitchPro, "dpad_right")    => Some(SW_DPAD_R),
        (Skin::SwitchPro, "dpad")              => Some(SW_DPAD),
        (Skin::SwitchPro, "dpad_horizontal")   => Some(SW_DPAD_H),
        (Skin::SwitchPro, "dpad_vertical")     => Some(SW_DPAD_V),
        (Skin::SwitchPro, "left_stick_horizontal") => Some(SW_LSTICK_H),
        (Skin::SwitchPro, "left_stick_vertical")   => Some(SW_LSTICK_V),
        (Skin::SwitchPro, "btn_ls")        => Some(SW_LS),
        (Skin::SwitchPro, "btn_rs")        => Some(SW_RS),
        (Skin::SwitchPro, "left_stick")    => Some(SW_LSTICK),
        (Skin::SwitchPro, "right_stick")   => Some(SW_RSTICK),
        (Skin::SwitchPro, "left_stick_up")     => Some(SW_LSTICK_U),
        (Skin::SwitchPro, "left_stick_down")   => Some(SW_LSTICK_D),
        (Skin::SwitchPro, "left_stick_left")   => Some(SW_LSTICK_L),
        (Skin::SwitchPro, "left_stick_right")  => Some(SW_LSTICK_R),
        (Skin::SwitchPro, "right_stick_up")    => Some(SW_RSTICK_U),
        (Skin::SwitchPro, "right_stick_down")  => Some(SW_RSTICK_D),
        (Skin::SwitchPro, "right_stick_left")  => Some(SW_RSTICK_L),
        (Skin::SwitchPro, "right_stick_right") => Some(SW_RSTICK_R),
        _ => None,
    }
}

/// SVG bytes for a gamepad-input glyph under `skin`, falling back to the FIRST
/// family that defines it when `skin` doesn't — so a family-specific control
/// (a PlayStation touchpad/mute, a Switch capture) keeps its NATIVE style even
/// under a pad whose set lacks it. Powers the dynamic `gp:<pin>` icon category.
pub fn gp_pin_svg(skin: Skin, pin_id: &str) -> Option<&'static [u8]> {
    if let Some(b) = pin_svg(skin, pin_id) {
        return Some(b);
    }
    for s in [Skin::Playstation, Skin::SwitchPro, Skin::Xbox] {
        if s == skin {
            continue;
        }
        if let Some(b) = pin_svg(s, pin_id) {
            return Some(b);
        }
    }
    None
}

/// Curated pins offered in the icon picker's "Gamepad inputs" category, as
/// `(pin_id, human label)`. Each renders via [`gp_pin_svg`] in the CURRENT pad's
/// style (see `macro_icons::current_gp_skin`) and is stored as the key
/// `gp:<pin_id>`, so already-placed icons restyle when the connected pad changes.
pub const GAMEPAD_INPUT_PINS: &[(&str, &str)] = &[
    // Face buttons
    ("btn_south", "South button"),
    ("btn_east", "East button"),
    ("btn_west", "West button"),
    ("btn_north", "North button"),
    // D-pad
    ("dpad", "D-pad"),
    ("dpad_up", "D-pad up"),
    ("dpad_down", "D-pad down"),
    ("dpad_left", "D-pad left"),
    ("dpad_right", "D-pad right"),
    // Shoulders + triggers
    ("btn_lb", "Left bumper"),
    ("btn_rb", "Right bumper"),
    ("left_trigger", "Left trigger"),
    ("right_trigger", "Right trigger"),
    // Sticks
    ("left_stick", "Left stick"),
    ("right_stick", "Right stick"),
    ("btn_ls", "Left stick (click)"),
    ("btn_rs", "Right stick (click)"),
    // Menu / system
    ("btn_start", "Start / Options / +"),
    ("btn_back", "Back / Share / −"),
    ("btn_guide", "Guide / Home"),
    ("btn_capture", "Capture / Share"),
    ("btn_mute", "Mute"),
    // Touchpad (family-agnostic glyphs — always render in their native PS style)
    ("btn_touchpad", "Touchpad (click)"),
    ("tz_touch", "Touchpad (touch)"),
    ("touch_swipe_x", "Touchpad swipe X"),
    ("touch_swipe_y", "Touchpad swipe Y"),
    ("tz_swipe_up", "Touchpad swipe up"),
    ("tz_swipe_down", "Touchpad swipe down"),
    ("tz_swipe_left", "Touchpad swipe left"),
    ("tz_swipe_right", "Touchpad swipe right"),
    ("touchpad_left", "Touchpad left"),
    ("touchpad_right", "Touchpad right"),
    // Rear paddles
    ("btn_paddle_l1", "Left paddle"),
    ("btn_paddle_r1", "Right paddle"),
];

/// Short label cut out of a glyph that stands for more than one control under
/// `skin`, or `None`. The generic paddle glyph serves both paddle rows on a side
/// (SDL exposes no vendor names for them; a Steam Controller's own glyphs carry
/// theirs); a Steam Controller's two trackpads share the touchpad's, and a
/// touchpad's two touch points share the press glyph.
pub fn extra_button_label(skin: Skin, pin_id: &str) -> Option<&'static str> {
    match (skin, pin_id) {
        // Their own glyphs say L4/R5 and SL/SR already.
        (Skin::Steam | Skin::SwitchPro, "btn_paddle_l1" | "btn_paddle_r1" | "btn_paddle_l2" | "btn_paddle_r2") => None,
        (_, "btn_paddle_l1") => Some("PL1"),
        (_, "btn_paddle_r1") => Some("PR1"),
        (_, "btn_paddle_l2") => Some("PL2"),
        (_, "btn_paddle_r2") => Some("PR2"),
        (Skin::Steam, "btn_touchpad" | "touch1_active" | "touch1_pressure") => Some("L"),
        (Skin::Steam, "btn_touchpad2" | "btn_misc2" | "touch2_active" | "touch2_pressure") => Some("R"),
        (_, "touch1_pressure") => Some("1"),
        (_, "touch2_pressure") => Some("2"),
        _ => None,
    }
}

/// Which pad a swipe gesture belongs to, as a letter: a Steam Controller's two
/// pads share the swipe glyphs just as they share its touch and click ones. Its
/// click and touch glyphs take a label CUT OUT of them (see
/// [`extra_button_label`]), but an arrowed swipe glyph has no clear space to cut
/// into — the arrow shaft is where the letter would go — so the side is painted
/// over it instead, by [`paint_pad_side_badge`].
///
/// Likewise which Joy-Con a rail button is on: both halves have an SL and an
/// SR, and the lettering fills the glyph.
pub fn pin_pad_side(skin: Skin, pin_id: &str) -> Option<&'static str> {
    if skin == Skin::SwitchPro {
        match pin_id {
            "btn_paddle_l1" | "btn_paddle_l2" => return Some("L"),
            "btn_paddle_r1" | "btn_paddle_r2" => return Some("R"),
            _ => {}
        }
    }
    let field = flexinput_core::touchzones::swipe_field(pin_id)?;
    (skin == Skin::Steam).then(|| if field == 0 { "L" } else { "R" })
}

/// Paint a chip's pad-side badge in its top-left corner — a dark disc carrying
/// `L` or `R`, as a Virtual Menu target's badge does — so a card naming one of a
/// two-pad controller's swipes says WHICH pad, not just that a pad was swiped.
/// No-op for any other pin, so every chip drawer can ask unconditionally. The
/// top-left is free: the top-right takes a menu badge, the bottom-right a timed
/// output's duration.
pub fn paint_pad_side_badge(
    painter: &egui::Painter,
    icon: egui::Rect,
    skin: Skin,
    pin_id: &str,
    tint: egui::Color32,
) {
    let Some(side) = pin_pad_side(skin, pin_id) else { return };
    let r = (icon.width().min(icon.height()) * 0.23).clamp(5.0, 10.0);
    let c = egui::pos2(icon.left() + r, icon.top() + r);
    painter.circle(c, r, egui::Color32::from_black_alpha(225),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(150)));
    painter.text(c, egui::Align2::CENTER_CENTER, side,
        egui::FontId::proportional(r * 1.3), tint);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steam_controller_wears_its_own_glyphs() {
        assert_eq!(pin_svg(Skin::Steam, "btn_misc5"), Some(SC_GRIP_L));
        assert_eq!(pin_svg(Skin::Steam, "btn_misc4"), Some(SC_CAP_R));
        assert_eq!(pin_svg(Skin::Steam, "btn_misc1"), Some(SC_AUX));
        // Read off its own reports the same controls answer to their own names,
        // and wear the same glyphs.
        assert_eq!(pin_svg(Skin::Steam, "btn_grip_l"), Some(SC_GRIP_L));
        assert_eq!(pin_svg(Skin::Steam, "btn_cap_r"), Some(SC_CAP_R));
        assert_eq!(pin_svg(Skin::Steam, "btn_capture"), Some(SC_AUX), "the three-dot button");
        assert_eq!(pin_svg(Skin::Steam, "btn_touchpad2"), pin_svg(Skin::Steam, "btn_touchpad"),
            "two pads, one glyph");
        assert_eq!(pin_svg(Skin::Steam, "btn_guide"), Some(SC_HOME),
            "its home button is no Xbox logo: the pad prints its own");
        assert_ne!(pin_svg(Skin::Steam, "btn_guide"), pin_svg(Skin::Xbox, "btn_guide"));
        assert_eq!(pin_svg(Skin::Steam, "btn_back"), Some(SC_VIEW), "View is Select");
        assert_eq!(pin_svg(Skin::Steam, "btn_start"), Some(SC_MENU), "Menu is Start");
        assert_eq!(pin_svg(Skin::Steam, "btn_paddle_l2"), Some(SC_L5));
        assert_eq!(pin_svg(Skin::Steam, "btn_misc2"), pin_svg(Skin::Steam, "btn_touchpad"), "two pads, one glyph");
        assert_eq!(pin_svg(Skin::Steam, "btn_south"), pin_svg(Skin::Xbox, "btn_south"), "its face is an Xbox one");
        // Its swipes are drawn on the square pad they happen on — the PS4's
        // touchpad shape, not PS5's wide bar — and say which pad they happened
        // on, since the arrow itself is the same either way.
        assert_eq!(pin_svg(Skin::Steam, "touch1_swipe_left"), Some(PS4_SWIPE_LEFT));
        assert_eq!(pin_svg(Skin::Steam, "touch2_swipe_y"), Some(PS4_SWIPE_V));
        assert_eq!(pin_svg(Skin::Steam, "touch_swipe_x"), Some(PS4_SWIPE_H), "its swipe OUT wears it too");
        assert_ne!(pin_svg(Skin::Steam, "touch1_swipe_up"), Some(PS_TP_SWIPE_UP), "no PS5 bar on this pad");
        assert_eq!(pin_pad_side(Skin::Steam, "touch1_swipe_up"), Some("L"));
        assert_eq!(pin_pad_side(Skin::Steam, "touch2_swipe_x"), Some("R"));
        assert_eq!(pin_pad_side(Skin::Xbox, "touch1_swipe_up"), None, "no other pad has these pins");
        assert_eq!(extra_button_label(Skin::Steam, "touch1_swipe_up"), None,
            "an arrowed glyph keeps no room for a cut-out letter");
        assert_eq!(pin_svg(Skin::Xbox, "btn_misc5"), None, "another pad's Misc 5 is no grip");
        assert_eq!(extra_button_label(Skin::Steam, "btn_touchpad"), Some("L"));
        assert_eq!(extra_button_label(Skin::Steam, "btn_touchpad2"), Some("R"));
        assert_eq!(extra_button_label(Skin::Steam, "touch2_pressure"), Some("R"));
        assert_eq!(extra_button_label(Skin::Steam, "btn_paddle_l1"), None, "its own glyph says L4");
        assert_eq!(extra_button_label(Skin::Xbox, "btn_paddle_l1"), Some("PL1"));
        assert_eq!(Skin::from_str(Skin::Steam.as_str()), Skin::Steam);
    }

    /// A label is cut out of the glyph: where it is, the glyph is clear.
    #[test]
    fn a_label_is_cut_out_of_its_glyph() {
        let svg = std::str::from_utf8(GEN_PADDLE_L).unwrap();
        let solid = |img: &egui::ColorImage| img.pixels.iter().filter(|p| p.a() > 128).count();
        let plain = crate::canvas::viewer::rasterize_svg_recolored(svg, 48, 48, "override", egui::Color32::TRANSPARENT).unwrap();
        let cut = crate::canvas::viewer::rasterize_svg_cutout(svg, 48, 48, "PL1").unwrap();
        assert!(solid(&cut) + 40 < solid(&plain), "{} vs {}", solid(&cut), solid(&plain));
    }

    #[test]
    fn gp_pin_svg_native_fallback_and_coverage() {
        // A pin the current skin DOES define resolves to that skin's glyph.
        assert_eq!(gp_pin_svg(Skin::Xbox, "btn_south"), pin_svg(Skin::Xbox, "btn_south"));

        // `btn_capture` exists for Xbox + Switch but NOT PlayStation → a PS pad
        // still gets a glyph, in the native style of a family that defines it.
        assert!(pin_svg(Skin::Playstation, "btn_capture").is_none());
        let fb = gp_pin_svg(Skin::Playstation, "btn_capture");
        assert!(fb.is_some(), "capture falls back to a family that has it");
        assert_eq!(fb, pin_svg(Skin::SwitchPro, "btn_capture"));

        // Family-agnostic control (mute) keeps its intended (PS) style under any pad.
        assert_eq!(gp_pin_svg(Skin::SwitchPro, "btn_mute"), pin_svg(Skin::Xbox, "btn_mute"));

        // Every curated picker pin resolves under every family (no dead entries).
        for (pin, _) in GAMEPAD_INPUT_PINS {
            for skin in [Skin::Xbox, Skin::Playstation, Skin::SwitchPro, Skin::Steam] {
                assert!(gp_pin_svg(skin, pin).is_some(), "{pin} unresolved under {skin:?}");
            }
        }
    }
}

#[cfg(test)]
mod prefix_tests {
    use super::*;

    /// Every device-id prefix a backend produces, with a real example.
    ///
    /// ⭐ **Add a backend, add a line here.** This list is the thing that was
    /// missing both times this broke.
    const IDS: &[(&str, &str)] = &[
        ("gilrs:xinput:0", "xinput"),
        ("gilrs:switch_pro:1", "switch_pro"),
        ("sdl:generic:2", "generic"),
        ("sdl:joycon_r:98b6e9a1b2c3", "joycon_r"),
        ("jc2:joycon2_l:da2d160f0169", "joycon2_l"),
        ("btc:switch_pro:da2d160f0169", "switch_pro"),
    ];

    /// ⛔ A device id whose prefix nothing recognises loses its CAPABILITIES,
    /// not just its icon.
    ///
    /// ⭐ This has now gone wrong twice, with the same shape both times: a
    /// backend was added, its prefix was not, and `phys_pad_slug` returned
    /// `None`. Downstream that means the conservative fallback — no gyro, no
    /// orientation, no calibration — so the pad appears, produces input, and
    /// simply has no settings, which reads as a half-finished feature rather
    /// than a missing string. First `sdl:`, then `btc:`.
    #[test]
    fn every_backend_prefix_resolves_to_its_slug() {
        for (id, want) in IDS {
            assert_eq!(
                phys_pad_slug(id),
                Some(*want),
                "{id} did not resolve — a pad on this backend gets no gyro, no                  orientation and no calibration, silently",
            );
        }
    }

    /// …and the same ids must count as physical pads.
    ///
    /// The two lists are maintained separately and drifted apart before, which
    /// is why they are checked against one set of examples.
    #[test]
    fn every_backend_prefix_counts_as_a_physical_pad() {
        for (id, _) in IDS {
            assert!(
                crate::app::graph::is_physical_pad_id(id),
                "{id} is not treated as a physical pad — it cannot be masked,                  rumbled, or routed like one",
            );
        }
    }

    /// A virtual pad must NOT be mistaken for a physical one.
    #[test]
    fn virtual_and_unknown_ids_are_not_physical_pads() {
        for id in ["vgamepad:xbox360:v0", "midi_in:0", "nonsense"] {
            assert_eq!(phys_pad_slug(id), None, "{id} resolved to a pad slug");
        }
    }

    /// Either generation's halves are halves, wear the Switch glyphs and their
    /// own side's silhouette; joycond's pair is one Nintendo pad. ZL/ZR bridge
    /// like a Switch Pro's on the original ones.
    #[test]
    fn joycons_are_switch_halves_of_either_generation() {
        let icon = |id: &str| match device_node_icon_for_id(id) {
            Some(NodeIconSpec::Single(svg)) => svg,
            None => panic!("{id} has no icon"),
        };
        for (id, half, svg) in [("sdl:joycon_l:aa", JoyConHalf::Left, DEV_JOYCON_L),
                                ("sdl:joycon_r:bb", JoyConHalf::Right, DEV_JOYCON_R),
                                ("jc2:joycon2_l:cc", JoyConHalf::Left, DEV_JOYCON_L),
                                ("jc2:joycon2_r:dd", JoyConHalf::Right, DEV_JOYCON_R)] {
            assert_eq!(joycon_half(id), Some(half), "{id}");
            assert_eq!(skin_from_device_id(id), Skin::SwitchPro, "{id}");
            assert!(icon(id) == svg, "{id} wears its side's silhouette");
        }
        assert_eq!(joycon_half("gilrs:switch_pro:0"), None);
        assert_eq!(joycon_half("sdl:joycon_pair:i0"), None, "a pair is a whole pad");
        assert_eq!(skin_from_device_id("sdl:joycon_pair:i0"), Skin::SwitchPro);
        assert!(icon("sdl:joycon_pair:i0") == DEV_JOYCON_PAIR);
        assert!(icon("gilrs:switch_pro:0") == DEV_SWITCH);
        use flexinput_devices::ControllerKind as K;
        assert!(device_card_svg(K::JoyCon2L, None, None) == DEV_JOYCON_L);
        assert!(device_card_svg(K::JoyConR, None, None) == DEV_JOYCON_R);
        assert!(device_card_svg(K::JoyConPair, None, None) == DEV_JOYCON_PAIR);
        for slug in ["switch_pro", "joycon_l", "joycon_r", "joycon_pair"] {
            assert!(slug_has_digital_triggers(slug), "{slug}");
        }
        assert!(!slug_has_digital_triggers("dualsense"));
    }

    /// Under the Switch skin the paddle pins are SL/SR, with which half in the
    /// corner badge rather than cut into the lettering; elsewhere they stay the
    /// generic paddle with its PL1… label.
    #[test]
    fn a_joycons_rail_buttons_are_sl_and_sr() {
        for (pin, svg, side) in [("btn_paddle_l1", SW_SL, "L"), ("btn_paddle_l2", SW_SR, "L"),
                                 ("btn_paddle_r1", SW_SL, "R"), ("btn_paddle_r2", SW_SR, "R")] {
            assert_eq!(pin_svg(Skin::SwitchPro, pin), Some(svg), "{pin}");
            assert_eq!(pin_pad_side(Skin::SwitchPro, pin), Some(side), "{pin}");
            assert_eq!(extra_button_label(Skin::SwitchPro, pin), None, "{pin}: SL/SR is on the glyph");
            assert_eq!(pin_svg(Skin::Xbox, pin), pin_svg(Skin::Playstation, pin), "{pin}");
            assert_ne!(pin_svg(Skin::Xbox, pin), Some(svg), "{pin}");
            assert_eq!(pin_pad_side(Skin::Xbox, pin), None, "{pin}");
        }
        assert_eq!(pin_pad_side(Skin::SwitchPro, "btn_south"), None);
    }
}
