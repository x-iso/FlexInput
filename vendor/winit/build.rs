// FLEXINPUT: upstream's cfg aliases, written out. Upstream generates them with
// the `cfg_aliases!` macro, whose expansion trips a future-incompatibility
// warning that cargo hides for registry crates but shows for a vendored one.
// Same aliases, same conditions, same check-cfg declarations.

fn main() {
    // The script doesn't depend on our code.
    println!("cargo:rerun-if-changed=build.rs");

    let var = |name: &str| std::env::var(name).ok();
    let cfg_has = |name: &str, value: &str| {
        var(name).is_some_and(|values| values.split(',').any(|v| v == value))
    };
    let target_os = |os: &str| cfg_has("CARGO_CFG_TARGET_OS", os);
    let feature = |name: &str| var(&format!("CARGO_FEATURE_{name}")).is_some();

    // Systems.
    let android_platform = target_os("android");
    let web_platform = cfg_has("CARGO_CFG_TARGET_FAMILY", "wasm") && target_os("unknown");
    let macos_platform = target_os("macos");
    let ios_platform = target_os("ios");
    let windows_platform = target_os("windows");
    let apple = target_os("ios") || target_os("macos");
    let free_unix =
        var("CARGO_CFG_UNIX").is_some() && !apple && !android_platform && !target_os("emscripten");
    let redox = target_os("redox");

    // Native displays.
    let x11_platform = feature("X11") && free_unix && !redox;
    let wayland_platform = feature("WAYLAND") && free_unix && !redox;
    let orbital_platform = redox;

    for (alias, set) in [
        ("android_platform", android_platform),
        ("web_platform", web_platform),
        ("macos_platform", macos_platform),
        ("ios_platform", ios_platform),
        ("windows_platform", windows_platform),
        ("apple", apple),
        ("free_unix", free_unix),
        ("redox", redox),
        ("x11_platform", x11_platform),
        ("wayland_platform", wayland_platform),
        ("orbital_platform", orbital_platform),
    ] {
        println!("cargo:rustc-check-cfg=cfg({alias})");
        if set {
            println!("cargo:rustc-cfg={alias}");
        }
    }

    // Winit defined cfgs.
    println!("cargo:rustc-check-cfg=cfg(unreleased_changelogs)");
}
