# Module UI guidelines

How module bodies on the canvas are laid out, and how the canvas lays out
modules. Read this before writing a new module body or reworking an old one.

Module bodies grew one at a time, each with its own widths, label styles and
spacing, and they don't read as one app. The Audio Stream Haptics rows were the
exception: a fixed label cell, one slider rail running to a shared right edge,
and a fixed value box on that edge. The body kit turns that pattern into the
default, and sizes modules by the canvas grid's blocks.

## The canvas grid

- The canvas is divided into square **blocks** of 28 canvas points. It's one
  fixed size, not a setting: modules are designed in multiples of it.
- **The major line is two blocks** (56 points, the block's old size). Everything
  that was designed against the 56-point step — a small module's four-block-square
  minimum, a knob's 112-point start, a note's 168-by-112 opening — is still those
  same sizes in points; what halving bought is only the granularity between them.
  When a rule is written in blocks, say which: "four blocks" and "two major blocks"
  are the same 112 points, and the tests use `MAJOR` for the second so a grid change
  can't quietly halve what an assertion asks for.
- A node's frame sits with its **edges on block centers**, and its outer size is
  a **whole number of blocks**. So two nodes never share a block: there is always
  at least one block between them, and the block lines run down the middle of
  every gap. The grid is drawn a screen pixel thin and faint: crosses on the
  block corners by default (or lines, or dashed lines), every 2 blocks — every
  major line — a little brighter, fading out when zoomed far out.
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
11. A control an input pin moves **additively** - a Dropdown menu's `shift`, where
    the pin adds to what the module was set to instead of replacing it - shows the
    **effective** value: the option coming out of the module rather than the stored
    base, and picking an entry moves the base so the entry picked is the one that
    lands. Greying it out (rule 9) would be wrong: the pin and the module are both
    doing work.

## Width and height

A kit body never sizes itself from what it measured: that is what made bodies
creep wider, stick, or flicker. It is handed its width instead.

- `FlexViewer::show_body` computes `kit_w` with `kit::body_width` for every
  module in `kit::uses_kit`, and passes it to the body.
- The body always fills its node. The node is as wide as the user dragged it
  (the node's `ui_w` param) or, if nobody has dragged it, as wide as the
  narrowest frame its content fits in — never as wide as the frame it happens
  to be standing in. That last was a ratchet: the body reports back the width
  it is given, so a module born into a default frame keeps it, and the Counter
  came out 252 points wide for a strip of words and three numbers because
  252-ish is what an untouched node starts with. Asking for less than the
  content needs gets the content's frame, filled, not a body squeezed inside a
  frame that can't shrink.
- The grip itself stops at the header's width and at that narrowest frame.
- The body reports exactly that width to the node whatever its rows draw, so a
  value or option too long for its cell can't tip the node over a block and back
  every other frame.
- Kit modules get a **grip on the frame's bottom-right corner**
  (`SnarlViewer::node_resizable` / `resize_node`). It sets `ui_w` in whole blocks, or any
  size with Alt, starting from the frame's actual size each time. The fork's grip now
  drags **both axes** — `resize_node(node, frame_width, frame_height, snarl)` — and shows
  the diagonal resize cursor. Which axes a viewer stores is its own business: kit modules
  take the width only, since nothing in a stack of rows wants to be taller.
- **The grip's floor has to come from the content, never from the frame as it stands.**
  The fork's default is the header's width, and `kit::body_width` returns its own frame
  floor as a second value; the viewer hands that back through `SnarlViewer::node_narrowest`
  and stores it per node while laying the body out. A floor read off the current frame is a
  ratchet: the node grows and can never be brought back.
- **A header that puts anything against its right edge must report what it actually
  holds** (`SnarlViewer::node_header_width`). A block laid out right-to-left draws against
  the frame's right edge, so the header's used rect spans the whole node — and the node's
  width formula, which is `max(header, pins)`, then holds it at whatever width it already
  has. That is exactly how the Text note ended up resizable one way only. The app measures
  the header's content as it draws it (everything before the right-edge block, plus what
  that block consumed) and reports that.
- **Height** follows the rows, and the canvas rounds it up to whole blocks.
- A **stretchy** module (`kit::stretchy`) is one whose body is a single surface rather than
  a stack of rows — a Text note, a Readout — so it takes the grip's height as well as its
  width, and `kit::body_height` hands it the difference. Three rules keep that honest:
  - The height param is a **floor, never a bound**. A body that runs out of room grows
    itself another block — a text field with more lines than fit — and nothing may pull it
    back to the number last dragged. Clamping the grip to the frame's current height would
    do exactly that, which is why the grip's only height bound is one block. For a text
    field the overrun is measured (lay the writing out at the field's width, reserve that
    height) because egui's multiline edit fills a floor and then clips rather than growing.
  - The width floor is what the body needs, in blocks — a note's is a field that can hold
    a word; a readout's is its current reading at the smallest size worth showing, so a
    long number asks for a wider module instead of becoming a squint.
  - A surface that *displays* something takes its size from the frame and fits its content
    to it, never the other way: the type of a readout shrinks and grows, a sample-and-hold
    pair of readings shrinks. Numbers on a wire change every frame; a module that resized
    with them would twitch.

## The header: marks and the pin stepper

A small module is its header and its pins. Nothing is built under them unless a
control needs a row of its own, so the header carries the two things that identify the
module and change it.

- **The mark.** `kit::MARKS` pairs a module with the symbol it's known by — `+ − × ÷ ↕`
  for the arithmetic, `> <` for the comparisons, which read `≥ ≤` while `or_equal` is
  on — and the header draws the mark in place of the name it shipped with, the name
  moving to the hover text. A module the user renamed keeps the name they gave it: that
  is what `kit::node_mark` checks, and a test keeps the table's names from drifting
  away from the registry. AND, OR, NOT, XOR, Equal and Not Equal keep their words:
  they're already short, and a one-glyph header would make the module narrower than the
  block it sits in.
- **The `− +`.** The variadic operations — Add, Subtract, Multiply, Divide, Min/Max —
  draw no body at all. Their stepper for adding and dropping inputs sits at the **right
  edge of the header** and the node is its header plus its pin rows:
  `kit::variadic_inputs` says which modules work that way and how many inputs they may
  have (2..=26, where `pin_letter` runs out of letters), `kit::header_stepper` draws
  the control, and `kit::stepper_w` is the room the header reserves for it. A mark is
  short enough that the name never decides the width, so all five come out **four blocks
  square** — the same 112 points they were at the old grid — which is what the rig
  asserts, along with inputs filling those four blocks up to five and the sixth taking a
  fifth block (140: at the old grid there was nothing between 112 and 168 to land on),
  then handing it back.
- **A count of pins a module *has* goes in the header, not in a body row.** The per-channel
  filters — Delay, Average, DC Filter — take a matched In/Out pair per channel, and the `− n +`
  for them is drawn in the header by the same `kit::header_stepper` (`kit::channel_range` says
  which modules and what range). It used to be a "Channels" row across the body, which made the
  body wide enough for a label, a count and two buttons so that the module's *settings* rows
  could be wider still: the count of channels was the reason a Delay with one number in it was
  252 points across.
- **A mark body has to say its height.** Bodies that paint a symbol and a couple of pills
  — the operations, gates, Switch, Sample and Hold, Inverse, Has Changed, Quantize, Vec to
  Deflection (`kit::mark_body`) — reserve nothing of their own, so their height came from
  the grid rather than their content: at a 56-point step a module of that kind could not be
  shorter than 112 whatever it held. At 28 the same content settles at 84, so `show_body`
  tops the frame up to four blocks after the body has been drawn, and that is the whole
  minimum — no other floor states it. The top-up is deliberately only for these bodies: a
  body that lays its own rows out against the room it's given reflows when it's given more,
  and the reflow feeds its width back into the frame. Measured: given the same top-up, a
  Constant moved its box and pill into one row and went 112→140 wide, and a fresh Delay
  unwrapped its rows and walked from 112 to 420 points wide across a twenty-frame idle. A
  mark has nothing to reflow, so the top-up lands and stops.
- The gates carry no stepper either: the engine reads a fixed pair of inputs from them,
  so there is no count for one to set.
- Greater / Less Than are off the kit: their entire body is the one `or equal` setting,
   drawn as a Switch-style button (fill, 1-point stroke, 4-point corners, its text in
   the middle) centred on the module, with the glyph under it. Nothing in them stretches,
   so they take no corner grip. Their button is 52 wide rather than the Switch's 64 for
   the reason under **the body is a column** below: in this layout the body is a third
   thing squeezed between the two pin columns and its width *adds* to theirs, so a
   112-point module leaves the middle column about 56 points.
- **Reserve, then draw in a child.** A header control reserves its box in the header's
  own row — that's what keeps the mark and the buttons from overlapping — and draws in
  a *child* of that row, against its right edge. Reserving keeps the header reporting a
  width worked out from its own contents; drawing in the child keeps the *placement* out
  of that measurement, so a node is never locked at a width it once happened to have.
- **The name is decoration, not structure.** What the header reports as its width
  (`header_content`, read back by the fork through `SnarlViewer::node_header_width`, which
  feeds `max(header, pins)`) is its **controls only**. A name folded into that number sets
  the module's narrowest width — and then the frame is never small enough for the name to
  hide, so the grip stops short and the title is what gets blamed. The name is drawn when it
  fits and dropped whole when it doesn't, never cropped.
- **A name's room is the frame minus what the row already took**: the fold chevron and the
  fork's drag space, read as `ui.min_rect().width()` before the label is added, then the
  right-edge control (the Uni/Bi switch) if one is coming, then the sockets' room when the
  node is **folded** — a collapsed node has no body to hang them on and draws them at the
  ends of the header row itself. Measuring against `ui.available_width()` in that row returns
  almost nothing and hides every title; measuring against nothing at all paints names under
  the controls.

## The body glyph

The operations and gates that have no controls of their own (`module_icons::ICONS`)
draw one mark in the room their pins leave over — a glyph, not a widget.

- The assets live in `app/assets/modules/`, named after the module's display name
   (`AND.svg`, `SUBTRACT.svg`, `GREATER_OR_EQUAL.svg`). They share one idiom: a 59×59
   box, a 4-point stroke with round caps and joins, no fill, and the mark filling the
   band the gate bodies fill (x 16.5..41.5, y 18.5..40.5). Gates and converters carry
   lead stubs, because for those the mark *is* a signal path — two leads merging is
   Axis to Vec, one splitting is Vec to Axis. An arithmetic operator doesn't, because a
   stub running into the bar of a plus is a mess of lines, and a Clamp or a Map Range
   draws the transfer it performs instead. `glyph_tests` holds every asset to those
   rules, so a new one can't quietly differ.
- The glyph is **painted, never allocated** (`module_icons::paint_glyph`): it fits the
   band from under the module's last control down to the frame, at most one block
   (`MAX_SIDE` 56), and isn't drawn at all under 16. That is why adding inputs makes the
   glyph smaller rather than pushing the module out to a third block — the 112 points
   rig measures for these modules stay 112 with a glyph on them.
- In the sandwich layout the body column runs the **height** of the pin rows, beside
   them rather than under them, so a module with five inputs still has a full column of
   mark in it: Map Range and Clamp draw a 56-point glyph like Add does. What shortens
   the column is a control in the body, like the comparisons' toggle.
- The two modules whose pins fill only **one** row — Abs and NOT — are a block shorter
   than that, with nowhere to put a mark. They reserve a constant band (`SQUARE_BAND` 68)
   in the body, which makes them square like their siblings and gives the mark a full
   block. The reservation is keyed on the module, never on its measured height: a
   reservation that read the node's size back would fight it from frame to frame.
- **Where the mark may start is the caller's business** (`band_top`). Only the caller
   knows whether what it reserved is space *for* the mark — Abs, NOT — or space taken away
   from it, like the comparisons' toggle. Deriving it from the cursor after the body has
   drawn gets the first case wrong: the band reserved for the mark becomes the thing that
   leaves no room in it, and both modules came out square and icon-less. Derive it as
   `cursor().top()` before reserving a band for the mark, and as
   `cursor().top().max(min_rect().bottom())` after a control.
- A painted mark leaves a trace: `paint_glyph` stores the side it drew at under
   `("module_glyph_side", node)` in ctx data. Nothing reads it at runtime; the rig does,
   because a module can measure 112 square and still show an empty body, and a
   size test alone won't notice.
- **Centred on the module, not on the body.** A node in this layout puts its inputs in a
   column on the left and its outputs in a column on the right, and the body is a third
   thing squeezed between them (`snarl`'s sandwich layout: `new_pins_size.x = inputs +
   body + outputs + spacing`). That column is not the middle of the module — `a` and `b`
   are one letter, `out` is three — so anything centred in the body sits visibly off the
   module's centre. Both the glyph and the `or equal` button take their centre from
   `module_icons::module_inner`, the frame the canvas last drew, and the column's width
   budget is why the button is 52 wide, not the Switch's 64.
- The same rule keeps the *size* honest: such a control reserves its constant box in the
   body's own flow and is *drawn* in a child at the centred position. Placing the
   allocation at an absolute x instead measures the body from its left edge to the
   module's centre, which reads the module's previous width back into its own contents —
   the same ratchet the corner grip had, arriving from a different direction.
- It's drawn in the weak text colour: identity, not content. The header and the pin
  labels stay the parts you read.
- Because such a body allocates nothing, snarl doesn't charge it the gap between rows
  either — a `FLEXINPUT PATCH` in `draw_body`. Without that, giving a module a glyph
  costs it a row of height for nothing, and a module already full of pins grows a block.
- Comparisons choose their glyph from `or_equal`, the same way their header mark does:
  `>` / `≥` and `<` / `≤`.

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
| `stepper_w(ui)` | 2 × `interact_size.y` + GAP | The header's `− +`: two square buttons and the gap between |
| `MARKS` | (id, shipped name, symbol) | Modules whose header shows a mark instead of that name |
| `MAX_SIDE` / `MIN_SIDE` / `PAD` | 56 / 16 / 6 | In `module_icons`: how big a glyph may get (56 points, two blocks of the current grid), when it stops being drawn, the air round it |
| `TOGGLE` | 52 × 20 | A boolean setting's Switch-style button; 52 because the body column of a 112-point module is ~56 wide |
| `PILL_H` / `PILL_FONT` | 20 / 12 | In `simple_bodies`: the setting pill's height and text, so a setting row reads the same wherever it sits |
| `MIN_FIELD_W` | 96 | In `show_body`: the narrowest a Text note's field gets, whatever the grip asks for |
| `WIDTH_PARAM` / `HEIGHT_PARAM` | `"ui_w"` / `"ui_h"` | Node params: the frame size the user dragged the node to |

## Settings: the pill and the button

A module's settings are its body. Two controls cover them, and both reserve a constant
box in the body's flow and draw in a child — placing the allocation by absolute position,
or wrapping the draw in a scope, folds the drawn rect back into the measured width and
walks the node out a block.

- **A setting with two values is a button** — the Switch module's shape, 52 × 20, one
  click to switch, its label saying which value is current. `or equal` on the comparisons
  and `sample` / `hold` on Sample and Hold are both this, and they're drawn by the same
  `kit_bodies::setting_button`, which also returns where the glyph's band starts under it.
  A pill was tried for the mode first: at `mode` + `sample` it measured 96 points across a
  body column that fits about 56, and the module went from 168 points to 224.
- **A setting with a short list is a pill**: one filled shape, `label` cell on the left in
  the selection colour, the current value on the right, and the value cell answering the
  pointer across its whole width — the place picks the setting, so the list is scrubbed
  rather than clicked through one at a time. `simple_bodies::setting_pill` and
  `pill_detent`; Constant's `res` is the one that uses it now.
- **The pill has no outline; the button does.** An outlined thing inside a module reads as
  pressable, and a pill isn't a thing to press — it's a setting that answers a drag, so it
  fills and nothing more, with the two fills meeting as the divider. Feedback comes from
  the fill changing under the pointer. A button earns its stroke because pressing it is
  the interaction.
- **A live display reserves a constant box.** Readout's number must not measure its own
  content: a value ticking past 1000 would add a digit, widen the module and take it back
  the next frame. The box is sized for `READOUT_FITS` and the *text* adapts — starting at
  16pt and stepping down until it fits — with numbers too big for four decimals written in
  scientific notation so the width stays roughly the one the box was sized for.

## The kit

The labelled-row kit is retired. No body lays out a label cell, a control and a value box any
more — `kit::body`, `k.row`, `k.full_row`, `k.value`, `k.pair`, `k.choice`, `k.section`,
`k.note`, `k.slider` and the in-body `k.stepper` are all gone, and the words they put beside the
controls were most of the width of the modules that used them. What survives is `kit::strip` (a
row of equal option cells — the Counter's mode and output wear it), `kit::header_stepper` (the
`− n +` that adds a matched In/Out pair to the per-channel filters), the width and height a
grip writes, and the floor arithmetic.

The pattern a body of settings follows now is **a box reserved at a constant size, with the
controls placed inside it**: the Counter's stack of cells two across, or a group of pills each
with its word above it (`kit_bodies::pill_group` with `caption_at` — Delay, Average and DC
Filter read Window, Spikes and Decay over their boxes rather than `win`, `spk` and `dec` inside
them, which asked the reader to expand an abbreviation the module had room not to need). Two
rules come with it:

- If the words won't fit across, they go two per row rather than the module going wider
  ("Loop Limit Bounce Unlimited" in one strip is 184 points of words; two rows of two costs a
  block of height the module's six pin rows already make free).
- A caption is painted, never allocated. The box was reserved at a constant, and a longer word
  must not be able to widen the module that reserved it.

## Converting a body

1. Write the new body in `crates/ui/src/canvas/viewer/kit_bodies.rs`: **reserve a box at a
   constant** (`ui.allocate_exact_size`) and place the controls inside it against
   `module_icons::module_inner`, which is the module's middle, not the body column's.
2. If it's a body that should fill a frame the user widened, take the width it is handed
   (`kit_w`) as something to stretch into, never as the demand —
   `((w - gap) * 0.5).max(FLOOR)` — and write that floor into `kit_min_width`, which is what
   an untouched module starts at.
3. Add the module to `kit::uses_kit` only if it's built from `kit::strip` cells; other bodies
   keep their own arm of the `show_body` dispatch in `viewer.rs`.
4. If the body is a list the user can reorder, the move carries the pin, its wires and any
   id list kept parallel to the pins (`pins::move_dynamic_pin`, `move_automap_port`); where
   the engine reads the list by index, keep the selection on the item rather than the slot.
5. Keep **every** `register_exposable_element(ui, node_id, element_id, rect)`
   call with the **same element ids**: pinned widgets, and their own renderers in
   `pinned.rs`, look elements up by id.
6. Delete the old body.

Bodies that fill a frame the grip widened: Counter (its cells stretch from a 56-point floor)
and the port lists — Selector, Split, the Auto-Map splitter and collector, the Dropdown menu.
Rebuilt in the same house style but with painted controls and **no width to fill**, because
their bodies allocate nothing a wider frame could stretch into: Delay, Average, DC Filter,
Inverse, Quantize, Logic Delay, Has Changed, Vec to Deflection, Switch, Sample and Hold,
Greater / Less Than.

**Constant has no grip and no width param.** Its whole body is one number, and the box is
as wide as the digits it holds — `30` tall, monospace 12, 8 of air each side — with a
floor set by `9999.99`: that number sits inside a module 112 points wide, and a longer one
(`10000.00`) takes the frame a block wider, handing the block back when the value shrinks.
The width is measured from the value's own text, never from the module's current width, so
it grows and shrinks on demand and can't stick. The box is drawn centred on the module —
in both directions — in a child, with a constant box reserved in the body's flow: wrapping
it in a style scope instead folded the centred child's rect back into the measured width
and walked the module out to 168 points.

**Its number stays 64-bit in the box.** The wire is `Signal::Float(f32)` — about seven
exact digits — but reading the param through `f32` in the UI rounded eight-digit values on
the way in and then made the box stop answering the mouse: at that size the gap between
neighbouring f32s is wider than a drag step, so every drag computed a change and stored
nothing. The value is therefore kept as stored, and the box clamps at `±1e12` — well
inside what an f32 wire can carry, and stated in the hover text rather than left to be
discovered. The clamp also bounds how long the digits, and so the module, can get.

**The resolution sits under the number in one divided pill.** Left cell `res`, right cell
the digits it keeps after the dot: `int`, `1`, `2`, `3`, `4`. Writing the step itself
(`0.0001`) would make the pill grow with every setting; the count doesn't. The pill is as
wide as the box and the pair reads as one object, not two controls that happen to line up.

- The value cell is **dragged, not clicked**. It's allocated with `click_and_drag` and the
  pointer's place across it picks one of the five settings, so the resolution scrubs
  left-to-right like a rail. A button was the first attempt and it read as a label:
  clicking once to move one step, on a 32-point cell, felt like nothing was happening.
- And it has **no outline**. An outlined thing inside a module reads as a button waiting to
  be pressed, which is the opposite of a setting that answers a drag; the two fills meeting
  is the only divider it needs. Feedback comes from the fill changing while it's hovered
  or dragged, not from a bright edge.
- **A pill with no label cell is one fully rounded shape.** The two cells are painted as
  two rects with their own corners — left pair rounded on the label, right pair on the value
  — so when the label is `""` (Inverse, Logic Delay, Has Changed, Vec to Deflection) the
  value cell's square left corners showed, which reads as the pill being cut off by the
  frame. With no label cell the value cell gets all four corners and the label rect isn't
  painted at all.
- The resolution is also the **drag granularity** of the number: a box reading tenths
  moves in tenths. Above four digits the step additionally grows by powers of ten, or a
  drag on a large value would need a thousand pixels to move its ones place — but it stays
  a whole number of resolution steps, so the box never moves in units it can't display.
- Resolution isn't decoration: the value is snapped to it, in the box and in the engine,
  so `3.7` at whole numbers drives `4`. The engine applies the same rule because a value
  can arrive without going through the box — an older scene, a preset, a pasted node.
- Changing the resolution doesn't rewrite the number. The param keeps its extra digits, so
  going `int` and back to four gives `3.7` again rather than the `4` that was on screen.
- The box's width is measured against the text as displayed — trailing zeros trimmed — so a
  whole number doesn't pay for a dot and two zeros it never shows. `9999.99` is the floor
  the box is built around, and it still fits 112 points.
- **Centred horizontally on the module; vertically, on whatever the controls are.** The
  module's centre includes its header, and the body column's centre is where its *rows*
  start, so the two answer different questions. A stack that fills the column — the
  Counter's cells, a run of port strips — centres in the column (`kit::group_top`): centred
  on the module it hangs high with all the empty space below it. A short group of pills in
  a tall column does the opposite — the column is as tall as the pin rows next to it, so its
  middle is low — and centres on the module's middle line instead
  (`module_icons::module_inner`). Neither centres horizontally on the column: its middle is
  offset by the pin labels, and `in` is one letter while `out` is three.
- The Knob will want the same control with a different meaning — steps that divide its 0-1
  scale (whole, halves, thirds, quarters) rather than decimal places — so it doesn't simply
  inherit this one.

**A Text note's body is the field and nothing else.** Its settings — font size, colour —
sit at the right edge of the header, the way the other header controls do, so the body is
purely what's written. Size comes from the frame's own corner grip in blocks, not from a
grip drawn inside the body at the text box's corner: two grips on one module, one of them
floating somewhere near the bottom of the writing, is the kind of thing that makes a note
look broken. Concretely:

- The body is handed `body_w` (what the text wraps to) and `body_h` (a floor) and reserves
  that size in the flow, so the note settles on the size the grip asked for rather than
  walking out from its own last width.
- The field is drawn across **what's inside the frame**, not across the body column: the
  column is padded by the pin columns' spacing, which on a module with no pins reads as a
  margin that comes and goes with the text. And it has no frame of its own — a second
  border a few points inside the module's looks like a box that shrank or grew depending
  on how full it is.
- More lines than fit → the writing is measured at the field's width, that height is
  reserved, and the note takes whole blocks. One line again → it comes back to the floor,
  not to the tallest it ever was.
- An unnudged note opens 168 points wide, sized from a 160-point field. It used to be
  four: the old body reserved 160 *inside* the frame's own margin and the canvas rounded
  that up, so the frame was wider than anything in it asked for.
- A patch saved before the grid took over keeps the width it had: the legacy `box_width`
  param is read as the default when `ui_w` was never set.

**A Readout is a stretchy display, and it starts small.** 112 points by 112, because three
was wider than any ordinary number needed: the type fills the space instead (`READOUT_MAX_FONT`
down to `READOUT_MIN_FONT`), the reading is written in scientific notation once four decimals
stop meaning anything, and the width floor is the current reading at the smallest size worth
showing — so a long number asks for a wider module rather than a squint, and a value on the
wire never resizes the module on its own.

**Sample and Hold shows what comes in and what goes out.** Two readings painted into the
space at the **bottom** of the frame — the input in the header's strong colour, the output
dim while the trigger or gate is idle and amber while it's doing something, so *holding* and
*following* look different. Nothing there is allocated: the pair shrinks to fit, so a
nine-figure value doesn't walk the module out a block, and it is 112 square. The mode
is the house two-state button in the body at `TOGGLE_SMALL` — this module's pins take 47 of
the 98 points a 112-point frame has inside its margins, and the 52-point size costs it a
third block — and the name gives way to an `S/H` mark in the header for the same arithmetic.
The readings are anchored to the bottom rather than divided through whatever is left: the
pin labels own the rows above, and centring the pair in the body column put the input among
them, where it read as a stray dash by the header. The stepped curve that used to sit in the
body went to Quantize, the module it actually describes.

**Quantize is a mark and two pills, in 112 square.** It was 224 points wide: a Factor drag
box and a Rounding choice spelling out four words, each on its own kit row. The staircase
mark goes across the pin rows like every other operation's; the two settings sit in the
slack under them — a `÷` pill (the number is subdivisions per unit, and the word "step"
barely fitted its own cell) dragged through doublings 1 → 1k, and the rounding word, which
opens a short list when clicked. Nothing in the body is allocated, so the module's width is
its pins' business, and it is **not** in `kit::uses_kit`: a body that allocates nothing has
no room to ask for, so a corner grip there would be a handle that does nothing. A factor
coming down a wire shows on the pill and the pill stops pretending to control it; exact
factors that aren't doublings are still typed in the pinned field. The mark is 38pt here
rather than the 56 a bare operation gets, because it shares the column with the pills — say
so in the comment when a module trades mark size for controls.

## Pin labels: short on the canvas, full on hover

The pin labels are usually the widest thing in a small module — "increased", "Deflection",
"direct" each cost points that the pins themselves don't. So:

- **The name the patch stores is never changed.** Abbreviating `logic.has_changed`'s
  `increased` to `inc` in the registry would move an id that patches, sinks and sub-patch
  layouts already refer to. The short form is a drawing decision.
- `kit::pin_short` picks the form: a numbered generic pin (`in_0`, `out_11`) comes down to
  its number, because the column it sits in already says in or out; the kit's common words
  have fixed short forms in one table; anything else longer than four letters loses its
  tail. The full name goes in the hover text of the label.
- Only modules small enough for a block to matter take the short forms
  (`kit::pin_abbreviates`). A Remapper or a JSM has room and keeps its labels spelled out.
- **Never on a port you choose by its name.** An Auto-Map splitter's or collector's pins, and
  a device module's, are labelled with what the user learned or plugged in — "Left Stick",
  "Channel Pressure · ch 2" — and reading which port feeds what is the whole reason the module
  is on the canvas. Those names are shown in full and the module pays the width; shortening one
  to fit a block trades the module's purpose for its size. A module's own fixed words
  (`increase`, `deflection`) carry no information the user is scanning for, so those do shorten.
- A module whose body says what comes out may hide its output labels entirely
  (`kit::pin_hides_outputs` — Knob today). The socket stays; the label doesn't.
- **A name that is shown is painted, never laid out.** A header label added to the row contributes
  its width to the row, and the row is one of the things the node's width is measured against:
  "DC Filter" was needing a 168-point module where the same module called "Average" managed with
  140. So the name is drawn where the fold chevron leaves off, `title_fits` (which knows the room
  the control at the right edge needs) decides whether it's drawn at all, and a module that wants
  a size it no longer gets from its word asks for it by name - an empty Readout starts at two
  blocks because it says so, not because "Readout" is long.
- **A list's `+` lives in the header**, at the right edge, the same square as the `− n +` and the
  other header controls: at the end of the list it reads as one more entry, and it puts a button
  where the rows' remove buttons line up.
- The header does the same job with names: `kit::MARKS` gives a module a symbol in place of
  its name (`Δ` for Has Changed, `∠` for Vec to Deflection, `S/H` for Sample and Hold), and
  the name moves to the hover text.
- **A title that doesn't fit the frame is left out, not cropped** (`kit::title_fits`). The
  header then reports only what it actually holds, so the module's narrowest frame comes
  from its pins and controls rather than from a word that was once drawn there; widen the
  frame and the name comes back. This only works because the small modules ask for their
  size out of their own content — see the next section. Measured: a Logic Delay renamed to a
  50-character name is still 112 square.
  - Inverse is deliberately *not* in `MARKS`: `±` reads as arithmetic, not as negate, and
    its name fits the 112 points its pins need.

## What a body is allowed to reserve

A two-block frame has 98 points inside its margins, and the pin columns take what their
labels and sockets take — measured, Switch's pins are 32 points, Sample and Hold's 47, and
the body column sits *between* them, so its width adds to the module's. Two failure modes,
both measured:

- A body that allocates **nothing** makes the module as narrow as its pins — Sample and
  Hold's 112 — but a control painted across that frame then hangs outside it. Inverse came
  out 56 wide (a block at the old grid, two now) carrying 82-point pills.
- A body that allocates its controls **in the column** pays their width: Inverse at 66 came out
  168 points wide — three of the old blocks.

The working shape: reserve a **constant box** sized to what 112 points leave over
(Inverse and Logic Delay reserve 48), and paint the controls into it. Split for the
modules whose pins are already wide (Quantize, Vec to Deflection, Switch, Has Changed):
they reserve a token box and paint the control across the frame, which is inside the
module and outside the column. Two details that only show up when something else moves:

- **The token box has to be wide enough to hold 112 points on its own**
  (`module_icons::MIN_BODY_W`, 40). Quantize, Axis to Vec and the other glyph modules were
  reaching 112 wide because the *word in their header* was wide; the moment a title that
  doesn't fit is dropped, they fall to 56 points — and a 56-point frame is too narrow to
  paint their pills or their mark in, so they stay empty and narrow with nothing left to ask
  for the room back. `a_small_module_holds_its_two_blocks_without_the_grid` measures it with
  the grid's sizing off, where no rounding covers it over.
- **Which end of the frame a painted control sits on is a house choice, not free.** A module
  with a mark paints the mark over the pin rows and its pill row along the bottom of the
  frame (Quantize, Vec to Deflection): centring the pill instead leaves the mark a band too
  short to read (it came out 6 points of glyph, which the painter rightly refuses). A module
  whose control *is* its body centres the control in the body column (Switch, Inverse, Logic
  Delay, Has Changed, Constant).

## Port strips: Selector, Split, AutoMap Combiner

Modules whose ports are a list the user counts out are drawn as a list, in the body:

- One **strip per port**: a drag handle, a `×` that appears only once there are more than
  two to choose between, and the port's name. The name field edits the **pin's own name** —
  what the patch stores — rather than a display string kept beside it, so the strip, the
  socket's hover text and the pinned field all say the same word.
- **`+` lives in the header** (`kit::header_plus`), one square at the right edge, the same
  size as the `−`/`+` pair. Removing is the strip's own `×`, which knows which port it is on.
- **A row is one block.** `STRIP_H` is 25, and the pin column adds its 3-point row
  spacing, so a row advances exactly 28 — one block. The frame's overhead above the
  first row isn't a whole number of rows (34.5 points), so a band of about 26 points of
  body is left under the last strip — measured, the same 26 from three ports to seven.
  That's as even as a constant row can be made: at the old 56-point block the same list
  alternated between 26 and 55 depending on whether the row count was odd, and with a
  27-point row it slid two points further off the block with every pair of ports.
  Two routes to making the leftover disappear were measured and rejected: trimming the
  header's margins until the overhead landed on a row boundary is a knife-edge (half a
  point off and one parity jumps a whole row), and deriving the row height from the
  blocks the list needs can't be done from the app side — the layout takes row heights
  from the other side's *stored* heights, a frame behind, and the node's overhead can't
  be read from its stored state either (the inner height minus the pin height came out
  45, 62 and 92 points in successive frames of one Selector, because the body can be
  taller than the pins). Three points off at the snap boundary is a whole block, so it
  went from a constant 26 to 65/11 swinging frame to frame. Until the layout reports the
  overhead as a number, the constant one-block row is the honest floor.
- **The strip and the socket are one row.** The body places its strips at
  `kit::strip_advance` = `STRIP_H + style item_spacing.y`, and `show_input` / `show_output`
  allocate those pins' rows at `kit::strip_pitch` = `STRIP_H` with no label at all. The two
  numbers differ on purpose: in a `Coil` layout the pin column adds its own row spacing on
  top of each row, and takes the row heights from the *other* side's rows, so the allocation
  is never the advance. Ask both sides for the same number and the sockets drift off the
  strips naming them by the spacing per row — and the pins' column, being taller than the
  strips' reservation, sets the module's height, so the leftover collects under the last strip
  and widens with every port added. `strip_pin` is keyed to the pin's **position**, not to a
  name like `in_0`: the strip's field renames the pin, and a rule keyed to the old name would
  leave a port called `kick` saying its name twice.
- **What is asked for is content; what is painted is the column.** The body reserves
  `need_w` — the handle, the `×` when there are more than two ports, the gaps, and the longest
  name clamped to `STRIP_NAME_MIN..STRIP_NAME_MAX` (28..140), plus the toggle's room when it
  shares a band — and that reservation is the floor the grip can be dragged down to: short
  names keep a Selector or Split at **three major blocks wide** (168 points) whatever the port count. The strips
  are then painted as wide as the column the frame leaves, ending flush with the side the
  sockets are on. Tie the *ask* to the column instead and the module can never be dragged
  narrower than it already is.
- **Every strip is the same width and flush with the others on the side its sockets are
  on** — left for a Selector's inputs, right for a Split's outputs. The toggle shares the
  first strip's band at the opposite edge, and the list reserves room for both, so the strip
  that sits beside it is no narrower than the rest. (A strip narrowed to make room was the
  first attempt: the column turned into a ladder and the squeezed strip's name ran past its
  edge.) A name longer than the cell is cut with an ellipsis, the whole name in hover text.
- **A move is previewed, then applied.** While the handle is held the dragged strip rides
  under the finger and the strips it will pass ease aside, one row each; the pins don't
  change until the drag ends, and when they do the painted offsets are re-keyed to the new
  order (`remap_offsets`) so the strips carry on from exactly where the preview left them.
  The frames have to keep coming while that settles — `request_repaint` while any offset is
  still more than half a point from its target.
  A Combiner's rows can't do that: they are live widgets with a socket beside each, and
  moving one mid-frame would move the socket and the pins' column with it. Its preview is
  the minimum that still answers the question a drag asks — which of three near-identical
  rows will this land on: the row under the finger is dimmed as lifted (egui paints the
  moving copy at the cursor), and a line is drawn where the gap will open. Which row is
  being dragged comes from the drop zones' hover payload; between zones egui says nothing,
  so the last one seen is kept while the button is held (`combiner_drag_key`) or the mark
  flickers in the three-point gaps.
- **Anything stored by port number follows the port, not the slot.** The Combiner keeps a
  port's default merge policy (`combiner_port_default`, keyed by port index) and a pin's
  pinned port (`combiner_pin_port`, whose *value* is a port index). The engine reads ports by
  position — the lowest offering a pin wins — so a reorder that permutes only the pins hands
  a device's policy to whichever device lands in its slot and repoints a pinned pin at
  whatever took its place. `combiner_move_input` permutes both; a new index-keyed setting has
  to be added there too, and the test for it drags a row and checks the settings came with it.
- **A module that draws its rows in the flow — the Combiner — has to be told the height.**
  Its rows contain dropdowns, so their height is the style's, not a constant: `kit::strip_pitch`
  returns `interact_size.y + 2 * COMBINER_PAD_V` and the body uses the same expression for the
  frame it draws the row in. Neither side can measure the other — the pins are laid out before
  the body — so the arithmetic is shared instead.
- **A control takes the width it was given, not the width it wants.** Three widgets will
  otherwise lay themselves out wider than the row: a `ComboBox` uses `Spacing::combo_width`
  - 150 points - as its minimum whatever is selected in it, so give it `.width(cell)` and
  let the popup size itself to its entries; a `kit::strip` adds its own spacing between
  cells on top of the style's, so the child drawing it sets `item_spacing.x = 0.0`; and a
  pill's label cell is measured in the monospace it is painted with
  (`simple_bodies::pill_label_width`), not in the proportional text style, or the colour
  block ends up narrower than the word on it.
- **A stretchy body reserves its box and draws inside the rect it reserved.** The note takes
  the leftover after its reservation; the Envelope uses the allocated rect itself, because
  it has to fill the frame that reservation was worked out from. Mixing the two leaves a
  body painting a strip of itself.
- **A move takes the wire with it** (`pins::move_dynamic_pin`): the engine reads these pins
  by position, so reordering without reconnecting silently rewires the patch.
- **The handle is a handle: grab cursor on hover, and the strip it belongs to picks up an
  edge** while it is hovered or dragged — the same reading the Combiner's rows give, so a
  list of strips and a list of rows behave alike under the pointer.
- **The port the selection is pointing at glows.** The engine mirrors a Selector's and
  Split's `select` value into `extra.last_signals` (added to the display list in **both**
  places it is kept), and the strip paints an outline and a faint fill at
  `selection.bg_fill` scaled by how much of the selection is on it. Interpolate mode gives
  the two ports either side their share, so the light fades from one to the next rather than
  jumping. A body that wants live state needs the engine's say-so: nothing else reaches it.
- **The Combiner's rows are all as wide as the widest content in the list** — the longest
  port name plus its device label, measured in the row's own font — and the resolution
  dropdown is pinned to the strip's right edge rather than trailing the name. Ragged rows
  read as several controls; a dropdown that wanders can't be scanned.
- The size is content-driven and reversible: the body reserves `rows × pitch` and as wide as
  the longest name, so adding a port takes a row and a longer name takes width, both handed
  back when they stop being needed. The module has no corner grip.
- The one setting sits on the row its own pin is on. Where the two columns have different
  numbers of rows — a Split has two inputs and as many outputs as the user added, and the
  bands of the two columns are shared by index — the strips own the bands and the setting
  shares the first one at its left, rather than pushing every socket out of line.
- **An inline text field inside a node body works**, unlike a dropdown: `ui.put(rect,
  TextEdit)` in the body's own space, `request_focus` on the frame after the one that opened
  it, commit on Enter or `lost_focus()`, Escape to drop. egui 0.33 has no select-all-on-focus
  for `TextEdit`, so the field opens **empty with the current value as its `hint_text`**:
  prefilling it and typing `7` commits `47`.

## This batch's shapes

- **Switch** 2×2 as it starts, wider when its caption asks: the button is measured from the
  stored caption and icon — at the size they're painted — and reserved in the flow, so a
  label set from the context menu takes another block and hands it back. It's centred on the
  module either way. Pins `dir`/`lat`/`out`.
- **Knob** 2×2 at rest, and whatever the corner grip asks for after that — the face fills
  the body it's given, so a wide knob is a horizontal fader and a tall one a vertical; no
  output label, and the Uni/Bi range switch in the header. **The reading is part of the
  body, at every size**: in the middle of a dial — the dial's indicator stops short of the
  middle so the two don't cross — under the track for a fader with a line to spare, and at
  the end of the track for a fader 56 points tall — and there the **track gives way to the
number, not the other way round**: the track shortens to whatever is left, down to 24 points,
which is still something to aim at, because a knob that dropped its reading shows nothing.
(An 84-point-wide knob is a step the finer grid makes reachable and leaves 27 points of track
after its reading; the rule used to want 36 and drew no number at that size.)
A dial never gives up a line of frame for
  the text: the thing you drag is what the body is for. Collapsed it draws **no face** and
  writes the number in the band under the header, which a folded frame still has; the
  `!open` early return exists because snarl runs a collapsed node's body and shifts it up
  under the header's clip, which painted the dial's rim across the header row. Painting
  outside the control's rect needs a painter clipped to the column (or the frame), not
  `ui.painter_at(face)`: the reading was silently cut away that way. A fader is thin by
  design, so the grip can take a knob to **56 points wide** — two blocks at
   this grid, the knob's own floor rather than the grid's step, since a 28-point block of
   track would be too thin to grab; one block *tall* is not on the
  table — the header and the frame's margins take most of a block, leaving nothing for the
  control — so the body's floor is one block of height. It's in `kit::stretchy` for the same
  reason a note or a Readout is: the body is one surface that takes the frame. (It used to draw in the
  fork's `FlippedSandwich` layout, where the body's height is *added* to the pins' rather
  than compared with them — a body filling its own column then grows the node by that column
  every frame.)
- **Inverse** 2×2 — its name in the header, a small two-state button (sign / range) and, in
  range mode, a max pill, the pair centred. Was 5×2.
- **Logic Delay** 2×2 — three rows in a 48-point box, centred: which edge waits, how long
  (dragged or typed), and the unit. Was 5×2.
- **Has Changed** 2×2 — a hysteresis pill centred on the module, taking no column width.
  Was 6×2.
- **Vec to Deflection** 2×2 — `∠` in the header, the bent-vector mark over its pin rows and
  one pill for the angle unit. Was 6×2, and briefly 1×2.
- **A pill is dragged or typed into**, never only dragged: `simple_bodies::value_pill` keeps
  the scrub (which is how a choice strip or a doubling ladder is set) and opens a field in
  the value cell on a double-click or Ctrl-click. A number the user can only scrub is a
  number they can't set.

## Pitfalls

- Inside a kit body, size things from the width the kit gives (`k.w`, a row's
  `rest`), never from `ui.available_width()`.
- One flexible part per row. Two rails on a row means two rows.
- Nothing past the right edge: a cell wider than a value box (the stepper's
  `− n +`) has to reserve its real width, or the whole body grows past the edge.
- A body that shows **what a wire is carrying into this node** needs the engine to mirror
  those inputs: `eval.rs` copies them into `last_inputs` for a named list of modules only,
  twice over — once for top-level nodes, once for nodes nested in a sub-patch. Add the
  module to both, or the body paints a dash and any colour that depends on a pin stays at
  its resting value forever, with nothing failing along the way. (A node's own **outputs**
  are mirrored for every module, in `last_out`, so an output reading works while the input
  one doesn't — which is how this reads: half the body is right.)
- A node body that wants a **dropdown** has to draw and hit-test one itself. egui's own
  popup machinery (`Popup::menu`, `ComboBox`) places its list against the layer the canvas
  draws nodes into, and the nodes are under the pan/zoom transform while the list is not:
  measured, a menu anchored to a pill at graph x 152 came out at x 904, and `.at_position()`
  only fixes the position, not the hit-testing — clicks inside the list went nowhere. The
  working shape is a painted list in the body's own space with `ui.interact(rect, id,
  Sense::click())` per row, open/closed state in `ctx` temp data, and `clicked_elsewhere()`
  to close it (not compared against a raw `interact_pos`, which is in screen coordinates
  while the body's rects are in graph ones — that closed the list on the frame it opened).
- Keep the hover texts when moving a body over; they are the module's help.
- Work out a body's width from the node's layout this frame (its `NodeState`),
  never from the frame rect last drawn: that one is a frame behind, and
  correcting against it overshoots every other frame.
- Pinned widgets (sub-patch bodies, overlays) follow their own scaling rules in
  `memory/pinned_widget_scaling.md`, not the kit.
- A closure kept in a `let` can't return a `DragValue` borrowing its argument
  (the lifetime won't infer); pass the closure straight to the row method, or
  use a `fn` item.
- A control measured by hand has to be *placed* by hand too. Two buttons put in a row
  layout land wherever that row's item spacing and fit rules decide, which is not the
  measurement the box around them was reserved with — on the canvas it left the header's
  `+` over the module's frame border, far from its `−`. `kit::header_stepper` gives each
  square a rect of its own: `h` on a side, `GAP` between them, the `+` ending on the
  reserved box's right edge.
