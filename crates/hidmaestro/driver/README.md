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
release **v2.0.1**, **MIT License**, unmodified. HIDMaestro ships its driver files
as resources embedded in `HIDMaestro.Core.dll`, so they were extracted from the
release asset `HIDMaestro-v2.0.1.zip` (SHA-256
`59bcd962f7bb5ab7c8c3d793887523d9f992b9ea29ca854d8c60b51b8f58c332`, matching the
digest GitHub records for it), resources `HIDMaestro.Native.x64.*`. v2.0.1 carries
the v2.0.0 driver build unchanged. Both INFs are the `v2.0.1` tag's with the
build's `DriverVer` stamp (`2.0.0.2201`). Both DLLs carry version `2.0.0.0` and
ship unsigned.

The shared-memory port in `src/shm.rs` was transcribed from v1.3.17 and rechecked
against v2.0.1: the input and output layouts are unchanged. v2.0.0 added the
**version contract** (`driver/hm_contract.h` upstream): a per-controller version
section carrying the identity tag of the controller's devnodes, and an identity
entry naming its index. Only under the contract does the driver read the
controller's new values — `FeatureAnswers` (the Sony calibration, pairing and
firmware replies the v1 driver held in code) and the XUSB companion's
`XusbAnswers`, without which the companion registers no XInput interface. See
`src/identity.rs`, `src/answers.rs` and `InputSection::create`.

The Sony and XUSB answers are compiled from HIDMaestro's own model files, vendored
under [`../profiles/models/`](../profiles/models) from the same tag (MIT).

The release's `THIRD-PARTY-NOTICES.txt` covers usbip-win2 and DsHidMini's DualShock
3 descriptor, neither of which FlexInput takes from it, so no notice beyond
HIDMaestro's own applies.

HIDMaestro's MIT licence — required to accompany these binaries and the port
derived from them — is reproduced verbatim in [`LICENSE`](LICENSE), copied from
the release (identical to the `v2.0.1` tag's). Keep it alongside these files in any
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
   (resources named `HIDMaestro.Native.x64.*` since v1.9.0; `HIDMaestro.Resources.*`
   before) and check the release digest. Windows PowerShell's
   `[Reflection.Assembly]::ReflectionOnlyLoadFrom(...).GetManifestResourceStream`
   reads them without running the assembly.
2. If an INF's models section changed, update the matching `hardware_ids` in
   `deploy.rs` — a test fails until you do. The pinned `DriverVer` in
   `deploy.rs`'s tests needs updating too.
3. Make sure `DriverVer` changed. At startup the helper compares each installed
   package's `DriverVer` with the vendored INF's and reinstalls on a mismatch
   (`reconcile_installed_driver`); an identical `DriverVer` means existing
   installs keep the old driver.
4. Re-read `SharedMemoryIO.cs`, `driver/driver.h` and `driver/hm_contract.h` for
   layout or protocol changes before trusting `shm.rs` against the new driver, and
   copy any changed `profiles/models` files the answers are compiled from.
