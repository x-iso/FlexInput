//! Virtual devices on Linux, under the same kind ids as on Windows so a patch
//! moves between the two unchanged: the Xbox 360 pad through uinput, the Sony
//! pads and the keyboard / mouse / media keys through UHID with the HIDMaestro
//! profiles. Both need write access to their `/dev` node, which [`access`] asks
//! for once.

pub(crate) mod access;
pub(crate) mod uhid;
mod uinput;

use crate::{layouts, DeviceKind, SinkPin, SourcePin, VirtualDevice};

pub use uinput::UiKeyTapper;

pub static DEVICE_KINDS: &[DeviceKind] = &[
    DeviceKind { kind_id: "virtual.hm.xinput",    display_name: "Virtual Xbox 360",          allows_multiple: true },
    DeviceKind { kind_id: "virtual.hm.ds4",       display_name: "Virtual DualShock 4",       allows_multiple: true },
    DeviceKind { kind_id: "virtual.hm.dualsense", display_name: "Virtual DualSense",         allows_multiple: true },
    DeviceKind { kind_id: "virtual.hm.dualsense_edge", display_name: "Virtual DualSense Edge", allows_multiple: true },
    DeviceKind { kind_id: "virtual.keymouse",     display_name: "Virtual Keyboard & Mouse",  allows_multiple: false },
];

/// The HIDMaestro profile a UHID pad is built from.
fn sony_profile_json(kind_id: &str) -> Option<&'static str> {
    use flexinput_hidmaestro::profile::presets;
    match kind_id {
        "virtual.hm.ds4" => Some(presets::DUALSHOCK_4_V2_JSON),
        "virtual.hm.dualsense" => Some(presets::DUALSENSE_JSON),
        "virtual.hm.dualsense_edge" => Some(presets::DUALSENSE_EDGE_JSON),
        _ => None,
    }
}

/// See `crate::kind_pin_metadata`.
pub fn kind_pin_metadata(
    kind_id: &str,
    instance: usize,
) -> Option<(&'static [SinkPin], &'static [SourcePin], String)> {
    let (sink, source, base_name): (&'static [SinkPin], &'static [SourcePin], &str) = match kind_id {
        "virtual.hm.xinput" => (layouts::XINPUT_SINK_PINS, layouts::XINPUT_SOURCE_PINS, "Virtual Xbox 360"),
        "virtual.hm.ds4" => (layouts::DS4_SINK_PINS, layouts::DS4_SOURCE_PINS, "Virtual DualShock 4"),
        "virtual.hm.dualsense" => (layouts::DUALSENSE_SINK_PINS, layouts::DUALSENSE_SOURCE_PINS, "Virtual DualSense"),
        "virtual.hm.dualsense_edge" => (layouts::DUALSENSE_EDGE_SINK_PINS, layouts::DUALSENSE_SOURCE_PINS, "Virtual DualSense Edge"),
        "virtual.keymouse" => (layouts::KEYMOUSE_DEFAULT_PINS, &[], "Virtual Keyboard & Mouse"),
        _ => return None,
    };
    Some((sink, source, instance_label(kind_id, base_name, instance).1))
}

pub fn create_device(kind_id: &str, instance: usize) -> Box<dyn VirtualDevice> {
    let base_name = DEVICE_KINDS
        .iter()
        .find(|k| k.kind_id == kind_id)
        .map_or("Virtual Controller", |k| k.display_name);
    let (device_id, display_name) = instance_label(kind_id, base_name, instance);
    if kind_id == "virtual.hm.xinput" {
        return Box::new(uinput::Xbox360::create(device_id, display_name));
    }
    if let Some(json) = sony_profile_json(kind_id) {
        // `None` only for an invalid bundled profile, a build bug.
        let dev = uhid::UhidDevice::create(&device_id, &display_name, json, instance as u32);
        return Box::new(dev.expect("bundled Sony profile parses"));
    }
    if kind_id == "virtual.keymouse" {
        return match crate::keymouse_hm::VirtualKeyMouseHm::create() {
            Some(km) => Box::new(km),
            None => Box::new(Unavailable { id: device_id, display_name }),
        };
    }
    panic!("Unknown virtual device kind: {kind_id}")
}

fn instance_label(base_id: &str, base_name: &str, instance: usize) -> (String, String) {
    if instance == 0 {
        (base_id.to_string(), base_name.to_string())
    } else {
        (format!("{base_id}.{instance}"), format!("{base_name} #{}", instance + 1))
    }
}

/// The keyboard & mouse when its nodes couldn't be made (no access): listed,
/// disconnected, writes dropped. Windows falls back to SendInput here; Linux
/// has no unprivileged injection to fall back to.
struct Unavailable {
    id: String,
    display_name: String,
}

impl VirtualDevice for Unavailable {
    fn id(&self) -> &str {
        &self.id
    }
    fn display_name(&self) -> &str {
        &self.display_name
    }
    fn sink_pins(&self) -> &'static [SinkPin] {
        layouts::KEYMOUSE_DEFAULT_PINS
    }
    fn send(&mut self, _pin: &str, _value: flexinput_core::Signal) {}
    fn flush(&mut self) {}
    fn is_connected(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_pin_metadata_and_a_profile_where_it_needs_one() {
        for k in DEVICE_KINDS {
            let (_, _, name) = kind_pin_metadata(k.kind_id, 0).unwrap_or_else(|| panic!("{}", k.kind_id));
            assert_eq!(name, k.display_name);
            if k.kind_id.starts_with("virtual.hm.") && k.kind_id != "virtual.hm.xinput" {
                let json = sony_profile_json(k.kind_id).unwrap();
                assert!(flexinput_hidmaestro::Profile::from_json(json).is_ok(), "{}", k.kind_id);
            }
        }
        assert_eq!(kind_pin_metadata("virtual.hm.xinput", 2).unwrap().2, "Virtual Xbox 360 #3");
        assert!(kind_pin_metadata("virtual.usb.dualsense", 0).is_none(), "no USB audio composite here");
    }
}
