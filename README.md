# blu-ghub

A small replacement for Logitech G HUB, for a G915 keyboard on its LIGHTSPEED
receiver and a G935 headset: battery levels, keyboard lighting, G-key macros
and headset buttons, without G HUB's background agent.

G HUB's agent can sit at a full CPU core for days and leak memory, and while it
does, typing suffers. The aim here is the opposite: a program that is idle when
nothing happens, sends nothing to the devices unless something needs changing,
and never sits in the keystroke path.

## Status

- **BluGHub** (the tray app): battery levels for the G915 and G935, the
  G935's G1-G3 as media keys and its lighting if you configure them, and a
  per-key layout editor for the G915 with a live preview on the keyboard.
- **BluGHub-diag**: the diagnostics used to work out what the devices do, and
  the keyboard's on-board G-keys.

Storing an edited layout in the keyboard, and keyboard macros, come next.
Until then the keyboard runs on what its on-board memory holds, which keeps
working with nothing installed.

## BluGHub

A tray icon. Hover it for both devices' battery; click it for the menu:

```
G915: 91%
G935: 76%
---------------------
Settings…
Read batteries now
Re-apply to the devices
Start with Windows
Open log folder
---------------------
Quit
```

**Settings…** opens a window in tabs: *Overview* (both devices side by
side: state, battery, voltage, the time of the reading, and their details;
the low-battery warning, how often batteries are read, how long logs are
kept, Start with Windows), the G915's *G-keys* and *layout* (below),
*Macros*, and the *G935* (buttons, sound, lighting). Save applies them at
once. The keyboard's settings are applied
in place, sending only what changed (new colours repaint those keys, new
G-key actions send nothing); a changed headset button hands the headset's
buttons back and takes them again. Saving rewrites the config file, so
comments written in it by hand do not survive; the version replaced is kept
as `BluGHub.toml.bak`. A file that could not be read when BluGHub started is
never saved over.

**Details** under each device: firmware (and the receiver's), serial number
and wireless id (masked until *Show* is clicked, so a screenshot can be
shared; never written to the log), the report rate (the active on-board
profile's) and the interval the receiver
records, the backlight level, the bank in use; for the headset its
sidetone, the equaliser in Windows (below) and, while blu-ghub has its
buttons, the microphone's mute and the boom. What cannot change is read
once; the rest each time the device connects or switches on. The devices
report no signal strength, radio channel or power draw.

**Headset equaliser.** What you hear from the G935 goes through Logitech's
audio processing in Windows ("Logitech HX2E Surround Sound Effect", which
G HUB's audio driver attaches to the headset's playback endpoint), and its
equaliser is the one that counts: the headset's own equaliser setting
(HID++ 0x8310) is accepted and read back, but does not change the sound.
The G935 tab shows Logitech's equaliser as a curve, a point per band (10
bands, 40 Hz to 12.5 kHz, ±12 dB, half-dB steps). With *Set the equaliser*
ticked, a point dragged up or down is written when let go, a double-click
puts it back to 0 dB, and Save keeps it as `equaliser` under `[headset]`
(one dB value a band); blu-ghub writes it at start-up when Windows has
another, and never otherwise. Unticked, it is left alone. Closing the window
without saving puts back what was there. A change is heard from the next
time playback starts (stop and play; pausing is not enough). It is set the
way Windows' Sound control panel sets effects, without administrator
rights; `BluGHub-diag apo-eq` shows it, and can set and restore it.

**Re-apply to the devices** (in the window and the tray menu) hands
everything back to both devices and takes it again with the saved settings,
painting the keyboard from nothing: for when a device shows something other
than the settings say.

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
app waits for them to announce themselves. The writes: turning on the
receiver's connection notifications if a replug has turned them off (G HUB
used to do this), taking the headset's buttons and the keyboard's G-keys
over and back, setting the headset's lighting (below), and, while the
layout editor is open or the keyboard is set to show blu-ghub's layout,
holding the keyboard's own lighting effects off and painting its keys in
its memory. Even
with no buttons or G-keys configured, each device gets one hand-back the
first time it is on, which undoes a takeover left behind by a killed run.

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

[keyboard.lighting]
show = "layout"               # stored (the keyboard's own) or layout (the keys below)

[keyboard.lighting.keys]      # usually imported from G HUB, see below
esc = "#2a6fd4"
f1 = "#d42a8c"
logo = "#2a6fd4"

[keyboard.gkeys]
show_bank = true              # G1-G5 in the bank's colour while blu-ghub has them
bank_colors = ["#2080ff", "#30d040", "#ff8010"]   # M1, M2, M3
light_m_keys = false          # light the M keys themselves (host mode, see below)

[keyboard.gkeys.m1]           # G1-G5 while M1 is lit; m2, m3 likewise
g1 = { text = "On my way!" }
g2 = { media = "play_pause" }
g3 = { macro = "copy-paste" }
g4 = { key = "left_ctrl" }    # held as long as G4 is
# g5 left out: what the keyboard's own profile says

[[macros]]
name = "copy-paste"
mode = "once"                 # once, while_held, toggle
delay_ms = 30                 # after each step
steps = [
  { down = "left_ctrl" }, { tap = "c" }, { up = "left_ctrl" },
  { delay = 200 },
  { down = "left_ctrl" }, { tap = "v" }, { up = "left_ctrl" },
]

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

[headset.sidetone]           # set at each power-on; left out: the headset's own
on = true                    # false: off (0), the level kept
level = 20                   # 0-100
```

**Keyboard lighting.** The G915 plays a layout stored in itself.
`[keyboard.lighting.keys]` is blu-ghub's own layout, edited in the settings
window's *G915 layout* tab: click a key, Ctrl-click or drag to pick more,
double-click to pick every key of one colour, then choose a colour (picker,
`#rrggbb`, or one of the colours in use). *Read G HUB's layouts…* reads a
copy of `%LOCALAPPDATA%\LGHUB\settings.db` (never the file itself), and
*Import* starts from the one picked.

`show` picks what the keyboard shows. `stored` (the default): its own
stored layout, and blu-ghub sends nothing. `layout`: blu-ghub's layout,
painted whenever the keyboard connects or wakes. blu-ghub then holds the
keyboard's own effects off (they would draw over painted keys within a
minute), for the lighting only: the keyboard still sleeps after its five
minutes, but no longer dims after one. It is painted into the keyboard's
memory only, so nothing is stored: without blu-ghub running, and for a
moment at each wake, the keyboard shows its stored layout.

While the *G915 layout* tab is open, the window has the focus and *this
layout* is picked, the layout being edited is shown on the keyboard as it
is (G-keys included), only the keys that changed, at most ten times a
second. Leaving the tab shows the saved choice again at once. Storing a layout in the keyboard itself is not done.

Key names follow the key's position on a US layout (`semicolon` is Ø on a
Nordic board); the full list is in `src/keymap.rs`.

**Keyboard G-keys.** Out of the box the G915 keeps its G-keys and does what
its own profile says (`BluGHub-diag set-gkeys` changes that). Give any key
in any bank an action and blu-ghub takes all five over (the keyboard hands
over all or none) each time the keyboard wakes, and hands them back when it
quits or crashes. A key left at `default` then keeps that bank's profile key
(read from the keyboard when the keys are taken), typed by blu-ghub.
On-board macros and profile switches on a G-key cannot be done that way,
and the log says so.

The M keys come with the G-keys: while blu-ghub has them, M1-M3 switch
blu-ghub's bank instead of the keyboard's profile, starting from the profile
the keyboard was on, and their lights stay dark (the keyboard does not show
them for the host in on-board mode). The keyboard stays on its profile, and
is back on it, lit, when blu-ghub hands the keys back. With `show_bank`,
G1-G5 light up in the bank's colour instead, which holds the keyboard's own
lighting effects off as a layout of blu-ghub's does (no dimming after a
minute; it still sleeps); the rest of the keyboard shows whichever layout
is chosen.

With `light_m_keys`, the bank's M key itself is lit. The keyboard only lets
the host light the M keys in host mode, so while blu-ghub has the G-keys it
switches the keyboard to host mode (its on-board profiles off; blu-ghub
stands in for them), and back to on-board mode when it lets go, including
on a crash. A power cycle also ends host mode. If blu-ghub is killed or
hangs meanwhile, the G and M keys do nothing until it starts again or the
keyboard is switched off and on; typing and the lighting are not affected.
At its first contact blu-ghub puts a keyboard it finds in host mode back
into on-board mode, unless it is about to use it.

Actions: `default`, `off`, `{ key = "ctrl+shift+f13" }` (a key held for as
long as the G-key is; `ctrl`, `shift`, `alt`, `win` and a key name, or a
key name alone such as `left_ctrl`, `f13`-`f24` included), `{ text = "..." }` (typed as Unicode, whatever
the layout; a line break presses Enter), `{ paste = "..." }`,
`{ paste_file = "C:\\path\\picture.png" }`, `{ media = "..." }` (the
headset buttons' actions), `{ macro = "name" }`. Macro steps:
`{ tap = "key" }`, `{ down = "key" }`, `{ up = "key" }`, `{ text = "..." }`,
`{ paste = "..." }`, `{ paste_file = "..." }`, `{ media = "..." }`,
`{ delay = ms }`, with the key names above plus `f13`-`f24`. A macro runs
`once` per press, over and over `while_held`, or as a `toggle` from one
press to the next; one runs at a time, and any key it left down is released
when it ends. The settings window picks the actions (the G-keys tab) and
makes the macros (the Macros tab: steps added, reordered and
removed; renaming a macro follows into the G-keys, and one in use cannot be
deleted). Typing goes in as if from a keyboard, so it does not reach
a window running as administrator. Windows 11's Notepad sometimes shows
typed text cut short or garbled; other programs show all of it.

**Pasting** goes through the clipboard: what is on it is saved, the text or
file is put there and Ctrl+V pressed, and after 0.4 s the saved contents go
back, unless something else was copied meanwhile. What blu-ghub puts there
stays out of Windows' clipboard history and cloud clipboard. A file is
pasted as a file, as Explorer's Copy does (chat programs upload it, a GIF
stays animated); an image file (PNG, JPEG, GIF, BMP, TIFF, WebP, icon) also
as a picture, and a PNG also as its own bytes, which keeps transparency in
browsers and chat programs. Saving and restoring covers text, files, rich
text and pictures; drawings held only as Windows metafiles are not put
back.

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
| `BluGHub-diag host-test [--leave]` | Tries the keyboard's host mode (software drives it, as G HUB does with its on-board memory off): asks for the word HOST first, lights M1, paints Escape red in its memory, and switches back to on-board mode on Enter. `--leave` stays in host mode, to see whether a power cycle ends it. |
| `BluGHub-diag apo-eq [set G1 .. G10 \| restore FILE]` | Shows the equaliser of Logitech's audio processing on the G935's playback endpoint (what Windows plays the headset through); `set` saves it to a file first and sets the 10 gains in dB, `restore` puts a saved one back. Nothing is sent to the headset. |
| `BluGHub-diag host-test --divert` | The same, with the G-keys taken as the tray app takes them: asks for the M1 light, waits 90 s with the keyboard untouched, asks for the M3 light, then hands the G-keys back and switches to on-board mode. |
| `BluGHub-diag host-test --restore` | Hands the G-keys back, says which mode the keyboard is in, and puts it back in on-board mode. |
| `BluGHub-diag slots` | Reads what the keyboard's stored-lighting slots hold: whether each is in use, its length, and the id and name G HUB gave it. Read requests only. |
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
