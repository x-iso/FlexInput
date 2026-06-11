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
