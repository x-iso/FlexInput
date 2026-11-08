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
release **v2.1.0**, **MIT License**, unmodified. HIDMaestro ships its driver files
as resources embedded in `HIDMaestro.Core.dll`, so they were extracted from the
release asset `HIDMaestro-v2.1.0.zip` (SHA-256
`7fe349f163864b8dfc36117af260d51bac94a2263b6cdfa719cb364a808397b4`, and its
`HIDMaestro.Core.dll` `f13e32591404db0f418af9265b8e240f71257297cfae5a41dcd696fd87e9b4d5`,
both matching the digests the release publishes), resources
`HIDMaestro.Native.x64.*`. Both INFs carry the build's `DriverVer` stamp
(`2.1.0.1716`); both DLLs carry version `2.1.0.0` and ship unsigned.

v2.1.0 is the first driver build since v2.0.0 (2.0.1–2.0.3 shipped it unchanged).
Against the v2.0.1 package it changes the driver's frame delivery — frames cached
before parked reads are served, serial-number SeqNo comparison across the wrap,
reads completed inside the lock that took the frame, SRW locks in place of
UMDF wait locks — and both INFs add `UmdfHostPriority = PriorityHigh`, which
runs each device's host in the high priority class (upstream measures a DS4's
worst stall under CPU load at 360 ms before, 1.4 ms after). The INFs' models
sections, and so `deploy.rs`'s hardware ids, are unchanged.

The shared-memory port in `src/shm.rs` was transcribed from v1.3.17 and rechecked
against v2.0.1 and v2.1.0: the input and output layouts and the version contract
(`hm_contract.h`'s head, driver protocol 5 / minimum 4) are unchanged. v2.1.0's
writer side is ported too: the signal gate (`Global\HIDMaestroSignal<N>`,
`shm.rs`'s `SignalGate`, transcribed from `SharedMemoryIO.WriteInputFrame`), which
hands a frame to a busy worker instead of signalling it and rings the XUSB
companion's doorbell only while it holds a request; `AckSignal = 0` in
`Controller<N>` (FlexInput never waits on the delivery acknowledgement), which
spares the driver a `SetEvent` per delivered frame; and the SeqNo that steps over
0 at the wrap (`next_even_seq`). Measured here, a frame's wakeup costs 367 ns with
a `SetEvent` every frame, 12 ns when the gate hands it over, and 359 ns when the
worker is waiting and it still signals. v2.0.0 added the
**version contract** (`driver/hm_contract.h` upstream): a per-controller version
section carrying the identity tag of the controller's devnodes, and an identity
entry naming its index. Only under the contract does the driver read the
controller's new values — `FeatureAnswers` (the Sony calibration, pairing and
firmware replies the v1 driver held in code) and the XUSB companion's
`XusbAnswers`, without which the companion registers no XInput interface. See
`src/identity.rs`, `src/answers.rs` and `InputSection::create`.

The Sony and XUSB answers are compiled from HIDMaestro's own model files, vendored
under [`../profiles/models/`](../profiles/models) from v2.0.1 (MIT); v2.1.0's are
byte-identical. So are its device profiles FlexInput keeps its own modified
copies of (`dualsense`, `dualsense-edge`, `dualshock-4-v2`, the two Sony
composites, `xbox-360-wired`), so the 2.1.0 update leaves FlexInput's report
encoding — the vendor block, pinned bytes and the composite's own feature
answers — exactly as it was.

The release's `THIRD-PARTY-NOTICES.txt` (identical in v2.0.1 and v2.1.0) covers usbip-win2 and DsHidMini's DualShock
3 descriptor, neither of which FlexInput takes from it, so no notice beyond
HIDMaestro's own applies.

HIDMaestro's MIT licence — required to accompany these binaries and the port
derived from them — is reproduced verbatim in [`LICENSE`](LICENSE), copied from
the release (unchanged from v2.0.1 to v2.1.0). Keep it alongside these files in any
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
