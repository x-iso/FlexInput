//! One dongle, shared by every transport that wants it.
//!
//! ⭐ **Bluetooth Classic and LE are not alternatives.** A dual-mode adapter
//! carries both at once over a single HCI transport — that is what "dual mode"
//! means, and it is exactly what the operating system does with a built-in
//! radio. There has never been a hardware reason to choose between Joy-Cons and
//! a Pro Controller on the same dongle.
//!
//! ❗ The reason it behaved as a choice was this crate: two transports each
//! called [`Dongle::open`], and WinUSB grants an interface to ONE claimant, so
//! the second was refused. A self-imposed limit that looked like a hardware one.
//!
//! # How sharing works
//!
//! The awkward part is that HCI is a single stream of events and ACL packets
//! for every link at once, and whoever reads it consumes it. Two readers on one
//! transport means each silently eats the other's traffic.
//!
//! So there is exactly ONE reader — a router thread — and it broadcasts:
//!
//! ```text
//!   dongle ──> router ──┬──> Joy-Con 2 transport   (ignores foreign handles)
//!                       └──> Classic transport     (ignores foreign handles)
//! ```
//!
//! Broadcasting rather than routing by connection handle is deliberate. Both
//! transports ALREADY filter every packet by handle — they have to, because one
//! transport can hold several links — so a copy they do not want costs one
//! comparison. Registering handle ownership would add a second place for the
//! truth to live, and the packets are a few hundred bytes at a few hundred
//! hertz.
//!
//! # Setup is exclusive, and that is the trick
//!
//! Connecting, pairing and GATT discovery are request/response conversations:
//! the helpers on [`Dongle`] send something and then read until the matching
//! reply arrives. Those cannot work through a broadcast, and rewriting every
//! one of them to take a subscription would touch far more code than this is
//! worth.
//!
//! Instead a transport takes an [`Radio::exclusive`] lease for the length of a
//! setup. The router stops reading while it is held, so the helpers get the raw
//! dongle and behave exactly as they always have. The cost is that the other
//! transport's packets go unread for the second or two a connect takes — which
//! is precisely what happened before, when the other transport could not open
//! the radio at all.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{AclPacket, Dongle, Event, Result};

/// One thing that arrived from the radio.
#[derive(Debug, Clone)]
pub enum Inbound {
    Event(Event),
    Acl(AclPacket),
}

/// How much a subscriber may fall behind before its oldest traffic is dropped.
///
/// ❗ Dropping is the right failure. A transport that stops draining — because
/// it is mid-connect, or wedged — must not make the queue grow without bound
/// and must not stall the router, because the router is the only thing feeding
/// the OTHER transport. Input is disposable; a stalled radio is not.
const QUEUE_LIMIT: usize = 512;

struct Sub {
    id: usize,
    queue: Mutex<VecDeque<Inbound>>,
    signal: Condvar,
    dropped: AtomicUsize,
}

/// The fan-out, separated from the transport.
///
/// ⭐ Split out so it can be TESTED. Queueing, bounding and subscriber
/// lifetime are the parts with real failure modes, and none of them depend on
/// a USB handle — keeping them in a type that has no `Dongle` means the tests
/// need no hardware and no fake one. (The first version of this file faked a
/// dongle with `mem::zeroed()`, which is unsound and would have had to be
/// leaked to avoid running libusb's destructor on a null handle. If a test
/// needs a fake that dangerous, the seam is in the wrong place.)
#[derive(Default)]
struct Bus {
    subs: Mutex<Vec<Arc<Sub>>>,
    next_id: AtomicUsize,
}

impl Bus {
    fn broadcast(&self, item: Inbound) {
        let subs = self.subs.lock().unwrap_or_else(|e| e.into_inner());
        for s in subs.iter() {
            let mut q = s.queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.len() >= QUEUE_LIMIT {
                q.pop_front();
                s.dropped.fetch_add(1, Ordering::Relaxed);
            }
            q.push_back(item.clone());
            s.signal.notify_one();
        }
    }

    fn add(&self) -> Arc<Sub> {
        let sub = Arc::new(Sub {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            queue: Mutex::new(VecDeque::new()),
            signal: Condvar::new(),
            dropped: AtomicUsize::new(0),
        });
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&sub));
        sub
    }

    fn remove(&self, id: usize) {
        let mut subs = self.subs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = subs.iter().position(|s| s.id == id) {
            subs.remove(i);
        }
    }
}

/// A shared dongle: one owner of the USB handle, many users of the radio.
pub struct Radio {
    dongle: Dongle,
    bus: Bus,
    /// Held while a transport is running a request/response conversation.
    exclusive: Mutex<()>,
    /// Set when the router should stop reading, so a lease is not kept waiting
    /// behind a read that is already in flight.
    paused: AtomicBool,
    shutdown: AtomicBool,
}

/// One transport's view of the radio.
pub struct Subscriber {
    radio: Arc<Radio>,
    sub: Arc<Sub>,
    /// Items pulled while looking for a different kind, put back in order.
    ///
    /// ⛔ **Without this, asking for an event THREW AWAY every packet in front
    /// of it.** Both transports drain events in a loop and then handle ACL, so
    /// the event drain silently consumed every input report that had queued up
    /// behind it — a controller that connected, negotiated its channels, and
    /// then appeared to send nothing at all. The probe never hit it because it
    /// reads the dongle directly and has no fan-out.
    ///
    /// "Discarding events that arrive first" was written in the doc comment as
    /// though it were a design decision. It was a data loss bug with a
    /// reassuring sentence attached.
    stash: Mutex<VecDeque<Inbound>>,
}

/// Exclusive use of the radio for the length of a setup conversation.
///
/// The router is paused while this exists. Keep it for as short a time as the
/// conversation needs — everything else on the radio is unread meanwhile.
pub struct Lease<'a> {
    radio: &'a Radio,
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl Lease<'_> {
    /// The raw dongle, for the request/response helpers.
    pub fn dongle(&self) -> &Dongle {
        &self.radio.dongle
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.radio.paused.store(false, Ordering::Release);
    }
}

impl Radio {
    /// Take an exclusive lease for a setup conversation.
    pub fn exclusive(&self) -> Lease<'_> {
        // Asked for BEFORE the lock so the router notices and stops reading
        // rather than being discovered mid-transfer.
        self.paused.store(true, Ordering::Release);
        let guard = self.exclusive.lock().unwrap_or_else(|e| e.into_inner());
        Lease { radio: self, _guard: guard }
    }

    /// Run something against the raw dongle under an exclusive lease.
    ///
    /// ⭐ The conversion path for code written against a dongle it owned. Every
    /// helper — scan enable, connect, GATT discovery, pairing — sends and then
    /// reads its own reply, so all of them need the router held off; wrapping
    /// the call is the whole change, and the helper itself is untouched.
    ///
    /// ❗ Keep the closure SHORT. Nothing else on the radio is read while it
    /// runs, so a long one starves the other transport. Anything that streams
    /// belongs on a subscription instead.
    pub fn with_dongle<R>(&self, f: impl FnOnce(&Dongle) -> R) -> R {
        let lease = self.exclusive();
        f(lease.dongle())
    }

    fn broadcast(&self, item: Inbound) {
        self.bus.broadcast(item);
    }
}

impl Subscriber {
    /// The shared radio, for leases and writes.
    pub fn radio(&self) -> &Arc<Radio> {
        &self.radio
    }

    /// Take the radio exclusively, HANDING BACK anything already queued here.
    ///
    /// ⛔ **The plain lease has a hole in it, and this is the patch.**
    ///
    /// `Radio::exclusive` stops the router, but it cannot un-read what the
    /// router already read. Between a transport deciding to set up a link and
    /// its lease taking hold, the router keeps going — so the first few events
    /// of that very conversation can land on the bus while the helpers, which
    /// read the dongle directly, see nothing.
    ///
    /// Traced on hardware, this cost exactly one event and the whole feature: a
    /// bonded controller reconnected, its `Link Key Request` was broadcast a
    /// moment before the lease closed, `page_and_pair` waited for a request
    /// that had already happened, and authentication failed with `0x05` — key
    /// missing, from a host holding the correct key. The giveaway was the
    /// request appearing in the subscription log *after* the failure it caused.
    ///
    /// So the queue is emptied back into the dongle, in order and at the front,
    /// before the caller gets the lease. Use this — not `radio().exclusive()` —
    /// for anything that drives a conversation.
    pub fn exclusive_reclaiming(&self) -> Lease<'_> {
        let lease = self.radio.exclusive();
        let mut events = Vec::new();
        let mut acl = Vec::new();
        let mut take = |item: Inbound| match item {
            Inbound::Event(e) => events.push(e),
            Inbound::Acl(p) => acl.push(p),
        };
        // Stash first: those were pulled even earlier than the queue.
        {
            let mut stash = self.stash.lock().unwrap_or_else(|e| e.into_inner());
            while let Some(item) = stash.pop_front() {
                take(item);
            }
        }
        {
            let mut q = self.sub.queue.lock().unwrap_or_else(|e| e.into_inner());
            while let Some(item) = q.pop_front() {
                take(item);
            }
        }
        if !events.is_empty() {
            lease.dongle().push_events_front(events);
        }
        if !acl.is_empty() {
            lease.dongle().push_acl_front(acl);
        }
        lease
    }

    /// Next item for this transport, or `None` on timeout.
    pub fn recv(&self, timeout: Duration) -> Option<Inbound> {
        if let Some(item) = self
            .stash
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            return Some(item);
        }
        let deadline = Instant::now() + timeout;
        let mut q = self.sub.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(item) = q.pop_front() {
                return Some(item);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let (next, timed_out) = self
                .sub
                .signal
                .wait_timeout(q, left)
                .unwrap_or_else(|e| e.into_inner());
            q = next;
            if timed_out.timed_out() && q.is_empty() {
                return None;
            }
        }
    }

    /// Next EVENT. Anything passed over is KEPT, not discarded.
    pub fn recv_event(&self, timeout: Duration) -> Option<Event> {
        match self.recv_matching(timeout, true) {
            Some(Inbound::Event(e)) => Some(e),
            _ => None,
        }
    }

    /// Next ACL packet. Anything passed over is KEPT, not discarded.
    pub fn recv_acl(&self, timeout: Duration) -> Option<AclPacket> {
        match self.recv_matching(timeout, false) {
            Some(Inbound::Acl(p)) => Some(p),
            _ => None,
        }
    }

    fn recv_matching(&self, timeout: Duration, want_event: bool) -> Option<Inbound> {
        take_matching(&self.stash, timeout, want_event, |t| self.recv(t))
    }

    /// How much traffic this subscriber has missed by falling behind.
    pub fn dropped(&self) -> usize {
        self.sub.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        self.radio.bus.remove(self.sub.id);
    }
}

/// Pull until one KIND turns up, putting everything else back in order.
///
/// ⛔ **Whatever is passed over must be KEPT.** Both transports drain events in
/// a loop and only then handle ACL, so a `recv_event` that discarded what it
/// stepped over ate every input report queued behind the events — a controller
/// that connected, negotiated its channels and then appeared to send nothing at
/// all. That is invisible to a probe, which reads the dongle directly and has
/// no fan-out to lose anything in.
///
/// ❗ Order is preserved by pushing the held items back onto the FRONT in
/// reverse. Input reports are a time series; delivering them shuffled is its
/// own kind of wrong and harder to notice than losing them.
///
/// A free function rather than a method so it can be tested against a plain
/// queue — the alternative needed a fake `Radio`, and every fake `Radio` this
/// file has attempted was unsound.
fn take_matching(
    stash: &Mutex<VecDeque<Inbound>>,
    timeout: Duration,
    want_event: bool,
    mut pull: impl FnMut(Duration) -> Option<Inbound>,
) -> Option<Inbound> {
    let deadline = Instant::now() + timeout;
    let mut held: Vec<Inbound> = Vec::new();
    let found = loop {
        let left = match deadline.checked_duration_since(Instant::now()) {
            Some(d) => d,
            None => break None,
        };
        match pull(left) {
            Some(item) => {
                if matches!(item, Inbound::Event(_)) == want_event {
                    break Some(item);
                }
                held.push(item);
            }
            None => break None,
        }
    };
    if !held.is_empty() {
        let mut stash = stash.lock().unwrap_or_else(|e| e.into_inner());
        for item in held.into_iter().rev() {
            stash.push_front(item);
        }
    }
    found
}

/// The process-wide shared radio, opened on first use.
///
/// ⭐ One `OnceLock`, so the second transport to ask gets the SAME dongle
/// rather than a refusal. Which of them asks first no longer decides anything,
/// which is the whole point — ownership used to be a startup race whose loser
/// reported "another process holds it" about its own process.
fn cell() -> &'static Mutex<Option<Arc<Radio>>> {
    static RADIO: OnceLock<Mutex<Option<Arc<Radio>>>> = OnceLock::new();
    RADIO.get_or_init(|| Mutex::new(None))
}

/// Open (or join) the shared radio at `vid:pid`.
///
/// `None` when no dongle could be opened at all. The first caller decides which
/// adapter is shared; later callers get it regardless of the ids they pass,
/// because there is one radio and sharing it is the point.
pub fn shared(vid: u16, pid: u16) -> Option<Arc<Radio>> {
    let mut slot = cell().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = slot.as_ref() {
        return Some(Arc::clone(r));
    }
    // ⛔ A FAILED open must never be remembered.
    //
    // ❗ This was a `OnceLock<Option<..>>`, which caches whichever answer came
    // first — including `None`. One unlucky attempt at startup (a dongle the
    // previous process had not finished releasing is the usual one) then
    // poisoned the whole session: every later call returned the cached failure,
    // both transports stayed dead, and the settings window cheerfully listed an
    // adapter as AVAILABLE because nothing was actually holding it. Two
    // contradictory truths from one stale cache.
    match open_and_start(vid, pid) {
        Ok(r) => {
            *slot = Some(Arc::clone(&r));
            Some(r)
        }
        Err(e) => {
            // Not cached, so the next caller tries again.
            log::debug!("[radio] cannot open dongle {vid:04x}:{pid:04x}: {e}");
            None
        }
    }
}

fn open_and_start(vid: u16, pid: u16) -> Result<Arc<Radio>> {
    let dongle = Dongle::open(vid, pid)?;
    dongle.reset_and_init()?;
    clear_stale_links(&dongle);
    let radio = Arc::new(Radio {
        dongle,
        bus: Bus::default(),
        exclusive: Mutex::new(()),
        paused: AtomicBool::new(false),
        shutdown: AtomicBool::new(false),
    });
    let r = Arc::clone(&radio);
    std::thread::Builder::new()
        .name("bt-radio".into())
        .spawn(move || route(r))
        .ok();
    Ok(radio)
}

/// Drop any link the dongle is still holding from a previous run.
///
/// ⭐ **A connection lives in the dongle's firmware, not in the process that
/// made it.** When FlexInput exits — and especially when it crashes — the
/// controller stays connected to a radio that no longer has an owner. On the
/// next launch the host knows nothing about that link, so the pad is ignored
/// entirely; and because the CONTROLLER still believes it is connected, it
/// neither advertises, nor pages, nor answers an inquiry. It is invisible in
/// every direction, and the only cure the user has is power-cycling the pad —
/// which is exactly the "can't find paired device for shit" report.
///
/// ❗ A USB reset is supposed to prevent this and evidently does not always:
/// some controllers keep their baseband links across it. So the handles are
/// swept explicitly. Handles are allocated from the bottom, one per link, and
/// no adapter here carries more than a handful — a short sweep covers every
/// link that could exist.
///
/// ⛔ Only safe HERE, before any transport has connected anything. Doing it
/// later would tear down live links belonging to whichever transport made them.
fn clear_stale_links(dongle: &Dongle) {
    // `Unknown Connection Identifier` is the expected answer for a handle that
    // was never in use, so errors are ignored by design.
    for handle in 1u16..=8 {
        let _ = dongle.disconnect(handle);
    }
    // Give the disconnections somewhere to land, so their events do not turn up
    // later as surprises attributed to a live link.
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        match dongle.read_event_timeout(Duration::from_millis(50)) {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    let _ = dongle.drain_events();
}

/// The single reader.
fn route(radio: Arc<Radio>) {
    while !radio.shutdown.load(Ordering::Relaxed) {
        // ❗ Checked before every read, and the lock is taken with `try_lock`.
        // Blocking on the lease lock here would let a long setup hold the
        // router inside a read it had already started, so a lease could not
        // take effect until the current transfer timed out.
        if radio.paused.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let Ok(_guard) = radio.exclusive.try_lock() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        // Short timeouts: this loop must return to the pause check quickly, and
        // a subscriber's own timeout is what governs how long IT waits.
        let mut idle = true;
        if let Ok(Some(e)) = radio.dongle.read_event_timeout(Duration::from_millis(2)) {
            radio.broadcast(Inbound::Event(e));
            idle = false;
        }
        if let Ok(Some(p)) = radio.dongle.read_acl(Duration::from_millis(2)) {
            radio.broadcast(Inbound::Acl(p));
            idle = false;
        }
        drop(_guard);
        if idle {
            // Nothing waiting: yield rather than spin the USB stack flat.
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Subscribe a transport to the shared radio.
pub fn subscribe(radio: &Arc<Radio>) -> Subscriber {
    let sub = radio.bus.add();
    Subscriber {
        radio: Arc::clone(radio),
        sub,
        stash: Mutex::new(VecDeque::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subscriber attached to a bare bus — no dongle involved.
    fn sub_of(bus: &Bus) -> Arc<Sub> {
        bus.add()
    }

    fn take(sub: &Arc<Sub>) -> Option<Inbound> {
        sub.queue.lock().unwrap().pop_front()
    }

    /// ⛔ EVERY subscriber gets EVERY packet.
    ///
    /// ⭐ One transport consuming a packet the other needed is precisely why
    /// two readers on one dongle was impossible, and why this exists. HCI is a
    /// single stream for every link at once; whoever reads it consumes it.
    #[test]
    fn every_subscriber_receives_every_packet() {
        let bus = Bus::default();
        let a = sub_of(&bus);
        let b = sub_of(&bus);
        bus.broadcast(Inbound::Event(Event::InquiryComplete { status: 0 }));
        assert!(take(&a).is_some(), "first subscriber missed it");
        assert!(take(&b).is_some(), "second subscriber missed it — one reader ate it");
    }

    /// ⛔ A subscriber that stops draining must not stall the radio.
    ///
    /// The router is the only thing feeding the OTHER transport, so unbounded
    /// growth or back-pressure here would turn one wedged transport into two.
    /// Dropping the oldest is the right failure: input is disposable, a stalled
    /// radio is not.
    #[test]
    fn a_subscriber_that_falls_behind_drops_rather_than_blocks() {
        let bus = Bus::default();
        let slow = sub_of(&bus);
        for _ in 0..(QUEUE_LIMIT + 50) {
            bus.broadcast(Inbound::Event(Event::InquiryComplete { status: 0 }));
        }
        assert_eq!(slow.dropped.load(Ordering::Relaxed), 50, "queue did not bound itself");
        assert_eq!(slow.queue.lock().unwrap().len(), QUEUE_LIMIT);
    }

    fn acl(n: u8) -> Inbound {
        Inbound::Acl(AclPacket { conn_handle: 1, cid: 0x0041, payload: vec![n] })
    }

    /// ⛔ Asking for an EVENT must not throw away queued input.
    ///
    /// ⭐ The bug that made a connected controller look silent, and the reason
    /// the probe could not reproduce it: the probe reads the dongle directly,
    /// so it has no fan-out to lose anything in. In the app, both transports
    /// drain events first and handle ACL second — so a discarding `recv_event`
    /// ate every report that had queued behind the events. Link up, channels
    /// negotiated, nothing delivered.
    #[test]
    fn asking_for_an_event_keeps_the_input_behind_it() {
        let stash: Mutex<VecDeque<Inbound>> = Mutex::new(VecDeque::new());
        let mut queue: VecDeque<Inbound> = VecDeque::new();
        queue.push_back(acl(1));
        queue.push_back(acl(2));
        queue.push_back(Inbound::Event(Event::InquiryComplete { status: 0 }));
        queue.push_back(acl(3));

        // Mirrors `Subscriber::recv`: the stash is consulted before the queue,
        // which is what makes putting items back mean anything.
        macro_rules! pull {
            () => {
                |_: Duration| {
                    stash
                        .lock()
                        .unwrap()
                        .pop_front()
                        .or_else(|| queue.pop_front())
                }
            };
        }
        let got = take_matching(&stash, Duration::from_millis(50), true, pull!());
        assert!(matches!(got, Some(Inbound::Event(_))), "did not find the event");

        // ⭐ The two reports it stepped over are still there, IN ORDER.
        let mut back = Vec::new();
        while let Some(x) = take_matching(&stash, Duration::from_millis(10), false, pull!()) {
            match x {
                Inbound::Acl(p) => back.push(p.payload[0]),
                Inbound::Event(_) => unreachable!(),
            }
        }
        assert_eq!(back, vec![1, 2, 3], "input was lost or reordered: {back:?}");
    }

    /// A dropped subscriber must stop receiving, or the bus leaks queues that
    /// nobody drains for the life of the process.
    #[test]
    fn removing_a_subscriber_stops_its_traffic() {
        let bus = Bus::default();
        let a = sub_of(&bus);
        assert_eq!(bus.subs.lock().unwrap().len(), 1);
        bus.remove(a.id);
        assert_eq!(bus.subs.lock().unwrap().len(), 0);
        bus.broadcast(Inbound::Event(Event::InquiryComplete { status: 0 }));
        assert!(take(&a).is_none(), "a removed subscriber still received traffic");
    }
}
