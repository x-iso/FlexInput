# Layout faults found and not fixed

Things the audit of the Macro Output / Envelope / Oscillator batch measured and could not be
settled inside this batch. Each one is a measurement, not a guess: the tests that produce the
numbers are in `crates/ui/src/canvas/grid_tests.rs` and are marked `#[ignore]` so the suite stays
green until someone picks the fault up. Dated 2026-11-13, on `feat/canvas-grid`, grid of 28-point
blocks.

---

## 1. A Macro list past eight ports loses its bottom rows

**Symptom.** With nine ports at the module's default width, the last row draws its drag dots and
its name and nothing else: no icon, no signal-type ComboBox, no `×`. They are not merely hidden -
they do not answer the pointer either. Clicking where the `×` of port 8 is traced removes nothing,
while the same click on port 2 removes the port.

**What is measured.** Counting words actually in the paint list, by port count (`Rig::painted_all`):

| ports | handles seen | `×` seen | type words seen | frame |
|------:|-------------:|---------:|----------------:|------:|
| 1..8  | as many as ports | as many as ports | as many as ports | 196 x 84…280 |
| 9     | 9 | 8 | 8 | 196 x 308 |
| 10    | 10 | 8 | 8 | 196 x 336 |
| 11    | 11 | 8 | 8 | 196 x 364 |
| 12    | 12 | 8 | 8 | 196 x 392 |

The rows themselves are laid out where they should be - the body traces `macro_row_8`,
`macro_name_8`, `macro_type_8` and `macro_remove_8` at correct rects inside the frame - and the
widgets report `child.is_visible() == true`, `enabled == true` and no pending discard. The gap is
the clip:

```
row 8:  rm rect  [252.0 549.5] - [274.0 569.5]
        child_clip / parent_clip  [[100.0 300.0] - [282.0 544.5]]
        parent_max_rect           [[108.0 325.0] - [274.0 594.0]]
```

so the body's **column runs 49.5 points below the area it is clipped to**, and everything a widget
paints there has an empty intersection with the clip: egui emits the shape with a degenerate clip
and the pass throws it away, and the hit test drops it with it. The handle's dots and the name are
painted by the body with `painter.text` at a point inside the clip's *x*-range, so their shapes
survive the cull and the fault looks selective - it is not.

**Why the clip is short.** snarl clips the body to the node's own rect
(`vendor/egui-snarl/src/ui.rs:2847`, `payload_clip_rect`, applied at 2593), and it expands that
rect with `final_rect.intersect(payload_clip_rect)` (2598) - the body can only grow the node as far
as the *previous* frame's clip, while FlexInput's grid rounds the drawn frame out to whole blocks
from the pin band. For nine ports the pins make the frame eleven blocks tall (308) and the node
rect snarl measures is roughly nine of them; the list is centred on the column, so its tail lands
in the rounding.

**What was tried.**

1. Reserve the band the rows actually occupy (`list_h` plus the centring offset) rather than
   `list_h` from the cursor - **no change**, the counts stayed 8/8/8.
2. Centre the list in `ui.max_rect().intersect(ui.clip_rect())` instead of in `ui.max_rect()` -
   **helps**: nine and ten ports came out complete, eleven and twelve still lost their last row,
   because the node rect itself cannot hold eleven rows.
3. Ask for one row of slack (`list_h + row_h`) so the node grows - **wrong fix**: the ask drives
   the height, so the short lists grew too (one port 84 → 140, three ports 140 → 168) while eleven
   and twelve were still short.

**Where the real fix sits.** Either the body clip in vendored snarl has to be the node rect *after*
the pins have been laid out (not the previous frame's), or the grid has to size a list module from
its pin band and hand that same rect to the body as its clip. Both are wider than this batch: the
first touches every node on the canvas, the second changes how `kit::strip_pitch` consumers are
measured. Until then a Macro past eight ports has a dead zone at the bottom of its list.

**Reproducing it.** `a_long_macro_list_keeps_every_rows_controls` (ignored) asserts nine painted
`×`s and nine type words for a nine-port Macro and fails with eight of each.

---

## 2. Twenty-one modules still paint their body over their own header when folded

The fold guard is `simple_bodies::node_open(snarl, node)` - reserve what the rows ask for, then
draw nothing. The batch's three modules have it, as do the Selector, Split, Macro, Dropdown and
AutoMap Combiner strip bodies and the Knob had it already. The rest of these still lay their bodies
out in the clipped space above a collapsed header, where they are out of sight and **still take the
pointer**:

| module | words above the frame | folded to |
|---|---:|---|
| `module.audio_stream_haptics` | 7 | 280 x 56 |
| `module.area_mapper` | 5 | 504 x 56 |
| `module.touch_zones` | 4 | 476 x 56 |
| `module.menu` | 4 | 616 x 56 |
| `module.vec_reshape` | 3 | 308 x 56 |
| `module.average` | 2 | 112 x 56 |
| `module.dc_filter` | 2 | 112 x 56 |
| `module.switch` | 1 | 112 x 56 |
| `module.label` | 1 | 168 x 56 |
| `module.svg` | 1 | 196 x 56 |
| `module.delay` | 1 | 112 x 56 |
| `module.twoway_response_curve` | 1 | 336 x 56 |
| `module.automap_twoway_response_curve` | 1 | 336 x 56 |
| `module.stick_rotation` | 1 | 252 x 56 |
| `module.input_viewer` | 1 | 532 x 56 |

`module.delay`, `module.average`, `module.dc_filter`, `module.switch`, `module.vec_reshape` and
`module.stick_rotation` are pill bodies from earlier batches on this branch, so the omission is
this project's own: the pills reserve their ask (the folded frame keeps its width) but the pill
widgets are still added above it. `module.label` is a different question - its text *is* its body,
and a folded label showing nothing may be right; it needs a decision, not a guard.

**Reproducing it.** `audit_which_folded_bodies_paint_over_their_header` (ignored) prints that
table for all 75 modules:
`cargo test -p flexinput-ui --lib audit_which_folded_bodies -- --ignored --nocapture`.

---

## 3. A folded port-list module cannot say which socket is which

A Selector, Split or Macro Output keeps the port name in its strip rows, and `kit::strip_pin`
suppresses the word beside the socket so the name is not drawn twice. Folded, the strips are gone -
and the socket labels cannot take their place, because a collapsed node's **pin column is laid out
above the header and clipped there**. Measured for a folded Selector (frame 56 tall):

```
y-86.5  "sel"  "out"
y-58.5  "0"
y-30.5  "1"
```

Every one of those words is painted outside the frame and cut. The interim step is already in:
`show_input`/`show_output` no longer draw those words when the node is folded, so they at least
stopped making a folded Split a block wider than its header. Getting the names back means moving
the label into the header row that carries the folded sockets - snarl-side work, and it needs the
same room arithmetic the header title already uses (`docs/MODULE_UI_GUIDELINES.md`, "The header:
marks and the pin stepper").

---

## 4. Left open by decision, not by failure

- **The scaled-grid overlay for response curves.** `show_scaled_grid`
  (`crates/ui/src/canvas/viewer/curve_bodies.rs:1799`) still draws the curve grid at unscaled
  spacing. Deferred to the response-curve batch.
- **The Envelope's per-copy graph overrides.** Pinned copies keep their own grid/labels/snap in the
  inspector *and* the module's header carries the switches. Kept deliberately - the grid options
  are pinnable items, so they stay on the module's UI; worth revisiting only if the two ever
  disagree in a way that reads as a bug.
- **Modules sized far past what they hold**, from `measure_every_module_size` (ignored), for a
  future batch: `module.menu 616 x 392`, `module.area_mapper 504 x 560`,
  `processing.gyro_3dof 504 x 476`, `module.touch_zones 532 x 336`, `processing.rws 448 x 448`,
  `display.oscilloscope 476 x 336`, `module.feedback_control 196 x 644` (44 inputs - its height is
  its pin band, so it is a different problem from the others).
