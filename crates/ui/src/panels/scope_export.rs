//! The Calibration window's gyro scope, saved as a picture.
//!
//! The selected time window at full detail: time runs down the image, newest
//! at the top like the scope, at about two pixels per sample (so a
//! one-sample spike keeps rows of its own) and as tall as that needs. Value
//! guides are sub-divided and labelled in °/s (and G when the accelerometer
//! is shown), the labels repeated down the image so a row is readable
//! wherever you are in it; time guides are labelled down the left.
//!
//! The scale is set by the bulk of the samples, not their peak — a single
//! spike would otherwise flatten the noise and any DC offset into a sliver.
//! Samples past it sit on the edge, labelled with their value.
//!
//! Built as an SVG and rasterized with resvg — the same renderer the app's
//! icons use — with the system fonts loaded for the labels.

use egui::Color32;
use flexinput_devices::gyro::{ACCEL_REF_G, GYRO_REF_DPS};

/// One trace.
pub(crate) struct Channel {
    pub name: &'static str,
    pub color: Color32,
    /// An accelerometer axis (labelled in G rather than °/s).
    pub accel: bool,
    /// `(ms before the newest sample, normalized value)`, newest first.
    pub samples: Vec<(f64, f32)>,
}

pub(crate) struct Export {
    /// The device, for the title.
    pub device: String,
    /// When it was taken, as shown in the picture.
    pub taken: String,
    pub window_ms: f64,
    pub channels: Vec<Channel>,
}

/// Image width, and the margins around the plot.
const W: f64 = 1400.0;
const LEFT: f64 = 104.0;
const RIGHT: f64 = 28.0;
const TOP: f64 = 150.0;
const BOTTOM: f64 = 70.0;
/// Pixels per sample down the time axis (on average — rows follow the
/// samples' own times).
const PX_PER_SAMPLE: f64 = 2.0;
/// Tallest plot drawn; beyond it the rows get closer than two pixels.
const MAX_PLOT_H: f64 = 16000.0;
const MIN_PLOT_H: f64 = 500.0;
/// The share of samples the scale is fitted to; the rest may sit on the edge.
const SCALE_QUANTILE: f64 = 0.995;
/// Narrowest full width, normalized: ±0.1 °/s.
const MIN_RANGE: f64 = 0.1 / 2000.0;
/// At most this many off-scale samples get a value label.
const MAX_CLIP_LABELS: usize = 300;
/// The value labels are repeated this far apart down the plot.
const LABEL_EVERY_PX: f64 = 1000.0;
const FONT: &str = "Segoe UI, Arial, sans-serif";

/// 1, 2 or 5 × a power of ten, the smallest at least `x`.
pub(crate) fn nice_step(x: f64) -> f64 {
    if !(x > 0.0) || !x.is_finite() {
        return 1.0;
    }
    let p = 10f64.powf(x.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if m * p >= x * 0.999_999 {
            return m * p;
        }
    }
    10.0 * p
}

/// Decimals that show every value on a `step` grid exactly.
pub(crate) fn decimals(step: f64) -> usize {
    (0..8)
        .find(|d| {
            let x = step * 10f64.powi(*d as i32);
            (x - x.round()).abs() < 1e-6 * x.max(1.0)
        })
        .unwrap_or(8)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// The picture as SVG, with its pixel size.
pub(crate) fn build_svg(e: &Export) -> (String, u32, u32) {
    let n = e.channels.iter().map(|c| c.samples.len()).max().unwrap_or(0).max(1);
    let window = e.window_ms.max(1.0);
    let plot_h = (n as f64 * PX_PER_SAMPLE).clamp(MIN_PLOT_H, MAX_PLOT_H);
    let px_per_ms = plot_h / window;
    let h = TOP + plot_h + BOTTOM;
    let plot_w = W - LEFT - RIGHT;
    let cx = LEFT + plot_w * 0.5;
    let half = plot_w * 0.5;

    // Symmetric value range fitted to the bulk of every trace, with room to
    // spare; spikes beyond it are clipped and labelled.
    let mut mags: Vec<f64> = e.channels.iter()
        .flat_map(|c| c.samples.iter().map(|(_, v)| v.abs() as f64))
        .collect();
    mags.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let bulk = mags.get(((mags.len().max(1) - 1) as f64 * SCALE_QUANTILE) as usize).copied().unwrap_or(0.0);
    let peak = mags.last().copied().unwrap_or(0.0);
    let range = (bulk * 1.6).max(MIN_RANGE).min(peak * 1.1).max(MIN_RANGE);
    let x_of = |v: f64| cx + (v / range).clamp(-1.0, 1.0) * half;
    let y_of = |ms: f64| TOP + ms * px_per_ms;

    let mut svg = String::with_capacity(64 * 1024 + n * 40 * e.channels.len());
    use std::fmt::Write as _;
    let _ = write!(svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{h}" viewBox="0 0 {W} {h}" font-family="{FONT}">"#);
    let _ = write!(svg, r##"<rect width="{W}" height="{h}" fill="#0f0f0f"/>"##);
    let _ = write!(svg, r##"<rect x="{LEFT}" y="{TOP}" width="{plot_w}" height="{plot_h}" fill="#151515" stroke="#3a3a3a"/>"##);

    // ── Header ──
    let rate = n as f64 / (window / 1000.0);
    let _ = write!(svg, r##"<text x="{LEFT}" y="34" font-size="22" fill="#e6e6e6">{} — gyro scope</text>"##, esc(&e.device));
    let _ = write!(svg,
        r##"<text x="{LEFT}" y="58" font-size="14" fill="#9a9a9a">{} ms · {} samples (~{:.0} Hz) · full width ±{} °/s · {}</text>"##,
        window as i64, n, rate, fmt_num(range * GYRO_REF_DPS as f64, 3), esc(&e.taken));
    if peak > range {
        let _ = write!(svg,
            r##"<text x="{}" y="58" font-size="14" fill="#ff7070" text-anchor="end">peak ±{} °/s — off-scale samples are pinned to the edge and labelled</text>"##,
            W - RIGHT, fmt_num(peak * GYRO_REF_DPS as f64, 3));
    }
    let mut lx = LEFT;
    for c in &e.channels {
        let col = hex(c.color);
        let _ = write!(svg, r#"<line x1="{lx}" y1="80" x2="{}" y2="80" stroke="{col}" stroke-width="3"/>"#, lx + 18.0);
        let _ = write!(svg, r#"<text x="{}" y="85" font-size="14" fill="{col}">{}</text>"#, lx + 24.0, esc(c.name));
        lx += 24.0 + 9.0 * c.name.len() as f64 + 22.0;
    }
    let _ = write!(svg, r##"<text x="{}" y="85" font-size="12" fill="#6f6f6f" text-anchor="end">newest at the top · time runs down</text>"##, W - RIGHT);

    // ── Value guides (vertical lines), in °/s ──
    let span_dps = range * GYRO_REF_DPS as f64;
    let major = nice_step(span_dps / 5.0);
    let minor = major / 5.0;
    let dec = decimals(major);
    let has_accel = e.channels.iter().any(|c| c.accel);
    let k_max = (span_dps / minor).floor() as i64;
    for k in -k_max..=k_max {
        let dps = k as f64 * minor;
        let x = x_of(dps / GYRO_REF_DPS as f64);
        let (stroke, w) = if k == 0 {
            ("#707070", 1.4)
        } else if k % 5 == 0 {
            ("#383838", 1.0)
        } else {
            ("#222222", 1.0)
        };
        let _ = write!(svg, r#"<line x1="{x:.2}" y1="{TOP}" x2="{x:.2}" y2="{}" stroke="{stroke}" stroke-width="{w}"/>"#, TOP + plot_h);
    }
    // Their labels: above the plot, below it, and repeated down it.
    let value_labels = |svg: &mut String, y: f64, halo: bool| {
        let paint = if halo { r##" stroke="#0f0f0f" stroke-width="4" paint-order="stroke""## } else { "" };
        let kmaj = (span_dps / major).floor() as i64;
        for k in -kmaj..=kmaj {
            let dps = k as f64 * major;
            let x = x_of(dps / GYRO_REF_DPS as f64);
            let txt = if k == 0 { "0".to_string() } else { format!("{:+.*}", dec, dps) };
            let _ = write!(svg, r##"<text x="{x:.2}" y="{y}" font-size="12" fill="#bdbdbd" text-anchor="middle"{paint}>{txt}</text>"##);
            if has_accel {
                let g = dps / GYRO_REF_DPS as f64 * ACCEL_REF_G as f64;
                let gt = if k == 0 { "0 G".to_string() } else { format!("{:+.*} G", dec + 2, g) };
                let _ = write!(svg, r##"<text x="{x:.2}" y="{}" font-size="10" fill="#8a8a5a" text-anchor="middle"{paint}>{gt}</text>"##, y + 13.0);
            }
        }
        let _ = write!(svg, r##"<text x="{}" y="{y}" font-size="11" fill="#7a7a7a" text-anchor="end"{paint}>°/s</text>"##, LEFT - 8.0);
    };
    value_labels(&mut svg, TOP - (if has_accel { 26.0 } else { 12.0 }), false);
    let mut y = TOP + LABEL_EVERY_PX;
    while y < TOP + plot_h - LABEL_EVERY_PX * 0.5 {
        value_labels(&mut svg, y, true);
        y += LABEL_EVERY_PX;
    }
    value_labels(&mut svg, TOP + plot_h + 18.0, false);

    // ── Time guides (horizontal lines), labelled down the left ──
    let t_major = nice_step(160.0 / px_per_ms);
    let t_minor = if t_major / 5.0 * px_per_ms >= 8.0 { t_major / 5.0 } else { t_major / 2.0 };
    let t_dec = decimals(t_minor);
    let minor_labels = t_minor * px_per_ms >= 22.0;
    let mut i = 0i64;
    loop {
        let ms = i as f64 * t_minor;
        if ms > window + 1e-9 {
            break;
        }
        let y = y_of(ms);
        let is_major = (ms / t_major - (ms / t_major).round()).abs() < 1e-6;
        let stroke = if i == 0 { "#707070" } else if is_major { "#383838" } else { "#202020" };
        let _ = write!(svg, r#"<line x1="{LEFT}" y1="{y:.2}" x2="{}" y2="{y:.2}" stroke="{stroke}"/>"#, LEFT + plot_w);
        if is_major || minor_labels {
            let (size, fill) = if is_major { (12, "#bdbdbd") } else { (10, "#6f6f6f") };
            let txt = if i == 0 { "0 ms".to_string() } else { format!("−{:.*} ms", t_dec, ms) };
            let _ = write!(svg, r#"<text x="{}" y="{:.2}" font-size="{size}" fill="{fill}" text-anchor="end">{txt}</text>"#, LEFT - 8.0, y + 4.0);
        }
        i += 1;
    }

    // ── Traces: a line through every sample, and a dot on each ──
    let dots = plot_h / n as f64 >= 1.5;
    for c in &e.channels {
        let col = hex(c.color);
        let mut pts = String::with_capacity(c.samples.len() * 18);
        for (ms, v) in &c.samples {
            let _ = write!(pts, "{:.2},{:.2} ", x_of(*v as f64), y_of(*ms));
        }
        let _ = write!(svg, r#"<polyline points="{pts}" fill="none" stroke="{col}" stroke-width="1.3" stroke-linejoin="round"/>"#);
        // Off-scale samples: a marker on the edge and the value beside it.
        for (ms, v) in c.samples.iter().filter(|(_, v)| (*v as f64).abs() > range).take(MAX_CLIP_LABELS) {
            let right = *v > 0.0;
            let x = if right { LEFT + plot_w } else { LEFT };
            let y = y_of(*ms);
            let (tip, back) = if right { (x, x - 9.0) } else { (x, x + 9.0) };
            let _ = write!(svg, r#"<polygon points="{tip:.2},{y:.2} {back:.2},{:.2} {back:.2},{:.2}" fill="{col}"/>"#, y - 5.0, y + 5.0);
            let val = *v as f64 * if c.accel { ACCEL_REF_G as f64 } else { GYRO_REF_DPS as f64 };
            let unit = if c.accel { "G" } else { "°/s" };
            let (tx, anchor) = if right { (back - 4.0, "end") } else { (back + 4.0, "start") };
            let _ = write!(svg,
                r##"<text x="{tx:.2}" y="{:.2}" font-size="11" fill="{col}" text-anchor="{anchor}" stroke="#151515" stroke-width="3" paint-order="stroke">{}{} {unit}</text>"##,
                y + 4.0, if val > 0.0 { "+" } else { "" }, fmt_num(val, 3));
        }
        if dots {
            let _ = write!(svg, r#"<g fill="{col}">"#);
            for (ms, v) in &c.samples {
                let _ = write!(svg, r#"<circle cx="{:.2}" cy="{:.2}" r="1.4"/>"#, x_of(*v as f64), y_of(*ms));
            }
            svg.push_str("</g>");
        }
    }
    svg.push_str("</svg>");
    (svg, W as u32, h.ceil() as u32)
}

/// A value with up to `sig` significant figures, trailing zeros trimmed.
fn fmt_num(v: f64, sig: i32) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let dec = (sig - 1 - v.abs().log10().floor() as i32).max(0) as usize;
    let s = format!("{v:.dec$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// Rasterize `svg` (from [`build_svg`]) to PNG bytes.
pub(crate) fn render_png(svg: &str, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    opt.font_family = "Segoe UI".into();
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| e.to_string())?;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or_else(|| format!("{w}×{h} is too large"))?;
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| e.to_string())
}

/// Local time, for the picture and its file name: `(shown, file-safe)`.
pub(crate) fn local_stamp() -> (String, String) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut t = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut t) };
        let windows_sys::Win32::Foundation::SYSTEMTIME { wYear: y, wMonth: mo, wDay: d, wHour: hh, wMinute: mm, wSecond: ss, .. } = t;
        (
            format!("{y}-{mo:02}-{d:02} {hh:02}:{mm:02}:{ss:02}"),
            format!("{y}-{mo:02}-{d:02} {hh:02}-{mm:02}-{ss:02}"),
        )
    }
    #[cfg(not(windows))]
    {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        (s.to_string(), s.to_string())
    }
}

/// A file name without the characters Windows refuses.
pub(crate) fn file_safe(s: &str) -> String {
    s.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(n: usize, window_ms: f64, accel: bool) -> Export {
        let mk = |name, color, accel, amp: f32| Channel {
            name,
            color,
            accel,
            samples: (0..n).map(|i| (i as f64 * window_ms / n as f64, amp * ((i as f32) * 0.3).sin())).collect(),
        };
        let mut channels = vec![
            mk("Roll", Color32::RED, false, 0.0001),
            mk("Pitch", Color32::GREEN, false, 0.0002),
            mk("Yaw", Color32::LIGHT_BLUE, false, 0.00005),
        ];
        if accel {
            channels.push(mk("Accel X", Color32::YELLOW, true, 0.001));
        }
        Export { device: "Pad <1> & co".into(), taken: "2026-10-24 12:00:00".into(), window_ms, channels }
    }

    #[test]
    fn two_pixels_a_sample_down_the_time_axis() {
        let (_, w, h) = build_svg(&export(1000, 1000.0, false));
        assert_eq!(w, W as u32);
        assert_eq!(h as f64, (TOP + 2000.0 + BOTTOM).ceil());
        // A very long recording stops at the tallest plot.
        let (_, _, h) = build_svg(&export(20000, 5000.0, false));
        assert_eq!(h as f64, (TOP + MAX_PLOT_H + BOTTOM).ceil());
    }

    #[test]
    fn it_parses_and_renders() {
        let (svg, w, h) = build_svg(&export(300, 500.0, true));
        assert!(svg.contains("Pad &lt;1&gt; &amp; co"), "the title is escaped");
        let png = render_png(&svg, w, h).expect("renders");
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn guide_steps_are_one_two_five() {
        assert_eq!(nice_step(0.03), 0.05);
        assert_eq!(nice_step(0.12), 0.2);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(decimals(0.05), 2);
        assert_eq!(decimals(0.2), 1);
        assert_eq!(decimals(5.0), 0);
        assert_eq!(decimals(25.0), 0);
        assert_eq!(decimals(2.5), 1);
        assert_eq!(decimals(0.04), 2);
    }

    #[test]
    fn a_spike_does_not_set_the_scale() {
        let mut e = export(1000, 1000.0, false);
        e.channels[1].samples[400].1 = 0.004; // 8 °/s among ±0.4 °/s
        let (svg, _, _) = build_svg(&e);
        assert!(svg.contains("off-scale samples are pinned"), "the header says so");
        assert!(svg.contains("+8 °/s</text>"), "and the spike is labelled with its value");
        assert!(svg.contains("full width ±0.6"), "the scale fits the bulk: {}", &svg[..600]);
    }

    #[test]
    fn file_names_lose_what_windows_refuses() {
        assert_eq!(file_safe("Pro: L/R?"), "Pro_ L_R_");
    }
}
