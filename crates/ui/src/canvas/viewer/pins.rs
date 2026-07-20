//! Pin rendering: type-colored PinInfo and the header-row snarl pin, plus the
//! direction-generic editing of nodes' dynamic (user-added) pin lists.

use super::*;

// ── dynamic pin lists ─────────────────────────────────────────────────────────

/// One side (inputs or outputs) of a node's dynamic pin list.
///
/// Editing those lists is identical for both sides; only five primitives
/// differ — which snarl id type addresses a pin, which end of
/// `connect(from_out, to_in)` the remote goes on, which pin vector to rewrite,
/// and how to read a pin's index and wires. Naming them lets the editing
/// algorithms exist once instead of as mirrored pairs that drift apart (MIDI
/// in/out, AutoMap Splitter/Collector).
pub(crate) trait PinSide {
    /// The pin type snarl hands the body (`OutPin` / `InPin`).
    type Pin;
    /// The id of the pin at the OTHER end of a wire, as stored in `remotes`.
    type Remote: Copy;

    fn index(pin: &Self::Pin) -> usize;
    fn remotes(pin: &Self::Pin) -> &[Self::Remote];
    /// Drop every wire attached to this side's pin `idx`.
    fn drop_at(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize);
    /// Reconnect `remote` to this side's pin `idx`.
    fn connect(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize, remote: Self::Remote);
    fn pins_mut(node: &mut NodeData) -> &mut Vec<PinDescriptor>;
}

pub(crate) struct Outputs;
pub(crate) struct Inputs;

impl PinSide for Outputs {
    type Pin = OutPin;
    type Remote = egui_snarl::InPinId;

    fn index(pin: &OutPin) -> usize { pin.id.output }
    fn remotes(pin: &OutPin) -> &[Self::Remote] { &pin.remotes }
    fn drop_at(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize) {
        snarl.drop_outputs(OutPinId { node, output: idx });
    }
    fn connect(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize, remote: Self::Remote) {
        snarl.connect(OutPinId { node, output: idx }, remote);
    }
    fn pins_mut(node: &mut NodeData) -> &mut Vec<PinDescriptor> { &mut node.outputs }
}

impl PinSide for Inputs {
    type Pin = InPin;
    type Remote = OutPinId;

    fn index(pin: &InPin) -> usize { pin.id.input }
    fn remotes(pin: &InPin) -> &[Self::Remote] { &pin.remotes }
    fn drop_at(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize) {
        snarl.drop_inputs(InPinId { node, input: idx });
    }
    fn connect(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize, remote: Self::Remote) {
        snarl.connect(remote, InPinId { node, input: idx });
    }
    fn pins_mut(node: &mut NodeData) -> &mut Vec<PinDescriptor> { &mut node.inputs }
}

/// Remove pin `rm_idx`, then slide every later pin down one slot, carrying its
/// wires with it (snarl addresses wires by index, so the tail must be dropped
/// and re-made rather than left dangling on stale indices).
///
/// `ids_key` names the params array of stable pin ids to keep in step, and
/// `id_offset` is how many leading pins that array does NOT cover — the
/// AutoMap Collector's ids exclude its passthrough `input[0]`, so its entry
/// for pin `n` lives at `n - 1`.
pub(crate) fn remove_dynamic_pin<S: PinSide>(
    node_id: NodeId,
    rm_idx: usize,
    pins: &[S::Pin],
    snarl: &mut Snarl<NodeData>,
    ids_key: &str,
    id_offset: usize,
) {
    let tail: Vec<Vec<S::Remote>> = pins[rm_idx..].iter().map(|p| S::remotes(p).to_vec()).collect();
    for i in 0..tail.len() {
        S::drop_at(snarl, node_id, rm_idx + i);
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        S::pins_mut(node).remove(rm_idx);
        if let Some(Value::Array(ids)) = node.params.get_mut(ids_key) {
            if let Some(i) = rm_idx.checked_sub(id_offset) {
                if i < ids.len() {
                    ids.remove(i);
                }
            }
        }
    }
    // `skip(1)` drops the removed pin's own wires; the rest shift down one.
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        for remote in remotes {
            S::connect(snarl, node_id, rm_idx + shift - 1, remote);
        }
    }
}

/// Keep only pins that have at least one wire, compacting the rest away.
pub(crate) fn clear_unused_dynamic_pins<S: PinSide>(
    node_id: NodeId,
    pins: &[S::Pin],
    snarl: &mut Snarl<NodeData>,
    ids_key: &str,
) {
    let connected: Vec<(usize, Vec<S::Remote>)> = pins
        .iter()
        .filter(|p| !S::remotes(p).is_empty())
        .map(|p| (S::index(p), S::remotes(p).to_vec()))
        .collect();

    for p in pins {
        S::drop_at(snarl, node_id, S::index(p));
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept_pins: Vec<PinDescriptor> = connected
            .iter()
            .map(|(idx, _)| S::pins_mut(node)[*idx].clone())
            .collect();
        let kept_ids: Vec<Value> = node.params.get(ids_key)
            .and_then(|v| v.as_array())
            .map(|ids| connected.iter()
                .map(|(idx, _)| ids.get(*idx).cloned().unwrap_or(Value::String(String::new())))
                .collect())
            .unwrap_or_default();
        *S::pins_mut(node) = kept_pins;
        if let Some(Value::Array(ids)) = node.params.get_mut(ids_key) {
            *ids = kept_ids;
        }
    }

    for (new_idx, (_, remotes)) in connected.iter().enumerate() {
        for &remote in remotes {
            S::connect(snarl, node_id, new_idx, remote);
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

pub(crate) fn pin_info(t: SignalType) -> PinInfo {
    let [r, g, b] = t.color_rgb();
    let outline = Color32::from_rgb(r, g, b);
    // Wire at rest is desaturated/dim (~45% of outline); the per-frame
    // brighten_wire_color() pumps it toward full + a small white lerp under
    // signal so flowing wires visibly stand out without idle wires shouting.
    let wire_rest = Color32::from_rgb(
        (r as u16 * 9 / 20) as u8,
        (g as u16 * 9 / 20) as u8,
        (b as u16 * 9 / 20) as u8,
    );
    let stroke = egui::Stroke::new(1.5, outline);
    if t == SignalType::AutoMap {
        // AutoMap chip needs more presence at rest — bump the dark interior
        // from ~20% to ~40% of the type color so the square reads clearly
        // even before any signal lights it up.
        let dark = Color32::from_rgb(
            (r as u16 * 2 / 5) as u8,
            (g as u16 * 2 / 5) as u8,
            (b as u16 * 2 / 5) as u8,
        );
        PinInfo::square()
            .with_fill(dark)
            .with_stroke(stroke)
            .with_wire_width_factor(4.0)
            .with_wire_color(wire_rest)
    } else {
        // Dark fill ~20% of the type color so the pin reads as an outlined ring.
        let dark = Color32::from_rgb(r / 5, g / 5, b / 5);
        PinInfo::circle()
            .with_fill(dark)
            .with_stroke(stroke)
            .with_wire_color(wire_rest)
    }
}

/// Which side of a half-circle pin is FLAT (i.e. faces the node body).
#[derive(Clone, Copy)]
pub(crate) enum HalfSide { Left, Right }

/// SnarlPin wrapper. Falls through to the default snarl pin geometry except:
/// - When `glow` is `Some`, paints a radial halo behind the pin scaled by
///   intensity (0..1) — used to make device pins light up under live signal.
/// - When `half` is `Some`, renders as a half-circle (flat side toward the
///   node) instead of a full circle — so pins sit flush against the node body
///   even though PinPlacement::Outside places them outside the frame.
/// - When `header_y` is `Some(y)`, the pin's vertical center is forced to `y`
///   (the device header's Y center) while X and shape stay column-aligned.
///   Used for the device source/sink AutoMap pins so they sit at the header
///   level (matching other modules' inlet/outlet layout) instead of in their
///   natural last-row column slot.
pub(crate) struct MaybeHeaderPin {
    pub(crate) inner: PinInfo,
    pub(crate) glow: Option<(Color32, f32)>,
    pub(crate) half: Option<HalfSide>,
    pub(crate) header_y: Option<f32>,
}

impl egui_snarl::ui::SnarlPin for MaybeHeaderPin {
    fn pin_rect(&self, x: f32, y0: f32, y1: f32, size: f32) -> egui::Rect {
        // Header-relocated AutoMap: keep snarl's column-aligned X and
        // half-side shift, but force Y to the device header's center.
        let y = self.header_y.unwrap_or((y0 + y1) * 0.5);
        // For half-circle pins, shift the center inward so the flat edge
        // (which lives at `center.x`) sits flush with the node body edge
        // — i.e. counteract PinPlacement::Outside's outward offset and
        // align with where snarl's default pin's outer edge would have
        // been. snarl gives us `x` = pin center under Outside; the pin's
        // outermost extent sits at x ± size/2. We want the flat edge
        // (center.x) to coincide with that outermost extent, so shift
        // the center inward by size/2.
        let cx = match self.half {
            Some(HalfSide::Right) => x + size * 0.5, // input → shift right (toward node)
            Some(HalfSide::Left)  => x - size * 0.5, // output → shift left  (toward node)
            None                  => x,
        };
        egui::Rect::from_center_size(egui::pos2(cx, y), egui::vec2(size, size))
    }
    fn draw(
        self,
        snarl_style: &egui_snarl::ui::SnarlStyle,
        style: &egui::Style,
        rect: egui::Rect,
        painter: &egui::Painter,
    ) -> egui_snarl::ui::PinWireInfo {
        // Paint the radial-glow halo *behind* the pin so the colored outline
        // remains crisp on top. Halo extends to 2.2× the pin radius.
        //
        // For half-circle pins we use a half-disc glow shaped to the same
        // outward sweep, so the halo doesn't bleed into the node body.
        if let Some((hot, intensity)) = self.glow {
            if intensity > 0.01 {
                let center = rect.center();
                let pin_r = (rect.width().min(rect.height())) * 0.5;
                let halo_r = pin_r * 2.2;
                match self.half {
                    None => paint_radial_glow(painter, center, halo_r, hot, intensity),
                    Some(HalfSide::Right) => paint_radial_glow_half(
                        painter, center, halo_r, hot, intensity,
                        std::f32::consts::FRAC_PI_2,
                        std::f32::consts::FRAC_PI_2 * 3.0,
                    ),
                    Some(HalfSide::Left) => paint_radial_glow_half(
                        painter, center, halo_r, hot, intensity,
                        -std::f32::consts::FRAC_PI_2,
                        std::f32::consts::FRAC_PI_2,
                    ),
                }
            }
        }
        // Lerp the inner fill from the default dark toward the hot type-color
        // by the glow intensity, so the inside of the pin gets visibly brighter
        // as signal flows (not just the halo outside it).
        let inner = if let Some((hot, intensity)) = self.glow {
            // `pin_info` always sets `fill`, so the unwrap is the live path;
            // the fallback only matters if some other callsite forgets it.
            let base = self.inner.fill.unwrap_or(Color32::from_gray(20));
            let t = intensity.clamp(0.0, 1.0);
            let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
            let mixed = Color32::from_rgb(
                lerp(base.r(), hot.r()),
                lerp(base.g(), hot.g()),
                lerp(base.b(), hot.b()),
            );
            PinInfo {
                fill: Some(mixed),
                ..self.inner
            }
        } else {
            self.inner
        };

        // Half-square path: AutoMap pins keep their square shape but show
        // only the outward-facing half (flat side flush with the node body).
        if let Some(side) = self.half {
            if inner.shape == Some(egui_snarl::ui::PinShape::Square) {
                let fill = inner.fill.unwrap_or(Color32::from_gray(20));
                let stroke = inner.stroke.unwrap_or(egui::Stroke::new(1.5, Color32::WHITE));
                let size = rect.width().min(rect.height());
                let center = rect.center();
                // Build the visible (outward) half of the square. Flat edge
                // sits at center.x; the half-rect extends outward by size/2.
                let half_rect = match side {
                    // Input pin: flat edge on the right (toward node), visible
                    // half extends to the LEFT of center.
                    HalfSide::Right => egui::Rect::from_min_max(
                        egui::pos2(center.x - size * 0.5, center.y - size * 0.5),
                        egui::pos2(center.x,              center.y + size * 0.5),
                    ),
                    // Output pin: flat edge on the left, visible half extends RIGHT.
                    HalfSide::Left => egui::Rect::from_min_max(
                        egui::pos2(center.x,              center.y - size * 0.5),
                        egui::pos2(center.x + size * 0.5, center.y + size * 0.5),
                    ),
                };
                painter.rect_filled(half_rect, 0.0, fill);
                // Stroke only the three outward edges so the flat side
                // doesn't draw a line through the node body.
                let (tl, tr, br, bl) = (
                    half_rect.left_top(),
                    half_rect.right_top(),
                    half_rect.right_bottom(),
                    half_rect.left_bottom(),
                );
                let outward_pts: Vec<egui::Pos2> = match side {
                    HalfSide::Right => vec![tr, tl, bl, br], // outward = left
                    HalfSide::Left  => vec![tl, tr, br, bl], // outward = right
                };
                painter.add(egui::Shape::line(outward_pts, stroke));
                // Synthesize the PinWireInfo from PinInfo's fields rather
                // than calling PinInfo::draw (which would re-paint the full
                // square on top of our half-rect). Wire style/width fall back
                // to PinInfo's snarl-style-aware accessors.
                let base_wire = inner.wire_color
                    .or(inner.fill)
                    .unwrap_or(Color32::WHITE);
                let mut color = base_wire;
                if let Some((_hot, intensity)) = self.glow {
                    color = brighten_wire_color(color, intensity);
                }
                return egui_snarl::ui::PinWireInfo {
                    color,
                    style: inner.wire_style.unwrap_or(egui_snarl::ui::WireStyle::Bezier5),
                    width_factor: inner.wire_width_factor.unwrap_or(1.0),
                };
            }
            use egui::epaint::{PathShape, PathStroke};
            let fill = inner.fill.unwrap_or(Color32::from_gray(20));
            let stroke = inner.stroke.unwrap_or(egui::Stroke::new(1.5, Color32::WHITE));
            let center = rect.center();
            let r = (rect.width().min(rect.height())) * 0.5;
            // Flat edge is vertical: x = center.x for inputs (flat on right,
            // curve sweeps to the left, i.e. outward) and for outputs (flat
            // on left, curve sweeps right, i.e. outward).
            //
            // Angle convention: 0 = +X (right), π/2 = +Y (down).
            // Input pin (flat right): sweep from +π/2 (down) → 3π/2 (up)
            //   through π (left). The convex side faces LEFT (away from node).
            // Output pin (flat left): sweep from -π/2 → +π/2 through 0
            //   (right). Convex side faces RIGHT (away from node).
            let (a0, a1) = match side {
                HalfSide::Right => (std::f32::consts::FRAC_PI_2,  std::f32::consts::FRAC_PI_2 * 3.0), // input
                HalfSide::Left  => (-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2),       // output
            };
            const N: usize = 12;
            let mut pts: Vec<egui::Pos2> = Vec::with_capacity(N + 1);
            for i in 0..=N {
                let t = i as f32 / N as f32;
                let a = a0 + (a1 - a0) * t;
                pts.push(center + r * egui::vec2(a.cos(), a.sin()));
            }
            // Two-pass paint so the outline traces ONLY the curved edge,
            // not the flat side that meets the node body. First a closed
            // filled half-disc (no stroke), then an OPEN polyline over
            // just the arc points so the stroke skips the flat closing.
            painter.add(egui::Shape::Path(PathShape {
                points: pts.clone(),
                closed: true,
                fill,
                stroke: PathStroke::NONE,
            }));
            painter.add(egui::Shape::Path(PathShape {
                points: pts,
                closed: false,
                fill: Color32::TRANSPARENT,
                stroke: PathStroke::from(stroke),
            }));

            // Wire info: same defaults PinInfo::draw computes.
            let mut wire_info = egui_snarl::ui::PinWireInfo {
                color: inner.wire_color.unwrap_or(fill),
                style: inner.wire_style.unwrap_or(egui_snarl::ui::WireStyle::Bezier5),
                width_factor: inner.wire_width_factor.unwrap_or(1.0),
            };
            if let Some((_hot, intensity)) = self.glow {
                wire_info.color = brighten_wire_color(wire_info.color, intensity);
            }
            return wire_info;
        }

        let mut wire_info = PinInfo::draw(&inner, snarl_style, style, rect, painter);
        if let Some((_hot, intensity)) = self.glow {
            wire_info.color = brighten_wire_color(wire_info.color, intensity);
        }
        wire_info
    }
}
