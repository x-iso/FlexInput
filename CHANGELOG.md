# Changelog

All notable changes to FlexInput are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.13.5] - 2026-08-23

### Fixed

- **Config Overlay: live widgets on a gyro path stayed frozen while editing.** The
  overlay's selective input-suppression passes through only the physical pins the
  tweaked control depends on, tracing the control's node upstream to its source.
  Two gaps left the gyro blocked — so the RWS Aim calibration ruler/room (and any
  Response Curve on the same path) wouldn't animate:
  - The upstream tracer followed each node's *first* input, but a **Selector**'s
    input 0 is its *select* control, not the data — so the walk chased the
    dropdown/switch instead of the routed source. It now follows the **active data
    branch** through `module.selector` (the currently-selected input) and
    `module.split` (the data input), reaching whichever physical source is live.
  - **RWS Aim** is driven by its Rotation input, but `scale` only affects the Mouse
    output — so with only the Stick output wired, the fallback (downstream) resolver
    found nothing and blocked the whole device. RWS now always passes its Rotation
    (IMU) input through so the calibration widget animates regardless of which
    output is used.

Three fixes for the same underlying trap: egui's `ctx.data()` and its layer→transform
map are shared by *every* window, while all of FlexInput's hosts (main canvas,
sub-patch editor, and the three overlays) report the same background layer id. Any
state keyed only by node id therefore collided as soon as one node was visible in two
places at once — and the host that painted last won. Plus a D-pad output fix.

### Fixed

- **Pinned Remapper cards disappearing or cropping past the widget border.** A
  whole-module pin paints into its own transform layer, keyed by `(layer, node)` —
  identical for the canvas and every overlay. Two hosts showing one node wrote a
  single transform, so one host's cards were painted through the other's, landing
  outside the clip band. Layer and scroll-state keys are now scoped per host
  (viewport + layer + pin).
- **Pinned Remapper clipping at a stale scroll position.** The clip rect was derived
  from the pre-clamp scroll offset while the layer transform used the post-clamp,
  post-scrollbar-drag one, so any frame where the clamp bit — a card expanding, a
  mapping added, the pin resized, the scrollbar dragged — painted the body at one
  offset and clipped it at another. Clip, transform and scrollbar now share one
  offset frozen for the frame, clamped up front against the previous frame's height.
- **Mapping-card drag-to-reorder doing nothing.** The drag's cross-frame state lives
  in `ctx.data()` under the node id alone, so a Remapper open in a sub-patch editor
  *and* pinned elsewhere had both hosts writing it. The non-dragging host stored
  `pointer_y: None`, leaving the insertion target permanently unresolved: no
  insertion line, no commit on release, and the drag lift reverted every frame.
  Reorder state is now scoped per host as well.
- **D-pad directions driven by a mapping never reaching the virtual pad.** The D-pad
  crosses the bus as four direction Bools, `dpad_x`/`dpad_y`, and a `dpad` Vec2, and
  sinks derive all four hat bits from the Vec2 when they have one — so a mapping that
  wrote only the Bool was cancelled by the still-zero pass-through Vec2 landing after
  it (visible as the Bool lighting up on an AutoMap splitter while the pad stayed
  idle). The Remapper now synthesizes the axis and Vec2 forms from the direction
  Bools whenever it drives any of them; directions it doesn't own keep passing
  through, and opposite directions cancel.

## [0.13.2] - 2026-07-31

Adds the **RWS Aim** module — gyro/stick camera aiming with a physically-grounded,
portable sensitivity you calibrate against an on-screen reference — plus a set of
window/quality-of-life fixes.

### Added

- **RWS Aim module** (`processing.rws`, Processing). Takes a rotation-rate Vec2
  (gyro axes, or a stick treated as a turn rate) and outputs per-tick mouse
  displacement — wire it to the KB/M **Mouse XY (move)** sink — scaled so a
  physical controller rotation maps **1:1** to the in-game camera once calibrated.
  **RWS** is then a plain multiplier on that ground truth. It drives the mouse via
  the displacement pin, which bypasses the device card's mouse-sensitivity, so the
  calibration is portable across presets.
  - **Calibration viewport**, pinnable to the Config Overlay in three views: a
    scrolling **degree ruler**, a perspective **3D cube room** (FOV-matched to
    your game so the on-screen turn rate reads 1:1 when calibrated), or **both**.
    Hit **Calibrate** and the reference spins at a known rate (`cal_speed` rev/s,
    independent of Scale) while the game turns via the output — dial **Scale**
    until the two match. Stop, and the reference follows live input with RWS
    applied, so you can confirm the lock still holds at any speed. The room is
    depth-shaded (bright head-on, dark at grazing corners); background opacity,
    FOV, tick spacing and labels are all adjustable.
  - **Safety:** Calibrate is disabled on the module itself — it takes over your
    real mouse — so it must be pinned to the Config Overlay and run from there
    with a gamepad. A ⚠ on the node explains why.
  - **Right-stick output.** A second `Stick` output emits a right-stick deflection
    (desired turn rate ÷ the game's full-deflection rate, clamped) for stick-aim
    games — wire it to a virtual Right Stick.
  - **Flick stick** on a second input: push past the deadzone to snap the camera to
    the stick's heading (smoothed), hold it out and rotate to track the camera 1:1.
    Flicks are 1:1 (RWS doesn't apply). Tracking is smoothed so a stick that polls
    slower than the eval loop doesn't reach the mouse as pulses; brief input
    dropouts hold the heading instead of dropping/re-firing the flick.
  - **Flick-stick source suppression** (modelled on the Virtual Menu): the stick
    feeding Flick is auto-detected and blocked downstream so it can't leak to its
    default mapping (e.g. the virtual Right Stick), while the module keeps steering
    from it via the pre-block snapshot. Off / Full / In-deadzone (block only past
    the deadzone).
  - Every control is a pinnable, gamepad-editable element; the input-mode dropdown
    and the Calibrate button live in the node header. Gamepad value stepping lands
    on clean numbers (RWS integers / 0.1 fine; FOV whole degrees).
- **Window geometry persistence.** The app restores its previous position, size
  and maximized state on launch (across new versions too), instead of cascading
  down-right each time.
- **Single-instance guard.** Launching a second copy focuses the running window
  rather than starting a conflicting instance (skipped during GPU-recovery
  relaunch so recovery isn't blocked).
- **Wire-drag edge-scroll.** Dragging a wire from an inlet/outlet toward the
  canvas edge auto-pans the view, so you can connect modules that don't fit on
  screen at a usable zoom.

### Fixed

- **Config Overlay stacked-item drag** — with objects stacked, dragging now moves
  the click-cycled *selected* item instead of always grabbing the top surface one.

## [0.13.0] - 2026-07-30

The headline is the **Config Overlay**: summon FlexInput's own controls over a
running game with a gamepad chord, adjust a response curve, a deadzone, or a
whole mapping card with the pad, and feel the change immediately — every other
input stays suppressed so tweaking never leaks into the game. Alongside it: a
full SDL controller backend that can take over every pad, one canonical IMU
frame so gyro behaves identically on every controller, Touch Zones "Touchpad
mode", and four new/reworked Math modules.

### Added

- **Config Overlay** — a transparent, always-on-top, click-through overlay that
  hosts editable module controls over whatever is on screen. Summon it with a
  global keyboard shortcut (default `Ctrl+Shift+C`, re-bindable) or a
  user-assigned gamepad chord that fires even while a game holds focus; dismiss
  with Esc, the Done button, or the same shortcut. Visibility persists across
  restart, like the info overlay.
  - **Pin any widget.** Arm "add element", click a control in the FlexInput
    window (pinnable elements light up amber), and it appears on the overlay.
    Sliders, response curves, toggles, dropdowns and numeric rows are
    interactive there; Touch Zones pads and mapping lists, Remapper / Map
    Action cards and the Virtual Menu field are fully editable; pure displays
    (3D view, scopes, readouts, SVG) pin as static reference. Layout edit mode
    reuses the info overlay's toolbar, snap grid and inspector.
  - **Selective input suppression.** While the overlay is up, physical input is
    blocked at the source so navigating it can't drive the game — except for
    exactly the pins the focused control depends on, which pass through so you
    feel what you are adjusting. The resolver traces the tweaked pin upstream
    to its physical device and narrows to that pin's axis group; a gyro curve
    passes the IMU pins only (the right stick, summed into the same mouse
    delta, stays blocked); a source-like knob with no upstream is traced
    *downstream* through the live selection state to the sinks it modulates and
    back to the physical inputs feeding them, so a gyro↔stick mix bias passes
    only the gyro and the currently routed stick.
  - **Pass-through toggle** (top bar, persisted, OFF by default): input reaches
    the game only while a pin is actually being tweaked, not while merely
    navigating. Turn it on for the always-on behaviour.
  - **Full gamepad navigation**, reusing the Easy-mode nav machinery rather than
    a parallel one — d-pad/left-stick spatial movement, a right-stick cursor
    clamped to the whole monitor, South to enter, value editing on knobs,
    switches and dropdowns, control-point editing on response curves (including
    hold-North bend handles), and RemapScroll into a mapping card. When the
    focused parameter is driven by the *left* stick, the editor swaps to the
    **right** stick so the left one stays free as the passthrough. The
    keyboard/mouse "Special" picker opens in its own always-on-top viewport so
    it is reachable over the game.
  - Icon-based gamepad legend and the Easy-mode selection bloom, so the overlay
    looks and reads like the rest of the app.
  - Config pins exposed from inside a sub-patch **save within that sub-patch**,
    so Easy-mode presets and shared `.fxsp` files can ship built-in overlays.
    The same applies to info-overlay pins.
- **Gamepad shortcut chords, reworked.** Every shortcut (see-through, panic,
  info overlay, config overlay, pin) carries a **press mode** — On press / Long
  press / Double tap — plus a time gap (hold duration, or max inter-tap gap),
  mirroring the remapper card controls. A background watcher is now the single
  engine for all five, so they fire while a game holds focus.
  - **"Only in gamepad navigation" now means by device, not by focus**:
    unchecked, shortcuts fire from any pad; checked, only from a pad currently
    selected for UI navigation. Either way they still work globally.
  - **Single-button shortcuts** are allowed for Guide, Capture and Mic-mute (the
    system buttons games don't use), unless that button is already read by a
    mapping — then a combo is still required. Single-button bindings can't use
    "on press" (it would fire mid-combo) and are nudged to "long press".
  - Assigned chords render as **controller button icons** in Settings, skinned
    to the connected pad, instead of plain text.
  - New **Overlays** settings group, plus a global shortcut (default
    `Ctrl+Shift+E`) that toggles the info overlay's layout-edit mode.
- **SDL controller backend** — a Settings switch, **"Route all pads through
  SDL"** (default off), reads every controller through SDL instead of the native
  gilrs/raw-HID paths, and re-arbitrates live without a restart. SDL pads are now
  first-class:
  - Real identity: the detected controller kind drives the device id
    (`sdl:dualsense:<serial>`), so the node icon, 3D model/skin, pin layout and
    calibration surface all match — a DualSense through SDL exposes its touchpad
    and gyro pins.
  - **Stable ids across reconnect** from the pad's serial number, so a canvas
    node re-attaches to the same physical pad instead of being orphaned.
  - **Lightbar** via `SDL_SetGamepadLED` on the same `lightbar_r/g/b` pins the
    native path uses, pushed only on an actual colour change.
  - **Rumble**, including pads whose layout declares only HD-rumble pins (a
    Switch Pro's per-side amplitudes collapse onto SDL's two motors).
  - Button/pin-name parity with the native layouts, HidHide cloaking, mapped
    feedback routing, digital-trigger handling and per-family button glyphs.
  - Not reachable through SDL, by design: DualSense adaptive-trigger resistance
    and true HD/voice-coil haptics, which need the raw-HID effect payloads the
    native path uses.
- **Touch Zones: Touchpad mode.** A per-node "Touchpad mode" dropdown next to
  mouse speed selects how the relative/absolute centre is applied — **Synced**
  (every card in a zone follows the top analog card, the old behaviour),
  **Per-card** (each analog card uses its own "Rel. center %"), or **Touchpad**
  (the pointer follows the finger's motion like a laptop touchpad). In Touchpad
  mode a stick target becomes a **finger-velocity trackball**: it tilts in the
  direction and speed you move and recentres when the finger stops.
  - **Threshold + response curve on swipe-direction cards.** With a threshold
    set, a swipe card becomes a *held* press gated by the curve-shaped
    deflection in that direction, rather than a one-shot flick; without one, the
    original flick detection is unchanged.
  - Touch Zones mapping cards are now gamepad-navigated **exactly like the
    Remapper's** — response curve (field 4), dot editor (5), threshold (6) and
    the per-card "Rel. center" slider (7), with mode and mouse speed changed by
    up/down instead of LT/RT.
  - The relative-mouse speed multiplier is **per zone**, matching the Touchpad
    mode dropdown beside it (it was node-global while the dropdown was not), and
    touchpad-mode mouse gain is usable out of the box — it was ~50× too weak, so
    a full-pad finger sweep moved the cursor about 11 px and read as "the
    multiplier does nothing".
- **New and reworked Math / Converter modules:**
  - **Min/Max** (`math.min_max`) — variadic node reporting the largest and
    smallest of its inputs on separate outlets. Only *wired* inputs count, so a
    spare pin doesn't peg the minimum at 0.
  - **Quantize** (`math.quantize`) — snaps to a grid of `factor` steps per unit
    (1 = integers, 2 = halves, 4 = quarters) with round / floor / ceil / trunc
    modes. An optional Factor pin overrides the body value while wired.
  - **Vec to Deflection** (`module.vec_to_deflection`, Converters) — splits a
    Vec2 into distance-from-centre and heading. Angle 0 is up and grows
    clockwise; the unit toggles between 0..1 and 0..360. A zero vector reads 0
    on both outputs rather than NaN.
  - All three are pinnable and gamepad-editable.
- **Mapping-output conflict warning** — when two mapping cards drive the same
  bus/sink pin the engine's merge keeps only one and the loser silently does
  nothing. Colliding cards now paint an amber outline and a ⚠ badge whose
  tooltip names the pin and the other module. Covers Remapper, Touch Zones,
  Virtual Menu and Lean; Macro ports and Virtual Menu targets are excluded
  because they merge by design.
- **Icon picker: "Gamepad inputs" category** — gamepad-control glyphs (faces,
  d-pad, bumpers/triggers, sticks and clicks, menu/system, touchpad click /
  touch / swipes / segments, paddles) that render in the *connected* pad's style
  and restyle live when you swap controllers. Available in every picker that
  already hosts the shared icon browser.
- **Window and canvas quality of life:**
  - Main window position, size and maximized state persist across launches (no
    more down-right cascade). A saved position on a monitor that is no longer
    present is dropped while keeping the size.
  - **Single interactive instance** per session: a second launch focuses the
    existing window and exits, so two copies can't fight over virtual devices
    and the elevated helper. GPU/monitor-loss recovery relaunches are exempt.
  - **Edge-scroll while dragging a wire** — the canvas pans when the cursor
    nears a viewport edge, so distant modules can be connected at a usable zoom.

### Changed

- **Negate is now "Inverse"** and gained a **unipolar** mode: instead of
  flipping the sign it mirrors inside `0..max`, so a 0→max ramp comes out
  max→0 and input past either end clips rather than going negative. The module
  id stays `math.negate`, so existing patches keep loading; a load migration
  retitles nodes still carrying the stock "Negate" name and leaves hand-renamed
  ones alone.
- **Touch Zones and Virtual Menu Ports mode now share one BSP zone tree.** Ports
  mode was the last hold-out computing zones from the raw grid, so dividing a
  Ports-mode zone made a full-width cut instead of a per-zone split and a
  Ports-mode radial menu edited incorrectly. An un-edited patch emits
  byte-identical port pin ids in the same order, and structural edits now
  preserve the downstream wiring of every surviving zone.
- **Touch Zones / Virtual Menu field navigation** was redesigned around a focus
  model (border / zone / seam) with a translucent highlight on the focused zone
  and a spatial, alternating zone↔border walk shared by the grid and radial
  renderers. The radial border editor (drag dividers, rotate the origin seam,
  double-click to recentre, +/− to add/remove) is now shared, so a pinned or
  overlaid radial field edits exactly like the node body.
- **Lean** is side tilt, not forward tilt, with the polarity corrected against
  real hardware, and it is now **gated on how the pad is actually being held**:
  each mode's gate reads the *smoothed* gravity estimate, so tilting far enough
  to rotate the pad out of the tested orientation can no longer collapse the
  gesture at its own extremes.
- The Guide-button summon for the config overlay was replaced by a
  user-assignable gamepad chord (the legacy settings fields are kept inert for
  back-compat).
- **Every pinnable value is now gamepad-editable**, wherever it is pinned: the
  Envelope's ADSR dots (with a sustain-line sub-mode) and all five of its
  setting rows, the Trigger Scope controls, the Virtual Menu "options" block and
  the Audio Stream Haptics EQ. Visual-only pins (scopes, vectorscope, 3D viewer,
  labels, SVG) stay pinnable for feedback but are no longer navigation targets,
  so a scope stacked over a real control can't intercept selection.
- Menu zone pickers exclude the menu's own pins (a zone could map to itself) and
  grey out analog outputs that make no sense for discrete zone selection —
  gamepad nav now honours the disable instead of mapping them anyway.

### Fixed

- **IMU frame, unified across every pad.** DualSense and Switch Pro delivered
  different accel frames, so the same physical tilt produced different values
  depending on the controller. The Sony parser had been swapping accel X/Y
  against its own gyro since April; the device layer now normalizes every pad to
  one canonical frame (x = forward, y = side, z = vertical).
  - The Gyro 3DOF module runs entirely in that frame, so Player/World modes no
    longer slant — a flat-on-table yaw stopped drifting the cursor up-and-over.
  - SDL sensor data is rotated into the canonical frame too (verified against the
    native parser rather than SDL's docs: accel and gyro need different sign
    permutations), and SDL touchpad Y is no longer flipped.
- **Switch Pro over Bluetooth.**
  - Gyro/accel no longer stream frozen values: the sensor-enable subcommand is
    re-asserted a few times over the first ~16 s after a wireless pad opens,
    giving the report-mode switch another chance once the link has settled.
  - The "frozen until reconnect" freeze is fixed: a raw-HID handle whose reads
    start returning 0 bytes after HidHide cloaks the device is now dropped and
    re-opened automatically once the stall passes 5 s (comfortably past the
    pad's transient ~3 s gaps), automating what a manual reconnect did.
- **HidHide cloaking for wireless pads.** The Bluetooth hardware-id needle used
  a 2-digit vendor source (`VID&02…`) while Windows reports a 4-digit one
  (`VID&0002…`) for paired controllers, so instance lookup returned nothing and
  wireless pads were never cloaked. Both forms match now.
- **Device enumeration.** A DInput pad surfaced by both SDL and gilrs is deduped
  at the merge (SDL wins, and a pad SDL can't open still comes through gilrs);
  SDL no longer opens FlexInput's own HIDMaestro XInput companion, which had
  been looping emulated output back in as a physical input.
- **HIDMaestro partial-install state** no longer strands the XUSB companion INF
  bound with its DLL gone (WUDFHost faulted on every load, and uninstall
  reported success while reinstall could never verify).
- **AutoMap bus reaches sink pins added after a node was saved** — a keymouse
  node saved before the `mouse_move` pins existed had them missing from its
  frozen pin list, so Touch Zones touchpad mouse silently did nothing. Current
  sink pins are now appended when building the target, with no patch migration.
- **3D controller viewer.**
  - Every viewer gets its own GPU state. Two visible viewers of the same model
    were sharing one slot, so with a sub-patch editor open beside an overlay pin
    the two cameras measured occlusion into the same query set and the overlay
    x-rayed everything.
  - X-ray line of sight is measured without occlusion queries (they return zero
    on some drivers), engages as fast as it clears, and no longer flickers when a
    part turns away from the camera.
  - Per-pin colour and model overrides live on the pin instead of a shared
    channel, so overlay swatch edits stick, alpha is preserved, and a pinned
    instance can't swallow a module-side `.fxcol` load.
  - A viewer inside a sub-patch now resolves its device instead of rendering an
    inert model with only gyro animating; button press travel scales by height
    rather than horizontal footprint.
- **Timer resolution and loop pacing.** `timeBeginPeriod(1)` raised the *global*
  system timer resolution, which Windows 11 honours only for the foreground
  process — backgrounded, the engine tick and device-I/O loop collapsed to
  ~64 Hz, so a gyro- or stick-driven mouse drew straight-line segments while
  another app was focused — and its system-wide 1 ms tick added DPC latency that
  stuttered other high-rate input such as an I2C-HID laptop trackpad. Both loops
  now wait on a per-thread high-resolution waitable timer (~0.5 ms precision, no
  global raise), with a process-level opt-out of timer throttling as backup.
- A swipe card's response-curve preview now traces the 1-D value along its own
  direction instead of the 2-D deflection magnitude, so the preview dot and the
  threshold line agree with what actually fires.
- Unwired input pins no longer glow with borrowed signals — the Gyro 3DOF module
  lit its Gyro and Accel pins in mismatched colours because two modules reuse the
  per-node signal slot for their own UI readouts.
- A pinned Remapper or Map Action in the config overlay no longer auto-captures
  input every frame; the mapping-card selection glow, curve dot highlight and
  bend handles now appear there (all of them were a per-viewport pass-counter
  mismatch, now handled once by a viewport-agnostic highlight subsystem).
- Rejecting a non-adjustable config pick no longer deadlocks and panics the app.
- In the overlay layout editors, clicking through a stack of overlapping items
  selects the one you cycled to — and dragging now moves *that* item instead of
  handing the drag to whatever sits on top.
- The overlay drops always-on-top around **every** blocking file dialog, not just
  one.

### Internal

- **Modular split.** `viewer.rs` went from 25,115 lines to a 1,255-line facade
  over 22 focused modules plus a crate-level `widgets` library; `app.rs` from
  14,981 to 5,866, with the I/O threads, graph building, window chrome, device
  pool, sub-patch editors, settings window and the whole gamepad-nav cluster
  moved out; `eval.rs` split into `eval/` with the module evaluators and tests
  lifted into their own files. Every move was verified verbatim, and all
  pre-split paths still resolve through glob re-exports.
- **Module registry seam** — engine eval dispatch, UI classification and the
  modules crate now meet through a registry, with Audio Stream Haptics gated
  behind a default-on `asth` cargo feature as the pilot for optional modules.
- **Documentation** — ten reference documents under `docs/` (architecture
  blueprint, engine internals, AutoMap system, UI architecture, devices, modules
  and network references, patch formats, development guidelines, and a docs
  README), followed by a full accuracy pass verifying every claim against source.

## [0.12.0-hotfix] - 2026-07-19

### Fixed

- **Virtual Menu driver suppression now happens at the source.** An open menu
  publishes a block request applied on the next tick, so its analog drivers
  reach only the menu's own navigation — not a mouse mapping, another module in
  the patch, or any sink — while the menu keeps steering off a pre-block
  snapshot. Four suppression modes: **Passthrough**, **Active** (block only
  drivers actually in use; gyro latches off its cursor leaving the deadzone,
  not raw rate), **Latch** (the first engaged driver owns the menu exclusively
  until it disengages), and **Full** (all enabled drivers while open).
- Selected zone cards fire a clean pulse and release — an off-bus output pin no
  longer latches pressed on the virtual pad.
- Press-mode Select works from a downstream Remapper (via a 1-tick macro
  carry-over, since a menu upstream of its own Select mapping is a feedback
  cycle).
- Editing a menu no longer resets its overlay size and placement: the rect
  write-back also lands in open sub-patch editors instead of being clobbered by
  their snapshot.
- Output AutoMap glow excludes suppressed driver pins.

## [0.12.0] - 2026-07-18

### Added

- **Virtual Menu module** — a pop-up grid/radial menu summoned by a mapped
  analog input (Macro-style named targeting), drawn on its own transparent,
  click-through overlay independent of the info overlay. Reuses the Touch Zones
  editor (BSP zones, partial dividers, per-zone mapping cards) plus a radial
  ring mode, per-zone name/icon, and hold/toggle/touch activation with optional
  input suppression while the menu is open.
- **Icon picker categories & search** — icons are embedded from
  `app/assets/general/` sub-folders at build time; the picker gains a category
  dropdown (default "All") and a name filter, shared by the Macro, Menu, and
  Touch Zones pickers. Ships a large [game-icons.net](https://game-icons.net/)
  set (CC BY 3.0, attributed in Settings, the README, and `ATTRIBUTION.md`).
- **Pinned Touch Zones / Menu style** — per-pin main/highlight colour overrides
  and a visibility mode (always show / show on touch / touched-zones-only, the
  last fading non-active zones to 20%) for pads pinned to a sub-patch layout or
  the screen overlay.
- **Info overlay** — a transparent, click-through, always-on-top layer over the
  whole screen (works over borderless/windowed games; exclusive fullscreen
  bypasses the compositor). Pin module UI elements onto it — from the tab
  canvas or from inside first-level sub-patches (Easy presets included) — and
  they render live with signal glow while every click passes through to the
  game underneath. Toggle it from the title bar (▣), a global keyboard
  shortcut (default `Ctrl+Shift+O`, re-bindable in Settings), or a learnable
  gamepad chord.
  - **Edit mode** (✏ button): drag, resize, recolor, and z-order pinned
    elements, and add the same Text/SVG/shape decorations as sub-patch
    layouts — the full layout toolbar, snap grid, and inspector are shared.
  - **Add element**: the overlay collapses to a glowing border while pinnable
    elements light up amber in the FlexInput window; click one and you're
    back on the overlay with the new pin selected, ready to place.
  - **Overlay frame rate** setting — the overlay paces its own repaint
    (default 60 FPS) independent of the background repaint rate, so pinned
    readouts stay smooth on top of a game while the main window idles.
  - Overlay layouts persist per patch tab (workspace, save files, recovery).

### Fixed

- Colour picker no longer darkens toward black or clamps RGB to the alpha value
  at reduced opacity — colour params are read as straight (un-premultiplied)
  bytes end to end, so menus and materials render the colour you picked.
- Double-clicking a zone divider recentres it between its immediate neighbours
  instead of overshooting and squashing the next zone (radial and grid editors).
- 3D viewer: each stick's dome / cap / rim are treated as one object for x-ray
  occlusion, so a cap covering its own dome no longer strobes the whole stick
  transparent.
- Switch Pro gyro now uses ±2000 dps like the other controllers, so a physical
  rotation reads the same normalized value on every pad.
- Transparent child windows no longer composite as an opaque white sheet on
  Win11 + AMD (missing `WS_EX_NOREDIRECTIONBITMAP` at window creation), and
  window resizes no longer churn the swapchain through stale buffered sizes.

## [0.11.7] - 2026-07-12

### Added

- **Touch Zones module** — divide a touchpad into a BSP tree of zones (including
  partial / in-zone dividers) and map each to buttons, chords, mouse, sticks, or
  scroll. Per-trigger activation glow, drag-reorder, combo capture, hold-zones,
  a per-zone adaptive relative/absolute centre, and full gamepad navigation.
- **Macro Output module** — user-created named, typed, iconed output ports
  (Bool / Float / Vec2 / Any) addressable BY NAME from Remapper, Touch Zones, and
  Lean pickers with no wires; custom SVG icons can be embedded into the patch.
- **Per-card response curves + activation thresholds** — every analog mapping
  card (Remapper, Lean, Touch Zones) gets its own response curve editor and an
  optional manual activation threshold (a horizontal line on the curve's output:
  the binding holds while the shaped value sits on/above it, releasing when it
  dips below). Shared Copy/Paste/Save/Load with the Response Curve module, and
  full gamepad navigation into the graph + threshold. Analog triggers are now
  captured as analog inputs during Learn so they can carry a curve.
- Horizontal + analog scroll for the virtual mouse.
- **Network Send / Network Receive modules** (Network category). Carry the
  AutoMap gamepad bus between two FlexInput instances over the network: wire a
  physical pad into **Network Send** on one PC and a **Network Receive** into a
  virtual pad on another. Three transport tiers, chosen per node:
  - **LAN (UDP)** — plaintext, IP + port only (also works over WAN with a static
    IP or port forward).
  - **Secure (PSK)** — the same UDP path with ChaCha20-Poly1305 authenticated
    encryption keyed by a shared passphrase (HKDF-SHA256), replay-protected.
  - **P2P (code)** — dial-by-code over [iroh](https://www.iroh.computer): the
    Receive node shows a short pairing **code** (its cryptographic identity); the
    Send node pastes it. No IP, no port, no port-forward — iroh hole-punches a
    direct connection and falls back to a relay when it can't, so it works
    through NAT, CGNAT, and VPNs. Encryption and authentication come from the
    keypair (the code can't be impersonated), so no passphrase is needed.
- **Keep-saved toggle** on the P2P tier: the pairing code / node key are NOT
  written to patches or workspace backups by default (so a shared patch never
  leaks them and each restart starts fresh); tick **Keep saved** to persist a
  stable code that travels with the patch.
- **Bidirectional haptics**: rumble, light bar, and adaptive-trigger feedback the
  game requests on the receiving PC's virtual pad travels back over the same link
  to the physical pad, riding the existing AutoMap feedback path.
- **Fail-safe**: if no valid packet arrives within a configurable staleness
  window (default 200 ms), the receive node publishes a neutral frame (sticks
  centered, buttons released) so a dropped link can never leave inputs stuck; and
  a physical pad's haptics are actively zeroed when their producer disappears.

## [0.10.7] - 2026-07-03

A new Vec Reshaper module for fighting analog-stick "diagonal stickiness" —
directional reshaping of a Vec2 with a visual editor.

### Added

- **Vec Reshaper module** (Processing category, Vec2 → Vec2). Reshapes a stick
  vector as a function of DIRECTION, which the radially-symmetric Vec Response
  Curve cannot do. Two orthogonal controls: a per-direction **Boundary** that
  sets the reachable output envelope (1.0 = circle, √2 = the square's corner, so
  a round stick can be expanded to fill a square for games that expect square
  response), and a per-direction **Gain** that accelerates/decelerates within
  that envelope (push diagonals faster to kill diagonal stickiness). One quadrant
  is edited; the rest mirror it (4-way, or X-mirror for asymmetric up/down).
- **Visual editor** on the node body: a direction→value curve (grid + snap on
  both axes) with a Gain/Boundary toggle, plus a live 2D pad showing the unit
  circle, the reshaped envelope, and a smooth **stretch-field gradient** (blue =
  accelerated, red = decelerated, transparent at neutral) so the internal shaping
  is visible without moving the stick. Live input→output dots trace the current
  deflection. Presets: Circle, Square, Diag+. Every element is individually
  pinnable in Easy mode and gamepad-navigable.

Gamepad navigation now reaches every Audio Stream Haptics control in Easy mode.

### Fixed

- **Audio Stream Haptics pinned elements are now editable via gamepad
  navigation.** The module was missing from the Easy-mode nav dispatch, so its
  pinned rows were selectable but not editable. All calibration rows (Volume,
  Release, Crossover, Amplitude floor/ceiling/curve, Balance, Swap, Rumble mix)
  and the capture-mode block (App/Focused/System + include-children) now route
  through the unified multi-field editor. The scope's EQ points are dot-editable
  through the same curve-dot path as Response Curve widgets (South enters,
  LS/dpad highlights a dot, RT/LT add/remove at the cursor, South edits a dot).

## [0.10.5] - 2026-07-01

SDL3 gamepad support for controllers FlexInput doesn't handle natively, and the
extra rear-paddle / misc buttons those pads expose, mappable through the AutoMap
system.

### Added

- **SDL3 gamepad backend** for controllers FlexInput doesn't parse natively
  (Steam Controller, 8BitDo, arcade sticks, third-party pads). gilrs and the
  raw-HID path keep the pads they handle well (Xbox/XInput, DualShock 4,
  DualSense, Switch Pro) with their tuned gyro/touchpad/HD-haptic overrides; SDL
  is enumerated only for pads that classify as generic, filtered by VID/PID so no
  controller is surfaced twice. For those pads it relays sticks, buttons, analog
  triggers, gyro/accel, touchpad, and the extra paddle/misc buttons. SDL is built
  from source and linked statically — no extra DLL to ship.
- **Extra buttons in the AutoMap system.** Rear paddles (`btn_paddle_l1/r1/l2/r2`)
  and misc buttons (`btn_misc1..6`) are now part of the canonical AutoMap pin set,
  so they can be mapped to anything via Remapper and other AutoMap modules. Rear
  paddles render a generic labeled icon (PL1/PR1/PL2/PR2); labels are
  device-agnostic for now.

## [0.10.4] - 2026-07-01

HidHide masking of remapped physical controllers, exact XInput player-slot
control, same-family physical/virtual pad fixes, Audio Stream Haptics raw
analysis outputs, and mixed-output smoothing.

### Added

- **HidHide masking** of remapped physical controllers via the elevated
  HIDMaestro helper, so a game sees only the virtual pad and not the physical
  device behind it. Masking is reconciled on device/patch changes and toggleable
  from Settings.
- **Exact XInput player-slot control:** a slot-reorder engine plus on-canvas and
  Easy-mode slot indicators, with safe virtual re-arrival so a re-created pad
  reclaims its slot. Resolves physical pads reading from the wrong slot after
  focus loss.
- **Audio Stream Haptics — raw analysis output pins.** Six new Float outputs
  after the AutoMap passthrough expose the raw two-band decomposition *before*
  the carrier/modulator (AM/RM) blend: per-band/per-side envelope followers
  (`LF/HF EF L/R`) and each band's carrier frequency in Hz (`LF/HF Hz`). Wire
  them to scopes/readouts or drive other modules from the audio analysis.
- **Audio Stream Haptics — pinnable capture-mode block.** The App/Focused/System
  selector (with its process picker and status line) can now be pinned to a
  sub-patch body like the calibration rows.
- **Braid mixed output (experimental):** optional Settings toggle that makes the
  virtual-gamepad and keyboard/mouse outputs **submit in strict alternation** (a
  shared turn token) so a gamepad HID report and a mouse `SendInput` never land
  in the same instant. Neither stream is muted or zeroed — the mouse accumulates
  between its turns (no motion lost) and an idle mouse just passes its turn, so it
  never chops the pad. Pacing is a per-lane rate: **Real-time** (fastest, lowest
  latency — limited only by the polling/mouse rate) or 500 / 250 / 125 Hz. For
  empirically probing games whose input arbiter behaves differently under
  simultaneous mixed output (confirmed to recover a game that lost mouse input
  intermittently under FlexInput). Off by default; effect is game-specific.

### Changed

- **Smoother virtual mouse with mixed output:** the Virtual Keyboard & Mouse
  emission loop now scales motion by the *real* elapsed time each tick instead of
  assuming a perfect interval, so cursor speed no longer lurches under scheduler
  jitter when a virtual gamepad is flushing concurrently. The loop also runs at
  1 kHz (was 500 Hz), halving the integer-pixel stair-step so slow stick-aim
  reads smoother.
- **Physical-mouse suppression is now configurable and game-aware.** It is
  automatically forced OFF in "mixed mode" (a virtual gamepad active alongside
  the keyboard/mouse), since games that warp/recenter the cursor each frame would
  otherwise make virtual mouse aim stutter. New Settings: a master on/off toggle
  and an adjustable release window (50–2000 ms, default 500).
- The virtual-mouse emission thread now runs at `TIME_CRITICAL`, and its per-tick
  motion is clamped to ≤4 ms of travel, so an occasional scheduler gap no longer
  discharges as a single cursor jump under heavy game load.

### Fixed

- **Physical/virtual same-family pad crossing.** A physical controller no longer
  freezes or reads from the wrong device when a virtual pad of the same family is
  present (DualSense gilrs-walk vs hidapi index crossing; XInput slot/Steam
  consolidation). Physical XInput is now read directly via `XInputGetState` so it
  survives focus loss, and the physical pad is correlated to its real slot.

## [0.10.2] - 2026-06-27

Touchpad output bindings for Remapper/Lean, a combiner mapping fix, and a
HIDMaestro driver-uninstall path with on-demand install from Easy mode.

### Added

- **Touchpad / swipe / mic output bindings** in the Remapper and Lean (3DOF→2D)
  "Special" picker. The picker is now a button (mouse-clickable cells + gamepad
  nav, same popup for both) offering the three DualSense touch zones, touchpad
  click, horizontal/vertical analog swipe (gated to analog inputs), and the
  DualSense mic button. The engine synthesizes real touch points from these
  bindings, stacking up to the two the hardware supports.
- **Uninstall HIDMaestro driver** path: tears down all live virtual device nodes,
  then removes every installed driver package via the elevated helper (new
  `UninstallDriver` IPC request + `deploy::uninstall_driver`, with an
  `Uninstall`/`Uninstalling` device-op and progress state).
- Easy mode gamepad output card stays **enabled when the driver is absent** —
  selecting a model installs HIDMaestro on demand (one admin prompt) via the
  normal create path, with a hint shown.

### Fixed

- **Combiner SORT** now picks the first *asserted* port (with fallback to the
  first port), so a Remapper's mapped output is no longer clobbered by a raw
  pass-through bus port — fixes broken gamepad button→button remapping inside a
  sub-patch (you'd get neither button, or both lighting up).
- **Touchpad combo logic:** buttons in a touch combo only *gate* the finger
  (activate it), while analog inputs drive the swipe axes — no longer "stuck at
  full value." Opposite cardinals of one axis cover both halves; a combo can map
  e.g. button + left-stick (all directions) to both touchpad-point axes.
- **Gate-button suppression for multi-axis touch combos:** a combo mixing
  opposite cardinals of one axis (which can never be simultaneously held) now
  correctly consumes its gate button from pass-through while active, instead of
  leaking it through.

## [0.10.0] - 2026-06-26

This release replaces the ViGEm backend with a pure-Rust HIDMaestro virtual-device
stack (Xbox 360 / XInput, DualShock 4, DualSense), adds a driver-free Audio Stream
Haptics module, and ships a large batch of rumble, device-fidelity, and UI fixes.

### Added

#### HIDMaestro — pure-Rust virtual devices (ViGEm removal)
- Pure-Rust HIDMaestro shared-memory client and HID descriptor parser + report
  encoder (DS4 path), with plain-HID device create/teardown entirely in Rust.
- `VirtualDevice` adapter wiring HIDMaestro into the existing virtual-device API.
- Driver availability probe, installed-INF discovery, and an elevated helper that
  deploys the driver (certificate + `pnputil`), bundled into the app via self
  re-exec rather than a separate binary.
- App integration: HIDMaestro outputs in Advanced mode, device persistence
  setting, and a per-instance driver config so devices report their real VID/PID.
- Working virtual **Xbox 360 / XInput** pad, including rumble-in across all rumble
  APIs, with customizable poll rate, multi-pad support, and a forked driver that
  returns real DualSense feature reports.
- Virtual **DualSense** touchpad emit and virtual→physical forwarding of DualSense
  LEDs and adaptive triggers.
- Gyro/accel encoding, touchpad-neutral handling, profile-driven rumble, and
  friendly device names.

#### Audio Stream Haptics
- Driver-free Audio Stream Haptics module (WASAPI loopback → rumble routing).

#### Devices & rumble
- Physical DualShock 4 touchpad decoding.
- Per-device rumble shaping UI; forwarding of game rumble from HIDMaestro virtual
  pads to physical controllers.
- Single gamepad output card with model selector + rumble range (Easy mode).

### Changed
- `cargo run` / `cargo build` at the workspace root now resolve to the GUI app.
- Async virtual-device lifecycle with a progress overlay and driver reinstall flow.
- Combine feedback from multiple virtual sinks onto a single physical pad.
- Persist setting clarified as HIDMaestro-only (not ViGEm).
- Foreground-gated GPU-loss stall handling.

### Fixed
- **Rumble:** Switch Pro HD-rumble write path, legacy-rumble pins routed to the HD
  voice coil, peak-hold so a same-tick on→off pulse survives, default HD-rumble
  frequency, and physical feedback delivered even under bypass.
- **Device fidelity:** DS4/DualSense digital L2/R2 triggers, DS4 read-back falling
  through to a gilrs WGI axis scramble, DS4 IMU/touchpad byte offsets (+2/+3 too
  high), and virtual pads no longer reporting a false 100% / physical battery.
- **Own-virtual detection:** distinguish own emulated pads from real same-VID/PID
  controllers by HID instance path / USB product string; restore tagging for both
  ViGEm (Xbox/DS4) and HIDMaestro pads.
- **Teardown reliability:** remove HID children and sweep orphaned ghost children
  on teardown, clean up orphans only on first hello, survive abrupt exit without
  orphaning nodes, guarantee a single helper across close→reopen and overlap, and
  collapse teardown to one device enumeration with parallel `pnputil`.
- **Permissions:** grant the unelevated app pipe and Global SHM write access
  (fixes OS error 5).
- **Persistence:** stop destroying virtual nodes on clean exit when persist is on,
  and restore helper persist after GPU recovery even when stalled.
- **UI:** restore node-drag and param edits inside the sub-patch editor, keep live
  visuals animating in the sub-patch editor, restore last-active tab on launch with
  per-canvas pan/zoom, manual MIDI refresh to stop periodic audio disruption,
  exclude battery from AutoMap port/wire glow, and kill the startup low-battery
  warning (show physical pad battery instead).
- **Devices:** stop ~2s input gaps caused by hidapi refresh on the I/O thread.

[0.10.0]: https://github.com/x-iso/FlexInput/compare/v0.9.7...v0.10.0
