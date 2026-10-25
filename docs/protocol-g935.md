# G935 headset: protocol notes

Checked against a real G935 (firmware U1 29.00 build 12, dongle 046D:0A87)
with `BluGHub-diag probe`, and cross-checked against HeadsetControl,
g933-utils, OpenRGB, Solaar and the Linux kernel's hid-logitech-hidpp.
"Probe" marks what this unit answered.

## Transport (Windows)

| Collection | Usage | Reports | Notes |
|---|---|---|---|
| `MI_03&COL01` | 000C:0001 consumer | 5 in | volume up/down only |
| `MI_03&COL02` | 000B:0001 telephony | 2 in | hook switch, phone mute |
| `MI_03&COL03` | FF43:0202 | 20 in/out (id 0x11) | **HID++** (long reports only, device 0xFF) |
| `MI_03&COL04` | FF00:0001 | 64 in/out/feature | vendor channel, **not HID++: never written** |

The headset has no short (0x10) reports: every request is a 20-byte long one.

## Feature table (probe)

HID++ 4.2: 1 FEATURE_SET, 2 DEVICE_FW_VERSION, 3 DEVICE_NAME ("G935 Gaming
Headset"), 4 COLOR_LED_EFFECTS 0x8070, 5 GKEY 0x8010, 6 EQUALIZER 0x8310,
7 SIDETONE 0x8300, 8 ADC_MEASUREMENT 0x1F20 (v0). Code looks indices up
through ROOT anyway.

## Battery: 0x1F20

fn0 → `[mV hi, mV lo, flags]`: flags 01 discharging, 03 charging, 07 full
(probe: `0F 09 01` = 3849 mV, discharging). The same bytes arrive as an event
(software id 0); `00 00 00` means the headset turned off, and a request to a
headset that is off gets an error reply. Version 0 here, so the power-off
timeout functions (v2+) are not available.

## Lighting: 0x8070

| Zone | Location | Effects (index → id) |
|---|---|---|
| 0 | logo | 0 off, 1 static, 2 breathing (0x000A), 3 cycle (0x0003) |
| 1 | primary (side strips) | same |

Persistency capability reads 00 on both zones. setZoneEffect is fn3
`[zone, effect index, params × 10, persist]`; send the strips (zone 1) before
the logo. Current effect: fnE `[zone]` (probe: both off).

## Buttons: 0x8010

3 G-keys (probe). fn2 `[01]` diverts them to the host, which then receives
events `[mask]` (bit 0 = G1); `[00]` hands them back to the headset.

## Sidetone 0x8300 and equaliser 0x8310

Sidetone fn0 → level 0-100 (probe: 0). Equaliser: 10 bands (32 Hz-16 kHz),
±12 dB (probe: all 0). Setting either is never done without an explicit user
action.

## Sources

- HeadsetControl: `lib/devices/logitech_g633_g933_935.hpp`, `logitech.hpp`
- g933-utils: `notes/notes.txt`, `libg933/src/*.rs`
- OpenRGB: `Controllers/LogitechController/LogitechProtocolCommon.*`
- Solaar: `lib/logitech_receiver/hidpp20.py`, `settings_templates.py`
- Linux: `drivers/hid/hid-logitech-hidpp.c`
