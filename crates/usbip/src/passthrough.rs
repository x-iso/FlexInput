//! A real HID device served over USB/IP: on Windows, the Bluetooth pads on
//! FlexInput's own adapter. Windows binds its own HID driver to the device,
//! so the pad reaches FlexInput, SDL and games as the USB controller it would
//! be on a cable — its own report descriptor, ids and reports. The Linux side
//! of the same job is a UHID passthrough (`flexinput_virtual`), which is what
//! BlueZ does there.
//!
//! ⭐ Nothing is answered from a table: every report the host reads or writes
//! is handed to the owner ([`HostRequest`]), put to the pad, and answered
//! with the pad's reply. Output reports go the same way, without waiting.
//!
//! ❗ Its serial is the pad's own (its Bluetooth address), never one of
//! FlexInput's "FX" ones, so FlexInput reads it as the physical pad it is.

use std::sync::{mpsc, Arc};

use crate::descriptors::DescriptorSet;
use crate::device::DeviceInner;
use crate::server::{self, HOST};

/// A report type, as the host names it in a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    Input,
    Output,
    Feature,
}

/// Something the host asked of the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRequest {
    /// An output report, its report id first when the descriptor numbers them.
    Output(Vec<u8>),
    /// Read a report: answer with `reply_get` and this `id`.
    GetReport { id: u32, kind: ReportKind, number: u8 },
    /// Write a report (its id first): answer with `reply_set`.
    SetReport { id: u32, kind: ReportKind, data: Vec<u8> },
}

/// What a [`Passthrough`] is created as.
pub struct PassthroughSpec<'a> {
    pub vid: u16,
    pub pid: u16,
    pub version: u16,
    /// The product string; the pad's own name.
    pub product: &'a str,
    /// The USB serial. Windows keys the device's instance on it, so the same
    /// pad keeps one identity — and one HidHide entry — across connections.
    pub serial: &'a str,
    pub descriptor: &'a [u8],
}

/// One passthrough device, served and attached to the host controller.
/// Dropping it plugs it out.
pub struct Passthrough {
    inner: Arc<DeviceInner>,
    server: Arc<server::Server>,
    requests: mpsc::Receiver<HostRequest>,
    vport: Option<i32>,
}

impl Passthrough {
    /// Serve the device and attach it. The host controller must be there
    /// ([`crate::vhci::is_available`]); installing it takes elevation, which
    /// is the caller's to ask for.
    pub fn create(spec: &PassthroughSpec) -> Result<Self, String> {
        let desc = DescriptorSet::hid(spec.vid, spec.pid, spec.version, spec.product, spec.serial, spec.descriptor)
            .map_err(|e| e.to_string())?;
        crate::sweep_stale_once();
        let server = server::global().map_err(|e| format!("USB/IP server: {e}"))?;
        let (tx, requests) = mpsc::channel();
        let inner = server.register(|busid, devid| Arc::new(DeviceInner::passthrough(desc, busid, devid, tx)));
        let mut dev = Passthrough { inner, server, requests, vport: None };
        dev.vport = Some(crate::vhci::attach(HOST, dev.server.port, &dev.inner.busid).map_err(|e| e.to_string())?);
        Ok(dev)
    }

    /// Hand the host an input report, report id first.
    pub fn input(&self, report: &[u8]) {
        self.inner.submit_input(report);
    }

    /// The host's next request, if one is waiting.
    pub fn next_request(&self) -> Option<HostRequest> {
        self.requests.try_recv().ok()
    }

    /// Answer a [`HostRequest::GetReport`]: the report (its id first), or
    /// `None` for one the device refused or never answered.
    pub fn reply_get(&self, id: u32, report: Option<&[u8]>) {
        self.inner.complete(id, report.filter(|r| !r.is_empty()));
    }

    /// Answer a [`HostRequest::SetReport`].
    pub fn reply_set(&self, id: u32, ok: bool) {
        self.inner.complete(id, ok.then_some(&[][..]));
    }
}

impl Drop for Passthrough {
    fn drop(&mut self) {
        // Plug out first: the driver closes the socket before it unplugs the
        // device, so the server sees the connection end cleanly.
        if let Some(p) = self.vport.take() {
            let _ = crate::vhci::detach(p);
        }
        self.server.unregister(&self.inner.busid);
        self.inner.disconnect();
    }
}
