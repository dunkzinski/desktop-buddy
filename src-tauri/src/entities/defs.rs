//! The static catalogue of prop definitions.
//!
//! A [`PropDef`] says what a prop *is* — which generator builds it, which states
//! it can be in, which sockets it can hang from, and how it grips. Nothing here
//! is per-instance: the instance is an [`super::store::Entity`], and the shape is
//! produced by [`super::gen::build`] from `(def, seed, params)`.
//!
//! Adding a prop (a lantern, a pickaxe, a chest) is one entry here plus one
//! builder in `gen.rs`; no renderer, route or plugin change.

use serde_json::{json, Value};

use super::gen;

/// What kind of thing this is. Drives the socket `accepts` match and, later, the
/// container/equipment rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropCategory {
    Tool,
    Container,
    Apparel,
    Furniture,
}

impl PropCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Container => "container",
            Self::Apparel => "apparel",
            Self::Furniture => "furniture",
        }
    }
}

/// One prop definition.
#[derive(Debug)]
pub struct PropDef {
    /// Stable id, also the `def` an entity carries.
    pub id: &'static str,
    pub label: &'static str,
    pub category: PropCategory,
    /// Matched against a socket's `accepts` list.
    pub tags: &'static [&'static str],
    /// Every state a caller may set. `default_state` must be one of them.
    pub states: &'static [&'static str],
    pub default_state: &'static str,
    /// Clips the generated mini-rig carries. A state sharing a clip's name plays
    /// it automatically — that is what keeps the page prop-agnostic.
    pub animations: &'static [&'static str],
    /// Sockets the prop itself exposes (for nesting, e.g. a helmet's lamp).
    pub own_sockets: &'static [&'static str],
    /// Default root rotation (radians, added to the host socket's) per host
    /// socket, so a rod is *gripped* rather than lying along the forearm.
    pub grips: &'static [(&'static str, f64)],
    pub summary: &'static str,
}

impl PropDef {
    /// Is `state` one this prop understands?
    pub fn knows_state(&self, state: &str) -> bool {
        self.states.iter().any(|known| *known == state)
    }

    /// How to orient the prop when it is attached to `socket`, if this prop has
    /// an opinion about that socket.
    pub fn grip_rotation(&self, socket: &str) -> Option<f64> {
        self.grips
            .iter()
            .find(|(name, _)| *name == socket)
            .map(|(_, rotation)| *rotation)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "category": self.category.as_str(),
            "tags": self.tags,
            "states": self.states,
            "default_state": self.default_state,
            "animations": self.animations,
            "own_sockets": self.own_sockets,
            "summary": self.summary,
        })
    }
}

/// The fishing rod. Length, segment count, grip wrap, reel, charm and the line
/// vary by seed; `cast`/`waiting`/`biting`/`reeling` are clips the page plays
/// without knowing what a rod is.
pub const FISHING_ROD: PropDef = PropDef {
    id: "fishing_rod",
    label: "Fishing rod",
    category: PropCategory::Tool,
    tags: &["rod", "tool"],
    states: &[
        "stowed",
        "equipped",
        "cast",
        "waiting",
        "biting",
        "reeling",
        // Done fishing, still holding it: the beat between the catch and the rod
        // going back in the pod.
        "resting",
        "broken",
    ],
    default_state: "stowed",
    animations: &["cast", "waiting", "biting", "reeling", "resting"],
    own_sockets: &["grip_point", "line_end"],
    // Held across the body, tip up and forward. Tuned against a generated
    // creature (rest hand rotation ~0.86 rad); a sidecar socket with its own
    // rotation still composes on top of this.
    grips: &[
        ("hand_primary", -1.25),
        ("hand_secondary", -1.25),
        ("back", -0.35),
        ("stage_ground", 0.0),
    ],
    summary: "A rod, a line and a bobber. Varies in length, wood and tackle.",
};

/// A **chest that is not a chest**: a faceted seed-pod, five or six petals
/// folded over the d12 face, that blooms open when you ask it to.
///
/// The whimsy is structural, not decorative — the lid *is* the flower. Closed, a
/// pentagon of blades meets in the middle and reads as a die; open, the blades
/// splay flat and the pod's core is a lit well with your things in it. Everything
/// else (petal count, facet style, a face, feet, moss, the palette) is seeded, so
/// two pods on the same stage are visibly different objects that behave the same.
pub const CHEST: PropDef = PropDef {
    id: "chest",
    label: "Curiosity pod",
    category: PropCategory::Container,
    tags: &["chest", "container", "storage", "pod"],
    // What the pod *is*. The motion between these (appear / opening / closing /
    // vanish) is a clip, not a state: there is no scheduler to move a state on
    // when a 400 ms animation finishes, so a state named "appearing" would just
    // linger and lie.
    states: &["dormant", "closed", "open"],
    default_state: "closed",
    animations: &["appear", "open", "close", "vanish"],
    own_sockets: &["chest_interior", "chest_lid", "chest_side"],
    grips: &[("stage_ground", 0.0), ("world", 0.0)],
    summary: "A seed-pod that blooms: faceted shell, petal lid, a lit well inside.",
};

/// The little things a pod comes with: a faceted stone, a seed, a scale, a bead.
///
/// Trinkets exist to make a summoned pod *worth opening* — a chest that is
/// always empty is furniture. They are decorative (no economy yet), owned by the
/// pod (`owner: "chest"`), and they come and go with it.
pub const TRINKET: PropDef = PropDef {
    id: "trinket",
    label: "Trinket",
    category: PropCategory::Furniture,
    tags: &["trinket", "gem", "curio"],
    states: &["idle", "glinting"],
    default_state: "idle",
    animations: &["glinting"],
    own_sockets: &["sparkle"],
    grips: &[],
    summary: "A small faceted find: a stone, a seed, a bead. Varies by seed.",
};

/// The sack the scavenging run carries. Phase 1 ships the definition and the
/// generator so the next activity needs no core change; nothing spawns it yet.
pub const LOOT_BAG: PropDef = PropDef {
    id: "loot_bag",
    label: "Loot bag",
    category: PropCategory::Container,
    tags: &["bag", "container"],
    states: &["stowed", "carried", "full", "torn"],
    default_state: "stowed",
    animations: &["settle"],
    own_sockets: &["shoulder_strap"],
    grips: &[
        ("hand_primary", 0.2),
        ("hand_secondary", 0.2),
        ("chest_back", 0.0),
        ("back", 0.0),
        ("stage_ground", 0.0),
    ],
    summary: "A sack with a strap, a flap and patches. Bulges when full.",
};

/* ------------------------------------------------------------------ *
 * The comedy props
 * ------------------------------------------------------------------ */

/// What comes out of a pod when you are looking for something else.
///
/// These exist for one joke: a gremlin rummages for an axe and produces a rubber
/// duck, an anvil and a magnifying glass first. They are ordinary props — same
/// generators, same states, same sockets — because the joke only works if the
/// pod really does hold them.
///
/// All four share the state name **`tumbling`**, and each answers with its own
/// clip: the duck flaps, the anvil tips and lands heavy, the magnifier wobbles on
/// its handle, the boot goes end over end. One word from the host, four different
/// gags — that is what the state→clip rule is for.

pub const RUBBER_DUCK: PropDef = PropDef {
    id: "rubber_duck",
    label: "Rubber duck",
    category: PropCategory::Tool,
    tags: &["junk", "toy", "bath"],
    states: &["idle", "squeaking", "tumbling"],
    default_state: "idle",
    animations: &["squeaking", "tumbling"],
    own_sockets: &[],
    grips: &[("hand_primary", 0.0), ("stage_ground", 0.0)],
    summary: "A bath toy with an expression of mild existential doubt.",
};

pub const ANVIL: PropDef = PropDef {
    id: "anvil",
    label: "Anvil",
    category: PropCategory::Tool,
    tags: &["junk", "heavy", "iron"],
    states: &["idle", "dropping", "tumbling"],
    default_state: "idle",
    animations: &["dropping", "tumbling"],
    own_sockets: &["anvil_horn"],
    grips: &[("hand_primary", 0.0), ("stage_ground", 0.0)],
    summary: "Ninety kilos of wrong answer.",
};

pub const MAGNIFYING_GLASS: PropDef = PropDef {
    id: "magnifying_glass",
    label: "Magnifying glass",
    category: PropCategory::Tool,
    tags: &["junk", "optics", "detective"],
    states: &["idle", "peering", "tumbling"],
    default_state: "idle",
    animations: &["peering", "tumbling"],
    own_sockets: &["lens"],
    grips: &[("hand_primary", -0.3), ("stage_ground", 0.0)],
    summary: "For finding things, in principle.",
};

pub const BOOT: PropDef = PropDef {
    id: "boot",
    label: "Boot",
    category: PropCategory::Apparel,
    tags: &["junk", "footwear"],
    states: &["idle", "tumbling"],
    default_state: "idle",
    animations: &["tumbling"],
    own_sockets: &[],
    grips: &[("hand_primary", 0.4), ("stage_ground", 0.0)],
    summary: "One boot. No explanation offered.",
};

/// The rummage pool, in the order `chest::find` picks from.
pub const JUNK: &[&PropDef] = &[&RUBBER_DUCK, &ANVIL, &MAGNIFYING_GLASS, &BOOT];

/// Every prop this build knows.
pub const PROPS: &[PropDef] = &[
    FISHING_ROD,
    LOOT_BAG,
    CHEST,
    TRINKET,
    RUBBER_DUCK,
    ANVIL,
    MAGNIFYING_GLASS,
    BOOT,
];

pub fn find(id: &str) -> Option<&'static PropDef> {
    PROPS.iter().find(|def| def.id == id)
}

/// The catalogue as JSON, for `GET /props` (and the future mining plugin's UI).
pub fn catalogue() -> Value {
    json!({
        "ok": true,
        "count": PROPS.len(),
        "props": PROPS.iter().map(PropDef::to_json).collect::<Vec<_>>(),
    })
}

/// Build a prop's mini-rig. Kept here so `defs` stays the one place that knows
/// both halves of a definition.
pub fn build_rig(def: &PropDef, seed: u64, params: &Value) -> Result<Value, String> {
    gen::build(def, seed, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_and_states_are_consistent() {
        let mut seen = HashSet::new();
        for def in PROPS {
            assert!(seen.insert(def.id), "duplicate prop id {}", def.id);
            assert!(
                def.knows_state(def.default_state),
                "{}: default state {:?} is not in the state list",
                def.id,
                def.default_state
            );
            for state in def.states {
                assert!(!state.is_empty(), "{}: empty state", def.id);
            }
            for grip in def.grips {
                assert!(!grip.0.is_empty(), "{}: empty grip socket", def.id);
            }
        }
    }

    #[test]
    fn every_animation_is_a_clip_the_generator_emits() {
        for def in PROPS {
            let rig = build_rig(def, 12_345, &json!({})).expect("build");
            let clips = rig
                .get("animations")
                .and_then(Value::as_object)
                .expect("animations");
            for clip in def.animations {
                assert!(
                    clips.contains_key(*clip),
                    "{}: advertised clip {clip:?} is missing from the generated rig",
                    def.id
                );
            }
            for socket in def.own_sockets {
                let ids: Vec<&str> = rig
                    .get("sockets")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
                            .collect()
                    })
                    .unwrap_or_default();
                assert!(
                    ids.contains(socket),
                    "{}: own socket {socket:?} is missing (has {ids:?})",
                    def.id
                );
            }
        }
    }

    #[test]
    fn every_declared_animation_is_a_clip_the_rig_carries() {
        // A state whose clip does not exist is a prop that silently freezes in the
        // last pose — the rod would "rest" mid-cast. Pin them together.
        for def in PROPS {
            let rig = build_rig(def, 7, &json!({})).expect("build");
            let clips = rig.get("animations").and_then(Value::as_object);
            for name in def.animations {
                assert!(
                    clips.is_some_and(|map| map.contains_key(*name)),
                    "{}: `{name}` is declared but the generated rig has no such clip",
                    def.id
                );
            }
        }
        // ...and the rod's rest really is one of them, since the plugin holds the
        // rod in it for a couple of seconds after fishing.
        assert!(FISHING_ROD.states.contains(&"resting"));
        assert!(FISHING_ROD.animations.contains(&"resting"));
    }

    #[test]
    fn every_generated_prop_is_a_rig_the_loader_accepts() {
        // The generator must never emit something `models::validate_rig` would
        // refuse: a prop is loaded by the same renderer as a creature.
        for def in PROPS {
            for seed in [0u64, 1, 7, 99, 123_456_789] {
                let rig = build_rig(def, seed, &json!({})).expect("build");
                let (bones, parts) =
                    crate::models::validate_rig(&rig).unwrap_or_else(|err| {
                        panic!("{} seed {seed}: invalid rig: {err}\n{rig:#}", def.id)
                    });
                assert!(bones >= 3, "{}: only {bones} bones", def.id);
                assert!(parts >= 3, "{}: only {parts} parts", def.id);
            }
        }
    }

    #[test]
    fn the_catalogue_reports_what_the_defs_say() {
        let value = catalogue();
        assert_eq!(value["ok"], true);
        assert_eq!(value["count"], PROPS.len());
        let ids: Vec<&str> = value["props"]
            .as_array()
            .expect("props")
            .iter()
            .filter_map(|entry| entry["id"].as_str())
            .collect();
        assert!(ids.contains(&"fishing_rod"));
        assert!(ids.contains(&"loot_bag"));
    }
}
