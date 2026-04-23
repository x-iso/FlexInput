use std::sync::{Arc, Mutex};

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use flexinput_core::Signal;

use crate::{DeviceBackend, PhysicalDevice};
use crate::identification::ControllerKind;

// ── Per-port state shared with the MIDI callback thread ───────────────────────

struct InPortState {
    cc: [f32; 128],
    pitch_bend: f32,
    /// Most recently received CC number; consumed by take_learned_cc().
    last_cc: Option<u8>,
}

impl Default for InPortState {
    fn default() -> Self {
        Self { cc: [0.0; 128], pitch_bend: 0.0, last_cc: None }
    }
}

// ── Entries ───────────────────────────────────────────────────────────────────

pub struct MidiInEntry {
    pub device_id: String,
    pub port_name: String,
    state: Arc<Mutex<InPortState>>,
    _conn: MidiInputConnection<()>,
}

pub struct MidiOutEntry {
    pub device_id: String,
    pub port_name: String,
    conn: MidiOutputConnection,
}

// ── Backend ───────────────────────────────────────────────────────────────────

pub struct MidiBackend {
    pub in_entries: Vec<MidiInEntry>,
    pub out_entries: Vec<MidiOutEntry>,
}

impl MidiBackend {
    pub fn new() -> Self {
        let mut backend = Self { in_entries: Vec::new(), out_entries: Vec::new() };
        backend.connect_all();
        backend
    }

    fn connect_all(&mut self) {
        let port_count = MidiInput::new("FlexInput-enum")
            .map(|mi| mi.ports().len())
            .unwrap_or(0);

        for idx in 0..port_count {
            let Ok(mi) = MidiInput::new("FlexInput") else { continue };
            let ports = mi.ports();
            let Some(port) = ports.get(idx) else { continue };
            let port_name = mi.port_name(port).unwrap_or_else(|_| format!("MIDI In {}", idx));
            let device_id = format!("midi_in:{}", idx);
            let state = Arc::new(Mutex::new(InPortState::default()));
            let state_cb = Arc::clone(&state);
            let Ok(conn) = mi.connect(port, "flexinput", move |_ts, msg, _| {
                midi_in_callback(msg, &state_cb);
            }, ()) else { continue };
            self.in_entries.push(MidiInEntry { device_id, port_name, state, _conn: conn });
        }

        let out_count = MidiOutput::new("FlexInput-enum")
            .map(|mo| mo.ports().len())
            .unwrap_or(0);

        for idx in 0..out_count {
            let Ok(mo) = MidiOutput::new("FlexInput") else { continue };
            let ports = mo.ports();
            let Some(port) = ports.get(idx) else { continue };
            let port_name = mo.port_name(port).unwrap_or_else(|_| format!("MIDI Out {}", idx));
            let device_id = format!("midi_out:{}", idx);
            let Ok(conn) = mo.connect(port, "flexinput") else { continue };
            self.out_entries.push(MidiOutEntry { device_id, port_name, conn });
        }
    }

    /// Return the last CC number received on this port, clearing it.
    /// Returns None if no CC arrived since the last call.
    pub fn take_learned_cc(&mut self, device_id: &str) -> Option<u8> {
        let entry = self.in_entries.iter_mut().find(|e| e.device_id == device_id)?;
        entry.state.lock().ok()?.last_cc.take()
    }

    /// Send a CC value to a MIDI OUT port (called by route_midi_out in the app).
    pub fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        let Some(entry) = self.out_entries.iter_mut().find(|e| e.device_id == device_id) else { return };
        let Some(cc_str) = pin_id.strip_prefix("cc_") else { return };
        let Ok(cc) = cc_str.parse::<u8>() else { return };
        let value = match signal {
            Signal::Float(f) => (f.clamp(0.0, 1.0) * 127.0).round() as u8,
            Signal::Bool(b)  => if b { 127 } else { 0 },
            Signal::Int(i)   => (i as f32).clamp(0.0, 127.0) as u8,
            _                => return,
        };
        let _ = entry.conn.send(&[0xB0, cc, value]);
    }
}

impl DeviceBackend for MidiBackend {
    /// Returns one PhysicalDevice per port with NO pre-built pins.
    /// Pins are added dynamically via the canvas node body.
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        let mut devs: Vec<PhysicalDevice> = self.in_entries.iter()
            .map(|e| PhysicalDevice {
                id: e.device_id.clone(),
                display_name: e.port_name.clone(),
                kind: ControllerKind::MidiIn,
                outputs: vec![],
                inputs: vec![],
                instance_path: None,
            })
            .collect();
        devs.extend(self.out_entries.iter().map(|e| PhysicalDevice {
            id: e.device_id.clone(),
            display_name: e.port_name.clone(),
            kind: ControllerKind::MidiOut,
            outputs: vec![],
            inputs: vec![],
            instance_path: None,
        }));
        devs
    }

    /// Emit all 128 CC values + pitch bend for every connected IN port.
    /// The canvas node's output_pin_ids selects which subset flows into the graph.
    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        let mut out = Vec::new();
        for entry in &self.in_entries {
            let Ok(state) = entry.state.lock() else { continue };
            for cc in 0u8..=127 {
                out.push((entry.device_id.clone(), format!("cc_{}", cc), Signal::Float(state.cc[cc as usize])));
            }
            out.push((entry.device_id.clone(), "pitch_bend".to_string(), Signal::Float(state.pitch_bend)));
        }
        out
    }
}

// ── Callback ──────────────────────────────────────────────────────────────────

fn midi_in_callback(msg: &[u8], state: &Arc<Mutex<InPortState>>) {
    if msg.is_empty() { return; }
    let Ok(mut s) = state.lock() else { return };
    match msg[0] & 0xF0 {
        0xB0 if msg.len() >= 3 => {
            let cc = msg[1] as usize;
            if cc < 128 {
                s.cc[cc] = msg[2] as f32 / 127.0;
                s.last_cc = Some(cc as u8);
            }
        }
        0xE0 if msg.len() >= 3 => {
            let raw = ((msg[2] as i32) << 7) | (msg[1] as i32);
            s.pitch_bend = (raw - 8192) as f32 / 8192.0;
        }
        _ => {}
    }
}

// ── CC name helper (pub for use in the UI crate) ─────────────────────────────

pub fn cc_display_name(cc: u8) -> String {
    let label = match cc {
        0  => Some("Bank Select"),
        1  => Some("Modulation"),
        2  => Some("Breath"),
        4  => Some("Foot"),
        5  => Some("Portamento Time"),
        6  => Some("Data Entry MSB"),
        7  => Some("Volume"),
        8  => Some("Balance"),
        10 => Some("Pan"),
        11 => Some("Expression"),
        12 => Some("Effect 1"),
        13 => Some("Effect 2"),
        64 => Some("Sustain"),
        65 => Some("Portamento"),
        66 => Some("Sostenuto"),
        67 => Some("Soft Pedal"),
        68 => Some("Legato"),
        69 => Some("Hold 2"),
        91 => Some("Reverb"),
        92 => Some("Tremolo"),
        93 => Some("Chorus"),
        94 => Some("Detune"),
        95 => Some("Phaser"),
        _  => None,
    };
    match label {
        Some(name) => format!("CC {} – {}", cc, name),
        None       => format!("CC {}", cc),
    }
}
