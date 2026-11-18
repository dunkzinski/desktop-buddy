//! Body plans — what makes a Drake not just a recoloured Beast.
//!
//! The first version of the generator had one skeleton with four palettes, so
//! every archetype came out the same chubby biped in a different colour. A plan
//! is the missing layer: it decides *what shape* the creature is, while
//! [`crate::generator`] decides how much of it exists at each tier.
//!
//! Everything here is a pure constant table — no RNG, no clock — so
//! `(seed, archetype, tier)` still reproduces a creature byte for byte.

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
    }
}

/// The plan for one archetype.
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
            let shape = (
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
    fn proportions_stay_in_a_sane_band() {
        for archetype in Archetype::ALL {
            let p = plan(archetype);
            for (name, factor) in [
                ("torso_w", p.torso_w),
                ("torso_h", p.torso_h),
                ("head_w", p.head_w),
                ("head_h", p.head_h),
                ("arm_len", p.arm_len),
                ("leg_len", p.leg_len),
            ] {
                assert!(
                    (0.4..=1.6).contains(&factor),
                    "{}: {name} = {factor} is outside the armature's safe band",
                    archetype.as_str()
                );
            }
        }
    }
}
