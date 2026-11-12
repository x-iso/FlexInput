# Iterating module body designs

A brief for proposing new layouts for a module's body, the part of a module
between its input and output pins. It's written to be handed, as is, to
whoever does the proposing, a person or a language model, together with the
module's current body code. Proposals come back as text mockups plus code, get
compared and picked, and the picks are integrated and checked in the app.

The rules the kit implements, and why, are in
[MODULE_UI_GUIDELINES.md](./MODULE_UI_GUIDELINES.md). This file is the short,
practical version for one task: redesign one body.

## What you're given

- The module's current body function (Rust, egui), e.g. `show_counter_body`.
- What the module does, in a sentence or two, and its params (names, types,
  ranges, defaults) as the current body reads them.
- This file.

## What you may change

Only how the body is laid out and labelled:

- The order and grouping of rows, and section captions.
- Which kind of row each setting gets: a slider with a value box, a value box
  alone, an option strip, a checkbox, a stepper, a pair of value boxes.
- Label text (a word or two), units (`.suffix(" ms")`), decimals, drag speed,
  whether a slider rail is logarithmic.
- Hover texts (the help). Rewrite them, don't drop them.
- When a control is greyed out (a setting that does nothing in the current
  mode, a value its input pin overrides when wired).
- The narrowest width the body needs (`kit_min_width`).

## What you must not change

- **Param keys and what's stored in them.** `"window_ms"` stays `"window_ms"`,
  holding the same unit and range. Show a value differently (a percent of a
  0..1 value, say) only by converting both ways in the body.
- **Pinnable element ids.** Every `register_exposable_element(ui, node_id, "id",
  rect)` call stays, with the same id, registering the rect of the rows that make
  up that element. Pinned widgets and the config overlay find elements by id. An
  element spread over several rows registers their union: `r1.union(r2)`.
- **Behaviour.** No new params (the engine wouldn't read them), none removed,
  no settings dropped from the body, no engine code.
- **Pins.** Their names, order and types belong to the module, not the body.
  (The Channels stepper adds and removes pin pairs; that's existing behaviour.)
- **Other modules, the canvas, the kit itself.** If the kit lacks something a
  design needs, say so in the notes instead of working around it.

## Building blocks

All bodies are built from the kit (`crates/ui/src/canvas/viewer/kit.rs`).
`kit::body(ui, w, |ui, k| { ... })` lays out a body `w` wide; inside it, `k`'s
methods each draw one row and return `(changed or response, row rect)`.

| Call | Draws |
| ---- | ----- |
| `k.slider(ui, "Label", &mut v, lo..=hi, log, \|v\| egui::DragValue::new(v)...)` | Label, slider rail, value box |
| `k.value(ui, "Label", &mut v, \|v\| egui::DragValue::new(v)...)` | Label, value box on the right edge |
| `k.pair(ui, "Label", ("Min", &mut a, enabled), ("Max", &mut b, enabled), \|v\| ...)` | Label, two captioned value boxes |
| `k.choice(ui, Some("Label"), &mut value, &[(value, "Text", "Hover"), ...])` | Label, strip of equal option cells (`None` label: the strip takes the whole row) |
| `kit::strip(ui, width, &mut value, &[...])` | The option strip alone, inside a `k.row` |
| `k.toggle(ui, "Label", &mut b, "text")` | Label, checkbox with its own text |
| `k.stepper(ui, "Label", n, 1..=64, ("− hover", "+ hover"))` | Label, `− n +`; returns `Some(-1)` / `Some(1)` |
| `k.row(ui, "Label", \|ui, rest\| { ... })` | Label, then your own content, `rest` points wide |
| `k.full_row(ui, \|ui, w\| { ... })` | A row with no label cell |
| `k.section(ui, "Title")` | Small caption and a hairline |
| `k.note(ui, "text")` | Small, weak, wrapped text (use sparingly; prefer hover text) |

Helpers in `kit_bodies.rs`: `param_f32 / param_str / param_bool(snarl, node_id,
key, default)` read a param, `set_f32 / set_value(snarl, node_id, key, v)` write
one, `wired(inputs, i)` says whether input pin `i` has a wire, `channels_row(...)`
is the shared Channels stepper.

Greying out: wrap the row, `ui.add_enabled_ui(enabled, |ui| k.value(...)).inner`.

## Sizes

- Label cell 64 points, value box 50, gap between cells 4, between rows 4, a
  row is about 18 tall. A body is at least 174 wide (label, a 52-point rail, a
  value box).
- The canvas lays modules out in blocks of 28 points; a module's width and
  height round up to whole blocks. (Every second block is the 56-point major
  line, and the sizes in these notes are written against that.) The body gets
  the module's width less its pin label columns and about 30 points of frame
  and spacing. A body 174 to about 210 wide therefore lands somewhere within
  28 points above what it asks for rather than on a 56-point step.
- Fit is your job: count characters. A label of 10 characters is about the
  most a 64-point label cell holds. An option cell needs roughly 7 points per
  character plus 10, so four options of 6 to 9 characters need about 240
  points of row: give the strip a row of its own (`None` label) or raise
  `kit_min_width`. A value box holds about 6 characters with its suffix
  (`"500 ms"`); limit decimals (`.max_decimals(1)`) to keep it there.

## Style

- One setting per row; at most one flexible part per row (the rail or the
  strip). Everything else is a fixed cell, so the cells of every row line up.
- Labels: a word or two, sentence case, no colons. Units go in the value box's
  suffix, not the label.
- Order rows the way the signal goes through the module, or the way you'd set
  them up: mode first, then its settings, the rarely touched ones last.
- Group with a section caption only when there are two or more groups; a short
  body needs none.
- Prefer a slider with a value box for continuous values with a useful range
  (log rail for times and other ranges spanning orders of magnitude); a value
  box alone for values with no useful range to slide across; an option strip
  for 2 to 4 modes; a checkbox for on/off.
- Hover text says what a control does and when you'd change it, in plain
  words, without restating the label.

## Pitfalls

- Don't size anything from `ui.available_width()`. Use the width the kit gives
  (`k.w`, a row's `rest`). Bodies that measure themselves made modules creep
  wider or flicker between two sizes.
- Don't put two flexible parts in a row, and don't put anything past the right
  edge: a cell wider than the row overflows it (the kit keeps the module's size
  steady, but it looks broken).
- Don't use raw egui widgets laid out side by side with `ui.horizontal`; put
  custom content inside `k.row` and size its cells explicitly.
- Don't drop an element's `register_exposable_element`, or change what rows its
  rect covers without saying so.
- A closure kept in a `let` can't return a `DragValue` borrowing its argument
  (the lifetime won't infer); pass the closure straight to the row method, or
  write a `fn`.
- Don't add colours, fonts or sizes of your own: the kit's are the theme's.

## What to hand back

Two or three designs that differ in a real way, not in wording. If they won't
all fit in one answer, give one design per answer, complete, rather than
several cut short. For each:

1. **A text mockup** of the body, to scale-ish, one line per row, with the
   option or value shown. Mark greyed controls with `( )`. For example:

   ```
   DC Filter ─────────────────────────────── (5 blocks)
   In  Window    [======|----------]  [500 ms]  Out
       Decay     [===|-------------]  [200 ms]
       Channels                  [−]  1  [+]
   ```

2. **The body function**, complete, using only the kit and the helpers above,
   keeping the signature the dispatch expects (the current one plus `w: f32`).
3. **`kit_min_width`** for it, if it needs more than 174.
4. **Notes**: what changed and why, anything the kit lacked, any element whose
   rows changed.

## Example

The DC Filter's current body, for the shape of a kit body:

```rust
pub(crate) fn show_dc_filter_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    w: f32,
) {
    let mut window = param_f32(snarl, node_id, "window_ms", 500.0);
    let mut decay = param_f32(snarl, node_id, "decay_ms", 200.0);
    kit::body(ui, w, |ui, k| {
        fn drag(v: &mut f32) -> egui::DragValue<'_> {
            egui::DragValue::new(v).speed(10.0).range(10.0..=60_000.0).max_decimals(0).suffix(" ms")
        }
        let (c1, r1) = k.slider(ui, "Window", &mut window, 10.0..=60_000.0, true, drag);
        let (c2, r2) = k.slider(ui, "Decay", &mut decay, 10.0..=60_000.0, true, drag);
        register_exposable_element(ui, node_id, "window_ms", r1);
        register_exposable_element(ui, node_id, "decay_ms", r2);
        if c1 || c2 {
            set_f32(snarl, node_id, "window_ms", window);
            set_f32(snarl, node_id, "decay_ms", decay);
        }
        channels_row(ui, k, node_id, inputs, outputs, snarl);
    });
}
```

## Prompt template

> You're proposing layouts for one module's body in FlexInput's node editor.
> Follow the brief below exactly: change only the layout and labels, keep every
> param key and element id, use only the kit. Give two or three designs that
> differ in a real way, each as a text mockup, the complete Rust body function,
> its `kit_min_width` if over 174, and notes.
>
> Module: **{name}**: {what it does}.
> Params: {key: type, range, default; one per line}.
> Pinnable elements: {ids and what each covers}.
> Current body:
> ```rust
> {the current function}
> ```
>
> Brief:
> {this file}

## Integrating a pick

Whoever integrates a chosen design (in this repo, with the app's tests):

- Check every rule above against the code: param keys, element ids, hover texts,
  no `available_width`, one flexible part per row.
- Add or update the module in `kit::uses_kit`, `kit_min_width` and the
  `show_body` dispatch, as in the guidelines' "Converting a body".
- `cargo test -p flexinput-ui`, including `kit_modules_settle_on_one_width`
  with the module added to its list, and a release build to look at it.
