//! Joy-Con 2 over USB HID.
//!
//! # Why this exists alongside the Bluetooth hub
//!
//! Windows reclaims **unpaired** BLE links on a ~30 s timer. That is not a
//! guess: an HCI capture shows `HCI_Disconnect` with reason `0x16` ("Connection
//! Terminated by Local Host") exactly 31.1 s after `LE Create Connection`, with
//! notifications still arriving every 15 ms and nothing whatsoever preceding
//! it. `GattSession.MaintainConnection`, constant traffic in both directions,
//! WinRT pairing and a hand-injected link key in the registry all fail to stop
//! it. See `hub` and `win_pair` for the full account.
//!
//! Over USB none of that applies — there is no BLE link to reclaim. The
//! controller enumerates as an ordinary HID gamepad
//! (`USB\VID_057E&PID_2066`, Usage Page Generic Desktop / Usage Game Pad), so
//! this is the transport to prefer whenever a cable is available.
//!
//! # The one thing that makes it look broken
//!
//! A freshly enumerated controller streams **nothing**: healthy device, 0 Hz,
//! no inputs. That is expected. `commands.md` is explicit that `0x03/0x0D`
//! ("Initialise USB") is *required before the controller will send input
//! reports over USB*. [`init_device`] sends it, followed by `0x03/0x03`
//! ("Enable USB HID Reports").
//!
//! # Shape
//!
//! One OS thread per controller, each owning its `HidDevice` (hidapi handles
//! are `Send` but not `Sync`), plus one enumeration thread. Everything the
//! caller sees is a snapshot behind a mutex, so FlexInput's device-I/O loop
//! never blocks on USB.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::hub::{PadKey, PadState};
use crate::protocol::{self, Side};
use crate::reports::{self, OrientationTracker, PadSnapshot, StickCalib};

/// How often to look for newly plugged controllers.
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Blocking read timeout. Long enough not to spin, short enough that shutdown
/// and unplug are noticed promptly.
const READ_TIMEOUT_MS: i32 = 500;
/// Consecutive read failures tolerated before the controller is considered gone.
///
/// A single timeout is normal when the controller is idle, so this must be a
/// count of *failures*, not of empty reads.
const MAX_READ_ERRORS: u32 = 5;

#[derive(Default)]
struct Shared {
    pads: Mutex<std::collections::HashMap<PadKey, PadState>>,
    open: Mutex<HashSet<String>>,
    shutdown: AtomicBool,
}

/// USB HID transport for Joy-Con 2 controllers.
pub struct Joycon2UsbHub {
    shared: Arc<Shared>,
}

impl Default for Joycon2UsbHub {
    fn default() -> Self {
        Self::new()
    }
}

impl Joycon2UsbHub {
    /// Start scanning for wired controllers.
    pub fn new() -> Self {
        let shared: Arc<Shared> = Arc::default();
        let scan_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("jc2-usb-scan".into())
            .spawn(move || scan_loop(scan_shared))
            .expect("spawn jc2-usb-scan thread");
        Self { shared }
    }

    /// Snapshot of every connected wired half.
    pub fn pads(&self) -> Vec<PadState> {
        let mut v: Vec<PadState> = self.shared.pads.lock().unwrap().values().cloned().collect();
        v.sort_by_key(|p| p.key);
        v
    }

    /// Drain per-pad report counts, for the live polling-rate display.
    pub fn take_event_counts(&self) -> Vec<(PadKey, u32)> {
        let mut pads = self.shared.pads.lock().unwrap();
        pads.iter_mut()
            .map(|(k, p)| (*k, std::mem::take(&mut p.events)))
            .collect()
    }

    /// Ask every thread to stop. Threads exit at their next read timeout.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for Joycon2UsbHub {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn scan_loop(shared: Arc<Shared>) {
    let mut api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("[jc2-usb] hidapi unavailable: {e}");
            return;
        }
    };

    while !shared.shutdown.load(Ordering::Relaxed) {
        if let Err(e) = api.refresh_devices() {
            eprintln!("[jc2-usb] device refresh failed: {e}");
        }

        for info in api.device_list() {
            if info.vendor_id() != protocol::NINTENDO_VID {
                continue;
            }
            let Some(side) = Side::from_pid(info.product_id()) else {
                continue;
            };
            // The path, not the VID/PID, is the identity: a charging grip can
            // present two halves with the same PID on different interfaces.
            let path = info.path().to_string_lossy().to_string();
            {
                let mut open = shared.open.lock().unwrap();
                if !open.insert(path.clone()) {
                    continue;
                }
            }

            let device = match info.open_device(&api) {
                Ok(d) => d,
                Err(e) => {
                    // Interface 1 of the composite device has no driver bound
                    // and cannot be opened; only the HID interface can. Failing
                    // to open is normal, so release the path and move on.
                    log::debug!("joycon2-usb: open failed for {path}: {e}");
                    shared.open.lock().unwrap().remove(&path);
                    continue;
                }
            };

            let serial = info.serial_number().unwrap_or_default().to_string();
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("jc2-usb-pad".into())
                .spawn(move || {
                    drive_device(&shared, side, device, &path, &serial);
                    shared.open.lock().unwrap().remove(&path);
                })
                .expect("spawn jc2-usb-pad thread");
        }

        std::thread::sleep(SCAN_INTERVAL);
    }
}

/// Derive a stable [`PadKey`] address for a wired controller.
///
/// USB gives no BD_ADDR, but `PadKey` is keyed by one because that is what
/// survives a Bluetooth reconnect. A hash of the HID path and serial stands in:
/// stable for as long as the controller stays on the same port, and distinct
/// between two same-side halves, which is all the key is required to be.
fn synthetic_address(side: Side, path: &str, serial: &str) -> [u8; 6] {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    serial.hash(&mut h);
    let v = h.finish();
    let mut addr = [0u8; 6];
    addr.copy_from_slice(&v.to_le_bytes()[..6]);
    // Mark the top byte so a wired pad can never collide with a real BD_ADDR.
    addr[0] = match side {
        Side::Left => 0xFE,
        Side::Right => 0xFF,
    };
    addr
}

/// Send the two commands without which the controller stays silent.
///
/// Failures are reported but not fatal: it is worth attempting the read loop
/// anyway, because a controller already initialised by something else will
/// stream regardless.
fn init_device(device: &HidDevice, side: Side) {
    let host = crate::host_addr::local_bluetooth_address().unwrap_or([0; 6]);
    let init = protocol::usb_cmd_frame(
        protocol::CMD_PAIRING_EXTRA,
        protocol::SUB_USB_INIT,
        &protocol::usb_init_data(&host),
    );
    match device.write(&init) {
        Ok(n) => eprintln!("[jc2-usb] {} sent 0x03/0x0D initialise-usb ({n} bytes)", side.display_name()),
        Err(e) => eprintln!("[jc2-usb] {} initialise-usb failed: {e}", side.display_name()),
    }

    let enable = protocol::usb_cmd_frame(
        protocol::CMD_PAIRING_EXTRA,
        protocol::SUB_USB_ENABLE_HID_REPORTS,
        &[],
    );
    match device.write(&enable) {
        Ok(n) => eprintln!("[jc2-usb] {} sent 0x03/0x03 enable-hid-reports ({n} bytes)", side.display_name()),
        Err(e) => eprintln!("[jc2-usb] {} enable-hid-reports failed: {e}", side.display_name()),
    }
}

fn drive_device(shared: &Arc<Shared>, side: Side, device: HidDevice, path: &str, serial: &str) {
    let key = PadKey {
        side,
        address: synthetic_address(side, path, serial),
    };
    eprintln!("[jc2-usb] {} opened at {path}", side.display_name());

    init_device(&device, side);

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
                field_rate: [0.0; 3],
                events: 0,
            },
        );
    }

    let mut calib = StickCalib::default();
    let mut orientation = OrientationTracker::default();
    let mut buf = [0u8; protocol::USB_REPORT_LEN + 1];
    let mut errors = 0u32;
    let mut reports = 0u64;
    // Same rationale as the Bluetooth dump: the report layout was wrong in the
    // published spec once already, and a handful of raw frames is what settled
    // it. Bounded so it cannot run away.
    let mut dumped = 0u32;
    const DUMP_MAX: u32 = 8;

    // Silence has to be diagnosable. Without these counters a controller that
    // times out every read looks exactly like one whose reads fail, or one
    // whose reports we parse and discard — three different bugs, one symptom
    // ("connected, 0 Hz"). Reported on a timer so the log stays readable.
    let mut timeouts = 0u64;
    let mut wrong_id = 0u64;
    let mut unparsed = 0u64;
    let mut last_report_log = std::time::Instant::now();

    while !shared.shutdown.load(Ordering::Relaxed) {
        if last_report_log.elapsed() >= Duration::from_secs(5) {
            last_report_log = std::time::Instant::now();
            eprintln!(
                "[jc2-usb] {} reports={reports} timeouts={timeouts} \
                 wrong_report_id={wrong_id} unparsed={unparsed} read_errors={errors}",
                side.display_name(),
            );
        }
        match device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => {
                timeouts += 1;
                continue; // idle, not an error
            }
            Ok(n) => {
                errors = 0;
                let report_id = buf[0];
                if report_id != protocol::USB_INPUT_REPORT_ID {
                    wrong_id += 1;
                    // Dump a few of these too: an unexpected report id is far
                    // more informative than silence, and the descriptor also
                    // defines a 2-byte report (0x08 on R, 0x07 on L).
                    if wrong_id <= 4 {
                        eprintln!(
                            "[jc2-usb] {} unexpected report id={report_id:#04x} len={n} raw={:02x?}",
                            side.display_name(),
                            &buf[..n.min(16)],
                        );
                    }
                    continue;
                }
                let payload = &buf[1..n];
                if dumped < DUMP_MAX {
                    dumped += 1;
                    eprintln!(
                        "[jc2-usb] {} report id={report_id:#04x} len={} raw={:02x?}",
                        side.display_name(),
                        payload.len(),
                        payload,
                    );
                }
                let Some(snap) = reports::parse_input(side, payload) else {
                    unparsed += 1;
                    continue;
                };
                let stick = calib.normalize(snap.stick_raw);
                let o = orientation.update(&snap.motion);
                reports += 1;
                if let Some(pad) = shared.pads.lock().unwrap().get_mut(&key) {
                    pad.streaming = true;
                    pad.snapshot = snap;
                    pad.stick = stick;
                    pad.gyro = o.rate_dps;
                    pad.orientation = o.euler_rad;
                    pad.events = pad.events.saturating_add(1);
                }
            }
            Err(e) => {
                errors += 1;
                if errors >= MAX_READ_ERRORS {
                    eprintln!(
                        "[jc2-usb] {} read failed {errors}× ({e}) — treating as unplugged",
                        side.display_name(),
                    );
                    break;
                }
            }
        }
    }

    eprintln!(
        "[jc2-usb] {} closed after {reports} reports",
        side.display_name(),
    );
    shared.pads.lock().unwrap().remove(&key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_addresses_are_stable_and_side_tagged() {
        let a = synthetic_address(Side::Right, r"\\?\hid#vid_057e&pid_2066", "abc");
        let b = synthetic_address(Side::Right, r"\\?\hid#vid_057e&pid_2066", "abc");
        assert_eq!(a, b, "same device must map to the same key across scans");
        assert_eq!(a[0], 0xFF, "right half is tagged so it cannot look like a BD_ADDR");
        assert_eq!(
            synthetic_address(Side::Left, r"\\?\hid#vid_057e&pid_2067", "abc")[0],
            0xFE,
        );
    }

    #[test]
    fn two_same_side_halves_get_distinct_keys() {
        let a = synthetic_address(Side::Right, r"\path\one", "");
        let b = synthetic_address(Side::Right, r"\path\two", "");
        assert_ne!(a, b, "two right halves on one grip must not collide");
    }
}
