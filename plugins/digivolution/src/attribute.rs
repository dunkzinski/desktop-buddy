//! The three Digimon attributes.
//!
//! This lives in its own module because **the generator needs it**: an attribute
//! is not a label on a branch, it is a body-plan modifier. A Vaccine line grows
//! a halo and feathered tips, a Data line grows crystals and circuitry, a Virus
//! line grows fangs, back spikes and toxic drips — same archetype, visibly
//! different creature.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribute {
    Vaccine,
    Data,
    Virus,
}

impl Serialize for Attribute {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Attribute {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Attribute::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown attribute {raw:?}")))
    }
}

impl Default for Attribute {
    fn default() -> Self {
        Attribute::Data
    }
}

impl Attribute {
    pub const ALL: [Attribute; 3] = [Attribute::Vaccine, Attribute::Data, Attribute::Virus];

    pub fn as_str(self) -> &'static str {
        match self {
            Attribute::Vaccine => "Vaccine",
            Attribute::Data => "Data",
            Attribute::Virus => "Virus",
        }
    }

    /// Lowercase, filename-safe spelling.
    pub fn slug(self) -> &'static str {
        match self {
            Attribute::Vaccine => "vaccine",
            Attribute::Data => "data",
            Attribute::Virus => "virus",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vaccine" => Some(Attribute::Vaccine),
            "data" => Some(Attribute::Data),
            "virus" => Some(Attribute::Virus),
            _ => None,
        }
    }

    pub fn index(self) -> u64 {
        match self {
            Attribute::Vaccine => 0,
            Attribute::Data => 1,
            Attribute::Virus => 2,
        }
    }

    /// The attribute a branch pulls toward: high bond favours Vaccine, a low one
    /// Virus, and Data is the neutral middle.
    pub fn shifted(self, bond: f64, step: i64) -> Self {
        let base = self.index() as i64;
        let bias = if bond > 0.66 {
            -1
        } else if bond < 0.33 {
            1
        } else {
            0
        };
        let index = (base + bias + step).rem_euclid(3);
        Attribute::ALL[index as usize]
    }

    /// One line describing what this attribute does to a body, for the UI.
    pub fn flavour(self) -> &'static str {
        match self {
            Attribute::Vaccine => "haloed, feathered, gentle",
            Attribute::Data => "crystalline, plated, digital",
            Attribute::Virus => "fanged, spiked, toxic",
        }
    }
}

/// The recency-weighted attribute mix across a creature's evolution line.
///
/// An evolution line is not one attribute: it is a *history*. The initial stage
/// has an attribute, every transition records the attribute it chose, and the
/// body the generator builds is a mixture of all of them with the newest choice
/// reading loudest. This type is that rule, in one place, so the rig, the HTTP
/// payloads and the UI cannot disagree about what a creature is made of.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AttributeBlend {
    /// One weight per attribute, in [`Attribute::ALL`] order, summing to `1.0`.
    pub weights: [f64; 3],
}

/// A weight below this is too faint to be worth growing furniture for, but it is
/// still reported (see [`AttributeBlend::summary`]).
pub const VISIBLE_WEIGHT: f64 = 0.04;

impl Default for AttributeBlend {
    fn default() -> Self {
        Self::pure(Attribute::default())
    }
}

impl AttributeBlend {
    /// A creature made of exactly one attribute — a fresh egg, and every state
    /// written before the blend existed.
    pub fn pure(attribute: Attribute) -> Self {
        let mut weights = [0.0; 3];
        weights[attribute.index() as usize] = 1.0;
        Self { weights }
    }

    /// Normalise any weight vector into a distribution. Negative, NaN and
    /// infinite entries count as zero; an all-zero vector falls back to Data,
    /// which is the attribute the plugin has always defaulted to.
    pub fn from_weights(weights: [f64; 3]) -> Self {
        let cleaned = weights.map(|weight| {
            if weight.is_finite() && weight > 0.0 {
                weight
            } else {
                0.0
            }
        });
        let total: f64 = cleaned.iter().sum();
        if total <= f64::EPSILON {
            return Self::pure(Attribute::default());
        }
        let mut out = [0.0f64; 3];
        let mut sum = 0.0;
        for (slot, weight) in out.iter_mut().zip(cleaned) {
            // Six decimals is plenty for a percentage readout and keeps the
            // numbers inspectable in a save file / test failure message.
            *slot = (weight / total * 1e6).round() / 1e6;
            sum += *slot;
        }
        // Fold the rounding residue into the loudest slot so the weights sum to
        // exactly `1.0` — downstream code divides by totals.
        let dominant = out
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.partial_cmp(b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.0.cmp(&a.0))
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        out[dominant] = (out[dominant] + (1.0 - sum)).clamp(0.0, 1.0);
        Self { weights: out }
    }

    /// The blend of a whole line, oldest entry first.
    ///
    /// The initial stage's attribute plus the attribute chosen at every
    /// transition, weighted by *recency*: the oldest entry counts `1`, the next
    /// `2`, … the newest `n`, then the steps are normalised to sum to `1.0`.
    ///
    /// Linear recency is deliberate. A half-life decays so fast that a six-stage
    /// line forgets its own egg, and the brief was the opposite: the newest
    /// choice leads, and the earlier stages stay *visible*. It also means a
    /// creature that has been Data for four stages keeps a Data-dominant body
    /// when it shifts to Virus — 60/40, not 47/53 — while the fresh Virus
    /// furniture still reads at a glance.
    ///
    /// Deterministic for a given line, which is what lets a rewind reproduce an
    /// earlier body exactly.
    pub fn from_lineage(lineage: &[Attribute]) -> Self {
        if lineage.is_empty() {
            return Self::pure(Attribute::default());
        }
        let steps = recency_weights(lineage.len());
        let mut weights = [0.0f64; 3];
        for (attribute, step) in lineage.iter().zip(steps.iter()) {
            weights[attribute.index() as usize] += *step;
        }
        Self::from_weights(weights)
    }

    /// The loudest attribute.
    pub fn dominant(self) -> Attribute {
        self.loudest()
            .into_iter()
            .next()
            .map(|(attribute, _)| attribute)
            .unwrap_or(Attribute::default())
    }

    pub fn weight(self, attribute: Attribute) -> f64 {
        self.weights[attribute.index() as usize]
    }

    pub fn sum(self) -> f64 {
        self.weights.iter().sum()
    }

    /// `true` when one attribute carries the whole mix.
    pub fn is_pure(self) -> bool {
        self.weights
            .iter()
            .any(|weight| (*weight - 1.0).abs() < 1e-9)
    }

    /// Every attribute with its weight, most influential first; ties keep
    /// [`Attribute::ALL`] order so the result never depends on float noise.
    pub fn loudest(self) -> Vec<(Attribute, f64)> {
        let mut out: Vec<(Attribute, f64)> = Attribute::ALL
            .iter()
            .map(|attribute| (*attribute, self.weight(*attribute)))
            .collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.index().cmp(&b.0.index()))
        });
        out
    }

    /// Whole percentages that always total 100, for the UI's chips and bars.
    pub fn percentages(self) -> [u8; 3] {
        let mut out = [0u8; 3];
        let mut sum = 0i32;
        for (slot, weight) in out.iter_mut().zip(self.weights) {
            *slot = (weight * 100.0).round().clamp(0.0, 100.0) as u8;
            sum += *slot as i32;
        }
        // The loudest chip absorbs the rounding, so the readout always adds up.
        let dominant = self.dominant().index() as usize;
        out[dominant] = (out[dominant] as i32 + (100 - sum)).clamp(0, 100) as u8;
        out
    }

    /// How large and frequent an attribute's furniture should be: `1.0` at the
    /// loudest share, tapering toward `0.6` for a faint one.
    ///
    /// Scaling relative to the peak (rather than to an absolute weight) is what
    /// keeps a one-hot blend at exactly `1.0`: a pure creature is generated by
    /// the blend machinery without being changed by it.
    pub fn prominence(self, attribute: Attribute) -> f64 {
        let peak = self.weights.iter().cloned().fold(0.0f64, f64::max);
        if peak <= 0.0 {
            return 0.0;
        }
        0.6 + 0.4 * (self.weight(attribute) / peak)
    }

    /// `58% Data · 42% Virus`, dropping attributes that round to nothing.
    pub fn summary(self) -> String {
        let percents = self.percentages();
        self.loudest()
            .into_iter()
            .filter_map(|(attribute, _)| {
                let percent = percents[attribute.index() as usize];
                (percent > 0).then(|| format!("{percent}% {}", attribute.as_str()))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The traits the mix actually grows, loudest first. A one-hot blend returns
    /// that attribute's own flavour line unchanged.
    pub fn blended_flavour(self) -> String {
        self.loudest()
            .into_iter()
            .filter(|(_, weight)| *weight >= VISIBLE_WEIGHT)
            .map(|(attribute, _)| attribute.flavour())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Raw (unnormalised) recency weights for an `n`-stage line, oldest first: the
/// linear ramp `1, 2, …, n`.
///
/// Exposed because it is the one number worth inspecting when a blend looks
/// wrong: `n = 3` gives `[1, 2, 3]`, so the newest stage counts three times the
/// oldest.
pub fn recency_weights(n: usize) -> Vec<f64> {
    (1..=n).map(|step| step as f64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_its_wire_spelling() {
        for attribute in Attribute::ALL {
            assert_eq!(Attribute::parse(attribute.as_str()), Some(attribute));
            assert_eq!(Attribute::parse(&attribute.slug()), Some(attribute));
        }
        assert_eq!(Attribute::parse("mega-ultra"), None);
    }

    #[test]
    fn every_attribute_has_its_own_flavour() {
        let mut seen = Vec::new();
        for attribute in Attribute::ALL {
            assert!(!seen.contains(&attribute.flavour()));
            seen.push(attribute.flavour());
        }
    }

    #[test]
    fn bond_shifts_the_family_toward_its_temperament() {
        // A devoted creature leans Vaccine, a feral one leans Virus.
        let devoted = Attribute::Virus.shifted(0.9, 0);
        assert_eq!(devoted, Attribute::Data);
        let feral = Attribute::Vaccine.shifted(0.05, 0);
        assert_eq!(feral, Attribute::Data);
    }

    #[test]
    fn the_newest_stages_weigh_most_and_the_older_ones_stay_visible() {
        // A shift that just happened leads the mix.
        let shift = AttributeBlend::from_lineage(&[Attribute::Data, Attribute::Virus]);
        assert_eq!(shift.dominant(), Attribute::Virus);
        assert!(shift.weight(Attribute::Virus) > shift.weight(Attribute::Data));

        // Four stages of Data and then a Virus shift: the long history still
        // leads (60/40), and the recent shift is plainly on the body.
        let long = AttributeBlend::from_lineage(&[
            Attribute::Data,
            Attribute::Data,
            Attribute::Data,
            Attribute::Virus,
        ]);
        assert_eq!(long.dominant(), Attribute::Data);
        assert!(long.weight(Attribute::Virus) >= 0.35, "the recent shift stays visible");
        assert!((long.sum() - 1.0).abs() < 1e-12, "normalised: {}", long.sum());
        assert_eq!(long.percentages().iter().map(|p| *p as u32).sum::<u32>(), 100);
        assert!(long.summary().contains("Virus"), "{}", long.summary());
        assert!(long.blended_flavour().contains("crystalline"));
    }

    #[test]
    fn a_one_hot_blend_is_the_pure_attribute_it_came_from() {
        for attribute in Attribute::ALL {
            let blend = AttributeBlend::from_lineage(&[attribute]);
            assert_eq!(blend, AttributeBlend::pure(attribute));
            assert!(blend.is_pure());
            assert_eq!(blend.prominence(attribute), 1.0);
            assert_eq!(blend.blended_flavour(), attribute.flavour());
            assert_eq!(blend.summary(), format!("100% {}", attribute.as_str()));
            for other in Attribute::ALL {
                if other != attribute {
                    assert_eq!(blend.prominence(other), 0.6);
                }
            }
        }
        // An empty line has nothing to mix: Data, the historical default.
        assert_eq!(
            AttributeBlend::from_lineage(&[]),
            AttributeBlend::pure(Attribute::default())
        );
    }

    #[test]
    fn the_weights_are_deterministic_monotone_and_sane() {
        assert_eq!(recency_weights(4), vec![1.0, 2.0, 3.0, 4.0]);
        let lineage = [
            Attribute::Vaccine,
            Attribute::Data,
            Attribute::Data,
            Attribute::Virus,
        ];
        assert_eq!(
            AttributeBlend::from_lineage(&lineage),
            AttributeBlend::from_lineage(&lineage)
        );
        // A reordered line is a different creature (recency is the whole point).
        let mut shuffled = lineage;
        shuffled.swap(0, 3);
        assert_ne!(
            AttributeBlend::from_lineage(&lineage),
            AttributeBlend::from_lineage(&shuffled)
        );
        // Nonsense weights still normalise into a distribution.
        assert_eq!(
            AttributeBlend::from_weights([f64::NAN, -2.0, 0.0]),
            AttributeBlend::pure(Attribute::default())
        );
        assert_eq!(AttributeBlend::from_weights([1.0, 1.0, 2.0]).percentages(), [25, 25, 50]);
    }
}
