//! Icon set + display registry for Macro Output ports.
//!
//! ## Icons
//! SVGs from `app/assets/general/` are embedded at compile time (same pattern
//! as `remapper_icons`). A port stores an icon KEY (`MacroPortDef::icon`);
//! [`macro_icon_svg`] resolves it back to bytes and [`macro_icon_texture`]
//! rasterizes + caches it for cells/chips. [`ALL_ICONS`] drives the icon
//! picker menu in the node body.
//!
//! ## Display registry
//! Macro pins ("macro:{id}") appear in mapping `out` arrays, mapping-card
//! chips, the KB/M picker, and Touch Zones' analog-output checks — far from
//! the Macro node that defines them, and several of those sites have no
//! `egui::Context` in reach (gamepad-nav helpers). So the app publishes a
//! per-frame snapshot of all defined ports (active tab, including nested
//! sub-patches) into a process-global; [`registry`] reads it back anywhere a
//! pin id needs a name/icon/type. Same single-writer-per-frame global pattern
//! as `flexinput_engine::current_sample_rate`.

use std::sync::Arc;

use flexinput_core::SignalType;

/// One defined macro port, as seen by pickers and chip renderers.
#[derive(Debug, Clone)]
pub struct MacroDisplayEntry {
    /// Bus pin id: `macro:{port_id}`.
    pub pin: String,
    /// User-facing port name.
    pub name: String,
    /// Icon key into [`ALL_ICONS`]; empty = no icon (render the name).
    pub icon: String,
    /// Custom icon SVG text embedded in the patch; non-empty wins over `icon`.
    pub icon_svg: String,
    pub signal_type: SignalType,
}

static REGISTRY: std::sync::RwLock<Option<Arc<Vec<MacroDisplayEntry>>>> =
    std::sync::RwLock::new(None);

/// Publish this frame's macro port table (called once per frame by `app.rs`).
pub fn publish_registry(entries: Arc<Vec<MacroDisplayEntry>>) {
    *REGISTRY.write().unwrap() = Some(entries);
}

/// The current macro port table (empty if nothing published yet this session).
pub fn registry() -> Arc<Vec<MacroDisplayEntry>> {
    REGISTRY.read().unwrap().clone().unwrap_or_default()
}

/// Look up one macro pin id in the registry.
pub fn registry_entry(pin: &str) -> Option<MacroDisplayEntry> {
    registry().iter().find(|e| e.pin == pin).cloned()
}

// ── Embedded SVG bytes (app/assets/general/) ─────────────────────────────────

macro_rules! a {
    ($p:literal) => {
        include_bytes!(concat!("../../../app/assets/general/", $p))
    };
}

const GEN_BUTTON_L: &[u8] = a!("generic_button_gl.svg");
const GEN_BUTTON_R: &[u8] = a!("generic_button_gr.svg");
const GEN_TRIGGER: &[u8] = a!("generic_button_trigger_c.svg");
const GEN_STICK: &[u8] = a!("generic_stick_finger.svg");
const GEN_GYRO: &[u8] = a!("shared_gyro.svg");

/// Every embedded macro icon: `(key, human label, svg bytes)`. Keys are
/// persisted in `macro_ports` — never rename one once shipped.
pub const ALL_ICONS: &[(&str, &str, &[u8])] = &[
    ("button_l", "Button (L)", GEN_BUTTON_L),
    ("button_r", "Button (R)", GEN_BUTTON_R),
    ("trigger", "Trigger", GEN_TRIGGER),
    ("stick", "Stick", GEN_STICK),
    ("gyro", "Gyro", GEN_GYRO),
];

/// Resolve an icon key to its embedded SVG bytes.
pub fn macro_icon_svg(key: &str) -> Option<&'static [u8]> {
    ALL_ICONS.iter().find(|(k, _, _)| *k == key).map(|(_, _, b)| *b)
}

/// Rasterize a macro icon to a cached white-on-transparent texture, sized for
/// a cell/chip of `size_pts` logical pixels (mirrors `kbm_cell_texture`).
pub fn macro_icon_texture(
    ctx: &egui::Context,
    key: &str,
    size_pts: f32,
) -> Option<egui::TextureHandle> {
    let bytes = macro_icon_svg(key)?;
    let text = std::str::from_utf8(bytes).ok()?;
    let cache_salt = bytes.as_ptr() as usize as u64;
    svg_texture_cached(ctx, text, cache_salt, size_pts)
}

/// Resolve a PORT's icon to a texture: the embedded-in-patch custom SVG when
/// present, else the embedded icon set by key, else `None` (render the name).
/// This is THE resolver every display site uses (body button, picker cells,
/// mapping-card chips, zone overlays) so custom icons appear everywhere the
/// stock ones do.
pub fn macro_port_icon_texture(
    ctx: &egui::Context,
    icon_key: &str,
    icon_svg: &str,
    size_pts: f32,
) -> Option<egui::TextureHandle> {
    if !icon_svg.is_empty() {
        // Content-hash the SVG so edits/reloads get a fresh texture and
        // identical icons on several ports share one.
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        icon_svg.hash(&mut h);
        return svg_texture_cached(ctx, icon_svg, h.finish(), size_pts);
    }
    macro_icon_texture(ctx, icon_key, size_pts)
}

/// Rasterize + cache one SVG at `size_pts`, keyed by (`cache_salt`, size).
fn svg_texture_cached(
    ctx: &egui::Context,
    svg_text: &str,
    cache_salt: u64,
    size_pts: f32,
) -> Option<egui::TextureHandle> {
    let size_px = (size_pts * ctx.pixels_per_point()).round() as u32;
    let cache_key = egui::Id::new(("macro_icon", cache_salt, size_px));
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_key)) {
        return Some(tex);
    }
    let img = crate::canvas::viewer::rasterize_svg_recolored(
        svg_text, size_px, size_px, "override", egui::Color32::TRANSPARENT)?;
    let handle = ctx.load_texture(
        format!("macro_icon_{cache_salt:x}_{size_px}"), img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(cache_key, handle.clone()));
    Some(handle)
}
