# HX Effects notifications (front panel)

What the HX Effects (firmware 3.80) sends unasked when its own footswitches are used.
Recorded 2026-10-12 on the Pi 4 with `tonepush watch` (TonePush v0.9.0), lines
timestamped by the shell. Meanings: TonePush `docs/_reference/opcodes.md` section 3.

TonePush captured these events only while HX Edit drove the pedal. This recording shows
that a **front-panel footswitch** sends the same bypass event. The `usb` backend
(`server/src/usb.rs`, `notice`) relies on it.

## Footswitch: one block off, then on

Excerpt: about 200 identical tick lines (`event 22 {68: 10, 121: 27}`) are cut; every
other line is as printed.

```text
16:24:42 event 49: {82: 0, 68: 5, 121: 17, 106: {98: 2, 59: true}}
16:24:42 event 39: {82: 1, 68: 3, 121: 19, 106: {98: 2, 26: 0}}
16:24:50 event 39: {82: 1, 68: 3, 121: 19, 106: {98: 2, 26: 0}}
16:24:51 event 49: {82: 0, 68: 5, 121: 17, 106: {98: 2, 59: false}}
```

| Event | (82, 68, 121) | Args | Meaning |
|---|---|---|---|
| 22 | (0, 10, 27) | nil | Steady tick, several per second. Noise |
| 49 | (0, 5, 17) | `{98: block, 59: enabled}` | Bypass changed. `98` is the slot index (`chain` position − 1) |
| 39 | (1, 3, 19) | `{98: block, 26: path}` | Selected block changed. Nothing visible |

## Notifications stopped after ~200 (fixed in our hx-usb patch)

On the HX Effects 3.80 the pedal stops sending notifications on the events channel
after about 200 of them, and stays silent across new sessions until its power is
pulled. Requests on the data channel keep working.

| Test (2026-10-13, Pi 4) | Notifications before silence |
|---|---|
| `tonepush watch` alone, right after a power cycle, **pedal untouched**, 3 min | 195 in ~35 s (14, 62, 59, 61 per 10 s), then 0 for the rest |
| **`helix-bridge watch 180` with patched hx-usb**, same conditions | **1096**: 64–66 every 10 s for the full 3 min. Fixed |
| A/B control, same evening: upstream `tonepush watch` vs patched | **195** (43, 63, 61, 29, then 0) vs **1095** |
| `tonepush watch` alone, footswitches pressed for 3 min | ~190 in 40 s, then 0; 4 of many presses arrived |
| Bridge `usb`, keep-alive only (`ff00de7`, `7c986a0`) | ~190, then 0 |
| Bridge `usb`, plus a `preset_info` request after each poll with data (`f5ec3d9`) | ~1300 in 90 s, then 0 |

So it reproduces with TonePush alone.

**Cause (found in TonePush's own HX Edit captures):** the pedal sends at most 4 KB
of notifications without an ACK frame (195 ticks × 21 bytes = 4095). HX Edit ACKs
almost every notification; hx-usb only sends keep-alives, which do not count. Fixed
in our patched copy of hx-usb: `third_party/hx-usb/PATCHES.md`. Verify on the pedal
with `sudo systemctl stop helix-bridge && sudo /opt/helix-bridge/bin/helix-bridge
watch 180` after a power cycle: the 10 s counts should stay above 0 for 3 minutes.

With the patch, the bridge's `usb` backend ran 2–3 minutes of footswitch use at
131–188 notifications per 10 s without a gap (2026-10-13). Upstream `tonepush` (still
shipped in the bundle for the `cli` backend) keeps the bug; if it is used for a long
`watch`, the pedal needs a power cycle afterwards.

Fixed upstream: https://github.com/crmne/tonepush/pull/25, merged 2026-10-14 (`76f3bc9`), not yet in a release.

### Draft upstream report (crmne/tonepush, superseded by PR #25)

> **`tonepush watch` stops receiving notifications after ~200 on HX Effects 3.80**
>
> HX Effects, firmware 3.80 (build d972399), TonePush v0.9.0 release binary, Raspberry
> Pi 4 (aarch64, Linux). After a power cycle, `tonepush watch` prints `event 22 {68: 10,
> 121: 27}` ticks and `event 49` bypass changes from the footswitches, then after about
> 195 notifications prints nothing more. Reproduces hands-off: power cycle, run
> `timeout 180 tonepush watch`, touch nothing: ~6 ticks/s (`event 22 {82: 0, 68: 10,
> 121: 27}`) for ~35 s, then silence for the remaining 2.5 min. Same count (~190) with
> footswitches in use, where `event 49` bypass changes stop arriving too. New sessions (`watch` again, other commands) open and
> answer requests, but receive no notifications until the pedal's power is pulled.
> A client that also sends `preset_info` after every poll that received data got ~1300
> notifications before the same silence. Is there an acknowledgement on the events
> channel (0x1002) that `keepalive` does not provide?

## Live updates stop after a restart or a lost session (2026-10-14)

Symptom: the server logs `notifications=0` every 10 s while `helix-bridge watch`
gets ~60. Not the poll loop: `HX_DEBUG=1` captures of both show two device
faults (details and fix: `third_party/hx-usb/PATCHES.md` section 2).

1. **Out-of-order frame.** One events frame carried a tick with its first 4 bytes
   moved behind its body. The stream reader lost step, and the session ended with
   "stream message declares 1779853698 bytes". HX Edit's own HX Stomp capture has
   the same frame. Fixed: the events stream re-aligns and loses one notification.
2. **Deaf events channel.** A session opened 0-4 s after another one ended (a
   service restart, a reconnect after a lost session) or right after the pedal
   powered up got no answer to its events handshake, and then no notification
   at all. Every open 15 s or more after worked. Fixed: the server waits 20 s
   after any of those before opening, the drain waits out a previous session's
   notifications, and a deaf session is noticed and reopened.

Under the server's poll loop the out-of-order frame shows up every 2-5 s (one
tick lost each time); a 30 s `watch` had none. Fewer ACKs per poll might avoid
it; not tried yet.

Checked and ruled out the same evening: no USB errors in `dmesg`, `vcgencmd
get_throttled` = `0x0`, USB autosuspend off for the pedal (`power/control=on`),
no package changes.

For a clean manual test, wait 40 s after stopping the service (or another
`watch`) before starting the next session.

## Not recorded yet

Knob turn (expected event 30), preset switch (8 then 4), snapshot switch (42/46). The
backend re-reads the chain on any event not known to be noise, so these work even if
the front panel differs; a recording would only confirm it.
