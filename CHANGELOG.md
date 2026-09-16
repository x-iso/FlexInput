# Changelog

All notable changes to FlexInput are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.14.0] - 2026-09-16

### Added

- **A factory preset with calibrated gyro aim.** Easy mode gains
  **General purpose preset with RWS**, a general-purpose layout whose gyro
  aiming runs through RWS Aim, so the camera turn can be calibrated to match a
  physical rotation 1:1 instead of tuned by feel.

- **RWS Aim: Mouse Scale in dots per 360°.** A header toggle switches the Mouse
  Scale readings between dots per degree and dots per 360° turn — the unit Steam
  Input and sensitivity databases use — everywhere the value appears: the body,
  pinned widgets, the ruler's value box, auto-cal results and gamepad stepping
  (100 dots, fine 10). Display only: the stored value and `.fxrws` presets are
  unchanged. Auto-cal also tips that, without a known value, calibrating at a low
  in-game sensitivity usually gives finer aim steps.

- **Remapper: chords that care about order, and a Sequence mode.** Clicking a
  card's `in →` pill switches it to **in order**, so A then B is a different card
  from B then A (the inputs join with › instead of +). While such a card waits
  for the rest of its chord, the inputs it has matched are held back from the
  rest of the node for up to the time gap, and a matched ordered card outranks a
  plain card over the same inputs, which becomes the fallback for the wrong
  order. The new **Sequence** mode fires on inputs pressed one after another,
  each within the time gap of the previous; earlier steps may already be
  released, and the card stays on while the last step is held. On a card with a
  single stick direction or trigger it times the move instead: leave zero and
  reach the threshold inside the gap.

### Changed

- **RWS Aim: a more compact body.** The calibrated constants share a row
  (**Mouse Scale** | Stick °/s), as do RWS | V/H — each still pins on its own.
  Auto-cal now sits above the ruler, with its guide and gamepad hints on rows of
  their own instead of stretching the module wide. The Mouse output is now
  labelled **Mouse Move (XY)**; saved patches are renamed on load, wires intact.

### Removed

- **The Joy-Con 2 experiment switches.** The investigation they served has
  concluded, so the protocol probing is gone along with its `FLEXINPUT_JC2_*`
  environment variables (added in 0.13.8), raw capture and frame dumps. Joy-Con 2
  now initialises with exactly the working recipe. This wasn't free to keep: a
  connect held the shared radio for over half a second of probes, during which a
  Bluetooth Classic controller on the same dongle couldn't be heard. The log also
  stops recording every advertisement the scan hears, and a refused scan start
  backs off for 5 s instead of retrying five times a second.

### Fixed

- **A paired Switch Pro reconnects over the Bluetooth dongle straight away.**
  Reconnecting took several tries and succeeded by chance: the dongle listened
  for incoming calls under 1% of the time, and a controller's call could be
  thrown away while the radio was busy or pushed out of the queue by Joy-Con 2
  scan traffic. When the call did get through, the controller still failed to
  connect about one time in three, because a question many Bluetooth stacks ask
  before opening a channel went unanswered. After such a failure the controller
  was left half-connected and had to be switched off and on to try again; a
  failed setup now closes the link so it can call straight back.

- **Bluetooth Classic input no longer stalls every two seconds.** The title-bar
  Bluetooth button checked for adapters by opening each one every two seconds —
  including the adapter streaming a controller, which interrupted the link. Input
  went stale in short bursts: flat lines in the gyro calibration trace, stick
  movement staggering at the same rhythm. The button now knows an adapter is in
  use without touching it.

- **Remapper: Short, Long and Double cards no longer swallow the presses they
  don't fire on.** A Short card ate long presses, a Long card ate taps and a
  Double card ate single taps. They now hold the input back only while deciding:
  if the card fires, the input is consumed; otherwise it comes back — live if
  still held, or a finished tap replayed at its original length. A held-back
  stick direction is capped where it crossed the threshold rather than zeroed.

- **Remapper: swapping the sticks with analog cards works when only one stick
  moves.** With one card mapping the left stick to the right and another the
  reverse, moving a single stick drove both, because a card only suppressed its
  source stick when every one of its directions was deflected at once.

## [0.13.8] - 2026-09-14

### Added

- **Third-party licences, in the app.** Settings → Credits gains a
  **Third-party licenses** button opening a viewer with the full licence text of
  everything FlexInput redistributes — linked-in crates (including SDL3, which is
  built from source and linked statically), the vendored HIDMaestro driver
  package, and the bundled models and icon sets. Texts are embedded verbatim from
  `licenses/` and from the licence files the vendored dependencies already ship,
  so a missing one is a build error rather than a silently dropped notice.

- **Resolution- and aspect-independent overlays.** Info- and config-overlay
  elements now re-anchor to the screen instead of living at fixed pixels, so a
  layout authored at one resolution holds up on another display or aspect ratio.
  Each element — module pins and decorations alike — carries a per-axis anchor
  picked from a 3×3 zone grid (corner / edge / centre) plus **independent
  Stretch X/Y** toggles that scale that dimension proportionally to keep the same
  relative width/height; `Auto` derives both point and stretch from where the
  element sits. An element can also be **anchored to another element**, so a box,
  its label and its icons travel and stretch together as a group (with a visible
  link guide, a selecting-target mode, and a pivot marker on the anchor point in
  edit mode). The overlay snap grid is now a zone-aligned percentage of the
  viewport rather than pixels.

- **RWS Aim: stick-aim, V/H bias, and measure-based calibration.** The stick
  wired to the Flick input can now drive BOTH the Mouse and Stick outputs as a
  rate aim with its own RWS multiplier (inside the flick deadzone when Flick is
  on, full range when off). A per-source **V/H sensitivity bias** scales the
  vertical axis relative to the calibrated horizontal, separately for the gyro
  and stick sources. And a new **measure-based auto-calibration** calibrates by
  turning the camera a known amount instead of eyeballing the ruler: **↕180°**
  (aim down → up, horizontal blocked) or **↔360°** (one full turn, vertical
  blocked). You pick which output to calibrate — **Mouse** sets Scale, **Stick**
  sets Stick °/s — and only that output drives the game during the sweep, so
  patches that wire both behind selectors calibrate correctly. The rotation is
  measured in the engine with its sign, so turning back after an overshoot
  subtracts; the result shows old → new (or why nothing changed), and a Stick
  sweep that maxed out the stick is refused. Fully gamepad-operable from the
  config overlay (◄► method, ▲▼ output, A start/finish, B cancel — A and B work
  however the sweep was started and are withheld from the game while it runs),
  since the mouse is busy driving the game — with an optional **snapshot comparison** that
  freezes the game frame behind the overlay (captured so our own overlay is
  excluded) and shows its left half at 70% as an alignment reference for the
  360° turn.

- **RWS Aim presets.** Save/Load in the module header stores the full aim feel —
  Scale, RWS, Stick °/s, V/H bias, flick and stick-aim settings, suppression —
  as a `.fxrws` file (with the derived counts-per-360 for reference), so a
  game's calibration can be recalled instead of redone.

- **The Bluetooth panel names an adapter that can never work.** Some chips only
  answer HCI after their vendor driver uploads firmware — Broadcom and Cypress
  patchram parts, Intel `ibt-*` parts — and binding the adapter to WinUSB is
  exactly what stops that driver loading. Such an adapter used to appear idle and
  healthy while nothing could talk to it, sending the user looking for a
  configuration mistake that didn't exist; it now shows in red with the reason,
  ahead of every other state. The reason is recorded when a transport actually
  fails (probing for it would reset a radio mid-conversation) and cleared on success.

- **Joy-Con 2 experiment switches, for testing on other hardware.** Environment
  variables select alternative init paths and extra capture — among them
  `FLEXINPUT_JC2_MODE`, `…_FEATURES`, `…_DISCOVER`, `…_MOUSE` and
  `FLEXINPUT_JC2_CAPTURE`, documented in `docs/BLUETOOTH_TRANSPORTS.md`. An experiment run never falls back
  to the Windows Bluetooth stack, because that produces a log indistinguishable
  from a healthy session. Defaults are unchanged.

### Changed

- **HIDMaestro updated from v1.3.17 to v1.7.3.** Both driver packages are now the
  upstream release, unmodified: the XInput companion is no longer FlexInput's own
  rebuild. That rebuild existed to add `PollIntervalMs`, a configurable timer for
  how often the companion answers Windows.Gaming.Input / GameInput reads — the
  stock companion answered on a fixed 8 ms timer, capping them at 125 Hz with up
  to 8 ms of added delay. v1.7.3 replaces the timer with an input doorbell:
  FlexInput signals it with every frame, and the pending read completes when the
  frame arrives, at whatever rate the I/O loop writes and with no timer phase
  delay. The polling-rate setting now reaches existing virtual Xbox 360 pads live
  rather than only at creation. Plain `XInputGetState` reads were never timer-bound
  and are unaffected. The release also fixes a driver race that could hand a
  reader the previous frame.
- **Driver updates now reach existing installs.** Nothing used to replace an
  installed driver, so a newer vendored HIDMaestro would never have been used on a
  machine that already had one. On startup the helper now compares each installed
  package's `DriverVer` with the vendored INF's and reinstalls on a mismatch,
  through the same once-only path as the per-machine re-signing below: device
  nodes are cleared (waiting until they're gone), the packages replaced, and
  virtual devices recreated. Existing 1.3.17 installs upgrade this way on first
  launch.

- **More room for two controllers on one Bluetooth dongle.** Each LE link now
  requests a 3.75 ms connection event instead of the shortest possible one. A
  single link already held 200 Hz, but a second controller cost one of the two
  about a fifth of its reports. The stack also requests the LE 2M PHY.

- **Joy-Con 2 gyro is steadier at rest.** Its rate is still derived from the
  controller's fused heading, and differencing that report to report got noisier
  as the link got faster: about 3 °/s of jitter with the pad lying on a table. The
  rate is now averaged over 25 ms of wall time, which adds about 12 ms of delay. A
  real angular-rate stream would make this unnecessary, but on the Mobapad M12-S
  the channel that enables it accepts commands and never answers them, across every
  variation tried. Retail Joy-Con 2 hardware is untested there and may differ.

- **Joy-Con 2 diagnostic logs are opt-in in release builds, and moved to
  `%APPDATA%\FlexInput\logs`.** `jc2-dongle.log`, `jc2-drift.log` and the rest were
  written beside the executable, where an installed app can't write, and grew for
  the whole session. They were also written on the controller's transport thread
  with a flush per line, which could freeze input for seconds when the file sat
  in a synced folder. They now go through a background writer, rotate at 5 MB
  keeping one previous generation, and in a release build open only when their
  environment variable is set (`FLEXINPUT_JC2_LOG=on`, or a path). Debug builds
  log as before. The helper's `flexinput-hidmaestro.log` moved to the same folder
  and is still always written.

### Removed

- **RWS Aim auto-spin calibration and the Gyro/Stick input selector.** The old
  method that spun the camera at a fixed rate is gone in favour of the measure
  calibration above, along with the header input-mode selector it needed; the
  module's rotation input is always a gyro rate. Stick °/s now has its own
  pinnable row, and the pinned V/H bias row is interactive.

### Fixed

- **Curve bends stay on their segment when dots are added or removed.** Each
  segment's bend was stored by position and never re-indexed, so removing a dot
  slid every later bend onto the segment to its left, and adding one slid them
  right. Removing a dot now clears only the bends of the two segments that met at
  it; adding a dot inside a bent segment splits the bend so the curve keeps its
  shape. Covers the Response Curve, Vec and Two-way curves, Vec Reshape's gain
  curve, the Envelope, and gamepad dot editing.

- **The config overlay no longer takes the game's focus.** Hovering a pin used
  to activate the overlay window — including a game's hidden cursor parked over
  a pin when the overlay was summoned, or a mouse output sweeping the cursor
  across one — so a game that only reads input while focused went deaf until the
  overlay was re-summoned. Hover no longer activates it; after a click it hands
  foreground back to the game once the cursor leaves the pins; and while a
  calibration sweep runs or a gamepad is editing a pin, the overlay ignores the
  mouse entirely (fully click-through) so the game keeps it.

- **An incompatible tab no longer wipes the whole workspace.** `workspace.json`
  (and the Save/Load Workspace files and the crash-recovery snapshot) used to be
  deserialized in one shot, so a single tab carrying a field from a different
  schema version failed the *entire* load — the app started empty and the autosave
  then overwrote everything with no backup. Loading is now per-tab resilient: a
  tab that no longer fits the schema is **blanked** (its slot, title and bindings
  kept, title marked "(recovered)") while every other tab loads normally, and the
  original bytes are copied to a timestamped `.corrupt-*.bak` sibling before
  anything can overwrite them.

- **Config-overlay selection glow tracks re-anchored pins.** The active-pin focus
  ring was drawn from the raw authored rect while the widget painted at its
  re-anchored position, so on a different resolution the glow sat away from the
  pin until an edit-mode round-trip realigned them; hit-testing, passthrough, nav
  targets and the ring now all use the resolved rect.

- **HIDMaestro's MIT licence now accompanies its binaries.** The signed driver
  package under `crates/hidmaestro/driver` was vendored and redistributed without
  the licence text MIT requires travel with it; the notice is now reproduced at
  `crates/hidmaestro/driver/LICENSE` and shown in the in-app viewer.

- **Any WinUSB-bound Bluetooth adapter works, not only the one FlexInput was
  developed on.** The Joy-Con 2 transport opened a hard-coded Realtek `0bda:a728`
  without consulting discovery, and the Classic transport fell back to it, so on
  any other machine FlexInput reported "no usable dongle" while a perfectly good
  adapter sat beside it. Adapters are now found by USB class; with two plugged in,
  both transports share whichever one is already open.

- **A switched-off Switch Pro froze connected Joy-Cons for two seconds in every
  twenty.** Paging a paired pad that wasn't there held the shared radio
  exclusively, so every other link on it went unserviced. The Classic transport
  now stands aside whenever the radio has carried traffic in the last half second.

- **Switch Pro sticks over Bluetooth Classic use the controller's own
  calibration.** They were normalised against a nominal range, so a stick
  saturated before reaching the diagonals (a square circularity plot), and a
  stick recalibrated on a Switch reached further one way than the other. The pad's
  factory and user calibration are now read over the link, with the user
  calibration taking precedence.

- **Retail Joy-Con 2 calibration was impossible, and every Joy-Con 2 inherited one
  grip's gyro offset.** Retail controllers interleave a 40-byte block that isn't
  motion data, and it was decoded as motion: on a pad lying still the
  accelerometer read 6–11 g every other frame, so stillness never registered and
  calibration, baseline capture and gyro-bias learning could never start. Only
  blocks of the expected length are decoded now, and an unreadable frame no longer
  counts as movement. Separately, the resting gyro offset measured on one Mobapad
  M12-S grip was applied to every Joy-Con 2 — up to 0.41 °/s of injected bias on
  any other unit. An uncalibrated controller now starts from zero.

- **3D orientation was scrambled on every pad except Joy-Con 2.** DualSense, DS4,
  Switch Pro and XInput pads take their orientation from the Gyro 3DOF module,
  which published it in the renderer's frame while the viewer converted as if it
  were canonical — so yaw drew as pitch, pitch as roll, and roll as yaw. Every
  orientation signal on a pin is now canonical, converted once by the renderer. A
  pinned 3D view, which applied no conversion at all, now agrees with its node.

- **Gamepad navigation no longer leaks through mapping Learn captures.** While
  Learn was armed on a Remapper, Map Action or 3DOF-to-2D Lean card, the chord
  being demonstrated still switched tabs, opened Alt+Tab or the preset list, ran
  undo/redo and moved the cursor. An armed capture now holds navigation off (the
  hold expires on its own if the capture goes stale), Touch Zones' gamepad learn
  uses the same hold, and Remapper's Stop disarms the capture.

### Security

- **The HIDMaestro driver is now signed on each machine, instead of trusting a
  certificate shared by every install.** FlexInput used to ship the driver's
  catalogs pre-signed by two self-signed build-machine certificates
  (`CN=HIDMaestroTestCert`, `CN=FlexInput HIDMaestro Driver`) and add both to
  `Root` and `TrustedPublisher` everywhere it installed — a single trust anchor
  common to all users, whose private key sat on whichever machine built the
  package. The elevated helper now creates a self-signed code-signing
  certificate per machine (`CN=FlexInput Local Driver Signing`) on a
  non-exportable RSA-3072 key, signs the driver binaries, builds the catalogs,
  and signs those before installing. A machine only ever trusts a key that was
  generated on it and can't leave it. Everything uses in-box Windows components;
  no SDK tooling ships (SignTool isn't redistributable, and Inf2Cat is WDK-only).
  The vendored `.cat` and `.cer` files are gone from the repository.
- **Existing installs are re-signed automatically, once.** On the first helper
  start after updating, an install still carrying the old signature is removed
  and reinstalled per-machine-signed — so virtual devices, including persisted
  ones, are recreated that one time — and the two legacy certificates are
  withdrawn from `Root` and `TrustedPublisher` by exact thumbprint. A legacy
  certificate whose private key is present on the machine is left in place. If
  re-signing keeps failing it stops after three attempts rather than recreating
  devices on every launch; **Reinstall drivers** re-signs on demand.
- **Driver staging moved out of the user's temp folder.** The package is now
  staged in a randomly named directory under `%windir%\Temp` that only
  Administrators and SYSTEM can write to, created with that ACL in place. Signing
  on the machine made the old location a local privilege escalation risk: an
  unelevated process could have swapped a driver binary before it was
  catalogued. `pnputil` and the staging path are also resolved through the OS
  rather than the `%SystemRoot%` environment variable.
- **Uninstall drivers now withdraws FlexInput's certificate trust too**, deleting
  the per-machine certificate and its key, where it previously left the signer
  certificates trusted.

## [0.13.5] - 2026-08-23

**Joy-Con 2 support — by way of an entire Bluetooth host stack of FlexInput's
own.** Switch 2 controllers are BLE devices that implement no standard profiles:
no HID-over-GATT, so Windows binds no driver and gilrs, SDL and hidapi never see
them at all. Everything is a vendor GATT service plus a command protocol. Worse,
Windows reclaims unpaired BLE links on a ~30 second timer that nothing available
to a GATT client prevents, and it cannot bond with these controllers either
(their legacy pairing uses a non-zero TK that WinRT has no way to supply). So
FlexInput now ships `crates/btle`: an independent Bluetooth host stack — LE
**and** Classic — driving a dedicated USB dongle over WinUSB, bypassing the OS
stack entirely. On that path a Joy-Con 2 holds a link indefinitely, and a Switch
Pro can be paired over Bluetooth Classic on the same radio at the same time.

### Added

- **Joy-Con 2 controllers** (`jc2:` devices), left and right halves, each a full
  FlexInput device with its own layout, calibration and 3D model. Three
  transports, all surfaced identically so only the connection differs:
  - **Dongle (preferred)** — FlexInput's own BLE stack over a WinUSB-bound USB
    dongle. Holds a link indefinitely at up to 200 Hz, connects **both** halves,
    and reaches the left half, which never enumerates over the USB charging grip
    at all. One thread owns the dongle and demultiplexes every link on it by
    connection handle. Override the device with `FLEXINPUT_JC2_DONGLE=vid:pid`.
  - **USB** — over the charging grip.
  - **Windows Bluetooth** — works, but subject to the ~30 s reclaim above.
- **FlexInput's own Bluetooth stack** (`crates/btle`) — HCI over WinUSB:
  - **LE**: scanning, `LE_Create_Connection`, ACL / L2CAP / ATT framing, MTU
    negotiation, and `HCI_LE_Enable_Encryption` from an out-of-band LTK with no
    SMP at all — the capability the crate exists for, and the one WinRT exposes
    no API for.
  - **BR/EDR (Bluetooth Classic)**: inquiry, paging, Secure Simple Pairing,
    L2CAP connection-oriented channels, and a keystore for the bonds.
  - **One radio, both transports.** A dual-mode adapter carries Classic and LE
    over a single HCI transport, but WinUSB grants an interface to one claimant —
    so there is now one reader, a router thread that broadcasts, and a lease that
    transports take for request/response conversations.
- **Bluetooth Classic gamepads on the dongle.** A Switch Pro paired to the dongle
  appears as an ordinary FlexInput device while Joy-Con 2 controllers stay
  connected on the same radio.
- **Bluetooth panel** — a Bluetooth button appears next to the logo *only* when a
  WinUSB-bound dongle is actually present. The panel lists adapters and paired
  controllers by manufacturer and model rather than by address, with a button to
  pair a new one. Adapters without the WinUSB driver are not listed at all, since
  there is nothing the app can do with them.
  - Adapter status says what it means: **active** when FlexInput is using it,
    **idle** when it is free, **held** when another program has it.
  - **Pairing no longer skips controllers it already knows** — the pad most in
    need of re-pairing is the one whose stored key has gone stale. Unpaired pads
    simply outrank paired ones.
  - The **key store location is a setting**, so pointing it at a cloud-synced
    folder carries the bond to every machine the dongle travels to.
- **Joy-Con 2 motion.** The accelerometer was located from hardware captures
  (i16 followed by two padding bytes on a 4-byte stride, 4096 LSB = 1 g) and is
  pinned by regression tests — the published layout for reports 0x07/0x08 is
  simply wrong over BLE, and the left half's report sits one byte earlier than the
  right's. Rotation comes from the controller's **own integrated motion**,
  differenced back into a rate; on hardware that is the difference between motion
  controls that are unusable and ones that can drive a cursor in fast circles.
  - Timing runs off the **device's own clock** carried in the report, not
    `Instant::now()` — BLE batches notifications, so host-side timing jitter was
    multiplicative: the harder the controller was swung, the more the rate
    zig-zagged.
  - Wrap discontinuities are rejected above 45° in a single report, while a
    full-scale 2000 °/s flick survives untouched.
  - Resting-drift compensation is anchored rather than averaged (a lagging EMA
    settles at a constant offset against sustained rotation, so it fought slow
    pans and then pulled them back), and its settle time is in seconds rather than
    samples, so it means the same thing at every polling rate.
  - Separate field and orientation gains, because yaw needs correcting for
    orientation tracking and not on the pin — one constant could only fix either
    by breaking the other.
- **Multi-device Easy mode.** A preset now accepts **one input device per AutoMap
  inlet** its sub-patch declares — so a multi-inlet preset takes several
  controllers, each keeping its own port, and a Remapper reading inlet 2 simply
  knows it is player 2. Devices are never merged onto one port, which would leak
  "which device did this come from" into every downstream curve and gate. The port
  is stored on the source node, so it survives save/reload and node reordering.
  Single-inlet presets behave exactly as before.
- **Orientation baseline calibration** for devices that report absolute
  orientation. A Joy-Con 2's published pose is measured against the world — yaw is
  magnetic north — which is not what a patch wants. Calibration captures the pose
  flat and removes it, so holding the device as it was when calibrated reads as
  identity. Uncalibrated devices pass their world pose through untouched.

### Changed

- **A pad on the dongle is now an ordinary device.** The backend prefix gate had
  grown a third backend and been updated for two, so Classic pads reached the
  canvas with no settings and no calibration — the same class of bug as the `sdl:`
  prefix before it, now covered by a test with one line per backend.
- Joy-Con 2 **pairing writes controller flash exactly once**, at first pairing.
  `0x03/0x09` is "Store Pairing Info" — a flash write into the same two-slot table
  that holds the console's entry — and it had been going out on every connect,
  including every button-press wake. Reconnects now re-send only `0x03/0x07`.
- The Joy-Con 2 link is requested at the **5 ms interval the console itself uses**,
  falling back to the 7.5 ms spec minimum if the dongle refuses. One input report
  arrives per connection event, so the interval *is* the report rate: 200 Hz
  instead of 66 Hz.

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
- **HidHide masked the wired Joy-Con from FlexInput itself.** Over USB the pad
  *does* get a HID node and arrives as an `sdl:` device, so it sailed past the
  `jc2:` prefix gate, got cloaked, and then opened and streamed nothing. It is
  excluded by controller kind now, since a prefix cannot express "this pad,
  whichever backend surfaced it".
- **Two 3D viewers of the same model drew the same pose.** The instance buffers
  were shared per model rather than per viewer, so both showed whichever pose was
  uploaded last and the second viewer looked broken rather than duplicated.
- **Easy mode's Calibrate opened whichever source node happened to be first**
  rather than the card it was pressed on.
- **Scanning starved an already-connected Joy-Con.** With one half connected, its
  poll rate cycled between ~67 Hz and zero every few seconds, settling only once
  the second half joined — `discover()` was a blocking 2 s scan window on the same
  thread that pumps ACL. Scanning is a state now: the loop keeps draining ACL and
  events throughout and stops the scan as soon as a controller is matched.
- **Both Joy-Con halves reported byte-identical frames.** A stale LE Connection
  Complete from the first controller was still queued when the second connect ran,
  so the second link was handed a handle that already belonged to the first — two
  links onto one stream, which looks entirely healthy until you notice the dumps
  match byte for byte. Pending events are drained before connecting, and a handle
  already in use is rejected and retried.
- **A failed connect left the dongle unable to scan.** A rejected
  `LE_Create_Connection` leaves the controller initiating, and while initiating it
  refuses `LE_Set_Scan_Enable` — so one refused parameter set poisoned every later
  scan. Scanning now cancels any pending initiator first, errors are reported
  instead of swallowed, and a refused connect bails on the Command Status rather
  than spending eighty seconds failing.
- **Bluetooth Classic: six separate faults, each enough on its own to stop a
  bonded controller ever reconnecting**, none of which reported anything useful.
  The accept asked to become master — a role switch this controller never
  completes, killing the link on LMP Response Timeout about ten seconds later,
  *after* authentication and encryption had both succeeded, which made it look
  like a device refusing to open its HID channels. Paging held the radio while the
  pad was calling in, and eager retries had it paging essentially 100% of the time.
  Abandoning a page host-side did not stop the radio paging. Either end may open
  the L2CAP HID channels and the same pad was observed doing it both ways minutes
  apart. `set_scan_enable` never checked its status byte, so a refusal read as
  success. And an event subscriber discarded every ACL packet it stepped over,
  eating the input reports of a controller that had connected perfectly.
- **A link key issued during connect was thrown away.** Re-pairing happens whenever
  the remote asks — which a controller in Sync mode always does — but only the
  pairing button stored the result, so a pad would pair perfectly and then never
  reconnect, its flash and our key file disagreeing. Keys also record the adapter
  that made them, since a key is worthless to any other dongle and the failure is
  otherwise completely silent.
- **The Classic transport deadlocked against itself on the first input report** —
  a non-reentrant mutex locked again inside the insert expression while the first
  guard was still alive — which then blocked `enumerate()` from the UI thread, so
  no device ever appeared either.
- **A truncated vendor HCI event aborted controller init outright.** Realtek
  dongles emit those constantly and this stack decodes none of them, so the dongle
  looked flaky while the link was fine. Truncation still fails for the five codes
  that *are* decoded, where inventing a handle would be worse.
- The WinRT hub's stand-down flag was attached after its worker started, leaving a
  window at each launch where it scanned with no flag — enough for Windows to claim
  a remembered controller. Standing down now also releases pads already held.
- **ACL packets were never put on air.** The packet-boundary flag was `0b10`
  ("first automatically-flushable"), which exists for BR/EDR; an LE-U link has no
  automatic flush and the host must send `0b00`. The dongle accepted the USB write
  and returned success either way, so the failure was silent in the worst possible
  way.
- **Input stayed in stub mode** — report counter incrementing at byte 0, every
  field past the header zero — until the vendor report-rate descriptor is written
  on the input characteristic, the step official software sends second-to-last
  during init.
- **A starved ACL read loop** consumed one packet per iteration while blocking up
  to 10 ms on it, capping the whole process near 90 packets a second shared between
  both links — measured as 60 Hz on one half against 6 Hz on the other. Draining is
  greedy now; two halves at a 5 ms interval need 400 packets a second.

### Internal

- `crates/btle` ships hardware bring-up and diagnostic binaries: `jc2_link`
  (connect, subscribe, init, then report throughput plus which byte offsets have
  ever changed), `jc2_imu` (a guided motion-sweep probe that identifies the
  accelerometer by physics — the one field triple whose vector magnitude holds at
  1 g through every orientation — then searches for gyro axes, with `--save` /
  `--load` so a decoding idea costs seconds against identical captured data
  instead of a fresh 45-second two-controller sweep), plus `bt_classic`,
  `hci_probe` and `jc2_crypt`.
- The gyro search is recorded as a **validated null result**: nothing in the report
  correlates with rotation rate — bytes 8–48, widths 10–21 bits, every bit offset,
  both halves, scored against the angular rate implied by the accelerometer, with
  the reference itself validated before any conclusion was drawn. The Joy-Con 2
  gyro is not a plain packed signed field, which is why the shipped implementation
  differentiates the controller's own integrated orientation instead. A ZYX
  Euler-rate-to-body-rate transform was also removed: 288 hypotheses (Euler in all
  six orders, the rotation vector, the vector part of a quaternion) were scored
  against measured gravity, and the best managed 40.0° against 49.2° for applying
  no rotation at all.
- The Joy-Con 2 attribute table is fully mapped (48 handles, properties read from
  the device), and the standard 0x30 accel/gyro block is parsed for controllers
  that populate it.
- btleplug 0.12 is vendored with a `GattSession.MaintainConnection` patch. It does
  not fix the ~30 s reclaim — nothing does — but it is correct, cheap, and it does
  make Windows re-establish a link it reclaimed.
- Diagnostic logs and IMU captures are gitignored, as is the Bluetooth link-key
  file: a link key is a shared secret between one controller and one dongle, so
  committing it would publish a credential that is useless to anyone else anyway.

## [0.13.3] - 2026-08-11

Three fixes for the same underlying trap: egui's `ctx.data()` and its layer→transform
map are shared by *every* window, while all of FlexInput's hosts (main canvas,
sub-patch editor, and the three overlays) report the same background layer id. Any
state keyed only by node id therefore collided as soon as one node was visible in two
places at once — and the host that painted last won. Plus a D-pad output fix, an
idle-virtual-mouse fix, and an optional sticky HidHide mask.

### Added

- **"Keep hiding through disconnects"** (`hidhide_sticky`, off by default) — the
  HidHide blacklist is rebuilt from the devices enumerated right now, so whenever
  a remapped pad blipped out of enumeration (a Bluetooth gap, or a reconnect that
  arrived already blacklisted and never reached our device list) the target set
  went empty and the pad was **unhidden system-wide** until it came back — windows
  of 3 to 94 seconds in the logs, during which a running game is handed the
  physical pad alongside the virtual one. With the setting on, the mask is kept
  for any pad the patch still wires up, and applied *at* arrival rather than ~2 s
  later. Unmapping the device, turning off "hide originals", or exiting all unhide
  as before.

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
- **An idle virtual mouse wrote a zero-delta HID report every tick**, at up to
  1 kHz, for as long as the app was open — the I/O thread calls `reset_outputs()`
  on a bypassed device every tick, and that armed the dirty flag unconditionally.
  Games that pick their glyph set from "which device reported last" then flickered
  between controller and keyboard prompts while the sticks moved, and every flip
  re-ran the game's cursor lock, warping the pointer to screen centre in menus. The
  flag is a true one-shot now: set once at creation so a reclaimed node still gets
  exactly one neutral frame, and never re-armed.

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
