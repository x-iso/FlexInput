//! Controller profile model — the subset of HIDMaestro's profile JSON that the
//! preset backends need.
//!
//! A profile JSON (see `crates/hidmaestro/profiles/*.json`, vendored from
//! HIDMaestro, MIT) carries everything required to deploy and drive a virtual
//! device: VID/PID, the raw HID **report descriptor** (hex), the input report
//! size, and the axis/button maps. Per the Phase-1 finding, the descriptor is
//! *data we carry verbatim*, not something we generate — so this module loads
//! it, parses it with [`crate::descriptor`], and exposes a usage-addressed
//! field set for the preset encoders.
//!
//! The fiddly semantic-axis *resolution* heuristics are intentionally not ported
//! yet; presets address fields by HID usage directly via [`Profile::field`].

use serde::Deserialize;

use crate::descriptor::{parse_descriptor, InputReport};

/// Raw deserialization shape of a HIDMaestro profile JSON (subset of fields).
#[derive(Debug, Deserialize)]
struct RawProfile {
    id: String,
    name: String,
    /// VID/PID are JSON strings like "0x054C".
    vid: String,
    pid: String,
    #[serde(rename = "inputReportSize")]
    input_report_size: Option<usize>,
    /// HID report descriptor as a hex string.
    descriptor: String,
    /// Usage (hex string, e.g. "0x32") → semantic role (e.g. "rightStickX").
    #[serde(rename = "axisMap")]
    axis_map: Option<std::collections::HashMap<String, String>>,
    /// HMButton bit index → descriptor button index (1-based in the descriptor).
    #[serde(rename = "buttonMap")]
    button_map: Option<Vec<i32>>,
}

/// A loaded, descriptor-parsed controller profile ready to encode reports for.
#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub vid: u16,
    pub pid: u16,
    /// On-wire input report size in bytes (from JSON; falls back to the parsed
    /// descriptor's byte size when the JSON omits it).
    pub input_report_size: usize,
    /// Raw descriptor bytes (hex-decoded).
    pub descriptor: Vec<u8>,
    /// Parsed first-input-report layout.
    pub report: InputReport,
    /// usage-hex → role (verbatim from JSON; consumed by the encoder later).
    pub axis_map: std::collections::HashMap<String, String>,
    /// HMButton bit index → 1-based descriptor button index.
    pub button_map: Option<Vec<i32>>,
}

#[derive(Debug)]
pub enum ProfileError {
    Json(String),
    Hex(String),
    Vidpid(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::Json(e) => write!(f, "profile JSON parse error: {e}"),
            ProfileError::Hex(e) => write!(f, "descriptor hex decode error: {e}"),
            ProfileError::Vidpid(e) => write!(f, "VID/PID parse error: {e}"),
        }
    }
}
impl std::error::Error for ProfileError {}

fn parse_hex_u16(s: &str) -> Result<u16, ProfileError> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(t, 16).map_err(|e| ProfileError::Vidpid(format!("{s}: {e}")))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, ProfileError> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if !cleaned.len().is_multiple_of(2) {
        return Err(ProfileError::Hex("odd number of hex digits".into()));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|e| ProfileError::Hex(format!("at {i}: {e}")))
        })
        .collect()
}

impl Profile {
    /// Parse a profile from its JSON text.
    pub fn from_json(json: &str) -> Result<Self, ProfileError> {
        let raw: RawProfile =
            serde_json::from_str(json).map_err(|e| ProfileError::Json(e.to_string()))?;
        let descriptor = decode_hex(&raw.descriptor)?;
        let report = parse_descriptor(&descriptor);
        let input_report_size = raw.input_report_size.unwrap_or_else(|| report.byte_size());
        Ok(Profile {
            id: raw.id,
            name: raw.name,
            vid: parse_hex_u16(&raw.vid)?,
            pid: parse_hex_u16(&raw.pid)?,
            input_report_size,
            descriptor,
            report,
            axis_map: raw.axis_map.unwrap_or_default(),
            button_map: raw.button_map,
        })
    }

    /// Look up a descriptor input field by `(usage_page, usage)`.
    pub fn field(&self, usage_page: u16, usage: u16) -> Option<&crate::descriptor::InputField> {
        self.report.field(usage_page, usage)
    }
}

/// Bundled preset profiles (vendored JSON). Add more as backends ship.
pub mod presets {
    /// DualShock 4 v2 (CUH-ZCT2), USB — a plain-HID Report-0x01 gamepad. Chosen
    /// as the Phase-2 validation target because its legacy `Data[]` report IS
    /// the wire report (no XUSB companion, unlike Xbox360).
    pub const DUALSHOCK_4_V2_JSON: &str = include_str!("../profiles/dualshock-4-v2.json");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_ds4v2_preset() {
        let p = Profile::from_json(presets::DUALSHOCK_4_V2_JSON).expect("load DS4v2");
        assert_eq!(p.id, "dualshock-4-v2");
        assert_eq!(p.vid, 0x054C);
        assert_eq!(p.pid, 0x09CC);
        assert_eq!(p.input_report_size, 64);
        assert_eq!(p.report.report_id, 0x01);
    }

    /// Golden: our parser must reproduce the exact field layout HIDMaestro's C#
    /// `info dualshock-4-v2` prints (captured 2026-06-12, v1.3.17). These
    /// offsets are load-bearing — a mismatch is the descriptor-side analog of
    /// the input-tearing bug.
    #[test]
    fn ds4v2_field_layout_matches_csharp_golden() {
        let p = Profile::from_json(presets::DUALSHOCK_4_V2_JSON).unwrap();
        let r = &p.report;

        // 504 input bits, Report ID 0x01, 76 fields, 64-byte wire report.
        assert_eq!(r.report_id, 0x01);
        assert_eq!(r.bit_size, 504);
        assert_eq!(r.fields.len(), 76);
        assert_eq!(r.byte_size(), 64);

        // Generic Desktop axes (usage page 0x01).
        let check = |up: u16, u: u16, off: i32, size: i32, lmax: i32| {
            let f = r.field(up, u).unwrap_or_else(|| panic!("missing usage {up:#x}/{u:#x}"));
            assert_eq!(
                (f.bit_offset, f.bit_size, f.logical_max),
                (off, size, lmax),
                "usage {up:#x}/{u:#x}"
            );
        };
        check(0x01, 0x30, 0, 8, 255); // X
        check(0x01, 0x31, 8, 8, 255); // Y
        check(0x01, 0x32, 16, 8, 255); // Z
        check(0x01, 0x35, 24, 8, 255); // Rz
        check(0x01, 0x39, 32, 4, 7); // Hat (4 bits, range 0..7)
        check(0x01, 0x33, 56, 8, 255); // Rx
        check(0x01, 0x34, 64, 8, 255); // Ry

        // Buttons 1..14 start at bit 36, one bit each.
        let b1 = r.field(0x09, 0x01).unwrap();
        assert_eq!((b1.bit_offset, b1.bit_size), (36, 1));
        let b14 = r.field(0x09, 0x0E).unwrap();
        assert_eq!((b14.bit_offset, b14.bit_size), (49, 1));
    }
}
