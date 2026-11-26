# Joy-Con 2 — Mobapad M12 Pro / joycon2cpp Recon

Findings from reading **`DarkEslariate/joycon2cpp-m12-pro-fix`** against FlexInput's
`crates/joycon2`. Kept so this recon does not have to be redone when the Joy-Con 2
support gets tweaked.

Nothing in this file was run on hardware except where marked **[measured]**, which means
FlexInput measured it (M12-S unless stated). Everything else is marked **[fork]** (the
joycon2cpp fork asserts it), **[agrees]** (both implementations independently reached it),
or **[hypothesis]** (worth testing, unproven).

Date of recon: 2026-11-21/22. The fork was cloned to `C:\tmp\jc2ref` (scratch copy —
re-clone from GitHub, this path is not durable).

---

## Contents

1. [What the fork is](#1-what-the-fork-is)
2. [Conclusions up front](#2-conclusions-up-front)
3. [GATT cross-reference](#3-gatt-cross-reference)
4. [Init sequence and the console session](#4-init-sequence-and-the-console-session)
5. [Report layouts](#5-report-layouts)
6. [Button maps (the useful tables)](#6-button-maps)
7. [The quaternion, decoded twice](#7-the-quaternion-decoded-twice)
8. [Contested numbers to settle](#8-contested-numbers-to-settle)
9. [Mouse / optical](#9-mouse--optical)
10. [Rumble](#10-rumble)
11. [Discovery and identity — the blocker](#11-discovery-and-identity--the-blocker)
12. [Report rate, ticks, and going above 200 Hz](#12-report-rate-ticks-and-going-above-200-hz)
13. [Actionable list, in order](#13-actionable-list-in-order)
14. [Open questions and traps](#14-open-questions-and-traps)
15. [Appendix — verbatim data](#15-appendix--verbatim-data)

---

## 1. What the fork is

A fork of **TheFrano/joycon2cpp** (itself a descendant of deaker's `joycon2cpp` research),
maintained by `@DarkEslariate`, MIT-licensed, adding support for the **Mobapad M12 Pro**
Switch 2 split pads.

- Only the **Windows testapp** is vendored: `testapp/src/testapp.cpp` (~3100 lines,
  ImGui + WinRT GATT + ViGEm output), `testapp/src/JoyConDecoder.cpp/h`, `DsuServer.cpp`,
  DSU pointer tester. There is no library, no CMake target for one.
- Two real commits: `8d94835` *"Added Mobapad M12 Pro support"* (~1400 lines) and
  `2377d8a` *"Add mouse support for Mobapad M12 Pro"* (311 lines). `MOBAPAD_CHANGES.md` is
  the author's own change list.
- ⚠️ The author's own commit note: *"Changed underlying code and bluetooth detection
  method, so it may not work with original switch 2 joycons, haven't tested yet."* So the
  fork's **retail Joy-Con 2 path is unverified by its own author**.
- Windows-only (ViGEmBus). No pairing/SMP code at all.

The whole M12 Pro mechanism, in one sentence: when the pad will not stream on the
characteristic the app expects, the fork **subscribes to the per-side characteristic,
repacks the console-style report it gets there into a synthetic standard report, and
derives a gyro by differencing the pad's hardware orientation quaternion** — an adapter in
front of an untouched decoder (`ConsoleToGenuine` :1806, `M12ProQuatToGyro` :1704).

---

## 2. Conclusions up front

| Area | Verdict |
|---|---|
| GATT attributes | **Identical**, every UUID, including the prelude char and the `85 00` vendor descriptor. **[agrees]** |
| Motion block offsets | The fork's console report lands exactly on FlexInput's **hand-measured** layout: block at `0x0F` (L) / `0x10` (R) with the one-byte left shift, accel at `base+0x12/0x16/0x1A` = `0x22/0x26/0x2A`, read as **i16**. Strong independent confirmation. **[agrees]** |
| Device tick | 12 bits, 1.25 ms units (800 Hz), 4096 wrap, no carry. **[agrees]** |
| The 12 mystery bytes | The fork decodes the **same 12 bytes** as FlexInput's `decode_motion_quat`: firmware orientation quaternion, largest-component-divided, component index in tag bits. FlexInput's decode is the more precise of the two. **[agrees]** |
| The `0x2f` payload | FlexInput's quoted "working reference implementation" frame `0c 91 01 02 00 04 00 00 2f 00 00 00` appears **byte-for-byte** in this project's Pro/GC init (`testapp.cpp:714`, `:738`). This is very plausibly that reference. |
| **Discovery** | 🚩 FlexInput will **never see an M12 Pro**. It requires VID `0x057E` *and* a known PID; the fork accepts any PID and asks the user for the side. |
| Buttons | The console report carries a **16-bit bitmap at `0x02`** with a bit order that is not Nintendo's. FlexInput has the Nintendo table only. |
| Report length | The fork had to lower its decoder gates to **6 / 7 / 16 bytes**. `parse_input`'s 8-byte gate is fine; `parse_common_input`'s 60-byte gate is not. |
| Rumble | The fork **encodes HD rumble**; FlexInput passes 16 raw bytes through and states it is "not encoded yet". Lift the frames. |
| Rate | The 800 Hz tick is the **sensor** clock, not a report-rate promise: one fused sample per report, and each report states its own span in ticks (timestamp bits 12-19) — mean **4.00** on a 200 Hz link ⇒ 4 ticks per report. **[measured]** (§12) |
| Temperature | **None on the M12-S.** Every unused byte of the per-side report is a constant zero under every mask tried. Retail Joy-Cons keep it at `0x2E` in the common report, which the M12-S never streams. The fork reads none either, and its README example is misaligned. **[measured]** (§5) |
| Pairing, dongle stack, USB, calibration reads, keep-alive, magnetometer work, orientation fusion | **FlexInput only.** The fork has none of it. |

---

## 3. GATT cross-reference

Naming trap: the fork's `RUMBLE_CHAR` is FlexInput's **rumble+command** char (0x0016) and
the fork's `VIBRATION_CHAR` is FlexInput's **rumble-only** char (0x0012). The names are
opposite to what they suggest; reading them naively sends commands to a rumble motor.

| Handle | Role | Fork constant | FlexInput constant | UUID |
|---|---|---|---|---|
| `0x0003` | vendor service (prelude) | — | `SVC_PRELUDE` | `00c5af5d-1964-4e30-8f51-1956f96bd280` |
| `0x0005` | prelude WRITE, `01 00` first | `SESSION_CHAR_UUID` | `CHR_PRELUDE_WRITE` | `00c5af5d-…-1956f96bd282` |
| `0x0008` | main service | — | `SVC_MAIN` | `ab7de9be-89fe-49ad-828f-118f09df7fd0` |
| `0x000A` | **common** input, report 0x05 | `INPUT_REPORT_UUID` | `CHR_INPUT_COMMON` | `ab7de9be-…-118f09df7fd2` |
| `0x000E` | **per-side** input, 0x07 (L) / 0x08 (R) | `NATIVE_INPUT_UUID_L` / `_R` | `CHR_INPUT_L` / `CHR_INPUT_R` | L `cc1bbbb5-7354-4d32-a716-a81cb241a32a`, R `d5a9e01e-2ffc-4cca-b20c-8b67142bf442` |
| `0x0010` | vendor "Set Report Rate?" descriptor on the input char | `NATIVE_ENABLE_DESC_UUID` | `DSC_REPORT_RATE` (payload `85 00`) | `679d5510-5a24-4dee-9557-95df80486ecb` |
| `0x0012` | rumble-only WRITE-NO-RESP | `VIBRATION_CHAR_UUID_L/_R` | `CHR_RUMBLE_L/_R` | L `289326cb-…`, R `fa19b0fb-…` |
| `0x0014` | bare command channel | `WRITE_COMMAND_UUID` | `CHR_CMD_BASIC` | `649d4ac9-8eb7-4e6c-af44-1ea54fe5f005` |
| `0x0016` | rumble + command channel (17-byte prefix) | `RUMBLE_CHAR_UUID_L/_R` | `CHR_RUMBLE_CMD_L/_R` | L `ce49a830-…`, R `65a724b3-…` |
| `0x001A` | command responses for `0x0014` | `CMD_RESPONSE_UUID_BASIC` | `CHR_CMD_RESP_BASIC` | `c765a961-…` |
| `0x001E` | command responses for `0x0016` (header at +`0x0F`) | `CMD_RESPONSE_UUID_L/_R` | `CHR_CMD_RESP_EXT_L/_R` | L `63a3810f-…`, R `640ca58e-…` |
| `0x0022` | unknown notify (enabled and discarded) | (caught by the `extraNotifyChars` catch-all) | `CHR_UNKNOWN_NOTIFY` | `d3bd69d2-841c-4241-ab15-f86f406d2a80` |

Fork-only, for Pro Controller 2 (`506d9f7d…` / `3dacbc7e…` / `cc483f51…`) and the NSO
GameCube pad (`46f6ad29…` / `af95885e…` / `3f8fb670…`). Out of scope for FlexInput
(`lib.rs`), so these are the only attribute UUIDs FlexInput does not have.

Both halves are separate peripherals, so L and R characteristics carry the **same handles**
on their own links.

**What each project treats as primary:** the fork subscribes `0x000A` (common) first and
falls back to `0x000E`; FlexInput subscribes only `0x000E` and documents `0x000A` as
refusing both read and notify on an M12-S (`ATT error 0x02`), with `0x0014` accepted-but-inert
(`BLUETOOTH_TRANSPORTS.md:124-147`) **[measured]**.

---

## 4. Init sequence and the console session

### Command framing
Identical in both: `[cmd][0x91][0x01][sub][00][len][00][00]` + data, prefixed with
17 zero bytes on `0x0016` (report id + 16-byte HD rumble) and nothing on `0x0014`.
Pro uses a 33-byte prefix and GC 13 **[fork]** — FlexInput does not handle those pads.
Responses land at `0x001E` with the header at offset `0x0F`; FlexInput's
`parse_response` validates the command id and falls back to scanning, which is the more
robust of the two (the fork has no response parsing at all — it sleeps 80 ms per command
and judges success purely by whether ≥3 full-size notifications arrived).

### Feature mask — the one real disagreement
- The fork's **Joy-Con 2** init sends `0x37` (buttons|sticks|IMU|**mouse**|rumble, no
  `IMU_RAW`), and its **Pro / GC** inits send **`0x2f`**.
- FlexInput settled on **`0x2f`** with `MOUSE` off
  (`protocol.rs:270`, test at `:562`) because `IMU_RAW` (0x08) is what turns the standard
  accel/gyro block on, and because `MOUSE` on (0x3f) **broke retail Joy-Con 2 reporting**
  **[measured]**.
- The fork hedges: its ladder tries `0xFF` and `0x2F` as fallback attempts
  (`ActivateInputStream` :918-929) after the default `0x37`.
- ⭐ byte-for-byte, `0c 91 01 02 00 04 00 00 2f 00 00 00` — the payload FlexInput's
  `protocol.rs:228` quotes from "a working reference implementation" — is exactly
  `testapp.cpp:714` (Pro) and `:725`. `trevlars/switch2-controllers-linux` is the other
  name FlexInput cites; both are consistent with this project.

### The fork's init ladder (`ActivateInputStream` :911)
1. `0x0016` with the 17-byte prefix, default mask → wait 1500 ms for ≥3 packets of ≥60 B.
2. `0x0014`, bare, default mask.
3. `0x0014` with mask `0xFF`, then `0x2F` (order swapped for Pro/GC).
4. **Between the first and second attempt**, if the pad is Joy-Con-like and the per-side
   char exists: `TryConsoleSession` (:821).

### `TryConsoleSession` — the M12 Pro recipe, step by step
1. Write **`01 00`** to the prelude/session char (`0x0005`). → FlexInput already does this
   (`hub.rs:964`, `WriteType::WithResponse`).
2. Run the full **Joy-Con 2** init on `0x0016` (17-byte prefix), falling back to `0x0014`.
3. Write **`85 00`** to the vendor descriptor `679d5510…` **on the per-side input char**.
   → FlexInput already does this on that same char (`hub.rs:1130`, `dongle.rs:764`).
4. Enable **notify (CCCD)** on the per-side input char, 3 retries at 300 ms.
5. Wait 2500 ms for ≥3 packets. If they arrive, mark `consoleMode = true` and start
   repacking every notification through `ConsoleToGenuine`.

**So FlexInput already performs steps 1-4.** What it lacks is (a) the will to accept the
pad in the first place, (b) the `0x0014` fallback rung, and (c) a decoder for the report
that arrives once it works.

### Steps the fork does not do
No pairing (`0x15` LTK exchange), no SMP, no controller-memory reads decoded (it sends
them), no keep-alive, no safe-mode handling, no connection-parameter negotiation beyond
`RequestPreferredConnectionParameters(ThroughputOptimized)`. Its log warns: *"the pad may
require Windows-level Bluetooth pairing first"* — it leans on the Windows stack and gives
up when the pad refuses it.

---

## 5. Report layouts

### The M12 Pro "console" report — as `ConsoleToGenuine` reads it **[fork]**

| Offset | Content |
|---|---|
| `0x00..0x01` | Timer (16 low bits of the device clock; copied into the synthetic report's `0x00..0x01`) |
| `0x02..0x03` | **Buttons, u16 LE** — 16-bit bitmap, not Nintendo's bit order (§6) |
| `0x05..0x07` | Own stick, 3-byte 12-bit packed (`x = b0 \| (b1&0x0F)<<8`, `y = b1>>4 \| b2<<4`) |
| `0x09..0x0A` | Mouse **dx, i16** — per-report delta, must be accumulated (§9) |
| `0x0B..0x0C` | Mouse **dy, i16** |
| `0x0E` (L) / `0x0F` (R) | Motion block length; the fork requires `≥ 0x1C` |
| `0x0F` (L) / `0x10` (R) = `base` | Motion block, below |
| `base+0..1` | Tick, bits 0-11, **1.25 ms units** |
| `base+3` | Must be **`0x0C`** — the fork's block-validity tag |
| `base+4` | Bits 0-1 = which quaternion component is the largest |
| `base+5..+16` | Quaternion, 3 × u32 → three 25-bit fields (§7) |
| `base+0x12 / +0x16 / +0x1A` | **Accel, i16 ×3** |
| Minimum length | `base + 0x1C` (≈ 0x2B / 0x2C bytes) |

⭐ `base+0x12 / 0x16 / 0x1A` **is** FlexInput's measured `0x22 / 0x26 / 0x2A` for the right
half, and the one-byte-earlier left. Two independent hands, same offsets, and the fork
also reads them as **i16** (matching the "i16 + two padding bytes, NOT i32" correction in
`reports.rs:413-417`). The mouse at `0x09/0x0B` is also FlexInput's `OFF_MOUSE`.

### The synthetic report the fork builds (its target = the **common** report 0x05)

| Written at | Content |
|---|---|
| `0x00..0x01` | Timer, copied through |
| `0x04..0x06` (L) / `0x03..0x05` (R) | Buttons, remapped 3-byte block (per-side **one-byte shift**, per-side bit map) |
| `0x0A..0x0C` (L) / `0x0D..0x0F` (R) | The half's own stick |
| `0x0D..0x0F` (L) / `0x0A..0x0C` (R) | The *other* stick, filled with `00 08 80` = `(x 0, y 2048)` |
| `0x10..0x13` | Accumulated mouse **position** (u16 ×2) |
| `0x29` | **`0x01` — marker byte** (see below) |
| `0x30..0x35` | Accel, axis-swapped `(x, z, −y)` |
| `0x36..0x3B` | Gyro, from the quaternion delta, ×133.333 counts per °/s |

### The report-validity gate the fork has and FlexInput does not
`JoyConDecoder.cpp:220-296` — a motion block is only accepted when:
- `buffer.size() ≥ 0x3C`, **and**
- **`buffer[0x29] == 0x01`**, **and**
- not all of `0x30..0x3B` is zero.

That marker byte is a cheap, testable validity rule for the standard motion block
**[hypothesis → worth confirming on the per-side report too, where FlexInput's
`motion_len == 30` gate lives]**.

### Length gates, side by side

| Parser | Gate |
|---|---|
| fork `GenerateDS4Report` | ≥ 7 bytes (L) / ≥ 6 bytes (R) — **lowered from `0x3C` by the M12 Pro commit** |
| fork Pro / NSOGC report | ≥ 16 bytes (lowered from `0x3C`) |
| fork motion | ≥ 60 bytes |
| FlexInput `parse_input` | ≥ 8 bytes; motion needs `motion_len == 30` **exactly** and `len ≥ 0x2E` |
| FlexInput `parse_common_input` | ≥ 60 bytes |

The "equality, not a floor" rule in `parse_input` is FlexInput's guard against retail
halves interleaving `motion_len` `0x1e` and `0x28` frames **[measured]** — keep it. The
fork has no such rule; it accepts any ≥60-byte buffer.

### Temperature — absent on the M12-S **[measured]**

Checked 2026-11-25. The question was whether warm-up gyro drift (faster while warming in
the hand, slower once the pad cools on the table) could be corrected against a temperature
reading.

- **Per-side report, every byte:** the October raw captures (`jc2_raw_*`, both halves, masks
  `0x2f` / `0xaf` / `0x80`, 1.8k–17k reports each) show every byte outside the known fields
  as a **constant `00`**. That covers `0x08`, `0x0E`, the two bytes before the accel
  (`0x20-21`), the pad bytes after each accel axis, and the whole tail `0x2E..0x3E`. The
  encrypted mask sweep (2026-10-14) had already found nothing past byte 45 under any mask.
- **Where it would be:** the common report puts temperature at `0x2E-0x2F`, just ahead of the
  standard accel/gyro block. On the per-side report that whole tail stays zero, because the
  M12-S ignores `IMU_RAW`. The common report on `0x000A` is the one the M12-S never sends
  (§3).
- **The timestamp's upper bits are a span, not a sensor** — see §12.
- **Battery byte:** constant `0x14` in every capture, so it cannot stand in for heat either.
- **The fork:** its decoder reads nothing past the accel (min length `base + 0x1C`). The
  README's worked example, "`0x2E` = `5f 0e` → 3679 → ~54 °C", is read **16 bytes off**: `5f 0e`
  sits at `0x1F` and is the battery voltage, 3679 mV. The reference capture actually has
  **`05 00`** at `0x2E`, which by the README's formula `25 °C + raw / 127` is **~25.0 °C**. That
  is plausible for a pad on a desk, but it is one sample of a retail pad's common report, so the
  formula stays **[fork]**.

⇒ On the M12-S, warm-up drift cannot be corrected against a temperature reading; it has to
be inferred from the motion data. The usable signal is the gyro bias on the axes gravity can
see (the two tilt axes ⊥ gravity) while the pad is held. Retail Joy-Con 2 (common report) or
an M12 Pro that serves `0x2E` would change this — check it in the §13 capture.

---

## 6. Button maps

### The M12 Pro console bitmap (u16 LE at `0x02`) **[fork]**

| Bit | Left half | Right half |
|---|---|---|
| 0 | D-pad Down | Y |
| 1 | D-pad Left | X |
| 2 | D-pad Right | B |
| 3 | D-pad Up | A |
| 4 | Minus | Plus |
| 5 | Stick press | Stick press |
| 6 | Capture | Home |
| 7 | SR | C |
| 8 | SL | SR |
| 12 | — | SL |
| 14 | ZL | ZR |
| 15 | L | R |

Note the d-pad runs **Down, Left, Right, Up** as bits 0-3 on the left half — not
sequential clockwise. Also note the L/R halves use **different bit numbers for the same
physical button** (stick press 5 on both; shoulder pair 14/15 on both; everything else
differs), so a single shared table is not possible.

### joycon2cpp's own 24-bit button state map (genuine pads)
State is read as `b3<<16 | b4<<8 | b5` (right) / `b4<<16 | b5<<8 | b6` (left),
`JoyConDecoder.cpp:300-322`, `:404-405`, `:788-792`:

| Left | mask | Right | mask |
|---|---|---|---|
| Down | `0x000001` | Plus | `0x000002` |
| Up | `0x000002` | Y | `0x000100` |
| Right | `0x000004` | B | `0x000200` |
| Left | `0x000008` | X | `0x000400` |
| rightShoulder (L) | `0x000010` | A | `0x000800` |
| leftShoulder (ZL) | `0x000020` | Stick press | `0x000004` |
| Minus | `0x000100` | Home | `0x000010` |
| Stick press | `0x000800` | Capture | `0x000020` |
| Home | `0x001000` | C | `0x000040` |
| Capture | `0x002000` | rightShoulder (R) | `0x004000` |
| C | `0x004000` | leftShoulder (ZR) | `0x008000` |

### FlexInput's map (per-side report, bytes at `0x02`/`0x03`)
`reports.rs:2867-2900`. Left byte0: down `0x01`, right `0x02`, left `0x04`, up `0x08`,
shoulder `0x10`, z `0x20`, minus `0x40`, stick `0x80`; byte1: capture `0x01`, sr `0x40`,
sl `0x80`. Right byte0: b `0x01`, a `0x02`, y `0x04`, x `0x08`, plus `0x40`, stick `0x80`;
byte1: home `0x01`, c `0x10`, sl `0x80`, sr `0x40`.

🚩 **These three tables are not interchangeable.** FlexInput's is the ndeadly table for the
**per-side** report at `0x02`. joycon2cpp's is for the **common** report at `0x03/0x04` with
a per-side shift. FlexInput's `parse_common_input` reads a 2-byte window at a **fixed
`0x04` for both sides** (no per-side shift), where joycon2cpp reads 3 bytes **with** a
per-side shift and per-side bit meanings. One of those two is wrong about the common
report, and FlexInput has never run `parse_common_input` on hardware (the common stream
refused on the M12-S). **Settle this with one button-press capture before wiring the
common report into anything.**

---

## 7. The quaternion, decoded twice

The same 12 bytes: FlexInput's `OFF_MOTION_PROBE = 0x14` (R) = the fork's `base+4`.

### FlexInput — `decode_motion_quat` (`reports.rs:570-594`)
`[tag1][f1 24-bit LE][tag2][f2][tag3][f3]`
- `tag1` bits 0-1 = which component is largest (0 = w, 1 = x, 2 = y, 3 = z).
- The three fields are the **next three components in cyclic order**, divided by the
  largest, as offset binary: **f1 = 26 bits** (`tag2` bits 0-1 on top), **f2 = 25 bits**
  (`tag3` bit 0 on top), **f3 = 24 bits**.
- Reassembly: `[1.0, f1, f2, f3]` placed cyclically at `largest`, then **normalised by the
  vector norm** — exact division-by-largest.
- Validity: tag shape (`b[0]&0xFC == 0`, `b[4]&0xFC == 0`, `b[8]&0x7E == 0`), so a pad
  sending something else returns `None` and falls back.
- Verified on five guided M12-S captures (2026-10-10): gravity held to **0.7-1.0°**,
  full turns integrate to 361-366°, flips to 357°, no step over 4°, tag-regime switches
  seamless. **[measured]**

### The fork — `M12ProQuatToGyro` (:1704)
`idx = buf[base+4] & 3`; three `u32` at `base+5/9/13`;
`f0 = (w0>>1) & 0x01FFFFFF`, `f1 = w1 & 0x01FFFFFF`, `f2 = ((w2<<1) | (w1>>31)) & 0x01FFFFFF`
— three **25-bit** fields. `v = (f − 2²⁴) / (2²⁴·√2)`, reject `Σv² > 1.05`,
`d = √(1 − Σv²)`, then place per `idx`:

| `idx` | `[w, x, y, z]` |
|---|---|
| 0 | `d, v0, v1, v2` |
| 1 | `v2, d, v0, v1` |
| 2 | `v1, v2, d, v0` |
| 3 | `v0, v1, v2, d` |

### The arithmetic differences, and which is right
- Field widths: the fork fixes 25/25/25; FlexInput's measured 26/25/24. FlexInput's `f1`
  is the fork's `f0 × 2`, and its `f3` is the fork's `f2 / 2` — so **the fields agree to
  within one bit**. The fork spends `base+12 bit 7` as the LSB of `f3`; FlexInput's comment
  records that bit as *"toggles freely and carries nothing"*. FlexInput wins that one
  **[measured]**.
- Scale: the fork divides by `2²⁴·√2` (so stored components ∈ ±0.707); FlexInput divides by
  `2ⁿ` (components = true ratio to the largest). **The fork's components are FlexInput's ÷ √2.**
- Reconstruction: the fork's `√(1 − Σv²)` is **only exact when the omitted component is
  exactly `1/√2` or `1`** (solving `2L⁴ − 3L² + 1 = 0`). Mid-range it carries a
  normalisation error of a few percent. FlexInput's normalise-by-norm is exact. ⭐ **If
  anything is taken from the fork's motion path, it is not the quaternion decode.**

### Rate derivation
- **Fork:** `q⁻¹ ⊗ q` between consecutive reports → angle-axis, `dt = dticks / 800`, axis
  swap `{wx, wz, −wy}`, **0.6 °/s deadzone**, EMA `α = 0.85`, then ×133.333 into i16 at
  `0x36..0x3B`. `dticks == 0` ⇒ reuse the last rate; `dticks > 400` (0.5 s) ⇒ resync.
  Opt-in via `g_fg.enabled`, with gain% and per-axis invert in the UI.
- **FlexInput:** `update_from_quat` (:2103) with a **learned resting quat bias**, gravity
  alignment from the accelerometer, `MAX_STEP` rejection, magnetometer-corrected heading,
  its own orientation quaternion output, and `TickClock` measuring seconds-per-tick from
  the stream. No deadzone (a learned bias beats one).
- ⭐ Both handle **`dticks == 0`** — a report that carries **no new IMU batch** (the pad
  re-sends the fused orientation). Independent confirmation; FlexInput's `TICK_MIN = 1`
  covers it (:2003).

---

## 8. Contested numbers to settle

Two places where the fork and FlexInput state incompatible facts, both **cheap to settle
with one guided rotation**.

**1. Gyro scale.**
- Fork/README: `48000 counts = 360 °/s` → **133.33 LSB per °/s** (⇒ ±245 °/s full scale).
- FlexInput `GYRO_LSB_PER_DPS = 1/0.07 = 14.286` (⇒ **±2000 °/s**), taken from command
  `0x11`'s reply, and `reports.rs:340-351` argues explicitly that 48000/360 "is off by a
  factor of nine". `jc2_imu.rs:2301` reports **three independent sweeps agreeing on 0.07
  °/s/LSB** **[measured]**.
- ⚠️ The fork's number is not a measurement — it is the constant it multiplies its
  °/s by to fill the DS4 axis. **FlexInput's figure is the better-evidenced one.**
- Test: rotate 360° at a steady rate, integrate the raw standard-block gyro counts, divide
  by 360. One run decides it.

**2. Sensor axis frame.**
- Fork writes accel/gyro as **`(x, z, −y)`**.
- `to_canonical_accel` is **`(y, −x, z)`** (raw axis 0 = side and inverted, 1 = forward,
  2 = vertical) — measured against DualSense and Switch Pro on the same motion
  **[measured]**, `reports.rs:895-913`.
- Both are proper rotations; they differ by 90° about the canonical z. They **cannot both
  be right**, and they were measured on **different products** (M12 Pro vs M12-S). ⇒
  Expect a **per-model mount table**, not one constant, once more than one clone is
  supported.

---

## 9. Mouse / optical

**Fork** (:1853-1870, :1922-1980): reads `dx, dy` as **i16 deltas per report** at
`0x09/0x0B`, **accumulates them into a running position** in `int32` with 16-bit wrap,
writes that at `0x10..0x13`, then the pointer path differences it (with wrap), applies
one of four sensitivity modes (OFF / FAST 1.0 / NORMAL 0.6 / SLOW 0.3), and calls
`SendInput`. **R → left click, ZR → right click, stick press → middle**, and **stick Y
drives the scroll wheel** (deadzone 4000, 120 units per notch). Mode cycling is on the
**C button** (`0x000040`), announced by the player LEDs and a `0x0A/0x02` sample.

**FlexInput:** `MouseTracker` (`reports.rs:2622-2740`) **auto-classifies** the field as
delta vs absolute (too-large value ⇒ absolute; 30 identical non-zero samples ⇒ absolute;
`(0,0)` after motion on a surface ⇒ delta) and publishes a **held velocity** in mouse units
plus a debounced `optical_contact` flag. Pins are only offered when
`feature::MOUSE_STREAMED` is true — and it is **false**, because `MOUSE` is off in
`0x2f` after it broke retail reporting. So today FlexInput **discards the mouse block
entirely** (`parse_streamed`).

⭐ The fork's need to **accumulate** is evidence for the **Delta** side of FlexInput's
disputed encoding, on a pad that streams it **without the `MOUSE` feature bit set** — the
fork's Joy-Con init uses `0x37` (mouse on) but its console session's `ProCon2`-style
masks are `0x2F`, and mouse data flows. Worth checking whether `MOUSE` is required at all
before it gets switched back on.

The fork has **no distance/contact field** in the console path — it never reads `0x0D`, so
the "0 = on a surface, ≈12 = lifted" reading stays FlexInput's alone **[measured]**.

---

## 10. Rumble

FlexInput passes a raw 16-byte payload to `0x0012` and states HD rumble *"is declared in
the layout but not encoded yet"* (`joycon2_backend.rs:545`). The fork encodes it. Lift
this.

**Halves** — 42-byte frame to the vibration char (`0x0012`), `SendRumble` (:976):
```
[0]=0x00  [1]=0x50 | (counter & 0x0F)   [2..6]=profile   rest zero
strong profile: 93 35 36 1C 0D
weak   profile: 4B 7D 80 5A 02          (chosen at amplitude > 0.5)
```
`0x50 | counter` = HF frequency + start/stop flags with a rolling 4-bit id. Only
**two** amplitude levels, polled every 4 ms off the ViGEm output report; one silent frame
sent on the falling edge.

**Pro Controller 2** — 58-byte frame, **two** 17-byte motor blocks at offsets 1 and 17
(`SendProRumble` :1016). **NSO GameCube** — 5-byte `[00 01 amp]` (`SendGCRumble` :1003).
`0x0A/0x02 [sampleId]` plays a canned sample (`0x04` = the click the fork uses for mode
changes).

FlexInput's init already sends the `0x0A/0x08` "send vibration data" frame verbatim
(`hub.rs:1087-1094`), so the encoding context is shared.

---

## 11. Discovery and identity — the blocker

**FlexInput** `[measured]`:
- `hub.rs:550-563` `identify()` — requires manufacturer data under company id **`0x0553`**,
  **VID `0x057E`**, and `Side::from_pid(pid)` ∈ {`0x2066` R, `0x2067` L, `0x2070`/`0x2071`
  safe mode}. Anything else ⇒ `None` ⇒ the pad is never seen.
- Same gate on the dongle path: `dongle.rs:683-698`.
- The **side is derived from the PID** — used for the characteristic set, the button map,
  and the device id slug.

**The fork** (`testapp.cpp:1115-1147`):
- Manufacturer data must start `01 00 03 7E 05` ⇒ **VID pinned to `0x057E`** (the fork
  requires Nintendo's VID too).
- `PidMatchesKind`: `known = pid ∈ {0x2066, 0x2067, 0x2069, 0x2073}`;
  **`if (!known || kind == Any) return true`** — an **unknown PID is accepted for every
  kind**.
- Fallback match on the advertisement **local name**: `joy-con`, `joycon`,
  `pro controller`, **`mobapad`**, `nintendo`.
- **The user picks the side in the UI**, because the PID does not identify it.

🚩 **Consequence:** the fork's design only needs a user-chosen side if the pad's PID is
outside the known set. If so, **FlexInput rejects the M12 Pro during discovery** and no
amount of decoding work below that gate will ever run. This is the first thing to fix, and
the only one that is a hard blocker.

Supporting data to collect when touching this: the M12 Pro's advertised **PID**, name
string, RSSI, and which GATT attributes it exposes. `crates/devices/src/bin/joycon2_probe.rs`
and `crates/btle/src/bin/jc2_link.rs` are the existing tools built for exactly that.

---

## 12. Report rate, ticks, and going above 200 Hz

**Two clocks. Don't conflate them.**

- The **tick** is the IMU/fusion clock: 1.25 ms (800 Hz), 12 bits, wraps at 4096 with no
  carry **[agrees]**.
- The **notification** is the link clock. Each per-side report carries **one** 30-byte
  motion block, and the timestamp's **bits 12-19 hold the ticks since the pad's previous
  report**: its own count of the span this report covers **[measured]**, see
  `Motion::tick` in `reports.rs`. Byte 3 is a constant `0x0C`.
  - Checked against the measured tick difference over the 19-min warm-up capture
    (2026-10-13, ~400k reports): equal in **every** left-half report and in all but **4 of
    228 218** right-half reports. In those 4 a report is missing from the capture: the field
    reads one report's span (4) while the tick jumped 8. A mismatch is therefore a
    **lost-report detector** (unused so far).
  - ⚠️ This file and `reports.rs` used to say "bits 15-16 count how many IMU samples the block
    covers (2 bits ⇒ ≤ 4)". That had the wrong position and the wrong meaning; spans up to
    60 ticks were observed.
- So at 200 Hz each report spans **4 ticks** (right half, single strong link: mean
  **4.001**, 88% exactly 4, the rest 3/5). The left half, sharing the radio, averaged
  **5.30** (~151 Hz) with spans of 8, 9, 13 and 16. The pad's IMU/fusion runs on the 800 Hz
  tick and the link samples it, so **800 Hz is sensor headroom, not report headroom**. That
  the IMU *samples* at 800 Hz is still inference; the tick rate is measured.
- What the tick buys is `dt` precision (1.25 ms, immune to link jitter — the reason
  `TickClock` measures seconds-per-tick from the stream rather than trusting host time).
  The fork also handles a report that carries **no** new tick (`dticks == 0`, re-sent
  orientation). That never happened in 400k M12-S reports, and FlexInput's `TICK_MIN = 1`
  rejects it.

**What sets the report rate** — FlexInput's own tree says it two ways, and both are real:
- `btle/lib.rs:1331-1345`: *"one input report arrives per connection event, so 15 ms is
  66 Hz and 5 ms is 200 Hz"*. Connect asks **4 units (5 ms)** first, falls back to **6..12
  (7.5-15 ms)**. Measured **133-200 Hz** (`reports.rs:621`) ⇒ 133 = 7.5 ms, 200 = 5 ms.
- `reports.rs:2048-2063`: *"BLE delivers several notifications in a single connection
  event, so reports arrive in bursts"* — one carries the whole inter-event gap, the rest
  land microseconds behind it. Excluding those bursts from the tick ratio made the tick N×
  too long and every rate N× too small.
- Max **connection-event length is capped at 6 units = 3.75 ms on purpose** (`lib.rs:1387-1400`)
  to leave airtime for the second half: single link steady **200 Hz**, two halves ⇒ one
  drops to **~156 Hz** (dropped reports, and the heading is quantised, so irregular drops
  make the gyro jagged).

**Levers, ranked:**
1. **Connection interval: spent.** 200 Hz = 5 ms, already **below the 7.5 ms LE floor**
   (`hci.rs:45-50`: "the controller rejects less"; the console reaches 5 ms with a vendor
   command). 400 Hz = 2.5 ms is not a legal LE interval. **Connection subrating (BT 5.3)
   can only stretch intervals** (multiplier ≥ 1) — no upward path. FlexInput is already at
   the Switch 2 console's own rate.
2. **Multiple reports per event: your cap is the lever to cut.** For a single-half setup,
   raise max CE length from 6 units to `0xFFFF` and measure. Airtime: 63 B + ATT/L2CAP/MIC
   ≈ 80 B on air ≈ 0.64 ms at 2M PHY + ~150 µs IFG ≈ 0.8 ms per notification ⇒ a 5 ms
   event carries ~5 ⇒ **~1000 Hz of radio capacity**; 400 Hz × 2 halves ≈ 12% duty.
   **Airtime is not the wall. Firmware is.**
3. ⭐ **The vendor descriptor nobody has turned: `679d5510…`, payload `85 00`.** Documented
   as "Set Report Rate**?**" (`protocol.rs:126`) and the probe calls the label *"a GUESS"*
   (`jc2_imu.rs:4537`): `0x85 0x00` is the **only value ever written** by FlexInput, the
   fork, and (per the fork's README) the reference tools.
   **[hypothesis]** `0x85` = **133**, which is exactly the rate a 7.5 ms link yields. If
   the field is a **u16 LE Hz** value, the sweep is `85 00` (133) → `90 01` (400) →
   `20 03` (800), plus a linear sweep of `0x20..0xFF` low byte. The harness exists:
   `write_report_rates` (`jc2_imu.rs:4924`) writes it to every matching descriptor, and
   `flexinput_btle::cadence::Cadence` reads the resulting cadence. **30-minute experiment,
   highest expected payoff.**
4. **Commands `0x10` / `0x11` / `0x16`.** `0x11`'s reply is where the 0.07 °/s/LSB figure
   came from ⇒ 0x11 carries the IMU's configured range/rate, so the ODR is plausibly
   settable/queryable there, and only subs `0x03` / `0x01` have ever been sent. Capture
   full reply payloads of 0x10/0x11/0x16 and diff against the ndeadly docs.

**What >200 Hz would actually buy:** latency and pointer/aim smoothness — **not** gyro
signal-to-noise. The per-side report has no raw gyro, so the rate is a **difference of a
fused orientation**; differencing over 1 tick instead of 4 raises the fusion noise ~2×
(√4:1), while field quantisation stays negligible (25-bit ⇒ LSB ≈ 3e-8 ⇒ ~0.003 °/s at
1.25 ms). The limit is the fusion's own noise, not the encoding.

**Retune checklist if the stream ever gets faster** — every one of these was tuned on a
133-200 Hz stream, and the 67→200 Hz jump already broke three of them (recorded in the
comments): `FIELD_GAIN`, `ORIENTATION_GAIN`, `MAX_STEP`, the `QuatBias` window, the
stillness detector (`reports.rs:4988`), `REPORT_HZ` (fallback seed only), the mouse
`NOMINAL_DT`/`STALE_S`, and the transport thread budget (`dlog.rs:56`).

**First measurement — done (2026-11-25, from the 2026-10-13 capture):** mean span **4.00**
ticks per report on the 200 Hz right half. So the tick is 1.25 ms and a report every tick
would be 800 Hz: a 400 Hz link is physically there for the taking, as far as the clock is
concerned. The alternative it ruled out was 12 ticks per report (a 2.4 kHz tick).

---

## 13. Actionable list, in order

1. **Accept the pad at all.** VID `0x057E` + unknown PID, and a side source that is not the
   PID (UI choice, which input characteristic streams, or the report shape).
   `hub.rs:550-563`, `dongle.rs:683-698`, `identification.rs:49`.
   *Payoff: everything below becomes reachable. Effort: small. Risk: must not start
   claiming every Nintendo-VID device in range.*
2. **Capture one M12 Pro**: advertised PID, name, GATT dump, ≥20 raw reports at rest +
   during a 360° rotation + during a mouse sweep. Also check whether `0x2E-0x2F` is ever
   non-zero, ideally from a cold start into a warm hand — a live temperature there would
   allow real thermal drift compensation, which the M12-S cannot offer (§5).
   `joycon2_probe.rs`, `jc2_link.rs`, `jc2_imu.rs` capture mode.
   *Payoff: turns every `[fork]` claim in this file into a `[measured]` one.*
3. **A console-report decoder**, as a third entry point beside `parse_input` /
   `parse_common_input`: 16-bit bitmap at `0x02` with the §6 per-side table, one stick at
   `0x05`, mouse deltas at `0x09/0x0B`, ≥7-byte gate, no battery byte at `0x01` (do not
   publish `battery`/`charging` from a console report — it will read garbage).
4. **Report-shape gate:** test `0x29 == 0x01` (+ "not all of `0x30..0x3B` zero") against
   the standard block, next to the existing `motion_len == 30` equality.
5. **Settle the two contested numbers** (§8): gyro scale 133.33 vs 14.286; axis frame
   `(x, z, −y)` vs `(y, −x, z)`. One guided 360° turn each. Expect a **per-model** mount
   table.
6. **Sweep the report-rate descriptor** (§12 lever 3), then probe `0x10/0x11/0x16`.
7. **Encode HD rumble** from §10's frames; the amplitude→profile pair (`0x93 35 36 1C 0D` /
   `0x4B 7D 80 5A 02`) and the `0x50 | counter` header are liftable as-is.
8. **Mouse:** keep the auto-classifier, note the fork's accumulation as Delta evidence, and
   test whether the pad streams mouse with `MOUSE` **off** before switching the bit back
   on (it broke retail reporting — `protocol.rs:264-269`).
9. **Keep what the fork lacks.** Do **not** adopt its quaternion decode, its no-pairing
   session, its 80 ms sleep-based init, or its `≥60 bytes ⇒ motion` gate.

---

## 14. Open questions and traps

- **Which button map is right for the common report 0x05?** Per-side 1-byte shift +
  per-side bit meanings (joycon2cpp) or a fixed `0x04` for both sides (FlexInput,
  `parse_common_input`)? FlexInput has never run that parser on hardware.
- **Is the M12 Pro's PID Nintendo-like?** Inferred from the fork's design, **not** read
  from an advertisement. Verify before designing around it.
- **M12 Pro ≠ M12-S.** Everything the fork reports is a **different product** from the pad
  FlexInput's whole decode rests on. Console-mode findings do not transfer automatically —
  the M12-S never answered a single command on `0x0014` and `0x000A` refused both read and
  notify **[measured]**.
- **The fork's README layout table is not its code.** It puts the sticks at `0x08/0x0B`
  (german77's notes); `JoyConDecoder.cpp:33` uses **`0x0A/0x0D`**, which is what
  `parse_common_input` uses. **Trust the code, not that table** — and the README's
  accel/gyro scale claims are the same source as the unverified 133.33 in §8.
- **`0x29` lives inside the per-side accel group** (`0x22` accel + padding on the right
  half). As a "marker" it is documented for the **common** report only. Do not apply it to
  `parse_input` without checking.
- **The fork is Windows/WinRT-only** and has no pairing, so its behaviour on a fresh,
  unpaired pad is: warn and fail. FlexInput's `0x15` LTK handshake is the way in.
- Rate-limits in this file are **per-link**; two halves share one radio, and the CE-length
  cap is a deliberate fairness choice, not a bug.

---

## 15. Appendix — verbatim data

### Fork constants
```
JOYCON_MANUFACTURER_ID = 1363        (0x0553, == NINTENDO_MANUFACTURER_ID)
AD prefix check:  d[0]=0x01 d[1]=0x00 d[2]=0x03 d[3]=0x7E d[4]=0x05
                  ⇒ VID 0x057E (LE at d[3..5]), PID = d[5] | d[6]<<8
PID_JOYCON_R = 0x2066   PID_JOYCON_L = 0x2067   PID_PRO = 0x2069   PID_GC = 0x2073
FULL_REPORT_LEN = 0x3C  (a "full" report for the stream-arrival test)
Update policies: LowLatency (per notification) / Balanced120Hz / Legacy60Hz
```
FlexInput's PIDs add the safe-mode pair `0x2070` / `0x2071`, which the fork does not know.

### Fork Joy-Con 2 init, verbatim (`JoyCon2InitCmds` :682, 17-byte prefix on 0x0016)
```
07 91 01 01 00 00 00 00
02 91 01 04 00 08 00 00 40 7E 00 00 00 30 01 00      read 0x40 @ 0x13000
10 91 01 01 00 00 00 00
16 91 01 01 00 00 00 00
0A 91 01 02 00 04 00 00 03 00 00 00                  play canned sample
09 91 01 07 00 08 00 00 01 00 00 00 00 00 00 00      player LEDs
0C 91 01 02 00 04 00 00 37 00 00 00                  feature mask 0x37
02 91 01 04 00 08 00 00 40 7E 00 00 80 30 01 00      read 0x40 @ 0x13080
02 91 01 04 00 08 00 00 40 7E 00 00 40 C0 1F 00      read 0x40 @ 0x1FC040
02 91 01 04 00 08 00 00 10 7E 00 00 40 30 01 00      read 0x10 @ 0x13040
02 91 01 04 00 08 00 00 18 7E 00 00 00 31 01 00      read 0x18 @ 0x13100
11 91 01 03 00 00 00 00
02 91 01 04 00 08 00 00 20 7E 00 00 60 30 01 00      read 0x20 @ 0x13060
0A 91 01 08 00 14 00 00 01 59 09 00 00 FF FF FF FF 35 00 46 00 00 00 00 00 00 00 00
11 91 01 01 00 00 00 00
0C 91 01 04 00 04 00 00 37 00 00 00                  feature confirm
```
Byte-for-byte the memory reads, LEDs, `0x0A/0x08` and feature frames FlexInput sends, with
`0x37` in place of `0x2f`. `ProCon2InitCmds` (:706) is the same with **`0x2F`**, adds a read
of `0x40 @ 0x130C0`, an all-`FF` `0x0A/0x08` payload, and drops `0x10/0x01`;
`NSOGCInitCmds` (:729) uses **`0x27`** and reads `0x02 @ 0x13140`.

`JC2_INIT_MEMORY_READS` (`protocol.rs:395`) is the same set FlexInput sends —
`(0x40,0x13000) (0x40,0x13080) (0x40,0x1FC040) (0x10,0x13040) (0x18,0x13100) (0x20,0x13060)`.

### Reference notification (fork README, from german77) — left Joy-Con, IMU on
```
08670000000000e0ff0ffff77f23287a0000000000000000000000000000005f0e007907000000000001ce7b52010500beffb501ee0ffeff04000200000000
```
The README's own field table for it (packet id `0x00` size 4, buttons `0x04` size 4, sticks
`0x08`/`0x0B`, mouse `0x0E/0x10`, magnetometer `0x16..0x1C`, battery `0x1C..0x20`,
temperature `0x2E`, accel `0x30..0x36`, gyro `0x36..0x3C`, analog triggers `0x3C/0x3D`)
**disagrees with the fork's code** on the sticks (`0x0A/0x0D`). Keep the hex: it is a real
capture and enough to settle the stick offsets analytically without hardware.

Decoded at the offsets the code and `parse_common_input` use (not the README's worked
example, which is read 16 bytes off):

| Offset | Bytes | Reading |
|---|---|---|
| `0x1F-0x20` | `5f 0e` | battery 3679 mV (the README's "temperature 54 °C") |
| `0x29` | `01` | the fork's motion-validity marker |
| `0x2E-0x2F` | `05 00` | temperature 5 ⇒ ~25.0 °C by `25 + raw/127` **[fork formula]** |
| `0x30..0x35` | | accel −66, 437, 4078 (1 g on the vertical axis) |
| `0x36..0x3B` | | gyro −2, 4, 2 (at rest) |

### FlexInput anchors used above
| What | Where |
|---|---|
| Feature mask `0x2f`, `IMU_RAW`, MOUSE-off history | `crates/joycon2/src/protocol.rs:216-286`, test `:562` |
| Prelude `01 00`; report-rate `85 00`; subscribe | `hub.rs:964`, `:1118-1138`; `dongle.rs:764` |
| Init over WinRT (commands, pairing, memory reads, LEDs) | `hub.rs:984-1116` |
| Per-side report layout and gates | `reports.rs:13-19`, `:353-395`, `:2981-3100` |
| Standard block `0x30`/`0x36`, `parse_common_input` | `reports.rs:48-62`, `:2904-2967` |
| Quaternion decode + tag validation | `reports.rs:552-594` |
| Accel i16-not-i32, left one-byte shift | `reports.rs:405-417`, `:370-385` |
| `TickClock`, tick bursts, `TICK_MIN` | `reports.rs:1994-2063`, `:520-529` |
| `to_canonical_accel` | `reports.rs:895-913` |
| `GYRO_LSB_PER_DPS` (14.286) and the 48000/360 rebuttal | `reports.rs:329-351` |
| Mouse tracker / auto-classify, `MOUSE_STREAMED` | `reports.rs:2584-2740`, `protocol.rs:270-275` |
| Optical pins gated on `MOUSE_STREAMED` | `crates/devices/src/layouts.rs:316-360` |
| HD rumble "not encoded yet" | `crates/devices/src/joycon2_backend.rs:536-546` |
| Discovery VID/PID gate | `hub.rs:543-563`, `dongle.rs:683-698`, `identification.rs:49` |
| 5 ms interval, CE-length cap, 200/156 Hz | `crates/btle/src/lib.rs:1325-1400`, `hci.rs:43-56` |
| M12-S: `0x0014` inert, `0x000A` refuses, masks swept | `docs/BLUETOOTH_TRANSPORTS.md:124-147` |
| Rate probe harness (`write_report_rates`) | `crates/btle/src/bin/jc2_imu.rs:4535-4560`, `:4905-4960` |
| Pad probe tools | `crates/devices/src/bin/joycon2_probe.rs`, `crates/btle/src/bin/jc2_link.rs` |

### Upstream references
- `ndeadly/switch2_controller_research` — FlexInput's protocol source (`bluetooth_interface.md`,
  `commands.md`, `hid_reports.md`, `descriptors.md`). ⚠️ Its `hid_reports.md` motion packing
  (`[ts u32][temp u16][accel xyz i16][gyro xyz i16]`) is **wrong for 0x07/0x08 over BLE**;
  FlexInput's measured offsets replace it (`reports.rs:31-35`).
- `TheFrano/joycon2cpp` (upstream of the fork) — where the `0x2f` payload came from.
- `trevlars/switch2-controllers-linux` (`ngc/protocol.py`) — source of the common-report
  layout FlexInput's `parse_common_input` uses, and of the `0x07` feature default that
  conflicts with `0x2f` (`protocol.rs:254-256`).
