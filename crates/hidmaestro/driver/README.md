# Vendored HIDMaestro driver package

The HIDMaestro virtual-controller driver binaries and INFs, vendored so
`flexinput-hidmaestro`'s `deploy.rs` can install them at runtime via `pnputil`.
There are deliberately **no catalogs and no certificates** here: the package is
signed on each machine it's installed on (see below).

| File | What |
|------|------|
| `HIDMaestro.dll` | UMDF2 virtual HID driver |
| `hidmaestro.inf` | main driver INF (`CatalogFile=hidmaestro.cat`, built at install) |
| `HMXInput.dll` | XUSB companion (the XInput identity for Xbox 360 profiles) |
| `hidmaestro_xusb.inf` | companion INF (`CatalogFile=hidmaestro_xusb.cat`, built at install) |
| `LICENSE` | HIDMaestro's MIT licence |

## Source & license

From **hifihedgehog/HIDMaestro** (https://github.com/hifihedgehog/HIDMaestro),
release **v1.7.3**, **MIT License**, unmodified. HIDMaestro ships its driver files
as resources embedded in `HIDMaestro.Core.dll`, so they were extracted from the
release asset `HIDMaestro-v1.7.3.zip` (SHA-256
`a337ddc70e90ff969deaaaad8c3f3f8b7a0ee5b61a6a9ff6183ca35950bd8503`, matching the
digest GitHub records for it). Both INFs are byte-identical to the `v1.7.3` tag's
except `DriverVer`, which the release build stamps (`1.4.7.48`). Both DLLs carry
version `1.7.3.0` and ship unsigned.

The shared-memory port in `src/shm.rs` was transcribed from v1.3.17 and rechecked
against v1.7.3: the section layouts are unchanged. v1.7.3 added the companion's
input doorbell, which `shm.rs` signals.

The release's `THIRD-PARTY-NOTICES.txt` covers only usbip-win2, which HIDMaestro
bundles for its USB-audio personas. FlexInput doesn't vendor it, so no notice
beyond HIDMaestro's own applies.

HIDMaestro's MIT licence — required to accompany these binaries and the port
derived from them — is reproduced verbatim in [`LICENSE`](LICENSE), copied from
the release (identical to the `v1.7.3` tag's). Keep it alongside these files in any
redistribution.

## Signing

At install the elevated helper signs the package on the machine itself
(`src/signing.rs`, driven by `src/deploy.rs`):

1. stages these files in a fresh directory only Administrators and SYSTEM can
   write to;
2. finds or creates `CN=FlexInput Local Driver Signing` in `LocalMachine\My`, a
   self-signed code-signing cert on a **non-exportable** RSA-3072 key;
3. Authenticode-signs both DLLs, builds each catalog with the attributes Inf2Cat
   writes, and signs the catalogs;
4. trusts that cert in `Root` + `TrustedPublisher`, then runs `pnputil`.

Only in-box Windows components are used (`mssign32`, `wintrust`, `crypt32`,
`ncrypt`). SignTool and Inf2Cat can't be used: SignTool is non-redistributable and
Inf2Cat ships only with the WDK.

Earlier builds vendored `hidmaestro.cat` / `hidmaestro_xusb.cat` pre-signed by
`CN=HIDMaestroTestCert` and `CN=FlexInput HIDMaestro Driver`, and trusted those
two certs on every install — one trust anchor shared by every user, with the
private key on whichever machine built the package. Those builds also shipped a
companion rebuilt from source to add a `PollIntervalMs` pump-period setting, which
v1.7.3's doorbell makes unnecessary.

## Updating these files

Replacing them with a newer HIDMaestro release needs no signing step:

1. Extract the DLLs and INFs from the release's `HIDMaestro.Core.dll`
   (resources named `HIDMaestro.Resources.*`) and check the release digest.
2. If an INF's models section changed, update the matching `hardware_ids` in
   `deploy.rs` — a test fails until you do. The pinned `DriverVer` in
   `deploy.rs`'s tests needs updating too.
3. Make sure `DriverVer` changed. At startup the helper compares each installed
   package's `DriverVer` with the vendored INF's and reinstalls on a mismatch
   (`reconcile_installed_driver`); an identical `DriverVer` means existing
   installs keep the old driver.
4. Re-read `SharedMemoryIO.cs` and `driver/driver.h` for layout or protocol
   changes before trusting `shm.rs` against the new driver.
