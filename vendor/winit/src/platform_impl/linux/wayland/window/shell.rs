//! FLEXINPUT PATCH: the shell role a window's surface has. Upstream every
//! window is an xdg toplevel; one asked for with
//! `WindowAttributesExtWayland::with_overlay_layer` is a wlr-layer-shell
//! surface instead, covering an output in the overlay layer. A layer surface
//! has none of a toplevel's window management (title, app id, sizes,
//! maximize, move, decorations), so those requests are dropped for it.
//!
//! An overlay must not take the keyboard from the game under it, so a layer
//! surface takes keyboard focus only while it was asked to be focused or a
//! text field in it wants typing (see `Window::set_layer_keyboard`).

use sctk::reexports::client::protocol::wl_output::WlOutput;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use sctk::shell::wlr_layer::{KeyboardInteractivity, LayerSurface};
use sctk::shell::xdg::window::{DecorationMode, Window as XdgWindow};
use sctk::shell::{Unsupported, WaylandSurface};

#[derive(Debug, Clone)]
pub enum Window {
    Xdg(XdgWindow),
    Layer(LayerSurface),
}

impl Window {
    /// The toplevel, unless this is a layer surface.
    pub fn xdg(&self) -> Option<&XdgWindow> {
        match self {
            Window::Xdg(window) => Some(window),
            Window::Layer(_) => None,
        }
    }

    pub fn is_layer(&self) -> bool {
        matches!(self, Window::Layer(_))
    }

    pub fn wl_surface(&self) -> &WlSurface {
        match self {
            Window::Xdg(window) => window.wl_surface(),
            // The trait's: LayerSurface's inherent one is private.
            Window::Layer(layer) => WaylandSurface::wl_surface(layer),
        }
    }

    pub fn commit(&self) {
        match self {
            Window::Xdg(window) => window.commit(),
            Window::Layer(layer) => layer.commit(),
        }
    }

    pub fn set_buffer_scale(&self, scale: u32) -> Result<(), Unsupported> {
        match self {
            Window::Xdg(window) => window.set_buffer_scale(scale),
            Window::Layer(layer) => layer.set_buffer_scale(scale),
        }
    }

    // Layer surface only.

    /// Whether the layer surface takes keyboard focus. A compositor focuses
    /// an overlay-layer surface the moment it starts accepting the keyboard
    /// (KWin activates it), so this is also how one is focused.
    pub fn set_keyboard(&self, on: bool) {
        if let Window::Layer(layer) = self {
            layer.set_keyboard_interactivity(if on {
                KeyboardInteractivity::OnDemand
            } else {
                KeyboardInteractivity::None
            });
        }
    }

    // Toplevel only.

    pub fn set_title(&self, title: impl Into<String>) {
        if let Some(window) = self.xdg() {
            window.set_title(title)
        }
    }

    pub fn set_app_id(&self, app_id: impl Into<String>) {
        if let Some(window) = self.xdg() {
            window.set_app_id(app_id)
        }
    }

    pub fn set_min_size(&self, min_size: Option<(u32, u32)>) {
        if let Some(window) = self.xdg() {
            window.set_min_size(min_size)
        }
    }

    pub fn set_max_size(&self, max_size: Option<(u32, u32)>) {
        if let Some(window) = self.xdg() {
            window.set_max_size(max_size)
        }
    }

    pub fn set_minimized(&self) {
        if let Some(window) = self.xdg() {
            window.set_minimized()
        }
    }

    pub fn set_maximized(&self) {
        if let Some(window) = self.xdg() {
            window.set_maximized()
        }
    }

    pub fn unset_maximized(&self) {
        if let Some(window) = self.xdg() {
            window.unset_maximized()
        }
    }

    pub fn set_fullscreen(&self, output: Option<&WlOutput>) {
        if let Some(window) = self.xdg() {
            window.set_fullscreen(output)
        }
    }

    pub fn unset_fullscreen(&self) {
        if let Some(window) = self.xdg() {
            window.unset_fullscreen()
        }
    }

    pub fn request_decoration_mode(&self, mode: Option<DecorationMode>) {
        if let Some(window) = self.xdg() {
            window.request_decoration_mode(mode)
        }
    }

    pub fn move_(&self, seat: &WlSeat, serial: u32) {
        if let Some(window) = self.xdg() {
            window.move_(seat, serial)
        }
    }

    pub fn resize(&self, seat: &WlSeat, serial: u32, edges: ResizeEdge) {
        if let Some(window) = self.xdg() {
            window.resize(seat, serial, edges)
        }
    }

    pub fn show_window_menu(&self, seat: &WlSeat, serial: u32, position: (i32, i32)) {
        if let Some(window) = self.xdg() {
            window.show_window_menu(seat, serial, position)
        }
    }
}
