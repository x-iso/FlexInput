//! FLEXINPUT PATCH: touchpad pinches on Wayland (`zwp_pointer_gestures_v1`).
//!
//! winit 0.30 reports a pinch only on macOS and iOS, so on Wayland a two-finger pinch on a
//! touchpad did nothing (two-finger scrolling still came through, as the wheel). This reports
//! it the way macOS does: `WindowEvent::PinchGesture`, its delta the change of scale since the
//! last update as a log, so the deltas add up to the log of the gesture's whole scale and
//! `exp(delta)` is how much this update zoomed. A pinch is reported to the window it began
//! over, until it ends.

use std::ops::Deref;
use std::sync::Mutex;

use sctk::compositor::SurfaceData;
use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_pinch_v1::{self, ZwpPointerGesturePinchV1},
    zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
};

use crate::event::{TouchPhase, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

/// The compositor's pointer gestures.
pub struct PointerGesturesState {
    manager: ZwpPointerGesturesV1,
}

impl PointerGesturesState {
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        // Version 1 has pinches; later ones add holds, which nothing here asks for.
        let manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        Ok(Self { manager })
    }
}

impl Deref for PointerGesturesState {
    type Target = ZwpPointerGesturesV1;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

/// A pinch in progress on one pointer: the window it began over, and the scale (from where
/// the fingers started) it last reported.
#[derive(Debug, Default)]
pub struct PinchData {
    inner: Mutex<Option<(WindowId, f64)>>,
}

impl Dispatch<ZwpPointerGesturesV1, GlobalData, WinitState> for PointerGesturesState {
    fn event(
        _state: &mut WinitState,
        _proxy: &ZwpPointerGesturesV1,
        _event: <ZwpPointerGesturesV1 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, PinchData, WinitState> for PointerGesturesState {
    fn event(
        state: &mut WinitState,
        _proxy: &ZwpPointerGesturePinchV1,
        event: <ZwpPointerGesturePinchV1 as Proxy>::Event,
        data: &PinchData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
        let mut pinch = data.inner.lock().unwrap();
        match event {
            zwp_pointer_gesture_pinch_v1::Event::Begin { surface, .. } => {
                // Over a window's decorations, the window itself.
                let Some(surface_data) = surface.data::<SurfaceData>() else {
                    *pinch = None;
                    return;
                };
                let window_id = wayland::make_wid(surface_data.parent_surface().unwrap_or(&surface));
                if !state.windows.get_mut().contains_key(&window_id) {
                    *pinch = None;
                    return;
                }
                *pinch = Some((window_id, 1.0));
                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture { device_id, delta: 0.0, phase: TouchPhase::Started },
                    window_id,
                );
            },
            zwp_pointer_gesture_pinch_v1::Event::Update { scale, .. } => {
                let Some((window_id, last)) = pinch.as_mut() else { return };
                if !(scale > 0.0 && *last > 0.0) {
                    return;
                }
                let delta = (scale / *last).ln();
                *last = scale;
                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture { device_id, delta, phase: TouchPhase::Moved },
                    *window_id,
                );
            },
            zwp_pointer_gesture_pinch_v1::Event::End { cancelled, .. } => {
                let Some((window_id, _)) = pinch.take() else { return };
                let phase = if cancelled != 0 { TouchPhase::Cancelled } else { TouchPhase::Ended };
                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture { device_id, delta: 0.0, phase },
                    window_id,
                );
            },
            _ => {},
        }
    }
}

delegate_dispatch!(WinitState: [ZwpPointerGesturesV1: GlobalData] => PointerGesturesState);
delegate_dispatch!(WinitState: [ZwpPointerGesturePinchV1: PinchData] => PointerGesturesState);
