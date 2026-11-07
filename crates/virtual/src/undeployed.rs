//! The stand-in for a device we were asked for and could not build.
//!
//! It exists because a virtual device that fails to deploy used to be either
//! swallowed or fatal. A composite persona whose attach the host controller
//! refused came back as a device that looked present, so the per-frame pool
//! reconcile treated the node as satisfied: nothing was ever deployed, nothing
//! retried, and the only trace was a line on stderr. And a build that returned
//! nothing at all fell through the kind table to `panic!`, on the device-ops
//! worker thread — killing the one thread that answers every later deploy
//! request, after which the app quietly stopped deploying devices at all.
//!
//! This holds the identity the pool and the canvas node expect, owns no OS
//! resource, reports itself disconnected, and carries the reason, so the UI can
//! say what happened instead.

use crate::{Signal, SinkPin, VirtualDevice};

/// A device that was asked for and never made it onto the system.
pub struct UndeployedDevice {
    id: String,
    display_name: String,
    pins: &'static [SinkPin],
    error: String,
}

impl UndeployedDevice {
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        pins: &'static [SinkPin],
        error: impl Into<String>,
    ) -> Self {
        Self { id: id.into(), display_name: display_name.into(), pins, error: error.into() }
    }
}

/// The stand-in for `kind_id`, with the pins that kind's node would have had,
/// so the node keeps its inputs and its label while the reason is shown.
pub fn stand_in(
    kind_id: &str,
    id: impl Into<String>,
    display_name: impl Into<String>,
    error: impl Into<String>,
) -> UndeployedDevice {
    let pins = crate::kind_pin_metadata(kind_id, 0).map(|(sink, _, _)| sink).unwrap_or(&[]);
    UndeployedDevice::new(id, display_name, pins, error)
}

impl VirtualDevice for UndeployedDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn sink_pins(&self) -> &'static [SinkPin] {
        self.pins
    }

    fn send(&mut self, _pin: &str, _value: Signal) {}

    fn flush(&mut self) {}

    fn is_connected(&self) -> bool {
        false
    }

    fn deploy_error(&self) -> Option<String> {
        Some(self.error.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts;

    #[test]
    fn it_keeps_the_identity_of_the_device_it_stands_in_for() {
        let d = UndeployedDevice::new(
            "virtual.usb.dualsense_edge",
            "Virtual DualSense Edge",
            layouts::DUALSENSE_EDGE_SINK_PINS,
            "usbip-win2 refused the attach",
        );
        assert_eq!(d.id(), "virtual.usb.dualsense_edge");
        assert_eq!(d.display_name(), "Virtual DualSense Edge");
        assert!(!d.sink_pins().is_empty(), "the node keeps its inputs");
        assert!(!d.is_connected(), "nothing is plugged in");
        assert_eq!(d.deploy_error().as_deref(), Some("usbip-win2 refused the attach"));
    }

    #[test]
    fn stand_in_finds_the_pins_of_the_kind_it_replaces() {
        let d = stand_in("virtual.usb.dualsense_edge", "virtual.usb.dualsense_edge", "Edge", "no");
        assert!(!d.sink_pins().is_empty(), "the kind's own layout, not nothing");
        assert_eq!(d.id(), "virtual.usb.dualsense_edge");
    }

    #[test]
    fn stand_in_for_an_unknown_kind_still_holds_its_identity() {
        let d = stand_in("virtual.nope", "virtual.nope", "Nope", "unknown kind");
        assert!(d.sink_pins().is_empty(), "no layout to borrow");
        assert_eq!(d.deploy_error().as_deref(), Some("unknown kind"));
    }
}
