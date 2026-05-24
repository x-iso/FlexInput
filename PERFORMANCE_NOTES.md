# Performance optimization notes

Running log of CPU/perf work on FlexInput. Updated as wins land and as new
profile data shifts the picture. Read alongside `Cargo.toml` for the
puffin + arc-swap rationale; this file holds *strategy* and *what's left*.

## Profiling setup

- `puffin = "0.20"` + `puffin_http = "0.17"` workspace deps, instrumentation
  permanently in place (compiles to ~no-ops when scopes are off).
- Settings → Profiler toggle (debug builds only — wrapped in
  `#[cfg(debug_assertions)]` so it doesn't ship in release).
- External viewer: `cargo install puffin_viewer`,
  then `puffin_viewer --url 127.0.0.1:8585` while FlexInput is running with
  the toggle enabled.
- **Trap to avoid**: `puffin::profile_scope!("name")` declared at function-
  body scope keeps its RAII guard alive until function exit, so it silently
  measures everything after it too. Always wrap in an explicit `{ }` block
  if you don't want it to swallow trailing code. We hit this once and lost
  a debugging cycle chasing a phantom 180 ms cost.

## Wins landed so far

### Round 1 (committed previously, v0.8.5)
- **`spawn_processing_thread` swap-instead-of-clone**: HashMap maps handed to
  the UI via `std::mem::swap`, not `.clone()` (3 HashMap clones/UI-frame saved
  on large patches). `crates/engine/src/thread.rs:228-237`.
- **ArcSwap for graph + device signals**: UI publishes new graph snapshot via
  `store(Arc::new(g))`; proc thread reads via `load_full()` (refcount bump,
  no clone). Same pattern for I/O thread → proc thread signal map.
  `crates/engine/src/thread.rs:17-22`.
- **Persistent `TickOutput` reused across ticks** (cleared in-place at top
  of `eval_graph_tick`) — kills 5 HashMap reallocs per tick at 2 kHz.
- **Vectorscope rendering**: per-sample circle dots (2000+ shapes/frame) →
  12-chunk fading polyline (≈150× shape reduction).
- **Bounded tail clone for scope history** instead of cloning the full
  VecDeque (20k entries).

Result: empty patch 8% → 0-1% CPU. Heavy workspace 25% → 9-10% CPU (release).

### Round 2 (this commit)
- **Pre-show snarl-clone gated on input** (`crates/ui/src/canvas/mod.rs:486`).
  `Canvas::show` was unconditionally cloning the entire snarl every frame so
  it had an undo snapshot ready *in case* the user mutated. Now snapshots
  only when `pointer.any_down() || pointer.any_released() || any key pressed`.
  Fallback to post-mutation clone if the gate missed a real mutation (rare).
- **Sub-patch editor mutation_gen infrastructure**: added `mutation_gen: u64`
  on `Canvas` (bumped in push_undo/push_snapshot/undo/redo) and
  `last_synced_parent_gen` on `SubPatchEditor`. **Not yet used** — the
  obvious gating (skip the parent-snarl pre-sync when gen matches) broke
  live-data flow (vectorscope animation, pinned-widget interactivity). See
  "What I tried that didn't work" below.

Result: `show_subpatch_editors` 197.5 ms → 155.7 ms, `canvas_show` 31.6 ms
→ 16.7 ms in debug. Release CPU with editor open: 11% (down from ~17%).

## What I tried that didn't work

**Gated subpatch editor pre-sync behind `mutation_gen`** —
`crates/ui/src/app.rs:5065` does `*sp.snarl.clone()` every frame to copy the
parent's inner snarl into the editor's canvas. I gated this with a gen
counter so the clone only ran when the parent had mutated. CPU dropped further
but introduced regressions:
- Vectorscope stopped animating inside the editor (no fresh signal data
  flowing in)
- Pinned-body widgets became read-only while the editor was open
- Modules didn't react visually to signal flow (only AutoMap ports/wires
  glowed)

Root cause: the per-frame "sync" wasn't just structural — it was also the
transport for live signal display data that the inner editor needed each
frame. Gating it cut the live-data path. **Reverted**. The mutation_gen
field remains as inert infrastructure for the eventual proper fix.

**Earlier round (also reverted)**: Canvas `dirty_gen` with
`pointer.any_down() || !events.is_empty()` check fired on every mouse move,
defeating the gate. Response curve trail dropped `steps` resampling, made
trails not follow the curve. User saw CPU go *up* to 18%.

**Earlier round (also reverted)**: Per-frame `snarl_fingerprint` via
`serde_json::Value::to_string()` — fingerprint was more expensive than the
work it was meant to skip.

## What's still on the table

### High-value, high-effort
1. **Shared sub-patch snarl storage** (the real fix for the editor regression).
   Replace `UiSubPatch::snarl: Box<Snarl<NodeData>>` with
   `Arc<RwLock<Snarl<NodeData>>>` so the editor and parent share storage —
   eliminates the per-frame clone entirely. Touch points: ~19 read/mutate
   sites across `app.rs`, `canvas/mod.rs`, `canvas/viewer.rs`; serde needs
   custom `serialize_with`/`deserialize_with`; save/load round-trip
   (`.fxsp`, `.fxp`) needs verification; undo system needs careful audit
   (parent currently doesn't track inner-edit history — that becomes more
   visible with shared storage).
   Estimated 2-3 hours + thorough manual UAT (save/load, undo, nested
   editors, layout-mode pin/unpin, cross-boundary paste).

2. **Selective-field sync** (option 2). Instead of cloning the entire snarl
   for live data, identify *which* fields in `NodeData` carry live-display
   data vs structural data, and only copy the live subset each frame.
   Cheaper to land than option 1, but ongoing maintenance burden — anyone
   adding a new "live" field has to remember to add it to the sync list.

### Medium-value
3. **`gilrs::enumerate_devices` 16 ms spike** — move to a background thread
   that publishes a fresh device list via ArcSwap, so the UI thread never
   blocks on enumeration.

4. **Sub-patch editor LOD / culling** — when the inner snarl is paint-heavy
   at small zoom, render simplified node placeholders instead of full
   widgets. Would attack the egui-snarl paint cost that remains even with
   shared storage. Requires either snarl upstream changes or a vendor patch.

### Future / speculative
5. **Audio-rate vs control-rate engine split** — for future audio I/O
   modules. Audio-rate (~44 kHz) modules need SIMD + per-block processing;
   control-rate (current 2 kHz) modules stay on the existing tick loop.
   Hard architectural change; defer until first audio module needs it.

6. **Parallel `eval_graph_tick`** — the graph is a DAG so independent
   sub-trees could run on a thread pool. Probably not worth it unless we
   see eval cost dominate again at higher sample rates.

## Not worth pursuing (investigated, ruled out)

- **`pull_outputs_and_display`** — the corrected profile measured 16.7 µs.
  Was the #1 suspect for a week, turned out to be the misleading-scope
  artifact (see "Trap to avoid" above). Innocent.

- **`eval::compute_node`** — 246 µs total at 83 calls per frame (≈3 µs
  each). Already fast; further optimization not warranted.

- **`PuffinServerImpl::send`** — 553 µs, only present when profiler is on.
  Not in release builds, not on the critical path during normal use.
