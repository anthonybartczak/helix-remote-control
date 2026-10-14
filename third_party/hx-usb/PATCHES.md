# Patches to TonePush hx-usb

Vendored from [TonePush](https://github.com/crmne/tonepush) tag `v0.9.0` (commit
`5e44812`), MIT (see `LICENSE`). The first commit adding this folder is the
unchanged upstream copy (only `Cargo.toml`'s workspace keys made explicit), so
`git diff` against it shows exactly what we changed. `server/Cargo.toml` swaps it
in with `[patch."https://github.com/crmne/tonepush"]`. The `tonepush` CLI shipped in
the Pi bundle is still built from upstream and does **not** have these patches.

Drop this folder once upstream has an equivalent fix. Upstream pull request:
https://github.com/crmne/tonepush/pull/25, **merged 2026-10-14 as `76f3bc9`**. Its
`hx-usb` code is identical to section 1 here. Not in a release yet (latest: `v0.9.1`,
without it). Section 2 is not upstream at all yet. When a release includes both:
move the TonePush pin to that tag (AGENTS.md 3, with a note in the commit), delete
this folder and the `[patch]` section in `server/Cargo.toml`. The bundled `tonepush` CLI gets the fix with the same bump. The patch prepared for
upstream, against TonePush `main` (53a6a65), and its pull request text are in
`upstream/`.

## 1. Acknowledge the events channel (2026-10-13)

**Symptom.** On an HX Effects (firmware 3.80) the pedal stops sending
notifications after 195 of them and stays silent, across new sessions, until its
power is pulled. Reproduces with upstream `tonepush watch` alone, pedal untouched:
195 ticks in ~35 s, then nothing (docs/hx-effects-notifications.md).

**Cause.** The device sends at most 4 KB of notifications that the host has not
acknowledged with an ACK frame. A tick is 21 bytes; 195 × 21 = 4095. hx-usb never
sends an ACK frame on the events channel while idle: `keepalive` carries the
current acknowledgement in its header, but the device does not count it, and
`keepalive` then marks the channel acknowledged, so later requests skip it too.

**Evidence that HX Edit does it differently.** In TonePush's own capture
`captures/mac-library-capture.log`, the device sends 3453 data frames (72 KB) on
the events channel and HX Edit answers with 3334 ACK frames, one per
notification within ~50 ms, `ack = 0x1000 + bytes received`. `02-ui-actions.log`:
460 data frames, 454 ACKs.

**Change.**
- `poll_notifications` sends an ACK on the events channel when it has
  unacknowledged bytes. No host transaction ever runs on that channel, so the
  "never ACK the busy channel mid-transaction" rule (PROTOCOL.md) is not touched.
- `keepalive` no longer marks the events channel acknowledged.

**Tests.** `fake::Pedal::events_window` models the 4 KB window (only ACK and data
frames move it). `commands::pedal_tests::watching_keeps_the_events_channel_flowing`
runs `tonepush watch`'s loop for 600 notifications: unpatched it stops at exactly
195, patched all arrive. Upstream's unit and replay tests still pass.

**On hardware: verified** 2026-10-13, HX Effects 3.80, Pi 4. After a power cycle,
pedal untouched, `helix-bridge watch 180`: 53, then 64–66 notifications every 10 s
for the full 3 minutes, 1096 in total (unpatched: 195, then silence).
A/B the same evening, power cycle before each run, hands off: upstream
`tonepush watch` (v0.9.0) 43, 63, 61, 29, then nothing - **195** total; patched
`helix-bridge watch 180` 64–66 every 10 s - **1095** total.

## 2. Survive the events channel going wrong (2026-10-14)

Not upstream yet. Two device faults, both found in `HX_DEBUG=1` captures on the
HX Effects 3.80 (Pi 4), and both also in TonePush's own material.

**2a. A frame sent out of order.** Now and then the device sends one
notification with its first four bytes moved behind the rest:

```text
expected  00 00 04 00 | 0d 00 00 00 <13-byte tick body> | 00 00 04 00 0d ...
received  0d 00 00 00 <13-byte tick body> | 00 00 04 00 | 00 00 04 00 0d ...
```

The byte count is right, so acknowledgements stay right, but the stream reader
reads a body as a length: "stream message declares 1779853698 bytes" ends the
session, or a 262144-byte length makes it wait while every later notification
piles up behind it. HX Edit's capture of an HX Stomp
(`captures/mac-enums-capture.log`) holds the same frame twice in ~3500
notifications, so the device does it with HX Edit too.

*Change:* `realign.rs`. On the events channel only, bytes pass to the stream
reader as whole messages that start `00 00 04 00` (or service 0, as the
service-open reply does) and declare at most 64 KB. Anything else is stepped
over a byte at a time until the stream lines up again. All ~70 000
notifications in TonePush's HX Edit captures pass; the slip costs the one
notification it cut through. `Session::events_skipped()` counts the bytes.

**2b. An events handshake left unanswered.** A session that opens too soon
after another one ended, or after the pedal powered up, can find the events
channel never answers its hello. Requests work; no notification arrives for
the whole session. Often the first open attempt fails and `open`'s immediate
retry is the deaf one. HX Effects 3.80, Pi 4, 2026-10-14: every open 0-4 s after
a session ended (service restart, reconnect after a lost session, the reopen
below in its first version) or 4 s after re-enumeration was deaf; every open
15 s or more after worked, `watch` and the hand-run server included. One cause
is that the device keeps sending to an ended session for up to ~4 KB (15-30 s);
TonePush's own `tests/fixtures/session.transcript` shows that case: stale ticks
before the handshake, the events hello unanswered, no events frame afterwards.

*Change:*
- `route` notes each channel's hello reply; `Session::events_answered()` says
  whether the events channel answered. The handshake bytes are unchanged (the
  replay transcript still matches).
- `drain` keeps reading while a previous session's notifications still arrive,
  up to 30 s, instead of stopping at 3 s with the pedal still streaming.
- The helix-bridge server (`server/src/usb.rs`) opens no session sooner than
  20 s after one ended, after the process started, or after the pedal
  appeared; a deaf session serves requests for 20 s, then is closed and opened
  again after the same 20 s, up to 3 times.

**Tests.** `realign::tests` (rotation, rotation right after a split, split
messages, service-open reply), `commands::pedal_tests::a_frame_sent_out_of_order_costs_one_notification`,
`a_session_says_whether_the_events_channel_answered` (the fake pedal now
answers handshakes, and can be told not to on the events channel),
`tests::a_drain_waits_on_notifications_only`. Replay test unchanged and passing.

**On hardware:** 2a verified 2026-10-14 (HX Effects 3.80, Pi 4): under the server
the realigner caught an out-of-order frame every 2-5 s (none in a 30 s `watch`,
which ACKs once per 700 ms) while 119-145 notifications per 10 s kept arriving.
2b: the 20 s settle is not verified yet.
