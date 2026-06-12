# Vendored HIDMaestro driver package

These are the **pre-signed** HIDMaestro virtual-controller driver binaries,
vendored so `flexinput-hidmaestro`'s `deploy.rs` can install them at runtime via
`pnputil` without shipping any signing toolchain.

| File | What |
|------|------|
| `HIDMaestro.dll` | UMDF2 virtual HID driver |
| `hidmaestro.inf` / `hidmaestro.cat` | main driver INF + signed catalog |
| `HMXInput.dll` | XUSB companion (for the future Xbox360 path) |
| `hidmaestro_xusb.inf` / `hidmaestro_xusb.cat` | companion INF + signed catalog |
| `HIDMaestroTestCert.cer` | public signer cert (added to Root/TrustedPublisher at install) |

## Source & license

From **hifihedgehog/HIDMaestro** (https://github.com/hifihedgehog/HIDMaestro),
release **v1.3.17**, **MIT License**. Extracted from the Windows DriverStore copy
installed from that release. The protocol port in this crate is pinned to the same
version (`shm.rs` constants match v1.3.17 `SharedMemoryIO.cs` / `driver/driver.h`).

## Distribution caveat

The catalogs are signed with `CN=HIDMaestroTestCert` (HIDMaestro's own test cert).
For a shipping FlexInput build, **re-sign the driver package with a cert you
control** at build time and replace `*.cat` + `HIDMaestroTestCert.cer`. The runtime
deploy logic in `deploy.rs` (trust cert → `pnputil /add-driver /install`) is
unchanged regardless of which cert signs the package.
