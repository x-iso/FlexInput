//! Learned layouts of raw joysticks (the `sdl:raw:` devices), as the device
//! node's Learn sees them.
//!
//! The layouts live in settings (`AppSettings::raw_pad_profiles`), which the
//! canvas has no route to — so, like the light-bar swatch (`device_led`), the
//! app publishes a snapshot each frame and edits come back through a queue it
//! drains. See `flexinput_core::raw_pad` for what a layout is.

use std::collections::HashMap;
use std::sync::Arc;

use flexinput_core::raw_pad::{ImuMap, RawBind, RawPadProfile, RawPadProfiles, RawSrc};

/// An edit made on a raw pad's node.
#[derive(Clone, Debug, PartialEq)]
pub enum RawEdit {
    /// Learned an element (replaces what it supersedes — `RawPadProfile::set`).
    Bind(RawBind),
    /// Forget whatever drives this pin (a gyro / accel pin: that sensor).
    RemovePin(String),
    /// Motion setup's result; a `None` leaves that sensor as it was.
    SetMotion { gyro: Option<ImuMap>, accel: Option<ImuMap> },
}

struct Snapshot {
    profiles: Arc<RawPadProfiles>,
    /// Device id → the profile key its model is stored under.
    keys: HashMap<String, String>,
}

static SNAPSHOT: std::sync::Mutex<Option<Snapshot>> = std::sync::Mutex::new(None);
/// `(profile key, device name, edit)`.
static REQUESTS: std::sync::Mutex<Vec<(String, String, RawEdit)>> = std::sync::Mutex::new(Vec::new());

/// Publish this frame's layouts and which raw device uses which (called by
/// the app before panels draw).
pub fn publish(profiles: Arc<RawPadProfiles>, keys: HashMap<String, String>) {
    if let Ok(mut g) = SNAPSHOT.lock() {
        *g = Some(Snapshot { profiles, keys });
    }
}

/// The profile key of a connected raw device; `None` if it isn't one (or
/// isn't connected).
pub fn key_of(device_id: &str) -> Option<String> {
    SNAPSHOT.lock().ok()?.as_ref()?.keys.get(device_id).cloned()
}

/// The layout learned for a connected raw device; empty when nothing is yet.
pub fn profile_of(device_id: &str) -> Option<RawPadProfile> {
    let g = SNAPSHOT.lock().ok()?;
    let s = g.as_ref()?;
    let key = s.keys.get(device_id)?;
    Some(s.profiles.get(key).cloned().unwrap_or_default())
}

/// What to call an `extra:` pin — a raw pad's element kept as its own port —
/// wherever it shows up away from its node (a Remapper chip, a Splitter row):
/// the name the user gave it, else the element's.
pub fn extra_label(pin: &str) -> String {
    let named = SNAPSHOT.lock().ok().and_then(|g| {
        g.as_ref()?.profiles.values()
            .flat_map(|p| p.binds.iter())
            .find(|b| b.pin == pin && !b.label.is_empty())
            .map(|b| b.label.clone())
    });
    named
        .or_else(|| RawSrc::from_extra_pin_id(pin).map(|s| s.label()))
        .unwrap_or_else(|| pin.to_string())
}

/// The `extra:` pins a connected raw device publishes, with their names —
/// what a Splitter downstream of it can add.
pub fn extra_pins_of(device_id: &str) -> Vec<(String, String, flexinput_core::SignalType)> {
    profile_of(device_id)
        .map(|p| p.binds.iter()
            .filter(|b| b.is_own_port())
            .map(|b| {
                let name = if b.label.is_empty() { b.src.label() } else { b.label.clone() };
                (b.pin.clone(), name, b.src.signal_type())
            })
            .collect())
        .unwrap_or_default()
}

/// Queue an edit to the layout stored under `key`; `name` labels a profile
/// this creates.
pub fn request(key: &str, name: &str, edit: RawEdit) {
    if let Ok(mut q) = REQUESTS.lock() {
        q.push((key.to_string(), name.to_string(), edit));
    }
}

/// Drain queued edits (called by the app each frame).
pub fn drain_requests() -> Vec<(String, String, RawEdit)> {
    REQUESTS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

/// Apply queued edits to the stored layouts. Returns true if anything
/// changed. A layout left with nothing in it is dropped, so forgetting every
/// element leaves no trace in settings.
pub fn apply_edits(profiles: &mut RawPadProfiles, edits: Vec<(String, String, RawEdit)>) -> bool {
    let mut changed = false;
    for (key, name, edit) in edits {
        let p = profiles.entry(key).or_default();
        if p.name.is_empty() {
            p.name = name;
        }
        match edit {
            RawEdit::Bind(b) => {
                p.set(b);
                changed = true;
            }
            RawEdit::RemovePin(pin) => changed |= p.remove_pin(&pin),
            RawEdit::SetMotion { gyro, accel } => {
                changed |= gyro.is_some() || accel.is_some();
                p.set_motion(gyro, accel);
            }
        }
    }
    profiles.retain(|_, p| !p.is_empty());
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use flexinput_core::raw_pad::RawSrc;

    fn bind(src: RawSrc, pin: &str) -> RawBind {
        RawBind { src, pin: pin.into(), rest: 0.0, full: 1.0, label: String::new() }
    }

    #[test]
    fn edits_build_a_named_profile_and_forgetting_everything_drops_it() {
        let mut profiles = RawPadProfiles::new();
        let changed = apply_edits(&mut profiles, vec![
            ("k".into(), "USB Joystick".into(), RawEdit::Bind(bind(RawSrc::Button(0), "btn_south"))),
            ("k".into(), "ignored".into(), RawEdit::Bind(bind(RawSrc::Button(1), "btn_east"))),
        ]);
        assert!(changed);
        assert_eq!(profiles["k"].name, "USB Joystick");
        assert_eq!(profiles["k"].binds.len(), 2);

        assert!(!apply_edits(&mut profiles, vec![
            ("k".into(), String::new(), RawEdit::RemovePin("btn_north".into())),
        ]), "removing an unbound pin changes nothing");

        apply_edits(&mut profiles, vec![
            ("k".into(), String::new(), RawEdit::RemovePin("btn_south".into())),
            ("k".into(), String::new(), RawEdit::RemovePin("btn_east".into())),
        ]);
        assert!(profiles.is_empty());
    }
}
