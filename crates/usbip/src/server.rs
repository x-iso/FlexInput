//! The in-process USB/IP server: listens on loopback and exports each
//! registered device under its busid. usbip-win2's host controller driver
//! connects here (kernel WSK, one TCP connection per attached device), runs
//! the OP_REQ_IMPORT handshake, then streams CMD_SUBMIT / CMD_UNLINK.
//!
//! The port is from a fixed range rather than ephemeral, so a later process
//! can name this server's location when cleaning up after a crash: the driver
//! keeps re-attaching a device whose connection dropped without a plug-out,
//! and cancelling that needs the exact host and port. The range is not
//! HIDMaestro's (18509..), so neither app's cleanup touches the other's.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};

use crate::device::{DeviceInner, Wire};
use crate::protocol::{
    read_fixed_str, read_iso_descriptor, write_op_common, write_usb_device, Command, IsoPacket,
    UsbDeviceBlock, BUS_ID_SIZE, CMD_SUBMIT, CMD_UNLINK, HEADER_SIZE, ISO_DESCRIPTOR_SIZE,
    MAX_ISO_PACKETS, OP_REP_DEVLIST, OP_REP_IMPORT, OP_REQ_DEVLIST, OP_REQ_IMPORT, ST_DEV_BUSY,
    ST_NODEV, ST_OK, USB_DEVICE_SIZE, VERSION,
};

/// 0x4658, "FX".
pub const BASE_PORT: u16 = 18008;
pub const PORT_RANGE: u16 = 16;
pub const HOST: &str = "127.0.0.1";

/// Whether `host:service` is a location this app's server could have used.
pub fn is_our_location(host: &str, service: &str) -> bool {
    host == HOST && service.parse::<u16>().is_ok_and(|p| (BASE_PORT..BASE_PORT + PORT_RANGE).contains(&p))
}

pub(crate) struct Server {
    pub(crate) port: u16,
    devices: Mutex<HashMap<String, Arc<DeviceInner>>>,
}

static SERVER: OnceLock<Mutex<Option<Arc<Server>>>> = OnceLock::new();

/// The process-wide server, started on first use. It lives as long as the
/// process; devices come and go.
pub(crate) fn global() -> std::io::Result<Arc<Server>> {
    let mut slot = SERVER.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some(s) = slot.as_ref() {
        return Ok(Arc::clone(s));
    }
    let mut last = None;
    for port in BASE_PORT..BASE_PORT + PORT_RANGE {
        match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => {
                let server = Arc::new(Server { port, devices: Mutex::new(HashMap::new()) });
                let accept = Arc::clone(&server);
                std::thread::Builder::new()
                    .name("usbip-accept".into())
                    .spawn(move || accept_loop(listener, accept))?;
                *slot = Some(Arc::clone(&server));
                return Ok(server);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no USB/IP port free")))
}

impl Server {
    /// Register a device under the first free busid (`1-N`) and return it.
    pub(crate) fn register(&self, make: impl FnOnce(String, u32) -> Arc<DeviceInner>) -> Arc<DeviceInner> {
        let mut devs = self.devices.lock().unwrap();
        let n = (1u32..).find(|n| !devs.contains_key(&format!("1-{n}"))).unwrap_or(1);
        let busid = format!("1-{n}");
        let dev = make(busid.clone(), (1 << 16) | n);
        devs.insert(busid, Arc::clone(&dev));
        dev
    }

    pub(crate) fn unregister(&self, busid: &str) {
        self.devices.lock().unwrap().remove(busid);
    }

    fn find(&self, busid: &str) -> Option<Arc<DeviceInner>> {
        self.devices.lock().unwrap().get(busid).cloned()
    }

    fn snapshot(&self) -> Vec<Arc<DeviceInner>> {
        self.devices.lock().unwrap().values().cloned().collect()
    }
}

fn accept_loop(listener: TcpListener, server: Arc<Server>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let _ = stream.set_nodelay(true);
        let server = Arc::clone(&server);
        let _ = std::thread::Builder::new()
            .name("usbip-conn".into())
            .spawn(move || serve(stream, &server));
    }
}

/// One connection's sending half. Every completion path — the reader, the
/// input path, the audio pacing thread — serialises here.
struct TcpWire(Mutex<TcpStream>);

impl Wire for TcpWire {
    fn send(&self, data: &[u8]) -> std::io::Result<()> {
        self.0.lock().unwrap().write_all(data)
    }

    fn close(&self) {
        let _ = self.0.lock().unwrap().shutdown(Shutdown::Both);
    }
}

fn serve(mut stream: TcpStream, server: &Server) {
    let Ok(writer) = stream.try_clone() else { return };
    let wire: Arc<dyn Wire> = Arc::new(TcpWire(Mutex::new(writer)));
    let mut claimed: Option<Arc<DeviceInner>> = None;
    let _ = handshake_then_commands(&mut stream, server, &wire, &mut claimed);
    if let Some(dev) = claimed {
        dev.release(&wire);
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn device_block(dev: &DeviceInner, out: &mut [u8]) {
    let d = &dev.desc;
    write_usb_device(out, &UsbDeviceBlock {
        path: &format!("/flexinput/{}", dev.busid),
        busid: &dev.busid,
        busnum: dev.devid >> 16,
        devnum: dev.devid & 0xFFFF,
        speed: d.speed,
        vid: d.vid,
        pid: d.pid,
        bcd_device: d.bcd_device,
        device_class: d.device[4],
        device_subclass: d.device[5],
        device_protocol: d.device[6],
        configuration_value: d.configuration_value,
        num_configurations: d.device[17],
        num_interfaces: d.num_interfaces,
    });
}

fn handshake_then_commands(
    stream: &mut TcpStream,
    server: &Server,
    wire: &Arc<dyn Wire>,
    claimed: &mut Option<Arc<DeviceInner>>,
) -> std::io::Result<()> {
    let mut op = [0u8; 8];
    stream.read_exact(&mut op)?;
    if u16::from_be_bytes([op[0], op[1]]) != VERSION {
        return Ok(());
    }
    match u16::from_be_bytes([op[2], op[3]]) {
        OP_REQ_DEVLIST => return send_devlist(server, wire),
        OP_REQ_IMPORT => {}
        _ => return Ok(()),
    }
    let mut busid = [0u8; BUS_ID_SIZE];
    stream.read_exact(&mut busid)?;
    let busid = read_fixed_str(&busid);
    let mut reply = vec![0u8; 8 + USB_DEVICE_SIZE];
    let Some(dev) = server.find(&busid) else {
        write_op_common(&mut reply, OP_REP_IMPORT, ST_NODEV);
        return wire.send(&reply[..8]);
    };
    if !dev.claim(Arc::clone(wire)) {
        write_op_common(&mut reply, OP_REP_IMPORT, ST_DEV_BUSY);
        return wire.send(&reply[..8]);
    }
    *claimed = Some(Arc::clone(&dev));
    write_op_common(&mut reply, OP_REP_IMPORT, ST_OK);
    device_block(&dev, &mut reply[8..]);
    wire.send(&reply)?;
    command_loop(stream, &dev)
}

fn send_devlist(server: &Server, wire: &Arc<dyn Wire>) -> std::io::Result<()> {
    let devices = server.snapshot();
    let mut buf = vec![0u8; 12];
    write_op_common(&mut buf, OP_REP_DEVLIST, ST_OK);
    buf[8..12].copy_from_slice(&(devices.len() as u32).to_be_bytes());
    for d in &devices {
        let at = buf.len();
        buf.resize(at + USB_DEVICE_SIZE, 0);
        device_block(d, &mut buf[at..]);
        // One usbip_usb_interface (class, subclass, protocol, pad) per
        // interface: the blob's alternate-setting-0 interface descriptors.
        let blob = &d.desc.configuration;
        let mut o = 0;
        while o + 2 <= blob.len() && blob[o] >= 2 {
            if blob[o + 1] == 0x04 && o + 8 <= blob.len() && blob[o + 3] == 0 {
                buf.extend_from_slice(&[blob[o + 5], blob[o + 6], blob[o + 7], 0]);
            }
            o += blob[o] as usize;
        }
    }
    wire.send(&buf)
}

fn command_loop(stream: &mut TcpStream, dev: &DeviceInner) -> std::io::Result<()> {
    let mut header = [0u8; HEADER_SIZE];
    loop {
        stream.read_exact(&mut header)?;
        let cmd = Command::parse(&header);
        match cmd.command {
            CMD_SUBMIT => {
                if cmd.is_isoch() && !(0..=MAX_ISO_PACKETS).contains(&cmd.number_of_packets) {
                    return Err(std::io::Error::other("number_of_packets out of range"));
                }
                let payload = if !cmd.is_in() && cmd.transfer_buffer_length > 0 {
                    let mut p = vec![0u8; cmd.transfer_buffer_length as usize];
                    stream.read_exact(&mut p)?;
                    Some(p)
                } else {
                    None
                };
                let mut iso: Vec<IsoPacket> = Vec::new();
                if cmd.is_isoch() {
                    let mut d = vec![0u8; cmd.number_of_packets as usize * ISO_DESCRIPTOR_SIZE];
                    stream.read_exact(&mut d)?;
                    iso = d.chunks_exact(ISO_DESCRIPTOR_SIZE).map(read_iso_descriptor).collect();
                }
                dev.handle_submit(&cmd, payload, iso);
            }
            CMD_UNLINK => dev.handle_unlink(cmd.seqnum, cmd.unlink_seqnum),
            other => return Err(std::io::Error::other(format!("unexpected command {other}"))),
        }
    }
}
