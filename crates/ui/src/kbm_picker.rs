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
    c("key_l", 9.75, 3.0), c("key_semicolon", 10.75, 3.0), c("key_apostrophe", 11.75, 3.0),
    cw("key_enter", 12.75, 3.0, 2.25),

    // ── Bottom letter row (y=4) ───────────────────────────────────────────
    cw("key_shift", 0.0, 4.0, 2.25),
    c("key_z", 2.25, 4.0), c("key_x", 3.25, 4.0), c("key_c", 4.25, 4.0), c("key_v", 5.25, 4.0),
    c("key_b", 6.25, 4.0), c("key_n", 7.25, 4.0), c("key_m", 8.25, 4.0),
    c("key_comma", 9.25, 4.0), c("key_period", 10.25, 4.0), c("key_slash", 11.25, 4.0),
    cw("key_shift", 12.25, 4.0, 2.75),

    // ── Modifier / space row (y=5) ────────────────────────────────────────
    cw("key_ctrl", 0.0, 5.0, 1.5), cw("key_win", 1.5, 5.0, 1.25), cw("key_alt", 2.75, 5.0, 1.25),
    cw("key_space", 4.0, 5.0, 6.0),
    cw("key_alt", 10.0, 5.0, 1.25), cw("key_ctrl", 11.25, 5.0, 1.5),

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
pub fn picker_cells(tz: bool, macros: &[crate::macro_icons::MacroDisplayEntry]) -> Vec<PickerCell> {
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
