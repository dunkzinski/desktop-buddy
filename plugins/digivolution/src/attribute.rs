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
}
