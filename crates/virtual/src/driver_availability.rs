/// Returns true if the ViGEmBus driver is installed and reachable.
///
/// Debug builds honour an override env var so the driver-missing UI can be
/// exercised without actually uninstalling ViGEmBus:
///   `FLEXINPUT_FAKE_NO_VIGEM=1`  → always report missing.
pub fn vigem_available() -> bool {
    #[cfg(debug_assertions)]
    {
        if fake_driver_missing("FLEXINPUT_FAKE_NO_VIGEM") {
            return false;
        }
    }
    #[cfg(windows)]
    {
        vigem_client::Client::connect().is_ok()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Returns true if the HIDMaestro driver is available (or can be installed by
/// the elevated helper). We report "available" when EITHER the driver is already
/// in the DriverStore OR our bundled helper exe is present next to the app (so a
/// first-run deploy is possible). This drives whether the HIDMaestro output
/// cards are enabled.
///
/// Debug builds honour `FLEXINPUT_FAKE_NO_HIDMAESTRO=1` to force "missing".
pub fn hidmaestro_available() -> bool {
    #[cfg(debug_assertions)]
    {
        if fake_driver_missing("FLEXINPUT_FAKE_NO_HIDMAESTRO") {
            return false;
        }
    }
    #[cfg(windows)]
    {
        if flexinput_hidmaestro::hidmaestro_available() {
            return true;
        }
        // Driver not yet installed — but if the helper is bundled, first-run
        // deploy can install it, so treat the backend as available.
        helper_exe_present()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// True if `hidmaestro_helper.exe` sits next to the running executable.
#[cfg(windows)]
fn helper_exe_present() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("hidmaestro_helper.exe")))
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Debug-only: treat an env var as "driver missing" when set to a truthy
/// value (`1`, `true`, `yes`, `on`, case-insensitive). Anything else — unset,
/// empty, or `0`/`false` — leaves real detection in charge.
#[cfg(debug_assertions)]
pub(crate) fn fake_driver_missing(var: &str) -> bool {
    match std::env::var(var) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}
