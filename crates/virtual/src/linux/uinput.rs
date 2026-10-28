//! uinput: an input device made from this process through `/dev/uinput`. The
//! virtual Xbox 360 pad is one, laid out exactly as the kernel's xpad driver
//! lays out a wired pad (same name, VID/PID, buttons, axes and ranges), which
//! is what SDL, Steam and Wine recognise as an Xbox 360 controller. Its rumble
//! is the force-feedback effects games upload and play on it, which a reader
//! thread answers. (The Sony pads and the keyboard/mouse are UHID: see `uhid`.)

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flexinput_core::Signal;
use flexinput_hidmaestro::encode::{button, Hat, PinState};

use crate::{layouts, SinkPin, SourcePin};

// <linux/input-event-codes.h>
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_FF: u16 = 0x15;
const EV_UINPUT: u16 = 0x0101;
const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const FF_RUMBLE: u16 = 0x50;
const FF_GAIN: u16 = 0x60;
const BUS_USB: u16 = 0x03;
// <linux/uinput.h>
const UI_FF_UPLOAD: u16 = 1;
const UI_FF_ERASE: u16 = 2;

/// ioctl numbers, from `_IOC(dir, 'U', nr, size)` with the asm-generic layout
/// (x86 and ARM; not MIPS / PowerPC / SPARC, which FlexInput doesn't target).
mod ioc {
    use super::{InputAbsSetup, UinputFfErase, UinputFfUpload, UinputSetup};
    use std::mem::size_of;
    const NONE: u64 = 0;
    const W: u64 = 1;
    const R: u64 = 2;
    const fn ioc(dir: u64, nr: u64, size: usize) -> u64 {
        (dir << 30) | ((size as u64) << 16) | ((b'U' as u64) << 8) | nr
    }
    pub const UI_DEV_CREATE: u64 = ioc(NONE, 1, 0);
    pub const UI_DEV_DESTROY: u64 = ioc(NONE, 2, 0);
    pub const UI_DEV_SETUP: u64 = ioc(W, 3, size_of::<UinputSetup>());
    pub const UI_ABS_SETUP: u64 = ioc(W, 4, size_of::<InputAbsSetup>());
    pub const UI_SET_EVBIT: u64 = ioc(W, 100, size_of::<libc::c_int>());
    pub const UI_SET_KEYBIT: u64 = ioc(W, 101, size_of::<libc::c_int>());
    pub const UI_SET_ABSBIT: u64 = ioc(W, 103, size_of::<libc::c_int>());
    pub const UI_SET_FFBIT: u64 = ioc(W, 107, size_of::<libc::c_int>());
    pub const UI_SET_PHYS: u64 = ioc(W, 108, size_of::<*const libc::c_char>());
    pub const UI_BEGIN_FF_UPLOAD: u64 = ioc(R | W, 200, size_of::<UinputFfUpload>());
    pub const UI_END_FF_UPLOAD: u64 = ioc(W, 201, size_of::<UinputFfUpload>());
    pub const UI_BEGIN_FF_ERASE: u64 = ioc(R | W, 202, size_of::<UinputFfErase>());
    pub const UI_END_FF_ERASE: u64 = ioc(W, 203, size_of::<UinputFfErase>());
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct InputAbsSetup {
    code: u16,
    absinfo: InputAbsinfo,
}

/// `struct ff_effect`: seven u16 of header, then a union whose pointer member
/// aligns it to 8. Rumble's two magnitudes are the union's first two u16.
#[repr(C)]
#[derive(Clone, Copy)]
struct FfEffect {
    kind: u16,
    id: i16,
    direction: u16,
    trigger_button: u16,
    trigger_interval: u16,
    replay_length: u16,
    replay_delay: u16,
    u: FfUnion,
}

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct FfUnion([u8; 32]);

#[repr(C)]
struct UinputFfUpload {
    request_id: u32,
    retval: i32,
    effect: FfEffect,
    old: FfEffect,
}

#[repr(C)]
struct UinputFfErase {
    request_id: u32,
    retval: i32,
    effect_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<UinputSetup>() == 92);
    assert!(std::mem::size_of::<InputAbsSetup>() == 28);
    assert!(std::mem::size_of::<FfEffect>() == 48);
    assert!(std::mem::size_of::<UinputFfUpload>() == 104);
    assert!(std::mem::size_of::<InputEvent>() == 24);
};

fn ioctl<T>(fd: libc::c_int, req: u64, arg: *const T) -> std::io::Result<()> {
    // SAFETY: every caller passes a request whose size matches `T`, and a
    // pointer to a live `T` (or to a value for the int-argument requests).
    if unsafe { libc::ioctl(fd, req as _, arg) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Like [`ioctl`], for the requests that take an int by value.
fn ioctl_int(fd: libc::c_int, req: u64, value: u16) -> std::io::Result<()> {
    // SAFETY: the UI_SET_*BIT requests take an int by value.
    if unsafe { libc::ioctl(fd, req as _, value as libc::c_int) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn event(kind: u16, code: u16, value: i32) -> InputEvent {
    InputEvent { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, kind, code, value }
}

fn write_events(file: &File, events: &[InputEvent]) {
    // SAFETY: InputEvent is plain repr(C) data; the slice is its bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), std::mem::size_of_val(events))
    };
    let _ = (&*file).write(bytes);
}

// ── The Xbox 360 pad ─────────────────────────────────────────────────────────

/// xpad's button order: SDL's Linux mapping for this pad numbers buttons in it.
const KEYS: [u16; 11] = [
    0x130, // BTN_A
    0x131, // BTN_B
    0x133, // BTN_X
    0x134, // BTN_Y
    0x136, // BTN_TL
    0x137, // BTN_TR
    0x13a, // BTN_SELECT
    0x13b, // BTN_START
    0x13c, // BTN_MODE
    0x13d, // BTN_THUMBL
    0x13e, // BTN_THUMBR
];

/// The FlexInput button each of [`KEYS`] carries.
const KEY_BITS: [u32; 11] = [
    button::A,
    button::B,
    button::X,
    button::Y,
    button::LEFT_BUMPER,
    button::RIGHT_BUMPER,
    button::BACK,
    button::START,
    button::GUIDE,
    button::LEFT_STICK,
    button::RIGHT_STICK,
];

/// xpad's axes and ranges for a wired 360 pad, in the order SDL numbers them.
const AXES: [(u16, i32, i32, i32, i32); 8] = [
    // code, min, max, fuzz, flat
    (ABS_X, -32768, 32767, 16, 128),
    (ABS_Y, -32768, 32767, 16, 128),
    (ABS_Z, 0, 255, 0, 0),
    (ABS_RX, -32768, 32767, 16, 128),
    (ABS_RY, -32768, 32767, 16, 128),
    (ABS_RZ, 0, 255, 0, 0),
    (ABS_HAT0X, -1, 1, 0, 0),
    (ABS_HAT0Y, -1, 1, 0, 0),
];

/// The effects a game has uploaded and which are playing — written by the
/// reader thread, read by `poll_outputs`.
#[derive(Default)]
struct Ff {
    /// Rumble effects by id: (strong, weak, length in ms, 0 = until stopped).
    effects: HashMap<i16, (u16, u16, u16)>,
    /// Playing effects, since when.
    playing: HashMap<i16, Instant>,
    /// FF_GAIN, 0..=0xFFFF.
    gain: u16,
}

impl Ff {
    /// The rumble now: the strongest of the playing effects, scaled by the
    /// gain. Drops effects whose length has run out.
    fn level(&mut self) -> (f32, f32) {
        let effects = &self.effects;
        self.playing.retain(|id, since| match effects.get(id) {
            Some(&(_, _, len)) => len == 0 || since.elapsed() < Duration::from_millis(len as u64),
            None => false,
        });
        let (mut s, mut w) = (0u16, 0u16);
        for id in self.playing.keys() {
            if let Some(&(es, ew, _)) = self.effects.get(id) {
                s = s.max(es);
                w = w.max(ew);
            }
        }
        let g = self.gain as f32 / 65535.0;
        (s as f32 / 65535.0 * g, w as f32 / 65535.0 * g)
    }
}

/// The reader thread: answer effect uploads and erases, track play and stop.
fn serve_ff(file: Arc<File>, ff: Arc<Mutex<Ff>>, stop: Arc<AtomicBool>) {
    let fd = file.as_raw_fd();
    let mut buf = [0u8; 24 * 16];
    while !stop.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the device's open fd.
        if unsafe { libc::poll(&mut pfd, 1, 100) } <= 0 {
            continue;
        }
        let n = match (&*file).read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for chunk in buf[..n].chunks_exact(24) {
            // SAFETY: a whole input_event the kernel wrote.
            let ev: InputEvent = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
            match (ev.kind, ev.code) {
                (EV_UINPUT, UI_FF_UPLOAD) => {
                    // SAFETY: zero is a valid bit pattern for this plain struct.
                    let mut up: UinputFfUpload = unsafe { std::mem::zeroed() };
                    up.request_id = ev.value as u32;
                    if ioctl(fd, ioc::UI_BEGIN_FF_UPLOAD, &mut up as *mut UinputFfUpload).is_err() {
                        continue;
                    }
                    let e = up.effect;
                    let mut ff = ff.lock().unwrap_or_else(|p| p.into_inner());
                    if e.kind == FF_RUMBLE {
                        let strong = u16::from_ne_bytes([e.u.0[0], e.u.0[1]]);
                        let weak = u16::from_ne_bytes([e.u.0[2], e.u.0[3]]);
                        ff.effects.insert(e.id, (strong, weak, e.replay_length));
                    } else {
                        // Only rumble is offered; anything else plays as nothing.
                        ff.effects.remove(&e.id);
                    }
                    drop(ff);
                    up.retval = 0;
                    let _ = ioctl(fd, ioc::UI_END_FF_UPLOAD, &up as *const UinputFfUpload);
                }
                (EV_UINPUT, UI_FF_ERASE) => {
                    let mut er = UinputFfErase { request_id: ev.value as u32, retval: 0, effect_id: 0 };
                    if ioctl(fd, ioc::UI_BEGIN_FF_ERASE, &mut er as *mut UinputFfErase).is_err() {
                        continue;
                    }
                    {
                        let mut ff = ff.lock().unwrap_or_else(|p| p.into_inner());
                        let id = er.effect_id as i16;
                        ff.effects.remove(&id);
                        ff.playing.remove(&id);
                    }
                    er.retval = 0;
                    let _ = ioctl(fd, ioc::UI_END_FF_ERASE, &er as *const UinputFfErase);
                }
                (EV_FF, FF_GAIN) => {
                    ff.lock().unwrap_or_else(|p| p.into_inner()).gain = ev.value.clamp(0, 0xFFFF) as u16;
                }
                (EV_FF, code) => {
                    let mut ff = ff.lock().unwrap_or_else(|p| p.into_inner());
                    if ev.value > 0 {
                        ff.playing.insert(code as i16, Instant::now());
                    } else {
                        ff.playing.remove(&(code as i16));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Values last written, so a flush sends only what changed.
type AxisValues = [i32; AXES.len()];

pub struct Xbox360 {
    id: String,
    display_name: String,
    pins: PinState,
    file: Option<Arc<File>>,
    ff: Arc<Mutex<Ff>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    last_keys: u32,
    last_axes: Option<AxisValues>,
    /// A rumble ping's level, standing in for the game's until it ends.
    ping: Option<(f32, f32)>,
}

impl Xbox360 {
    /// Create the pad; one that couldn't be created comes back disconnected.
    pub fn create(id: String, display_name: String) -> Self {
        let mut pad = Xbox360 {
            id,
            display_name,
            pins: PinState::new(),
            file: None,
            ff: Arc::new(Mutex::new(Ff { gain: 0xFFFF, ..Ff::default() })),
            stop: Arc::new(AtomicBool::new(false)),
            reader: None,
            last_keys: 0,
            last_axes: None,
            ping: None,
        };
        if let Err(e) = super::access::ensure() {
            eprintln!("[uinput] {}: no access to {} ({e})", pad.id, super::access::UINPUT);
            return pad;
        }
        let phys = format!("{}{}", flexinput_core::own_virtual::PHYS_PREFIX, pad.id);
        match build(&phys) {
            Ok(file) => {
                let file = Arc::new(file);
                let (f2, ff, stop) = (Arc::clone(&file), Arc::clone(&pad.ff), Arc::clone(&pad.stop));
                pad.reader = std::thread::Builder::new()
                    .name("uinput-ff".into())
                    .spawn(move || serve_ff(f2, ff, stop))
                    .ok();
                pad.file = Some(file);
                eprintln!("[uinput] {} up as an Xbox 360 pad", pad.id);
            }
            Err(e) => eprintln!("[uinput] {}: create failed: {e}", pad.id),
        }
        pad
    }

    /// The current state as xpad reports it.
    fn values(&self) -> (u32, AxisValues) {
        let s = self.pins.state();
        // The encoder state's sticks are 0..1 with Y already pointing down,
        // which is xpad's sign; its triggers are 0..1.
        let stick = |v: f32| ((v * 2.0 - 1.0) * 32767.0).round() as i32;
        let trigger = |v: f32| (v * 255.0).round() as i32;
        let (hx, hy) = match s.hat {
            Hat::Neutral => (0, 0),
            Hat::North => (0, -1),
            Hat::NorthEast => (1, -1),
            Hat::East => (1, 0),
            Hat::SouthEast => (1, 1),
            Hat::South => (0, 1),
            Hat::SouthWest => (-1, 1),
            Hat::West => (-1, 0),
            Hat::NorthWest => (-1, -1),
        };
        let axes = [
            stick(s.left_stick_x),
            stick(s.left_stick_y),
            trigger(s.left_trigger),
            stick(s.right_stick_x),
            stick(s.right_stick_y),
            trigger(s.right_trigger),
            hx,
            hy,
        ];
        (s.buttons, axes)
    }
}

/// Make the uinput device.
fn build(phys: &str) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(super::access::UINPUT)?;
    let fd = file.as_raw_fd();
    for ev in [EV_KEY, EV_ABS, EV_FF] {
        ioctl_int(fd, ioc::UI_SET_EVBIT, ev)?;
    }
    for k in KEYS {
        ioctl_int(fd, ioc::UI_SET_KEYBIT, k)?;
    }
    for (code, minimum, maximum, fuzz, flat) in AXES {
        ioctl_int(fd, ioc::UI_SET_ABSBIT, code)?;
        let setup = InputAbsSetup {
            code,
            absinfo: InputAbsinfo { value: 0, minimum, maximum, fuzz, flat, resolution: 0 },
        };
        ioctl(fd, ioc::UI_ABS_SETUP, &setup)?;
    }
    for f in [FF_RUMBLE, FF_GAIN] {
        ioctl_int(fd, ioc::UI_SET_FFBIT, f)?;
    }
    let phys = std::ffi::CString::new(phys).map_err(std::io::Error::other)?;
    // SAFETY: UI_SET_PHYS takes a NUL-terminated string the kernel copies.
    if unsafe { libc::ioctl(fd, ioc::UI_SET_PHYS as _, phys.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut setup = UinputSetup {
        id: InputId {
            bustype: BUS_USB,
            vendor: 0x045E,
            product: 0x028E,
            version: flexinput_core::own_virtual::VERSION,
        },
        name: [0; 80],
        // xpad's memless force feedback holds 16.
        ff_effects_max: 16,
    };
    let name = b"Microsoft X-Box 360 pad";
    setup.name[..name.len()].copy_from_slice(name);
    ioctl(fd, ioc::UI_DEV_SETUP, &setup)?;
    ioctl(fd, ioc::UI_DEV_CREATE, std::ptr::null::<u8>())?;
    Ok(file)
}

impl crate::VirtualDevice for Xbox360 {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn sink_pins(&self) -> &'static [SinkPin] {
        layouts::XINPUT_SINK_PINS
    }

    fn send(&mut self, pin: &str, value: Signal) {
        crate::apply_pin(&mut self.pins, pin, value);
    }

    fn flush(&mut self) {
        let Some(file) = &self.file else { return };
        let (keys, axes) = self.values();
        let mut events = Vec::with_capacity(KEYS.len() + AXES.len() + 1);
        for (code, bit) in KEYS.iter().zip(KEY_BITS) {
            if (keys ^ self.last_keys) & bit != 0 || self.last_axes.is_none() {
                events.push(event(EV_KEY, *code, (keys & bit != 0) as i32));
            }
        }
        for (i, (code, ..)) in AXES.iter().enumerate() {
            if self.last_axes.map_or(true, |last| last[i] != axes[i]) {
                events.push(event(EV_ABS, *code, axes[i]));
            }
        }
        if !events.is_empty() {
            events.push(event(EV_SYN, SYN_REPORT, 0));
            write_events(file, &events);
        }
        self.last_keys = keys;
        self.last_axes = Some(axes);
    }

    fn reset_outputs(&mut self) {
        self.pins.reset();
        self.flush();
    }

    fn is_connected(&self) -> bool {
        self.file.is_some()
    }

    fn source_pins(&self) -> &'static [SourcePin] {
        layouts::XINPUT_SOURCE_PINS
    }

    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> {
        let game = self.ff.lock().unwrap_or_else(|p| p.into_inner()).level();
        let (strong, weak) = self.ping.unwrap_or(game);
        vec![("rumble_strong", Signal::Float(strong)), ("rumble_weak", Signal::Float(weak))]
    }

    fn inject_rumble_ping(&mut self, strong: f32, weak: f32) {
        // The ping handler sets the level each tick and (0, 0) when it ends,
        // which hands rumble back to the game.
        self.ping = (strong > 0.0 || weak > 0.0).then_some((strong, weak));
    }
}

impl Drop for Xbox360 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        if let Some(file) = self.file.take() {
            // Closing removes it too; destroying first is the documented order.
            let _ = ioctl(file.as_raw_fd(), ioc::UI_DEV_DESTROY, std::ptr::null::<u8>());
        }
    }
}

// ── The switcher's keyboard ──────────────────────────────────────────────────

const KEY_TAB: u16 = 15;
const KEY_LEFTALT: u16 = 56;
const KEY_UP: u16 = 103;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_DOWN: u16 = 108;
/// The last key of a full keyboard (`KEY_MICMUTE`). The device offers all of
/// them so udev tags it a keyboard and the compositor takes its keys as one.
const KEY_LAST_KEYBOARD: u16 = 248;

/// A key the switcher taps, by the names the Windows tapper takes.
fn switcher_key(name: &str) -> Option<u16> {
    Some(match name {
        "tab" => KEY_TAB,
        "arrowleft" => KEY_LEFT,
        "arrowright" => KEY_RIGHT,
        "arrowup" => KEY_UP,
        "arrowdown" => KEY_DOWN,
        _ => return None,
    })
}

/// Whether the button holding Alt is still down.
type StillHeld = Box<dyn Fn() -> bool + Send>;

enum KeyCmd {
    AltDown(StillHeld),
    AltUp,
    Tap(u16),
}

/// A command queued behind the access prompt is dropped once it is this old,
/// so granting access doesn't replay a switch the user gave up on.
const KEY_CMD_STALE: Duration = Duration::from_secs(1);
/// How long a new keyboard is left before its first key: the compositor opens
/// it a moment after it appears, and keys sent before that are lost.
const KEYBOARD_SETTLE: Duration = Duration::from_millis(300);
/// How often a held Alt checks the button holding it.
const KEY_HOLD_POLL: Duration = Duration::from_millis(20);

/// The gamepad Alt+Tab switcher's keyboard: a uinput keyboard of its own,
/// independent of the virtual-device pool, which the compositor's own Alt+Tab
/// answers (KDE and GNOME bind it; Hyprland and Sway don't by default). A
/// worker thread owns the device and writes the keys, so the UI thread never
/// waits on it, nor on the access prompt the first one may need.
pub struct UiKeyTapper {
    tx: std::sync::mpsc::Sender<(Instant, KeyCmd)>,
}

impl Default for UiKeyTapper {
    fn default() -> Self {
        Self::new()
    }
}

impl UiKeyTapper {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("switcher-keys".into())
            .spawn(move || serve_keys(rx));
        UiKeyTapper { tx }
    }

    /// Hold Alt while `still_held` says the button holding it is down. The
    /// worker lets go as soon as it isn't: the UI, which releases it too,
    /// can stop running while the switcher covers its window (Wayland stops
    /// a hidden window's frames), and Alt must not stay down.
    pub fn alt_down_while(&mut self, still_held: impl Fn() -> bool + Send + 'static) {
        let _ = self.tx.send((Instant::now(), KeyCmd::AltDown(Box::new(still_held))));
    }

    /// Release Alt if held (commits the switcher's selection).
    pub fn alt_up(&mut self) {
        let _ = self.tx.send((Instant::now(), KeyCmd::AltUp));
    }

    /// Tap a key once: "tab" or an arrow ("arrowleft" …).
    pub fn tap(&mut self, name: &str) {
        if let Some(code) = switcher_key(name) {
            let _ = self.tx.send((Instant::now(), KeyCmd::Tap(code)));
        }
    }
}

fn serve_keys(rx: std::sync::mpsc::Receiver<(Instant, KeyCmd)>) {
    use std::sync::mpsc::RecvTimeoutError;
    let phys = format!("{}switcher-keys", flexinput_core::own_virtual::PHYS_PREFIX);
    // Steam's udev rules usually grant uinput already; ask only when they don't.
    let built = match build_keyboard(&phys) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => super::access::ensure()
            .map_err(std::io::Error::other)
            .and_then(|()| build_keyboard(&phys)),
        r => r,
    };
    let file = match built {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[uinput] switcher keyboard: {e}");
            return;
        }
    };
    std::thread::sleep(KEYBOARD_SETTLE);
    let key = |code: u16, down: bool| {
        write_events(&file, &[event(EV_KEY, code, down as i32), event(EV_SYN, SYN_REPORT, 0)]);
    };
    let mut held: Option<StillHeld> = None;
    // Alt went stale, so its taps would land without it.
    let mut skipping = false;
    loop {
        match rx.recv_timeout(KEY_HOLD_POLL) {
            Ok((at, cmd)) => {
                let stale = at.elapsed() > KEY_CMD_STALE;
                match cmd {
                    KeyCmd::AltDown(_) if stale => skipping = true,
                    KeyCmd::AltDown(f) => {
                        if held.is_none() {
                            key(KEY_LEFTALT, true);
                        }
                        held = Some(f);
                    }
                    KeyCmd::AltUp => {
                        skipping = false;
                        if held.take().is_some() {
                            key(KEY_LEFTALT, false);
                        }
                    }
                    KeyCmd::Tap(_) if stale || skipping => {}
                    KeyCmd::Tap(code) => {
                        key(code, true);
                        key(code, false);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if held.as_ref().is_some_and(|still_held| !still_held()) {
            held = None;
            key(KEY_LEFTALT, false);
        }
    }
    if held.is_some() {
        key(KEY_LEFTALT, false);
    }
    let _ = ioctl(file.as_raw_fd(), ioc::UI_DEV_DESTROY, std::ptr::null::<u8>());
}

/// Make the switcher's keyboard: every key, so it's taken as a keyboard, though
/// only Alt, Tab and the arrows are ever pressed.
fn build_keyboard(phys: &str) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(super::access::UINPUT)?;
    let fd = file.as_raw_fd();
    ioctl_int(fd, ioc::UI_SET_EVBIT, EV_KEY)?;
    for k in 1..=KEY_LAST_KEYBOARD {
        ioctl_int(fd, ioc::UI_SET_KEYBIT, k)?;
    }
    let phys = std::ffi::CString::new(phys).map_err(std::io::Error::other)?;
    // SAFETY: UI_SET_PHYS takes a NUL-terminated string the kernel copies.
    if unsafe { libc::ioctl(fd, ioc::UI_SET_PHYS as _, phys.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut setup = UinputSetup {
        // USB, so libinput takes it for an external keyboard and never pairs
        // it with a touchpad's disable-while-typing.
        id: InputId { bustype: BUS_USB, vendor: 0, product: 0, version: flexinput_core::own_virtual::VERSION },
        name: [0; 80],
        ff_effects_max: 0,
    };
    let name = b"FlexInput Switcher Keys";
    setup.name[..name.len()].copy_from_slice(name);
    ioctl(fd, ioc::UI_DEV_SETUP, &setup)?;
    ioctl(fd, ioc::UI_DEV_CREATE, std::ptr::null::<u8>())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualDevice;
    use glam::Vec2;

    fn unbuilt() -> Xbox360 {
        // No node: `create` would ask for access. These test the layout.
        Xbox360 {
            id: "virtual.hm.xinput".into(),
            display_name: "test".into(),
            pins: PinState::new(),
            file: None,
            ff: Arc::new(Mutex::new(Ff { gain: 0xFFFF, ..Ff::default() })),
            stop: Arc::new(AtomicBool::new(false)),
            reader: None,
            last_keys: 0,
            last_axes: None,
            ping: None,
        }
    }

    #[test]
    fn ioctl_numbers_are_the_kernels() {
        // From <linux/uinput.h> on x86_64.
        assert_eq!(ioc::UI_DEV_CREATE, 0x5501);
        assert_eq!(ioc::UI_DEV_DESTROY, 0x5502);
        assert_eq!(ioc::UI_DEV_SETUP, 0x405c_5503);
        assert_eq!(ioc::UI_ABS_SETUP, 0x401c_5504);
        assert_eq!(ioc::UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(ioc::UI_SET_PHYS, 0x4008_556c);
        assert_eq!(ioc::UI_BEGIN_FF_UPLOAD, 0xc068_55c8);
        assert_eq!(ioc::UI_END_FF_ERASE, 0x400c_55cb);
    }

    #[test]
    fn pins_land_where_xpad_puts_them() {
        let mut pad = unbuilt();
        pad.send("left_stick", Signal::Vec2(Vec2::new(1.0, 1.0))); // right and up
        pad.send("right_trigger", Signal::Float(1.0));
        pad.send("btn_west", Signal::Bool(true));
        pad.send("btn_guide", Signal::Bool(true));
        pad.send("dpad_left", Signal::Bool(true));
        let (keys, axes) = pad.values();
        assert_eq!(axes[0], 32767, "ABS_X right");
        assert_eq!(axes[1], -32767, "ABS_Y up is negative, as xpad reports it");
        assert_eq!(axes[5], 255, "ABS_RZ is the right trigger");
        assert_eq!((axes[6], axes[7]), (-1, 0), "hat left");
        assert_eq!(axes[3], 0, "right stick centred");
        let pressed: Vec<u16> = KEYS.iter().zip(KEY_BITS).filter(|(_, b)| keys & b != 0).map(|(k, _)| *k).collect();
        assert_eq!(pressed, [0x133, 0x13c], "X is BTN_X, guide is BTN_MODE");
    }

    #[test]
    fn switcher_keys_are_the_kernels() {
        // <linux/input-event-codes.h>
        assert_eq!(switcher_key("tab"), Some(15));
        assert_eq!(switcher_key("arrowup"), Some(103));
        assert_eq!(switcher_key("arrowleft"), Some(105));
        assert_eq!(switcher_key("arrowright"), Some(106));
        assert_eq!(switcher_key("arrowdown"), Some(108));
        assert_eq!(switcher_key("enter"), None);
        assert_eq!((KEY_LEFTALT, KEY_LAST_KEYBOARD), (56, 248));
    }

    #[test]
    fn rumble_is_the_strongest_playing_effect_until_it_runs_out() {
        let mut ff = Ff { gain: 0xFFFF, ..Ff::default() };
        ff.effects.insert(0, (0xFFFF, 0, 0));
        ff.effects.insert(1, (0x8000, 0x4000, 1));
        assert_eq!(ff.level(), (0.0, 0.0), "uploaded, not playing");
        ff.playing.insert(1, Instant::now());
        ff.playing.insert(0, Instant::now());
        let (s, w) = ff.level();
        assert_eq!(s, 1.0);
        assert!((w - 0x4000 as f32 / 65535.0).abs() < 1e-6);
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(ff.level(), (1.0, 0.0), "the 1 ms effect ran out; the endless one plays on");
        ff.gain = 0x8000;
        assert!((ff.level().0 - 0x8000 as f32 / 65535.0).abs() < 1e-4, "gain scales");
    }
}
