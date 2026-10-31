//! Allocation-free lookups in maps keyed by `(device_id, pin_id)`.
//!
//! The signal maps (`dev_sigs`, the AutoMap bus, sink outputs) are keyed by
//! `(String, String)`. A plain `map.get(&(dev.to_string(), pin.to_string()))`
//! allocates two Strings just to look a value up — at the engine's sample rate
//! (default 2 kHz), with a hundred-odd pins per bus module, that allocation was
//! most of the engine's CPU time (an owned-key lookup measured ~4–10x a borrowed
//! one). [`PinMapExt`] looks the same entry up from two `&str`s instead.
//!
//! How: `(String, String)` borrows as `dyn PinKey`, whose `Hash` writes the two
//! strings exactly as the tuple's `Hash` does and whose `Eq` compares them, so a
//! `(&str, &str)` probe finds the owned key (the `Borrow` contract).

use std::borrow::Borrow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};

use crate::Signal;

/// The hasher for maps rebuilt or probed every engine tick. FxHash: these keys
/// are short, local and not attacker-controlled, and SipHash (std's default)
/// measured ~20% of the whole engine tick.
pub type FastHasher = rustc_hash::FxBuildHasher;

/// A `(device_id, pin_id) → Signal` map on [`FastHasher`] — the engine's bus,
/// its preprocessed device signals and its sink outputs.
pub type SigMap = HashMap<(String, String), Signal, FastHasher>;

// Spare `(String, String)` keys, so a map rebuilt every tick reuses last tick's
// key allocations instead of allocating two Strings per entry. Per thread: the
// engine runs on one. Filled by [`recycle_keys`], drawn by [`PinMapExt::set_pin`].
thread_local! {
    static KEY_POOL: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}
const KEY_POOL_CAP: usize = 1 << 15;

/// An owned key for `(dev, pin)`, reusing a pooled key's buffers when one is spare.
pub fn pooled_key(dev: &str, pin: &str) -> (String, String) {
    let spare = KEY_POOL.with(|p| p.borrow_mut().pop());
    match spare {
        Some((mut d, mut p)) => {
            d.clear();
            d.push_str(dev);
            p.clear();
            p.push_str(pin);
            (d, p)
        }
        None => (dev.to_string(), pin.to_string()),
    }
}

/// Empty `map`, keeping its table capacity and handing its keys to the pool.
pub fn recycle_keys<V, S>(map: &mut HashMap<(String, String), V, S>) {
    KEY_POOL.with(|p| {
        let mut p = p.borrow_mut();
        for (k, _) in map.drain() {
            if p.len() < KEY_POOL_CAP {
                p.push(k);
            }
        }
    });
}

/// A `(device_id, pin_id)` pair, owned or borrowed.
pub trait PinKey {
    fn dev(&self) -> &str;
    fn pin(&self) -> &str;
}

impl PinKey for (String, String) {
    fn dev(&self) -> &str { &self.0 }
    fn pin(&self) -> &str { &self.1 }
}

impl PinKey for (&str, &str) {
    fn dev(&self) -> &str { self.0 }
    fn pin(&self) -> &str { self.1 }
}

impl<'a> Borrow<dyn PinKey + 'a> for (String, String) {
    fn borrow(&self) -> &(dyn PinKey + 'a) { self }
}

// Must hash exactly like `(String, String)`: a tuple hashes its fields in
// order, and `String` hashes as its `str`.
impl Hash for dyn PinKey + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.dev().hash(state);
        self.pin().hash(state);
    }
}

impl PartialEq for dyn PinKey + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.dev() == other.dev() && self.pin() == other.pin()
    }
}

impl Eq for dyn PinKey + '_ {}

/// `get` / `contains_key` / `get_mut` / `remove` by `(&str, &str)`, without
/// building an owned key.
pub trait PinMapExt<V> {
    fn get_pin(&self, dev: &str, pin: &str) -> Option<&V>;
    fn has_pin(&self, dev: &str, pin: &str) -> bool;
    fn get_pin_mut(&mut self, dev: &str, pin: &str) -> Option<&mut V>;
    fn remove_pin(&mut self, dev: &str, pin: &str) -> Option<V>;
    /// Insert or overwrite. A new key comes from the key pool when it has one.
    fn set_pin(&mut self, dev: &str, pin: &str, v: V);
    /// `entry((dev, pin)).or_insert(v)`, without building the key when present.
    fn set_pin_if_absent(&mut self, dev: &str, pin: &str, v: V);
    /// `entry((dev, pin)).and_modify(merge).or_insert(v)`, without building the
    /// key when present.
    fn merge_pin<F: FnOnce(&mut V)>(&mut self, dev: &str, pin: &str, v: V, merge: F);
}

impl<V, S: BuildHasher> PinMapExt<V> for HashMap<(String, String), V, S> {
    #[inline]
    fn get_pin(&self, dev: &str, pin: &str) -> Option<&V> {
        self.get(&(dev, pin) as &dyn PinKey)
    }
    #[inline]
    fn has_pin(&self, dev: &str, pin: &str) -> bool {
        self.contains_key(&(dev, pin) as &dyn PinKey)
    }
    #[inline]
    fn get_pin_mut(&mut self, dev: &str, pin: &str) -> Option<&mut V> {
        self.get_mut(&(dev, pin) as &dyn PinKey)
    }
    #[inline]
    fn remove_pin(&mut self, dev: &str, pin: &str) -> Option<V> {
        self.remove(&(dev, pin) as &dyn PinKey)
    }
    #[inline]
    fn set_pin(&mut self, dev: &str, pin: &str, v: V) {
        match self.get_mut(&(dev, pin) as &dyn PinKey) {
            Some(slot) => *slot = v,
            None => { self.insert(pooled_key(dev, pin), v); }
        }
    }
    #[inline]
    fn set_pin_if_absent(&mut self, dev: &str, pin: &str, v: V) {
        if !self.contains_key(&(dev, pin) as &dyn PinKey) {
            self.insert(pooled_key(dev, pin), v);
        }
    }
    #[inline]
    fn merge_pin<F: FnOnce(&mut V)>(&mut self, dev: &str, pin: &str, v: V, merge: F) {
        match self.get_mut(&(dev, pin) as &dyn PinKey) {
            Some(slot) => merge(slot),
            None => { self.insert(pooled_key(dev, pin), v); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_probe_finds_owned_key() {
        let mut m: HashMap<(String, String), i32> = HashMap::new();
        m.insert(("gilrs:0".into(), "btn_a".into()), 1);
        m.insert(("gilrs:0".into(), "btn_b".into()), 2);
        m.insert(("collector:3".into(), "btn_a".into()), 3);
        assert_eq!(m.get_pin("gilrs:0", "btn_a"), Some(&1));
        assert_eq!(m.get_pin("collector:3", "btn_a"), Some(&3));
        assert!(m.has_pin("gilrs:0", "btn_b"));
        // The split point matters: ("gilrs:0b", "tn_a") is a different key.
        assert!(!m.has_pin("gilrs:0b", "tn_a"));
        assert!(!m.has_pin("gilrs:0", "btn_ab"));
        *m.get_pin_mut("gilrs:0", "btn_a").unwrap() = 9;
        assert_eq!(m.remove_pin("gilrs:0", "btn_a"), Some(9));
        assert!(!m.has_pin("gilrs:0", "btn_a"));
        m.set_pin("gilrs:0", "btn_b", 5);
        m.set_pin("gilrs:1", "btn_b", 6);
        assert_eq!(m.get(&("gilrs:0".to_string(), "btn_b".to_string())), Some(&5));
        assert_eq!(m.get(&("gilrs:1".to_string(), "btn_b".to_string())), Some(&6));
    }

    #[test]
    fn recycled_keys_come_back_clean() {
        let mut m: SigMap = SigMap::default();
        m.set_pin("collector:12", "left_stick_x", Signal::Float(1.0));
        m.set_pin("remap:3", "btn_south", Signal::Bool(true));
        recycle_keys(&mut m);
        assert!(m.is_empty());
        // Shorter and longer names than the pooled buffers held.
        m.set_pin("c:1", "a", Signal::Float(2.0));
        m.set_pin("collector:123456", "right_trigger_long_name", Signal::Float(3.0));
        m.set_pin_if_absent("c:1", "a", Signal::Float(9.0));
        m.set_pin_if_absent("c:1", "b", Signal::Float(4.0));
        let mut keys: Vec<_> = m.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, [
            ("c:1".to_string(), "a".to_string()),
            ("c:1".to_string(), "b".to_string()),
            ("collector:123456".to_string(), "right_trigger_long_name".to_string()),
        ]);
        assert_eq!(m.get_pin("c:1", "a"), Some(&Signal::Float(2.0)));
    }

    #[test]
    fn works_with_any_hasher() {
        use std::hash::BuildHasherDefault;
        use std::collections::hash_map::DefaultHasher;
        let mut m: HashMap<(String, String), i32, BuildHasherDefault<DefaultHasher>> = HashMap::default();
        for i in 0..200 {
            m.insert((format!("dev{}", i % 7), format!("pin{i}")), i);
        }
        for i in 0..200 {
            assert_eq!(m.get_pin(&format!("dev{}", i % 7), &format!("pin{i}")), Some(&i));
        }
    }
}
