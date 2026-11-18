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
    /// A long tapered beak over a small round skull — avian.
    Beak,
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
    /// A fan of long feathers over the brow.
    Comb,
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
    /// A feathered wing folded against the flank — avian.
    Wing,
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
    /// Two thin pairs of legs — insect.
    Insectoid,
    /// A heavy stone leg ending in a blunt foot — golem.
    Stonework,
    /// No legs, a coiled serpent base — serpent.
    Coiled,
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

/// The secondary silhouette furniture a **form** adds on top of its archetype's
/// body plan.
///
/// This is a drawing vocabulary, not a body plan: each archetype's form table
/// below picks from it for itself, and the generator hangs the geometry on the
/// bone that already owns that region (neck ruff on the trunk, sensor pair on
/// the skull, blade on the tail). Two creatures that differ only here differ in
/// outline — a maned Beast and a bare one are not the same silhouette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accessory {
    None,
    /// Layered frill around the neck and shoulders — manes, ruffs.
    Mane,
    /// A row of plates marching down the spine.
    Ridge,
    /// Angular spurs off each shoulder.
    Spurs,
    /// Swept fins from the hips.
    HipFins,
    /// A fan along the jaw line — gills, hoods, dewlaps.
    Frill,
    /// A second, shorter sensor pair beside the crest.
    Antennae,
    /// Blades along the tail.
    TailBlades,
    /// A ring around the throat.
    Collar,
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
    // ---- per-form knobs: the archetype's own silhouette variables ----
    /// Multiplier on the archetype's own ear/gill/antenna geometry.
    pub ear_scale: f64,
    /// Rotation (radians) added to the ear's bind angle: swept back or flared.
    pub ear_angle: f64,
    /// Multiplier on the crest's length and part sizes.
    pub crest_scale: f64,
    /// Rotation (radians) added to the crest's bind angle.
    pub crest_spread: f64,
    /// Multiplier on the tail's segment length (a whip vs a stub).
    pub tail_scale: f64,
    /// Rotation (radians) added to the tail root: how high it carries itself.
    pub tail_arc: f64,
    /// Rotation (radians) added to every shoulder/hip bind angle.
    pub arm_stance: f64,
    pub leg_stance: f64,
    /// The form's secondary silhouette furniture.
    pub accessory: Accessory,
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
    /// Seeded marking: 0 none, 1 spots, 2 stripes, 3 patches.
    pub marking: u8,
    /// The seeded build: 0 canonical, 1 lean, 2 broad (proportions + marking).
    pub build: u8,
    /// Which of this archetype's forms the seed drew (0..[`FORM_COUNT`]).
    pub form: u8,
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
        ear_scale: 1.0,
        ear_angle: 0.0,
        crest_scale: 1.0,
        crest_spread: 0.0,
        tail_scale: 1.0,
        tail_arc: 0.0,
        arm_stance: 0.0,
        leg_stance: 0.0,
        accessory: Accessory::None,
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
        build: 0,
        form: 0,
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

        // Bird: beaked, combed, folded wings, feathered tail.
        Archetype::Avian => Plan {
            head: HeadKind::Beak,
            ears: EarKind::None,
            crest: CrestKind::Comb,
            arms: ArmKind::Wing,
            legs: LegKind::Hoofed,
            tail: TailKind::Fan,
            torso: TorsoKind::Round,
            torso_w: 0.9,
            torso_h: 1.0,
            head_w: 0.86,
            head_h: 0.86,
            arm_len: 1.1,
            leg_len: 1.12,
            neck: 14.0,
            feathers: true,
            ..base()
        },

        // Insect: domed shell, antennae, thin limbs, stub abdomen.
        Archetype::Insect => Plan {
            head: HeadKind::Dome,
            ears: EarKind::Antenna,
            crest: CrestKind::Antenna,
            arms: ArmKind::Claw,
            legs: LegKind::Insectoid,
            tail: TailKind::Stub,
            torso: TorsoKind::Barrel,
            torso_w: 0.86,
            torso_h: 1.02,
            head_w: 0.94,
            head_h: 0.92,
            arm_len: 0.86,
            leg_len: 0.72,
            ..base()
        },

        // Serpent: long jaw, no limbs, a coiled base, a very long tail.
        Archetype::Serpent => Plan {
            head: HeadKind::Muzzle,
            ears: EarKind::None,
            crest: CrestKind::Fin,
            arms: ArmKind::Flipper,
            legs: LegKind::Coiled,
            tail: TailKind::Tentacle,
            torso: TorsoKind::Round,
            torso_w: 0.84,
            torso_h: 1.16,
            head_w: 1.04,
            head_h: 0.84,
            arm_len: 0.7,
            ..base()
        },

        // Golem: carved stone, blunt fists, heavy stance.
        Archetype::Golem => Plan {
            head: HeadKind::Boxy,
            ears: EarKind::None,
            crest: CrestKind::Spikes,
            arms: ArmKind::Hand,
            legs: LegKind::Stonework,
            tail: TailKind::Stub,
            torso: TorsoKind::Slab,
            armor: true,
            torso_w: 1.22,
            torso_h: 1.04,
            head_w: 1.08,
            head_h: 0.9,
            arm_len: 1.14,
            leg_len: 0.86,
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

/* ------------------------------------------------------------------ *
 * Forms — each archetype's own structural variations
 * ------------------------------------------------------------------ */

/// How many seeded variants `Spec::variant` can draw. `BUILD_COUNT` builds ×
/// `FORM_COUNT` forms.
pub const VARIANT_COUNT: u8 = BUILD_COUNT * FORM_COUNT;
/// Proportions and markings: canonical, lean, broad.
pub const BUILD_COUNT: u8 = 3;
/// Structural forms inside one archetype. Four is enough that a Beast, a Drake
/// and a Serpent each get a recognisable family, and small enough that every
/// form stays hand-tuned instead of generated into mush.
pub const FORM_COUNT: u8 = 4;

/// The form table for one archetype.
///
/// Every archetype keeps its own function here on purpose: a shared "quadruped
/// template" with per-archetype numbers is how twelve body plans quietly become
/// one body plan with twelve palettes. These functions only ever touch the plan
/// they were handed, so no archetype's mutations can reach another's.
fn form_of(archetype: Archetype, p: Plan, form: u8) -> Plan {
    let form = form % FORM_COUNT;
    match archetype {
        Archetype::Beast => beast_form(p, form),
        Archetype::Drake => drake_form(p, form),
        Archetype::Cyborg => cyborg_form(p, form),
        Archetype::Holy => holy_form(p, form),
        Archetype::Aqua => aqua_form(p, form),
        Archetype::Cervid => cervid_form(p, form),
        Archetype::Machine => machine_form(p, form),
        Archetype::Alien => alien_form(p, form),
        Archetype::Avian => avian_form(p, form),
        Archetype::Insect => insect_form(p, form),
        Archetype::Serpent => serpent_form(p, form),
        Archetype::Golem => golem_form(p, form),
    }
}

// Beast: the baseline round-clawed biped, in four builds of the same animal.
fn beast_form(p: Plan, form: u8) -> Plan {
    match form {
        // 0 — the classic: round skull, short pointed ears, paired horns.
        0 => p,
        // 1 — lupine: swept leaf ears, a neck ruff, longer legs, higher tail.
        1 => Plan {
            ears: EarKind::Long,
            ear_scale: 1.12,
            ear_angle: -0.14,
            crest_scale: 0.78,
            crest_spread: 0.1,
            head_h: p.head_h * 1.04,
            torso_w: p.torso_w * 0.94,
            leg_len: p.leg_len * 1.08,
            tail_scale: 1.15,
            tail_arc: 0.08,
            accessory: Accessory::Mane,
            ..p
        },
        // 2 — ursine: heavy trunk, short legs, tiny ears, buzz-cut horns.
        2 => Plan {
            ear_scale: 0.7,
            crest_scale: 0.68,
            head_w: p.head_w * 1.1,
            head_h: p.head_h * 0.94,
            torso_w: p.torso_w * 1.12,
            arm_len: p.arm_len * 1.08,
            leg_len: p.leg_len * 0.88,
            leg_stance: 0.05,
            tail_scale: 0.7,
            ..p
        },
        // 3 — sabre: narrow skull, a fan of spikes, a whip tail, shoulder spurs.
        _ => Plan {
            crest: CrestKind::Spikes,
            crest_scale: 1.28,
            crest_spread: -0.1,
            head_w: p.head_w * 0.92,
            head_h: p.head_h * 0.9,
            torso_w: p.torso_w * 0.92,
            leg_len: p.leg_len * 1.02,
            tail_scale: 1.3,
            tail_arc: 0.16,
            arm_stance: -0.08,
            accessory: Accessory::Spurs,
            ..p
        },
    }
}

// Drake: long snout, ribbed barrel, wings — in four builds of the same dragon.
fn drake_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — wyrm: a longer neck, paired horns and a dorsal ridge down the back.
        1 => Plan {
            crest: CrestKind::Horns,
            crest_scale: 1.12,
            crest_spread: 0.12,
            neck: 16.0,
            torso_w: p.torso_w * 0.9,
            torso_h: p.torso_h * 1.05,
            head_w: p.head_w * 0.94,
            tail_scale: 1.2,
            tail_arc: 0.1,
            accessory: Accessory::Ridge,
            ..p
        },
        // 2 — wyvern: broad chest, heavy jaw, wide spike crest, hip fins.
        2 => Plan {
            crest_scale: 1.35,
            crest_spread: -0.14,
            neck: 4.0,
            torso_w: p.torso_w * 1.12,
            head_h: p.head_h * 1.08,
            arm_len: p.arm_len * 1.12,
            leg_len: p.leg_len * 0.9,
            leg_stance: 0.06,
            tail_scale: 0.85,
            accessory: Accessory::HipFins,
            ..p
        },
        // 3 — serpentine: a low narrow trunk, a long bladed tail, a long neck.
        _ => Plan {
            crest_scale: 0.8,
            neck: 20.0,
            torso_w: p.torso_w * 0.86,
            torso_h: p.torso_h * 0.95,
            tail_scale: 1.3,
            tail_arc: 0.2,
            accessory: Accessory::TailBlades,
            ..p
        },
    }
}

// Cyborg: plated slab, visor, cannon, treads — four machine-human hybrids.
fn cyborg_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — sensor unit: a tall mast, wide receivers, a second antenna pair.
        1 => Plan {
            crest: CrestKind::Antenna,
            crest_scale: 1.2,
            crest_spread: -0.12,
            ear_scale: 1.25,
            head_h: p.head_h * 0.9,
            torso_w: p.torso_w * 0.94,
            arm_stance: 0.1,
            accessory: Accessory::Antennae,
            ..p
        },
        // 2 — siege: wider shoulders, a longer blade, spurs, a squatter stance.
        2 => Plan {
            crest_scale: 1.25,
            torso_w: p.torso_w * 1.14,
            torso_h: p.torso_h * 1.02,
            arm_len: p.arm_len * 1.1,
            leg_len: p.leg_len * 0.9,
            leg_stance: 0.08,
            accessory: Accessory::Spurs,
            ..p
        },
        // 3 — scout: leggier, narrower, a short blade and small receivers.
        _ => Plan {
            crest_scale: 0.8,
            ear_scale: 0.8,
            head_w: p.head_w * 0.94,
            torso_w: p.torso_w * 0.9,
            torso_h: p.torso_h * 1.03,
            leg_len: p.leg_len * 1.12,
            accessory: Accessory::Collar,
            ..p
        },
    }
}

// Holy: dome head, disc ears, robe, wings — four orders of the same angel.
fn holy_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — seraph: a tall blade of light, a jaw frill, a higher brow.
        1 => Plan {
            crest: CrestKind::Blade,
            crest_scale: 1.3,
            crest_spread: -0.08,
            head_h: p.head_h * 1.06,
            torso_w: p.torso_w * 0.92,
            arm_stance: 0.08,
            accessory: Accessory::Frill,
            ..p
        },
        // 2 — cherub: a rounder, wider body on shorter legs, a throat ring.
        2 => Plan {
            torso_w: p.torso_w * 1.1,
            torso_h: p.torso_h * 0.95,
            head_w: p.head_w * 1.1,
            head_h: p.head_h * 1.02,
            arm_len: p.arm_len * 0.9,
            leg_len: p.leg_len * 0.92,
            ear_scale: 1.2,
            accessory: Accessory::Collar,
            ..p
        },
        // 3 — oracle: a tall narrow skull on a long neck, an upright stance.
        _ => Plan {
            crest: CrestKind::Blade,
            crest_scale: 0.9,
            neck: 16.0,
            head_w: p.head_w * 0.92,
            head_h: p.head_h * 1.12,
            torso_w: p.torso_w * 0.88,
            torso_h: p.torso_h * 1.1,
            leg_len: p.leg_len * 1.06,
            ..p
        },
    }
}

// Aqua: wide low skull, gill fans, flippers, fin tail — four fish shapes.
fn aqua_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — eel: a long body, a huge fan tail, hip fins, a taller head fin.
        1 => Plan {
            crest_scale: 1.2,
            head_w: p.head_w * 1.1,
            head_h: p.head_h * 0.8,
            torso_w: p.torso_w * 0.86,
            torso_h: p.torso_h * 1.12,
            leg_len: p.leg_len * 0.8,
            tail_scale: 1.35,
            tail_arc: 0.12,
            accessory: Accessory::HipFins,
            ..p
        },
        // 2 — ray: a wide flat body, oversized head, pectoral spurs.
        2 => Plan {
            crest_scale: 0.85,
            head_w: p.head_w * 1.18,
            torso_w: p.torso_w * 1.16,
            torso_h: p.torso_h * 0.9,
            arm_len: p.arm_len * 0.85,
            arm_stance: 0.14,
            leg_stance: 0.1,
            tail_scale: 0.9,
            accessory: Accessory::Spurs,
            ..p
        },
        // 3 — pike: a narrow head and body, a jaw frill, sharper fins.
        _ => Plan {
            crest_scale: 0.8,
            head_w: p.head_w * 0.96,
            head_h: p.head_h * 0.9,
            torso_w: p.torso_w * 0.92,
            leg_len: p.leg_len * 0.9,
            arm_len: p.arm_len * 0.8,
            tail_scale: 0.95,
            accessory: Accessory::Frill,
            ..p
        },
    }
}

// Cervid: tall, slim, long neck, antlers, hooves — four deer builds.
fn cervid_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — elk: a broader trunk, a longer neck, a neck ruff, big antlers.
        1 => Plan {
            crest_scale: 1.3,
            crest_spread: 0.1,
            neck: 24.0,
            torso_w: p.torso_w * 1.06,
            head_w: p.head_w * 1.06,
            leg_len: p.leg_len * 0.94,
            ear_scale: 1.1,
            accessory: Accessory::Mane,
            ..p
        },
        // 2 — gazelle: slimmer and taller, with smaller ears and antlers.
        2 => Plan {
            crest_scale: 0.8,
            torso_w: p.torso_w * 0.88,
            leg_len: p.leg_len * 1.12,
            ear_scale: 1.15,
            ear_angle: -0.2,
            head_w: p.head_w * 0.9,
            tail_arc: 0.12,
            ..p
        },
        // 3 — moose: a heavy head with a dewlap, a stockier frame.
        _ => Plan {
            crest_scale: 1.2,
            torso_w: p.torso_w * 1.12,
            torso_h: p.torso_h * 1.04,
            arm_len: p.arm_len * 1.1,
            leg_len: p.leg_len * 0.9,
            head_w: p.head_w * 1.12,
            head_h: p.head_h * 1.06,
            ear_scale: 0.85,
            accessory: Accessory::Frill,
            ..p
        },
    }
}

// Machine: boxy, treads, pistons, engine vents — four robots.
fn machine_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — turret: a squat wide chassis with heavy arms and a long mast.
        1 => Plan {
            crest_scale: 1.25,
            torso_w: p.torso_w * 1.1,
            torso_h: p.torso_h * 0.9,
            arm_len: p.arm_len * 1.15,
            leg_len: p.leg_len * 0.85,
            leg_stance: 0.1,
            accessory: Accessory::Spurs,
            ..p
        },
        // 2 — tracker: a taller chassis, wide receivers, a second sensor pair.
        2 => Plan {
            crest_scale: 0.8,
            ear_scale: 1.3,
            torso_w: p.torso_w * 0.94,
            torso_h: p.torso_h * 1.06,
            arm_stance: 0.12,
            accessory: Accessory::Antennae,
            ..p
        },
        // 3 — crawler: narrow and long, short arms, a sensor collar.
        _ => Plan {
            torso_w: p.torso_w * 0.9,
            torso_h: p.torso_h * 1.08,
            arm_len: p.arm_len * 0.85,
            head_w: p.head_w * 0.94,
            accessory: Accessory::Collar,
            ..p
        },
    }
}

// Alien: bulbous head, stalk eyes, tentacles, floating mantle — four visitors.
fn alien_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — grey: a huge cranium on a slim body, longer tendrils, a jaw frill.
        1 => Plan {
            head_w: p.head_w * 1.15,
            head_h: p.head_h * 1.14,
            torso_w: p.torso_w * 0.86,
            arm_len: p.arm_len * 1.12,
            ear_scale: 1.25,
            tail_scale: 0.85,
            accessory: Accessory::Frill,
            ..p
        },
        // 2 — hulk: a broad body under a spined crown, short thick tendrils.
        2 => Plan {
            crest: CrestKind::Spikes,
            crest_scale: 1.2,
            crest_spread: 0.14,
            head_w: p.head_w * 1.1,
            head_h: p.head_h * 0.94,
            torso_w: p.torso_w * 1.14,
            torso_h: p.torso_h * 1.02,
            arm_len: p.arm_len * 0.85,
            ear_scale: 0.85,
            accessory: Accessory::Ridge,
            ..p
        },
        // 3 — watcher: an even wider skull, longer eye stalks, swept hip fins.
        _ => Plan {
            head_w: p.head_w * 1.15,
            torso_w: p.torso_w * 0.92,
            torso_h: p.torso_h * 1.1,
            ear_scale: 1.4,
            ear_angle: -0.1,
            tail_scale: 1.2,
            leg_stance: 0.04,
            accessory: Accessory::HipFins,
            ..p
        },
    }
}

// Avian: beaked, combed, folded wings — four birds.
fn avian_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — raptor: a lean frame on long legs, a tall comb, a jaw frill.
        1 => Plan {
            crest_scale: 1.2,
            crest_spread: -0.1,
            head_w: p.head_w * 0.94,
            head_h: p.head_h * 0.94,
            torso_w: p.torso_w * 0.9,
            leg_len: p.leg_len * 1.1,
            arm_len: p.arm_len * 1.05,
            accessory: Accessory::Frill,
            ..p
        },
        // 2 — owl: a wide round skull with ear tufts, a short neck, a small comb.
        2 => Plan {
            ears: EarKind::Pointed,
            ear_scale: 0.9,
            ear_angle: 0.16,
            crest_scale: 0.7,
            neck: 8.0,
            head_w: p.head_w * 1.16,
            head_h: p.head_h * 1.1,
            torso_w: p.torso_w * 1.06,
            leg_len: p.leg_len * 0.94,
            ..p
        },
        // 3 — crane: a long neck and legs, a big comb, long tail plumes.
        _ => Plan {
            crest_scale: 1.35,
            neck: 24.0,
            torso_w: p.torso_w * 0.88,
            torso_h: p.torso_h * 1.04,
            leg_len: p.leg_len * 1.16,
            tail_scale: 1.15,
            tail_arc: 0.1,
            accessory: Accessory::TailBlades,
            ..p
        },
    }
}

// Insect: domed shell, antennae, thin limbs, stub abdomen — four bugs.
fn insect_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — beetle: a wide domed shell, a short crest, a dorsal ridge.
        1 => Plan {
            crest_scale: 0.8,
            head_w: p.head_w * 1.06,
            torso_w: p.torso_w * 1.1,
            torso_h: p.torso_h * 0.96,
            leg_len: p.leg_len * 0.88,
            arm_len: p.arm_len * 1.08,
            leg_stance: 0.08,
            accessory: Accessory::Ridge,
            ..p
        },
        // 2 — mantis: long raptor arms, a tall frame, shoulder spurs.
        2 => Plan {
            crest_scale: 1.25,
            torso_w: p.torso_w * 0.88,
            torso_h: p.torso_h * 1.08,
            arm_len: p.arm_len * 1.25,
            leg_len: p.leg_len * 1.08,
            ear_scale: 1.2,
            arm_stance: -0.12,
            accessory: Accessory::Spurs,
            ..p
        },
        // 3 — wasp: a narrow thorax and a long bladed abdomen.
        _ => Plan {
            crest_scale: 0.9,
            ear_scale: 0.8,
            torso_w: p.torso_w * 0.94,
            head_w: p.head_w * 0.92,
            leg_len: p.leg_len * 0.95,
            tail_scale: 1.3,
            tail_arc: 0.14,
            accessory: Accessory::TailBlades,
            ..p
        },
    }
}

// Serpent: long jaw, no legs, a coiled base, a long tail — four snakes.
fn serpent_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — cobra: a wide skull, a tall head fin, and a hood frill.
        1 => Plan {
            crest_scale: 1.35,
            crest_spread: 0.1,
            head_w: p.head_w * 1.12,
            head_h: p.head_h * 0.9,
            torso_w: p.torso_w * 0.92,
            tail_scale: 1.15,
            accessory: Accessory::Frill,
            ..p
        },
        // 2 — python: a heavy body with a low head and a dorsal ridge.
        2 => Plan {
            crest_scale: 0.8,
            head_w: p.head_w * 1.06,
            torso_w: p.torso_w * 1.1,
            torso_h: p.torso_h * 1.08,
            arm_stance: 0.1,
            tail_scale: 1.25,
            accessory: Accessory::Ridge,
            ..p
        },
        // 3 — viper: a narrow raised head over a coiled, bladed tail.
        _ => Plan {
            crest_scale: 1.1,
            head_w: p.head_w * 0.96,
            head_h: p.head_h * 0.94,
            torso_w: p.torso_w * 0.86,
            torso_h: p.torso_h * 1.2,
            tail_scale: 0.95,
            tail_arc: 0.18,
            accessory: Accessory::TailBlades,
            ..p
        },
    }
}

// Golem: carved stone, blunt fists, heavy stance — four statues.
fn golem_form(p: Plan, form: u8) -> Plan {
    match form {
        0 => p,
        // 1 — colossus: the widest shoulders, a tall spike crown, shoulder spurs.
        1 => Plan {
            crest_scale: 1.3,
            crest_spread: 0.12,
            torso_w: p.torso_w * 1.16,
            torso_h: p.torso_h * 1.08,
            arm_len: p.arm_len * 1.1,
            leg_len: p.leg_len * 0.92,
            head_w: p.head_w * 1.06,
            leg_stance: 0.1,
            accessory: Accessory::Spurs,
            ..p
        },
        // 2 — sentry: an upright column of stone with a short crown and a collar.
        2 => Plan {
            crest_scale: 0.7,
            torso_w: p.torso_w * 0.9,
            torso_h: p.torso_h * 1.06,
            leg_len: p.leg_len * 1.1,
            head_w: p.head_w * 0.92,
            accessory: Accessory::Collar,
            ..p
        },
        // 3 — boulder: a squat boulder of a body, huge arms, a back ridge.
        _ => Plan {
            crest_scale: 1.0,
            torso_w: p.torso_w * 1.2,
            torso_h: p.torso_h * 0.94,
            arm_len: p.arm_len * 1.2,
            leg_len: p.leg_len * 0.85,
            head_w: p.head_w * 1.12,
            head_h: p.head_h * 0.9,
            arm_stance: -0.1,
            accessory: Accessory::Ridge,
            ..p
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
    /// as two individuals rather than one creature drawn twice. The *form* (see
    /// the per-archetype tables above) moves the silhouette; the build moves the
    /// proportions and the marking.
    fn with_build(mut self, build: u8) -> Self {
        self.build = build % BUILD_COUNT;
        match self.build {
            1 => {
                // Lean: taller, narrower, longer legs.
                self.torso_w *= 0.93;
                self.torso_h *= 1.03;
                self.head_w *= 1.05;
                self.leg_len *= 1.1;
                self.arm_len *= 1.04;
            }
            2 => {
                // Broad: wider, squatter, heavier shoulders.
                self.torso_w *= 1.09;
                self.torso_h *= 0.96;
                self.head_h *= 0.95;
                self.leg_len *= 0.92;
                self.arm_len *= 1.06;
            }
            _ => {}
        }
        self
    }

    /// The whole seeded variant: form, build and marking out of one number.
    fn with_variant(mut self, variant: u8) -> Self {
        let variant = variant % VARIANT_COUNT;
        self = self.with_build(variant % BUILD_COUNT);
        // Four markings (none / spots / stripes / patches) with both the build
        // and the form moving the count, so the marking is not just a second
        // name for the build.
        self.marking = (self.build + self.form) % 4;
        self
    }
}

/// The whole plan: archetype, then its form, then attribute, then seeded build.
pub fn plan_for(archetype: Archetype, attribute: Attribute, variant: u8) -> Plan {
    let variant = variant % VARIANT_COUNT;
    let mut plan = plan(archetype);
    plan.form = (variant / BUILD_COUNT) % FORM_COUNT;
    plan = form_of(archetype, plan, plan.form);
    plan.with_element(Element::of(attribute))
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

    /// Everything a viewer reads as "outline": the structural choices, the
    /// proportions, the posture and the accessory — but not the palette.
    fn silhouette(p: &Plan) -> String {
        format!(
            "{:?}/{:?}/{:?}/{:?}/{:?}/{:?}/{:?}/w{}/a{:?}/\
             {:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}/{:.2},{:.2},{:.2},{:.2}",
            p.head,
            p.ears,
            p.crest,
            p.arms,
            p.legs,
            p.tail,
            p.torso,
            p.wings as u8,
            p.accessory,
            p.torso_w,
            p.torso_h,
            p.head_w,
            p.head_h,
            p.arm_len,
            p.leg_len,
            p.ear_scale,
            p.crest_scale,
            p.tail_scale,
            p.ear_angle,
            p.crest_spread,
            p.tail_arc,
            p.leg_stance,
        )
    }

    #[test]
    fn every_archetype_has_four_distinct_forms() {
        // The headline of this layer: the same archetype, same attribute, same
        // build, four different creatures — before a colour is chosen.
        for archetype in Archetype::ALL {
            let mut seen = Vec::new();
            for form in 0..FORM_COUNT {
                // `form` lives in the high half of the variant number.
                let p = plan_for(archetype, Attribute::default(), form * BUILD_COUNT);
                assert_eq!(p.form, form);
                let shape = silhouette(&p);
                assert!(
                    !seen.contains(&shape),
                    "{}'s form {form} is another of its own forms",
                    archetype.as_str()
                );
                seen.push(shape);
            }
        }
    }

    #[test]
    fn archetypes_stay_distinct_in_every_form() {
        // Forms must not blur the body plans into each other: at any form index
        // the twelve archetypes are still twelve different creatures.
        for form in 0..FORM_COUNT {
            let mut seen: Vec<(String, &'static str)> = Vec::new();
            for archetype in Archetype::ALL {
                let p = plan_for(archetype, Attribute::default(), form * BUILD_COUNT);
                let shape = silhouette(&p);
                if let Some((_, other)) = seen.iter().find(|(known, _)| known == &shape) {
                    panic!(
                        "{}'s form {form} is the same creature as {other}'s",
                        archetype.as_str()
                    );
                }
                seen.push((shape, archetype.as_str()));
            }
        }
    }

    #[test]
    fn the_variant_covers_every_marking() {
        for archetype in Archetype::ALL {
            let markings: std::collections::BTreeSet<u8> = (0..VARIANT_COUNT)
                .map(|variant| plan_for(archetype, Attribute::default(), variant).marking)
                .collect();
            assert_eq!(
                markings.len(),
                4,
                "{} only reaches {} of the four markings",
                archetype.as_str(),
                markings.len()
            );
        }
    }

    #[test]
    fn proportions_stay_in_a_sane_band() {
        for archetype in Archetype::ALL {
            for attribute in Attribute::ALL {
                for variant in 0..VARIANT_COUNT {
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
                            "{} {} v{variant}: {name} = {factor} \
                             is outside the armature's safe band",
                            archetype.as_str(),
                            attribute.as_str()
                        );
                    }
                    // The form knobs are multipliers and offsets on the same
                    // armature, so they need bands of their own: a form may not
                    // fold a ear inside the skull or swing a limb through the
                    // body.
                    for (name, factor, lo, hi) in [
                        ("ear_scale", p.ear_scale, 0.6, 1.5),
                        ("crest_scale", p.crest_scale, 0.6, 1.5),
                        ("tail_scale", p.tail_scale, 0.6, 1.5),
                        ("ear_angle", p.ear_angle, -0.25, 0.2),
                        ("crest_spread", p.crest_spread, -0.2, 0.2),
                        ("tail_arc", p.tail_arc, -0.25, 0.25),
                        ("arm_stance", p.arm_stance, -0.15, 0.15),
                        ("leg_stance", p.leg_stance, -0.15, 0.15),
                    ] {
                        assert!(
                            (lo..=hi).contains(&factor),
                            "{} {} v{variant}: {name} = {factor} is outside {lo}..{hi}",
                            archetype.as_str(),
                            attribute.as_str()
                        );
                    }
                }
            }
        }
    }
}
