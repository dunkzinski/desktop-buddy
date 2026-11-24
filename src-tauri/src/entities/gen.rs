//! Procedural prop generation.
//!
//! Each builder emits a **mini-rig in the model format** — bones, parts, clips,
//! sockets — so the page renders it with the same transform maths as the avatar
//! and the loader can validate it. Nothing here draws anything; a prop is data.
//!
//! The metamorphosis lesson applies unchanged: **vary shape first, colour
//! last.** Every builder picks structural axes (segment count, reel, flap style,
//! patch count …) before it picks a palette, and the variety tests count
//! *structural* tuples — one skeleton with N palettes is not variety.
//!
//! Sizes are authored in **rig units**, the same space as the creature's bones.
//! A generated gremlin has `root.scale = 0.55`, so a 200-unit rod draws ~110 px;
//! the socket's world scale applies automatically (`socketWorld` multiplies by
//! the bone's scale), which is what keeps props proportional across rigs.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::defs::PropDef;
use super::rng::Rng;

/// Wood, wrap and metal palettes. Indexed by the seed, never by the structure.
const WOODS: &[&str] = &["#8a5a2b", "#a8743f", "#6f4522", "#b9894e"];
const DARK_WOODS: &[&str] = &["#5d3a1c", "#4a2e16", "#6b4526"];
const WRAPS: &[&str] = &["#b8412f", "#2f6fb8", "#c9a227", "#3f8a5a", "#8a4fb8"];
const METALS: &[&str] = &["#b8b8c4", "#7d7d8c", "#d9c26a"];
const HIDES: &[&str] = &["#9c7b4f", "#7a5c3a", "#b08d5c"];
const PATCHES: &[&str] = &["#5a4632", "#a8563f", "#3f5a7a"];

/// Build the mini-rig for `def`.
///
/// Deterministic in `(def, seed, params)`: the same triple always yields the same
/// JSON, which is what lets an entity be recreated from its seed alone.
pub fn build(def: &PropDef, seed: u64, params: &Value) -> Result<Value, String> {
    let mut rng = Rng::new(mix(seed, def.id));
    match def.id {
        "fishing_rod" => Ok(fishing_rod(&mut rng, params)),
        "loot_bag" => Ok(loot_bag(&mut rng, params)),
        "chest" => Ok(chest(&mut rng, params)),
        other => Err(format!("no generator for prop {other:?}")),
    }
}

/// Gameplay numbers, generated from the same seed as the look so a fancier rod
/// really is a better rod. Kept out of the rig: the rig is presentation.
pub fn traits(def: &PropDef, seed: u64) -> BTreeMap<String, f64> {
    let mut rng = Rng::new(mix(seed, def.id) ^ 0xA5A5_5A5A);
    let mut out = BTreeMap::new();
    match def.id {
        "fishing_rod" => {
            out.insert("bite_luck".to_string(), round2(rng.range(0.0, 0.18)));
            out.insert("durability".to_string(), round2(rng.range(0.75, 1.25)));
            out.insert("cast_range".to_string(), round2(rng.range(0.9, 1.15)));
        }
        "loot_bag" => {
            out.insert("capacity".to_string(), rng.int(4, 10) as f64);
            out.insert("find_luck".to_string(), round2(rng.range(0.0, 0.12)));
        }
        "chest" => {
            // Slots are a gameplay number, not a costume: a bigger pod is rarer,
            // and `capacity_of` reads exactly this trait.
            out.insert("capacity".to_string(), rng.int(5, 8) as f64);
            out.insert("whimsy".to_string(), round2(rng.range(0.4, 1.0)));
            out.insert("sturdiness".to_string(), round2(rng.range(0.7, 1.2)));
            out.insert("creak".to_string(), round2(rng.range(0.0, 1.0)));
        }
        _ => {}
    }
    out
}

/// Fold the def id into the seed so one number gives a different rod and bag.
fn mix(seed: u64, id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/* ------------------------------------------------------------------ *
 * params
 * ------------------------------------------------------------------ */

fn number_param(params: &Value, key: &str) -> Option<f64> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn bool_param(params: &Value, key: &str) -> Option<bool> {
    params.get(key).and_then(Value::as_bool)
}

fn int_param(params: &Value, key: &str) -> Option<i64> {
    params.get(key).and_then(Value::as_i64)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/* ------------------------------------------------------------------ *
 * builders
 * ------------------------------------------------------------------ */

/// A rod: grip, 2–5 shaft segments, tip, line, bobber, and optional reel/charm.
fn fishing_rod(rng: &mut Rng, params: &Value) -> Value {
    let crude = bool_param(params, "crude").unwrap_or(false);
    let segments = int_param(params, "segments")
        .map(|value| value.clamp(2, 5) as usize)
        .unwrap_or_else(|| rng.below(4) as usize + 2);
    let length_mul = number_param(params, "length")
        .unwrap_or(1.0)
        .clamp(0.6, 1.6);

    // Structure first.
    let rings = if crude { 0 } else { rng.below(3) };
    let grip_style = rng.below(3); // 0 plain, 1 wrapped, 2 banded
    let reel = bool_param(params, "reel").unwrap_or_else(|| !crude && rng.chance(0.55));
    let charm = bool_param(params, "charm").unwrap_or_else(|| rng.chance(0.4));
    let length = (150.0 + rng.range(0.0, 60.0)) * length_mul;
    let grip_len = round2(24.0 + rng.range(0.0, 10.0));
    let shaft_total = (length - grip_len).max(60.0);
    let seg = round2(shaft_total / segments as f64);
    let line_len = round2(40.0 + rng.range(0.0, 50.0));

    // Colour last.
    let wood = if crude {
        rng.pick(&["#7d6a4a", "#6a5a3d"], "#7d6a4a")
    } else {
        WOODS[rng.below(WOODS.len() as u32) as usize]
    };
    let dark = if crude {
        "#5b4c33"
    } else {
        DARK_WOODS[rng.below(DARK_WOODS.len() as u32) as usize]
    };
    let wrap = if crude {
        "#8a7a5a"
    } else {
        WRAPS[rng.below(WRAPS.len() as u32) as usize]
    };
    let metal = METALS[rng.below(METALS.len() as u32) as usize];

    let mut bones: Vec<Value> = Vec::new();
    bones.push(bone("root", None, [0.0, 0.0], 0.0, 0, vec![]));

    // Grip. The butt cap sits *behind* the socket so the hand looks like it is
    // holding the rod rather than pinching its end.
    let mut grip_parts = vec![
        capsule("grip", round2(grip_len + 6.0), 11.0, [0.0, 0.0], dark, 1),
        ellipse("butt_cap", 9.0, 9.0, [-4.0, 0.0], metal, 0),
    ];
    if grip_style > 0 {
        for index in 0..(rings + if grip_style == 2 { 2 } else { 1 }) {
            let x = round2(4.0 + f64::from(index) * (grip_len - 8.0) / 3.0);
            grip_parts.push(ellipse(
                &format!("wrap_{index}"),
                12.0,
                12.0,
                [x, 0.0],
                wrap,
                2,
            ));
        }
    }
    bones.push(bone("grip", Some("root"), [0.0, 0.0], 0.0, 1, grip_parts));

    // Shaft: each segment hangs off the previous one's tip, with a little
    // authored kink so a rod is never a straight stick.
    let mut parent = "grip".to_string();
    let mut shaft_end = grip_len;
    for index in 0..segments {
        let id = format!("shaft{}", index + 1);
        let thickness = round2((6.6 - index as f64 * 0.9).max(2.6));
        let jitter = round2(rng.range(-0.03, 0.03));
        let parts = vec![capsule(
            &format!("seg{}", index + 1),
            round2(seg + thickness * 0.5),
            thickness,
            [0.0, 0.0],
            wood,
            2,
        )];
        bones.push(bone(
            &id,
            Some(&parent),
            [round2(shaft_end), 0.0],
            jitter,
            2,
            parts,
        ));
        parent = id;
        shaft_end = seg;
    }

    let tip_len = round2(seg * 0.7);
    bones.push(bone(
        "tip",
        Some(&parent),
        [round2(seg), 0.0],
        round2(rng.range(-0.02, 0.02)),
        2,
        vec![capsule(
            "tip_wood",
            round2(tip_len + 1.0),
            2.6,
            [0.0, 0.0],
            wood,
            2,
        )],
    ));

    // Line hangs down from the tip (+y is down on this canvas).
    bones.push(bone(
        "line",
        Some("tip"),
        [tip_len, 0.0],
        1.32,
        1,
        vec![capsule(
            "line_thread",
            line_len,
            1.3,
            [0.0, 0.0],
            "#dfe4ee",
            1,
        )],
    ));
    bones.push(bone(
        "bobber",
        Some("line"),
        [line_len, 0.0],
        -1.32,
        3,
        vec![
            ellipse("bob_top", 8.0, 8.0, [0.0, 3.0], "#d2453f", 3),
            ellipse("bob_bottom", 8.0, 6.0, [0.0, -2.0], "#f2f2f2", 3),
        ],
    ));

    if reel {
        bones.push(bone(
            "reel",
            Some("grip"),
            [round2(grip_len * 0.42), 7.0],
            -1.5708,
            3,
            vec![
                ellipse("reel_body", 13.0, 13.0, [0.0, 0.0], metal, 3),
                capsule("reel_foot", 8.0, 4.0, [0.0, -6.0], metal, 3),
            ],
        ));
    }
    if charm {
        bones.push(bone(
            "charm",
            Some("grip"),
            [round2(grip_len * 0.3), -7.0],
            0.5,
            3,
            vec![
                ellipse("charm_bead", 5.0, 5.0, [0.0, 0.0], wrap, 3),
                capsule("charm_feather", 16.0, 4.0, [10.0, 0.0], metal, 3),
            ],
        ));
    }

    json!({
        "kind": "prop",
        "def": "fishing_rod",
        "bones": bones,
        "animations": {
            "cast": clip(520.0, false, vec![
                kf(0.0, json!({ "root": { "rotation": 0.0 }, "shaft1": { "rotation": 0.0 },
                                "tip": { "rotation": 0.0 }, "line": { "rotation": 0.0 } })),
                kf(0.35, json!({ "root": { "rotation": -0.55 }, "shaft1": { "rotation": -0.18 },
                                 "tip": { "rotation": -0.22 }, "line": { "rotation": -0.35 } })),
                kf(0.7, json!({ "root": { "rotation": 0.12 }, "shaft1": { "rotation": 0.1 },
                                "tip": { "rotation": 0.08 }, "line": { "rotation": -0.5 } })),
                kf(1.0, json!({ "root": { "rotation": 0.0 }, "shaft1": { "rotation": 0.0 },
                                "tip": { "rotation": 0.0 }, "line": { "rotation": 0.0 } })),
            ]),
            "waiting": clip(2400.0, true, vec![
                kf(0.0, json!({ "tip": { "rotation": 0.0 }, "line": { "rotation": 0.0 },
                                "bobber": { "y": 0.0 } })),
                kf(0.5, json!({ "tip": { "rotation": 0.05 }, "line": { "rotation": 0.04 },
                                "bobber": { "y": 2.0 } })),
                kf(1.0, json!({ "tip": { "rotation": 0.0 }, "line": { "rotation": 0.0 },
                                "bobber": { "y": 0.0 } })),
            ]),
            "biting": clip(300.0, true, vec![
                kf(0.0, json!({ "tip": { "rotation": 0.0 }, "line": { "rotation": 0.0 },
                                "bobber": { "y": 0.0 } })),
                kf(0.3, json!({ "tip": { "rotation": -0.14 }, "line": { "rotation": 0.1 },
                                "bobber": { "y": 7.0 } })),
                kf(0.6, json!({ "tip": { "rotation": 0.05 }, "line": { "rotation": -0.02 },
                                "bobber": { "y": 3.0 } })),
                kf(1.0, json!({ "tip": { "rotation": 0.0 }, "line": { "rotation": 0.0 },
                                "bobber": { "y": 0.0 } })),
            ]),
            "reeling": clip(900.0, true, vec![
                kf(0.0, json!({ "shaft1": { "rotation": 0.0 }, "shaft2": { "rotation": 0.0 },
                                "line": { "rotation": 0.0 } })),
                kf(0.5, json!({ "shaft1": { "rotation": -0.18 }, "shaft2": { "rotation": -0.14 },
                                "line": { "rotation": -0.12 } })),
                kf(1.0, json!({ "shaft1": { "rotation": 0.0 }, "shaft2": { "rotation": 0.0 },
                                "line": { "rotation": 0.0 } })),
            ]),
        },
        "sockets": [
            { "id": "grip_point", "bone": "grip", "pos": [0.0, 0.0], "z": 0, "label": "Grip" },
            { "id": "line_end", "bone": "bobber", "pos": [0.0, 0.0], "z": 0, "label": "Line end" }
        ],
        "structure": {
            "segments": segments,
            "reel": reel,
            "charm": charm,
            "rings": rings,
            "grip_style": grip_style,
            "crude": crude,
            "length_bucket": (length / 20.0).round() as i64,
        },
    })
}

/// A sack: body, flap, strap, tie, patches; `fullness` bulges the body.
fn loot_bag(rng: &mut Rng, params: &Value) -> Value {
    let body_shape = rng.below(3); // 0 round, 1 tall, 2 squat
    let flap = rng.below(3); // 0 plain, 1 buckled, 2 patch-flap
    let strap = rng.below(2); // 0 single, 1 double
    let tie = rng.below(2);
    let patches = rng.below(4);
    let fullness = number_param(params, "fullness")
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);

    let hide = HIDES[rng.below(HIDES.len() as u32) as usize];
    let patch_color = |rng: &mut Rng| PATCHES[rng.below(PATCHES.len() as u32) as usize];

    let (body_w, body_h) = match body_shape {
        0 => (46.0, 44.0),
        1 => (38.0, 52.0),
        _ => (54.0, 36.0),
    };
    let bulge = 1.0 + fullness * 0.18;
    let body_w = round2(body_w * bulge);
    let body_h = round2(body_h * bulge);

    let mut body_parts = vec![ellipse("sack", body_w, body_h, [0.0, 0.0], hide, 1)];
    for index in 0..patches {
        let x = round2(rng.range(-body_w * 0.3, body_w * 0.3));
        let y = round2(rng.range(-body_h * 0.3, body_h * 0.3));
        body_parts.push(ellipse(
            &format!("patch_{index}"),
            round2(rng.range(7.0, 12.0)),
            round2(rng.range(6.0, 10.0)),
            [x, y],
            patch_color(rng),
            2,
        ));
    }

    let mut bones = vec![
        bone("root", None, [0.0, 0.0], 0.0, 0, vec![]),
        bone("body", Some("root"), [0.0, 0.0], 0.0, 1, body_parts),
    ];

    // Flap over the mouth of the sack.
    let flap_h = if flap == 2 { 12.0 } else { 8.0 };
    let mut flap_parts = vec![ellipse(
        "flap",
        round2(body_w * 0.9),
        flap_h,
        [0.0, 0.0],
        "#00000022",
        4,
    )];
    if flap == 1 {
        flap_parts.push(ellipse("buckle", 6.0, 6.0, [0.0, 4.0], "#d9c26a", 5));
    }
    bones.push(bone(
        "flap",
        Some("body"),
        [0.0, round2(-body_h * 0.42)],
        0.0,
        4,
        flap_parts,
    ));

    if tie == 1 {
        bones.push(bone(
            "tie",
            Some("body"),
            [0.0, round2(-body_h * 0.3)],
            0.0,
            5,
            vec![capsule(
                "cord",
                round2(body_w * 0.8),
                3.0,
                [0.0, 0.0],
                "#3a2c1c",
                5,
            )],
        ));
    }

    // Strap(s) arc over the shoulder.
    let mut strap_bones = Vec::new();
    let count = if strap == 1 { 2 } else { 1 };
    for index in 0..count {
        let offset = if count == 2 {
            (index as f64 - 0.5) * 10.0
        } else {
            0.0
        };
        strap_bones.push(bone(
            &format!("strap{index}"),
            Some("body"),
            [round2(offset), round2(-body_h * 0.5)],
            -0.5,
            3,
            vec![capsule(
                &format!("strap_cloth{index}"),
                46.0,
                5.0,
                [0.0, 0.0],
                "#4a3a28",
                3,
            )],
        ));
    }
    bones.extend(strap_bones);

    json!({
        "kind": "prop",
        "def": "loot_bag",
        "bones": bones,
        "animations": {
            "settle": clip(600.0, false, vec![
                kf(0.0, json!({ "body": { "scale": 1.0 } })),
                kf(0.3, json!({ "body": { "scale": 1.06 } })),
                kf(1.0, json!({ "body": { "scale": 1.0 } })),
            ]),
        },
        "sockets": [
            { "id": "shoulder_strap", "bone": "body", "pos": [0.0, -12.0], "z": 3, "label": "Strap" }
        ],
        "structure": {
            "body_shape": body_shape,
            "flap": flap,
            "strap": strap,
            "tie": tie,
            "patches": patches,
            "fullness_bucket": (fullness * 4.0).round() as i64,
        },
    })
}

/* ------------------------------------------------------------------ *
 * json helpers
 * ------------------------------------------------------------------ */

/* ------------------------------------------------------------------ *
 * The chest: a faceted pod that blooms
 * ------------------------------------------------------------------ */

/// Angles are authored in degrees and stored in radians; a pod is all angles.
fn rad(degrees: f64) -> f64 {
    degrees * std::f64::consts::PI / 180.0
}

/// Vertices of a regular polygon in rig units, `rotation_deg` 0 = vertex at 12
/// o'clock (canvas +y is down, so that is -90 deg).
fn regular_polygon(radius: f64, sides: usize, rotation_deg: f64) -> Vec<[f64; 2]> {
    (0..sides)
        .map(|index| {
            let angle = rad(-90.0 + rotation_deg)
                + (index as f64) * 2.0 * std::f64::consts::PI / (sides as f64);
            [round2(radius * angle.cos()), round2(radius * angle.sin())]
        })
        .collect()
}

/// A faceted part. `points` are authoritative for drawing; `size` is the bounding
/// box so every other consumer (bounds, hit regions, the editor) has a rectangle
/// without knowing about polygons.
fn polygon(id: &str, points: &[[f64; 2]], offset: [f64; 2], color: &str, z: i32) -> Value {
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    for point in points {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    json!({
        "id": id,
        "shape": "poly",
        "points": points,
        "size": [round2((max[0] - min[0]).max(1.0)), round2((max[1] - min[1]).max(1.0))],
        "offset": [round2(offset[0]), round2(offset[1])],
        "pivot": [0.0, 0.0],
        "color": color,
        "z": z,
    })
}

/// One keyframe covering every petal plus the body, so the lid moves as one
/// object. Petals get a hair of per-blade jitter: a perfectly synchronised
/// rosette reads as a machine part, not a flower.
fn petal_frame(
    petal_ids: &[String],
    rotation: f64,
    body_scale: f64,
    jitter: f64,
    rng: &mut Rng,
) -> Value {
    let mut map = serde_json::Map::new();
    for id in petal_ids {
        map.insert(
            id.clone(),
            json!({ "rotation": round2(rotation + rng.range(-jitter, jitter)) }),
        );
    }
    map.insert("body".to_string(), json!({ "scale": round2(body_scale) }));
    Value::Object(map)
}

/// The pod. Closed, the blades meet in the middle and the silhouette is a die;
/// open, each blade has swung 180 degrees about its own rim bone and the lit well
/// shows.
fn chest(rng: &mut Rng, params: &Value) -> Value {
    // ---- structure (before colour) --------------------------------------
    let sides = int_param(params, "petals")
        .map(|value| value.clamp(3, 8) as usize)
        .unwrap_or_else(|| if rng.below(3) == 0 { 6 } else { 5 });
    let radius = rng.range(46.0, 56.0);
    // How the shell reads as a solid: a rotated inner face, spokes, or scales.
    let facet_style = rng.below(3);
    let has_face = rng.below(10) < 6;
    let feet = rng.below(3) as usize;
    let charm = rng.below(4) == 0;
    let blush = rng.below(5) == 0;

    // ---- palette --------------------------------------------------------
    let palettes: [(&str, &str, &str, &str, &str); 6] = [
        ("#3f6b4a", "#5d8f5f", "#27412e", "#d8f2a6", "#87b86a"), // moss
        ("#6b4a3f", "#8f6a5d", "#412a27", "#f2d8a6", "#b8866a"), // brass
        ("#4a3f6b", "#6a5d8f", "#2a2741", "#cda6f2", "#8a6ab8"), // dusk
        ("#3f5a6b", "#5d7f8f", "#273641", "#a6e2f2", "#6a9eb8"), // tide
        ("#6b3f52", "#8f5d72", "#412730", "#f2a6c4", "#b86a8a"), // coral
        ("#5c5c3f", "#7f7f5d", "#3a3a27", "#f0f2a6", "#a8a86a"), // dry
    ];
    let (body, facet, rim, accent, petal_fill) =
        palettes[rng.below(palettes.len() as u32) as usize];

    // ---- shell ----------------------------------------------------------
    let outline = regular_polygon(radius, sides, 0.0);
    let mut shell = vec![
        // A shadow, so the pod sits on the desktop instead of hovering.
        ellipse(
            "shadow",
            radius * 1.7,
            radius * 0.42,
            [0.0, radius * 0.82],
            "#00000026",
            4,
        ),
        // The rim first: a slightly larger polygon behind the shell gives the
        // silhouette an edge without a stroke pass.
        polygon(
            "rim",
            &regular_polygon(radius * 1.05, sides, 0.0),
            [0.0, 0.0],
            rim,
            9,
        ),
    ];

    match facet_style {
        0 => {
            let inner = regular_polygon(radius * 0.62, sides, 180.0 / sides as f64);
            shell.push(polygon("facet", &inner, [0.0, -radius * 0.03], facet, 12));
        }
        1 => {
            for point in &outline {
                shell.push(ellipse(
                    "spoke",
                    2.4,
                    radius * 0.5,
                    [point[0] * 0.5, point[1] * 0.5],
                    facet,
                    12,
                ));
            }
            shell.push(ellipse(
                "spoke_core",
                radius * 0.42,
                radius * 0.42,
                [0.0, 0.0],
                facet,
                13,
            ));
        }
        _ => {
            for row in 0..2 {
                let ring = radius * (0.34 + row as f64 * 0.26);
                let count = sides + row;
                for index in 0..count {
                    let angle =
                        rad(-90.0 + index as f64 * 360.0 / count as f64 + row as f64 * 12.0);
                    shell.push(ellipse(
                        "scale",
                        radius * 0.3,
                        radius * 0.2,
                        [round2(ring * angle.cos()), round2(ring * angle.sin() * 0.9)],
                        facet,
                        12,
                    ));
                }
            }
        }
    }

    // The shell itself, then the well it hides: a dark interior, a lit core and a
    // rim shadow. This is what the petals uncover, so it has to look like
    // somewhere things live.
    shell.push(polygon("shell", &outline, [0.0, 0.0], body, 10));
    shell.push(ellipse(
        "well",
        radius * 1.02,
        radius * 0.62,
        [0.0, -radius * 0.05],
        rim,
        11,
    ));
    shell.push(ellipse(
        "glow",
        radius * 0.68,
        radius * 0.4,
        [0.0, -radius * 0.05],
        accent,
        12,
    ));
    shell.push(ellipse(
        "core",
        radius * 0.28,
        radius * 0.22,
        [0.0, -radius * 0.05],
        rim,
        13,
    ));

    if has_face {
        // Two sleepy eyes. To the gremlin, this thing is a pet.
        let eye_y = -radius * 0.34;
        shell.push(ellipse(
            "eye_l",
            radius * 0.1,
            radius * 0.12,
            [-radius * 0.17, eye_y],
            "#12160fcc",
            14,
        ));
        shell.push(ellipse(
            "eye_r",
            radius * 0.1,
            radius * 0.12,
            [radius * 0.17, eye_y],
            "#12160fcc",
            14,
        ));
        if blush {
            for side in [-1.0, 1.0] {
                shell.push(ellipse(
                    "blush",
                    radius * 0.11,
                    radius * 0.06,
                    [side * radius * 0.33, eye_y + radius * 0.13],
                    "#ffffff33",
                    14,
                ));
            }
        }
    }

    let foot_angles: &[f64] = match feet {
        0 => &[],
        1 => &[90.0],
        _ => &[50.0, 130.0],
    };
    for (index, degrees) in foot_angles.iter().enumerate() {
        let angle = rad(*degrees);
        shell.push(ellipse(
            &format!("foot{index}"),
            radius * 0.24,
            radius * 0.16,
            [
                round2(radius * 0.82 * angle.cos()),
                round2(radius * 0.82 * angle.sin()),
            ],
            rim,
            7,
        ));
    }

    let mut bones = vec![
        // The root is a bone in the list, like every other rig: `validate_rig`
        // resolves `parent` against `bones`, so a top-level "root" key would leave
        // `body` pointing at nothing.
        bone("root", None, [0.0, 0.0], 0.0, 0, vec![]),
        bone("body", Some("root"), [0.0, 0.0], 0.0, 10, shell),
    ];

    // ---- petals: the lid -------------------------------------------------
    // The rosette lives *inside* the shell: closed, it is a flower-shaped seam
    // that leaves the faceted pentagon silhouette visible (that is the "d12"
    // read); open, each blade swings out past the rim into a star.
    let rim_radius = radius * 0.5;
    let blade_len = radius * 0.62;
    let blade_w = radius * 0.26;
    let mut petal_ids = Vec::new();
    for index in 0..sides {
        let angle = rad(-90.0 + index as f64 * 360.0 / sides as f64);
        let pos = [
            round2(rim_radius * angle.cos()),
            round2(rim_radius * angle.sin()),
        ];
        // Closed: local +y points at the centre, so the blade lies over the
        // opening with its tip past the middle. Rotating a bone by `a` sends its
        // local +y to (-sin a, cos a), so "inward" at angle θ is θ + π/2 — not
        // θ + π, which aims the blade tangentially and turns the rosette into a
        // pinwheel.
        let closed_rotation = angle + std::f64::consts::FRAC_PI_2;
        let blade = [
            [0.0, -blade_w * 0.32],
            [blade_w, blade_w * 0.2],
            [blade_w * 0.64, blade_len * 0.7],
            [0.0, blade_len],
            [-blade_w * 0.64, blade_len * 0.7],
            [-blade_w, blade_w * 0.2],
        ];
        let shade = if index % 2 == 0 { petal_fill } else { facet };
        let mut parts = vec![polygon("blade", &blade, [0.0, 0.0], shade, 0)];
        // A vein down each blade: what makes the closed pod read as folded petals
        // instead of a flat star.
        parts.push(ellipse(
            "vein",
            blade_w * 0.16,
            blade_len * 0.58,
            [0.0, blade_len * 0.46],
            "#00000033",
            0,
        ));
        if index == 0 && charm {
            parts.push(ellipse(
                "charm",
                radius * 0.18,
                radius * 0.18,
                [0.0, blade_len * 0.44],
                accent,
                1,
            ));
        }
        let id = format!("petal{index}");
        // The petal bones sit above the shell and above anything in the well, so
        // a closed lid really covers its contents.
        bones.push(bone(
            &id,
            Some("body"),
            pos,
            closed_rotation,
            30 + index as i32,
            parts,
        ));
        petal_ids.push(id);
    }

    // ---- animation -------------------------------------------------------
    // Channels are built from the actual petal list: a six-petal pod animates
    // every blade, and a five-petal one never names a missing bone.
    let open = clip(
        560.0,
        false,
        vec![
            kf(0.0, petal_frame(&petal_ids, 0.0, 1.0, 0.0, rng)),
            // Overshoot and settle: an eager pod.
            kf(
                0.72,
                petal_frame(&petal_ids, -std::f64::consts::PI * 1.08, 1.04, 0.05, rng),
            ),
            kf(
                1.0,
                petal_frame(&petal_ids, -std::f64::consts::PI, 1.0, 0.02, rng),
            ),
        ],
    );
    let close = clip(
        480.0,
        false,
        vec![
            kf(
                0.0,
                petal_frame(&petal_ids, -std::f64::consts::PI, 1.0, 0.0, rng),
            ),
            kf(0.86, petal_frame(&petal_ids, 0.06, 0.97, 0.03, rng)),
            kf(1.0, petal_frame(&petal_ids, 0.0, 1.0, 0.0, rng)),
        ],
    );
    let appear = clip(
        420.0,
        false,
        vec![
            kf(0.0, json!({ "root": { "scale": 0.2, "rotation": -0.35 } })),
            kf(0.72, petal_frame(&petal_ids, 0.1, 1.0, 0.06, rng)),
            kf(1.0, petal_frame(&petal_ids, 0.0, 1.0, 0.0, rng)),
        ],
    );
    let vanish = clip(
        360.0,
        false,
        vec![
            kf(0.0, json!({ "root": { "scale": 1.0, "rotation": 0.0 } })),
            kf(0.6, json!({ "root": { "scale": 0.82, "rotation": 0.55 } })),
            kf(1.0, json!({ "root": { "scale": 0.0, "rotation": 1.3 } })),
        ],
    );

    json!({
        "format": 1,
        "name": "chest",
        "label": "Curiosity pod",
        "notes": format!(
            "Seeded pod: {sides} petals, facet style {facet_style}, face {}, {feet} feet{}.",
            if has_face { "yes" } else { "no" },
            if charm { ", charm" } else { "" }
        ),
        "structure": {
            "sides": sides,
            "facet": facet_style,
            "face": has_face,
            "feet": feet,
            "charm": charm,
            "blush": blush,
            "radius": round2(radius),
        },
        "bones": bones,
        "animations": {
            "appear": appear,
            "open": open,
            "close": close,
            "vanish": vanish,
        },
        "sockets": [
            { "id": "chest_interior", "bone": "body", "pos": [0.0, round2(-radius * 0.05)], "z": 26, "label": "Interior" },
            { "id": "chest_lid", "bone": "body", "pos": [0.0, round2(-radius * 0.6)], "z": 40, "label": "Lid" },
            { "id": "chest_side", "bone": "body", "pos": [round2(radius * 0.9), 0.0], "z": 14, "label": "Side" },
        ],
    })
}

fn bone(
    id: &str,
    parent: Option<&str>,
    pos: [f64; 2],
    rotation: f64,
    z: i32,
    parts: Vec<Value>,
) -> Value {
    json!({
        "id": id,
        "parent": parent,
        "pos": [round2(pos[0]), round2(pos[1])],
        "rotation": rotation,
        "scale": 1.0,
        "z": z,
        "parts": parts,
    })
}

/// A capsule hangs from its joint: `pivot` cancels, so the offset is the joint.
fn capsule(id: &str, width: f64, height: f64, offset: [f64; 2], color: &str, z: i32) -> Value {
    json!({
        "id": id,
        "shape": "capsule",
        "size": [round2(width), round2(height)],
        "pivot": [0.0, 0.5],
        "offset": [round2(offset[0]), round2(offset[1])],
        "color": color,
        "z": z,
    })
}

/// An ellipse is centred on its part origin, so `offset` is the centre.
fn ellipse(id: &str, width: f64, height: f64, center: [f64; 2], color: &str, z: i32) -> Value {
    json!({
        "id": id,
        "shape": "ellipse",
        "size": [round2(width), round2(height)],
        "pivot": [0.0, 0.0],
        "offset": [round2(center[0]), round2(center[1])],
        "color": color,
        "z": z,
    })
}

fn clip(duration_ms: f64, r#loop: bool, keyframes: Vec<Value>) -> Value {
    json!({
        "duration_ms": duration_ms,
        "loop": r#loop,
        "keyframes": keyframes,
    })
}

fn kf(t: f64, bones: Value) -> Value {
    json!({ "t": t, "bones": bones })
}

/* ------------------------------------------------------------------ *
 * tests
 * ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::defs::{find, PROPS};
    use std::collections::HashSet;

    fn structural(rig: &Value) -> String {
        rig.get("structure")
            .map(|value| value.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn the_same_seed_and_params_replay_byte_for_byte() {
        for def in PROPS {
            let a = build(def, 987_654_321, &json!({})).expect("build");
            let b = build(def, 987_654_321, &json!({})).expect("build");
            assert_eq!(a, b, "{} is not deterministic", def.id);
            // A different seed must move something.
            let c = build(def, 987_654_322, &json!({})).expect("build");
            assert_ne!(
                structural(&a),
                structural(&c),
                "{} ignores its seed",
                def.id
            );
        }
    }

    #[test]
    fn params_override_structure_deterministically() {
        let def = find("fishing_rod").expect("rod");
        let rig = build(
            def,
            5,
            &json!({ "segments": 4, "reel": false, "crude": true }),
        )
        .expect("build");
        let shaft_bones = rig["bones"]
            .as_array()
            .expect("bones")
            .iter()
            .filter(|bone| {
                bone["id"]
                    .as_str()
                    .map(|id| id.starts_with("shaft"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(shaft_bones, 4);
        assert!(rig["bones"]
            .as_array()
            .expect("bones")
            .iter()
            .all(|bone| bone["id"] != "reel"));
        assert_eq!(rig["structure"]["crude"], true);
    }

    /// Shape first, colour last: 100 seeds must vary *structurally*, not just in
    /// palette. This is the test the first metamorphosis generator lacked.
    #[test]
    fn a_hundred_seeds_vary_the_shape_not_just_the_colour() {
        for def in PROPS {
            let mut shapes = HashSet::new();
            for seed in 0..100u64 {
                let rig = build(def, seed * 7919 + 13, &json!({})).expect("build");
                shapes.insert(structural(&rig));
            }
            assert!(
                shapes.len() >= 60,
                "{}: only {} distinct structures across 100 seeds — that is a recolour, not variety",
                def.id,
                shapes.len()
            );
        }
    }

    #[test]
    fn every_generated_rig_is_loadable_and_has_its_advertised_clips() {
        for def in PROPS {
            let rig = build(def, 4242, &json!({})).expect("build");
            crate::models::validate_rig(&rig).expect("valid");
            let clips = rig["animations"].as_object().expect("clips");
            for name in def.animations {
                assert!(clips.contains_key(*name), "{}: missing clip {name}", def.id);
                let duration = clips[*name]["duration_ms"].as_f64().unwrap_or(0.0);
                assert!(duration > 0.0, "{}: clip {name} has no duration", def.id);
            }
        }
    }

    #[test]
    fn traits_are_deterministic_and_plausible() {
        for def in PROPS {
            let a = traits(def, 77);
            let b = traits(def, 77);
            assert_eq!(a, b, "{}: traits are not deterministic", def.id);
            assert!(!a.is_empty(), "{}: no traits", def.id);
            for (key, value) in &a {
                assert!(value.is_finite(), "{}: {key} is not finite", def.id);
            }
        }
        let rod = traits(find("fishing_rod").unwrap(), 77);
        let luck = rod["bite_luck"];
        assert!(
            (0.0..=0.18).contains(&luck),
            "bite_luck out of range: {luck}"
        );
    }

    #[test]
    fn the_rod_hangs_its_line_down_and_keeps_the_bobber_upright() {
        let rig = build(find("fishing_rod").unwrap(), 11, &json!({})).expect("build");
        let bones = rig["bones"].as_array().expect("bones");
        let line = bones.iter().find(|b| b["id"] == "line").expect("line");
        let bobber = bones.iter().find(|b| b["id"] == "bobber").expect("bobber");
        let line_rot = line["rotation"].as_f64().unwrap();
        let bobber_rot = bobber["rotation"].as_f64().unwrap();
        assert!(line_rot > 1.0, "the line should hang down, got {line_rot}");
        assert!(
            (line_rot + bobber_rot).abs() < 0.01,
            "the bobber must stay upright relative to the line"
        );
    }
}
