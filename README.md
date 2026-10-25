# blu-ghub

A small replacement for Logitech G HUB, for a G915 keyboard on its LIGHTSPEED
receiver and a G935 headset: battery levels, keyboard lighting, G-key macros
and headset buttons, without G HUB's background agent.

G HUB's agent can sit at a full CPU core for days and leak memory, and while it
does, typing suffers. The aim here is the opposite: a program that is idle when
nothing happens, sends nothing to the devices unless something needs changing,
and never sits in the keystroke path.

## Status

Diagnostics only, so far. The first job is evidence: whether G HUB (or the
Logitech driver it installs) is what causes repeated or stuck keys, and what
the devices actually send. The tray app comes next.

## BluGHub-diag

A console program. **None of its modes send anything to a device.**

| Command | What it does |
|---|---|
| `BluGHub-diag caps` | Lists every Logitech HID collection with its usage and report sizes, read from the descriptors through handles opened with no access rights. |
| `BluGHub-diag keys [--minutes N]` | Key-timing logger. Watches every keyboard through Raw Input (a copy of the input, not a hook, so it cannot slow typing) and flags the signatures of lost or late key releases. |
| `BluGHub-diag sniff [--minutes N]` | Passive capture of the HID++ traffic from the receiver and the headset: notifications, and replies to whatever other program is talking to them. |

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
