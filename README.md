# blu-ghub

A small replacement for Logitech G HUB, for a G915 keyboard on its LIGHTSPEED
receiver and a G935 headset: battery levels, keyboard lighting, G-key macros
and headset buttons, without G HUB's background agent.

G HUB's agent can sit at a full CPU core for days and leak memory, and while it
does, typing suffers. The aim here is the opposite: a program that is idle when
nothing happens, sends nothing to the devices unless something needs changing,
and never sits in the keystroke path.

## Status

- **BluGHub** (the tray app): battery levels for the G915 and G935.
- **BluGHub-diag**: the diagnostics used to work out what the devices do.

Lighting, G-keys and headset buttons come next. Until then the keyboard runs
on what its on-board memory holds, which keeps working with nothing installed.

## BluGHub

A tray icon. Hover it for both devices' battery; click it for the menu:

```
G915: 91%
G935: 76%
---------------------
Read batteries now
Start with Windows
Open log folder
---------------------
Quit
```

A sleeping keyboard shows its last reading ("asleep (last 91%)"), a headset
that is switched off likewise. When a battery drops to the warning level a
notification says so, once, until it has been charged again.

**What it sends to the devices.** HID++ *read* requests, and only:

- when a device connects or wakes up (the receiver announces it),
- when you pick "Read batteries now", or after the PC resumes,
- as a fallback every 30 minutes while a device is awake.

Both devices also report battery changes on their own, so the fallback rarely
matters. Nothing is sent to a sleeping keyboard or a switched-off headset; the
app waits for them to announce themselves. The one write is to the receiver,
not the keyboard: turning on its connection notifications if a replug has
turned them off (G HUB used to do this).

Between those moments the app is idle: its main thread waits for Windows
messages and each device's worker waits for that device, with no timers.

**Running it.** `BluGHub.exe` starts it; starting it again while it runs
just shows a notification. `BluGHub --quit` stops a running instance and
waits until it has. "Start with Windows" adds a per-user Run entry
(`BluGHub --hidden`). Release builds put a shortcut in the Start Menu under
*Blu Software*.

**Config**: `%APPDATA%\BluGHub\BluGHub.toml`, every field optional:

```toml
[settings]
low_battery_percent = 20     # 0 = no warning
battery_check_minutes = 60   # the fallback reading, 5-1440
log_retention_days = 14
```

**Logs**: `%APPDATA%\BluGHub\logs\BluGHub.log.<date>`, one file a day.

## BluGHub-diag

A console program. `caps`, `keys` and `sniff` send nothing to any device;
`probe` and `dump-onboard` send HID++ *read* requests only.

| Command | What it does |
|---|---|
| `BluGHub-diag caps` | Lists every Logitech HID collection with its usage and report sizes, read from the descriptors through handles opened with no access rights. |
| `BluGHub-diag keys [--minutes N]` | Key-timing logger. Watches every keyboard through Raw Input (a copy of the input, not a hook, so it cannot slow typing) and flags the signatures of lost or late key releases. |
| `BluGHub-diag sniff [--minutes N]` | Passive capture of the HID++ traffic from the receiver and the headset: notifications, and replies to whatever other program is talking to them. |
| `BluGHub-diag probe` | Asks the receiver, keyboard and headset what they are and support: feature table, name, firmware, battery, on-board profile mode, lighting zones and effects. Only "get" requests. |
| `BluGHub-diag dump-onboard` | Reads the keyboard's on-board profile memory into one file per sector, with checksums: the backup to have before anything is ever written there. |

`probe` and `dump-onboard` refuse to run while another blu-ghub process is
talking to the devices. What they found is written up in
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
