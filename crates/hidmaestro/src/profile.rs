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
    #[serde(rename = "productString")]
    product_string: Option<String>,
    #[serde(rename = "deviceDescription")]
    device_description: Option<String>,
    #[serde(rename = "versionNumber")]
    version_number: Option<u32>,
    /// HID report descriptor as a hex string.
    descriptor: String,
    /// Usage (hex string, e.g. "0x32") → semantic role (e.g. "rightStickX").
    #[serde(rename = "axisMap")]
    axis_map: Option<std::collections::HashMap<String, String>>,
    /// HMButton bit index → descriptor button index (1-based in the descriptor).
    #[serde(rename = "buttonMap")]
    button_map: Option<Vec<i32>>,
    /// The real device's full input-report layout (byte-addressed). Carries the
    /// gyro/accel/touchpad regions the HID *descriptor* exposes only as opaque
    /// vendor blobs, so the encoder can drive IMU + neutralize the touchpad.
    #[serde(rename = "extendedReport")]
    extended_report: Option<RawExtendedReport>,
    /// The real device's output report layout — used to locate the rumble motor
    /// bytes (which differ between DS4 and DualSense) for feedback decode.
    #[serde(rename = "extendedOutputReport")]
    extended_output_report: Option<RawExtendedReport>,
    /// True for Xbox360/XInput profiles that need an XUSB companion devnode
    /// (created alongside the HID node so `xinput`/Steam discover the pad).
    #[serde(rename = "requiresXusbCompanion")]
    requires_xusb_companion: Option<bool>,
    /// HIDMaestro driver `FunctionMode`: 0 = plain HID, 1 = XUSB/XInput. Defaults
    /// to 1 when `requiresXusbCompanion`, else 0. An explicit value overrides
    /// (lets us test the mode independently of companion creation).
    #[serde(rename = "functionMode")]
    function_mode: Option<u32>,
}

/// Raw `extendedReport` shape (subset). Byte offsets are 1-based in the JSON
/// (byte 0 is the report ID), matching the on-wire report including the RID.
#[derive(Debug, Deserialize)]
struct RawExtendedReport {
    fields: Vec<RawExtField>,
}

#[derive(Debug, Deserialize)]
struct RawExtField {
    byte: Option<usize>,
    /// Range form, e.g. "45-47" for an rgb24 lightbar. We only need its start.
    bytes: Option<String>,
    #[serde(rename = "type")]
    ty: Option<String>,
    semantic: Option<String>,
}

impl RawExtField {
    /// The field's starting byte offset, from either `byte` (scalar) or the start
    /// of a `bytes:"lo-hi"` range.
    fn start_byte(&self) -> Option<usize> {
        if let Some(b) = self.byte {
            return Some(b);
        }
        let r = self.bytes.as_deref()?;
        let lo = r.split('-').next()?.trim();
        lo.parse().ok()
    }
}

/// Byte offsets (into the on-wire report, RID included at byte 0) for the IMU
/// and touchpad regions the descriptor doesn't expose as drivable fields.
/// A field the profile doesn't declare is `None`; offsets address the low byte
/// of each little-endian int16.
#[derive(Debug, Clone, Default)]
pub struct ExtendedLayout {
    pub gyro_pitch: Option<usize>,
    pub gyro_yaw: Option<usize>,
    pub gyro_roll: Option<usize>,
    pub accel_x: Option<usize>,
    pub accel_y: Option<usize>,
    pub accel_z: Option<usize>,
    /// First byte of each touchpad finger record (the active-flag/counter byte).
    /// A real device sets bit 7 of this byte when the finger is NOT touching.
    pub touch_fingers: Vec<usize>,
    /// Output-report byte of the strong/left (large) rumble motor, RID-inclusive.
    pub out_left_motor: Option<usize>,
    /// Output-report byte of the weak/right (small) rumble motor, RID-inclusive.
    pub out_right_motor: Option<usize>,
    /// Input-report byte carrying the battery level (RID-inclusive). The virtual
    /// pad stamps this to "full" every frame — a low reported battery makes some
    /// hosts disable haptics, and there's nothing to gain from a virtual pad
    /// reporting anything else. `None` when the profile declares no battery field.
    pub battery_level: Option<usize>,
    /// OUTPUT-report byte of the lightbar's first (R) channel, RID-inclusive; G/B
    /// follow at +1/+2 (the report packs them as a contiguous rgb24). DS4 +
    /// DualSense. Lets `poll_outputs` surface the game's lightbar colour so it can
    /// route to a physical pad's light bar. `None` when the profile has no lightbar.
    pub out_lightbar_r: Option<usize>,
    /// OUTPUT-report byte of the DualSense player-indicator LED field (RID-inclusive).
    pub out_player_led: Option<usize>,
    /// OUTPUT-report byte of the DualSense mic-mute LED field (RID-inclusive).
    pub out_mic_led: Option<usize>,
    /// OUTPUT-report byte where the DualSense RIGHT adaptive-trigger effect block
    /// begins (RID-inclusive). The block is 11 bytes: `[mode, params..]`.
    pub out_trigger_r: Option<usize>,
    /// OUTPUT-report byte where the LEFT adaptive-trigger effect block begins.
    pub out_trigger_l: Option<usize>,
}

impl ExtendedLayout {
    fn from_raw(input: Option<&RawExtendedReport>, output: Option<&RawExtendedReport>) -> Self {
        let mut out = ExtendedLayout::default();
        if let Some(raw) = input {
            for f in &raw.fields {
                let (Some(byte), Some(sem)) = (f.byte, f.semantic.as_deref()) else {
                    continue;
                };
                match sem {
                    "gyroPitch" => out.gyro_pitch = Some(byte),
                    "gyroYaw" => out.gyro_yaw = Some(byte),
                    "gyroRoll" => out.gyro_roll = Some(byte),
                    "accelX" => out.accel_x = Some(byte),
                    "accelY" => out.accel_y = Some(byte),
                    "accelZ" => out.accel_z = Some(byte),
                    "batteryLevel" => out.battery_level = Some(byte),
                    _ => {}
                }
                if f.ty.as_deref() == Some("touchpad-finger") {
                    out.touch_fingers.push(byte);
                }
            }
        }
        if let Some(raw) = output {
            for f in &raw.fields {
                let (Some(byte), Some(sem)) = (f.start_byte(), f.semantic.as_deref()) else {
                    continue;
                };
                match sem {
                    "leftMotor" => out.out_left_motor = Some(byte),
                    "rightMotor" => out.out_right_motor = Some(byte),
                    "lightbar" => out.out_lightbar_r = Some(byte),
                    "playerIndicator" => out.out_player_led = Some(byte),
                    "muteLed" => out.out_mic_led = Some(byte),
                    "rightTriggerEffect" => out.out_trigger_r = Some(byte),
                    "leftTriggerEffect" => out.out_trigger_l = Some(byte),
                    _ => {}
                }
            }
        }
        out
    }

    /// True if the profile declares a gyro region (all three axes present).
    pub fn has_gyro(&self) -> bool {
        self.gyro_pitch.is_some() && self.gyro_yaw.is_some() && self.gyro_roll.is_some()
    }
    /// True if the profile declares an accelerometer region.
    pub fn has_accel(&self) -> bool {
        self.accel_x.is_some() && self.accel_y.is_some() && self.accel_z.is_some()
    }
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
    /// USB product string (e.g. "Wireless Controller"). Used for the device's
    /// registry config + friendly name.
    pub product_string: Option<String>,
    /// Optional device description (defaults to `product_string`).
    pub device_description: Option<String>,
    /// USB bcdDevice version (defaults to 0x0100, the Sony USB convention).
    pub version_number: u32,
    /// Raw descriptor bytes (hex-decoded).
    pub descriptor: Vec<u8>,
    /// Parsed first-input-report layout.
    pub report: InputReport,
    /// usage-hex → role (verbatim from JSON; consumed by the encoder later).
    pub axis_map: std::collections::HashMap<String, String>,
    /// HMButton bit index → 1-based descriptor button index.
    pub button_map: Option<Vec<i32>>,
    /// Byte-addressed IMU + touchpad layout from `extendedReport` (the regions
    /// the HID descriptor exposes only as opaque vendor blobs). Empty when the
    /// profile declares no `extendedReport`.
    pub extended: ExtendedLayout,
    /// True if this profile needs an XUSB/XInput companion devnode (Xbox360).
    pub requires_xusb_companion: bool,
    /// HIDMaestro driver `FunctionMode` (0 = plain HID, 1 = XUSB).
    pub function_mode: u32,
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
        let extended = ExtendedLayout::from_raw(
            raw.extended_report.as_ref(),
            raw.extended_output_report.as_ref(),
        );
        let requires_xusb_companion = raw.requires_xusb_companion.unwrap_or(false);
        let function_mode = raw
            .function_mode
            .unwrap_or(if requires_xusb_companion { 1 } else { 0 });
        Ok(Profile {
            id: raw.id,
            name: raw.name,
            vid: parse_hex_u16(&raw.vid)?,
            pid: parse_hex_u16(&raw.pid)?,
            input_report_size,
            product_string: raw.product_string,
            device_description: raw.device_description,
            version_number: raw.version_number.unwrap_or(0x0100),
            descriptor,
            report,
            axis_map: raw.axis_map.unwrap_or_default(),
            button_map: raw.button_map,
            extended,
            requires_xusb_companion,
            function_mode,
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

    /// DualSense (CFI-ZCT1), USB — plain-HID Sony pad, `VID_054C&PID_0CE6`. The
    /// HIDMaestro capability ViGEm can't provide.
    pub const DUALSENSE_JSON: &str = include_str!("../profiles/dualsense.json");

    /// Xbox 360 / XInput (`VID_045E&PID_028E`). Needs the XUSB companion node;
    /// input flows through the SHM `GipData[]` (XINPUT_GAMEPAD) region, not the
    /// HID descriptor. This is the capability that lets FlexInput drop ViGEm.
    pub const XBOX360_JSON: &str = include_str!("../profiles/xbox360.json");
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
        // Plain-HID Sony pad: no XUSB companion, FunctionMode 0.
        assert!(!p.requires_xusb_companion);
        assert_eq!(p.function_mode, 0);
    }

    #[test]
    fn loads_xbox360_preset() {
        let p = Profile::from_json(presets::XBOX360_JSON).expect("load xbox360");
        assert_eq!(p.id, "xbox360");
        assert_eq!(p.vid, 0x045E);
        // PID is 0x02FF (Xbox One wired family), deliberately != the real 360's
        // 0x028E so the read side can distinguish our virtual from a physical Xbox
        // 360; VID stays 045E so it still classifies + works as XInput.
        assert_eq!(p.pid, 0x02FF);
        // XUSB companion + FunctionMode 1 (defaulted from requiresXusbCompanion).
        assert!(p.requires_xusb_companion);
        assert_eq!(p.function_mode, 1);
        // Descriptor must parse to a usable gamepad layout: the standard axes
        // and at least the face/shoulder buttons present.
        let r = &p.report;
        assert!(r.field(0x01, 0x30).is_some(), "X (left stick X)");
        assert!(r.field(0x01, 0x31).is_some(), "Y (left stick Y)");
        assert!(r.field(0x01, 0x32).is_some(), "Z (right stick X)");
        assert!(r.field(0x01, 0x35).is_some(), "Rz (right stick Y)");
        assert!(r.field(0x01, 0x33).is_some(), "Rx (left trigger)");
        assert!(r.field(0x01, 0x34).is_some(), "Ry (right trigger)");
        assert!(r.field(0x09, 0x01).is_some(), "button 1");
        assert!(r.field(0x09, 0x08).is_some(), "button 8");
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
