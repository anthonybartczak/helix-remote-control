# HX Effects: model notes for the UI

Summary in our own words of HX Edit Pilot's Guide 3.80, "Model Lists" (pp. 57–70).
Do not copy Line 6's tables into the repo. The authoritative, machine-readable source is
HX Edit's catalog (`HX_EDIT_RESOURCES`), which TonePush already reads.

Where to read the original:
- Dev machine: `C:\Program Files (x86)\Line6\HX Edit\res\HX Edit Pilot's Guide.pdf`
- Online: https://line6.com/data/6/0a00051aff0e673cd37585ccd/application/pdf/HX%20Edit%20Pilots%20Guide%203.80%20-%20English%20.pdf

## What matters for this project

- **HX Effects has no Amp, Amp+Cab, Preamp, Cab, Mic or IR blocks.** The model browser in
  our UI must hide those categories when the device is HX Effects (matches `irs` being
  empty in `server/fixtures/`).
- **Effect categories on HX Effects:** Distortion, Dynamics, EQ, Modulation, Delay, Reverb,
  Pitch/Synth, Filter, Wah, Volume/Pan, plus Send/Return / FX Loop and Looper. Take the list
  and the category colours from the catalog, not from hard-coded strings.
- **Subcategories:** most effects come as Mono and Stereo; older Line 6 models are "Legacy".
  A few are Stereo-only or Mono-only. The UI should show this as a small badge.
- **Polyphonic pitch models** (Poly Pitch, Poly Wham, Poly Capo, 12 String, Poly Detune, Poly
  Sustain, Feedbacker) are very DSP-heavy. Expect "not enough DSP" refusals when adding them
  in Phase 6; surface the device's error, don't retry.
- **3 Note / 4 OSC Generator** make sound with nothing plugged in and default to bypassed.
- **Common parameter meanings** (Mix, Level, Trails, Headroom, Low/High Cut, Speed/Rate with
  Note Sync, Time, Scale, Spread) are useful for UI tooltips. Write tooltip text ourselves.
- Model names here match what `tonepush chain` prints with the catalog loaded (e.g.
  `Fassel`, `Simple Pitch`, `Noise Gate`, `FX Loop 1`).
