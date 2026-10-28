//! Starting layouts for raw pads, from the community SDL_GameControllerDB.
//!
//! SDL only maps a gamepad its built-in list knows; a DirectInput pad outside
//! it arrives as a raw joystick (`raw_pad`) for the user to Learn. The
//! community database (`vendor/SDL_GameControllerDB`, refreshed from upstream
//! monthly by `.github/workflows/sdl-refresh.yml`) knows many more. Its
//! Windows entries were recorded through SDL's DirectInput driver, so their
//! `b3` / `a1` / `h0.4` name exactly the elements a raw pad publishes as
//! `raw:b3` / `raw:a1` / `raw:h0` — an entry converts into a
//! [`RawPadProfile`] with nothing lost. The app gives a raw pad nobody has
//! taught yet the layout its entry describes; Learn then changes any part of
//! it like any other.

use std::sync::OnceLock;

use crate::raw_pad::{RawBind, RawPadProfile, RawSrc, HAT_DPAD_PIN};

const DB: &str = include_str!("../../../vendor/SDL_GameControllerDB/gamecontrollerdb.txt");

struct Entry {
    vid: u16,
    pid: u16,
    name: &'static str,
    mapping: &'static str,
}

/// The database's Windows DirectInput entries. Parsed once.
fn entries() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| DB.lines().filter_map(parse_line).collect())
}

/// `guid,name,mapping…,platform:Windows,` → its vendor / product ids. Only
/// GUIDs in SDL's standard layout with no driver signature — what SDL's
/// DirectInput driver writes — count: an XInput, HIDAPI or RawInput entry
/// numbers its elements another way.
fn parse_line(line: &'static str) -> Option<Entry> {
    let line = line.trim();
    if line.starts_with('#') || !line.contains("platform:Windows") {
        return None;
    }
    let (guid, rest) = line.split_once(',')?;
    let (name, mapping) = rest.split_once(',')?;
    if guid.len() != 32 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(guid.get(2 * i..2 * i + 2)?, 16).ok();
    let word = |i: usize| Some(u16::from_le_bytes([byte(i)?, byte(i + 1)?]));
    // Bytes 6-7 and 10-11 are zero padding, 14 the driver signature.
    if word(6)? != 0 || word(10)? != 0 || byte(14)? != 0 {
        return None;
    }
    let (vid, pid) = (word(4)?, word(8)?);
    (vid != 0 && pid != 0).then_some(Entry { vid, pid, name, mapping })
}

/// One `b3` / `a1` / `+a2~` / `h0.4` value of a mapping.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Input {
    Button(u8),
    /// An axis read over `lo..hi` of its −1..1 range.
    Axis { index: u8, lo: f32, hi: f32 },
    Hat { index: u8, mask: u8 },
}

fn parse_input(v: &str) -> Option<Input> {
    let (half, v) = match v.as_bytes().first()? {
        b'+' => (Some(1.0), &v[1..]),
        b'-' => (Some(-1.0), &v[1..]),
        _ => (None, v),
    };
    let (v, invert) = match v.strip_suffix('~') {
        Some(v) => (v, true),
        None => (v, false),
    };
    let (kind, rest) = v.split_at_checked(1)?;
    match kind {
        "b" => rest.parse().ok().map(Input::Button),
        "a" => {
            let index = rest.parse().ok()?;
            let (lo, hi) = match half {
                Some(dir) => (0.0, dir),
                None => (-1.0, 1.0),
            };
            let (lo, hi) = if invert { (hi, lo) } else { (lo, hi) };
            Some(Input::Axis { index, lo, hi })
        }
        "h" => {
            let (h, m) = rest.split_once('.')?;
            Some(Input::Hat { index: h.parse().ok()?, mask: m.parse().ok()? })
        }
        _ => None,
    }
}

/// A gamepad button key → its canonical pin.
fn button_pin(key: &str) -> Option<&'static str> {
    Some(match key {
        "a" => "btn_south",
        "b" => "btn_east",
        "x" => "btn_west",
        "y" => "btn_north",
        "back" => "btn_back",
        "guide" => "btn_guide",
        "start" => "btn_start",
        "leftstick" => "btn_ls",
        "rightstick" => "btn_rs",
        "leftshoulder" => "btn_lb",
        "rightshoulder" => "btn_rb",
        "dpup" => "dpad_up",
        "dpdown" => "dpad_down",
        "dpleft" => "dpad_left",
        "dpright" => "dpad_right",
        "misc1" => "btn_misc1",
        "misc2" => "btn_misc2",
        "misc3" => "btn_misc3",
        "misc4" => "btn_misc4",
        "misc5" => "btn_misc5",
        "misc6" => "btn_misc6",
        // SDL's paddle1..4 are the right/left upper and lower paddles (P1, P3,
        // P2, P4 on an Elite pad).
        "paddle1" => "btn_paddle_r1",
        "paddle2" => "btn_paddle_l1",
        "paddle3" => "btn_paddle_r2",
        "paddle4" => "btn_paddle_l2",
        "touchpad" => "btn_touchpad",
        _ => return None,
    })
}

/// Convert one mapping string to a layout.
fn convert(name: &str, mapping: &str) -> RawPadProfile {
    let mut p = RawPadProfile { name: name.to_string(), from_db: true, ..Default::default() };
    let mut hat_dirs: Vec<(&str, u8, u8)> = Vec::new();
    for part in mapping.split(',') {
        let Some((key, value)) = part.split_once(':') else { continue };
        // A half-axis OUTPUT (`+leftx:b2`) has no counterpart in a layout.
        if key.starts_with('+') || key.starts_with('-') {
            continue;
        }
        let Some(input) = parse_input(value) else { continue };
        let bind = |src: RawSrc, pin: &str, rest: f32, full: f32| RawBind {
            src,
            pin: pin.to_string(),
            rest,
            full,
            label: String::new(),
        };
        match key {
            "leftx" | "lefty" | "rightx" | "righty" => {
                let Input::Axis { index, lo, hi } = input else { continue };
                // SDL's gamepad Y grows downward; the canonical one upward.
                let flip = if key.ends_with('y') { -1.0 } else { 1.0 };
                let pin = match key {
                    "leftx" => "left_stick_x",
                    "lefty" => "left_stick_y",
                    "rightx" => "right_stick_x",
                    _ => "right_stick_y",
                };
                let mid = (lo + hi) / 2.0;
                p.set(bind(RawSrc::Axis(index), pin, mid, mid + flip * (hi - lo) / 2.0));
            }
            "lefttrigger" | "righttrigger" => {
                let pin = if key == "lefttrigger" { "left_trigger" } else { "right_trigger" };
                match input {
                    Input::Axis { index, lo, hi } => p.set(bind(RawSrc::Axis(index), pin, lo, hi)),
                    Input::Button(i) => p.set(bind(RawSrc::Button(i), pin, 0.0, 1.0)),
                    Input::Hat { .. } => {}
                }
            }
            _ => {
                let Some(pin) = button_pin(key) else { continue };
                match input {
                    Input::Button(i) => p.set(bind(RawSrc::Button(i), pin, 0.0, 1.0)),
                    // Past halfway over the mapped range, like Learn's
                    // "Button (past halfway)".
                    Input::Axis { index, lo, hi } => p.set(bind(RawSrc::Axis(index), pin, lo, hi)),
                    Input::Hat { index, mask } => hat_dirs.push((key, index, mask)),
                }
            }
        }
    }
    // The D-pad on a hat, the usual way round, is the whole D-pad.
    let standard = [("dpup", 1u8), ("dpright", 2), ("dpdown", 4), ("dpleft", 8)];
    if let Some(&(_, hat, _)) = hat_dirs.first() {
        let whole = standard.iter().all(|(k, m)| hat_dirs.iter().any(|(hk, hi, hm)| hk == k && *hi == hat && hm == m));
        if whole {
            p.set(RawBind { src: RawSrc::Hat(hat), pin: HAT_DPAD_PIN.into(), rest: 0.0, full: 1.0, label: String::new() });
        }
    }
    p
}

/// The community database's layout for a pad, if it has one: matched on
/// vendor and product id, the entry named like the pad preferred where
/// several share them.
pub fn layout_for(vid: Option<u16>, pid: Option<u16>, name: &str) -> Option<RawPadProfile> {
    let (vid, pid) = (vid?, pid?);
    let matches: Vec<&Entry> = entries().iter().filter(|e| e.vid == vid && e.pid == pid).collect();
    let e = matches.iter().find(|e| e.name.eq_ignore_ascii_case(name.trim())).or(matches.first())?;
    Some(convert(e.name, e.mapping))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw_pad::RawReading;
    use crate::Signal;
    use glam::Vec2;
    use std::collections::HashMap;

    const LINE: &str = "03000000790000000600000000000000,G-Shark GS-GP702,a:b2,b:b1,back:b8,\
        dpdown:h0.4,dpleft:h0.8,dpright:h0.2,dpup:h0.1,leftshoulder:b4,leftstick:b10,\
        lefttrigger:b6,leftx:a0,lefty:a1,rightshoulder:b5,rightstick:b11,righttrigger:a5,\
        rightx:a2,righty:a4~,start:b9,x:b3,y:b0,+leftx:b13,platform:Windows,";

    fn run(p: &RawPadProfile, axes: &[f32], buttons: &[bool], hats: &[Vec2]) -> HashMap<String, Signal> {
        let mut out = HashMap::new();
        p.compile().apply(&RawReading { axes, buttons, hats, reports: &() },
            |id, s| { out.insert(id.to_string(), s); });
        out
    }

    #[test]
    fn a_standard_entry_parses_and_others_are_skipped() {
        let e = parse_line(LINE).unwrap();
        assert_eq!((e.vid, e.pid, e.name), (0x0079, 0x0006, "G-Shark GS-GP702"));
        assert!(parse_line("# Windows").is_none());
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        assert!(parse_line(leak(LINE.replace("platform:Windows", "platform:Linux"))).is_none());
        // An XInput GUID ("xinput" in ASCII) isn't a DirectInput entry.
        assert!(parse_line("78696e70757401000000000000000000,XInput Controller,a:b0,platform:Windows,").is_none());
        // A driver signature (HIDAPI 'h') numbers elements another way.
        assert!(parse_line(leak(LINE.replacen("00000000000000,", "00000000006800,", 1))).is_none());
    }

    #[test]
    fn an_entry_converts_into_a_working_layout() {
        let p = convert("G-Shark GS-GP702", LINE.split_once(',').unwrap().1.split_once(',').unwrap().1);
        assert!(p.from_db);
        let mut buttons = [false; 14];
        buttons[2] = true; // a
        buttons[6] = true; // lefttrigger (a button on this pad)
        let mut axes = [0.0f32; 6];
        axes[1] = -1.0; // lefty: pushed up (SDL Y down)
        axes[4] = -1.0; // righty~: inverted, so −1 is DOWN
        axes[5] = 0.0; // righttrigger: full axis, halfway
        let out = run(&p, &axes, &buttons, &[Vec2::new(0.0, 1.0)]);
        assert_eq!(out.get("btn_south"), Some(&Signal::Bool(true)));
        assert_eq!(out.get("btn_east"), Some(&Signal::Bool(false)));
        assert_eq!(out.get("left_trigger"), Some(&Signal::Float(1.0)));
        assert_eq!(out.get("left_stick_y"), Some(&Signal::Float(1.0)), "up reads up");
        assert_eq!(out.get("right_stick_y"), Some(&Signal::Float(-1.0)), "an inverted axis stays right");
        assert_eq!(out.get("right_trigger"), Some(&Signal::Float(0.5)));
        assert_eq!(out.get("dpad_up"), Some(&Signal::Bool(true)), "the hat is the whole D-pad");
        assert!(!p.binds.iter().any(|b| b.src == RawSrc::Button(13)), "the half-axis output is skipped");
    }

    #[test]
    fn the_bundled_database_loads_and_matches_by_ids() {
        assert!(entries().len() > 500, "{} entries", entries().len());
        let e = &entries()[0];
        let p = layout_for(Some(e.vid), Some(e.pid), "no such name").unwrap();
        assert!(!p.binds.is_empty());
        assert_eq!(layout_for(Some(0xFFFF), Some(0xFFFE), ""), None);
        assert_eq!(layout_for(None, Some(1), ""), None);
    }

    #[test]
    fn every_bundled_entry_converts_to_something_that_compiles() {
        for e in entries() {
            let p = convert(e.name, e.mapping);
            assert!(!p.compile().is_empty(), "{} ({:04x}:{:04x}) converts to nothing", e.name, e.vid, e.pid);
        }
    }
}
