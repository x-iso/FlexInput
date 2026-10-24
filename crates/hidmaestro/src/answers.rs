//! The answers HIDMaestro's v2.0 drivers serve only under the version contract
//! (see [`crate::identity`]):
//!
//! - `FeatureAnswers`: the controller's GET_FEATURE replies. For the Sony pads
//!   these are the calibration, pairing and firmware reports the v1 driver held
//!   in code; SDL, hid-playstation and games read them, and some (F1 22) drop a
//!   pad that answers zeros.
//! - `XusbAnswers`: the XUSB companion's IOCTL replies. Without them the
//!   companion registers no XInput interface at all.
//!
//! Both are compiled from HIDMaestro's own model files (`profiles/models`,
//! vendored from v2.0.1, MIT) into the blobs the drivers parse at add:
//! `driver.h`'s `HM_FEATURE_*` layout (the SDK's `FeatureStubTable.ToBlob`) and
//! `companion.c`'s `HM_XUSB_ANSWERS_*` layout (`XusbAnswers.Build`). Only the
//! parts of the model format the vendored models use are supported, and a model
//! using anything else fails to load (every vendored model is loaded by a test),
//! so a future model can't quietly compile into a different table.

use serde::Deserialize;

const SONY_DUALSENSE: &str = include_str!("../profiles/models/sony-dualsense.json");
const SONY_DUALSENSE_EDGE: &str = include_str!("../profiles/models/sony-dualsense-edge.json");
const SONY_DUALSHOCK4: &str = include_str!("../profiles/models/sony-dualshock4.json");
const XUSB_COMPANION: &str = include_str!("../profiles/models/xusb-companion.json");

// driver.h, HM_FEATURE_*.
const FEATURE_FORMAT: u32 = 4;
const FEATURE_HEADER: usize = 16;
const FEATURE_ENTRY: usize = 16;
const FEATURE_MAX_ENTRIES: usize = 64;
const FEATURE_MAX_DATA: usize = 64;
const FEATURE_MAX_ARENA: usize = 16384;
const FEATURE_MAX_SIZES: usize = 4;
const FEATURE_FLAG_NOTIFY: u8 = 0x10;

// companion.c, HM_XUSB_ANSWERS_*.
const XUSB_FORMAT: u32 = 4;
const XUSB_HEADER: usize = 12;
const XUSB_ENTRY: usize = 12;
const XUSB_MAX_ENTRIES: usize = 8;
const XUSB_MAX_DATA: usize = 64;
const XUSB_MAX_BLOB: usize = 512;
const XUSB_MODE_EXACT: u16 = 0;
const XUSB_MODE_UP_TO: u16 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelFile {
    #[serde(rename = "id")]
    _id: String,
    #[serde(rename = "comment", default)]
    _comment: Option<String>,
    #[serde(rename = "featureStubs")]
    feature_stubs: Option<FeatureStubs>,
    #[serde(rename = "xusbAnswers")]
    xusb_answers: Option<XusbModel>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureStubs {
    reports: Vec<FeatureReport>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureReport {
    id: String,
    size: Option<usize>,
    #[serde(rename = "minLength")]
    min_length: Option<usize>,
    sizes: Option<Vec<usize>>,
    data: String,
    notify: Option<bool>,
    patch: Option<Vec<FeaturePatch>>,
    #[serde(rename = "comment", default)]
    _comment: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeaturePatch {
    at: usize,
    length: usize,
    encoding: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct XusbModel {
    defaults: XusbDefaults,
    answers: Vec<XusbAnswer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct XusbDefaults {
    via: String,
    #[serde(rename = "subType")]
    sub_type: String,
    flags: Vec<String>,
    battery: XusbBattery,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct XusbBattery {
    #[serde(rename = "type")]
    kind: String,
    level: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct XusbAnswer {
    #[serde(rename = "name")]
    _name: String,
    ioctl: String,
    #[serde(rename = "minOutput")]
    min_output: u16,
    mode: String,
    bytes: String,
    size: Option<usize>,
    #[serde(default)]
    fields: Vec<XusbField>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct XusbField {
    at: usize,
    value: String,
}

/// The Sony model a controller of this identity takes, as HIDMaestro's
/// identity defaults pick it for a USB profile (`identity-defaults.json`).
fn sony_model(vid: u16, pid: u16) -> Option<&'static str> {
    if vid != 0x054C {
        return None;
    }
    Some(match pid {
        0x05C4 | 0x09CC | 0x0BA0 => SONY_DUALSHOCK4,
        0x0DF2 => SONY_DUALSENSE_EDGE,
        _ => SONY_DUALSENSE,
    })
}

/// The `FeatureAnswers` value for a controller of this identity at transport
/// index `index`: its model's table with the index patched in, or the header
/// alone for an identity without one.
pub fn feature_answers(vid: u16, pid: u16, index: u32) -> Vec<u8> {
    match sony_model(vid, pid) {
        // The vendored models compile (a test loads each one), so a failure here
        // can't happen in a build; an empty table is the safe answer if it did.
        Some(json) => compile_feature_answers(json, index).unwrap_or_else(|_| empty_feature_answers()),
        None => empty_feature_answers(),
    }
}

/// A table with no answers: the header alone.
pub fn empty_feature_answers() -> Vec<u8> {
    let mut blob = vec![0u8; FEATURE_HEADER];
    blob[0..4].copy_from_slice(&(FEATURE_HEADER as u32).to_le_bytes());
    blob[4..8].copy_from_slice(&FEATURE_FORMAT.to_le_bytes());
    blob
}

/// One plain entry: answered the same way whatever the host last SET.
struct FeatureEntry {
    id: u8,
    flags: u8,
    size: u8,
    min_length: u8,
    /// The sizes served, largest first, or empty.
    sizes: Vec<u8>,
    /// The answer, `size` bytes, patches applied.
    page: Vec<u8>,
}

fn compile_feature_answers(json: &str, index: u32) -> Result<Vec<u8>, String> {
    let model: ModelFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let reports = model.feature_stubs.ok_or("the model has no featureStubs")?.reports;
    if reports.len() > FEATURE_MAX_ENTRIES {
        return Err(format!("{} reports, past the driver's {FEATURE_MAX_ENTRIES}", reports.len()));
    }
    let mut entries = Vec::with_capacity(reports.len());
    for r in &reports {
        let at = format!("report {}", r.id);
        let id = parse_hex_byte(&r.id).ok_or_else(|| format!("{at}: id is not a hex byte"))?;
        let data = parse_hex(&r.data).ok_or_else(|| format!("{at}: data is not hex bytes"))?;
        let sizes = r.sizes.clone().unwrap_or_default();
        if sizes.len() > FEATURE_MAX_SIZES || sizes.iter().any(|&s| s == 0 || s > FEATURE_MAX_DATA) {
            return Err(format!("{at}: sizes {sizes:?} outside what the driver holds"));
        }
        // FeatureStubTable.Normalize: the largest size served, else the given
        // size, else the data's own length.
        let size = match (sizes.iter().max(), r.size) {
            (Some(&max), _) => max,
            (None, Some(s)) if s > 0 => s,
            _ => data.len(),
        };
        if size == 0 || size > FEATURE_MAX_DATA || data.len() > size {
            return Err(format!("{at}: size {size} with {} data bytes", data.len()));
        }
        let min_length = match sizes.iter().min() {
            Some(&min) => min,
            None => r.min_length.unwrap_or(0),
        };
        if min_length > size {
            return Err(format!("{at}: minLength {min_length} past size {size}"));
        }
        let mut page = data;
        page.resize(size, 0);
        for p in r.patch.iter().flatten() {
            if p.encoding != "uint8" || p.source != "index" || p.length != 1 || p.at >= size {
                return Err(format!("{at}: a patch this port doesn't apply ({} {} at {})", p.encoding, p.source, p.at));
            }
            page[p.at] = index as u8;
        }
        let mut sizes: Vec<u8> = sizes.iter().map(|&s| s as u8).collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        entries.push(FeatureEntry {
            id,
            // Without notify an answer publishes nothing on a UMDF2 controller.
            flags: if r.notify == Some(true) { FEATURE_FLAG_NOTIFY } else { 0 },
            size: size as u8,
            min_length: min_length as u8,
            sizes,
            page,
        });
    }
    let arena: usize = entries.iter().map(|e| e.sizes.len() + e.page.len()).sum();
    if arena > FEATURE_MAX_ARENA {
        return Err(format!("an arena of {arena} bytes, past the driver's {FEATURE_MAX_ARENA}"));
    }
    let arena_start = FEATURE_HEADER + entries.len() * FEATURE_ENTRY;
    let mut blob = vec![0u8; arena_start + arena];
    let len = blob.len() as u32;
    blob[0..4].copy_from_slice(&len.to_le_bytes());
    blob[4..8].copy_from_slice(&FEATURE_FORMAT.to_le_bytes());
    blob[8..10].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    // blob[10]: no reports a message entry follows; [11] reserved; [12..16] none.
    let mut offset = 0usize;
    for (i, e) in entries.iter().enumerate() {
        let h = &mut blob[FEATURE_HEADER + i * FEATURE_ENTRY..FEATURE_HEADER + (i + 1) * FEATURE_ENTRY];
        h[0] = e.id;
        h[1] = e.flags;
        // [2..7]: report slot, message offset and length, parameter offset and
        // byte, all 0 for a plain entry.
        h[7] = 1; // one page
        h[8] = e.size;
        h[9] = e.min_length;
        h[10] = e.sizes.len() as u8;
        h[12..14].copy_from_slice(&(offset as u16).to_le_bytes());
        let a = arena_start + offset;
        blob[a..a + e.sizes.len()].copy_from_slice(&e.sizes);
        let p = a + e.sizes.len();
        blob[p..p + e.page.len()].copy_from_slice(&e.page);
        offset += e.sizes.len() + e.page.len();
    }
    Ok(blob)
}

/// The `XusbAnswers` value for an Xbox 360 controller of this identity at
/// transport index `index`, with the model's defaults for everything a profile's
/// xinput block could set: a wired gamepad with voice and plug-in-module support.
pub fn xusb_answers(vid: u16, pid: u16, index: u32) -> Vec<u8> {
    compile_xusb_answers(XUSB_COMPANION, vid, pid, index).unwrap_or_default()
}

fn compile_xusb_answers(json: &str, vid: u16, pid: u16, index: u32) -> Result<Vec<u8>, String> {
    let model: ModelFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let x = model.xusb_answers.ok_or("the model has no xusbAnswers")?;
    if x.defaults.via != "xusbCompanion" {
        return Err(format!("defaults.via is {}", x.defaults.via));
    }
    // XInputSpec's tables.
    let sub_type: u8 = match x.defaults.sub_type.as_str() {
        "unknown" => 0x00,
        "gamepad" => 0x01,
        "wheel" => 0x02,
        "arcadeStick" => 0x03,
        "flightStick" => 0x04,
        "dancePad" => 0x05,
        "guitar" => 0x06,
        "guitarAlternate" => 0x07,
        "drumKit" => 0x08,
        "guitarBass" => 0x0B,
        "arcadePad" => 0x13,
        s => return Err(format!("subType {s}")),
    };
    let mut flags: u16 = 0;
    for f in &x.defaults.flags {
        flags |= match f.as_str() {
            "ffb" => 0x0001,
            "wireless" => 0x0002,
            "voice" => 0x0004,
            "pmd" => 0x0008,
            "noNavigation" => 0x0010,
            s => return Err(format!("flag {s}")),
        };
    }
    let battery_type: u8 = match x.defaults.battery.kind.as_str() {
        "disconnected" => 0x00,
        "wired" => 0x01,
        "alkaline" => 0x02,
        "nimh" => 0x03,
        "unknown" => 0xFF,
        s => return Err(format!("battery type {s}")),
    };
    let battery_level: u8 = match x.defaults.battery.level.as_str() {
        "empty" => 0x00,
        "low" => 0x01,
        "medium" => 0x02,
        "full" => 0x03,
        s => return Err(format!("battery level {s}")),
    };
    let value_of = |name: &str| -> Option<(u32, usize)> {
        Some(match name {
            "subType" => (sub_type as u32, 1),
            "flags" => (flags as u32, 2),
            "information.vid" | "capabilities.vid" => (vid as u32, 2),
            "information.pid" | "capabilities.pid" => (pid as u32, 2),
            "battery.type" => (battery_type as u32, 1),
            "battery.level" => (battery_level as u32, 1),
            "controllerIndex" => (index, 1),
            _ => return None,
        })
    };
    if x.answers.len() > XUSB_MAX_ENTRIES {
        return Err(format!("{} answers, past the companion's {XUSB_MAX_ENTRIES}", x.answers.len()));
    }
    let mut entries: Vec<(u32, u16, u16, Vec<u8>)> = Vec::with_capacity(x.answers.len());
    for a in &x.answers {
        let code = a
            .ioctl
            .strip_prefix("0x")
            .or_else(|| a.ioctl.strip_prefix("0X"))
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .ok_or_else(|| format!("ioctl {}", a.ioctl))?;
        let mode = match a.mode.as_str() {
            "exact" => XUSB_MODE_EXACT,
            "upTo" => XUSB_MODE_UP_TO,
            m => return Err(format!("mode {m}")),
        };
        let mut data = parse_hex(&a.bytes).ok_or_else(|| format!("{}: bytes", a.ioctl))?;
        let size = a.size.unwrap_or(data.len());
        if size < data.len() || size == 0 || size > XUSB_MAX_DATA {
            return Err(format!("{}: size {size}", a.ioctl));
        }
        data.resize(size, 0);
        for f in &a.fields {
            let (v, width) = value_of(&f.value).ok_or_else(|| format!("{}: value {}", a.ioctl, f.value))?;
            if f.at + width > size {
                return Err(format!("{}: {} at {} past {size}", a.ioctl, f.value, f.at));
            }
            for b in 0..width {
                data[f.at + b] = (v >> (8 * b)) as u8;
            }
        }
        entries.push((code, a.min_output, mode, data));
    }
    let arena: usize = entries.iter().map(|e| e.3.len()).sum();
    let len = XUSB_HEADER + entries.len() * XUSB_ENTRY + arena;
    if len > XUSB_MAX_BLOB {
        return Err(format!("a blob of {len} bytes, past the companion's {XUSB_MAX_BLOB}"));
    }
    let mut blob = vec![0u8; len];
    blob[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    blob[4..8].copy_from_slice(&XUSB_FORMAT.to_le_bytes());
    blob[8..12].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    let data_start = XUSB_HEADER + entries.len() * XUSB_ENTRY;
    let mut offset = 0usize;
    for (i, (code, min_output, mode, data)) in entries.iter().enumerate() {
        let at = XUSB_HEADER + i * XUSB_ENTRY;
        blob[at..at + 4].copy_from_slice(&code.to_le_bytes());
        blob[at + 4..at + 6].copy_from_slice(&min_output.to_le_bytes());
        blob[at + 6..at + 8].copy_from_slice(&mode.to_le_bytes());
        blob[at + 8..at + 10].copy_from_slice(&(offset as u16).to_le_bytes());
        blob[at + 10..at + 12].copy_from_slice(&(data.len() as u16).to_le_bytes());
        blob[data_start + offset..data_start + offset + data.len()].copy_from_slice(data);
        offset += data.len();
    }
    Ok(blob)
}

fn parse_hex_byte(s: &str) -> Option<u8> {
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u8::from_str_radix(h, 16).ok()
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() % 2 != 0 {
        return None;
    }
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }
    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    /// The driver's own acceptance test for a blob, `LoadFeatureAnswers` in
    /// driver.c, transcribed: a blob that fails it fails the device's start.
    fn driver_accepts_feature_blob(blob: &[u8]) -> Result<(), String> {
        if blob.len() < FEATURE_HEADER {
            return Err("short".into());
        }
        let count = u16_at(blob, 8) as usize;
        let on_count = blob[10] as usize;
        if u32_at(blob, 0) as usize != blob.len() || u32_at(blob, 4) != FEATURE_FORMAT
            || count > FEATURE_MAX_ENTRIES || on_count > 4
        {
            return Err("header".into());
        }
        let arena_start = FEATURE_HEADER + count * FEATURE_ENTRY;
        if arena_start > blob.len() || blob.len() - arena_start > FEATURE_MAX_ARENA {
            return Err("arena".into());
        }
        let arena = blob.len() - arena_start;
        for i in 0..count {
            let e = &blob[FEATURE_HEADER + i * FEATURE_ENTRY..][..FEATURE_ENTRY];
            let (flags, value_len, page_count, size, min_len, size_count) = (e[1], e[4], e[7], e[8], e[9], e[10]);
            let message = flags & 0x01 != 0;
            let echo = flags & 0x04 != 0;
            if flags & !0x1F != 0 {
                return Err(format!("entry {i}: flags"));
            }
            if !message && (value_len != 0 || flags & 0x02 != 0 || echo || page_count != 1) {
                return Err(format!("entry {i}: plain entry"));
            }
            if page_count == 0 || page_count > 16 {
                return Err(format!("entry {i}: pages"));
            }
            if size == 0 || size as usize > FEATURE_MAX_DATA || min_len > size || size_count as usize > FEATURE_MAX_SIZES {
                return Err(format!("entry {i}: size"));
            }
            let offset = u16_at(e, 12) as usize;
            let need = value_len as usize + size_count as usize + page_count as usize * size as usize;
            if offset + need > arena {
                return Err(format!("entry {i}: past the arena"));
            }
            let sizes = &blob[arena_start + offset + value_len as usize..][..size_count as usize];
            for k in 0..sizes.len() {
                if sizes[k] == 0 || sizes[k] > size || (k > 0 && sizes[k] > sizes[k - 1]) {
                    return Err(format!("entry {i}: sizes"));
                }
            }
        }
        Ok(())
    }

    /// The answer the driver serves for report `id` (`ServeFeature`), page 0.
    fn served(blob: &[u8], id: u8) -> Option<(u8, u8, Vec<u8>, Vec<u8>)> {
        let count = u16_at(blob, 8) as usize;
        let arena_start = FEATURE_HEADER + count * FEATURE_ENTRY;
        (0..count).find_map(|i| {
            let e = &blob[FEATURE_HEADER + i * FEATURE_ENTRY..][..FEATURE_ENTRY];
            if e[0] != id {
                return None;
            }
            let offset = arena_start + u16_at(e, 12) as usize;
            let sizes = blob[offset..offset + e[10] as usize].to_vec();
            let page = blob[offset + e[10] as usize..][..e[8] as usize].to_vec();
            Some((e[1], e[9], sizes, page))
        })
    }

    #[test]
    fn every_vendored_sony_model_compiles_into_a_blob_the_driver_takes() {
        for (name, json) in [("dualsense", SONY_DUALSENSE), ("edge", SONY_DUALSENSE_EDGE), ("ds4", SONY_DUALSHOCK4)] {
            let blob = compile_feature_answers(json, 3).unwrap_or_else(|e| panic!("{name}: {e}"));
            driver_accepts_feature_blob(&blob).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(u16_at(&blob, 8), 7, "{name}: seven answers");
        }
    }

    #[test]
    fn the_dualsense_table_matches_its_model() {
        let blob = feature_answers(0x054C, 0x0CE6, 5);
        // Calibration 0x05: 41 bytes, every answer notifies.
        let (flags, min_len, sizes, page) = served(&blob, 0x05).unwrap();
        assert_eq!(flags, FEATURE_FLAG_NOTIFY);
        assert_eq!((min_len, sizes.len(), page.len()), (41, 0, 41));
        assert_eq!(&page[..7], &[0x05, 0, 0, 0, 0, 0, 0]);
        // Pairing 0x09: 20 bytes, the MAC's last byte is the index.
        let (_, _, _, page) = served(&blob, 0x09).unwrap();
        assert_eq!(page.len(), 20);
        assert_eq!(&page[..7], &[0x09, 0x02, 0x48, 0x4D, 0x00, 0x00, 5]);
        // DS4 calibration 0x02: two sizes, largest first; the shorter is the minimum.
        let (_, min_len, sizes, page) = served(&blob, 0x02).unwrap();
        assert_eq!((sizes, min_len, page.len()), (vec![41, 37], 37, 41));
        // Firmware 0x20 is the captured 64 bytes.
        assert_eq!(served(&blob, 0x20).unwrap().3.len(), 64);
    }

    #[test]
    fn models_follow_the_identity_defaults() {
        let first_firmware_byte = |pid| served(&feature_answers(0x054C, pid, 0), 0x20).map(|a| a.3);
        assert_ne!(first_firmware_byte(0x0DF2), first_firmware_byte(0x0CE6), "the Edge has its own firmware line");
        // The DualShock 4 lists its calibration's references in its own order.
        assert_ne!(
            served(&feature_answers(0x054C, 0x09CC, 0), 0x05).map(|a| a.3),
            served(&feature_answers(0x054C, 0x0CE6, 0), 0x05).map(|a| a.3),
        );
        // No model: the header alone, which the driver takes.
        let empty = feature_answers(0x045E, 0x028E, 0);
        assert_eq!(empty, empty_feature_answers());
        assert_eq!(empty.len(), FEATURE_HEADER);
        driver_accepts_feature_blob(&empty).unwrap();
    }

    #[test]
    fn a_model_using_more_than_this_port_supports_is_refused() {
        let paged = SONY_DUALSENSE.replacen("\"notify\": true", "\"pages\": [\"05\"], \"notify\": true", 1);
        assert!(compile_feature_answers(&paged, 0).is_err());
        let serial = SONY_DUALSENSE.replacen("\"source\": \"index\"", "\"source\": \"serial\"", 1);
        assert!(compile_feature_answers(&serial, 0).is_err());
    }

    /// The companion's acceptance test, `LoadAnswers` in companion.c.
    fn companion_accepts(blob: &[u8]) -> Vec<(u32, u16, u16, Vec<u8>)> {
        assert!(blob.len() >= XUSB_HEADER && blob.len() <= XUSB_MAX_BLOB);
        assert_eq!(u32_at(blob, 0) as usize, blob.len());
        assert_eq!(u32_at(blob, 4), XUSB_FORMAT);
        let count = u32_at(blob, 8) as usize;
        assert!(count <= XUSB_MAX_ENTRIES);
        let arena = XUSB_HEADER + count * XUSB_ENTRY;
        (0..count)
            .map(|i| {
                let e = &blob[XUSB_HEADER + i * XUSB_ENTRY..][..XUSB_ENTRY];
                let (mode, offset, bytes) = (u16_at(e, 6), u16_at(e, 8) as usize, u16_at(e, 10) as usize);
                assert!(bytes <= XUSB_MAX_DATA && arena + offset + bytes <= blob.len());
                assert!(mode == XUSB_MODE_EXACT || mode == XUSB_MODE_UP_TO);
                (u32_at(e, 0), u16_at(e, 4), mode, blob[arena + offset..arena + offset + bytes].to_vec())
            })
            .collect()
    }

    #[test]
    fn the_xbox_360_companion_answers_as_a_wired_gamepad() {
        let answers = companion_accepts(&xusb_answers(0x045E, 0x028E, 2));
        assert_eq!(answers.len(), 5);
        let get = |code, min| answers.iter().find(|a| a.0 == code && a.1 == min).unwrap();
        // GET_INFORMATION: VID and PID at 8 and 10.
        assert_eq!(get(0x8000_6000, 0).3, [0x03, 0x01, 0x01, 0, 0, 0, 0, 0, 0x5E, 0x04, 0x8E, 0x02]);
        // GET_CAPABILITIES 0101: the gamepad subtype at 3.
        assert_eq!(get(0x8000_E004, 0).3[3], 0x01);
        // GET_CAPABILITIES 0102: subtype, voice|pmd flags, VID/PID, the index.
        let caps = &get(0x8000_E004, 36).3;
        assert_eq!(caps.len(), 36);
        assert_eq!((caps[3], u16_at(caps, 4), u16_at(caps, 6), u16_at(caps, 8), caps[12]), (1, 0x000C, 0x045E, 0x028E, 2));
        // GET_BATTERY_INFORMATION: wired, full (v1.8.1's fix).
        assert_eq!(get(0x8000_E018, 0).3, [0, 0, 0x01, 0x03]);
        // GET_INFORMATION_EX: up to 64 bytes.
        let ex = get(0x8000_E3FC, 0);
        assert_eq!((ex.2, ex.3.len()), (XUSB_MODE_UP_TO, 64));
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex_byte("0xA3"), Some(0xA3));
        assert_eq!(parse_hex("03 01 FF"), Some(vec![3, 1, 0xFF]));
        assert_eq!(parse_hex("0"), None);
    }
}
