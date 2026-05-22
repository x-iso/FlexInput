//! Windows foreground-process detection and visible-window enumeration.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
        if IsWindowVisible(hwnd) == 0 {
            return 1; // TRUE — continue
        }

        let mut title_buf = [0u16; 256];
        let title_len =
            GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32);
        if title_len == 0 {
            return 1;
        }
        let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);

        if pid == GetCurrentProcessId() {
            return 1; // skip our own process
        }

        if let Some(exe) = exe_name_for_pid(pid) {
            let list = &mut *(lparam as *mut Vec<(String, String)>);
            list.push((exe, title));
        }
        1
    }

    unsafe extern "system" fn enum_cb_full(hwnd: HWND, lparam: LPARAM) -> i32 {
        if IsWindowVisible(hwnd) == 0 { return 1; }
        let mut title_buf = [0u16; 256];
        let title_len = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32);
        if title_len == 0 { return 1; }
        let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == GetCurrentProcessId() { return 1; }
        if let Some((full_path, exe_name)) = exe_full_for_pid(pid) {
            let list = &mut *(lparam as *mut Vec<(String, String, String)>);
            list.push((full_path, exe_name, title));
        }
        1
    }

    fn exe_name_for_pid(pid: u32) -> Option<String> {
        unsafe {
            let handle: HANDLE =
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut buf = [0u16; 260];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(handle);
            if ok == 0 {
                return None;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        }
    }

    fn exe_full_for_pid(pid: u32) -> Option<(String, String)> {
        unsafe {
            let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() { return None; }
            let mut buf = [0u16; 260];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, 1, buf.as_mut_ptr(), &mut len);
            CloseHandle(handle);
            if ok == 0 { return None; }
            let full_path = String::from_utf16_lossy(&buf[..len as usize]);
            let exe_name = std::path::Path::new(&full_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| full_path.clone());
            Some((full_path, exe_name))
        }
    }

    /// Enumerate visible top-level windows, returning `(exe_name, window_title)` pairs.
    /// Deduplicated per exe — the entry with the longest title is kept.
    pub fn enumerate_windows() -> Vec<(String, String)> {
        let mut raw: Vec<(String, String)> = Vec::new();
        unsafe {
            EnumWindows(Some(enum_cb), &mut raw as *mut _ as LPARAM);
        }
        // Deduplicate: per exe_name keep the entry with the longest window title.
        let mut map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (exe, title) in raw {
            let entry = map.entry(exe).or_default();
            if title.len() > entry.len() {
                *entry = title;
            }
        }
        let mut result: Vec<(String, String)> = map.into_iter().collect();
        result.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        result
    }

    /// Enumerate visible top-level windows, returning `(full_path, exe_name, window_title)`.
    /// Deduplicated per full_path — the entry with the longest title is kept.
    /// Used for HidHide whitelist management (requires full paths).
    pub fn enumerate_processes_full() -> Vec<(String, String, String)> {
        let mut raw: Vec<(String, String, String)> = Vec::new();
        unsafe {
            EnumWindows(Some(enum_cb_full), &mut raw as *mut _ as LPARAM);
        }
        let mut map: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        for (full_path, exe_name, title) in raw {
            let entry = map.entry(full_path).or_insert_with(|| (exe_name, String::new()));
            if title.len() > entry.1.len() {
                entry.1 = title;
            }
        }
        let mut result: Vec<(String, String, String)> = map
            .into_iter()
            .map(|(path, (name, title))| (path, name, title))
            .collect();
        result.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        result
    }

    /// Returns the exe filename of the current foreground window's process,
    /// or `None` if FlexInput itself is foreground.
    pub fn foreground_exe() -> Option<String> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == GetCurrentProcessId() {
                return None;
            }
            exe_name_for_pid(pid)
        }
    }

    /// Returns the current foreground HWND as an opaque `isize` token, or
    /// `None` if it belongs to FlexInput's own process (we don't want to
    /// flip-flop to our own subwindows). Used by the pin flip-flop feature.
    pub fn foreground_hwnd() -> Option<isize> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() { return None; }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == GetCurrentProcessId() { return None; }
            Some(hwnd as isize)
        }
    }

    /// Best-effort bring `hwnd` to the foreground. Windows blocks foreground
    /// changes from a process that is not itself foreground (the "focus
    /// stealing prevention" rule); we sidestep it by briefly attaching our
    /// thread input to the target's GUI thread so Windows treats the call
    /// as same-process. Returns true if SetForegroundWindow reported success.
    /// Best-effort bring `hwnd` to both the *top* of the z-order AND give
    /// it input focus. Windows treats "foreground" and "z-order top" as
    /// separate concerns:
    ///   * `SetForegroundWindow` is what gives keyboard focus, but it's
    ///     gated by the focus-stealing-prevention rule and may silently
    ///     fail (flashing the taskbar button instead of raising).
    ///   * `SetWindowPos` with `HWND_TOP` reorders the window stack but
    ///     does NOT change focus.
    /// To consistently flip-flop FlexInput off-top AND surface the target
    /// app, we do BOTH. The `AttachThreadInput` dance temporarily merges
    /// our input queue with the target's so the focus rule passes.
    pub fn bring_hwnd_to_front(hwnd_isize: isize) -> bool {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            IsWindow, IsIconic, ShowWindow, SetForegroundWindow, SetWindowPos,
            SW_RESTORE, GetWindowThreadProcessId as GetWinThreadPid,
            HWND_TOP, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SWP_NOACTIVATE,
        };
        use windows_sys::Win32::System::Threading::{
            AttachThreadInput, GetCurrentThreadId,
        };
        unsafe {
            let hwnd = hwnd_isize as HWND;
            if hwnd.is_null() || IsWindow(hwnd) == 0 { return false; }
            if IsIconic(hwnd) != 0 { ShowWindow(hwnd, SW_RESTORE); }

            // Synthesize an ALT keystroke to reset Windows' focus
            // stealing prevention timer for our process. Without this,
            // SetForegroundWindow can silently fail and only flash the
            // taskbar button.
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
                keybd_event, KEYEVENTF_KEYUP, VK_MENU,
            };
            keybd_event(VK_MENU as u8, 0, 0, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);

            // Raise z-order first (no activation yet). This dislodges
            // FlexInput from the visual top even if the subsequent
            // foreground call is denied.
            SetWindowPos(
                hwnd, HWND_TOP, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );

            let target_tid = GetWinThreadPid(hwnd, std::ptr::null_mut());
            let our_tid    = GetCurrentThreadId();
            let attached   = target_tid != 0
                && target_tid != our_tid
                && AttachThreadInput(our_tid, target_tid, 1) != 0;
            let ok = SetForegroundWindow(hwnd) != 0;
            if attached {
                AttachThreadInput(our_tid, target_tid, 0);
            }
            ok
        }
    }

    /// Drop `hwnd` out of the topmost z-order band synchronously, without
    /// changing focus. We need this on pin-off because `eframe` defers
    /// the `WindowLevel::Normal` viewport command into winit's event
    /// loop — by the time the target-app raise call runs, our window is
    /// still in the topmost band and refuses to give up the top slot.
    pub fn drop_topmost(hwnd_isize: isize) {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_NOTOPMOST,
            SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE,
        };
        unsafe {
            let hwnd = hwnd_isize as HWND;
            if hwnd.is_null() { return; }
            SetWindowPos(
                hwnd, HWND_NOTOPMOST, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

}

#[cfg(windows)]
pub use imp::{
    enumerate_windows, enumerate_processes_full, foreground_exe,
    foreground_hwnd, bring_hwnd_to_front,
    drop_topmost,
};

#[cfg(not(windows))]
pub fn enumerate_windows() -> Vec<(String, String)> {
    vec![]
}

#[cfg(not(windows))]
pub fn enumerate_processes_full() -> Vec<(String, String, String)> {
    vec![]
}

#[cfg(not(windows))]
pub fn foreground_exe() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn foreground_hwnd() -> Option<isize> { None }

#[cfg(not(windows))]
pub fn bring_hwnd_to_front(_hwnd: isize) -> bool { false }

#[cfg(not(windows))]
pub fn drop_topmost(_hwnd: isize) {}

