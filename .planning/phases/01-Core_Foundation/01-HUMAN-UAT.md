---
status: partial
phase: 01-Core_Foundation
source: [01-VERIFICATION.md]
started: 2026-05-11T00:00:00Z
updated: 2026-05-11T00:00:00Z
---

## Current Test

[awaiting human testing]

## Tests

### 1. Group-into-Subpatch UI trigger
expected: Selecting multiple connected modules and triggering group action (via keyboard shortcut or context menu) creates a subpatch node with the selected modules inside, boundary wires become subpatch ports.
result: [pending]

### 2. Cross-boundary paste live round-trip
expected: Copying nodes from outer canvas (Ctrl+C) and pasting into an open SubPatchEditor inner canvas (Ctrl+V) inserts the copied nodes with AutoMap bridge nodes at the boundary.
result: [pending]

### 3. XInput physical rumble dispatch
expected: An XInput controller connected to Windows responds with motor vibration when rumble_strong or rumble_weak signals are routed through a RumbleOutput module in the graph.
result: [pending]

## Summary

total: 3
passed: 0
issues: 0
pending: 3
skipped: 0
blocked: 0

## Gaps
