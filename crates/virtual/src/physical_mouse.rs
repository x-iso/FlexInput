//! Physical-mouse detection for "Pause virtual mouse when you move a physical
//! mouse".
//!
//! Listens to Windows Raw Input, which says which DEVICE every mouse event came
//! from, instead of guessing from where the cursor ended up. The guess paused
//! the virtual mouse for anything that moved the cursor without us: a game
//! re-centring or warping it (on a key press, a menu, a mode change), or our
//! own move landing on the cursor later than the 16 ms it allowed — none of it
//! a physical mouse, all of it a dead virtual mouse for half a second.
//!
//! An event counts as ours, and is ignored, when it comes from
//! * the HIDMaestro virtual mouse — a real HID device, told apart by its
//!   VID/PID (`is_our_virtual_mouse`), or
//! * our own SendInput — no device handle, stamped with enigo's marker in
//!   `dwExtraInfo` (`windows::send_wheel` uses it too).
//!
//! Anything else is someone else's mouse input: a physical mouse, a laptop
//! touchpad (Windows synthesizes its pointer events without a device handle),
//! or another program injecting input — all of which the virtual mouse
//! should make way for.
//!
//! Raw input is registered once per process per device class, so this takes the
//! MOUSE registration from winit, which only ever used it for
//! `DeviceEvent::MouseMotion` (nothing in FlexInput reads it) and registers just
//! once, at startup. winit keeps the keyboard registration.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Mouse movement, in counts, that must add up within `JITTER_WINDOW` before it
/// counts as the user moving a mouse — an optical sensor resting on a noisy
/// surface reports the odd single count on its own.
const JITTER_COUNTS: i32 = 3;
const JITTER_WINDOW: Duration = Duration::from_millis(100);

/// The HIDMaestro virtual mouse's VID and PID range (profiles/mouse.json).
const VIRTUAL_MOUSE_VID: u16 = 0x1209;
const VIRTUAL_MOUSE_PID_MASK: u16 = 0xFFF0;
const VIRTUAL_MOUSE_PID_BASE: u16 = 0xF1E0;

static EPOCH: OnceLock<Instant> = OnceLock::new();
/// µs since `EPOCH` of the last physical-mouse activity, +1 (0 = none yet).
static LAST_ACTIVITY_US: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

fn now_us() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Start listening, once per process. Cheap to call every tick.
fn ensure_listening() {
    if STARTED.swap(true, Ordering::Relaxed) { return; }
    let _ = now_us(); // fix the epoch before the thread can race it
    let spawned = std::thread::Builder::new()
        .name("physical-mouse".into())
        .spawn(listen);
    if let Err(e) = spawned {
        eprintln!("[physical-mouse] couldn't start the listener: {e}");
    }
}

/// Should the virtual mouse's motion yield right now? The user setting, outside
/// mixed mode (see `mouse_suppression_effective`), and another mouse used
/// within the configured window. Starts the listener the first time it's asked
/// with the setting on.
pub fn suppressing() -> bool {
    let (on, release_ms) = crate::mouse_suppression_effective();
    if !on { return false; }
    ensure_listening();
    active_within(Duration::from_millis(release_ms as u64))
}

/// Was a physical mouse used within the last `window`?
fn active_within(window: Duration) -> bool {
    let last = LAST_ACTIVITY_US.load(Ordering::Relaxed);
    last != 0 && now_us().saturating_sub(last - 1) < window.as_micros() as u64
}

fn mark_activity() {
    LAST_ACTIVITY_US.store(now_us() + 1, Ordering::Relaxed);
}

/// Is `vid`/`pid` the HIDMaestro virtual mouse?
fn is_our_virtual_mouse(vid: u16, pid: u16) -> bool {
    vid == VIRTUAL_MOUSE_VID && pid & VIRTUAL_MOUSE_PID_MASK == VIRTUAL_MOUSE_PID_BASE
}

/// Is a device-less (injected or synthesized) event one of ours?
fn is_our_injection(extra_info: u32) -> bool {
    extra_info == enigo::EVENT_MARKER
}

/// Movement jitter filter: a run of movement counts, restarted after a pause.
#[derive(Default)]
struct Jitter {
    sum: i32,
    since: Option<Instant>,
}

impl Jitter {
    /// Add one event's movement; true once the run counts as the user moving.
    fn add(&mut self, dx: i32, dy: i32, now: Instant) -> bool {
        if self.since.map_or(true, |t| now.duration_since(t) > JITTER_WINDOW) {
            self.sum = 0;
            self.since = Some(now);
        }
        self.sum = self.sum.saturating_add(dx.saturating_abs()).saturating_add(dy.saturating_abs());
        self.sum >= JITTER_COUNTS
    }
}

// ── Win32 ────────────────────────────────────────────────────────────────────

type Hwnd = isize;

#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    wnd_proc: unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    icon_sm: isize,
}

#[repr(C)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: [i32; 2],
    private: u32,
}

#[repr(C)]
struct RawInputDevice {
    usage_page: u16,
    usage: u16,
    flags: u32,
    target: Hwnd,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputHeader {
    kind: u32,
    size: u32,
    device: isize,
    wparam: usize,
}

/// RAWMOUSE: `usFlags`, then a 4-aligned union whose button half is
/// `usButtonFlags` + `usButtonData`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawMouse {
    flags: u16,
    _pad: u16,
    button_flags: u16,
    button_data: u16,
    raw_buttons: u32,
    last_x: i32,
    last_y: i32,
    extra_info: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputMouse {
    header: RawInputHeader,
    mouse: RawMouse,
}

#[repr(C)]
struct HiddAttributes {
    size: u32,
    vendor_id: u16,
    product_id: u16,
    version: u16,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex_style: u32, class: *const u16, name: *const u16, style: u32,
        x: i32, y: i32, w: i32, h: i32, parent: Hwnd, menu: isize, instance: isize,
        param: *const std::ffi::c_void,
    ) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn RegisterRawInputDevices(devices: *const RawInputDevice, count: u32, size: u32) -> i32;
    fn GetRawInputData(raw: isize, command: u32, data: *mut std::ffi::c_void, size: *mut u32, header_size: u32) -> u32;
    fn GetRawInputDeviceInfoW(device: isize, command: u32, data: *mut std::ffi::c_void, size: *mut u32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn CreateFileW(
        name: *const u16, access: u32, share: u32, security: *const std::ffi::c_void,
        disposition: u32, flags: u32, template: isize,
    ) -> isize;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[link(name = "hid")]
extern "system" {
    fn HidD_GetAttributes(device: isize, attributes: *mut HiddAttributes) -> u8;
}

const HWND_MESSAGE: Hwnd = -3;
const WM_INPUT: u32 = 0x00FF;
const WM_INPUT_DEVICE_CHANGE: u32 = 0x00FE;
const GIDC_REMOVAL: usize = 2;
const RIDEV_INPUTSINK: u32 = 0x0000_0100;
const RIDEV_DEVNOTIFY: u32 = 0x0000_2000;
const RID_INPUT: u32 = 0x1000_0003;
const RIDI_DEVICENAME: u32 = 0x2000_0007;
const RIM_TYPEMOUSE: u32 = 0;
const MOUSE_MOVE_ABSOLUTE: u16 = 0x01;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE: isize = -1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wnd_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// VID/PID of a raw-input device, read from the HID device it names.
fn device_vid_pid(device: isize) -> Option<(u16, u16)> {
    unsafe {
        let mut len = 0u32;
        GetRawInputDeviceInfoW(device, RIDI_DEVICENAME, std::ptr::null_mut(), &mut len);
        if len == 0 { return None; }
        let mut name = vec![0u16; len as usize + 1];
        if GetRawInputDeviceInfoW(device, RIDI_DEVICENAME, name.as_mut_ptr().cast(), &mut len) == u32::MAX {
            return None;
        }
        // No access rights needed to read a HID device's attributes, which is
        // what lets this open a mouse the system holds exclusively.
        let handle = CreateFileW(name.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(), OPEN_EXISTING, 0, 0);
        if handle == INVALID_HANDLE || handle == 0 { return None; }
        let mut attrs = HiddAttributes {
            size: std::mem::size_of::<HiddAttributes>() as u32,
            vendor_id: 0, product_id: 0, version: 0,
        };
        let ok = HidD_GetAttributes(handle, &mut attrs) != 0;
        CloseHandle(handle as *mut std::ffi::c_void);
        ok.then_some((attrs.vendor_id, attrs.product_id))
    }
}

fn listen() {
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("FlexInputPhysicalMouse");
        let class = WndClassExW {
            cb_size: std::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            wnd_proc,
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
            icon_sm: 0,
        };
        RegisterClassExW(&class);
        let hwnd = CreateWindowExW(0, class_name.as_ptr(), class_name.as_ptr(), 0,
            0, 0, 0, 0, HWND_MESSAGE, 0, instance, std::ptr::null());
        if hwnd == 0 {
            eprintln!("[physical-mouse] couldn't create the listener window");
            return;
        }
        // Mouse only — the keyboard registration stays winit's. INPUTSINK so
        // events arrive while a game, not FlexInput, has focus.
        let device = RawInputDevice {
            usage_page: 0x01,
            usage: 0x02,
            flags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            target: hwnd,
        };
        if RegisterRawInputDevices(&device, 1, std::mem::size_of::<RawInputDevice>() as u32) == 0 {
            eprintln!("[physical-mouse] couldn't register for raw mouse input");
            return;
        }

        // hDevice → "is our virtual mouse", looked up once per device.
        let mut ours: HashMap<isize, bool> = HashMap::new();
        let mut jitter = Jitter::default();
        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            match msg.message {
                WM_INPUT => {
                    let mut raw: RawInputMouse = std::mem::zeroed();
                    let mut size = std::mem::size_of::<RawInputMouse>() as u32;
                    let got = GetRawInputData(msg.lparam, RID_INPUT, (&mut raw as *mut RawInputMouse).cast(),
                        &mut size, std::mem::size_of::<RawInputHeader>() as u32);
                    if got != u32::MAX && raw.header.kind == RIM_TYPEMOUSE {
                        let m = raw.mouse;
                        let device = raw.header.device;
                        let is_ours = if device == 0 {
                            is_our_injection(m.extra_info)
                        } else {
                            *ours.entry(device).or_insert_with(|| {
                                device_vid_pid(device).is_some_and(|(v, p)| is_our_virtual_mouse(v, p))
                            })
                        };
                        if !is_ours {
                            let moved = if m.flags & MOUSE_MOVE_ABSOLUTE != 0 {
                                // Tablets, remote desktop: a position, not a step.
                                // Any report means the device is in use.
                                true
                            } else {
                                jitter.add(m.last_x, m.last_y, Instant::now())
                            };
                            if moved || m.button_flags != 0 {
                                mark_activity();
                            }
                        }
                    }
                }
                WM_INPUT_DEVICE_CHANGE if msg.wparam == GIDC_REMOVAL => {
                    // The handle may be reused for another device.
                    ours.remove(&msg.lparam);
                }
                _ => {}
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_virtual_mouse_is_recognised() {
        assert!(is_our_virtual_mouse(0x1209, 0xF1E6));
        assert!(is_our_virtual_mouse(0x1209, 0xF1E2), "every PID the profile has used");
        assert!(!is_our_virtual_mouse(0x1209, 0x0001));
        assert!(!is_our_virtual_mouse(0x046D, 0xC52B), "a Logitech receiver is physical");
        assert!(is_our_injection(enigo::EVENT_MARKER));
        assert!(!is_our_injection(0), "a touchpad or another program's input");
    }

    #[test]
    fn a_resting_sensor_is_not_a_hand_on_the_mouse() {
        let t0 = Instant::now();
        let mut j = Jitter::default();
        assert!(!j.add(1, 0, t0));
        assert!(!j.add(0, 1, t0 + Duration::from_millis(500)), "lone counts far apart");
        assert!(!j.add(1, 0, t0 + Duration::from_millis(1000)));
        assert!(j.add(2, 1, t0 + Duration::from_millis(1010)), "a real move adds up fast");
    }

    /// The layouts the raw-input calls read through must match Windows' (x64).
    #[test]
    fn win32_layouts() {
        assert_eq!(std::mem::size_of::<RawInputHeader>(), 24);
        assert_eq!(std::mem::size_of::<RawMouse>(), 24);
        assert_eq!(std::mem::size_of::<RawInputMouse>(), 48);
        assert_eq!(std::mem::size_of::<RawInputDevice>(), 16);
        assert_eq!(std::mem::size_of::<WndClassExW>(), 80);
    }
}
