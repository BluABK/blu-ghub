# G915 over its LIGHTSPEED receiver: protocol notes

What blu-ghub relies on, checked against a real G915 (firmware MPK 09.04 build
42, receiver C547 firmware 04.02 build 9) with `BluGHub-diag probe` and
`dump-onboard`, and cross-checked against open-source implementations
(Solaar, OpenRGB, libratbag, keyleds). "Probe" marks what this unit answered;
anything without it comes from the references only.

## Transport (Windows)

| Collection | Usage | Reports | Used for |
|---|---|---|---|
| C547 `MI_02&COL01` | FF00:0001 | 7 bytes in/out (id 0x10) | HID++ short: receiver register requests, HID++ 1.0 errors and notifications |
| C547 `MI_02&COL02` | FF00:0002 | 20 bytes in/out (id 0x11) | HID++ long: every HID++ 2.0 request and reply, long register replies |

- A short report written to the long collection (or the reverse) fails; pick
  the collection by report length.
- One request/reply pair can cross both: a long register read (sub-id 0x83) is
  sent short and answered long.
- Every open handle gets its own copy of each input report (probe: a read-only
  sniffer saw all of blu-ghub's replies).
- The keyboard is device index **1** behind the receiver (probe: indices 2-6
  answer HID++ 1.0 error 0x08 "unknown device"). On its own cable it would be
  0xFF on the C33E device.

## Receiver registers (HID++ 1.0, device 0xFF)

| Register | Probe reply | Meaning |
|---|---|---|
| short 0x00 | `00 01 00` | notification flags: wireless notifications on |
| short 0x02 | `00 01 00` | connection state: 1 device |
| short 0xF1 [01..04] | firmware/bootloader versions | |
| long 0xB5 [20 + n] | `20 07 08 40 7C ...` | pairing info: wpid 0x407C (G915) for slot 1 |
| long 0xB5 [30 + n] | serial number | |
| long 0xB5 [40 + n] | name length + "G915 KEYBOARD" | |

## Feature table (probe)

HID++ 4.2. Indices are listed for reference only; code always looks them up
through ROOT, because they differ between firmware versions and between
wireless and wired.

| Index | Feature | Notes |
|---|---|---|
| 1 | 0x0001 FEATURE_SET | |
| 2 | 0x0003 DEVICE_FW_VERSION | 5 entities: bootloader BOT 77.03, firmware MPK 09.04 |
| 3 | 0x0005 DEVICE_NAME | "G915 WIRELESS RGB MECHANICAL GAMING KEYBOARD" |
| 4 | 0x1D4B WIRELESS_DEVICE_STATUS | events on reconnect |
| 7 | 0x1001 BATTERY_VOLTAGE (v3) | fn0 → `[mV hi, mV lo, flags]` |
| 10 | 0x8071 RGB_EFFECTS | see below |
| 11 | 0x8081 PER_KEY_LIGHTING_V2 (v2) | 123 zones |
| 15 | 0x4522 GAME_MODE | **never called** (one reference's "sync" call clears the key list) |
| 17 | 0x8010 GKEY | 5 keys |
| 18 | 0x8020 MKEYS | 3 keys |
| 19 | 0x8030 MR | |
| 20 | 0x8040 BRIGHTNESS_CONTROL | fn0 `00 64 05 03` (max 100), fn1 current |
| 21 | 0x8100 ONBOARD_PROFILES | see below |
| 23, 24 | 0x00C2, 0x00D0 | firmware update; never used |
| 25+ | hidden engineering features | never used |

No 0x1000/0x1004 battery feature, no 0x8070.

## Battery: 0x1001

fn0 returns millivolts (big-endian) and a flags byte: bit 7 external power,
low bits the charge state. Percentage comes from a voltage table (Solaar's,
linear interpolation). The device also sends the same three bytes as an event
(software id 0) when they change.

## Lighting

**Per-key, 0x8081 (volatile).** GetInfo `fn0 [00, page]` returns a 14-byte
bitmap per page (zone = page × 112 + bit). Zones on this unit: 0-111 (HID
keyboard usage − 3: F1-F12 = 55-66), modifiers 104-111, 153 (brightness key),
155-158 (play/pause, mute, next, previous), 180-184 (G1-G5), 210 (logo).
Set functions (OpenRGB, Solaar, OpenLogi, and a G HUB capture agree):

| fn | Params | Per request |
|---|---|---|
| 1 | `[zone, r, g, b] × 4`; a zone `FF` ends a shorter list | 4 zones |
| 2 | `[first zone, (r g b) × 5]` consecutive zones | 5 |
| 5 | `[first, last, r, g, b] × 3` ranges | 3 ranges |
| 6 | `[r, g, b, zone × 13]` one colour | 13 zones |
| 7 | frame end: `[persistence, frame(2), frames to next(2)]` | |

Nothing shows until fn7. Its first byte is the persistence: 0 = RAM only,
1 = also into the keyboard's own memory, which blu-ghub **never sends**.
Replies echo the request (fn1: the zones, unused slots as `FF`).

**Verified (E7), in on-board mode with the stored custom effect running and
software control unclaimed (0x8071 fn5 get → `00 00 00`):** fn1 `[26 FF 00 00
FF]` (Escape red) then fn7 `[00 ...]` turned Escape red and left every other
key and the logo as they were. No claim, no mode change, nothing stored.
Others report the stored layout comes back after a minute idle, on an M key,
or at power-off. Escape is zone 0x26 (HID usage 0x29 − 3).

**Effects, 0x8071.** Two clusters (probe):

| Cluster | Location | Effects (index → id) |
|---|---|---|
| 0 | logo | 0 off, 1 static, 2 cycle (0x0003), 3 breathing (0x000A) |
| 1 | keys | 0 off, 1 static, 2 breathing (0x000A), 3 cycle (0x0003), 4 wave (0x0004), 5 ripple (0x000B), 6 custom (0x000C) |

Effect indices differ per cluster; always look them up. Software control
(fn5 get) reads `00`: the keyboard drives its own lighting. Power config (fn7
get): idle 60 s, sleep 300 s.

**Stored.** The per-key layout survives a power-cycle: the keys cluster runs
the on-board *custom* effect (0x000C), whose per-key data lives outside the
profile sectors below.

## On-board profiles: 0x8100

Description (probe): memory model 01, profile format 04, macro format 01,
3 profiles (+3 in ROM), 8 buttons, 16 sectors of 255 bytes. Mode: on-board
(fn2 = 1). Current profile: fn4 → sector.

Reads: fn5 `[sector hi, lo, offset hi, lo]` → 16 bytes; the last read of a
sector starts at size − 16. Each sector ends in a CRC-CCITT (poly 0x1021,
init 0xFFFF) of the bytes before it, big-endian.

Writes (verified on this unit, through the receiver): fn6 `[sector hi, lo,
offset hi, lo, count hi, lo]` (offset 0, count = sector size), then fn7 with
16 bytes at a time (the last block zero-padded), then fn8 with no parameters
in a **short** report, as libratbag does. A changed button in the active
profile takes effect at once; no reload is needed. blu-ghub only writes a
sector that first read back with a valid checksum, after saving it, and reads
it back afterwards. The mode switch (fn1) is never sent.

**Sector 0, directory:** 4-byte entries `[sector hi, sector lo, enabled, ff]`,
`ff` padded.

**Profile sector (format 04):**

| Offset | Size | Content |
|---|---|---|
| 0 | 32 | settings (report rate etc.); `3c 00 2c 01` at 28 |
| 32 | 8 × 4 | buttons: G1-G5, then M1-M3 |
| 96 | 8 × 4 | the same buttons in the G-shift layer |
| 160 | 48 | name, UTF-16LE, `ff`-filled when unnamed |
| 208 | 4 × 11 | lighting: logo, keys, logo (power-save), keys (power-save) |
| 253 | 2 | CRC |

Button entries (libratbag's encoding):

| Bytes | Meaning |
|---|---|
| `80 02 MM UU` | keyboard key: modifier bits `MM`, HID usage `UU` (`80 02 00 3a` = F1) |
| `80 03 ...` | consumer control |
| `90 0D 00 NN` | select on-board profile NN (how M1-M3 are wired) |
| `00 ...` | macro (sector, offset) |
| `ff ff ff ff` | disabled |

Lighting entries: `[effect id, params...]`, e.g. `0a 00 dc ff 0b b8 01 64` =
breathing, colour 00 DC FF, period 3000 ms, intensity 100; `0c ... 64` =
the custom (per-key) effect at intensity 100.

## Sources

- Solaar: `lib/logitech_receiver/` (hidpp20.py, hidpp20_constants.py,
  settings_templates.py, rgb_power.py, diversion.py, base.py)
- OpenRGB: `Controllers/LogitechController/` (G915 and HID++ 2.0 controllers)
- libratbag: `src/hidpp20.c` (on-board profiles, button and LED encodings)
- keyleds: `libkeyleds/src/feature_gkeys.c`, `feature_gamemode.c`
