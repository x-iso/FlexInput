# Vendored HIDMaestro driver package

The HIDMaestro virtual-controller driver binaries and INFs, vendored so
`flexinput-hidmaestro`'s `deploy.rs` can install them at runtime via `pnputil`.
There are deliberately **no catalogs and no certificates** here: the package is
signed on each machine it's installed on (see below).

| File | What |
|------|------|
| `HIDMaestro.dll` | UMDF2 virtual HID driver |
| `hidmaestro.inf` | main driver INF (`CatalogFile=hidmaestro.cat`, built at install) |
| `HMXInput.dll` | XUSB companion, rebuilt from source with a configurable `PollIntervalMs` |
| `hidmaestro_xusb.inf` | companion INF (`CatalogFile=hidmaestro_xusb.cat`, built at install) |
| `LICENSE` | HIDMaestro's MIT licence |

## Source & license

From **hifihedgehog/HIDMaestro** (https://github.com/hifihedgehog/HIDMaestro),
release **v1.3.17**, **MIT License**. Extracted from the Windows DriverStore copy
installed from that release. The protocol port in this crate is pinned to the same
version (`shm.rs` constants match v1.3.17 `SharedMemoryIO.cs` / `driver/driver.h`).

HIDMaestro's MIT licence — required to accompany these binaries and the port
derived from them — is reproduced verbatim in [`LICENSE`](LICENSE), copied from
the `v1.3.17` tag. Keep it alongside these files in any redistribution.

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
private key on whichever machine built the package. The helper re-signs such an
install once at startup and removes those two certs (by exact thumbprint).

**Updating these files** (e.g. a newer HIDMaestro release) needs no signing step:
replace the DLLs/INFs, and if an INF's models section changes, update the
matching `hardware_ids` in `deploy.rs` (a test fails until you do). Note that an
existing install is **not** replaced automatically: `ensure_driver_installed`
returns early whenever the driver is present, so machines keep the old package
until "Reinstall drivers". A driver update needs its own upgrade trigger.
