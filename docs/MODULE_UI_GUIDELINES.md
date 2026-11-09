# Module UI guidelines

How module bodies on the canvas are laid out, and how the canvas lays out
modules. Read this before writing a new module body or reworking an old one.

Module bodies grew one at a time, each with its own widths, label styles and
spacing, and they don't read as one app. The Audio Stream Haptics rows were the
exception: a fixed label cell, one slider rail running to a shared right edge,
and a fixed value box on that edge. The body kit turns that pattern into the
default, and sizes modules by the canvas grid's blocks.

## The canvas grid

- The canvas is divided into square **blocks** of 56 canvas points. It's one
  fixed size, not a setting: modules are designed in multiples of it.
- A node's frame sits with its **edges on block centers**, and its outer size is
  a **whole number of blocks**. So two nodes never share a block: there is always
  at least one block between them, and the block lines run down the middle of
  every gap. The grid is drawn a screen pixel thin and faint: crosses on the
  block corners by default (or lines, or dashed lines), every 8 blocks a little
  brighter, fading out when zoomed far out.
- **Dragging** snaps a node as it goes. **Alt** drags freely.
- A node **dropped onto others**, or **grown into them** by the user (its width
  grip, adding a channel), pushes them out of the way by whole blocks. A pushed
  node pushes whatever it lands on in turn.
- **New nodes** the user adds (Add-module menu, paste, insert between, adding a
  device) are inserted with `Snarl::insert_node_placed` / `place_node`: once drawn
  at their real size they snap to the grid where they were put (a paste lands at
  the pointer) and push whatever they landed on aside. A pasted group moves as
  one, so it keeps its shape. Loading a patch or undoing never moves nodes.
- **Wires** are drawn one of three ways (the grid menu's Wires setting):
  - **Direct:** a plain curve from pin to pin, over whatever's in between.
  - **Around** (the default): between the stubs at its pins, the shortest way
    round the nodes in its way at any angle (through the corners of the nodes,
    held off by the Clearance setting), so it's straight wherever nothing is in
    the way. Each node a wire goes round widens by that wire's width for the
    next, shortest wires first, so wires going round a node together go round it
    side by side. It leaves and reaches the pins level, each corner rounded as
    much as the nodes near it allow. With nothing in the way that's the plain
    curve. Easier to follow than the grid style; nothing is moved to make room.
  - **Along the grid:** as follows.
- Along the grid, **wires** run along the block lines, through the gaps and
  around the nodes, drawn thicker than the curves they replace (an Auto-Map
  bundle keeps its own width) and with rounded corners. Every wire sharing a
  stretch of line gets its own lane, packed edge to edge with 3 points clear
  between, ordered so they cross as little as they can, and moved off whichever
  side is nearer a node. Lanes turning together turn concentrically. A short
  jog between two runs (pins nearly in line, a wire stepping into its lane) is
  an eased S instead of two corners. A wire takes a less crowded line when one
  costs about the same, so a big fan-out spreads over neighboring lines.
- In both routed styles each wire has a dark underlay, so where two cross, the
  one on top visibly cuts across the other. Nodes off the grid are walked around
  along their own edges, and a wire goes through a node only when nothing else
  is possible. Only wires that something happened to are routed again (their
  ends moved, or their route ran where a node moved or resized), so dragging a
  node across a big patch stays cheap.
- Along the grid, when more wires share a gap than fit in it, the gap
  **widens**: after the user drops or resizes a module or connects a wire, the
  module on the far side of a crowded line moves away from it by as many blocks
  as the extra lanes need (the module the user just placed stays put), and the
  wires reroute into the room.
- The canvas's zoom toolbar has two menus. The **grid menu**: show the grid
  (its style, how often a line is brighter, and how bright it is overall, for
  low-contrast themes), snap modules, push modules out of the way. The **wires
  menu**:
  - how wires are routed (Direct, Around with its Clearance, Along the grid);
  - how much a module's wires are **highlighted**: hovering or selecting a
    module thickens its wires a little and dims the rest of the patch (the
    modules at their other ends excepted), fading in and out;
  - the **activity glow** range: the least any activity glows to the most full
    activity does (both handles at the left turns glow off);
  - what makes an **Auto-Map bus glow**: any reading away from zero; readings
    moving away from where they rest (each reading's resting value follows it
    quickly while it's near and slowly while it's far, so a pad lying still
    goes dark and a held stick keeps glowing for a good while); or only live
    input (the default: a touch's position and pressure count only while there's
    a touch). Battery never counts.
- In Advanced mode, the I/O nodes Easy mode arranges around its sub-patch sit
  on the grid a block apart.

Implementation: `vendor/egui-snarl/src/ui/grid.rs` (snapping, pushes,
placement, the route cache) and `vendor/egui-snarl/src/ui/route.rs` (the
router and lanes), both marked `FLEXINPUT PATCH`.

## Body anatomy

1. A body is a **column of rows**.
2. A row starts with a **label cell**, then its control, and ends on the body's
   **shared right edge**.
3. A row has **at most one flexible part** (a slider rail or a choice strip),
   which takes whatever width is left. Everything else is a fixed cell, so the
   cells of every row line up.
4. **Labels** are `TextStyle::Small` in the weak text color, clipped to their
   cell. Keep them to a word or two; a label that needs explaining gets hover
   text.
5. **Values** sit in fixed-width `DragValue` boxes on the right edge, with their
   unit as a suffix (`" ms"`).
6. **Times and other wide ranges** get a logarithmic rail, so the small settings
   are as easy to hit as the large ones.
7. **Options** are a strip of equal-width cells, each with hover text saying what
   it does.
8. **Sections** get a small caption followed by a hairline to the right edge, not
   a box.
9. A control that does nothing right now (its input pin is wired, or another
   setting overrides it) is **greyed** with `add_enabled_ui`, and its label says
   why when there's room: `Factor (wired)`.
10. **Explanations go in hover text**, not on the canvas.

## Width and height

A kit body never sizes itself from what it measured: that is what made bodies
creep wider, stick, or flicker. It is handed its width instead.

- `FlexViewer::show_body` computes `kit_w` with `kit::body_width` for every
  module in `kit::uses_kit`, and passes it to the body.
- The body always fills its node. The node is as wide as the user dragged it
  (the node's `ui_w` param), or as it already is, but never narrower than the
  narrowest frame the rows fit in, rounded up to whole blocks. Asking for less
  gets that frame, filled, not a body squeezed inside a frame that can't shrink.
- The grip itself stops at the header's width and at that narrowest frame.
- The body reports exactly that width to the node whatever its rows draw, so a
  value or option too long for its cell can't tip the node over a block and back
  every other frame.
- Kit modules get a **width grip** on the frame's bottom-right corner
  (`SnarlViewer::node_resizable` / `resize_node`). It sets `ui_w` in whole blocks,
  or any width with Alt, starting from the frame's actual width each time.
- **Height** follows the rows, and the canvas rounds it up to whole blocks.

## Measurements

All in `crates/ui/src/canvas/viewer/kit.rs`.

| Constant      | Value | What it is |
| ------------- | ----: | ---------- |
| `LABEL_W`     | 64    | Label cell |
| `VALUE_W`     | 50    | Value box |
| `GAP`         | 4     | Between the cells of a row |
| `ROW_GAP`     | 4     | Between rows |
| `SECTION_GAP` | 6     | Above a section caption |
| `MIN_FLEX`    | 52    | Narrowest a row's flexible part gets |
| `MIN_W`       | 174   | Narrowest body: `LABEL_W + GAP + MIN_FLEX + GAP + VALUE_W` |
| row height    | `interact_size.y` | From the style |
| `WIDTH_PARAM` | `"ui_w"` | Node param: the frame width the user dragged the node to |

## The kit

`kit::body(ui, w, |ui, k| …)` lays out a body `w` wide with the row spacing set;
`k` is a `Kit`. Every row method returns `(changed or response, row rect)`; the
rect is what `register_exposable_element` takes.

| Call | Draws |
| ---- | ----- |
| `k.row(ui, label, \|ui, rest\| …)` | Label cell, then your own content in the `rest` of the width |
| `k.full_row(ui, \|ui, w\| …)` | A row with no label cell |
| `k.slider(ui, label, &mut v, range, log, \|v\| DragValue…)` | Label, slider rail, value box |
| `k.value(ui, label, &mut v, \|v\| DragValue…)` | Label, value box on the right edge |
| `k.pair(ui, label, (caption, &mut a, enabled), (caption, &mut b, enabled), \|v\| DragValue…)` | Label, two captioned value boxes (Min / Max) |
| `k.choice(ui, Some(label) or None, &mut value, &[(value, text, hover)])` | Label, option strip |
| `kit::strip(ui, width, &mut value, options)` | The option strip alone, for inside `k.row` |
| `k.toggle(ui, label, &mut b, text)` | Label, checkbox |
| `k.stepper(ui, label, count, range, (minus_hover, plus_hover))` | Label, `− n +`; returns `Some(-1)` / `Some(1)` |
| `k.section(ui, title)` | Caption and hairline |
| `k.note(ui, text)` | Small weak text, wrapped to the body |

## Converting a body

1. Write the new body in `crates/ui/src/canvas/viewer/kit_bodies.rs`, taking an
   extra `w: f32`, built from `kit::body` and the row methods.
2. Add the module id to `kit::uses_kit`.
3. If its rows need more than `MIN_W` (a strip of four options does), add it to
   `kit_min_width`.
4. Pass `kit_w` at its arm of the `show_body` dispatch in `viewer.rs`.
5. Keep **every** `register_exposable_element(ui, node_id, element_id, rect)`
   call with the **same element ids**: pinned widgets, and their own renderers in
   `pinned.rs`, look elements up by id.
6. Delete the old body.

Converted so far: Delay, Average, DC Filter, Inverse, Quantize, Logic Delay,
Counter, Has Changed, Greater / Less Than, Vec to Deflection.

## Pitfalls

- Inside a kit body, size things from the width the kit gives (`k.w`, a row's
  `rest`), never from `ui.available_width()`.
- One flexible part per row. Two rails on a row means two rows.
- Nothing past the right edge: a cell wider than a value box (the stepper's
  `− n +`) has to reserve its real width, or the whole body grows past the edge.
- Keep the hover texts when moving a body over; they are the module's help.
- Work out a body's width from the node's layout this frame (its `NodeState`),
  never from the frame rect last drawn: that one is a frame behind, and
  correcting against it overshoots every other frame.
- Pinned widgets (sub-patch bodies, overlays) follow their own scaling rules in
  `memory/pinned_widget_scaling.md`, not the kit.
- A closure kept in a `let` can't return a `DragValue` borrowing its argument
  (the lifetime won't infer); pass the closure straight to the row method, or
  use a `fn` item.
