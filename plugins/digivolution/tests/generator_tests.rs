//! End-to-end verification of the procedural generator.
//!
//! These are the acceptance tests for the Digivolution plugin: they generate a
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

use char_rig_digivolution_plugin::attribute::Attribute;
use char_rig_digivolution_plugin::generator::{generate, validate, Archetype, Spec, Tier};
use char_rig_digivolution_plugin::skeleton::{
    hierarchy_is_sound, min_world_y, world_transforms, Manifest, MAX_PARTS, MONITOR_BUDGET_Y,
    ROOT_SCALE,
};

/// Every spec the suite exercises: all tiers × all archetypes for a handful of
/// seeds, which is enough to hit the random arms (wing angles, tail curves,
/// palettes) without a slow sweep.
fn corpus() -> Vec<Manifest> {
    let seeds = [0u64, 7, 42, 0xC0FFEE];
    let mut out = Vec::new();
    for seed in seeds {
        for archetype in Archetype::ALL {
            for tier in Tier::ALL {
                for attribute in Attribute::ALL {
                    out.push(generate(&Spec::with_attribute(seed, archetype, tier, attribute)));
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
    for manifest in corpus() {
        let where_ = label(&manifest);
        assert_eq!(manifest.format, "char-rig/model@1", "{where_}");
        assert!(!manifest.name.is_empty(), "{where_}");
        assert!(!manifest.label.is_empty(), "{where_}");
        assert!(!manifest.bones.is_empty(), "{where_}");

        // It survives a round trip through JSON exactly as it will be written.
        let text = manifest.to_pretty_json();
        let parsed: Manifest =
            serde_json::from_str(&text).unwrap_or_else(|err| panic!("{where_}: {err}"));
        assert_eq!(parsed, manifest, "{where_}: the written JSON must round-trip");

        // And the plugin's own validator agrees.
        let report = validate(&manifest).unwrap_or_else(|err| panic!("{where_}: {err}"));
        assert!(report.bones > 0);
        assert!(report.parts > 0);
        assert!(report.clips >= 3, "{where_}: breathe, wave and digivolve");
    }
}

#[test]
fn every_parent_reference_resolves_and_no_hierarchy_cycles() {
    for manifest in corpus() {
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
    for manifest in corpus() {
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
    for manifest in corpus() {
        let where_ = label(&manifest);
        let ids: HashSet<&str> = manifest.bone_ids().into_iter().collect();
        let clips = manifest.animations.as_ref().expect("clips");
        for name in ["breathe", "wave", "digivolve"] {
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
        assert!(!clips["digivolve"].r#loop);
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
    for manifest in corpus() {
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
    for manifest in corpus() {
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
    for manifest in corpus() {
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
    for manifest in corpus() {
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
    for manifest in corpus() {
        let where_ = label(&manifest);
        assert!(
            manifest.part_count() < MAX_PARTS,
            "{where_}: {} parts (budget {MAX_PARTS})",
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
    for manifest in corpus() {
        let where_ = label(&manifest);
        let head = manifest.bone("head").expect("head");
        let tagged = |tag: &str| -> Vec<&char_rig_digivolution_plugin::skeleton::Part> {
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
    for manifest in corpus() {
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
    // The headline rule: Vaccine, Data and Virus are three *forms*, not three
    // labels. Same seed, same archetype, same tier — different rig.
    for archetype in Archetype::ALL {
        for tier in [Tier::InTraining, Tier::Rookie, Tier::Champion, Tier::Mega] {
            let mut signatures = HashSet::new();
            for attribute in Attribute::ALL {
                let spec = Spec::with_attribute(0x5EED, archetype, tier, attribute);
                let manifest = generate(&spec);
                validate(&manifest).unwrap_or_else(|err| {
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
    let vaccine = generate(&Spec::with_attribute(11, Archetype::Beast, Tier::Champion, Attribute::Vaccine));
    let virus = generate(&Spec::with_attribute(11, Archetype::Beast, Tier::Champion, Attribute::Virus));
    let data = generate(&Spec::with_attribute(11, Archetype::Beast, Tier::Champion, Attribute::Data));

    let ids = |manifest: &Manifest| -> HashSet<String> {
        manifest
            .bones
            .iter()
            .flat_map(|bone| bone.parts.iter().map(|part| part.id.clone()))
            .collect()
    };
    let (v, w, d) = (ids(&vaccine), ids(&virus), ids(&data));
    assert!(v.contains("halo"), "a Vaccine line wears a halo: {v:?}");
    assert!(v.contains("feather"), "and feathered wings");
    assert!(w.contains("tooth_l"), "a Virus line grows fangs");
    assert!(w.contains("spine_l"), "and back spines");
    assert!(d.contains("crystal_l"), "a Data line grows crystals: {d:?}");
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
            let spec = Spec::with_attribute(seed, archetype, Tier::Rookie, Attribute::Data);
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
                Attribute::Virus,
            ));
            validate(&manifest).unwrap_or_else(|err| {
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
                Attribute::Virus,
            ));
            let champion = generate(&Spec::with_attribute(
                seed,
                archetype,
                Tier::Champion,
                Attribute::Virus,
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
        let fresh = generate(&Spec::with_attribute(7, archetype, Tier::Fresh, Attribute::Virus));
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
                    parts < MAX_PARTS,
                    "{} has {parts} parts (budget {MAX_PARTS})",
                    manifest.name
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
        assert!(manifest.name.starts_with("digi-holy-"));
        assert!(manifest.name.contains(tier.slug()));
        assert_eq!(manifest.label, spec.label());
    }
}
