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
    /// Raw MIDI message count since the last `take_event_counts` drain.
    /// Drained by the I/O thread to compute live per-device polling rates.
    event_count: u32,
}

impl Default for InPortState {
    fn default() -> Self {
        Self { cc: [0.0; 128], pitch_bend: 0.0, last_cc: None, event_count: 0 }
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
    /// Sticky slot index per port name. Once a port name is assigned slot N,
    /// it keeps slot N even if it disappears and comes back later. New port
    /// names get the lowest unused slot. Keeps `midi_in:<N>` IDs stable across
    /// hot-plug so saved patches keep working.
    in_slots: std::collections::HashMap<String, usize>,
    out_slots: std::collections::HashMap<String, usize>,
}

impl MidiBackend {
    pub fn new() -> Self {
        let mut backend = Self {
            in_entries: Vec::new(),
            out_entries: Vec::new(),
            in_slots: std::collections::HashMap::new(),
            out_slots: std::collections::HashMap::new(),
        };
        backend.rescan();
        backend
    }

    /// Query the Windows MIDI subsystem for the current list of live IN/OUT
    /// port names. Does NOT need `&self`, so callers can run it WITHOUT
    /// holding the MidiBackend lock. This is important because on Windows
    /// with loopMIDI installed, `MidiInput::new()` + `ports()` can take tens
    /// of milliseconds — long enough to stall the UI thread if it's blocked
    /// waiting on the same lock. Call this first, then pass the result to
    /// `apply_port_diff()` which only briefly takes the lock to mutate state.
    pub fn list_live_ports() -> (Vec<String>, Vec<String>) {
        let t0 = std::time::Instant::now();
        let mut ins: Vec<String> = Vec::new();
        if let Ok(mi) = MidiInput::new("FlexInput-enum") {
            for port in mi.ports() {
                if let Ok(name) = mi.port_name(&port) {
                    ins.push(name);
                }
            }
        }
        let mut outs: Vec<String> = Vec::new();
        if let Ok(mo) = MidiOutput::new("FlexInput-enum") {
            for port in mo.ports() {
                if let Ok(name) = mo.port_name(&port) {
                    outs.push(name);
                }
            }
        }
        let dt = t0.elapsed();
        if dt > std::time::Duration::from_millis(5) {
            eprintln!("[midi] list_live_ports took {:?} ({} in, {} out)", dt, ins.len(), outs.len());
        }
        (ins, outs)
    }

    /// Close connections to MIDI ports that aren't pinned by the canvas.
    /// On Windows MMSystem, an open MIDI handle keeps the port reported by
    /// `midiInGetNumDevs()` — so loopMIDI can't actually remove a port we
    /// still hold open. Call this BEFORE `list_live_ports()` to let
    /// no-longer-needed ports disappear from the OS enumeration. Canvas-
    /// wired ports (in `pinned_device_ids`) stay open so signals keep
    /// flowing.
    pub fn release_unpinned(&mut self, pinned_device_ids: &std::collections::HashSet<String>) {
        let before_in = self.in_entries.len();
        self.in_entries.retain(|e| pinned_device_ids.contains(&e.device_id));
        let before_out = self.out_entries.len();
        self.out_entries.retain(|e| pinned_device_ids.contains(&e.device_id));
        let released = (before_in - self.in_entries.len()) + (before_out - self.out_entries.len());
        if released > 0 {
            eprintln!("[midi] release_unpinned: closed {} handles (in={}, out={} remain)",
                released, self.in_entries.len(), self.out_entries.len());
        }
    }

    /// Apply a previously-fetched live port list: drop entries whose port
    /// vanished, open connections for newly-seen ports. Holds the
    /// MidiBackend lock for as little time as possible — all the slow
    /// Windows enumeration was already done in `list_live_ports()`.
    /// Opening a new MIDI connection IS still slow, but only happens once
    /// per newly-added port (not every rescan), so steady-state cost is zero.
    pub fn apply_port_diff(&mut self, live_in: &[String], live_out: &[String]) {
        // ── Inputs ───────────────────────────────────────────────────────────
        let before_in = self.in_entries.len();
        self.in_entries.retain(|e| live_in.iter().any(|n| n == &e.port_name));
        let dropped_in = before_in - self.in_entries.len();

        let mut added_in = 0usize;
        for name in live_in {
            if self.in_entries.iter().any(|e| &e.port_name == name) { continue; }
            let slot = match self.in_slots.get(name).copied() {
                Some(s) => s,
                None => {
                    let s = next_free_slot(&self.in_slots);
                    self.in_slots.insert(name.clone(), s);
                    s
                }
            };
            let Ok(mi) = MidiInput::new("FlexInput") else { continue };
            let ports = mi.ports();
            let Some(port) = ports.iter().find(|p| mi.port_name(p).ok().as_deref() == Some(name.as_str())) else { continue };
            let device_id = format!("midi_in:{}", slot);
            let state = Arc::new(Mutex::new(InPortState::default()));
            let state_cb = Arc::clone(&state);
            let Ok(conn) = mi.connect(port, "flexinput", move |_ts, msg, _| {
                midi_in_callback(msg, &state_cb);
            }, ()) else { continue };
            self.in_entries.push(MidiInEntry { device_id, port_name: name.clone(), state, _conn: conn });
            added_in += 1;
        }

        // ── Outputs ──────────────────────────────────────────────────────────
        let before_out = self.out_entries.len();
        self.out_entries.retain(|e| live_out.iter().any(|n| n == &e.port_name));
        let dropped_out = before_out - self.out_entries.len();

        let mut added_out = 0usize;
        for name in live_out {
            if self.out_entries.iter().any(|e| &e.port_name == name) { continue; }
            let slot = match self.out_slots.get(name).copied() {
                Some(s) => s,
                None => {
                    let s = next_free_slot(&self.out_slots);
                    self.out_slots.insert(name.clone(), s);
                    s
                }
            };
            let Ok(mo) = MidiOutput::new("FlexInput") else { continue };
            let ports = mo.ports();
            let Some(port) = ports.iter().find(|p| mo.port_name(p).ok().as_deref() == Some(name.as_str())) else { continue };
            let device_id = format!("midi_out:{}", slot);
            let Ok(conn) = mo.connect(port, "flexinput") else { continue };
            self.out_entries.push(MidiOutEntry { device_id, port_name: name.clone(), conn });
            added_out += 1;
        }

        if added_in + dropped_in + added_out + dropped_out > 0 {
            eprintln!("[midi] apply_port_diff: in +{}/-{}={}, out +{}/-{}={}",
                added_in, dropped_in, self.in_entries.len(),
                added_out, dropped_out, self.out_entries.len());
        }
    }

    /// Convenience: fetch live ports and apply the diff in one call.
    /// Holds the lock for the entire operation — only use this for the
    /// initial connect from `new()` where no contention is possible.
    pub fn rescan(&mut self) {
        let (ins, outs) = Self::list_live_ports();
        self.apply_port_diff(&ins, &outs);
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

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        for entry in &self.in_entries {
            if let Ok(mut state) = entry.state.lock() {
                let n = std::mem::take(&mut state.event_count);
                if n > 0 {
                    out.push((entry.device_id.clone(), n));
                }
            }
        }
        out
    }
}

// ── Slot allocation ───────────────────────────────────────────────────────────

/// Lowest non-negative integer not present as a value in `slots`.
fn next_free_slot(slots: &std::collections::HashMap<String, usize>) -> usize {
    let used: std::collections::HashSet<usize> = slots.values().copied().collect();
    (0..).find(|n| !used.contains(n)).unwrap()
}

// ── Callback ──────────────────────────────────────────────────────────────────

fn midi_in_callback(msg: &[u8], state: &Arc<Mutex<InPortState>>) {
    if msg.is_empty() { return; }
    let Ok(mut s) = state.lock() else { return };
    s.event_count = s.event_count.saturating_add(1);
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
