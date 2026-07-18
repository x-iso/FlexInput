//! Material grouping + per-model default colour schemes for the 3D controller
//! viewer. Each mesh part (named by its `.obj` file, e.g. `top_shell`,
//! `bottom_shell`, `extra`, `left_cap`, `left_ring`) maps to a [`MaterialGroup`];
//! the viewer node stores a per-group colour scheme (defaulting to a per-model
//! palette that approximates the real controller). The mic (when a model has
//! one) is a fixed non-editable colour.
//!
//! Two parts get special roles:
//! - `bottom_shell` is the controller's *secondary* shell piece
//!   ([`ShellSecondary`]). On the DualSense it is black by default; on every
//!   other model it defaults to matching the main shell, but keeps its own
//!   control.
//! - `extra` is the **LED strip** ([`Led`]) on the DualSense / DualShock 4.
//!   Its scheme colour is the fallback; the viewer relays the live device LED
//!   colour onto it when available.
//!
//! The four face buttons and the two sticks are each split so they can be
//! coloured independently (many pads have coloured ABXY, or contrasting
//! sticks). [`ROWS`] groups the fine-grained groups into labelled editor rows.

/// A recolourable (or, for `Mic`, fixed) material group. The discriminant is the
/// stable index into a `Scheme` array — do not reorder after shipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum MaterialGroup {
    /// Main shell (`top_shell` + any unclassified shell piece).
    ShellMain = 0,
    /// Secondary shell (`bottom_shell`) — separate control, black on DualSense.
    ShellSecondary = 1,
    /// LED strip / lightbar (`extra`). Relays the live device LED when available.
    Led = 2,
    Touchpad = 3,
    /// Face buttons, individually recolourable (A/B/X/Y = south/east/west/north).
    FaceA = 4,
    FaceB = 5,
    FaceX = 6,
    FaceY = 7,
    Dpad = 8,
    Menu = 9,
    Bumper = 10,
    Trigger = 11,
    /// Left stick: dome + cap + rim.
    LeftDome = 12,
    LeftCap = 13,
    LeftRim = 14,
    /// Right stick: dome + cap + rim.
    RightDome = 15,
    RightCap = 16,
    RightRim = 17,
    /// PS / Xbox / Home logo button (`guide`).
    Logo = 18,
    /// Fixed cloudy-white; never user-editable.
    Mic = 19,
}

/// Number of material groups (length of a [`Scheme`]).
pub const N_GROUPS: usize = 20;

/// Per-group RGBA colour scheme, indexed by `MaterialGroup as usize`. Alpha
/// 255 = opaque; anything lower renders the group as translucent plastic
/// (drawn in a sorted blend pass after the opaque parts).
pub type Scheme = [[u8; 4]; N_GROUPS];

/// Fixed colour for the mic capsule — a soft cloudy white.
pub const MIC_COLOR: [u8; 3] = [224, 223, 216];

/// Default "white" for light-shelled controllers. Pure white reads as too
/// bright / unrealistic under the flat shading, so we use a soft grey-white.
pub const SOFT_WHITE: [u8; 3] = [175, 175, 175];

/// Filename of the per-model editable default palette inside each model folder.
pub const DEFAULT_SCHEME_FILE: &str = "colors.fxcol";

/// Editor rows: a display label + the material swatches shown on that row.
pub const ROWS: &[(&str, &[MaterialGroup])] = &[
    ("Shell", &[MaterialGroup::ShellMain, MaterialGroup::ShellSecondary]),
    ("LED", &[MaterialGroup::Led]),
    ("Touchpad", &[MaterialGroup::Touchpad]),
    (
        "Buttons",
        &[
            MaterialGroup::FaceA,
            MaterialGroup::FaceB,
            MaterialGroup::FaceX,
            MaterialGroup::FaceY,
            MaterialGroup::Dpad,
            MaterialGroup::Menu,
            MaterialGroup::Logo,
        ],
    ),
    ("Shoulders", &[MaterialGroup::Bumper, MaterialGroup::Trigger]),
    (
        "Left stick",
        &[MaterialGroup::LeftDome, MaterialGroup::LeftCap, MaterialGroup::LeftRim],
    ),
    (
        "Right stick",
        &[MaterialGroup::RightDome, MaterialGroup::RightCap, MaterialGroup::RightRim],
    ),
];

impl MaterialGroup {
    /// Short per-swatch label (tooltip in the editor).
    pub fn label(self) -> &'static str {
        match self {
            MaterialGroup::ShellMain => "Shell — main",
            MaterialGroup::ShellSecondary => "Shell — secondary (bottom)",
            MaterialGroup::Led => "LED strip (relays live device LED)",
            MaterialGroup::Touchpad => "Touchpad",
            MaterialGroup::FaceA => "Face A / Cross (south)",
            MaterialGroup::FaceB => "Face B / Circle (east)",
            MaterialGroup::FaceX => "Face X / Square (west)",
            MaterialGroup::FaceY => "Face Y / Triangle (north)",
            MaterialGroup::Dpad => "D-pad",
            MaterialGroup::Menu => "Menu buttons",
            MaterialGroup::Bumper => "Bumpers",
            MaterialGroup::Trigger => "Triggers",
            MaterialGroup::LeftDome => "Left stick dome",
            MaterialGroup::LeftCap => "Left stick cap",
            MaterialGroup::LeftRim => "Left stick rim",
            MaterialGroup::RightDome => "Right stick dome",
            MaterialGroup::RightCap => "Right stick cap",
            MaterialGroup::RightRim => "Right stick rim",
            MaterialGroup::Logo => "Logo",
            MaterialGroup::Mic => "Mic",
        }
    }

    /// Stable param key for persisting a per-group override.
    pub fn key(self) -> &'static str {
        match self {
            MaterialGroup::ShellMain => "shell",
            MaterialGroup::ShellSecondary => "shell_secondary",
            MaterialGroup::Led => "led",
            MaterialGroup::Touchpad => "touchpad",
            MaterialGroup::FaceA => "face_a",
            MaterialGroup::FaceB => "face_b",
            MaterialGroup::FaceX => "face_x",
            MaterialGroup::FaceY => "face_y",
            MaterialGroup::Dpad => "dpad",
            MaterialGroup::Menu => "menu",
            MaterialGroup::Bumper => "bumper",
            MaterialGroup::Trigger => "trigger",
            MaterialGroup::LeftDome => "left_dome",
            MaterialGroup::LeftCap => "left_cap",
            MaterialGroup::LeftRim => "left_rim",
            MaterialGroup::RightDome => "right_dome",
            MaterialGroup::RightCap => "right_cap",
            MaterialGroup::RightRim => "right_rim",
            MaterialGroup::Logo => "logo",
            MaterialGroup::Mic => "mic",
        }
    }
}

/// Map a part's `.obj` name to its material group. Order matters — specific
/// names are matched before the generic shell fallback.
pub fn group_for_part(name: &str) -> MaterialGroup {
    use MaterialGroup::*;
    let n = name.to_ascii_lowercase();
    let right = n.starts_with("right");
    if n.contains("mic") {
        return Mic;
    }
    // Home/guide logo button, plus the Switch Pro capture (screenshot) button
    // — the model names that part "misc" — which shares the home button's
    // colour group per user preference.
    if n.contains("guide")
        || n.contains("capture")
        || n.contains("screenshot")
        || n.starts_with("misc")
    {
        return Logo;
    }
    // The `extra` mesh on DS/DS4 is the LED strip; also match explicit names.
    if n.contains("extra") || n.contains("led") || n.contains("light") {
        return Led;
    }
    if n.contains("touch") {
        return Touchpad; // touchpad, touch_point1/2
    }
    if n.starts_with("dpad") {
        return Dpad;
    }
    if n.ends_with("_cap") {
        return if right { RightCap } else { LeftCap };
    }
    if n.ends_with("_ring") {
        return if right { RightRim } else { LeftRim };
    }
    if n.ends_with("_stick") {
        return if right { RightDome } else { LeftDome };
    }
    if n.contains("bumper") {
        return Bumper;
    }
    if n.contains("trigger") {
        return Trigger;
    }
    if n.contains("start")
        || n.contains("back")
        || n.contains("select")
        || n.contains("option")
        || n.contains("share")
        || n.contains("create")
        || n.contains("plus")
        || n.contains("minus")
    {
        return Menu;
    }
    match n.as_str() {
        "a_button" => return FaceA,
        "b_button" => return FaceB,
        "x_button" => return FaceX,
        "y_button" => return FaceY,
        _ => {}
    }
    // The secondary shell piece (DualSense: the black inner piece).
    if n.contains("bottom") {
        return ShellSecondary;
    }
    // top/mid shells, paddles, anything else → main shell.
    ShellMain
}

/// Coarse **occlusion object** id for x-ray grouping. The x-ray ghost measures
/// each mesh part's visible-vs-total footprint, but a stick is modelled as
/// three separate meshes (dome + cap + rim): the cap covering its *own* dome
/// makes the dome read as "hidden" and ghosts it, even though the stick as a
/// whole is plainly on camera — and it strobes at the covering edge. Collapsing
/// the three stick meshes into one object means self-occlusion inside the stick
/// never trips x-ray; only the whole stick going behind the body drops its
/// combined visibility below threshold. Every other part stays its own object
/// (keyed by `part_index`), so unrelated parts still ghost independently.
pub fn xray_object_for_part(name: &str, part_index: usize) -> u32 {
    use MaterialGroup::*;
    match group_for_part(name) {
        LeftDome | LeftCap | LeftRim => u32::MAX,
        RightDome | RightCap | RightRim => u32::MAX - 1,
        _ => part_index as u32,
    }
}

/// Neutral distinct-grey palette used when a model has no tuned default.
/// Order MUST match the `MaterialGroup` discriminants.
/// Widen an RGB table to the RGBA [`Scheme`] (opaque).
const fn with_alpha(rgb: [[u8; 3]; N_GROUPS]) -> Scheme {
    let mut out = [[0u8; 4]; N_GROUPS];
    let mut i = 0;
    while i < N_GROUPS {
        out[i] = [rgb[i][0], rgb[i][1], rgb[i][2], 255];
        i += 1;
    }
    out
}

const NEUTRAL: Scheme = with_alpha([
    [46, 47, 52], // ShellMain
    [46, 47, 52], // ShellSecondary — matches main by default
    [64, 64, 64], // Led — cloudy plastic strip (grey, lit by the live relay)
    [38, 39, 44], // Touchpad
    [72, 74, 82], // FaceA
    [72, 74, 82], // FaceB
    [72, 74, 82], // FaceX
    [72, 74, 82], // FaceY
    [56, 58, 65], // Dpad
    [62, 64, 71], // Menu
    [50, 51, 57], // Bumper
    [50, 51, 57], // Trigger
    [34, 35, 39], // LeftDome
    [62, 64, 71], // LeftCap
    [40, 41, 46], // LeftRim
    [34, 35, 39], // RightDome
    [62, 64, 71], // RightCap
    [40, 41, 46], // RightRim
    [82, 84, 92], // Logo
    MIC_COLOR,    // Mic
]);

/// Set every group in `groups` to opaque `c` (palettes stay RGB; alpha is a
/// user-edit / fxcol concept).
fn put(s: &mut Scheme, groups: &[MaterialGroup], c: [u8; 3]) {
    for g in groups {
        s[*g as usize] = [c[0], c[1], c[2], 255];
    }
}

/// The built-in per-model palette (the fallback / seed for
/// [`DEFAULT_SCHEME_FILE`]). Unknown models fall back to [`NEUTRAL`].
pub fn builtin_scheme(model: &str) -> Scheme {
    use MaterialGroup::*;
    let faces = [FaceA, FaceB, FaceX, FaceY];
    let sticks = [LeftDome, RightDome];
    let caps = [LeftCap, RightCap];
    let rims = [LeftRim, RightRim];
    let mut s = NEUTRAL;
    match model {
        // White main shell, black secondary shell, blue lightbar, dark
        // buttons/sticks, soft-white touchpad/bumpers/triggers.
        "DualSense" => {
            put(&mut s, &[ShellMain], SOFT_WHITE);
            put(&mut s, &[ShellSecondary], [40, 41, 46]);
            put(&mut s, &[Led], [64, 64, 64]);
            put(&mut s, &[Touchpad, Bumper, Trigger], SOFT_WHITE);
            put(&mut s, &faces, [58, 60, 68]);
            put(&mut s, &[Dpad], [52, 54, 61]);
            put(&mut s, &[Menu], [92, 94, 102]);
            put(&mut s, &[Logo], [70, 72, 80]);
            put(&mut s, &sticks, [38, 39, 44]);
            put(&mut s, &caps, [38, 39, 44]);
            put(&mut s, &rims, [40, 41, 46]);
        }
        // Matte black body; has a lightbar between touchpad and sticks.
        "DualShock 4" => {
            put(&mut s, &[ShellMain, ShellSecondary], [40, 42, 48]);
            put(&mut s, &[Led], [64, 64, 64]);
            put(&mut s, &[Touchpad], [46, 48, 54]);
            put(&mut s, &[Bumper, Trigger], [46, 48, 54]);
            put(&mut s, &sticks, [30, 31, 35]);
            put(&mut s, &caps, [58, 60, 67]);
            put(&mut s, &rims, [34, 35, 40]);
        }
        // Black body, grey buttons; no lightbar (bottom matches top).
        "Switch Pro" | "Left Joycon" | "Right Joycon" | "Joycon Grip" => {
            put(&mut s, &[ShellMain, ShellSecondary], [36, 37, 41]);
            put(&mut s, &[Touchpad], [38, 39, 44]);
            put(&mut s, &faces, [58, 60, 67]);
            put(&mut s, &[Menu], [62, 64, 71]);
            put(&mut s, &[Bumper, Trigger], [42, 43, 49]);
            put(&mut s, &sticks, [30, 31, 35]);
            put(&mut s, &caps, [56, 58, 65]);
        }
        // Black plastic, translucent-grey sticks.
        "Xbox One" | "Xbox 360" => {
            put(&mut s, &[ShellMain, ShellSecondary], [38, 40, 45]);
            put(&mut s, &[Touchpad], [40, 41, 46]);
            put(&mut s, &faces, [64, 66, 74]);
            put(&mut s, &[Menu], [58, 60, 67]);
            put(&mut s, &[Bumper, Trigger], [46, 48, 54]);
            put(&mut s, &sticks, [32, 33, 37]);
            put(&mut s, &caps, [58, 60, 67]);
            put(&mut s, &[Logo], [84, 86, 94]);
        }
        // Classic indigo shell; the A button is famously green.
        "Gamecube" | "Wavebird" => {
            put(&mut s, &[ShellMain, ShellSecondary], [78, 66, 120]);
            put(&mut s, &faces, [70, 60, 108]);
            put(&mut s, &[FaceA], [96, 150, 128]);
            put(&mut s, &[Touchpad], [62, 52, 96]);
            put(&mut s, &[Dpad, Menu, Bumper, Trigger], [70, 60, 108]);
            put(&mut s, &[Logo], [90, 78, 132]);
            put(&mut s, &sticks, [46, 40, 72]);
            put(&mut s, &caps, [70, 60, 108]);
            put(&mut s, &rims, [58, 50, 90]);
        }
        _ => {}
    }
    s
}

/// The default per-group scheme for a model: the model folder's editable
/// [`DEFAULT_SCHEME_FILE`] overlaid on the built-in palette, falling back to the
/// built-in palette when the file is absent. Cached per model (edits require an
/// app restart, like the mesh cache).
static SCHEME_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Scheme>>,
> = std::sync::OnceLock::new();

fn scheme_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Scheme>> {
    SCHEME_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Drop all cached per-model default schemes (e.g. when the user models
/// directory changes and a same-named model may now shadow a bundled one).
pub fn clear_scheme_cache() {
    if let Ok(mut c) = scheme_cache().lock() {
        c.clear();
    }
}

pub fn default_scheme(model: &str) -> Scheme {
    if let Ok(c) = scheme_cache().lock() {
        if let Some(s) = c.get(model) {
            return *s;
        }
    }
    let mut scheme = builtin_scheme(model);
    if let Some(map) = load_model_fxcol(model) {
        apply_fxcol_map(&mut scheme, &map);
    }
    if let Ok(mut c) = scheme_cache().lock() {
        c.insert(model.to_string(), scheme);
    }
    scheme
}

/// Read a model's `colors.fxcol` into a `key → value` map, if present.
/// Resolves through the model source tiers (user dir → disk assets →
/// embedded), so a user-directory model's palette wins.
fn load_model_fxcol(model: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = crate::model::callback::model_file(model, DEFAULT_SCHEME_FILE)?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .as_object()
        .cloned()
}

/// Overlay a `key → [r,g,b]` / `[r,g,b,a]` map onto a scheme (only the
/// editable [`ROWS`] groups; missing keys keep the built-in value; a missing
/// alpha means opaque).
fn apply_fxcol_map(scheme: &mut Scheme, map: &serde_json::Map<String, serde_json::Value>) {
    for (_, groups) in ROWS {
        for &g in *groups {
            if let Some(arr) = map.get(g.key()).and_then(|v| v.as_array()) {
                if arr.len() >= 3 {
                    scheme[g as usize] = [
                        arr[0].as_u64().unwrap_or(0) as u8,
                        arr[1].as_u64().unwrap_or(0) as u8,
                        arr[2].as_u64().unwrap_or(0) as u8,
                        arr.get(3).and_then(|v| v.as_u64()).unwrap_or(255) as u8,
                    ];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_names_map_to_expected_groups() {
        assert_eq!(group_for_part("top_shell"), MaterialGroup::ShellMain);
        assert_eq!(group_for_part("bottom_shell"), MaterialGroup::ShellSecondary);
        assert_eq!(group_for_part("extra"), MaterialGroup::Led);
        assert_eq!(group_for_part("left_cap"), MaterialGroup::LeftCap);
        assert_eq!(group_for_part("right_cap"), MaterialGroup::RightCap);
        assert_eq!(group_for_part("left_ring"), MaterialGroup::LeftRim);
        assert_eq!(group_for_part("right_ring"), MaterialGroup::RightRim);
        assert_eq!(group_for_part("left_stick"), MaterialGroup::LeftDome);
        assert_eq!(group_for_part("right_stick"), MaterialGroup::RightDome);
        assert_eq!(group_for_part("a_button"), MaterialGroup::FaceA);
        assert_eq!(group_for_part("b_button"), MaterialGroup::FaceB);
        assert_eq!(group_for_part("x_button"), MaterialGroup::FaceX);
        assert_eq!(group_for_part("y_button"), MaterialGroup::FaceY);
        assert_eq!(group_for_part("start_button"), MaterialGroup::Menu);
        assert_eq!(group_for_part("guide_button"), MaterialGroup::Logo);
        assert_eq!(group_for_part("dpad_up"), MaterialGroup::Dpad);
        assert_eq!(group_for_part("touchpad"), MaterialGroup::Touchpad);
        assert_eq!(group_for_part("left_trigger"), MaterialGroup::Trigger);
        assert_eq!(group_for_part("left_bumper"), MaterialGroup::Bumper);
    }

    #[test]
    fn xray_object_groups_stick_meshes() {
        // The three meshes of one stick collapse to a single occlusion object,
        // so a cap covering its own dome can't ghost the stick.
        let l = [
            xray_object_for_part("left_stick", 0),
            xray_object_for_part("left_cap", 1),
            xray_object_for_part("left_ring", 2),
        ];
        assert!(l.iter().all(|&x| x == l[0]), "left stick meshes share one object");
        let r = [
            xray_object_for_part("right_stick", 3),
            xray_object_for_part("right_cap", 4),
            xray_object_for_part("right_ring", 5),
        ];
        assert!(r.iter().all(|&x| x == r[0]), "right stick meshes share one object");
        assert_ne!(l[0], r[0], "left and right sticks are distinct objects");

        // Every non-stick part is its own object (keyed by part index) and can
        // never collide with the stick sentinels (u32::MAX / MAX-1).
        assert_eq!(xray_object_for_part("top_shell", 6), 6);
        assert_eq!(xray_object_for_part("dpad_up", 7), 7);
        assert_ne!(xray_object_for_part("top_shell", 6), l[0]);
    }

    #[test]
    fn schemes_are_full_length() {
        assert_eq!(builtin_scheme("DualSense").len(), N_GROUPS);
        assert_eq!(builtin_scheme("unknown-model").len(), N_GROUPS);
    }

    #[test]
    fn non_dualsense_secondary_shell_matches_main() {
        for m in ["Switch Pro", "Xbox One", "Gamecube", "unknown"] {
            let s = builtin_scheme(m);
            assert_eq!(
                s[MaterialGroup::ShellMain as usize],
                s[MaterialGroup::ShellSecondary as usize],
                "{m}: secondary shell should match main by default"
            );
        }
        let ds = builtin_scheme("DualSense");
        assert_ne!(
            ds[MaterialGroup::ShellMain as usize],
            ds[MaterialGroup::ShellSecondary as usize]
        );
    }

    /// Regenerate the editable `colors.fxcol` seed file in every model folder
    /// from the built-in palettes. Run manually after tweaking a palette:
    /// `cargo test -p flexinput-ui dump_default_fxcol -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_default_fxcol_files() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/assets/models");
        for entry in std::fs::read_dir(&base).expect("read models dir").flatten() {
            let dir = entry.path();
            if !dir.join("info.txt").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let scheme = builtin_scheme(&name);
            let mut map = serde_json::Map::new();
            for (_, groups) in ROWS {
                for &g in *groups {
                    let c = scheme[g as usize];
                    map.insert(g.key().to_string(), serde_json::json!([c[0], c[1], c[2]]));
                }
            }
            let json = serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap();
            let path = dir.join(DEFAULT_SCHEME_FILE);
            std::fs::write(&path, json).unwrap();
            println!("wrote {}", path.display());
        }
    }
}
