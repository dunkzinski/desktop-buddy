//! End-to-end verification of the procedural generator.
//!
//! These are the acceptance tests for the Metamorphosis plugin: they generate a
//! creature for **every archetype × tier × a spread of seeds** and assert the
//! four properties `MODEL_FORMAT.md` demands of a hand-written rig:
//!
//! 1. a structurally valid manifest — parent references resolve, no hierarchy
//!    cycle, no `limits` clamping a bind rotation, every keyframe bone exists;
//! 2. forward-kinematics symmetry — `world_x(bone_l) == -world_x(bone_r)` and
//!    `world_y(bone_l) == world_y(bone_r)` for every paired joint;
//! 3. monitor bounds — `min_world_y >= -170` across all parts, with the root at
//!    `scale: 0.55`;
//! 4. the format's shape rules (pivots, face tags, z-stack) and the part budget.

use std::collections::HashSet;

use desktop_buddy_metamorphosis_plugin::attribute::{Attribute, AttributeBlend};
use desktop_buddy_metamorphosis_plugin::evolution::Metamorphosis;
use desktop_buddy_metamorphosis_plugin::generator::{
    generate, max_parts, validate_at, Archetype, Spec, Tier,
};
use desktop_buddy_metamorphosis_plugin::skeleton::{
    hierarchy_is_sound, min_world_y, world_transforms, Manifest, MAX_PARTS, MONITOR_BUDGET_Y,
    ROOT_SCALE,
};

/// Every spec the suite exercises: all tiers × all archetypes for a handful of
/// seeds, which is enough to hit the random arms (wing angles, tail curves,
/// palettes) without a slow sweep.
fn corpus() -> Vec<(Tier, Manifest)> {
    let seeds = [0u64, 7, 42, 0xC0FFEE];
    let mut out = Vec::new();
    for seed in seeds {
        for archetype in Archetype::ALL {
            for tier in Tier::ALL {
                for attribute in Attribute::ALL {
                    out.push((
                        tier,
                        generate(&Spec::with_attribute(seed, archetype, tier, attribute)),
                    ));
                }
            }
        }
    }
    out
}

fn label(manifest: &Manifest) -> String {
    manifest
        .notes
        .clone()
        .unwrap_or_else(|| manifest.name.clone())
}

/* ------------------------------------------------------------------ *
 * 1. Schema, parents, cycles, limits
 * ------------------------------------------------------------------ */

#[test]
fn every_manifest_is_valid_json_for_the_format() {
    for (tier, manifest) in corpus() {
        let where_ = label(&manifest);
        assert_eq!(manifest.format, "desktop-buddy/model@1", "{where_}");
        assert!(!manifest.name.is_empty(), "{where_}");
        assert!(!manifest.label.is_empty(), "{where_}");
        assert!(!manifest.bones.is_empty(), "{where_}");

        // It survives a round trip through JSON exactly as it will be written.
        let text = manifest.to_pretty_json();
        let parsed: Manifest =
            serde_json::from_str(&text).unwrap_or_else(|err| panic!("{where_}: {err}"));
        assert_eq!(parsed, manifest, "{where_}: the written JSON must round-trip");

        // And the plugin's own validator agrees, against this stage's budget.
        let report = validate_at(&manifest, tier).unwrap_or_else(|err| panic!("{where_}: {err}"));
        assert!(report.bones > 0);
        assert!(report.parts > 0);
        assert!(report.clips >= 3, "{where_}: breathe, wave and metamorphose");
    }
}

#[test]
fn every_parent_reference_resolves_and_no_hierarchy_cycles() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let ids: HashSet<&str> = manifest.bone_ids().into_iter().collect();
        assert_eq!(ids.len(), manifest.bones.len(), "{where_}: duplicate ids");

        for bone in &manifest.bones {
            if let Some(parent) = bone.parent.as_deref() {
                assert!(ids.contains(parent), "{where_}: {parent:?} is missing");
                assert_ne!(parent, bone.id, "{where_}: a bone is its own parent");
            }
        }
        // `root` is the only root.
        let roots: Vec<&str> = manifest
            .bones
            .iter()
            .filter(|bone| bone.parent.is_none())
            .map(|bone| bone.id.as_str())
            .collect();
        assert_eq!(roots, vec!["root"], "{where_}");

        hierarchy_is_sound(&manifest.bones).unwrap_or_else(|err| panic!("{where_}: {err}"));
    }
}

#[test]
fn no_limits_clamp_a_bind_rotation() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        for bone in &manifest.bones {
            let Some(limits) = &bone.limits else {
                continue;
            };
            assert!(
                limits.min <= limits.max,
                "{where_}: {:?} has inverted limits",
                bone.id
            );
            assert!(
                limits.min <= bone.rotation && bone.rotation <= limits.max,
                "{where_}: limits {:?} on {:?} clamp the bind rotation {} \
                 (the classic \"one wing up\" bug)",
                limits,
                bone.id,
                bone.rotation
            );
        }
    }
}

#[test]
fn every_keyframe_names_a_real_bone_and_stays_in_range() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let ids: HashSet<&str> = manifest.bone_ids().into_iter().collect();
        let clips = manifest.animations.as_ref().expect("clips");
        for name in ["breathe", "wave", "metamorphose"] {
            let clip = clips
                .get(name)
                .unwrap_or_else(|| panic!("{where_}: no {name} clip"));
            assert!(clip.duration_ms > 0, "{where_}: {name} has no duration");
            assert!(clip.keyframes.len() >= 2, "{where_}: {name} needs keys");
            for frame in &clip.keyframes {
                assert!(
                    (0.0..=1.0).contains(&frame.t),
                    "{where_}: {name} key t {} outside 0..=1",
                    frame.t
                );
                for (bone, delta) in &frame.bones {
                    assert!(ids.contains(bone.as_str()), "{where_}: {name} → {bone}");
                    for value in [delta.rotation, delta.x, delta.y, delta.scale] {
                        if let Some(value) = value {
                            assert!(value.is_finite(), "{where_}: {name} has a NaN");
                        }
                    }
                }
            }
        }
        // `breathe` loops, the other two do not (a one-shot flourish).
        assert!(clips["breathe"].r#loop);
        assert!(!clips["wave"].r#loop);
        assert!(!clips["metamorphose"].r#loop);
    }
}

/* ------------------------------------------------------------------ *
 * 2. Forward-kinematics symmetry
 * ------------------------------------------------------------------ */

/// Every `(left, right)` pair present in the rig.
fn pairs(manifest: &Manifest) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for bone in &manifest.bones {
        if let Some(stem) = bone.id.strip_suffix("_l") {
            let right = format!("{stem}_r");
            if manifest.bone(&right).is_some() {
                out.push((bone.id.clone(), right));
            }
        }
    }
    out
}

#[test]
fn paired_chains_are_mirror_symmetric_in_world_space() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let world = world_transforms(&manifest.bones);
        let pairs = pairs(&manifest);
        assert!(!pairs.is_empty(), "{where_}: no paired limbs at all");

        for (left, right) in &pairs {
            let l = world
                .get(left)
                .unwrap_or_else(|| panic!("{where_}: {left} missing"));
            let r = world
                .get(right)
                .unwrap_or_else(|| panic!("{where_}: {right} missing"));
            assert!(
                (l.x + r.x).abs() < 1e-9,
                "{where_}: world_x({left}) = {} is not -world_x({right}) = {}",
                l.x,
                r.x
            );
            assert!(
                (l.y - r.y).abs() < 1e-9,
                "{where_}: world_y({left}) = {} is not world_y({right}) = {}",
                l.y,
                r.y
            );
            assert!(
                (l.scale - r.scale).abs() < 1e-9,
                "{where_}: {left}/{right} scales differ"
            );
            // Rotations mirror across the vertical axis, not by plain negation.
            let mirrored = l.rot.sin().atan2(-l.rot.cos());
            let diff = (mirrored - r.rot).abs().min((mirrored - r.rot - std::f64::consts::TAU).abs());
            assert!(
                diff < 1e-6,
                "{where_}: {left} rot {} is not the vertical mirror of {right} rot {}",
                l.rot,
                r.rot
            );
        }
    }
}

#[test]
fn the_centerline_is_unmirrored_and_vertical() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let world = world_transforms(&manifest.bones);
        for id in ["root", "pelvis", "torso", "head", "tail"] {
            let bone = manifest
                .bone(id)
                .unwrap_or_else(|| panic!("{where_}: no centerline bone {id}"));
            assert!(bone.parent.is_none() || !bone.id.ends_with("_l"));
            let world = world.get(id).expect("resolved");
            assert!(
                world.x.abs() < 1e-9,
                "{where_}: centerline bone {id} sits at x = {} (not 0)",
                world.x
            );
            let _ = bone;
        }
    }
}

/* ------------------------------------------------------------------ *
 * 3. Monitor bounds
 * ------------------------------------------------------------------ */

#[test]
fn the_root_is_scaled_for_the_monitor_budget() {
    for (_tier, manifest) in corpus() {
        let root = manifest.bone("root").expect("root");
        assert_eq!(root.parent, None, "the root has no parent");
        assert!(
            (root.scale - ROOT_SCALE).abs() < 1e-12,
            "root scale is {} not {}",
            root.scale,
            ROOT_SCALE
        );
        assert_eq!(root.pos, [0.0, 0.0]);
    }
}

#[test]
fn nothing_paints_above_the_top_clip_budget() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let top = min_world_y(&manifest);
        assert!(
            top >= MONITOR_BUDGET_Y,
            "{where_}: artwork reaches {top} px, above the {MONITOR_BUDGET_Y} px budget"
        );
        // Sanity in the other direction: a rig that paints nothing above its
        // root would be a degenerate creature, not a compact one.
        assert!(top < 0.0, "{where_}: nothing is above the root ({top})");
    }
}

/* ------------------------------------------------------------------ *
 * 4. Format shape rules
 * ------------------------------------------------------------------ */

#[test]
fn pivots_and_part_budget_follow_the_format() {
    for (tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let budget = max_parts(tier);
        assert!(
            manifest.part_count() <= budget,
            "{where_}: {} parts (budget {budget})",
            manifest.part_count()
        );
        for bone in &manifest.bones {
            for part in &bone.parts {
                match part.shape.as_str() {
                    "capsule" => assert_eq!(
                        part.pivot,
                        [0.0, 0.5],
                        "{where_}: capsule {:?} has pivot {:?}",
                        part.id,
                        part.pivot
                    ),
                    "ellipse" | "circle" => assert_eq!(
                        part.pivot,
                        [0.0, 0.0],
                        "{where_}: {} {:?} has pivot {:?}",
                        part.shape,
                        part.id,
                        part.pivot
                    ),
                    other => panic!("{where_}: unexpected shape {other:?}"),
                }
                assert!(
                    part.size[0] > 0.0 && part.size[1] > 0.0,
                    "{where_}: {:?} has an empty size {:?}",
                    part.id,
                    part.size
                );
                assert!(
                    part.color.starts_with('#') && part.color.len() == 7,
                    "{where_}: {:?} has colour {:?}",
                    part.id,
                    part.color
                );
            }
        }
    }
}

#[test]
fn the_face_uses_split_mouths_ellipse_brows_and_separate_pupils() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let head = manifest.bone("head").expect("head");
        let tagged = |tag: &str| -> Vec<&desktop_buddy_metamorphosis_plugin::skeleton::Part> {
            head.parts
                .iter()
                .filter(|part| part.tag.as_deref() == Some(tag))
                .collect()
        };

        let mouths = tagged("mouth");
        assert_eq!(mouths.len(), 2, "{where_}: mouth must be split in two halves");
        let xs: Vec<f64> = mouths.iter().map(|part| part.offset[0]).collect();
        assert!(
            xs.contains(&-7.0) && xs.contains(&7.0),
            "{where_}: mouth halves at ±7, got {xs:?}"
        );

        let brows = tagged("brow");
        assert_eq!(brows.len(), 2, "{where_}");
        for brow in brows {
            assert_eq!(brow.shape, "ellipse", "{where_}: brows must be ellipses");
        }

        let eyes = tagged("eye");
        let pupils = tagged("pupil");
        assert_eq!(eyes.len(), pupils.len(), "{where_}");
        assert!(eyes.len() >= 2, "{where_}: at least a normal pair");
        for pupil in pupils {
            assert_ne!(
                pupil.tag.as_deref(),
                Some("eye"),
                "{where_}: a pupil tagged `eye` squashes flat on every blink"
            );
            assert_ne!(pupil.id, "eye", "{where_}");
        }
        // The head is addressable as the head (`isHeadBone` also matches ids).
        assert!(head.id.contains("head"));
    }
}

#[test]
fn the_z_stack_keeps_wings_behind_and_arms_in_front() {
    for (_tier, manifest) in corpus() {
        let where_ = label(&manifest);
        let z = |id: &str| manifest.bone(id).map(|bone| bone.z);
        let torso = z("torso").expect("torso");
        let head = z("head").expect("head");

        for id in ["wing_r", "wing_l", "wing_tip_r", "wing_tip_l", "tail", "tail2", "tail3"] {
            if let Some(value) = z(id) {
                assert!(
                    value < torso,
                    "{where_}: {id} z={value} must paint behind the torso (z={torso})"
                );
            }
        }
        for id in ["leg_r", "leg_l", "shin_r", "foot_r"] {
            if let Some(value) = z(id) {
                assert!(value < torso, "{where_}: {id} z={value} must sit under the waist");
            }
        }
        for id in ["arm_r", "arm_l", "forearm_r", "forearm_l", "hand_r", "hand_l"] {
            if let Some(value) = z(id) {
                assert!(
                    value > torso,
                    "{where_}: {id} z={value} must drape in front of the torso"
                );
            }
        }
        assert!(head > torso, "{where_}: the head paints over the body");
        for id in ["horn_r", "horn_l"] {
            if let Some(value) = z(id) {
                assert!(value < head, "{where_}: {id} must emerge from behind the skull");
            }
        }
    }
}

/* ------------------------------------------------------------------ *
 * 4b. Attribute and build variety
 * ------------------------------------------------------------------ */

/// Collect the geometry of a manifest — the things a viewer actually sees.
fn shape_signature(manifest: &Manifest) -> String {
    let mut sig = String::new();
    for bone in &manifest.bones {
        sig.push_str(&format!("{}@{:?};", bone.id, bone.pos));
        for part in &bone.parts {
            sig.push_str(&format!(
                "{}/{}/{:?}/{:?}/{};",
                part.id, part.shape, part.size, part.offset, part.color
            ));
        }
    }
    sig
}

#[test]
fn each_attribute_grows_a_different_creature() {
    // The headline rule: Seraph, Prism and Feral are three *forms*, not three
    // labels. Same seed, same archetype, same tier — different rig.
    for archetype in Archetype::ALL {
        for tier in [Tier::InTraining, Tier::Rookie, Tier::Champion, Tier::Mega] {
            let mut signatures = HashSet::new();
            for attribute in Attribute::ALL {
                let spec = Spec::with_attribute(0x5EED, archetype, tier, attribute);
                let manifest = generate(&spec);
                validate_at(&manifest, tier).unwrap_or_else(|err| {
                    panic!("{} {} {}: {err}", archetype.as_str(), tier.as_str(), attribute.as_str())
                });
                signatures.insert(shape_signature(&manifest));
            }
            assert_eq!(
                signatures.len(),
                3,
                "{} {} produces the same rig for two attributes",
                archetype.as_str(),
                tier.as_str()
            );
        }
    }
}

#[test]
fn the_attribute_shows_up_in_the_parts_not_only_the_colour() {
    // A tint alone would pass a "different manifest" check while looking
    // identical on the desktop, so check the *shape* vocabulary as well.
    let seraph = generate(&Spec::with_attribute(11, Archetype::Beast, Tier::Champion, Attribute::Seraph));
    let feral = generate(&Spec::with_attribute(11, Archetype::Beast, Tier::Champion, Attribute::Feral));
    let prism = generate(&Spec::with_attribute(11, Archetype::Beast, Tier::Champion, Attribute::Prism));

    let ids = |manifest: &Manifest| -> HashSet<String> {
        manifest
            .bones
            .iter()
            .flat_map(|bone| bone.parts.iter().map(|part| part.id.clone()))
            .collect()
    };
    let (v, w, d) = (ids(&seraph), ids(&feral), ids(&prism));
    assert!(v.contains("halo"), "a Seraph line wears a halo: {v:?}");
    assert!(v.contains("feather"), "and feathered wings");
    assert!(w.contains("tooth_l"), "a Feral line grows fangs");
    assert!(w.contains("spine_l"), "and back spines");
    assert!(d.contains("crystal_l"), "a Prism line grows crystals: {d:?}");
    assert!(d.contains("circuit_l"), "and circuit traces");
}

#[test]
fn different_seeds_build_different_individuals() {
    // Beyond archetype and attribute: within one plan, the seed still decides
    // the form (which of the archetype's four silhouettes), the build
    // (lean / canonical / broad) and the marking (none / spots / stripes /
    // patches).
    for archetype in Archetype::ALL {
        let mut signatures = HashSet::new();
        let mut variants = HashSet::new();
        for seed in 0..6u64 {
            let spec = Spec::with_attribute(seed, archetype, Tier::Rookie, Attribute::Prism);
            variants.insert(spec.variant());
            signatures.insert(shape_signature(&generate(&spec)));
        }
        assert_eq!(
            signatures.len(),
            6,
            "{}: six seeds collapsed to fewer rigs",
            archetype.as_str()
        );
        assert!(
            variants.len() >= 3,
            "{}: every seed landed on the same variant",
            archetype.as_str()
        );
    }
}

/// The sorted, de-duplicated part ids of a rig. Two rigs with the same set grew
/// the same furniture, whatever their colours.
fn part_ids(manifest: &Manifest) -> String {
    let mut ids: Vec<&str> = manifest
        .bones
        .iter()
        .flat_map(|bone| bone.parts.iter().map(|part| part.id.as_str()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.join(",")
}

/// The bind pose: every bone's rotation, which is where a form's posture and
/// stance live. Palette-blind on purpose.
fn bind_pose(manifest: &Manifest) -> String {
    let mut pose: Vec<String> = manifest
        .bones
        .iter()
        .map(|bone| format!("{}@{:.3}", bone.id, bone.rotation))
        .collect();
    pose.sort();
    pose.join(",")
}

/// The part ids a form's accessory contributes (one per accessory kind: the
/// first id of each set).
const FORM_PARTS: [&str; 8] = [
    "mane",
    "ridge",
    "spur_l",
    "hipfin_l",
    "frill_l",
    "ant2_l",
    "collar_ring",
    "tail_blade",
];

fn form_parts(manifest: &Manifest) -> Vec<&'static str> {
    FORM_PARTS
        .iter()
        .copied()
        .filter(|id| {
            manifest
                .bones
                .iter()
                .any(|bone| bone.parts.iter().any(|part| part.id == *id))
        })
        .collect()
}

#[test]
fn one_archetype_builds_many_silhouettes() {
    // The point of the form layer: twelve seeds of one archetype are not one
    // creature with twelve palettes. Several different *part sets* (the form's
    // accessory and the marking) and twelve different *bind poses* (the form's
    // stances, plus the seeded jitter).
    for archetype in Archetype::ALL {
        let mut sets = HashSet::new();
        let mut poses = HashSet::new();
        for seed in 0..12u64 {
            let manifest = generate(&Spec::with_attribute(
                seed,
                archetype,
                Tier::Champion,
                Attribute::Feral,
            ));
            validate_at(&manifest, Tier::Champion).unwrap_or_else(|err| {
                panic!("{} seed {seed}: {err}", archetype.as_str())
            });
            sets.insert(part_ids(&manifest));
            poses.insert(bind_pose(&manifest));
        }
        assert!(
            sets.len() >= 5,
            "{}: twelve seeds grew only {} distinct part sets",
            archetype.as_str(),
            sets.len()
        );
        assert_eq!(
            poses.len(),
            12,
            "{}: two seeds share a bind pose",
            archetype.as_str()
        );
    }
}

#[test]
fn the_form_is_visible_from_fresh() {
    // Stage-one identity: whatever the form hangs on the body (mane, ridge,
    // spurs, fins, frill, sensor pair, collar, tail blades) is already there on
    // a `Fresh` hatchling — it is scaled down by the tier, never withheld until
    // In-Training or Champion.
    for archetype in Archetype::ALL {
        let mut fresh_sets = HashSet::new();
        for seed in 0..12u64 {
            let fresh = generate(&Spec::with_attribute(
                seed,
                archetype,
                Tier::Fresh,
                Attribute::Feral,
            ));
            let champion = generate(&Spec::with_attribute(
                seed,
                archetype,
                Tier::Champion,
                Attribute::Feral,
            ));
            assert_eq!(
                form_parts(&fresh),
                form_parts(&champion),
                "{} seed {seed}: the form changed between Fresh and Champion",
                archetype.as_str()
            );
            fresh_sets.insert(part_ids(&fresh));
        }
        assert!(
            fresh_sets.len() >= 5,
            "{}: only {} distinct Fresh hatchlings in twelve seeds",
            archetype.as_str(),
            fresh_sets.len()
        );
    }
}

#[test]
fn the_same_archetype_at_fresh_is_still_its_own_creature() {
    // Fresh is where sameness used to hide. Every archetype must still read as
    // itself at stage one, accessory and all.
    let mut shapes = HashSet::new();
    for archetype in Archetype::ALL {
        let fresh = generate(&Spec::with_attribute(7, archetype, Tier::Fresh, Attribute::Feral));
        assert!(
            shapes.insert(part_ids(&fresh)),
            "{} shares its Fresh part set with another archetype",
            archetype.as_str()
        );
    }
}

/* ------------------------------------------------------------------ *
 * 5. Tier growth & determinism
 * ------------------------------------------------------------------ */

#[test]
fn bone_and_part_counts_grow_with_the_tier_and_stay_under_budget() {
    for archetype in Archetype::ALL {
        for seed in [0u64, 5, 12345] {
            let mut previous: Option<(usize, usize)> = None;
            for tier in Tier::ALL {
                let manifest = generate(&Spec::new(seed, archetype, tier));
                let bones = manifest.bones.len();
                let parts = manifest.part_count();
                assert!(
                    bones > 0 && bones <= 40,
                    "{} has {bones} bones",
                    manifest.name
                );
                assert!(
                    parts <= max_parts(tier),
                    "{} has {parts} parts (budget {})",
                    manifest.name,
                    max_parts(tier)
                );
                if let Some((previous_bones, previous_parts)) = previous {
                    assert!(
                        bones >= previous_bones,
                        "{} lost bones versus the tier below ({bones} < {previous_bones})",
                        manifest.name
                    );
                    assert!(
                        parts >= previous_parts,
                        "{} lost parts versus the tier below ({parts} < {previous_parts})",
                        manifest.name
                    );
                }
                previous = Some((bones, parts));
            }
        }
    }
}

#[test]
fn a_seed_reproduces_the_same_creature() {
    for tier in Tier::ALL {
        let spec = Spec::new(0x1234_5678, Archetype::Cyborg, tier);
        let a = generate(&spec);
        let b = generate(&spec);
        assert_eq!(a.to_pretty_json(), b.to_pretty_json());

        // A neighbouring seed is a different creature.
        let other = generate(&Spec::new(0x1234_5679, Archetype::Cyborg, tier));
        assert_ne!(a.to_pretty_json(), other.to_pretty_json());
    }
}

#[test]
fn each_tier_carries_the_stage_in_its_identity() {
    for tier in Tier::ALL {
        let spec = Spec::new(77, Archetype::Holy, tier);
        let manifest = generate(&spec);
        assert_eq!(manifest.name, spec.slug());
        assert!(manifest.name.starts_with("buddy-holy-"));
        assert!(manifest.name.contains(tier.slug()));
        assert_eq!(manifest.label, spec.label());
    }
}

#[test]
fn forty_seeds_of_one_family_are_forty_creatures() {
    // The variety promise, measured the way a viewer would: count distinct
    // *structural templates* (which parts exist at all — form accessory,
    // second trait, marking), not hash-everything distinctness, which a 1%
    // shade of brown already satisfies. Before the individual layer this
    // plateaued at the twelve variants the first table shipped (four forms x
    // three builds); forms, builds and markings since then only raised it.
    for archetype in Archetype::ALL {
        let mut templates = HashSet::new();
        let mut poses = HashSet::new();
        for seed in 0..40u64 {
            let manifest = generate(&Spec::with_attribute(
                seed,
                archetype,
                Tier::Champion,
                Attribute::Prism,
            ));
            validate_at(&manifest, Tier::Champion).unwrap_or_else(|err| {
                panic!("{} seed {seed}: {err}", archetype.as_str())
            });
            templates.insert(part_ids(&manifest));
            poses.insert(bind_pose(&manifest));
        }
        assert!(
            templates.len() >= 18,
            "{}: forty seeds grew only {} distinct templates",
            archetype.as_str(),
            templates.len()
        );
        assert_eq!(
            poses.len(),
            40,
            "{}: two of forty individuals share a bind pose",
            archetype.as_str()
        );
    }
}

#[test]
fn every_creature_can_actually_wave() {
    // The wave button is not a clip: the page adds a bone's `anim.wave` to its
    // rotation while the avatar is in `waving` mode (`ui/js/rig.js`). A rig that
    // ships `wave: 0` on every bone therefore answers the button by standing
    // still — which is what every procedurally generated creature did.
    for archetype in Archetype::ALL {
        for tier in Tier::ALL {
            let manifest = generate(&Spec::with_attribute(
                0x5EED,
                archetype,
                tier,
                Attribute::Prism,
            ));
            let strongest = manifest
                .bones
                .iter()
                .map(|bone| bone.anim.wave)
                .fold(0.0_f64, f64::max);
            assert!(
                strongest >= 0.5,
                "{} {}: the strongest wave is {strongest} rad — nothing would move",
                archetype.as_str(),
                tier.as_str()
            );
            // …and the body has to be alive at rest too, or it reads as a
            // drawing rather than a creature.
            let root = manifest
                .bones
                .iter()
                .find(|bone| bone.id == "root")
                .expect("root");
            assert!(root.anim.bob > 0.0, "{} {}: no idle bob", archetype.as_str(), tier.as_str());
        }
    }
}

/* ------------------------------------------------------------------ *
 * 6. Lineage blends — recency-weighted attributes
 * ------------------------------------------------------------------ */

/// Generate one stage of a creature with the given lineage (oldest first).
fn blended(seed: u64, archetype: Archetype, tier: Tier, lineage: &[Attribute]) -> Manifest {
    let attribute = *lineage.last().expect("a lineage has at least one attribute");
    generate(&Spec::with_blend(
        seed,
        archetype,
        tier,
        attribute,
        AttributeBlend::from_lineage(lineage),
    ))
}

fn part_ids_of(manifest: &Manifest) -> HashSet<String> {
    manifest
        .bones
        .iter()
        .flat_map(|bone| bone.parts.iter().map(|part| part.id.clone()))
        .collect()
}

fn part_size(manifest: &Manifest, id: &str) -> Option<[f64; 2]> {
    manifest
        .bones
        .iter()
        .flat_map(|bone| bone.parts.iter())
        .find(|part| part.id == id)
        .map(|part| part.size)
}

#[test]
fn the_part_ceiling_is_100_through_champion_and_150_for_ultimate_and_mega() {
    // The format's budget, per stage: high-tier multi-attribute forms need room
    // for layered silhouette detail, the lower stages do not.
    for tier in [Tier::Fresh, Tier::InTraining, Tier::Rookie, Tier::Champion] {
        assert_eq!(
            max_parts(tier),
            MAX_PARTS,
            "{} must keep the {MAX_PARTS}-part ceiling",
            tier.as_str()
        );
    }
    for tier in [Tier::Ultimate, Tier::Mega] {
        assert_eq!(
            max_parts(tier),
            150,
            "{} must have room for a blended silhouette",
            tier.as_str()
        );
    }

    // And no generated rig ever exceeds its own stage's ceiling.
    for (tier, manifest) in corpus() {
        assert!(
            manifest.part_count() <= max_parts(tier),
            "{}: {} parts over a {}-part budget",
            manifest.name,
            manifest.part_count(),
            max_parts(tier)
        );
    }
}

#[test]
fn a_one_hot_blend_generates_the_pure_attribute_rig() {
    // `Spec::with_attribute` is now "a blend of one": the two spellings must be
    // the same bytes, or a hatchling would change shape when its state gained a
    // `blend` field.
    for archetype in Archetype::ALL {
        for tier in Tier::ALL {
            for attribute in Attribute::ALL {
                let plain = generate(&Spec::with_attribute(0x5EED, archetype, tier, attribute));
                let blended = generate(&Spec::with_blend(
                    0x5EED,
                    archetype,
                    tier,
                    attribute,
                    AttributeBlend::pure(attribute),
                ));
                assert_eq!(
                    plain.to_pretty_json(),
                    blended.to_pretty_json(),
                    "{} {} {}: a one-hot blend changed the rig",
                    archetype.as_str(),
                    tier.as_str(),
                    attribute.as_str()
                );
            }
        }
    }
}

#[test]
fn a_blended_lineage_wears_both_attributes_furniture() {
    // Equal parts Prism and a recent Feral: crystal growths, circuit traces *and*
    // spines, fangs and toxic accents, on one body.
    let manifest = blended(
        0x5EED,
        Archetype::Beast,
        Tier::Champion,
        &[Attribute::Prism, Attribute::Feral],
    );
    let report = validate_at(&manifest, Tier::Champion).expect("a blended rig validates");
    // Exact mirroring within the format's bound: the same ~1e-15 FK residue a
    // pure rig has (a one-hot blend is byte-identical to it — see
    // `a_one_hot_blend_generates_the_pure_attribute_rig`).
    assert!(report.symmetry_error < 1e-9, "mirroring drifted: {}", report.symmetry_error);
    assert!(report.min_world_y >= MONITOR_BUDGET_Y);
    assert!(report.bones <= 40);

    let ids = part_ids_of(&manifest);
    for id in ["crystal_l", "circuit_l", "spine_l", "tooth_l"] {
        assert!(ids.contains(id), "{id} is missing from a Prism/Feral blend: {ids:?}");
    }

    // The paint is blended too: neither a pure Prism nor a pure Feral colour.
    let skin = |manifest: &Manifest| -> String {
        manifest
            .bones
            .iter()
            .flat_map(|bone| bone.parts.iter())
            .find(|part| part.id == "skull")
            .map(|part| part.color.clone())
            .expect("a skull")
    };
    let data_only = blended(0x5EED, Archetype::Beast, Tier::Champion, &[Attribute::Prism]);
    let virus_only = blended(0x5EED, Archetype::Beast, Tier::Champion, &[Attribute::Feral]);
    assert_ne!(skin(&manifest), skin(&data_only), "the mix is not a pure Prism paint");
    assert_ne!(skin(&manifest), skin(&virus_only), "nor a pure Feral paint");
}

#[test]
fn the_quieter_attribute_shrinks_and_loses_its_extra_detail_first() {
    // Four stages of Prism, then a Feral shift: Prism dominates (60/40). The viral
    // furniture is still there, but smaller, and the viral *ornament* that only
    // a dominant share earns is already gone.
    let dominant_data = blended(
        0x5EED,
        Archetype::Beast,
        Tier::Champion,
        &[
            Attribute::Prism,
            Attribute::Prism,
            Attribute::Prism,
            Attribute::Feral,
        ],
    );
    validate_at(&dominant_data, Tier::Champion).expect("valid");

    let ids = part_ids_of(&dominant_data);
    assert!(ids.contains("spine_l"), "the recent viral spines still show");
    assert!(ids.contains("crystal_core"), "the dominant prism core is kept");
    assert!(
        !ids.contains("spine_mid"),
        "the faint attribute loses its extra detail before the dominant one does"
    );

    // …and what survives is smaller than it would be at full share.
    let equal = blended(
        0x5EED,
        Archetype::Beast,
        Tier::Champion,
        &[Attribute::Prism, Attribute::Feral],
    );
    let faint = part_size(&dominant_data, "spine_l").expect("faint spine");
    let full = part_size(&equal, "spine_l").expect("full spine");
    assert!(
        faint[0] < full[0] && faint[1] < full[1],
        "a 40% share must draw smaller spines than a 50% share: {faint:?} vs {full:?}"
    );

    // A share under the plan threshold does not reach the silhouette at all.
    // (A linear lineage cannot get below `2/(n+1)`, so build the weights.)
    let faint = AttributeBlend::from_weights([0.0, 0.92, 0.08]);
    assert!(faint.weight(Attribute::Feral) < 0.12, "{}", faint.summary());
    let faintest = generate(&Spec::with_blend(
        0x5EED,
        Archetype::Beast,
        Tier::Champion,
        Attribute::Prism,
        faint,
    ));
    assert!(
        !part_ids_of(&faintest).contains("spine_l"),
        "an 8% share is a tint, not spines"
    );
}

#[test]
fn a_blend_is_deterministic_and_depends_on_the_lineage() {
    let lineage = [Attribute::Prism, Attribute::Prism, Attribute::Feral];
    let a = blended(0xC0FFEE, Archetype::Drake, Tier::Ultimate, &lineage);
    let b = blended(0xC0FFEE, Archetype::Drake, Tier::Ultimate, &lineage);
    assert_eq!(a.to_pretty_json(), b.to_pretty_json());

    // The same final attribute from a different history is a different body.
    let other = blended(
        0xC0FFEE,
        Archetype::Drake,
        Tier::Ultimate,
        &[Attribute::Feral, Attribute::Feral, Attribute::Feral],
    );
    assert_ne!(a.to_pretty_json(), other.to_pretty_json());
}

#[test]
fn blended_rigs_keep_every_rig_invariant() {
    let lineages: [&[Attribute]; 6] = [
        &[Attribute::Prism],
        &[Attribute::Prism, Attribute::Feral],
        &[Attribute::Feral, Attribute::Prism, Attribute::Prism],
        &[Attribute::Seraph, Attribute::Prism, Attribute::Feral],
        &[
            Attribute::Prism,
            Attribute::Prism,
            Attribute::Prism,
            Attribute::Feral,
        ],
        &[
            Attribute::Feral,
            Attribute::Seraph,
            Attribute::Prism,
            Attribute::Feral,
            Attribute::Seraph,
        ],
    ];
    for archetype in Archetype::ALL {
        for tier in Tier::ALL {
            for lineage in lineages {
                let manifest = blended(0x5EED, archetype, tier, lineage);
                let where_ = label(&manifest);
                let report = validate_at(&manifest, tier)
                    .unwrap_or_else(|err| panic!("{where_}: {err}"));
                assert!(report.bones <= 40, "{where_}: {} bones", report.bones);
                assert!(
                    report.parts <= max_parts(tier),
                    "{where_}: {} parts",
                    report.parts
                );
                assert!(report.symmetry_error < 1e-9, "{where_}: asymmetric");
                assert!(
                    report.min_world_y >= MONITOR_BUDGET_Y,
                    "{where_}: paints at {} px",
                    report.min_world_y
                );
                assert!(report.clips >= 3);
            }
        }
    }
}

#[test]
fn a_rewind_restores_the_earlier_blend_and_rig() {
    let mut state = Metamorphosis::new(0xBEEF, Archetype::Drake, Attribute::Prism);
    let fresh_blend = state.blend();

    // Branch 1 is a different attribute, so the line genuinely mixes.
    let first = state.metamorphose(Some(1), true).expect("first evolution");
    let rookie_blend = state.blend();
    let rookie_rig = state.manifest().to_pretty_json();
    assert_eq!(state.history.last().unwrap().to_attribute, Some(first.attribute));
    assert_ne!(rookie_blend, fresh_blend);

    state.metamorphose(Some(2), true).expect("second evolution");
    assert_ne!(state.blend(), rookie_blend, "the second step shifted the mix");
    assert_eq!(state.history.len(), 2);

    // One step back: the *exact* earlier body, and the exact earlier mix.
    let back = state.revert(1).expect("rewind");
    assert_eq!(state.blend(), rookie_blend);
    assert_eq!(back.blend, rookie_blend);
    assert_eq!(state.manifest().to_pretty_json(), rookie_rig);
    assert_eq!(state.history.len(), 1);

    // All the way home: the egg is pure again.
    state.revert(9).expect("rewind to the egg");
    assert_eq!(state.blend(), fresh_blend);
    assert!(state.history.is_empty());
}

#[test]
fn a_legacy_history_still_resolves_a_lineage() {
    // A save written before `to_attribute` existed: no field on either entry.
    let state: Metamorphosis = serde_json::from_str(
        r#"{
            "seed": 1234,
            "archetype": "drake",
            "tier": "Champion",
            "attribute": "Feral",
            "generation": 2,
            "slug": "buddy-drake-champion-feral-000004d2",
            "history": [
                { "from": "Fresh", "to": "In-Training", "slug": "a", "at_ms": 5,
                  "from_seed": 1, "from_attribute": "Prism" },
                { "from": "In-Training", "to": "Champion", "slug": "b", "at_ms": 6,
                  "from_seed": 2, "from_attribute": "Prism" }
            ]
        }"#,
    )
    .expect("legacy save");
    assert!(state.history.iter().all(|entry| entry.to_attribute.is_none()));

    let lineage = state.lineage();
    assert_eq!(lineage.len(), state.history.len() + 1);
    assert_eq!(lineage[0], Attribute::Prism, "the egg's attribute survives");
    assert_eq!(*lineage.last().unwrap(), Attribute::Feral, "and the live one is newest");
    // Prism twice, Feral once: a 50/50 tie, resolved deterministically by the
    // attribute order rather than by float noise.
    let blend = state.blend();
    assert!((blend.sum() - 1.0).abs() < 1e-12);
    assert_eq!(blend.weight(Attribute::Feral), blend.weight(Attribute::Prism));
    assert!(blend.summary().contains("Feral"), "{}", blend.summary());
    validate_at(&state.manifest(), state.tier).expect("a legacy line still generates a valid rig");
}
