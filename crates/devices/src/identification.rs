#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControllerKind {
    XInput,
    DualShock4,
    DualSense,
    SwitchPro,
    /// Joy-Con 2, left half. Reached only through the BLE backend — these never
    /// appear to gilrs/SDL/hidapi because the controller implements no
    /// HID-over-GATT service for Windows to bind a driver to.
    JoyCon2L,
    /// Joy-Con 2, right half.
    JoyCon2R,
    Generic,
    MidiIn,
    MidiOut,
}

impl ControllerKind {
    pub fn detect(name: &str, vid: Option<u16>, pid: Option<u16>) -> Self {
        // VID/PID is authoritative when available.
        if let (Some(v), Some(p)) = (vid, pid) {
            match (v, p) {
                // Sony DS4
                (0x054C, 0x05C4)
                | (0x054C, 0x09CC)
                | (0x054C, 0x0BA0) => return Self::DualShock4,
                // Sony DualSense
                (0x054C, 0x0CE6)
                | (0x054C, 0x0DF2) => return Self::DualSense,
                // Nintendo Switch Pro (USB and Bluetooth share PID 0x2009)
                (0x057E, 0x2009) => return Self::SwitchPro,
                // Joy-Con 2. MUST be matched before the Nintendo catch-all
                // below, which would otherwise classify every Switch 2
                // controller as a Switch Pro and hand it the wrong pin layout.
                // `0x2066`/`0x2067` are normal mode; `0x2070`/`0x2071` are the
                // safe mode the controller falls back to after a failed
                // firmware update. Note R has the LOWER id.
                (0x057E, 0x2066) | (0x057E, 0x2070) => return Self::JoyCon2R,
                (0x057E, 0x2067) | (0x057E, 0x2071) => return Self::JoyCon2L,
                // Any other Nintendo VID — catch BT paths where PID may differ
                // or gilrs reports a variant PID not in the list above.
                (0x057E, _) => return Self::SwitchPro,
                // Microsoft Xbox / XInput class
                (0x045E, _) => return Self::XInput,
                _ => {}
            }
        }

        // Nintendo VID with no PID — some Bluetooth paths on Windows only expose VID.
        if vid == Some(0x057E) {
            return Self::SwitchPro;
        }

        // Name-based fallback (covers Bluetooth and unusual driver names).
        let n = name.to_ascii_lowercase();
        if n.contains("dualsense") {
            return Self::DualSense;
        }
        if n.contains("dualshock") || (n.contains("wireless controller") && n.contains("sony")) {
            return Self::DualShock4;
        }
        // DS4 with generic driver often just reports "Wireless Controller"
        //if n.contains("wireless controller") {
        //    return Self::DualShock4;
        //}
        if n.contains("pro controller") {
            return Self::SwitchPro;
        }
        if n.contains("xbox") || n.contains("xinput") || n.contains("microsoft") {
            return Self::XInput;
        }

        Self::Generic
    }

    /// Stable short slug used as part of the device ID string (e.g. `"gilrs:dualsense:0"`).
    pub fn id_slug(self) -> &'static str {
        match self {
            Self::XInput     => "xinput",
            Self::DualShock4 => "ds4",
            Self::DualSense  => "dualsense",
            Self::SwitchPro  => "switch_pro",
            Self::JoyCon2L   => "joycon2_l",
            Self::JoyCon2R   => "joycon2_r",
            Self::Generic    => "generic",
            Self::MidiIn     => "midi_in",
            Self::MidiOut    => "midi_out",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::XInput     => "Xbox / XInput",
            Self::DualShock4 => "DualShock 4",
            Self::DualSense  => "DualSense",
            Self::SwitchPro  => "Switch Pro Controller",
            // ❗ "Switch 2", not "Joy-Con 2". The Mobapad M12-S clones
            // Nintendo's VID and the genuine Joy-Con 2 product ids
            // (0x057E / 0x2066-7), so nothing in the identity distinguishes
            // them and naming either one specifically mislabels the other.
            // What IS common to both is the Switch 2 protocol this backend
            // speaks, so the name says that and no more.
            Self::JoyCon2L   => "Switch 2 Controller (L)",
            Self::JoyCon2R   => "Switch 2 Controller (R)",
            Self::Generic    => "Generic Gamepad",
            Self::MidiIn     => "MIDI Input Port",
            Self::MidiOut    => "MIDI Output Port",
        }
    }

    /// Whether this is one half of a Joy-Con 2 pair. Used by the UI to group
    /// the two halves and by the BLE backend's device-id prefix checks.
    pub fn is_joycon2(self) -> bool {
        matches!(self, Self::JoyCon2L | Self::JoyCon2R)
    }

    /// Whether this controller exposes pressure-sensitive analog triggers
    /// (LT/RT as a 0..1 axis). Switch Pro's ZL/ZR are digital-only buttons, so
    /// it returns false — the UI forces the digital-trigger override ON for it.
    /// Joy-Con 2's ZL/ZR are digital for the same reason. MIDI ports have no
    /// triggers and return false too.
    pub fn has_analog_triggers(self) -> bool {
        matches!(self, Self::XInput | Self::DualShock4 | Self::DualSense | Self::Generic)
    }
}
