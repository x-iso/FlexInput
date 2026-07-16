# 3D controller models

The models in this folder are adapted from
[larfingshnew/3d-controller-overlay](https://github.com/larfingshnew/3d-controller-overlay)
(MIT license). Many thanks to its author.

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
   lines — position x/y/z (lines 1–3), rotation axis x/y/z (4–6), padding (7),
   scale placeholders (8–9), rotation angle in radians (10; X-axis rotation
   when the axis is all zeros), padding (11–14), and surface half-extents
   (15–16, used by `touch_point*` parts as a fallback touch area).
4. Optionally add `colors.fxcol` (`"group_key": [r, g, b]`) for the default
   palette — group keys match the colour editor (`shell`, `shell_secondary`,
   `led`, `touchpad`, `face_a`…`face_y`, `dpad`, `menu`, `bumper`, `trigger`,
   `left_dome`/`left_cap`/`left_rim`, `right_*`, `logo`).

Models can also live OUTSIDE the app in a user folder with the same structure
(Settings → "User models folder"); a same-named folder there overrides the
bundled model.
