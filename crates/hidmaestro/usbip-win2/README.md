# Vendored usbip-win2 installer

| File | What |
|------|------|
| `USBip-0.9.8.1-x64.exe` | usbip-win2's own x64 installer, unmodified |
| `LICENSE.txt` | its BSD 2-Clause licence |

## Source & license

From **vadimgrn/usbip-win2** (https://github.com/vadimgrn/usbip-win2), release
**v.0.9.8.1**, asset `USBip-0.9.8.1-x64.exe`, SHA-256
`38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a` — the value
HIDMaestro pins for the same file. `LICENSE.txt` is the tag's own.
The installer carries Microsoft-signed drivers: the virtual host controller
(`usbip2_ude`) and the root-hub filter (`usbip2_filter`).

The binary is embedded in `flexinput.exe` (`src/usbip_bundled.rs`), and the build
test `the_embedded_installer_is_the_pinned_release` fails if it isn't the
pinned file. At install the elevated helper writes it to a fresh directory only
Administrators and SYSTEM can open, checks the hash again, writes the licence
beside it, and runs it with `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`.

## Why 0.9.8.1

It is the first release with the fix for the attach work item that blocked
driver unload in 0.9.8.0 (usbip-win2#188), and the version HIDMaestro's
composite personas were validated against. `flexinput-usbip`'s `vhci.rs` also speaks the
0.9.8.0 and 0.9.7.x request layouts, so a machine where another program
installed one of those still works.

## Updating

Replace the installer, update `VERSION`, `INSTALLER_NAME` and
`INSTALLER_SHA256` in `src/usbip_bundled.rs`, and recheck `flexinput-usbip`'s
`src/vhci.rs` request layouts against the new tag's `include/usbip/vhci.h`.
