use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let sub_patches_dir = PathBuf::from(&manifest_dir)
        .join("../../app/assets/sub-patches");

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_file = PathBuf::from(&out_dir).join("generated_presets.rs");

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
