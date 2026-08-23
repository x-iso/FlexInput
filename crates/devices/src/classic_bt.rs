//! Bluetooth Classic gamepads on FlexInput's own dongle.
//!
//! ⭐ **Why this exists at all.** Windows runs one Bluetooth radio stack, so a
//! controller paired through Windows shares its adapter with every headset,
//! mouse and phone in range. A dongle bound to WinUSB and driven by our own HCI
//! stack is a radio that belongs to one controller — which is where the latency
//! and the polling rate come from.
//!
//! The path is the one proven by `bt_classic`: page the controller, hand back
//! the stored link key, open the two HID L2CAP channels, and read input reports
//! off the interrupt channel.
//!
//! ❗ **Pairing is NOT done here, deliberately.** Pairing replaces whatever host
//! the controller was bonded to — on a Switch Pro, its console — and a
//! background thread that quietly re-bonds any gamepad it can see would be a
//! genuinely bad thing to ship. Pairing is an explicit act performed with the
//! `bt_classic` tool; this connects only to addresses that already have a
//! stored link key.
//!
//! ⭐ **Runs alongside the Joy-Con 2 transport on the same dongle.** A
//! dual-mode adapter carries Bluetooth Classic and LE at once — that is what
//! dual mode is — and both transports share one radio through
//! [`flexinput_btle::radio`]. There is no switch and no trade: the reason this
//! ever looked like a choice was that each transport called `Dongle::open`
//! separately, and WinUSB grants an interface to one claimant.
//!
//! ❗ Reads come from a subscription. Anything that sends and waits for a reply
//! — paging, pairing, opening an L2CAP channel — runs under
//! `Radio::with_dongle`, which holds the shared reader off for the length of
//! that conversation.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flexinput_btle::{keystore, l2cap};
use flexinput_core::Signal;

use crate::gyro::{parse_switch_pro_report, push_switch_pro_buttons, HidReading};
use crate::{layouts, ControllerKind, DeviceBackend, DevicePin, PhysicalDevice};

/// How long a link may go silent before it is treated as gone.
const STALE: Duration = Duration::from_secs(3);

/// Gap between connection attempts, so a controller that is simply switched off
/// does not spin the radio.
const RETRY_GAP: Duration = Duration::from_secs(5);

/// One connected classic controller.
#[derive(Clone)]
struct PadState {
    address: [u8; 6],
    reading: HidReading,
    last: Instant,
    /// The controller's friendly name from the key store, if it was recorded.
    name: Option<String>,
    /// Reports since the last drain, for the per-device rate display.
    ///
    /// ❗ Counted here rather than inferred from the signal stream: `poll`
    /// republishes the last reading every tick whether or not a new report
    /// arrived, so counting polls would show the UI's frame rate and counting
    /// changes would show zero for a controller sitting still. Neither is the
    /// polling rate.
    events: u32,
}

/// What a pairing run is doing, for the UI to show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PairPhase {
    #[default]
    Idle,
    Searching,
    Pairing(String),
    Done(String),
    Failed(String),
}

/// What the transport is doing, so the UI never has to say nothing.
///
/// ⭐ Every one of these was previously invisible. "The button does nothing"
/// is the report that follows from a background thread that fails silently, and
/// the fix is not a better button — it is saying which of these states it is in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Disabled,
    /// Enabled, but no Bluetooth adapter could be opened.
    NoRadio(String),
    /// Radio held, nothing paired yet.
    Idle,
    /// Radio held. `connected` links are up; `streaming` of those are actually
    /// delivering input.
    ///
    /// ⭐ Two numbers, because they came apart on hardware and one number hid
    /// it: the panel said "1 of 1 connected" while no controller appeared
    /// anywhere, because a link being UP and a link DELIVERING REPORTS are
    /// different things and only the second produces a device.
    Running { paired: usize, connected: usize, streaming: usize },
}

#[derive(Default)]
struct Shared {
    status: Mutex<Status>,
    /// Set by the UI to ask for a pairing run; cleared when it starts.
    ///
    /// ⭐ A REQUEST, not an action. The radio belongs to this backend's thread,
    /// and a UI that opened the dongle itself would be a second owner — the
    /// exact conflict that already cost a debugging session. The button sets a
    /// flag; the thread that owns the hardware does the work and reports back.
    pair_requested: AtomicBool,
    pair_phase: Mutex<PairPhase>,
    pads: Mutex<HashMap<[u8; 6], PadState>>,
    shutdown: AtomicBool,
    /// Set once the radio could not be opened, so the reason is logged once
    /// rather than every retry.
    yielded: AtomicBool,
    /// Same, for "enabled but nothing is paired".
    said_empty: AtomicBool,
    /// Set by the worker once it has disconnected everything and returned.
    stopped: AtomicBool,
}

pub struct ClassicBtBackend {
    shared: Arc<Shared>,
}

/// Handle the UI uses to drive pairing without touching the radio.
#[derive(Clone, Default)]
pub struct PairControl {
    shared: Arc<Shared>,
}

impl PairControl {
    /// Ask the backend to look for a controller in pairing mode and bond it.
    pub fn request(&self) {
        self.shared.pair_requested.store(true, Ordering::Relaxed);
    }

    /// What the current (or last) run is doing.
    pub fn phase(&self) -> PairPhase {
        self.shared.pair_phase.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// What the transport itself is doing.
    pub fn status(&self) -> Status {
        self.shared.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Whether a usable radio was found. A pair button is pointless without
    /// one, and saying so beats a button that silently does nothing.
    pub fn transport_enabled(&self) -> bool {
        !matches!(self.status(), Status::Disabled | Status::NoRadio(_))
    }
}

/// The process-wide control handle.
///
/// One backend is constructed per process, so a global handle is honest about
/// what is actually there — and it saves threading an Arc through the whole UI
/// for one button.
pub fn pair_control() -> PairControl {
    static CTL: std::sync::OnceLock<PairControl> = std::sync::OnceLock::new();
    CTL.get_or_init(PairControl::default).clone()
}

impl ClassicBtBackend {
    pub fn new() -> Self {
        // Shares the same `Shared` the UI's control handle holds, so a request
        // made before the thread starts is still seen.
        let shared = Arc::clone(&pair_control().shared);
        let s = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("bt-classic".into())
            .spawn(move || run(s))
            .ok();
        Self { shared }
    }
}

impl Drop for ClassicBtBackend {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        // ⭐ Wait for the worker to tear its links down.
        //
        // ❗ A link lives in the DONGLE, not in this process. Exiting without
        // disconnecting leaves the controller believing it is still connected —
        // after which it neither pages, nor advertises, nor answers an inquiry,
        // so the next run cannot find it by any means and the user has to
        // power-cycle the pad. Bounded, because a tidy exit is not worth
        // hanging the application over.
        let deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < deadline && !self.shared.stopped.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// `vid:pid` of the dongle to use.
///
/// ⭐ **Nothing here is specific to one adapter.** A Bluetooth dongle announces
/// itself by USB CLASS — wireless controller / RF / Bluetooth — so the first
/// one that is actually openable is picked automatically and any WinUSB-bound
/// adapter works. The Realtek ids are only a last-resort fallback for the case
/// where discovery finds nothing at all, which keeps the error message naming a
/// concrete device instead of `0000:0000`.
///
/// The environment variable still wins, and is what you use to pin a SECOND
/// adapter to this transport while the Joy-Con hub keeps the first.
fn dongle_ids() -> Option<(u16, u16)> {
    let Ok(raw) = std::env::var("FLEXINPUT_BT_CLASSIC_DONGLE") else {
        // ⛔ No hardcoded fallback. Guessing a vendor id reports "your dongle
        // is missing" about somebody else's dongle — see
        // `flexinput_btle::preferred_dongle`.
        return flexinput_btle::preferred_dongle();
    };
    let mut it = raw.split(':');
    match (
        it.next().and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok()),
        it.next().and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok()),
    ) {
        (Some(v), Some(p)) => Some((v, p)),
        _ => {
            eprintln!(
                "[bt-classic] FLEXINPUT_BT_CLASSIC_DONGLE={raw:?} is not vid:pid                  — ignoring it and using whichever adapter is present"
            );
            flexinput_btle::preferred_dongle()
        }
    }
}

/// The most controllers held at once.
///
/// Not a protocol limit — a classic radio manages seven active links — but a
/// practical one: every connected pad shares this thread and the dongle's
/// bandwidth, and four is already more than a couch holds.
const MAX_LINKS: usize = 4;

/// How long a background page may hold the radio.
///
/// ⛔ **Every second here is a second the shared radio is deaf to everything
/// else**, Joy-Cons included — a page takes an exclusive lease. At three
/// seconds every five, a switched-off controller was making the radio
/// unavailable sixty per cent of the time, which is not a background task, it
/// is a denial of service with a retry timer.
const CONNECT_PATIENCE: Duration = Duration::from_secs(2);

/// How long to spend on a link the REMOTE opened.
///
/// ⛔ **Not the same budget, because it is not the same cost.** The two seconds
/// above are a limit on how long a PAGE may hold the radio away from everything
/// else, and a page at a switched-off controller earns nothing by lasting
/// longer. A link the controller opened is the opposite: the conversation is
/// already happening, the radio is doing the one thing it is here to do, and
/// cutting it off achieves nothing but losing it.
///
/// Sharing the page budget cost exactly that — a reconnection that had
/// authenticated and was mid-handshake, abandoned at two seconds with
/// `timed out (connected=false, paired=true)` while the controller was still
/// answering. Connection Complete on an incoming link routinely takes longer
/// than a page does, because authentication happens along the way.

const ADOPT_PATIENCE: Duration = Duration::from_secs(10);

/// How often to page a paired controller that has not called in, while at
/// least one other controller is connected.
///
/// ⭐ **Rare, because a live link is worth more than a fast reconnect.** Every
/// page takes an exclusive lease, and spending it on a pad that is switched off
/// steals it from pads that are switched on.
const PAGE_FALLBACK: Duration = Duration::from_secs(30);

/// How often to page when NOTHING is connected.
///
/// ⛔ **Paging and listening are MUTUALLY EXCLUSIVE, and listening wins.**
///
/// A radio that is paging is not page-scanning, so every second spent calling a
/// controller is a second in which that controller cannot call us — and calling
/// us is how a bonded pad reconnects. The two mechanisms are not complementary;
/// they compete for the same radio.
///
/// This was briefly set to 3 s on the theory that a page is a coin flip worth
/// flipping often. Measured live, it was catastrophic: 23 attempts in 46
/// seconds at 2 s of patience each is a radio that is paging ONE HUNDRED PER
/// CENT of the time. The pad called and called into a host that was, by
/// construction, never listening. The probe which did receive incoming pages
/// was the one that never paged at all.
///
/// So the gap is long, the patience is short, and the duty cycle is stated
/// rather than left to emerge: roughly one second of paging per twenty, and
/// nineteen spent listening — which is what the mechanism actually needs.
///
/// ❗ Timed from the END of an attempt. Timing it from the start subtracts the
/// patience from the gap, which is how three seconds became zero.
const PAGE_EAGER: Duration = Duration::from_secs(20);

/// How often to repeat the "waiting for a controller" line.
///
/// ❗ Said once, it reads as "gave up". Said every pass, it is a firehose.
const NOTE_REPEAT: Duration = Duration::from_secs(120);

/// One live controller.
struct Link {
    addr: [u8; 6],
    conn: u16,
    control: l2cap::Channel,
    interrupt: l2cap::Channel,
    last: Instant,
    name: Option<String>,
    /// Whether any input report has arrived on this link yet.
    reported: bool,
}

/// Answer the remote's L2CAP signalling on a live link.
///
/// ⭐ Only configuration and disconnection, which are the two a HID device
/// actually sends once its channels are up. Both must be answered: an
/// unanswered configuration request is re-sent until the remote gives up, and
/// an unanswered disconnection request leaves a half-closed channel behind.
fn answer_signalling(
    radio: &flexinput_btle::radio::Radio,
    link: &Link,
    pkt: &flexinput_btle::AclPacket,
) {
    let Some(sig) = l2cap::parse_signal(&pkt.payload) else { return };
    match sig.code {
        l2cap::SIG_CONFIGURE_REQUEST => {
            if let Some((dest, opts)) = l2cap::parse_configure_request(&sig.data) {
                let remote = if dest == link.interrupt.local_cid {
                    link.interrupt.remote_cid
                } else {
                    link.control.remote_cid
                };
                let reply = l2cap::encode_signal(
                    l2cap::SIG_CONFIGURE_RESPONSE,
                    sig.identifier,
                    &l2cap::configure_response(remote, &opts),
                );
                let _ = radio.with_dongle(|d| {
                    d.send_att_raw(link.conn, l2cap::CID_SIGNALLING, &reply)
                });
            }
        }
        l2cap::SIG_DISCONNECTION_REQUEST => {
            let reply = l2cap::encode_signal(
                l2cap::SIG_DISCONNECTION_RESPONSE,
                sig.identifier,
                &sig.data,
            );
            let _ = radio.with_dongle(|d| {
                d.send_att_raw(link.conn, l2cap::CID_SIGNALLING, &reply)
            });
        }
        _ => {}
    }
}

/// Say so when a stored key belongs to a DIFFERENT dongle.
///
/// ⛔ **The failure this exists for is completely silent.** The controller
/// remembers the address of the radio it paired with and pages that address;
/// a key carried over from another adapter leaves the pad blinking at a host
/// which is listening correctly, with page scan verified on, for a call that is
/// addressed to somebody else. No error is raised anywhere, because nothing
/// went wrong — the two are simply not talking to each other.
///
/// Entries written before the adapter was recorded have `None` and are left
/// alone: unknown is not the same as wrong, and guessing would cry wolf on
/// every pairing that predates this.
fn warn_about_foreign_keys(
    radio: &flexinput_btle::radio::Radio,
    known: &std::collections::BTreeMap<String, keystore::Pairing>,
) {
    let Ok(ours) = radio.with_dongle(|d| d.read_bd_addr()) else { return };
    if trace() {
        eprintln!("[bt-classic] this adapter is {}", keystore::format_addr(ours));
    }
    for (addr, p) in known {
        match p.adapter {
            Some(a) if a != ours => eprintln!(
                "[bt-classic] ⚠ the key for {addr} was made with adapter {} —                  this dongle is {}. The controller is paging the other one and                  will never call us. Pair it again with this dongle.",
                keystore::format_addr(a),
                keystore::format_addr(ours),
            ),
            _ => {}
        }
    }
}

fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FLEXINPUT_BT_TRACE").is_ok())
}

fn set_status(shared: &Arc<Shared>, s: Status) {
    if let Ok(mut g) = shared.status.lock() {
        if *g != s {
            *g = s;
        }
    }
}

fn run(shared: Arc<Shared>) {
    run_inner(&shared);
    shared.stopped.store(true, Ordering::Relaxed);
}

fn run_inner(shared: &Arc<Shared>) {
    let mut links: Vec<Link> = Vec::new();
    let mut next_try = Instant::now();
    let mut last_note: Option<Instant> = None;
    let mut radio: Option<Arc<flexinput_btle::radio::Radio>> = None;
    let mut sub: Option<flexinput_btle::radio::Subscriber> = None;

    // ❗ Cached. `keystore::load` reads a file, and this loop runs as fast as
    // the radio delivers packets — a disk read per input report is absurd, and
    // it is time not spent draining the queue that carries them.
    let mut known = keystore::load();
    let mut known_at = Instant::now();
    while !shared.shutdown.load(Ordering::Relaxed) {
        if known_at.elapsed() > Duration::from_secs(2) {
            known = keystore::load();
            known_at = Instant::now();
        }
        // ⛔ Read BEFORE the empty-store early return.
        //
        // ❗ This is what made "Pair new controller" do nothing at all: with no
        // controllers paired the loop hit `continue` here and never reached the
        // pairing code below — so pairing from scratch, the one case the button
        // exists for, was the exact case it could not serve. An empty store is
        // a reason to keep the radio closed, not a reason to ignore the user.
        let want_pair = shared.pair_requested.load(Ordering::Relaxed);
        if known.is_empty() && !want_pair {
            set_status(&shared, Status::Idle);
            // ⭐ Said ONCE, with the path. "Enabled but nothing happens" is the
            // state a user is most likely to hit — the pairing tool writes its
            // key file beside its own binary, which need not be where the app
            // looks — and an empty store is indistinguishable from a broken
            // transport unless it says which file it read.
            if !shared.said_empty.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[bt-classic] no controllers paired — key store is {}\n\
                     [bt-classic] pair one with: cargo run -p flexinput-btle \
                     --bin bt_classic -- --pair <address>",
                    keystore::path().display(),
                );
            }
            radio = None;
            sub = None;
            links.clear();
            std::thread::sleep(RETRY_GAP);
            continue;
        }
        shared.said_empty.store(false, Ordering::Relaxed);
        if radio.is_none() {
            match dongle_ids().and_then(|(v, p)| flexinput_btle::radio::shared(v, p)) {
                Some(r) => {
                    if trace() {
                        eprintln!("[bt-classic] radio obtained; enabling page scan");
                    }
                    // ⭐ ANSWER incoming pages. A bonded controller that is
                    // switched on calls its host rather than waiting to be
                    // called, and `HCI_Reset` leaves scanning off — so without
                    // this the pad blinks, pages a deaf radio, and gives up.
                    // Page scan only: our own controllers can find us, a
                    // general inquiry cannot.
                    match r.with_dongle(|d| {
                        d.set_scan_enable(0x02).and_then(|()| d.read_scan_enable())
                    }) {
                        // ⭐ Read back, not assumed. "Nothing arrives" looks the
                        // same whether the radio is deaf or simply not being
                        // called, and only one of those is our bug.
                        Ok(mask) if mask & 0x02 != 0 => {
                            if trace() {
                                eprintln!("[bt-classic] page scan on (scan_enable {mask:#04x})");
                            }
                        }
                        Ok(mask) => eprintln!(
                            "[bt-classic] page scan did NOT take: scan_enable {mask:#04x}"
                        ),
                        Err(e) => eprintln!("[bt-classic] could not enable page scan: {e}"),
                    }
                    warn_about_foreign_keys(&r, &known);
                    sub = Some(flexinput_btle::radio::subscribe(&r));
                    shared.yielded.store(false, Ordering::Relaxed);
                    radio = Some(r);
                }
                None => {
                    set_status(
                        &shared,
                        Status::NoRadio("no WinUSB-bound Bluetooth dongle".into()),
                    );
                    if !shared.yielded.swap(true, Ordering::Relaxed) {
                        eprintln!(
                            "[bt-classic] no usable dongle — is one bound to WinUSB \
                             via Zadig?"
                        );
                    }
                }
            }
            if radio.is_none() {
                std::thread::sleep(RETRY_GAP);
                continue;
            }
        }
        let (Some(r), Some(sub)) = (radio.as_ref(), sub.as_ref()) else { continue };

        // ── Drop links that have gone quiet ──────────────────────────────
        links.retain(|l| {
            let alive = l.last.elapsed() < STALE;
            if !alive {
                eprintln!("[bt-classic] {} disconnected", keystore::format_addr(l.addr));
                shared.pads.lock().unwrap().remove(&l.addr);
            }
            alive
        });

        // ── Bring up anything paired but not yet connected ───────────────
        //
        // ⭐ One attempt per pass, not a loop over every missing address: each
        // attempt costs `CONNECT_PATIENCE` of this thread, and trying four
        // switched-off controllers back to back would stall the ones that are
        // on for twelve seconds.
        if Instant::now() >= next_try && links.len() < MAX_LINKS {
            // With a link up the lease belongs to the controller that is
            // actually playing; with none, to listening for one.
            let gap = if links.is_empty() { PAGE_EAGER } else { PAGE_FALLBACK };
            // ⛔ Set from the END of the attempt, below — never here. Timing
            // the gap from the start subtracts the page's own patience from it,
            // and a 3 s gap behind a 2 s page leaves one second of listening in
            // every three. Measured, it left none at all.
            next_try = Instant::now() + gap;
            let missing = known
                .iter()
                .filter_map(|(t, p)| keystore::parse_addr(t).map(|a| (a, p.key)))
                .find(|(a, _)| !links.iter().any(|l| l.addr == *a));
            if let Some((addr, key)) = missing {
                // ❗ A CID pair per SLOT. Channel ids are scoped to their ACL
                // link so reuse would in fact be legal, but distinct ids make a
                // stray packet impossible to misattribute while several links
                // are being set up — and cost nothing.
                let base = 0x0040 + (links.len() as u16) * 2;
                if trace() {
                    eprintln!("[bt-classic] paging {}", keystore::format_addr(addr));
                }
                let outcome = connect(sub, addr, key, base);
                next_try = Instant::now() + gap;
                match outcome {
                    Ok(mut link) => {
                        link.name = known
                            .get(&keystore::format_addr(addr))
                            .and_then(|p| p.name.clone());
                        eprintln!("[bt-classic] {} connected", keystore::format_addr(addr));
                        last_note = None;
                        links.push(link);
                    }
                    Err(ref e) if trace() => {
                        eprintln!("[bt-classic] page {} failed: {e}",
                            keystore::format_addr(addr));
                        let due = last_note
                            .map(|t: Instant| t.elapsed() > NOTE_REPEAT)
                            .unwrap_or(true);
                        if due {
                            last_note = Some(Instant::now());
                        }
                    }
                    Err(_) => {
                        // ⭐ Not an error, and no longer reported as one.
                        //
                        // ❗ A page failing means the controller is not
                        // listening right now, which for a switched-off pad is
                        // every single time. The old message said "did not
                        // answer" once and then went quiet forever, which reads
                        // exactly like the transport gave up — while it was in
                        // fact still waiting, and would still have accepted the
                        // controller the moment it was switched on.
                        //
                        // What is worth saying is what is actually true: we are
                        // listening. Repeated on a slow timer so it stays
                        // visible without becoming noise.
                        let due = last_note
                            .map(|t: Instant| t.elapsed() > NOTE_REPEAT)
                            .unwrap_or(true);
                        if due {
                            last_note = Some(Instant::now());
                            eprintln!(
                                "[bt-classic] waiting for {} — switch it on and it \
                                 will connect itself.",
                                keystore::format_addr(addr),
                            );
                        }
                    }
                }
            }
        }

        let streaming = shared
            .pads
            .lock()
            .map(|p| p.values().filter(|s| s.last.elapsed() < STALE).count())
            .unwrap_or(0);
        set_status(
            shared,
            Status::Running { paired: known.len(), connected: links.len(), streaming },
        );

        // ── A pairing run, if the UI asked for one ───────────────────────
        //
        // ❗ Done HERE rather than anywhere else because this thread owns the
        // radio. An inquiry takes seconds and blocks connected controllers, so
        // it happens only when explicitly requested — never on a timer.
        if shared.pair_requested.swap(false, Ordering::Relaxed) {
            let began = Instant::now();
            run_pairing(r, &shared);
            // ⛔ **Do not count our own inquiry against the pads.**
            //
            // An inquiry holds the radio for about eight seconds, and `STALE`
            // is three — so pairing a second controller silently killed every
            // controller already connected, which is exactly what a live trace
            // showed: link up at 1.7 s, pairing requested, "disconnected" a few
            // seconds later, with the pad still sitting there perfectly happy.
            //
            // The pad was not quiet; WE were not listening. Pushing `last`
            // forward by the time we spent elsewhere makes staleness mean what
            // it is supposed to mean.
            let held = began.elapsed();
            for l in &mut links {
                l.last += held;
            }
            if let Ok(mut pads) = shared.pads.lock() {
                for s in pads.values_mut() {
                    s.last += held;
                }
            }
            continue;
        }

        // ── Answer anything calling us ───────────────────────────────────
        //
        // Checked before the ACL read: a Connection Request that is not
        // answered promptly is abandoned by the remote, and the pad then
        // retries from scratch — which is exactly the "blinks and searches
        // again" loop.
        while let Some(evt) = sub.recv_event(Duration::from_millis(1)) {
            // Temporary trace: FLEXINPUT_BT_TRACE=1 prints every event the
            // subscription delivers, so "the controller never called" can be
            // told apart from "the call was delivered and dropped".
            if trace() {
                eprintln!("[bt-classic] evt {evt:?}");
            }
            let flexinput_btle::Event::ConnectionRequest { address, .. } = evt else {
                continue;
            };
            let text = keystore::format_addr(address);
            let Some(key) = known.get(&text).map(|p| p.key) else {
                // ❗ Not ours. Left unanswered rather than rejected: a refusal
                // is a definite "go away" to a device that may be trying to
                // reach a completely different host, and silence costs it only
                // one retry.
                continue;
            };
            if links.iter().any(|l| l.addr == address) {
                continue;
            }
            eprintln!("[bt-classic] {text} is calling — accepting");
            let base = 0x0040 + (links.len() as u16) * 2;
            // ❗ Accept and authenticate under ONE lease.
            //
            // Accepting in its own lease and then taking a second one to wait
            // for the reply is a race the shared reader wins about as often as
            // not: `Connection Complete` can arrive in the gap, get broadcast,
            // and never be seen by the code waiting for it — which shows up as
            // a reconnect that times out for no visible reason.
            match adopt(sub, address, key, base) {
                Ok(mut link) => {
                    link.name = known.get(&text).and_then(|p| p.name.clone());
                    eprintln!("[bt-classic] {text} connected (incoming)");
                    last_note = None;
                    links.push(link);
                }
                Err(e) => eprintln!("[bt-classic] {text} incoming link failed: {e}"),
            }
        }

        // ── Service every link from ONE read ─────────────────────────────
        //
        // The whole point of the restructure: reports for all connected
        // controllers arrive interleaved on the same transport, so there is one
        // reader and it routes by connection handle. A blocking read per pad
        // meant the second controller was never serviced at all.
        // ⭐ A BATCH per pass, not one packet. A Pro Controller sends about 220
        // reports a second; taking one per loop iteration cannot keep up with
        // that, so the shared radio's queue fills and the oldest input is
        // dropped — the controller looks connected and stutters or stalls.
        for _ in 0..64 {
            let Some(pkt) = sub.recv_acl(Duration::from_millis(2)) else {
                break;
            };
            let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) else {
                continue;
            };
            // ❗ L2CAP signalling must be ANSWERED, not skipped.
            //
            // Everything that was not an input report used to be discarded
            // here, including the remote's own configuration and disconnection
            // requests. A controller that asks something and is met with
            // silence tears the channel down — which presents as a link that is
            // up and delivers nothing, exactly what the panel reported while no
            // device appeared.
            if pkt.cid == l2cap::CID_SIGNALLING {
                answer_signalling(r, link, &pkt);
                continue;
            }
            if pkt.cid != link.interrupt.local_cid {
                continue;
            }
        // The leading byte is the HID transaction header (`0xa1` = DATA on the
        // input pipe), not part of the report. Handing it to the parser shifts
        // every field by one and yields plausible nonsense.
            if pkt.payload.len() < 2 || pkt.payload[0] != 0xA1 {
                continue;
            }
            let Some(reading) = parse_switch_pro_report(&pkt.payload[1..]) else {
                continue;
            };
            if !link.reported {
                link.reported = true;
                eprintln!(
                    "[bt-classic] {} is sending input",
                    keystore::format_addr(link.addr)
                );
            }
            link.last = Instant::now();
            // ⛔ **ONE lock, held once.** This used to call `pads.lock()` a
            // second time inside the value expression of an `insert` whose own
            // guard was still alive — and `std::sync::Mutex` is not reentrant,
            // so the transport thread deadlocked against itself on the FIRST
            // input report to arrive. Everything up to that point looked
            // perfect: paired, encrypted, both HID channels configured, "is
            // sending input" printed once, and then the whole backend stopped
            // dead. `enumerate` blocked on the same mutex from the UI thread,
            // so no device ever appeared either.
            //
            // The `entry` API does the read and the write under the single
            // guard, which is both correct and shorter than what it replaces.
            {
                let mut pads = shared.pads.lock().unwrap();
                let slot = pads.entry(link.addr).or_insert_with(|| PadState {
                    address: link.addr,
                    reading,
                    last: link.last,
                    name: link.name.clone(),
                    events: 0,
                });
                slot.reading = reading;
                slot.last = link.last;
                slot.name = link.name.clone();
                slot.events = slot.events.saturating_add(1);
            }
        }
    }

    // ⭐ Hand every controller back before the thread dies.
    //
    // ❗ The link lives in the DONGLE. Exiting without disconnecting leaves the
    // pad believing it is still connected, after which it neither pages nor
    // advertises nor answers an inquiry — invisible in every direction, with
    // power-cycling the controller as the user's only recourse. This is the
    // difference between "quit and relaunch" working and not.
    if let (Some(r), false) = (radio.as_ref(), links.is_empty()) {
        eprintln!("[bt-classic] disconnecting {} controller(s)", links.len());
        r.with_dongle(|d| {
            for l in &links {
                let _ = d.disconnect(l.conn);
            }
            // Let the disconnections actually go out; dropping the handle the
            // instant after queueing them can lose them.
            let deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < deadline {
                match d.read_event_timeout(Duration::from_millis(50)) {
                    Ok(Some(_)) => {}
                    _ => break,
                }
            }
        });
    }
}

/// Find a controller in pairing mode and bond it.
///
/// ⭐ Picks by Class of Device, and only ever an unpaired one. Bonding a
/// controller REPLACES the host it belongs to, so choosing the wrong device
/// here means walking to a console and re-pairing it — the reason this is one
/// deliberate button and not something that happens on its own.
fn run_pairing(radio: &flexinput_btle::radio::Radio, shared: &Arc<Shared>) {
    let set = |p: PairPhase| {
        if let Ok(mut g) = shared.pair_phase.lock() {
            *g = p;
        }
    };
    set(PairPhase::Searching);
    let _ = radio.with_dongle(|d| d.set_inquiry_mode(0x01));
    let known = keystore::load();

    // ⭐ Stop inquiring the INSTANT a new gamepad answers.
    //
    // ❗ A device answers a page only while it is page-scanning, and a
    // controller in pairing mode cycles through that state rather than sitting
    // in it. Running the inquiry to its full length and paging afterwards
    // spends exactly the window in which the answer was easy — which is why a
    // controller that had plainly just replied to the inquiry then failed to be
    // paged with `Page Timeout`. Cancelling early puts the page out while it is
    // still listening for one.
    // ⭐ ONE lease across inquiry AND page. Releasing between them would let
    // the shared reader consume the very Connection Complete the page is
    // waiting for — and an inquiry that has to be cancelled cleanly before a
    // page can go out is a single conversation, not two.
    let lease = radio.exclusive();
    let dongle = lease.dongle();
    // Recorded WITH the bond: a key only works for the adapter that made it.
    let adapter = dongle.read_bd_addr().ok();
    let found = dongle
        .inquiry_until(8.0, &mut |r| {
            r.looks_like_a_gamepad()
                && !known.contains_key(&keystore::format_addr(r.address))
        })
        .unwrap_or_default();
    // ⭐ Every answer, not just the ones that qualify. "No controller found"
    // has two very different causes — nothing on the air, or something on the
    // air that was filtered out — and they need opposite fixes.
    if trace() {
        eprintln!("[bt-classic] inquiry heard {} device(s)", found.len());
        for r in &found {
            eprintln!(
                "[bt-classic]   {} class {:02x?} rssi {:?} gamepad={} known={}",
                keystore::format_addr(r.address),
                r.class_of_device,
                r.rssi,
                r.looks_like_a_gamepad(),
                known.contains_key(&keystore::format_addr(r.address)),
            );
        }
    }

    // ⛔ **Already-paired controllers are CANDIDATES, not exclusions.**
    //
    // This used to filter them out entirely, on the reasoning that re-bonding a
    // working pad by accident would be bad. The effect was that the single case
    // most in need of this button — a stored key that no longer matches, after
    // a dongle swap, a re-pair on a console, or a key file copied between
    // machines — was the one case it refused to handle. The user held Sync, the
    // controller answered the inquiry, and pairing reported "No new controller
    // found" about a controller sitting right in front of them.
    //
    // The original worry is met by ORDERING rather than by exclusion: an
    // unpaired pad always outranks a paired one, so pairing a genuinely new
    // controller still cannot grab a working one, and within each group the
    // strongest signal wins — the pad the user just picked up is the near one.
    let mut pads: Vec<_> = found.iter().filter(|r| r.looks_like_a_gamepad()).collect();
    pads.sort_by_key(|r| {
        let already = known.contains_key(&keystore::format_addr(r.address));
        (already, -(r.rssi.unwrap_or(-127) as i32))
    });

    let Some(r) = pads.first() else {
        set(PairPhase::Failed(
            "No controller found. Hold its Sync button until the lights run, then try again."
                .into(),
        ));
        return;
    };
    let addr = r.address;
    let text = keystore::format_addr(addr);
    set(PairPhase::Pairing(text.clone()));

    let mut quiet = |_: &str| {};
    // ⭐ Retried, because a Page Timeout is an ordinary event rather than a
    // verdict. The remote has to be page-scanning at the moment the page lands,
    // and a controller alternating between scanning and paging its old host
    // will miss some attempts however well timed. Three tries turns a coin flip
    // into a near certainty; reporting the first failure to the user does not.
    let mut outcome = Err(String::new());
    for attempt in 1..=3 {
        match dongle.page_and_pair(
            addr,
            r.page_scan_repetition_mode,
            r.clock_offset,
            None,
            Duration::from_secs(12),
            &mut quiet,
        ) {
            Ok(link) => {
                outcome = Ok(link);
                break;
            }
            Err(e) => {
                let msg = e.to_string();
                // Only a timeout is worth retrying. A refusal is a decision the
                // remote made, and asking again just annoys it.
                let retryable = msg.contains("0x04") || msg.contains("timed out");
                outcome = Err(msg);
                if !retryable || attempt == 3 {
                    break;
                }
                set(PairPhase::Pairing(format!("{text} (attempt {})", attempt + 1)));
                std::thread::sleep(Duration::from_millis(600));
            }
        }
    }
    match outcome {
        Ok(link) => {
            let name = dongle
                .remote_name(addr, r.page_scan_repetition_mode, r.clock_offset)
                .ok()
                .filter(|n| !n.is_empty());
            let label = name.clone().unwrap_or_else(|| text.clone());
            match link.link_key {
                Some(k) => match keystore::put(addr, k, name.as_deref(), adapter) {
                    Ok(_) => set(PairPhase::Done(label)),
                    // The bond already replaced the controller's old host, so a
                    // key that did not save is a real loss, not a retry.
                    Err(e) => set(PairPhase::Failed(format!(
                        "Paired, but the key could not be saved: {e}"
                    ))),
                },
                None => set(PairPhase::Failed("Paired but no link key arrived".into())),
            }
            let _ = dongle.disconnect(link.conn_handle);
        }
        Err(e) => set(PairPhase::Failed(format!(
            "{text} did not pair after 3 tries: {e}"
        ))),
    }
}

/// Finish a link the REMOTE started: authenticate, encrypt, open HID.
///
/// The same path as [`connect`] minus the paging — see `NO_PAGE`.
fn adopt(
    sub: &flexinput_btle::radio::Subscriber,
    addr: [u8; 6],
    key: [u8; 16],
    cid_base: u16,
) -> Result<Link, String> {
    bring_up(sub, addr, key, cid_base, flexinput_btle::NO_PAGE, 0x0000)
}

/// Page a known device and bring its HID interrupt channel up.
fn connect(
    sub: &flexinput_btle::radio::Subscriber,
    addr: [u8; 6],
    key: [u8; 16],
    cid_base: u16,
) -> Result<Link, String> {
    // ⭐ Page scan repetition mode R2 and an UNKNOWN clock offset.
    //
    // A bonded controller that is simply switched on is not discoverable, so
    // there is no fresh inquiry result to take these from. R2 with a zero
    // offset is the conservative pair: the radio listens across the full
    // page-scan window instead of assuming it knows when the remote wakes.
    bring_up(sub, addr, key, cid_base, 0x02, 0x0000)
}

fn bring_up(
    sub: &flexinput_btle::radio::Subscriber,
    addr: [u8; 6],
    key: [u8; 16],
    cid_base: u16,
    psrm: u8,
    clock: u16,
) -> Result<Link, String> {
    // Silent normally; under trace, the whole conversation. A setup that
    // fails at one step is unreadable without the steps that came before it.
    // See `ADOPT_PATIENCE`: a page is rationed, an incoming link is not.
    let patience = if psrm == flexinput_btle::NO_PAGE {
        ADOPT_PATIENCE
    } else {
        CONNECT_PATIENCE
    };
    let mut quiet = |m: &str| {
        if trace() {
            eprintln!("[bt-classic]   {m}");
        }
    };
    // ⭐ ONE lease for accept/page, pairing and both L2CAP channels. Each step
    // waits for a reply, and a lease released in between would hand those
    // replies to the shared reader instead.
    // ⛔ Reclaiming, not plain. The events of this very conversation may
    // already be on the bus — see `exclusive_reclaiming`.
    let lease = sub.exclusive_reclaiming();
    let dongle = lease.dongle();
    // An incoming link has to be accepted from INSIDE the lease, or its
    // `Connection Complete` can be broadcast before anyone is waiting for it.
    if psrm == flexinput_btle::NO_PAGE {
        dongle.accept_connection(addr).map_err(|e| e.to_string())?;
    }
    let link = dongle
        .page_and_pair(addr, psrm, clock, Some(key), patience, &mut quiet)
        .map_err(|e| e.to_string())?;
    // ⛔ **A key we were just given is SAVED, not dropped on the floor.**
    //
    // `page_and_pair` re-pairs from scratch whenever the remote asks it to —
    // which a controller sitting in Sync mode always does — and it hands the
    // new key back on the link. Only the pairing button ever stored one, so
    // every other path completed a real bond and then discarded the half of it
    // the host is responsible for keeping.
    //
    // The result is a controller that pairs perfectly and can never reconnect
    // again: its flash holds the new key, our file still holds the old one, and
    // every reconnection afterwards fails authentication with `0x05` — key
    // missing — from a host that is certain it has the key. Traced end to end;
    // it is what broke this bond in the first place.
    if let Some(fresh) = link.link_key.filter(|k| *k != key) {
        let adapter = dongle.read_bd_addr().ok();
        match keystore::put(addr, fresh, None, adapter) {
            Ok(_) => eprintln!(
                "[bt-classic] {} issued a new link key — saved",
                keystore::format_addr(addr)
            ),
            // Loud: the bond on the controller has ALREADY been replaced, so a
            // key that failed to save means re-pairing, not retrying.
            Err(e) => eprintln!(
                "[bt-classic] ⚠ {} re-paired but its new key could NOT be saved: {e}",
                keystore::format_addr(addr)
            ),
        }
    }
    // Control before interrupt — the HID profile expects that order and some
    // devices refuse the reverse.
    // ⭐ ONE path, whichever side called. `l2cap_hid` gives the remote a head
    // start and asks for anything it does not offer, so this no longer has to
    // guess which end of the link intends to open the channels — a guess that
    // was wrong in one direction or the other on every reconnection.
    let (control, interrupt) = dongle
        .l2cap_hid(link.conn_handle, cid_base, Duration::from_secs(6), &mut quiet)
        .map_err(|e| e.to_string())?;
    Ok(Link {
        addr,
        conn: link.conn_handle,
        control,
        interrupt,
        last: Instant::now(),
        name: None,
        reported: false,
    })
}

fn device_id(addr: [u8; 6]) -> String {
    format!("btc:switch_pro:{}", addr.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

impl DeviceBackend for ClassicBtBackend {
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        let pads = self.shared.pads.lock().unwrap();
        pads.values()
            .filter(|p| p.last.elapsed() < STALE)
            .map(|p| PhysicalDevice {
                id: device_id(p.address),
                // ⭐ The controller's OWN name, captured at pair time. A
                // device list that reads "Pro Controller" beats one that reads
                // a hard-coded guess, and with several pads it is the only way
                // to tell them apart.
                display_name: p.name.clone().unwrap_or_else(|| "Pro Controller (dongle)".into()),
                kind: ControllerKind::SwitchPro,
                outputs: layouts::outputs_for(ControllerKind::SwitchPro),
                inputs: layouts::inputs_for(ControllerKind::SwitchPro),
                // No Windows HID node exists for it — the OS never bound a
                // driver, which is the whole point — so there is nothing for
                // HidHide to mask and nothing to look up.
                instance_path: None,
                vid: None,
                pid: None,
            })
            .collect()
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        let mut pads = self.shared.pads.lock().unwrap();
        pads.values_mut()
            .map(|p| {
                let n = std::mem::take(&mut p.events);
                (device_id(p.address), n)
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        let pads = self.shared.pads.lock().unwrap();
        let mut out = Vec::with_capacity(pads.len() * 32);
        for p in pads.values().filter(|p| p.last.elapsed() < STALE) {
            let dev = device_id(p.address);
            let g = &p.reading;
            for (pin, v) in [
                ("gyro_x", g.gyro_x),
                ("gyro_y", g.gyro_y),
                ("gyro_z", g.gyro_z),
                ("accel_x", g.accel_x),
                ("accel_y", g.accel_y),
                ("accel_z", g.accel_z),
            ] {
                out.push((dev.clone(), pin.into(), Signal::Float(v)));
            }
            if let Some(sb) = g.switch_buttons {
                push_switch_pro_buttons(&mut out, &dev, &sb);
            }
            if let Some(b) = g.battery {
                out.push((dev.clone(), "battery".into(), Signal::Float(b)));
            }
        }
        out
    }
}

/// Pins a classic controller publishes, for tests and for the device list.
pub fn outputs() -> Vec<DevicePin> {
    layouts::outputs_for(ControllerKind::SwitchPro)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One IMU frame from the real capture, repeated three times — the report
    /// carries three and the parser averages them, so identical frames give
    /// exactly the single-frame value.
    const IMU_FRAME: [u8; 12] = [
        0x90, 0xfd, 0x09, 0x00, 0x9f, 0x10, // accel  -624,   9, 4255
        0xfd, 0xff, 0x02, 0x00, 0xfc, 0xff, // gyro     -3,   2,   -4
    ];

    /// A real `0x30` report, captured off the interrupt channel of a Pro
    /// Controller connected through the dongle.
    fn real_report() -> Vec<u8> {
        let mut r = vec![
            0x30, // report id
            0xa2, // timer
            0x60, // battery / connection info
            0x00, 0x00, 0x00, // buttons: right, shared, left — none held
            0xec, 0xe7, 0x7e, // left stick, 12-bit packed
            0x3a, 0xb8, 0x79, // right stick
            0x0c, // vibrator ack
        ];
        for _ in 0..3 {
            r.extend_from_slice(&IMU_FRAME);
        }
        assert_eq!(r.len(), 49, "a 0x30 report is 49 bytes");
        r
    }

    /// ⭐ The captured report must decode to a controller sitting still on a
    /// desk — which is what it was.
    ///
    /// This is the strongest assertion available without hardware: the
    /// accelerometer measures a constant-magnitude vector, so checking that the
    /// decode yields ONE GRAVITY validates the field offsets, the byte order
    /// and the scale factor together. Any one of them wrong and the magnitude
    /// is wrong.
    #[test]
    fn the_captured_report_decodes_to_one_gravity() {
        let r = real_report();
        let g = parse_switch_pro_report(&r).expect("a real 0x30 report must parse");
        let mag = (g.accel_x * g.accel_x + g.accel_y * g.accel_y + g.accel_z * g.accel_z).sqrt();
        // Pins are normalised so 1.0 = 8 g, making one gravity 0.125.
        assert!(
            (mag - 0.125).abs() < 0.02,
            "accel magnitude {mag} is not one gravity — offsets, byte order or              scale is wrong (raw was -624, 9, 4255)",
        );
        // Resting on a desk: gravity almost entirely on the vertical axis.
        assert!(g.accel_z > 0.11, "gravity not on the vertical axis: {}", g.accel_z);
        // And a still controller reads a near-zero rate.
        let spin = g.gyro_x.abs() + g.gyro_y.abs() + g.gyro_z.abs();
        assert!(spin < 0.01, "a resting controller reported {spin} of rotation");
    }

    /// ⛔ The HID transaction header is NOT part of the report.
    ///
    /// `0xa1` prefixes every input report on the interrupt channel. Handing it
    /// to the parser shifts every field by one byte, and the result is not an
    /// error — it is a report that decodes to plausible nonsense, which is far
    /// harder to notice than a refusal.
    #[test]
    fn the_transaction_header_must_be_stripped_first() {
        let r = real_report();
        let mut with_header = vec![0xA1u8];
        with_header.extend_from_slice(&r);
        assert!(
            parse_switch_pro_report(&with_header).is_none(),
            "a report parsed WITH its transaction header still attached",
        );
        assert!(parse_switch_pro_report(&with_header[1..]).is_some());
    }

    /// Buttons decode by physical position, not by Nintendo's labels.
    #[test]
    fn buttons_land_on_positional_pins() {
        let mut r = real_report();
        r[3] = 0x08; // right byte, bit 3 = A
        let g = parse_switch_pro_report(&r).expect("parses");
        let sb = g.switch_buttons.expect("switch buttons present");
        assert!(sb.btn_a, "A not decoded");
        let mut out = Vec::new();
        push_switch_pro_buttons(&mut out, "test", &sb);
        let east = out.iter().find(|(_, p, _)| p == "btn_east").expect("btn_east pin");
        assert!(matches!(east.2, Signal::Bool(true)), "A must publish as EAST");
        let south = out.iter().find(|(_, p, _)| p == "btn_south").expect("btn_south pin");
        assert!(matches!(south.2, Signal::Bool(false)), "A must not publish as south");
    }

    #[test]
    fn a_device_id_is_stable_and_address_tagged() {
        let a = [0xda, 0x2d, 0x16, 0x0f, 0x01, 0x69];
        assert_eq!(device_id(a), "btc:switch_pro:da2d160f0169");
        // Two controllers must not collide.
        assert_ne!(device_id(a), device_id([0; 6]));
    }

    /// ⛔ The dongle selector must not silently fall back on a typo.
    #[test]
    fn the_dongle_selector_parses_vid_pid() {
        std::env::set_var("FLEXINPUT_BT_CLASSIC_DONGLE", "0bda:a728");
        assert_eq!(dongle_ids(), Some((0x0BDA, 0xA728)));
        std::env::set_var("FLEXINPUT_BT_CLASSIC_DONGLE", "0x1234:0x5678");
        assert_eq!(dongle_ids(), Some((0x1234, 0x5678)));
        // ⛔ With no override it is DISCOVERED, never assumed. Asserting a
        // particular Realtek here is what let the hardcoded fallback survive —
        // the test passed on the one machine where the wrong answer was right.
        std::env::remove_var("FLEXINPUT_BT_CLASSIC_DONGLE");
        assert_eq!(dongle_ids(), flexinput_btle::preferred_dongle());
    }
}
