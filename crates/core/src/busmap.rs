//! The engine's per-tick working map: `(bus, pin) → Signal` as dense arrays.
//!
//! A patch hands one bus from module to module, each adding its name to a copy
//! of what came before, so a tick writes thousands of entries. In a hash map
//! keyed by two Strings that was most of the tick: hashing, comparing, copying
//! keys, growing tables. Here a bus or pin name is interned to a small integer the
//! first time it's seen, and each bus is an array indexed by pin, so a write
//! after the first tick touches no strings beyond the two lookups, allocates
//! nothing, and handing a bus on is copying array slots.
//!
//! The pins every patch has ([`known_pins`]) have fixed ids, so a loop over them
//! needs no strings at all: [`BusMap::view`] and the `*_canonical` methods. Other
//! names are interned per map, so this isn't for maps that cross a thread: those
//! stay [`SigMap`](crate::SigMap)s.

use std::collections::HashMap;

use crate::automap::{known_index, known_pins, ALL_PINS};
use crate::pinkey::{FastHasher, PinMapExt, SigMap};
use crate::Signal;

/// More than any patch has, kept from growing for ever if names keep changing
/// (a tab switch brings new bus names; a long session could see many).
const MAX_BUSES: usize = 4096;
const MAX_PINS: usize = 8192;

#[derive(Default)]
struct Bus {
    /// Indexed by pin id; only as long as the highest pin this bus has held.
    vals: Vec<Option<Signal>>,
    /// Whether the pin is in `order`, which outlives a removal so that removing
    /// doesn't have to search it.
    listed: Vec<bool>,
    /// The pins in the order they were first set since the last clear.
    order: Vec<u32>,
    live: usize,
}

impl Clone for Bus {
    fn clone(&self) -> Self {
        Self {
            vals: self.vals.clone(),
            listed: self.listed.clone(),
            order: self.order.clone(),
            live: self.live,
        }
    }
    fn clone_from(&mut self, src: &Self) {
        self.vals.clone_from(&src.vals);
        self.listed.clone_from(&src.listed);
        self.order.clone_from(&src.order);
        self.live = src.live;
    }
}

impl Bus {
    #[inline]
    fn get(&self, pin: usize) -> Option<&Signal> {
        self.vals.get(pin).and_then(Option::as_ref)
    }

    #[inline]
    fn set(&mut self, pin: usize, v: Signal) {
        if pin >= self.vals.len() {
            self.vals.resize(pin + 1, None);
            self.listed.resize(pin + 1, false);
        }
        if self.vals[pin].is_none() {
            self.live += 1;
            if !self.listed[pin] {
                self.listed[pin] = true;
                self.order.push(pin as u32);
            }
        }
        self.vals[pin] = Some(v);
    }

    fn clear(&mut self) {
        for &pin in &self.order {
            self.vals[pin as usize] = None;
            self.listed[pin as usize] = false;
        }
        self.order.clear();
        self.live = 0;
    }
}

/// A `(bus, pin) → Signal` map with the shape of [`SigMap`](crate::SigMap). Iterates
/// in the order pins were first set, bus by bus.
#[derive(Default)]
pub struct BusMap {
    bus_ids: HashMap<String, u32, FastHasher>,
    /// Pins not in [`known_pins`], numbered from after them.
    pin_ids: HashMap<String, u32, FastHasher>,
    bus_names: Vec<String>,
    pin_names: Vec<String>,
    buses: Vec<Bus>,
    /// Unique to this exact set of names, so [`clone_from`](Clone::clone_from)
    /// can tell it already has them.
    names_gen: u64,
}

/// Where each new set of names takes its [`BusMap::names_gen`] from.
static NAMES_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_names_gen() -> u64 {
    NAMES_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Clone for BusMap {
    fn clone(&self) -> Self {
        Self {
            bus_ids: self.bus_ids.clone(),
            pin_ids: self.pin_ids.clone(),
            bus_names: self.bus_names.clone(),
            pin_names: self.pin_names.clone(),
            buses: self.buses.clone(),
            names_gen: self.names_gen,
        }
    }
    /// Reuses this map's arrays, and its names if they're the same ones: a tick
    /// can copy a map this way with no allocation.
    fn clone_from(&mut self, src: &Self) {
        if self.names_gen != src.names_gen {
            self.bus_ids.clone_from(&src.bus_ids);
            self.pin_ids.clone_from(&src.pin_ids);
            self.bus_names.clone_from(&src.bus_names);
            self.pin_names.clone_from(&src.pin_names);
            self.names_gen = src.names_gen;
        }
        self.buses.clone_from(&src.buses);
    }
}

/// One bus of a [`BusMap`], read by pin index: `ALL_PINS[i]` is at `at(i)`.
#[derive(Clone, Copy)]
pub struct BusView<'a>(Option<&'a Bus>);

impl<'a> BusView<'a> {
    #[inline]
    pub fn at(self, ix: usize) -> Option<&'a Signal> {
        self.0?.get(ix)
    }
}

impl BusMap {
    #[inline]
    fn find_pin(&self, pin: &str) -> Option<usize> {
        known_index(pin).or_else(|| {
            let n = *self.pin_ids.get(pin)? as usize;
            Some(known_pins().names.len() + n)
        })
    }

    #[inline]
    fn find(&self, dev: &str, pin: &str) -> Option<(usize, usize)> {
        Some((*self.bus_ids.get(dev)? as usize, self.find_pin(pin)?))
    }

    fn pin_name(&self, id: usize) -> &str {
        let known = &known_pins().names;
        match known.get(id) {
            Some(n) => n,
            None => &self.pin_names[id - known.len()],
        }
    }

    /// A bus to read by pin index. Empty for a bus nothing has written, or `""`.
    #[inline]
    pub fn view(&self, dev: &str) -> BusView<'_> {
        if dev.is_empty() {
            return BusView(None);
        }
        BusView(self.bus_ids.get(dev).map(|&b| &self.buses[b as usize]))
    }

    /// A bus's number, stable until the next [`clear`](Self::clear), for
    /// [`at`](Self::at) and [`set_at`](Self::set_at) in a loop.
    #[inline]
    pub fn bus_id(&mut self, dev: &str) -> usize {
        self.intern_bus(dev)
    }

    /// The number of a bus already written; `None` for any other, or `""`.
    #[inline]
    pub fn find_bus(&self, dev: &str) -> Option<usize> {
        if dev.is_empty() {
            return None;
        }
        self.bus_ids.get(dev).map(|&b| b as usize)
    }

    /// As [`view`](Self::view), by number, without borrowing past the call.
    #[inline]
    pub fn at(&self, bus: usize, ix: usize) -> Option<Signal> {
        self.buses[bus].get(ix).copied()
    }

    #[inline]
    pub fn set_at(&mut self, bus: usize, ix: usize, v: Signal) {
        self.buses[bus].set(ix, v);
    }

    /// For every pin of [`ALL_PINS`], in order, write `(to, pin)` from `from`,
    /// else from `fallback`, leaving it alone if neither has one.
    pub fn pass_canonical(&mut self, to: &str, from: &str, fallback: BusView<'_>) {
        let t = self.intern_bus(to);
        let f = (!from.is_empty()).then(|| self.bus_ids.get(from).copied()).flatten();
        for ix in 0..ALL_PINS.len() {
            let sig = f.and_then(|f| self.buses[f as usize].get(ix).copied())
                .or_else(|| fallback.at(ix).copied());
            if let Some(s) = sig {
                self.buses[t].set(ix, s);
            }
        }
    }

    /// For every pin of [`ALL_PINS`] that `to` doesn't carry, take `src`'s, if any.
    pub fn fill_canonical(&mut self, to: &str, src: BusView<'_>) {
        let t = self.intern_bus(to);
        for ix in 0..ALL_PINS.len() {
            if self.buses[t].get(ix).is_none() {
                if let Some(&s) = src.at(ix) {
                    self.buses[t].set(ix, s);
                }
            }
        }
    }

    fn intern_bus(&mut self, dev: &str) -> usize {
        if let Some(&id) = self.bus_ids.get(dev) {
            return id as usize;
        }
        let id = self.buses.len();
        self.names_gen = next_names_gen();
        self.bus_ids.insert(dev.to_string(), id as u32);
        self.bus_names.push(dev.to_string());
        self.buses.push(Bus::default());
        id
    }

    fn intern_pin(&mut self, pin: &str) -> usize {
        if let Some(id) = self.find_pin(pin) {
            return id;
        }
        let n = self.pin_names.len();
        self.names_gen = next_names_gen();
        self.pin_ids.insert(pin.to_string(), n as u32);
        self.pin_names.push(pin.to_string());
        known_pins().names.len() + n
    }

    /// Empty the map for the next tick, keeping its names and arrays.
    pub fn clear(&mut self) {
        if self.buses.len() > MAX_BUSES || self.pin_names.len() > MAX_PINS {
            *self = Self { names_gen: next_names_gen(), ..Self::default() };
            return;
        }
        for bus in &mut self.buses {
            bus.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.buses.iter().map(|b| b.live).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.buses.iter().all(|b| b.live == 0)
    }

    #[inline]
    pub fn get(&self, key: &(String, String)) -> Option<&Signal> {
        self.get_pin(&key.0, &key.1)
    }

    #[inline]
    pub fn contains_key(&self, key: &(String, String)) -> bool {
        self.has_pin(&key.0, &key.1)
    }

    #[inline]
    pub fn insert(&mut self, key: (String, String), v: Signal) {
        self.set_pin(&key.0, &key.1, v);
    }

    #[inline]
    pub fn remove(&mut self, key: &(String, String)) -> Option<Signal> {
        self.remove_pin(&key.0, &key.1)
    }

    pub fn iter(&self) -> impl Iterator<Item = ((&str, &str), &Signal)> + '_ {
        self.buses.iter().zip(&self.bus_names).flat_map(move |(bus, name)| {
            bus.order.iter().filter_map(move |&pin| {
                let v = bus.get(pin as usize)?;
                Some(((name.as_str(), self.pin_name(pin as usize)), v))
            })
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.iter().map(|(k, _)| k)
    }

    /// The buses holding anything.
    pub fn buses(&self) -> impl Iterator<Item = &str> + '_ {
        self.buses.iter().zip(&self.bus_names)
            .filter(|(bus, _)| bus.live > 0)
            .map(|(_, name)| name.as_str())
    }

    /// One bus's pins, without walking the others.
    pub fn pins_of<'a>(&'a self, dev: &str) -> impl Iterator<Item = (&'a str, &'a Signal)> + 'a {
        let bus = self.bus_ids.get(dev).map(|&id| &self.buses[id as usize]);
        bus.into_iter().flat_map(move |bus| {
            bus.order.iter().filter_map(move |&pin| {
                Some((self.pin_name(pin as usize), bus.get(pin as usize)?))
            })
        })
    }

    /// One bus's pins other than [`ALL_PINS`]: learned keys, MIDI, markers.
    pub fn extras_of<'a>(&'a self, dev: &str) -> impl Iterator<Item = (&'a str, &'a Signal)> + 'a {
        let bus = self.bus_ids.get(dev).map(|&id| &self.buses[id as usize]);
        bus.into_iter().flat_map(move |bus| {
            bus.order.iter().filter(|&&pin| (pin as usize) >= ALL_PINS.len()).filter_map(move |&pin| {
                Some((self.pin_name(pin as usize), bus.get(pin as usize)?))
            })
        })
    }

    /// Hand on what `from` carries beyond [`ALL_PINS`], over what `to` has.
    pub fn copy_bus_extras(&mut self, from: &str, to: &str) {
        let Some(&f) = self.bus_ids.get(from) else { return };
        let f = f as usize;
        let t = self.intern_bus(to);
        if f == t {
            return;
        }
        let (src, dst) = if f < t {
            let (a, b) = self.buses.split_at_mut(t);
            (&a[f], &mut b[0])
        } else {
            let (a, b) = self.buses.split_at_mut(f);
            (&b[0], &mut a[t])
        };
        for &pin in &src.order {
            let pin = pin as usize;
            if pin >= ALL_PINS.len() {
                if let Some(&v) = src.get(pin) {
                    dst.set(pin, v);
                }
            }
        }
    }

    /// Hand a bus on: every `(from, pin)` whose pin `keep` accepts, written to
    /// `(to, pin)` over what `to` already carries.
    pub fn copy_bus(&mut self, from: &str, to: &str, mut keep: impl FnMut(&str) -> bool) {
        let Some(&f) = self.bus_ids.get(from) else { return };
        let f = f as usize;
        let t = self.intern_bus(to);
        if f == t {
            return;
        }
        let (src, dst) = if f < t {
            let (a, b) = self.buses.split_at_mut(t);
            (&a[f], &mut b[0])
        } else {
            let (a, b) = self.buses.split_at_mut(f);
            (&b[0], &mut a[t])
        };
        for &pin in &src.order {
            let pin = pin as usize;
            if let Some(&v) = src.get(pin) {
                let known = &known_pins().names;
                let name = match known.get(pin) {
                    Some(n) => *n,
                    None => self.pin_names[pin - known.len()].as_str(),
                };
                if keep(name) {
                    dst.set(pin, v);
                }
            }
        }
    }
}

impl PinMapExt<Signal> for BusMap {
    #[inline]
    fn get_pin(&self, dev: &str, pin: &str) -> Option<&Signal> {
        let (b, p) = self.find(dev, pin)?;
        self.buses[b].get(p)
    }
    #[inline]
    fn has_pin(&self, dev: &str, pin: &str) -> bool {
        self.get_pin(dev, pin).is_some()
    }
    #[inline]
    fn get_pin_mut(&mut self, dev: &str, pin: &str) -> Option<&mut Signal> {
        let (b, p) = self.find(dev, pin)?;
        self.buses[b].vals.get_mut(p)?.as_mut()
    }
    fn remove_pin(&mut self, dev: &str, pin: &str) -> Option<Signal> {
        let (b, p) = self.find(dev, pin)?;
        let bus = &mut self.buses[b];
        let old = bus.vals.get_mut(p)?.take();
        if old.is_some() {
            bus.live -= 1;
        }
        old
    }
    #[inline]
    fn set_pin(&mut self, dev: &str, pin: &str, v: Signal) {
        let b = self.intern_bus(dev);
        let p = self.intern_pin(pin);
        self.buses[b].set(p, v);
    }
    #[inline]
    fn set_pin_if_absent(&mut self, dev: &str, pin: &str, v: Signal) {
        let b = self.intern_bus(dev);
        let p = self.intern_pin(pin);
        if self.buses[b].get(p).is_none() {
            self.buses[b].set(p, v);
        }
    }
    #[inline]
    fn merge_pin<F: FnOnce(&mut Signal)>(&mut self, dev: &str, pin: &str, v: Signal, merge: F) {
        let b = self.intern_bus(dev);
        let p = self.intern_pin(pin);
        match self.buses[b].vals.get_mut(p).and_then(Option::as_mut) {
            Some(slot) => merge(slot),
            None => self.buses[b].set(p, v),
        }
    }
}

// Spare `PinSigs` tables: a mapping module makes one a tick.
thread_local! {
    static SPARE_TABLES: std::cell::RefCell<Vec<Vec<Option<Signal>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// One module's look at a bus for one tick, `pin → Signal`: the known pins in an
/// array, anything else (MIDI, learned keys) in a map. A module fills it, adds
/// the pins it derives, and reads it back by name a few times.
#[derive(Default)]
pub struct PinSigs {
    /// Empty until the first known pin; then one slot per [`known_pins`].
    known: Vec<Option<Signal>>,
    other: HashMap<String, Signal, FastHasher>,
}

impl Clone for PinSigs {
    fn clone(&self) -> Self {
        let mut c = Self { known: Vec::new(), other: self.other.clone() };
        if !self.known.is_empty() {
            c.table().copy_from_slice(&self.known);
        }
        c
    }
}

impl Drop for PinSigs {
    fn drop(&mut self) {
        if self.known.is_empty() {
            return;
        }
        let mut t = std::mem::take(&mut self.known);
        t.fill(None);
        SPARE_TABLES.with(|s| {
            let mut s = s.borrow_mut();
            if s.len() < 64 {
                s.push(t);
            }
        });
    }
}

impl PinSigs {
    fn table(&mut self) -> &mut Vec<Option<Signal>> {
        if self.known.is_empty() {
            self.known = SPARE_TABLES.with(|s| s.borrow_mut().pop())
                .unwrap_or_else(|| vec![None; known_pins().names.len()]);
        }
        &mut self.known
    }

    /// For every pin of [`ALL_PINS`]: `first`'s value, else `second`'s.
    pub fn fill_canonical(&mut self, first: BusView<'_>, second: BusView<'_>) {
        let t = self.table();
        for ix in 0..ALL_PINS.len() {
            if let Some(&s) = first.at(ix).or_else(|| second.at(ix)) {
                t[ix] = Some(s);
            }
        }
    }

    /// The value of `ALL_PINS[ix]`.
    #[inline]
    pub fn canonical(&self, ix: usize) -> Option<&Signal> {
        self.known.get(ix).and_then(Option::as_ref)
    }

    #[inline]
    pub fn get(&self, pin: &str) -> Option<&Signal> {
        match known_index(pin) {
            Some(ix) => self.known.get(ix).and_then(Option::as_ref),
            None => self.other.get(pin),
        }
    }

    #[inline]
    pub fn contains_key(&self, pin: &str) -> bool {
        self.get(pin).is_some()
    }

    #[inline]
    pub fn insert(&mut self, pin: impl AsRef<str>, v: Signal) {
        let pin = pin.as_ref();
        match known_index(pin) {
            Some(ix) => self.table()[ix] = Some(v),
            None => match self.other.get_mut(pin) {
                Some(slot) => *slot = v,
                None => { self.other.insert(pin.to_string(), v); }
            },
        }
    }

    pub fn get_mut(&mut self, pin: &str) -> Option<&mut Signal> {
        match known_index(pin) {
            Some(ix) => self.known.get_mut(ix)?.as_mut(),
            None => self.other.get_mut(pin),
        }
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut Signal)> + '_ {
        let names = &known_pins().names;
        self.known.iter_mut().zip(names.iter())
            .filter_map(|(v, n)| Some((*n, v.as_mut()?)))
            .chain(self.other.iter_mut().map(|(k, v)| (k.as_str(), v)))
    }

    pub fn insert_if_absent(&mut self, pin: &str, v: Signal) {
        if !self.contains_key(pin) {
            self.insert(pin, v);
        }
    }

    pub fn remove(&mut self, pin: &str) -> Option<Signal> {
        match known_index(pin) {
            Some(ix) => self.known.get_mut(ix)?.take(),
            None => self.other.remove(pin),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Signal)> + '_ {
        let names = &known_pins().names;
        self.known.iter().zip(names.iter())
            .filter_map(|(v, n)| Some((*n, v.as_ref()?)))
            .chain(self.other.iter().map(|(k, v)| (k.as_str(), v)))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> + '_ {
        self.iter().map(|(k, _)| k)
    }

    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }
}

impl std::fmt::Debug for PinSigs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<'a> IntoIterator for &'a PinSigs {
    type Item = (&'a str, &'a Signal);
    type IntoIter = Box<dyn Iterator<Item = (&'a str, &'a Signal)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl IntoIterator for PinSigs {
    type Item = (String, Signal);
    type IntoIter = std::vec::IntoIter<(String, Signal)>;
    fn into_iter(self) -> Self::IntoIter {
        let v: Vec<_> = self.iter().map(|(k, s)| (k.to_string(), *s)).collect();
        v.into_iter()
    }
}

impl FromIterator<(String, Signal)> for PinSigs {
    fn from_iter<I: IntoIterator<Item = (String, Signal)>>(iter: I) -> Self {
        let mut m = Self::default();
        for (k, v) in iter {
            m.insert(k, v);
        }
        m
    }
}

impl FromIterator<((String, String), Signal)> for BusMap {
    fn from_iter<I: IntoIterator<Item = ((String, String), Signal)>>(iter: I) -> Self {
        let mut m = Self::default();
        for (k, v) in iter {
            m.insert(k, v);
        }
        m
    }
}

impl std::ops::Index<&(String, String)> for BusMap {
    type Output = Signal;
    fn index(&self, key: &(String, String)) -> &Signal {
        self.get(key).expect("no entry for this bus and pin")
    }
}

impl From<&BusMap> for SigMap {
    fn from(m: &BusMap) -> SigMap {
        m.iter().map(|((d, p), v)| ((d.to_string(), p.to_string()), *v)).collect()
    }
}

impl IntoIterator for BusMap {
    type Item = ((String, String), Signal);
    type IntoIter = <SigMap as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        SigMap::from(&self).into_iter()
    }
}

impl<'a> IntoIterator for &'a BusMap {
    type Item = ((&'a str, &'a str), &'a Signal);
    type IntoIter = Box<dyn Iterator<Item = Self::Item> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl std::fmt::Debug for BusMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f32) -> Signal {
        Signal::Float(v)
    }

    #[test]
    fn set_get_overwrite_and_miss() {
        let mut m = BusMap::default();
        assert_eq!(m.get_pin("a", "x"), None);
        m.set_pin("a", "x", f(1.0));
        m.set_pin("a", "x", f(2.0));
        assert_eq!(m.get_pin("a", "x"), Some(&f(2.0)));
        assert_eq!(m.get_pin("a", "y"), None);
        assert_eq!(m.get_pin("b", "x"), None);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn a_pin_only_another_bus_holds_is_a_miss() {
        let mut m = BusMap::default();
        m.set_pin("a", "x", f(1.0));
        m.set_pin("a", "y", f(1.0));
        m.set_pin("b", "x", f(2.0));
        assert_eq!(m.get_pin("b", "y"), None, "beyond the end of b's array");
        assert!(!m.has_pin("b", "y"));
        assert_eq!(m.remove_pin("b", "y"), None);
    }

    #[test]
    fn absent_and_merge_only_write_when_they_should() {
        let mut m = BusMap::default();
        m.set_pin_if_absent("a", "x", f(1.0));
        m.set_pin_if_absent("a", "x", f(2.0));
        assert_eq!(m.get_pin("a", "x"), Some(&f(1.0)));
        m.merge_pin("a", "x", f(9.0), |s| *s = f(3.0));
        assert_eq!(m.get_pin("a", "x"), Some(&f(3.0)));
        m.merge_pin("a", "y", f(9.0), |s| *s = f(3.0));
        assert_eq!(m.get_pin("a", "y"), Some(&f(9.0)));
    }

    #[test]
    fn a_removed_pin_is_gone_until_set_again_and_not_listed_twice() {
        let mut m = BusMap::default();
        m.set_pin("a", "x", f(1.0));
        assert_eq!(m.remove_pin("a", "x"), Some(f(1.0)));
        assert_eq!(m.remove_pin("a", "x"), None);
        assert!(m.is_empty());
        assert_eq!(m.iter().count(), 0);
        m.set_pin("a", "x", f(2.0));
        assert_eq!(m.len(), 1);
        assert_eq!(m.iter().count(), 1);
    }

    #[test]
    fn clear_empties_everything_and_the_map_still_works() {
        let mut m = BusMap::default();
        m.set_pin("a", "x", f(1.0));
        m.set_pin("b", "y", f(1.0));
        m.clear();
        assert!(m.is_empty());
        assert_eq!(m.get_pin("a", "x"), None);
        assert_eq!(m.iter().count(), 0);
        m.set_pin("a", "y", f(5.0));
        assert_eq!(m.keys().collect::<Vec<_>>(), [("a", "y")]);
    }

    #[test]
    fn iter_goes_bus_by_bus_in_the_order_pins_were_first_set() {
        let mut m = BusMap::default();
        m.set_pin("a", "z", f(1.0));
        m.set_pin("b", "q", f(2.0));
        m.set_pin("a", "m", f(3.0));
        m.set_pin("a", "z", f(4.0));
        let got: Vec<_> = m.iter().map(|((d, p), v)| (d, p, *v)).collect();
        assert_eq!(got, [("a", "z", f(4.0)), ("a", "m", f(3.0)), ("b", "q", f(2.0))]);
        let a: Vec<_> = m.pins_of("a").map(|(p, _)| p).collect();
        assert_eq!(a, ["z", "m"]);
        assert_eq!(m.pins_of("none").count(), 0);
    }

    #[test]
    fn copy_bus_overwrites_filters_and_leaves_the_source() {
        let mut m = BusMap::default();
        m.set_pin("from", "x", f(1.0));
        m.set_pin("from", "skip", f(1.0));
        m.set_pin("to", "x", f(9.0));
        m.set_pin("to", "own", f(9.0));
        m.copy_bus("from", "to", |p| p != "skip");
        assert_eq!(m.get_pin("to", "x"), Some(&f(1.0)));
        assert_eq!(m.get_pin("to", "own"), Some(&f(9.0)));
        assert_eq!(m.get_pin("to", "skip"), None);
        assert_eq!(m.get_pin("from", "skip"), Some(&f(1.0)));
    }

    #[test]
    fn copy_bus_works_in_both_table_orders_and_onto_itself() {
        let mut m = BusMap::default();
        m.set_pin("first", "x", f(1.0));
        m.set_pin("second", "y", f(2.0));
        m.copy_bus("second", "first", |_| true);
        assert_eq!(m.get_pin("first", "y"), Some(&f(2.0)));
        m.copy_bus("first", "second", |_| true);
        assert_eq!(m.get_pin("second", "x"), Some(&f(1.0)));
        m.copy_bus("first", "first", |_| true);
        assert_eq!(m.pins_of("first").count(), 2);
        m.copy_bus("nothing", "third", |_| true);
        assert_eq!(m.pins_of("third").count(), 0);
    }

    #[test]
    fn a_removed_source_pin_is_not_handed_on() {
        let mut m = BusMap::default();
        m.set_pin("from", "x", f(1.0));
        m.remove_pin("from", "x");
        m.copy_bus("from", "to", |_| true);
        assert_eq!(m.get_pin("to", "x"), None);
    }

    #[test]
    fn clone_from_is_an_exact_copy_whatever_the_target_held() {
        let mut src = BusMap::default();
        src.set_pin("a", "x", f(1.0));
        src.set_pin("b", "key_f", f(2.0));
        let mut dst = BusMap::default();
        dst.set_pin("z", "key_q", f(9.0));
        dst.set_pin("a", "key_q", f(9.0));
        dst.clone_from(&src);
        assert_eq!(dst.get_pin("a", "x"), Some(&f(1.0)));
        assert_eq!(dst.get_pin("b", "key_f"), Some(&f(2.0)));
        assert_eq!(dst.get_pin("z", "key_q"), None);
        assert_eq!(dst.len(), 2);

        // Same names, new values: and a pin gone from the source goes.
        src.clear();
        src.set_pin("a", "x", f(3.0));
        dst.clone_from(&src);
        assert_eq!(dst.get_pin("a", "x"), Some(&f(3.0)));
        assert_eq!(dst.get_pin("b", "key_f"), None);
        assert_eq!(dst.len(), 1);

        // New names after a copy, on either side.
        src.set_pin("c", "key_w", f(4.0));
        dst.set_pin("d", "key_e", f(5.0));
        dst.clone_from(&src);
        assert_eq!(dst.get_pin("c", "key_w"), Some(&f(4.0)));
        assert_eq!(dst.get_pin("d", "key_e"), None);
        assert_eq!(dst.keys().count(), 2);
    }

    #[test]
    fn extras_are_everything_but_the_canonical_pins() {
        let canon = ALL_PINS[0].id;
        let marker = format!("{}{canon}", crate::automap::CONSUMED_PREFIX);
        let mut m = BusMap::default();
        m.set_pin("from", canon, f(1.0));
        m.set_pin("from", "key_f", f(2.0));
        m.set_pin("from", &marker, f(3.0));
        m.set_pin("from", "left_stick_up", f(4.0));
        let mut extras: Vec<_> = m.extras_of("from").map(|(p, _)| p.to_string()).collect();
        extras.sort();
        let mut want: Vec<String> = m.pins_of("from").map(|(p, _)| p)
            .filter(|p| !crate::automap::is_canonical_pin(p)).map(str::to_string).collect();
        want.sort();
        assert_eq!(extras, want);
        assert_eq!(extras.len(), 3);

        m.set_pin("to", canon, f(9.0));
        m.set_pin("to", "key_f", f(9.0));
        m.copy_bus_extras("from", "to");
        assert_eq!(m.get_pin("to", canon), Some(&f(9.0)), "canonical pins stay");
        assert_eq!(m.get_pin("to", "key_f"), Some(&f(2.0)));
        assert_eq!(m.get_pin("to", &marker), Some(&f(3.0)));
        m.copy_bus_extras("none", "to");
    }

    #[test]
    fn known_and_other_pins_read_back_by_name_and_by_index() {
        let mut m = BusMap::default();
        let (a, b) = (ALL_PINS[3].id, ALL_PINS[7].id);
        m.set_pin("bus", a, f(1.0));
        m.set_pin("bus", "key_f", f(2.0));
        assert_eq!(m.view("bus").at(3), Some(&f(1.0)));
        assert_eq!(m.get_pin("bus", "key_f"), Some(&f(2.0)));
        assert_eq!(m.view("bus").at(7), None);
        assert_eq!(m.view("none").at(3), None);
        assert_eq!(m.view("").at(3), None);
        let keys: Vec<_> = m.pins_of("bus").map(|(p, _)| p).collect();
        assert_eq!(keys, [a, "key_f"]);
        let id = m.bus_id("bus");
        m.set_at(id, 7, f(3.0));
        assert_eq!(m.get_pin("bus", b), Some(&f(3.0)));
        assert_eq!(m.at(id, 7), Some(f(3.0)));
    }

    #[test]
    fn pass_canonical_prefers_from_and_leaves_the_rest_alone() {
        let (x, y, z) = (ALL_PINS[0].id, ALL_PINS[1].id, ALL_PINS[2].id);
        let mut dev = BusMap::default();
        dev.set_pin("pad", x, f(1.0));
        dev.set_pin("pad", y, f(1.0));
        let mut m = BusMap::default();
        m.set_pin("from", x, f(2.0));
        m.set_pin("from", "key_f", f(2.0));
        m.set_pin("to", z, f(3.0));
        m.pass_canonical("to", "from", dev.view("pad"));
        assert_eq!(m.get_pin("to", x), Some(&f(2.0)), "from beats the fallback");
        assert_eq!(m.get_pin("to", y), Some(&f(1.0)), "the fallback fills in");
        assert_eq!(m.get_pin("to", z), Some(&f(3.0)), "what neither has stays");
        assert_eq!(m.get_pin("to", "key_f"), None, "canonical pins only");
    }

    #[test]
    fn fill_canonical_only_fills_what_is_missing() {
        let (x, y) = (ALL_PINS[0].id, ALL_PINS[1].id);
        let mut dev = BusMap::default();
        dev.set_pin("pad", x, f(1.0));
        dev.set_pin("pad", y, f(1.0));
        let mut m = BusMap::default();
        m.set_pin("to", x, f(2.0));
        m.fill_canonical("to", dev.view("pad"));
        assert_eq!(m.get_pin("to", x), Some(&f(2.0)));
        assert_eq!(m.get_pin("to", y), Some(&f(1.0)));
    }

    #[test]
    fn pin_sigs_hold_known_and_other_pins_and_come_back_clean() {
        let mut p = PinSigs::default();
        assert!(p.is_empty());
        p.insert("left_stick_up", f(1.0));
        p.insert("midi:ch1:cc1", f(2.0));
        p.insert(String::from("left_stick_up"), f(3.0));
        p.insert_if_absent("left_stick_up", f(9.0));
        assert_eq!(p.get("left_stick_up"), Some(&f(3.0)));
        assert_eq!(p.get("midi:ch1:cc1"), Some(&f(2.0)));
        assert_eq!(p.len(), 2);
        assert_eq!(p.clone().remove("left_stick_up"), Some(f(3.0)));
        assert_eq!(p.remove("midi:ch1:cc1"), Some(f(2.0)));
        drop(p);
        assert!(PinSigs::default().is_empty());
        let mut q = PinSigs::default();
        q.insert("right_stick_up", f(1.0));
        assert_eq!(q.get("left_stick_up"), None, "a spare table comes back empty");
    }

    #[test]
    fn pin_sigs_fill_from_two_buses_in_order_of_preference() {
        let (x, y) = (ALL_PINS[0].id, ALL_PINS[1].id);
        let mut a = BusMap::default();
        a.set_pin("c", x, f(1.0));
        let mut b = BusMap::default();
        b.set_pin("d", x, f(2.0));
        b.set_pin("d", y, f(2.0));
        let mut p = PinSigs::default();
        p.fill_canonical(a.view("c"), b.view("d"));
        assert_eq!(p.canonical(0), Some(&f(1.0)));
        assert_eq!(p.get(y), Some(&f(2.0)));
    }

    #[test]
    fn too_many_names_start_the_tables_over_at_a_clear() {
        let mut m = BusMap::default();
        for i in 0..=MAX_PINS {
            m.set_pin("a", &i.to_string(), f(1.0));
        }
        m.clear();
        assert!(m.pin_names.is_empty());
        m.set_pin("a", "x", f(2.0));
        assert_eq!(m.get_pin("a", "x"), Some(&f(2.0)));
    }
}
