//! Keeping windows on top under KDE Plasma on Wayland.
//!
//! Wayland gives a window no say in its stacking, so always-on-top is a no-op
//! there. KWin can be told instead, through scripts loaded over D-Bus:
//! - one at startup that sets "keep above" on this process's windows whose
//!   title starts with "FlexInput " (the overlays and the pickers that open
//!   over a game; the main window is plain "FlexInput" and the sub-patch
//!   editors start with "✦", so neither matches), and keeps them out of the
//!   taskbar and Alt+Tab;
//! - one per pin toggle that sets or clears it on the main window. Pinning a
//!   minimized window restores and focuses it, and unpinning minimizes it
//!   again, as on Windows.
//!
//! The overlays and pickers are layer-shell surfaces wherever the compositor
//! offers layer shell, KWin included (see the vendored winit's
//! `with_overlay_layer`), which stack above every window, a fullscreen game
//! too. The overlays script only matters where they open as ordinary windows
//! instead: "keep above" stacks those over ordinary windows, not over a game
//! that is fullscreen and focused. X11 sessions honour always-on-top as they
//! are.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// The overlays also leave the taskbar and Alt+Tab, as their
/// `with_taskbar(false)` makes them do on Windows. Plasma 6 names the window
/// list `windowList` / `windowAdded`, Plasma 5 `clientList` / `clientAdded`.
/// `PID` is replaced with this process's.
const OVERLAYS_SCRIPT: &str = r#"// Written by FlexInput (pid PID): keeps its over-the-game windows above
// other windows. Replaced on FlexInput's next start; inert once it exits.
(function () {
    function keepAbove(w) {
        if (w && w.pid === PID && String(w.caption).indexOf("FlexInput ") === 0) {
            w.keepAbove = true;
            w.skipTaskbar = true;
            w.skipSwitcher = true;
            w.skipPager = true;
        }
    }
    function watch(w) {
        keepAbove(w);
        if (w && w.captionChanged) {
            w.captionChanged.connect(function () { keepAbove(w); });
        }
    }
    var all = workspace.windowList ? workspace.windowList() : workspace.clientList();
    for (var i = 0; i < all.length; i++) {
        watch(all[i]);
    }
    (workspace.windowAdded || workspace.clientAdded).connect(watch);
})();
"#;

/// Pins the main window, restoring and focusing it when it is minimized. It
/// stays loaded while pinned and remembers that: when the unpin script clears
/// "keep above", it minimizes the window again. Also catches the window being
/// mapped after it runs (the pin is restored on the first frame).
const PIN_SCRIPT: &str = r#"// Written by FlexInput (pid PID): pins its main window.
(function () {
    var wasMinimized = false;
    function pin(w) {
        if (!w || w.pid !== PID || w.caption !== "FlexInput") {
            return;
        }
        if (w.minimized) {
            wasMinimized = true;
            w.minimized = false;
        }
        w.keepAbove = true;
        if (workspace.activeWindow !== undefined) {
            workspace.activeWindow = w;
        } else {
            workspace.activeClient = w;
        }
        w.keepAboveChanged.connect(function () {
            if (!w.keepAbove && wasMinimized) {
                wasMinimized = false;
                w.minimized = true;
            }
        });
    }
    var all = workspace.windowList ? workspace.windowList() : workspace.clientList();
    for (var i = 0; i < all.length; i++) {
        pin(all[i]);
    }
    (workspace.windowAdded || workspace.clientAdded).connect(pin);
})();
"#;

/// Unpins the main window. The pin script, still loaded, sees it and puts a
/// window it restored back to minimized.
const UNPIN_SCRIPT: &str = r#"// Written by FlexInput (pid PID): unpins its main window.
(function () {
    var all = workspace.windowList ? workspace.windowList() : workspace.clientList();
    for (var i = 0; i < all.length; i++) {
        var w = all[i];
        if (w.pid === PID && w.caption === "FlexInput") {
            w.keepAbove = false;
        }
    }
})();
"#;

/// The pin state last handed to KWin, so a repeat isn't loaded again: the
/// shortcut watcher and the UI both apply a gamepad pin toggle (see
/// [`toggle_pin`]), and reloading the pin script would forget that it
/// restored the window.
static PINNED: AtomicBool = AtomicBool::new(false);

enum Request {
    Overlays,
    Pin(bool),
}

/// Load the overlays script. Returns at once, like [`pin_main_window`].
pub(crate) fn keep_overlays_above() {
    send(Request::Overlays);
}

/// Pin the main window above other windows, or unpin it.
pub(crate) fn pin_main_window(on: bool) {
    if PINNED.swap(on, Ordering::Relaxed) != on {
        send(Request::Pin(on));
    }
}

/// Flip the pin, for the gamepad shortcut watcher: it reaches KWin even while
/// the UI thread is idle behind a minimized window. The UI then flips its own
/// state to match and its [`pin_main_window`] call finds nothing to do.
pub(crate) fn toggle_pin() {
    pin_main_window(!PINNED.load(Ordering::Relaxed));
}

/// Hand a request to the worker, which keeps one D-Bus connection and applies
/// requests in order. Nothing outside Plasma on Wayland.
fn send(req: Request) {
    static WORKER: OnceLock<Option<Mutex<Sender<Request>>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let kde = std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|d| d.split(':').any(|part| part.eq_ignore_ascii_case("KDE")));
        if !(wayland && kde) {
            return None;
        }
        let (tx, rx) = channel();
        std::thread::Builder::new().name("kwin-script".into()).spawn(move || serve(rx)).ok()?;
        Some(Mutex::new(tx))
    });
    if let Some(tx) = worker {
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(req);
    }
}

fn serve(rx: Receiver<Request>) {
    let conn = match dbus::blocking::Connection::new_session() {
        Ok(c) => c,
        Err(e) => return eprintln!("[kwin] no session bus: {e}"),
    };
    while let Ok(first) = rx.recv() {
        // A burst of pin toggles: only the last one matters.
        let mut overlays = false;
        let mut pin = None;
        for req in std::iter::once(first).chain(rx.try_iter()) {
            match req {
                Request::Overlays => overlays = true,
                Request::Pin(on) => pin = Some(on),
            }
        }
        if overlays {
            match load(&conn, "flexinput-keep-above", OVERLAYS_SCRIPT) {
                Ok(()) => eprintln!("[kwin] overlays kept above other windows"),
                Err(e) => eprintln!("[kwin] couldn't load the keep-above script: {e}"),
            }
        }
        // The pin script replaces the last one (and an unpin left loaded);
        // the unpin script leaves it loaded, to minimize again.
        let result = match pin {
            Some(true) => unload(&conn, "flexinput-unpin").and_then(|()| load(&conn, "flexinput-pin", PIN_SCRIPT)),
            Some(false) => load(&conn, "flexinput-unpin", UNPIN_SCRIPT),
            None => Ok(()),
        };
        if let (Some(on), Err(e)) = (pin, result) {
            eprintln!("[kwin] couldn't {} the main window: {e}", if on { "pin" } else { "unpin" });
        }
    }
}

/// Load `script` as `plugin` in place of any earlier copy (a previous run's,
/// matching that run's pid, or the last pin toggle's) and run it.
fn load(conn: &dbus::blocking::Connection, plugin: &str, script: &str) -> Result<(), Box<dyn std::error::Error>> {
    // KWin reads the script from a file. The runtime dir is per user and in
    // memory; temp is the fallback.
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("{plugin}.js"));
    std::fs::write(&path, script.replace("PID", &std::process::id().to_string()))?;

    unload(conn, plugin)?;
    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(5));
    let path = path.to_string_lossy();
    let (id,): (i32,) = scripting.method_call(IFACE, "loadScript", (path.as_ref(), plugin))?;
    if id < 0 {
        return Err("KWin refused to load it".into());
    }
    // Runs every loaded script that isn't running yet: this one.
    let () = scripting.method_call(IFACE, "start", ())?;
    Ok(())
}

const IFACE: &str = "org.kde.kwin.Scripting";

/// Unload `plugin` if it is loaded.
fn unload(conn: &dbus::blocking::Connection, plugin: &str) -> Result<(), Box<dyn std::error::Error>> {
    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(5));
    let _: (bool,) = scripting.method_call(IFACE, "unloadScript", (plugin,))?;
    Ok(())
}
