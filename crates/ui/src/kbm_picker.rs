//! Gamepad-navigable virtual keyboard / mouse picker.
//!
//! Opened from a Remapper's "Special" slot (gamepad South) during the output
//! Learn phase. Renders a keyboard-shaped grid of KBM icons in a modal window;
//! the user navigates cells with the left stick / D-pad, presses South to append
//! the focused pin to the output chord (`draft_output`), North to reset that
//! chord, and East to close. This lets a controller-only user assign
//! keyboard/mouse outputs without a physical keyboard or mouse.
//!
//! The picker is driven entirely from the top-level nav driver (`app.rs`), NOT
//! from inside a pinned body — pinned remapper bodies render in a child
//! TSTransform layer where painting/relocking the egui ctx deadlocks epaint.
//!
//! ## Layout model
//!
//! Cells carry explicit `(x, y)` grid positions (in key units, 1.0 = a standard
//! key) so the picker can lay out a real keyboard shape with SEPARATE clusters:
//! the main alpha block on the left, the nav cluster (Ins/Del/Home/End/PgUp/
//! PgDn) to its upper right, the arrow keys below that in the traditional
//! inverted-T, and the mouse cluster further right. A row index isn't enough to
//! express side-by-side clusters, so navigation is spatial (nearest cell in the
//! pressed direction) rather than row/col stepping.

/// One key cell: the output pin id it appends, its grid position `(x, y)` in
/// key units (origin top-left), and its width (1.0 = a standard key). Height is
/// always one unit.
#[derive(Clone, Copy)]
pub struct KbmCell {
    pub pin: &'static str,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// When true, the cell only applies to mappings whose captured INPUT is
    /// analog (stick / trigger). The picker greys it out and ignores activation
    /// otherwise. Used by the touchpad swipe bindings.
    pub analog_only: bool,
}

const fn c(pin: &'static str, x: f32, y: f32) -> KbmCell {
    KbmCell { pin, x, y, width: 1.0, analog_only: false }
}
const fn cw(pin: &'static str, x: f32, y: f32, width: f32) -> KbmCell {
    KbmCell { pin, x, y, width, analog_only: false }
}
/// Analog-only cell (width 1).
const fn ca(pin: &'static str, x: f32, y: f32) -> KbmCell {
    KbmCell { pin, x, y, width: 1.0, analog_only: true }
}

// Cluster x-origins. The main block spans x≈0..15. The nav cluster sits just
// right of it, the mouse cluster further right again.
const NAV_X: f32 = 15.5; // Insert/Home/PgUp column start
const MOUSE_X: f32 = 19.5; // mouse cluster column start
const TOUCH_X: f32 = 23.5; // touchpad cluster column start (right of the mouse cluster)

/// The full keyboard + mouse layout as absolutely-positioned cells. Pin ids
/// match `remapper_icons::pin_svg` so every cell resolves to an SVG icon.
pub const KBM_LAYOUT: &[KbmCell] = &[
    // ── Function row (y=0) ────────────────────────────────────────────────
    c("key_escape", 0.0, 0.0),
    c("key_f1", 1.0, 0.0), c("key_f2", 2.0, 0.0), c("key_f3", 3.0, 0.0), c("key_f4", 4.0, 0.0),
    c("key_f5", 5.0, 0.0), c("key_f6", 6.0, 0.0), c("key_f7", 7.0, 0.0), c("key_f8", 8.0, 0.0),
    c("key_f9", 9.0, 0.0), c("key_f10", 10.0, 0.0), c("key_f11", 11.0, 0.0), c("key_f12", 12.0, 0.0),

    // ── Number row (y=1) ──────────────────────────────────────────────────
    c("key_backtick", 0.0, 1.0),
    c("key_num1", 1.0, 1.0), c("key_num2", 2.0, 1.0), c("key_num3", 3.0, 1.0),
    c("key_num4", 4.0, 1.0), c("key_num5", 5.0, 1.0), c("key_num6", 6.0, 1.0),
    c("key_num7", 7.0, 1.0), c("key_num8", 8.0, 1.0), c("key_num9", 9.0, 1.0),
    c("key_num0", 10.0, 1.0), c("key_minus", 11.0, 1.0), c("key_equals", 12.0, 1.0),
    cw("key_backspace", 13.0, 1.0, 2.0),

    // ── Top letter row (y=2) ──────────────────────────────────────────────
    cw("key_tab", 0.0, 2.0, 1.5),
    c("key_q", 1.5, 2.0), c("key_w", 2.5, 2.0), c("key_e", 3.5, 2.0), c("key_r", 4.5, 2.0),
    c("key_t", 5.5, 2.0), c("key_y", 6.5, 2.0), c("key_u", 7.5, 2.0), c("key_i", 8.5, 2.0),
    c("key_o", 9.5, 2.0), c("key_p", 10.5, 2.0),
    c("key_openbracket", 11.5, 2.0), c("key_closebracket", 12.5, 2.0),
    cw("key_backslash", 13.5, 2.0, 1.5),

    // ── Home row (y=3) ────────────────────────────────────────────────────
    cw("key_capslock", 0.0, 3.0, 1.75),
    c("key_a", 1.75, 3.0), c("key_s", 2.75, 3.0), c("key_d", 3.75, 3.0), c("key_f", 4.75, 3.0),
    c("key_g", 5.75, 3.0), c("key_h", 6.75, 3.0), c("key_j", 7.75, 3.0), c("key_k", 8.75, 3.0),
    c("key_l", 9.75, 3.0), c("key_semicolon", 10.75, 3.0), c("key_quote", 11.75, 3.0),
    cw("key_enter", 12.75, 3.0, 2.25),

    // ── Bottom letter row (y=4) ───────────────────────────────────────────
    cw("key_lshift", 0.0, 4.0, 2.25),
    c("key_z", 2.25, 4.0), c("key_x", 3.25, 4.0), c("key_c", 4.25, 4.0), c("key_v", 5.25, 4.0),
    c("key_b", 6.25, 4.0), c("key_n", 7.25, 4.0), c("key_m", 8.25, 4.0),
    c("key_comma", 9.25, 4.0), c("key_period", 10.25, 4.0), c("key_slash", 11.25, 4.0),
    cw("key_rshift", 12.25, 4.0, 2.75),

    // ── Modifier / space row (y=5) ────────────────────────────────────────
    cw("key_lctrl", 0.0, 5.0, 1.5), cw("key_lwin", 1.5, 5.0, 1.25),
    cw("key_lalt", 2.75, 5.0, 1.25),
    cw("key_space", 4.0, 5.0, 6.0),
    cw("key_ralt", 10.0, 5.0, 1.25), cw("key_rctrl", 11.25, 5.0, 1.5),

    // ── Nav cluster (right of the main block) ─────────────────────────────
    // System row (aligned with the function row): PrintScreen / Pause-Break,
    // mirroring where PrtSc/ScrLk/Pause sit above the nav cluster on a real
    // keyboard.
    c("key_printscreen", NAV_X, 0.0), c("key_pause", NAV_X + 1.0, 0.0),
    // Top row: Insert / Home / Page Up.
    c("key_insert", NAV_X, 1.0), c("key_home", NAV_X + 1.0, 1.0), c("key_pageup", NAV_X + 2.0, 1.0),
    // Bottom row: Delete / End / Page Down.
    c("key_delete", NAV_X, 2.0), c("key_end", NAV_X + 1.0, 2.0), c("key_pagedown", NAV_X + 2.0, 2.0),

    // ── Arrow keys (inverted-T, below the nav cluster) ────────────────────
    c("key_arrowup", NAV_X + 1.0, 4.0),
    c("key_arrowleft", NAV_X, 5.0), c("key_arrowdown", NAV_X + 1.0, 5.0), c("key_arrowright", NAV_X + 2.0, 5.0),

    // ── Mouse cluster (further right) ─────────────────────────────────────
    c("mouse_left", MOUSE_X, 1.0), c("mouse_middle", MOUSE_X + 1.0, 1.0), c("mouse_right", MOUSE_X + 2.0, 1.0),
    c("mouse_back", MOUSE_X, 2.0), c("mouse_forward", MOUSE_X + 1.0, 2.0),
    // Scroll directions arranged like a d-pad: up on top, left/down/right below.
    c("scroll_up", MOUSE_X + 1.0, 3.0),
    c("scroll_left", MOUSE_X, 4.0), c("scroll_down", MOUSE_X + 1.0, 4.0), c("scroll_right", MOUSE_X + 2.0, 4.0),

    // ── Touchpad cluster (DualSense/DualShock) ────────────────────────────
    // Row 1: the three touch zones (finger touch, no click).
    c("touch_left", TOUCH_X, 1.0), c("touch_center", TOUCH_X + 1.0, 1.0), c("touch_right", TOUCH_X + 2.0, 1.0),
    // Row 2: touchpad click + DualSense mic (mute) button.
    c("btn_touchpad", TOUCH_X, 2.0), c("btn_mute", TOUCH_X + 1.0, 2.0),
    // Row 3: analog swipes — only usable when an analog input is captured.
    ca("touch_swipe_x", TOUCH_X, 3.0), ca("touch_swipe_y", TOUCH_X + 1.0, 3.0),
];

/// An owned picker cell — the static [`KBM_LAYOUT`] plus the current patch's
/// dynamic macro ports resolve into these. `macro_meta` carries the port's
/// display entry only for macro cells (their pin ids aren't in the KBM icon
/// set, so the renderer needs the name/icon/custom-SVG alongside).
#[derive(Clone)]
pub struct PickerCell {
    pub pin: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub analog_only: bool,
    pub macro_meta: Option<crate::macro_icons::MacroDisplayEntry>,
}

impl From<&KbmCell> for PickerCell {
    fn from(c: &KbmCell) -> Self {
        PickerCell {
            pin: c.pin.to_string(),
            x: c.x,
            y: c.y,
            width: c.width,
            analog_only: c.analog_only,
            macro_meta: None,
        }
    }
}

// The gamepad cluster sits below the keyboard, to the right of where the macro
// cluster grows, so neither can ever reach the other.
const PAD_X: f32 = 15.5;
const PAD_Y: f32 = 6.6;

/// The gamepad, as a shape you can find a button on without reading it.
///
/// Offered only where a pad button is a legal TARGET — JSM's virtual pad
/// output. The Remapper's picker leaves it out: there a pad button is an input
/// being remapped, and offering it as an output would be offering a mapping to
/// itself.
pub const PAD_LAYOUT: &[KbmCell] = &[
    // Triggers above bumpers, the way they sit under your fingers.
    c("left_trigger", PAD_X, PAD_Y), c("right_trigger", PAD_X + 6.0, PAD_Y),
    c("btn_lb", PAD_X, PAD_Y + 1.0), c("btn_rb", PAD_X + 6.0, PAD_Y + 1.0),
    // Back / guide / start across the middle.
    c("btn_back", PAD_X + 2.0, PAD_Y + 1.0),
    c("btn_guide", PAD_X + 3.0, PAD_Y + 1.0),
    c("btn_start", PAD_X + 4.0, PAD_Y + 1.0),
    // D-pad as a plus on the left, face buttons as a diamond on the right.
    c("dpad_up", PAD_X + 1.0, PAD_Y + 2.0), c("btn_north", PAD_X + 5.0, PAD_Y + 2.0),
    c("dpad_left", PAD_X, PAD_Y + 3.0), c("dpad_right", PAD_X + 2.0, PAD_Y + 3.0),
    c("btn_west", PAD_X + 4.0, PAD_Y + 3.0), c("btn_east", PAD_X + 6.0, PAD_Y + 3.0),
    c("dpad_down", PAD_X + 1.0, PAD_Y + 4.0), c("btn_south", PAD_X + 5.0, PAD_Y + 4.0),
    // Stick clicks at the bottom, under the hands they belong to.
    c("btn_ls", PAD_X + 1.0, PAD_Y + 5.0), c("btn_rs", PAD_X + 5.0, PAD_Y + 5.0),
    // The rear paddles, in the pairs a hand finds them in. JSM calls them after
    // the JoyCon rail buttons that sit in the same place (LSL / LSR / RSL / RSR).
    c("btn_paddle_l1", PAD_X, PAD_Y + 6.0), c("btn_paddle_l2", PAD_X + 1.0, PAD_Y + 6.0),
    c("btn_paddle_r1", PAD_X + 5.0, PAD_Y + 6.0), c("btn_paddle_r2", PAD_X + 6.0, PAD_Y + 6.0),
    // Capture / Share, and the extra buttons a pad may carry. JSM's CAPTURE is
    // one name for "touchpad click or Capture" (its own words), so this cell and
    // the touchpad's click cell both write it — as they should, since a config
    // written on one pad has to run on the other.
    c("btn_capture", PAD_X, PAD_Y + 7.0),
    c("btn_misc1", PAD_X + 1.0, PAD_Y + 7.0), c("btn_misc2", PAD_X + 2.0, PAD_Y + 7.0),
    c("btn_misc3", PAD_X + 3.0, PAD_Y + 7.0), c("btn_misc4", PAD_X + 4.0, PAD_Y + 7.0),
    c("btn_misc5", PAD_X + 5.0, PAD_Y + 7.0), c("btn_misc6", PAD_X + 6.0, PAD_Y + 7.0),
];

/// Where the gamepad cluster's caption goes, in grid units.
pub fn pad_caption_at() -> (f32, f32) {
    (PAD_X, PAD_Y - 0.42)
}

/// Is this one of the gamepad cluster's cells?
///
/// Used to draw it with a pad skin rather than the keyboard one — and the Xbox
/// skin specifically, because the names this picker writes are JSM's `X_`
/// family, and a board that shows a ✕ while writing `X_A` reads as a mistake.
pub fn is_pad_cell(pin: &str) -> bool {
    PAD_LAYOUT.iter().any(|c| c.pin == pin)
}

/// Grid Y where the macro cluster starts (one gap row below the keyboard).
pub const MACRO_Y: f32 = 6.6;
/// Macro cells per row before wrapping.
const MACRO_PER_ROW: usize = 14;

/// The cell set actually shown (and navigated) for the current picker mode.
///
/// In Touch-Zones mode a touchpad can't remap to itself, so the touchpad cluster
/// is hidden and analog OUTPUT cells (mouse delta + sticks) fill the vacated
/// columns. Those extras must be REAL cells in this list — otherwise gamepad nav
/// (which walks this list) can't reach them, and the focus ring lands on a
/// hidden cell, making the cursor vanish. Single source of truth shared by the
/// renderer, the spatial nav, and activation.
///
/// `macros` is the patch's defined macro ports, laid out as an extra cluster
/// BELOW the keyboard at [`MACRO_Y`].
pub fn picker_cells(
    tz: bool,
    pad: bool,
    macros: &[crate::macro_icons::MacroDisplayEntry],
) -> Vec<PickerCell> {
    let mut cells: Vec<PickerCell> = if !tz {
        KBM_LAYOUT.iter().map(PickerCell::from).collect()
    } else {
        // Touchpad cluster pins hidden in Touch-Zones mode (see kbm_picker_window).
        let hidden = |pin: &str| matches!(pin,
            "touch_left" | "touch_center" | "touch_right"
            | "btn_touchpad" | "touch_swipe_x" | "touch_swipe_y" | "btn_mute");
        let mut cells: Vec<PickerCell> = KBM_LAYOUT.iter()
            .filter(|c| !hidden(c.pin)).map(PickerCell::from).collect();
        // Analog outputs placed in the vacated touchpad columns: mouse-delta and the
        // full analog sticks (touch position → deflection). The KB/M grid has no
        // stick keys, so they live here — and stay navigable. Marked analog-only so
        // they are greyed for targets where analog output makes no sense (a Virtual
        // Menu zone triggers a DISCRETE selection); a touch/touchpad zone keeps them
        // enabled (`picker_analog_input_ok` returns true for non-menu touch zones).
        cells.push((&ca("mouse_x", TOUCH_X, 1.0)).into());
        cells.push((&ca("mouse_y", TOUCH_X + 1.0, 1.0)).into());
        cells.push((&ca("mouse", TOUCH_X, 2.0)).into());
        cells.push((&ca("left_stick", TOUCH_X, 3.0)).into());
        cells.push((&ca("right_stick", TOUCH_X + 1.0, 3.0)).into());
        // Analog (variable-speed) scroll — the touch-zone deflection sets the rate.
        cells.push((&ca("scroll_y", TOUCH_X, 4.0)).into());
        cells.push((&ca("scroll_x", TOUCH_X + 1.0, 4.0)).into());
        cells
    };
    if pad {
        cells.extend(PAD_LAYOUT.iter().map(PickerCell::from));
    }
    for (i, entry) in macros.iter().enumerate() {
        cells.push(PickerCell {
            pin: entry.pin.clone(),
            x: (i % MACRO_PER_ROW) as f32,
            y: MACRO_Y + (i / MACRO_PER_ROW) as f32 * 1.1,
            width: 1.0,
            analog_only: false,
            macro_meta: Some(entry.clone()),
        });
    }
    cells
}

/// Width/height of a cell list in grid units (for sizing the window).
pub fn layout_extent(cells: &[PickerCell]) -> (f32, f32) {
    let mut max_x = 0.0f32;
    let mut max_y = 0.0f32;
    for cell in cells {
        max_x = max_x.max(cell.x + cell.width);
        max_y = max_y.max(cell.y + 1.0);
    }
    (max_x, max_y)
}

/// A valid index into `cells`, clamped.
pub fn clamp_index(cells: &[PickerCell], idx: usize) -> usize {
    idx.min(cells.len().saturating_sub(1))
}

/// Center point of a cell in grid units (for spatial navigation + hit-testing).
fn cell_center(cell: &PickerCell) -> (f32, f32) {
    (cell.x + cell.width * 0.5, cell.y + 0.5)
}

/// Find the nearest cell to `from` in the pressed direction (`dx`, `dy` are the
/// unit direction: e.g. right = (1, 0), up = (0, -1)). Returns the current index
/// if no cell lies in that direction. Scores by primary-axis distance plus a
/// cross-axis penalty so navigation favors the same row/column.
pub fn nearest_in_dir(
    cells: &[PickerCell],
    from: usize,
    dx: f32,
    dy: f32,
    usable: impl Fn(&PickerCell) -> bool,
) -> usize {
    let from = clamp_index(cells, from);
    let (cx, cy) = cell_center(&cells[from]);
    let mut best = from;
    let mut best_score = f32::INFINITY;
    for (i, cell) in cells.iter().enumerate() {
        // Skip the current cell and any DISABLED cell (analog output on a
        // discrete target, or a self-target), so focus can't land on — and
        // therefore can't map — a greyed cell.
        if i == from || !usable(cell) { continue; }
        let (ox, oy) = cell_center(cell);
        let vx = ox - cx;
        let vy = oy - cy;
        // Project onto the pressed direction; reject anything not ahead of us.
        let primary = vx * dx + vy * dy;
        if primary <= 0.05 { continue; }
        // Cross-axis offset (perpendicular distance).
        let cross = (vx * dy - vy * dx).abs();
        let score = primary + cross * 2.5;
        if score < best_score {
            best_score = score;
            best = i;
        }
    }
    best
}

// ── typing, rather than binding ───────────────────────────────────────────────
//
// The same grid serves a second purpose: entering TEXT from a pad. A JSM comment
// or a quoted config-file name can't be built out of pin ids, and neither can the
// name of a preset. Only what an activated cell MEANS changes, so the layout,
// the spatial navigation and the window stay one implementation.

/// What activating a cell does while the picker is typing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Typed {
    /// Append this character.
    Char(char),
    /// Take the last character back.
    Backspace,
    /// Latch (or unlatch) the shifted form of what follows.
    Caps,
    /// Finish, keeping what was typed. Enter, where a typist expects it.
    Commit,
    /// Finish, keeping nothing. Escape, likewise.
    Cancel,
}

/// The two characters a US keyboard puts on one key.
///
/// Without the shifted halves a comment can't hold a `?` and a file name can't
/// hold a `_`, which are exactly the characters a config wants.
const PAIRS: &[(&str, char, char)] = &[
    ("key_backtick", '`', '~'),
    ("key_num1", '1', '!'), ("key_num2", '2', '@'), ("key_num3", '3', '#'),
    ("key_num4", '4', '$'), ("key_num5", '5', '%'), ("key_num6", '6', '^'),
    ("key_num7", '7', '&'), ("key_num8", '8', '*'), ("key_num9", '9', '('),
    ("key_num0", '0', ')'),
    ("key_minus", '-', '_'), ("key_equals", '=', '+'),
    ("key_openbracket", '[', '{'), ("key_closebracket", ']', '}'),
    ("key_backslash", '\\', '|'),
    ("key_semicolon", ';', ':'), ("key_quote", '\'', '"'),
    ("key_comma", ',', '<'), ("key_period", '.', '>'), ("key_slash", '/', '?'),
];

/// What this cell contributes to what is being typed, or `None` if it
/// contributes nothing — which is what greys it while typing. A function key or
/// a mouse button has no character to give.
pub fn cell_typed(pin: &str, caps: bool) -> Option<Typed> {
    if let Some(rest) = pin.strip_prefix("key_") {
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Some(Typed::Char(if caps {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }));
            }
        }
    }
    if let Some((_, plain, shifted)) = PAIRS.iter().find(|(p, _, _)| *p == pin) {
        return Some(Typed::Char(if caps { *shifted } else { *plain }));
    }
    Some(match pin {
        "key_space" => Typed::Char(' '),
        "key_backspace" => Typed::Backspace,
        // Any of the keys a typist reaches for to change case — either Shift,
        // sided or not, and Caps Lock.
        "key_shift" | "key_lshift" | "key_rshift" | "key_capslock" => Typed::Caps,
        "key_enter" => Typed::Commit,
        "key_escape" => Typed::Cancel,
        _ => return None,
    })
}

/// The pin a JSM binding would name, for a cell this grid spells differently.
///
/// The layout is built from egui's captured key names, which call the digit row
/// `Num7`; a binding publishes `key_7`, and that is the spelling the name index
/// is keyed by. One rename, in the one place that knows the layout's spelling.
pub fn pin_as_bound(pin: &str) -> &str {
    match pin.strip_prefix("key_num") {
        Some(d) if d.len() == 1 && d.starts_with(|c: char| c.is_ascii_digit()) => {
            match d {
                "0" => "key_0", "1" => "key_1", "2" => "key_2", "3" => "key_3",
                "4" => "key_4", "5" => "key_5", "6" => "key_6", "7" => "key_7",
                "8" => "key_8", _ => "key_9",
            }
        }
        _ => pin,
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;

    #[test]
    fn the_keys_that_carry_characters_carry_both_of_theirs() {
        assert_eq!(cell_typed("key_a", false), Some(Typed::Char('a')));
        assert_eq!(cell_typed("key_a", true), Some(Typed::Char('A')));
        assert_eq!(cell_typed("key_space", false), Some(Typed::Char(' ')));
        // Shifted halves, without which a comment can't hold a `?` and a file
        // name can't hold a `_`.
        assert_eq!(cell_typed("key_slash", true), Some(Typed::Char('?')));
        assert_eq!(cell_typed("key_minus", true), Some(Typed::Char('_')));
        assert_eq!(cell_typed("key_num1", false), Some(Typed::Char('1')));
        assert_eq!(cell_typed("key_num1", true), Some(Typed::Char('!')));
        // The keys a typist expects to mean something other than a character.
        assert_eq!(cell_typed("key_backspace", false), Some(Typed::Backspace));
        assert_eq!(cell_typed("key_shift", false), Some(Typed::Caps));
        assert_eq!(cell_typed("key_lshift", false), Some(Typed::Caps), "sided too");
        assert_eq!(cell_typed("key_rshift", false), Some(Typed::Caps));
        assert_eq!(cell_typed("key_capslock", false), Some(Typed::Caps));
        // Ctrl, Alt and Win type nothing, sided or not.
        assert_eq!(cell_typed("key_lctrl", false), None);
        assert_eq!(cell_typed("key_rwin", false), None);
        assert_eq!(cell_typed("key_enter", false), Some(Typed::Commit));
        assert_eq!(cell_typed("key_escape", false), Some(Typed::Cancel));
        // And the ones with nothing to give, which is what greys them.
        assert_eq!(cell_typed("key_f5", false), None);
        assert_eq!(cell_typed("mouse_left", false), None);
        assert_eq!(cell_typed("key_arrowup", false), None);
        assert_eq!(cell_typed("touch_swipe_x", false), None);
    }

    /// Every cell on the board either types something or is greyed while
    /// typing — there is no cell that silently does nothing when pressed.
    #[test]
    fn every_key_on_the_board_either_types_or_is_greyed() {
        for cell in KBM_LAYOUT {
            let typed = cell_typed(cell.pin, false);
            let letterish = cell.pin.starts_with("key_")
                && !matches!(
                    cell.pin,
                    "key_f1" | "key_f2" | "key_f3" | "key_f4" | "key_f5" | "key_f6"
                        | "key_f7" | "key_f8" | "key_f9" | "key_f10" | "key_f11" | "key_f12"
                        | "key_tab"
                        | "key_lctrl" | "key_rctrl" | "key_lalt" | "key_ralt"
                        | "key_lwin" | "key_rwin"
                        | "key_printscreen" | "key_pause" | "key_insert" | "key_delete"
                        | "key_home" | "key_end" | "key_pageup" | "key_pagedown"
                        | "key_arrowup" | "key_arrowdown" | "key_arrowleft" | "key_arrowright"
                );
            assert_eq!(
                typed.is_some(),
                letterish,
                "{} types {typed:?}, which is not what its place on a keyboard says",
                cell.pin
            );
        }
    }

    /// The digit row is spelled two ways in this codebase, and only one of them
    /// is what a binding publishes.
    #[test]
    fn the_digit_row_is_asked_for_under_the_name_a_binding_uses() {
        assert_eq!(pin_as_bound("key_num7"), "key_7");
        assert_eq!(pin_as_bound("key_num0"), "key_0");
        assert_eq!(pin_as_bound("key_a"), "key_a", "everything else is left alone");
        assert_eq!(pin_as_bound("mouse_left"), "mouse_left");
        // Every digit cell on the board resolves, so none of them greys out for
        // want of a name.
        for cell in KBM_LAYOUT {
            if cell.pin.starts_with("key_num") {
                let bound = pin_as_bound(cell.pin);
                assert!(
                    bound.len() == 5 && bound.ends_with(|c: char| c.is_ascii_digit()),
                    "{} became {bound}",
                    cell.pin
                );
            }
        }
    }
}

#[cfg(test)]
mod pad_cluster_tests {
    use super::*;

    /// The gamepad cluster can't overlap anything else on the board, and every
    /// cell it adds is one JSM can name — a key you can focus and press that
    /// then does nothing is worse than one that isn't there.
    #[test]
    fn the_gamepad_cluster_sits_clear_of_the_rest_of_the_board() {
        let with_pad = picker_cells(false, true, &[]);
        let without = picker_cells(false, false, &[]);
        assert_eq!(with_pad.len(), without.len() + PAD_LAYOUT.len());

        let overlaps = |a: &PickerCell, b: &PickerCell| {
            a.x < b.x + b.width && b.x < a.x + a.width && (a.y - b.y).abs() < 1.0
        };
        for (i, a) in with_pad.iter().enumerate() {
            for b in with_pad.iter().skip(i + 1) {
                assert!(!overlaps(a, b), "{} overlaps {} at ({}, {})", a.pin, b.pin, a.x, a.y);
            }
        }
        // And it stays out of the column the macro cluster grows down.
        for cell in PAD_LAYOUT {
            assert!(cell.x >= MACRO_PER_ROW as f32, "{} is where macros go", cell.pin);
        }
    }

    /// Every cell on the gamepad cluster can START a line — that is what the
    /// cluster is for. Only some of them can END one: a virtual Xbox pad has no
    /// paddles and no Misc buttons, so nothing can be bound TO those, and the
    /// board greys them where a value goes rather than offering a name that
    /// doesn't exist.
    #[test]
    fn every_gamepad_cell_can_start_a_line_and_only_some_can_end_one() {
        let outputs = flexinput_engine::eval::jsm_names_by_pin();
        let inputs = flexinput_engine::eval::jsm_input_names_by_pin();
        for cell in PAD_LAYOUT {
            let pin = pin_as_bound(cell.pin);
            assert!(
                inputs.contains_key(pin),
                "{} is on the board but a line can't be started WITH it",
                cell.pin
            );
            if let Some(out) = outputs.get(pin) {
                assert_ne!(
                    inputs.get(pin),
                    Some(out),
                    "{}: the two sides of a line would read the same, which they don't",
                    cell.pin
                );
            }
            assert!(is_pad_cell(cell.pin));
        }
        // What a virtual pad reports, and so what a binding can drive.
        for pin in [
            "btn_south", "btn_east", "btn_west", "btn_north", "btn_lb", "btn_rb",
            "btn_ls", "btn_rs", "btn_back", "btn_start", "btn_guide",
            "dpad_up", "dpad_down", "dpad_left", "dpad_right",
            "left_trigger", "right_trigger",
        ] {
            assert!(outputs.contains_key(pin), "{pin} should be bindable TO");
        }
        // And what it hasn't got. These are inputs only, which is why the board
        // has to know which side of the line it is inserting into.
        for pin in ["btn_paddle_l1", "btn_paddle_r2", "btn_misc1", "btn_misc6", "btn_capture"] {
            assert!(
                !outputs.contains_key(pin),
                "{pin} has an output name now, so it should be offered on both sides"
            );
        }
        assert!(!is_pad_cell("key_a"), "and the keyboard is not the gamepad");
    }

    /// Every character this board can type either has a glyph in the icon set or
    /// is one of the few drawn over the blank key. A tenth one appearing without
    /// a face would just look like a bug.
    #[test]
    fn every_character_the_board_types_has_a_face() {
        /// The glyphs the KB/M set hasn't got. Drawn as text over the blank key.
        const DRAWN_OVER_BLANK: &[char] =
            &['@', '#', '$', '%', '&', '(', ')', '{', '}', '|'];
        for cell in KBM_LAYOUT {
            for caps in [false, true] {
                let Some(Typed::Char(c)) = cell_typed(cell.pin, caps) else { continue };
                if c == ' ' {
                    continue;
                }
                let has_icon = crate::canvas::remapper_icons::char_svg(c).is_some();
                // A letter keeps its pin's icon, which reads the same either way.
                let is_letter = c.is_ascii_alphabetic();
                assert!(
                    has_icon || is_letter || DRAWN_OVER_BLANK.contains(&c),
                    "{:?} on {} (caps={caps}) has no face and isn't listed as drawn",
                    c,
                    cell.pin
                );
            }
        }
        // And the listed ones really are missing, so the list can't rot into a
        // set of characters that quietly have icons now.
        for c in DRAWN_OVER_BLANK {
            assert!(
                crate::canvas::remapper_icons::char_svg(*c).is_none(),
                "{c:?} has an icon now and should be taken off the drawn list"
            );
        }
    }
}
