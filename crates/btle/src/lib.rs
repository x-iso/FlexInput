//! HCI transport over a USB Bluetooth dongle bound to WinUSB.
//!
//! See `Cargo.toml` for why this crate exists. In short: the OS Bluetooth stack
//! cannot hold a Joy-Con 2 link and cannot bond with one, so FlexInput drives a
//! dedicated dongle itself.
//!
//! # USB transport layer
//!
//! The Bluetooth Core spec defines a fixed USB layout that every dongle
//! implements, so none of this is vendor-specific:
//!
//! | Direction | Endpoint | Carries |
//! |---|---|---|
//! | host → controller | control, `bmRequestType = 0x20` | HCI commands |
//! | controller → host | interrupt IN | HCI events |
//! | both | bulk | ACL data (connections) |
//!
//! Only commands and events are needed to prove the transport works; ACL comes
//! later, with connections.

use std::time::Duration;

pub mod hci;

pub use hci::{CommandComplete, Event, Opcode};

/// Errors from the dongle transport.
#[derive(Debug)]
pub enum Error {
    /// No USB device with the requested VID/PID, or it is not WinUSB-bound.
    NotFound { vid: u16, pid: u16 },
    /// libusb refused an operation. On Windows the usual cause is the device
    /// still being owned by the Bluetooth driver rather than WinUSB.
    Usb(rusb::Error),
    /// The dongle does not expose the endpoints the Bluetooth spec requires.
    NoEndpoint(&'static str),
    /// A reply arrived but could not be decoded.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound { vid, pid } => write!(
                f,
                "no USB device {vid:04x}:{pid:04x} (is it bound to WinUSB via Zadig?)"
            ),
            Error::Usb(e) => write!(f, "usb error: {e}"),
            Error::NoEndpoint(what) => write!(f, "dongle exposes no {what} endpoint"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusb::Error> for Error {
    fn from(e: rusb::Error) -> Self {
        Error::Usb(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// An open Bluetooth dongle, ready to exchange HCI traffic.
pub struct Dongle {
    handle: rusb::DeviceHandle<rusb::GlobalContext>,
    event_ep: u8,
    interface: u8,
    timeout: Duration,
}

impl Dongle {
    /// Open the dongle at `vid:pid` and claim its HCI interface.
    ///
    /// Interface 0 is the HCI interface on every spec-compliant dongle;
    /// interface 1 carries isochronous SCO (voice), which is of no interest.
    pub fn open(vid: u16, pid: u16) -> Result<Self> {
        // `mut` is required by `set_auto_detach_kernel_driver` below, which is
        // compiled out on Windows — hence unused there specifically.
        #[cfg_attr(windows, allow(unused_mut))]
        let mut handle = rusb::open_device_with_vid_pid(vid, pid)
            .ok_or(Error::NotFound { vid, pid })?;

        // Windows has no kernel driver to detach once WinUSB is bound, and this
        // is unsupported there — hence best-effort. It matters on Linux, where
        // btusb would otherwise still own the device.
        #[cfg(not(windows))]
        let _ = handle.set_auto_detach_kernel_driver(true);

        let interface = 0u8;
        handle.claim_interface(interface)?;

        let event_ep = find_interrupt_in_endpoint(handle.device(), interface)?;

        Ok(Self {
            handle,
            event_ep,
            interface,
            timeout: Duration::from_secs(2),
        })
    }

    /// The interrupt IN endpoint address events arrive on.
    pub fn event_endpoint(&self) -> u8 {
        self.event_ep
    }

    /// Send an HCI command.
    ///
    /// Commands go over the *control* endpoint, not a bulk one — a detail the
    /// Bluetooth spec fixes and which is easy to get wrong when coming from
    /// serial-transport (H4) examples, where commands carry a `0x01` type byte.
    /// Over USB the endpoint itself identifies the packet type, so there is no
    /// leading type byte here.
    pub fn send_command(&self, opcode: Opcode, params: &[u8]) -> Result<()> {
        let packet = hci::encode_command(opcode, params);
        // bmRequestType: host-to-device | class | interface.
        let request_type = rusb::request_type(
            rusb::Direction::Out,
            rusb::RequestType::Class,
            rusb::Recipient::Interface,
        );
        self.handle
            .write_control(request_type, 0x00, 0x0000, self.interface as u16, &packet, self.timeout)?;
        Ok(())
    }

    /// Read one HCI event, or `Ok(None)` if none arrived before the timeout.
    ///
    /// A timeout is not an error: dongles are quiet when idle, and callers
    /// waiting for a specific event need to distinguish "nothing yet" from
    /// "broken".
    pub fn read_event(&self) -> Result<Option<Event>> {
        let mut buf = [0u8; 260];
        match self.handle.read_interrupt(self.event_ep, &mut buf, self.timeout) {
            Ok(n) => hci::parse_event(&buf[..n]).map(Some),
            Err(rusb::Error::Timeout) => Ok(None),
            Err(e) => Err(Error::Usb(e)),
        }
    }

    /// Send a command and wait for the `Command Complete` that matches it.
    ///
    /// Unrelated events are skipped rather than treated as failures — a dongle
    /// emits plenty unprompted, and the earlier Joy-Con work was repeatedly
    /// misled by validators that locked onto the first thing they saw.
    pub fn command_sync(&self, opcode: Opcode, params: &[u8]) -> Result<CommandComplete> {
        self.send_command(opcode, params)?;
        for _ in 0..16 {
            match self.read_event()? {
                Some(Event::CommandComplete(cc)) if cc.opcode == opcode => return Ok(cc),
                Some(_) => continue,
                None => break,
            }
        }
        Err(Error::Protocol(format!(
            "no Command Complete for {opcode:?} within timeout"
        )))
    }
}

impl Dongle {
    /// Reset the controller and enable the events this stack depends on.
    ///
    /// The event-mask step is not optional housekeeping. After `HCI_Reset` the
    /// event mask reverts to the spec default `0x00001FFFFFFFFFFF`, which has
    /// **bit 61 (LE Meta) clear** — and every LE event, advertising reports
    /// included, arrives wrapped in LE Meta. Skip this and scanning appears to
    /// work, reports zero devices, and gives no error to explain why.
    pub fn reset_and_init(&self) -> Result<()> {
        let cc = self.command_sync(hci::Opcode::RESET, &[])?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!("HCI_Reset status {:?}", cc.status())));
        }

        // 0x3FFFFFFFFFFFFFFF — everything the spec defines, LE Meta included,
        // without setting reserved bits above 61.
        let event_mask: [u8; 8] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x3f];
        let cc = self.command_sync(hci::Opcode::SET_EVENT_MASK, &event_mask)?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "HCI_Set_Event_Mask status {:?}",
                cc.status()
            )));
        }

        // LE sub-events. Bit 1 is Advertising Report — the one that matters
        // here — but enabling the low byte covers connection complete and
        // connection update too, which the next stages need.
        let le_mask: [u8; 8] = [0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let cc = self.command_sync(hci::Opcode::LE_SET_EVENT_MASK, &le_mask)?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "HCI_LE_Set_Event_Mask status {:?}",
                cc.status()
            )));
        }
        Ok(())
    }

    /// Begin an active LE scan.
    ///
    /// Active (not passive) because Joy-Con 2 controllers are only identifiable
    /// by the manufacturer data in their advertisements — they publish no
    /// service UUIDs and no name — and a scan request is what reliably pulls
    /// the full payload.
    ///
    /// Interval and window are in 0.625 ms units. The defaults here scan ~30 ms
    /// out of every ~60 ms: aggressive enough to find a controller quickly
    /// without monopolising a radio that will soon also be holding a link.
    pub fn start_le_scan(&self) -> Result<()> {
        let interval: u16 = 0x0060; // 60 × 0.625 ms = 60 ms
        let window: u16 = 0x0030; //   48 × 0.625 ms = 30 ms
        let mut params = Vec::with_capacity(7);
        params.push(0x01); // active scanning
        params.extend_from_slice(&interval.to_le_bytes());
        params.extend_from_slice(&window.to_le_bytes());
        params.push(0x00); // own address type: public
        params.push(0x00); // filter policy: accept all
        let cc = self.command_sync(hci::Opcode::LE_SET_SCAN_PARAMETERS, &params)?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "LE_Set_Scan_Parameters status {:?}",
                cc.status()
            )));
        }
        // filter_duplicates = 0: a controller's payload can change between
        // advertisements, and de-duplicating hides that.
        let cc = self.command_sync(hci::Opcode::LE_SET_SCAN_ENABLE, &[0x01, 0x00])?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "LE_Set_Scan_Enable status {:?}",
                cc.status()
            )));
        }
        Ok(())
    }

    /// Stop scanning. Best-effort: reported but not fatal.
    pub fn stop_le_scan(&self) -> Result<()> {
        self.command_sync(hci::Opcode::LE_SET_SCAN_ENABLE, &[0x00, 0x00])?;
        Ok(())
    }
}

impl Drop for Dongle {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface);
    }
}

/// Locate the interrupt IN endpoint that carries HCI events.
///
/// Read from the descriptors rather than assuming the conventional `0x81`,
/// because dongles do vary and a wrong address fails as a silent timeout —
/// indistinguishable from a dongle that simply is not answering.
fn find_interrupt_in_endpoint(
    device: rusb::Device<rusb::GlobalContext>,
    interface: u8,
) -> Result<u8> {
    let config = device.active_config_descriptor()?;
    for iface in config.interfaces() {
        if iface.number() != interface {
            continue;
        }
        for desc in iface.descriptors() {
            for ep in desc.endpoint_descriptors() {
                if ep.transfer_type() == rusb::TransferType::Interrupt
                    && ep.direction() == rusb::Direction::In
                {
                    return Ok(ep.address());
                }
            }
        }
    }
    Err(Error::NoEndpoint("interrupt IN"))
}
