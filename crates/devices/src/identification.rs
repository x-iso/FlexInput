#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControllerKind {
    XInput,
    DualShock4,
    DualSense,
    SwitchPro,
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
            Self::Generic    => "Generic Gamepad",
            Self::MidiIn     => "MIDI Input Port",
            Self::MidiOut    => "MIDI Output Port",
        }
    }
}
