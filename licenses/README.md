# Third-party licence texts

Verbatim licences for everything FlexInput **redistributes**. They are embedded
into the binary by [`crates/ui/src/licenses.rs`](../crates/ui/src/licenses.rs)
with `include_str!` and shown in-app under **Settings → Credits → Third-party
licenses**.

Files here are *copies*, kept only for dependencies that don't already carry a
licence file somewhere in this repo. Where one does, `licenses.rs` points
straight at it instead — no copy, so it can't drift:

| Already in the repo | Path |
|---|---|
| FlexInput | [`LICENSE`](../LICENSE) |
| HIDMaestro | [`crates/hidmaestro/driver/LICENSE`](../crates/hidmaestro/driver/LICENSE) |
| egui-snarl | [`vendor/egui-snarl/LICENSE-MIT`](../vendor/egui-snarl/LICENSE-MIT) |
| btleplug | [`vendor/btleplug/LICENSE.md`](../vendor/btleplug/LICENSE.md) |

## Where each copy came from

| File | Source |
|------|--------|
| `egui.txt` | `emilk/egui` tag `0.33.3`, `LICENSE-MIT` — covers egui, eframe, egui_extras, and the vendored egui-wgpu / egui-winit forks |
| `gilrs.txt` | `gilrs-project/gilrs`, `LICENSE-MIT` |
| `SDL.txt` | `libsdl-org/SDL` tag `release-3.4.10`, `LICENSE.txt` — SDL is built from source and linked statically, so it ships inside `flexinput.exe` |
| `sdl3.txt` | `sdl3` 0.18.4 crate source, `LICENSE` |
| `sdl3-sys.txt` | `sdl3-sys` 0.6.6+SDL-3.4.10 crate source, `LICENSE.md` |
| `midir.txt` | `midir` 0.11.0 crate source, `LICENSE` |
| `rfd.txt` | `rfd` 0.15.4 crate source, `LICENSE` |
| `serde.txt` | `serde` 1.0.228 crate source, `LICENSE-MIT` |
| `enigo.txt` | `enigo` 0.6.1 crate source, `LICENSE` |
| `3d-controller-overlay.txt` | `larfingshnew/3d-controller-overlay`, `LICENSE` — the models in `app/assets/models` |
| `CC-BY-3.0.txt` | creativecommons.org legal code — the game-icons.net icons in `app/assets/general` |
| `CC0-1.0.txt` | creativecommons.org legal code — the Kenney input-prompt icons |

Where a dependency offers a choice of licences (`MIT OR Apache-2.0`), the copy
here is the option FlexInput takes; `licenses.rs` still records the full choice
in each entry's `spdx` field.

## Keeping this current

`include_str!` means a missing or renamed file is a **build error**, not a
silently dropped notice. What it can't catch is a licence that changed upstream,
or a new dependency nobody added an entry for. So:

- **Adding a dependency whose code or assets ship in a build** — add a `License`
  entry in `licenses.rs`, and a copy here if the crate source has no licence file
  already in the repo.
- **Bumping a dependency across a major version** — re-copy its licence and check
  the copyright line. Version-pinned rows in the table above should be updated to
  the version actually built.
- **Not needed** for tools the user installs separately (HidHide, ViGEmBus) or
  for build-time-only dependencies — neither is redistributed.
