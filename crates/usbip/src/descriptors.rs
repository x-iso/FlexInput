//! The descriptor store for one emulated composite device: the verbatim
//! device / configuration / other-speed blobs a profile carries, its HID
//! report descriptor and its strings, answering every GET_DESCRIPTOR with a
//! real pad's bytes.
//!
//! Construction checks the blobs against each other and against the profile's
//! structured `usbConfiguration`, because the blob is the wire truth and the
//! structured spec drives routing: a profile where the two disagree is
//! mis-authored, and the device must not come up half right. Checks: the
//! device blob's VID/PID match the profile, the configuration blob's
//! wTotalLength matches its size, every endpoint the spec declares is in the
//! blob with the same transfer type, packet size and interval, and the HID
//! class descriptor's report length is the profile report descriptor's.
//!
//! Sony composites only: one HID interface, no Microsoft OS descriptors.
//! A profile asking for more is refused rather than half served.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::protocol::{SPEED_FULL, SPEED_HIGH};

#[derive(Debug)]
pub struct DescriptorError(pub String);

impl std::fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "composite profile: {}", self.0)
    }
}

impl std::error::Error for DescriptorError {}

fn err<T>(msg: impl Into<String>) -> Result<T, DescriptorError> {
    Err(DescriptorError(msg.into()))
}

// ── Profile JSON (the subset the usbip backend reads) ────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProfile {
    id: String,
    vid: String,
    pid: String,
    descriptor: String,
    product_string: Option<String>,
    manufacturer_string: Option<String>,
    serial_string: Option<String>,
    configuration_string: Option<String>,
    usb_configuration: Option<RawUsbConfiguration>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUsbConfiguration {
    device_descriptor: String,
    configuration_descriptor: String,
    other_speed_configuration_descriptor: Option<String>,
    bus_speed: Option<String>,
    #[serde(default)]
    interfaces: Vec<RawInterface>,
    #[serde(default)]
    audio_controls: Vec<RawAudioControl>,
    microsoft_os: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInterface {
    interface_number: u8,
    function: String,
    #[serde(default)]
    alt_settings: Vec<RawAltSetting>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAltSetting {
    alt_setting: u8,
    #[serde(default)]
    endpoints: Vec<RawEndpoint>,
    audio_stream: Option<RawAudioStream>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEndpoint {
    address: u8,
    transfer_type: String,
    max_packet_size: u16,
    interval: u8,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAudioStream {
    channels: u16,
    bits_per_sample: u16,
    sample_rate_hz: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAudioControl {
    unit_id: u8,
    #[serde(default)]
    mute_cur: i16,
    #[serde(default)]
    volume_min_raw: i16,
    #[serde(default)]
    volume_max_raw: i16,
    #[serde(default)]
    volume_res_raw: i16,
    #[serde(default)]
    volume_cur_raw: i16,
}

// ── Parsed model ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

/// One endpoint from the configuration blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointInfo {
    pub address: u8,
    pub attributes: u8,
    pub max_packet_size: u16,
    pub interval: u8,
    pub interface: u8,
    pub alt_setting: u8,
}

impl EndpointInfo {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    pub fn transfer_type(&self) -> TransferType {
        match self.attributes & 0x03 {
            0 => TransferType::Control,
            1 => TransferType::Isochronous,
            2 => TransferType::Bulk,
            _ => TransferType::Interrupt,
        }
    }
}

/// One audio streaming alternate setting: the format of its isochronous pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStream {
    pub interface: u8,
    pub alt_setting: u8,
    /// True for the microphone (device to host).
    pub is_in: bool,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub sample_rate_hz: u32,
}

impl AudioStream {
    /// Bytes of one sample frame (all channels).
    pub fn frame_bytes(&self) -> usize {
        self.channels as usize * (self.bits_per_sample as usize / 8)
    }

    /// Bytes of one 1 ms service interval. Every rate in scope is a whole
    /// number of samples per millisecond.
    pub fn bytes_per_ms(&self) -> usize {
        self.sample_rate_hz as usize / 1000 * self.frame_bytes()
    }
}

/// A UAC1 feature unit's mute and volume, as the real pad reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioControl {
    pub unit_id: u8,
    pub mute_cur: i16,
    pub volume_min: i16,
    pub volume_max: i16,
    pub volume_res: i16,
    pub volume_cur: i16,
}

#[derive(Debug, Clone)]
pub struct DescriptorSet {
    pub profile_id: String,
    pub device: Vec<u8>,
    pub configuration: Vec<u8>,
    pub other_speed: Option<Vec<u8>>,
    pub report: Vec<u8>,
    pub qualifier: [u8; 10],
    pub configuration_value: u8,
    pub num_interfaces: u8,
    pub vid: u16,
    pub pid: u16,
    pub bcd_device: u16,
    /// `usb_device_speed` for the import reply.
    pub speed: u32,
    /// Endpoints keyed by bEndpointAddress (direction bit included).
    pub endpoints: BTreeMap<u8, EndpointInfo>,
    pub hid_interface: u8,
    hid_class: Vec<u8>,
    pub audio_streams: Vec<AudioStream>,
    pub audio_controls: Vec<AudioControl>,
    manufacturer: Option<String>,
    product: Option<String>,
    serial: Option<String>,
    configuration_name: Option<String>,
    i_manufacturer: u8,
    i_product: u8,
    i_serial: u8,
    i_configuration: u8,
}

fn hex(s: &str, field: &str) -> Result<Vec<u8>, DescriptorError> {
    let clean: Vec<u8> = s.bytes().filter(|c| c.is_ascii_hexdigit()).collect();
    if !clean.len().is_multiple_of(2) {
        return err(format!("{field}: odd number of hex digits"));
    }
    clean
        .chunks(2)
        .map(|p| {
            u8::from_str_radix(std::str::from_utf8(p).unwrap_or("zz"), 16)
                .map_err(|e| DescriptorError(format!("{field}: {e}")))
        })
        .collect()
}

fn hex_u16(s: &str) -> Result<u16, DescriptorError> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(t, 16).map_err(|e| DescriptorError(format!("{s}: {e}")))
}

impl DescriptorSet {
    /// Build the store from a composite profile. `serial` is served at the
    /// device's serial index: a profile without a captured serial (the Sony
    /// pads declare iSerial 0) gets its iSerial pointed at the first string
    /// index nothing else uses, so Windows keys the USB instance on the serial
    /// rather than on the virtual port, and the device keeps one identity
    /// across lives.
    pub fn from_profile_json(json: &str, serial: &str) -> Result<Self, DescriptorError> {
        let raw: RawProfile =
            serde_json::from_str(json).map_err(|e| DescriptorError(format!("JSON: {e}")))?;
        let Some(cfg) = raw.usb_configuration else {
            return err(format!("'{}' has no usbConfiguration", raw.id));
        };
        if cfg.microsoft_os.is_some() {
            return err("Microsoft OS descriptors aren't supported");
        }
        let mut device = hex(&cfg.device_descriptor, "deviceDescriptor")?;
        let configuration = hex(&cfg.configuration_descriptor, "configurationDescriptor")?;
        let other_speed = cfg
            .other_speed_configuration_descriptor
            .as_deref()
            .map(|h| hex(h, "otherSpeedConfigurationDescriptor"))
            .transpose()?;
        let report = hex(&raw.descriptor, "descriptor")?;

        if device.len() != 18 || device[0] != 18 || device[1] != 0x01 {
            return err("deviceDescriptor is not an 18-byte device descriptor");
        }
        let vid = u16::from_le_bytes([device[8], device[9]]);
        let pid = u16::from_le_bytes([device[10], device[11]]);
        if (vid, pid) != (hex_u16(&raw.vid)?, hex_u16(&raw.pid)?) {
            return err(format!(
                "deviceDescriptor {vid:04X}:{pid:04X} doesn't match the profile's {}:{}",
                raw.vid, raw.pid
            ));
        }
        let bcd_device = u16::from_le_bytes([device[12], device[13]]);
        let speed = match cfg.bus_speed.as_deref() {
            Some(s) if s.eq_ignore_ascii_case("full") => SPEED_FULL,
            _ => SPEED_HIGH,
        };
        let i_manufacturer = device[14];
        let i_product = device[15];
        let mut i_serial = device[16];
        let i_configuration = configuration.get(6).copied().unwrap_or(0);
        let serial = match raw.serial_string {
            Some(captured) if !captured.is_empty() => Some(captured),
            _ => {
                if i_serial == 0 {
                    let mut next = 1u8;
                    while next == i_manufacturer || next == i_product || next == i_configuration {
                        next += 1;
                    }
                    i_serial = next;
                    device[16] = next;
                }
                Some(serial.to_string())
            }
        };

        // Device qualifier, from the device descriptor (USB 2.0 §9.6.2).
        let qualifier = [0x0A, 0x06, device[2], device[3], device[4], device[5], device[6], device[7], device[17], 0];

        if configuration.len() < 9 || configuration[1] != 0x02 {
            return err("configurationDescriptor doesn't start with a configuration header");
        }
        let total = u16::from_le_bytes([configuration[2], configuration[3]]) as usize;
        if total != configuration.len() {
            return err(format!("configurationDescriptor wTotalLength {total} != its {} bytes", configuration.len()));
        }
        let num_interfaces = configuration[4];
        let configuration_value = configuration[5];

        // Walk the blob: endpoint table and HID class descriptor(s).
        let mut endpoints = BTreeMap::new();
        let mut hid: Vec<(u8, Vec<u8>)> = Vec::new();
        let (mut cur_iface, mut cur_alt) = (0xFFu8, 0u8);
        let mut off = 0;
        while off + 2 <= configuration.len() {
            let len = configuration[off] as usize;
            if len < 2 || off + len > configuration.len() {
                return err(format!("configurationDescriptor is malformed at offset {off}"));
            }
            let d = &configuration[off..off + len];
            match d[1] {
                0x04 if len >= 9 => {
                    cur_iface = d[2];
                    cur_alt = d[3];
                }
                0x05 if len >= 7 => {
                    endpoints.insert(d[2], EndpointInfo {
                        address: d[2],
                        attributes: d[3],
                        max_packet_size: u16::from_le_bytes([d[4], d[5]]),
                        interval: d[6],
                        interface: cur_iface,
                        alt_setting: cur_alt,
                    });
                }
                0x21 if len >= 9 => hid.push((cur_iface, d.to_vec())),
                _ => {}
            }
            off += len;
        }
        let (hid_interface, hid_class) = match hid.as_slice() {
            [one] => one.clone(),
            [] => return err("configurationDescriptor has no HID interface"),
            _ => return err("more than one HID interface isn't supported"),
        };
        let declared = u16::from_le_bytes([hid_class[7], hid_class[8]]) as usize;
        if declared != report.len() {
            return err(format!(
                "HID class descriptor declares a {declared}-byte report descriptor; the profile's is {} bytes",
                report.len()
            ));
        }

        // The structured spec against the blob.
        let mut audio_streams = Vec::new();
        for iface in &cfg.interfaces {
            for alt in &iface.alt_settings {
                for ep in &alt.endpoints {
                    let Some(found) = endpoints.get(&ep.address) else {
                        return err(format!("spec endpoint {:#04x} is absent from the configuration blob", ep.address));
                    };
                    let want = match ep.transfer_type.as_str() {
                        "isochronous" => TransferType::Isochronous,
                        "bulk" => TransferType::Bulk,
                        "interrupt" => TransferType::Interrupt,
                        other => return err(format!("endpoint {:#04x}: unknown transfer type {other}", ep.address)),
                    };
                    if found.transfer_type() != want
                        || found.max_packet_size != ep.max_packet_size
                        || found.interval != ep.interval
                    {
                        return err(format!("spec endpoint {:#04x} disagrees with the blob", ep.address));
                    }
                }
                if let Some(s) = &alt.audio_stream {
                    let is_in = match iface.function.as_str() {
                        "audioStreamingIn" => true,
                        "audioStreamingOut" => false,
                        other => return err(format!("interface {} ({other}) declares an audio stream", iface.interface_number)),
                    };
                    audio_streams.push(AudioStream {
                        interface: iface.interface_number,
                        alt_setting: alt.alt_setting,
                        is_in,
                        channels: s.channels,
                        bits_per_sample: s.bits_per_sample,
                        sample_rate_hz: s.sample_rate_hz,
                    });
                }
            }
        }
        let audio_controls = cfg
            .audio_controls
            .iter()
            .map(|c| AudioControl {
                unit_id: c.unit_id,
                mute_cur: c.mute_cur,
                volume_min: c.volume_min_raw,
                volume_max: c.volume_max_raw,
                volume_res: c.volume_res_raw,
                volume_cur: c.volume_cur_raw,
            })
            .collect();

        Ok(DescriptorSet {
            profile_id: raw.id,
            device,
            configuration,
            other_speed,
            report,
            qualifier,
            configuration_value,
            num_interfaces,
            vid,
            pid,
            bcd_device,
            speed,
            endpoints,
            hid_interface,
            hid_class,
            audio_streams,
            audio_controls,
            manufacturer: raw.manufacturer_string,
            product: raw.product_string,
            serial,
            configuration_name: raw.configuration_string,
            i_manufacturer,
            i_product,
            i_serial,
            i_configuration,
        })
    }

    /// The serial served at the device's serial index.
    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    /// A standard GET_DESCRIPTOR, or `None` to stall as the real pad does.
    pub fn get_descriptor(&self, ty: u8, index: u8) -> Option<Vec<u8>> {
        match ty {
            0x01 => Some(self.device.clone()),
            0x02 if index == 0 => Some(self.configuration.clone()),
            0x03 => self.string_descriptor(index),
            // A high-speed device answers the qualifier (the DualSense and
            // Edge dumps show it); a full-speed-only one stalls it.
            0x06 if self.speed >= SPEED_HIGH => Some(self.qualifier.to_vec()),
            // Other-speed only when the profile carries a real capture of it.
            0x07 if index == 0 => self.other_speed.clone(),
            _ => None,
        }
    }

    /// A HID-class GET_DESCRIPTOR on the HID interface: 0x22 is the report
    /// descriptor, 0x21 the HID class descriptor.
    pub fn get_hid_descriptor(&self, ty: u8) -> Option<Vec<u8>> {
        match ty {
            0x22 => Some(self.report.clone()),
            0x21 => Some(self.hid_class.clone()),
            _ => None,
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        if index == 0 {
            return Some(vec![0x04, 0x03, 0x09, 0x04]); // one LANGID: en-US
        }
        let s = if index == self.i_manufacturer && index != 0 {
            self.manufacturer.as_deref()
        } else if index == self.i_product && index != 0 {
            self.product.as_deref()
        } else if index == self.i_serial && index != 0 {
            self.serial.as_deref()
        } else if index == self.i_configuration && index != 0 {
            self.configuration_name.as_deref()
        } else {
            None
        }?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut d = Vec::with_capacity(2 + units.len() * 2);
        d.push((2 + units.len() * 2) as u8);
        d.push(0x03);
        for u in units {
            d.extend_from_slice(&u.to_le_bytes());
        }
        Some(d)
    }

    /// The format of the audio stream an alternate setting opens, if any.
    pub fn audio_stream(&self, interface: u8, alt_setting: u8) -> Option<&AudioStream> {
        self.audio_streams.iter().find(|s| s.interface == interface && s.alt_setting == alt_setting)
    }

    /// The speaker/haptics (host to device) stream.
    pub fn output_stream(&self) -> Option<&AudioStream> {
        self.audio_streams.iter().find(|s| !s.is_in)
    }

    /// The microphone (device to host) stream.
    pub fn input_stream(&self) -> Option<&AudioStream> {
        self.audio_streams.iter().find(|s| s.is_in)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{DUALSENSE_COMPOSITE_JSON, DUALSENSE_EDGE_COMPOSITE_JSON};

    fn ds() -> DescriptorSet {
        DescriptorSet::from_profile_json(DUALSENSE_COMPOSITE_JSON, "FX0123456789AB").unwrap()
    }

    #[test]
    fn dualsense_composite_parses_its_four_interfaces() {
        let d = ds();
        assert_eq!((d.vid, d.pid), (0x054C, 0x0CE6));
        assert_eq!(d.num_interfaces, 4);
        assert_eq!(d.configuration.len(), 227);
        assert_eq!(d.speed, SPEED_HIGH);
        assert_eq!(d.hid_interface, 3);
        let ep = |a: u8| d.endpoints[&a];
        assert_eq!(ep(0x01).transfer_type(), TransferType::Isochronous);
        assert_eq!(ep(0x01).max_packet_size, 392);
        assert_eq!((ep(0x01).interface, ep(0x01).alt_setting), (1, 1));
        assert_eq!(ep(0x82).transfer_type(), TransferType::Isochronous);
        assert_eq!(ep(0x84).transfer_type(), TransferType::Interrupt);
        assert_eq!(ep(0x03).transfer_type(), TransferType::Interrupt);
        let out = d.output_stream().unwrap();
        assert_eq!((out.channels, out.bits_per_sample, out.sample_rate_hz), (4, 16, 48_000));
        assert_eq!(out.bytes_per_ms(), 384);
        assert_eq!(d.input_stream().unwrap().bytes_per_ms(), 192);
        assert_eq!(d.audio_controls.len(), 2);
    }

    #[test]
    fn serial_takes_the_first_free_string_index() {
        let d = ds();
        // The real pad: iManufacturer 1, iProduct 2, iSerial 0.
        assert_eq!(&d.device[14..17], &[1, 2, 3]);
        let s = d.get_descriptor(0x03, 3).unwrap();
        assert_eq!(s[1], 0x03);
        let text: Vec<u16> = s[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&text).unwrap(), "FX0123456789AB");
        assert_eq!(d.serial(), Some("FX0123456789AB"));
    }

    #[test]
    fn standard_descriptors_answer_and_stall_like_the_pad() {
        let d = ds();
        assert_eq!(d.get_descriptor(0x01, 0).unwrap().len(), 18);
        assert_eq!(d.get_descriptor(0x06, 0).unwrap(), vec![0x0A, 0x06, 0x00, 0x02, 0, 0, 0, 0x40, 0x01, 0]);
        assert!(d.get_descriptor(0x02, 1).is_none());
        assert!(d.get_descriptor(0x03, 0xEE).is_none(), "no Microsoft OS string");
        assert_eq!(d.get_descriptor(0x03, 0).unwrap(), vec![4, 3, 9, 4]);
        assert_eq!(d.get_hid_descriptor(0x22).unwrap().len(), 273);
    }

    #[test]
    fn edge_composite_parses_with_its_own_report_descriptor() {
        let d = DescriptorSet::from_profile_json(DUALSENSE_EDGE_COMPOSITE_JSON, "FX0000000000AA").unwrap();
        assert_eq!((d.vid, d.pid), (0x054C, 0x0DF2));
        assert_eq!(d.get_hid_descriptor(0x22).unwrap().len(), 389);
        assert!(d.other_speed.is_none(), "no full-speed capture of the Edge");
        assert!(d.get_descriptor(0x07, 0).is_none());
        assert_eq!(d.output_stream().unwrap().channels, 4);
    }

    #[test]
    fn a_report_descriptor_of_the_wrong_length_is_refused() {
        let mut v: serde_json::Value = serde_json::from_str(DUALSENSE_COMPOSITE_JSON).unwrap();
        let short = v["descriptor"].as_str().unwrap()[..20].to_string();
        v["descriptor"] = serde_json::Value::String(short);
        assert!(DescriptorSet::from_profile_json(&v.to_string(), "FX").is_err());
    }
}
