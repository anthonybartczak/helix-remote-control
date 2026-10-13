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

## Not recorded yet

Knob turn (expected event 30), preset switch (8 then 4), snapshot switch (42/46). The
backend re-reads the chain on any event not known to be noise, so these work even if
the front panel differs; a recording would only confirm it.
