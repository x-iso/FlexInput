# JSM Config and JoyShockMapper

The **JSM Config** module runs a [JoyShockMapper](https://github.com/Electronicks/JoyShockMapper)
config inside a FlexInput patch. Its press rules, timings and chord layering are ported from
JoyShockMapper 3.6.2's source. It also runs the additions of the
[JSM_custom_curve](https://github.com/evan1mclean/JSM_custom_curve) fork. A config written for
JoyShockMapper should load and play the way it does there.

This page is for people who already know JoyShockMapper. It covers what FlexInput adds, where
the rules differ, and what the module leaves to other parts of FlexInput. You don't have to
remember any of it to find out what a line does: the editor tints every line by what became
of it, and explains underneath the ones that need explaining.

- [Where the module sits](#where-the-module-sits)
- [FlexInput's own additions](#flexinputs-own-additions)
  - [Chord ranks and waits](#chord-ranks-and-waits)
  - [`@` targets](#-targets)
  - [MIDI](#midi)
  - [Flicks bound to a button](#flicks-bound-to-a-button)
  - [The gyro turning a stick](#the-gyro-turning-a-stick)
  - [Gyro spaces and axes](#gyro-spaces-and-axes)
  - [The custom-curve fork's settings](#the-custom-curve-forks-settings)
  - [The editor](#the-editor)
- [Tabs and layers](#tabs-and-layers)
- [Other rules that differ](#other-rules-that-differ)
- [What the module doesn't run](#what-the-module-doesnt-run)

## Where the module sits

- **One pad in, the bus out.** The module goes on an AutoMap wire. It reads the pad upstream,
  applies the config, and passes the result on to whatever is downstream: a virtual pad, the
  keyboard and mouse, a MIDI Out, or more modules.
- **Pass-through or strict.** By default, inputs the config never mentions pass through
  untouched, and the ones it mentions are taken over. With the header toggle **Only what the
  config says** on, the module publishes nothing but the config's own output. That is how
  JoyShockMapper behaves when the pad is hidden from the game.
- **Virtual pads are wired, not chosen.** `VIRTUAL_CONTROLLER` is ignored. Pad outputs
  (`X_A`, `PS_CROSS` and the rest) reach whatever virtual pad is wired downstream. As in
  JoyShockMapper, `X_` and `PS_` names are two names for one pin, so either reaches any pad
  wired there.
- **Each config is a tab.** A quoted file name switches to the tab of that name, or pulls it
  in. See [Tabs and layers](#tabs-and-layers).

## FlexInput's own additions

None of these exist in JoyShockMapper. A config that uses one won't load there the same way:
JoyShockMapper rejects the line or doesn't know the name.

### Chord ranks and waits

JoyShockMapper decides between held chords by press order alone: the chord pressed last wins.
It also never holds a press back waiting for a chord, so a chord counts only if its button is
already down. Two marks on a chord line change that.

| Mark | Example | What it does |
|------|---------|--------------|
| `:N` rank | `ZR,E:0 = PS_CIRCLE` | Beats another held chord binding E that has a higher rank or none, whichever was pressed last |
| `~N` on the chorded button | `ZR,E~40 = PS_CIRCLE` | E waits up to 40 ms for ZR before taking a binding |
| `~N` on the chord button | `ZR~40,E = PS_CIRCLE` | ZR's own binding waits up to 40 ms for E, and is skipped for that press if E comes |

The marks combine: `ZR~40,E~40:0 = PS_CIRCLE`. The `~` sits on the button that waits and the
rank goes at the end, after the second button's wait. Spaces around them are fine, and a
wait may be written `~40ms`. The line notes that it uses FlexInput's own marks.

#### Ranks

```
T1,E = LMOUSE
ZR,E:0 = PS_CIRCLE    # with T1 and ZR both held, E is always PS_CIRCLE
```

- When two held chords both bind the same button, the lower rank wins, whatever order they
  were pressed in.
- Lines of equal rank fall back to JoyShockMapper's rule: the chord pressed last wins.
- **Unranked lines rank below every number.** A config with no ranks behaves exactly as it
  does in JoyShockMapper, and you only rank the line that should win.
- A rank only compares lines for the same button. `ZR,E:0` doesn't make ZR beat T1 on any
  other button.
- Modeshifts take a rank too. `ZR,GYRO_SENS:0 = 2` beats `T1,GYRO_SENS = 4` while both are
  held.
- The choice is made when the button goes down. If E is already held when ZR is pressed, E
  keeps the binding it started with until you let go. That's JoyShockMapper's rule that a
  press keeps its binding.

#### Waits

A wait gives a chord a moment to complete when you press the two buttons almost together.
Which side waits depends on what goes wrong in your roll.

- **`ZR,E~40`: E waits for ZR.** Use this when E sometimes lands a moment before ZR and so
  misses the chord. Pressed alone, E takes its own binding when the wait runs out, which
  means every solo press of E is delayed by up to that long. Add a wait only where you
  actually roll the two together.
- **`ZR~40,E`: ZR's own binding waits for E.** Use this when ZR has a binding of its own
  (`ZR = PS_R2`) that shouldn't fire when you roll into the chord. ZR works as a chord
  straight away; only its own binding is held back. If E comes in time, ZR's own binding is
  skipped for that whole press. If E doesn't come, ZR's binding starts once the wait is out.
  This side does nothing for E pressed first, because by the time ZR arrives E has already
  taken its binding. For that, use `ZR,E~40`, or both.

Either way:

- The wait ends as soon as the chord is made.
- A press only waits for a chord that could still win it. With a better-ranked chord already
  held, there is no wait.
- The press keeps its own timing. Tap and hold are measured from when the button went down,
  so a tap that waited is still a tap.
- Let go during the wait, and it plays as a quick press of whatever binding it would have
  taken.
- A wait is 1 to 1000 ms.

Only chord lines take marks. A modeshift takes a rank but no wait, since there's no press to
hold back. Anywhere else, the line is an error that says where the marks go.

### `@` targets

`@` has no meaning in JoyShockMapper's grammar, so FlexInput uses it for its own targets.
Names with spaces go in quotes: `@"Weapon wheel — Show"`.

- **As an output:** `S = @Reload` drives a Macro Output port, and `N = @"Weapon wheel — Show"`
  a Virtual Menu entry. Typing an `@` name that doesn't exist yet makes its macro.
- **As a button:** `@Jump = SPACE` reads a Macro Output port as a button. It works in a chord
  (`@Jump,E = SPACE`, `E,@Jump = SPACE`), as a modeshift (`@Aim,GYRO_SENS = 2`) and as the
  gyro's switch (`GYRO_OFF = @Aim`). `@Throttle>30` counts as pressed from 30%. With another
  JSM node driving the port from a second device, one config can use that device's inputs as
  chord buttons.
- **MIDI SysEx:** `@"midi:sx:F07E7F0601F7"` sends a SysEx message.

### MIDI

`MIDI_*` names work on both sides of the `=`, for example `S = MIDI_C4` and
`MIDI_CC64 = GYRO_ON`.

- **Settings:** `MIDI_CHANNEL`, `MIDI_IN_CHANNEL`, `MIDI_VELOCITY`, `MIDI_IN_THRESHOLD`,
  `GYRO_MIDI_SCALE` and `ACCEL_MIDI_SCALE`.
- **Continuous sources:** sticks, the touchpad, the gyro and the accelerometer can send MIDI
  values: `LEFT_STICK_MODE = MIDI` with `LEFT_MIDI_X = MIDI_CC1`, and so on.
- **Analog values through a binding:** an analog value carries through a binding where MIDI
  is involved, so `ZL = MIDI_CC7` sends the trigger's pull.
- **Reaching a synth:** MIDI goes out through a MIDI Out module wired downstream. The
  editor's command list has a **MIDI…** row that builds a name for you.

### Flicks bound to a button

`FLICK_R90` turns 90° right once per press, and `FLICK_L45` turns 45° left. Any angle from
1 to 360 works. The turn takes `FLICK_TIME` unless you give it its own: `FLICK_R90_120MS`.
It goes wherever `FLICK_STICK_OUTPUT` says.

### The gyro turning a stick

`GYRO_OUTPUT = LEFT_STICK_ROTATION` (or `RIGHT_STICK_ROTATION`) turns the direction of that
stick as you turn the controller, while the stick is pushed. Its settings are:

- `GYRO_STICK_ROTATION_DEADZONE`
- `GYRO_STICK_ROTATION_MODE` (`ABSOLUTE` or `RELATIVE`)
- `GYRO_STICK_ROTATION_RETURN_TIME`
- `GYRO_STICK_ROTATION_SMOOTH_TIME`
- `GYRO_STICK_ROTATION_TAKEOVER`, `GYRO_STICK_ROTATION_TAKEOVER_SPEED` and
  `GYRO_STICK_ROTATION_OUTER`

It's the same rotation as the Gyro to Stick Rotation module.

### Gyro spaces and axes

- **`GYRO_SPACE = ORIENT`:** the camera follows where the controller's nose points, so it ends
  up where the pad points whatever path it took to get there. It's the same as the 3DOF to
  2D module's Orient.
- **`LOCAL_AXIS_OFFSET`:** with `GYRO_SPACE = LOCAL`, sets how far your neutral hold is
  pitched from lying flat, and measures yaw and roll about that hold. It's modelled on Steam
  Input's local-space pitch offset.
- **`ROLL_CONTRIBUTION`:** also works with `GYRO_SPACE = LOCAL` here. The fork only honours it
  with `YAW_PLUS_ROLL`.

### The custom-curve fork's settings

These come from the JSM_custom_curve fork, not from stock JoyShockMapper, and run here:

- `ACCEL_CURVE` (`LINEAR`, `NATURAL`, `POWER`, `QUADRATIC`, `SIGMOID`, `JUMP`) with
  `ACCEL_NATURAL_VHALF`, `ACCEL_POWER_VREF`, `ACCEL_POWER_EXPONENT`, `ACCEL_SIGMOID_MID`,
  `ACCEL_SIGMOID_WIDTH` and `ACCEL_JUMP_TAU`
- `GYRO_SMOOTHING_DECAY`, `ONE_EURO_MIN_CUTOFF` and `ONE_EURO_SPEED_COEFF`
- `GYRO_ANGLE_SNAP` and `GYRO_ANGLE_SNAP_EASE`
- `DECEL_BRAKE_STRENGTH` and `DECEL_BRAKE_THRESHOLD`
- `GYRO_SPACE = YAW_PLUS_ROLL` and `ROLL_CONTRIBUTION`

### The editor

- **Line status:** every line is tinted by what the parser made of it: running, ignored with
  a reason, waiting on something, or an error. Lines that need a word of explanation are
  listed underneath.
- **Tune:** the header's **Tune** toggle adds the sensitivity curve, drawn live, and a fader
  for every numeric setting. A fader rewrites the number on its own line, so the text stays
  the source of truth.
- **Commands…:** right-click the editor for a list of every setting, command and button the
  module knows, each with JoyShockMapper's own help text.
- **Gamepad editing:** the whole config can be edited from a gamepad, with a cursor that moves
  by whole tokens.

## Tabs and layers

A JoyShockMapper config does layers by loading another config file. Here each config is a
tab, and a quoted file name finds the tab of that name.

- **A tab is named by its file name alone.** `GyroConfigs/vehicle.txt`,
  `Autoload/GTA5/vehicle.txt` and `vehicle.txt` all mean the tab `vehicle`. That lets a set of
  configs be imported from wherever they sat and travel inside the patch.
- **Bound to a button, a file name switches tabs.** `HOME = "driving.txt"` switches which tab
  is running.
- **On a line of its own, it pulls that tab in right there.** `base.txt` applies that tab's
  lines at that point, changing only what they name, as JoyShockMapper loading the file
  mid-config would. This goes one level deep: a config named inside a pulled-in tab isn't
  followed, and the line says so.
- **A switched-to tab runs on its own.** This is the biggest difference from JoyShockMapper,
  where loading a file applies its lines over everything already set. Here a switch runs only
  the new tab and whatever it pulls in, so bindings and settings from the tab you left don't
  carry over. To layer a tab over a base, start it with the base's name on a line of its own:

  ```
  # the tab "driving":
  walking.txt          # everything from walking, then the changes
  HOME = "walking.txt"
  E = W
  ```

- **A switch starts the new tab fresh, except for presses under way.** A press keeps its
  binding until you let go, as in JoyShockMapper, so the tab it started on finishes it. A key
  held across the switch stays down until its button is released. What resets: settings
  changed by a binding (`S = "GYRO_SENS = 2"`), toggles the old tab left on, and the sticks',
  triggers', aiming and touchpad state. The gravity estimate is kept, because how you hold the
  pad hasn't changed.
- **The tab open in the editor is home.** A config starts there, an edit to any tab sends it
  back there, and `RESET_MAPPINGS` bound to a button returns to it. On a line of its own,
  `RESET_MAPPINGS` discards everything above it, as in JoyShockMapper. At the very top of a
  tab there's nothing to discard, because each tab compiles on its own, and the line says so.

## Other rules that differ

- **A modeshift takes hold one engine tick after its button.** That's well under a
  millisecond at FlexInput's tick rates.
- **A setting changed by a binding lasts until `RESET_MAPPINGS`, a tab switch or an edit.**
  `S = "GYRO_SENS = 2"` changes the setting from that press on, as typing it into
  JoyShockMapper's console would, but a tab switch also clears it here. A modeshift held on
  top still has the last word.
- **Keys pause while you type in the editor.** While you're typing in a config's editor with
  FlexInput focused, its keyboard and mouse bindings stop, so a binding under test can't type
  into the text.
- **A trigger threshold of 0 starts at 2%.** JoyShockMapper's default of 0 means "the
  slightest press", which on a trigger resting a hair above zero would read as held for good.
- **A press let go during a wait lasts a tap's length.** That covers the simultaneous-press
  window and a chord wait. JoyShockMapper lets the key go on its next poll; here it stays down
  for 40 ms, so a quick press always reaches the game.

## What the module doesn't run

Every one of these is accepted, not an error, and the line says why it does nothing.

### Left to other parts of FlexInput

| Setting or command | Why |
|--------------------|-----|
| `AUTO_CALIBRATE_GYRO`, `RESTART_GYRO_CALIBRATION`, `FINISH_GYRO_CALIBRATION`, `CALIBRATE_TRIGGERS`, and a `CALIBRATE` binding | The device card owns calibration |
| `CALCULATE_REAL_WORLD_CALIBRATION` | The RWS Aim module measures it; set `REAL_WORLD_CALIBRATION` from what it tells you. The Tune panel can calibrate it too |
| `LEFT_TRIGGER_OFFSET`, `LEFT_TRIGGER_RANGE`, `RIGHT_TRIGGER_OFFSET`, `RIGHT_TRIGGER_RANGE` | The device card measures trigger travel; effect zones are fractions of it here |
| `VIRTUAL_CONTROLLER` | Wire a virtual pad downstream instead |
| `AUTOLOAD` | FlexInput loads profiles its own way |
| `AUTOCONNECT`, `RECONNECT_CONTROLLERS`, `MERGE`, `SPLIT` | FlexInput tracks connected controllers itself |
| `WHITELIST_SHOW`, `WHITELIST_ADD`, `WHITELIST_REMOVE` | FlexInput hides pads through HidHide, in Settings |
| `JOYCON_GYRO_MASK`, `JOYCON_MOTION_MASK`, `CONTROLLER_ORIENTATION = JOYCON_SIDEWAYS` | FlexInput treats each Joy-Con as its own device |
| `JSM_DIRECTORY` | Configs live in the module's tabs, not a folder |
| `TICK_TIME` | FlexInput's engine sets its own tick rate, in Settings |
| `COUNTER_OS_MOUSE_SPEED`, `IGNORE_OS_MOUSE_SPEED` | FlexInput's mouse output doesn't go through the OS pointer speed |
| `HIDE_MINIMIZED`, `README`, `HELP`, `CLEAR`, `QUIT`, `SLEEP` | JoyShockMapper's own window and console |
| `IGNORE_GYRO_DEVICES` | The module is handed one pad by the patch; don't wire that pad in, or use `GYRO_OFF` |
| `TELEMETRY_ENABLED`, `TELEMETRY_PORT` | The fork's socket feeds its separate GUI; the editor here is the GUI |

### Not yet

| Feature | Why |
|---------|-----|
| `MOUSE_RING` stick mode, `SCREEN_RESOLUTION_X`, `SCREEN_RESOLUTION_Y` | Placing the pointer outright needs an absolute mouse output the bus doesn't have yet |
| `HYBRID_AIM` and its settings (`STICKLIKE_FACTOR`, `MOUSELIKE_FACTOR`, `RETURN_DEADZONE_IS_ACTIVE`, `RETURN_DEADZONE_ANGLE`, `RETURN_DEADZONE_ANGLE_CUTOFF`, `EDGE_PUSH_IS_ACTIVE`) | Needs a shared stick-and-gyro aim state the module doesn't keep yet |
| Numpad keys | The keyboard output doesn't send numpad keys yet |
| Scroll Lock, Num Lock, Pause and the Context key | Not in the keyboard output's key table yet |
