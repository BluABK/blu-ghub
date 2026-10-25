# G935 headset: protocol notes

Checked against a real G935 (firmware U1 29.00 build 12, dongle 046D:0A87)
with `BluGHub-diag probe`, and cross-checked against HeadsetControl,
g933-utils, OpenRGB, Solaar and the Linux kernel's hid-logitech-hidpp.
"Probe" marks what this unit answered.

## Transport (Windows)

| Collection | Usage | Reports | Notes |
|---|---|---|---|
| `MI_03&COL01` | 000C:0001 consumer | 5 in | volume up/down only |
| `MI_03&COL02` | 000B:0001 telephony | 2 in | report 08: Phone Mute, Hook Switch; the mute button and boom while the G-keys are diverted |
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
the logo. Current effect: fnE `[zone]` (probe: both off). Zone info is fn1
`[zone]` → `[zone, location(2), effect count, persistency]`, effect info fn2
`[zone, index]` → `[zone, index, effect id(2), ...]`.

Parameters (Solaar's layout; HeadsetControl sends the same for this
headset): static `[r g b ramp]`; breathing `[r g b period(2, ms) form
intensity]`; cycle `[0 0 0 0 0 period(2, ms) intensity]`. blu-ghub sends
persist 0 and writes a configured zone at every power-on.

## Buttons: 0x8010

3 G-keys (probe). fn2 `[01]` diverts them to the host and fn2 `[00]` hands
them back; the reply echoes the parameter (sniffed: `11 FF 05 29 01`). While
diverted, the headset sends events `[mask]`: one on press and one on release
(`00`), bit 0 = G1 (verified: `01`, `02`, `04`).

**Diverting the G-keys also diverts the microphone mute** (verified with
`sniff --headset-all`). With the buttons diverted, the mute button and the
boom no longer mute anything; they report on the telephony collection
(COL02) instead, one report per action, with no release:

| Report | Action | In the descriptor |
|---|---|---|
| `08 01` | mute button | Phone Mute (0B:002F, relative) |
| `08 10` | boom raised: sent **twice**, about 35 ms apart | no: padding to Windows |
| `08 20` | boom lowered | no: padding to Windows |

(Bit 1 is Hook Switch, 0B:0020, never seen.) Each is followed some 50-80 ms
later by an empty consumer report. Raising the boom still mutes in the
headset, diverted or not; lowering it, or the mute button, does nothing
while diverted. The host is expected to mute;
the headset then follows the USB audio mute of its microphone, which is what
Windows' mute of the recording device sets, LED included (verified: muting
and unmuting the device in Windows' sound settings changed both). G HUB
always diverts, so under G HUB this was G HUB's job.

Not diverted, the headset mutes by itself and lights the LED, and the PC
sees only an empty consumer report (`01 00 00 00 00`) per action: Windows'
mute setting does not change and shows a stale state.

Switching the headset off ends a diversion (verified: diverted, switched
off, switched on with nothing running: the mute button worked by itself
again). A request to a headset that is off is answered by the dongle with
HID++ 2.0 error 05 "not allowed". blu-ghub re-diverts at every power-on, and
a run that sees the buttons diverted when it does not want them hands them
back.

Windows' mute can be stale in either direction when a diversion starts, as
the headset may have muted or unmuted by itself before; and Windows passes
on only a change of its own state. blu-ghub therefore forces an unmute
through (mute, then unmute) and makes the first mute-button press after a
takeover a mute (forced the same way); a plain mute (the boom raised) is
never forced, as the headset has already muted itself.

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
