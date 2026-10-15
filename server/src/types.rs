//! Effect types inside HX Edit's categories, for the model picker: Boost,
//! Overdrive, Distortion and Fuzz inside Distortion, and so on.
//!
//! Line 6 has no such grouping: HX Edit and the device stop at category and
//! Mono / Stereo / Legacy shelf. This table is ours, written 2026-10-15 for HX
//! Edit 3.80 from the gear each model is based on (Pilot's Guide pp. 57–62)
//! and what the original pedals are generally known as. Judgement calls:
//! Hedgehog D9 (Maxon SD-9) and KWB are Distortion, as their makers call them;
//! Legendary Drive (a high-gain channel) is Distortion; Horizon Drive, Pillars
//! and Compulsive Drive are Overdrive; bass drives and DIs get their own type.
//!
//! Keyed by category and model name as HX Edit spells them (Mono and Stereo
//! share a name). A model missing here lands in "Other"; a test checks the
//! table against the catalog when this machine has one.

/// An effect type and the models in it.
type Type = (&'static str, &'static [&'static str]);

/// (category, [(type, [model names])]), types in display order.
const TYPES: &[(&str, &[Type])] = &[
    (
        "Distortion",
        &[
            ("Boost", &["Kinky Boost", "Deranged Master", "Vital Boost"]),
            (
                "Overdrive",
                &[
                    "Minotaur",
                    "Teemah!",
                    "Heir Apparent",
                    "Tone Sovereign",
                    "Alpaca Rouge",
                    "Compulsive Drive",
                    "Dyhana Drive",
                    "Horizon Drive",
                    "Valve Driver",
                    "Top Secret OD",
                    "Prize Drive",
                    "Scream 808",
                    "Pillars",
                    "Stupor OD",
                    "Tube Drive",
                    "Screamer",
                    "Overdrive",
                    "Colordrive",
                    "L6 Drive",
                ],
            ),
            (
                "Distortion",
                &[
                    "Hedgehog D9",
                    "Deez One Vintage",
                    "Deez One Mod",
                    "Ratatouille Dist",
                    "Vermin Dist",
                    "Vital Dist",
                    "KWB",
                    "Legendary Drive",
                    "Swedish Chainsaw",
                    "Classic Dist",
                    "Heavy Dist",
                    "L6 Distortion",
                    "Killer Z",
                ],
            ),
            (
                "Fuzz",
                &[
                    "Arbitrator Fuzz",
                    "Pocket Fuzz",
                    "Bighorn Fuzz",
                    "Triangle Fuzz",
                    "Dark Dove Fuzz",
                    "Ballistic Fuzz",
                    "Industrial Fuzz",
                    "Tycoctavia Fuzz",
                    "Wringer Fuzz",
                    "Thrifter Fuzz",
                    "Xenomorph Fuzz",
                    "Buzz Saw",
                    "Facial Fuzz",
                    "Jumbo Fuzz",
                    "Fuzz Pi",
                    "Jet Fuzz",
                    "Sub Oct Fuzz",
                    "Octave Fuzz",
                ],
            ),
            (
                "Bass",
                &[
                    "Ampeg Scrambler",
                    "ZeroAmp Bass DI",
                    "Regal Bass DI",
                    "Obsidian 7000",
                    "Clawthorn Drive",
                    "Bronze Master",
                ],
            ),
            ("Lo-fi", &["Megaphone", "Bitcrusher"]),
        ],
    ),
    (
        "Dynamics",
        &[
            (
                "Compressor",
                &[
                    "Deluxe Comp",
                    "Red Squeeze",
                    "Kinky Comp",
                    "Ampeg Opto Comp",
                    "Rochester Comp",
                    "LA Studio Comp",
                    "3-Band Comp",
                    "Tube Comp",
                    "Red Comp",
                    "Blue Comp",
                    "Blue Comp Treb",
                    "Vetta Comp",
                    "Vetta Juice",
                    "Boost Comp",
                ],
            ),
            ("Gate", &["Noise Gate", "Hard Gate", "Horizon Gate"]),
            ("Swell & Feedback", &["Autoswell", "Feedbacker"]),
        ],
    ),
    (
        "EQ",
        &[
            (
                "Tone & Cut",
                &[
                    "Simple EQ",
                    "Low and High Cut",
                    "Low/High Shelf",
                    "Simple Tilt",
                    "Parametric",
                ],
            ),
            ("Graphic", &["10 Band Graphic", "Cali Q Graphic"]),
            ("Acoustic", &["Acoustic Sim"]),
        ],
    ),
    (
        "Modulation",
        &[
            (
                "Chorus",
                &[
                    "Chorus",
                    "70s Chorus",
                    "PlastiChorus",
                    "Ampeg Liquifier",
                    "4-Voice Chorus",
                    "Trinity Chorus",
                    "Double Take",
                    "Analog Chorus",
                    "Tri Chorus",
                    "Dimension",
                ],
            ),
            (
                "Flanger",
                &[
                    "Gray Flanger",
                    "Harmonic Flanger",
                    "Courtesan Flange",
                    "Dynamix Flanger",
                    "Analog Flanger",
                    "Jet Flanger",
                    "AC Flanger",
                    "80A Flanger",
                ],
            ),
            (
                "Phaser",
                &[
                    "Script Mod Phase",
                    "Pebble Phaser",
                    "Deluxe Phaser",
                    "Script Phase",
                    "Panned Phaser",
                    "Barberpole Phaser",
                    "Dual Phaser",
                    "Phaser",
                ],
            ),
            (
                "Tremolo & Pan",
                &[
                    "Optical Trem",
                    "60s Bias Trem",
                    "Tremolo",
                    "Harmonic Tremolo",
                    "Bleat Chop Trem",
                    "Pattern Trem",
                    "Panner",
                    "Bias Tremolo",
                    "Opto Tremolo",
                ],
            ),
            (
                "Vibe & Rotary",
                &[
                    "Ubiquitous Vibe",
                    "FlexoVibe",
                    "U-Vibe",
                    "Vibe Rotary",
                    "122 Rotary",
                    "145 Rotary",
                    "Triple Rotary",
                    "Rotary Drum",
                    "Rotary Drum/Horn",
                ],
            ),
            (
                "Vibrato & Warble",
                &[
                    "Bubble Vibrato",
                    "Pitch Vibrato",
                    "Poly Detune",
                    "Retro Reel",
                    "Tape Eater",
                    "Warble Eater",
                ],
            ),
            (
                "Ring Mod & Other",
                &[
                    "AM Ring Mod",
                    "Pitch Ring Mod",
                    "Ring Modulator",
                    "Freq Shift",
                    "Random S&H",
                    "Sweeper",
                ],
            ),
        ],
    ),
    (
        "Delay",
        &[
            (
                "Digital",
                &[
                    "Simple Delay",
                    "Vintage Digital",
                    "Ducked Delay",
                    "Ping Pong",
                    "Dynamic",
                    "Stereo",
                    "Digital",
                    "Digital w/Mod",
                    "Lo Res",
                ],
            ),
            (
                "Analog",
                &[
                    "Bucket Brigade",
                    "Adriatic Delay",
                    "Elephant Man",
                    "Mod/Chorus Echo",
                    "Analog Echo",
                    "Analog w/Mod",
                ],
            ),
            (
                "Tape & Echo",
                &[
                    "Transistor Tape",
                    "Cosmos Echo",
                    "Tube Echo",
                    "Tape Echo",
                    "Echo Platter",
                    "Multi-Head",
                ],
            ),
            (
                "Multi-tap & Rhythmic",
                &[
                    "Dual Delay",
                    "Multitap 4",
                    "Multitap 6",
                    "Multi Pass",
                    "Euclidean Delay",
                    "Crisscross",
                    "Tesselator",
                    "Ratchet",
                ],
            ),
            (
                "Creative",
                &[
                    "Reverse Delay",
                    "Reverse",
                    "Sweep Echo",
                    "Vintage Swell",
                    "Adriatic Swell",
                    "Auto-Vol Echo",
                    "Pitch Echo",
                    "Harmony Delay",
                    "Heliosphere",
                    "Poly Sustain",
                    "Glitch Delay",
                    "ADT",
                    "Bubble Echo",
                    "Phaze Eko",
                ],
            ),
        ],
    ),
    (
        "Reverb",
        &[
            ("Room", &["Dynamic Room", "Room", "Chamber", "Tile"]),
            ("Hall", &["Dynamic Hall", "Hall", "Cave"]),
            ("Plate", &["Dynamic Plate", "Plate"]),
            ("Spring", &["Hot Springs", "'63 Spring", "Spring"]),
            (
                "Ambient & Creative",
                &[
                    "Dynamic Ambience",
                    "Dynamic Bloom",
                    "Shimmer",
                    "Nonlinear",
                    "Glitz",
                    "Ganymede",
                    "Searchlights",
                    "Plateaux",
                    "Double Tank",
                    "Echo",
                    "Ducking",
                    "Octo",
                    "Particle Verb",
                ],
            ),
        ],
    ),
    (
        "Pitch/Synth",
        &[
            (
                "Pitch & Harmony",
                &[
                    "Pitch Wham",
                    "Twin Harmony",
                    "Simple Pitch",
                    "Dual Pitch",
                    "Smart Harmony",
                ],
            ),
            ("Octave", &["Boctaver", "Bass Octaver", "Double Bass"]),
            (
                "Polyphonic",
                &["Poly Pitch", "Poly Wham", "Poly Capo", "12-String"],
            ),
            (
                "Synth",
                &[
                    "3 Osc Synth",
                    "Octi Synth",
                    "Synth O Matic",
                    "Attack Synth",
                    "Synth String",
                    "Growler",
                    "Buzz Wave",
                    "Rez Synth",
                    "Seismik Synth",
                    "Analog Synth",
                    "Synth Lead",
                    "String Theory",
                    "Synth FX",
                    "Saturn 5 RingMod",
                    "Synth Harmony",
                ],
            ),
            ("Generator", &["3 Note Generator", "4 OSC Generator"]),
        ],
    ),
    (
        "Filter",
        &[
            (
                "Envelope",
                &[
                    "Mutant Filter",
                    "V Tron",
                    "Tron Up",
                    "Tron Down",
                    "Spin Cycle",
                    "Slow Filter",
                ],
            ),
            (
                "LFO & Step",
                &[
                    "Mystery Filter",
                    "Auto Filter",
                    "Asheville Pattrn",
                    "Seeker",
                    "Obi Wah",
                    "Throbber",
                    "Comet Trails",
                ],
            ),
            ("Fixed & Vocal", &["Q Filter", "Voice Box"]),
        ],
    ),
];

/// Where a model's type is not in the table.
pub const OTHER: &str = "Other";

/// The type of model `name` in `category`. `None` for a category without
/// types (Wah, Volume/Pan, Send/Return); [`OTHER`] for a model the table
/// does not name.
pub fn of(category: &str, name: &str) -> Option<&'static str> {
    let (_, types) = TYPES.iter().find(|(c, _)| *c == category)?;
    Some(
        types
            .iter()
            .find(|(_, models)| models.contains(&name))
            .map_or(OTHER, |(t, _)| *t),
    )
}

/// A category's types in display order, [`OTHER`] last. Empty without types.
pub fn order(category: &str) -> Vec<&'static str> {
    TYPES
        .iter()
        .find(|(c, _)| *c == category)
        .map(|(_, types)| {
            types
                .iter()
                .map(|(t, _)| *t)
                .chain(std::iter::once(OTHER))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_types() {
        assert_eq!(of("Distortion", "Kinky Boost"), Some("Boost"));
        assert_eq!(of("Distortion", "Minotaur"), Some("Overdrive"));
        assert_eq!(of("Distortion", "Swedish Chainsaw"), Some("Distortion"));
        assert_eq!(of("Distortion", "Bighorn Fuzz"), Some("Fuzz"));
        // A name the table does not know, and a category without types.
        assert_eq!(of("Distortion", "Brand New Drive"), Some(OTHER));
        assert_eq!(of("Wah", "Fassel"), None);
        assert_eq!(order("Wah"), Vec::<&str>::new());
        assert_eq!(
            order("Distortion"),
            [
                "Boost",
                "Overdrive",
                "Distortion",
                "Fuzz",
                "Bass",
                "Lo-fi",
                OTHER
            ]
        );
    }

    /// No model in two types of one category, and no type twice.
    #[test]
    fn table_is_consistent() {
        for (category, types) in TYPES {
            let mut names: Vec<&str> = types.iter().flat_map(|(_, m)| m.iter().copied()).collect();
            let count = names.len();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), count, "{category}: a model in two types");
            let mut kinds: Vec<&str> = types.iter().map(|(t, _)| *t).collect();
            kinds.dedup();
            assert_eq!(kinds.len(), types.len(), "{category}");
        }
    }
}
