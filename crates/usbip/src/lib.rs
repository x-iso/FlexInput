//! Composite USB personas over USB/IP.
//!
//! A real USB DualSense is four interfaces: USB Audio Class speaker/haptics
//! out, microphone in, and HID. The UMDF driver FlexInput's plain virtual pads
//! use can present only the HID one, and no user-mode API can create an audio
//! endpoint — it needs a driver-backed USB device. So this crate serves the
//! whole composite as a USB/IP device on loopback, and usbip-win2's signed
//! virtual host controller attaches it: Windows then binds its own
//! usbaudio.sys and hidusb to a device that is a real pad at every level a
//! filter can inspect, and a game streams its authored haptic waveforms to it
//! as it would to the pad. Linux's UHID has the same limit, and the kernel's
//! own USB/IP host controller attaches the same server's device there
//! (`vhci_linux.rs`, as [`vhci`]).
//!
//! Port of HIDMaestro's usbip backend (hifihedgehog/HIDMaestro,
//! `sdk/HIDMaestro.Core/Internal/Usbip`, MIT) — its protocol, descriptor,
//! device and isochronous-engine logic, and the transport behaviour its notes
//! record — restricted to the Sony composites and fed in-process instead of
//! through shared memory.
//!
//! The same server also passes a real HID device through
//! ([`passthrough`]): the Bluetooth pads on FlexInput's own adapter, which
//! Windows then reads as the USB controllers they'd be on a cable.

pub mod descriptors;
pub mod profiles;
pub mod protocol;

mod audio;
mod device;
mod feature;
mod pacer;
#[cfg(any(windows, target_os = "linux"))]
pub mod passthrough;
mod server;
#[cfg(windows)]
pub mod vhci;
#[cfg(target_os = "linux")]
#[path = "vhci_linux.rs"]
pub mod vhci;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub use audio::PcmSink;
pub use descriptors::{AudioStream, DescriptorError, DescriptorSet};
pub use device::{OutputFrame, OutputKind};
#[cfg(any(windows, target_os = "linux"))]
pub use passthrough::{HostRequest, Passthrough, PassthroughSpec, ReportKind};
pub use server::{is_our_location, BASE_PORT, HOST, PORT_RANGE};

use device::DeviceInner;

#[derive(Debug)]
pub enum Error {
    Descriptor(DescriptorError),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Descriptor(e) => e.fmt(f),
            Error::Io(e) => write!(f, "USB/IP server: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The USB serial for the device keyed `key` (FlexInput's device id): "FX"
/// and twelve hex digits of a hash of the key. Windows keys the device's USB
/// instance on its serial, so the same key gets the same instance id, device
/// paths and container on every life — and the prefix lets FlexInput tell its
/// own composites from real pads (see [`is_own_serial`]).
pub fn serial_for(key: &str) -> String {
    // FNV-1a: stable across builds and platforms, unlike std's hasher.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("FX{:012X}", h & 0xFFFF_FFFF_FFFF)
}

/// Plug out what a crashed session left attached, once per process: before
/// the server exports its first device, or the driver's re-attach loop for a
/// dead session's device could import the new one first. See
/// [`vhci::sweep_stale`]. Every kind of device this server exports goes
/// through here, so a later one never sweeps an earlier one away.
#[cfg(any(windows, target_os = "linux"))]
pub fn sweep_stale_once() {
    static SWEPT: AtomicBool = AtomicBool::new(false);
    if !SWEPT.swap(true, Ordering::AcqRel) {
        vhci::sweep_stale(None);
    }
}

/// Whether a USB serial is one [`serial_for`] makes.
pub fn is_own_serial(serial: &str) -> bool {
    flexinput_core::own_virtual::is_own_usb_serial(serial)
}

/// One composite device, registered with the process's USB/IP server and
/// waiting for the host controller to attach it. Dropping it unregisters the device and
/// closes its connection; detach it from the host controller first, so the
/// driver closes the socket and unplugs the device cleanly.
pub struct EmulatedDevice {
    inner: Arc<DeviceInner>,
    server: Arc<server::Server>,
}

impl EmulatedDevice {
    /// Build the device a composite profile describes and register it. `key`
    /// is FlexInput's device id (it fixes the serial); `index` makes the
    /// pairing MAC unique per controller.
    pub fn new(profile_json: &str, key: &str, index: u8) -> Result<Self, Error> {
        let desc = DescriptorSet::from_profile_json(profile_json, &serial_for(key)).map_err(Error::Descriptor)?;
        let server = server::global().map_err(Error::Io)?;
        let inner = server.register(|busid, devid| Arc::new(DeviceInner::new(desc, busid, devid, index)));
        Ok(EmulatedDevice { inner, server })
    }

    /// The busid the device is exported under.
    pub fn busid(&self) -> &str {
        &self.inner.busid
    }

    /// The loopback port of the server exporting it.
    pub fn port(&self) -> u16 {
        self.server.port
    }

    pub fn descriptors(&self) -> &DescriptorSet {
        &self.inner.desc
    }

    /// Whether the host controller has imported the device.
    pub fn is_connected(&self) -> bool {
        self.inner.is_claimed()
    }

    /// Hand the host an input report, report id first.
    pub fn submit_input(&self, report: &[u8]) {
        self.inner.submit_input(report);
    }

    /// The reports the host wrote since the last call, oldest first.
    pub fn take_outputs(&self) -> Vec<OutputFrame> {
        self.inner.take_outputs()
    }

    /// Feed microphone PCM (interleaved, the stream's format). Returns the
    /// bytes accepted.
    pub fn submit_mic(&self, pcm: &[u8]) -> usize {
        self.inner.submit_mic(pcm)
    }

    /// Receive each window of speaker/haptic PCM the host streams, on the
    /// pacing thread. Keep it short; `None` stops delivery.
    pub fn set_pcm_sink(&self, sink: Option<PcmSink>) {
        self.inner.set_pcm_sink(sink);
    }

    /// Whether the host has the speaker/haptics stream open.
    pub fn output_stream_open(&self) -> bool {
        self.inner.output_stream_open()
    }
}

impl Drop for EmulatedDevice {
    fn drop(&mut self) {
        self.server.unregister(&self.inner.busid);
        self.inner.set_pcm_sink(None);
        self.inner.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    #[test]
    fn serials_are_stable_well_formed_and_recognised() {
        let a = serial_for("virtual.hm.dualsense_full");
        assert_eq!(a, serial_for("virtual.hm.dualsense_full"));
        assert_ne!(a, serial_for("virtual.hm.dualsense_full.1"));
        assert!(is_own_serial(&a), "{a}");
        assert!(!is_own_serial("FX12"));
        assert!(!is_own_serial("HM0123456789AB"));
        assert!(!is_own_serial("FX0123456789ab"), "uppercase only, as made");
    }

    fn read_reply(s: &mut TcpStream, payload: usize) -> (u32, u32, i32, i32, Vec<u8>) {
        let mut buf = vec![0u8; HEADER_SIZE + payload];
        s.read_exact(&mut buf).unwrap();
        crate::device::tests::parse_reply(&buf)
    }

    fn import(dev: &EmulatedDevice) -> TcpStream {
        let mut s = TcpStream::connect((HOST, dev.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut req = vec![0u8; 8 + BUS_ID_SIZE];
        write_op_common(&mut req, OP_REQ_IMPORT, 0);
        req[8..8 + dev.busid().len()].copy_from_slice(dev.busid().as_bytes());
        s.write_all(&req).unwrap();
        let mut rep = vec![0u8; 8 + USB_DEVICE_SIZE];
        s.read_exact(&mut rep).unwrap();
        assert_eq!(u32::from_be_bytes([rep[4], rep[5], rep[6], rep[7]]), ST_OK);
        s
    }

    // The whole path over TCP, as usbip-win2 drives it: import, enumerate,
    // read input, write output.
    #[test]
    fn a_client_imports_enumerates_and_exchanges_reports() {
        let dev = EmulatedDevice::new(profiles::DUALSENSE_COMPOSITE_JSON, "test.e2e", 0).unwrap();
        let mut s = import(&dev);
        for _ in 0..100 {
            if dev.is_connected() { break; }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(dev.is_connected());

        let get_device = Command {
            command: CMD_SUBMIT,
            seqnum: 1,
            direction: 1,
            transfer_buffer_length: 18,
            number_of_packets: NON_ISOCH,
            setup: [0x80, 0x06, 0x00, 0x01, 0, 0, 18, 0],
            ..Default::default()
        };
        s.write_all(&get_device.to_bytes()).unwrap();
        let (cmd, seq, status, actual, data) = read_reply(&mut s, 18);
        assert_eq!((cmd, seq, status, actual), (RET_SUBMIT, 1, 0, 18));
        assert_eq!(&data[8..12], &[0x4C, 0x05, 0xE6, 0x0C], "054C:0CE6");

        let read = Command {
            command: CMD_SUBMIT,
            seqnum: 2,
            direction: 1,
            ep: 4,
            transfer_buffer_length: 64,
            number_of_packets: NON_ISOCH,
            ..Default::default()
        };
        s.write_all(&read.to_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        dev.submit_input(&[0x01; 64]);
        let (_, seq, _, actual, data) = read_reply(&mut s, 64);
        assert_eq!((seq, actual), (2, 64));
        assert_eq!(data, vec![0x01; 64]);

        let write = Command {
            command: CMD_SUBMIT,
            seqnum: 3,
            direction: 0,
            ep: 3,
            transfer_buffer_length: 3,
            number_of_packets: NON_ISOCH,
            ..Default::default()
        };
        s.write_all(&write.to_bytes()).unwrap();
        s.write_all(&[0x02, 0x10, 0x20]).unwrap();
        let (_, seq, status, actual, _) = read_reply(&mut s, 0);
        assert_eq!((seq, status, actual), (3, 0, 3));
        assert_eq!(dev.take_outputs()[0].data, vec![0x10, 0x20]);
    }

    #[test]
    fn a_second_import_is_refused_busy_and_dropping_disconnects() {
        let dev = EmulatedDevice::new(profiles::DUALSENSE_EDGE_COMPOSITE_JSON, "test.busy", 1).unwrap();
        let mut first = import(&dev);
        let mut second = TcpStream::connect((HOST, dev.port())).unwrap();
        second.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut req = vec![0u8; 8 + BUS_ID_SIZE];
        write_op_common(&mut req, OP_REQ_IMPORT, 0);
        req[8..8 + dev.busid().len()].copy_from_slice(dev.busid().as_bytes());
        second.write_all(&req).unwrap();
        let mut rep = [0u8; 8];
        second.read_exact(&mut rep).unwrap();
        assert_eq!(u32::from_be_bytes([rep[4], rep[5], rep[6], rep[7]]), ST_DEV_BUSY);

        drop(dev);
        let mut b = [0u8; 1];
        assert_eq!(first.read(&mut b).unwrap_or(0), 0, "the connection closes");
    }

    /// A passthrough serves the pad's own descriptor and serial, and the
    /// host's report reads wait for the device's answer.
    #[test]
    fn a_passthrough_forwards_report_requests_and_answers_with_the_reply() {
        let report = [0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0xC0];
        let desc = DescriptorSet::hid(0x045E, 0x02E0, 0x0903, "Xbox Wireless Controller", "98B6E9ABCDEF", &report).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let server = server::global().unwrap();
        let dev = server.register(|busid, devid| Arc::new(DeviceInner::passthrough(desc, busid, devid, tx)));
        let mut s = {
            let mut s = TcpStream::connect((HOST, server.port)).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut req = vec![0u8; 8 + BUS_ID_SIZE];
            write_op_common(&mut req, OP_REQ_IMPORT, 0);
            req[8..8 + dev.busid.len()].copy_from_slice(dev.busid.as_bytes());
            s.write_all(&req).unwrap();
            let mut rep = vec![0u8; 8 + USB_DEVICE_SIZE];
            s.read_exact(&mut rep).unwrap();
            s
        };
        let control = |seqnum: u32, setup: [u8; 8], len: i32| Command {
            command: CMD_SUBMIT,
            seqnum,
            direction: 1,
            transfer_buffer_length: len,
            number_of_packets: NON_ISOCH,
            setup,
            ..Default::default()
        };
        // The serial, string 2.
        s.write_all(&control(1, [0x80, 0x06, 0x02, 0x03, 0x09, 0x04, 0xFF, 0], 255).to_bytes()).unwrap();
        let (_, _, status, actual, data) = read_reply(&mut s, 26);
        assert_eq!((status, actual), (0, 26));
        let serial: String = char::decode_utf16(data[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])))
            .map(|c| c.unwrap())
            .collect();
        assert_eq!(serial, "98B6E9ABCDEF");
        assert!(!is_own_serial(&serial));
        // The report descriptor, as the pad gave it.
        s.write_all(&control(2, [0x81, 0x06, 0x00, 0x22, 0, 0, 0xFF, 0], 255).to_bytes()).unwrap();
        assert_eq!(read_reply(&mut s, report.len()).4, report);
        // GET_REPORT(Feature 3): handed out, answered by the "device".
        s.write_all(&control(3, [0xA1, 0x01, 0x03, 0x03, 0, 0, 0x10, 0], 16).to_bytes()).unwrap();
        let req = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(req, HostRequest::GetReport { id: 3, kind: ReportKind::Feature, number: 3 });
        dev.complete(3, Some(&[0x03, 0xAA, 0xBB]));
        let (_, seq, status, actual, data) = read_reply(&mut s, 3);
        assert_eq!((seq, status, actual), (3, 0, 3));
        assert_eq!(data, vec![0x03, 0xAA, 0xBB]);
        server.unregister(&dev.busid);
    }

    #[test]
    fn devices_get_distinct_busids_on_one_server() {
        let a = EmulatedDevice::new(profiles::DUALSENSE_COMPOSITE_JSON, "test.a", 0).unwrap();
        let b = EmulatedDevice::new(profiles::DUALSENSE_COMPOSITE_JSON, "test.b", 1).unwrap();
        assert_ne!(a.busid(), b.busid());
        assert_eq!(a.port(), b.port());
        assert!(is_our_location(HOST, &a.port().to_string()));
    }
}
