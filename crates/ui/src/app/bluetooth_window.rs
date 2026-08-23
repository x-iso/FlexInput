//! The Bluetooth dongle panel: adapters, paired controllers, and where the
//! keys live.
//!
//! ⭐ **One place for a subsystem whose state is otherwise invisible.** Every
//! question this answers used to require a terminal: which adapters can we even
//! use, which controllers are paired to them, where is the key file, and why is
//! the dongle not being used right now. Those are exactly the questions asked
//! when something does not work, and answering them in a log the user has to be
//! told to find is not answering them.

use std::time::{Duration, Instant};

use flexinput_btle::{keystore, DongleInfo};

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
}

impl BluetoothState {
    /// Whether any Bluetooth adapter is visible at all — drives the title-bar
    /// button. Cached on the same timer as the window's own list.
    pub fn present(&mut self) -> bool {
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
                "Pairing needs a WinUSB-bound Bluetooth dongle. Bind one with \
                 Zadig and restart.",
            )
            .small()
            .color(egui::Color32::from_gray(150)),
        );
        return;
    }

    let phase = ctl.phase();
    let busy = matches!(phase, PairPhase::Searching | PairPhase::Pairing(_));
    ui.horizontal(|ui| {
        let label = if busy { "Pairing…" } else { "Pair new controller" };
        if ui
            .add_enabled(!busy, egui::Button::new(label))
            .on_hover_text(
                "Put the controller in pairing mode first — on a Switch Pro, hold \
                 the small Sync button until the lights run back and forth.\n\n\
                 ❗ Pairing REPLACES the host it currently belongs to.",
            )
            .clicked()
        {
            ctl.request();
        }
        match &phase {
            PairPhase::Searching => {
                ui.spinner();
                ui.label(egui::RichText::new("looking for a controller…").small());
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
            egui::RichText::new(
                "No WinUSB-bound Bluetooth dongle. Adapters owned by Windows are \
                 not listed — bind one with Zadig to use it here.",
            )
            .small()
            .color(egui::Color32::from_gray(160)),
        );
    }
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
            let (mark, colour, what) = if let Some(why) = &a.problem {
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
        });
    }

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
                let _ = std::process::Command::new("explorer").arg(dir).spawn();
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
