# Bluetooth Transports Reference

## Overview

FlexInput ships its own Bluetooth host stack. Two backends use it, both driving a
USB dongle bound to WinUSB rather than going through the Windows Bluetooth stack:

| Backend | File | Device ID prefix | Radio |
|---------|------|------------------|-------|
| Joy-Con 2 / Switch 2 pads | `crates/devices/src/joycon2_backend.rs` | `jc2:` | BLE (GATT) |
| Bluetooth Classic gamepads | `crates/devices/src/classic_bt.rs` | `btc:switch_pro:` | BR/EDR (HID over L2CAP) |

Both are registered unconditionally in `init_backends()` and enumerate **nothing**
until a controller actually connects, so a machine with no dongle pays one idle
thread each. The Joy-Con 2 backend is behind the `joycon2` cargo feature
(default on); the Classic backend is not gated.

**Why a private stack at all.** Switch 2 controllers expose no HID-over-GATT
service, so Windows binds no driver and gilrs, SDL and hidapi can never see them.
For Classic pads the reason is different: Windows runs one radio for every
headset, mouse and phone in range, and a dedicated dongle is where the latency
and polling rate come from.

---

## Crate Layout

### `crates/btle` — the host stack

| File | Purpose | Lines |
|------|---------|-------|
| `lib.rs` | `Dongle`: USB transport, HCI commands, LE + BR/EDR connect, ATT | ~2700 |
| `hci.rs` | Opcodes, event parsing, `CommandComplete`/`CommandStatus` | ~970 |
| `acl.rs` | ACL fragmentation, ATT PDU encode/decode, notifications | ~740 |
| `radio.rs` | Shared-radio arbitration: router thread, subscribers, leases | ~660 |
| `keystore.rs` | Persisted BR/EDR link keys and BLE LTKs | ~450 |
| `l2cap.rs` | L2CAP channels and signalling | ~300 |
| `joycon.rs` | Joy-Con 2 GATT handle constants | ~250 |
| `cadence.rs` | Inter-report timing metric, shared by both transports | ~170 |

### `crates/joycon2` — the Switch 2 protocol

| File | Purpose | Lines |
|------|---------|-------|
| `reports.rs` | Report parsing, orientation tracking, calibration, drift | ~4500 |
| `dongle.rs` | The dongle transport: scan, connect, init, stream | ~990 |
| `hub.rs` | Fallback transport over the Windows stack (btleplug/WinRT) | ~1850 |
| `protocol.rs` | Command framing, feature bits, UUIDs | ~640 |
| `dlog.rs` | The diagnostic logs and their release gating | ~370 |
| `pairing.rs` | The `0x15` LTK handshake | ~390 |
| `usb.rs` | WinUSB device enumeration and claiming | ~380 |

---

## The Shared Radio

A dual-mode adapter carries Classic and LE simultaneously — that is what dual
mode is. Both transports therefore share **one** `Dongle` through
`flexinput_btle::radio`:

```rust
let radio = flexinput_btle::radio::shared(vid, pid)?;   // whoever asks first opens it
let sub   = flexinput_btle::radio::subscribe(&radio);   // fan-out of events + ACL
```

- **Reads** come from the subscription. Reading the transport directly consumes
  the other backend's traffic as well as your own.
- **Anything that sends and waits for a reply** — paging, pairing, opening an
  L2CAP channel, a GATT read — runs inside `radio.with_dongle(|d| …)`, which
  holds the router off for the length of that conversation.

This was not always so, and the failure mode is worth knowing: each transport
used to call `Dongle::open` separately, WinUSB grants an interface to one
claimant, and the loser reported "another process holds it" about its own
process.

### Adapter selection

`preferred_dongle()` enumerates by **USB class**, not vendor id — the spec
assigns HCI-over-USB the triple class `0xE0` / subclass `0x01` / protocol `0x01`,
and every conforming dongle reports it. `FLEXINPUT_JC2_DONGLE=vid:pid` overrides.
When nothing is bound to WinUSB, the Bluetooth window shows a red ⛔ row rather
than failing silently.

---

## Joy-Con 2 (BLE)

### Connection

LE connect at the **spec-floor 7.5 ms** connection interval (`6` units of
1.25 ms; lower is rejected), with a max connection-event length of 6 units
(3.75 ms) so a second controller cannot starve the first. After connect the link
requests the **LE 2M PHY** — best effort, since the interval cannot go lower and
on-air rate is the only remaining headroom.

Observed report rates: **~200 Hz** for a single half; adding the second costs one
of them roughly a fifth of its reports.

### Initialisation is the recipe and nothing else

Everything in `connect_and_init` runs inside a radio lease, and a lease stops the
shared reader for **both** transports. Every extra write is time a Bluetooth
Classic controller cannot be heard. The init therefore sends exactly the working
sequence — MTU, the input and command-response CCCDs, the report-rate descriptor,
the handshake, pairing, memory reads, feedback and feature select — and nothing
the protocol investigation added on top of it.

### Handles

Taken from `crates/btle/src/joycon.rs`. Discovery by UUID was run against every
controller tested and never disagreed with these constants, so it is not done at
connect time.

| Handle | Role |
|--------|------|
| `0x000A` | Common input report (**never streams — see below**) |
| `0x000E` | Per-side input report — the working stream |
| `0x0014` | Command channel, bare frames |
| `0x0016` | Command channel, 17-byte rumble prefix — the working one |
| `0x001A` | Command responses for `0x0014` |
| `0x001E` | Command responses for `0x0016` (15-byte header offset) |

### ⚠️ Known limitation: the common input stream

The gyro published for these pads is **derived**, not measured. The per-side
report on `0x000E` carries a fused absolute angle and no angular rate, so every
rate is a difference over a quantised field. The standard motion block at
`0x30`/`0x36` — six contiguous `i16` of real accel and gyro, which every other PC
implementation reads — is **always zero** on the hardware tested here.

That block lives on `0x000A`, which is enabled by commands on `0x0014`. Measured,
on a Mobapad M12-S:

- Every command on `0x0016` is answered (`REPLY on 0x001a … ack 0x78`, all 19).
- **No command on `0x0014` is ever answered**, on either half, across repeated
  connections.
- `0x000A` declares `READ|NOTIFY` and refuses both (`ATT error 0x02`).
- Feature masks `0x07`, `0x0F`, `0x2F`, `0x3F`, `0xFF` all leave the block zero.

So on this hardware `0x0014` is accepted-but-inert and the common stream cannot
be enabled. **Retail Joy-Con 2 hardware is untested against this** and may well
differ. Consequences of the derived path: drift, and boundary artefacts
(mirroring and jumps) whose exact encoding is still unresolved — a wrap and a
fold produce the same value range and are distinguishable only across a boundary,
which needs consecutive raw reports — work for the standalone probes under
`crates/btle/src/bin/`, not the app.

---

## Bluetooth Classic

### Pairing is deliberately not automatic

Pairing replaces whatever host the controller was bonded to — on a Switch Pro,
its console. A background thread that quietly re-bonds any gamepad in range would
be a bad thing to ship. Pairing is an explicit act performed with the
`bt_classic` tool; the backend connects **only** to addresses that already have a
stored link key.

### The traps in bonded reconnection

Each of these silently breaks reconnection on its own. The first six stop it
working at all; the rest make it work **by luck**, which is harder to spot
because it sometimes succeeds.

1. `Accept_Connection_Request` must use role byte **`0x01`** (remain slave). Role
   `0x00` never completes the switch on a Switch Pro and surfaces as LMP
   Response Timeout `0x24`.
2. Page scan must be enabled (`Write_Scan_Enable` bit 1) — and its status
   checked, not assumed.
3. An abandoned page must be cancelled (`Create_Connection_Cancel`), or the radio
   stays deaf.
4. A newly issued link key must be **saved**, not discarded.
5. L2CAP channel setup is symmetric — either side may initiate, so both
   directions must be handled.
6. Control channel before interrupt; the HID profile expects that order.
7. **Page scan must be configured, not left at the default.** After `HCI_Reset`
   a controller listens for 11.25 ms every 1.28 s — under 1% of the time. A
   switched-on pad pages its host for only a few seconds, so whether it landed
   in a window was chance. `set_fast_connectable` applies BlueZ's
   fast-connectable values: interlaced scan, a window every 160 ms.
8. **No wait loop may discard events it passes over.** A command waiting for its
   Command Complete runs inside a lease, with the shared reader stopped, so
   anything it reads and drops is gone for every transport. `command_sync` and the
   LE connect path used to do exactly that, eating a calling controller's
   `Connection Request`. They now put back everything except advertising
   reports once their wait ends.
9. **A transport must not be sent traffic it never reads.** Subscriber queues are
   bounded and evict their oldest entry. An LE scan produces hundreds of
   advertising reports a second, and the Classic transport — which uses none —
   was queueing them all, so the oldest entry evicted was the `Connection
   Request` it was waiting for. It subscribes with
   `radio::subscribe_without_adverts`.
10. **Every L2CAP signalling command must be answered or refused.** Channel setup
    handled five commands and ignored the rest, including the **Information
    Request** many stacks send before opening a channel. A peer that asks waits
    for the answer — neither offering its channels nor answering requests for
    ours — and then drops the link. The trace reads "remote did not offer",
    `granted=false`, and a timeout. A peer with our answer cached does not ask,
    which is why it fails only sometimes. `l2cap::housekeeping_reply` answers
    Information and Echo Requests and rejects anything unimplemented.
11. **One signalling packet can carry several commands.** Only the first was
    decoded, so a channel request bundled behind another command was lost.
    Use `l2cap::parse_signals`.
12. **An unanswered channel request is sent again** (every 2 s, same identifier),
    and channel setup reads events so a link that drops is reported with its
    real reason instead of as a timeout.
13. **Do not page a controller whose incoming link just failed.** It goes
    straight back to calling us, and a paging radio cannot hear it — the page
    times out with `0x04` while the pad's call goes unanswered.
14. **A failed channel setup must take the link down.** By then the link is up
    and encrypted; leaving it standing strands the controller — connected to us
    at the radio level so it cannot call again, untracked by us so nothing uses
    or closes it — until it switches itself off. `disconnect_and_wait` closes it
    so the pad can call straight back.

### Stick calibration

Report `0x30` carries raw 12-bit sticks. Real travel is off-centre and
asymmetric, so without the controller's own calibration the normalised value
saturates on the axes before the diagonals and the circularity plot comes out
**square**.

The four calibration regions are read out of SPI flash by subcommand `0x10`:

| Address | Bytes | Contents |
|---------|-------|----------|
| `0x603D` | 9 | Factory left — max, centre, min |
| `0x6046` | 9 | Factory right — centre, min, max |
| `0x8010` | 11 | User left — valid when prefixed `B2 A1` |
| `0x801B` | 11 | User right — same |

Two things about this are easy to get wrong and both have been:

- **The blobs store offsets from centre, not absolute positions.** Reading them
  as absolute saturates the stick early — the square response again.
- **All four replies must arrive before the calibration is built.** The factory
  pair lands first; building on it discards the user calibration, which is where
  a controller recalibrated on a Switch stores its corrected centre. The symptom
  is one stick correct and the other reaching further one way than the other.

The requests are fired into the normal packet flow and the replies picked out of
it by **echoed address** — a `0x21` is the acknowledgement for every subcommand,
so "the next reply" is routinely somebody else's. Four retry rounds, then the
link settles for the fallback numbers and streams normally.

---

## Diagnostics

### Logs are opt-in for release builds

All diagnostic logs are gated at the single point they are opened
(`dlog::Sink::open`). In a **debug** build they behave as before; in a **release**
build a log is written only if its variable is set. Each takes a path, or `on`
for the default location, or `off` to disable.

| Variable | File | Contents |
|----------|------|----------|
| `FLEXINPUT_JC2_LOG` | `jc2-dongle.log` | Connection lifecycle |
| `FLEXINPUT_JC2_DRIFT_LOG` | `jc2-drift.log` | One drift reading every 30 s |
| `FLEXINPUT_JC2_IMU_LOG` | `jc2-imu-diag.log` | IMU/orientation diagnostics |

Long-lived logs go to `%APPDATA%\FlexInput\logs`; the IMU log goes beside the
executable, because it is meant to be found and mailed by someone who will not be
walked through AppData. Each rotates at 5 MB keeping one generation, and each is
written by its own thread — writing inline with a flush per line stalls the
transport thread and manufactures the symptom being investigated.

The connection log records lifecycle only — scan start and stop, connect
attempts and outcomes, disconnects. It used to log every advertisement the scan
heard, which in an ordinary room is hundreds of lines a second.

### Measuring report cadence

`flexinput_btle::cadence::Cadence` measures how evenly reports arrive. The
Classic transport prints it every 5 s in debug builds, or in release with
`FLEXINPUT_BTC_CADENCE=on`:

```
[bt-classic] da:2d:16:0f:01:69 cadence: 170.2 Hz over 851 reports, mean 5.87 ms
  (min 0.02, max 31.4), jitter 3.1 ms/step
```

Per-step jitter is tracked separately from the mean because they answer different
questions: a stream alternating 5 ms and 25 ms has the same mean as a steady
15 ms one and behaves nothing like it. It distinguishes "this is the device's
chosen rate" from "this is our loss".

### No protocol experiments in the app

The investigation into the Joy-Con 2 common input stream used a set of
environment switches that rewrote the initialisation sequence, probed attributes
and captured raw reports. It concluded (see *Known limitation* above) and the
switches were removed: each one put extra traffic on a radio the Classic transport
shares. Protocol probing belongs in the standalone binaries under
`crates/btle/src/bin/`, which own the dongle outright and affect nothing else.

---

## Common Pitfalls & Gotchas

### 1. Draining ACL from the wrong place

Inside a `with_dongle` lease, read with `dongle.drain_acl`. Outside one, read
from the subscription. Mixing them means one transport eats the other's packets.

### 2. ATT Write Response carries no handle

A bare `0x13` cannot be attributed to a request. If several writes are in flight,
matching "the next write response" will credit somebody else's answer to yours.
Error responses **do** name the handle and are safe to match.

### 3. Absence is only evidence when you can show what you would have accepted

A diagnostic that logs "no reply" while discarding every non-matching packet
cannot distinguish a silent peer, a reply on an unwatched handle, and a broken
matcher. Log what arrived, not just what matched.

### 4. Cross-log timestamp comparison is unsound

Each log has its own clock origin. Two files cannot be interleaved to infer
ordering; that is what the shared IMU log is for.

### 5. The keystore holds per-machine secrets

Link keys and LTKs are shared secrets for a specific adapter and controller.
They live in `%APPDATA%\FlexInput` (override with `FLEXINPUT_BT_KEY_DIR`) and
must never be committed. Entries are tagged with the adapter address they belong
to, since a key issued by one adapter is useless to another.

### 6. A convenient uncalibrated entry point becomes the default

`parse_switch_pro_report` (no calibration) sat next to the real parser and the
Classic path used it for as long as it existed, shipping the square-stick
response. It is now `#[cfg(test)]` so no transport can reach for it by accident.

### 8. Never touch the hardware to answer a question the process already knows

`discover()` enumerates the USB bus and opens adapters to read their names. The
title-bar Bluetooth button called it every two seconds, on the UI thread, and it
opened the very adapter a transport was streaming from — control transfers into
a live link, on a timer. Whether this process holds an adapter is `is_ours` /
`holding_any`, which cost nothing; names are cached from the last time an adapter
was opened while idle.

### 7. A wait loop under a lease must put back what it passes over

Reading an event inside `with_dongle` consumes it for everyone: the router is
stopped, so no other subscriber will ever see it. Matching one reply and
`continue`-ing past the rest is therefore not "skipping" — it is deleting. Collect
the others and hand them back with `push_events_front` **after** the wait, never
during it: re-queued mid-wait they are read straight back ahead of the wire, and
the loop spins on them instead of reaching its own reply.
