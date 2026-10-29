fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // Route every C `open` in the binary (SDL's among them) through
    // `__wrap_open` in main.rs, so SDL can read a controller FlexInput hid.
    // The target's OS and arch, not the build host's; the arches are the ones
    // `__wrap_open` is defined for.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os == "linux" && matches!(arch.as_str(), "x86_64" | "aarch64") {
        println!("cargo:rustc-link-arg-bin=flexinput=-Wl,--wrap=open");
    }
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // Non-fatal: failing to embed the icon shouldn't break the build
            // (e.g. when llvm-rc / windres aren't available on a fresh box).
            println!("cargo:warning=icon embed failed: {e}");
        }
    }
}
