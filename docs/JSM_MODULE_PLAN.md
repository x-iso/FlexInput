# JSM Config module — plan

Branch `feat/jsm-config`. A module whose body is a text editor holding a
JoyShockMapper config, applying it to the AutoMap bus with JSM's own rules.

Spec source: the official repo <https://github.com/Electronicks/JoyShockMapper>
(JibbSmart points to it), 3.6.2 as of Oct 2025. Check names against its README,
`include/JoyShockMapper.h` enums and `src/Mapping.cpp` binding regex — not
memory. GitHub fetches are blocked in this environment; `git clone --depth 1`
into `C:/tmp` works.

## Goal and non-goals

**Goal:** a JSM user pastes or loads their existing config, it works, and they
keep editing it in the module. Feeling at home matters more than matching the
rest of FlexInput.

**Non-goals:**
- Driving existing modules. This module applies the config itself. Where its
  behaviour diverges from the Remapper, that is the point: it is an alternative
  way to configure, not a second front-end for one engine.
- Gamepad configuration. Nav reaches the pinned editor and focuses it so a
  keyboard can type; nothing more.
- JSM's app-level plumbing (see *Ignored* below).

## Locked decisions

- **Text is the only source of truth.** Save writes back what was loaded,
  comments and formatting included.
- **Tabs.** The module holds several named config files as tabs, each with its
  own Load / Save. A quoted file name in a binding resolves to a tab, which is
  how JSM configs do action layers.
- **Tabs are named by file name alone.** Loading a file names the tab after it,
  dropping the directory and the `.txt`. A binding that loads
  `GyroConfigs/vehicle.txt`, `Autoload/GTA5/vehicle.txt` or `vehicle.txt` all
  resolve to the tab `vehicle`. So a set of related configs can be imported from
  wherever they sit, and from then on they travel inside the patch / sub-patch
  preset. A name that resolves to no tab is an error on that line, with the
  missing name shown.
- **Header toggle: pass through vs strict.** Pass through leaves inputs the
  config never mentions on the bus; strict emits only what the config produces
  (JSM's own behaviour, where the pad is hidden from the game).
- **Unsupported lines stay visible.** Every line gets a parse status in the
  editor; nothing is silently dropped.
- **Rumble, light bar and adaptive triggers are supported**, through the
  feedback override layers added in `bcac2b4` (see AUTOMAP_SYSTEM.md →
  *Feedback Layers*).

## Node shape

- `module.jsm`, display name "JSM Config", category "AutoMap".
- Input 0 `Device` (AutoMap). Output 0 `AutoMap` — republishes the bus as
  `jsm:{uid}`, so it must join `republishes_automap_bus` in
  `crates/ui/src/module_ui_info.rs` and the graph builder's automap-source stop
  set. Pattern to copy: the Remapper's `remap:{uid}` publish plus
  `remapper_pass_through_and_suppress` for what it claims.
- Params: `jsm_tabs: Array<{name, text}>`, `jsm_active_tab: u32`,
  `jsm_strict: bool`, plus `_automap_device_id` / `_automap_collector_id` and a
  `_jsm_dest_dev` for feedback (stamped like ASTH's `_asth_dest_dev` in
  `crates/ui/src/app/graph.rs`).
- Engine code in `crates/engine/src/eval/modules/jsm/`: `parse.rs`, `bind.rs`
  (button state machines), `sticks.rs`, `gyro.rs`, `touch.rs`, `feedback.rs`.
  Registered through the registry seam (`EvalHooks { publish }`,
  `crates/engine/src/eval/registry.rs`) behind a default-on `jsm` cargo feature,
  following the ASTH feature-forwarding chain (app → ui → modules).
- Runtime state: `NodeState.jsm: Option<Box<JsmState>>` — per-button state
  machines, stick/gyro/flick state, active modeshifts, the compiled config and
  the text generation it was compiled from (recompile only when the text
  changes).

## Name tables

**Inputs** (JSM → AutoMap pin). JSM uses Nintendo positions with cardinal face
buttons:

| JSM | pin | JSM | pin |
|---|---|---|---|
| `N` `E` `S` `W` | `btn_north` `btn_east` `btn_south` `btn_west` | `UP` `DOWN` `LEFT` `RIGHT` | `dpad_*` |
| `L` `R` | `btn_lb` `btn_rb` | `ZL` `ZR` | `left_trigger` / `right_trigger` (+ `btn_lt_dig` / `btn_rt_dig`) |
| `ZLF` `ZRF` | trigger full pull (derived) | `-` `+` | `btn_back` `btn_start` |
| `HOME` | `btn_guide` | `CAPTURE` | `btn_touchpad` / `btn_capture` |
| `L3` `R3` | `btn_ls` `btn_rs` | `LSL` `LSR` `RSL` `RSR` | `btn_paddle_l1/l2/r1/r2` |
| `MIC` | `btn_mute` | `TOUCH` | `touch1_active` / `touch2_active` |
| `LUP…LRING` `RUP…RRING` | derived from `left_stick` / `right_stick` | `MUP…MRING`, `LEAN_LEFT/RIGHT` | derived from `gyro_*` + `accel_*` |
| `T1…T25`, `TUP…TRING` | derived from `touch1_*` / `touch2_*` | | |

Inputs a pad lacks simply never fire, with a note on the line.

**Outputs.** Keyboard and mouse go to the KB/M sink vocabulary: `key_<name>`
(the `key_name_to_hid_usage` universe in `crates/virtual/src/keymouse_hm.rs`),
`mouse_left/right/middle/back/forward`, `scroll_up/down`, `mouse_move_x/y` for
pointer motion. Gaps to fill there, or to report as unsupported:
- **Numpad** (`N0`–`N9`, `ADD`, `SUBTRACT`, `DIVIDE`, `MULTIPLY`, `DECIMAL`) —
  no HID usages yet; `num0`–`num9` currently map to the digit ROW.
- **Media / volume** (`VOLUME_UP`, `MUTE`, `NEXT_TRACK`, `PLAY_PAUSE`, …) — not
  in the table (they are consumer-page usages, not keyboard-page).
- **Left/right modifier variants** (`LSHIFT` vs `RSHIFT`, same for Ctrl/Alt) —
  only generic `key_shift` / `key_ctrl` / `key_alt` exist. Map both onto the
  generic pin and say so once per config.

Virtual pad outputs (`X_A`, `PS_CROSS`, … — aliases of each other in JSM) map to
the canonical pad pins (`btn_south`, `btn_lb`, `left_trigger`, `left_stick`, …).
The user wires whatever virtual pad they want downstream; `VIRTUAL_CONTROLLER`
itself is ignored, with a hint when a config's outputs need a pad and none is
downstream.

## Semantics to implement

**Bindings.** One combo operator per line — JSM's own regex allows exactly one:
`A,B` chorded press, `A+B` simultaneous press, `A*B` diagonal press, `A,A`
double press. Plus:
- tap & hold (`W = R E`), `NONE` as a deliberate no-op;
- action modifiers `^` toggle, `!` instant, `-` release;
- event modifiers `\` start, `/` release, `'` tap, `_` hold, `+` turbo;
  defaults: first key of several = tap, second = hold, a third needs an explicit
  modifier; `/` requires an action modifier;
- a quoted console command as a binding, instant by default — including loading
  another config (a tab);
- gyro actions as bindings (`GYRO_OFF`, `GYRO_ON`, `GYRO_INVERT`,
  `GYRO_TRACKBALL`, …) and `CALIBRATE`, which a tap holds for 0.5 s;
- `SMALL_RUMBLE`, `BIG_RUMBLE`, `Rhhhh` — a button that rumbles the pad.

**Timing defaults:** hold 150 ms, double press 150 ms, simultaneous press 50 ms,
turbo period 80 ms, tap output 40 ms (500 ms for gyro actions and calibrate),
trigger skip delay 150 ms. All settable, except `SIM_PRESS_WINDOW` which JSM
does not allow a modeshift to change.

**Chords and modeshifts.** Chords stack: with several chord buttons held the
latest wins. A modeshift is active while its button is down even with nothing
bound. Nearly every setting can be chorded (`ZLF,GYRO_SENS = 0.5`); the
exceptions are `AUTOLOAD`, `JSM_DIRECTORY`, `SIM_PRESS_WINDOW`, `TICK_TIME`,
`GRID_SIZE`, `HIDE_MINIMIZED`, `VIRTUAL_CONTROLLER`. `NONE` clears a modeshift,
`NONE\` means "the gyro button is none". On leaving a stick-mode modeshift the
stick is ignored until it recenters.

**Sticks.** Modes `NO_MOUSE` (cross-gate directions), `AIM`, `FLICK`,
`FLICK_ONLY`, `ROTATE_ONLY`, `MOUSE_RING`, `MOUSE_AREA`, `SCROLL_WHEEL`,
`HYBRID_AIM`, plus the virtual-pad ones (`LEFT_STICK`, `RIGHT_STICK`,
`*_ANGLE_TO_X/Y`, `*_WIND_X`, `*_STEER_X`), ring modes inner/outer, per-stick
and shared deadzones, `STICK_SENS` / `STICK_POWER` / acceleration, axis
inversion, `CONTROLLER_ORIENTATION`.

**Triggers.** `TRIGGER_THRESHOLD` (with `-1` = hair trigger), full-pull bindings
and the modes `NO_FULL`, `NO_SKIP`, `NO_SKIP_EXCLUSIVE`, `MUST_SKIP`,
`MAY_SKIP`, and the responsive `*_R` variants; `X_LT` / `PS_L2` to drive a
virtual pad's analog trigger.

**Gyro.** `GYRO_SENS` with min/max sensitivity and thresholds, `GYRO_SPACE`
(local, player turn/lean, world turn/lean — the same spaces Gyro 3DOF has),
axis inversion and `MOUSE_X/Y_FROM_GYRO_AXIS`, smoothing, cutoff, trackball
decay, `GYRO_OUTPUT` (mouse, a stick, or `PS_MOTION`) with the undeadzone /
unpower / virtual-scale counters. `REAL_WORLD_CALIBRATION` and `IN_GAME_SENS`
convert to RWS-style counts per degree; output goes to `mouse_move`, which
bypasses the KB/M card's `mouse_sensitivity`, so a shared config stays portable.
Flick stick reuses the RWS flick math (`compute_rws`,
`crates/engine/src/eval/modules/rws.rs`); `FLICK_STICK_OUTPUT` can send it to a
stick instead.

**Touchpad and motion stick.** `TOUCHPAD_MODE` grid-and-stick or mouse,
`GRID_SIZE` up to 25 cells, touch sticks with their own radius and deadzone,
`TOUCHPAD_DUAL_STAGE_MODE` (touch and click as a two-stage trigger), motion
stick modes and deadzones in degrees, `LEAN_THRESHOLD`,
`SET_MOTION_STICK_NEUTRAL`.

**Feedback.** `RUMBLE` (default on) passes the game's rumble; off silences it.
`LIGHT_BAR` takes `xRRGGBB`, three 0–255 values or a colour name.
`ADAPTIVE_TRIGGER` (default on) has JSM shape trigger resistance from the
trigger modes, and `LEFT/RIGHT_TRIGGER_EFFECT` takes `OFF`, `ON` or one of
`RESISTANCE`, `BOW`, `GALLOPING`, `SEMI_AUTOMATIC`, `AUTOMATIC`, `MACHINE` with
their values. All of this writes `feedback_override:{target}` so it replaces
what the game asks for, per kind. Our trigger pins currently describe four
effects (off, resistance, click, vibration) with start / end / strength /
frequency: `RESISTANCE`, `SEMI_AUTOMATIC` and `AUTOMATIC` fit; `BOW`,
`GALLOPING` and `MACHINE` need new effect values on the pins and in the
DualSense encoding. Per-controller `LEFT/RIGHT_TRIGGER_OFFSET/RANGE` are
accepted and applied to the resistance positions.

## Pass-through vs strict

In strict mode only what the config produces leaves the module. In pass-through
mode an input is taken over when the config mentions it anywhere: bound, part of
a chord / simultaneous / diagonal / double press, used as a modeshift button, or
a gyro on/off button. Also:
- a stick is taken over when its mode is set or any of its directions or ring is
  bound; otherwise it passes through as an analog stick;
- a trigger is taken over when it, its full pull, or its mode is set;
- gyro and accelerometer pins always pass through, so a virtual DualShock or
  DualSense downstream still gets motion.

Someone who wants a mentioned button in the game too binds it to the pad output
(`L = X_LB`), which is plain JSM.

## Ignored, with a visible note

App-level and hardware-calibration commands: `AUTOLOAD`, `AUTOCONNECT`,
`WHITELIST_*`, `JSM_DIRECTORY`, `TICK_TIME`, `HIDE_MINIMIZED`, `SLEEP`,
`README`, `HELP`, `CLEAR`, `QUIT`, `RECONNECT_CONTROLLERS` (and `MERGE` /
`SPLIT`), `RESTART_GYRO_CALIBRATION` / `FINISH_GYRO_CALIBRATION` /
`AUTO_CALIBRATE_GYRO` / `CALIBRATE_TRIGGERS`, `COUNTER_OS_MOUSE_SPEED` /
`IGNORE_OS_MOUSE_SPEED`, `VIRTUAL_CONTROLLER`, `JOYCON_GYRO_MASK` /
`JOYCON_MOTION_MASK`. FlexInput owns these through the device card, HidHide and
its own profiles. `RESET_MAPPINGS` and `CALCULATE_REAL_WORLD_CALIBRATION` are
worth honouring inside the module.

## Editor and diagnostics

Body: a tab bar (add / rename / remove, Load and Save per tab) over a monospace
text editor. Header: the pass-through / strict toggle. The editor is a pinnable
element so it can go on the config overlay, and nav can focus it for typing
(`pinned.rs` / `expose.rs`, and the rules in `memory/pinned_widget_scaling.md`).

Each line is tinted by what the parser made of it: an error, an accepted line,
or accepted-but-ignored-here (with the reason on hover — "the device card owns
gyro calibration", "needs a virtual pad downstream"). A per-config summary
counts the ignored lines. Re-parse as the text changes but keep the last valid
compiled config running, so a half-typed line never breaks the mapping. While
the editor has keyboard focus the module's own keyboard output pauses, so a
binding under test can't type into it.

## Phases

**Phase 1 landed** (engine `eval/modules/jsm/`, UI `canvas/viewer/jsm.rs`): the
node and its cargo feature, tabs with Load / Save, the editor with per-line
statuses and a pinnable editor for the config overlay, the name tables, the
press machinery, JSM's timings, and the pass-through / strict toggle. Accepted
against JoyShockMapper's own `GyroConfigs/xbox.txt` and `Desktop.txt`, which load
without errors and play. The registrations a module needs are listed under
*Wiring a new module* in DEVELOPMENT_GUIDELINES.md — the body gate and the
AutoMap stamping were both missed on the first pass here.

**Phase 2 landed** (engine `eval/modules/jsm/analog.rs`): the trigger threshold
and JSM's hair trigger, the dual-stage full pull with every skip mode and
`TRIGGER_SKIP_DELAY`, and digital sticks — `NO_MOUSE` directions in JSM's eight
sectors, `SCROLL_WHEEL` notches, ring modes, deadzones, axis inversion and
`CONTROLLER_ORIENTATION`. Ported from JSM's own `processTriggerPress` /
`processStick` state machines rather than from the README. Accepted end to end:
`Desktop.txt`'s scroll wheel turns a stick on the bus into wheel notches.

Three deliberate departures, all visible in the editor:

- **A resting trigger has to clear a little noise.** JSM's default threshold is 0
  — "the slightest press" — which on a pad whose trigger rests a hair above zero
  would sit on permanently. A threshold of 0 means 0.02 here; anything the config
  sets above that is used as written.
- **Only what runs is taken over.** A stick left in a mouse or pad mode, and a
  full pull the trigger mode never fires, keep passing through instead of going
  quiet for a binding that can't run. JSM has no equivalent choice to make: its
  pad is hidden from the game.
- **The editor says why a binding is inert.** A stick direction bound while that
  stick aims the mouse, or `ZLF` with `ZL_MODE` left at `NO_FULL`, gets a note on
  the line. JSM is silent about both, and it is the first thing that confuses
  someone whose config "doesn't work".

`CONTROLLER_ORIENTATION` turns the sticks now and the gyro when phase 3 arrives;
`JOYCON_SIDEWAYS` is ignored, since FlexInput treats each Joy-Con as its own
device.

**Phase 3 landed** (engine `eval/modules/jsm/aim.rs`): the gyro as a mouse —
`GYRO_SENS` and the `MIN`/`MAX` ramp, `REAL_WORLD_CALIBRATION` / `IN_GAME_SENS`,
the axis masks and signs, smoothing, the cutoff band, the trackball, and the
`GYRO_ON` / `GYRO_OFF` buttons (button or stick) alongside the gyro action
bindings — plus stick `AIM` with its power curve and acceleration, flick stick
(`FLICK`, `FLICK_ONLY`, `ROTATE_ONLY`, snapping, the eased pay-out and the
rotation smoother) and `MOUSE_AREA`. Ported from JSM's own IMU callback and
`handleFlickStick`. Output goes to the bus's `mouse_move`, a per-tick pixel
displacement — the same thing JSM computes and hands to `moveMouse`.

**The axis convention is the one thing here that wants a pad to confirm it.** JSM
names gyro axes in its own frame; ours are different, and two of the three are
pinned by what JSM's *defaults* must do (turn right → aim right, tilt up → aim
up), which gives JSM's Y = `-gyro_z` and JSM's X = `+gyro_y`. Nothing in JSM's
defaults uses its Z (roll), so its sign can't be derived the same way: it is
mapped to `+gyro_x`, and a config that puts `Z` in a mouse axis mask may want
`GYRO_AXIS_X` / `GYRO_AXIS_Y` inverted. The reasoning is written out at the top
of `aim.rs`.

Deferred, each with the reason on the line in the editor:

- **Gravity-referenced gyro spaces** (`PLAYER_TURN`, `PLAYER_LEAN`, `WORLD_TURN`,
  `WORLD_LEAN`). They need a gravity vector in JSM's frame, and ours differs (see
  [[imu-canonical-frame]]: accel and gyro sit in bases differing by
  `diag(1,−1,−1)`). Getting that wrong is invisible to a test and wrong on
  hardware, so it lands with the motion stick in phase 6, which needs the same
  work.
- **`MOUSE_RING`** and `SCREEN_RESOLUTION_*`: they place the pointer outright, and
  the bus has no absolute mouse pin — only displacement.
- **`HYBRID_AIM`** and its settings cluster (`STICKLIKE_FACTOR`,
  `MOUSELIKE_FACTOR`, the return-deadzone and edge-push settings).
- **`FLICK_STICK_OUTPUT` / `VIRTUAL_STICK_CALIBRATION`**: flick to a virtual stick
  rather than the mouse belongs with pad output in phase 5.

`CALCULATE_REAL_WORLD_CALIBRATION` is ignored with a pointer to the RWS Aim
module, which is how FlexInput measures real-world sensitivity; `CALIBRATE` as a
binding runs but recalibrates nothing, because the device card owns that.

**Phase 4 landed** (`parse::resolve`, and the runtime seams it needed): any
setting this module runs can be chorded — `ZL,GYRO_SENS = 4` reads 4 while ZL is
held and the config's own value the rest of the time. One code path parses a
value whether it comes from a plain line or a modeshift, so a modeshift is
checked when the config compiles: a bad value is an error on its line, and a
setting a later phase owns waits with that setting rather than with modeshifts.
Resolution is JSM's: walk the chords held, latest first, and take the first that
has something to say about that setting — so two chords over one setting go to
whichever went down last, while each still governs the settings the other doesn't
touch. Both stick rules came with it:

- **A stick waits for centre.** When a chord that was supplying a stick's mode is
  released while the stick is still pushed, the stick does nothing at all until it
  reads centred — otherwise the mode underneath is handed a stick already out at
  full, and a flick stick would read that as a fresh flick.
- **A flick can't be cut off mid-turn.** While a flick is still paying out, the
  stick stays in flick mode whatever the mode underneath now says.

One departure: JSM decides "a flick is unfinished" by `flick_percent_done < 1`,
which is also true for a stick that has never flicked at all (it starts at zero),
so in stock JSM the check appears to force `FLICK_ONLY` on a stick in any other
mode. A flick being underway is tracked outright here instead.

The settings in force are resolved from the chord stack as the press machinery
left it at the end of the previous tick, so a modeshift takes hold one tick after
its button. That keeps the order inside a tick simple — settings, then buttons,
then aiming — and one tick is imperceptible at any rate the engine runs.

**Phase 5 landed** (`jsm/pad.rs`, and the seams in `aim.rs` it needed): a config
can drive a virtual pad rather than the keyboard and mouse. With it, **JSM's own
shipped `Xbox.txt` runs end to end** — both triggers passed through, both sticks
driving the pad's own — and a test pins that, so a future phase cannot quietly
regress it.

Everything on this side works in the unit JSM works in: degrees per second of
in-game camera turn. A stick push and a gyro rate are both converted into that
rate, added, and only then converted back into a stick position, which is what
lets a gyro and a stick share one virtual stick without either clipping the other.
`VIRTUAL_STICK_CALIBRATION` is the rate a fully deflected stick produces in the
game being played, so it scales the whole conversion.

Four things worth knowing before touching this code again:

- **A virtual stick goes on the bus in all three forms** — the `Vec2` and both
  floats — for exactly the reason the D-pad did. A sink prefers the vector, so a
  disagreeing pair of floats is at best ignored and at worst wins. There is a test
  for a stick pushed *up*, separately from one pushed right: the conversion passes
  through camera space where y counts down, and a broken round trip inverts every
  virtual stick vertically while a sideways-only test stays green.
- **`GYRO_OUTPUT` pointed at a stick silences the whole mouse**, not just the
  gyro's share of it, so an `AIM` stick stops moving the pointer too. That is
  JSM's own gating (`gyroOutput == MOUSE` guards the entire `moveMouse` call) and
  it looks far more like an oversight than a design — but a config was tuned on
  it, so it is reproduced, and the editor says so on the `GYRO_OUTPUT` line rather
  than leaving it to be discovered.
- **`X_LT` / `X_RT` split a trigger in two.** Its pull goes straight to the pad
  and its own bindings stop running, but it still stacks a chord — JSM returns
  early out of the trigger state machine and poked the chord stack by hand, so
  `Runtime::tick` grew a `chord_only` predicate that does the same thing in the
  same place. The chord rule is looser than the state machine's, too: any pull off
  the rest is the soft press and only the end of travel is the full one, because
  there is no state machine left to ask.
- **`*_UNDEADZONE_*` is for the gyro, not for a stick.** It runs the *game's*
  deadzone backwards so a small contribution starts above it instead of at zero.
  For a stick alone it is arithmetically a no-op — the stick's own length is
  divided out and multiplied straight back in — which is right, since a stick
  already reaches the whole range and it is the gyro's nudges a game's deadzone
  swallows.

Two departures, both deliberate:

- **`LEFT_STEER_X` / `RIGHT_STEER_X` are an error on a thumbstick**, not pending.
  JSM refuses them there (they are `MOTION_STICK_MODE`'s alone) and warns; no
  later phase will make them work on a thumbstick, so calling them pending would
  be a promise we would never keep. The message names where they do work and what
  to use instead. This is what `Parsed::Refused` exists for.
- **`GYRO_OUTPUT = PS_MOTION` is implemented by doing nothing**, which is exactly
  right here: it means "don't aim with the gyro, let the pad's own motion reach
  the pad", and the pass-through already does that — so the gyro pins are simply
  left unclaimed. The line says as much.

One faithful port that reads like a bug and isn't: a stick driving a virtual stick
uses the **previous** tick's direction with the **current** tick's length, because
JSM deadzones `lastX`/`lastY` in place and hands those to `processGyroStick`. It is
one tick of lag on direction only, imperceptible at any rate the engine runs, and
there is a mutation-checked test so nobody "fixes" it by accident.

A flick bound for a stick is a wholly different sum from a flick bound for the
mouse, not a scaled one: a mouse can be moved any distance in a tick, so JSM eases
the turn out over `FLICK_TIME`, while a stick can only be pushed so far — so there
the turn becomes "hold it right over at `VIRTUAL_STICK_CALIBRATION` deg/s for
however long the angle takes", and `FLICK_TIME` has nothing to say at all.

**Phase 6 landed** (`jsm/motion.rs` and `jsm/touch.rs`): the touchpad, the motion
stick, the lean buttons, and the gyro spaces measured against gravity. **There is
now no button in JSM's vocabulary this module cannot read** — a test walks every
one of them and asserts as much, so the day a new input source appears it will be
a deliberate change rather than a silent regression.

### Which way round the axes go, properly this time

This phase is the first thing in the module to **combine accelerometer and gyro
readings**, and on our bus the two are not in the same basis. Against the pad's
body axes — F forward, R the player's right, U out of the face — our accel
components are in `(F, -R, U)` and our gyro components are in `(F, R, -U)`. Both
are right-handed, so nothing looks wrong until something combines them, which is
exactly what a gravity-referenced gyro space does. See [[imu-canonical-frame]];
the same mismatch is a known, deliberately unfixed bug in Gyro 3DOF's
Player/World modes, and this module does **not** inherit it: both readings are
converted into JSM's own frame up front and the maths is done there.

| JSM | from our accel | from our gyro |
| --- | --- | --- |
| X (right / pitch) | `accel_y` | `gyro_y` |
| Y (up / yaw) | `-accel_z` | `-gyro_z` |
| Z (forward / roll) | `-accel_x` | `gyro_x` |

The gyro column is the mapping phase 3 derived independently, from what JSM's
*defaults* have to do. Two derivations from different evidence meeting is the
closest thing to reassurance available without hardware, and the accel column is
pinned pose by pose by `gravity_lands_in_jsms_frame` — flat, nose up, right grip
down — so a sign error fails a test instead of shipping.

### Gravity, and the four things measured against it

JSM asks its motion library for a fused gravity estimate; our bus carries raw
sensors, so gravity here is a low-passed accelerometer direction, the way Gyro
3DOF already does it. Gravity is the only *sustained* acceleration a hand-held pad
sees, so the filter settles onto "down" while a shake averages out — and the lag
is the point, since the question is "how is this being held?", not "what is it
doing this instant?". `GRAVITY_TAU` is 0.5 s. A pad reporting no accelerometer at
all reads as *don't know*, not as *flat*: the difference between a motion stick
that rests and one that runs to an edge.

- **The motion stick** is how far gravity has swung from straight down, with a
  half-turn reaching full deflection. `MOTION_DEADZONE_INNER` / `_OUTER` are in
  **degrees of tilt** in JSM (15 and 135) against that 180° range, so they are
  stored divided by 180, in the units every other deadzone here uses.
- **The lean buttons** fire past `LEAN_THRESHOLD` degrees of side tilt, with
  `CONTROLLER_ORIENTATION` picking which body axis counts as "to the side".
- **`LEFT_STEER_X` / `RIGHT_STEER_X`** are the motion stick's alone, which is why
  phase 5 made them an error on a thumbstick. Past vertical the lean angle folds
  back, so leaning further keeps steering the same way rather than unwinding.
- **The gravity gyro spaces** work out which way "turning" or "leaning" points
  given where down is, and take the gyro's component along it. All four fade out
  as the pad rolls onto its side, where a pitch axis derived from gravity means
  almost nothing — and that fade needs testing at the *edge* of its band, where
  the axis is still computable but worthless. A test that only checks a pad exactly
  on its side passes with the fade removed, because the axis degenerates and reads
  zero anyway; that was a real mutation survivor here.

### The touchpad

`TOUCHPAD_MODE` is a grid of buttons plus a relative stick per finger, a mouse, or
a pass-through to a virtual pad.

- **Units.** `TOUCH_STICK_RADIUS` (300) and `TOUCHPAD_SENS` are in touchpad
  *points*, and JSM's defaults were chosen against a DS4 / DualSense pad reporting
  1920 × 1080. Our bus normalises a finger to -1..1, so the two are bridged by that
  same nominal size and a config carrying JSM's defaults feels as it did. A
  differently sized touchpad is scaled to the same -1..1, so a sweep across it is a
  sweep across this one — the honest choice, since nothing on the bus says how many
  millimetres that was.
- **A touch stick is relative**: it measures the drag from wherever the finger
  landed, not where on the pad it is, and lifting re-centres it. The tick a finger
  lands counts as no movement at all, or a new touch would jump by however far it
  is from where the last one ended.
- **A grid boundary belongs to the cell before it** (JSM rounds up and subtracts
  one). The obvious `floor` gives the other answer and differs *only* exactly on
  the line, so there is a test sitting on it.
- **`GRID_SIZE` past 25 cells is an error, not a clamp**: the buttons stop at
  `T25`, so a 6×6 grid would have cells nothing could ever be bound to.
- **The touchpad is JSM's third trigger.** A finger down is the soft pull and a
  click is the full one, run through the same dual-stage machine under
  `TOUCHPAD_DUAL_STAGE_MODE`. JSM feeds it 0.99 for a finger and 1.0 for a click,
  and that 0.99 is load-bearing: it is what lets a skip mode tell a tap from a
  press, and what stops a resting finger from ever reading as a click.

One shared consequence: lifting a finger and releasing the click in the same tick
releases the full stage first and the soft one a tick later, because
`DelayFullPress` holds the soft press while the full one goes. That is the
dual-stage machine's own shape, shared with every real trigger, so it is left
alone rather than special-cased for the touchpad.

Both touch sticks drive one set of `TUP`…`TRING` names, as in JSM, so two fingers
pushing the same way is the same as one. All five of JSM's sticks — left, right,
motion, and one per finger — now run through the one `Analog::stick` routine, which
is what makes `MUP` behave exactly as `LUP` does and what let the motion and touch
sticks inherit every stick mode, virtual-pad output included, for free.

**Phase 7 landed** (`jsm/feedback.rs`): rumble, the light bar and the adaptive
triggers — the only things this module sends *backwards*. They go out as
`feedback_override:{pad}`, the layer that takes a kind of feedback over from the
game, which is what JSM does while it owns the pad. The destination is the physical
pad upstream, stamped as `_jsm_dest_dev` by the graph builder exactly the way Audio
Stream Haptics' is; with nothing resolved, nothing is published rather than a stray
override nobody drains.

**`RUMBLE` decides who owns the group, and that needed care.** With it on (JSM's
default) the rumble group is left alone so the game keeps it — claiming it and
writing zero would silence a game that was rumbling perfectly well. With it off, or
while a binding is rumbling, the group *is* claimed and a definitive value written
every tick, zero included. That is the same rule the output pins follow: a latched
amplitude buzzes for ever.

### The adaptive triggers do not line up, and the editor says so

JSM carries the DualSense's full vocabulary — seven usable modes, up to six
parameters. Our bus carries the four the DualSense encoder implements. Four of
JSM's seven land exactly:

| JSM | ours | JSM's parameters |
| --- | --- | --- |
| `ON` (default) | *nothing* — left as the game set it | — |
| `OFF` | off | — |
| `RESISTANCE` | feedback | start, force |
| `SEMI_AUTOMATIC` | weapon | start, end, force |
| `AUTOMATIC` | vibration | start, force, frequency |

`BOW`, `GALLOPING` and `MACHINE` need two forces or a second frequency, and there
is nowhere on the bus to put them. A config asking for one **is not an error** —
JSM knows the name and so do we — but the line says what will actually happen and
names the nearest thing that works, and the trigger is left as the game set it
rather than quietly given something that feels wrong. Extending the bus to carry
them touches the pin list, the DualSense encoder and every feedback-producing
module's vocabulary; that belongs on its own, not smuggled in here.

Units are JSM's own: zones 0-9, force 0-7, frequency 0-255. Our pins are all
`Float` 0..1 with the device layer scaling each back, so every value is divided by
its JSM maximum on the way out and a config's numbers mean what they meant.

Two details worth keeping:

- **`ADAPTIVE_TRIGGER = OFF` overrules whatever effect is set** — one switch to
  stop the triggers fighting you, which is how JSM uses it.
- **`#RRGGBB` can never be a light-bar colour**, because `#` starts a comment and
  takes the value with it. The parser used to accept it, which was dead code; now
  the error says why, so it reads as the config grammar rather than a parser bug.

`LEFT_TRIGGER_OFFSET` / `_RANGE` are ignored with a reason: JSM writes them from
its own trigger-calibration routine in the DualSense's raw travel, and our effect
zones are fractions of the trigger already — so there is nothing to declare, and
calibration is the device card's job here.

**Phase 8 landed**: action layers. A JSM config does layers by *loading another
config file*, and this module holds each config as a tab, so a quoted file name
resolves to the tab of that name — path and `.txt` dropped, matched
case-insensitively. Both forms work:

- **Bound to a button** (`HOME = "driving.txt"`) it switches which tab is running.
  The tab the editor has open is where a config starts, and where `RESET_MAPPINGS`
  or any edit sends it back to; a switch is not always visible on screen, which is
  the one place this model departs in *feel* from JSM's (there, the console tells
  you). The switch happens **after** everything else in the tick, so the layer being
  left gets to finish — including releasing what it drove.
- **On its own line** (`defaults.txt`) it applies everything in that tab right
  there, as JSM loading the file mid-config would. One level only: a config named
  *inside* an included tab is not followed, and the line says so. Two configs naming
  each other therefore cannot loop, and there is a test that they don't.

A tab that isn't there is an error naming the tabs that are — the most useful thing
it can say, since the fix is always "load it into a tab" or "you meant one of
these". Only a name with a `.txt` suffix or a path separator is taken for a config
at all; a bare word is far likelier a mistyped command, and calling it a missing tab
would bury the real mistake.

`RESET_MAPPINGS` works both ways too: bound to a button it goes back to the open
tab, and on its own line it discards everything above it, as JSM does. At the very
top of a file — where a JSM config almost always puts it, to clear a *previous*
config — there is nothing to discard, because each tab compiles on its own. The line
says exactly that rather than sitting there looking effective.

**Two real bugs came out of building this**, both found by reasoning about what a
layer switch has to do:

1. **A rebound button was using the FIRST binding, not the last.** A binding is an
   assignment (`mappings[button] = value` in JSM), so `S = A` followed by `S = B`
   means B. Bindings were appended and the runtime took the first match, so it meant
   A. That was wrong on its own, and it is the whole basis of the "set defaults, then
   override" shape a config with includes or a reset is written in. The line that
   loses now says which line took over.
2. **A layer switch left the old layer's keys latched on the bus.** The tab holding
   `key_a` is gone and the new one has never heard of it, so nothing published `false`
   and the sink kept holding `true` — the original stuck-key bug in a different coat.
   The module now remembers every pin *any* layer has claimed and keeps telling all
   of them where they stand. The test that nearly missed this asserted "the key is no
   longer true", which absence satisfies; the honest assertion is that `false` is
   actively published, with a fresh collector map every tick.

Console commands are also finished here. A command this module runs
(`SET_MOTION_STICK_NEUTRAL`, `RESET_MAPPINGS`) is live; one FlexInput owns itself is
`Ignored` with the *same reason* the bare command line gives, so a command means the
same thing whether written alone or bound to a button. A line whose every step is
such a command reads as ignored; a real key beside one still runs.

## `REAL_WORLD_CALIBRATION` and a measured number

Reported from hands-on testing: a calibration measured with the RWS Aim module
seemed to need a multiplier of about four in this module.

The arithmetic here is JSM's own and is now pinned by a test —
`moveMouse(velocity * RWC / IN_GAME_SENS * dt)`, verified against
`shapedSensitivityMoveMouse` in JSM's `InputHelpers.h`, with a 90° turn at `RWC = 1`
moving exactly 90 counts. So a constant factor cannot come from the formula; it can
only come from what a number *means*. There are exactly two places it can hide:

1. **`IN_GAME_SENS` double-counts.** A calibration measured in-game — by RWS Aim, or
   by JSM's own `CALCULATE_REAL_WORLD_CALIBRATION` — already has the in-game
   sensitivity baked in, because that is what was on screen while it was measured.
   Setting `IN_GAME_SENS` as well divides by it a second time, and the aim comes out
   exactly that many times too slow. Both lines now say so in the editor.
2. **`MIN_GYRO_SENS` and `MAX_GYRO_SENS` as a pair** make the sensitivity vary with
   turn speed, so no single multiplier matches — which would also explain a factor
   that "varies depending on the target".

The old error text on that line claimed it was "how many mouse counts make a full
turn", which is wrong by 360 and could have sent someone the other way. It now says
counts per degree.

## `X_` and `PS_` names stay aliases

Raised after hands-on testing: since `PS_UP` and `X_UP` land on the same
`dpad_up` pin, should a `PS_` name be routed only to a DualSense / DS4 sink when a
patch has one of each family wired downstream?

**No, and deliberately.** In JSM the two families ARE one set of names — a config
picks its pad with `VIRTUAL_CONTROLLER`, and `PS_CROSS` is simply how a
PlayStation player spells `X_A`. Routing by prefix would mean a config written
with `PS_` names silently stops working the moment it is wired to an XInput pad,
which is the exact class of silent failure the editor is supposed to abolish. It
would also make one config behave differently in two patches, and the module
cannot see the patch it sits in anyway.

Two pads of different families wanting different mappings is a *patch* question,
and the patch already answers it: give each pad its own JSM module (or its own
tab), wired to its own sink. Every pad-output line says as much in the editor —
that it needs a pad wired downstream, and that the prefix doesn't pick one.

## Phase 9 — the custom-curve fork

**Landed** (`jsm/cc.rs`). Kept in its own file because it is a *fork's* vocabulary:
a config written for stock JSM never touches any of it, and someone comparing this
against upstream should see at a glance what is and isn't Electronicks'. Every line
that uses one says so, too — a config carrying these won't load in an unmodified
JSM, and that is worth knowing before you save it.

All three traps the survey called out are handled:

- **`ACCEL_CURVE`'s five shapes** are implemented from the fork's own formulas, and
  the editor warns on a curve that ignores `MAX_GYRO_THRESHOLD` — Natural, Power and
  Sigmoid take their shape from their own parameters in absolute deg/s, so setting a
  top threshold and then switching curve silently stops it mattering.
- **`ONE_EURO_FILTER` stays a command**, global and sticky until `RESET_MAPPINGS`, so
  it cannot be chorded while the two numbers that tune it can. The line says so.
- **The pipeline order is the fork's**, not the obvious one: the brake measures
  *before* the snap so a snap-induced slowdown is not mistaken for the hands
  stopping, and the speed is recomputed afterwards for the curve.

One deliberate departure, and it is the bug the survey spotted. The fork's *eased*
angle snap sets the surviving axis to the full vector magnitude the moment the
direction enters the snap zone, while only fading the other — so right at the edge,
where the ease blend is still zero, the output already gains speed. Here the
surviving axis is blended in over the same curve: identical at full snap, smooth in
between. There is a test sitting exactly at the zone's edge.

`GYRO_SMOOTHING_DECAY` needs a word of warning that is in the editor too: it reuses
`GYRO_SMOOTH_TIME` and `GYRO_SMOOTH_THRESHOLD` for a different smoother, so the same
two numbers feel different. Neither smoother is "less" smoothing than the other —
which is why the test for it asserts that the two *differ*, not that one lags more.

**The fork's ten extra button names**: `MISC1`…`MISC6` are our `btn_misc1`…`6`, one
for one. `LTOUCH`, `RTOUCH`, `LMINI` and `RMINI` have no pin on our bus, and rather
than guess at a paddle or a misc slot — on one pad `LMINI` is a rail button, on
another something else — they compile, never fire, and say why on the line. That
needed a new `BtnSource::Absent(reason)`, which is the honest shape for "a name we
know and cannot read".

## Phase 9 — the survey it was built from

`evan1mclean/JSM_custom_curve` adds features on top of JSM that JSM users may be
carrying configs for. **Surveyed** at fork commit `0ace2da` against upstream
3.6.2: its `SettingID` enum, its command registry and its gyro pipeline were
diffed line by line, and the whole delta is 18 settings, one command and ten
button names. Everything it adds lives in the gyro path, which is why it belongs
after phase 3 rather than beside it — each item below extends a pipeline that
already exists.

Nothing here is guesswork about behaviour: the numbers are the fork's own
defaults and the maths below is its own, read from source.

### The acceleration curves — the fork's headline feature

`ACCEL_CURVE` replaces the straight line between `MIN_GYRO_SENS` and
`MAX_GYRO_SENS` with one of five shapes, applied per axis exactly where the
existing ramp is applied:

| Setting | Default | Meaning |
| --- | --- | --- |
| `ACCEL_CURVE` | `LINEAR` | `LINEAR` \| `NATURAL` \| `POWER` \| `QUADRATIC` \| `SIGMOID` \| `JUMP` |
| `ACCEL_NATURAL_VHALF` | `200` | deg/s at which `NATURAL` sits halfway up |
| `ACCEL_POWER_VREF` | `0.01` | deg/s where `POWER` starts climbing |
| `ACCEL_POWER_EXPONENT` | `0.5` | `POWER`'s exponent |
| `ACCEL_SIGMOID_MID` | `20` | deg/s at `SIGMOID`'s inflection |
| `ACCEL_SIGMOID_WIDTH` | `8` | `SIGMOID` steepness; larger is gentler |
| `ACCEL_JUMP_TAU` | `1.5` | how sharply `JUMP` approaches its step |

Written with `s` for the sensitivity returned, `ω` for `max(0, speed - MIN_GYRO_THRESHOLD)`
in deg/s, and `lo`/`hi` for the two sens settings:

- `NATURAL`: `s = hi - (hi-lo)·exp(-ln2·ω/vHalf)` — approaches `hi` but never
  reaches it.
- `POWER`: `s = lo + (hi-lo)·(1 - exp(-(ω/vRef)^exponent))`.
- `QUADRATIC`: `s = lo + (hi-lo)·(ω/cap)²`, flat at `hi` above the cap, where
  `cap` is `MAX_GYRO_THRESHOLD`.
- `SIGMOID`: logistic in `(ω - mid)/width`, then rescaled so ω=0 gives exactly
  `lo`.
- `JUMP`: `exp((ω-cap)/tau)` rising to a step at `cap` (`MAX_GYRO_THRESHOLD`
  again), likewise rescaled so ω=0 gives `lo`; `tau <= 0` is an instant step.

**The trap worth writing down before anyone implements this:** only `LINEAR`,
`QUADRATIC` and `JUMP` use `MAX_GYRO_THRESHOLD` at all. `NATURAL`, `POWER` and
`SIGMOID` ignore it completely and take their shape from their own parameters in
absolute deg/s. A user who sets a threshold pair and then switches curve will see
the top threshold silently stop mattering — so the editor should say so on the
`MAX_GYRO_THRESHOLD` line when the chosen curve does not consult it. That is the
kind of silent surprise the honesty guarantee exists for, and it is the fork's
behaviour we would be reproducing, not a bug we would be introducing.

### The rest of the delta

| Setting | Default | What it does |
| --- | --- | --- |
| `GYRO_SMOOTHING_DECAY` | `OFF` | swaps the rolling-average smoother for an exponential one whose time constant shrinks as the stick speeds up |
| `ONE_EURO_MIN_CUTOFF` | `6.0` | the one-euro filter's resting cutoff, Hz |
| `ONE_EURO_SPEED_COEFF` | `0.3` | how fast that cutoff opens up with speed (β) |
| `GYRO_ANGLE_SNAP` | `0.0` | snap to the nearest axis within this many degrees |
| `GYRO_ANGLE_SNAP_EASE` | `OFF` | ease into the snap instead of jumping |
| `DECEL_BRAKE_STRENGTH` | `0.0` | 0..1, how hard to brake after a fast flick |
| `DECEL_BRAKE_THRESHOLD` | `25.0` | deg/s of deceleration over 10 ms before braking starts |
| `ROLL_CONTRIBUTION` | `0.0` | percent, -100..100, roll's share of horizontal turn |
| `IGNORE_GYRO_DEVICES` | `""` | space-separated `VID:PID` list whose gyro is ignored |
| `TELEMETRY_ENABLED` / `TELEMETRY_PORT` | off | socket the fork's GUI reads the live curve from |

Plus one command, `ONE_EURO_FILTER`, and one new gyro space, `YAW_PLUS_ROLL`.

- **`ONE_EURO_FILTER` is a command, not a setting.** It switches the filter on
  globally for every pad and stays on until `RESET_MAPPINGS`, so unlike the two
  numbers that tune it, *it cannot be chorded*. Reproduce that asymmetry rather
  than quietly making it a setting — a config that relies on it being sticky
  would otherwise behave differently here. The filter itself is small: a
  first-order low-pass on the signal whose cutoff is `min_cutoff + β·|smoothed
  derivative|`, with the derivative itself low-passed at 1 Hz, and both parts
  reset whenever gyro is blocked.
- **`YAW_PLUS_ROLL` is default `LOCAL` plus a roll term**, and it uses the same
  signs: `x = -yaw - roll·(ROLL_CONTRIBUTION/100)`, `y = -pitch`. Confirmed
  against upstream's `LOCAL` block, where X-from-Y and X-from-Z are both
  negative — so this costs one arm in the gyro-space match and no new
  convention.
- **Order matters in the fork's pipeline** and is not the obvious one: decel
  brake measures speed *before* angle snap (so a snap-induced slowdown does not
  read as braking), the snap then runs, and only then is speed recomputed for the
  curve. Implement it in that order or the two features fight.
- **The decel brake keeps a 50 ms history** and compares now against 10 ms ago,
  with engage and release integrators (10 ms / 6 ms) and a speed gate of 2..60
  deg/s. The fork times this off the wall clock; ours should use the tick's own
  `dt`, which is more honest at any tick rate and makes it testable.
- **One fidelity question to settle at implementation time, not now.** In the
  eased snap branch the surviving axis is set to the full vector magnitude as
  soon as the direction enters the snap zone, while the other axis is only
  faded — so at the very edge of the zone, where the ease blend is still zero,
  the output already gains magnitude. That reads like an oversight in the fork
  rather than an intent. Decide then whether to match it exactly or blend both
  axes, and say which in a comment either way.

### What is deliberately not coming across

- **`IGNORE_GYRO_DEVICES` does not apply here.** It exists because JSM grabs
  every pad it can see; this module is handed one device by the patch and cannot
  see a VID or PID. The patch already answers it — don't wire that pad, or use
  `GYRO_OFF`. Worth a status on the line so a config carrying it is not silently
  ignored.
- **`TELEMETRY_ENABLED` / `TELEMETRY_PORT` do not apply either.** They exist to
  feed the fork's separate GUI over a socket. Our editor *is* the GUI, so the
  live sensitivity graph is something to draw natively from values we already
  hold — no socket, no port.
- **The fork's GUI itself is out of scope** as a program; the parts of it worth
  having (a live sens graph, a curve preview) are editor work, and a curve
  preview has prior art in this repo already — the per-card response curve
  editor.
- **Its ten new button names** (`LTOUCH`, `RTOUCH`, `LMINI`, `RMINI`,
  `MISC1`..`MISC6`) are SDL3 extended inputs. Take them as *input* names, mapped
  to whichever bus pins exist, and let the phase-4 honesty machinery report the
  rest: a name a wired pad does not report already says so on its line.

1. **Skeleton + parser + digital bindings.** Node, feature gate, bus republish,
   tabs with Load / Save, the editor with diagnostics, name tables, the button
   state machines (tap/hold, all modifiers, chord, simultaneous, diagonal,
   double, turbo), timing settings, pass-through / strict. Acceptance: a
   button-only config from the JSM `GyroConfigs` folder loads and plays.
2. **Triggers + digital sticks.** Threshold and hair trigger, full pull and the
   skip modes, stick `NO_MOUSE` directions and rings, `SCROLL_WHEEL`.
3. **Gyro and flick.** Gyro mouse with sensitivity ramps, spaces, smoothing and
   cutoff, real-world calibration, `AIM`, `FLICK` and its variants, `MOUSE_RING`
   / `MOUSE_AREA`, `HYBRID_AIM`.
4. **Modeshifts.** Chorded settings with the stack and the stick-recenter rule.
   *(landed — see above)*
5. **Virtual pad output.** Pad bindings, stick modes to virtual sticks,
   angle-to-axis and wind modes, gyro to a stick.
   *(landed — see above)*
6. **Touchpad and motion stick.** Grid, touch sticks, dual stage, motion stick
   and lean.
   *(landed — see above)*
7. **Feedback.** Rumble on/off, light bar, adaptive triggers, rumble bindings.
   *(landed — see above. The pin-model extension for bow / galloping / machine is
   deliberately NOT part of it: those three are reported honestly instead, and
   extending the bus is its own change.)*
8. **Action layers.** Quoted commands, including loading another tab, and
   `RESET_MAPPINGS`.
   *(landed — see above)*
9. **The custom-curve fork.** The five acceleration curves, decay smoothing, the
   one-euro filter, angle snap, the deceleration brake and `YAW_PLUS_ROLL`.
   *(landed — see above)*

Each phase lands with engine tests in the style of the Remapper's
(`rig_steps`-like tick stepping), and with mutation checks that the tests catch
the semantics they describe.

## Settled, and deliberately left out

- **Paths are ignored, file names are the identity** — see *Tabs are named by
  file name alone* above. Configs travel with the patch, not with a folder on
  disk.
- **Autoload stays FlexInput's job**, done the way FlexInput already does it. No
  per-tab "load when this game takes focus", and JSM's own `AUTOLOAD` is ignored.
- **Export to Remapper cards: later, not now.** It would be neat for someone
  leaving JSM behind, but the Remapper can't reproduce every config one to one
  without gaining abilities first, so a partial export would mislead.
