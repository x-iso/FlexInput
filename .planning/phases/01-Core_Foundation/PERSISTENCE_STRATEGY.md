# FlexInput Patch Persistence Strategy — Phase 1

## Current State

The core patch format is versioned at `PATCH_VERSION = 1` (defined in `crates/core/src/patch.rs`).
Patches are serialized as JSON using `serde` and saved with the `.fxp` extension.

## Guiding Principle

Phase 1 must not break existing `.fxp` files. Any structural addition must be backward-compatible,
meaning older files that lack new fields must deserialize successfully using Rust defaults.

---

## Safe Extension Pattern

New optional fields are added using two serde attributes together:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub new_field: Option<T>,
```

or for collections:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub new_list: Vec<T>,
```

**Rules:**
1. `#[serde(default)]` ensures missing keys in older files produce a sensible Rust default.
2. `skip_serializing_if` keeps the serialized file clean: absent means "use default".
3. Required fields (those present in PATCH_VERSION 1: `version`, `nodes`, `wires`) must never be made optional or renamed without a version bump.

---

## Device Feedback Metadata (Phase 1 Specific)

Physical device output pins (rumble, RGB, adaptive triggers) added in Phase 1 are stored as
node `params` entries on device-output nodes. This is backward-compatible because:

- `NodeInstance.params` is already `HashMap<String, serde_json::Value>` with `#[serde(default)]`.
- Older patches without those param keys deserialize to an empty map, which is correct (no feedback state).
- New patches that contain feedback params are still readable by older code — unknown params are simply ignored
  by application logic (they sit inert in the HashMap).

No `PATCH_VERSION` bump is required for Phase 1 changes.

---

## When to Bump PATCH_VERSION

Increment `PATCH_VERSION` only when a field is:
- Removed or renamed (old files would fail to deserialize without migration).
- Changed from optional to required.
- Given a new serialization representation (e.g., type change from `u32` to `String`).

When a bump is necessary:
1. Increment `PATCH_VERSION` in `crates/core/src/patch.rs`.
2. Implement a migration function `fn migrate_v1_to_v2(json: &serde_json::Value) -> serde_json::Value`.
3. Detect the old version at load time and apply migration before deserializing into the new struct.
4. Add a regression fixture for the old format and a test that migration produces the correct result.
5. Update this document with the new version semantics.

---

## Backward Compatibility Test Coverage

Regression tests live at `crates/core/tests/patch_compat.rs` and use fixtures in
`crates/core/tests/fixtures/`:

| Fixture | Purpose |
|---------|---------|
| `compat_v1_basic.json` | Minimal v1 patch: 2 nodes, 1 wire, no device feedback |
| `compat_v1_device_feedback.json` | v1 patch with device output node (rumble params) |

Run with:
```
cargo test --package flexinput-core --test patch_compat
```

Any change to `Patch`, `NodeInstance`, `Wire`, or `SubPatch` that causes these tests to fail
signals a backward-compatibility break and requires a version migration or rollback.
