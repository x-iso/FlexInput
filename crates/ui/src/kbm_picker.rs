//! Gamepad-navigable virtual keyboard / mouse picker.
//!
//! Opened from a Remapper's "Special" slot (gamepad South) during the output
//! Learn phase. Renders a keyboard-ish grid of KBM icons in a modal window; the
//! user navigates cells with the left stick / D-pad, presses South to append the
//! focused pin to the output chord (`draft_output`), North to reset that chord,
//! and East to close. This lets a controller-only user assign keyboard/mouse
//! outputs without a physical keyboard or mouse.
//!
//! The picker is driven entirely from the top-level nav driver (`app.rs`), NOT
//! from inside a pinned body — pinned remapper bodies render in a child
//! TSTransform layer where painting/relocking the egui ctx deadlocks epaint.

/// One key cell: the output pin id it appends, and its width in grid units
/// (1.0 = a standard key). Wider keys (Space, Shift, …) span more.
#[derive(Clone, Copy)]
pub struct KbmCell {
    pub pin: &'static str,
    pub width: f32,
}

const fn k(pin: &'static str) -> KbmCell { KbmCell { pin, width: 1.0 } }
const fn kw(pin: &'static str, width: f32) -> KbmCell { KbmCell { pin, width } }

/// The keyboard layout, row by row, ending with a mouse row. Pin ids match
/// `remapper_icons::pin_svg` so every cell resolves to an SVG icon.
pub const KBM_LAYOUT: &[&[KbmCell]] = &[
    // Function row.
    &[
        k("key_escape"),
        k("key_f1"), k("key_f2"), k("key_f3"), k("key_f4"),
        k("key_f5"), k("key_f6"), k("key_f7"), k("key_f8"),
        k("key_f9"), k("key_f10"), k("key_f11"), k("key_f12"),
    ],
    // Number row.
    &[
        k("key_backtick"),
        k("key_num1"), k("key_num2"), k("key_num3"), k("key_num4"), k("key_num5"),
        k("key_num6"), k("key_num7"), k("key_num8"), k("key_num9"), k("key_num0"),
        k("key_minus"), k("key_equals"), kw("key_backspace", 2.0),
    ],
    // Top letter row.
    &[
        kw("key_tab", 1.5),
        k("key_q"), k("key_w"), k("key_e"), k("key_r"), k("key_t"),
        k("key_y"), k("key_u"), k("key_i"), k("key_o"), k("key_p"),
        k("key_openbracket"), k("key_closebracket"), kw("key_backslash", 1.5),
    ],
    // Home row.
    &[
        kw("key_capslock", 1.75),
        k("key_a"), k("key_s"), k("key_d"), k("key_f"), k("key_g"),
        k("key_h"), k("key_j"), k("key_k"), k("key_l"),
        k("key_semicolon"), k("key_apostrophe"), kw("key_enter", 2.25),
    ],
    // Bottom letter row.
    &[
        kw("key_shift", 2.25),
        k("key_z"), k("key_x"), k("key_c"), k("key_v"), k("key_b"),
        k("key_n"), k("key_m"),
        k("key_comma"), k("key_period"), k("key_slash"), kw("key_shift", 2.75),
    ],
    // Modifier / space row.
    &[
        kw("key_ctrl", 1.5), kw("key_win", 1.25), kw("key_alt", 1.25),
        kw("key_space", 6.0),
        kw("key_alt", 1.25), kw("key_ctrl", 1.5),
    ],
    // Nav / arrows row.
    &[
        k("key_insert"), k("key_home"), k("key_delete"), k("key_end"),
        k("key_arrowup"), k("key_arrowleft"), k("key_arrowdown"), k("key_arrowright"),
    ],
    // Mouse row.
    &[
        kw("mouse_left", 1.5), kw("mouse_right", 1.5),
        kw("scroll_up", 1.5), kw("scroll_down", 1.5),
    ],
];

/// Clamp a (row, col) cursor to the layout, returning a valid cell position.
pub fn clamp_cursor(row: usize, col: usize) -> (usize, usize) {
    let r = row.min(KBM_LAYOUT.len().saturating_sub(1));
    let c = col.min(KBM_LAYOUT[r].len().saturating_sub(1));
    (r, c)
}
