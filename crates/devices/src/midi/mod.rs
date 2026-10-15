//! MIDI In/Out ports over midir (WinMM on Windows).
//!
//! * [`decode`] turns incoming bytes into sparse, latched bus pins.
//! * [`encode`] turns a sink's desired pins into outgoing messages.
//! * [`guard`] cancels our own echoes and mutes runaway feedback loops.
//!
//! Ports are listed whenever they are enumerated, but only OPENED while a
//! patch uses them (a `device.source`/`device.sink` node references the id).
//! Legacy WinMM input ports are single-client: holding every port open would
//! lock a DAW out of hardware FlexInput isn't even using.

pub mod decode;
pub mod encode;
pub mod guard;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use flexinput_core::{Signal, SignalType};

use crate::identification::ControllerKind;
use crate::{DeviceBackend, DevicePin, PhysicalDevice};

/// Pin id of a MIDI In node's AutoMap output.
pub const AUTOMAP_OUT_PIN: &str = "automap_out";
/// Pin id of a MIDI Out node's AutoMap input.
pub const AUTOMAP_IN_PIN: &str = "automap_in";

pub use decode::InPortDecoder;
pub use encode::OutPortEncoder;
pub use guard::{LoopGuard, PairInfo, PairStatus};
pub use flexinput_core::midi::cc_display_name;

// ── Entries ───────────────────────────────────────────────────────────────────

pub struct MidiInEntry {
    pub device_id: String,
    pub port_name: String,
    state: Arc<Mutex<InPortDecoder>>,
    _conn: MidiInputConnection<()>,
}

pub struct MidiOutEntry {
    pub device_id: String,
    pub port_name: String,
    conn: MidiOutputConnection,
    encoder: OutPortEncoder,
}

// ── Backend ───────────────────────────────────────────────────────────────────

pub struct MidiBackend {
    /// Open input connections (pinned ports only).
    pub in_entries: Vec<MidiInEntry>,
    /// Open output connections (pinned ports only).
    pub out_entries: Vec<MidiOutEntry>,
    /// Port names from the last enumeration, open or not.
    live_in: Vec<String>,
    live_out: Vec<String>,
    /// Sticky slot index per port name. Once a port name is assigned slot N,
    /// it keeps slot N even if it disappears and comes back later. New port
    /// names get the lowest unused slot. Keeps `midi_in:<N>` IDs stable across
    /// hot-plug so saved patches keep working.
    in_slots: HashMap<String, usize>,
    out_slots: HashMap<String, usize>,
    /// Shared with every input callback thread (echo checks) and the send
    /// path (fingerprints, breaker).
    guard: Arc<Mutex<LoopGuard>>,
}

impl Default for MidiBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiBackend {
    /// An empty backend. Enumeration runs on the MIDI watch thread (slow
    /// Win32 calls), never on construction.
    pub fn new() -> Self {
        Self {
            in_entries: Vec::new(),
            out_entries: Vec::new(),
            live_in: Vec::new(),
            live_out: Vec::new(),
            in_slots: HashMap::new(),
            out_slots: HashMap::new(),
            guard: Arc::new(Mutex::new(LoopGuard::new())),
        }
    }

    /// Handle to the loop guard, for [`Self::open_ports`] (which runs without
    /// the backend lock).
    pub fn guard_handle(&self) -> Arc<Mutex<LoopGuard>> {
        Arc::clone(&self.guard)
    }

    /// Query the Windows MIDI subsystem for the current list of live IN/OUT
    /// port names. Does NOT need `&self`, so callers can run it WITHOUT
    /// holding the MidiBackend lock. This is important because on Windows
    /// with loopMIDI installed, `MidiInput::new()` + `ports()` can take tens
    /// of milliseconds — long enough to stall the UI thread if it's blocked
    /// waiting on the same lock. Call this first, then pass the result to
    /// `apply_port_list()` which only briefly takes the lock to mutate state.
    pub fn list_live_ports() -> (Vec<String>, Vec<String>) {
        let t0 = Instant::now();
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

    /// Record a freshly enumerated port list: assign stable slots and drop
    /// connections whose port vanished. Opens nothing — see [`Self::open_ports`].
    pub fn apply_port_list(&mut self, live_in: &[String], live_out: &[String]) {
        for name in live_in {
            if !self.in_slots.contains_key(name) {
                let s = next_free_slot(&self.in_slots);
                self.in_slots.insert(name.clone(), s);
            }
        }
        for name in live_out {
            if !self.out_slots.contains_key(name) {
                let s = next_free_slot(&self.out_slots);
                self.out_slots.insert(name.clone(), s);
            }
        }
        self.live_in = live_in.to_vec();
        self.live_out = live_out.to_vec();
        self.close_where(|e| !live_in.contains(&e.port_name), |e| !live_out.contains(&e.port_name));
    }

    /// Close connections the pinned set no longer references. Instant; an Out
    /// port first releases any note it is still sounding.
    pub fn close_unpinned(&mut self, pinned: &HashSet<String>) {
        self.close_where(|e| !pinned.contains(&e.device_id), |e| !pinned.contains(&e.device_id));
    }

    /// ⚠ Connections are dropped (closed) only AFTER the guard lock is
    /// released. Closing an input waits on its driver callback, and that
    /// callback takes the guard lock — holding it across the close could
    /// deadlock.
    fn close_where(
        &mut self,
        close_in: impl Fn(&MidiInEntry) -> bool,
        close_out: impl Fn(&MidiOutEntry) -> bool,
    ) {
        let (closing_in, keep_in): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.in_entries).into_iter().partition(|e| close_in(e));
        let (mut closing_out, keep_out): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.out_entries).into_iter().partition(|e| close_out(e));
        self.in_entries = keep_in;
        self.out_entries = keep_out;
        if closing_in.is_empty() && closing_out.is_empty() {
            return;
        }
        for e in &mut closing_out {
            // Releases bypass the guard: they must go out even if the breaker
            // muted this port.
            for msg in e.encoder.apply(&[]) {
                let _ = e.conn.send(&msg.to_bytes());
            }
        }
        if let Ok(mut g) = self.guard.lock() {
            for id in closing_in.iter().map(|e| &e.device_id).chain(closing_out.iter().map(|e| &e.device_id)) {
                g.unregister(id);
            }
        }
        drop(closing_in);
        drop(closing_out);
    }

    /// Pinned live ports that aren't open yet, as `(port name, device id)`
    /// for inputs and outputs. Cheap (no Win32 calls), so the watch thread
    /// can check it often.
    pub fn ports_to_open(&self, pinned: &HashSet<String>) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let ins = self.live_in.iter()
            .map(|n| (n.clone(), in_id(self.in_slots[n])))
            .filter(|(_, id)| pinned.contains(id) && !self.in_entries.iter().any(|e| &e.device_id == id))
            .collect();
        let outs = self.live_out.iter()
            .map(|n| (n.clone(), out_id(self.out_slots[n])))
            .filter(|(_, id)| pinned.contains(id) && !self.out_entries.iter().any(|e| &e.device_id == id))
            .collect();
        (ins, outs)
    }

    /// Open connections for [`Self::ports_to_open`]'s result. Slow (re-lists the
    /// OS ports for handles, then opens each), and needs no `&self`, so call
    /// it WITHOUT the backend lock and hand the result to [`Self::adopt`].
    pub fn open_ports(
        ins: &[(String, String)],
        outs: &[(String, String)],
        guard: Arc<Mutex<LoopGuard>>,
    ) -> (Vec<MidiInEntry>, Vec<MidiOutEntry>) {
        let mut in_entries = Vec::new();
        if !ins.is_empty() {
            if let Ok(mi) = MidiInput::new("FlexInput-enum") {
                let ports = mi.ports();
                for (name, device_id) in ins {
                    let Some(port) = ports.iter().find(|p| mi.port_name(p).ok().as_deref() == Some(name.as_str())) else {
                        continue;
                    };
                    // Each connection consumes its MidiInput, so make one per port.
                    let Ok(conn_in) = MidiInput::new("FlexInput") else { continue };
                    let state = Arc::new(Mutex::new(InPortDecoder::new()));
                    let state_cb = Arc::clone(&state);
                    let guard_cb = Arc::clone(&guard);
                    let id_cb = device_id.clone();
                    let conn = conn_in.connect(port, "flexinput", move |ts, msg, _| {
                        let now = Instant::now();
                        // Our own output coming back: drop it before the
                        // decoder, so a mapping never reacts to it.
                        if guard_cb.lock().is_ok_and(|mut g| g.on_receive(&id_cb, msg, now)) {
                            return;
                        }
                        if let Ok(mut s) = state_cb.lock() {
                            s.feed(msg, ts, now);
                        }
                    }, ());
                    match conn {
                        Ok(conn) => in_entries.push(MidiInEntry {
                            device_id: device_id.clone(),
                            port_name: name.clone(),
                            state,
                            _conn: conn,
                        }),
                        Err(e) => eprintln!("[midi] open input {name:?} failed: {e}"),
                    }
                }
            }
        }

        let mut out_entries = Vec::new();
        if !outs.is_empty() {
            if let Ok(mo) = MidiOutput::new("FlexInput-enum") {
                let ports = mo.ports();
                for (name, device_id) in outs {
                    let Some(port) = ports.iter().find(|p| mo.port_name(p).ok().as_deref() == Some(name.as_str())) else {
                        continue;
                    };
                    let Ok(conn_out) = MidiOutput::new("FlexInput") else { continue };
                    match conn_out.connect(port, "flexinput") {
                        Ok(conn) => out_entries.push(MidiOutEntry {
                            device_id: device_id.clone(),
                            port_name: name.clone(),
                            conn,
                            encoder: OutPortEncoder::new(),
                        }),
                        Err(e) => eprintln!("[midi] open output {name:?} failed: {e}"),
                    }
                }
            }
        }
        (in_entries, out_entries)
    }

    /// Take ownership of connections from [`Self::open_ports`]. Any that became
    /// redundant meanwhile (already open, or no longer pinned) are dropped,
    /// which closes them.
    pub fn adopt(&mut self, ins: Vec<MidiInEntry>, outs: Vec<MidiOutEntry>, pinned: &HashSet<String>) {
        let now = Instant::now();
        // Redundant connections are dropped after the guard lock is released
        // (see `close_where`).
        let mut redundant_in = Vec::new();
        let mut redundant_out = Vec::new();
        {
            let mut guard = self.guard.lock().ok();
            for e in ins {
                if pinned.contains(&e.device_id) && !self.in_entries.iter().any(|x| x.device_id == e.device_id) {
                    if let Some(g) = guard.as_mut() { g.register_in(&e.device_id, &e.port_name); }
                    self.in_entries.push(e);
                } else {
                    redundant_in.push(e);
                }
            }
            for e in outs {
                if pinned.contains(&e.device_id) && !self.out_entries.iter().any(|x| x.device_id == e.device_id) {
                    if let Some(g) = guard.as_mut() { g.register_out(&e.device_id, &e.port_name, now); }
                    self.out_entries.push(e);
                } else {
                    redundant_out.push(e);
                }
            }
        }
        drop(redundant_in);
        drop(redundant_out);
    }

    /// Loop-guard state for display: known in/out pairs and muted Out ports.
    pub fn guard_status(&self) -> (Vec<PairInfo>, Vec<String>) {
        let Ok(g) = self.guard.lock() else { return (Vec::new(), Vec::new()) };
        let muted = self.out_entries.iter()
            .filter(|e| g.is_muted(&e.device_id))
            .map(|e| e.device_id.clone())
            .collect();
        (g.pairs(), muted)
    }

    /// Unmute an Out port the loop breaker muted. It resumes from a clean
    /// encoder, so the next tick re-sends whatever the patch currently holds.
    pub fn reset_breaker(&mut self, out_id: &str) {
        if let Ok(mut g) = self.guard.lock() {
            g.reset_breaker(out_id, Instant::now());
        }
        if let Some(e) = self.out_entries.iter_mut().find(|e| e.device_id == out_id) {
            e.encoder = OutPortEncoder::new();
        }
    }

    /// Release every note an In port still holds (see
    /// [`InPortDecoder::flush_notes`]). No-op for a port that isn't open.
    pub fn flush_input(&mut self, in_id: &str) {
        if let Some(e) = self.in_entries.iter().find(|e| e.device_id == in_id) {
            if let Ok(mut s) = e.state.lock() {
                s.flush_notes();
            }
        }
    }

    /// Out ports the loop breaker muted since the last call.
    pub fn take_breaker_trips(&mut self) -> Vec<String> {
        self.guard.lock().map(|mut g| g.take_trips()).unwrap_or_default()
    }

    /// The pin the last learnable message on this port addressed (concrete
    /// channel), clearing it. `None` if nothing learnable arrived since the
    /// last call, or the port isn't open.
    pub fn take_learned_pin(&mut self, device_id: &str) -> Option<flexinput_core::midi::MidiPin> {
        let entry = self.in_entries.iter_mut().find(|e| e.device_id == device_id)?;
        entry.state.lock().ok()?.take_last_pin()
    }

    /// Drive every open MIDI Out port with this tick's desired pins, keyed by
    /// device id. A port absent from `frames` gets an empty frame, which
    /// releases whatever it was still holding (bypass, unwired sink).
    pub fn send_frames(&mut self, frames: &HashMap<String, Vec<(String, Signal)>>) {
        let now = Instant::now();
        let Ok(mut guard) = self.guard.lock() else { return };
        for e in &mut self.out_entries {
            // A muted port is left alone entirely; reset_breaker restarts it
            // from a clean encoder.
            if guard.is_muted(&e.device_id) {
                continue;
            }
            let desired = frames.get(&e.device_id).map(Vec::as_slice).unwrap_or(&[]);
            for msg in e.encoder.apply(desired) {
                if !guard.on_send(&e.device_id, &msg, now) {
                    // The breaker just tripped. Release whatever is sounding
                    // (bypassing the guard — these must get out) and stop.
                    for rel in e.encoder.apply(&[]) {
                        let _ = e.conn.send(&rel.to_bytes());
                    }
                    e.encoder = OutPortEncoder::new();
                    eprintln!("[midi] loop breaker muted {} ({})", e.device_id, e.port_name);
                    break;
                }
                let _ = e.conn.send(&msg.to_bytes());
            }
        }
    }
}

fn in_id(slot: usize) -> String {
    format!("midi_in:{slot}")
}

fn out_id(slot: usize) -> String {
    format!("midi_out:{slot}")
}

impl DeviceBackend for MidiBackend {
    /// One PhysicalDevice per live port (open or not).
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        // Each port carries exactly one fixed pin, its AutoMap port; every
        // MIDI pin is added to the node dynamically.
        let automap = |id: &str| vec![DevicePin {
            id: id.to_string(),
            display_name: "Auto-Map".to_string(),
            signal_type: SignalType::AutoMap,
        }];
        let dev = |id: String, name: &str, kind: ControllerKind| PhysicalDevice {
            id,
            display_name: name.to_string(),
            kind,
            outputs: if kind == ControllerKind::MidiIn { automap(AUTOMAP_OUT_PIN) } else { vec![] },
            inputs: if kind == ControllerKind::MidiOut { automap(AUTOMAP_IN_PIN) } else { vec![] },
            instance_path: None,
            vid: None,
            pid: None,
        };
        let mut devs: Vec<PhysicalDevice> = self.live_in.iter()
            .map(|n| dev(in_id(self.in_slots[n]), n, ControllerKind::MidiIn))
            .collect();
        devs.extend(self.live_out.iter().map(|n| dev(out_id(self.out_slots[n]), n, ControllerKind::MidiOut)));
        devs
    }

    /// Emit the pins that are away from rest on every open IN port.
    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        puffin::profile_function!();
        let now = Instant::now();
        let mut out = Vec::new();
        for entry in &self.in_entries {
            let Ok(mut state) = entry.state.lock() else { continue };
            for (pin, sig) in state.poll(now) {
                out.push((entry.device_id.clone(), pin, sig));
            }
        }
        out
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        for entry in &self.in_entries {
            if let Ok(mut state) = entry.state.lock() {
                let n = state.take_event_count();
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
fn next_free_slot(slots: &HashMap<String, usize>) -> usize {
    let used: HashSet<usize> = slots.values().copied().collect();
    (0..).find(|n| !used.contains(n)).unwrap()
}
