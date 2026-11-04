//! Display-synced output: when the display the game is on shows a frame, so
//! the virtual devices can report on a grid locked to the frames.
//!
//! A game reads the virtual mouse once a frame and sums what arrived since the
//! last. Reported at a rate that doesn't divide the frame rate, the number of
//! reports in a frame changes from frame to frame, and with it how far a held
//! stick or gyro moves the pointer in that frame: motion judders at the beat
//! between the two rates. A very high output rate only hides it; a few
//! reports a frame, on a grid locked to the frames, remove it.
//!
//! The frames come from the display's vblank events, waited for on a thread
//! of its own: on Windows `D3DKMTWaitForVerticalBlankEvent` (user mode, no
//! elevation) on the display under the foreground window, on Linux DRM's
//! (see `drm`). Measured on Windows with a 180 Hz FreeSync laptop panel
//! (2026-11), borderless and exclusive fullscreen alike: one event per
//! refresh, 0.37 ± 0.03 ms before the panel starts drawing it, about one in a
//! thousand missing. Under VRR the panel refreshes when a frame arrives, so
//! the events follow the game's frames. Below the panel's range the driver
//! shows every frame twice, evenly, and the grid follows that as it would a
//! doubled frame rate (the reports per frame stay even); for a moment after a
//! frame-rate change it shows only some frames twice, and those repeats are
//! skipped.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Whether this platform has a display clock. Elsewhere [`next_tick`] never
/// has a grid and output stays on its fixed schedule.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "linux"));

/// The slowest frame rate the catch-up clamps allow for on the grid: a frame
/// can take far longer than the display's fastest refresh.
pub const SLOWEST_FRAME_HZ: f32 = 30.0;

/// How display-synced output is set up; [`sync`] is `None` while it's off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSync {
    /// Reports per frame.
    pub per_frame: u32,
    /// Where in the frame they sit: the first this share of a frame after the
    /// frame's vblank event, the rest evenly after it.
    pub phase: f32,
    /// Under VRR, follow the frames as they come. Off, report at the display's
    /// maximum rate as if it were fixed.
    pub follow_vrr: bool,
}

// 0 = off. Read by the I/O thread and the SendInput mouse thread every pass.
static PER_FRAME: AtomicU32 = AtomicU32::new(0);
static PHASE_BITS: AtomicU32 = AtomicU32::new(0);
static FOLLOW_VRR: AtomicBool = AtomicBool::new(true);

/// Turn display-synced output on (starting the clock) or off. Called by the UI
/// every frame with the current settings.
pub fn set_sync(s: Option<OutputSync>) {
    match s {
        Some(s) => {
            PHASE_BITS.store(s.phase.clamp(0.0, 0.99).to_bits(), Ordering::Relaxed);
            FOLLOW_VRR.store(s.follow_vrr, Ordering::Relaxed);
            PER_FRAME.store(s.per_frame.clamp(1, 16), Ordering::Relaxed);
            if SUPPORTED {
                start();
            }
        }
        None => PER_FRAME.store(0, Ordering::Relaxed),
    }
}

/// The current setup, `None` while display-synced output is off.
pub fn sync() -> Option<OutputSync> {
    let per_frame = PER_FRAME.load(Ordering::Relaxed);
    (per_frame > 0).then(|| OutputSync {
        per_frame,
        phase: f32::from_bits(PHASE_BITS.load(Ordering::Relaxed)),
        follow_vrr: FOLLOW_VRR.load(Ordering::Relaxed),
    })
}

/// The followed display's refresh rate (its maximum under VRR), the last one
/// seen; 0 until the clock has read a display.
static NOMINAL_HZ: AtomicU32 = AtomicU32::new(0);

/// The display's refresh rate, its maximum under VRR. Only the clock thread
/// reads the display: this is called from the output threads every pass.
pub fn nominal_hz() -> Option<u32> {
    let hz = NOMINAL_HZ.load(Ordering::Relaxed);
    (hz > 0).then_some(hz)
}

/// The output rate display-synced output stands for: `per_frame` reports at
/// the display's refresh rate (60 Hz when unknown). Under VRR the reports go
/// out per frame shown, so as many as this at most.
pub fn output_hz(per_frame: u32) -> u32 {
    per_frame.max(1) * nominal_hz().unwrap_or(60)
}

// ── The grid ─────────────────────────────────────────────────────────────────

/// What the clock thread publishes: the grid's origin and step.
#[derive(Clone, Copy)]
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
struct Beat {
    /// The last frame's event, the grid's origin.
    anchor: Instant,
    /// The frame period the grid steps by, in seconds.
    period: f64,
    /// When the latest event of any kind arrived.
    last_event: Instant,
    /// The rate frames arrive at, from the median interval.
    frame_hz: f32,
    vrr: bool,
}

static BEAT: Mutex<Option<Beat>> = Mutex::new(None);
/// The display followed, as Windows names it ("DISPLAY1"); empty for none.
static DISPLAY: Mutex<String> = Mutex::new(String::new());

/// No event for this long (a display asleep, a mode change): the grid is
/// stale and output falls back to its fixed schedule.
const STALE: Duration = Duration::from_millis(250);

/// The first report time after `after` on the grid: `per_frame` reports a
/// frame, starting `phase` of a frame after the frame's event. `None` while
/// the clock has no frames to go by (off, starting, no events, not Windows).
pub fn next_tick(after: Instant, s: OutputSync) -> Option<Instant> {
    let b = (*BEAT.lock().ok()?)?;
    if b.last_event.elapsed() > STALE {
        return None;
    }
    Some(grid_after(b.anchor, b.period, s.per_frame, s.phase, after))
}

fn grid_after(anchor: Instant, period: f64, per_frame: u32, phase: f32, after: Instant) -> Instant {
    let step = period / per_frame.max(1) as f64;
    let first = phase.clamp(0.0, 0.99) as f64 * period;
    let k = ((secs_since(after, anchor) - first) / step).floor() + 1.0;
    let t = offset(anchor, first + k * step);
    // Rounding can land it on `after` itself.
    if t <= after { offset(anchor, first + (k + 1.0) * step) } else { t }
}

/// `t - from` in seconds, negative when `t` is earlier.
fn secs_since(t: Instant, from: Instant) -> f64 {
    if t >= from {
        (t - from).as_secs_f64()
    } else {
        -(from - t).as_secs_f64()
    }
}

fn offset(t: Instant, secs: f64) -> Instant {
    if secs >= 0.0 {
        t + Duration::from_secs_f64(secs)
    } else {
        t.checked_sub(Duration::from_secs_f64(-secs)).unwrap_or(t)
    }
}

/// What the Settings readout shows.
#[derive(Clone, Debug)]
pub struct Status {
    /// The display followed ("DISPLAY1"); empty when none was found.
    pub display: String,
    /// Its refresh rate, its maximum under VRR; 0 when unknown.
    pub nominal_hz: u32,
    /// The rate frames arrive at.
    pub frame_hz: f32,
    /// The panel refreshes off its fixed cadence: VRR at work.
    pub vrr: bool,
    /// Frames are arriving and the grid is locked to them.
    pub live: bool,
}

/// `None` while display-synced output is off.
pub fn status() -> Option<Status> {
    sync()?;
    let display = DISPLAY.lock().map(|d| d.clone()).unwrap_or_default();
    let beat = BEAT.lock().ok().and_then(|b| *b);
    Some(Status {
        display,
        nominal_hz: NOMINAL_HZ.load(Ordering::Relaxed),
        frame_hz: beat.map_or(0.0, |b| b.frame_hz),
        vrr: beat.is_some_and(|b| b.vrr),
        live: beat.is_some_and(|b| b.last_event.elapsed() <= STALE),
    })
}

// ── Exact waits ──────────────────────────────────────────────────────────────

/// Waits out to an exact instant: the bulk in a timer wait, the last stretch
/// spun, the spun stretch sized from how late the timer has been waking.
pub struct PreciseWait {
    /// How late the timer wakes, in seconds (moving average).
    late: f64,
}

impl Default for PreciseWait {
    fn default() -> Self {
        Self { late: 100e-6 }
    }
}

impl PreciseWait {
    /// How much before the instant the timer wait ends and the spin starts.
    pub fn margin(&self) -> Duration {
        Duration::from_secs_f64((2.0 * self.late + 50e-6).clamp(50e-6, 500e-6))
    }

    /// Block until `t`, `coarse` sleeping the bulk of it.
    pub fn until(&mut self, t: Instant, mut coarse: impl FnMut(Duration)) {
        let now = Instant::now();
        if t <= now {
            return;
        }
        let margin = self.margin();
        let left = t - now;
        if left > margin {
            let d = left - margin;
            coarse(d);
            // A wake a millisecond late is a preemption, not the timer: it
            // would blow the margin up.
            let late = Instant::now().saturating_duration_since(now + d).as_secs_f64().min(1e-3);
            self.late += (late - self.late) * 0.05;
        }
        while Instant::now() < t {
            std::hint::spin_loop();
        }
    }
}

// ── Turning events into a grid ───────────────────────────────────────────────

/// Recent intervals the period is the median of: robust to a missed event, a
/// repeated frame or a slow one, and quick to follow a real change.
#[cfg(any(windows, target_os = "linux", test))]
const RING: usize = 15;

/// Turns the raw vblank events into the grid's origin and period.
#[cfg(any(windows, target_os = "linux", test))]
struct Tracker {
    /// The display mode's period, in seconds.
    nominal: Option<f64>,
    ring: [f64; RING],
    len: usize,
    pos: usize,
    last_raw: Option<Instant>,
    anchor: Option<Instant>,
}

#[cfg(any(windows, target_os = "linux", test))]
impl Tracker {
    fn new(nominal_hz: u32) -> Self {
        Self {
            nominal: (nominal_hz > 0).then(|| 1.0 / nominal_hz as f64),
            ring: [0.0; RING],
            len: 0,
            pos: 0,
            last_raw: None,
            anchor: None,
        }
    }

    /// The median recent interval, the display mode's period until there are
    /// a few.
    fn period(&self) -> Option<f64> {
        if self.len < 3 {
            return self.nominal;
        }
        let mut v = self.ring;
        let v = &mut v[..self.len];
        v.sort_unstable_by(f64::total_cmp);
        Some(v[self.len / 2])
    }

    /// Whether the panel refreshes off its fixed cadence: over 30% of recent
    /// intervals aren't a whole number of the mode's periods.
    fn vrr(&self) -> bool {
        let Some(nominal) = self.nominal else { return false };
        if self.len < 8 {
            return false;
        }
        let off = self.ring[..self.len]
            .iter()
            .filter(|&&d| {
                let q = d / nominal;
                q.round() < 1.0 || (q - q.round()).abs() > 0.06
            })
            .count();
        off * 10 > self.len * 3
    }

    fn on_event(&mut self, e: Instant, follow_vrr: bool) {
        if let Some(prev) = self.last_raw {
            let d = (e - prev).as_secs_f64();
            // Longer is the display idling or asleep, not a frame.
            if d < 0.25 {
                self.ring[self.pos] = d;
                self.pos = (self.pos + 1) % RING;
                self.len = (self.len + 1).min(RING);
            }
        }
        self.last_raw = Some(e);
        let Some(period) = self.period() else {
            self.anchor = Some(e);
            return;
        };
        match self.anchor {
            // VRR not followed: run on at the maximum rate from where it was.
            Some(_) if !follow_vrr && self.vrr() => {}
            // Sooner than most of a frame after the last: a repeated frame.
            Some(a) if (e - a).as_secs_f64() < 0.6 * period => {}
            _ => self.anchor = Some(e),
        }
    }

    fn beat(&self, last_event: Instant, follow_vrr: bool) -> Option<Beat> {
        let anchor = self.anchor?;
        let measured = self.period()?;
        let vrr = self.vrr();
        let period = if vrr && !follow_vrr { self.nominal.unwrap_or(measured) } else { measured };
        Some(Beat { anchor, period, last_event, frame_hz: (1.0 / measured) as f32, vrr })
    }
}

// ── The clock thread ─────────────────────────────────────────────────────────

static RUNNING: AtomicBool = AtomicBool::new(false);

fn start() {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("display-clock".into())
        .spawn(|| loop {
            #[cfg(any(windows, target_os = "linux"))]
            run();
            if let Ok(mut b) = BEAT.lock() {
                *b = None;
            }
            RUNNING.store(false, Ordering::Release);
            // Turned back on between the loop's last check and the flag going
            // down: no one else will start it.
            if PER_FRAME.load(Ordering::Relaxed) == 0 || RUNNING.swap(true, Ordering::AcqRel) {
                break;
            }
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
    }
}

/// How often the clock looks for the display to follow, and how often it
/// rereads that display's mode.
#[cfg(any(windows, target_os = "linux"))]
const RECHECK_DISPLAY: Duration = Duration::from_millis(500);
#[cfg(any(windows, target_os = "linux"))]
const RECHECK_MODE: Duration = Duration::from_secs(2);

/// What a platform's display lookup decided.
#[cfg(any(windows, target_os = "linux"))]
enum Found {
    /// Go on with the display followed.
    Keep,
    /// Follow this one instead (none: nothing to follow).
    Switch(Option<platform::Display>),
}

#[cfg(windows)]
use win as platform;
#[cfg(target_os = "linux")]
use drm as platform;

/// Waits for the followed display's vblank events until display-synced output
/// goes off. A wait blocks until the next refresh, so the display is looked
/// for again between events.
#[cfg(any(windows, target_os = "linux"))]
fn run() {
    platform::raise_priority();
    let mut cur: Option<(platform::Display, platform::Source)> = None;
    let mut tracker = Tracker::new(0);
    let mut next_display = Instant::now();
    let mut next_mode = Instant::now();
    let mut errors = 0u32;
    while PER_FRAME.load(Ordering::Relaxed) > 0 {
        let now = Instant::now();
        if now >= next_display {
            next_display = now + RECHECK_DISPLAY;
            // The same display's mode read again: a refresh rate change
            // resets the tracker.
            let reread = now >= next_mode;
            if reread {
                next_mode = now + RECHECK_MODE;
            }
            if let Found::Switch(d) = platform::find(cur.as_mut().map(|(d, _)| d), reread) {
                cur = d.and_then(|d| platform::Source::open(&d).map(|s| (d, s)));
                let hz = cur.as_ref().map_or(0, |(d, _)| d.nominal_hz);
                tracker = Tracker::new(hz);
                if hz > 0 {
                    NOMINAL_HZ.store(hz, Ordering::Relaxed);
                }
                if let Ok(mut name) = DISPLAY.lock() {
                    *name = cur.as_ref().map(|(d, _)| d.name.clone()).unwrap_or_default();
                }
                if let Ok(mut b) = BEAT.lock() {
                    *b = None;
                }
            }
        }
        let Some((_, src)) = &cur else {
            std::thread::sleep(RECHECK_DISPLAY);
            continue;
        };
        let Some(e) = src.wait() else {
            // A display going away (or off) fails its waits: look again.
            errors += 1;
            if errors >= 10 {
                errors = 0;
                cur = None;
                next_display = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        errors = 0;
        let follow = FOLLOW_VRR.load(Ordering::Relaxed);
        tracker.on_event(e, follow);
        if let Ok(mut b) = BEAT.lock() {
            *b = tracker.beat(e, follow);
        }
    }
}

#[cfg(windows)]
mod win {
    use windows_sys::Wdk::Graphics::Direct3D::{
        D3DKMTCloseAdapter, D3DKMTOpenAdapterFromGdiDisplayName, D3DKMTWaitForVerticalBlankEvent,
        D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMGDIDISPLAYNAME, D3DKMT_WAITFORVERTICALBLANKEVENT,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS,
        HMONITOR, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    use std::time::Instant;
    use super::Found;

    pub(super) struct Display {
        monitor: HMONITOR,
        device: [u16; 32],
        pub name: String,
        /// The mode's refresh rate; 0 when Windows reports the hardware default.
        pub nominal_hz: u32,
    }

    /// The display under the foreground window: the game's, while it's played.
    pub(super) fn find(cur: Option<&mut Display>, reread: bool) -> Found {
        let want = foreground_monitor();
        let same = cur.as_ref().is_some_and(|d| d.monitor == want);
        if same && !reread {
            return Found::Keep;
        }
        match (cur, display_of(want)) {
            (Some(d), Some(f)) if d.device == f.device && d.nominal_hz == f.nominal_hz => {
                // The same display, perhaps under a new handle.
                d.monitor = f.monitor;
                Found::Keep
            }
            // Unreadable: keep the one followed only if it's still the one in front.
            (Some(_), None) if same => Found::Keep,
            (_, f) => Found::Switch(f),
        }
    }

    /// The monitor under the foreground window, the primary when there is none.
    fn foreground_monitor() -> HMONITOR {
        // SAFETY: plain FFI; a null window asks for the primary.
        unsafe {
            let hwnd = GetForegroundWindow();
            let flag = if hwnd.is_null() { MONITOR_DEFAULTTOPRIMARY } else { MONITOR_DEFAULTTONEAREST };
            MonitorFromWindow(hwnd, flag)
        }
    }

    fn display_of(monitor: HMONITOR) -> Option<Display> {
        if monitor.is_null() {
            return None;
        }
        // SAFETY: FFI with sized out-structs; MONITORINFOEXW starts with the
        // MONITORINFO GetMonitorInfoW takes, its cbSize telling it which.
        unsafe {
            let mut mi: MONITORINFOEXW = core::mem::zeroed();
            mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>()) == 0 {
                return None;
            }
            let mut dm: DEVMODEW = core::mem::zeroed();
            dm.dmSize = size_of::<DEVMODEW>() as u16;
            let hz = if EnumDisplaySettingsW(mi.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) != 0 {
                dm.dmDisplayFrequency
            } else {
                0
            };
            let n = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
            let name = String::from_utf16_lossy(&mi.szDevice[..n]).trim_start_matches(r"\\.\").to_string();
            Some(Display { monitor, device: mi.szDevice, name, nominal_hz: if hz > 1 { hz } else { 0 } })
        }
    }

    /// A display's vblank events.
    pub(super) struct Source {
        adapter: u32,
        vidpn: u32,
    }

    impl Source {
        pub fn open(d: &Display) -> Option<Self> {
            let mut o = D3DKMT_OPENADAPTERFROMGDIDISPLAYNAME { DeviceName: d.device, ..Default::default() };
            // SAFETY: FFI with a valid in/out struct.
            if unsafe { D3DKMTOpenAdapterFromGdiDisplayName(&mut o) } != 0 {
                return None;
            }
            Some(Self { adapter: o.hAdapter, vidpn: o.VidPnSourceId })
        }

        /// Block until the next vblank and say when it was: as it wakes, so
        /// the thread runs time-critical. `None` on an error.
        pub fn wait(&self) -> Option<Instant> {
            let w = D3DKMT_WAITFORVERTICALBLANKEVENT { hAdapter: self.adapter, hDevice: 0, VidPnSourceId: self.vidpn };
            // SAFETY: FFI on an adapter we opened.
            (unsafe { D3DKMTWaitForVerticalBlankEvent(&w) } == 0).then(Instant::now)
        }
    }

    impl Drop for Source {
        fn drop(&mut self) {
            // SAFETY: FFI; the adapter was opened by us and isn't used after.
            unsafe {
                D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: self.adapter });
            }
        }
    }

    /// Events are consumed as soon as they're signalled; a late wake shifts
    /// the grid by as much.
    pub(super) fn raise_priority() {
        // SAFETY: plain FFI on the current thread's pseudo-handle.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        }
    }
}

/// DRM's vblank events: `DRM_IOCTL_WAIT_VBLANK` on a card's primary node,
/// which any client may wait on without being the display master. Each reply
/// carries the kernel's stamp of the vblank, on CLOCK_MONOTONIC (Instant's
/// clock), so a late wake doesn't move the grid. Wayland has no foreground
/// window to ask, so the display followed is the lit one with the highest
/// refresh rate: the game's, wherever one display is the gaming one.
#[cfg(target_os = "linux")]
mod drm {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use super::Found;

    /// `_IOWR('d', nr, size)` and `_IO('d', nr)`, with the asm-generic layout
    /// (see linux/uinput.rs).
    mod ioc {
        use super::{CardRes, GetConnector, GetEncoder, ModeCrtc, WaitVblank};
        use std::mem::size_of;
        const fn ioc(dir: u64, nr: u64, size: usize) -> u64 {
            (dir << 30) | ((size as u64) << 16) | ((b'd' as u64) << 8) | nr
        }
        const RW: u64 = 3;
        pub const DROP_MASTER: u64 = ioc(0, 0x1f, 0);
        pub const WAIT_VBLANK: u64 = ioc(RW, 0x3a, size_of::<WaitVblank>());
        pub const GETRESOURCES: u64 = ioc(RW, 0xa0, size_of::<CardRes>());
        pub const GETCRTC: u64 = ioc(RW, 0xa1, size_of::<ModeCrtc>());
        pub const GETENCODER: u64 = ioc(RW, 0xa6, size_of::<GetEncoder>());
        pub const GETCONNECTOR: u64 = ioc(RW, 0xa7, size_of::<GetConnector>());
    }

    // <drm/drm_mode.h>, <drm/drm.h>
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)] // laid out for the kernel
    struct CardRes {
        fb_id_ptr: u64,
        crtc_id_ptr: u64,
        connector_id_ptr: u64,
        encoder_id_ptr: u64,
        count_fbs: u32,
        count_crtcs: u32,
        count_connectors: u32,
        count_encoders: u32,
        min_width: u32,
        max_width: u32,
        min_height: u32,
        max_height: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    #[allow(dead_code)] // laid out for the kernel
    struct ModeInfo {
        clock: u32,
        hdisplay: u16,
        hsync_start: u16,
        hsync_end: u16,
        htotal: u16,
        hskew: u16,
        vdisplay: u16,
        vsync_start: u16,
        vsync_end: u16,
        vtotal: u16,
        vscan: u16,
        vrefresh: u32,
        flags: u32,
        kind: u32,
        name: [u8; 32],
    }

    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)] // laid out for the kernel
    struct ModeCrtc {
        set_connectors_ptr: u64,
        count_connectors: u32,
        crtc_id: u32,
        fb_id: u32,
        x: u32,
        y: u32,
        gamma_size: u32,
        mode_valid: u32,
        mode: ModeInfo,
    }

    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)] // laid out for the kernel
    struct GetEncoder {
        encoder_id: u32,
        encoder_type: u32,
        crtc_id: u32,
        possible_crtcs: u32,
        possible_clones: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)] // laid out for the kernel
    struct GetConnector {
        encoders_ptr: u64,
        modes_ptr: u64,
        props_ptr: u64,
        prop_values_ptr: u64,
        count_modes: u32,
        count_props: u32,
        count_encoders: u32,
        encoder_id: u32,
        connector_id: u32,
        connector_type: u32,
        connector_type_id: u32,
        connection: u32,
        mm_width: u32,
        mm_height: u32,
        subpixel: u32,
        pad: u32,
    }

    /// `union drm_wait_vblank` as its reply; the request's `signal` (an
    /// unsigned long) sits where `tval_sec` does.
    #[repr(C)]
    #[allow(dead_code)] // laid out for the kernel
    struct WaitVblank {
        kind: u32,
        sequence: u32,
        tval_sec: libc::c_long,
        tval_usec: libc::c_long,
    }

    const _: () = assert!(size_of::<CardRes>() == 64);
    const _: () = assert!(size_of::<ModeInfo>() == 68);
    const _: () = assert!(size_of::<ModeCrtc>() == 104);
    const _: () = assert!(size_of::<GetEncoder>() == 20);
    const _: () = assert!(size_of::<GetConnector>() == 80);

    const VBLANK_RELATIVE: u32 = 0x1;
    const VBLANK_HIGH_CRTC_SHIFT: u32 = 1;
    const VBLANK_HIGH_CRTC_MASK: u32 = 0x3e;
    const CONNECTED: u32 = 1;

    /// `DRM_MODE_CONNECTOR_*`, named as the kernel names connectors.
    const CONNECTOR_TYPES: [&str; 21] = [
        "Unknown", "VGA", "DVI-I", "DVI-D", "DVI-A", "Composite", "SVIDEO", "LVDS", "Component",
        "DIN", "DP", "HDMI-A", "HDMI-B", "TV", "eDP", "Virtual", "DSI", "DPI", "Writeback", "SPI", "USB",
    ];

    fn ioctl<T>(f: &File, req: u64, arg: &mut T) -> bool {
        // SAFETY: `arg` is the #[repr(C)] struct `req` is sized for.
        unsafe { libc::ioctl(f.as_raw_fd(), req as _, arg as *mut T) == 0 }
    }

    /// A card's primary node, never as its master: whoever opens a card with
    /// no master becomes it (a compositor restarting, a VT switch), which would
    /// lock the compositor out.
    fn open_card(path: &Path) -> Option<File> {
        let f = std::fs::OpenOptions::new().read(true).write(true).open(path).ok()?;
        // SAFETY: an argless ioctl on our own fd; fails harmlessly when this
        // fd isn't the master.
        unsafe { libc::ioctl(f.as_raw_fd(), ioc::DROP_MASTER as _) };
        Some(f)
    }

    pub(super) struct Display {
        card: PathBuf,
        crtc_index: u32,
        crtc_id: u32,
        pub name: String,
        pub nominal_hz: u32,
    }

    /// Looked for again on every mode reread only: there's no foreground
    /// display to watch for between them.
    pub(super) fn find(cur: Option<&mut Display>, reread: bool) -> Found {
        if cur.is_some() && !reread {
            return Found::Keep;
        }
        match (cur, fastest()) {
            (Some(d), Some(f)) if d.card == f.card && d.crtc_id == f.crtc_id && d.nominal_hz == f.nominal_hz => Found::Keep,
            (_, f) => Found::Switch(f),
        }
    }

    /// Every lit CRTC on every card: the one refreshing fastest.
    fn fastest() -> Option<Display> {
        let mut best: Option<Display> = None;
        for n in 0..16 {
            let card = PathBuf::from(format!("/dev/dri/card{n}"));
            let Some(f) = open_card(&card) else { continue };
            for d in displays(&f, &card) {
                if best.as_ref().is_none_or(|b| d.nominal_hz > b.nominal_hz) {
                    best = Some(d);
                }
            }
        }
        best
    }

    fn displays(f: &File, card: &Path) -> Vec<Display> {
        let mut res = CardRes::default();
        if !ioctl(f, ioc::GETRESOURCES, &mut res) || res.count_crtcs == 0 {
            return Vec::new();
        }
        let mut crtcs = vec![0u32; res.count_crtcs as usize];
        let mut conns = vec![0u32; res.count_connectors as usize];
        let mut res = CardRes {
            crtc_id_ptr: crtcs.as_mut_ptr() as u64,
            count_crtcs: crtcs.len() as u32,
            connector_id_ptr: conns.as_mut_ptr() as u64,
            count_connectors: conns.len() as u32,
            ..Default::default()
        };
        if !ioctl(f, ioc::GETRESOURCES, &mut res) {
            return Vec::new();
        }
        // A list that grew between the calls is filled only as far as asked.
        crtcs.truncate((res.count_crtcs as usize).min(crtcs.len()));
        conns.truncate((res.count_connectors as usize).min(conns.len()));

        // Each lit CRTC's connector, for its name. Asked with no room for
        // modes, which a non-master's ask never probes for.
        let mut names: Vec<(u32, String)> = Vec::new();
        for &id in &conns {
            let mut c = GetConnector { connector_id: id, ..Default::default() };
            if !ioctl(f, ioc::GETCONNECTOR, &mut c) || c.connection != CONNECTED || c.encoder_id == 0 {
                continue;
            }
            let mut e = GetEncoder { encoder_id: c.encoder_id, ..Default::default() };
            if !ioctl(f, ioc::GETENCODER, &mut e) || e.crtc_id == 0 {
                continue;
            }
            let kind = CONNECTOR_TYPES.get(c.connector_type as usize).copied().unwrap_or("Unknown");
            names.push((e.crtc_id, format!("{kind}-{}", c.connector_type_id)));
        }

        let card_name = card.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        crtcs.iter().enumerate().filter_map(|(i, &id)| {
            let mut c = ModeCrtc { crtc_id: id, ..Default::default() };
            if !ioctl(f, ioc::GETCRTC, &mut c) || c.mode_valid == 0 {
                return None;
            }
            let m = c.mode;
            let hz = if m.vrefresh > 0 {
                m.vrefresh
            } else if m.htotal > 0 && m.vtotal > 0 {
                (m.clock as u64 * 1000 / (m.htotal as u64 * m.vtotal as u64)) as u32
            } else {
                0
            };
            let name = names.iter().find(|n| n.0 == id)
                .map_or_else(|| format!("{card_name} CRTC {i}"), |n| n.1.clone());
            Some(Display { card: card.to_path_buf(), crtc_index: i as u32, crtc_id: id, name, nominal_hz: hz })
        }).collect()
    }

    /// A CRTC's vblank events.
    pub(super) struct Source {
        file: File,
        kind: u32,
    }

    impl Source {
        pub fn open(d: &Display) -> Option<Self> {
            let file = open_card(&d.card)?;
            // CRTC 0 is the default; the rest go in the high-CRTC field.
            let crtc = (d.crtc_index << VBLANK_HIGH_CRTC_SHIFT) & VBLANK_HIGH_CRTC_MASK;
            Some(Self { file, kind: VBLANK_RELATIVE | crtc })
        }

        /// Block until the next vblank and say when it was, by the kernel's
        /// stamp. `None` on an error (the CRTC off, the wait interrupted).
        pub fn wait(&self) -> Option<Instant> {
            let mut w = WaitVblank { kind: self.kind, sequence: 1, tval_sec: 0, tval_usec: 0 };
            if !ioctl(&self.file, ioc::WAIT_VBLANK, &mut w) {
                return None;
            }
            Some(stamped(w.tval_sec, w.tval_usec))
        }
    }

    /// A CLOCK_MONOTONIC stamp as an Instant (the same clock), by how long ago
    /// it was; now, if the stamp doesn't look like one.
    fn stamped(sec: libc::c_long, usec: libc::c_long) -> Instant {
        let now = Instant::now();
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: FFI with a valid out-struct.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        let ago = (ts.tv_sec as i128 - sec as i128) * 1_000_000_000 + ts.tv_nsec as i128 - usec as i128 * 1000;
        if (0..100_000_000).contains(&ago) {
            now.checked_sub(Duration::from_nanos(ago as u64)).unwrap_or(now)
        } else {
            now
        }
    }

    /// The kernel stamps each vblank, so a late wake doesn't move the grid:
    /// nothing to raise.
    pub(super) fn raise_priority() {}

    #[cfg(test)]
    mod tests {
        /// As libdrm's headers have them on a 64-bit build.
        #[test]
        #[cfg(target_pointer_width = "64")]
        fn ioctl_numbers_match_the_kernel_headers() {
            assert_eq!(super::ioc::DROP_MASTER, 0x641f);
            assert_eq!(super::ioc::WAIT_VBLANK, 0xc018_643a);
            assert_eq!(super::ioc::GETRESOURCES, 0xc040_64a0);
            assert_eq!(super::ioc::GETCRTC, 0xc068_64a1);
            assert_eq!(super::ioc::GETENCODER, 0xc014_64a6);
            assert_eq!(super::ioc::GETCONNECTOR, 0xc050_64a7);
        }

        /// Lists card0 when it can be opened: a card without display hardware
        /// (WSL's) offers nothing rather than failing, and what a real one
        /// offers has a name.
        #[test]
        fn card0_lists_without_failing() {
            let card = std::path::Path::new("/dev/dri/card0");
            if let Some(f) = super::open_card(card) {
                for d in super::displays(&f, card) {
                    assert!(!d.name.is_empty());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: f64) -> Instant {
        base + Duration::from_secs_f64(ms / 1e3)
    }

    fn ms(base: Instant, t: Instant) -> f64 {
        secs_since(t, base) * 1e3
    }

    #[test]
    fn grid_steps_from_the_anchor_by_phase_and_per_frame() {
        let base = Instant::now();
        let anchor = at(base, 100.0);
        // 10 ms frames, 4 a frame, starting halfway: 105, 107.5, 110, 112.5, …
        let t = grid_after(anchor, 0.010, 4, 0.5, at(base, 106.0));
        assert!((ms(base, t) - 107.5).abs() < 1e-6);
        // Strictly after: a tick exactly on `after` is the next one.
        let t = grid_after(anchor, 0.010, 4, 0.5, at(base, 107.5));
        assert!((ms(base, t) - 110.0).abs() < 1e-6);
        // Before the anchor's first tick, and before the anchor itself.
        let t = grid_after(anchor, 0.010, 4, 0.5, at(base, 99.0));
        assert!((ms(base, t) - 100.0).abs() < 1e-6);
        let t = grid_after(anchor, 0.010, 1, 0.0, at(base, 100.0));
        assert!((ms(base, t) - 110.0).abs() < 1e-6);
    }

    #[test]
    fn fixed_refresh_locks_to_every_event_through_a_missed_one() {
        let base = Instant::now();
        let mut tr = Tracker::new(180);
        let p = 1000.0 / 180.0;
        let mut last = base;
        for i in 0..30 {
            if i == 20 {
                continue; // missed
            }
            last = at(base, i as f64 * p);
            tr.on_event(last, true);
        }
        let b = tr.beat(last, true).unwrap();
        assert_eq!(b.anchor, last);
        assert!((b.period * 1e3 - p).abs() < 1e-6);
        assert!(!b.vrr);
    }

    #[test]
    fn vrr_follows_the_frames_and_skips_a_lone_repeat() {
        // 72 fps on a 180 Hz panel, as measured.
        let base = Instant::now();
        let mut tr = Tracker::new(180);
        let frame = 1000.0 / 72.0;
        let mut t = 0.0;
        for _ in 0..20 {
            t += frame;
            tr.on_event(at(base, t), true);
        }
        let b = tr.beat(at(base, t), true).unwrap();
        assert!(b.vrr);
        assert!((b.period * 1e3 - frame).abs() < 1e-6);
        assert_eq!(b.anchor, at(base, t));
        // The driver shows the last frame again one fastest refresh on (the
        // probe saw 5.6 ms): not a frame.
        tr.on_event(at(base, t + 5.6), true);
        assert_eq!(tr.anchor, Some(at(base, t)));
        // The next real frame is.
        tr.on_event(at(base, t + frame), true);
        assert_eq!(tr.anchor, Some(at(base, t + frame)));
    }

    #[test]
    fn steady_doubling_becomes_the_cadence() {
        // 39.6 fps below the panel's range: every frame shown twice, evenly.
        let base = Instant::now();
        let mut tr = Tracker::new(180);
        let half = 1000.0 / 79.2;
        let mut t = 0.0;
        for _ in 0..40 {
            t += half;
            tr.on_event(at(base, t), true);
        }
        let b = tr.beat(at(base, t), true).unwrap();
        assert!((b.period * 1e3 - half).abs() < 1e-6);
        assert_eq!(b.anchor, at(base, t));
    }

    #[test]
    fn vrr_not_followed_runs_on_at_the_maximum_rate() {
        let base = Instant::now();
        let mut tr = Tracker::new(180);
        let frame = 1000.0 / 126.0;
        let mut t = 0.0;
        for _ in 0..20 {
            t += frame;
            tr.on_event(at(base, t), false);
        }
        let frozen = tr.anchor;
        for _ in 0..5 {
            t += frame;
            tr.on_event(at(base, t), false);
        }
        assert_eq!(tr.anchor, frozen);
        let b = tr.beat(at(base, t), false).unwrap();
        assert!((b.period - 1.0 / 180.0).abs() < 1e-9);
        // Followed again, it picks the frames back up.
        t += frame;
        tr.on_event(at(base, t), true);
        assert_eq!(tr.anchor, Some(at(base, t)));
    }

    #[test]
    fn precise_wait_never_returns_early() {
        let mut w = PreciseWait::default();
        for _ in 0..5 {
            let t = Instant::now() + Duration::from_millis(3);
            w.until(t, std::thread::sleep);
            assert!(Instant::now() >= t);
        }
    }
}
