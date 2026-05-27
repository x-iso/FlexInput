# Easy Mode Sub-Patch Presets

This folder holds the factory sub-patch presets that show up as chips in Easy
mode's center panel. Each preset is a single `.fxsp` file — the same format
produced by the "Save Sub-patch" button inside a sub-patch editor window in
Advanced mode.

A user-chosen extra folder (set via **Settings → Easy mode → User presets
folder**) is also scanned at startup; user presets are tagged "(user)" in
the chip strip and override factory presets with the same display name.

## Authoring a preset

1. Open the app in Advanced mode.
2. Drop a `Sub-Patch` module on the canvas; open its **Edit** window.
3. Inside the sub-patch, build whatever processing the preset needs.
4. **Add at least one AutoMap-typed inlet and one AutoMap-typed outlet** to
   the sub-patch (Pins → Add → AutoMap). Easy mode wires the active
   physical gamepad's `Auto-Map →` output into the first AutoMap inlet,
   and routes the first AutoMap outlet to every active virtual sink's
   `← Auto-Map` input.
5. (Optional) Lay out exposed module elements and decorations in the
   sub-patch body — these will eventually render in the Easy mode central
   panel.
6. Save the sub-patch via the editor's **Save Sub-patch…** button. Drop
   the resulting `.fxsp` into this folder.
7. (Optional) Drop a `<preset-name>.png` next to the `.fxsp` to use as the
   chip thumbnail.

## AutoMap contract

A preset is "Easy-mode-usable" if it declares at least one AutoMap inlet
**and** at least one AutoMap outlet. Presets without either are loaded but
their input/output legs become no-ops:

- No AutoMap inlet → physical input doesn't reach the sub-patch.
- No AutoMap outlet → virtual sinks receive nothing.

Future v2 will lift the single-input restriction: Shift-clicking additional
gamepads in the left panel will allocate them to the 2nd, 3rd, … AutoMap
inlets in declaration order. Presets that want to support multi-input
should declare multiple AutoMap inlets up front.
