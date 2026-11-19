//! The procedural rig generator.
//!
//! One pure function — [`generate`] — turns `(seed, archetype, tier)` into a
//! complete `char-rig/model@1` manifest. No clock, no filesystem, no host call:
//! the same spec always yields the same bytes, which is what makes the creatures
//! reproducible and the tests meaningful.
//!
//! The authoring order is the one `MODEL_FORMAT.md` asks for:
//!
//! 1. centerline first (`root`, `pelvis`, `torso`, `neck`, `head`, `tail`);
//! 2. the **right** side chains (`*_r`) only, one after another;
//! 3. every right chain is mirrored with [`mirror_chain`] — chain roots with
//!    `atan2(sin θ, −cos θ)`, descendants with `−θ`.
//!
//! Then the shape rules are enforced structurally (pivots live in the `Part`
//! constructors), the monitor budget is verified, and the keyframe clips are
//! generated from the finished skeleton so their mirrored deltas can never name
//! a bone that does not exist.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::attribute::{Attribute, AttributeBlend};
use crate::plan::{
    self, Accessory, ArmKind, CrestKind, EarKind, HeadKind, LegKind, Plan, TailKind, TorsoKind,
};
use crate::rng::{mix_seed, Rng};
use crate::skeleton::{
    hierarchy_is_sound, left_id, min_world_y, mirror_chain, round4, Anim, Bone, BoneDelta, Clip,
    Keyframe, Part, MAX_PARTS, MONITOR_BUDGET_Y, ROOT_SCALE,
};

/// Re-exported so a caller (and the integration tests) can name the type the
/// generator returns without reaching into `skeleton`.
pub use crate::skeleton::Manifest;

/* ------------------------------------------------------------------ *
 * Spec
 * ------------------------------------------------------------------ */

/// The four families the generator knows how to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Archetype {
    Beast,
    Drake,
    Cyborg,
    Holy,
    /// Fish: wide low head, gills, flippers, fin tail.
    Aqua,
    /// Deer: tall and slim, antlers, hooves, stub tail.
    Cervid,
    /// Robot: boxy, treads, pistons, engine vents.
    Machine,
    /// Alien: huge bulbous head, tentacles, floating mantle.
    Alien,
    /// Bird: beaked, combed, folded wings.
    Avian,
    /// Insect: domed shell, antennae, thin limbs.
    Insect,
    /// Serpent: long jaw, no legs, a coiled base.
    Serpent,
    /// Golem: carved stone, blunt fists, heavy stance.
    Golem,
}

impl Archetype {
    pub const ALL: [Archetype; 12] = [
        Archetype::Beast,
        Archetype::Drake,
        Archetype::Cyborg,
        Archetype::Holy,
        Archetype::Aqua,
        Archetype::Cervid,
        Archetype::Machine,
        Archetype::Alien,
        Archetype::Avian,
        Archetype::Insect,
        Archetype::Serpent,
        Archetype::Golem,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Archetype::Beast => "beast",
            Archetype::Drake => "drake",
            Archetype::Cyborg => "cyborg",
            Archetype::Holy => "holy",
            Archetype::Aqua => "aqua",
            Archetype::Cervid => "cervid",
            Archetype::Machine => "machine",
            Archetype::Alien => "alien",
            Archetype::Avian => "avian",
            Archetype::Insect => "insect",
            Archetype::Serpent => "serpent",
            Archetype::Golem => "golem",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "beast" => Some(Archetype::Beast),
            "drake" | "dragon" => Some(Archetype::Drake),
            "cyborg" | "android" => Some(Archetype::Cyborg),
            "holy" | "angel" => Some(Archetype::Holy),
            "aqua" | "fish" => Some(Archetype::Aqua),
            "cervid" | "deer" => Some(Archetype::Cervid),
            "machine" | "robot" => Some(Archetype::Machine),
            "alien" => Some(Archetype::Alien),
            "avian" | "bird" => Some(Archetype::Avian),
            "insect" | "bug" => Some(Archetype::Insect),
            "serpent" | "snake" => Some(Archetype::Serpent),
            "golem" | "rock" => Some(Archetype::Golem),
            _ => None,
        }
    }
}

/// The six growth stages, in order.
///
/// Serialised through [`Tier::as_str`] (`"In-Training"`), the same spelling the
/// API reports, and parsed leniently (`in_training`, `InTraining` and
/// `in-training` all land on the same stage) — one spelling on the wire, many
/// accepted on the way in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Fresh,
    InTraining,
    Rookie,
    Champion,
    Ultimate,
    Mega,
}

impl Tier {
    pub const ALL: [Tier; 6] = [
        Tier::Fresh,
        Tier::InTraining,
        Tier::Rookie,
        Tier::Champion,
        Tier::Ultimate,
        Tier::Mega,
    ];

    pub fn index(self) -> usize {
        match self {
            Tier::Fresh => 0,
            Tier::InTraining => 1,
            Tier::Rookie => 2,
            Tier::Champion => 3,
            Tier::Ultimate => 4,
            Tier::Mega => 5,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Fresh => "Fresh",
            Tier::InTraining => "In-Training",
            Tier::Rookie => "Rookie",
            Tier::Champion => "Champion",
            Tier::Ultimate => "Ultimate",
            Tier::Mega => "Mega",
        }
    }

    /// Lowercase, filename-safe spelling (`in-training`).
    pub fn slug(self) -> &'static str {
        match self {
            Tier::Fresh => "fresh",
            Tier::InTraining => "in-training",
            Tier::Rookie => "rookie",
            Tier::Champion => "champion",
            Tier::Ultimate => "ultimate",
            Tier::Mega => "mega",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        match normalized.as_str() {
            "fresh" => Some(Tier::Fresh),
            "in-training" | "intraining" | "training" => Some(Tier::InTraining),
            "rookie" => Some(Tier::Rookie),
            "champion" => Some(Tier::Champion),
            "ultimate" => Some(Tier::Ultimate),
            "mega" => Some(Tier::Mega),
            _ => None,
        }
    }

    pub fn next(self) -> Option<Tier> {
        match self {
            Tier::Fresh => Some(Tier::InTraining),
            Tier::InTraining => Some(Tier::Rookie),
            Tier::Rookie => Some(Tier::Champion),
            Tier::Champion => Some(Tier::Ultimate),
            Tier::Ultimate => Some(Tier::Mega),
            Tier::Mega => None,
        }
    }
}

impl Serialize for Tier {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Tier {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Tier::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown stage {raw:?}")))
    }
}

/// Everything a creature's shape is derived from.
///
/// The attribute belongs here, not on the branch: a Vaccine Rookie and a Virus
/// Rookie are different *rigs*, so the attribute has to reach the generator —
/// and so does the lineage `blend`, because a stage is shaped by every attribute
/// its line has been, not just the one it is now.
/// The variant is the seeded build (lean / canonical / broad) and is derived
/// from the seed so it costs the caller nothing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub seed: u64,
    pub archetype: Archetype,
    pub tier: Tier,
    #[serde(default)]
    pub attribute: Attribute,
    /// The recency-weighted lineage mix, when the creature has a history.
    ///
    /// `None` means "exactly `attribute`" — the pre-blend spelling — so a
    /// hatchling, a legacy save and every existing caller generate through the
    /// identical code path with a one-hot mix. (This one field is why `Spec` is
    /// `PartialEq` and not `Eq`: a blend is three floats.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<AttributeBlend>,
}

impl Spec {
    pub fn new(seed: u64, archetype: Archetype, tier: Tier) -> Self {
        Self::with_attribute(seed, archetype, tier, Attribute::default())
    }

    pub fn with_attribute(
        seed: u64,
        archetype: Archetype,
        tier: Tier,
        attribute: Attribute,
    ) -> Self {
        Self {
            seed,
            archetype,
            tier,
            attribute,
            blend: None,
        }
    }

    /// A stage of a creature whose evolution line mixed attributes.
    ///
    /// `attribute` stays the creature's *current* attribute (it names the
    /// folder), while `blend` carries the history that shapes the body.
    pub fn with_blend(
        seed: u64,
        archetype: Archetype,
        tier: Tier,
        attribute: Attribute,
        blend: AttributeBlend,
    ) -> Self {
        Self {
            seed,
            archetype,
            tier,
            attribute,
            blend: Some(blend),
        }
    }

    /// The mix this rig is generated from; falls back to the single attribute,
    /// which is what `generate` did before the blend existed.
    pub fn blend(&self) -> AttributeBlend {
        self.blend
            .unwrap_or_else(|| AttributeBlend::pure(self.attribute))
    }

    /// The seeded variant: one of `plan::VARIANT_COUNT` numbers, packing an
    /// archetype-specific **form** (a structural silhouette) and a **build**
    /// (proportions + marking).
    pub fn variant(&self) -> u8 {
        // Mix, then draw from an actual SplitMix64 stream: seeds 0, 1, 2 all
        // landing on one body is the bug this is here to fix, and `% 12` makes
        // it easier to hit than `% 3` did. One more avalanche step (rather than
        // a bare modulo of the mixed word) keeps small seeds — the ones people
        // type — spread across the forms instead of clustering on one.
        let mut rng = Rng::new(mix_seed(self.seed, &[0x5EED]));
        rng.below(u32::from(plan::VARIANT_COUNT)) as u8
    }

    /// `digi-drake-champion-virus-1f3a9c02` — stable, lowercase, filesystem-safe.
    ///
    /// The attribute is part of the name because it is part of the rig: without
    /// it, a Vaccine and a Virus creature of the same seed/archetype/stage are
    /// two different monsters fighting over one `models/<slug>/` folder.
    pub fn slug(&self) -> String {
        format!(
            "digi-{}-{}-{}-{:08x}",
            self.archetype.as_str(),
            self.tier.slug(),
            self.attribute.slug(),
            (self.seed & 0xFFFF_FFFF) as u32
        )
    }

    pub fn label(&self) -> String {
        format!(
            "{} {} ({})",
            title_case(self.archetype.as_str()),
            self.tier.as_str(),
            self.attribute.as_str()
        )
    }
}

/// `title_case`, for callers outside the crate (the status payload builds its
/// archetype list with it).
pub fn title_case_pub(value: &str) -> String {
    title_case(value)
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/* ------------------------------------------------------------------ *
 * Palette
 * ------------------------------------------------------------------ */

/// Every colour a creature is painted with. Two accents plus the eye pair, as
/// the format's authoring notes recommend — a third accent only works as a
/// highlight.
#[derive(Debug, Clone)]
struct Palette {
    body: String,
    body_dark: String,
    accent: String,
    glow: String,
    eye: String,
    pupil: String,
    metal: String,
}

fn base_palette(archetype: Archetype) -> Palette {
    match archetype {
        Archetype::Beast => Palette {
            body: "#8a6b4a".into(),
            body_dark: "#56412e".into(),
            accent: "#e8b25c".into(),
            glow: "#ffd28a".into(),
            eye: "#f6efe2".into(),
            pupil: "#241c14".into(),
            metal: "#7c848c".into(),
        },
        Archetype::Drake => Palette {
            body: "#6d3b52".into(),
            body_dark: "#3f2434".into(),
            accent: "#ff7a3d".into(),
            glow: "#ffc857".into(),
            eye: "#f2e6c9".into(),
            pupil: "#1d1116".into(),
            metal: "#8d7a86".into(),
        },
        Archetype::Cyborg => Palette {
            body: "#4a5a6a".into(),
            body_dark: "#222c36".into(),
            accent: "#38d6ff".into(),
            glow: "#9be7ff".into(),
            eye: "#e8f6ff".into(),
            pupil: "#0b1418".into(),
            metal: "#aab6c2".into(),
        },
        Archetype::Holy => Palette {
            body: "#c9c3e4".into(),
            body_dark: "#8d86b4".into(),
            accent: "#ffd86b".into(),
            glow: "#fff3b0".into(),
            eye: "#fffdf5".into(),
            pupil: "#2b2440".into(),
            metal: "#e6e2f2".into(),
        },
        Archetype::Aqua => Palette {
            body: "#3f9c93".into(),
            body_dark: "#1f5b5c".into(),
            accent: "#ffd166".into(),
            glow: "#a8f0e4".into(),
            eye: "#f4fffb".into(),
            pupil: "#0d2b2c".into(),
            metal: "#8fb7b3".into(),
        },
        Archetype::Cervid => Palette {
            body: "#b08053".into(),
            body_dark: "#6d4a2c".into(),
            accent: "#f4e3b2".into(),
            glow: "#fff6d8".into(),
            eye: "#fbf3e4".into(),
            pupil: "#241a10".into(),
            metal: "#9aa0a6".into(),
        },
        Archetype::Machine => Palette {
            body: "#8c929b".into(),
            body_dark: "#4a4f57".into(),
            accent: "#ff5f56".into(),
            glow: "#ffe08a".into(),
            eye: "#e9fbff".into(),
            pupil: "#12161b".into(),
            metal: "#c2c8d0".into(),
        },
        Archetype::Alien => Palette {
            body: "#7ad14f".into(),
            body_dark: "#3d6b2a".into(),
            accent: "#c77dff".into(),
            glow: "#e2ff8a".into(),
            eye: "#fffbe6".into(),
            pupil: "#16220f".into(),
            metal: "#9d8f6a".into(),
        },
        Archetype::Avian => Palette {
            body: "#d8a13f".into(),
            body_dark: "#8a5f1f".into(),
            accent: "#ff6b57".into(),
            glow: "#ffe6a8".into(),
            eye: "#fffdf2".into(),
            pupil: "#2a1a08".into(),
            metal: "#b8b2a4".into(),
        },
        Archetype::Insect => Palette {
            body: "#6f7a2e".into(),
            body_dark: "#39421a".into(),
            accent: "#f2c14e".into(),
            glow: "#d7f76b".into(),
            eye: "#f7ffdf".into(),
            pupil: "#141a07".into(),
            metal: "#9aa06f".into(),
        },
        Archetype::Serpent => Palette {
            body: "#4a7d5a".into(),
            body_dark: "#24402f".into(),
            accent: "#e0e04a".into(),
            glow: "#b7ff9c".into(),
            eye: "#f6ffe9".into(),
            pupil: "#0e1a12".into(),
            metal: "#8e9c8f".into(),
        },
        Archetype::Golem => Palette {
            body: "#8b8175".into(),
            body_dark: "#4d463d".into(),
            accent: "#d9a441".into(),
            glow: "#ffd98a".into(),
            eye: "#ffe9b0".into(),
            pupil: "#241f18".into(),
            metal: "#b3aa9c".into(),
        },
    }
}

/// What one attribute does to the paint job: body tint, accent tint, glow tint,
/// and how hard each is mixed in.
fn attribute_paint(attribute: Attribute) -> (&'static str, &'static str, &'static str, f64) {
    match attribute {
        // Blessed lines lift toward light.
        Attribute::Vaccine => ("#fff4d0", "#ffe9a3", "#fffbe8", 0.22),
        // Crypto lines cool toward signal blue.
        Attribute::Data => ("#6fd8ff", "#48e0ff", "#b8f4ff", 0.20),
        // Viral lines sink into rot.
        Attribute::Virus => ("#2a0d1d", "#ff3d81", "#9dff4a", 0.24),
    }
}

/// `#rrggbb` → three bytes. Malformed input is black, matching [`mix`].
fn hex_rgb(hex: &str) -> [u8; 3] {
    let digits = hex.trim_start_matches('#');
    if digits.len() != 6 {
        return [0, 0, 0];
    }
    let at = |start: usize| u8::from_str_radix(&digits[start..start + 2], 16).unwrap_or(0);
    [at(0), at(2), at(4)]
}

/// Mix one paint channel (0 body, 1 accent, 2 glow) across the lineage by
/// weight. A one-hot blend returns that attribute's exact colour, so a pure
/// creature's paint is byte-identical to the pre-blend palette.
fn blend_paint(blend: AttributeBlend, channel: usize) -> String {
    let mut acc = [0.0f64; 3];
    for attribute in Attribute::ALL {
        let weight = blend.weight(attribute);
        if weight <= 0.0 {
            continue;
        }
        let paint = attribute_paint(attribute);
        let hex = match channel {
            0 => paint.0,
            1 => paint.1,
            _ => paint.2,
        };
        let rgb = hex_rgb(hex);
        for (slot, value) in acc.iter_mut().zip(rgb) {
            *slot += value as f64 * weight;
        }
    }
    let byte = |value: f64| value.clamp(0.0, 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(acc[0]), byte(acc[1]), byte(acc[2]))
}

/// Tier and seed both move the palette: later stages sit darker and richer (a
/// Mega should not be pastel), and the seed jitters each channel a little so two
/// Rookies with different seeds read as different creatures.
fn palette_for(archetype: Archetype, blend: AttributeBlend, tier: Tier, rng: &mut Rng) -> Palette {
    let base = base_palette(archetype);
    let step = tier.index() as f64;
    let darken = 1.0 - step * 0.035;
    let jitter = |rng: &mut Rng| rng.range(0.94, 1.06);
    let body = shade(&base.body, darken * jitter(rng));
    let body_dark = shade(&base.body_dark, darken * 0.98 * jitter(rng));
    let accent = shade(&base.accent, (1.0 + step * 0.012) * jitter(rng));
    let glow = shade(&base.glow, (1.0 + step * 0.02) * jitter(rng));
    let metal = shade(&base.metal, darken * jitter(rng));
    // The attribute *mix* tints the family. Only a fraction, or every archetype
    // would end up the same three colours again — but the fraction itself is
    // blended, so a Data body with a recent Virus shift is cool *and* bruised
    // rather than one flat colour.
    let tint = blend_paint(blend, 0);
    let accent_tint = blend_paint(blend, 1);
    let glow_tint = blend_paint(blend, 2);
    let strength: f64 = Attribute::ALL
        .iter()
        .map(|attribute| blend.weight(*attribute) * attribute_paint(*attribute).3)
        .sum();
    let body = mix(&body, &tint, strength);
    let body_dark = mix(&body_dark, &tint, strength * 0.8);
    let accent = mix(&accent, &accent_tint, strength * 1.4);
    let glow = mix(&glow, &glow_tint, strength * 1.2);
    // The last degree of freedom: hue. Without it two individuals of one
    // archetype under one attribute are the same animal in the same paint. The
    // body drifts a little (a littermate, not a new species); the accent travels
    // further, because the accent is what the eye reads first.
    let body_hue = rng.range(-0.045, 0.045);
    let accent_hue = rng.range(-0.11, 0.11);
    let saturation = rng.range(0.90, 1.12);
    Palette {
        body: shift_hsv(&body, body_hue, saturation, 1.0),
        body_dark: shift_hsv(&body_dark, body_hue, saturation, 1.0),
        accent: shift_hsv(&accent, accent_hue, 1.0, 1.0),
        glow: shift_hsv(&glow, accent_hue * 0.5, 1.0, 1.0),
        eye: base.eye,
        pupil: base.pupil,
        metal,
    }
}

/// Rotate an `#rrggbb` colour's hue by `turns` (1.0 is a full turn) and scale
/// its saturation, keeping the value — the seeded paint job.
fn shift_hsv(hex: &str, turns: f64, sat_mul: f64, val_mul: f64) -> String {
    let digits = hex.trim_start_matches('#');
    if digits.len() != 6 {
        return hex.to_string();
    }
    let channel = |start: usize| -> f64 {
        u8::from_str_radix(&digits[start..start + 2], 16).unwrap_or(0) as f64 / 255.0
    };
    let (r, g, b) = (channel(0), channel(2), channel(4));
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    let hue = if chroma == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    } / 6.0;
    let sector = (hue + turns).rem_euclid(1.0) * 6.0;
    let saturation = if max == 0.0 { 0.0 } else { chroma / max };
    let value = (max * val_mul).clamp(0.0, 1.0);
    let saturation = (saturation * sat_mul).clamp(0.0, 1.0);
    let c = value * saturation;
    let x = c * (1.0 - ((sector % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match sector as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    let byte = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r1), byte(g1), byte(b1))
}

/// Blend `#rrggbb` `a` toward `b` by `t` (0 = a, 1 = b).
fn mix(a: &str, b: &str, t: f64) -> String {
    let parse = |hex: &str| -> [u8; 3] {
        let digits = hex.trim_start_matches('#');
        if digits.len() != 6 {
            return [0, 0, 0];
        }
        let at = |start: usize| u8::from_str_radix(&digits[start..start + 2], 16).unwrap_or(0);
        [at(0), at(2), at(4)]
    };
    let (ca, cb) = (parse(a), parse(b));
    let t = t.clamp(0.0, 1.0);
    let blend = |index: usize| {
        (ca[index] as f64 * (1.0 - t) + cb[index] as f64 * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    format!("#{:02x}{:02x}{:02x}", blend(0), blend(1), blend(2))
}

/// Multiply an `#rrggbb` colour by `factor`, clamped to the byte range.
fn shade(hex: &str, factor: f64) -> String {
    let digits = hex.trim_start_matches('#');
    if digits.len() != 6 {
        return hex.to_string();
    }
    let channel = |start: usize| {
        let value = u8::from_str_radix(&digits[start..start + 2], 16).unwrap_or(0) as f64;
        (value * factor).round().clamp(0.0, 255.0) as u8
    };
    format!("#{:02x}{:02x}{:02x}", channel(0), channel(2), channel(4))
}

/* ------------------------------------------------------------------ *
 * Dimensions per tier
 * ------------------------------------------------------------------ */

/// Bone lengths in local pixels. Everything here grows with the tier, which is
/// the visible half of "evolves"; the root scale keeps the world-space result
/// inside the monitor budget.
#[derive(Debug, Clone, Copy)]
struct Dims {
    torso_w: f64,
    torso_h: f64,
    head_w: f64,
    head_h: f64,
    upper_arm: f64,
    forearm: f64,
    thigh: f64,
    shin: f64,
    tail: f64,
    wing: f64,
}

impl Dims {
    /// The tier sets the scale; the plan sets the *shape* that scale is applied
    /// to, so a slim deer and a squat robot are the same "Rookie growth".
    fn for_tier(tier: Tier, plan: Plan, rng: &mut Rng) -> Self {
        let t = tier.index() as f64;
        let wobble = |rng: &mut Rng| rng.range(0.94, 1.06);
        Self {
            torso_w: (70.0 + 5.0 * t) * plan.torso_w * wobble(rng),
            torso_h: (78.0 + 6.0 * t) * plan.torso_h * wobble(rng),
            head_w: (66.0 + 3.0 * t) * plan.head_w * wobble(rng),
            head_h: (58.0 + 3.0 * t) * plan.head_h * wobble(rng),
            // The form's `*_segment` multiplies one bone and divides the other,
            // so the limb keeps its reach and changes where it bends: a heavy
            // upper arm over a stubby forearm reads as a different joint, not as
            // a shorter arm.
            upper_arm: (34.0 + 3.0 * t) * plan.arm_len * plan.arm_segment * wobble(rng),
            forearm: (28.0 + 3.0 * t) * plan.arm_len / plan.arm_segment * wobble(rng),
            thigh: (36.0 + 6.0 * t) * plan.leg_len * plan.leg_segment * wobble(rng),
            shin: (32.0 + 5.0 * t) * plan.leg_len / plan.leg_segment * wobble(rng),
            tail: (46.0 + 5.0 * t) * plan.tail_scale * wobble(rng),
            wing: (56.0 + 9.0 * t) * wobble(rng),
        }
    }
}

/// Which attachments exist at a tier. This is the "bone count and limb
/// complexity scale by tier" rule in one place.
#[derive(Debug, Clone, Copy)]
struct Features {
    neck: bool,
    forearm: bool,
    hand: bool,
    shin: bool,
    foot: bool,
    horn: bool,
    horns_tip: bool,
    wings: bool,
    wing_tip: bool,
    wing_tip2: bool,
    eyespot: bool,
    armor: bool,
    tail_segments: usize,
    cheek_plates: bool,
    third_eye: bool,
    crown: bool,
    halo: bool,
    aura: bool,
    /// The crest branches (antler tines, antenna bulbs, tentacle tips).
    crest_branch: bool,
    /// A third crest tine — the "grown up" stage of a branched crest.
    crest_extra: bool,
    /// Third arm segment (tentacles) and its tip.
    arm_third: bool,
    /// Ear tips: swivelling second ear segment.
    ear_tip: bool,
    /// Fin webbing on the tail.
    tail_web: bool,
    /// Which tier these flags were derived for — the attribute details are
    /// tier-gated too, and they only ever see the `Features` value.
    tier_index: usize,
}

impl Features {
    fn for_spec(spec: &Spec, plan: Plan) -> Self {
        let tier = spec.tier;
        let t = tier.index();
        Self {
            neck: t >= 2,
            forearm: t >= 1,
            hand: t >= 2,
            shin: t >= 1,
            foot: t >= 2,
            // The crest is the archetype's signature, so it exists from Fresh —
            // small, and growing with every stage. Hiding it until In-Training
            // is what made every hatchling look like the same blob.
            horn: plan.crest != CrestKind::None,
            horns_tip: t >= 2,
            wings: plan.wings && t >= 3,
            wing_tip: t >= 4,
            wing_tip2: t >= 5,
            eyespot: t >= 4,
            armor: plan.armor && t >= 3,
            // Two segments minimum: one straight capsule with a knob on the
            // end reads as a limb, not a tail.
            tail_segments: 2 + t / 3,
            cheek_plates: plan.crest == CrestKind::Horns && t >= 4,
            third_eye: t >= 5,
            crown: t >= 5,
            halo: spec.archetype == Archetype::Holy && t >= 4,
            aura: plan.visor && t >= 2,
            crest_branch: t >= 1,
            crest_extra: t >= 3,
            arm_third: t >= 2,
            ear_tip: t >= 3,
            tail_web: t >= 2,
            tier_index: t,
        }
    }
}

/* ------------------------------------------------------------------ *
 * z layers
 * ------------------------------------------------------------------ */

const Z_HEIGHT: i32 = -8; // within-bone detail, always behind its siblings
const Z_WING: i32 = 14;
const Z_WING_TIP: i32 = 13;
const Z_TAIL: i32 = 11;
const Z_LEG: i32 = 12;
const Z_PELVIS: i32 = 18;
const Z_TORSO: i32 = 20;
const Z_ARM: i32 = 25;
const Z_HAND: i32 = 26;
const Z_HORN: i32 = 28;
const Z_NECK: i32 = 29;
const Z_HEAD: i32 = 31;

/* ------------------------------------------------------------------ *
 * Generator
 * ------------------------------------------------------------------ */

/// The wave gesture.
///
/// The desktop page adds `anim.wave` to a bone's rotation while the avatar is
/// in `waving` mode (`ui/js/rig.js`), so a rig whose `wave` is 0 stands still
/// and "waving" is a button that does nothing — which is exactly what every
/// procedural creature did. The amplitudes match the hand-authored rigs (0.9 rad
/// on the shoulder, tapering down the arm), and the forearm and hand trail the
/// shoulder by a beat so it reads as a wave instead of a twitch.
///
/// The `phase` stagger is shared with the `walk` channel — the format gives a
/// bone one phase — so the arm swing also lands slightly out of step with the
/// legs, which is what the hand-drawn rigs do.
fn give_wave(bones: &mut [Bone]) {
    for (id, wave) in [
        ("arm_r", 0.9),
        ("arm_l", 0.9),
        ("forearm_r", 0.45),
        ("forearm_l", 0.45),
        ("hand_r", 0.22),
        ("hand_l", 0.22),
        ("wing_r", 0.5),
        ("wing_l", 0.5),
    ] {
        if let Some(bone) = bones.iter_mut().find(|bone| bone.id == id) {
            bone.anim.wave = wave;
        }
    }
    for (id, phase) in [
        ("forearm_r", 0.35),
        ("forearm_l", 0.35),
        ("hand_r", 0.6),
        ("hand_l", 0.6),
    ] {
        if let Some(bone) = bones.iter_mut().find(|bone| bone.id == id) {
            bone.anim.phase = phase;
        }
    }
}

/// Build the manifest for `spec`.
pub fn generate(spec: &Spec) -> Manifest {
    let mut rng = Rng::new(mix_seed(
        spec.seed,
        &[spec.archetype as u64, spec.tier.index() as u64 + 1],
    ));
    // The lineage mix is resolved once, here, and handed to every layer that
    // cares: the plan (which furniture exists), the palette (how it is painted)
    // and the detail trimmers. One value, one meaning.
    let blend = spec.blend();
    let plan = plan::plan_for_seed_blend(spec.archetype, blend, spec.variant(), spec.seed);
    let pal = palette_for(spec.archetype, blend, spec.tier, &mut rng);
    let dims = Dims::for_tier(spec.tier, plan, &mut rng);
    let features = Features::for_spec(spec, plan);
    // The form's secondary geometry. It is authored as parts (not a bone), so it
    // costs no joint, appears at every tier from `Fresh`, and grows with the
    // tier because every size below is a fraction of the tier-scaled `Dims`.
    // A creature may wear two pieces of family furniture — the plan's own
    // accessory and the individual's second trait.
    let accessories: Vec<(&'static str, Vec<Part>)> = std::iter::once(plan.accessory)
        .chain(std::iter::once(plan.accessory2))
        .filter(|kind| *kind != Accessory::None)
        .map(|kind| accessory_parts(&pal, dims, Plan { accessory: kind, ..plan }))
        .filter(|(_, parts)| !parts.is_empty())
        .collect();

    let mut bones: Vec<Bone> = Vec::new();

    /* -------- centerline -------- */

    bones.push(
        Bone::new("root", None, [0.0, 0.0], Z_TORSO)
            .scaled(ROOT_SCALE)
            // The idle bob. Without it the creature is nailed to its footprint
            // and reads as a drawing rather than as something breathing — the
            // hand-authored rigs all carry 3-7 px here.
            .anim(Anim {
                bob: 5.0,
                ..Anim::default()
            }),
    );

    let mut pelvis = Bone::new("pelvis", Some("root"), [0.0, 50.0], Z_PELVIS);
    pelvis.parts = vec![
        Part::ellipse(
            "hips",
            &pal.body_dark,
            [round4(dims.torso_w * 0.72), round4(dims.torso_h * 0.42)],
            [0.0, 2.0],
            0,
            None,
        ),
        Part::ellipse(
            "hip_plate",
            &pal.body,
            [round4(dims.torso_w * 0.5), round4(dims.torso_h * 0.22)],
            [0.0, -6.0],
            1,
            None,
        ),
    ];
    if plan.legs == LegKind::Coiled {
        // Three stacked rings: a coiled serpent holding itself up.
        for (index, ring) in [(0, 0.9), (1, 0.74), (2, 0.58)].iter() {
            pelvis.parts.push(Part::ellipse(
                if *index == 0 { "coil" } else { "coil_more" },
                if *index % 2 == 0 { &pal.body_dark } else { &pal.body },
                [
                    round4(dims.torso_w * ring),
                    round4(dims.torso_w * ring * 0.42),
                ],
                [0.0, round4(dims.torso_h * (0.22 + *index as f64 * 0.16))],
                2 + *index as i32,
                None,
            ));
        }
    }
    if plan.legs == LegKind::Hover {
        // No legs: a hovering ring instead, and the body sits above it.
        pelvis.parts.push(Part::ellipse(
            "hover_skirt",
            &pal.body_dark,
            [round4(dims.torso_w * 0.9), round4(dims.torso_h * 0.3)],
            [0.0, round4(dims.torso_h * 0.34)],
            2,
            None,
        ));
        pelvis.parts.push(Part::ellipse(
            "hover_ring",
            &pal.glow,
            [round4(dims.torso_w * 0.6), 8.0],
            [0.0, round4(dims.torso_h * 0.5)],
            3,
            None,
        ));
    }
    for (bone_id, parts) in &accessories {
        if *bone_id == "pelvis" {
            pelvis.parts.extend(parts.iter().cloned());
        }
    }
    pelvis.anim = Anim::sway(0.01, 0.0);
    bones.push(pelvis);

    let mut torso = Bone::new("torso", Some("pelvis"), [0.0, -50.0], Z_TORSO);
    torso.anim = Anim::sway(0.015, 0.0);
    let half_h = dims.torso_h / 2.0;
    torso.parts = vec![
        // A structural spine capsule, run along the bone's local +x chord and
        // therefore rotated upright — kept inset inside the chest ellipse
        // (see `structural_len`), never tangent to the contour.
        Part::capsule(
            "spine",
            &pal.body_dark,
            [round4(structural_len(half_h, 10.0)), 10.0],
            [0.0, 0.0],
            0,
        )
        .turned(-std::f64::consts::FRAC_PI_2),
        Part::ellipse(
            "chest",
            &pal.body,
            [round4(dims.torso_w), round4(dims.torso_h)],
            [0.0, 0.0],
            1,
            None,
        ),
        Part::ellipse(
            "belly",
            &pal.body_dark,
            [round4(dims.torso_w * 0.62), round4(dims.torso_h * 0.55)],
            [0.0, round4(dims.torso_h * 0.24)],
            2,
            None,
        ),
        Part::ellipse(
            "collar",
            &pal.accent,
            [round4(dims.torso_w * 0.52), 9.0],
            [0.0, round4(-dims.torso_h * 0.42)],
            3,
            None,
        ),
    ];
    if features.armor {
        torso.parts.push(Part::ellipse(
            "chest_plate",
            &pal.metal,
            [round4(dims.torso_w * 0.46), round4(dims.torso_h * 0.34)],
            [0.0, round4(-dims.torso_h * 0.1)],
            4,
            None,
        ));
    }
    if features.aura {
        torso.parts.push(Part::circle(
            "core_glow",
            &pal.glow,
            round4(dims.torso_w * 0.16),
            [0.0, round4(-dims.torso_h * 0.16)],
            5,
            None,
        ));
    }
    // The plan's trunk furniture. Each of these is what makes the silhouette
    // recognisable from across the desktop: ribs, plates, a skirt, a mantle.
    match plan.torso {
        TorsoKind::Barrel => {
            for (index, sign) in [(-1.0, -1.0), (1.0, 1.0)] {
                torso.parts.push(Part::ellipse(
                    if index < 0.0 { "rib_l" } else { "rib_r" },
                    &pal.metal,
                    [round4(dims.torso_w * 0.22), round4(dims.torso_h * 0.5)],
                    [round4(sign * dims.torso_w * 0.34), round4(-dims.torso_h * 0.06)],
                    5,
                    None,
                ));
            }
            if features.armor {
                torso.parts.push(Part::ellipse(
                    "belly_scales",
                    &pal.accent,
                    [round4(dims.torso_w * 0.5), round4(dims.torso_h * 0.16)],
                    [0.0, round4(dims.torso_h * 0.28)],
                    5,
                    None,
                ));
            }
        }
        TorsoKind::Slab => {
            for (id, sign) in [("slab_l", -1.0), ("slab_r", 1.0)] {
                torso.parts.push(Part::ellipse(
                    id,
                    &pal.metal,
                    [round4(dims.torso_w * 0.24), round4(dims.torso_h * 0.62)],
                    [round4(sign * dims.torso_w * 0.32), round4(-dims.torso_h * 0.06)],
                    5,
                    None,
                ));
            }
            torso.parts.push(Part::ellipse(
                "seam",
                &pal.pupil,
                [round4(dims.torso_w * 0.7), 5.0],
                [0.0, round4(dims.torso_h * 0.14)],
                4,
                None,
            ));
        }
        TorsoKind::Robe => {
            // A flared skirt below the chest: the holy plan's whole outline.
            torso.parts.push(Part::ellipse(
                "robe_skirt",
                &pal.body_dark,
                [round4(dims.torso_w * 0.92), round4(dims.torso_h * 0.62)],
                [0.0, round4(dims.torso_h * 0.62)],
                2,
                None,
            ));
            torso.parts.push(Part::ellipse(
                "robe_hem",
                &pal.accent,
                [round4(dims.torso_w * 0.96), 10.0],
                [0.0, round4(dims.torso_h * 0.92)],
                3,
                None,
            ));
        }
        TorsoKind::Mantle => {
            torso.parts.push(Part::ellipse(
                "mantle",
                &pal.body,
                [round4(dims.torso_w * 1.12), round4(dims.torso_h * 1.05)],
                [0.0, round4(dims.torso_h * 0.16)],
                0,
                None,
            ));
            torso.parts.push(Part::ellipse(
                "mantle_rim",
                &pal.accent,
                [round4(dims.torso_w * 1.16), 9.0],
                [0.0, round4(dims.torso_h * 0.66)],
                1,
                None,
            ));
            torso.parts.push(Part::circle(
                "mantle_core",
                &pal.glow,
                round4(dims.torso_w * 0.3),
                [0.0, round4(-dims.torso_h * 0.1)],
                6,
                None,
            ));
            torso.parts.push(Part::circle(
                "mantle_pupil",
                &pal.pupil,
                round4(dims.torso_w * 0.12),
                [0.0, round4(-dims.torso_h * 0.1)],
                7,
                None,
            ));
        }
        TorsoKind::Round => {}
    }
    if plan.dorsal_fin {
        torso.parts.push(Part::ellipse(
            "dorsal_fin",
            &pal.accent,
            [round4(dims.torso_w * 0.2), round4(dims.torso_h * 0.8)],
            [0.0, round4(-dims.torso_h * 0.62)],
            Z_HEIGHT,
            None,
        ));
    }
    if plan.gills {
        for (id, sign) in [("gill_slit_l", -1.0), ("gill_slit_r", 1.0)] {
            torso.parts.push(
                Part::ellipse(
                    id,
                    &pal.accent,
                    [round4(dims.torso_w * 0.16), 6.0],
                    [round4(sign * dims.torso_w * 0.36), round4(-dims.torso_h * 0.2)],
                    5,
                    None,
                )
                .turned(round4(sign * 0.35)),
            );
        }
    }
    if plan.engine {
        for (id, sign) in [("vent_l", -1.0), ("vent_r", 1.0)] {
            torso.parts.push(Part::ellipse(
                id,
                &pal.metal,
                [round4(dims.torso_w * 0.2), round4(dims.torso_h * 0.34)],
                [round4(sign * dims.torso_w * 0.34), round4(-dims.torso_h * 0.28)],
                5,
                None,
            ));
            torso.parts.push(Part::circle(
                if sign < 0.0 { "jet_l" } else { "jet_r" },
                &pal.glow,
                round4(dims.torso_w * 0.1),
                [round4(sign * dims.torso_w * 0.34), round4(-dims.torso_h * 0.46)],
                6,
                None,
            ));
        }
    }
    torso.parts.extend(aspect_torso_parts(&pal, dims, plan, features, blend));
    for (bone_id, parts) in &accessories {
        if *bone_id == "torso" {
            torso.parts.extend(parts.iter().cloned());
        }
    }
    if plan.spots && features.armor {
        for index in 0..3 {
            torso.parts.push(Part::circle(
                "spot",
                &pal.glow,
                round4(dims.torso_w * (0.08 + index as f64 * 0.015)),
                [
                    round4(dims.torso_w * (-0.3 + index as f64 * 0.3)),
                    round4(dims.torso_h * (-0.1 + index as f64 * 0.16)),
                ],
                5,
                None,
            ));
        }
    }
    bones.push(torso);

    let head_y = if features.neck {
        let neck_y = -(half_h + 14.0);
        let mut neck = Bone::new("neck", Some("torso"), [0.0, round4(neck_y)], Z_NECK)
            .anim(Anim::sway(0.008, 0.4));
        neck.parts = vec![
            Part::ellipse(
                "neck",
                &pal.body,
                [round4(dims.torso_w * 0.42), round4(24.0)],
                [0.0, 0.0],
                0,
                None,
            ),
            Part::ellipse(
                "neck_trim",
                &pal.accent,
                [round4(dims.torso_w * 0.34), 6.0],
                [0.0, 4.0],
                1,
                None,
            ),
        ];
        neck = neck.limited(0.45);
        bones.push(neck);
        // Relative to the **neck** (the head's parent), half a skull minus a few
        // px of overlap, so the skull's lower half sits over the neck ellipse.
        // Getting this frame wrong is what leaves the head floating a visible
        // gap above the body.
        round4(-(dims.head_h * 0.5 - 6.0))
    } else {
        round4(-(half_h + dims.head_h * 0.5 - 8.0))
    };

    let mut head = Bone::new(
        "head",
        Some(if features.neck { "neck" } else { "torso" }),
        [0.0, head_y],
        Z_HEAD,
    );
    head.anim = Anim::sway(0.012, 0.9);
    head.parts = head_parts(&pal, dims, features, plan);
    head.parts.extend(aspect_head_parts(&pal, dims, plan, features, blend));
    for (bone_id, parts) in &accessories {
        if *bone_id == "head" {
            head.parts.extend(parts.iter().cloned());
        }
    }
    if features.halo {
        head.parts.insert(
            0,
            Part::ellipse(
                "halo_outer",
                &pal.glow,
                [round4(dims.head_w * 0.9), 18.0],
                [0.0, round4(-dims.head_h * 0.95)],
                Z_HEIGHT,
                None,
            ),
        );
        head.parts.insert(
            1,
            Part::ellipse(
                "halo_inner",
                &pal.body_dark,
                [round4(dims.head_w * 0.68), 11.0],
                [0.0, round4(-dims.head_h * 0.95)],
                Z_HEIGHT + 1,
                None,
            ),
        );
    }
    bones.push(head);

    push_tail(&mut bones, &pal, dims, features, plan, &mut rng);

    /* -------- right-side chains, each mirrored immediately -------- */

    let mark = bones.len();
    push_arm(&mut bones, &pal, dims, features, plan, &mut rng);
    apply_stance(&mut bones, mark, plan.arm_stance);
    mirror_chain(&mut bones, mark);

    let mark = bones.len();
    push_leg(&mut bones, &pal, dims, features, plan, &mut rng);
    apply_stance(&mut bones, mark, plan.leg_stance);
    mirror_chain(&mut bones, mark);

    if features.horn {
        let mark = bones.len();
        push_crest(&mut bones, &pal, dims, features, plan, &mut rng);
        // The form's crest scale and spread are applied to the finished chain in
        // one place, so tine bones keep hanging off their parent
        // (`scale_parts` grows the artwork around the joint it was authored
        // from).
        scale_crest(&mut bones[mark..], plan.crest_scale, plan.crest_spread);
        mirror_chain(&mut bones, mark);
    }

    let mark = bones.len();
    push_ear(&mut bones, &pal, dims, features, plan, &mut rng);
    mirror_chain(&mut bones, mark);

    if features.wings {
        let mark = bones.len();
        push_wing(&mut bones, &pal, dims, features, plan, &mut rng);
        mirror_chain(&mut bones, mark);
    } else if plan.wings {
        // A hatchling's wings are folded buds: without them a Fresh Drake is
        // indistinguishable from a Fresh Beast, which is exactly the sameness
        // the archetypes exist to break.
        let mark = bones.len();
        push_wing_bud(&mut bones, &pal, dims, plan);
        mirror_chain(&mut bones, mark);
    }

    // Every bone exists now; the wave gesture is per-bone animation data, not
    // geometry, so it is applied last — after the arms (whatever kind they are)
    // and before the clips read the skeleton.
    give_wave(&mut bones);

    let mut manifest = Manifest {
        format: "char-rig/model@1".to_string(),
        name: spec.slug(),
        label: spec.label(),
        notes: Some(match spec.blend {
            // A one-hot mix *is* the single attribute, so it keeps the old note
            // (and therefore the old bytes) rather than announcing a blend.
            Some(blend) if !blend.is_pure() => format!(
                "procedurally generated: seed={} archetype={} tier={} blend={}",
                spec.seed,
                spec.archetype.as_str(),
                spec.tier.as_str(),
                blend.summary()
            ),
            _ => format!(
                "procedurally generated: seed={} archetype={} tier={}",
                spec.seed,
                spec.archetype.as_str(),
                spec.tier.as_str()
            ),
        }),
        animations: None,
        bones,
    };

    // The clips are built from the finished skeleton, so they can only name
    // bones that exist.
    let mut animations = BTreeMap::new();
    animations.insert("breathe".to_string(), breathe_clip(&manifest, spec.tier));
    animations.insert("wave".to_string(), wave_clip(&manifest));
    animations.insert("digivolve".to_string(), digivolve_clip(&manifest, spec.tier));
    manifest.animations = Some(animations);

    enforce_monitor_budget(&mut manifest);
    trim_details(&mut manifest, spec);
    manifest
}

/// Grow a bone's artwork about the joint it was authored from. Used for the
/// per-form accessory geometry (ears, crests): the offset is the piece's centre
/// in the bone's local frame, so scaling both keeps the shape hinged where it
/// was drawn.
fn scale_parts(parts: &mut [Part], scale: f64) {
    if (scale - 1.0).abs() < 1e-9 {
        return;
    }
    for part in parts {
        part.size[0] = round4(part.size[0] * scale);
        part.size[1] = round4(part.size[1] * scale);
        part.offset[0] = round4(part.offset[0] * scale);
        part.offset[1] = round4(part.offset[1] * scale);
    }
}

/// Scale a whole crest chain and swing its root by `spread`.
///
/// The root's attachment point is left where the body plan put it (a horn grows
/// out of the skull, it does not slide toward the skull's centre); the child
/// tines scale with their parent, including the position they hang from.
fn scale_crest(chain: &mut [Bone], scale: f64, spread: f64) {
    for (index, bone) in chain.iter_mut().enumerate() {
        if index == 0 {
            bone.rotation = round4(bone.rotation + spread);
        } else {
            bone.pos[0] = round4(bone.pos[0] * scale);
            bone.pos[1] = round4(bone.pos[1] * scale);
        }
        bone.length = round4(bone.length * scale);
        scale_parts(&mut bone.parts, scale);
    }
}

/// Nudge every chain root in `bones[from..]` — a bone hanging off the trunk or
/// the pelvis — by `stance`, which is how a form sets its posture. Children are
/// untouched: they are posed relative to their parent, so their world pose
/// follows automatically (and stays mirrored).
fn apply_stance(bones: &mut [Bone], from: usize, stance: f64) {
    if stance == 0.0 {
        return;
    }
    for bone in &mut bones[from..] {
        if matches!(bone.parent.as_deref(), Some("torso") | Some("pelvis")) {
            bone.rotation = round4(bone.rotation + stance);
        }
    }
}

/// The form's secondary silhouette geometry, hung on the bone that owns the
/// region: `("pelvis" | "torso" | "head" | "tail", parts)`.
///
/// Every size is a fraction of the tier-scaled `Dims`, so the accessory is there
/// on a `Fresh` hatchling (small) and grows with every stage — the form is part
/// of the creature's identity from stage one, not a Champion-only flourish.
/// `TailBlades` is drawn by `push_tail` and returns no parts here.
fn accessory_parts(pal: &Palette, dims: Dims, plan: Plan) -> (&'static str, Vec<Part>) {
    let w = dims.torso_w;
    let h = dims.torso_h;
    let hw = dims.head_w;
    let hh = dims.head_h;
    match plan.accessory {
        Accessory::None => ("torso", Vec::new()),

        // A layered ruff from the shoulders up to the jaw hinge.
        Accessory::Mane => (
            "torso",
            vec![
                Part::ellipse(
                    "mane",
                    &pal.body_dark,
                    [round4(w * 0.92), round4(h * 0.3)],
                    [0.0, round4(-h * 0.5)],
                    6,
                    None,
                ),
                Part::ellipse(
                    "mane_mid",
                    &pal.accent,
                    [round4(w * 0.72), round4(h * 0.22)],
                    [0.0, round4(-h * 0.64)],
                    6,
                    None,
                ),
                Part::ellipse(
                    "mane_tip",
                    &pal.body,
                    [round4(w * 0.48), round4(h * 0.16)],
                    [0.0, round4(-h * 0.78)],
                    6,
                    None,
                ),
            ],
        ),

        // Three plates marching down the spine — the whole back line changes.
        Accessory::Ridge => (
            "torso",
            vec![
                Part::ellipse(
                    "ridge",
                    &pal.accent,
                    [round4(w * 0.18), round4(h * 0.36)],
                    [0.0, round4(-h * 0.6)],
                    6,
                    None,
                ),
                Part::ellipse(
                    "ridge_mid",
                    &pal.accent,
                    [round4(w * 0.15), round4(h * 0.3)],
                    [0.0, round4(-h * 0.82)],
                    6,
                    None,
                ),
                Part::ellipse(
                    "ridge_tip",
                    &pal.accent,
                    [round4(w * 0.12), round4(h * 0.24)],
                    [0.0, round4(-h * 0.98)],
                    6,
                    None,
                ),
            ],
        ),

        // Angled spurs off the top of each shoulder.
        Accessory::Spurs => (
            "torso",
            vec![
                Part::ellipse(
                    "spur_l",
                    &pal.accent,
                    [round4(w * 0.34), round4(h * 0.1)],
                    [round4(-w * 0.44), round4(-h * 0.34)],
                    6,
                    None,
                )
                .turned(round4(-0.5)),
                Part::ellipse(
                    "spur_r",
                    &pal.accent,
                    [round4(w * 0.34), round4(h * 0.1)],
                    [round4(w * 0.44), round4(-h * 0.34)],
                    6,
                    None,
                )
                .turned(round4(0.5)),
            ],
        ),

        // Swept fins from the hips: the lower silhouette widens.
        Accessory::HipFins => (
            "pelvis",
            vec![
                Part::ellipse(
                    "hipfin_l",
                    &pal.accent,
                    [round4(w * 0.52), round4(h * 0.16)],
                    [round4(-w * 0.46), round4(h * 0.06)],
                    3,
                    None,
                )
                .turned(round4(-0.34)),
                Part::ellipse(
                    "hipfin_r",
                    &pal.accent,
                    [round4(w * 0.52), round4(h * 0.16)],
                    [round4(w * 0.46), round4(h * 0.06)],
                    3,
                    None,
                )
                .turned(round4(0.34)),
            ],
        ),

        // A fan along the jaw: gills, a hood, a dewlap.
        Accessory::Frill => (
            "head",
            vec![
                Part::ellipse(
                    "frill_l",
                    &pal.accent,
                    [round4(hw * 0.32), round4(hh * 0.16)],
                    [round4(-hw * 0.36), round4(hh * 0.28)],
                    4,
                    None,
                )
                .turned(round4(-0.42)),
                Part::ellipse(
                    "frill_r",
                    &pal.accent,
                    [round4(hw * 0.32), round4(hh * 0.16)],
                    [round4(hw * 0.36), round4(hh * 0.28)],
                    4,
                    None,
                )
                .turned(round4(0.42)),
                Part::ellipse(
                    "frill_mid",
                    &pal.body_dark,
                    [round4(hw * 0.26), round4(hh * 0.12)],
                    [0.0, round4(hh * 0.34)],
                    4,
                    None,
                ),
            ],
        ),

        // A second, shorter sensor pair beside the crest.
        Accessory::Antennae => (
            "head",
            vec![
                Part::ellipse(
                    "ant2_l",
                    &pal.metal,
                    [round4(hw * 0.5), 6.0],
                    [round4(-hw * 0.36), round4(-hh * 0.66)],
                    Z_HEIGHT,
                    None,
                )
                .turned(round4(-0.7)),
                Part::ellipse(
                    "ant2_r",
                    &pal.metal,
                    [round4(hw * 0.5), 6.0],
                    [round4(hw * 0.36), round4(-hh * 0.66)],
                    Z_HEIGHT,
                    None,
                )
                .turned(round4(0.7)),
                Part::circle(
                    "ant2_tip",
                    &pal.glow,
                    6.0,
                    [0.0, round4(-hh * 0.8)],
                    Z_HEIGHT,
                    None,
                ),
            ],
        ),

        // A ring around the throat.
        Accessory::Collar => (
            "torso",
            vec![
                Part::ellipse(
                    "collar_ring",
                    &pal.metal,
                    [round4(w * 0.58), 12.0],
                    [0.0, round4(-h * 0.44)],
                    6,
                    None,
                ),
                Part::ellipse(
                    "collar_in",
                    &pal.body_dark,
                    [round4(w * 0.44), 6.0],
                    [0.0, round4(-h * 0.44)],
                    6,
                    None,
                ),
            ],
        ),

        // Drawn per tail segment in `push_tail`.
        Accessory::TailBlades => ("tail", Vec::new()),
    }
}

/// The part budget for a stage.
///
/// Fresh through Champion keep the format's 100-part ceiling. Ultimate and Mega
/// get 150: a six-stage line that has mixed two or three attributes genuinely
/// has more silhouette to draw, and holding it to a Rookie's budget was deleting
/// exactly the layered detail the blend exists to show.
pub fn max_parts(tier: Tier) -> usize {
    match tier {
        Tier::Fresh | Tier::InTraining | Tier::Rookie | Tier::Champion => MAX_PARTS,
        Tier::Ultimate | Tier::Mega => 150,
    }
}

/// Parts that carry no silhouette. When a rich plan (a Mega machine under a rich
/// attribute mix) runs into the budget, these go first — the creature must lose
/// decoration, never a limb.
///
/// Each entry is tagged with the attribute whose **share of the lineage** decides
/// how soon it is sacrificed: the faintest/oldest attribute's decoration goes
/// before the dominant/latest attribute loses anything. `None` marks seed and
/// tier decoration (markings, seams, the form's own furniture) which carries no
/// attribute identity, so it is sacrificed ahead of every attribute's share.
/// Within one share the authored order below is the removal order.
///
/// Structural and limb parts are deliberately absent from this table: they are
/// not decoration, so no blend can ever trim them.
const DECORATION: [(&str, Option<Attribute>); 38] = [
    ("spot", None),
    ("stripe", None),
    ("patch", None),
    ("patch_l", None),
    ("patch_r", None),
    ("seam", None),
    ("barrel_ring", None),
    ("feather_more", None),
    ("mane_tip", None),
    ("ridge_tip", None),
    ("frill_mid", None),
    ("ant2_tip", None),
    ("mane_mid", None),
    ("ridge_mid", None),
    ("spur_l", None),
    ("spur_r", None),
    ("hipfin_l", None),
    ("hipfin_r", None),
    ("frill_l", None),
    ("frill_r", None),
    ("ant2_l", None),
    ("ant2_r", None),
    ("collar_in", None),
    ("collar_ring", None),
    ("tail_blade", None),
    ("mane", None),
    ("ridge", None),
    ("halo_in", Some(Attribute::Vaccine)),
    ("circuit2_l", Some(Attribute::Data)),
    ("circuit2_r", Some(Attribute::Data)),
    ("circuit_l", Some(Attribute::Data)),
    ("circuit_r", Some(Attribute::Data)),
    ("crystal_core", Some(Attribute::Data)),
    ("tooth_more_l", Some(Attribute::Virus)),
    ("tooth_more_r", Some(Attribute::Virus)),
    ("drip_l", Some(Attribute::Virus)),
    ("drip_r", Some(Attribute::Virus)),
    ("spine_mid", Some(Attribute::Virus)),
];

fn trim_details(manifest: &mut Manifest, spec: &Spec) {
    // Four parts of headroom under the tier's cap, so the caller never gets a rig
    // that passes today and fails after one more detail is added.
    let budget = max_parts(spec.tier).saturating_sub(4);
    if manifest.part_count() <= budget {
        return;
    }
    let blend = spec.blend();
    // Sacrifice the faintest attribute's decoration first. An attribute that has
    // almost left the lineage must not cost the dominant one its silhouette.
    let mut order: Vec<usize> = (0..DECORATION.len()).collect();
    order.sort_by(|left, right| {
        let share = |index: usize| match DECORATION[index].1 {
            Some(attribute) => blend.weight(attribute),
            None => -1.0,
        };
        share(*left)
            .partial_cmp(&share(*right))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.cmp(right))
    });
    for index in order {
        if manifest.part_count() <= budget {
            return;
        }
        let id = DECORATION[index].0;
        for bone in manifest.bones.iter_mut() {
            bone.parts.retain(|part| part.id != id);
        }
    }
}

/// Largest capsule length that stays inset inside an ellipse of radius
/// `half_span` (the format's rule: `0.5 × capsule_height + 5 px` of margin).
fn structural_len(half_span: f64, height: f64) -> f64 {
    (half_span - height / 2.0 - 5.0 - (height / 2.0)).max(12.0)
}

/* ------------------------------------------------------------------ *
 * Parts
 * ------------------------------------------------------------------ */

/// The face, in the order the format asks for: skull, muzzle, brow pair
/// (ellipses), eye/pupil pairs, split mouth — with the skull and jaw shaped by
/// the plan, so a fish, a deer and a robot do not share one head.
fn head_parts(pal: &Palette, dims: Dims, features: Features, plan: Plan) -> Vec<Part> {
    let w = dims.head_w;
    let h = dims.head_h;
    let mut parts: Vec<Part> = Vec::new();

    // --- skull ---
    match plan.head {
        HeadKind::Round | HeadKind::Muzzle => {
            parts.push(Part::ellipse(
                "skull",
                &pal.body,
                [round4(w), round4(h)],
                [0.0, 0.0],
                0,
                None,
            ));
        }
        HeadKind::Dome => {
            parts.push(Part::ellipse(
                "skull",
                &pal.body,
                [round4(w * 0.94), round4(h * 1.04)],
                [0.0, round4(-h * 0.04)],
                0,
                None,
            ));
            // A wide calm brow band — the dome's whole character is the brow.
            parts.push(Part::ellipse(
                "brow_band",
                &pal.body_dark,
                [round4(w * 0.9), round4(h * 0.24)],
                [0.0, round4(-h * 0.26)],
                1,
                None,
            ));
        }
        HeadKind::Bulbous => {
            parts.push(Part::ellipse(
                "skull",
                &pal.body,
                [round4(w), round4(h * 0.96)],
                [0.0, 0.0],
                0,
                None,
            ));
            // A swollen cranium above the eyes: this is what makes the head read
            // as "fish/alien" rather than "round".
            parts.push(Part::ellipse(
                "cranium",
                &pal.body_dark,
                [round4(w * 0.78), round4(h * 0.42)],
                [0.0, round4(-h * 0.42)],
                1,
                None,
            ));
        }
        HeadKind::Beak => {
            // A small round skull with the beak doing the work.
            parts.push(Part::ellipse(
                "skull",
                &pal.body,
                [round4(w * 0.9), round4(h * 0.92)],
                [0.0, 0.0],
                0,
                None,
            ));
            parts.push(Part::ellipse(
                "brow_plume",
                &pal.body_dark,
                [round4(w * 0.82), round4(h * 0.26)],
                [0.0, round4(-h * 0.28)],
                1,
                None,
            ));
        }
        HeadKind::Boxy => {
            parts.push(Part::ellipse(
                "skull",
                &pal.body,
                [round4(w), round4(h * 0.88)],
                [0.0, round4(-h * 0.04)],
                0,
                None,
            ));
            parts.push(Part::ellipse(
                "skull_plate",
                &pal.metal,
                [round4(w * 0.86), round4(h * 0.44)],
                [0.0, round4(-h * 0.3)],
                1,
                None,
            ));
        }
    }

    if features.third_eye {
        parts.push(Part::circle(
            "third_eye",
            &pal.eye,
            round4(w * 0.16),
            [0.0, round4(-h * 0.44)],
            2,
            Some("eye"),
        ));
        parts.push(Part::circle(
            "third_pupil",
            &pal.pupil,
            round4(w * 0.07),
            [0.0, round4(-h * 0.44)],
            3,
            Some("pupil"),
        ));
    }

    // --- muzzle / jaw ---
    match plan.head {
        HeadKind::Round => {
            parts.push(Part::ellipse(
                "muzzle",
                &pal.body_dark,
                [round4(w * 0.62), round4(h * 0.46)],
                [0.0, round4(h * 0.3)],
                3,
                None,
            ));
            parts.push(Part::circle(
                "nose",
                &pal.pupil,
                round4(w * 0.11),
                [0.0, round4(h * 0.16)],
                4,
                None,
            ));
        }
        HeadKind::Muzzle => {
            // Long snout: the nose sits far forward and the jaw hangs under it.
            parts.push(Part::ellipse(
                "muzzle",
                &pal.body_dark,
                [round4(w * 0.86), round4(h * 0.44)],
                [0.0, round4(h * 0.44)],
                3,
                None,
            ));
            parts.push(Part::ellipse(
                "jaw",
                &pal.body_dark,
                [round4(w * 0.7), round4(h * 0.24)],
                [0.0, round4(h * 0.62)],
                3,
                None,
            ));
            parts.push(Part::circle(
                "nose",
                &pal.pupil,
                round4(w * 0.1),
                [0.0, round4(h * 0.3)],
                4,
                None,
            ));
            // Fangs: two small light wedges at the corners of the jaw.
            for (id, sign) in [("fang_l", -1.0), ("fang_r", 1.0)] {
                parts.push(Part::ellipse(
                    id,
                    &pal.eye,
                    [round4(w * 0.07), round4(h * 0.12)],
                    [round4(sign * w * 0.22), round4(h * 0.5)],
                    5,
                    None,
                ));
            }
        }
        HeadKind::Dome => {
            parts.push(Part::ellipse(
                "muzzle",
                &pal.body_dark,
                [round4(w * 0.54), round4(h * 0.34)],
                [0.0, round4(h * 0.36)],
                3,
                None,
            ));
            parts.push(Part::circle(
                "nose",
                &pal.pupil,
                round4(w * 0.08),
                [0.0, round4(h * 0.22)],
                4,
                None,
            ));
            // A gem on the brow: the holy plan's one piece of jewellery.
            parts.push(Part::ellipse(
                "brow_gem",
                &pal.accent,
                [round4(w * 0.14), round4(h * 0.2)],
                [0.0, round4(-h * 0.14)],
                4,
                None,
            ));
        }
        HeadKind::Bulbous => {
            parts.push(Part::ellipse(
                "muzzle",
                &pal.body_dark,
                [round4(w * 0.7), round4(h * 0.3)],
                [0.0, round4(h * 0.34)],
                3,
                None,
            ));
            for (id, sign) in [("nostril_l", -1.0), ("nostril_r", 1.0)] {
                parts.push(Part::circle(
                    id,
                    &pal.pupil,
                    round4(w * 0.05),
                    [round4(sign * w * 0.12), round4(h * 0.26)],
                    4,
                    None,
                ));
            }
        }
        HeadKind::Beak => {
            // Two mandibles: the taper is what makes a beak read as a beak.
            parts.push(
                Part::ellipse(
                    "beak_upper",
                    &pal.accent,
                    [round4(w * 0.62), round4(h * 0.26)],
                    [0.0, round4(h * 0.38)],
                    3,
                    None,
                )
                .turned(round4(0.12)),
            );
            parts.push(
                Part::ellipse(
                    "beak_lower",
                    &pal.accent,
                    [round4(w * 0.5), round4(h * 0.16)],
                    [0.0, round4(h * 0.52)],
                    3,
                    None,
                )
                .turned(round4(-0.1)),
            );
            for (id, sign) in [("nostril_l", -1.0), ("nostril_r", 1.0)] {
                parts.push(Part::circle(
                    id,
                    &pal.pupil,
                    round4(w * 0.04),
                    [round4(sign * w * 0.1), round4(h * 0.28)],
                    4,
                    None,
                ));
            }
        }
        HeadKind::Boxy => {
            parts.push(Part::ellipse(
                "muzzle",
                &pal.metal,
                [round4(w * 0.62), round4(h * 0.26)],
                [0.0, round4(h * 0.42)],
                3,
                None,
            ));
            for (id, sign) in [("vent_l", -1.0), ("vent_r", 1.0)] {
                parts.push(Part::ellipse(
                    id,
                    &pal.pupil,
                    [round4(w * 0.14), 4.0],
                    [round4(sign * w * 0.16), round4(h * 0.42)],
                    4,
                    None,
                ));
            }
        }
    }

    // --- brows, eyes, pupils, mouth: the format's face contract, every plan ---
    let (eye_w, eye_h, eye_dx) = if plan.head == HeadKind::Bulbous {
        // Oversized and wide-set: the aquatic/alec look.
        (w * 0.3, h * 0.3, w * 0.26)
    } else {
        (w * 0.24, h * 0.24, w * 0.2)
    };
    if plan.visor && plan.head != HeadKind::Boxy {
        parts.push(Part::ellipse(
            "visor_band",
            &pal.metal,
            [round4(w * 0.84), round4(h * 0.3)],
            [0.0, round4(-h * 0.06)],
            4,
            None,
        ));
    }
    for (id, sign) in [("brow_l", -1.0), ("brow_r", 1.0)] {
        parts.push(
            Part::ellipse(
                id,
                &pal.body_dark,
                [round4(w * 0.26), 7.0],
                [round4(sign * w * 0.19), round4(-h * 0.08)],
                5,
                Some("brow"),
            )
            .turned(round4(sign * 0.12)),
        );
    }
    for (id, sign) in [("eye_l", -1.0), ("eye_r", 1.0)] {
        parts.push(Part::ellipse(
            id,
            &pal.eye,
            [round4(eye_w), round4(eye_h)],
            [round4(sign * eye_dx), round4(-h * 0.02)],
            6,
            Some("eye"),
        ));
    }
    for (id, sign) in [("pupil_l", -1.0), ("pupil_r", 1.0)] {
        parts.push(Part::circle(
            id,
            &pal.pupil,
            round4(w * 0.1),
            [round4(sign * eye_dx), round4(-h * 0.02)],
            7,
            Some("pupil"),
        ));
    }
    // Two halves at ±7 px, so `mouth_curve` lifts and tilts both corners.
    for (id, sign) in [("mouth_l", -1.0), ("mouth_r", 1.0)] {
        parts.push(Part::ellipse(
            id,
            &pal.pupil,
            [round4(w * 0.2), 5.0],
            [round4(sign * 7.0), round4(h * 0.36)],
            8,
            Some("mouth"),
        ));
    }

    if plan.crest == CrestKind::Horns || features.cheek_plates {
        parts.push(Part::ellipse(
            "crest",
            &pal.accent,
            [round4(w * 0.16), round4(h * 0.46)],
            [0.0, round4(-h * 0.62)],
            Z_HEIGHT,
            None,
        ));
    }
    if plan.crest == CrestKind::Fin {
        // A centre fin instead of horns: tall, thin, swept back.
        parts.push(Part::ellipse(
            "head_fin",
            &pal.accent,
            [round4(w * 0.18), round4(h * 0.7)],
            [0.0, round4(-h * 0.6)],
            Z_HEIGHT,
            None,
        ));
    }
    if features.crown {
        // Mega gets a crown band and a gem: the last stage has to *read* as a
        // last stage, not just as one more part on the forehead.
        parts.push(Part::ellipse(
            "crown_band",
            &pal.glow,
            [round4(w * 0.72), 9.0],
            [0.0, round4(-h * 0.52)],
            Z_HEIGHT + 2,
            None,
        ));
        parts.push(Part::circle(
            "crown_gem",
            &pal.accent,
            round4(w * 0.12),
            [0.0, round4(-h * 0.52)],
            Z_HEIGHT + 3,
            None,
        ));
    }
    if features.cheek_plates {
        for (id, sign) in [("cheek_l", -1.0), ("cheek_r", 1.0)] {
            parts.push(Part::ellipse(
                id,
                &pal.accent,
                [round4(w * 0.16), round4(h * 0.3)],
                [round4(sign * w * 0.44), round4(h * 0.12)],
                Z_HEIGHT + 1,
                None,
            ));
        }
    }
    // The form's own skull shape: the muzzle group stretches along the face's
    // forward axis, so a long-jawed variant and a pug-faced one are the same
    // archetype with different bones, not different colours. Eyes, brows and
    // the split mouth are deliberately left alone (they sit on the skull, and
    // the mouth halves must stay at ±7 px for the expression contract).
    stretch_snout(&mut parts, plan.snout);
    parts
}

/// The parts that count as "the muzzle": stretched by `plan.snout` along the
/// head's forward axis, which is `+y` in the bone's local frame.
const SNOUT_PARTS: [&str; 9] = [
    "muzzle",
    "jaw",
    "nose",
    "beak_upper",
    "beak_lower",
    "nostril_l",
    "nostril_r",
    "fang_l",
    "fang_r",
];

fn stretch_snout(parts: &mut [Part], factor: f64) {
    if (factor - 1.0).abs() < 1e-9 {
        return;
    }
    for part in parts
        .iter_mut()
        .filter(|part| SNOUT_PARTS.contains(&part.id.as_str()))
    {
        part.size[0] = round4(part.size[0] * factor);
        part.offset[1] = round4(part.offset[1] * factor);
    }
}

/// The folded wing a winged plan wears before it can fly.
fn push_wing_bud(bones: &mut Vec<Bone>, pal: &Palette, dims: Dims, plan: Plan) {
    let span = dims.wing * 0.34;
    bones.push(
        Bone::new(
            "wing_r",
            Some("torso"),
            [round4(dims.torso_w * 0.4), round4(-dims.torso_h * 0.42)],
            Z_WING,
        )
        .rotated(round4(-0.35))
        .length(round4(span))
        .anim(Anim::sway(0.02, 0.0))
        .with_parts(vec![
            Part::ellipse(
                "wing_bud",
                &pal.body_dark,
                [round4(span * 1.3), round4(span * 0.9)],
                [round4(span * 0.3), 0.0],
                0,
                None,
            ),
            Part::ellipse(
                "wing_bud_tip",
                &pal.accent,
                [round4(span * 0.7), round4(span * 0.4)],
                [round4(span * 0.9), 0.0],
                1,
                None,
            ),
        ]),
    );
    if plan.feathers {
        bones.last_mut().unwrap().parts.push(Part::ellipse(
            "bud_feather",
            &pal.glow,
            [round4(span * 0.8), 6.0],
            [round4(span * 0.5), 0.0],
            2,
            None,
        ));
    }
}

/// The share of an attribute above which its *extra* detail (a second layer of
/// spikes, a second circuit run, a core crystal, an inner halo) is worth the
/// parts budget.
///
/// Proportional prominence means a faint contributor still gets its silhouette,
/// just not its ornament — and a genuinely minor share (40% against a 60%
/// dominant one) loses that ornament first, which is what keeps the dominant
/// attribute's identity intact when a rig does run into the cap.
const FULL_DETAIL: f64 = 0.9;

/// What the **attribute mix** puts on the trunk: crystals, circuit traces,
/// spines, markings. Shared by every plan, so a Virus deer and a Virus robot are
/// both unmistakably viral while staying a deer and a robot.
///
/// Every part an attribute owns is scaled by that attribute's prominence in the
/// lineage. A Data body with a recent Virus shift therefore keeps full-size
/// crystal growths *and* grows visibly prominent spines, with the older/quieter
/// contributor smaller and losing its optional detail first.
fn aspect_torso_parts(
    pal: &Palette,
    dims: Dims,
    plan: Plan,
    features: Features,
    blend: AttributeBlend,
) -> Vec<Part> {
    let w = dims.torso_w;
    let h = dims.torso_h;
    let t = features.tier_index;
    let data = blend.prominence(Attribute::Data);
    let virus = blend.prominence(Attribute::Virus);
    let mut parts = Vec::new();

    if plan.crystals {
        // Crystal growths break the body's ellipse contour — that is the point.
        for (id, sign) in [("crystal_l", -1.0), ("crystal_r", 1.0)] {
            parts.push(
                Part::ellipse(
                    id,
                    &pal.accent,
                    [round4(w * 0.3 * data), round4(h * 0.16 * data)],
                    [round4(sign * w * 0.42), round4(-h * 0.34)],
                    6,
                    None,
                )
                .turned(round4(sign * 0.7)),
            );
        }
        if t >= 3 && data >= FULL_DETAIL {
            parts.push(Part::ellipse(
                "crystal_core",
                &pal.glow,
                [round4(w * 0.16 * data), round4(h * 0.12 * data)],
                [0.0, round4(-h * 0.44)],
                7,
                None,
            ));
        }
    }
    if plan.circuit && t >= 3 {
        for (index, sign) in [(-1.0, -0.34), (1.0, 0.34)] {
            parts.push(Part::ellipse(
                if index < 0.0 { "circuit_l" } else { "circuit_r" },
                &pal.glow,
                [round4(w * 0.5 * data), round4(3.0 * data).max(1.0)],
                [0.0, round4(sign * h)],
                5,
                None,
            ));
        }
        // A dominant Data line runs a second pair of traces across the belly:
        // part *count*, not just size, follows the share.
        if data >= FULL_DETAIL {
            for sign in [-0.12f64, 0.12] {
                parts.push(Part::ellipse(
                    if sign < 0.0 { "circuit2_l" } else { "circuit2_r" },
                    &pal.accent,
                    [round4(w * 0.34 * data), 2.0],
                    [0.0, round4(sign * h)],
                    4,
                    None,
                ));
            }
        }
    }
    if plan.back_spikes && t >= 1 {
        for (id, sign) in [("spine_l", -1.0), ("spine_r", 1.0)] {
            parts.push(
                Part::ellipse(
                    id,
                    &pal.accent,
                    [round4(w * 0.22 * virus), round4(h * 0.1 * virus)],
                    [round4(sign * w * 0.3), round4(-h * 0.5)],
                    6,
                    None,
                )
                .turned(round4(sign * -0.8)),
            );
        }
        if t >= 3 && virus >= FULL_DETAIL {
            parts.push(
                Part::ellipse(
                    "spine_mid",
                    &pal.accent,
                    [round4(w * 0.18 * virus), round4(h * 0.08 * virus)],
                    [0.0, round4(-h * 0.58)],
                    6,
                    None,
                )
                .turned(-std::f64::consts::FRAC_PI_2),
            );
        }
    }
    // Seeded markings: the third layer of variety, after archetype and build.
    match plan.marking {
        1 => {
            for index in 0..3 {
                parts.push(Part::circle(
                    "spot",
                    &pal.glow,
                    round4(w * (0.07 + index as f64 * 0.01)),
                    [
                        round4(w * (-0.26 + index as f64 * 0.26)),
                        round4(h * (-0.22 + index as f64 * 0.2)),
                    ],
                    5,
                    None,
                ));
            }
        }
        2 => {
            for index in 0..2 {
                parts.push(Part::ellipse(
                    "stripe",
                    &pal.body_dark,
                    [round4(w * 0.62), 6.0],
                    [0.0, round4(-h * 0.3 + index as f64 * h * 0.34)],
                    4,
                    None,
                ));
            }
        }
        3 => {
            // Patches: two shoulder blotches and one belly, so it reads as a
            // pattern rather than as a stripe with an offset.
            for (id, sign) in [("patch_l", -1.0), ("patch_r", 1.0)] {
                parts.push(Part::ellipse(
                    id,
                    &pal.accent,
                    [round4(w * 0.3), round4(h * 0.24)],
                    [round4(sign * w * 0.4), round4(-h * 0.3)],
                    5,
                    None,
                ));
            }
            parts.push(Part::ellipse(
                "patch",
                &pal.glow,
                [round4(w * 0.5), round4(h * 0.3)],
                [0.0, round4(h * 0.2)],
                4,
                None,
            ));
        }
        _ => {}
    }
    parts
}

/// What the attribute *mix* puts on the head: a ring of light, fangs, drips,
/// each scaled and gated by its share of the lineage.
fn aspect_head_parts(
    pal: &Palette,
    dims: Dims,
    plan: Plan,
    features: Features,
    blend: AttributeBlend,
) -> Vec<Part> {
    let w = dims.head_w;
    let h = dims.head_h;
    let t = features.tier_index;
    let vaccine = blend.prominence(Attribute::Vaccine);
    let data = blend.prominence(Attribute::Data);
    let virus = blend.prominence(Attribute::Virus);
    let mut parts = Vec::new();

    if plan.halo_ring {
        parts.push(Part::ellipse(
            "halo",
            &pal.glow,
            [round4(w * 0.86 * vaccine), round4(16.0 * vaccine).max(2.0)],
            [0.0, round4(-h * 0.86)],
            Z_HEIGHT,
            None,
        ));
        // The inner disc is what makes the halo read as a ring rather than a
        // glowing plate, so it follows the halo itself (scaled), not the share.
        parts.push(Part::ellipse(
            "halo_in",
            &pal.body_dark,
            [round4(w * 0.6 * vaccine), round4(8.0 * vaccine).max(1.0)],
            [0.0, round4(-h * 0.86)],
            Z_HEIGHT + 1,
            None,
        ));
    }
    if plan.fangs {
        for (id, sign) in [("tooth_l", -1.0), ("tooth_r", 1.0)] {
            parts.push(Part::ellipse(
                id,
                &pal.eye,
                [round4(w * 0.05 * virus).max(0.5), round4(h * 0.1 * virus).max(0.5)],
                [round4(sign * w * 0.14), round4(h * 0.46)],
                9,
                None,
            ));
        }
        // A dominant viral line grows a second, smaller pair behind the first.
        if virus >= FULL_DETAIL {
            for (id, sign) in [("tooth_more_l", -1.0), ("tooth_more_r", 1.0)] {
                parts.push(Part::ellipse(
                    id,
                    &pal.eye,
                    [round4(w * 0.035), round4(h * 0.07)],
                    [round4(sign * w * 0.26), round4(h * 0.42)],
                    9,
                    None,
                ));
            }
        }
    }
    if plan.crystals && t >= 3 && data >= FULL_DETAIL {
        for (id, sign) in [("bit_l", -1.0), ("bit_r", 1.0)] {
            parts.push(
                Part::ellipse(
                    id,
                    &pal.glow,
                    [round4(w * 0.14 * data), round4(h * 0.14 * data)],
                    [round4(sign * w * 0.66), round4(-h * 0.16)],
                    Z_HEIGHT,
                    None,
                )
                .turned(round4(0.35)),
            );
        }
    }
    if plan.drips && t >= 3 {
        for (id, sign) in [("drip_l", -1.0), ("drip_r", 1.0)] {
            parts.push(Part::circle(
                id,
                &pal.glow,
                round4(w * 0.05 * virus).max(0.5),
                [round4(sign * w * 0.3), round4(h * 0.5)],
                9,
                None,
            ));
        }
    }
    parts
}

/// Ears (or gills, or antennae) — a mirrored pair of bones hung off the skull so
/// they can sway with the head rather than being welded to it.
fn push_ear(
    bones: &mut Vec<Bone>,
    pal: &Palette,
    dims: Dims,
    features: Features,
    plan: Plan,
    rng: &mut Rng,
) {
    let w = dims.head_w;
    let h = dims.head_h;
    let (id, angle, length, parts) = match plan.ears {
        EarKind::None => return,
        EarKind::Pointed => (
            "ear_r",
            rng.range(-0.95, -0.7),
            22.0 + w * 0.1,
            vec![Part::ellipse(
                "ear",
                &pal.body_dark,
                [round4(24.0 + w * 0.1), 20.0],
                [0.0, 0.0],
                0,
                None,
            )],
        ),
        EarKind::Long => (
            "ear_r",
            rng.range(-1.15, -0.95),
            46.0 + w * 0.34,
            vec![
                Part::ellipse(
                    "ear",
                    &pal.body_dark,
                    [round4(46.0 + w * 0.34), 22.0],
                    [0.0, 0.0],
                    0,
                    None,
                ),
                Part::ellipse(
                    "ear_inner",
                    &pal.body,
                    [round4(34.0 + w * 0.24), 12.0],
                    [0.0, 0.0],
                    1,
                    None,
                ),
            ],
        ),
        EarKind::Gill => (
            "ear_r",
            rng.range(-0.5, -0.28),
            30.0,
            vec![
                Part::ellipse("gill_a", &pal.accent, [34.0, 12.0], [0.0, 0.0], 0, None),
                Part::ellipse("gill_b", &pal.accent, [30.0, 10.0], [2.0, 10.0], 0, None),
                Part::ellipse("gill_c", &pal.accent, [24.0, 8.0], [4.0, 19.0], 0, None),
            ],
        ),
        EarKind::Antenna => (
            "ear_r",
            rng.range(-1.5, -1.2),
            30.0,
            vec![
                Part::capsule("ear_rod", &pal.metal, [30.0, 6.0], [0.0, 0.0], 0),
                Part::circle("ear_bulb", &pal.glow, 11.0, [28.0, 0.0], 1, None),
            ],
        ),
        EarKind::Stalk => (
            "ear_r",
            rng.range(-1.25, -1.0),
            40.0,
            vec![
                Part::capsule("ear_rod", &pal.body, [40.0, 9.0], [0.0, 0.0], 0),
                Part::circle("stalk_orb", &pal.glow, 19.0, [38.0, 0.0], 1, None),
                Part::circle("stalk_pupil", &pal.pupil, 8.0, [40.0, 0.0], 2, None),
            ],
        ),
        EarKind::Disc => (
            "ear_r",
            rng.range(-0.55, -0.35),
            26.0,
            vec![
                Part::circle("ear_disc", &pal.body, round4(40.0), [0.0, 0.0], 0, None),
                Part::circle("ear_disc_in", &pal.body_dark, round4(24.0), [0.0, 0.0], 1, None),
            ],
        ),
    };
    let mut bone = Bone::new(
        id,
        Some("head"),
        [round4(w * 0.3), round4(-h * 0.22)],
        Z_HORN - 1,
    )
    .rotated(round4(angle + plan.ear_angle))
    .length(round4(length * plan.ear_scale))
    .anim(Anim::sway(0.03, 0.4))
    .with_parts(parts);
    if features.ear_tip && plan.ears == EarKind::Long {
        // Long ears get a drooping tip so they are not two straight leaves.
        bone = bone.with_parts(vec![
            Part::ellipse(
                "ear",
                &pal.body_dark,
                [round4(46.0 + w * 0.34), 22.0],
                [0.0, 0.0],
                0,
                None,
            ),
            Part::ellipse(
                "ear_inner",
                &pal.body,
                [round4(34.0 + w * 0.24), 12.0],
                [0.0, 0.0],
                1,
                None,
            ),
            // The tip rides the *ellipse's* far edge, not the bone's `length`:
            // the ear ellipse is centred on the joint, so `length - 4` left the
            // droop hanging ~30 px past the artwork (a floating dot).
            Part::ellipse(
                "ear_tip",
                &pal.body_dark,
                [round4(20.0), 14.0],
                [round4((46.0 + w * 0.34) * 0.5 - 2.0), round4(-4.0)],
                2,
                None,
            ),
        ]);
    }
    scale_parts(&mut bone.parts, plan.ear_scale);
    bones.push(bone);
}

/// The crest: horns, antlers, a blade, antennae, a spine fan.
fn push_crest(
    bones: &mut Vec<Bone>,
    pal: &Palette,
    dims: Dims,
    features: Features,
    plan: Plan,
    rng: &mut Rng,
) {
    let w = dims.head_w;
    let h = dims.head_h;
    let base = [round4(w * 0.26), round4(-h * 0.3)];
    let t = features.tier_index;
    match plan.crest {
        CrestKind::None => {}
        CrestKind::Horns => {
            let angle = round4(rng.range(-1.15, -0.85));
            let length = round4((28.0 + w * 0.12) * (0.62 + t as f64 * 0.09));
            let mut parts = vec![Part::capsule("horn", &pal.accent, [length, 13.0], [0.0, 0.0], 0)];
            if features.horns_tip {
                parts.push(Part::circle(
                    "horn_tip",
                    &pal.glow,
                    11.0,
                    [round4(length - 4.0), 0.0],
                    1,
                    None,
                ));
            }
            bones.push(
                Bone::new("horn_r", Some("head"), base, Z_HORN)
                    .rotated(angle)
                    .length(length)
                    .with_parts(parts),
            );
        }
        CrestKind::Antlers => {
            // Two segments and up to three tines: a branch, not a spike.
            let angle = round4(rng.range(-1.35, -1.1));
            let length = round4(26.0 + w * 0.16);
            let mut parts = vec![Part::capsule("antler", &pal.body_dark, [length, 13.0], [0.0, 0.0], 0)];
            if features.crest_branch {
                parts.push(
                    Part::capsule(
                        "antler_tine",
                        &pal.body_dark,
                        [round4(20.0), 9.0],
                        [round4(length * 0.62), round4(-4.0)],
                        1,
                    )
                    .turned(round4(-0.55)),
                );
            }
            if features.crest_extra {
                parts.push(
                    Part::capsule(
                        "antler_tine2",
                        &pal.body_dark,
                        [round4(16.0), 8.0],
                        [round4(length * 0.34), round4(-2.0)],
                        1,
                    )
                    .turned(round4(-0.9)),
                );
                parts.push(Part::circle(
                    "antler_bead",
                    &pal.accent,
                    round4(9.0),
                    [round4(length * 0.9), round4(-4.0)],
                    2,
                    None,
                ));
            }
            bones.push(
                Bone::new("antler_r", Some("head"), base, Z_HORN)
                    .rotated(angle)
                    .length(length)
                    .with_parts(parts),
            );
            if features.crest_branch {
                bones.push(
                    Bone::new(
                        "antler_tip_r",
                        Some("antler_r"),
                        [round4(length - 4.0), 0.0],
                        Z_HORN,
                    )
                    .rotated(round4(rng.range(0.45, 0.6)))
                    .length(round4(length * 0.6))
                    .with_parts(vec![Part::capsule(
                        "antler_tip",
                        &pal.body_dark,
                        [round4(length * 0.6), 7.0],
                        [0.0, 0.0],
                        0,
                    )]),
                );
            }
        }
        CrestKind::Blade => {
            let length = round4(34.0 + w * 0.18);
            let mut parts = vec![Part::ellipse(
                "blade",
                &pal.accent,
                [length, 16.0],
                [round4(length * 0.5), 0.0],
                0,
                None,
            )];
            if features.crest_branch {
                parts.push(Part::ellipse(
                    "blade_edge",
                    &pal.glow,
                    [round4(length * 0.7), 6.0],
                    [round4(length * 0.5), 0.0],
                    1,
                    None,
                ));
            }
            bones.push(
                Bone::new("crest_r", Some("head"), base, Z_HORN)
                    .rotated(round4(-1.0))
                    .length(length)
                    .with_parts(parts),
            );
        }
        CrestKind::Antenna => {
            let length = round4(34.0 + w * 0.16);
            let mut parts = vec![Part::capsule("antenna", &pal.metal, [length, 6.0], [0.0, 0.0], 0)];
            parts.push(Part::circle(
                "antenna_light",
                &pal.accent,
                10.0,
                [round4(length - 3.0), 0.0],
                1,
                None,
            ));
            bones.push(
                Bone::new("crest_r", Some("head"), base, Z_HORN)
                    .rotated(round4(-1.45))
                    .length(length)
                    .anim(Anim::sway(0.04, 0.2))
                    .with_parts(parts),
            );
        }
        CrestKind::Spikes => {
            let length = round4(26.0 + w * 0.14);
            let mut parts = Vec::new();
            for (index, angle) in [-0.5, -0.1, 0.35].iter().enumerate() {
                parts.push(
                    Part::capsule(
                        if index == 0 { "spike" } else { "spike_extra" },
                        &pal.accent,
                        [round4(length * (1.0 - index as f64 * 0.18)), 10.0],
                        [round4(index as f64 * 9.0), round4(index as f64 * 5.0)],
                        0,
                    )
                    .turned(round4(*angle)),
                );
            }
            bones.push(
                Bone::new("crest_r", Some("head"), base, Z_HORN)
                    .rotated(round4(-1.2))
                    .length(length)
                    .with_parts(parts),
            );
        }
        CrestKind::Comb => {
            let length = round4(24.0 + w * 0.22);
            let mut parts = Vec::new();
            for (index, tilt) in [-0.5, -0.12, 0.26].iter().enumerate() {
                parts.push(
                    Part::ellipse(
                        if index == 1 { "comb" } else { "comb_more" },
                        &pal.accent,
                        [round4(length), round4(9.0)],
                        [round4(index as f64 * 4.0), round4(index as f64 * 3.0)],
                        0,
                        None,
                    )
                    .turned(round4(*tilt)),
                );
            }
            bones.push(
                Bone::new("comb_r", Some("head"), base, Z_HORN)
                    .rotated(round4(-1.5))
                    .length(length)
                    .anim(Anim::sway(0.03, 0.3))
                    .with_parts(parts),
            );
        }
        // Drawn as a head part, not a bone: a centre fin must not be mirrored.
        CrestKind::Fin => {}
    }
}

/// The tail — five shapes, one rule: it leaves the **rump** and sweeps **up**,
/// so it never hangs in the gap between the legs (which is what made the first
/// version read badly, whatever the tail itself was shaped like), and it is
/// never the same capsule primitive the arms and legs use.
fn push_tail(
    bones: &mut Vec<Bone>,
    pal: &Palette,
    dims: Dims,
    features: Features,
    plan: Plan,
    rng: &mut Rng,
) {
    let segment_lengths = [1.0, 0.82, 0.66];
    let mut parent = "pelvis".to_string();
    let mut previous = dims.tail;
    // A stub is a stub: one short segment tucked against the rump, not a chain
    // that wanders off to the left.
    let segments = if plan.tail == TailKind::Stub {
        1
    } else {
        features.tail_segments
    };

    for index in 0..segments {
        let id = if index == 0 {
            "tail".to_string()
        } else {
            format!("tail{}", index + 1)
        };
        let length = if plan.tail == TailKind::Stub {
            round4(dims.tail * 0.42)
        } else {
            round4(dims.tail * segment_lengths[index.min(2)])
        };
        let girth = round4((26.0 - index as f64 * 5.0).max(10.0));
        let last = index + 1 == segments;
        let (pos, rotation) = if index == 0 {
            // Up and back (≈186–197°) from the top of the rump.
            ([0.0, -4.0], round4(rng.range(3.24, 3.44) + plan.tail_arc))
        } else {
            (
                [round4(previous - girth * 0.35), 0.0],
                // Each joint lifts the chain, so the tip turns over.
                round4(rng.range(0.28, 0.38) - index as f64 * 0.06),
            )
        };
        let mut bone = Bone::new(&id, Some(&parent), pos, Z_TAIL)
            .rotated(rotation)
            .length(length)
            .anim(Anim::sway(0.02, 0.3 + index as f64 * 0.35));

        let mut parts: Vec<Part> = match plan.tail {
            TailKind::BlobFin | TailKind::Fan | TailKind::Tentacle => vec![Part::ellipse(
                "tail_blob",
                &pal.body_dark,
                [round4(length * 1.05), girth],
                [round4(length * 0.5), 0.0],
                0,
                None,
            )],
            TailKind::Cable => vec![
                Part::capsule("tail_cable", &pal.metal, [round4(length), 9.0], [0.0, 0.0], 0),
                Part::ellipse(
                    "tail_ring",
                    &pal.accent,
                    [8.0, 18.0],
                    [round4(length * 0.45), 0.0],
                    1,
                    None,
                ),
            ],
            TailKind::Stub => vec![Part::ellipse(
                "tail_tuft",
                &pal.body_dark,
                [round4(length * 0.9), round4(girth * 1.2)],
                [round4(length * 0.35), 0.0],
                0,
                None,
            )],
        };

        if last {
            match plan.tail {
                TailKind::BlobFin => {
                    parts.push(Part::ellipse(
                        "tail_fin",
                        &pal.accent,
                        [round4(girth * 0.75), round4(girth * 2.2)],
                        [round4(length - girth * 0.55), 0.0],
                        1,
                        None,
                    ));
                    parts.push(Part::ellipse(
                        "tail_fin_web",
                        &pal.glow,
                        [round4(girth * 0.5), round4(girth * 1.2)],
                        [round4(length - girth * 0.1), 0.0],
                        2,
                        None,
                    ));
                }
                TailKind::Fan => {
                    // Three flat vanes in a fan, the way a fish tail opens out.
                    // A fan wide enough to be seen past the hips: three vanes
                    // around the chain's own end, not three slivers on it.
                    for (vane, (id, tilt)) in [
                        ("tail_vane_up", -0.62),
                        ("tail_vane_mid", 0.0),
                        ("tail_vane_dn", 0.62),
                    ]
                    .iter()
                    .enumerate()
                    {
                        parts.push(
                            Part::ellipse(
                                id,
                                &pal.accent,
                                [round4(length * 1.5), round4(16.0)],
                                [
                                    round4(length * 1.05),
                                    round4(*tilt * length * 0.5),
                                ],
                                1 + vane as i32,
                                None,
                            )
                            .turned(round4(*tilt)),
                        );
                    }
                    parts.push(Part::ellipse(
                        "tail_fan_web",
                        &pal.glow,
                        [round4(length * 0.7), round4(30.0)],
                        [round4(length * 1.1), 0.0],
                        4,
                        None,
                    ));
                }
                TailKind::Stub => {
                    parts.push(Part::ellipse(
                        "tail_tip",
                        &pal.accent,
                        [round4(length * 0.5), round4(girth * 0.9)],
                        [round4(length * 0.8), 0.0],
                        2,
                        None,
                    ));
                }
                TailKind::Cable => {
                    parts.push(Part::ellipse(
                        "tail_plug",
                        &pal.body_dark,
                        [round4(length * 0.4), 20.0],
                        [round4(length), 0.0],
                        2,
                        None,
                    ));
                }
                TailKind::Tentacle => {
                    parts.push(Part::circle(
                        "tail_tip",
                        &pal.glow,
                        round4(girth * 1.1),
                        [round4(length - girth * 0.2), 0.0],
                        1,
                        None,
                    ));
                }
            }
        } else if plan.tail == TailKind::Tentacle && features.tail_web {
            parts.push(Part::ellipse(
                "tail_barb",
                &pal.accent,
                [round4(12.0), round4(6.0)],
                [round4(length * 0.8), round4(-girth * 0.6)],
                1,
                None,
            ));
        }

        if plan.accessory == Accessory::TailBlades || plan.accessory2 == Accessory::TailBlades {
            // The blade rides every segment, so a longer tail grows more of it.
            parts.push(
                Part::ellipse(
                    "tail_blade",
                    &pal.accent,
                    [round4(length * 0.6), round4(girth * 0.8)],
                    [round4(length * 0.55), round4(-girth * 0.7)],
                    2,
                    None,
                )
                .turned(-0.5),
            );
        }

        bone = bone.with_parts(parts);
        bones.push(bone);
        parent = id;
        previous = length;
    }
}

fn push_arm(
    bones: &mut Vec<Bone>,
    pal: &Palette,
    dims: Dims,
    features: Features,
    plan: Plan,
    rng: &mut Rng,
) {
    let shoulder = [round4(dims.torso_w * 0.5 - 2.0), round4(-dims.torso_h * 0.3)];
    match plan.arms {
        ArmKind::Tentacle => {
            // Three thin segments that curl inward, no shoulder cap: a tendril,
            // not an arm with flippers on the end.
            let lengths = [
                round4(dims.upper_arm * 1.05),
                round4(dims.forearm * 0.95),
                round4(dims.forearm * 0.8),
            ];
            let girths = [11.0, 9.0, 7.0];
            let mut parent = "torso".to_string();
            let mut pos = shoulder;
            let segments = if features.arm_third { 3 } else { 2 };
            for index in 0..segments {
                let id = if index == 0 {
                    "arm_r".to_string()
                } else {
                    format!("arm{}_r", index + 1)
                };
                let angle = if index == 0 {
                    round4(rng.range(0.95, 1.15))
                } else {
                    round4(rng.range(-0.42, -0.22))
                };
                let mut parts = vec![Part::ellipse(
                    if index == 0 { "arm_seg" } else { "arm_seg2" },
                    &pal.body,
                    [round4(lengths[index] * 1.05), girths[index]],
                    [round4(lengths[index] * 0.5), 0.0],
                    0,
                    None,
                )];
                if index + 1 == segments {
                    parts.push(Part::circle(
                        "arm_sucker",
                        &pal.glow,
                        round4(girths[index] * 1.2),
                        [round4(lengths[index] * 0.9), 0.0],
                        1,
                        None,
                    ));
                }
                bones.push(
                    Bone::new(&id, Some(&parent), pos, Z_ARM)
                        .rotated(angle)
                        .length(lengths[index])
                        .anim(Anim::walk(0.3 - index as f64 * 0.06, 0.0))
                        .with_parts(parts),
                );
                parent = id;
                pos = [round4(lengths[index] - girths[index] * 0.4), 0.0];
            }
        }
        ArmKind::Wing => {
            // A folded wing worn as an arm: shoulder, panel, feather tips.
            let span = round4(dims.upper_arm * 1.5);
            let mut parts = vec![
                Part::ellipse(
                    "wing_shoulder",
                    &pal.body_dark,
                    [round4(dims.torso_w * 0.26), round4(dims.torso_w * 0.24)],
                    [0.0, 0.0],
                    0,
                    None,
                ),
                Part::ellipse(
                    "arm_wing",
                    &pal.body,
                    [round4(span * 1.1), round4(span * 0.5)],
                    [round4(span * 0.5), 0.0],
                    1,
                    None,
                ),
            ];
            for (index, tilt) in [-0.3, 0.05, 0.4].iter().enumerate() {
                parts.push(
                    Part::ellipse(
                        if index == 0 { "wing_feather" } else { "wing_feather_more" },
                        &pal.accent,
                        [round4(span * 0.7), round4(span * 0.2)],
                        [round4(span * 0.9), round4(*tilt * span * 0.28)],
                        2,
                        None,
                    )
                    .turned(round4(*tilt)),
                );
            }
            bones.push(
                Bone::new("arm_r", Some("torso"), shoulder, Z_ARM)
                    .rotated(round4(rng.range(1.05, 1.25)))
                    .length(span)
                    .anim(Anim::walk(0.3, 0.0))
                    .with_parts(parts),
            );
        }
        ArmKind::Flipper => {
            let len = round4(dims.upper_arm * 0.9);
            let mut parts = vec![
                Part::ellipse(
                    "shoulder",
                    &pal.body_dark,
                    [round4(dims.torso_w * 0.24), round4(dims.torso_w * 0.22)],
                    [0.0, 0.0],
                    0,
                    None,
                ),
                // Wide and flat, hinged at the shoulder: a paddle.
                Part::ellipse(
                    "flipper",
                    &pal.body,
                    [round4(len * 1.5), round4(len * 0.9)],
                    [round4(len * 0.7), 0.0],
                    1,
                    None,
                ),
                Part::ellipse(
                    "flipper_ray",
                    &pal.accent,
                    [round4(len * 0.9), 5.0],
                    [round4(len * 0.8), 0.0],
                    2,
                    None,
                ),
            ];
            if features.forearm {
                parts.push(Part::ellipse(
                    "flipper_tip",
                    &pal.body,
                    [round4(len * 0.7), round4(len * 0.4)],
                    [round4(len * 1.25), round4(-len * 0.1)],
                    1,
                    None,
                ));
            }
            bones.push(
                Bone::new("arm_r", Some("torso"), shoulder, Z_ARM)
                    .rotated(round4(rng.range(1.15, 1.35)))
                    .length(len)
                    .anim(Anim::walk(0.34, 0.0))
                    .with_parts(parts),
            );
        }
        ArmKind::Cannon => {
            let len = round4(dims.upper_arm);
            let mut parts = vec![
                Part::circle(
                    "shoulder_ball",
                    &pal.metal,
                    round4(dims.torso_w * 0.26),
                    [0.0, 0.0],
                    0,
                    None,
                ),
                Part::capsule("piston", &pal.body_dark, [len, 17.0], [0.0, 0.0], 0),
            ];
            if features.armor {
                parts.push(Part::ellipse(
                    "pauldron",
                    &pal.metal,
                    [round4(dims.torso_w * 0.32), round4(dims.torso_w * 0.22)],
                    [0.0, -4.0],
                    1,
                    None,
                ));
            }
            bones.push(
                Bone::new("arm_r", Some("torso"), shoulder, Z_ARM)
                    .rotated(round4(rng.range(0.95, 1.15)))
                    .length(len)
                    .anim(Anim::walk(0.38, 0.0))
                    .with_parts(parts),
            );
            let mut parent = "arm_r".to_string();
            let mut joint = round4(len - 6.0);
            if features.forearm {
                // The barrel: a fat capsule with a glowing muzzle ring.
                let barrel = round4(dims.forearm * 1.05);
                bones.push(
                    Bone::new("forearm_r", Some(&parent), [joint, 0.0], Z_ARM)
                        .rotated(round4(rng.range(0.08, 0.2)))
                        .length(barrel)
                        .anim(Anim::walk(0.2, 0.0))
                        .with_parts(vec![
                            Part::capsule("barrel", &pal.metal, [barrel, 20.0], [0.0, 0.0], 0),
                            Part::ellipse(
                                "barrel_ring",
                                &pal.accent,
                                [10.0, 24.0],
                                [round4(barrel - 6.0), 0.0],
                                1,
                                None,
                            ),
                        ]),
                );
                parent = "forearm_r".to_string();
                joint = round4(barrel - 6.0);
                if features.hand {
                    bones.push(
                        Bone::new("hand_r", Some(&parent), [joint, 0.0], Z_HAND)
                            .anim(Anim::walk(0.12, 0.0))
                            .with_parts(vec![Part::circle(
                                "muzzle_light",
                                &pal.glow,
                                round4(14.0),
                                [round4(6.0), 0.0],
                                0,
                                None,
                            )]),
                    );
                }
            }
        }
        ArmKind::Claw | ArmKind::Hand => {
            let shoulder_angle = round4(rng.range(0.95, 1.15));
            let mut arm = Bone::new("arm_r", Some("torso"), shoulder, Z_ARM)
                .rotated(shoulder_angle)
                .length(round4(dims.upper_arm))
                .anim(Anim::walk(0.38, 0.0));
            let mut parts = vec![
                Part::ellipse(
                    "shoulder",
                    &pal.body_dark,
                    [round4(dims.torso_w * 0.28), round4(dims.torso_w * 0.26)],
                    [0.0, 0.0],
                    0,
                    None,
                ),
                Part::capsule("upper_arm", &pal.body, [round4(dims.upper_arm), 15.0], [0.0, 0.0], 1),
            ];
            if features.armor {
                parts.push(Part::ellipse(
                    "pauldron",
                    &pal.metal,
                    [round4(dims.torso_w * 0.3), round4(dims.torso_w * 0.2)],
                    [0.0, -4.0],
                    2,
                    None,
                ));
            }
            arm = arm.with_parts(parts);
            bones.push(arm);

            let mut parent = "arm_r".to_string();
            let mut joint = round4(dims.upper_arm - 6.0);
            if features.forearm {
                let mut parts = vec![Part::capsule(
                    "forearm",
                    &pal.body,
                    [round4(dims.forearm), 13.0],
                    [0.0, 0.0],
                    0,
                )];
                if features.armor {
                    parts.push(Part::ellipse(
                        "bracer",
                        &pal.accent,
                        [round4(dims.forearm * 0.5), 18.0],
                        [round4(dims.forearm * 0.45), 0.0],
                        1,
                        None,
                    ));
                }
                bones.push(
                    Bone::new("forearm_r", Some(&parent), [joint, 0.0], Z_ARM)
                        .rotated(round4(rng.range(0.1, 0.25)))
                        .length(round4(dims.forearm))
                        .anim(Anim::walk(0.22, 0.0))
                        .with_parts(parts),
                );
                parent = "forearm_r".to_string();
                joint = round4(dims.forearm - 6.0);

                if features.hand {
                    let mut parts = vec![Part::ellipse(
                        "hand",
                        &pal.body_dark,
                        [17.0, 17.0],
                        [round4(6.0), 0.0],
                        0,
                        None,
                    )];
                    if plan.arms == ArmKind::Claw {
                        // Two claws, so a beast's hand is a claw and a hand.
                        for (index, (id, tilt)) in
                            [("claw", -0.35), ("claw_extra", 0.35)].iter().enumerate()
                        {
                            parts.push(
                                Part::ellipse(
                                    id,
                                    &pal.accent,
                                    [14.0, 6.0],
                                    [round4(14.0), round4(*tilt * 6.0)],
                                    1,
                                    None,
                                )
                                .turned(round4(*tilt)),
                            );
                            let _ = index;
                        }
                    }
                    bones.push(
                        Bone::new("hand_r", Some(&parent), [joint, 0.0], Z_HAND)
                            .anim(Anim::walk(0.12, 0.0))
                            .with_parts(parts),
                    );
                }
            }
        }
    }
}

fn push_leg(
    bones: &mut Vec<Bone>,
    pal: &Palette,
    dims: Dims,
    features: Features,
    plan: Plan,
    rng: &mut Rng,
) {
    match plan.legs {
        LegKind::Hover => {
            // No legs at all: the body floats, so the pelvis carries the hover
            // skirt instead (added in `generate`).
        }
        LegKind::Treads => {
            let hip = [round4(dims.torso_w * 0.3), 6.0];
            let mut parts = vec![
                Part::ellipse(
                    "tread_block",
                    &pal.body_dark,
                    [round4(dims.thigh * 2.0), round4(dims.shin * 0.9)],
                    [0.0, round4(dims.shin * 0.5)],
                    0,
                    None,
                ),
                // Rollers along the bottom edge: the reason it reads as a tread.
                // Small and dark: full-size pale circles read as beads strung
                // along the bottom rather than as a tread.
                Part::circle("roller_a", &pal.body_dark, 9.0, [round4(-dims.thigh * 0.6), round4(dims.shin * 0.85)], 1, None),
                Part::circle("roller_b", &pal.metal, 9.0, [0.0, round4(dims.shin * 0.85)], 1, None),
                Part::circle("roller_c", &pal.body_dark, 9.0, [round4(dims.thigh * 0.6), round4(dims.shin * 0.85)], 1, None),
            ];
            if features.armor {
                parts.push(Part::ellipse(
                    "tread_plate",
                    &pal.metal,
                    [round4(dims.thigh * 1.4), 10.0],
                    [0.0, round4(dims.shin * 0.2)],
                    2,
                    None,
                ));
            }
            bones.push(
                Bone::new("leg_r", Some("pelvis"), hip, Z_LEG)
                    .length(round4(dims.thigh * 2.0))
                    .with_parts(parts),
            );
        }
        LegKind::Hoofed => {
            let hip = [round4(dims.torso_w * 0.24), 6.0];
            bones.push(
                Bone::new("leg_r", Some("pelvis"), hip, Z_LEG)
                    .rotated(round4(rng.range(1.3, 1.45)))
                    .length(round4(dims.thigh))
                    .anim(Anim::walk(0.24, std::f64::consts::PI))
                    .with_parts(vec![
                        Part::capsule("thigh", &pal.body, [round4(dims.thigh), 17.0], [0.0, 0.0], 0),
                        Part::ellipse("knee", &pal.body_dark, [16.0, 16.0], [round4(dims.thigh - 6.0), 0.0], 1, None),
                    ]),
            );
            if features.shin {
                bones.push(
                    Bone::new("shin_r", Some("leg_r"), [round4(dims.thigh - 8.0), 0.0], Z_LEG)
                        .rotated(round4(rng.range(0.05, 0.16)))
                        .length(round4(dims.shin))
                        .anim(Anim::walk(0.14, std::f64::consts::PI))
                        .with_parts(vec![Part::capsule("shin", &pal.body, [round4(dims.shin), 13.0], [0.0, 0.0], 0)]),
                );
                if features.foot {
                    bones.push(
                        Bone::new("foot_r", Some("shin_r"), [round4(dims.shin - 8.0), 0.0], Z_LEG)
                            .rotated(-1.0)
                            .with_parts(vec![
                                Part::ellipse("hoof", &pal.metal, [20.0, 12.0], [round4(6.0), 0.0], 0, None),
                                Part::ellipse("hoof_cuff", &pal.accent, [10.0, 14.0], [0.0, 0.0], 1, None),
                            ]),
                    );
                }
            }
        }
        LegKind::Insectoid => {
            // Two thin pairs: the second pair is a separate bone so it can step
            // out of phase, which is what sells "insect" in motion.
            for (pair, (id, hip_x, angle)) in [
                ("", dims.torso_w * 0.26, 1.45),
                ("hind_", dims.torso_w * 0.16, 1.7),
            ]
            .into_iter()
            .enumerate()
            {
                let length = if pair == 0 { dims.thigh } else { dims.thigh * 0.84 };
                bones.push(
                    Bone::new(
                        &format!("{id}leg_r"),
                        Some("pelvis"),
                        [round4(hip_x), 6.0],
                        Z_LEG,
                    )
                    .rotated(round4(angle))
                    .length(round4(length))
                    .anim(Anim::walk(0.2 + pair as f64 * 0.12, pair as f64 * std::f64::consts::PI))
                    .with_parts(vec![
                        Part::capsule(
                            "insect_leg",
                            &pal.body_dark,
                            [round4(length), 8.0],
                            [0.0, 0.0],
                            0,
                        ),
                        Part::ellipse(
                            "insect_joint",
                            &pal.accent,
                            [9.0, 9.0],
                            [round4(length - 4.0), 0.0],
                            1,
                            None,
                        ),
                    ]),
                );
            }
        }
        LegKind::Stonework => {
            let length = round4(dims.thigh * 1.1);
            bones.push(
                Bone::new(
                    "leg_r",
                    Some("pelvis"),
                    [round4(dims.torso_w * 0.3), 6.0],
                    Z_LEG,
                )
                .rotated(round4(rng.range(1.4, 1.5)))
                .length(length)
                .anim(Anim::walk(0.18, std::f64::consts::PI))
                .with_parts(vec![
                    Part::capsule("stone_leg", &pal.body, [length, 26.0], [0.0, 0.0], 0),
                    Part::ellipse(
                        "stone_foot",
                        &pal.body_dark,
                        [round4(length * 0.6), 20.0],
                        [round4(length - 4.0), 0.0],
                        1,
                        None,
                    ),
                    Part::ellipse(
                        "stone_crack",
                        &pal.accent,
                        [round4(length * 0.4), 3.0],
                        [round4(length * 0.4), 0.0],
                        2,
                        None,
                    ),
                ]),
            );
        }
        LegKind::Coiled => {
            // A serpent has no legs: the coil is drawn on the pelvis instead, so
            // the chain here stays empty on purpose.
        }
        LegKind::Finned => {
            let hip = [round4(dims.torso_w * 0.3), 6.0];
            bones.push(
                Bone::new("leg_r", Some("pelvis"), hip, Z_LEG)
                    .rotated(round4(rng.range(1.2, 1.4)))
                    .length(round4(dims.thigh * 0.7))
                    .anim(Anim::walk(0.18, std::f64::consts::PI))
                    .with_parts(vec![
                        Part::ellipse(
                            "leg_stub",
                            &pal.body,
                            [round4(dims.thigh * 0.9), round4(dims.thigh * 0.5)],
                            [round4(dims.thigh * 0.35), 0.0],
                            0,
                            None,
                        ),
                        Part::ellipse(
                            "leg_fin",
                            &pal.accent,
                            [round4(dims.thigh * 0.8), round4(dims.thigh * 0.16)],
                            [round4(dims.thigh * 0.4), 0.0],
                            1,
                            None,
                        ),
                        Part::ellipse(
                            "leg_fin2",
                            &pal.accent,
                            [round4(dims.thigh * 0.6), round4(dims.thigh * 0.12)],
                            [round4(dims.thigh * 0.3), round4(dims.thigh * 0.16)],
                            1,
                            None,
                        )
                        .turned(round4(0.3)),
                    ]),
            );
        }
        LegKind::Biped => {
            let hip_angle = round4(rng.range(1.35, 1.55));
            let mut leg = Bone::new(
                "leg_r",
                Some("pelvis"),
                [round4(dims.torso_w * 0.28), 6.0],
                Z_LEG,
            )
            .rotated(hip_angle)
            .length(round4(dims.thigh));
            let mut parts = vec![
                Part::capsule("thigh", &pal.body, [round4(dims.thigh), 18.0], [0.0, 0.0], 0),
                Part::ellipse("knee", &pal.body_dark, [16.0, 16.0], [round4(dims.thigh - 6.0), 0.0], 1, None),
            ];
            if features.armor {
                parts.push(Part::ellipse(
                    "thigh_plate",
                    &pal.metal,
                    [round4(dims.thigh * 0.7), 14.0],
                    [round4(dims.thigh * 0.3), 0.0],
                    2,
                    None,
                ));
            }
            leg = leg.with_parts(parts).anim(Anim::walk(0.26, std::f64::consts::PI));
            bones.push(leg);

            if features.shin {
                bones.push(
                    Bone::new("shin_r", Some("leg_r"), [round4(dims.thigh - 8.0), 0.0], Z_LEG)
                        .rotated(round4(rng.range(0.02, 0.14)))
                        .length(round4(dims.shin))
                        .anim(Anim::walk(0.14, std::f64::consts::PI))
                        .with_parts(vec![Part::capsule("shin", &pal.body, [round4(dims.shin), 14.0], [0.0, 0.0], 0)]),
                );
                if features.foot {
                    bones.push(
                        Bone::new("foot_r", Some("shin_r"), [round4(dims.shin - 8.0), 0.0], Z_LEG)
                            .rotated(-0.85)
                            .with_parts(vec![Part::ellipse(
                                "foot",
                                &pal.body_dark,
                                [26.0, 13.0],
                                [round4(8.0), 0.0],
                                0,
                                None,
                            )]),
                    );
                }
            }
        }
    }
}

fn push_wing(
    bones: &mut Vec<Bone>,
    pal: &Palette,
    dims: Dims,
    features: Features,
    plan: Plan,
    rng: &mut Rng,
) {
    let root_angle = round4(rng.range(-0.62, -0.42));
    let span = dims.wing;
    let panel_w = round4(span * 0.86);
    let panel_h = round4(span * 0.86);
    let panel_offset = round4(span * 0.46);
    let mut parts = vec![
        Part::ellipse(
            "wing_shoulder",
            &pal.body_dark,
            [round4(span * 0.52), round4(span * 0.46)],
            [0.0, 0.0],
            0,
            None,
        ),
        Part::ellipse(
            "wing_panel",
            &pal.body_dark,
            [panel_w, panel_h],
            [panel_offset, 0.0],
            1,
            None,
        ),
        // The spar is structural: it must stay inset inside the panel ellipse,
        // or its straight edge cuts across the curved silhouette.
        Part::capsule(
            "wing_spar",
            &pal.accent,
            [round4(structural_len(panel_w / 2.0, 10.0)), 10.0],
            [panel_offset, 0.0],
            2,
        ),
    ];
    if plan.feathers {
        // Three feather vanes trailing the spar: a blessed wing is feathered,
        // not a flat panel.
        for (index, tilt) in [-0.34, 0.0, 0.34].iter().enumerate() {
            parts.push(
                Part::ellipse(
                    if index == 0 { "feather" } else { "feather_more" },
                    &pal.glow,
                    [round4(span * 0.42), round4(span * 0.14)],
                    [round4(panel_offset - span * 0.1), round4(*tilt * span * 0.34)],
                    3,
                    None,
                )
                .turned(round4(*tilt)),
            );
        }
    }
    if features.eyespot {
        // One eyespot is four parts: ring, disc, core, offset pip.
        let spot = [round4(panel_offset + span * 0.12), round4(-panel_h * 0.1)];
        parts.extend([
            Part::circle("eyespot_ring", &pal.body, round4(span * 0.2), spot, 3, None),
            Part::circle("eyespot_disc", &pal.accent, round4(span * 0.14), spot, 4, None),
            Part::circle("eyespot_core", &pal.glow, round4(span * 0.07), spot, 5, None),
            Part::circle(
                "eyespot_pip",
                &pal.pupil,
                round4(span * 0.045),
                [round4(spot[0] + 3.0), round4(spot[1] - 3.0)],
                6,
                None,
            ),
        ]);
    }
    let wing = Bone::new(
        "wing_r",
        Some("torso"),
        // At the shoulder, not on the sternum: a wing panel that starts inside
        // the chest reads as a second face.
        [round4(dims.torso_w * 0.45), round4(-dims.torso_h * 0.5)],
        Z_WING,
    )
    .rotated(root_angle)
    .length(round4(span))
    .anim(Anim::sway(0.03, 0.0))
    .with_parts(parts);
    bones.push(wing);

    if features.wing_tip {
        let tip = Bone::new(
            "wing_tip_r",
            Some("wing_r"),
            [panel_offset, 0.0],
            Z_WING_TIP,
        )
        .rotated(round4(rng.range(0.14, 0.3)))
        .length(round4(span * 0.4))
        .anim(Anim::sway(0.04, 0.5))
        .with_parts(vec![
            Part::ellipse(
                "wing_tip_panel",
                &pal.body_dark,
                [round4(span * 0.5), round4(span * 0.4)],
                [round4(span * 0.22), 0.0],
                0,
                None,
            ),
            Part::ellipse(
                "wing_tip_edge",
                &pal.accent,
                [round4(span * 0.3), round4(span * 0.22)],
                [round4(span * 0.34), 0.0],
                1,
                None,
            ),
        ]);
        bones.push(tip);
    }

    if features.wing_tip2 {
        let crest = Bone::new(
            "wing_tip2_r",
            Some(if features.wing_tip { "wing_tip_r" } else { "wing_r" }),
            [round4(span * 0.34), 0.0],
            Z_WING_TIP - 1,
        )
        .rotated(round4(rng.range(0.18, 0.34)))
        .length(round4(span * 0.3))
        .anim(Anim::sway(0.05, 0.7))
        .with_parts(vec![
            Part::ellipse(
                "wing_tip2_panel",
                &pal.body_dark,
                [round4(span * 0.42), round4(span * 0.3)],
                [round4(span * 0.2), 0.0],
                0,
                None,
            ),
            Part::ellipse(
                "wing_tip2_edge",
                &pal.accent,
                [round4(span * 0.26), round4(span * 0.16)],
                [round4(span * 0.34), 0.0],
                1,
                None,
            ),
        ]);
        bones.push(crest);
    }
}

/* ------------------------------------------------------------------ *
 * Clips
 * ------------------------------------------------------------------ */

/// Every bone id paired with its twin, as `(left, right)`.
fn pairs(manifest: &Manifest) -> Vec<(String, String)> {
    manifest
        .bones
        .iter()
        .filter_map(|bone| {
            if bone.id.ends_with("_l") {
                let right = format!("{}_r", bone.id.trim_end_matches("_l"));
                if manifest.bone(&right).is_some() {
                    return Some((bone.id.clone(), right));
                }
            }
            None
        })
        .collect()
}

/// Add a mirrored pair of rotation deltas: `+a` on the left, `−a` on the right.
fn add_pair(
    pose: &mut BTreeMap<String, BoneDelta>,
    pairs: &[(String, String)],
    prefix: &str,
    amount: f64,
) {
    for (left, right) in pairs {
        if left.starts_with(prefix) {
            pose.insert(left.clone(), BoneDelta::rotation(amount));
            pose.insert(right.clone(), BoneDelta::rotation(-amount));
        }
    }
}

fn breathe_clip(manifest: &Manifest, tier: Tier) -> Clip {
    let lift = 2.0 + tier.index() as f64 * 0.6;
    let mut mid = BTreeMap::new();
    if manifest.bone("root").is_some() {
        mid.insert("root".to_string(), BoneDelta::shifted(0.0, lift));
    }
    if manifest.bone("torso").is_some() {
        mid.insert("torso".to_string(), BoneDelta::scaled(1.02));
    }
    if manifest.bone("head").is_some() {
        mid.insert("head".to_string(), BoneDelta::rotation(0.03));
    }
    let mut rest = BTreeMap::new();
    if manifest.bone("root").is_some() {
        rest.insert("root".to_string(), BoneDelta::shifted(0.0, 0.0));
    }
    Clip {
        duration_ms: 2400 + tier.index() as u64 * 120,
        r#loop: true,
        keyframes: vec![
            Keyframe {
                t: 0.0,
                bones: rest.clone(),
            },
            Keyframe { t: 0.5, bones: mid },
            Keyframe { t: 1.0, bones: rest },
        ],
    }
}

fn wave_clip(manifest: &Manifest) -> Clip {
    let pairs = pairs(manifest);
    let build = |amount: f64, head: f64| {
        let mut pose = BTreeMap::new();
        add_pair(&mut pose, &pairs, "arm", amount);
        add_pair(&mut pose, &pairs, "forearm", amount * 0.35);
        add_pair(&mut pose, &pairs, "wing", amount * 0.4);
        if manifest.bone("head").is_some() {
            pose.insert("head".to_string(), BoneDelta::rotation(head));
        }
        pose
    };
    Clip {
        duration_ms: 1600,
        r#loop: false,
        keyframes: vec![
            Keyframe {
                t: 0.0,
                bones: BTreeMap::new(),
            },
            Keyframe {
                t: 0.35,
                bones: build(1.5, 0.05),
            },
            Keyframe {
                t: 0.7,
                bones: build(1.7, -0.04),
            },
            Keyframe {
                t: 1.0,
                bones: build(0.0, 0.0),
            },
        ],
    }
}

/// The stage transition: crouch, flare, then settle bigger than it started.
fn digivolve_clip(manifest: &Manifest, tier: Tier) -> Clip {
    let pairs = pairs(manifest);

    let mut crouch = BTreeMap::new();
    if manifest.bone("root").is_some() {
        crouch.insert("root".to_string(), BoneDelta::shifted(0.0, 8.0));
    }
    if manifest.bone("torso").is_some() {
        crouch.insert("torso".to_string(), BoneDelta::rotation(0.06));
    }
    if manifest.bone("head").is_some() {
        crouch.insert("head".to_string(), BoneDelta::rotation(0.2));
    }
    add_pair(&mut crouch, &pairs, "arm", 0.35);
    add_pair(&mut crouch, &pairs, "wing", 0.3);
    add_pair(&mut crouch, &pairs, "leg", 0.12);

    let mut flare = BTreeMap::new();
    if manifest.bone("root").is_some() {
        flare.insert("root".to_string(), BoneDelta::scaled(1.06));
    }
    if manifest.bone("torso").is_some() {
        flare.insert("torso".to_string(), BoneDelta::rotation(-0.05));
    }
    if manifest.bone("head").is_some() {
        flare.insert("head".to_string(), BoneDelta::rotation(-0.14));
    }
    add_pair(&mut flare, &pairs, "arm", 1.35);
    add_pair(&mut flare, &pairs, "forearm", 0.4);
    add_pair(&mut flare, &pairs, "wing", 0.9);
    add_pair(&mut flare, &pairs, "leg", -0.08);
    if manifest.bone("tail").is_some() {
        flare.insert("tail".to_string(), BoneDelta::rotation(0.25));
    }

    let mut settle = BTreeMap::new();
    if manifest.bone("root").is_some() {
        settle.insert("root".to_string(), BoneDelta::scaled(1.015));
    }
    add_pair(&mut settle, &pairs, "arm", 0.5);
    add_pair(&mut settle, &pairs, "wing", 0.35);

    Clip {
        duration_ms: 3000 + tier.index() as u64 * 300,
        r#loop: false,
        keyframes: vec![
            Keyframe {
                t: 0.0,
                bones: BTreeMap::new(),
            },
            Keyframe { t: 0.3, bones: crouch },
            Keyframe { t: 0.62, bones: flare },
            Keyframe { t: 0.85, bones: settle },
            Keyframe {
                t: 1.0,
                bones: BTreeMap::new(),
            },
        ],
    }
}

/* ------------------------------------------------------------------ *
 * Guards
 * ------------------------------------------------------------------ */

/// Keep the painted artwork inside the top-monitor budget.
///
/// The generator's own budgets already leave ~70 px of headroom, so this is a
/// belt-and-braces guard for hand-tuned numbers: shifting the pelvis down moves
/// the whole body (and only the body) with it, which never disturbs the left/
/// right symmetry because only `y` changes.
fn enforce_monitor_budget(manifest: &mut Manifest) {
    for _ in 0..8 {
        let top = min_world_y(manifest);
        if !top.is_finite() || top >= MONITOR_BUDGET_Y {
            return;
        }
        let overflow = MONITOR_BUDGET_Y - top;
        if let Some(pelvis) = manifest.bones.iter_mut().find(|bone| bone.id == "pelvis") {
            pelvis.pos[1] = round4(pelvis.pos[1] + overflow / ROOT_SCALE);
        } else {
            return;
        }
    }
}

/// Structural checks the generator promises its callers, against the part budget
/// of the **base** stages (100). Prefer [`validate_at`] when the tier is known;
/// `POST /digivolve` and the plugin's status/install paths use that.
pub fn validate(manifest: &Manifest) -> Result<Report, String> {
    validate_at(manifest, Tier::Champion)
}

/// Structural checks against the budget of `tier` — the same checks, but a Mega
/// is allowed the 150 parts [`max_parts`] grants it.
pub fn validate_at(manifest: &Manifest, tier: Tier) -> Result<Report, String> {
    if manifest.format != "char-rig/model@1" {
        return Err(format!("unexpected format {:?}", manifest.format));
    }
    hierarchy_is_sound(&manifest.bones)?;

    let parts = manifest.part_count();
    let budget = max_parts(tier);
    if parts > budget {
        return Err(format!(
            "{parts} parts exceeds the {}-part budget for {}",
            budget,
            tier.as_str()
        ));
    }

    for bone in &manifest.bones {
        if let Some(limits) = &bone.limits {
            let bind = bone.rotation;
            if bind < limits.min || bind > limits.max {
                return Err(format!(
                    "limits on {:?} clamp its bind rotation ({bind} outside {}..{})",
                    bone.id, limits.min, limits.max
                ));
            }
        }
        for part in &bone.parts {
            match part.shape.as_str() {
                "ellipse" | "circle" | "capsule" | "face" => {}
                other => return Err(format!("part {:?} has shape {other:?}", part.id)),
            }
            match part.shape.as_str() {
                "capsule" => {
                    if part.pivot != [0.0, 0.5] {
                        return Err(format!("capsule {:?} must use pivot [0, 0.5]", part.id));
                    }
                }
                _ => {
                    if part.pivot != [0.0, 0.0] {
                        return Err(format!(
                            "{} {:?} must use pivot [0, 0]",
                            part.shape, part.id
                        ));
                    }
                }
            }
            if !part.size[0].is_finite()
                || !part.size[1].is_finite()
                || !part.offset[0].is_finite()
                || !part.offset[1].is_finite()
            {
                return Err(format!("part {:?} has a non-finite size or offset", part.id));
            }
        }
    }

    let world = crate::skeleton::world_transforms(&manifest.bones);
    let mut worst = 0.0f64;
    let mut culprit = String::new();
    for (left, right) in pairs(manifest) {
        let (Some(l), Some(r)) = (world.get(&left), world.get(&right)) else {
            continue;
        };
        let error = (l.x + r.x).abs().max((l.y - r.y).abs());
        if error > worst {
            worst = error;
            culprit = format!("{left}/{right}");
        }
    }
    if worst > 1e-6 {
        return Err(format!("left/right chains disagree by {worst} px ({culprit})"));
    }

    let top = min_world_y(manifest);
    if top < MONITOR_BUDGET_Y {
        return Err(format!("artwork reaches {top} px, above the budget"));
    }

    let clips = manifest
        .animations
        .as_ref()
        .map(|clips| clips.len())
        .unwrap_or(0);
    for clip in manifest.animations.iter().flat_map(|clips| clips.values()) {
        if clip.duration_ms == 0 {
            return Err("a clip has duration_ms 0".to_string());
        }
        for frame in &clip.keyframes {
            if !(0.0..=1.0).contains(&frame.t) {
                return Err(format!("keyframe t {} is outside 0..=1", frame.t));
            }
            for id in frame.bones.keys() {
                if manifest.bone(id).is_none() {
                    return Err(format!("a keyframe references unknown bone {id:?}"));
                }
            }
        }
    }

    Ok(Report {
        bones: manifest.bones.len(),
        parts,
        clips,
        min_world_y: top,
        symmetry_error: worst,
        budget,
    })
}

/// What [`validate`] measured — handy for logs and `GET /digimon/status`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Report {
    pub bones: usize,
    pub parts: usize,
    pub clips: usize,
    pub min_world_y: f64,
    pub symmetry_error: f64,
    /// The part cap this rig was validated against (see [`max_parts`]).
    pub budget: usize,
}

/// Left ids that exist for a right chain, used by the tests.
pub fn twin_of(id: &str) -> Option<String> {
    left_id(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tier_and_archetype_validates() {
        for archetype in Archetype::ALL {
            for tier in Tier::ALL {
                let manifest = generate(&Spec::new(0xC0FFEE, archetype, tier));
                let report = validate_at(&manifest, tier).unwrap_or_else(|err| {
                    panic!("{} {}: {err}", archetype.as_str(), tier.as_str())
                });
                assert!(report.parts <= max_parts(tier));
                assert_eq!(report.budget, max_parts(tier));
                assert!(report.min_world_y >= MONITOR_BUDGET_Y);
            }
        }
    }

    #[test]
    fn complexity_grows_with_the_tier() {
        let mut previous_bones = 0;
        let mut previous_parts = 0;
        for tier in Tier::ALL {
            let manifest = generate(&Spec::new(77, Archetype::Drake, tier));
            let bones = manifest.bones.len();
            let parts = manifest.part_count();
            assert!(
                bones >= previous_bones,
                "{} has fewer bones than the tier below",
                tier.as_str()
            );
            assert!(
                parts >= previous_parts,
                "{} has fewer parts than the tier below",
                tier.as_str()
            );
            previous_bones = bones;
            previous_parts = parts;
        }
    }

    #[test]
    fn the_same_spec_is_byte_identical() {
        let a = generate(&Spec::new(9, Archetype::Holy, Tier::Champion));
        let b = generate(&Spec::new(9, Archetype::Holy, Tier::Champion));
        assert_eq!(a.to_pretty_json(), b.to_pretty_json());
    }

    #[test]
    fn structural_capsules_stay_inside_their_ellipse() {
        // A limb capsule legitimately runs past its shoulder joint — the rule is
        // about *structural* detail (spine, wing spars) that must not cut across
        // the curved silhouette it decorates. Those are named, so this test
        // looks at exactly them instead of guessing from geometry.
        for (seed, archetype, tier) in [
            (11u64, Archetype::Cyborg, Tier::Ultimate),
            (0u64, Archetype::Beast, Tier::Fresh),
            (7, Archetype::Holy, Tier::Mega),
        ] {
            let manifest = generate(&Spec::new(seed, archetype, tier));
            let mut checked = 0;
            for bone in &manifest.bones {
                let ellipses: Vec<&Part> = bone
                    .parts
                    .iter()
                    .filter(|part| part.shape == "ellipse")
                    .collect();
                for capsule in bone.parts.iter().filter(|part| part.shape == "capsule") {
                    if capsule.id != "spine" && !capsule.id.ends_with("_spar") {
                        continue;
                    }
                    checked += 1;
                    let margin = capsule.size[1] / 2.0 + 5.0;
                    let (lo, hi) = capsule.local_box();
                    let cos = capsule.rotation.cos();
                    let sin = capsule.rotation.sin();
                    let mut xs = Vec::new();
                    let mut ys = Vec::new();
                    for [x, y] in [
                        [lo[0], lo[1]],
                        [hi[0], lo[1]],
                        [hi[0], hi[1]],
                        [lo[0], hi[1]],
                    ] {
                        xs.push(x * cos - y * sin + capsule.offset[0]);
                        ys.push(x * sin + y * cos + capsule.offset[1]);
                    }
                    let (min_x, max_x) = (
                        xs.iter().cloned().fold(f64::INFINITY, f64::min),
                        xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                    );
                    let (min_y, max_y) = (
                        ys.iter().cloned().fold(f64::INFINITY, f64::min),
                        ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                    );
                    let fits = ellipses.iter().any(|ellipse| {
                        let half_w = ellipse.size[0] / 2.0 - margin;
                        let half_h = ellipse.size[1] / 2.0 - margin;
                        min_x >= ellipse.offset[0] - half_w
                            && max_x <= ellipse.offset[0] + half_w
                            && min_y >= ellipse.offset[1] - half_h
                            && max_y <= ellipse.offset[1] + half_h
                    });
                    assert!(
                        fits,
                        "capsule {:?} on {:?} pokes out of every ellipse on that bone",
                        capsule.id, bone.id
                    );
                }
            }
            assert!(checked > 0, "the spine is a structural capsule");
        }
    }

    #[test]
    fn the_face_follows_the_tag_rules() {
        let manifest = generate(&Spec::new(3, Archetype::Beast, Tier::Champion));
        let head = manifest.bone("head").expect("head");
        let by_tag = |tag: &str| -> Vec<&Part> {
            head.parts
                .iter()
                .filter(|part| part.tag.as_deref() == Some(tag))
                .collect()
        };
        assert_eq!(by_tag("eye").len(), 2, "two eyes");
        assert_eq!(by_tag("pupil").len(), 2, "and two separate pupils");
        assert_eq!(by_tag("mouth").len(), 2, "mouth is split in halves");
        assert_eq!(by_tag("brow").len(), 2);
        for part in by_tag("brow") {
            assert_eq!(part.shape, "ellipse", "brows must be ellipses");
        }
        let mouths: Vec<f64> = by_tag("mouth").iter().map(|part| part.offset[0]).collect();
        assert!(mouths.contains(&-7.0) && mouths.contains(&7.0));
        // The pupil is never tagged `eye` (it would squash flat on blink).
        for part in by_tag("pupil") {
            assert_ne!(part.tag.as_deref(), Some("eye"));
        }
    }

    #[test]
    fn the_z_stack_puts_wings_behind_and_arms_in_front() {
        let manifest = generate(&Spec::new(21, Archetype::Drake, Tier::Mega));
        let z = |id: &str| manifest.bone(id).map(|bone| bone.z);
        let torso = z("torso").unwrap();
        let head = z("head").unwrap();
        for id in ["wing_r", "wing_l", "wing_tip_r", "tail"] {
            assert!(z(id).unwrap() < torso, "{id} must paint behind the torso");
        }
        for id in ["arm_r", "arm_l", "hand_r"] {
            assert!(z(id).unwrap() > torso, "{id} must paint in front of the torso");
        }
        assert!(head > z("arm_r").unwrap(), "the head paints last");
        // Whatever the plan calls its crest (horns, antlers, a blade, antennae),
        // it sits on the Z_HORN layer, behind the skull.
        let crest = manifest
            .bones
            .iter()
            .find(|bone| bone.z == Z_HORN)
            .expect("a crest bone");
        assert!(
            crest.z < head,
            "{} must emerge from behind the skull",
            crest.id
        );
    }

    #[test]
    fn the_digivolve_clip_mirrors_its_deltas() {
        let manifest = generate(&Spec::new(5, Archetype::Drake, Tier::Ultimate));
        let clip = manifest.animations.as_ref().unwrap().get("digivolve").unwrap();
        let mut checked = 0;
        for frame in &clip.keyframes {
            for (id, delta) in &frame.bones {
                if let Some(right) = twin_of(id) {
                    if let Some(other) = frame.bones.get(&right) {
                        let left = delta.rotation.unwrap_or(0.0);
                        let right = other.rotation.unwrap_or(0.0);
                        assert!(
                            (left + right).abs() < 1e-9,
                            "{id} {left} vs {right} must be opposite"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 0, "the clip actually animates paired bones");
    }

    #[test]
    fn the_monitor_budget_holds_for_a_hundred_seeds() {
        for seed in 0..100u64 {
            for archetype in Archetype::ALL {
                for tier in [Tier::Champion, Tier::Mega] {
                    let manifest = generate(&Spec::new(seed, archetype, tier));
                    let top = min_world_y(&manifest);
                    assert!(
                        top >= MONITOR_BUDGET_Y,
                        "seed {seed} {} {} reaches {top}",
                        archetype.as_str(),
                        tier.as_str()
                    );
                }
            }
        }
    }
}
