//! Background worker for blocking virtual-device lifecycle operations.
//!
//! Creating, destroying, or reinstalling the driver for a HIDMaestro virtual
//! device talks to the elevated helper over a named pipe — a **synchronous**
//! call that can block for seconds (up to ~60 s while waiting for UAC). Doing
//! that on the UI thread froze the whole app (see
//! `flexinput_hidmaestro::helper`). This module moves every such op onto a
//! single long-lived worker thread so the UI stays responsive and can paint a
//! progress overlay.
//!
//! Flow: the UI sends a [`DeviceOp`] down `tx`; the worker performs the blocking
//! work, updates the shared [`DeviceOpProgress`] (read by the UI each frame to
//! draw the overlay), then sends a [`DeviceOpResult`] back up and requests an
//! egui repaint. The UI drains results at the top of `update()` and mutates the
//! shared device pool there — so a freshly built device only becomes visible to
//! the I/O thread *after* it's fully constructed (which also fixes the
//! startup-race where the I/O thread `flush()`ed a half-initialized section).
//!
//! `VirtualDevice: Send`, so devices are built on the worker and the finished
//! `Box` is handed back to the UI thread to drop into the pool. `Destroy` moves
//! the device *to* the worker so its `Drop` (which calls `helper::destroy`) runs
//! off the UI thread too.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use flexinput_virtual::VirtualDevice;

/// A lifecycle operation requested by the UI thread.
pub enum DeviceOp {
    /// Build (or reclaim) the virtual device with this full id (e.g.
    /// `virtual.hm.ds4`, `virtual.xinput.1`). Replies `Created` or `Failed`.
    Create { device_id: String },
    /// Tear down a device. The `Box` is moved here so its `Drop` (helper
    /// `destroy`) runs on the worker. Replies `Removed`.
    Destroy { device_id: String, device: Box<dyn VirtualDevice> },
    /// Force a clean driver reinstall, then re-create `device_ids`. `current`
    /// holds the live HIDMaestro devices to tear down first (moved here so their
    /// Drop runs on the worker, before the driver swap). Replies `Reinstalled`.
    Reinstall { device_ids: Vec<String>, current: Vec<Box<dyn VirtualDevice>> },
}

/// Result of a [`DeviceOp`], consumed by the UI thread.
pub enum DeviceOpResult {
    /// A device finished building — push it into the shared pool.
    Created { device: Box<dyn VirtualDevice> },
    /// A `Destroy` completed (device already dropped on the worker).
    Removed { device_id: String },
    /// A reinstall completed: `devices` are the rebuilt pads to install into the
    /// pool; `errors` collects any per-step failures to surface to the user.
    Reinstalled { devices: Vec<Box<dyn VirtualDevice>>, errors: Vec<String> },
    /// A `Create` failed; the id is no longer in-flight.
    Failed { device_id: String, error: String },
}

/// Which kind of op is in flight — drives the overlay's icon/wording.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProgressKind {
    Installing,
    Deploying,
    Removing,
    Reinstalling,
}

/// Live progress for the modal overlay. `None` = no op in flight.
#[derive(Clone)]
pub struct DeviceOpProgress {
    pub kind: ProgressKind,
    pub label: String,
    pub detail: Option<String>,
}

impl DeviceOpProgress {
    fn new(kind: ProgressKind, label: impl Into<String>) -> Self {
        DeviceOpProgress { kind, label: label.into(), detail: None }
    }
}

/// Handle the UI keeps: the op sender, the result receiver, and the shared
/// progress cell. Dropping the sender ends the worker loop.
pub struct DeviceOpHandle {
    pub tx: Sender<DeviceOp>,
    pub rx: Receiver<DeviceOpResult>,
    pub progress: Arc<Mutex<Option<DeviceOpProgress>>>,
}

/// Spawn the worker thread and return the UI-side handle. `ctx` is cloned so the
/// worker can request a repaint when an op completes (cheap; `egui::Context` is
/// an `Arc` internally).
pub fn spawn(ctx: egui::Context) -> DeviceOpHandle {
    let (op_tx, op_rx) = std::sync::mpsc::channel::<DeviceOp>();
    let (res_tx, res_rx) = std::sync::mpsc::channel::<DeviceOpResult>();
    let progress: Arc<Mutex<Option<DeviceOpProgress>>> = Arc::new(Mutex::new(None));
    let worker_progress = Arc::clone(&progress);

    std::thread::Builder::new()
        .name("flexinput-device-ops".into())
        .spawn(move || worker_loop(op_rx, res_tx, worker_progress, ctx))
        .expect("spawn device-ops worker");

    DeviceOpHandle { tx: op_tx, rx: res_rx, progress }
}

fn worker_loop(
    op_rx: Receiver<DeviceOp>,
    res_tx: Sender<DeviceOpResult>,
    progress: Arc<Mutex<Option<DeviceOpProgress>>>,
    ctx: egui::Context,
) {
    // Blocks until a message arrives; ends when the UI drops its sender.
    while let Ok(op) = op_rx.recv() {
        let set = |p: Option<DeviceOpProgress>| {
            if let Ok(mut g) = progress.lock() {
                *g = p;
            }
            ctx.request_repaint();
        };

        match op {
            DeviceOp::Create { device_id } => {
                set(Some(DeviceOpProgress::new(
                    ProgressKind::Deploying,
                    format!("Deploying {}…", friendly(&device_id)),
                )));
                let result = match build_device(&device_id) {
                    Some(device) => DeviceOpResult::Created { device },
                    None => DeviceOpResult::Failed {
                        device_id: device_id.clone(),
                        error: format!("could not build device '{device_id}'"),
                    },
                };
                set(None);
                let _ = res_tx.send(result);
                ctx.request_repaint();
            }
            DeviceOp::Destroy { device_id, device } => {
                set(Some(DeviceOpProgress::new(
                    ProgressKind::Removing,
                    format!("Removing {}…", friendly(&device_id)),
                )));
                drop(device); // helper::destroy runs here, off the UI thread
                set(None);
                let _ = res_tx.send(DeviceOpResult::Removed { device_id });
                ctx.request_repaint();
            }
            DeviceOp::Reinstall { device_ids, current } => {
                let mut errors = Vec::new();

                // 1. Tear down current HM devices first (their Drop releases the
                //    sections; the helper also force-removes nodes before the
                //    driver swap, but dropping our handles first is cleaner).
                set(Some(DeviceOpProgress::new(
                    ProgressKind::Reinstalling,
                    "Removing virtual controllers…",
                )));
                drop(current);

                // 2. Reinstall the driver (blocking; one UAC if helper is down).
                set(Some(DeviceOpProgress::new(
                    ProgressKind::Installing,
                    "Reinstalling HIDMaestro driver…",
                )));
                #[cfg(windows)]
                if let Err(e) = flexinput_hidmaestro::helper::reinstall_driver() {
                    errors.push(format!("driver reinstall: {e}"));
                }

                // 3. Re-create the devices that were on the canvas.
                let mut devices = Vec::new();
                for (i, id) in device_ids.iter().enumerate() {
                    set(Some(DeviceOpProgress::new(
                        ProgressKind::Reinstalling,
                        format!("Re-deploying {} ({}/{})…", friendly(id), i + 1, device_ids.len()),
                    )));
                    match build_device(id) {
                        Some(d) => devices.push(d),
                        None => errors.push(format!("re-deploy '{id}' failed")),
                    }
                }

                set(None);
                let _ = res_tx.send(DeviceOpResult::Reinstalled { devices, errors });
                ctx.request_repaint();
            }
        }
    }
}

/// Build a virtual device from its full id (e.g. `virtual.hm.ds4`,
/// `virtual.xinput.1`). Mirrors the kind/instance split in
/// `app::try_create_virtual_device` but runs on the worker thread.
fn build_device(id: &str) -> Option<Box<dyn VirtualDevice>> {
    let (kind_id, instance) = match id.rfind('.') {
        Some(dot) => match id[dot + 1..].parse::<usize>() {
            Ok(n) => (&id[..dot], n),
            Err(_) => (id, 0),
        },
        None => (id, 0),
    };
    let known = flexinput_virtual::available_device_kinds()
        .iter()
        .any(|k| k.kind_id == kind_id);
    if !known {
        return None;
    }
    Some(flexinput_virtual::create_device(kind_id, instance))
}

/// Short human label for a device id, for overlay wording.
fn friendly(id: &str) -> &str {
    match id {
        _ if id.starts_with("virtual.hm.dualsense") => "DualSense",
        _ if id.starts_with("virtual.hm.ds4") => "DualShock 4",
        _ if id.starts_with("virtual.hm.xinput") => "Xbox 360 controller",
        _ if id.starts_with("virtual.ds4") => "DualShock 4",
        _ if id.starts_with("virtual.xinput") => "Xbox controller",
        _ if id.starts_with("virtual.keymouse") => "keyboard/mouse",
        _ => "device",
    }
}
