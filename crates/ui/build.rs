use std::{env, fs, path::PathBuf};

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

/// Generate the Macro Output icon table from every `.svg` in
/// `app/assets/general/`, so the shippable icon set is a property of that
/// folder — not a hardcoded list in `macro_icons.rs`. Dropping in / renaming /
/// removing an SVG changes the pickers on the next build with no code edit.
///
/// Each entry is `(key, label, bytes)`:
///  - `key`   = the file stem (e.g. `shared_gyro`). This is persisted in saved
///              patches' `macro_ports.icon`, so it must be stable — renaming a
///              file changes the key and orphans that reference (same as before,
///              when keys were curated by hand).
///  - `label` = the stem prettified for hover text ("shared_gyro" -> "Shared
///              Gyro"). Cosmetic only.
fn gen_macro_icons(manifest_dir: &str, out_dir: &str) {
    let general_dir = PathBuf::from(manifest_dir).join("../../app/assets/general");
    let out_file = PathBuf::from(out_dir).join("generated_macro_icons.rs");

    println!("cargo:rerun-if-changed=../../app/assets/general");

    let mut entries: Vec<(String, String, PathBuf)> = Vec::new();
    if general_dir.is_dir() {
        for entry in fs::read_dir(&general_dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("svg") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let label = prettify(&stem);
            // Canonicalize so include_bytes! gets an absolute path that
            // survives wherever OUT_DIR ends up.
            if let Ok(abs) = fs::canonicalize(&path) {
                entries.push((stem, label, abs));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let mut code = String::from(
        "/// Every embedded macro icon: `(key, human label, svg bytes)`.\n\
         /// Generated at build time from `app/assets/general/*.svg`.\n\
         pub static ALL_ICONS: &[(&str, &str, &[u8])] = &[\n",
    );
    for (key, label, path) in &entries {
        let path_str = path.to_str().unwrap().replace('\\', "/");
        code.push_str(&format!(
            "    ({key:?}, {label:?}, include_bytes!({path_str:?})),\n"
        ));
    }
    code.push_str("];\n");

    fs::write(&out_file, code).unwrap();
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
