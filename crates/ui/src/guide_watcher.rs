//! Background gamepad-shortcut chord watcher, driven by the shared
//! `proc_device_signals` map.
//!
//! This thread is the single engine for gamepad shortcuts, so they fire even
//! while a *game* holds focus. It runs on its own thread and reads the same
//! signal map the I/O thread publishes, detecting the user's assigned chords
//! regardless of which window is foreground.
//!
//! The "only in gamepad navigation" setting scopes WHICH pad may fire the four
//! window shortcuts (see-through / panic / info-overlay / pin), via
//! [`ChordWatchConfig::nav_only`] + the nav-device set the UI publishes:
//!  * OFF — they fire from ANY connected pad, unconditionally.
//!  * ON  — they fire only from a pad currently selected for UI navigation; if
//!    none is selected, they don't fire.
//! The **config-overlay** chord ignores the setting and always fires from any
//! pad (its whole purpose is to be summonable mid-game).
//!
//! Why the shared map and not a private `Gilrs` instance: DualSense's PS button
//! and Switch's Home aren't exposed through gilrs's standard XInput/HID mapping
//! on Windows — only through the raw HID parsing the I/O thread does in
//! `flexinput-devices::gilrs_backend`, which publishes to `proc_device_signals`.
//! Reading the same map gets correct detection for every surfaced controller.
//!
//! Detection: a chord is "held" when every button in its combo is pressed on
//! ONE non-virtual device (FlexInput's own virtual pads `gilrs:<kind>:v<N>` are
//! excluded — a mapped press loops back through them and would double-fire).
//! Firing follows the configured press mode via the shared
//! [`crate::gamepad_nav::chord_fire`] helper, so it matches the in-app shortcut
//! semantics exactly (`down` / `long` / `double` + `gap_ms`). A startup grace
//! window and a per-target post-fire refractory absorb BT-handshake noise and
//! duplicate-device echoes.
//!
//! Shortcuts are EXCLUSIVE, in two senses:
//!  * Against each other: when one assigned combo is a strict subset of
//!    another (Home vs Home+D-pad), holding the larger one cancels the smaller
//!    for the rest of that hold, and an `On press` subset fires on RELEASE
//!    instead (it can't know at the press edge whether a larger chord follows).
//!  * Against the game and FlexInput's own nav: the buttons a shortcut is using
//!    are published as an owned `(device, pin)` set ([`owned_pins`]). The engine
//!    zeroes them before every tick and gamepad nav masks them out, each until
//!    that button is released.
//!
//! Leaders ([`LEADER_PINS`]: Home, Capture) are HELD BACK rather than simply
//! withheld: a leader press no shortcut ended up using is replayed to the game
//! as a short tap on release ([`Leaders`]), unless the user gave Home to
//! FlexInput outright ([`ChordWatchConfig::home_exclusive`]).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use flexinput_core::Signal;

use crate::gamepad_nav::{chord_fire, ChordFireState};

/// System buttons allowed to bind a shortcut ON THEIR OWN (no combo): the
/// Guide/PS/Home button and the Capture / Mic-mute button.
pub const STANDALONE_CHORD_PINS: &[&str] = &["btn_guide", "btn_mute", "btn_capture"];

/// Buttons a shortcut LEADS with. One that belongs to an assigned shortcut is
/// held back from the game whenever it's pressed, and while it's held the rest
/// of its combos' buttons are too — so Home+D-pad never leaks the D-pad press.
/// A press no shortcut used is replayed on release (see [`Leaders`]).
///
/// ❗ Not Mic-mute, though it may bind alone: games bind it natively, so it is
/// an ordinary chord button — withheld only once its whole chord is held.
const LEADER_PINS: &[&str] = &["btn_guide", "btn_capture"];

/// How long a replayed leader tap is held on the game's side. Several frames at
/// any game's rate; the watcher's poll stretches it by up to one interval.
const REPLAY_PULSE: Duration = Duration::from_millis(100);

/// One assigned shortcut chord: the buttons plus its press mode / time gap.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShortcutSpec {
    /// The chord buttons (canonical pin ids, e.g. `["btn_lb", "btn_rb"]`), or a
    /// single allowed button (`btn_guide` / `btn_capture`).
    pub combo: Vec<String>,
    /// Press mode: `"down"` | `"long"` | `"double"` (see `chord_fire`).
    pub mode: String,
    /// Time gap (ms) the mode reads (hold time / inter-tap gap).
    pub gap_ms: f32,
}

/// Live config for the watcher, shared with the UI thread (which republishes it
/// whenever a binding changes). A `None` target is unassigned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChordWatchConfig {
    /// When true, the four window shortcuts (all but config overlay) fire only
    /// from a gamepad currently selected for UI navigation — restricted to the
    /// device set the UI publishes. When no nav device is selected they don't
    /// fire at all. The config-overlay chord ignores this and fires from any pad.
    pub nav_only: bool,
    /// When true, a Home press no shortcut used is swallowed instead of
    /// replayed to the game. Home only — Capture is always replayed.
    pub home_exclusive: bool,
    pub seethrough: Option<ShortcutSpec>,
    pub panic: Option<ShortcutSpec>,
    pub overlay: Option<ShortcutSpec>,
    pub pin: Option<ShortcutSpec>,
    pub config: Option<ShortcutSpec>,
}

/// The five toggle flags the watcher raises; the UI loop consumes each once per
/// frame and applies the corresponding action. Shared (each is its own `Arc`)
/// with the keyboard-hotkey listeners, which raise the same flags.
#[derive(Clone)]
pub struct ShortcutToggles {
    pub seethrough: Arc<AtomicBool>,
    pub panic: Arc<AtomicBool>,
    pub overlay: Arc<AtomicBool>,
    pub pin: Arc<AtomicBool>,
    pub config: Arc<AtomicBool>,
}

const POLL_INTERVAL: Duration = Duration::from_millis(33); // ~30 Hz
/// Post-fire refractory. A physical press mapped through to a virtual pad can
/// come back as a second edge on another device a poll or two later (and this
/// machine has been seen surfacing outright duplicate pads); the window absorbs
/// that echo. Longer than any loopback latency, shorter than a human re-toggle.
const TOGGLE_REFRACTORY_MS: u64 = 400;
/// Grace window after the thread starts before any fire is honored. Covers a
/// controller present at launch whose BT handshake / first raw-HID reports can
/// momentarily latch buttons true.
const STARTUP_GRACE_MS: u64 = 750;

/// Per-target edge/timing state kept across poll iterations.
#[derive(Default)]
struct TargetWatch {
    fire_state: ChordFireState,
    last_toggle: Option<Instant>,
    /// A larger shortcut containing this combo was held during the current
    /// hold, so the press is its, not ours. Cleared on release.
    cancelled: bool,
}

type SignalMap = HashMap<(String, String), Signal>;

/// Which physical controller a device id belongs to, for chord purposes.
///
/// ⭐ **A split controller is two devices and one pad.** Joy-Con 2 halves
/// enumerate separately — they connect separately, and either can be used
/// alone — so the left half publishes Minus, L and the D-pad while the right
/// publishes Home, Plus and the face buttons. Nothing links them.
///
/// ❗ That made every cross-half chord impossible. `combo_held` requires ONE
/// device to hold the whole combo, which is right for keeping two PLAYERS' pads
/// from jointly firing a shortcut — but a grip is one player, and Home lives on
/// the right half while most of what anyone would pair it with lives on the
/// left. The shortcut simply never fired, with nothing to see anywhere.
///
/// Grouping the halves restores the intent: one player, one pad. Two grips in
/// the room would share a group, since the halves carry no pairing information
/// to tell one grip from another — worth knowing, and still strictly better
/// than a chord that cannot fire at all.
fn chord_group(dev: &str) -> &str {
    // Only SPLIT controllers group. A classic pad is one device and one
    // controller, so it must not share a group with anything.
    if dev.starts_with("jc2:") { "jc2" } else { dev }
}

/// True when one physical controller holds every button in `combo`.
/// When `device_filter` is `Some`, only devices in that set are considered (used
/// to restrict the four window shortcuts to the nav-selected pads).
fn combo_held(snap: &SignalMap, combo: &[String], device_filter: Option<&HashSet<String>>) -> bool {
    if combo.is_empty() { return false; }
    held_by_group(snap, combo, device_filter)
        .values()
        .any(|held| held.len() >= combo.len())
}

/// Per controller group, which of `combo`'s buttons are currently held.
/// ❗ The set of BUTTON IDS, not a count: the two halves of a grip both publish
/// `btn_sl`, so counting hits would let one button pressed on both halves
/// satisfy a two-button chord.
fn held_by_group<'a>(
    snap: &'a SignalMap,
    combo: &[String],
    device_filter: Option<&HashSet<String>>,
) -> HashMap<&'a str, HashSet<&'a str>> {
    let mut per_group: HashMap<&str, HashSet<&str>> = HashMap::new();
    for ((dev, sig), val) in snap.iter() {
        if !device_eligible(dev, device_filter) { continue; }
        if !matches!(val, Signal::Bool(true)) { continue; }
        if combo.iter().any(|p| p == sig) {
            per_group.entry(chord_group(dev)).or_default().insert(sig.as_str());
        }
    }
    per_group
}

/// A physical pad that may drive shortcuts: never FlexInput's own virtual pads,
/// and only the nav-selected ones when `device_filter` is set.
fn device_eligible(dev: &str, device_filter: Option<&HashSet<String>>) -> bool {
    if crate::app::is_own_virtual_gilrs_id(dev) { return false; }
    device_filter.map_or(true, |f| f.contains(dev))
}

/// True when `big` holds every button of `small` plus at least one more.
fn strict_superset(big: &[String], small: &[String]) -> bool {
    big.len() > small.len() && small.iter().all(|p| big.contains(p))
}

/// Evaluate one target: if its chord fires (past grace + refractory), raise the
/// toggle flag. Resets edge state when the target is unassigned.
///
/// `superset_assigned` = some other assigned shortcut strictly contains this
/// combo; `superset_held` = one of those is fully held right now. The first
/// moves an `On press` shortcut to fire on release; the second cancels this
/// shortcut for the rest of the hold.
///
/// Returns true when the chord fired this poll — even inside the startup grace
/// or the refractory, where the flag isn't raised: the press was still meant
/// for a shortcut, so a leader in it must not be replayed to the game.
fn service_target(
    tw: &mut TargetWatch,
    spec: &Option<ShortcutSpec>,
    toggle: &AtomicBool,
    held: bool,
    superset_assigned: bool,
    superset_held: bool,
    started_at: Instant,
) -> bool {
    let Some(spec) = spec else {
        // Unassigned (or handled elsewhere): clear state so a later re-assign
        // starts from a clean edge instead of swallowing the first press.
        *tw = TargetWatch::default();
        return false;
    };
    if spec.combo.is_empty() { return false; }
    let now = Instant::now();
    // Home long-press must not fire because Home was the first half of
    // Home+D-pad: the larger chord owns the whole hold.
    if held && superset_held { tw.cancelled = true; }
    // ❗ An `On press` subset can't fire at the press edge — the larger chord
    // may still be on its way — so it waits for the release and fires only if
    // nothing larger was held in between.
    let defer_to_release = superset_assigned
        && !matches!(spec.mode.as_str(), "long" | "double");
    let was_down = tw.fire_state.down;
    let fired = chord_fire(&mut tw.fire_state, held, &spec.mode, spec.gap_ms, now);
    let cancelled = tw.cancelled;
    if cancelled { tw.fire_state.cancel_hold(); }
    if !held { tw.cancelled = false; }
    let fire = if defer_to_release { was_down && !held } else { fired };
    if fire && !cancelled {
        let in_grace = now.duration_since(started_at)
            < Duration::from_millis(STARTUP_GRACE_MS);
        let in_refractory = tw.last_toggle
            .map(|t| now.duration_since(t) < Duration::from_millis(TOGGLE_REFRACTORY_MS))
            .unwrap_or(false);
        if !in_grace && !in_refractory {
            toggle.store(true, Ordering::Relaxed);
            tw.last_toggle = Some(now);
        }
    }
    fire && !cancelled
}

/// One assigned shortcut with the pads allowed to drive it (`None` = any pad).
type Target<'a> = (&'a Option<ShortcutSpec>, Option<&'a HashSet<String>>);

/// The `(device, pin)` pairs the shortcuts own this poll — withheld from the
/// game (engine source-block) and from FlexInput's gamepad nav.
///
/// Per shortcut, per controller group:
///  * a leader ([`LEADER_PINS`]) in its combo is owned always — held back;
///    [`Leaders`] decides whether the game gets it replayed;
///  * while a leader is held, or the whole combo is, every button of the combo
///    is owned — the leader rule is what catches the D-pad of Home+D-pad before
///    it lands, since the watcher polls slower than the engine ticks.
/// Plus anything owned last poll that is still pressed: ownership ends at the
/// button's own release, not the chord's, or letting go of Home first would
/// hand a still-held D-pad to the game mid-press.
///
/// Only keys the snapshot actually has are owned (blocking a key inserts it
/// into the engine's view). Owning a digital D-pad direction also owns the
/// pad's hat pins, which carry the same press on Switch / PlayStation pads.
fn owned_pins(
    snap: &SignalMap,
    targets: &[Target],
    prev: &HashSet<(String, String)>,
) -> HashSet<(String, String)> {
    let mut out: HashSet<(String, String)> = HashSet::new();
    let mut own = |dev: &str, pin: &str| {
        let key = (dev.to_string(), pin.to_string());
        if snap.contains_key(&key) { out.insert(key); }
    };
    for &(spec, filter) in targets {
        let Some(spec) = spec else { continue };
        if spec.combo.is_empty() { continue; }
        let held = held_by_group(snap, &spec.combo, filter);
        let devs: HashSet<&str> = snap.keys()
            .map(|(d, _)| d.as_str())
            .filter(|d| device_eligible(d, filter))
            .collect();
        for dev in devs {
            let group_held = held.get(chord_group(dev));
            let leader_held = group_held.map_or(false, |h|
                h.iter().any(|p| LEADER_PINS.contains(p)));
            let whole_held = group_held.map_or(false, |h| h.len() >= spec.combo.len());
            for pin in &spec.combo {
                if leader_held || whole_held || LEADER_PINS.contains(&pin.as_str()) {
                    own(dev, pin);
                }
            }
        }
    }
    for (dev, pin) in prev {
        if matches!(snap.get(&(dev.clone(), pin.clone())), Some(Signal::Bool(true))) {
            own(dev, pin);
        }
    }
    let dpads: Vec<String> = out.iter()
        .filter(|(_, p)| p.starts_with("dpad_") && p != "dpad_x" && p != "dpad_y")
        .map(|(d, _)| d.clone())
        .collect();
    for dev in dpads {
        for hat in ["dpad", "dpad_x", "dpad_y"] {
            let key = (dev.clone(), hat.to_string());
            if snap.contains_key(&key) { out.insert(key); }
        }
    }
    out
}

type Keys = HashSet<(String, String)>;

/// Hold-back/replay state of one leader button on one controller group.
#[derive(Default)]
struct LeaderHold {
    /// Held at the last poll.
    held: bool,
    /// A shortcut used this hold, so the game doesn't get it.
    used: bool,
    /// The leader keys pressed during this hold — where a replay goes.
    keys: Keys,
    /// A replay waiting out a double-tap window: when it starts, and where.
    pending: Option<(Instant, Keys)>,
    /// A replay on the game's side right now: until when, and where.
    playing: Option<(Instant, Keys)>,
}

/// Hold-back/replay for the leaders. A leader is always withheld from the game
/// ([`owned_pins`]); this decides when the game gets it back as a tap.
///
/// A hold is USED when, between its press and release, a shortcut containing
/// the leader fired, or a multi-button shortcut containing it was fully held on
/// that controller (that covers a cancelled long-press too — the larger chord
/// took it). An unused hold is replayed as a [`REPLAY_PULSE`] tap on release.
/// A double-tap shortcut on the leader alone delays the replay by its gap, and
/// a new press inside that window drops it: that tap was the double's first.
#[derive(Default)]
struct Leaders {
    holds: HashMap<(String, &'static str), LeaderHold>,
}

impl Leaders {
    /// Advance one poll. `fired[i]` = `targets[i]` fired this poll. Returns the
    /// leader keys the game should see pressed right now.
    fn update(
        &mut self,
        snap: &SignalMap,
        targets: &[Target],
        fired: &[bool],
        home_exclusive: bool,
        now: Instant,
    ) -> Keys {
        let mut out = Keys::new();
        for &leader in LEADER_PINS {
            let with_leader: Vec<(usize, &ShortcutSpec, Option<&HashSet<String>>)> = targets.iter()
                .enumerate()
                .filter_map(|(i, &(s, f))| s.as_ref().map(|s| (i, s, f)))
                .filter(|(_, s, _)| s.combo.iter().any(|p| p == leader))
                .collect();
            if with_leader.is_empty() {
                // No longer a shortcut button: it isn't held back either.
                self.holds.retain(|(_, l), _| *l != leader);
                continue;
            }
            let fired_any = with_leader.iter().any(|&(i, _, _)| fired[i]);
            let double_wait = with_leader.iter()
                .filter(|(_, s, _)| s.mode == "double" && s.combo.len() == 1)
                .map(|(_, s, _)| Duration::from_millis(s.gap_ms.max(0.0) as u64))
                .max()
                .unwrap_or_default();

            let mut pressed: HashMap<String, Keys> = HashMap::new();
            for ((dev, pin), val) in snap.iter() {
                if pin != leader || !matches!(val, Signal::Bool(true)) { continue; }
                if !with_leader.iter().any(|&(_, _, f)| device_eligible(dev, f)) { continue; }
                pressed.entry(chord_group(dev).to_string()).or_default()
                    .insert((dev.clone(), pin.clone()));
            }
            let chord_held_on = |group: &str| with_leader.iter()
                .filter(|(_, s, _)| s.combo.len() > 1)
                .any(|(_, s, f)| held_by_group(snap, &s.combo, *f)
                    .get(group).map_or(false, |h| h.len() >= s.combo.len()));

            let mut groups: HashSet<String> = pressed.keys().cloned().collect();
            groups.extend(self.holds.keys().filter(|(_, l)| *l == leader).map(|(g, _)| g.clone()));
            for group in groups {
                let h = self.holds.entry((group.clone(), leader)).or_default();
                let now_pressed = pressed.remove(&group);
                let held = now_pressed.is_some();
                if held && !h.held {
                    h.used = false;
                    h.keys.clear();
                    h.pending = None;
                }
                if let Some(keys) = now_pressed { h.keys.extend(keys); }
                if (held || h.held) && (fired_any || chord_held_on(&group)) {
                    h.used = true;
                }
                if !held && h.held && !h.used && !(leader == "btn_guide" && home_exclusive) {
                    h.pending = Some((now + double_wait, std::mem::take(&mut h.keys)));
                }
                h.held = held;
                if h.pending.as_ref().map_or(false, |(at, _)| *at <= now) {
                    let (_, keys) = h.pending.take().unwrap();
                    h.playing = Some((now + REPLAY_PULSE, keys));
                }
                if let Some((until, keys)) = &h.playing {
                    if *until > now { out.extend(keys.iter().cloned()); } else { h.playing = None; }
                }
            }
        }
        self.holds.retain(|_, h| h.held || h.pending.is_some() || h.playing.is_some());
        out
    }
}

pub fn spawn_chord_watcher(
    config: Arc<RwLock<ChordWatchConfig>>,
    toggles: ShortcutToggles,
    // Devices currently selected for UI navigation, republished by the UI each
    // frame. Consulted only when `nav_only` is set.
    nav_devices: Arc<RwLock<HashSet<String>>>,
    proc_device_signals: flexinput_engine::ArcSignals,
    // Where the owned buttons are published: the engine blocks them from the
    // game, gamepad nav masks them. See [`owned_pins`].
    owned_out: flexinput_engine::UiSourceBlock,
    // Leader taps being replayed: the engine shows the game these as pressed.
    // See [`Leaders`].
    replay_out: flexinput_engine::UiSourceBlock,
) {
    std::thread::Builder::new()
        .name("gamepad-shortcut-watcher".into())
        .spawn(move || {
            let started_at = Instant::now();
            let mut watches: [TargetWatch; 5] = Default::default();
            let mut owned: Keys = Keys::new();
            let mut leaders = Leaders::default();
            let mut replay: Keys = Keys::new();

            loop {
                let cfg = config.read().map(|c| c.clone()).unwrap_or_default();

                // Snapshot the signal map. With the ArcSwap publish model the
                // load is a refcount bump and iteration walks the snapshot
                // without contending the I/O thread.
                let snap = proc_device_signals.load_full();

                // The four window shortcuts restrict to the nav-selected pads
                // when nav-only is on; the config chord always fires from any.
                let nav_set = nav_devices.read().map(|s| s.clone()).unwrap_or_default();
                let window_filter = if cfg.nav_only { Some(&nav_set) } else { None };

                let targets: [Target; 5] = [
                    (&cfg.seethrough, window_filter),
                    (&cfg.panic, window_filter),
                    (&cfg.overlay, window_filter),
                    (&cfg.pin, window_filter),
                    (&cfg.config, None),
                ];
                let flags = [&toggles.seethrough, &toggles.panic, &toggles.overlay,
                    &toggles.pin, &toggles.config];
                let combos: [Option<&[String]>; 5] = targets.map(|(s, _)|
                    s.as_ref().map(|s| s.combo.as_slice()).filter(|c| !c.is_empty()));
                let held: [bool; 5] = std::array::from_fn(|i| combos[i]
                    .map_or(false, |c| combo_held(&snap, c, targets[i].1)));

                let mut fired = [false; 5];
                for i in 0..5 {
                    let mut superset_assigned = false;
                    let mut superset_held = false;
                    if let Some(small) = combos[i] {
                        for j in (0..5).filter(|&j| j != i) {
                            if combos[j].map_or(false, |big| strict_superset(big, small)) {
                                superset_assigned = true;
                                superset_held |= held[j];
                            }
                        }
                    }
                    fired[i] = service_target(&mut watches[i], targets[i].0, flags[i], held[i],
                        superset_assigned, superset_held, started_at);
                }

                // The leader stays in `owned` while it's replayed; the engine
                // lets a replayed key through its shortcut block.
                let now_replay = leaders.update(&snap, &targets, &fired,
                    cfg.home_exclusive, Instant::now());
                let now_owned = owned_pins(&snap, &targets, &owned);
                if now_replay != replay {
                    if let Ok(mut g) = replay_out.write() {
                        *g = now_replay.clone();
                    }
                    replay = now_replay;
                }
                if now_owned != owned {
                    if let Ok(mut g) = owned_out.write() {
                        *g = now_owned.clone();
                    }
                    owned = now_owned;
                }

                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("failed to spawn gamepad-shortcut-watcher thread");
}

#[cfg(test)]
mod chord_tests {
    use super::*;

    fn snap(entries: &[(&str, &str, bool)]) -> SignalMap {
        entries
            .iter()
            .map(|(d, p, v)| ((d.to_string(), p.to_string()), Signal::Bool(*v)))
            .collect()
    }

    fn combo(pins: &[&str]) -> Vec<String> {
        pins.iter().map(|s| s.to_string()).collect()
    }

    /// ⛔ A chord spanning the two halves of one grip must fire.
    ///
    /// Home is on the right half; almost anything worth pairing it with is on
    /// the left. Requiring a single device meant these chords could never fire,
    /// and produced no diagnostic of any kind — the shortcut was simply inert.
    #[test]
    fn a_chord_across_the_halves_of_one_grip_fires() {
        let s = snap(&[
            ("jc2:joycon2_r:aabb", "btn_guide", true),
            ("jc2:joycon2_l:ccdd", "btn_back", true),
        ]);
        assert!(combo_held(&s, &combo(&["btn_guide", "btn_back"]), None));
    }

    /// …and it still must not fire from one half alone.
    #[test]
    fn one_half_alone_does_not_satisfy_a_two_button_chord() {
        let s = snap(&[("jc2:joycon2_r:aabb", "btn_guide", true)]);
        assert!(!combo_held(&s, &combo(&["btn_guide", "btn_back"]), None));
    }

    /// ⛔ The SAME button id held on two grouped devices is one button, not
    /// two.
    ///
    /// Grouping means a tally of HITS would let one button pressed on both
    /// halves satisfy a two-button chord. Counting distinct ids is what stops
    /// that. The Joy-Con halves happen to share no button id today — the rail
    /// buttons take per-side paddle ids — but the grouping is what makes this
    /// reachable at all, so the rule belongs with it rather than with whichever
    /// pin list happens to overlap.
    #[test]
    fn the_same_button_on_two_grouped_devices_is_not_two_buttons() {
        let s = snap(&[
            ("jc2:joycon2_l:ccdd", "btn_guide", true),
            ("jc2:joycon2_r:aabb", "btn_guide", true),
        ]);
        assert!(!combo_held(&s, &combo(&["btn_guide", "btn_back"]), None));
    }

    /// ⛔ Two separate pads must still not jointly fire a chord — that is the
    /// rule the grouping had to be careful not to dissolve.
    #[test]
    fn two_different_controllers_still_cannot_share_a_chord() {
        let s = snap(&[
            ("gilrs:xinput:0", "btn_guide", true),
            ("gilrs:xinput:1", "btn_back", true),
        ]);
        assert!(!combo_held(&s, &combo(&["btn_guide", "btn_back"]), None));
    }

    fn spec(pins: &[&str], mode: &str) -> Option<ShortcutSpec> {
        Some(ShortcutSpec { combo: combo(pins), mode: mode.into(), gap_ms: 1.0 })
    }

    /// Past the startup grace, so only the rules under test decide.
    fn long_ago() -> Instant {
        Instant::now() - Duration::from_secs(10)
    }

    /// ⛔ Home long-press must not fire when Home was the first half of
    /// Home+D-pad — the reported bug: holding long enough did both.
    #[test]
    fn a_larger_chord_cancels_the_long_press_inside_it() {
        let (mut tw, flag, t0) = (TargetWatch::default(), AtomicBool::new(false), long_ago());
        let home = spec(&["btn_guide"], "long");
        service_target(&mut tw, &home, &flag, true, true, false, t0);
        service_target(&mut tw, &home, &flag, true, true, true, t0);
        std::thread::sleep(Duration::from_millis(5));
        // D-pad let go, Home still held past the threshold: still the chord's.
        service_target(&mut tw, &home, &flag, true, true, false, t0);
        assert!(!flag.load(Ordering::Relaxed));
        // A fresh hold of Home alone works again.
        service_target(&mut tw, &home, &flag, false, true, false, t0);
        service_target(&mut tw, &home, &flag, true, true, false, t0);
        std::thread::sleep(Duration::from_millis(5));
        service_target(&mut tw, &home, &flag, true, true, false, t0);
        assert!(flag.load(Ordering::Relaxed));
    }

    /// An `On press` shortcut with a larger chord built on it waits for the
    /// release, and fires there only when the larger chord never happened.
    #[test]
    fn an_on_press_subset_fires_on_release_unless_the_larger_chord_happened() {
        let (mut tw, flag, t0) = (TargetWatch::default(), AtomicBool::new(false), long_ago());
        let home = spec(&["btn_guide"], "down");
        service_target(&mut tw, &home, &flag, true, true, false, t0);
        assert!(!flag.load(Ordering::Relaxed), "must not fire at the press edge");
        service_target(&mut tw, &home, &flag, false, true, false, t0);
        assert!(flag.load(Ordering::Relaxed));

        let (mut tw, flag) = (TargetWatch::default(), AtomicBool::new(false));
        service_target(&mut tw, &home, &flag, true, true, false, t0);
        service_target(&mut tw, &home, &flag, true, true, true, t0);
        service_target(&mut tw, &home, &flag, false, true, false, t0);
        assert!(!flag.load(Ordering::Relaxed));
    }

    /// Without a larger chord around, `On press` still fires at the press.
    #[test]
    fn an_on_press_shortcut_with_nothing_larger_fires_at_the_press() {
        let (mut tw, flag, t0) = (TargetWatch::default(), AtomicBool::new(false), long_ago());
        service_target(&mut tw, &spec(&["btn_guide"], "down"), &flag, true, false, false, t0);
        assert!(flag.load(Ordering::Relaxed));
    }

    fn key(d: &str, p: &str) -> (String, String) {
        (d.to_string(), p.to_string())
    }

    /// The leader is withheld outright; while it's held its chord's other
    /// buttons are too (hat included), and each stays owned until released.
    #[test]
    fn a_held_leader_owns_its_chord_until_each_button_is_released() {
        let pin = spec(&["btn_guide", "dpad_up"], "down");
        let targets: [Target; 1] = [(&pin, None)];
        let pad = "gilrs:switch:0";
        let mut s = snap(&[(pad, "btn_guide", false), (pad, "dpad_up", false), (pad, "btn_south", false)]);
        s.insert(key(pad, "dpad"), Signal::Vec2(Default::default()));

        let idle = owned_pins(&s, &targets, &HashSet::new());
        assert_eq!(idle, HashSet::from([key(pad, "btn_guide")]));

        s.insert(key(pad, "btn_guide"), Signal::Bool(true));
        let leading = owned_pins(&s, &targets, &idle);
        assert!(leading.contains(&key(pad, "dpad_up")), "D-pad owned before it's pressed");
        assert!(leading.contains(&key(pad, "dpad")), "hat follows the digital direction");
        assert!(!leading.contains(&key(pad, "btn_south")));

        // Home let go first, D-pad still held: still withheld.
        s.insert(key(pad, "dpad_up"), Signal::Bool(true));
        s.insert(key(pad, "btn_guide"), Signal::Bool(false));
        let trailing = owned_pins(&s, &targets, &leading);
        assert!(trailing.contains(&key(pad, "dpad_up")));

        s.insert(key(pad, "dpad_up"), Signal::Bool(false));
        assert_eq!(owned_pins(&s, &targets, &trailing), idle);
    }

    /// Mic-mute may bind alone but games bind it natively: it's no leader, so
    /// it isn't withheld while idle and never replayed.
    #[test]
    fn mic_mute_is_not_held_back() {
        let mic = spec(&["btn_mute"], "long");
        let targets: [Target; 1] = [(&mic, None)];
        let s = snap(&[("gilrs:ps5:0", "btn_mute", false)]);
        assert!(owned_pins(&s, &targets, &HashSet::new()).is_empty());
    }

    const PAD: &str = "gilrs:switch:0";

    fn home(pressed: bool, dpad: bool) -> SignalMap {
        snap(&[(PAD, "btn_guide", pressed), (PAD, "dpad_up", dpad)])
    }

    /// Run `Leaders` over `(snapshot, fired, ms since start)` polls; returns
    /// whether Home was being replayed after each one.
    fn replays(targets: &[Target], exclusive: bool, polls: &[(SignalMap, bool, u64)]) -> Vec<bool> {
        let mut l = Leaders::default();
        let t0 = Instant::now();
        polls.iter().map(|(s, fired, ms)| {
            let fired = vec![*fired; targets.len()];
            let out = l.update(s, targets, &fired, exclusive, t0 + Duration::from_millis(*ms));
            out.contains(&key(PAD, "btn_guide"))
        }).collect()
    }

    /// A Home tap no shortcut used reaches the game on release, as a tap.
    #[test]
    fn an_unused_home_press_is_replayed_as_a_tap() {
        let cfg = spec(&["btn_guide"], "long");
        let targets: [Target; 1] = [(&cfg, None)];
        let r = replays(&targets, false, &[
            (home(true, false), false, 0),
            (home(false, false), false, 50),
            (home(false, false), false, 100),
            (home(false, false), false, 50 + REPLAY_PULSE.as_millis() as u64),
        ]);
        assert_eq!(r, [false, true, true, false]);
    }

    /// …but not when a shortcut used it, or when Home is FlexInput's only.
    #[test]
    fn a_used_or_exclusive_home_press_is_not_replayed() {
        let cfg = spec(&["btn_guide"], "long");
        let pin = spec(&["btn_guide", "dpad_up"], "down");
        let targets: [Target; 2] = [(&cfg, None), (&pin, None)];
        // The long-press fired.
        let fired = replays(&targets, false, &[
            (home(true, false), false, 0),
            (home(true, false), true, 600),
            (home(false, false), false, 650),
        ]);
        // Home+D-pad was held: the pin chord took it, whatever fired.
        let chorded = replays(&targets, false, &[
            (home(true, false), false, 0),
            (home(true, true), false, 50),
            (home(false, false), false, 100),
        ]);
        let exclusive = replays(&targets, true, &[
            (home(true, false), false, 0),
            (home(false, false), false, 50),
        ]);
        assert!(!fired.contains(&true) && !chorded.contains(&true) && !exclusive.contains(&true));
    }

    /// A double-tap shortcut on Home delays the replay by its gap, and a
    /// second press inside it means the first tap was the double's.
    #[test]
    fn a_double_tap_shortcut_holds_the_replay_for_its_gap() {
        let dbl = Some(ShortcutSpec { combo: combo(&["btn_guide"]), mode: "double".into(), gap_ms: 300.0 });
        let targets: [Target; 1] = [(&dbl, None)];
        let single = replays(&targets, false, &[
            (home(true, false), false, 0),
            (home(false, false), false, 50),
            (home(false, false), false, 200),
            (home(false, false), false, 360),
        ]);
        assert_eq!(single, [false, false, false, true]);
        let double = replays(&targets, false, &[
            (home(true, false), false, 0),
            (home(false, false), false, 50),
            (home(true, false), true, 150),
            (home(false, false), false, 200),
            (home(false, false), false, 600),
        ]);
        assert!(!double.contains(&true));
    }

    /// An ordinary-button chord has no leader: nothing is owned until the
    /// whole combo is held.
    #[test]
    fn an_ordinary_chord_owns_its_buttons_only_once_complete() {
        let both = spec(&["btn_lb", "btn_rb"], "long");
        let targets: [Target; 1] = [(&both, None)];
        let s = snap(&[("gilrs:xinput:0", "btn_lb", true), ("gilrs:xinput:0", "btn_rb", false)]);
        assert!(owned_pins(&s, &targets, &HashSet::new()).is_empty());
        let s = snap(&[("gilrs:xinput:0", "btn_lb", true), ("gilrs:xinput:0", "btn_rb", true)]);
        assert_eq!(owned_pins(&s, &targets, &HashSet::new()).len(), 2);
    }

    /// A single ordinary pad holding both buttons fires, as it always did.
    #[test]
    fn one_ordinary_pad_holding_both_still_fires() {
        let s = snap(&[
            ("gilrs:xinput:0", "btn_guide", true),
            ("gilrs:xinput:0", "btn_back", true),
        ]);
        assert!(combo_held(&s, &combo(&["btn_guide", "btn_back"]), None));
    }
}
