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

`evan1mclean/JSM_custom_curve` adds features on top of JSM 3.x that JSM users may
be carrying configs for. **Not yet surveyed**: github was unreachable from the dev
machine when this was written (`git clone` and a raw fetch both failed while an
ordinary push had just worked), so the fork's own settings have not been read
first-hand and nothing here is designed yet. Before implementing: clone it, diff
its `SettingID` list and command registry against upstream 3.6.2 the way phases
1-4 were done, and write the caveats down here first. Slot it after the phases
that share its ground (aiming, phase 3) so its curve settings extend a pipeline
that already exists rather than growing a second one.

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
6. **Touchpad and motion stick.** Grid, touch sticks, dual stage, motion stick
   and lean.
7. **Feedback.** Rumble on/off, light bar, adaptive triggers (plus the pin-model
   extension for bow / galloping / machine), rumble bindings.
8. **Action layers.** Quoted commands, including loading another tab, and
   `RESET_MAPPINGS`.

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
