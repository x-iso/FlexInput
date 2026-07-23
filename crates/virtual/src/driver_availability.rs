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
/// A **half-installed** DriverStore reports `false`: a first-run deploy cannot
/// repair it (installing over the top leaves the stranded package), so treating
/// it as available is what produced "everything connected, nothing emitted".
/// Call [`hidmaestro_status`] to tell that case apart from a clean absence.
///
/// Debug builds honour `FLEXINPUT_FAKE_NO_HIDMAESTRO=1` to force "missing".
pub fn hidmaestro_available() -> bool {
    matches!(hidmaestro_status(), HidMaestroStatus::Ok)
}

/// Why the HIDMaestro backend is unusable, when it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidMaestroStatus {
    /// Installed and complete, or installable via the bundled helper.
    Ok,
    /// Not installed and no helper to install it.
    Missing,
    /// Half-installed: one of the two driver packages is in the DriverStore
    /// without the other. Output silently does nothing in this state, and the
    /// stranded package makes WUDFHost fault, so it must be reported rather
    /// than folded into `Missing` — a reinstall over the top will not fix it.
    HalfInstalled { has_main: bool, has_xusb: bool },
}

impl HidMaestroStatus {
    /// One-line, user-facing explanation. `None` when [`HidMaestroStatus::Ok`].
    pub fn message(self) -> Option<String> {
        match self {
            HidMaestroStatus::Ok => None,
            HidMaestroStatus::Missing => {
                Some("HIDMaestro driver is not installed.".to_string())
            }
            HidMaestroStatus::HalfInstalled { has_main, .. } => {
                let missing = if has_main {
                    "the XInput companion package is missing"
                } else {
                    "the main driver package is missing"
                };
                Some(format!(
                    "HIDMaestro is half-installed — {missing}. Virtual pads will \
                     accept connections but emit nothing. Use 'Reinstall drivers' \
                     to remove both packages and install cleanly."
                ))
            }
        }
    }
}

/// Classify the HIDMaestro backend's usability, distinguishing a half-installed
/// DriverStore from a clean absence. [`hidmaestro_available`] collapses both to
/// `false`/`true` for callers that only gate UI enablement; use this when the
/// user needs to be told *why*.
pub fn hidmaestro_status() -> HidMaestroStatus {
    #[cfg(debug_assertions)]
    {
        if fake_driver_missing("FLEXINPUT_FAKE_NO_HIDMAESTRO") {
            return HidMaestroStatus::Missing;
        }
        if fake_driver_missing("FLEXINPUT_FAKE_HALF_HIDMAESTRO") {
            return HidMaestroStatus::HalfInstalled { has_main: false, has_xusb: true };
        }
    }
    #[cfg(windows)]
    {
        match flexinput_hidmaestro::driver_state() {
            flexinput_hidmaestro::DriverState::Complete => HidMaestroStatus::Ok,
            flexinput_hidmaestro::DriverState::Partial { has_main, has_xusb } => {
                HidMaestroStatus::HalfInstalled { has_main, has_xusb }
            }
            flexinput_hidmaestro::DriverState::Missing => {
                if helper_exe_present() {
                    HidMaestroStatus::Ok
                } else {
                    HidMaestroStatus::Missing
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        HidMaestroStatus::Missing
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
