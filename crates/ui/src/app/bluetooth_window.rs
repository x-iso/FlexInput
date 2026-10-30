//! The Bluetooth dongle panel: adapters, paired controllers, and where the
//! keys live.
//!
//! ⭐ **One place for a subsystem whose state is otherwise invisible.** Every
//! question this answers used to require a terminal: which adapters can we even
//! use, which controllers are paired to them, where is the key file, and why is
//! the dongle not being used right now. Those are exactly the questions asked
//! when something does not work, and answering them in a log the user has to be
//! told to find is not answering them.
//!
//! It's also where an adapter is handed to FlexInput, and back: on Linux a
//! udev rule (`flexinput_btle::linux`), on Windows WinUSB installed on it the
//! way Zadig does (`flexinput_hidmaestro::winusb_bind`, in the elevated
//! helper). Every adapter is listed, with what the system's own Bluetooth does
//! with it — Windows runs one radio at a time, Linux all of them.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flexinput_btle::{keystore, DongleInfo, SystemUse};

/// Whose Bluetooth the system's is, for the panel's sentences: "Windows'
/// Bluetooth", "Linux's Bluetooth".
#[cfg(windows)]
const SYSTEMS: &str = "Windows'";
#[cfg(not(windows))]
const SYSTEMS: &str = "Linux's";

/// How often the adapter list is refreshed while the window is open.
///
/// ❗ Enumerating USB is not free and this runs on the UI thread, so it is not
/// done per frame. Two seconds is fast enough that plugging a dongle in feels
/// live and slow enough to cost nothing.
const RESCAN: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct BluetoothState {
    pub open: bool,
    adapters: Vec<DongleInfo>,
    last_scan: Option<Instant>,
    /// Set when a removal fails, so the reason is on screen rather than lost.
    error: Option<String>,
    /// An adapter being handed over or back, while the system's permission
    /// prompt is up.
    handover: Option<Handover>,
    /// What came of the last handover, kept on screen.
    handover_note: Option<Result<String, String>>,
    /// Windows: usbip-win2 being installed on its own, for an adapter handed
    /// over before handing over installed it. `None` inside until answered.
    #[cfg(windows)]
    usbip_install: Option<Arc<Mutex<Option<Result<(), String>>>>>,
    #[cfg(windows)]
    usbip_note: Option<Result<(), String>>,
}

struct Handover {
    to_flexinput: bool,
    /// `None` until the permission prompt is answered.
    result: Arc<Mutex<Option<Result<(), String>>>>,
}

/// What a handover needs to name the adapter: its ids (Linux's udev rule) or
/// its device instance (Windows' driver install).
struct Target {
    vid: u16,
    pid: u16,
    instance_id: Option<String>,
}

impl BluetoothState {
    /// Hand an adapter to FlexInput or back, on a thread of its own: it waits
    /// on the system's permission prompt.
    ///
    /// ⭐ Giving one back lets go of it first (`flexinput_btle::withhold`):
    /// the system can't take an adapter FlexInput still holds. It stays
    /// withheld once given back — the system may take a moment to load its
    /// driver, and FlexInput mustn't take it again meanwhile — and is allowed
    /// again when handed over, or when the give-back fails.
    fn start_handover(&mut self, a: &DongleInfo, to_flexinput: bool) {
        let target = Target { vid: a.vid, pid: a.pid, instance_id: a.instance_id.clone() };
        let result = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&result);
        std::thread::spawn(move || {
            flexinput_btle::withhold(target.vid, target.pid, !to_flexinput);
            if !to_flexinput {
                // Normally at once; a transport mid-connection notices when
                // that conversation ends.
                let deadline = Instant::now() + Duration::from_secs(10);
                while flexinput_btle::is_ours(target.vid, target.pid) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let r = hand_over(&target, to_flexinput);
            if r.is_err() && !to_flexinput {
                flexinput_btle::withhold(target.vid, target.pid, false);
            }
            if let Ok(mut s) = slot.lock() {
                *s = Some(r);
            }
        });
        self.handover = Some(Handover { to_flexinput, result });
        self.handover_note = None;
    }
}

/// Linux: rewrite the udev rule, through pkexec.
#[cfg(target_os = "linux")]
fn hand_over(t: &Target, to_flexinput: bool) -> Result<(), String> {
    let _ = &t.instance_id;
    flexinput_btle::linux::set_dedicated(t.vid, t.pid, to_flexinput)
}

/// Windows: install WinUSB on it, or remove that again, in the elevated helper.
///
/// ⭐ Handing one over also installs usbip-win2, under the same permission
/// prompt: the controllers FlexInput passes to Windows as USB devices need
/// it. First, because installing it re-enumerates the USB root hubs, which
/// would reset the adapter just handed over. Its failure doesn't stop the
/// handover — the Switch Pro and the PlayStation pads don't need it — and
/// the window offers it again.
#[cfg(windows)]
fn hand_over(t: &Target, to_flexinput: bool) -> Result<(), String> {
    let id = t
        .instance_id
        .as_deref()
        .ok_or_else(|| format!("Windows doesn't list adapter {:04x}:{:04x} as a device", t.vid, t.pid))?;
    if !to_flexinput {
        return flexinput_hidmaestro::helper::unbind_winusb(id).map_err(|e| e.to_string());
    }
    let usbip = flexinput_hidmaestro::helper::ensure_usbip();
    flexinput_hidmaestro::helper::bind_winusb(id).map_err(|e| e.to_string())?;
    usbip.map_err(|e| format!("Handed over, but usbip-win2 didn't install ({e}): install it below."))
}

/// Windows: whether the controllers passed to Windows as USB devices still
/// wait on usbip-win2, with an adapter handed over — and the button that
/// installs it, in the elevated helper.
#[cfg(windows)]
fn usbip_row(ui: &mut egui::Ui, st: &mut BluetoothState) {
    if let Some(slot) = &st.usbip_install {
        match slot.lock().ok().and_then(|mut r| r.take()) {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("Installing usbip-win2… answer Windows' permission prompt.").small());
                });
                ui.ctx().request_repaint_after(Duration::from_millis(200));
                return;
            }
            Some(r) => {
                st.usbip_note = Some(r);
                st.usbip_install = None;
            }
        }
    }
    if let Some(Err(e)) = &st.usbip_note {
        ui.label(egui::RichText::new(format!("usbip-win2 didn't install: {e}")).small().color(egui::Color32::from_rgb(230, 140, 60)));
    }
    if !st.adapters.iter().any(|a| a.dedicated) || flexinput_usbip::vhci::is_available() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(
                "Controllers other than the Switch Pro, Xbox, DualShock 4 and DualSense reach \
                 Windows as USB devices, through usbip-win2 (a virtual USB host controller).",
            )
            .small()
            .color(egui::Color32::from_rgb(230, 140, 60)),
        );
        if ui
            .small_button("Install")
            .on_hover_text("Installs usbip-win2: one Windows permission prompt. The USB ports re-enumerate once.")
            .clicked()
        {
            let result = Arc::new(Mutex::new(None));
            let slot = Arc::clone(&result);
            std::thread::spawn(move || {
                let r = flexinput_hidmaestro::helper::ensure_usbip().map_err(|e| e.to_string());
                if let Ok(mut s) = slot.lock() {
                    *s = Some(r);
                }
            });
            st.usbip_install = Some(result);
            st.usbip_note = None;
        }
    });
}

/// While a handover waits on the permission prompt, say so; once it's
/// answered, what came of it.
fn handover_status(ui: &mut egui::Ui, st: &mut BluetoothState) {
    #[cfg(windows)]
    const WAITING: &str = "Waiting for Windows' permission prompt…";
    #[cfg(not(windows))]
    const WAITING: &str = "Waiting for your password…";
    #[cfg(windows)]
    const GIVEN_BACK: &str = "Done. Windows' Bluetooth has it again. If Windows asks for a \
                              restart, that finishes its driver change.";
    #[cfg(not(windows))]
    const GIVEN_BACK: &str = "Done. The adapter was re-plugged, and Linux's Bluetooth has it \
                              again.";
    let progress = st
        .handover
        .as_ref()
        .map(|h| (h.to_flexinput, h.result.lock().ok().and_then(|mut r| r.take())));
    match progress {
        Some((_, None)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(WAITING).small());
            });
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        Some((to_flexinput, Some(r))) => {
            st.handover_note = Some(r.map(|()| {
                if to_flexinput {
                    "Done. Bluetooth Classic controllers can use it now; restart \
                     FlexInput for Joy-Con 2 controllers to."
                        .to_string()
                } else {
                    GIVEN_BACK.to_string()
                }
            }));
            st.handover = None;
            // The list shows the change at once.
            st.last_scan = None;
        }
        None => {}
    }
    match &st.handover_note {
        Some(Ok(note)) => {
            ui.label(egui::RichText::new(note).small().color(egui::Color32::from_rgb(120, 200, 120)));
        }
        Some(Err(e)) => {
            ui.label(
                egui::RichText::new(format!("Couldn't change that: {e}"))
                    .small()
                    .color(egui::Color32::from_rgb(230, 140, 60)),
            );
        }
        None => {}
    }
}

/// An adapter row's handover button: `Some(true)` to hand it to FlexInput,
/// `Some(false)` to give it back to the system, when clicked.
fn handover_button(ui: &mut egui::Ui, busy: bool, a: &DongleInfo) -> Option<bool> {
    #[cfg(windows)]
    const BACK: &str = "Give back to Windows";
    #[cfg(not(windows))]
    const BACK: &str = "Give back to Linux";
    #[cfg(windows)]
    const BACK_TIP: &str = "Stop FlexInput using this adapter: removes the WinUSB driver \
                            from it (FlexInput's, or one Zadig installed), and Windows' \
                            Bluetooth takes it back once FlexInput lets go of it.\n\nOne \
                            Windows permission prompt.";
    #[cfg(not(windows))]
    const BACK_TIP: &str = "Stop FlexInput using this adapter. Linux's Bluetooth gets it back \
                            once FlexInput lets go of it, at the latest when FlexInput \
                            closes.\n\nAsks for your password.";
    #[cfg(windows)]
    const USE_TIP: &str = "Hand this adapter to FlexInput's own Bluetooth, for Joy-Con 2 and \
                           Bluetooth Classic controllers: installs Windows' WinUSB driver on \
                           it, as Zadig does. Windows' Bluetooth stops using it until you \
                           give it back, FlexInput running or not, and anything connected \
                           through it now (headphones, a mouse) disconnects. Use a second \
                           adapter if you have one.\n\nOne Windows permission prompt.";
    #[cfg(not(windows))]
    const USE_TIP: &str = "Hand this adapter to FlexInput's own Bluetooth, for Joy-Con 2 and \
                           Bluetooth Classic controllers. While FlexInput runs it takes the \
                           adapter from Linux's Bluetooth: anything connected through it now \
                           (headphones, a mouse) disconnects. Use a second adapter if you have \
                           one.\n\nAsks for your password once.";
    let (label, tip) = if a.dedicated { (BACK, BACK_TIP) } else { ("Use with FlexInput", USE_TIP) };
    ui.add_enabled(!busy, egui::Button::new(egui::RichText::new(label).small()))
        .on_hover_text(tip)
        .clicked()
        .then_some(!a.dedicated)
}

/// What the system's own Bluetooth does with an adapter that isn't FlexInput's.
fn system_use(a: &DongleInfo) -> String {
    match &a.system {
        SystemUse::Active { name: Some(hci), default: true } => format!("{SYSTEMS} Bluetooth uses it ({hci}, the default)"),
        SystemUse::Active { name: Some(hci), default: false } => format!("{SYSTEMS} Bluetooth uses it ({hci})"),
        SystemUse::Active { name: None, .. } => format!("{SYSTEMS} Bluetooth uses it"),
        SystemUse::Standby => {
            "Windows' Bluetooth radio, not in use: Windows runs one adapter at a time".to_string()
        }
        SystemUse::None => "no driver has it".to_string(),
    }
}

impl BluetoothState {
    /// Whether any Bluetooth adapter is visible at all — drives the title-bar
    /// button. Cached on the same timer as the window's own list.
    pub fn present(&mut self) -> bool {
        // ⛔ Holding a dongle already answers the question. This is called from
        // the title bar every frame and used to enumerate the whole USB bus
        // every two seconds regardless — on the UI thread, forever, while the
        // adapter it was enumerating streamed a controller.
        if flexinput_btle::holding_any() {
            return true;
        }
        self.refresh_if_stale();
        !self.adapters.is_empty()
    }

    fn refresh_if_stale(&mut self) {
        let stale = self.last_scan.map(|t| t.elapsed() > RESCAN).unwrap_or(true);
        if stale {
            self.adapters = flexinput_btle::discover();
            self.last_scan = Some(Instant::now());
        }
    }
}

/// One line saying what the classic transport is actually doing.
///
/// ⭐ The panel's first job. Everything below it is detail; this is the answer
/// to "why is nothing happening", which is the question that brings anyone
/// here.
fn transport_status(ui: &mut egui::Ui) {
    use flexinput_devices::classic_bt::{pair_control, Status};
    let (text, colour) = match pair_control().status() {
        Status::Disabled => (
            "Classic controllers: starting…".to_string(),
            egui::Color32::from_gray(150),
        ),
        Status::NoRadio(why) => (
            format!("Classic controllers: no usable adapter — {why}"),
            egui::Color32::from_rgb(230, 140, 60),
        ),
        Status::Idle => (
            "Classic controllers: ready, nothing paired yet.".to_string(),
            egui::Color32::from_gray(180),
        ),
        // ⭐ "Connected" and "sending input" reported separately, because they
        // came apart on hardware and one number hid it: the panel said
        // "1 of 1 connected" while no controller appeared anywhere.
        Status::Running { paired, connected, streaming } => {
            if streaming == connected {
                (
                    format!("Classic controllers: {streaming} of {paired} sending input."),
                    if streaming > 0 {
                        egui::Color32::from_rgb(120, 200, 120)
                    } else {
                        egui::Color32::from_gray(180)
                    },
                )
            } else {
                (
                    format!(
                        "Classic controllers: {connected} connected, but only                          {streaming} sending input — a link is up and silent."
                    ),
                    egui::Color32::from_rgb(230, 140, 60),
                )
            }
        }
    };
    ui.label(egui::RichText::new(text).small().color(colour));
}

/// The "pair a new controller" control and whatever the last run reported.
///
/// ⭐ The button only REQUESTS. The radio belongs to the classic transport's
/// thread, and a UI that opened the dongle itself would be a second owner of
/// one device — which has already broken this app once, silently.
fn pair_row(ui: &mut egui::Ui) {
    use flexinput_devices::classic_bt::{pair_control, PairPhase};
    let ctl = pair_control();

    if !ctl.transport_enabled() {
        ui.label(
            egui::RichText::new(
                "Pairing needs a Bluetooth adapter handed to FlexInput: use the button \
                 beside one above.",
            )
            .small()
            .color(egui::Color32::from_gray(150)),
        );
        return;
    }

    let phase = ctl.phase();
    let busy = ctl.busy();
    ui.horizontal(|ui| {
        // ⭐ The search runs until a controller pairs, this is pressed, or the
        // window closes (see `show`).
        if busy {
            if ui
                .button("Cancel")
                .on_hover_text("Stop looking for a controller.")
                .clicked()
            {
                ctl.cancel();
            }
        } else if ui
            .button("Pair new controller")
            .on_hover_text(format!(
                "Put the controller in pairing mode first — on a Switch Pro, hold \
                 the small Sync button until the lights run back and forth; on a \
                 DualShock 4 or DualSense, hold Share or Create with PS until the \
                 light flashes. FlexInput keeps looking until one pairs, you \
                 cancel, or this window closes.\n\n{}\n\n\
                 ❗ Pairing REPLACES the host it currently belongs to.",
                if cfg!(target_os = "linux") {
                    "Any Bluetooth Classic controller works: Linux reads it as it \
                     would through its own Bluetooth."
                } else {
                    "Any Bluetooth Classic controller works. FlexInput reads Switch \
                     Pro, Xbox, DualShock 4 and DualSense controllers itself, and passes \
                     the others to Windows as USB controllers (that takes usbip-win2)."
                },
            ))
            .clicked()
        {
            ctl.request();
        }
        match &phase {
            PairPhase::Searching(note) => {
                ui.spinner();
                ui.label(egui::RichText::new("looking for a controller in pairing mode…").small());
                // How the last attempt went, while the search carries on.
                if let Some(n) = note {
                    ui.label(
                        egui::RichText::new(n)
                            .small()
                            .color(egui::Color32::from_rgb(230, 140, 60)),
                    );
                }
            }
            PairPhase::Pairing(a) => {
                ui.spinner();
                ui.label(egui::RichText::new(format!("pairing {a}…")).small());
            }
            PairPhase::Done(n) => {
                ui.label(
                    egui::RichText::new(format!("✔ paired {n}"))
                        .small()
                        .color(egui::Color32::from_rgb(120, 200, 120)),
                );
            }
            PairPhase::Failed(e) => {
                ui.label(
                    egui::RichText::new(e)
                        .small()
                        .color(egui::Color32::from_rgb(230, 140, 60)),
                );
            }
            PairPhase::Idle => {}
        }
    });
    // A run in progress changes state on another thread; without this the
    // spinner would sit still until the pointer moved.
    if busy {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
}

pub fn show(ctx: &egui::Context, st: &mut BluetoothState, key_dir: Option<&str>) {
    if !st.open {
        return;
    }
    st.refresh_if_stale();
    let mut open = st.open;
    egui::Window::new("Bluetooth")
        .open(&mut open)
        .resizable(true)
        .default_width(460.0)
        .show(ctx, |ui| body(ui, st, key_dir));
    // A pairing search lasts as long as the window that started it.
    if !open {
        flexinput_devices::classic_bt::pair_control().cancel();
    }
    st.open = open;
}

fn body(ui: &mut egui::Ui, st: &mut BluetoothState, key_dir: Option<&str>) {
    transport_status(ui);
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Adapters")
            .strong(),
    );
    ui.add_space(2.0);
    if st.adapters.is_empty() {
        ui.label(
            egui::RichText::new("No USB Bluetooth adapter.")
                .small()
                .color(egui::Color32::from_gray(160)),
        );
    }
    handover_status(ui, st);
    let mut start_handover: Option<(DongleInfo, bool)> = None;
    for a in &st.adapters {
        ui.horizontal(|ui| {
            // ⭐ The state names say what they MEAN for the user, not what
            // libusb reported.
            //
            // ❗ "available" versus "in use" read backwards: being in use by
            // FlexInput is the WORKING state — the radio is open and serving
            // controllers — while an adapter nothing has opened is the idle
            // one. Labelling the good state with the word that sounds worse,
            // and giving no hint whether "in use" meant this app, a leftover
            // process or a helper, made the panel unreadable exactly when it
            // was being consulted.
            //
            // ⛔ A recorded failure OUTRANKS every other state. An adapter that
            // is WinUSB-bound, openable and mute reads as a perfectly healthy
            // "idle" row, which is the most misleading thing this panel could
            // possibly say — the user goes looking for a configuration mistake
            // that does not exist.
            //
            // The adapters the system's Bluetooth keeps are listed too, with
            // what it does with them: FlexInput's only once handed over, with
            // the button at the end of the row.
            let system;
            let (mark, colour, what) = if !a.dedicated {
                system = system_use(a);
                let colour = if matches!(a.system, SystemUse::Active { .. }) {
                    egui::Color32::from_rgb(110, 160, 230)
                } else {
                    egui::Color32::from_gray(150)
                };
                ("●", colour, system.as_str())
            } else if let Some(why) = &a.problem {
                ("⛔", egui::Color32::from_rgb(225, 90, 90), why.as_str())
            } else if a.ours {
                (
                    "●",
                    egui::Color32::from_rgb(120, 200, 120),
                    "active — FlexInput is using this adapter",
                )
            } else if a.available {
                (
                    "○",
                    egui::Color32::from_gray(150),
                    "idle — not in use",
                )
            } else {
                (
                    "⚠",
                    egui::Color32::from_rgb(230, 140, 60),
                    "held by another program — close it, or unplug and replug \
                     the dongle",
                )
            };
            ui.label(egui::RichText::new(mark).color(colour));
            // Manufacturer and model first — the ids are for telling two
            // identical dongles apart, not for identifying the one in your hand.
            ui.label(a.describe());
            ui.label(
                egui::RichText::new(format!("{} · bus {}", a.ids(), a.bus))
                    .small()
                    .color(egui::Color32::from_gray(140)),
            );
            ui.label(
                egui::RichText::new(what)
                    .small()
                    .color(egui::Color32::from_gray(150)),
            );
            if let Some(to_flexinput) = handover_button(ui, st.handover.is_some(), a) {
                start_handover = Some((a.clone(), to_flexinput));
            }
        });
        // Before it's handed over: whether it's worth it.
        if let Some(why) = a.likely_unusable().filter(|_| !a.dedicated) {
            let mut text = format!("⚠ {why}: it will likely not find controllers.");
            if a.built_in {
                text.push_str(&format!(" And {SYSTEMS} own Bluetooth would lose it."));
            }
            ui.label(egui::RichText::new(text).small().color(egui::Color32::from_rgb(230, 140, 60)));
        }
    }
    if let Some((a, to_flexinput)) = start_handover {
        st.start_handover(&a, to_flexinput);
    }
    #[cfg(windows)]
    usbip_row(ui, st);

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    ui.label(egui::RichText::new("Paired controllers").strong());
    ui.label(
        egui::RichText::new(
            "A pairing belongs to the DONGLE, not this PC — so the same dongle \
             works on another machine, as long as the key file goes with it.",
        )
        .small()
        .color(egui::Color32::from_gray(150)),
    );
    ui.add_space(4.0);

    let paired = keystore::load();
    let devices = keystore::load_devices();
    if paired.is_empty() {
        ui.label(
            egui::RichText::new("Nothing paired yet.")
                .small()
                .color(egui::Color32::from_gray(160)),
        );
    }
    let mut forget: Option<[u8; 6]> = None;
    for (addr, p) in &paired {
        ui.horizontal(|ui| {
            // The controller's own name, recorded when it was paired. Falls
            // back to the address for entries made before names were stored.
            match &p.name {
                Some(n) => ui.label(n),
                None => ui.label(
                    egui::RichText::new("(name not recorded)")
                        .italics()
                        .color(egui::Color32::from_gray(150)),
                ),
            };
            ui.label(
                egui::RichText::new(addr)
                    .small()
                    .monospace()
                    .color(egui::Color32::from_gray(140)),
            );
            // What it said it is, once it has (see `keystore::DeviceInfo`).
            let info = devices.get(addr);
            if let Some(d) = info {
                ui.label(
                    egui::RichText::new(format!("{:04x}:{:04x}", d.vid, d.pid))
                        .small()
                        .monospace()
                        .color(egui::Color32::from_gray(140)),
                );
            }
            if let Some(why) = flexinput_devices::classic_bt::unusable_reason(info) {
                ui.label(
                    egui::RichText::new("⚠ not read here")
                        .small()
                        .color(egui::Color32::from_rgb(230, 140, 60)),
                )
                .on_hover_text(format!("Paired, but {why}."));
            }
            // ❗ Four bytes only. The key is a shared secret, and a panel is the
            // most likely thing in this app to end up in a screenshot.
            let head: String = p.key[..4].iter().map(|b| format!("{b:02x}")).collect();
            ui.label(
                egui::RichText::new(format!("key {head}…"))
                    .small()
                    .color(egui::Color32::from_gray(140)),
            );
            if ui
                .small_button("Forget")
                .on_hover_text(
                    "Removes this host's half of the pairing. The controller keeps \
                     its own half and will keep trying to reconnect until it is \
                     paired again.",
                )
                .clicked()
            {
                if let Some(a) = keystore::parse_addr(addr) {
                    forget = Some(a);
                }
            }
        });
    }
    ui.add_space(6.0);
    pair_row(ui);
    if let Some(a) = forget {
        match keystore::forget(a) {
            Ok(_) => st.error = None,
            Err(e) => st.error = Some(format!("Could not update the key file: {e}")),
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    ui.label(egui::RichText::new("Key file").strong());
    ui.horizontal(|ui| {
        let p = keystore::path();
        ui.label(
            egui::RichText::new(p.display().to_string())
                .small()
                .monospace()
                .color(egui::Color32::from_gray(170)),
        );
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(p.display().to_string());
        }
        if let Some(dir) = p.parent().map(|d| d.to_path_buf()) {
            if ui.small_button("Open folder").clicked() {
                // Best-effort: a missing folder is not worth an error dialog,
                // and the path is on screen either way.
                #[cfg(windows)]
                const OPEN: &str = "explorer";
                #[cfg(not(windows))]
                const OPEN: &str = "xdg-open";
                let _ = std::process::Command::new(OPEN).arg(dir).spawn();
            }
        }
    });
    if key_dir.is_none() {
        ui.label(
            egui::RichText::new(
                "Set a folder in Settings — a cloud-synced one means the pairing \
                 follows the dongle to any PC with nothing to copy by hand.",
            )
            .small()
            .color(egui::Color32::from_gray(150)),
        );
    }

    if let Some(err) = &st.error {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(err).small().color(egui::Color32::from_rgb(230, 140, 60)));
    }
}
