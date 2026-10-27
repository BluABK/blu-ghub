//! Equaliser presets: blu-ghub's own curves, and the user's, saved by name
//! in the config (`[[headset.eq_presets]]`).

use crate::config::EqPreset;

/// One of blu-ghub's own presets, as a curve of (Hz, dB) points: joined
/// straight on a log frequency scale and level beyond the ends, so it fits
/// whatever bands the equaliser has.
pub struct Standard {
    pub name: &'static str,
    pub about: &'static str,
    points: &'static [(f32, f32)],
}

pub const STANDARD: [Standard; 7] = [
    Standard { name: "Flat", about: "Every band at 0 dB: the sound as it comes.", points: &[] },
    Standard {
        name: "Bass boost",
        about: "More low end, the mids and treble left alone.",
        points: &[(50.0, 6.0), (150.0, 4.0), (400.0, 1.0), (800.0, 0.0)],
    },
    Standard {
        name: "Treble boost",
        about: "Brighter: more presence, treble and air.",
        points: &[(1500.0, 0.0), (4000.0, 2.0), (8000.0, 4.0), (12000.0, 5.0)],
    },
    Standard {
        name: "Voice",
        about: "Speech forward: less rumble, more of the mids where words are.",
        points: &[
            (50.0, -3.0),
            (250.0, 0.0),
            (1000.0, 2.0),
            (2500.0, 3.0),
            (5000.0, 2.0),
            (10000.0, 0.0),
        ],
    },
    Standard {
        name: "V-shape",
        about: "Lows and highs up, mids down: lively for music.",
        points: &[
            (50.0, 5.0),
            (150.0, 3.0),
            (500.0, 0.0),
            (1200.0, -2.0),
            (3000.0, 0.0),
            (7000.0, 3.0),
            (12000.0, 4.0),
        ],
    },
    Standard {
        name: "Cinema",
        about: "Deep lows for effects, with dialogue kept clear.",
        points: &[
            (50.0, 4.0),
            (120.0, 2.5),
            (300.0, 0.0),
            (2000.0, 1.0),
            (4000.0, 2.0),
            (10000.0, 1.0),
        ],
    },
    Standard {
        name: "Footsteps",
        about: "For games: less boom, more of the high mids where steps and reloads are.",
        points: &[
            (50.0, -3.0),
            (250.0, -1.0),
            (1000.0, 0.0),
            (2500.0, 3.0),
            (4500.0, 4.0),
            (9000.0, 1.0),
            (12000.0, 0.0),
        ],
    },
];

impl Standard {
    /// The curve's gain at each of `freqs`, to 0.5 dB.
    pub fn gains(&self, freqs: &[f32]) -> Vec<f32> {
        freqs.iter().map(|&hz| (at(self.points, hz) * 2.0).round() / 2.0 + 0.0).collect()
    }
}

fn at(points: &[(f32, f32)], hz: f32) -> f32 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else { return 0.0 };
    if hz <= first.0 {
        return first.1;
    }
    for w in points.windows(2) {
        let ((f0, g0), (f1, g1)) = (w[0], w[1]);
        if hz <= f1 {
            let t = (hz.ln() - f0.ln()) / (f1.ln() - f0.ln());
            return g0 + t * (g1 - g0);
        }
    }
    last.1
}

/// The name of the preset `gains` is, if any: blu-ghub's first, then the
/// user's.
pub fn matching(gains: &[f32], freqs: &[f32], custom: &[EqPreset]) -> Option<String> {
    let same = |other: &[f32]| {
        other.len() == gains.len() && other.iter().zip(gains).all(|(a, b)| (a - b).abs() < 0.01)
    };
    STANDARD
        .iter()
        .find(|s| same(&s.gains(freqs)))
        .map(|s| s.name.to_owned())
        .or_else(|| custom.iter().find(|p| same(&p.gains)).map(|p| p.name.clone()))
}

/// Is `name` one of blu-ghub's own (which a saved preset cannot replace)?
pub fn is_standard(name: &str) -> bool {
    STANDARD.iter().any(|s| s.name.eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invented bands, not any device's.
    const FREQS: [f32; 10] =
        [32.0, 64.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

    #[test]
    fn curves_fit_any_bands_in_half_db_steps() {
        for s in &STANDARD {
            let g = s.gains(&FREQS);
            assert_eq!(g.len(), FREQS.len());
            for x in g {
                assert!((-12.0..=12.0).contains(&x), "{}: {x}", s.name);
                assert_eq!((x * 2.0).fract(), 0.0, "{}: {x}", s.name);
            }
        }
        assert!(STANDARD[0].gains(&FREQS).iter().all(|g| *g == 0.0));
        // Bass boost: up at the bottom, level from 800 Hz.
        let bass = STANDARD[1].gains(&FREQS);
        assert_eq!(bass[0], 6.0);
        assert_eq!(bass[9], 0.0);
        // Half way between 50 and 150 Hz on a log scale is about 87 Hz.
        assert!((at(STANDARD[1].points, 86.6) - 5.0).abs() < 0.01);
    }

    #[test]
    fn presets_are_recognised_by_their_gains() {
        let custom = [EqPreset { name: "Mine".into(), gains: vec![1.0; 10] }];
        assert_eq!(matching(&[0.0; 10], &FREQS, &custom).as_deref(), Some("Flat"));
        assert_eq!(matching(&[1.0; 10], &FREQS, &custom).as_deref(), Some("Mine"));
        assert_eq!(matching(&[2.0; 10], &FREQS, &custom), None);
        assert_eq!(
            matching(&STANDARD[4].gains(&FREQS), &FREQS, &custom).as_deref(),
            Some("V-shape")
        );
        assert!(is_standard(" flat "));
        assert!(!is_standard("Mine"));
    }
}
