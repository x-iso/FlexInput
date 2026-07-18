use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let sub_patches_dir = PathBuf::from(&manifest_dir)
        .join("../../app/assets/sub-patches");

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_file = PathBuf::from(&out_dir).join("generated_presets.rs");

    gen_macro_icons(&manifest_dir, &out_dir);

    let mut entries: Vec<(String, PathBuf)> = Vec::new();

    if sub_patches_dir.is_dir() {
        for entry in fs::read_dir(&sub_patches_dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("fxsp") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unnamed")
                    .to_string();
                // Canonicalize so include_bytes! gets an absolute path that
                // survives wherever the OUT_DIR ends up.
                if let Ok(abs) = fs::canonicalize(&path) {
                    entries.push((name, abs));
                }
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
    }

    println!("cargo:rerun-if-changed=../../app/assets/sub-patches");

    let mut code = String::from(
        "pub static FACTORY_PRESETS: &[(&str, &[u8])] = &[\n",
    );
    for (name, path) in &entries {
        let path_str = path.to_str().unwrap().replace('\\', "/");
        code.push_str(&format!(
            "    ({name:?}, include_bytes!({path_str:?})),\n",
            name = name,
            path_str = path_str,
        ));
    }
    code.push_str("];\n");

    fs::write(&out_file, code).unwrap();
}

/// Generate the Macro Output icon table from every `.svg` under
/// `app/assets/general/`, so the shippable icon set is a property of that
/// folder — not a hardcoded list in `macro_icons.rs`. Dropping in / renaming /
/// removing an SVG (or an entire category folder) changes the pickers on the
/// next build with no code edit.
///
/// Icons may be sorted into **category sub-folders** (`general/weapons/*.svg`,
/// `general/ui/*.svg`, …). The picker exposes those categories in a dropdown.
/// Files loose directly in `general/` are "Uncategorized"; nesting deeper than
/// one level keeps the *top* sub-folder as the category.
///
/// Each entry is `(key, label, category, bytes)`:
///  - `key`      = the file stem (e.g. `shared_gyro`). Persisted in saved
///                 patches' `macro_ports.icon`, so it must be stable AND unique
///                 across all sub-folders — renaming a file changes the key and
///                 orphans that reference. A duplicate stem emits a build
///                 warning (first one wins at lookup).
///  - `label`    = the stem prettified for hover text. Cosmetic.
///  - `category` = the prettified sub-folder name (drives the picker dropdown).
fn gen_macro_icons(manifest_dir: &str, out_dir: &str) {
    let general_dir = PathBuf::from(manifest_dir).join("../../app/assets/general");
    let out_file = PathBuf::from(out_dir).join("generated_macro_icons.rs");

    println!("cargo:rerun-if-changed=../../app/assets/general");

    // (key, label, category, abs path).
    let mut entries: Vec<(String, String, String, PathBuf)> = Vec::new();
    if general_dir.is_dir() {
        collect_icons(&general_dir, None, &mut entries);
        // Group by category, then key — the picker iterates in this order, so
        // the "All" view clusters each category into a contiguous block.
        entries.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    }

    // Warn on duplicate keys: the key is persisted in patches, so two SVGs
    // sharing a stem across folders shadow each other (first wins).
    {
        let mut seen = std::collections::HashSet::new();
        for (key, _, cat, _) in &entries {
            if !seen.insert(key.as_str()) {
                println!(
                    "cargo:warning=duplicate icon key '{key}' (in category '{cat}') — \
                     rename one; icon keys must be unique across all general/ sub-folders"
                );
            }
        }
    }

    // Distinct category list for the dropdown (sorted, unique). "All" is added
    // by the UI, not stored here.
    let mut cats: Vec<&str> = entries.iter().map(|e| e.2.as_str()).collect();
    cats.sort_unstable();
    cats.dedup();

    let mut code = String::from(
        "/// Every embedded macro icon: `(key, human label, category, svg bytes)`.\n\
         /// Generated at build time from `app/assets/general/**/*.svg`; the\n\
         /// category is the prettified immediate sub-folder under `general/`.\n\
         pub static ALL_ICONS: &[(&str, &str, &str, &[u8])] = &[\n",
    );
    for (key, label, cat, path) in &entries {
        let path_str = path.to_str().unwrap().replace('\\', "/");
        code.push_str(&format!(
            "    ({key:?}, {label:?}, {cat:?}, include_bytes!({path_str:?})),\n"
        ));
    }
    code.push_str("];\n\n");
    code.push_str(
        "/// Distinct icon categories (prettified sub-folder names), sorted. Drives\n\
         /// the picker's category dropdown; \"All\" is added by the UI.\n\
         pub static ICON_CATEGORIES: &[&str] = &[\n",
    );
    for cat in &cats {
        code.push_str(&format!("    {cat:?},\n"));
    }
    code.push_str("];\n");

    fs::write(&out_file, code).unwrap();
}

/// Recursively collect `.svg` files under `dir`. `category` is `Some` once we
/// have descended into a sub-folder of `general/` (its prettified name); loose
/// files at the top level are `None` → "Uncategorized". Nesting deeper keeps
/// the first sub-folder's category.
fn collect_icons(
    dir: &Path,
    category: Option<&str>,
    out: &mut Vec<(String, String, String, PathBuf)>,
) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Watch each sub-folder so added/removed icons re-trigger the build
            // (a `rerun-if-changed` on the parent alone misses nested edits).
            println!("cargo:rerun-if-changed={}", path.display());
            let this_cat = category.map(str::to_string).unwrap_or_else(|| {
                prettify(path.file_name().and_then(|s| s.to_str()).unwrap_or(""))
            });
            collect_icons(&path, Some(&this_cat), out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("svg") {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let label = prettify(stem);
            let cat = category.unwrap_or("Uncategorized").to_string();
            // Canonicalize so include_bytes! gets an absolute path that
            // survives wherever OUT_DIR ends up.
            if let Ok(abs) = fs::canonicalize(&path) {
                out.push((stem.to_string(), label, cat, abs));
            }
        }
    }
}

/// `shared_gyro_pitch` -> `Shared Gyro Pitch`. Splits on `_`/`-`, title-cases
/// each word.
fn prettify(stem: &str) -> String {
    stem.split(|c| c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
