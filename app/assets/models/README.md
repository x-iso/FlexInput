# 3D controller models

The models in this folder are adapted from
[larfingshnew/3d-controller-overlay](https://github.com/larfingshnew/3d-controller-overlay)
(MIT license). Many thanks to its author.

**Except `Steam Controller/`**, which is **not** MIT: it is © 2026 Valve
Corporation, modified by FlickStick Videos and then by ceski for
[ceski-1/3d-controller-overlay](https://github.com/ceski-1/3d-controller-overlay),
under [CC BY-NC-SA 4.0](http://creativecommons.org/licenses/by-nc-sa/4.0/)
(non-commercial use only). It is bundled exactly as ceski publishes it; its
`license.txt` and `license_attribution.txt` must stay with it. Only its
`colors.fxcol` is FlexInput's own.

## Folder structure

Each controller lives in its own folder:

```
<Model Name>/
  info.txt          # part list + per-part transforms (see below)
  <part>.obj        # one mesh per part (positions + normals)
  colors.fxcol      # optional editable default colour palette (JSON)
```

## Adding a custom model

1. Create a folder with the model name (this is what the picker shows).
2. Export each part as a separate `.obj` (triangles/quads, with normals).
   Part NAMES drive behaviour — the viewer maps them to material groups and
   animations (`top_shell`, `bottom_shell`, `extra` = LED strip, `a_button`,
   `dpad_up`, `left_stick`/`left_cap`/`left_ring`, `left_trigger`,
   `left_bumper`, `touchpad`, `touch_point1/2`, `guide_button`, …). Follow
   the existing folders (DualSense is the most complete example).
3. Write `info.txt`: for every part, the filename line followed by 16 numeric
   lines — position x/y/z (lines 1–3), rotation axis x/y/z (4–6), unused
   (7–10), rotation angle in radians (11; X-axis rotation when the axis is all
   zeros), unused (12–14), and the touch area's width and depth (15–16; on
   `touch_point*` parts the finger dot ranges over half of each, either side of
   where it rests — on a one-pad model the `touchpad` mesh's measured size
   wins). Three more lines may follow (17–19): the touch area's tilt in
   radians about X, then Y, then Z, for an angled pad.
4. Optionally add `colors.fxcol` (`"group_key": [r, g, b]`) for the default
   palette — group keys match the colour editor (`shell`, `shell_secondary`,
   `led`, `touchpad`, `face_a`…`face_y`, `dpad`, `menu`, `bumper`, `trigger`,
   `left_dome`/`left_cap`/`left_rim`, `right_*`, `logo`).

Models can also live OUTSIDE the app in a user folder with the same structure
(Settings → "User models folder"); a same-named folder there overrides the
bundled model.

## Models from ceski's fork

Folders from [ceski-1/3d-controller-overlay](https://github.com/ceski-1/3d-controller-overlay)
load as they are. That fork writes 19-line blocks and names parts after SDL's
gamepad buttons; the viewer reads its names as the ones above: `south_button`
/ `east_button` / `west_button` / `north_button` → `a_button` … `y_button`,
`left_shoulder` → `left_bumper`, `left_stick_base` / `_ring` / `_cap` →
`left_stick` / `left_ring` / `left_cap` (and the right-hand ones), and
`misc2` → `touchpad2`, a second trackpad. Its `paddle1`–`paddle4` follow SDL's
order (right 1, left 1, right 2, left 2) and become `paddle_r1`, `paddle_l1`,
`paddle_r2`, `paddle_l2`; the original layout's `paddle1`–`paddle4` count by
position on a Joy-Con's rail instead.

When the two `touch_point` parts rest at different places, each is on its own
trackpad: touch 1 moves on the left pad and touch 2 on the right, each sized
and tilted by its own block.
