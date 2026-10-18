//! The composite personas: profile JSON vendored from HIDMaestro (MIT), each a
//! real pad's USB descriptors byte for byte, with `buttonMap` re-indexed to
//! flexinput-hidmaestro's button bits (see each file's notes). Their HID
//! report layout is the plain profile's, so the same encoder drives both.

/// DualSense (CFI-ZCT1), USB: speaker + haptics out (4 ch, 48 kHz, 16-bit;
/// channels 3 and 4 drive the voice-coil actuators), stereo microphone in, HID.
pub const DUALSENSE_COMPOSITE_JSON: &str = include_str!("../profiles/dualsense-composite.json");

/// DualSense Edge (CFI-ZCP1), USB: the same audio as the DualSense, the Edge's
/// own 389-byte HID descriptor and 1 ms input polling.
pub const DUALSENSE_EDGE_COMPOSITE_JSON: &str = include_str!("../profiles/dualsense-edge-composite.json");
