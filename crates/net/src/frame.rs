//! In-memory representation of one network frame in each direction, plus the
//! slot layouts that map the canonical AutoMap pin tables onto dense `f32`
//! arrays for the wire.
//!
//! Layouts are derived from `flexinput_core::automap` at first use:
//!   * input direction  → [`ALL_PINS`]            (pad → game PC)
//!   * feedback direction → [`FEEDBACK_INLET_PINS`] (game PC → pad)
//!
//! A Vec2 pin occupies two consecutive slots (x, y); Bool pins are encoded as
//! 0.0 / 1.0. The layouts are append-only by contract (see the guard comment on
//! `ALL_PINS`): a peer running a newer build simply has more pins at the tail,
//! and both sides decode the common prefix. Mid-list insertions are detected by
//! the layout hash in the packet header (decoded min-prefix + warned).

use std::collections::HashMap;
use std::sync::OnceLock;

use flexinput_core::automap::{ALL_PINS, FEEDBACK_INLET_PINS};
use flexinput_core::signal::{Signal, SignalType};

/// One canonical pin's place in the dense slot array.
#[derive(Clone, Copy, Debug)]
pub struct PinSlot {
    pub id: &'static str,
    pub ty: SignalType,
    /// First f32 slot this pin occupies.
    pub offset: usize,
    /// 1 for Float/Bool, 2 for Vec2.
    pub width: usize,
}

/// Dense slot layout for one direction's canonical pin table.
pub struct SlotLayout {
    pub pins: Vec<PinSlot>,
    pub n_slots: usize,
    /// FNV-1a over the concatenated pin ids (with separators) — cheap identity
    /// check for "both peers agree what slot N means".
    pub layout_hash: u32,
    index: HashMap<&'static str, usize>,
}

impl SlotLayout {
    fn build(pins: impl Iterator<Item = (&'static str, SignalType)>) -> Self {
        let mut out = Vec::new();
        let mut index = HashMap::new();
        let mut offset = 0usize;
        let mut hash: u32 = 0x811c9dc5; // FNV-1a offset basis
        for (id, ty) in pins {
            let width = match ty {
                SignalType::Vec2 => 2,
                _ => 1,
            };
            index.insert(id, out.len());
            out.push(PinSlot { id, ty, offset, width });
            offset += width;
            for b in id.as_bytes().iter().chain(&[0u8]) {
                hash ^= *b as u32;
                hash = hash.wrapping_mul(0x01000193);
            }
        }
        Self { pins: out, n_slots: offset, layout_hash: hash, index }
    }

    pub fn pin_index(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }
}

/// Slot layout for the input direction (`ALL_PINS`).
pub fn input_layout() -> &'static SlotLayout {
    static LAYOUT: OnceLock<SlotLayout> = OnceLock::new();
    LAYOUT.get_or_init(|| SlotLayout::build(ALL_PINS.iter().map(|p| (p.id, p.signal_type))))
}

/// Slot layout for the feedback direction (`FEEDBACK_INLET_PINS`).
pub fn feedback_layout() -> &'static SlotLayout {
    static LAYOUT: OnceLock<SlotLayout> = OnceLock::new();
    LAYOUT.get_or_init(|| SlotLayout::build(FEEDBACK_INLET_PINS.iter().map(|p| (p.id, p.signal_type))))
}

/// Off-spec pin riding along with a bus frame (e.g. a Remapper's custom learned
/// key that isn't in `ALL_PINS`). Sparse, name-addressed.
#[derive(Clone, Debug, PartialEq)]
pub struct Extra {
    pub name: String,
    pub value: Signal,
}

/// One tick's worth of the forward AutoMap bus (pad → game PC).
///
/// `present[i]` mirrors the bus semantics of "this pin is driven this tick";
/// absent pins keep their zero slots on the wire but are NOT published on the
/// receiving side, so an upstream device that doesn't expose e.g. gyro doesn't
/// fight a locally-wired gyro source.
#[derive(Clone, Debug, PartialEq)]
pub struct BusFrame {
    pub present: Vec<bool>,
    pub slots: Vec<f32>,
    pub extras: Vec<Extra>,
}

impl Default for BusFrame {
    fn default() -> Self {
        Self::empty()
    }
}

impl BusFrame {
    /// All pins absent — the state before any upstream signal exists.
    pub fn empty() -> Self {
        let layout = input_layout();
        Self {
            present: vec![false; layout.pins.len()],
            slots: vec![0.0; layout.n_slots],
            extras: Vec::new(),
        }
    }

    /// All pins present at their neutral value (sticks centered, buttons
    /// released, triggers 0). Published locally by the receive node when the
    /// link goes stale so a lost sender can't leave inputs stuck.
    pub fn neutral() -> Self {
        let layout = input_layout();
        Self {
            present: vec![true; layout.pins.len()],
            slots: vec![0.0; layout.n_slots],
            extras: Vec::new(),
        }
    }

    /// Set a canonical pin by id. Non-canonical names go to `extras`.
    pub fn set(&mut self, pin_id: &str, sig: Signal) {
        let layout = input_layout();
        match layout.pin_index(pin_id) {
            Some(i) => self.set_idx(i, sig),
            None => {
                if let Some(e) = self.extras.iter_mut().find(|e| e.name == pin_id) {
                    e.value = sig;
                } else {
                    self.extras.push(Extra { name: pin_id.to_string(), value: sig });
                }
            }
        }
    }

    pub fn set_idx(&mut self, pin_idx: usize, sig: Signal) {
        let layout = input_layout();
        let Some(slot) = layout.pins.get(pin_idx) else { return };
        self.present[pin_idx] = true;
        match (slot.ty, sig) {
            (SignalType::Vec2, Signal::Vec2(v)) => {
                self.slots[slot.offset] = v.x;
                self.slots[slot.offset + 1] = v.y;
            }
            // Vec2 pin driven by a scalar (coercion mismatch upstream): x only.
            (SignalType::Vec2, s) => self.slots[slot.offset] = s.as_float(),
            (_, s) => self.slots[slot.offset] = s.as_float(),
        }
    }

    /// Reconstruct the typed signal for canonical pin `pin_idx`, or `None` if
    /// the pin is absent from this frame (or unknown to this build).
    pub fn get_idx(&self, pin_idx: usize) -> Option<Signal> {
        let layout = input_layout();
        let slot = layout.pins.get(pin_idx)?;
        if !self.present.get(pin_idx).copied().unwrap_or(false) {
            return None;
        }
        Some(match slot.ty {
            SignalType::Vec2 => Signal::Vec2(glam::Vec2::new(
                self.slots[slot.offset],
                self.slots[slot.offset + 1],
            )),
            SignalType::Bool => Signal::Bool(self.slots[slot.offset] >= 0.5),
            _ => Signal::Float(self.slots[slot.offset]),
        })
    }

    /// Iterate all present canonical pins as `(pin_id, Signal)`.
    pub fn iter_present(&self) -> impl Iterator<Item = (&'static str, Signal)> + '_ {
        let layout = input_layout();
        layout
            .pins
            .iter()
            .enumerate()
            .filter_map(move |(i, slot)| self.get_idx(i).map(|s| (slot.id, s)))
    }
}

/// One tick's worth of the feedback channel (game PC → pad): rumble, lightbar,
/// LEDs, adaptive triggers. All pins are Float; present-with-0.0 is meaningful
/// (active zeroing stops a running rumble).
#[derive(Clone, Debug, PartialEq)]
pub struct FeedbackFrame {
    pub present: Vec<bool>,
    pub vals: Vec<f32>,
}

impl Default for FeedbackFrame {
    fn default() -> Self {
        Self::empty()
    }
}

impl FeedbackFrame {
    pub fn empty() -> Self {
        let layout = feedback_layout();
        Self {
            present: vec![false; layout.pins.len()],
            vals: vec![0.0; layout.n_slots],
        }
    }

    pub fn set(&mut self, pin_id: &str, value: f32) {
        let layout = feedback_layout();
        if let Some(i) = layout.pin_index(pin_id) {
            self.present[i] = true;
            self.vals[i] = value;
        }
    }

    pub fn iter_present(&self) -> impl Iterator<Item = (&'static str, f32)> + '_ {
        let layout = feedback_layout();
        layout.pins.iter().enumerate().filter_map(move |(i, slot)| {
            self.present.get(i).copied().unwrap_or(false).then(|| (slot.id, self.vals[i]))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_consistent() {
        let l = input_layout();
        assert_eq!(l.pins.len(), ALL_PINS.len());
        let widths: usize = l.pins.iter().map(|p| p.width).sum();
        assert_eq!(widths, l.n_slots);
        // Every pin resolvable by id, offsets strictly increasing.
        let mut expect = 0;
        for (i, p) in l.pins.iter().enumerate() {
            assert_eq!(l.pin_index(p.id), Some(i));
            assert_eq!(p.offset, expect);
            expect += p.width;
        }
        let f = feedback_layout();
        assert_eq!(f.pins.len(), FEEDBACK_INLET_PINS.len());
        // Feedback pins are all Float → slots == pins.
        assert_eq!(f.n_slots, f.pins.len());
        assert_ne!(l.layout_hash, f.layout_hash);
    }

    #[test]
    fn bus_frame_roundtrips_types() {
        let mut fr = BusFrame::empty();
        fr.set("left_stick", Signal::Vec2(glam::Vec2::new(0.5, -1.0)));
        fr.set("btn_south", Signal::Bool(true));
        fr.set("left_trigger", Signal::Float(0.25));
        fr.set("custom_key_q", Signal::Bool(true)); // off-spec → extras

        let l = input_layout();
        assert_eq!(
            fr.get_idx(l.pin_index("left_stick").unwrap()),
            Some(Signal::Vec2(glam::Vec2::new(0.5, -1.0)))
        );
        assert_eq!(fr.get_idx(l.pin_index("btn_south").unwrap()), Some(Signal::Bool(true)));
        assert_eq!(fr.get_idx(l.pin_index("left_trigger").unwrap()), Some(Signal::Float(0.25)));
        assert_eq!(fr.get_idx(l.pin_index("right_trigger").unwrap()), None); // absent
        assert_eq!(fr.extras.len(), 1);
        assert_eq!(fr.iter_present().count(), 3);
    }

    #[test]
    fn neutral_is_all_present_all_zero() {
        let fr = BusFrame::neutral();
        assert!(fr.present.iter().all(|&p| p));
        assert!(fr.slots.iter().all(|&s| s == 0.0));
        let l = input_layout();
        assert_eq!(fr.get_idx(l.pin_index("btn_south").unwrap()), Some(Signal::Bool(false)));
    }

    #[test]
    fn feedback_frame_active_zero() {
        let mut fb = FeedbackFrame::empty();
        fb.set("rumble_strong", 0.0);
        // present-with-zero must survive: it's how rumble gets STOPPED.
        assert_eq!(fb.iter_present().collect::<Vec<_>>(), vec![("rumble_strong", 0.0)]);
    }
}
