//! Running under XWayland where Wayland can't keep the overlays up.
//!
//! The overlays and the pickers that open over a game are layer-shell surfaces
//! on Wayland (see the vendored winit's `with_overlay_layer`). A compositor
//! without layer shell (GNOME's Mutter, gamescope) makes them ordinary
//! toplevels: below a fullscreen game, and in the dash and Alt+Tab. As an X11
//! app they keep their place there: transparent taskbar-less viewports are
//! notification windows on X11 (the FLEXINPUT PATCH in the vendored
//! egui-winit), which Mutter stacks in its top layer, the one popup menus use,
//! and leaves out of the dash and Alt+Tab. The cost is XWayland's: at a
//! fractional scale the whole app is upscaled, so it looks soft.

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

/// The event loop hook that starts FlexInput on X11 instead of Wayland, when
/// the Wayland compositor has no layer shell and XWayland is there to use.
/// `None` keeps winit's own pick.
pub fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    use winit::platform::x11::EventLoopBuilderExtX11 as _;
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    // WAYLAND_SOCKET is a connection handed over once: probing would take it
    // from winit. Without DISPLAY there's no XWayland to move to.
    if !set("WAYLAND_DISPLAY") || set("WAYLAND_SOCKET") || !set("DISPLAY") {
        return None;
    }
    if has_layer_shell()? {
        return None;
    }
    eprintln!("[display] no layer shell on this Wayland desktop: running under XWayland");
    Some(Box::new(|builder| {
        builder.with_x11();
    }))
}

/// Whether the compositor offers wlr-layer-shell. `None` when it can't be
/// reached, which leaves winit to fail or fall back as it would anyway.
fn has_layer_shell() -> Option<bool> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, _queue) = registry_queue_init::<Probe>(&conn).ok()?;
    Some(globals.contents().with_list(|list| list.iter().any(|g| g.interface == "zwlr_layer_shell_v1")))
}

/// The registry's initial burst is all the probe reads; later events are
/// never dispatched.
struct Probe;

impl Dispatch<WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
