# Acknowledge the events channel so notifications keep flowing

No user-visible UI change.

## Problem

On an HX Effects (firmware 3.80, build d972399) the device stops sending notifications after 195 of them and stays silent, across new sessions, until its 9V power is pulled. Requests on the data channel keep working.

`tonepush watch` alone reproduces it (v0.9.0 release binary, Raspberry Pi 4, aarch64 Linux): power-cycle the pedal, run `timeout 180 tonepush watch`, touch nothing. Ticks (`event 22 {82: 0, 68: 10, 121: 27}`) arrive at about 6 per second for about 35 s, then nothing for the rest of the run. Per 10 s: 43, 63, 61, 29, then 0. With footswitches in use, `event 49` bypass changes stop at the same point.

Possibly related: #5.

## Cause

The device sends at most 4 KB of notifications that no ACK frame (type 0x08) has acknowledged. A tick is 21 bytes, and 195 ticks are 4095 bytes. While idle, hx-usb only sends keep-alives on the events channel. The device does not count their header acknowledgement, and `keepalive` then marks the channel acknowledged, so the drain before the next request skips it as well.

HX Edit does it differently. In `captures/mac-library-capture.log` the device sends 3453 data frames (72 KB) on the events channel and HX Edit answers with 3334 ACK frames, each within about 50 ms, with `ack = 0x1000 + bytes received`. In `02-ui-actions.log`: 460 data frames, 454 ACKs.

## Change

- `poll_notifications` sends an ACK on the events channel whenever the channel is owed bytes. No host transaction ever runs on that channel, so the rule against acknowledging the busy channel mid-transaction is not touched.
- `keepalive` no longer marks the events channel acknowledged.
- `fake::Pedal::events_window` models the 4 KB window: only ACK and data frames move it. The new test `watching_keeps_the_events_channel_flowing` runs `watch`'s loop for 600 notifications. Without the fix it fails at exactly 195.
- `PROTOCOL.md`: new paragraph "The events channel needs ACK frames, not keep-alives."

## Testing

- `cargo fmt --all --check`, `cargo clippy -p hx-usb --all-targets -- -D warnings`, `cargo test -p hx-usb` (54 unit tests and the replay test pass), `cargo check -p tonepush-cli`. I did not run the full workspace clippy or the GUI.
- Hardware, HX Effects 3.80 on a Raspberry Pi 4, power cycle before each run, pedal untouched, 3 minutes. Upstream `tonepush watch`: 195, then silence. The same hx-usb code with this change, run through a small client that does exactly what `watch` does (poll, sleep 700 ms, keep-alive): 1095 and 1096 in two runs, 64 to 66 every 10 s throughout. The `tonepush` binary itself was not rebuilt and run on the device with this change.
- The same client with footswitches in use for 2 to 3 minutes: 131 to 188 notifications every 10 s, no gaps.
- Not tested on an HX Stomp or any other device.
