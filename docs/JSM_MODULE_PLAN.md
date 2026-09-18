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
