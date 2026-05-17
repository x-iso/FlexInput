fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
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
