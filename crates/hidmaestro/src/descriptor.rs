//! HID report-descriptor parser + report bit-packer.
//!
//! Faithful Rust port of the data-driven core of HIDMaestro's
//! `Internal/HidReportBuilder.cs` (v1.3.17): [`parse_descriptor`] walks a raw
//! HID report descriptor into a flat list of [`InputField`]s (the first input
//! report only), and [`build_report_into`]/[`write_bits`] pack values into a
//! report buffer with correct little-endian bit placement.
//!
//! This is the portable, controller-agnostic half. The fiddly *semantic-axis
//! resolution* (`ApplyLayoutSemantics` / `ResolveSemantics` / `ApplyAxisMap`)
//! is intentionally NOT ported yet — for the preset profiles FlexInput ships,
//! fields are addressed directly by HID usage (see `profile.rs`), which is
//! exact and avoids the heuristic layer.

/// One scalar input field decoded from the descriptor: a usage at a bit
/// position with a logical range. Buttons expand to one field per button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputField {
    pub usage_page: u16,
    pub usage: u16,
    pub bit_offset: i32,
    pub bit_size: i32,
    pub logical_min: i32,
    pub logical_max: i32,
    pub is_constant: bool,
    pub report_count: i32,
}

/// Parsed input-report layout: the report ID (0 if none), the fields of the
/// first input report, and the report's total bit/byte size.
#[derive(Debug, Clone, Default)]
pub struct InputReport {
    pub report_id: u8,
    pub fields: Vec<InputField>,
    pub bit_size: i32,
}

impl InputReport {
    /// Byte length of the on-wire report, including the leading Report-ID byte
    /// when `report_id != 0` (matches `HidReportBuilder.InputReportByteSize`).
    pub fn byte_size(&self) -> usize {
        ((self.bit_size + 7) / 8) as usize + if self.report_id != 0 { 1 } else { 0 }
    }

    /// First field matching `(usage_page, usage)`, or `None`. Used by the
    /// preset encoders to address sticks/triggers/hat by usage directly.
    pub fn field(&self, usage_page: u16, usage: u16) -> Option<&InputField> {
        self.fields
            .iter()
            .find(|f| f.usage_page == usage_page && f.usage == usage && !f.is_constant)
    }
}

/// Parse a raw HID report descriptor, returning the **first** input report's
/// layout. Port of `HidReportBuilder.ParseDescriptor`.
pub fn parse_descriptor(desc: &[u8]) -> InputReport {
    let mut out = InputReport::default();

    // Parser state (mirrors the C# locals).
    let mut usage_page: u16 = 0;
    let mut usages: Vec<u16> = Vec::new();
    let mut usage_min: u16 = 0;
    let mut usage_max: u16 = 0;
    let mut report_size: i32 = 0;
    let mut report_count: i32 = 0;
    let mut logical_min: i32 = 0;
    let mut logical_max: i32 = 0;
    let mut report_id: u8 = 0;
    let mut bit_offset: i32 = 0;
    let mut first_input_report_id = true;

    let mut i = 0usize;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            i += 3; // long item — skip
            continue;
        }

        let mut b_size = (prefix & 0x03) as usize;
        if b_size == 3 {
            b_size = 4;
        }
        let b_type = (prefix >> 2) & 0x03;
        let b_tag = (prefix >> 4) & 0x0F;

        // Little-endian item data.
        let mut value: i32 = 0;
        if i + b_size < desc.len() {
            for j in 0..b_size {
                value |= (desc[i + 1 + j] as i32) << (8 * j);
            }
        }
        // Sign-extend for signed items (Logical Min, etc.).
        let mut signed_value = value;
        if b_size > 0 && b_size < 4 && (value & (1 << (b_size * 8 - 1))) != 0 {
            signed_value |= ((0xFFFF_FFFFu32) << (b_size * 8)) as i32;
        }

        match b_type {
            0 => {
                // Main
                match b_tag {
                    8 => {
                        // Input
                        let is_constant = (value & 0x01) != 0;
                        if report_id != 0 && first_input_report_id {
                            out.report_id = report_id;
                            first_input_report_id = false;
                        }
                        // Only the first input report ID is materialized.
                        if report_id == out.report_id || (report_id == 0 && out.report_id == 0) {
                            if usage_min != 0 && usage_max != 0 {
                                // Button range.
                                for b in 0..report_count {
                                    let mut u = usage_min.wrapping_add(b as u16);
                                    if u > usage_max {
                                        u = usage_max;
                                    }
                                    out.fields.push(InputField {
                                        usage_page,
                                        usage: u,
                                        bit_offset: bit_offset + b * report_size,
                                        bit_size: report_size,
                                        logical_min,
                                        logical_max,
                                        is_constant,
                                        report_count,
                                    });
                                }
                            } else {
                                for c in 0..report_count {
                                    let u = usages.get(c as usize).copied().unwrap_or(0);
                                    out.fields.push(InputField {
                                        usage_page,
                                        usage: u,
                                        bit_offset: bit_offset + c * report_size,
                                        bit_size: report_size,
                                        logical_min,
                                        logical_max,
                                        is_constant,
                                        report_count,
                                    });
                                }
                            }
                            bit_offset += report_size * report_count;
                        }
                        usages.clear();
                        usage_min = 0;
                        usage_max = 0;
                    }
                    9 | 11 => {
                        // Output / Feature — different report, skip.
                        usages.clear();
                        usage_min = 0;
                        usage_max = 0;
                    }
                    10 => {
                        // Collection — the preceding usage belongs to it, not input.
                        usages.clear();
                        usage_min = 0;
                        usage_max = 0;
                    }
                    12 => { /* End Collection */ }
                    _ => {}
                }
            }
            1 => {
                // Global
                match b_tag {
                    0 => usage_page = value as u16,
                    1 => logical_min = signed_value,
                    2 => {
                        // Logical Max — treated unsigned when min >= 0.
                        logical_max = if logical_min >= 0 && signed_value < 0 {
                            value
                        } else {
                            signed_value
                        };
                    }
                    7 => report_size = value,
                    8 => {
                        report_id = value as u8;
                        if first_input_report_id {
                            // First Report ID encountered — reset bit cursor.
                            bit_offset = 0;
                        }
                    }
                    9 => report_count = value,
                    _ => {}
                }
            }
            2 => {
                // Local
                match b_tag {
                    0 => {
                        // Usage
                        if b_size == 4 {
                            // Extended usage: high 16 = page, low 16 = usage.
                            usage_page = (value >> 16) as u16;
                            usages.push((value & 0xFFFF) as u16);
                        } else {
                            usages.push(value as u16);
                        }
                    }
                    1 => usage_min = value as u16,
                    2 => usage_max = value as u16,
                    _ => {}
                }
            }
            _ => {}
        }

        i += 1 + b_size;
    }

    out.bit_size = bit_offset;
    out
}

/// Write `value` as a little-endian bitfield of `bit_size` bits at `bit_offset`
/// into `buffer`. Port of `HidReportBuilder.WriteBits` (byte-aligned fast path
/// + bit-by-bit fallback).
pub fn write_bits(buffer: &mut [u8], bit_offset: i32, bit_size: i32, value: i32) {
    if bit_offset < 0 || bit_size <= 0 {
        return;
    }
    // Fast path: byte-aligned offset and byte-multiple size.
    if (bit_offset & 7) == 0 && (bit_size & 7) == 0 {
        let byte_idx = (bit_offset >> 3) as usize;
        let byte_cnt = (bit_size >> 3) as usize;
        let mut v = value as u32;
        for k in 0..byte_cnt {
            let idx = byte_idx + k;
            if idx >= buffer.len() {
                break;
            }
            buffer[idx] = (v & 0xFF) as u8;
            v >>= 8;
        }
        return;
    }
    // Fallback: arbitrary alignment / non-byte-multiple (button bitmaps, etc.).
    for b in 0..bit_size {
        let bit = (value >> b) & 1;
        let pos = bit_offset + b;
        let byte_idx = (pos >> 3) as usize;
        let bit_idx = (pos & 7) as u32;
        if byte_idx < buffer.len() {
            if bit != 0 {
                buffer[byte_idx] |= 1u8 << bit_idx;
            } else {
                buffer[byte_idx] &= !(1u8 << bit_idx);
            }
        }
    }
}

/// Write a normalized `[0.0, 1.0]` value into `field`, scaled into its logical
/// range and clamped. Port of the `WriteField` local in `BuildReportInto`.
pub fn write_field(report: &mut [u8], id_offset: i32, field: &InputField, normalized: f64) {
    let span = (field.logical_max - field.logical_min) as f64;
    let mut raw = (normalized * span + field.logical_min as f64) as i32;
    raw = raw.clamp(field.logical_min, field.logical_max);
    write_bits(report, field.bit_offset + id_offset, field.bit_size, raw);
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal 2-axis (X,Y 8-bit) + 8-button descriptor, hand-built so we can
    // assert parser output without external fixtures. Generic desktop gamepad.
    const MINI_DESC: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x05, // Usage (Game Pad)
        0xA1, 0x01, // Collection (Application)
        0x09, 0x01, //   Usage (Pointer)
        0xA1, 0x00, //   Collection (Physical)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x15, 0x00, //     Logical Min (0)
        0x26, 0xFF, 0x00, //  Logical Max (255)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x02, //     Report Count (2)
        0x81, 0x02, //     Input (Data,Var,Abs)
        0xC0, //         End Collection
        0x05, 0x09, //   Usage Page (Button)
        0x19, 0x01, //   Usage Min (1)
        0x29, 0x08, //   Usage Max (8)
        0x15, 0x00, //   Logical Min (0)
        0x25, 0x01, //   Logical Max (1)
        0x75, 0x01, //   Report Size (1)
        0x95, 0x08, //   Report Count (8)
        0x81, 0x02, //   Input (Data,Var,Abs)
        0xC0, //         End Collection
    ];

    #[test]
    fn parses_axes_and_buttons() {
        let r = parse_descriptor(MINI_DESC);
        assert_eq!(r.report_id, 0);
        // 2 axis fields + 8 button fields.
        assert_eq!(r.fields.len(), 10);
        assert_eq!(r.bit_size, 8 * 2 + 1 * 8); // 24 bits
        assert_eq!(r.byte_size(), 3);

        let x = r.field(0x01, 0x30).expect("X axis");
        assert_eq!((x.bit_offset, x.bit_size, x.logical_max), (0, 8, 255));
        let y = r.field(0x01, 0x31).expect("Y axis");
        assert_eq!(y.bit_offset, 8);
        // Button 1 at bit 16.
        let b1 = r.field(0x09, 0x01).expect("button 1");
        assert_eq!((b1.bit_offset, b1.bit_size), (16, 1));
    }

    #[test]
    fn packs_axis_and_button_bits() {
        let r = parse_descriptor(MINI_DESC);
        let mut report = vec![0u8; r.byte_size()];

        // X = full (1.0) → 255 at byte 0; Y = center (0.5) → 127 at byte 1.
        write_field(&mut report, 0, r.field(0x01, 0x30).unwrap(), 1.0);
        write_field(&mut report, 0, r.field(0x01, 0x31).unwrap(), 0.5);
        // Button 3 (bit 16+2 = 18) pressed.
        let b3 = r.field(0x09, 0x03).unwrap();
        write_bits(&mut report, b3.bit_offset, b3.bit_size, 1);

        assert_eq!(report[0], 255);
        assert_eq!(report[1], 127);
        assert_eq!(report[2], 0b0000_0100); // button 3 → bit 2 of byte 2
    }
}
