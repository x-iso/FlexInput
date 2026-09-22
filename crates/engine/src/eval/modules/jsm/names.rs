//! JSM's names, and what they are on our bus.
//!
//! Input side: JSM's `ButtonID` (`include/JoyShockMapper.h`) — Nintendo
//! positions with cardinal face buttons — and where each one reads from.
//! Output side: JSM's key / mouse / pad / action names (`nameToKey` in
//! `src/win32/PlatformDefinitions.cpp`) mapped onto the AutoMap pins our
//! keyboard-mouse and virtual-pad sinks accept.
//!
//! Names are matched case-insensitively on already-trimmed tokens. A name JSM
//! knows but we can't drive yet returns [`Out::Unsupported`] with the reason, so
//! the editor can say why instead of silently dropping the line.

/// A JSM button, in its own vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Btn {
    Up, Down, Left, Right,
    L, Zl, Minus, E, S, N, W, R, Zr, Plus, Home,
    Lsl, Lsr, Rsl, Rsr, L3, R3,
    LeanLeft, LeanRight, Mic,
    /// Left stick direction / ring.
    Lup, Ldown, Lleft, Lright, Lring,
    /// Right stick direction / ring.
    Rup, Rdown, Rleft, Rright, Rring,
    /// Motion stick direction / ring.
    Mup, Mdown, Mleft, Mright, Mring,
    /// Touchpad: any finger down, and the click (JSM's full press).
    Touch, Capture,
    /// Trigger full pull.
    Zlf, Zrf,
    /// Touch stick direction / ring.
    Tup, Tdown, Tleft, Tright, Tring,
    /// Touchpad grid cell, 1-based (`T1`…`T25`).
    T(u8),
    // ── the custom-curve fork's extra buttons ────────────────────────────────
    /// `MISC1`…`MISC6`, 1-based — SDL's generic extra buttons, which our bus has
    /// one for one.
    Misc(u8),
    /// Capacitive touch on a stick, and the "mini" shoulder buttons. The fork reads
    /// these from SDL3; our bus has no pin for any of them, so they say so.
    LTouch,
    RTouch,
    LMini,
    RMini,
}

/// Where a button reads from on the bus.
pub enum BtnSource {
    /// A Bool pin.
    Pin(&'static str),
    /// An analog trigger, with the digital button some pads report instead.
    Trigger { analog: &'static str, digital: &'static str },
    /// Full pull of an analog trigger.
    TriggerFull { analog: &'static str },
    /// A stick direction or ring — derived from the stick's x/y.
    Stick { stick: StickId, dir: Dir },
    /// Derived from the motion sensors.
    Motion { dir: Dir },
    /// Controller lean, from the motion sensors.
    Lean { right: bool },
    /// Any finger on the touchpad.
    Touch,
    /// A touchpad grid cell (1-based) or touch-stick direction.
    TouchZone { cell: Option<u8>, dir: Option<Dir> },
    /// A name JSM (or its fork) accepts that nothing on our bus reports, with the
    /// reason. It reads as never pressed, and its line says why — which beats both
    /// calling the name an error and guessing at a pin that might be something else
    /// entirely.
    Absent(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StickId { Left, Right }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Dir { Up, Down, Left, Right, Ring }

impl Btn {
    /// Every button the vocabulary has, for a list to offer.
    ///
    /// Round-tripped against `from_name`/`name` by a test, so a button added to
    /// the enum but forgotten here is caught — a list that quietly omits a
    /// button is a list people stop trusting.
    pub const ALL: &'static [Btn] = &[
        Btn::Up, Btn::Down, Btn::Left, Btn::Right,
        Btn::L, Btn::Zl, Btn::Zlf, Btn::Minus,
        Btn::E, Btn::S, Btn::N, Btn::W,
        Btn::R, Btn::Zr, Btn::Zrf, Btn::Plus, Btn::Home,
        Btn::Lsl, Btn::Lsr, Btn::Rsl, Btn::Rsr, Btn::L3, Btn::R3,
        Btn::LeanLeft, Btn::LeanRight, Btn::Mic,
        Btn::Lup, Btn::Ldown, Btn::Lleft, Btn::Lright, Btn::Lring,
        Btn::Rup, Btn::Rdown, Btn::Rleft, Btn::Rright, Btn::Rring,
        Btn::Mup, Btn::Mdown, Btn::Mleft, Btn::Mright, Btn::Mring,
        Btn::Tup, Btn::Tdown, Btn::Tleft, Btn::Tright, Btn::Tring,
        Btn::Touch, Btn::Capture,
        Btn::LTouch, Btn::RTouch, Btn::LMini, Btn::RMini,
        // The numbered families, listed rather than generated so the round-trip
        // test covers their bounds too.
        Btn::T(1), Btn::T(2), Btn::T(3), Btn::T(4), Btn::T(5),
        Btn::T(6), Btn::T(7), Btn::T(8), Btn::T(9), Btn::T(10),
        Btn::T(11), Btn::T(12), Btn::T(13), Btn::T(14), Btn::T(15),
        Btn::T(16), Btn::T(17), Btn::T(18), Btn::T(19), Btn::T(20),
        Btn::T(21), Btn::T(22), Btn::T(23), Btn::T(24), Btn::T(25),
        Btn::Misc(1), Btn::Misc(2), Btn::Misc(3),
        Btn::Misc(4), Btn::Misc(5), Btn::Misc(6),
    ];

    /// Parse a JSM button name. `None` when it isn't one.
    pub fn from_name(name: &str) -> Option<Btn> {
        use Btn::*;
        // `MISC1`…`MISC6` — the fork's names for SDL's generic extra buttons.
        if let Some(d) = name.to_ascii_uppercase().strip_prefix("MISC") {
            if let Ok(n) = d.parse::<u8>() {
                if (1..=6).contains(&n) {
                    return Some(Btn::Misc(n));
                }
            }
        }
        // Grid cells T1..T25 (T alone is not a button; TOUCH is).
        if let Some(rest) = name.strip_prefix('T').or_else(|| name.strip_prefix('t')) {
            if let Ok(n) = rest.parse::<u8>() {
                return (1..=25).contains(&n).then_some(T(n));
            }
        }
        Some(match name.to_ascii_uppercase().as_str() {
            "UP" => Up, "DOWN" => Down, "LEFT" => Left, "RIGHT" => Right,
            "L" => L, "ZL" => Zl, "-" => Minus, "E" => E, "S" => S, "N" => N, "W" => W,
            "R" => R, "ZR" => Zr, "+" => Plus, "HOME" => Home,
            "LSL" => Lsl, "LSR" => Lsr, "RSL" => Rsl, "RSR" => Rsr,
            "L3" => L3, "R3" => R3,
            "LEAN_LEFT" => LeanLeft, "LEAN_RIGHT" => LeanRight, "MIC" => Mic,
            "LUP" => Lup, "LDOWN" => Ldown, "LLEFT" => Lleft, "LRIGHT" => Lright, "LRING" => Lring,
            "RUP" => Rup, "RDOWN" => Rdown, "RLEFT" => Rleft, "RRIGHT" => Rright, "RRING" => Rring,
            "MUP" => Mup, "MDOWN" => Mdown, "MLEFT" => Mleft, "MRIGHT" => Mright, "MRING" => Mring,
            "TOUCH" => Touch, "CAPTURE" => Capture,
            "LTOUCH" => LTouch, "RTOUCH" => RTouch, "LMINI" => LMini, "RMINI" => RMini,
            "ZLF" => Zlf, "ZRF" => Zrf,
            "TUP" => Tup, "TDOWN" => Tdown, "TLEFT" => Tleft, "TRIGHT" => Tright, "TRING" => Tring,
            _ => return None,
        })
    }

    /// The name JSM knows this button by (for diagnostics).
    pub fn name(self) -> String {
        use Btn::*;
        match self {
            Up => "UP".into(), Down => "DOWN".into(), Left => "LEFT".into(), Right => "RIGHT".into(),
            L => "L".into(), Zl => "ZL".into(), Minus => "-".into(), E => "E".into(), S => "S".into(),
            N => "N".into(), W => "W".into(), R => "R".into(), Zr => "ZR".into(), Plus => "+".into(),
            Home => "HOME".into(), Lsl => "LSL".into(), Lsr => "LSR".into(), Rsl => "RSL".into(),
            Rsr => "RSR".into(), L3 => "L3".into(), R3 => "R3".into(),
            LeanLeft => "LEAN_LEFT".into(), LeanRight => "LEAN_RIGHT".into(), Mic => "MIC".into(),
            Lup => "LUP".into(), Ldown => "LDOWN".into(), Lleft => "LLEFT".into(),
            Lright => "LRIGHT".into(), Lring => "LRING".into(),
            Rup => "RUP".into(), Rdown => "RDOWN".into(), Rleft => "RLEFT".into(),
            Rright => "RRIGHT".into(), Rring => "RRING".into(),
            Mup => "MUP".into(), Mdown => "MDOWN".into(), Mleft => "MLEFT".into(),
            Mright => "MRIGHT".into(), Mring => "MRING".into(),
            Touch => "TOUCH".into(), Capture => "CAPTURE".into(),
            LTouch => "LTOUCH".into(), RTouch => "RTOUCH".into(),
            LMini => "LMINI".into(), RMini => "RMINI".into(),
            Misc(n) => format!("MISC{n}"),
            Zlf => "ZLF".into(), Zrf => "ZRF".into(),
            Tup => "TUP".into(), Tdown => "TDOWN".into(), Tleft => "TLEFT".into(),
            Tright => "TRIGHT".into(), Tring => "TRING".into(),
            T(n) => format!("T{n}"),
        }
    }

    /// Where this button reads from on the bus.
    pub fn source(self) -> BtnSource {
        use Btn::*;
        use BtnSource as S;
        match self {
            Up => S::Pin("dpad_up"), Down => S::Pin("dpad_down"),
            Left => S::Pin("dpad_left"), Right => S::Pin("dpad_right"),
            L => S::Pin("btn_lb"), R => S::Pin("btn_rb"),
            Zl => S::Trigger { analog: "left_trigger", digital: "btn_lt_dig" },
            Zr => S::Trigger { analog: "right_trigger", digital: "btn_rt_dig" },
            Zlf => S::TriggerFull { analog: "left_trigger" },
            Zrf => S::TriggerFull { analog: "right_trigger" },
            Minus => S::Pin("btn_back"), Plus => S::Pin("btn_start"), Home => S::Pin("btn_guide"),
            // JSM's CAPTURE is the touchpad click on PlayStation pads and the
            // Capture button on Switch ones; our pads expose both pins.
            Capture => S::Pin("btn_touchpad"),
            N => S::Pin("btn_north"), E => S::Pin("btn_east"), S => S::Pin("btn_south"), W => S::Pin("btn_west"),
            L3 => S::Pin("btn_ls"), R3 => S::Pin("btn_rs"), Mic => S::Pin("btn_mute"),
            // JoyCon SL/SR sit where an Elite pad has its paddles.
            Lsl => S::Pin("btn_paddle_l1"), Lsr => S::Pin("btn_paddle_l2"),
            Rsl => S::Pin("btn_paddle_r1"), Rsr => S::Pin("btn_paddle_r2"),
            Lup => S::Stick { stick: StickId::Left, dir: Dir::Up },
            Ldown => S::Stick { stick: StickId::Left, dir: Dir::Down },
            Lleft => S::Stick { stick: StickId::Left, dir: Dir::Left },
            Lright => S::Stick { stick: StickId::Left, dir: Dir::Right },
            Lring => S::Stick { stick: StickId::Left, dir: Dir::Ring },
            Rup => S::Stick { stick: StickId::Right, dir: Dir::Up },
            Rdown => S::Stick { stick: StickId::Right, dir: Dir::Down },
            Rleft => S::Stick { stick: StickId::Right, dir: Dir::Left },
            Rright => S::Stick { stick: StickId::Right, dir: Dir::Right },
            Rring => S::Stick { stick: StickId::Right, dir: Dir::Ring },
            Mup => S::Motion { dir: Dir::Up }, Mdown => S::Motion { dir: Dir::Down },
            Mleft => S::Motion { dir: Dir::Left }, Mright => S::Motion { dir: Dir::Right },
            Mring => S::Motion { dir: Dir::Ring },
            LeanLeft => S::Lean { right: false }, LeanRight => S::Lean { right: true },
            Touch => S::Touch,
            Misc(n) => S::Pin(match n {
                1 => "btn_misc1",
                2 => "btn_misc2",
                3 => "btn_misc3",
                4 => "btn_misc4",
                5 => "btn_misc5",
                _ => "btn_misc6",
            }),
            // The fork reads these off SDL3's extended button set. Nothing on our bus
            // carries them, and guessing at a paddle or a misc slot would be worse
            // than saying so: on one pad `LMINI` is a rail button, on another it is
            // something else entirely.
            LTouch | RTouch => S::Absent(
                "nothing on the bus reports capacitive touch on a stick — a pad that senses it \
                 usually surfaces it as one of the MISC buttons instead",
            ),
            LMini | RMini => S::Absent(
                "the bus has no pin for a mini shoulder button; try LSL / LSR / RSL / RSR for a \
                 rail button, or a MISC one for whatever your pad calls it",
            ),
            Tup => S::TouchZone { cell: None, dir: Some(Dir::Up) },
            Tdown => S::TouchZone { cell: None, dir: Some(Dir::Down) },
            Tleft => S::TouchZone { cell: None, dir: Some(Dir::Left) },
            Tright => S::TouchZone { cell: None, dir: Some(Dir::Right) },
            Tring => S::TouchZone { cell: None, dir: Some(Dir::Ring) },
            T(n) => S::TouchZone { cell: Some(n), dir: None },
        }
    }
}

/// What a bound name does when its event fires.
#[derive(Clone, PartialEq, Debug)]
pub enum Out {
    /// A bus pin held while the binding is on: a key, mouse button or pad pin.
    Pin(String),
    /// A pin pulsed rather than held (scroll notches).
    Pulse(String),
    /// Gyro control that overlaps whatever else the button does.
    Gyro(GyroAction),
    /// Recalibrate the gyro.
    Calibrate,
    /// Rumble the pad: JSM's SMALL_RUMBLE / BIG_RUMBLE / `Rhhhh`, as the
    /// (strong, weak) amplitudes JSM sends.
    Rumble { strong: f32, weak: f32 },
    /// A console command in quotes, run when the event fires.
    Command(String),
    /// Switch to another config — JSM loads a file, we switch to the tab of that
    /// name. This is how a JSM config does action layers.
    Layer(String),
    /// `RESET_MAPPINGS`: back to the config as written, dropping anything a layer
    /// switch or a held chord had changed.
    Reset,
    /// A deliberate no-op (`NONE`, `DEFAULT`) — takes the slot without acting.
    None,
    /// JSM knows this name; we can't drive it yet.
    Unsupported { name: String, why: &'static str },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GyroAction { On, Off, Invert, InvertX, InvertY, Trackball, TrackballX, TrackballY }

/// Why a name JSM accepts isn't live here.
const WHY_NUMPAD: &str = "numpad keys need HID usages our keyboard sink doesn't send yet";
const WHY_MEDIA: &str = "media and volume keys are consumer-page usages our keyboard sink doesn't send yet";
const WHY_LOCKS: &str = "Scroll Lock / Num Lock / Pause / Context aren't in our keyboard sink's key table yet";

/// A note attached to a line whose name we accepted but with a caveat.
pub struct OutNote {
    pub out: Out,
    pub note: Option<String>,
}

/// Parse an output name (already stripped of modifiers and quotes handled by the
/// caller for commands). Returns the action, plus a note for the line when a
/// mapping is approximate.
///
/// Nothing is approximate today — the left/right modifiers were the only ones,
/// and they now reach the OS as themselves — so nothing sets a note. The field
/// stays because the next name we can only get close to will want it, and the
/// line already knows how to show one.
pub fn out_from_name(name: &str) -> Option<OutNote> {
    let plain = |p: &str| Some(OutNote { out: Out::Pin(p.to_string()), note: None });
    let pulse = |p: &str| Some(OutNote { out: Out::Pulse(p.to_string()), note: None });
    let unsupported = |why: &'static str| Some(OutNote {
        out: Out::Unsupported { name: name.to_string(), why },
        note: None,
    });
    let upper = name.to_ascii_uppercase();

    // Letters and digits.
    if upper.len() == 1 {
        let c = upper.chars().next().unwrap();
        if c.is_ascii_alphanumeric() {
            return plain(&format!("key_{}", c.to_ascii_lowercase()));
        }
        if let Some(p) = punctuation_pin(c) {
            return plain(p);
        }
    }
    // F1..F20 (our sink stops at F20; JSM's help lists further keys it can't send either).
    if let Some(n) = upper.strip_prefix('F').and_then(|d| d.parse::<u8>().ok()) {
        return if (1..=20).contains(&n) {
            plain(&format!("key_f{n}"))
        } else {
            unsupported("only F1-F20 reach our keyboard sink")
        };
    }
    // Numpad: N0..N9 plus the operator keys.
    if let Some(d) = upper.strip_prefix('N') {
        if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) {
            return unsupported(WHY_NUMPAD);
        }
    }

    Some(match upper.as_str() {
        "NONE" | "DEFAULT" => OutNote { out: Out::None, note: None },
        // ── Keyboard ──────────────────────────────────────────────────────────
        "ENTER" => return plain("key_enter"),
        "ESC" => return plain("key_escape"),
        "SPACE" => return plain("key_space"),
        "BACKSPACE" => return plain("key_backspace"),
        "TAB" => return plain("key_tab"),
        "CAPS_LOCK" => return plain("key_capslock"),
        "PAGEUP" => return plain("key_pageup"),
        "PAGEDOWN" => return plain("key_pagedown"),
        "HOME" => return plain("key_home"),
        "END" => return plain("key_end"),
        "INSERT" => return plain("key_insert"),
        "DELETE" => return plain("key_delete"),
        "SCREENSHOT" => return plain("key_printscreen"),
        "UP" => return plain("key_arrowup"),
        "DOWN" => return plain("key_arrowdown"),
        "LEFT" => return plain("key_arrowleft"),
        "RIGHT" => return plain("key_arrowright"),
        "SHIFT" => return plain("key_shift"),
        "CONTROL" => return plain("key_ctrl"),
        "ALT" => return plain("key_alt"),
        // Sided modifiers reach the OS as themselves: the HID keyboard sets the
        // half of the modifier byte that was asked for, and the SendInput
        // fallback has its own sided virtual keys. (They used to be flattened
        // onto the generic ones, with a note on the line saying so.)
        "LSHIFT" => return plain("key_lshift"),
        "RSHIFT" => return plain("key_rshift"),
        "LCONTROL" => return plain("key_lctrl"),
        "RCONTROL" => return plain("key_rctrl"),
        "LALT" => return plain("key_lalt"),
        "RALT" => return plain("key_ralt"),
        "LWINDOWS" => return plain("key_lwin"),
        "RWINDOWS" => return plain("key_rwin"),
        "SCROLL_LOCK" | "NUM_LOCK" | "PAUSE" | "CONTEXT" => return unsupported(WHY_LOCKS),
        "ADD" | "SUBTRACT" | "SUBSTRACT" | "DIVIDE" | "MULTIPLY" | "DECIMAL" => return unsupported(WHY_NUMPAD),
        "VOLUME_UP" | "VOLUME_DOWN" | "MUTE" | "NEXT_TRACK" | "PREV_TRACK"
            | "STOP_TRACK" | "PLAY_PAUSE" => return unsupported(WHY_MEDIA),
        // ── Mouse ─────────────────────────────────────────────────────────────
        "LMOUSE" => return plain("mouse_left"),
        "RMOUSE" => return plain("mouse_right"),
        "MMOUSE" => return plain("mouse_middle"),
        "BMOUSE" => return plain("mouse_back"),
        "FMOUSE" => return plain("mouse_forward"),
        "SCROLLUP" => return pulse("scroll_up"),
        "SCROLLDOWN" => return pulse("scroll_down"),
        // ── Gyro control + calibration ────────────────────────────────────────
        "GYRO_ON" => OutNote { out: Out::Gyro(GyroAction::On), note: None },
        "GYRO_OFF" => OutNote { out: Out::Gyro(GyroAction::Off), note: None },
        "GYRO_INVERT" => OutNote { out: Out::Gyro(GyroAction::Invert), note: None },
        "GYRO_INV_X" => OutNote { out: Out::Gyro(GyroAction::InvertX), note: None },
        "GYRO_INV_Y" => OutNote { out: Out::Gyro(GyroAction::InvertY), note: None },
        "GYRO_TRACKBALL" => OutNote { out: Out::Gyro(GyroAction::Trackball), note: None },
        "GYRO_TRACK_X" => OutNote { out: Out::Gyro(GyroAction::TrackballX), note: None },
        "GYRO_TRACK_Y" => OutNote { out: Out::Gyro(GyroAction::TrackballY), note: None },
        "CALIBRATE" => OutNote { out: Out::Calibrate, note: None },
        // ── Rumble ────────────────────────────────────────────────────────────
        // JSM's own amplitudes: SMALL_RUMBLE = R0080, BIG_RUMBLE = RFFFF, where
        // the high byte is the strong motor and the low byte the weak one.
        "SMALL_RUMBLE" => OutNote { out: rumble_from_hex(0x0080), note: None },
        "BIG_RUMBLE" => OutNote { out: rumble_from_hex(0xFFFF), note: None },
        _ => {
            if let Some(pad) = pad_pin(&upper) {
                return plain(pad);
            }
            if let Some(hex) = upper.strip_prefix('R') {
                if hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    if let Ok(raw) = u16::from_str_radix(hex, 16) {
                        return Some(OutNote { out: rumble_from_hex(raw), note: None });
                    }
                }
            }
            return None;
        }
    })
}

fn rumble_from_hex(raw: u16) -> Out {
    Out::Rumble {
        strong: (raw >> 8) as f32 / 255.0,
        weak: (raw & 0xFF) as f32 / 255.0,
    }
}

fn punctuation_pin(c: char) -> Option<&'static str> {
    Some(match c {
        ';' => "key_semicolon",
        '\'' => "key_quote",
        ',' => "key_comma",
        '.' => "key_period",
        '/' => "key_slash",
        '\\' => "key_backslash",
        '[' => "key_openbracket",
        ']' => "key_closebracket",
        '+' => "key_plus",
        '-' => "key_minus",
        '`' => "key_backtick",
        _ => return None,
    })
}

/// Is this pin one a virtual pad has to be wired up to receive? A config full of
/// `X_*` bindings does nothing at all until one is, which is worth saying on the
/// line rather than leaving the user to wonder.
pub fn is_pad_pin(pin: &str) -> bool {
    pin.starts_with("btn_")
        || pin.starts_with("dpad")
        || pin.ends_with("_trigger")
        || pin.ends_with("_stick")
}

/// Virtual-pad output names. JSM's `PS_*` names are aliases of the `X_*` ones,
/// and both land on our canonical pad pins — the user wires whichever virtual
/// pad they want downstream.
fn pad_pin(upper: &str) -> Option<&'static str> {
    Some(match upper {
        "X_A" | "PS_CROSS" => "btn_south",
        "X_B" | "PS_CIRCLE" => "btn_east",
        "X_X" | "PS_SQUARE" => "btn_west",
        "X_Y" | "PS_TRIANGLE" => "btn_north",
        "X_LB" | "PS_L1" => "btn_lb",
        "X_RB" | "PS_R1" => "btn_rb",
        "X_LS" | "PS_L3" => "btn_ls",
        "X_RS" | "PS_R3" => "btn_rs",
        "X_BACK" | "PS_SHARE" => "btn_back",
        "X_START" | "PS_OPTIONS" => "btn_start",
        "X_GUIDE" | "PS_HOME" => "btn_guide",
        "PS_PAD_CLICK" => "btn_touchpad",
        "X_UP" | "PS_UP" => "dpad_up",
        "X_DOWN" | "PS_DOWN" => "dpad_down",
        "X_LEFT" | "PS_LEFT" => "dpad_left",
        "X_RIGHT" | "PS_RIGHT" => "dpad_right",
        "X_LT" | "PS_L2" => "left_trigger",
        "X_RT" | "PS_R2" => "right_trigger",
        _ => return None,
    })
}
