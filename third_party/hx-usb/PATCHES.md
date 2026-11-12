# Patches to TonePush hx-usb

Vendored from [TonePush](https://github.com/crmne/tonepush) tag `v0.12.1` (commit
`dcdedcf`), MIT (see `LICENSE`). Only `Cargo.toml`'s workspace keys are made
explicit; everything else we changed is in `src/`. To see exactly what, diff `src/`
against `crates/hx-usb/src/` of a TonePush `v0.12.1` checkout: the result is
sections 2 to 4 below and nothing else. `server/Cargo.toml` swaps this copy in with
`[patch."https://github.com/crmne/tonepush"]`. The `tonepush` CLI shipped in the Pi
bundle is built from upstream: it has section 1 (upstream since `v0.10.0`) but
**not** sections 2 to 4. `../hx-proto/tests/` holds the two upstream test files
the crate's tests read.

History of the pin: `v0.9.0` (`5e44812`) with sections 1 to 3 as local patches until
2026-11-11, then `v0.12.1` with section 1 taken from upstream and sections 2 and 3
merged onto it by hand (three-way, nine conflicts, all where upstream's copy of
section 1 and ours had touched the same lines). Not run on the pedal since.

Drop this folder once upstream has an equivalent of sections 2 and 3: move the
TonePush pin to that tag (AGENTS.md 3, with a note in the commit), delete this
folder and the `[patch]` section in `server/Cargo.toml`. Offered upstream on
2026-11-11 as https://github.com/crmne/tonepush/pull/118 (open): the realigner
(2a and 3) and `events_answered` (2b), on TonePush `main` at `d4094a3`. The text and
patches as sent are `upstream/PR-2.md`, `PR-2-0001` and `PR-2-0002`. Held back:
`PR-2-0003`, the 30 s drain, and the server's 20 s settle, which is ours anyway.
On the maintainer's triage rule (transport changes belong in the device's profile)
the upstream realigner runs only where `DeviceProfile::realigns_events` is set, the
HX Effects' profile so far (`PR-2-0004`). Our copy here realigns for every device,
which is the same thing for us: the bridge only talks to an HX Effects.

Faults found in upstream review are fixed there (`PR-2-0005` to `PR-2-0011`) and
here: section 4. `PR-2-0008`, a line `tonepush watch` prints when the events
channel did not answer, is CLI only and has nothing to port.
crmne/tonepush pull request 100 (open, another contributor) reports fault 2a on a
Helix Floor (3.80) and may be merged instead of or before ours.

## 1. Acknowledge the events channel (2026-10-13)

**Upstream now, no longer a local patch.** Pull request
https://github.com/crmne/tonepush/pull/25, merged 2026-10-14 as `76f3bc9`, released
in `v0.10.0` (2026-10-26). The patch and pull request text as sent are in
`upstream/`. What follows is the record of the fault and the fix.

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

Offered upstream 2026-11-11 (pull request 118), not merged. Two device faults, both found in `HX_DEBUG=1` captures on the
HX Effects 3.80 (Pi 4). The second is also in TonePush's own material; the first
is not (see the correction in 2a).

**2a. A frame sent out of order.** Now and then the device sends one
notification with its first four bytes moved behind the rest:

```text
expected  00 00 04 00 | 0d 00 00 00 <13-byte tick body> | 00 00 04 00 0d ...
received  0d 00 00 00 <13-byte tick body> | 00 00 04 00 | 00 00 04 00 0d ...
```

The byte count is right, so acknowledgements stay right, but the stream reader
reads a body as a length: "stream message declares 1779853698 bytes" ends the
session, or a 262144-byte length makes it wait while every later notification
piles up behind it.

*Correction 2026-11-11.* This section said HX Edit's capture of an HX Stomp
(`captures/mac-enums-capture.log`) holds the same frame twice, so the device does
it with HX Edit too. A re-check for the upstream pull request did not find it: all
17 of TonePush's capture logs read in order (70 507 messages on the events
channel). The one frame of this shape in `mac-enums-capture.log` is the first
frame of the capture, the rest of a message the device split across two frames,
which is in order. So the fault is confirmed on the HX Effects only.

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

## 3. Check a realigned message's body (2026-10-15)

Offered upstream 2026-11-11 with 2a (pull request 118), not merged. Follows 2a.

**Symptom.** HX Effects 3.80, Pi 4, helix-bridge: turning knobs on the pedal
ended the session within a minute or so, every time, with "stream message is
not MessagePack: unexpected end of buffer" or "… map key was neither integer
nor string". Out-of-order frames came every 2-5 s meanwhile.

**Cause.** Stepping over an out-of-order frame a byte at a time can stop on
bytes inside a notification that pass 2a's header check. Parameter
notifications carry floats: `ca 00 00 00 00` (0.0) and the next key read as
originator 0, service 0, a small length. The realigner passed that slice on,
the stream reader failed on its body, and every hx-usb error but a refusal
ends the session.

**Change.** `realign.rs`: a candidate passes only if its body decodes, and on
service 4 (notifications) only if it decodes as an RPC message; else it is
stepped over like any other misaligned byte. `MAX_MESSAGE` drops from 64 KB to
4 KB (the device never has more than 4 KB unacknowledged here, section 1), so
a false header with a modest length stalls the stream for less. Any real
notification this rejects would already have ended the session in
`poll_notifications`, which decodes it the same way.

**Tests.** `realign::tests::a_header_over_a_body_that_is_not_a_message_is_stepped_over`
(not MessagePack, MessagePack but not RPC, the float-zero case). All hx-usb
tests and the replay test pass.

**On hardware: verified** 2026-10-15, HX Effects 3.80, Pi 4: knob turns on several
blocks for about 1.5 minutes, 39 out-of-order frames stepped over (7-56 bytes each,
under a second apart at times), no session lost. Unpatched, the same use ended the
session within about a minute, twice. Each slip still costs the notification it cut
through, so a knob value can lag until its next change.

## 4. Faults found in upstream review (2026-11-11 and 2026-11-12)

Offered upstream with 2a and 3 (pull request 118), not merged. Both were in our
own code from sections 2a and 3, found by the automated review of that pull
request. 4a and 4b were ported here the same day from `upstream/PR-2-0005` and
`PR-2-0006`; 4c and 4d on 2026-11-12 from `PR-2-0007`, `PR-2-0009` and
`PR-2-0010`, by taking
`realign.rs` as it stands upstream (only comments differ).

**4a. A notification split across the service opening was dropped.** The device
splits a notification across two frames after its first four bytes.
`open_service` cleared the realigner after the read that follows the service
opening, so a first half that arrived with that read was lost, and the body
that followed was stepped over: one notification, 17 bytes on the counter.

*Change:* only the clear after the handshake stays. What the channel sent
before it was for the session before, and the device does not finish a message
it cut short there: in TonePush's `captures/mac-enums-capture.log` frame seq
0xd9 ends in a message's first four bytes, HX Edit's handshake follows 4.8 s
later, and the stream then starts on a fresh header.

**4b. A false header could hold back up to 4 KB.** After a slip, bytes can read
as a header that declares up to `MAX_MESSAGE`. The realigner waited for that
many bytes, about half a minute of ticks with nothing delivered, before it
could reject it.

*Change:* a header whose message is still missing bytes is stepped over once a
whole message has arrived behind it (the device sends a message's bytes
together, in two frames at most). With nothing whole behind it, it is still
waited for, as a message split after its first eight bytes needs.

**Tests.** `commands::pedal_tests::a_notification_split_across_the_service_opening_is_kept`
(the fake pedal's `events_at_open`), `realign::tests::a_false_header_does_not_hold_back_what_follows`,
`a_header_split_from_its_body_still_waits`. The first two failed before their
change (checked in the upstream clone, same code). All 70 507 events messages
in TonePush's HX Edit captures pass through the changed realigner as before.

**4c. A false header could take a real notification with it.** The realigner
passed a candidate whose body began with any value that decodes; "as an RPC
message" in section 3 was weaker than it reads, because TonePush decodes any
value it does not recognise as `Message::Other`. A false header over a nil and
a real notification was passed on as one message, and the notification went
with it, with nothing counted as skipped.

*Change:* a candidate is exactly one notification, or the bare service number
that answers the service opening, filling its declared body, or it is stepped
over. TonePush's HX Edit captures hold nothing else on this channel: 70 492
notifications and 15 service-open replies, none with bytes after its value.
The header's service number is not held against a notification (`PR-2-0010`):
the first version of this change required 4 there and so dropped everything
the fake pedal's `notify` sends, which writes 0. Our own
`watching_keeps_the_events_channel_flowing` caught that here; upstream no test
did, because there only the HX Effects profile realigns.

**4d. An unfinished header was abandoned on evidence from inside its own body.**
After 4b a header still missing bytes was stepped over as soon as a whole
message lay behind it. What lies behind an unfinished header is inside its
declared body, so a notification carrying another one's bytes as data, cut
after them, lost its own header.

*Change:* two things together show a header was false: it has already waited
through a whole frame (no message has been seen in more than two), and a whole
notification has arrived behind it. A false header is therefore stepped over
one frame later than under 4b, and no later. What is left: nothing establishes
two frames as a limit on the HX Effects, so a notification that took three and
carried another one's bytes as data would still lose its header. The upstream
reviewer keeps that open; a fresh `HX_DEBUG` capture counting frames per
message would settle how real it is.

**Tests for 4c and 4d.** `realign::tests::a_header_over_another_notification_is_not_taken_for_one`,
`a_split_notification_is_kept_when_its_body_looks_like_a_reply`,
`a_split_notification_is_kept_when_its_body_holds_a_notification`,
`a_false_header_is_given_one_more_frame`. Each failed before its change
(checked in the upstream clone). The captures pass as before.

**On hardware:** not run yet, none of 4a to 4d.
