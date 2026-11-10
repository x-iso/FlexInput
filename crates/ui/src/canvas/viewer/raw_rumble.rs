//! The rumble test on a raw pad's node.
//!
//! A pad that speaks the USB force-feedback protocol (HID PID) can rumble
//! without its original driver (`flexinput_devices::hid_pid`), but nothing in
//! its descriptor says which force-feedback channel spins which motor. The
//! test plays each channel in turn and asks what was felt; the answers become
//! the pad's rumble map (`RumbleMap::from_test`) in its layout, and from then
//! on the backend plays the pad's `rumble_strong` / `rumble_weak` through it.

use super::*;

use flexinput_core::raw_pad::{Felt, RawPadProfile, RumbleChannel, RumbleMap};
use flexinput_devices::hid_pid;

use crate::raw_learn::{self, RawEdit};

#[derive(Clone, Default)]
struct RumbleTest {
    active: bool,
    /// The channels to play, in order.
    channels: Vec<RumbleChannel>,
    /// One answer per channel played so far.
    answers: Vec<(RumbleChannel, Felt)>,
    /// When the current channel last started playing (egui time).
    played_at: Option<f64>,
    /// Every channel answered: what the answers add up to (`None`: nothing
    /// was felt).
    result: Option<Option<RumbleMap>>,
}

impl RumbleTest {
    fn start(channels: Vec<RumbleChannel>) -> Self {
        RumbleTest { active: true, channels, ..Default::default() }
    }
}

fn state_id(node: NodeId, dev_id: &str) -> egui::Id {
    egui::Id::new(("raw_pad_rumble", node.0, dev_id))
}

fn describe(map: &RumbleMap) -> &'static str {
    if map.strong == map.weak { "one motor" } else { "heavy + light motors" }
}

/// The rumble row of a raw pad's header: shown only for a pad with a USB
/// force-feedback interface. What was learned, the test while it runs, and
/// its result.
pub(crate) fn show_rumble_section(
    ui: &mut egui::Ui,
    node: NodeId,
    dev_id: &str,
    key: &str,
    dev_name: &str,
    profile: &RawPadProfile,
) {
    let channels = hid_pid::rumble_channels(dev_id);
    if channels.is_empty() {
        return;
    }
    let sid = state_id(node, dev_id);
    let mut test: RumbleTest = ui.ctx().data(|d| d.get_temp(sid)).unwrap_or_default();

    if !test.active {
        ui.horizontal(|ui| {
            match &profile.rumble {
                Some(map) => {
                    ui.label(egui::RichText::new(format!("Rumble: {} ✓", describe(map))).small());
                }
                None => {
                    ui.label(egui::RichText::new("Force-feedback rumble found").small()
                        .color(Color32::from_rgb(127, 201, 127)));
                }
            }
            let label = if profile.rumble.is_some() { "Redo rumble test" } else { "Rumble test" };
            if ui.small_button(label).on_hover_text(
                "This controller has a USB force-feedback interface FlexInput can drive \
                 without the controller's own driver. The test rumbles it once per channel \
                 — hold it and say which motor you felt. After that, rumble sent to this \
                 controller plays through it.",
            ).clicked() {
                test = RumbleTest::start(channels.clone());
            }
            if profile.rumble.is_some()
                && ui.small_button("Forget").on_hover_text(
                    "Stop rumbling through the force-feedback interface; rumble goes back \
                     to whatever the system driver offers.",
                ).clicked()
            {
                raw_learn::request(key, dev_name, RawEdit::SetRumble(None));
            }
        });
    }

    if test.active {
        request_repaint_throttled(ui.ctx());
        let t = ui.input(|i| i.time);
        let mut cancel = false;
        match test.result {
            None => {
                let step = test.answers.len();
                let channel = test.channels[step];
                if test.played_at.is_none() {
                    hid_pid::probe(dev_id, channel);
                    test.played_at = Some(t);
                }
                let playing = test.played_at.is_some_and(|p| t - p < hid_pid::PROBE_MS as f64 / 1000.0);
                let text = if playing { "rumbling… hold the controller" } else { "which motor was that?" };
                ui.label(egui::RichText::new(format!("Rumble test {}/{} — {text}", step + 1, test.channels.len()))
                    .small().strong().color(Color32::from_rgb(106, 167, 255)));
                let mut answer = None;
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(!playing, |ui| {
                        if ui.small_button("Heavy").on_hover_text("A deep, slow rumble").clicked() {
                            answer = Some(Felt::Heavy);
                        }
                        if ui.small_button("Light").on_hover_text("A fast, buzzy rumble").clicked() {
                            answer = Some(Felt::Light);
                        }
                        if ui.small_button("Both").clicked() {
                            answer = Some(Felt::Both);
                        }
                        if ui.small_button("Nothing").clicked() {
                            answer = Some(Felt::Nothing);
                        }
                        if ui.small_button("Play again").clicked() {
                            test.played_at = None;
                        }
                    });
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if let Some(felt) = answer {
                    test.answers.push((channel, felt));
                    test.played_at = None;
                    if test.answers.len() == test.channels.len() {
                        test.result = Some(RumbleMap::from_test(&test.answers));
                    }
                }
            }
            Some(Some(map)) => {
                ui.label(egui::RichText::new(format!("Rumble works: {}", describe(&map))).small()
                    .color(Color32::from_rgb(127, 201, 127)));
                ui.horizontal(|ui| {
                    if ui.small_button("Use it").clicked() {
                        raw_learn::request(key, dev_name, RawEdit::SetRumble(Some(map)));
                        cancel = true;
                    }
                    if ui.small_button("Redo").clicked() {
                        test = RumbleTest::start(test.channels.clone());
                    }
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            }
            Some(None) => {
                ui.label(egui::RichText::new("Nothing rumbled — rumble stays as it was.").small()
                    .color(Color32::from_rgb(232, 180, 65)));
                ui.horizontal(|ui| {
                    if ui.small_button("Try again").clicked() {
                        test = RumbleTest::start(test.channels.clone());
                    }
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            }
        }
        if cancel {
            test = RumbleTest::default();
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(sid, test));
}
