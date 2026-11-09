//! The main window's always-on-top pin, for code that opens another window.
//!
//! A pinned main window sits in the topmost band, so every ordinary window
//! FlexInput opens lands under it: native file and folder dialogs, sub-patch
//! editors, Explorer, the browser behind a link. Those need focus and can't be
//! reached under the pin, so opening one drops it. Call [`release`] just
//! before: it needs no `&mut FlexInputApp`, takes the window out of the
//! topmost band at once, and leaves the app to settle the rest (the setting,
//! winit's window level) on its next pass.
//!
//! "At once" matters because most dialogs block the UI thread: a viewport
//! command wouldn't reach winit until the dialog closed. So the pin is dropped
//! around winit: by Win32 on Windows, by a request of our own to the window
//! manager on X11 (which is also GNOME, through XWayland), and by KWin's
//! script on Plasma's Wayland, which runs on its own thread.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicU32;

/// What the app still owes winit after [`release`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Released {
    /// Send it `WindowLevel::Normal` too: it keeps its own copy of the level
    /// (Windows), or nothing reached the window manager yet.
    TellWinit,
    /// Leave it be. On X11 the window manager already has it, winit keeps no
    /// copy, and sending it again would raise us over the window just opened:
    /// Mutter raises a window on any change to its ABOVE state, removal too.
    Done,
}

/// Mirrors `settings.pin_active`; the app republishes it every pass.
static PINNED: AtomicBool = AtomicBool::new(false);
/// The main window's HWND, 0 until known (and always 0 off Windows).
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
/// The main window's X11 id, 0 until known and on Wayland.
#[cfg(target_os = "linux")]
static MAIN_XWINDOW: AtomicU32 = AtomicU32::new(0);
/// Set by [`release`], taken by the app: 0 none, else a [`Released`] + 1.
static RELEASED: AtomicU8 = AtomicU8::new(0);

/// Take the main window's id from its handle, the first pass it has one.
pub(crate) fn note_main_window(frame: &eframe::Frame) {
    static NOTED: AtomicBool = AtomicBool::new(false);
    if NOTED.load(Ordering::Relaxed) {
        return;
    }
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    let Ok(handle) = frame.window_handle() else { return };
    match handle.as_raw() {
        RawWindowHandle::Win32(w) => MAIN_HWND.store(w.hwnd.get(), Ordering::Relaxed),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xlib(w) => MAIN_XWINDOW.store(w.window as u32, Ordering::Relaxed),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xcb(w) => MAIN_XWINDOW.store(w.window.get(), Ordering::Relaxed),
        _ => {}
    }
    NOTED.store(true, Ordering::Relaxed);
}

/// The app's pin state, for [`release`] to act on.
pub(crate) fn publish(pinned: bool) {
    PINNED.store(pinned, Ordering::Relaxed);
}

/// Whether the main window is pinned, as of the last [`publish`] or
/// [`release`] — so it already reads false in the pass that released it.
pub(crate) fn pinned() -> bool {
    PINNED.load(Ordering::Relaxed)
}

/// Unpin the main window: another window is about to open that it would
/// cover. Nothing while it isn't pinned.
pub(crate) fn release() {
    if !PINNED.swap(false, Ordering::Relaxed) {
        return;
    }
    #[allow(unused_mut)]
    let mut owed = Released::TellWinit;
    let hwnd = MAIN_HWND.load(Ordering::Relaxed);
    if hwnd != 0 {
        crate::process_list::drop_topmost(hwnd);
    }
    #[cfg(target_os = "linux")]
    {
        let xwindow = MAIN_XWINDOW.load(Ordering::Relaxed);
        if xwindow != 0 {
            match x11_drop_above(xwindow) {
                Ok(()) => owed = Released::Done,
                Err(e) => eprintln!("[pin] couldn't unpin on X11: {e}"),
            }
        }
        crate::kwin::pin_main_window(false);
    }
    RELEASED.store(owed as u8 + 1, Ordering::Relaxed);
}

/// Whether [`release`] dropped the pin since the last call, and what winit
/// is still owed if it did.
pub(crate) fn take_released() -> Option<Released> {
    match RELEASED.swap(0, Ordering::Relaxed) {
        0 => None,
        1 => Some(Released::TellWinit),
        _ => Some(Released::Done),
    }
}

/// Ask the window manager to take `_NET_WM_STATE_ABOVE` off `window`: the
/// message winit's `set_window_level` sends, but on a connection of our own,
/// so it goes out now instead of after the pass. Returns once the server has
/// handled it, so the WM hears it before the map of any window opened after.
#[cfg(target_os = "linux")]
fn x11_drop_above(window: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask};
    const NET_WM_STATE_REMOVE: u32 = 0;
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots.get(screen).ok_or("no such screen")?.root;
    let state = conn.intern_atom(false, b"_NET_WM_STATE")?;
    let above = conn.intern_atom(false, b"_NET_WM_STATE_ABOVE")?;
    let (state, above) = (state.reply()?.atom, above.reply()?.atom);
    let event = ClientMessageEvent::new(32, window, state, [NET_WM_STATE_REMOVE, above, 0, 0, 0]);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )?;
    // A round trip: once this replies, the server has delivered the event.
    conn.get_input_focus()?.reply()?;
    Ok(())
}
