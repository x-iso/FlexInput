//! Bounded cache for textures rasterized per pixel size (SVG icons, glyphs,
//! pills). Those used to live in egui's temp data under a key that includes the
//! size, never evicted: a pinned widget or overlay element being resized, or a
//! DPI change, left one texture per size behind for the rest of the session.
//! Here the least recently used ones are dropped once the cache holds more than
//! [`BUDGET_BYTES`] of pixels; a dropped handle frees its texture.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Pixel budget for cached size-keyed textures (RGBA, 4 bytes per pixel).
const BUDGET_BYTES: usize = 48 * 1024 * 1024;

#[derive(Default)]
struct Lru {
    entries: HashMap<egui::Id, (egui::TextureHandle, u64)>,
    bytes: usize,
    clock: u64,
}

fn bytes_of(h: &egui::TextureHandle) -> usize {
    let [w, h] = h.size();
    w * h * 4
}

fn with_lru<R>(ctx: &egui::Context, f: impl FnOnce(&mut Lru) -> R) -> R {
    let lru = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Arc<Mutex<Lru>>>(egui::Id::new("fxi_tex_cache")).clone()
    });
    let mut lru = lru.lock().unwrap();
    f(&mut lru)
}

/// The texture cached under `key`, if any (marks it recently used).
pub(crate) fn get(ctx: &egui::Context, key: egui::Id) -> Option<egui::TextureHandle> {
    with_lru(ctx, |lru| {
        lru.clock += 1;
        let now = lru.clock;
        lru.entries.get_mut(&key).map(|(h, used)| {
            *used = now;
            h.clone()
        })
    })
}

/// Cache `handle` under `key`, evicting the least recently used textures while
/// the cache is over budget (never the one just added).
pub(crate) fn put(ctx: &egui::Context, key: egui::Id, handle: egui::TextureHandle) {
    with_lru(ctx, |lru| {
        lru.clock += 1;
        let now = lru.clock;
        let added = bytes_of(&handle);
        if let Some((old, _)) = lru.entries.insert(key, (handle, now)) {
            lru.bytes -= bytes_of(&old);
        }
        lru.bytes += added;
        while lru.bytes > BUDGET_BYTES && lru.entries.len() > 1 {
            let Some(oldest) = lru.entries.iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| *k)
            else { break };
            if let Some((h, _)) = lru.entries.remove(&oldest) {
                lru.bytes -= bytes_of(&h);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(ctx: &egui::Context, name: &str, side: usize) -> egui::TextureHandle {
        ctx.load_texture(name, egui::ColorImage::filled([side, side], egui::Color32::WHITE),
            egui::TextureOptions::LINEAR)
    }

    #[test]
    fn evicts_least_recently_used_over_budget() {
        let ctx = egui::Context::default();
        // 2048² × 4 = 16 MB each: the fourth pushes past the 48 MB budget.
        let side = 2048;
        for i in 0..3 {
            put(&ctx, egui::Id::new(i), tex(&ctx, "t", side));
        }
        assert!(get(&ctx, egui::Id::new(0)).is_some()); // 0 is now the most recent
        put(&ctx, egui::Id::new(3), tex(&ctx, "t", side));
        assert!(get(&ctx, egui::Id::new(1)).is_none(), "oldest unused one goes");
        assert!(get(&ctx, egui::Id::new(0)).is_some());
        assert!(get(&ctx, egui::Id::new(3)).is_some());
    }
}
