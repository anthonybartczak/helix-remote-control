# Patches to TonePush hx-usb

Vendored from [TonePush](https://github.com/crmne/tonepush) tag `v0.9.0` (commit
`5e44812`), MIT (see `LICENSE`). The first commit adding this folder is the
unchanged upstream copy (only `Cargo.toml`'s workspace keys made explicit), so
`git diff` against it shows exactly what we changed. `server/Cargo.toml` swaps it
in with `[patch."https://github.com/crmne/tonepush"]`. The `tonepush` CLI shipped in
the Pi bundle is still built from upstream and does **not** have these patches.

Drop this folder once upstream has an equivalent fix. Upstream pull request:
https://github.com/crmne/tonepush/pull/25 (opened 2026-10-13). Once it is merged and
released, move the TonePush pin to that release, delete this folder and the
`[patch]` section in `server/Cargo.toml`. The patch prepared for
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
