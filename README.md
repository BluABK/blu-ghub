# blu-ghub

A small replacement for Logitech G HUB, for a G915 keyboard on its LIGHTSPEED
receiver and a G935 headset: battery levels, keyboard lighting, G-key macros
and headset buttons, without G HUB's background agent.

G HUB's agent can sit at a full CPU core for days and leak memory, and while it
does, typing suffers. The aim here is the opposite: a program that is idle when
nothing happens, sends nothing to the devices unless something needs changing,
and never sits in the keystroke path.

## Status

- **BluGHub** (the tray app): battery levels for the G915 and G935, and the
  G935's G1-G3 as media keys if you configure them.
- **BluGHub-diag**: the diagnostics used to work out what the devices do, and
  the keyboard's on-board G-keys.

Lighting and keyboard macros come next. Until then the keyboard runs on what
its on-board memory holds, which keeps working with nothing installed.

## BluGHub

A tray icon. Hover it for both devices' battery; click it for the menu:

```
G915: 91%
G935: 76%
---------------------
Settings…
Read batteries now
Start with Windows
Open log folder
---------------------
Quit
```

**Settings…** opens a window with each device's state, battery, voltage and
the time of the reading, and the settings below: the G935's buttons and
lighting, the low-battery warning, how often batteries are read, how long logs are kept,
and Start with Windows. Save applies them at once (a changed button hands
the headset's buttons back and takes them again). Saving rewrites the
config file, so comments written in it by hand do not survive; the version
replaced is kept as `BluGHub.toml.bak`. A file that could not be read when
BluGHub started is never saved over.

The window exists only while it is open: closing it frees its renderer, and
the Direct3D driver hands its memory back some seconds later. While open and
untouched it draws nothing; only the graphics driver keeps time with the
display.

A sleeping keyboard shows its last reading ("asleep (last 91%)"), a headset
that is switched off likewise. When a battery drops to the warning level a
notification says so, once, until it has been charged again.

**What it sends to the devices.** HID++ *read* requests, and only:

- when a device connects or wakes up (the receiver announces it),
- when you pick "Read batteries now", or after the PC resumes,
- as a fallback every 30 minutes while a device is awake.

Both devices also report battery changes on their own, so the fallback rarely
matters. Nothing is sent to a sleeping keyboard or a switched-off headset; the
app waits for them to announce themselves. Two writes: turning on the
receiver's connection notifications if a replug has turned them off (G HUB
used to do this), and taking the headset's buttons over and back (below).
Even with no buttons configured, the headset gets one hand-back the first
time it is on, which undoes a takeover left behind by a killed run.

Between those moments the app is idle: its main thread waits for Windows
messages and each device's worker waits for that device, with no timers.

**Running it.** `BluGHub.exe` starts it; starting it again while it runs
opens the settings window. `BluGHub --quit` stops a running instance and
waits until it has. "Start with Windows" adds a per-user Run entry
(`BluGHub --hidden`). Release builds put a shortcut in the Start Menu under
*Blu Software*.

**Headset buttons.** Out of the box blu-ghub leaves the G935's G1-G3 alone:
they do what the headset's own firmware does with them. Give any of them an
action in the config and blu-ghub takes all three over (the headset hands
over all or none) each time the headset switches on, and hands them back when
it quits.

Taking the G-keys over takes the microphone mute with them: the headset no
longer mutes by itself, and reports its mute button and boom to the PC
instead. blu-ghub then mutes and unmutes the microphone in Windows (mute
button: toggle; boom up: mute; boom down: unmute), and the headset and its
LED follow, so Windows shows the real state, which it does not when the
headset mutes by itself. If blu-ghub cannot find the headset's microphone
in Windows (or cannot tell it from another), or cannot read the mute button,
it does not take the buttons over at all. If it crashes it hands them back on
the way out; if it is killed, it hands them back when next started.
Switching the headset off also ends a takeover.

After each takeover, the first press of the mute button always mutes: until
then Windows' idea of the mute may be stale either way, as the headset may
have muted or unmuted by itself. Press again to unmute.

While G HUB's agent runs, blu-ghub leaves the headset's buttons and mute to
it, configured or not, until the headset is next switched on (or the
batteries are read, or the PC resumes).

The actions are sent as media keys, so the media player or the volume of the
default output reacts as to a keyboard's media keys. Windows drops them
while a window running as administrator has the focus, and while the PC is
locked.

**Config**: `%APPDATA%\BluGHub\BluGHub.toml`, every field optional:

```toml
[settings]
low_battery_percent = 20     # 0 = no warning
battery_check_minutes = 60   # the fallback reading, 5-1440
log_retention_days = 14

[headset.buttons]
g1 = "previous_track"
g2 = "next_track"
g3 = "off"

[headset.lighting.sides]     # the strips round the ear cups
effect = "breathing"         # off, static, breathing, cycle
color = "#3c8f5a"            # static and breathing
period_ms = 6000             # breathing and cycle, 1-65535
brightness = 60              # 1-100 (steady: the colour is dimmed)

[headset.lighting.logo]
effect = "off"
```

**Headset lighting.** A zone with no `[headset.lighting.*]` section is never
written and shows whatever the headset does by itself. A configured one is
set each time the headset switches on, since the headset cannot store
lighting; the settings window edits both.

Button actions: `default` (leave it to the headset), `off`, `play_pause`,
`stop`, `next_track`, `previous_track`, `mute` (the PC's sound output),
`volume_up`, `volume_down`. All lower case. With any button set, one left at
`default` does nothing. A file that does not parse is ignored as a whole,
with a warning in the log: the app then runs on the defaults and takes
nothing over.

**Logs**: `%APPDATA%\BluGHub\logs\BluGHub.log.<date>`, one file a day.

## BluGHub-diag

A console program. `caps`, `keys` and `sniff` send nothing to any device;
`probe` and `dump-onboard` send HID++ *read* requests only.

| Command | What it does |
|---|---|
| `BluGHub-diag caps` | Lists every Logitech HID collection with its usage, report sizes and input items, read from the descriptors through handles opened with no access rights. |
| `BluGHub-diag keys [--minutes N]` | Key-timing logger. Watches every keyboard through Raw Input (a copy of the input, not a hook, so it cannot slow typing) and flags the signatures of lost or late key releases. |
| `BluGHub-diag sniff [--minutes N] [--headset-all]` | Passive capture of the HID++ traffic from the receiver and the headset: notifications, and replies to whatever other program is talking to them. `--headset-all` also listens to the headset's other collections (volume wheel, telephony, its vendor channel) and names what their reports carry from their own descriptors. |
| `BluGHub-diag probe` | Asks the receiver, keyboard and headset what they are and support: feature table, name, firmware, battery, on-board profile mode, lighting zones and effects. Only "get" requests. |
| `BluGHub-diag dump-onboard` | Reads the keyboard's on-board profile memory into one file per sector, with checksums: the backup to have before anything is ever written there. |

### Changing the keyboard's on-board G-keys

The G-keys live in the keyboard's own memory, one set per M1/M2/M3 profile,
and work with nothing installed. These commands change them:

| Command | What it does |
|---|---|
| `BluGHub-diag set-gkeys 2 G3=F15 G5=LCtrl G1=off` | Sets G-keys in profile 2 (M2). Keys: `F1`-`F24`, `LCtrl` `LShift` `LAlt` `LWin` (and `R...`), `off`, or `usage:0xNN`. |
| `BluGHub-diag write-test 2` | Writes profile 2 back unchanged, to prove the write path. |
| `BluGHub-diag restore-sector 2 FILE` | Puts a saved copy of a profile back. |
| `BluGHub-diag light-test [--claim]` | Paints Escape red over the keyboard's stored lighting, in its memory only (nothing stored; a power cycle undoes it). `--claim` takes the lighting from the keyboard's own effects first, and hands it back on Enter. |
| `BluGHub-diag keys --echo` | Prints each press of a non-text key (F-keys, modifiers, ...) to check the result. |

Each write first reads the profile and checks its checksum, saves it to
`%APPDATA%\BluGHub\diag\<command>-<time>\sector-000N.bin` (the file
`restore-sector` takes), then writes and reads it back to compare. A change to
the active profile takes effect immediately.

`probe`, `dump-onboard` and the writing commands refuse to run while another
blu-ghub process is talking to the devices. What they found is written up in
[docs/protocol-g915.md](docs/protocol-g915.md) and
[docs/protocol-g935.md](docs/protocol-g935.md).

Everything is also written to `%APPDATA%\BluGHub\diag\<mode>-<date>-<time>.log`.
Ctrl+C (or closing the console) stops a run and writes a summary; long runs
also write one every hour.

### What `keys` looks for

- **Lost key-up.** A keyboard reports its whole key state whenever anything
  changes. If the report that releases a key never arrives, Windows keeps the
  key down and auto-repeats it ("tttttt") until the next report, usually the
  next key press, corrects the state. Signature: a key that auto-repeated and
  was released within a few milliseconds of another key's press.
- **Text key held past auto-repeat.** A letter, digit or space that repeated
  and was then released on its own. Rare in normal typing.
- **Chatter.** The same key pressed again within 25 ms of its release: a
  bouncing switch rather than the radio.
- **Device arrivals and removals**, for receivers that drop off the USB bus.

Each line names the keyboard it came from, so a G915 through its receiver, the
same G915 on its cable, and G HUB's virtual keyboard are told apart.

### What the log contains, and what it never does

Ordinary typing is never written. A `keys` log holds:

- the list of keyboards (by type and USB id);
- one line per suspicious event (lost key-up, text key held past repeat,
  chatter, a release without a press, a keyboard arriving or leaving);
- hourly and final counts.

In those lines, keys that type nothing are named (Ctrl, F15, arrows), because
"Left Ctrl stuck" is the point. Letters, digits, punctuation and space appear
only as `key#3fa1`, a random id that changes every run. Text injected by
programs (password-manager auto-type, remote desktop) is only counted.

Pressing **Scroll Lock twice** marks an incident. The dump shows only the presses
that auto-repeated in the last 15 seconds (when they went down, how long they
repeated, when they came up) and any key event within 50 ms of those moments,
with text keys as press numbers (`p17`) rather than keys. The rest of the
window is reduced to a count, so the log keeps no rhythm or structure of what
was being typed either.

## Building

```
cargo build --release
cargo test
cargo clippy --all-targets
cargo fmt
```

Windows only. The build cleans stale outputs from its own `target/` folder;
`TARGET_GC=off|dry|force` steers that.
