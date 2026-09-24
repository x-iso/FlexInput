//! "Calibrate RWC": the RWS Aim measure calibration, on JSM's constants and in
//! a column narrow enough for the tune panel.
//!
//! Same idea, same gamepad flow: turn the camera a known amount while the engine
//! integrates how far the pad actually turned (`eval/modules/jsm/cal.rs`), then
//! back-solve the constant that would have made the two match. What differs is
//! where the answer goes — a JSM config's numbers live in its TEXT, so a Finish
//! rewrites the setting's line (or writes one, if the config never set it)
//! instead of poking a node param.
//!
//! The panel is a column beside the editor and can be genuinely narrow, so the
//! controls stack — method and output on one row, the snapshot toggle on the
//! next, guidance under that — rather than running along one line the way the
//! RWS pin's do.

use super::*;

/// Mouse: `REAL_WORLD_CALIBRATION`, the counts per degree JSM aims with.
const RWC: &str = "REAL_WORLD_CALIBRATION";
/// `IN_GAME_SENS` divides it. JSM's default is 1, and a config that never sets
/// the command is exactly that — neutral.
const IGS: &str = "IN_GAME_SENS";
/// Stick: deg/s of camera turn at full virtual-stick deflection.
const VSC: &str = "VIRTUAL_STICK_CALIBRATION";

/// JSM's own defaults, for a config that doesn't set the command (`Settings` in
/// `eval/modules/jsm/aim.rs` and `pad.rs`).
const RWC_DEFAULT: f32 = 40.0;
const IGS_DEFAULT: f32 = 1.0;
const VSC_DEFAULT: f32 = 360.0;

/// Past this peak deflection the stick was pinned at full tilt for part of the
/// sweep, so the game turned slower than the pad did and a back-solve would come
/// out wrong. Small headroom for noise. Mirrors `RWS_STICK_SAT_LIMIT`.
const STICK_SAT_LIMIT: f32 = 1.02;

/// What a calibration pass wants written once the snarl borrow is released.
#[derive(Default)]
pub(crate) struct CalEdits {
    /// Node params to set — the widget's own transient state.
    pub(crate) params: Vec<(&'static str, Value)>,
    /// The active tab's config text, rewritten by a Finish.
    pub(crate) text: Option<String>,
}

fn msg_key(node_id: NodeId) -> egui::Id {
    egui::Id::new(("jsm_cal_msg", node_id.0))
}

/// What a setting is set to in this config, or JSM's default when it isn't set.
///
/// Read from the text every time rather than cached: the text is the source of
/// truth, and a hand edit between two sweeps has to count.
pub(crate) fn setting(text: &str, name: &str, default: f32) -> f32 {
    flexinput_engine::eval::jsm_knobs(text)
        .iter()
        .find(|k| k.name.eq_ignore_ascii_case(name))
        .map(|k| k.value)
        .unwrap_or(default)
}

/// The counts per degree this config actually aims with: the calibration divided
/// by the in-game sensitivity, which is what JSM does everywhere it aims.
pub(crate) fn counts_per_degree(text: &str) -> f32 {
    setting(text, RWC, RWC_DEFAULT) / setting(text, IGS, IGS_DEFAULT).max(1e-6)
}

/// Is this node's calibration panel open?
pub(crate) fn is_open(snarl: &Snarl<NodeData>, node_id: NodeId) -> bool {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("cal_open").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// The sweep running on this node, as the engine reads it: "off" / "pitch" / "yaw".
pub(crate) fn measuring(snarl: &Snarl<NodeData>, node_id: NodeId) -> String {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("cal_measure").and_then(|v| v.as_str()))
        .filter(|a| *a == "pitch" || *a == "yaw")
        .unwrap_or("off")
        .to_string()
}

/// The turn the method asks for, in degrees.
fn target_of(axis: &str) -> f32 {
    if axis == "yaw" { 360.0 } else { 180.0 }
}

/// The engine's trailing display outputs: degrees turned so far, and the sweep's
/// peak UNCLAMPED stick deflection. Read from `last_out` so every window showing
/// this widget sees the one measurement the engine is keeping, whatever each
/// one's repaint rate.
fn measured(snarl: &Snarl<NodeData>, node_id: NodeId) -> (f32, f32) {
    let Some(node) = snarl.get_node(node_id) else { return (0.0, 0.0) };
    let f = |i: usize| match node.extra.last_out.get(i).copied().flatten() {
        Some(Signal::Float(v)) if v.is_finite() => v,
        _ => 0.0,
    };
    (
        f(flexinput_engine::eval::JSM_CAL_DEG_OUT),
        f(flexinput_engine::eval::JSM_CAL_PEAK_OUT),
    )
}

/// Back-solve the constant for the chosen output and rewrite its line.
///
/// Mouse: the sweep drove the game at `RWC / IN_GAME_SENS` counts per degree and
/// the user reports the camera having turned `target`, so that many counts per
/// degree produced `target / |θ|` of the turn we want — scale by `|θ| / target`.
/// The answer is written back as a calibration, multiplied by the in-game
/// sensitivity the config divides it by, so a config that sets `IN_GAME_SENS`
/// keeps aiming the same after the line lands. A config that doesn't set it is
/// the neutral case, `IN_GAME_SENS = 1`.
///
/// Stick: the constant is a RATE rather than a gain, so it goes the other way —
/// a turn that took more rotation than asked means the stick is slower than it
/// claims.
fn finish(text: &str, axis: &str, stick: bool, theta: f32, peak: f32) -> (Option<String>, String) {
    let target = target_of(axis);
    if theta.abs() < 5.0 {
        return (None, "⚠ No rotation measured — nothing changed".into());
    }
    if stick {
        if peak > STICK_SAT_LIMIT {
            return (
                None,
                format!("⚠ Turned faster than full stick ({peak:.1}×) — turn slower and re-run"),
            );
        }
        let old = setting(text, VSC, VSC_DEFAULT).max(1.0);
        let new = (old * target / theta.abs()).max(1.0);
        let out = flexinput_engine::eval::jsm_set_setting(text, VSC, new, false);
        return (Some(out), format!("✓ Stick °/s {old:.0} → {new:.0}"));
    }
    let igs = setting(text, IGS, IGS_DEFAULT).max(1e-6);
    let old_rwc = setting(text, RWC, RWC_DEFAULT);
    let old_cal = old_rwc / igs;
    if old_cal <= 0.0 {
        return (
            None,
            format!("⚠ {RWC} is 0 — set a nonzero value first"),
        );
    }
    let new_cal = old_cal * theta.abs() / target;
    let new_rwc = new_cal * igs;
    let out = flexinput_engine::eval::jsm_set_setting(text, RWC, new_rwc, false);
    let note = if (igs - 1.0).abs() > 1e-3 {
        format!(" (at sens {igs})")
    } else {
        String::new()
    };
    (Some(out), format!("✓ RWC {old_rwc:.0} → {new_rwc:.0}{note}"))
}

/// The "Calibrate RWC" row and, once open, the measure controls under it.
///
/// Returns the header row's rect, so the tune panel can publish it as the nav
/// field the pad focuses before pressing South.
///
/// `force_open` is for the standalone pin, which IS the calibration widget —
/// there is nothing for it to collapse to, and a pin showing one folded row
/// would be a pin showing nothing.
pub(crate) fn calibrate_block(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    width: f32,
    text: &str,
    force_open: bool,
) -> (egui::Rect, CalEdits) {
    let mut e = CalEdits::default();
    let open = force_open || is_open(snarl, node_id);
    let axis = measuring(snarl, node_id);
    let sweeping = axis != "off";
    let (theta, peak) = measured(snarl, node_id);

    let (pending, out_stick, ref_on, finish_flag) = snarl
        .get_node(node_id)
        .map(|n| {
            let p = |k: &str| n.params.get(k);
            (
                p("cal_pending")
                    .and_then(|v| v.as_str())
                    .filter(|s| *s == "pitch" || *s == "yaw")
                    .unwrap_or("yaw")
                    .to_string(),
                p("cal_output").and_then(|v| v.as_str()) == Some("stick"),
                p("cal_ref_shot").and_then(|v| v.as_bool()).unwrap_or(false),
                p("cal_finish").and_then(|v| v.as_bool()).unwrap_or(false),
            )
        })
        .unwrap_or_else(|| ("yaw".into(), false, false, false));

    ui.set_max_width(width);

    // ── the header row: the label, and the sweep's progress while one runs ───
    let header = ui
        .horizontal(|ui| {
            let marker = if force_open { "" } else if open { "▾ " } else { "▸ " };
            let label = egui::RichText::new(format!("{marker}Calibrate RWC")).small();
            let label = if sweeping {
                label.strong().color(egui::Color32::from_rgb(255, 200, 80))
            } else {
                label
            };
            let header_click = ui
                .selectable_label(open && !force_open, label)
                .on_hover_text(
                    "Turn the camera a known amount; the constant for the chosen output is\n\
                     solved from how far the pad really turned.\n\n\
                     Mouse solves REAL_WORLD_CALIBRATION (times IN_GAME_SENS, which\n\
                     defaults to 1 when the config doesn't set it); Stick solves\n\
                     VIRTUAL_STICK_CALIBRATION.",
                )
                .clicked();
            // Pinned on its own it is always open, so the header is a label
            // rather than a disclosure.
            if header_click && !force_open {
                e.params.push(("cal_open", Value::Bool(!open)));
            }
            if sweeping {
                ui.label(
                    egui::RichText::new(format!(
                        "{:.0}° / {:.0}°",
                        theta.abs(),
                        target_of(&axis)
                    ))
                    .small()
                    .strong()
                    .color(egui::Color32::from_rgb(255, 200, 80)),
                );
            }
        })
        .response
        .rect;

    if !open {
        // A sweep started from the pad with the panel shut still has to be
        // finishable, so honour the flag even while collapsed.
        if finish_flag {
            apply_finish(&mut e, text, &axis, out_stick, theta, peak, sweeping, ui, node_id);
        }
        return (header, e);
    }

    if sweeping {
        // ── running: finish, cancel, and what to do with the camera ─────────
        ui.horizontal_wrapped(|ui| {
            if ui.button(egui::RichText::new("✓ Finish").small()).clicked() {
                e.params.push(("cal_finish", Value::Bool(true)));
            }
            if ui.button(egui::RichText::new("✗ Cancel").small()).clicked() {
                e.params.push(("cal_measure", Value::String("off".into())));
            }
        });
        if out_stick && peak > STICK_SAT_LIMIT {
            ui.label(
                egui::RichText::new(format!("⚠ Too fast — stick maxed ({peak:.1}×)"))
                    .small()
                    .color(egui::Color32::from_rgb(230, 150, 110)),
            );
        }
        let instruction = match (axis.as_str(), out_stick) {
            ("pitch", false) => "Turn the camera fully UP, then Finish",
            ("pitch", true) => "Turn fully UP, steadily, then Finish",
            (_, false) => "Turn one full 360°, then Finish",
            (_, true) => "Turn one full 360°, steadily, then Finish",
        };
        hint(ui, instruction);
        super::pad_hints::pad_hint(ui, "{btn_south} Finish · {btn_east} Cancel");
    } else {
        // ── row 1: which turn, and which output it solves for ───────────────
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(pending == "pitch", egui::RichText::new("↕180°").small())
                .on_hover_text("Vertical: aim straight DOWN, then turn straight UP.")
                .clicked()
            {
                e.params.push(("cal_pending", Value::String("pitch".into())));
            }
            if ui
                .selectable_label(pending == "yaw", egui::RichText::new("↔360°").small())
                .on_hover_text("Horizontal: turn one full 360°.")
                .clicked()
            {
                e.params.push(("cal_pending", Value::String("yaw".into())));
            }
            ui.separator();
            if ui
                .selectable_label(!out_stick, egui::RichText::new("Mouse").small())
                .on_hover_text(format!("Solve {RWC} (counts per degree)."))
                .clicked()
            {
                e.params.push(("cal_output", Value::String("mouse".into())));
            }
            if ui
                .selectable_label(out_stick, egui::RichText::new("Stick").small())
                .on_hover_text(format!("Solve {VSC} (deg/s at full deflection)."))
                .clicked()
            {
                e.params.push(("cal_output", Value::String("stick".into())));
            }
        });

        // ── row 2: the snapshot reference, and what the config says today ───
        ui.horizontal_wrapped(|ui| {
            // Only the 360° horizontal method has a frame worth comparing against.
            if pending == "yaw" {
                let mut shot = ref_on;
                if ui
                    .checkbox(&mut shot, egui::RichText::new("snapshot").small())
                    .on_hover_text(
                        "Freeze the game frame behind the overlay at sweep start and show\n\
                         its left half at 70% as an alignment reference for the full turn.",
                    )
                    .changed()
                {
                    e.params.push(("cal_ref_shot", Value::Bool(shot)));
                }
            }
            let cur = if out_stick {
                format!("= {:.0} °/s", setting(text, VSC, VSC_DEFAULT))
            } else {
                format!("= {:.1} cts/°", counts_per_degree(text))
            };
            ui.label(egui::RichText::new(cur).small().weak());
        });

        // ── rows 3+: the recent result, where to point, and the pad's keys ──
        if let Some((m, t)) = ui.ctx().data(|d| d.get_temp::<(String, f64)>(msg_key(node_id))) {
            if ui.input(|i| i.time) - t < 5.0 {
                let col = if m.starts_with('✓') {
                    egui::Color32::from_rgb(150, 220, 150)
                } else {
                    egui::Color32::from_rgb(230, 180, 120)
                };
                ui.label(egui::RichText::new(m).small().color(col));
            }
        }
        if pending == "pitch" {
            hint(ui, "Aim the camera straight DOWN first");
        } else {
            hint(ui, "Aim straight ahead first");
        }
        super::pad_hints::pad_hint(
            ui,
            "{btn_south} Start · {dpad_left}{dpad_right} method ·              {dpad_up}{dpad_down} out · {btn_north} snapshot · {btn_east} close",
        );
        if !out_stick {
            hint(
                ui,
                "No reference? Set a LOW in-game mouse sensitivity first — finer \
                 angular resolution measures better.",
            );
        }
    }

    if finish_flag {
        apply_finish(&mut e, text, &axis, out_stick, theta, peak, sweeping, ui, node_id);
    }
    (header, e)
}

/// Consume the `cal_finish` flag: solve and rewrite when a sweep is running,
/// and clear it either way so it can't fire again next frame.
#[allow(clippy::too_many_arguments)]
fn apply_finish(
    e: &mut CalEdits,
    text: &str,
    axis: &str,
    stick: bool,
    theta: f32,
    peak: f32,
    sweeping: bool,
    ui: &egui::Ui,
    node_id: NodeId,
) {
    e.params.push(("cal_finish", Value::Bool(false)));
    if !sweeping {
        return;
    }
    let (new_text, msg) = finish(text, axis, stick, theta, peak);
    e.text = new_text;
    e.params.push(("cal_measure", Value::String("off".into())));
    let now = ui.input(|i| i.time);
    ui.ctx()
        .data_mut(|d| d.insert_temp(msg_key(node_id), (msg, now)));
}

/// A guidance line: small, weak, and wrapped, since the column is narrow.
fn hint(ui: &mut egui::Ui, s: &str) {
    ui.label(egui::RichText::new(s).small().weak());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measured_turn_scales_the_calibration_it_was_measured_at() {
        // Driving at 40 counts/° turned the camera 360° for 180° of pad, so the
        // game is twice as sensitive as 1:1 — halve the calibration.
        let (t, msg) = finish("REAL_WORLD_CALIBRATION = 40", "yaw", false, 180.0, 0.0);
        assert_eq!(t.as_deref(), Some("REAL_WORLD_CALIBRATION = 20"));
        assert!(msg.starts_with('✓'), "{msg}");
    }

    #[test]
    fn in_game_sens_travels_with_the_answer() {
        // The config aims at 40/2 = 20 counts/°. A 180° pad turn for a 360° camera
        // turn wants 10 counts/°, and the line has to still be divided by 2 — so
        // 20 lands in the text, not 10.
        let cfg = "IN_GAME_SENS = 2\nREAL_WORLD_CALIBRATION = 40";
        let (t, _) = finish(cfg, "yaw", false, 180.0, 0.0);
        let out = t.unwrap();
        assert!(out.contains("REAL_WORLD_CALIBRATION = 20"), "{out}");
        // The sensitivity line is left exactly as the author wrote it.
        assert!(out.contains("IN_GAME_SENS = 2"), "{out}");
        assert_eq!(counts_per_degree(&out), 10.0);
    }

    #[test]
    fn a_config_that_never_set_the_command_calibrates_from_jsms_defaults() {
        // No RWC and no IN_GAME_SENS: 40 counts/° and a neutral sensitivity of 1.
        let (t, _) = finish("GYRO_SENS = 2", "yaw", false, 720.0, 0.0);
        let out = t.unwrap();
        // Two full pad turns for one camera turn → twice the calibration.
        assert!(out.contains("REAL_WORLD_CALIBRATION = 80"), "{out}");
        // Appended, with the config it was measured on left intact.
        assert!(out.starts_with("GYRO_SENS = 2"), "{out}");
    }

    #[test]
    fn the_stick_constant_goes_the_other_way() {
        // 720° of pad for the 360° asked → the stick turns half as fast as it says.
        let (t, msg) = finish("VIRTUAL_STICK_CALIBRATION = 360", "yaw", true, 720.0, 0.5);
        assert_eq!(t.as_deref(), Some("VIRTUAL_STICK_CALIBRATION = 180"));
        assert!(msg.contains("360 → 180"), "{msg}");
    }

    #[test]
    fn a_stick_pinned_at_full_tilt_is_refused_rather_than_solved() {
        let (t, msg) = finish("VIRTUAL_STICK_CALIBRATION = 360", "yaw", true, 400.0, 1.6);
        assert!(t.is_none(), "a saturated sweep must not write a constant");
        assert!(msg.starts_with('⚠') && msg.contains("1.6×"), "{msg}");
    }

    #[test]
    fn barely_moving_changes_nothing() {
        let (t, msg) = finish("REAL_WORLD_CALIBRATION = 40", "yaw", false, 2.0, 0.0);
        assert!(t.is_none());
        assert!(msg.starts_with('⚠'), "{msg}");
    }

    #[test]
    fn a_zero_calibration_says_so_instead_of_dividing_by_it() {
        let (t, msg) = finish("REAL_WORLD_CALIBRATION = 0", "yaw", false, 360.0, 0.0);
        assert!(t.is_none());
        assert!(msg.starts_with('⚠'), "{msg}");
    }

    #[test]
    fn the_pitch_method_solves_against_a_half_turn() {
        // 180° of pad for the 180° asked is already 1:1 — nothing should move.
        let (t, _) = finish("REAL_WORLD_CALIBRATION = 40", "pitch", false, 180.0, 0.0);
        assert_eq!(t.as_deref(), Some("REAL_WORLD_CALIBRATION = 40"));
    }

    #[test]
    fn turning_the_other_way_calibrates_the_same() {
        // The integral is signed so a turn back subtracts, but the ANSWER only
        // depends on how far round it went.
        let (a, _) = finish("REAL_WORLD_CALIBRATION = 40", "yaw", false, 180.0, 0.0);
        let (b, _) = finish("REAL_WORLD_CALIBRATION = 40", "yaw", false, -180.0, 0.0);
        assert_eq!(a, b);
    }

    #[test]
    fn an_authors_spacing_and_comment_survive_the_rewrite() {
        let cfg = "REAL_WORLD_CALIBRATION   =   40   # measured on the range";
        let (t, _) = finish(cfg, "yaw", false, 180.0, 0.0);
        let out = t.unwrap();
        assert_eq!(out, "REAL_WORLD_CALIBRATION   =   20   # measured on the range");
    }
}
