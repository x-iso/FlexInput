# Layout faults found and not fixed

Things the audit of the Macro Output / Envelope / Oscillator batch measured and could not be
settled inside this batch. Each one is a measurement, not a guess: the tests that produce the
numbers are in `crates/ui/src/canvas/grid_tests.rs` and are marked `#[ignore]` so the suite stays
green until someone picks the fault up. Dated 2026-11-13, on `feat/canvas-grid`, grid of 28-point
blocks.

---

## 1. A Macro list past eight ports loses its bottom rows - settled: it never did

This one was the test rig, not the module. The rig's screen is 1600 x 1000 points and the canvas
in it is zoomed to 2x, so it shows 500 graph points of height; a nine-port Macro is taller than
what is left under it, and the "clip that stops 49.5 points short" was the bottom edge of the
screen. Widgets past it are culled and can't be clicked, exactly as measured - in the rig only.
`a_long_macro_list_keeps_every_rows_controls` now runs (not ignored) with the list in view, and
finds every row's handle, type and `×`.

What it leaves behind is a rule for reading the rig, now in `docs/MODULE_UI_GUIDELINES.md`
("Pitfalls"): before a missing shape is blamed on a clip, check that the thing is on the rig's
screen at all.

---

## 2. Twenty-one modules paint their body over their own header when folded - fixed, in one place

The guard used to be a line each body had to remember (`simple_bodies::node_open`: reserve the
ask, then return), and twenty-one of seventy-five had not. It is now in `show_body`
(`crates/ui/src/canvas/viewer.rs`), ahead of every body: a folded node's body is still laid out,
so the folded frame keeps the width it had, but its painter is switched off and its clip is
emptied, so nothing is drawn and nothing can be hit. Every module folds to exactly the size it
did before (compared for all 75), and `no_folded_module_draws_its_body` holds all of them to it.

The per-body `node_open` returns that were already there still work and are now redundant; they
can go when those bodies are next touched. `module.label` falls under the same rule: folded, a
note shows its header and not its text.

One thing a folded module does still draw, on purpose: a Knob, a Constant and a Readout write
their number in the band a folded frame keeps under its header (`simple_bodies::folded_reading`,
painted by `show_body` itself). The first cut of this guard took the Knob's number away with
everything else; `a_folded_knob_constant_and_readout_still_show_their_number` keeps it.

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
