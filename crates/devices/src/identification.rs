#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
                // Nintendo Switch Pro
                (0x057E, 0x2009) => return Self::SwitchPro,
                // Microsoft Xbox / XInput class
                (0x045E, _) => return Self::XInput,
                _ => {}
            }
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
        if n.contains("wireless controller") {
            return Self::DualShock4;
        }
        if n.contains("pro controller") {
            return Self::SwitchPro;
        }
        if n.contains("xbox") || n.contains("xinput") || n.contains("microsoft") {
            return Self::XInput;
        }

        Self::Generic
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
