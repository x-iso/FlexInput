//! Media and volume keys as outputs.
//!
//! They aren't keyboard keys to the OS: a keyboard sends them on the HID
//! **Consumer** page (0x0C), not the Keyboard page the rest of the keys are on,
//! and Windows names them with their own virtual-key codes. So they get their
//! own `media_` pins rather than `key_` ones — which also keeps them out of
//! everything that treats `key_` pins as typing (the picker's typing board,
//! the JSM editor's "this would type into me" guard).
//!
//! One table, read by the virtual keyboard backends (HID usage, VK), JSM (its
//! names), the picker (labels) and Learn (the VK a physical keyboard reports).

pub struct MediaKey {
    /// The bus pin.
    pub pin: &'static str,
    /// Full name, for chips and tooltips.
    pub name: &'static str,
    /// A few characters, for a picker cell.
    pub short: &'static str,
    /// JoyShockMapper's name for it.
    pub jsm: &'static str,
    /// HID Consumer-page usage.
    pub usage: u16,
    /// Windows virtual-key code.
    pub vk: u16,
}

const fn key(
    pin: &'static str,
    name: &'static str,
    short: &'static str,
    jsm: &'static str,
    usage: u16,
    vk: u16,
) -> MediaKey {
    MediaKey { pin, name, short, jsm, usage, vk }
}

/// In the order they sit on a keyboard's media row.
pub static MEDIA_KEYS: &[MediaKey] = &[
    key("media_prev_track", "Previous Track", "Prev", "PREV_TRACK", 0xB6, 0xB1),
    key("media_play_pause", "Play / Pause", "Play", "PLAY_PAUSE", 0xCD, 0xB3),
    key("media_next_track", "Next Track", "Next", "NEXT_TRACK", 0xB5, 0xB0),
    key("media_stop", "Stop", "Stop", "STOP_TRACK", 0xB7, 0xB2),
    key("media_mute", "Mute", "Mute", "MUTE", 0xE2, 0xAD),
    key("media_volume_down", "Volume Down", "Vol-", "VOLUME_DOWN", 0xEA, 0xAE),
    key("media_volume_up", "Volume Up", "Vol+", "VOLUME_UP", 0xE9, 0xAF),
];

pub fn media_key(pin: &str) -> Option<&'static MediaKey> {
    if !pin.starts_with("media_") {
        return None;
    }
    MEDIA_KEYS.iter().find(|k| k.pin == pin)
}

pub fn is_media_pin(pin: &str) -> bool {
    media_key(pin).is_some()
}

/// The media key a Windows virtual-key code names, if any.
pub fn media_key_by_vk(vk: u16) -> Option<&'static MediaKey> {
    MEDIA_KEYS.iter().find(|k| k.vk == vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_media_key_is_distinct_and_found() {
        for (i, k) in MEDIA_KEYS.iter().enumerate() {
            assert!(k.pin.starts_with("media_"), "{}", k.pin);
            for o in &MEDIA_KEYS[i + 1..] {
                assert!(k.pin != o.pin && k.usage != o.usage && k.vk != o.vk && k.jsm != o.jsm,
                    "{} and {} collide", k.pin, o.pin);
            }
            assert!(std::ptr::eq(media_key(k.pin).unwrap(), k));
            assert!(std::ptr::eq(media_key_by_vk(k.vk).unwrap(), k));
        }
        assert!(!is_media_pin("key_m"));
    }

    /// The usages a Consumer Control collection carries for these (HID Usage
    /// Tables, Consumer page) and Windows' VK_ codes — the kind of numbers
    /// that are silently wrong.
    #[test]
    fn the_codes_are_the_standard_ones() {
        let get = |p: &str| media_key(p).unwrap();
        assert_eq!((get("media_play_pause").usage, get("media_play_pause").vk), (0xCD, 0xB3));
        assert_eq!((get("media_volume_up").usage, get("media_volume_up").vk), (0xE9, 0xAF));
        assert_eq!((get("media_mute").usage, get("media_mute").vk), (0xE2, 0xAD));
        assert_eq!((get("media_next_track").usage, get("media_next_track").vk), (0xB5, 0xB0));
    }
}
