/// Smoke tests for sink output routing to physical device backends.
///
/// The production code that drives physical devices lives inside
/// `spawn_io_thread` in `crates/ui/src/app.rs`.  That function is
/// intentionally not exposed as a public API, so these tests verify the
/// routing *logic pattern* in isolation rather than end-to-end with a live
/// I/O thread.
///
/// Specifically: only `gilrs:`-prefixed device IDs should be forwarded to
/// `DeviceBackend::send()`.  Non-gilrs IDs (virtual devices, midi_out:, etc.)
/// must NOT be forwarded via the backend path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use flexinput_core::Signal;
use flexinput_devices::{DeviceBackend, PhysicalDevice};

// ── Minimal mock backend ───────────────────────────────────────────────────────

struct MockBackend {
    calls: Arc<Mutex<Vec<(String, String, Signal)>>>,
}

impl MockBackend {
    fn new(calls: Arc<Mutex<Vec<(String, String, Signal)>>>) -> Self {
        Self { calls }
    }
}

impl DeviceBackend for MockBackend {
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        vec![]
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        vec![]
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        self.calls
            .lock()
            .unwrap()
            .push((device_id.to_string(), pin_id.to_string(), signal));
    }
}

// ── Routing logic helper (mirrors app.rs I/O thread dispatch) ─────────────────

/// Reproduces the physical-output dispatch predicate from `spawn_io_thread`:
///
/// ```text
/// for ((device_id, pin_id), &signal) in &sink_outputs {
///     if device_id.starts_with("gilrs:") {
///         for backend in &mut backends { backend.send(device_id, pin_id, signal); }
///     }
/// }
/// ```
fn dispatch_sink_outputs(
    sink_outputs: &HashMap<(String, String), Signal>,
    backends: &mut [Box<dyn DeviceBackend>],
) {
    for ((device_id, pin_id), &signal) in sink_outputs {
        if device_id.starts_with("gilrs:") {
            for backend in backends.iter_mut() {
                backend.send(device_id, pin_id, signal);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn gilrs_sink_outputs_reach_backend() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut backends: Vec<Box<dyn DeviceBackend>> =
        vec![Box::new(MockBackend::new(Arc::clone(&calls)))];

    let mut sink_outputs = HashMap::new();
    sink_outputs.insert(
        ("gilrs:0".to_string(), "rumble_strong".to_string()),
        Signal::Float(0.75),
    );
    sink_outputs.insert(
        ("gilrs:0".to_string(), "rumble_weak".to_string()),
        Signal::Float(0.5),
    );

    dispatch_sink_outputs(&sink_outputs, &mut backends);

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        2,
        "Both rumble pins should reach the backend; got {:?}",
        *recorded
    );

    let has_strong = recorded.iter().any(|(dev, pin, sig)| {
        dev == "gilrs:0" && pin == "rumble_strong" && *sig == Signal::Float(0.75)
    });
    let has_weak = recorded.iter().any(|(dev, pin, sig)| {
        dev == "gilrs:0" && pin == "rumble_weak" && *sig == Signal::Float(0.5)
    });

    assert!(has_strong, "rumble_strong should be forwarded to backend");
    assert!(has_weak, "rumble_weak should be forwarded to backend");
}

#[test]
fn non_gilrs_sink_outputs_do_not_reach_backend() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut backends: Vec<Box<dyn DeviceBackend>> =
        vec![Box::new(MockBackend::new(Arc::clone(&calls)))];

    let mut sink_outputs = HashMap::new();
    // Virtual XInput device — should NOT go through DeviceBackend::send().
    sink_outputs.insert(
        ("virtual.xinput".to_string(), "south".to_string()),
        Signal::Bool(true),
    );
    // MIDI output — should NOT go through DeviceBackend::send().
    sink_outputs.insert(
        ("midi_out:0".to_string(), "cc1".to_string()),
        Signal::Float(0.9),
    );

    dispatch_sink_outputs(&sink_outputs, &mut backends);

    let recorded = calls.lock().unwrap();
    assert!(
        recorded.is_empty(),
        "Non-gilrs device IDs must not be sent to DeviceBackend::send(); got {:?}",
        *recorded
    );
}

#[test]
fn rgb_and_dualsense_pins_route_via_gilrs_path() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut backends: Vec<Box<dyn DeviceBackend>> =
        vec![Box::new(MockBackend::new(Arc::clone(&calls)))];

    let mut sink_outputs = HashMap::new();
    // RGB lightbar pins (DualShock 4 / DualSense).
    sink_outputs.insert(
        ("gilrs:1".to_string(), "lightbar_r".to_string()),
        Signal::Float(1.0),
    );
    sink_outputs.insert(
        ("gilrs:1".to_string(), "lightbar_g".to_string()),
        Signal::Float(0.0),
    );
    sink_outputs.insert(
        ("gilrs:1".to_string(), "lightbar_b".to_string()),
        Signal::Float(0.5),
    );
    // DualSense adaptive trigger pins (new pin names).
    sink_outputs.insert(
        ("gilrs:1".to_string(), "trigger_r_strength".to_string()),
        Signal::Float(0.8),
    );
    sink_outputs.insert(
        ("gilrs:1".to_string(), "trigger_l_strength".to_string()),
        Signal::Float(0.6),
    );

    dispatch_sink_outputs(&sink_outputs, &mut backends);

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        5,
        "All 5 physical output pins (RGB + trigger strength) should reach backend; got {:?}",
        *recorded
    );
}
