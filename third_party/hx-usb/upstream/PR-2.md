<!--
Not part of the pull request text (GitHub hides this comment). Notes for sending:
- Base: crmne/tonepush main at d4094a3 (v0.12.1 plus 10 commits), 2026-11-11.
- Apply: git am PR-2-0001-*.patch PR-2-0002-*.patch PR-2-0003-*.patch
- Commit 3 (the 30 s drain) is the part most likely to be refused. To send
  without it, apply only the first two patches, delete item 3 under "Change",
  the sentence about it under "Testing", and "Commit 3 is the one ..." at the end.
- Upstream wants review evidence attached to the pull request, not committed:
  attach the `tonepush watch` output with the error and the 180 s watch log
  if you still have them, and say so under "Testing".
-->

# Survive an out-of-order events frame, and say when the events channel did not answer

No user-visible UI change. On an HX Effects, `tonepush watch` keeps running where it now ends after a few seconds.

## Problem

Two faults on the events channel (`0x1002`) of an HX Effects (firmware 3.80), both seen from a Raspberry Pi 4 (aarch64 Linux). This follows #25.

1. **A frame sent out of order.** Now and then the device sends one notification with its first four bytes behind the rest of it:

   ```text
   expected  00 00 04 00 | 0d 00 00 00 <13-byte body> | 00 00 04 00 0d ...
   received  0d 00 00 00 <13-byte body> | 00 00 04 00 | 00 00 04 00 0d ...
   ```

   The byte count is right, so acknowledgements stay right, but the stream reader takes a body for a length. With the v0.12.1 release binary, `tonepush watch` ends after about 5 seconds with `protocol error: stream message declares 1779853698 bytes` (that is `0d 00 00 00 82 69 16 6a` read as a header). The other outcome is `00 00 04 00 00 00 04 00` read as a 262144-byte message, which the reader waits for while every later notification queues behind it.

   How often it happens varies with the setup. With the pedal on a port of its own: one such frame in three days of a mostly idle client, but one every 2 to 5 s while knobs were turned on the pedal. With the pedal behind a 4-port USB hub (since 2026-11-11): about 3 KB of them in three minutes of idle watching.

2. **An events handshake left unanswered.** A session opened 0 to 4 s after another one ended, or 4 s after the pedal re-enumerated, got no reply to its HELLO on `0x1002`. Requests work, but no notification arrives for the whole session. Every open 15 s or more after worked. `crates/hx-usb/tests/fixtures/session.transcript` records the same thing: 42 stale events frames before the handshake, the control and data HELLOs answered, the events one not, and no events frame afterwards.

## Change

Three commits, one topic each.

1. **Keep the events stream on message boundaries.** New `realign.rs`, used on the events channel only. It passes on whole messages that start `00 00 04 00` (or service 0, as one captured service-open reply does), declare at most 4 KB (the device never has more than that unacknowledged, #25) and have a body that decodes, as an RPC message on service 4. Anything else is stepped over a byte at a time until the stream lines up again. A slip costs the one notification it cut through. `Session::events_skipped()` counts the bytes, and `HX_DEBUG` prints each slip. The body check is needed: stepping through a parameter notification, `ca 00 00 00 00` (a float 0.0) and the key after it pass as a header, and the reader then failed with "stream message is not MessagePack".
2. **Say whether the events channel answered its handshake.** `route` notes each channel's HELLO reply and `Session::events_answered()` reports the events channel's. The handshake bytes are unchanged (the replay test still matches) and nothing is retried: when to open again stays the caller's decision.
3. **Wait out a previous session's notifications before a handshake.** `drain` keeps reading while transfers carry events data, for up to 30 s in all instead of 3. Acknowledgements and keep-alives do not extend it, so a quiet pedal is drained exactly as before. The cost: a command run right after another session, on a pedal that is still sending that session's ticks, waits longer before its handshake.

`PROTOCOL.md` gets two paragraphs: "The events stream can lose its place" and "An events handshake can go unanswered".

Not in this pull request: helix-bridge, the client these were written for, also waits 20 s after a session ends before it opens the next one, and reopens a session whose events channel did not answer, up to 3 times. That is its own policy and stays there.

## Testing

- `cargo fmt --all --check` and `cargo test -p hx-usb` on Windows x86-64 with the pinned 1.99.1 toolchain: 75 unit tests and the replay test pass. Each commit was checked on its own (73, 74 and 75 unit tests).
- `cargo clippy -p hx-usb --all-targets -- -D warnings` is clean. The workspace command from CI, `cargo clippy --all-targets -- -D warnings`, stops on Windows at `crates/voidx-client/src/backup.rs:1050` (`let mut options`, changed only under `#[cfg(unix)]`), a file this branch does not touch, and `hx-ruby` does not build here without Ruby. With those two set aside (`cargo clippy --workspace --exclude hx-ruby --all-targets -- -D warnings -A unused-mut`) the workspace is clean.
- I did not run `cargo test --workspace` or the GUI, and nothing here was run on Linux or macOS.
- New tests: `realign::tests` (whole messages, a message split across frames, the out-of-order frame, the same right after a split, the service-open reply, a plausible header over a body that is not a message, a huge length), `a_frame_sent_out_of_order_costs_one_notification` (through `poll_notifications`), `a_session_says_whether_the_events_channel_answered`, `a_drain_waits_on_notifications_only`. The fake pedal now answers handshakes.
- The HX Edit captures in `captures/`: I put every events frame of all 17 logs through `realign` and the stream reader with a throwaway test. All 70 507 messages pass. The only bytes stepped over are 17 at the head of `mac-enums-capture.log`, the rest of a message that began before the capture did. None of the captures holds an out-of-order frame (there the HX Stomp only splits a message across two frames, in order), so I have no evidence of fault 1 on anything but an HX Effects.
- Hardware, HX Effects 3.80 on a Raspberry Pi 4, 2026-11-11, pedal untouched, behind the USB hub. The v0.12.1 release `tonepush watch`: ends after about 5 s with the error above. The same three changes on top of v0.12.1's hx-usb, run through a small client that does what `watch` does (poll, sleep 700 ms, keep-alive), 180 s: 1135 notifications, 2961 bytes stepped over, session kept. This branch itself (main at `d4094a3` plus the three commits) was not built and run on the device.
- Same pedal on a port of its own, 2026-10-15, knobs turned on several blocks for about 1.5 minutes: 39 out-of-order frames stepped over (7 to 56 bytes each), no session lost. Without the body check the same use ended the session within about a minute, twice.
- Commits 2 and 3: the unanswered handshake was observed on hardware as described under "Problem". The accessor and the longer drain are covered by the tests above only. The longer drain has not been measured on hardware on its own, because the client it was written for waits 20 s before opening a session anyway.
- Not tested on an HX Stomp or any other device.

I only have an HX Effects. Could you run this against your HX Stomp (`cargo test -p hx-usb --test device -- --ignored`) before merging? Commit 3 is the one that can change timing there, since those tests open sessions back to back. If you would rather not have it, say so and I will drop it; the first two commits stand without it.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
