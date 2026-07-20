//! Network Send/Receive node bodies + persisted widget-size helpers.

use super::*;

/// Read a persisted widget size from node params (e.g. a resizable scope), or the
/// supplied default when absent.
pub(crate) fn read_widget_size(snarl: &Snarl<NodeData>, node_id: NodeId, key: &str, default: egui::Vec2) -> egui::Vec2 {
    snarl.get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_array()))
        .and_then(|a| Some(egui::vec2(a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32)))
        .unwrap_or(default)
}

/// Persist a resizable widget's size into node params so it survives reopen.
pub(crate) fn write_widget_size(snarl: &mut Snarl<NodeData>, node_id: NodeId, key: &str, size: egui::Vec2) {
    if let Some(n) = snarl.get_node_mut(node_id) {
        n.params.insert(key.into(), serde_json::json!([size.x, size.y]));
    }
}

/// EF oscilloscope for the Audio Stream Haptics node: a rolling time-domain trace
/// of the captured audio peak (faint) overlaid with the **shaped** haptic
/// amplitude (bright) that actually drives the rumble. The bright trace applies
/// the same Volume → Curve → amp-range shaping the engine does, so dragging those
/// sliders visibly reshapes it in real time (live preview). Fills the resizable
/// rect it's given.
/// Effective uid for a node's live engine-side data (ASTH captures, network
/// link status): raw node id at the top level, or the namespaced uid folded
/// through the sub-patch parent chain — must match the uid the manager
/// registers + the engine reads, otherwise the body finds no data (the "no
/// signal" the pinned/nested widget showed).
pub(crate) fn effective_publish_uid(node_id: NodeId, parent: Option<&AutomapGlowParent<'_>>) -> usize {
    match parent {
        None => node_id.0,
        Some(p) => flexinput_engine::namespaced_uid(crate::app::fold_outer_uid_app(p), node_id.0),
    }
}

// ── Network Send / Receive ────────────────────────────────────────────────────
//
// Persisted params (defaults applied at read time, see flexinput-net docs):
//   shared: net_transport ("udp" | "psk" | "quic"), net_psk (String)
//   send:   net_host (String), net_port (u16), net_rate_hz (u32)
//   recv:   net_bind_port (u16), net_stale_ms (u32), net_fb_rate_hz (u32)
//
// The bodies only edit params + display link status; sockets live in
// flexinput-net's manager (reconciled by the proc thread from the snapshot).

pub(crate) fn net_transport_label(t: &str) -> &'static str {
    match t {
        "psk" => "Secure (PSK)",
        // "quic" is a legacy alias kept so old patches still display sensibly.
        "p2p" | "quic" => "P2P (code)",
        _ => "LAN (UDP)",
    }
}

/// Read a node's transport param, normalizing the legacy "quic" alias to "p2p".
pub(crate) fn net_transport_of(node_id: NodeId, snarl: &Snarl<NodeData>) -> String {
    let t = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("net_transport").and_then(|v| v.as_str()))
        .unwrap_or("udp");
    if t == "quic" { "p2p".to_string() } else { t.to_string() }
}

/// Transport selector + (PSK tier only) passphrase field. Writes params in place.
pub(crate) fn net_transport_controls(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let transport = net_transport_of(node_id, snarl);

    ui.horizontal(|ui| {
        ui.label("Mode:");
        egui::ComboBox::from_id_salt(("net_transport", node_id))
            .selected_text(net_transport_label(&transport))
            .show_ui(ui, |ui| {
                for t in ["udp", "psk", "p2p"] {
                    if ui
                        .selectable_label(transport == t, net_transport_label(t))
                        .clicked()
                    {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            node.params
                                .insert("net_transport".to_string(), Value::String(t.to_string()));
                        }
                    }
                }
            });
    });

    // Passphrase applies only to the PSK-over-UDP tier. P2P authenticates by the
    // pairing code itself (iroh's TLS keyed by the endpoint keypair), so it needs
    // no passphrase.
    if transport == "psk" {
        let mut psk = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("net_psk").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        ui.horizontal(|ui| {
            ui.label("Passphrase:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut psk)
                    .password(true)
                    .desired_width(110.0),
            );
            if resp.changed() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("net_psk".to_string(), Value::String(psk.clone()));
                }
            }
        });
        let hint = if psk.is_empty() {
            "Both ends need the same passphrase."
        } else {
            "Saved as plain text in the patch file."
        };
        ui.label(egui::RichText::new(hint).small().weak());
    }
}

/// Numeric param row with label. Writes the param on change.
pub(crate) fn net_num_param(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    label: &str,
    key: &str,
    default: f64,
    range: std::ops::RangeInclusive<f64>,
    suffix: &str,
) {
    let val = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_f64()))
        .unwrap_or(default);
    let mut v = val;
    ui.horizontal(|ui| {
        ui.label(label);
        let resp = ui.add(
            egui::DragValue::new(&mut v)
                .range(range)
                .max_decimals(0)
                .suffix(suffix),
        );
        if resp.changed() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(n) = Number::from_f64(v.round()) {
                    node.params.insert(key.to_string(), Value::Number(n));
                }
            }
        }
    });
}

/// Live link-status row: colored dot + short state text + traffic counters.
pub(crate) fn net_status_row(uid: usize, ui: &mut egui::Ui) {
    let st = flexinput_net::status(uid);
    let (color, text) = match &st.state {
        flexinput_net::LinkState::Idle => (Color32::GRAY, "idle".to_string()),
        flexinput_net::LinkState::Listening => (Color32::YELLOW, "waiting for peer".to_string()),
        flexinput_net::LinkState::Connected => {
            let peer = st.remote.as_deref().unwrap_or("peer");
            (Color32::from_rgb(80, 220, 100), format!("connected · {peer}"))
        }
        flexinput_net::LinkState::Error(e) => (Color32::from_rgb(240, 80, 80), e.clone()),
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(egui::RichText::new(text).small());
    });
    if st.state == flexinput_net::LinkState::Connected || st.rx_pps > 0 || st.tx_pps > 0 {
        ui.label(
            egui::RichText::new(format!("tx {}/s · rx {}/s", st.tx_pps, st.rx_pps))
                .small()
                .weak(),
        );
    }
    if st.layout_warn {
        ui.label(
            egui::RichText::new("⚠ peer runs a different FlexInput version")
                .small()
                .color(Color32::YELLOW),
        );
    }
    // Live traffic counters only repaint if something requests frames.
    request_repaint_throttled(ui.ctx());
}

/// "Keep saved" checkbox: gates whether the identity params (peer code / secret)
/// are written to the patch + workspace/recovery backups. Off by default so a
/// shared patch never leaks them; the strip happens in `sanitize_snarl_for_save`.
pub(crate) fn net_keep_checkbox(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let mut keep = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("net_keep").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    if ui.checkbox(&mut keep, "Keep saved").changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("net_keep".to_string(), Value::Bool(keep));
        }
    }
    if !keep {
        ui.label(egui::RichText::new("Not saved — cleared on restart, kept out of shared patches.").small().weak());
    }
}

pub(crate) fn show_net_send_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    let uid = effective_publish_uid(node_id, automap_parent);
    let body = ui.vertical(|ui| {
        ui.set_min_width(170.0);
        net_transport_controls(node_id, ui, snarl);

        if net_transport_of(node_id, snarl) == "p2p" {
            // Dial-by-code: paste the peer Receive node's pairing code.
            let mut peer = snarl
                .get_node(node_id)
                .and_then(|n| n.params.get("net_peer").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            ui.label("Peer code:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut peer)
                    .hint_text("paste code")
                    .desired_width(150.0),
            );
            if resp.changed() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("net_peer".to_string(), Value::String(peer.trim().to_string()));
                }
            }
            net_keep_checkbox(node_id, ui, snarl);
        } else {
            let mut host = snarl
                .get_node(node_id)
                .and_then(|n| n.params.get("net_host").and_then(|v| v.as_str()))
                .unwrap_or("127.0.0.1")
                .to_string();
            ui.horizontal(|ui| {
                ui.label("Host:");
                let resp = ui.add(egui::TextEdit::singleline(&mut host).desired_width(110.0));
                if resp.changed() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params
                            .insert("net_host".to_string(), Value::String(host.trim().to_string()));
                    }
                }
            });
            net_num_param(node_id, ui, snarl, "Port:", "net_port", 46700.0, 1.0..=65535.0, "");
        }
        net_num_param(node_id, ui, snarl, "Rate:", "net_rate_hz", 500.0, 30.0..=2000.0, " Hz");
        ui.separator();
        net_status_row(uid, ui);
    });
    // Whole node is pinnable to a sub-patch's Easy-mode layout.
    register_exposable_element(ui, node_id, "whole_module", body.response.rect);
}

pub(crate) fn show_net_recv_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    let uid = effective_publish_uid(node_id, automap_parent);
    let body = ui.vertical(|ui| {
        ui.set_min_width(170.0);
        net_transport_controls(node_id, ui, snarl);

        if net_transport_of(node_id, snarl) == "p2p" {
            // Ensure a stable node secret exists (its public key is our code).
            let mut secret = snarl
                .get_node(node_id)
                .and_then(|n| n.params.get("net_secret").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            if secret.is_empty() {
                secret = flexinput_net::generate_secret_key();
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("net_secret".to_string(), Value::String(secret.clone()));
                }
            }
            // Prefer the code derived directly from the secret (instant); fall
            // back to whatever the worker published once bound.
            let code = flexinput_net::endpoint_id_for_secret(&secret)
                .or_else(|| flexinput_net::status(uid).code);
            ui.label("Your code (share with sender):");
            if let Some(code) = code {
                ui.horizontal(|ui| {
                    if ui.button("📋 Copy").clicked() {
                        ui.ctx().copy_text(code.clone());
                    }
                    ui.add(
                        egui::Label::new(egui::RichText::new(&code).monospace().small())
                            .truncate(),
                    );
                });
            } else {
                ui.label(egui::RichText::new("(starting…)").small().weak());
            }
            net_keep_checkbox(node_id, ui, snarl);
        } else {
            net_num_param(node_id, ui, snarl, "Listen port:", "net_bind_port", 46700.0, 1.0..=65535.0, "");
        }
        net_num_param(node_id, ui, snarl, "Fail-safe after:", "net_stale_ms", 200.0, 50.0..=5000.0, " ms");
        net_num_param(node_id, ui, snarl, "Feedback rate:", "net_fb_rate_hz", 200.0, 30.0..=1000.0, " Hz");
        ui.separator();
        net_status_row(uid, ui);
    });
    // Whole node is pinnable to a sub-patch's Easy-mode layout.
    register_exposable_element(ui, node_id, "whole_module", body.response.rect);
}
