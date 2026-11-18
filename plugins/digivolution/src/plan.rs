//! Body plans — what makes a Drake not just a recoloured Beast.
//!
//! The first version of the generator had one skeleton with four palettes, so
//! every archetype came out the same chubby biped in a different colour. A plan
//! is the missing layer: it decides *what shape* the creature is, while
//! [`crate::generator`] decides how much of it exists at each tier.
//!
//! Everything here is a pure constant table — no RNG, no clock — so
//! `(seed, archetype, tier)` still reproduces a creature byte for byte.

use crate::attribute::Attribute;
use crate::generator::Archetype;

/// Skull and face construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadKind {
    /// Round skull with a soft muzzle — the classic beast.
    Round,
    /// Long snout with fangs and a heavy jaw — draconic.
    Muzzle,
    /// Tall dome, wide brow, small chin — serene.
    Dome,
    /// Very wide, very low skull with oversized eyes — aquatic.
    Bulbous,
    /// Flat wide skull with a visor band and a chin plate — mechanical.
    Boxy,
}

/// What sits either side of the skull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarKind {
    /// Short upright cones.
    Pointed,
    /// Long leaf ears that swivel back — deer.
    Long,
    /// Layered side fans that read as gills — fish.
    Gill,
    /// Thin rods with a bulb on the end.
    Antenna,
    /// Rods that end in a glowing ball — alien.
    Stalk,
    /// Flat discs pushed to the sides — holy.
    Disc,
    None,
}

/// What grows out of the top of the skull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrestKind {
    /// Short paired horns.
    Horns,
    /// Branching, many-tined antlers — deer.
    Antlers,
    /// A single swept blade.
    Blade,
    /// A whip antenna with a light on the tip.
    Antenna,
    /// A fan of short spines.
    Spikes,
    /// A tall centre fin.
    Fin,
    None,
}

/// Front limbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmKind {
    /// Two segments ending in a hand with claws.
    Claw,
    /// Two segments ending in a plain hand.
    Hand,
    /// Short, wide, no elbow — aquatic.
    Flipper,
    /// Thin segmented tendrils that curl — alien.
    Tentacle,
    /// Heavy barrel on a piston — mechanical.
    Cannon,
}

/// Support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegKind {
    /// Thigh, shin, foot.
    Biped,
    /// Slim digitigrade leg ending in a hoof — deer.
    Hoofed,
    /// Stubby fin rather than a leg — fish.
    Finned,
    /// A wide tread block with rollers — mechanical.
    Treads,
    /// Nothing at all: the body floats on a skirt.
    Hover,
}

/// What trails behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailKind {
    /// Tapering blobs ending in a paddle fin.
    BlobFin,
    /// Two wide flat fins fanned out — fish.
    Fan,
    /// One short tuft — deer.
    Stub,
    /// Thin segments with a plug on the end — mechanical.
    Cable,
    /// Blobs that taper into a hooked tip — alien.
    Tentacle,
}

/// The trunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorsoKind {
    /// Chest ellipse, belly, collar.
    Round,
    /// Ribbed barrel with side rings — draconic.
    Barrel,
    /// Wide, short, plated — mechanical.
    Slab,
    /// Chest over a flared skirt — holy.
    Robe,
    /// A bell-shaped mantle over a glowing core — alien.
    Mantle,
}

/// A creature's whole silhouette in one table.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub head: HeadKind,
    pub ears: EarKind,
    pub crest: CrestKind,
    pub arms: ArmKind,
    pub legs: LegKind,
    pub tail: TailKind,
    pub torso: TorsoKind,
    /// Shoulder wings (bats, feathers, energy panels).
    pub wings: bool,
    pub armor: bool,
    // Proportions: multipliers on the tier baseline. The tier still scales
    // everything up; these decide the *shape* of what is scaled.
    pub torso_w: f64,
    pub torso_h: f64,
    pub head_w: f64,
    pub head_h: f64,
    pub arm_len: f64,
    pub leg_len: f64,
    /// Extra neck length in px at Rookie and above.
    pub neck: f64,
    // Details that only some plans carry.
    pub dorsal_fin: bool,
    pub gills: bool,
    pub visor: bool,
    pub engine: bool,
    pub spots: bool,
    // ---- what the attribute adds on top of the plan ----
    /// A ring of light above the head (Vaccine).
    pub halo_ring: bool,
    /// Feather tips on the wings (Vaccine).
    pub feathers: bool,
    /// Crystal growth on the shoulders and back (Data).
    pub crystals: bool,
    /// Circuit traces on the trunk (Data).
    pub circuit: bool,
    /// A row of spines down the back (Virus).
    pub back_spikes: bool,
    /// Extra fangs and a clawed grip (Virus).
    pub fangs: bool,
    /// Toxic drips from the jaw and tail (Virus).
    pub drips: bool,
    /// Seeded marking: 0 none, 1 spots, 2 stripes.
    pub marking: u8,
    /// The seeded build: 0 canonical, 1 lean, 2 broad.
    pub variant: u8,
}

const fn base() -> Plan {
    Plan {
        head: HeadKind::Round,
        ears: EarKind::Pointed,
        crest: CrestKind::Horns,
        arms: ArmKind::Claw,
        legs: LegKind::Biped,
        tail: TailKind::BlobFin,
        torso: TorsoKind::Round,
        wings: false,
        armor: false,
        torso_w: 1.0,
        torso_h: 1.0,
        head_w: 1.0,
        head_h: 1.0,
        arm_len: 1.0,
        leg_len: 1.0,
        neck: 0.0,
        dorsal_fin: false,
        gills: false,
        visor: false,
        engine: false,
        spots: false,
        halo_ring: false,
        feathers: false,
        crystals: false,
        circuit: false,
        back_spikes: false,
        fangs: false,
        drips: false,
        marking: 0,
        variant: 0,
    }
}

/// Which family the attribute forms. The element is what makes a Vaccine line
/// and a Virus line of the *same* archetype look like different creatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element {
    Plain,
    Blessed,
    Crypto,
    Viral,
}

impl Element {
    pub fn of(attribute: Attribute) -> Self {
        match attribute {
            Attribute::Vaccine => Element::Blessed,
            Attribute::Data => Element::Crypto,
            Attribute::Virus => Element::Viral,
        }
    }
}

/// The plan for one archetype, before its attribute and seed get a say.
pub fn plan(archetype: Archetype) -> Plan {
    match archetype {
        // The baseline: short, round, clawed, horned.
        Archetype::Beast => Plan {
            spots: true,
            ..base()
        },

        // Long snout, ribbed barrel, wings, thick tail.
        Archetype::Drake => Plan {
            head: HeadKind::Muzzle,
            ears: EarKind::None,
            crest: CrestKind::Spikes,
            arms: ArmKind::Hand,
            legs: LegKind::Biped,
            tail: TailKind::BlobFin,
            torso: TorsoKind::Barrel,
            wings: true,
            armor: true,
            torso_w: 1.14,
            torso_h: 1.06,
            head_w: 1.08,
            head_h: 0.94,
            neck: 8.0,
            ..base()
        },

        // Plated slab, visor, cannon arm, cable tail, treads.
        Archetype::Cyborg => Plan {
            head: HeadKind::Boxy,
            ears: EarKind::Antenna,
            crest: CrestKind::Blade,
            arms: ArmKind::Cannon,
            legs: LegKind::Treads,
            tail: TailKind::Cable,
            torso: TorsoKind::Slab,
            armor: true,
            torso_w: 1.2,
            torso_h: 0.94,
            head_w: 1.06,
            head_h: 0.82,
            arm_len: 1.06,
            leg_len: 0.72,
            ..base()
        },

        // Dome head, disc ears, halo, robe, feathered wings.
        Archetype::Holy => Plan {
            head: HeadKind::Dome,
            ears: EarKind::Disc,
            crest: CrestKind::None,
            arms: ArmKind::Hand,
            legs: LegKind::Biped,
            tail: TailKind::BlobFin,
            torso: TorsoKind::Robe,
            wings: true,
            torso_w: 0.94,
            torso_h: 1.08,
            head_w: 0.94,
            head_h: 1.12,
            arm_len: 0.96,
            neck: 10.0,
            ..base()
        },

        // Fish: wide low head, gill fans, dorsal fin, flippers, fin tail.
        Archetype::Aqua => Plan {
            head: HeadKind::Bulbous,
            ears: EarKind::Gill,
            crest: CrestKind::Fin,
            arms: ArmKind::Flipper,
            legs: LegKind::Finned,
            tail: TailKind::Fan,
            torso: TorsoKind::Round,
            torso_w: 1.1,
            torso_h: 0.92,
            head_w: 1.24,
            head_h: 0.84,
            arm_len: 0.74,
            leg_len: 0.6,
            dorsal_fin: true,
            gills: true,
            ..base()
        },

        // Deer: tall, slim, long neck, long ears, antlers, hooves, stub tail.
        Archetype::Cervid => Plan {
            head: HeadKind::Round,
            ears: EarKind::Long,
            crest: CrestKind::Antlers,
            arms: ArmKind::Hand,
            legs: LegKind::Hoofed,
            tail: TailKind::Stub,
            torso: TorsoKind::Round,
            torso_w: 0.84,
            torso_h: 1.06,
            head_w: 0.86,
            head_h: 0.9,
            arm_len: 1.06,
            leg_len: 1.34,
            neck: 20.0,
            spots: true,
            ..base()
        },

        // Robot: boxy, antenna, pistons, treads, engine, cable tail.
        Archetype::Machine => Plan {
            head: HeadKind::Boxy,
            ears: EarKind::Antenna,
            crest: CrestKind::Antenna,
            arms: ArmKind::Cannon,
            legs: LegKind::Treads,
            tail: TailKind::Cable,
            torso: TorsoKind::Slab,
            armor: true,
            torso_w: 1.18,
            torso_h: 1.0,
            head_w: 1.0,
            head_h: 0.86,
            arm_len: 1.1,
            leg_len: 0.66,
            visor: true,
            engine: true,
            ..base()
        },

        // Alien: huge bulbous head, stalk eyes, tentacles, floating mantle.
        Archetype::Alien => Plan {
            head: HeadKind::Bulbous,
            ears: EarKind::Stalk,
            crest: CrestKind::None,
            arms: ArmKind::Tentacle,
            legs: LegKind::Hover,
            tail: TailKind::Tentacle,
            torso: TorsoKind::Mantle,
            torso_w: 0.9,
            torso_h: 1.06,
            head_w: 1.34,
            head_h: 1.06,
            arm_len: 0.92,
            ..base()
        },
    }
}

impl Plan {
    /// What the attribute does to this body.
    ///
    /// The mutations are deliberately *structural* (a new crest, wings, claws,
    /// crystals) rather than a colour swap: the request was that Vaccine, Data
    /// and Virus digivolve into visibly different creatures, and a tint alone
    /// would not survive a five-second look at the desktop.
    fn with_element(mut self, element: Element) -> Self {
        match element {
            Element::Plain => {}
            Element::Blessed => {
                self.halo_ring = true;
                self.feathers = true;
                // Wings are the Vaccine signature; every blessed line flies.
                self.wings = true;
                // Spikes and antennae become a swept blade of light.
                self.crest = match self.crest {
                    CrestKind::Spikes | CrestKind::Antenna | CrestKind::None => CrestKind::Blade,
                    other => other,
                };
                self.head_h *= 1.05;
                self.torso_w *= 0.97;
            }
            Element::Crypto => {
                self.crystals = true;
                self.circuit = true;
                // Every Data line gets a sensor band across the face: it is the
                // one read that separates Crypto from Blessed at a glance, when
                // both wear wings and a ring of light.
                self.visor = true;
                // Nothing on a Data line is organic tusk; it hardens into a
                // blade or a sensor mast.
                self.crest = match self.crest {
                    CrestKind::Horns | CrestKind::Antlers | CrestKind::Spikes => CrestKind::Blade,
                    other => other,
                };
                self.head_w *= 1.03;
            }
            Element::Viral => {
                self.back_spikes = true;
                self.fangs = true;
                self.drips = true;
                // Fangs and spines instead of ornament; claws instead of hands.
                self.crest = match self.crest {
                    CrestKind::Blade | CrestKind::Antenna | CrestKind::None => CrestKind::Spikes,
                    other => other,
                };
                self.arms = match self.arms {
                    ArmKind::Hand => ArmKind::Claw,
                    other => other,
                };
                // A viral line is heavier in the chest and lower in the head.
                self.torso_w *= 1.04;
                self.head_h *= 0.96;
            }
        }
        self
    }

    /// The seeded build inside a plan: lean, canonical or broad.
    ///
    /// This is what makes two Rookies of the same archetype and attribute read
    /// as two individuals rather than one creature drawn twice.
    fn with_variant(mut self, variant: u8) -> Self {
        self.variant = variant % 3;
        match self.variant {
            1 => {
                // Lean: taller, narrower, longer legs.
                self.torso_w *= 0.93;
                self.torso_h *= 1.03;
                self.head_w *= 1.05;
                self.leg_len *= 1.1;
                self.arm_len *= 1.04;
                self.marking = 2; // stripes
            }
            2 => {
                // Broad: wider, squatter, heavier shoulders.
                self.torso_w *= 1.09;
                self.torso_h *= 0.96;
                self.head_h *= 0.95;
                self.leg_len *= 0.92;
                self.arm_len *= 1.06;
                self.marking = 1; // spots
            }
            _ => {
                self.marking = 0;
            }
        }
        self
    }
}

/// The whole plan: archetype, then attribute, then the seeded build.
pub fn plan_for(archetype: Archetype, attribute: Attribute, variant: u8) -> Plan {
    plan(archetype)
        .with_element(Element::of(attribute))
        .with_variant(variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_archetype_gets_a_plan_of_its_own_shape() {
        // A plan table where two archetypes share every knob is a palette
        // rotation wearing a plan's clothes.
        let mut shapes = Vec::new();
        for archetype in Archetype::ALL {
            let p = plan(archetype);
            let shape = format!(
                "{:?}/{:?}/{:?}/{:?}/{:?}/{:?}/{:?}/{}",
                p.head, p.ears, p.crest, p.arms, p.legs, p.tail, p.torso, p.wings,
            );
            assert!(
                !shapes.contains(&shape),
                "{} has the same body plan as another archetype",
                archetype.as_str()
            );
            shapes.push(shape);
        }
    }

    #[test]
    fn every_attribute_gives_each_archetype_a_different_body() {
        // The headline promise: for one archetype, Vaccine/Data/Virus are three
        // different creatures, not three tints of one.
        for archetype in Archetype::ALL {
            let mut seen = Vec::new();
            for attribute in Attribute::ALL {
                let p = plan_for(archetype, attribute, 0);
                let shape = format!(
                    "{:?}/{:?}/{:?}/{:?}/{:?}/{:?}/{:?}/{}/{}/{}/{}/{}/{}/{}",
                    p.head, p.ears, p.crest, p.arms, p.legs, p.tail, p.torso,
                    p.wings, p.halo_ring, p.crystals, p.back_spikes, p.feathers,
                    p.circuit, p.fangs,
                );
                assert!(
                    !seen.contains(&shape),
                    "{} under {} has the same body plan as another attribute",
                    archetype.as_str(),
                    attribute.as_str()
                );
                seen.push(shape);
            }
        }
    }

    #[test]
    fn the_three_builds_are_three_shapes() {
        for archetype in Archetype::ALL {
            let lean = plan_for(archetype, Attribute::Data, 1);
            let plain = plan_for(archetype, Attribute::Data, 0);
            let broad = plan_for(archetype, Attribute::Data, 2);
            assert!(lean.torso_w < plain.torso_w, "leaner trunk");
            assert!(broad.torso_w > plain.torso_w, "broader trunk");
            assert!(lean.leg_len > broad.leg_len, "leggier than broad");
            assert_ne!(lean.marking, broad.marking, "and marked differently");
            assert_eq!(plain.marking, 0, "the canonical build is unmarked");
        }
    }

    #[test]
    fn proportions_stay_in_a_sane_band() {
        for archetype in Archetype::ALL {
            for attribute in Attribute::ALL {
                for variant in 0..3u8 {
                    let p = plan_for(archetype, attribute, variant);
                    for (name, factor) in [
                ("torso_w", p.torso_w),
                ("torso_h", p.torso_h),
                ("head_w", p.head_w),
                ("head_h", p.head_h),
                ("arm_len", p.arm_len),
                ("leg_len", p.leg_len),
            ] {
                assert!(
                    (0.4..=1.7).contains(&factor),
                    "{} {} v{variant}: {name} = {factor} is outside the armature's safe band",
                    archetype.as_str(),
                    attribute.as_str()
                );
                    }
                }
            }
        }
    }
}
