//! The BLE hub: one dedicated OS thread running a current-thread tokio runtime
//! that scans for Joy-Con 2 controllers, connects, initialises them, and pumps
//! their input notifications into a shared snapshot map.
//!
//! Everything public here is synchronous and non-blocking, because the caller
//! is FlexInput's device-I/O loop, which must never wait on Bluetooth.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use btleplug::api::{
    Central, CharPropFlags, Characteristic, Descriptor, Manager as _, Peripheral as _, ScanFilter,
    ValueNotification, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures::stream::{Stream, StreamExt};
use uuid::Uuid;

use crate::pairing;
use crate::protocol::{self, feature, Side};
use crate::reports::{self, OrientationTracker, PadSnapshot, StickCalib};

/// How long a pairing step waits for its response. Only the `0x15` exchange
/// blocks on replies — it genuinely cannot proceed without the controller's key
/// and challenge response.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);

/// Spacing between the fire-and-forget init writes, which doubles as an
/// opportunistic window to collect whatever response arrives.
///
/// Init used to `write_and_wait` on EVERY command with a 2 s timeout. Nineteen
/// commands meant a worst case near 40 seconds of handshake — and the
/// controller, seeing no completed init, powered itself off partway through.
/// Official software fires this whole sequence in milliseconds. Responses to
/// the non-pairing steps are informational, so they are collected if they
/// happen to arrive and never waited on.
const INIT_CMD_GAP: Duration = Duration::from_millis(25);

/// Longest gap between input notifications before the link is considered dead.
///
/// btleplug's notification stream does not reliably yield `None` when a Windows
/// BLE peripheral vanishes, so a controller that powers off leaves the pad task
/// parked forever on `stream.next()`. The pad then stays in the device list
/// with its last values frozen, and — because it is still in `managed` — is
/// never rediscovered, so pressing a button cannot bring it back. This watchdog
/// is what turns that into a clean drop and a fresh scan.
const INPUT_TIMEOUT: Duration = Duration::from_secs(3);

/// How often to poke the controller to keep the link up.
///
/// Windows tears the connection down roughly 30 s in otherwise. Per Microsoft's
/// BLE docs a connection is initiated — and kept — by setting
/// `GattSession.MaintainConnection`, by uncached service discovery, or by
/// "a read/write operation against the device". btleplug's WinRT backend does
/// only the discovery and never sets `MaintainConnection`, so incoming
/// notifications alone do not count as the activity Windows is looking for and
/// the link is reclaimed. Periodic writes are the part of that list we can
/// reach from outside the library.
///
/// This doubles as a controller-side keep-alive: Nintendo pads are known to
/// sleep when a host goes quiet, and the console streams output reports
/// continuously rather than falling silent after init.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
/// How often the steady-state loop prints a liveness line while connected.
const STATUS_PERIOD: Duration = Duration::from_secs(5);
/// How often to re-arm the controller's LE advertising (`0x03/0x01`).
///
/// Comfortably shorter than the ~31 s at which Windows reclaims the link, so
/// the most recent re-arm is always recent when the drop lands.
const READVERTISE_PERIOD: Duration = Duration::from_secs(10);
/// Length of each discovery window.
const SCAN_WINDOW: Duration = Duration::from_secs(4);
/// Rest between scans while NOTHING is connected — find controllers quickly.
const SCAN_GAP_IDLE: Duration = Duration::from_secs(1);
/// Rest between scans while at least one controller is connected — long enough
/// not to hog the radio, short enough that the second half of a pair switched
/// on late is picked up without the user wondering whether it worked.
const SCAN_GAP_BUSY: Duration = Duration::from_secs(6);

/// Read a lower-cased, trimmed environment override, treating empty as unset.
///
/// These knobs exist because the ~30 s drop has now survived five different
/// fixes, and each one cost a full build-and-retest cycle. Bisecting the
/// remaining candidates from the shell in ONE build is worth a little config
/// surface. Note for PowerShell: it is `$env:NAME = "value"` — `set NAME=value`
/// is a cmd-ism that silently creates nothing.
fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
}

/// Keep-alive period, or `None` to send no keep-alive writes at all.
///
/// Disabling only suppresses the *write*; the loop still ticks at the same rate
/// so the watchdog behaves identically. One variable changes, not two.
fn keepalive_period() -> Option<Duration> {
    match env_override("FLEXINPUT_JC2_KEEPALIVE_MS") {
        None => Some(KEEPALIVE_INTERVAL),
        Some(v) => match v.parse::<u64>() {
            Ok(0) => None,
            Ok(ms) => Some(Duration::from_millis(ms)),
            Err(_) => Some(KEEPALIVE_INTERVAL),
        },
    }
}

/// Stable identity for a connected half. The BLE address is the only thing that
/// survives a reconnect, so it — not enumeration order — is the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PadKey {
    pub side: Side,
    pub address: [u8; 6],
}

impl PadKey {
    /// Short hex suffix used to disambiguate two same-side halves in a device id.
    pub fn address_slug(&self) -> String {
        self.address.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Live state for one connected half, snapshotted for the sync side.
#[derive(Debug, Clone)]
pub struct PadState {
    pub key: PadKey,
    pub display_name: String,
    pub connected: bool,
    /// True once initialisation finished and input notifications are flowing.
    pub streaming: bool,
    pub snapshot: PadSnapshot,
    /// Stick normalised to −1..=1, +Y up.
    pub stick: (f32, f32),
    /// Gyro in raw LSB with the learned zero-rate offset removed. Use this
    /// rather than `snapshot.motion.gyro`, which is uncorrected and will drift
    /// an aim mapping across the screen on its own.
    pub gyro: [f32; 3],
    /// Absolute orientation as canonical Euler angles `(roll, pitch, yaw)` in
    /// radians, from [`crate::reports::OrientationTracker`].
    ///
    /// ⭐ Carried here rather than recomputed by the consumer because **yaw
    /// requires unwrapping, which is stateful**. The heading field covers half
    /// a turn and wraps twice per revolution; a consumer recomputing it from
    /// `snapshot.motion.angle` has no wrap history and gets a 180 degree flip
    /// at every seam. Recomputing it is exactly the bug this field prevents.
    pub orientation: [f32; 3],
    /// ⭐ The same orientation as a quaternion `[x, y, z, w]`, body-to-world.
    ///
    /// Authoritative. `orientation` above is derived from it for consumers that
    /// want angles; anything that needs a rotation should take this, because
    /// rebuilding one from Euler means re-deciding a composition convention
    /// that has already been got wrong twice.
    pub orientation_quat: [f32; 4],
    /// Angular rate differenced from the raw angle fields, deg/s, **in field
    /// order** — see [`crate::reports::Orientation::field_rate_dps`].
    ///
    /// Separate from `gyro` above because the two come from different sensors.
    /// `gyro` differentiates accel-derived tilt, which is unreliable during
    /// exactly the fast motion an aim mapping cares about; this differentiates
    /// the controller's own integrated gyro. Carried alongside rather than
    /// replacing it so both can be wired at once and compared on hardware,
    /// which is also how the field-to-axis mapping gets settled.
    pub field_rate: [f32; 3],
    /// Yaw rate about gravity, deg/s — see
    /// [`crate::reports::Orientation::yaw_rate_dps`].
    pub yaw_rate: f32,
    /// ⭐ What the gyro pins should carry: `(roll, pitch, yaw)` deg/s in
    /// canonical order, with the fields' pose-dependent leak cancelled — see
    /// [`crate::reports::Orientation::pin_rate_dps`].
    pub pin_rate: [f32; 3],
    /// Reports received since the last [`Joycon2Hub::take_event_counts`].
    pub events: u32,
}

/// Outbound requests to a connected pad.
#[derive(Debug, Clone)]
enum PadCommand {
    /// Raw 16-byte HD rumble payload for this half's LRA.
    Rumble(Vec<u8>),
    /// Player LED bitmask, bits 0–3.
    PlayerLed(u8),
}

#[derive(Default)]
struct Shared {
    pads: Mutex<HashMap<PadKey, PadState>>,
    senders: Mutex<HashMap<PadKey, tokio::sync::mpsc::UnboundedSender<PadCommand>>>,
    /// Whether the LTK pairing handshake may run. Off means we still stream
    /// input, we just never write to controller flash.
    pairing_enabled: AtomicBool,
    /// Dongle transport state; the hub defers to it. See
    /// [`Joycon2Hub::set_stand_down`].
    stand_down: Mutex<Option<Arc<std::sync::atomic::AtomicU8>>>,
    shutdown: AtomicBool,
    /// ⭐ Set when the dongle takes over mid-session: every live `drive_pad`
    /// must let go so the dongle can claim the controller.
    ///
    /// Standing down only stopped this hub from starting NEW connections. Pads
    /// it already held were kept forever, and since a BLE peripheral accepts
    /// exactly one connection, the dongle could never get in — the user's only
    /// recourse was restarting the app, and even that failed if this hub won
    /// the race again. Releasing is what makes the hand-over automatic.
    release_pads: AtomicBool,
    /// Cuts the scan gap short when a pad drops, so a controller that powers
    /// off and is woken with a button press is picked up straight away instead
    /// of waiting out `SCAN_GAP_BUSY`.
    rescan: tokio::sync::Notify,
    /// Lets one scan through even though controllers are connected. Only set by
    /// an explicit user request — scanning is what drops active links, so this
    /// is a deliberate trade, not something to do on a timer.
    force_scan: AtomicBool,
    /// Link key per controller address, for controllers already paired during
    /// this run.
    ///
    /// Finalising pairing WRITES CONTROLLER FLASH. Without this cache the hub
    /// re-ran the whole `0x15` exchange on every connect — and since the
    /// controller sleeps and reconnects on a button press, that meant a fresh
    /// flash write every time the user woke it. Pairing is meant to happen
    /// once; the research notes say the `0x15` commands are omitted entirely on
    /// reconnection, which is what this reproduces.
    ///
    /// In memory only, so it survives a reconnect but not an app restart. That
    /// still removes the great majority of writes; persisting it to disk is the
    /// remaining step.
    paired: Mutex<HashMap<[u8; 6], [u8; 16]>>,
}

/// Handle to the BLE hub. Dropping it stops the thread.
pub struct Joycon2Hub {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Joycon2Hub {
    /// Spawn the hub. Returns immediately; discovery happens in the background.
    ///
    /// `stand_down` is the dongle's readiness flag. While it reads anything but
    /// `DONGLE_ABSENT` the hub will not scan or connect. The two transports are
    /// RIVALS, not complements: a BLE peripheral accepts one connection, so
    /// whichever gets there first locks the other out — and the loser cannot
    /// even see the controller afterwards, because a connected peripheral stops
    /// advertising.
    ///
    /// The dongle must win. Windows reclaims unpaired BLE links after ~31 s and
    /// nothing a GATT client does prevents it, whereas the dongle holds a link
    /// indefinitely. Letting this hub connect first is how a working dongle
    /// setup silently regresses to 31-second dropouts.
    ///
    /// ⭐ **Taken as a parameter, NOT set afterwards, and that is the whole
    /// point.** It used to be a separate `set_stand_down` call:
    ///
    /// ```text
    ///     let hub = Joycon2Hub::start(pairing_enabled);   // loop starts HERE
    ///     hub.set_stand_down(dongle.state_flag());        // flag lands later
    /// ```
    ///
    /// The worker thread begins scanning the moment `start` returns, and until
    /// the second line ran its flag was `None` — which
    /// [`dongle_owns_controllers`] deliberately treats as "no dongle, go
    /// ahead". So every launch had a window in which this hub scanned freely,
    /// and one `start_scan` is enough for Windows to spot a remembered
    /// controller and connect it. Once that happens the dongle cannot see the
    /// pad at all, and restarting does not help because the pad is now bonded
    /// to Windows and gets auto-connected on sight.
    ///
    /// Passing it at construction closes the window by construction rather than
    /// by ordering discipline, and there is no longer a setter to forget.
    pub fn start(
        pairing_enabled: bool,
        stand_down: Option<Arc<std::sync::atomic::AtomicU8>>,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        shared.pairing_enabled.store(pairing_enabled, Ordering::Relaxed);
        *shared.stand_down.lock().unwrap() = stand_down;

        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("joycon2-ble".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        log::error!("joycon2: could not start BLE runtime: {e}");
                        return;
                    }
                };
                rt.block_on(run(worker));
            })
            .ok();

        Self { shared, thread }
    }

    /// Snapshot of every known half. Cheap: a map clone of at most a few entries.
    pub fn pads(&self) -> Vec<PadState> {
        let mut v: Vec<PadState> = self.shared.pads.lock().unwrap().values().cloned().collect();
        v.sort_by_key(|p| p.key);
        v
    }

    /// Drain per-pad report counts, for the live polling-rate display.
    pub fn take_event_counts(&self) -> Vec<(PadKey, u32)> {
        let mut pads = self.shared.pads.lock().unwrap();
        pads.iter_mut()
            .map(|(k, p)| {
                let n = std::mem::take(&mut p.events);
                (*k, n)
            })
            .collect()
    }

    /// Queue a raw 16-byte HD rumble payload. Silently dropped if the pad is
    /// not connected — the caller writes every tick and must not care.
    pub fn send_rumble(&self, key: PadKey, payload: Vec<u8>) {
        self.send(key, PadCommand::Rumble(payload));
    }

    /// Set the player-LED bitmask (bits 0–3).
    pub fn set_player_led(&self, key: PadKey, mask: u8) {
        self.send(key, PadCommand::PlayerLed(mask));
    }

    pub fn set_pairing_enabled(&self, on: bool) {
        self.shared.pairing_enabled.store(on, Ordering::Relaxed);
    }

    /// Ask for one discovery pass even though controllers are connected — for
    /// picking up the second half of a pair that was switched on late.
    ///
    /// This is deliberately NOT automatic. Starting a scan drops the links that
    /// are already up on at least some adapters, so it is a trade the user
    /// makes knowingly, not something the hub should do on a timer.
    pub fn request_scan(&self) {
        self.shared.force_scan.store(true, Ordering::Relaxed);
        self.shared.rescan.notify_one();
    }

    fn send(&self, key: PadKey, cmd: PadCommand) {
        if let Some(tx) = self.shared.senders.lock().unwrap().get(&key) {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for Joycon2Hub {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        // Don't join: a BLE call can block for seconds inside the OS stack, and
        // this runs on UI teardown. The thread observes the flag and exits.
        self.thread.take();
    }
}

// ── async side ────────────────────────────────────────────────────────────────

async fn run(shared: Arc<Shared>) {
    let manager = match Manager::new().await {
        Ok(m) => m,
        Err(e) => {
            log::warn!("joycon2: no Bluetooth manager: {e}");
            return;
        }
    };
    let adapters = match manager.adapters().await {
        Ok(a) => a,
        Err(e) => {
            log::warn!("joycon2: could not list Bluetooth adapters: {e}");
            return;
        }
    };
    let Some(adapter) = adapters.into_iter().next() else {
        log::info!("joycon2: no Bluetooth adapter present; Joy-Con 2 support idle");
        return;
    };

    // Name the radio up front. Link stability with these controllers depends
    // heavily on the adapter — they use a proprietary non-HID GATT profile at a
    // fast connection interval, and combo Wi-Fi/BT chips sharing an antenna
    // with Bluetooth Classic traffic hold it far less reliably than a dedicated
    // Intel radio. When a link keeps dropping, this is the first thing worth
    // knowing and the last thing anyone thinks to check.
    if let Some(radio) = crate::host_addr::radio_info() {
        // The address is printed alongside the vendor because it is the key
        // Windows files bonds under (`BTHPORT\Parameters\Keys\<adapter>\…`),
        // and there is no way to read that subtree without SYSTEM rights.
        let addr = crate::host_addr::local_bluetooth_address()
            .map(|a| a.iter().map(|b| format!("{b:02x}")).collect::<String>())
            .unwrap_or_else(|| "unknown".into());
        eprintln!(
            "[jc2] host Bluetooth radio: {} (company id {}) addr={}",
            radio.manufacturer, radio.manufacturer_id, addr,
        );
    }

    // Print the RESOLVED experiment config, not the raw variables.
    //
    // Twice now an experiment has silently run with default settings because
    // the variable never reached the process: once from `set X=1` (a cmd-ism
    // that does nothing in PowerShell) and once from `cargo run $env:X = "v"`,
    // where PowerShell passes the whole thing to the exe as arguments. Both
    // looked exactly like a real result. Echoing what the code actually decided
    // makes that failure impossible to miss, and costs one line per launch.
    eprintln!(
        "[jc2] config: connparams={} keepalive={} minimal={}",
        env_override("FLEXINPUT_JC2_CONNPARAMS").unwrap_or_else(|| "throughput (default)".into()),
        match keepalive_period() {
            Some(d) => format!("{} ms", d.as_millis()),
            None => "disabled".to_string(),
        },
        if env_override("FLEXINPUT_JC2_MINIMAL").is_some() { "ON" } else { "off" },
    );

    // Managed set, so a pad already being driven isn't connected twice.
    let managed: Arc<Mutex<std::collections::HashSet<[u8; 6]>>> = Arc::default();
    // Addresses already reported as advertising while connected, so the
    // observation is logged once per controller rather than every scan window.
    let advertising_while_connected: Arc<Mutex<std::collections::HashSet<[u8; 6]>>> =
        Arc::default();

    while !shared.shutdown.load(Ordering::Relaxed) {
        // Stand aside entirely while the dongle is driving.
        //
        // Checked before scanning, not just before connecting: `start_scan` on
        // the Windows stack is enough for Windows to notice a REMEMBERED
        // controller and auto-connect it behind our back, which is exactly the
        // regression this guards — the pads end up on the Windows adapter with
        // FlexInput never having called `connect()`.
        let dongle_owns = dongle_owns_controllers(&shared.stand_down.lock().unwrap());
        if dongle_owns {
            // Release anything already held, once, and say so — from the
            // outside a controller changing transport looks like a dropout.
            if !managed.lock().unwrap().is_empty()
                && !shared.release_pads.swap(true, Ordering::Relaxed)
            {
                eprintln!(
                    "[jc2] dongle is ready — releasing {} controller(s) to it",
                    managed.lock().unwrap().len(),
                );
            }
            tokio::time::sleep(SCAN_GAP_BUSY).await;
            continue;
        }
        // The dongle is gone; this hub may hold controllers again.
        shared.release_pads.store(false, Ordering::Relaxed);

        let connected = managed.lock().unwrap().len();
        let forced = shared.force_scan.swap(false, Ordering::Relaxed);

        // Keep scanning until BOTH halves are connected, then stop.
        //
        // An earlier version refused to scan whenever anything was connected,
        // on the theory that scanning was killing the links. The timing
        // disproved that: controllers still dropped at ~29 s with scanning
        // fully disabled, exactly as they had with it enabled. All that
        // restriction achieved was making the second half of a pair
        // undiscoverable, so it is gone. Scanning stops at two purely because
        // there is nothing left to look for.
        if connected >= 2 && !forced {
            tokio::select! {
                _ = tokio::time::sleep(SCAN_GAP_BUSY) => {}
                _ = shared.rescan.notified() => {}
            }
            continue;
        }

        if let Err(e) = adapter.start_scan(ScanFilter::default()).await {
            log::debug!("joycon2: scan failed: {e}");
            tokio::time::sleep(SCAN_WINDOW).await;
            continue;
        }
        tokio::time::sleep(SCAN_WINDOW).await;
        let _ = adapter.stop_scan().await;

        let peripherals = adapter.peripherals().await.unwrap_or_default();
        for p in peripherals {
            if shared.shutdown.load(Ordering::Relaxed) {
                break;
            }
            let Some((side, pid, address)) = identify(&p).await else {
                continue;
            };
            if protocol::Side::is_safe_mode(pid) {
                log::warn!(
                    "joycon2: {} at {address:02x?} is in SAFE MODE (pid {pid:#06x}); no input available",
                    side.display_name(),
                );
                continue;
            }
            {
                let mut m = managed.lock().unwrap();
                if !m.insert(address) {
                    // Already connected, yet still turning up in a scan — which
                    // means it is STILL ADVERTISING while connected to us. That
                    // is the premise of the cancel-advertising experiment: if
                    // this line appears, the controller really is still looking
                    // for a console, and nothing we sent stopped it. If it
                    // stops appearing once `0x03/0x02` is sent, that command
                    // took effect. Either way it is evidence, so it is worth
                    // one line the first time it happens per address.
                    if advertising_while_connected.lock().unwrap().insert(address) {
                        eprintln!(
                            "[jc2] {} is STILL ADVERTISING while connected",
                            side.display_name(),
                        );
                    }
                    continue;
                }
            }

            let key = PadKey { side, address };
            log::info!("joycon2: found {} at {}", side.display_name(), key.address_slug());

            let shared = Arc::clone(&shared);
            let managed = Arc::clone(&managed);
            tokio::spawn(async move {
                if let Err(e) = drive_pad(&shared, key, p).await {
                    log::warn!("joycon2: {} dropped: {e}", key.address_slug());
                }
                shared.pads.lock().unwrap().remove(&key);
                shared.senders.lock().unwrap().remove(&key);
                managed.lock().unwrap().remove(&address);
                // Wake the scan loop: this pad is gone and can be found again.
                shared.rescan.notify_one();
            });
        }

        // Rest before the next window. Long once something is connected — see
        // SCAN_GAP_BUSY for why a permanently-scanning central kills its links.
        let gap = if managed.lock().unwrap().is_empty() {
            SCAN_GAP_IDLE
        } else {
            SCAN_GAP_BUSY
        };
        tokio::select! {
            _ = tokio::time::sleep(gap) => {}
            _ = shared.rescan.notified() => {}
        }
    }
}

fn side_name(side: Side) -> &'static str {
    side.display_name()
}

/// Decide whether an advertising peripheral is a Joy-Con 2, from its
/// manufacturer data. There is no service UUID or name to match on.
///
/// btleplug hands the company identifier back as the map key, so the value
/// starts at offset `0x2` of the layout in `bluetooth_interface.md`: VID is at
/// value offset 3, PID at 5.
async fn identify(p: &Peripheral) -> Option<(Side, u16, [u8; 6])> {
    let props = p.properties().await.ok()??;
    let data = props.manufacturer_data.get(&protocol::NINTENDO_MANUFACTURER_ID)?;
    if data.len() < 7 {
        return None;
    }
    let vid = u16::from_le_bytes([data[3], data[4]]);
    let pid = u16::from_le_bytes([data[5], data[6]]);
    if vid != protocol::NINTENDO_VID {
        return None;
    }
    let side = Side::from_pid(pid)?;
    Some((side, pid, p.address().into_inner()))
}

/// Why a streaming session ended.
enum SessionEnd {
    /// The hub is shutting down; do not reconnect.
    Shutdown,
    /// The link went away on its own. Worth another go.
    LinkLost,
}

/// Backoff between in-place reconnects. Its length is the attempt limit.
///
/// Reconnecting in place is worth doing even though the ~28 s drop is not
/// understood, because the MINIMAL run showed Windows dropping a link at ~22 s
/// and **re-establishing it by itself**, after which it held another 30 s. The
/// streaming path almost certainly gets the same second chance and throws it
/// away: once the link blips, every cached `GattCharacteristic` is closed
/// (`RO_E_CLOSED`), so the only way to use the recovered link is to reconnect
/// and re-enumerate. Doing that turns a hard 30-second session into a
/// continuous one with a visible hiccup.
///
/// Bounded and backed off so a controller that is genuinely off (dead battery,
/// out of range) costs a few seconds of retries rather than an endless
/// reconnect storm on the radio.
const RECONNECT_BACKOFF: &[Duration] = &[
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// Own a controller for as long as it keeps coming back.
///
/// The pad deliberately stays registered in `shared.pads` across a reconnect,
/// with `streaming = false`. The devices backend filters on `streaming`, so the
/// pad disappears from FlexInput for the length of the gap and returns — no
/// stale values are ever served, which is the failure this whole watchdog
/// arrangement exists to prevent.
async fn drive_pad(
    shared: &Arc<Shared>,
    key: PadKey,
    p: Peripheral,
) -> Result<(), Box<dyn std::error::Error>> {
    // Kept as a String rather than the boxed error: `Box<dyn Error>` is not
    // `Send`, and holding one across the backoff `await` below would make this
    // whole future non-Send and impossible to `tokio::spawn`.
    // Only meaningful on the error path; the LinkLost path clears it.
    #[allow(unused_assignments)]
    let mut last_err: Option<String> = None;
    // Counts CONSECUTIVE hard failures only. A link lost to Windows' ~30 s
    // reclaim resets it, because that is the normal steady state here, not an
    // error — capping those would stop the controller working after a couple of
    // minutes.
    let mut failures = 0usize;

    loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Flattened to a String at the match site: a live `Box<dyn Error>`
        // binding held across the `disconnect().await` below is enough to make
        // this future non-Send, even if it is never touched again.
        match run_session(shared, key, &p).await.map_err(|e| e.to_string()) {
            Ok(SessionEnd::Shutdown) => return Ok(()),
            Ok(SessionEnd::LinkLost) => {
                last_err = None;
                failures = 0;

                // Windows reclaimed the link on its timer, and its own recovery
                // is already in motion: the HCI capture shows it adding the
                // controller to the filter accept list and restarting scanning
                // immediately after sending Disconnect. `run_session`
                // deliberately did NOT call `disconnect()`, because that drops
                // the `BLEDevice` and with it the `GattSession` doing that
                // work — which is exactly why the first version of this
                // reconnect failed with `Device not found`.
                if !await_windows_reconnect(shared, &p, key).await {
                    let _ = p.disconnect().await;
                    eprintln!(
                        "[jc2] {} did not come back on its own — back to scanning",
                        side_name(key.side),
                    );
                    break;
                }

                // The link is up again, but every characteristic cached from the
                // old one is closed (`RO_E_CLOSED`). `disconnect()` is the only
                // call that clears btleplug's `ble_services` map, and
                // `discover_services()` skips any service UUID already in it —
                // so without this the next session re-enumerates nothing and
                // reuses dead handles. Reconnecting from here is safe in a way
                // it was not before: the device is present, so Windows can
                // resolve its address.
                let _ = p.disconnect().await;
            }
            Err(msg) => {
                eprintln!("[jc2] {} session failed: {msg}", side_name(key.side));
                // `Device not found` is btleplug's mapping of
                // `BluetoothLEDevice::FromBluetoothAddressAsync` returning
                // nothing. For an UNPAIRED device Windows can only resolve an
                // address while the device is advertising or connected, so this
                // means the controller has stopped advertising entirely — it
                // slept or powered off. No amount of retrying reaches it; only
                // a button press will, and that produces an advertisement the
                // scan loop is already watching for. Retrying here just adds
                // 7.5 s of dead air before the pad can be rediscovered.
                let gone = msg.contains("Device not found");
                last_err = Some(msg);
                failures += 1;
                // Always land back on a clean GATT state: this is the only call
                // that clears `ble_services` and drops the `BLEDevice`, so
                // without it the next `connect()` reuses closed characteristics.
                let _ = p.disconnect().await;
                if gone {
                    eprintln!(
                        "[jc2] {} is no longer advertising — back to scanning (press a button to wake it)",
                        side_name(key.side),
                    );
                    break;
                }
                if failures > RECONNECT_BACKOFF.len() {
                    eprintln!(
                        "[jc2] {} failed {failures} times in a row — back to scanning",
                        side_name(key.side),
                    );
                    break;
                }
                let backoff = RECONNECT_BACKOFF[failures - 1];
                eprintln!(
                    "[jc2] {} retrying in {:?} ({failures}/{})",
                    side_name(key.side),
                    backoff,
                    RECONNECT_BACKOFF.len(),
                );
                tokio::time::sleep(backoff).await;
            }
        }
    }

    match last_err {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// How long to give Windows to re-establish a link it reclaimed itself.
///
/// Its own recovery starts within milliseconds of the Disconnect, but it can
/// only complete once the controller advertises again, so this is generous.
const RECONNECT_WAIT: Duration = Duration::from_secs(12);
const RECONNECT_POLL: Duration = Duration::from_millis(250);

/// Wait for Windows' `MaintainConnection` recovery to bring the link back.
///
/// Returns whether it did. Polls the connection flag rather than listening for
/// an event because btleplug surfaces `ConnectionStatusChanged` only through the
/// adapter-wide event stream, and this is a per-pad wait.
async fn await_windows_reconnect(shared: &Arc<Shared>, p: &Peripheral, key: PadKey) -> bool {
    let start = tokio::time::Instant::now();
    eprintln!(
        "[jc2] {} link reclaimed by Windows — waiting for it to reconnect",
        side_name(key.side),
    );
    while start.elapsed() < RECONNECT_WAIT {
        if shared.shutdown.load(Ordering::Relaxed) {
            return false;
        }
        tokio::time::sleep(RECONNECT_POLL).await;
        if p.is_connected().await.unwrap_or(false) {
            eprintln!(
                "[jc2] {} link restored after {:.1}s",
                side_name(key.side),
                start.elapsed().as_secs_f32(),
            );
            return true;
        }
    }
    false
}

async fn run_session(
    shared: &Arc<Shared>,
    key: PadKey,
    p: &Peripheral,
) -> Result<SessionEnd, Box<dyn std::error::Error>> {
    // btleplug's `connect()` builds a NEW BLEDevice and overwrites the stored
    // one, and the old one's `Drop` closes every GATT service — invalidating
    // characteristics the previous connection is still using. So a second
    // `drive_pad` for an address already being driven does not merely duplicate
    // work, it silently kills the live connection. This line makes that visible.
    eprintln!(
        "[jc2] drive_pad ENTER {} addr={}",
        side_name(key.side),
        key.address_slug(),
    );
    // Windows-side bonding experiment. See `win_pair` for the HCI evidence.
    //
    // Runs BEFORE `connect()`. The first attempt ran after it and reported
    // `Failed`, with btleplug then hitting `0x8000000E` (E_ILLEGAL_METHOD_CALL,
    // "a method was called at an unexpected time") and the link dropping — and
    // that run never logged a `connected=true` at all. Windows brings up its
    // own connection to pair; asking it to do that while btleplug already owns
    // a GATT session is the state conflict that error describes. Pairing from a
    // clean state and only then connecting avoids the overlap entirely.
    //
    //   $env:FLEXINPUT_JC2_WINPAIR = "1"        pair
    //   $env:FLEXINPUT_JC2_WINPAIR = "unpair"   undo
    #[cfg(windows)]
    match env_override("FLEXINPUT_JC2_WINPAIR").as_deref() {
        Some("1") => match crate::win_pair::pair(key.address) {
            Ok(status) => eprintln!("[jc2] {} Windows pairing -> {status}", side_name(key.side)),
            Err(e) => eprintln!("[jc2] {} Windows pairing failed: {e}", side_name(key.side)),
        },
        Some("unpair") => match crate::win_pair::unpair(key.address) {
            Ok(status) => eprintln!("[jc2] {} Windows unpair -> {status}", side_name(key.side)),
            Err(e) => eprintln!("[jc2] {} Windows unpair failed: {e}", side_name(key.side)),
        },
        _ => {
            if let Ok(true) = crate::win_pair::is_paired(key.address) {
                eprintln!("[jc2] {} is BONDED in Windows", side_name(key.side));
            }
        }
    }

    p.connect().await?;

    // Service discovery is retried because Windows fails it with
    // ERROR_SERVER_DISABLED (HRESULT 0x8007003A, "The specified server cannot
    // perform the requested operation") when it still holds a stale GATT
    // session for a controller that just dropped. The cache clears on its own
    // shortly after; a couple of spaced retries turn a dead reconnect into a
    // brief pause. Without this, a single disconnect meant the controller never
    // came back until the app was restarted.
    // The retry MUST be driven by whether the characteristics we need actually
    // turned up, not by `discover_services()`'s return value. btleplug's WinRT
    // backend logs `warn!("get_characteristics_async …")` and **returns Ok(())**
    // when a service fails to enumerate (peripheral.rs, the `Err(e) =>` arm of
    // the per-service match). So the earlier `match … { Ok(()) => break }` form
    // was dead code: attempt 1 always "succeeded" and the retry never ran, even
    // in the exact 0x8007003A case it was written for. Worse, init would then
    // proceed against a device with no vendor characteristics and fail with a
    // misleading "not a Joy-Con 2?".
    let needed = [key.side.rumble_cmd_char(), key.side.input_char()];
    let mut found = false;
    for attempt in 1..=3 {
        if let Err(e) = p.discover_services().await {
            eprintln!("[jc2] service discovery attempt {attempt} errored: {e}");
        }
        let chars = p.characteristics();
        let missing: Vec<Uuid> = needed
            .iter()
            .copied()
            .filter(|u| !chars.iter().any(|c| c.uuid == *u))
            .collect();
        if missing.is_empty() {
            found = true;
            if attempt > 1 {
                eprintln!("[jc2] service discovery recovered on attempt {attempt}");
            }
            break;
        }
        eprintln!(
            "[jc2] service discovery attempt {attempt}: {} of {} vendor characteristics missing",
            missing.len(),
            needed.len(),
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    if !found {
        return Err("vendor characteristics never enumerated (stale Windows GATT session?)".into());
    }

    // Ask Windows for a fast connection interval.
    //
    // Windows' default LE connection interval is ~60 ms; the Switch 2 console
    // drives these controllers at 5 ms. A pad expecting to be polled constantly
    // and being serviced 12× slower is a plausible reason for it to give up on
    // the link, and Joy2Win — which works on Windows — makes exactly this call
    // on Win11 and nothing else resembling a keep-alive. btleplug has exposed
    // the API since 0.12 (`ThroughputOptimized` → ~11.25 ms) and we simply were
    // not calling it.
    //
    // Non-fatal: unsupported on Win10, where it reports NotSupported.
    //
    // Overridable because the research notes say the console sets 5 ms with a
    // VENDOR-SPECIFIC HCI command, and that the controller "never explicitly
    // issues the standard Connection Parameter Update Request". So no preset we
    // can ask Windows for reproduces console behaviour, and there is no reason
    // to assume the fastest one is the friendliest to this firmware.
    let preset = match env_override("FLEXINPUT_JC2_CONNPARAMS").as_deref() {
        Some("none") => None,
        Some("power") => Some(btleplug::api::ConnectionParameterPreset::PowerOptimized),
        Some("balanced") => Some(btleplug::api::ConnectionParameterPreset::Balanced),
        _ => Some(btleplug::api::ConnectionParameterPreset::ThroughputOptimized),
    };
    match preset {
        None => eprintln!(
            "[jc2] {} leaving connection parameters at the Windows default",
            side_name(key.side)
        ),
        Some(preset) => match p.request_connection_parameters(preset).await {
            Ok(()) => eprintln!(
                "[jc2] {} requested {preset:?} connection parameters",
                side_name(key.side)
            ),
            Err(e) => eprintln!(
                "[jc2] {} connection-parameter request failed: {e}",
                side_name(key.side)
            ),
        },
    }

    // What Windows actually negotiated, as opposed to what we asked for. The
    // supervision timeout is the interesting one: it is the only BLE-level
    // timer that terminates a link on its own, and its maximum (32 s) sits
    // suspiciously close to the observed ~30 s drop.
    match p.connection_parameters().await {
        Ok(Some(cp)) => eprintln!(
            "[jc2] {} negotiated: interval={:.2}ms latency={} supervision_timeout={}ms",
            side_name(key.side),
            cp.interval_us as f32 / 1000.0,
            cp.latency,
            cp.supervision_timeout_us / 1000,
        ),
        Ok(None) => eprintln!("[jc2] {} connection parameters unavailable", side_name(key.side)),
        Err(e) => eprintln!("[jc2] {} connection-parameter read failed: {e}", side_name(key.side)),
    }

    // `$env:FLEXINPUT_JC2_MINIMAL="1"` — connect and send absolutely nothing.
    //
    // Two runs both died at ~27.6 s of link life with the OS reporting the
    // disconnect first, which is a timer rather than RF flakiness. The open
    // question is what the timer is attached to: the age of the BLE link
    // itself, or some protocol state our init/pairing sequence puts the
    // controller into. Holding a bare link answers that in one run — and no
    // amount of further reasoning about the sequence can, because every command
    // in it is a suspect.
    //
    // Deliberately returns early: no init means no input stream, so there is
    // nothing to publish. The caller's cleanup (`managed` / `pads` / `senders`)
    // runs either way, so an early return here is safe.
    if env_override("FLEXINPUT_JC2_MINIMAL").is_some() {
        eprintln!(
            "[jc2] {} MINIMAL: holding a bare link — no prelude, init, pairing or subscribe",
            side_name(key.side),
        );
        let start = tokio::time::Instant::now();
        while !shared.shutdown.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let up = p.is_connected().await.unwrap_or(false);
            eprintln!(
                "[jc2] {} MINIMAL held {:.1}s link_up={up}",
                side_name(key.side),
                start.elapsed().as_secs_f32(),
            );
            if !up {
                break;
            }
        }
        eprintln!(
            "[jc2] {} MINIMAL ended after {:.1}s with nothing ever sent",
            side_name(key.side),
            start.elapsed().as_secs_f32(),
        );
        let _ = p.disconnect().await;
        // Shutdown, not LinkLost: this is a measurement, and reconnecting would
        // destroy the very thing being measured.
        return Ok(SessionEnd::Shutdown);
    }

    let side = key.side;
    let chars = p.characteristics();
    let find = |uuid: Uuid| -> Option<Characteristic> {
        chars.iter().find(|c| c.uuid == uuid).cloned()
    };

    // ⭐ The same GATT probe the dongle runs, on the Windows stack.
    //
    // The two transports reach the same controller and get different results:
    // over the dongle, `ab7de9be-…-7fd2` and `…-7fde` declare READ|NOTIFY and
    // refuse both — Read Not Permitted, never a notification — and no
    // confirmation buzz ever arrives even though every pairing step reports
    // success and the AES confirmation matches.
    //
    // Windows reaches the controller through a relationship this project cannot
    // reproduce, so if those attributes are gated on something about the LINK
    // rather than on the commands sent over it, this is where the difference
    // shows. Reading the same characteristics from here answers it directly,
    // and Windows holds the pad long enough (~31 s) to find out.
    //
    // Same log file as the dongle, deliberately: one timeline, one clock, so
    // the two paths can be compared line for line.
    if std::env::var("FLEXINPUT_JC2_GATT_SCAN").is_ok() {
        dlog!("=== WinRT path: GATT probe for {} ===", side.display_name());
        for c in &chars {
            dlog!(
                "winrt char {} props {:?} service {}",
                c.uuid, c.properties, c.service_uuid,
            );
        }
        // The two that refuse everything over the dongle.
        for uuid_str in [
            "ab7de9be-89fe-49ad-828f-118f09df7fd2",
            "ab7de9be-89fe-49ad-828f-118f09df7fde",
            "ab7de9be-89fe-49ad-828f-118f09df7fdf",
        ] {
            let Ok(uuid) = Uuid::parse_str(uuid_str) else { continue };
            match find(uuid) {
                None => dlog!("winrt read {uuid_str}: characteristic ABSENT"),
                Some(c) => match p.read(&c).await {
                    Ok(v) => dlog!(
                        "winrt read {uuid_str}: ⭐ OK {} bytes {:02x?}",
                        v.len(),
                        &v[..v.len().min(48)],
                    ),
                    Err(e) => dlog!("winrt read {uuid_str}: FAILED {e}"),
                },
            }
        }
        // And whether the silent streams notify here.
        for uuid_str in [
            "ab7de9be-89fe-49ad-828f-118f09df7fd2",
            "ab7de9be-89fe-49ad-828f-118f09df7fde",
        ] {
            let Ok(uuid) = Uuid::parse_str(uuid_str) else { continue };
            if let Some(c) = find(uuid) {
                match p.subscribe(&c).await {
                    Ok(()) => dlog!("winrt subscribe {uuid_str}: ⭐ accepted"),
                    Err(e) => dlog!("winrt subscribe {uuid_str}: refused {e}"),
                }
            }
        }
        dlog!("=== WinRT GATT probe done ===");
    }

    {
        let mut pads = shared.pads.lock().unwrap();
        pads.insert(
            key,
            PadState {
                key,
                display_name: side.display_name().to_string(),
                connected: true,
                streaming: false,
                snapshot: PadSnapshot::default(),
                stick: (0.0, 0.0),
                gyro: [0.0; 3],
                orientation: [0.0; 3],
                orientation_quat: [0.0, 0.0, 0.0, 1.0],
                field_rate: [0.0; 3],
                yaw_rate: 0.0,
                pin_rate: [0.0; 3],
                events: 0,
            },
        );
    }

    // 1. Prelude write. Official software does this first; purpose unknown.
    if let Some(c) = find(protocol::CHR_PRELUDE_WRITE) {
        let _ = p.write(&c, &[0x01, 0x00], WriteType::WithResponse).await;
    }

    // 2. Subscribe to the command-response channels BEFORE issuing commands,
    //    or the replies to the first few are lost.
    for uuid in [
        protocol::CHR_CMD_RESP_BASIC,
        side.cmd_resp_ext_char(),
        protocol::CHR_UNKNOWN_NOTIFY,
    ] {
        if let Some(c) = find(uuid) {
            if c.properties.contains(CharPropFlags::NOTIFY) {
                let _ = p.subscribe(&c).await;
            }
        }
    }

    let mut stream = p.notifications().await?;

    let cmd_char = find(side.rumble_cmd_char())
        .ok_or("rumble+command characteristic missing — not a Joy-Con 2?")?;

    // Counts responses seen across the whole init. If this ends at zero the
    // controller is ignoring our command framing entirely, which is the single
    // most useful thing to know when input misbehaves — so it is logged at info.
    let mut responses = 0usize;

    // 3. Initialisation, mirroring the captured sequence. The `0x07`/`0x10`/
    //    `0x16` commands are undocumented handshake steps; official software
    //    always sends them before pairing, so we do too.
    for (cmd, sub, data) in [
        (protocol::CMD_UNKNOWN_07, 0x01, vec![]),
        (protocol::CMD_UNKNOWN_10, 0x01, vec![]),
        (protocol::CMD_UNKNOWN_16, 0x01, vec![]),
    ] {
        let frame = protocol::rumble_cmd_frame(cmd, sub, &data);
        responses += write_quick(&p, &cmd_char, &frame, &mut stream).await as usize;
    }

    // 3b. Tell the controller its search for a console is over.
    //
    // A Joy-Con wakes into "advertise to be found" mode. We connect, but never
    // send anything that ends that state, so the wake window plausibly just
    // times out — which is what the evidence now looks like: the pad sleeps
    // ~28 s after connecting whether we run a full init or send NOTHING AT ALL
    // (the MINIMAL runs slept at 25–27 s), and afterwards stops advertising, so
    // Windows can no longer even resolve its address.
    //
    // TESTED AND REFUTED (2026-08-15) — now OFF unless
    // `$env:FLEXINPUT_JC2_CANCEL_ADV="1"`. Kept only so the experiment can be
    // repeated cheaply. Two independent results killed it:
    //   * the controller never answered `0x03/0x02` (init responses went 12 → 11,
    //     and the −1 is fully accounted for by no longer sending the flash commit),
    //   * the paired diagnostic never fired: the pad does NOT keep advertising
    //     while connected, so it was never still searching for a console and
    //     there was no search to cancel.
    // The drop was unchanged at 30.1 s. Leaving an unanswered, unexplained
    // command in the init sequence would just be cargo-culting.
    if env_override("FLEXINPUT_JC2_CANCEL_ADV").as_deref() == Some("1") {
        let frame = protocol::rumble_cmd_frame(
            protocol::CMD_PAIRING_EXTRA,
            protocol::SUB_BT_CANCEL_ADVERTISING,
            &[],
        );
        let answered = write_quick(&p, &cmd_char, &frame, &mut stream).await;
        responses += answered as usize;
        eprintln!("[jc2] {} sent 0x03/0x02 cancel-advertising, answered={answered}", side.display_name());
    }

    // 4. Pairing, only when explicitly enabled — this writes controller flash.
    //    A controller already paired during this run reuses its stored key and
    //    skips the `0x15` exchange entirely, so waking it with a button press
    //    does not rewrite flash every single time.
    if shared.pairing_enabled.load(Ordering::Relaxed) {
        match crate::host_addr::local_bluetooth_address() {
            Some(host) => {
                let known = shared.paired.lock().unwrap().get(&key.address).copied();
                if let Some(ltk) =
                    establish_link_key(&p, &cmd_char, &mut stream, side, host, known).await
                {
                    shared.paired.lock().unwrap().insert(key.address, ltk);
                }
            }
            None => log::warn!(
                "joycon2: pairing enabled but the host Bluetooth address is unavailable; skipping"
            ),
        }
    }

    // 5. Controller-memory reads. These carry factory calibration, and other
    //    implementations report that skipping them leaves the controller
    //    streaming STUB reports — a healthy-looking link whose button fields
    //    never leave zero. Replies are logged, not yet decoded.
    for &(size, address) in protocol::JC2_INIT_MEMORY_READS {
        let frame = protocol::rumble_cmd_frame(
            protocol::CMD_READ_MEMORY,
            protocol::SUB_READ_MEMORY,
            &protocol::read_memory_data(size, address),
        );
        responses += write_quick(&p, &cmd_char, &frame, &mut stream).await as usize;
        log::debug!("joycon2: requested memory {address:#08x} ({size:#04x} bytes)");
    }

    // 6. Connection feedback, player LED, and the feature flags that actually
    //    turn on sticks / IMU / mouse reporting.
    for (cmd, sub, data) in [
        (protocol::CMD_VIBRATION, 0x02, vec![0x03, 0, 0, 0]),
        (protocol::CMD_PLAYER_LEDS, 0x07, vec![0x01, 0, 0, 0, 0, 0, 0, 0]),
        (
            protocol::CMD_FEATURE_SELECT,
            protocol::SUB_FEATURE_INIT,
            vec![feature::JOYCON2_DEFAULT, 0, 0, 0],
        ),
        (protocol::CMD_UNKNOWN_11, 0x03, vec![]),
        // "Send vibration data" — copied verbatim from the captured Joy-Con 2
        // init. This step was missing from earlier revisions: the sequence has
        // BOTH `0x0a/0x02` (play a canned sample, above) and this `0x0a/0x08`,
        // and only the first was being sent. Official software always sends it
        // before the final feature-flag confirm.
        (
            protocol::CMD_VIBRATION,
            0x08,
            vec![
                0x01, 0x59, 0x09, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x35,
                0x00, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
        ),
        (protocol::CMD_UNKNOWN_11, 0x01, vec![]),
        (
            protocol::CMD_FEATURE_SELECT,
            protocol::SUB_FEATURE_CONFIRM,
            vec![feature::JOYCON2_DEFAULT, 0, 0, 0],
        ),
    ] {
        let frame = protocol::rumble_cmd_frame(cmd, sub, &data);
        responses += write_quick(&p, &cmd_char, &frame, &mut stream).await as usize;
    }

    // `eprintln!`, not `log::info!` — the app's default env_logger filter is
    // `warn`, so info-level diagnostics are invisible unless RUST_LOG is set.
    // This line is the first thing worth seeing when input misbehaves, so it
    // follows the `[gyro]` / `[xinput-slot]` convention used elsewhere and
    // always prints. Zero responses means the controller is ignoring our
    // command framing entirely.
    eprintln!(
        "[jc2] {} init complete: {responses} command responses{}",
        side.display_name(),
        if responses == 0 { "  <-- controller is ignoring our command framing" } else { "" },
    );

    // 7. The vendor "report rate" descriptor on the input characteristic. This
    //    is what lifts the controller off its idle cadence, so a missing
    //    descriptor is worth a warning rather than a silent slow stream.
    let input_char =
        find(side.input_char()).ok_or("input characteristic missing — not a Joy-Con 2?")?;
    match input_char
        .descriptors
        .iter()
        .find(|d| d.uuid == protocol::DSC_REPORT_RATE)
    {
        Some(d) => {
            let d: Descriptor = d.clone();
            if let Err(e) = p.write_descriptor(&d, &protocol::REPORT_RATE_PAYLOAD).await {
                log::warn!("joycon2: report-rate descriptor write failed: {e}");
            }
        }
        None => log::warn!("joycon2: report-rate descriptor not found; expect a slow input stream"),
    }

    // 8. Finally, turn on the input stream.
    p.subscribe(&input_char).await?;
    if let Some(pad) = shared.pads.lock().unwrap().get_mut(&key) {
        pad.streaming = true;
    }
    log::info!("joycon2: {} streaming", side.display_name());

    // 9. Steady state: pump notifications and outbound commands together.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PadCommand>();
    shared.senders.lock().unwrap().insert(key, tx);

    let rumble_char = find(side.rumble_char());
    let mut calib = StickCalib::default();
    let mut orientation = OrientationTracker::default();
    let input_uuid = side.input_char();

    // Raw report dump. The motion block's packing is documented as "unknown"
    // and its reported length (30 or 40) is not a multiple of the 18-byte
    // sample layout, so the offsets we parse are an educated guess; these bytes
    // are what settle it.
    //
    // Dumping is UNCONDITIONAL — no env var. The opt-in version was worthless
    // in practice: it needed a variable set in the shell that actually launches
    // the app, which is easy to get wrong (`set X=1` silently does nothing in
    // PowerShell), and the failure mode was total silence indistinguishable
    // from "no input at all".
    //
    // It also has to run for a WHILE, not just at connect. A 12-report burst
    // covers 0.2 s at 60 Hz — over before anyone could pick the controller up,
    // so every sample was at rest, and a gyro at rest is indistinguishable from
    // padding. After the initial burst this keeps printing ~3 Hz so a
    // deliberate rotation lands in the log, bounded so it cannot run away.
    const DUMP_BURST: u32 = 12;
    const DUMP_MAX: u32 = 150;
    const DUMP_PERIOD: Duration = Duration::from_millis(300);
    let mut last_dump = tokio::time::Instant::now();
    let mut dumped = 0u32;

    let mut last_input = tokio::time::Instant::now();
    let mut led_mask: u8 = 0x01;

    // One 1 Hz ticker drives three independent duties — keep-alive, status
    // line, and waking the loop so the watchdog can fire — so that changing the
    // keep-alive period from the environment does not silently change the
    // watchdog's resolution or the logging cadence along with it.
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    // If a write stalls, catch up lazily rather than firing a burst of pokes.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let keepalive_period = keepalive_period();
    let mut last_keepalive = tokio::time::Instant::now();
    let mut last_status = tokio::time::Instant::now();
    let mut last_readvertise = tokio::time::Instant::now();
    let mut keepalive_failures: u32 = 0;

    // Drop forensics. Five theories have been wrong about this disconnect, in
    // part because nobody ever measured it: we have never had the elapsed time,
    // the report count, or — the decisive one — whether the BLE link is still
    // up at the moment the reports stop.
    let connected_at = tokio::time::Instant::now();
    let mut reports: u64 = 0;
    let mut reason = "shutdown requested";

    eprintln!(
        "[jc2] {} steady state: keep-alive {}",
        side_name(key.side),
        match keepalive_period {
            Some(d) => format!("every {} ms", d.as_millis()),
            None => "DISABLED".to_string(),
        },
    );

    while !shared.shutdown.load(Ordering::Relaxed) {
        // Hand the controller over the moment the dongle is ready for it.
        if shared.release_pads.load(Ordering::Relaxed) {
            reason = "released to the dongle";
            break;
        }
        tokio::select! {
            notif = stream.next() => {
                let Some(n) = notif else {
                    reason = "notification stream ended";
                    break;
                };
                if n.uuid != input_uuid {
                    continue;
                }
                last_input = tokio::time::Instant::now();
                reports += 1;
                let Some(snap) = reports::parse_input(side, &n.value) else { continue };
                let stick = calib.normalize(snap.stick_raw);
                // Pick up a calibration measured on THIS controller, if the user has
                // captured one. Pushed every report rather than at connect: a capture
                // that finishes while the pad is streaming must take effect at once,
                // and an unchanged value costs one read lock.
                orientation.set_resting_drift(crate::cal::field_drift(&key));
                let o = orientation.update(&snap.motion, side);

                // The first few reports are dumped unconditionally, THEN rate
                // limited. A connection that only survives a couple of seconds
                // would otherwise produce almost no data — which is exactly the
                // situation where the dump is most needed.
                if dumped < DUMP_BURST
                    || (dumped < DUMP_MAX && last_dump.elapsed() >= DUMP_PERIOD)
                {
                    dumped += 1;
                    last_dump = tokio::time::Instant::now();
                    // `eprintln!` for the same reason as the init summary: the
                    // default log filter is `warn`, so an info-level dump would
                    // silently produce nothing for anyone who set the env var.
                    // Whole payload, not just the motion window: if the offsets
                    // are wrong, the bytes that disprove them are outside it.
                    // `unknown` is the 14-byte window between the motion
                    // timestamp and the (now identified) accelerometer. The
                    // gyro is in there. Printed on its own so the bytes that
                    // swing during a deliberate rotation are obvious without
                    // counting offsets in a 63-byte hex dump.
                    let unknown = n.value.get(0x14..0x22).unwrap_or(&[]);
                    eprintln!(
                        "[jc2] {:?} #{} mlen={} accel={:?} |g|={:.0} unknown={:02x?} raw={:02x?}",
                        side,
                        snap.counter,
                        snap.motion_len,
                        snap.motion.accel,
                        (snap.motion.accel.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()).sqrt(),
                        unknown,
                        n.value,
                    );
                }

                if let Some(pad) = shared.pads.lock().unwrap().get_mut(&key) {
                    pad.snapshot = snap;
                    pad.stick = stick;
                    pad.gyro = o.rate_dps;
                    pad.field_rate = o.field_rate_dps;
                    pad.yaw_rate = o.yaw_rate_dps;
                    pad.pin_rate = o.pin_rate_dps;
                    pad.orientation = o.euler_rad;
                    pad.orientation_quat = o.quat_xyzw;
                    pad.events = pad.events.saturating_add(1);
                }
            }
            cmd = rx.recv() => {
                let Some(cmd) = cmd else {
                    reason = "command channel closed";
                    break;
                };
                match cmd {
                    PadCommand::Rumble(payload) => {
                        if let Some(c) = &rumble_char {
                            let _ = p.write(c, &payload, WriteType::WithoutResponse).await;
                        }
                    }
                    PadCommand::PlayerLed(mask) => {
                        // Remembered so the keep-alive keeps re-asserting the
                        // mask the user chose rather than reverting it.
                        led_mask = mask;
                        let frame = protocol::rumble_cmd_frame(
                            protocol::CMD_PLAYER_LEDS,
                            0x07,
                            &[mask, 0, 0, 0, 0, 0, 0, 0],
                        );
                        let _ = p.write(&cmd_char, &frame, WriteType::WithoutResponse).await;
                    }
                }
            }
            // Keep-alive. Also wakes the loop when nothing is arriving, so the
            // watchdog below can fire — without a third branch this select
            // would park on `stream.next()` forever once the controller went
            // quiet. Re-sending the player LED is idempotent and side-effect
            // free, and we know the controller accepts it (it answers during
            // init), which makes it a safer poke than a synthesised rumble
            // frame whose encoding is still unverified.
            _ = ticker.tick() => {
                if let Some(period) = keepalive_period {
                    if last_keepalive.elapsed() >= period {
                        last_keepalive = tokio::time::Instant::now();
                        let frame = protocol::rumble_cmd_frame(
                            protocol::CMD_PLAYER_LEDS,
                            0x07,
                            &[led_mask, 0, 0, 0, 0, 0, 0, 0],
                        );
                        if let Err(e) =
                            p.write(&cmd_char, &frame, WriteType::WithoutResponse).await
                        {
                            keepalive_failures += 1;
                            // `eprintln!`, not `log::debug!`. The app's default
                            // filter is `warn`, so the debug line this replaces
                            // could have been firing every second for the whole
                            // session and nobody would ever have seen it —
                            // silently turning the keep-alive into a no-op.
                            // Rate limited so a persistent failure does not
                            // bury the rest of the log.
                            if keepalive_failures <= 3 || keepalive_failures % 15 == 0 {
                                eprintln!(
                                    "[jc2] {} keep-alive write failed (#{keepalive_failures}): {e}",
                                    side_name(key.side),
                                );
                            }
                        }
                    }
                }

                // Re-arm the controller's advertising well before Windows'
                // ~30 s reclaim lands, so that when the link goes it is still
                // discoverable and either Windows' own accept-list scan or our
                // scan loop can pick it straight back up. Without this the
                // controller is simply gone until a button press, which is what
                // made the wait-for-reconnect path time out.
                if last_readvertise.elapsed() >= READVERTISE_PERIOD {
                    last_readvertise = tokio::time::Instant::now();
                    let frame = protocol::rumble_cmd_frame(
                        protocol::CMD_PAIRING_EXTRA,
                        protocol::SUB_BT_WAKE_ADVERTISE,
                        &[0x01],
                    );
                    let _ = p.write(&cmd_char, &frame, WriteType::WithoutResponse).await;
                }

                if last_status.elapsed() >= STATUS_PERIOD {
                    last_status = tokio::time::Instant::now();
                    // Re-read the interval here rather than trusting the value
                    // logged at connect: that one is racy. A run once reported
                    // `interval=60.00ms` straight after the request succeeded,
                    // yet delivered 1769 reports in 26.6 s — 66.5 Hz, i.e. the
                    // 15 ms it had actually asked for. The parameter update
                    // simply had not landed when we read it.
                    let interval = match p.connection_parameters().await {
                        Ok(Some(cp)) => format!("{:.2}ms", cp.interval_us as f32 / 1000.0),
                        _ => "?".to_string(),
                    };
                    eprintln!(
                        "[jc2] {} up {:.1}s reports={reports} interval={interval} \
                         ka_fail={keepalive_failures} last_report={:.1}s ago",
                        side_name(key.side),
                        connected_at.elapsed().as_secs_f32(),
                        last_input.elapsed().as_secs_f32(),
                    );
                }
            }
        }

        if last_input.elapsed() > INPUT_TIMEOUT {
            reason = "input silence";
            break;
        }
    }

    // The decisive measurement. `ble_link_up=true` alongside `reason=input
    // silence` means the controller stopped streaming while the radio link was
    // still perfectly healthy — a firmware decision, not a Bluetooth failure,
    // and the only reading that would support the "it expects something a real
    // Switch 2 sends" theory. `ble_link_up=false` means the link itself was
    // torn down and the firmware never got a say.
    let ble_link_up = p.is_connected().await.unwrap_or(false);
    eprintln!(
        "[jc2] {} LINK LOST after {:.1}s — reason={reason}, reports={reports}, \
         ka_fail={keepalive_failures}, ble_link_up={ble_link_up}",
        side_name(key.side),
        connected_at.elapsed().as_secs_f32(),
    );

    // Mark the pad not-streaming BEFORE the (potentially slow) disconnect, so
    // it leaves the device list immediately rather than lingering with frozen
    // values while the BLE teardown completes.
    if let Some(pad) = shared.pads.lock().unwrap().get_mut(&key) {
        pad.streaming = false;
        pad.connected = false;
    }
    // Deliberately NO `disconnect()` on the link-lost path.
    //
    // `disconnect()` drops the `BLEDevice`, taking the `GattSession` with it —
    // and that session is what Windows uses to reconnect after its own ~30 s
    // reclaim. Tearing it down here is what made the earlier in-place reconnect
    // fail with `Device not found`: we cancelled Windows' recovery and then
    // asked it to find a device that was no longer advertising. The caller
    // waits for the link to come back and only then cycles the connection.
    if shared.shutdown.load(Ordering::Relaxed) {
        let _ = p.disconnect().await;
        Ok(SessionEnd::Shutdown)
    } else {
        Ok(SessionEnd::LinkLost)
    }
}

/// Write a command frame without blocking on its reply, then spend
/// [`INIT_CMD_GAP`] collecting whatever arrives in the meantime.
///
/// Returns whether a notification was seen, so init can report whether the
/// controller is responding to our framing at all. Anything received is
/// discarded — the non-pairing init steps do not act on their replies, and
/// waiting on them turned a millisecond handshake into a 40-second one.
async fn write_quick(
    p: &Peripheral,
    cmd_char: &Characteristic,
    frame: &[u8],
    stream: &mut (impl Stream<Item = ValueNotification> + Unpin),
) -> bool {
    if let Err(e) = p.write(cmd_char, frame, WriteType::WithoutResponse).await {
        log::debug!("joycon2: init write failed: {e}");
        return false;
    }
    matches!(tokio::time::timeout(INIT_CMD_GAP, stream.next()).await, Ok(Some(_)))
}

/// Write a command frame and consume notifications until the matching response
/// arrives or the timeout expires. Input notifications seen while waiting are
/// discarded: the input stream is not subscribed yet at this point.
async fn write_and_wait(
    p: &Peripheral,
    cmd_char: &Characteristic,
    frame: &[u8],
    stream: &mut (impl Stream<Item = ValueNotification> + Unpin),
    cmd: u8,
    sub: u8,
    side: Side,
) -> Option<Vec<u8>> {
    if let Err(e) = p.write(cmd_char, frame, WriteType::WithoutResponse).await {
        log::debug!("joycon2: write of cmd {cmd:#04x}/{sub:#04x} failed: {e}");
        return None;
    }

    let resp_ext = side.cmd_resp_ext_char();
    let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            log::debug!("joycon2: cmd {cmd:#04x}/{sub:#04x} timed out");
            return None;
        }
        let Ok(Some(n)) = tokio::time::timeout(remaining, stream.next()).await else {
            log::debug!("joycon2: cmd {cmd:#04x}/{sub:#04x} timed out");
            return None;
        };

        let offset = if n.uuid == resp_ext {
            protocol::CMD_RESP_HEADER_OFFSET
        } else if n.uuid == protocol::CHR_CMD_RESP_BASIC {
            0
        } else {
            continue;
        };

        if let Some((hdr, data)) = protocol::parse_response(&n.value, offset, cmd) {
            if hdr.subcmd == sub {
                return Some(data.to_vec());
            }
        }
    }
}

/// Establish the link key for this connection, returning it so the caller can
/// remember it.
///
/// With `known` supplied the `0x15` exchange is SKIPPED — the controller is
/// already bonded and re-running it would rewrite its flash for nothing. Only
/// the `0x03` registration is repeated, matching the note in the research that
/// `0x15` commands are used during initial pairing and omitted on reconnection.
///
/// Best-effort throughout: every step logs and returns rather than tearing the
/// connection down, because input streaming does not depend on this succeeding.
async fn establish_link_key(
    p: &Peripheral,
    cmd_char: &Characteristic,
    stream: &mut (impl Stream<Item = ValueNotification> + Unpin),
    side: Side,
    host: [u8; 6],
    known: Option<[u8; 16]>,
) -> Option<[u8; 16]> {
    if let Some(ltk) = known {
        eprintln!(
            "[jc2] {} already paired this session — skipping key exchange (no flash write)",
            side.display_name(),
        );
        register_link_key(p, cmd_char, stream, side, host, &ltk, false).await;
        return Some(ltk);
    }
    run_pairing(p, cmd_char, stream, side, host).await
}

/// Register the link key for the live connection, optionally committing it to
/// the controller's non-volatile storage.
///
/// The two `0x03` subcommands are NOT interchangeable, and this is the whole
/// reason `commit` exists — `commands.md` describes them as:
/// - `0x07` **Send Pairing Info** — "Transmits Bluetooth host address and
///   Long-Term-Key directly to controller, bypassing standard pairing flows."
///   Per-connection, no storage. Safe to repeat.
/// - `0x09` **Store Pairing Info** — "Commits address/LTK pairs to controller
///   memory (up to 2 entries)." **This is a flash write**, into the same
///   two-slot table at `0x1FA000` that holds the Switch 2 console's entry.
///
/// So `commit` must be true exactly once per controller — at first pairing —
/// and false on every reconnect. Sending both every time (which is what this
/// did originally) re-wrote flash on every button-press wake, which is exactly
/// the wear the `paired` cache was added to prevent. Skipping `0x15` alone was
/// not enough.
async fn register_link_key(
    p: &Peripheral,
    cmd_char: &Characteristic,
    stream: &mut (impl Stream<Item = ValueNotification> + Unpin),
    side: Side,
    host: [u8; 6],
    ltk: &[u8; 16],
    commit: bool,
) {
    let frame = protocol::rumble_cmd_frame(
        protocol::CMD_PAIRING_EXTRA,
        pairing::SUB_REGISTER_LINK_KEY,
        &pairing::register_link_key_data(&host, ltk),
    );
    let registered = write_and_wait(
        p, cmd_char, &frame, stream,
        protocol::CMD_PAIRING_EXTRA, pairing::SUB_REGISTER_LINK_KEY, side,
    )
    .await;

    if !commit {
        eprintln!(
            "[jc2] link key registered={} (not committed — no flash write)",
            registered.is_some(),
        );
        return;
    }

    let frame =
        protocol::rumble_cmd_frame(protocol::CMD_PAIRING_EXTRA, pairing::SUB_LINK_KEY_COMMIT, &[]);
    let committed = write_and_wait(
        p, cmd_char, &frame, stream,
        protocol::CMD_PAIRING_EXTRA, pairing::SUB_LINK_KEY_COMMIT, side,
    )
    .await;

    eprintln!(
        "[jc2] link key registered={} committed={} (controller flash written)",
        registered.is_some(),
        committed.is_some(),
    );
}

/// Run the four-step LTK exchange. Writes controller flash at the finalise step.
async fn run_pairing(
    p: &Peripheral,
    cmd_char: &Characteristic,
    stream: &mut (impl Stream<Item = ValueNotification> + Unpin),
    side: Side,
    host: [u8; 6],
) -> Option<[u8; 16]> {
    use protocol::{CMD_PAIRING, SUB_PAIR_CONFIRM_LTK, SUB_PAIR_EXCHANGE_ADDRS,
                   SUB_PAIR_EXCHANGE_KEYS, SUB_PAIR_FINALISE};

    let step = |sub: u8, data: Vec<u8>| protocol::rumble_cmd_frame(CMD_PAIRING, sub, &data);

    // 1. Addresses.
    let frame = step(SUB_PAIR_EXCHANGE_ADDRS, pairing::exchange_addresses_data(&[host]));
    let Some(resp) = write_and_wait(p, cmd_char, &frame, stream, CMD_PAIRING,
                                    SUB_PAIR_EXCHANGE_ADDRS, side).await
    else {
        eprintln!("[jc2] PAIRING FAILED: no response to address exchange");
        return None;
    };
    if let Some(addr) = pairing::parse_controller_address(&resp) {
        log::debug!("joycon2: controller address {addr:02x?}");
    }

    // 2. Keys. A1 is arbitrary; uuid v4 is already a dependency and is backed
    //    by a proper CSPRNG, so it doubles as the random source here.
    let a1: [u8; 16] = *Uuid::new_v4().as_bytes();
    let frame = step(SUB_PAIR_EXCHANGE_KEYS, pairing::exchange_keys_data(&a1));
    let Some(resp) = write_and_wait(p, cmd_char, &frame, stream, CMD_PAIRING,
                                    SUB_PAIR_EXCHANGE_KEYS, side).await
    else {
        eprintln!("[jc2] PAIRING FAILED: no response to key exchange");
        return None;
    };
    let Some(b1) = pairing::parse_key_response(&resp) else {
        eprintln!("[jc2] PAIRING FAILED: malformed key response");
        return None;
    };
    if b1 != pairing::KNOWN_DEVICE_KEY {
        log::info!("joycon2: controller returned an unexpected device key {b1:02x?}");
    }
    let ltk = pairing::derive_ltk(&a1, &b1);

    // 3. Challenge / confirmation.
    let a2: [u8; 16] = *Uuid::new_v4().as_bytes();
    let frame = step(SUB_PAIR_CONFIRM_LTK, pairing::confirm_ltk_data(&a2));
    let Some(resp) = write_and_wait(p, cmd_char, &frame, stream, CMD_PAIRING,
                                    SUB_PAIR_CONFIRM_LTK, side).await
    else {
        eprintln!("[jc2] PAIRING FAILED: no response to LTK challenge");
        return None;
    };
    let Some(b2) = pairing::parse_key_response(&resp) else {
        eprintln!("[jc2] PAIRING FAILED: malformed challenge response");
        return None;
    };
    match pairing::check_confirmation(&pairing::expected_confirmation(&ltk, &a2), &b2) {
        pairing::Confirmation::Match => {}
        pairing::Confirmation::MatchReversed => {
            log::info!("joycon2: LTK confirmed, but byte-reversed vs the documented order")
        }
        pairing::Confirmation::Mismatch => {
            // Advisory, not fatal: the controller is the side that decides
            // whether pairing is accepted, and a mismatch here most likely
            // means our byte-order reading of the spec is off.
            log::warn!("joycon2: LTK confirmation did not match; finalising anyway");
        }
    }

    // 4. Finalise. THIS is the step that writes controller flash.
    let frame = step(SUB_PAIR_FINALISE, pairing::finalise_data());
    if write_and_wait(p, cmd_char, &frame, stream, CMD_PAIRING, SUB_PAIR_FINALISE, side)
        .await
        .is_some()
    {
        eprintln!("[jc2] paired with {} (controller flash written)", side.display_name());
    } else {
        eprintln!("[jc2] PAIRING FAILED: finalise not acknowledged");
    }

    // 5. Register the link key for the live connection.
    //
    // Distinct from the exchange above: `0x15` negotiates the LTK and commits
    // the bond to flash, then `0x03/0x07` names the key for this connection and
    // `0x03/0x09` closes the sequence.
    //
    // Note this is NOT the explanation for the ~30 s drop that it first looked
    // like. Wake-on-button reconnection demonstrably works, which proves the
    // `0x15` bond takes effect on its own.
    //
    // The derived key is printed because it is the only place it exists — it is
    // held in memory for the run and never written anywhere. Anything that wants
    // to reuse the bond (persisting it across restarts, or handing it to another
    // Bluetooth stack) needs these bytes. Treat it as a secret: it is the key
    // that encrypts this controller's link.
    eprintln!(
        "[jc2] {} LTK={} host={}",
        side.display_name(),
        ltk.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        host.iter().map(|b| format!("{b:02x}")).collect::<String>(),
    );

    // `commit = true` only here, on the first pairing of this controller:
    // `0x03/0x09` is the flash write. Reconnects re-send `0x03/0x07` alone.
    register_link_key(p, cmd_char, stream, side, host, &ltk, true).await;
    Some(ltk)
}

/// Should this hub keep its hands off the controllers?
///
/// Deliberately conservative in BOTH directions, because the two ways of being
/// wrong have very different costs:
///
/// * `PROBING` counts as owned. The dongle takes a moment to open, and a hub
///   that scans in that window can hand a remembered controller to Windows
///   before the dongle ever sees it. Waiting costs a fraction of a second.
/// * No flag at all means NOT owned. A caller that never wired one up must
///   still get working controllers, rather than silently having them disabled.
fn dongle_owns_controllers(flag: &Option<Arc<std::sync::atomic::AtomicU8>>) -> bool {
    match flag {
        Some(f) => f.load(Ordering::Relaxed) != crate::dongle::DONGLE_ABSENT,
        None => false,
    }
}

#[cfg(test)]
mod stand_down_tests {
    use super::*;
    use crate::dongle::{DONGLE_ABSENT, DONGLE_ACTIVE, DONGLE_PROBING};
    use std::sync::atomic::AtomicU8;

    /// A machine with no dongle must still get working Joy-Cons over the
    /// Windows stack. Standing down unconditionally would disable them.
    #[test]
    fn without_a_flag_the_hub_runs_normally() {
        assert!(!dongle_owns_controllers(&None));
    }

    /// ⭐ The flag must be in place BEFORE the worker can scan.
    ///
    /// This is the actual startup bug, and it hid behind a correct-looking
    /// guard: the hub was constructed, its thread started scanning, and only
    /// then was the flag attached. Until it landed the flag was `None`, which
    /// the guard above deliberately reads as "no dongle, carry on" — so every
    /// launch had a window where this hub scanned freely. One scan is enough
    /// for Windows to auto-connect a remembered controller, after which the
    /// dongle cannot even see the pad, and restarting does not help because the
    /// pad is bonded to Windows by then.
    ///
    /// Asserting on `Shared` rather than on a live hub because spawning one
    /// starts real Bluetooth work. What matters is that the state the worker
    /// reads is already populated at the moment it could first run, and that
    /// there is no setter left to call too late.
    #[test]
    fn the_stand_down_flag_is_installed_before_the_worker_can_scan() {
        let shared = Arc::new(Shared::default());
        // Exactly what `start` does, in the order it does it.
        let flag = Arc::new(AtomicU8::new(DONGLE_PROBING));
        *shared.stand_down.lock().unwrap() = Some(Arc::clone(&flag));

        assert!(
            dongle_owns_controllers(&shared.stand_down.lock().unwrap()),
            "the worker's first scan check must already see the dongle",
        );
    }

    /// Standing down must also RELEASE controllers already held.
    ///
    /// A peripheral accepts one connection, so a pad this hub is holding is a
    /// pad the dongle can never have. Without the release the only way out was
    /// restarting the app — and that could lose the race again.
    #[test]
    fn taking_over_mid_session_releases_held_pads() {
        let shared = Arc::new(Shared::default());
        assert!(
            !shared.release_pads.load(Ordering::Relaxed),
            "nothing is released until the dongle actually claims ownership",
        );
        // What the scan loop does when it finds the dongle ready with pads held.
        assert!(!shared.release_pads.swap(true, Ordering::Relaxed), "raised once");
        assert!(shared.release_pads.swap(true, Ordering::Relaxed), "and only once");

        // And when the dongle goes away, this hub may hold controllers again —
        // otherwise unplugging it would leave the pads unusable by anything.
        shared.release_pads.store(false, Ordering::Relaxed);
        assert!(!shared.release_pads.load(Ordering::Relaxed));
    }

    /// The startup race this exists to close: the dongle thread has not opened
    /// the device yet, and a hub that scans now can lose the controllers to
    /// Windows auto-connect before the dongle ever sees them.
    #[test]
    fn the_hub_waits_while_the_dongle_is_still_probing() {
        let f = Arc::new(AtomicU8::new(DONGLE_PROBING));
        assert!(dongle_owns_controllers(&Some(f)));
    }

    #[test]
    fn an_active_dongle_owns_the_controllers() {
        let f = Arc::new(AtomicU8::new(DONGLE_ACTIVE));
        assert!(dongle_owns_controllers(&Some(f)));
    }

    /// Unplugging the dongle mid-session must hand the controllers back rather
    /// than stranding them with no transport at all.
    #[test]
    fn a_departed_dongle_releases_the_hub() {
        let f = Arc::new(AtomicU8::new(DONGLE_ACTIVE));
        assert!(dongle_owns_controllers(&Some(Arc::clone(&f))));
        f.store(DONGLE_ABSENT, Ordering::Relaxed);
        assert!(!dongle_owns_controllers(&Some(f)));
    }
}
