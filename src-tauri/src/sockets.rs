//! Semantic attachment points ("sockets") on a rig.
//!
//! An item or prop is never welded to a hard-coded coordinate: it attaches to a
//! **named** socket (`hand_primary`, `head_helmet`, `chest_interior`, …), and the
//! socket resolves to a bone plus a local transform. That indirection is what
//! lets the same fishing rod live in a chest, in a paw, or on the ground without
//! the renderer or the plugin knowing which.
//!
//! # Where the definitions come from
//!
//! In order of precedence, first one wins per id:
//!
//! 1. `<model>/sockets.json` — a sidecar next to `rig.json`. Hand-authored, and
//!    the only place the editor cannot clobber (see below).
//! 2. a `sockets` array inside `model.json` / `rig.json` — what the procedural
//!    generator writes. `models::validate_rig` deliberately accepts unknown root
//!    keys, so this loads today.
//! 3. **derived** from the bones — the reason ~15 shipped rigs and every
//!    generated `buddy-*` creature gets usable sockets without being edited.
//!
//! The sidecar exists because `ui/js/rig.js::serializeRig` only re-emits
//! `format/name/label/notes/animations/bones`: a `sockets` block that lived in
//! `model.json` would be silently deleted the first time someone dragged a part
//! in the editor. `rig.json` is not an option either — it is the override the
//! editor writes.
//!
//! # Derivation, and why it is not "the bone called hand"
//!
//! The shipped rigs do not agree on vocabulary. `xenomorph-7` is thirteen bones
//! and four tentacles with no arms and no head at all; a generated creature is
//! `arm_r → arm2_r → arm3_r` with the paw on the *distal* bone; `demo-blob` has
//! `arm_l → arm_l_hand` but a bare `arm_r`; and a work-in-progress stub can name
//! its legs `arm_r`/`arm_l`. So the resolver scores candidates instead of
//! matching one id:
//!
//! * an arm/tentacle bone beats a leaf bone,
//! * the declared side (`_r`, `_l`, `left…`, `right…`) beats an inferred one,
//! * deeper in its chain beats shallower (the paw, not the shoulder),
//! * the rest position on the expected side beats the other side,
//! * `id` breaks the last tie so the result is deterministic.
//!
//! A rig with only one arm still gets both `hand_primary` and `hand_secondary`:
//! the second hand is then the next best candidate, not a missing socket, because
//! "the other paw" is a more useful answer than "no such thing".
//!
//! Bad explicit entries are skipped with a warning rather than failing the load.
//! A stale socket after a rig edit must not brick the avatar, and the heuristics
//! are always there as a fallback.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

/// Sidecar file naming the sockets of one model folder.
pub const SIDECAR_FILE: &str = "sockets.json";

pub const HAND_PRIMARY: &str = "hand_primary";
pub const HAND_SECONDARY: &str = "hand_secondary";
pub const HEAD_HELMET: &str = "head_helmet";
pub const NECK_COLLAR: &str = "neck_collar";
pub const CHEST_FRONT: &str = "chest_front";
pub const CHEST_BACK: &str = "chest_back";
pub const BACK: &str = "back";
pub const STAGE_GROUND: &str = "stage_ground";
pub const PERCH_SURFACE: &str = "perch_surface";

/// The order sockets are reported in: the vocabulary reads front-to-back, hands
/// first, virtual surfaces last. Anything explicit but unknown follows,
/// alphabetically, so a plugin's custom socket is never dropped.
const CANONICAL_ORDER: &[&str] = &[
    HAND_PRIMARY,
    HAND_SECONDARY,
    HEAD_HELMET,
    NECK_COLLAR,
    CHEST_FRONT,
    CHEST_BACK,
    BACK,
    STAGE_GROUND,
    PERCH_SURFACE,
];

/// One attachment point, resolved against a specific rig.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Socket {
    pub id: String,
    /// The bone the socket hangs from. `None` for a virtual surface
    /// ([`STAGE_GROUND`], [`PERCH_SURFACE`]) that the page resolves from the
    /// avatar's own position instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// Local offset from the bone joint, in the bone's rotated frame.
    pub pos: [f64; 2],
    pub rotation: f64,
    pub scale: f64,
    /// Draw-order band. Lower draws behind the body: `back` is 40,
    /// `head_helmet` 380, `hand_primary` 420. The page adds the part's own `z`.
    pub z: i32,
    /// What may be attached: matched against a prop definition's tags. An empty
    /// list means "anything".
    pub accepts: Vec<String>,
    /// The socket on the other side of the body, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
    pub label: String,
    /// `"bone"` or `"virtual"`.
    pub kind: &'static str,
    /// How this socket came to exist: `"sidecar"`, `"manifest"` or `"derived"`.
    pub source: &'static str,
}

/* ------------------------------------------------------------------ *
 * Bone table
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone)]
struct BoneInfo {
    parent: Option<String>,
    pos: [f64; 2],
    rotation: f64,
    scale: f64,
    tags: Vec<String>,
    depth: usize,
}

fn pair(value: Option<&Value>) -> [f64; 2] {
    let list = value.and_then(Value::as_array);
    let get = |at: usize| {
        list.and_then(|items| items.get(at))
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite())
            .unwrap_or(0.0)
    };
    [get(0), get(1)]
}

fn number(value: Option<&Value>, fallback: f64) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .unwrap_or(fallback)
}

fn collect_bones(rig: &Value) -> BTreeMap<String, BoneInfo> {
    let mut bones: BTreeMap<String, BoneInfo> = BTreeMap::new();
    let Some(list) = rig.get("bones").and_then(Value::as_array) else {
        return bones;
    };
    for bone in list {
        let Some(id) = bone.get("id").and_then(Value::as_str) else {
            continue;
        };
        let tags = bone
            .get("parts")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.get("tag").and_then(Value::as_str))
                    .map(|tag| tag.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        bones.insert(
            id.to_string(),
            BoneInfo {
                parent: bone
                    .get("parent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pos: pair(bone.get("pos")),
                rotation: number(bone.get("rotation"), 0.0),
                scale: number(bone.get("scale"), 1.0).abs().max(0.0001),
                tags,
                depth: 0,
            },
        );
    }

    // Chain depth, for "the paw beats the shoulder". A stray cycle (validation
    // rejects those, but this also runs on unvalidated input) stops at 64.
    for id in bones.keys().cloned().collect::<Vec<_>>() {
        let mut depth = 0usize;
        let mut cursor = bones.get(&id).and_then(|bone| bone.parent.clone());
        while let Some(parent) = cursor {
            if depth >= 64 {
                break;
            }
            depth += 1;
            cursor = bones.get(&parent).and_then(|bone| bone.parent.clone());
        }
        if let Some(bone) = bones.get_mut(&id) {
            bone.depth = depth;
        }
    }
    bones
}

/// Rest-pose position of every joint, relative to the avatar — the same
/// arithmetic `ui/js/rig.js::computeWorld` runs with the avatar at the origin.
fn bind_pose(bones: &BTreeMap<String, BoneInfo>) -> BTreeMap<String, (f64, f64)> {
    // (x, y, rotation, scale) while resolving: a child's position depends on its
    // parent's rotation and scale, so the intermediate pass has to carry both.
    let mut placed: BTreeMap<String, (f64, f64, f64, f64)> = BTreeMap::new();
    let mut guard = 0usize;
    while placed.len() < bones.len() && guard < bones.len() + 8 {
        guard += 1;
        for (id, info) in bones {
            if placed.contains_key(id) {
                continue;
            }
            let parent = match &info.parent {
                Some(parent) => match placed.get(parent) {
                    Some(place) => Some(*place),
                    // Parent exists but is not placed yet (or is missing): wait a
                    // pass, and fall back to the local position for a dangling id.
                    None if bones.contains_key(parent) => continue,
                    None => None,
                },
                None => None,
            };
            let (px, py, prot, pscale) = match parent {
                Some((x, y, rot, scale)) => (x, y, rot, scale),
                None => (0.0, 0.0, 0.0, 1.0),
            };
            let cos = prot.cos();
            let sin = prot.sin();
            placed.insert(
                id.clone(),
                (
                    px + (info.pos[0] * cos - info.pos[1] * sin) * pscale,
                    py + (info.pos[0] * sin + info.pos[1] * cos) * pscale,
                    prot + info.rotation,
                    pscale * info.scale,
                ),
            );
        }
    }
    for (id, info) in bones {
        placed
            .entry(id.clone())
            .or_insert((info.pos[0], info.pos[1], info.rotation, info.scale));
    }
    placed
        .into_iter()
        .map(|(id, (x, y, _, _))| (id, (x, y)))
        .collect()
}

/* ------------------------------------------------------------------ *
 * Classification
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Arm,
    Tentacle,
    Wing,
    Head,
    Neck,
    Torso,
    Leg,
    Foot,
    Tail,
    Other,
}

/// Side from the id's tokens, then its prefix, then its part tags. `None` means
/// "not declared" — the caller may infer one from the rest position.
fn declared_side(id: &str, tags: &[String]) -> Option<Side> {
    let lower = id.to_ascii_lowercase();
    for token in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        match token {
            "l" | "left" => return Some(Side::Left),
            "r" | "right" => return Some(Side::Right),
            _ => {}
        }
    }
    if lower.starts_with("left") {
        return Some(Side::Left);
    }
    if lower.starts_with("right") {
        return Some(Side::Right);
    }
    for tag in tags {
        if tag.ends_with("_left") || tag == "left" {
            return Some(Side::Left);
        }
        if tag.ends_with("_right") || tag == "right" {
            return Some(Side::Right);
        }
    }
    None
}

fn family(id: &str, tags: &[String]) -> Family {
    let lower = id.to_ascii_lowercase();
    let first = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("");

    if lower.contains("head")
        || tags
            .iter()
            .any(|tag| matches!(tag.as_str(), "head" | "eye" | "mouth" | "brow" | "pupil" | "face"))
    {
        return Family::Head;
    }
    if lower.contains("foot") {
        return Family::Foot;
    }
    if lower.contains("leg") || lower.contains("thigh") || lower.contains("shin") {
        return Family::Leg;
    }
    if lower.contains("tail") {
        return Family::Tail;
    }
    if lower.contains("wing") {
        return Family::Wing;
    }
    if lower.contains("neck") {
        return Family::Neck;
    }
    if lower.contains("tentacle") {
        return Family::Tentacle;
    }
    if first.starts_with("arm")
        || first.starts_with("hand")
        || first.starts_with("forearm")
        || first == "claw"
        || first == "paw"
        || lower.ends_with("_arm")
        || lower.ends_with("_hand")
        || tags
            .iter()
            .any(|tag| tag.starts_with("arm") || tag.starts_with("hand"))
    {
        return Family::Arm;
    }
    if lower.contains("torso") || lower.contains("chest") || lower == "body" {
        return Family::Torso;
    }
    Family::Other
}

fn has_children(id: &str, bones: &BTreeMap<String, BoneInfo>) -> bool {
    bones
        .values()
        .any(|bone| bone.parent.as_deref() == Some(id))
}

/* ------------------------------------------------------------------ *
 * Candidate scoring
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone)]
struct Candidate {
    id: String,
    weight: u8,
    side: Option<Side>,
    depth: usize,
    x: f64,
    y: f64,
    spread: f64,
}

fn hand_pool(bones: &BTreeMap<String, BoneInfo>, places: &BTreeMap<String, (f64, f64)>) -> Vec<Candidate> {
    let root_x = bones
        .iter()
        .find(|(_, bone)| bone.parent.is_none())
        .and_then(|(id, _)| places.get(id))
        .map(|place| place.0)
        .unwrap_or(0.0);

    bones
        .iter()
        .filter_map(|(id, bone)| {
            let weight = match family(id, &bone.tags) {
                Family::Arm => 3,
                Family::Tentacle => 2,
                // A wing tip or a plain leaf is a last resort: better than no
                // socket, never preferred over a limb.
                Family::Wing | Family::Other => {
                    if has_children(id, bones) {
                        return None;
                    }
                    1
                }
                _ => return None,
            };
            let (x, y) = places.get(id).copied().unwrap_or((0.0, 0.0));
            let side = declared_side(id, &bone.tags).or_else(|| {
                if (x - root_x).abs() < 1.0 {
                    None
                } else if x > root_x {
                    Some(Side::Right)
                } else {
                    Some(Side::Left)
                }
            });
            Some(Candidate {
                id: id.clone(),
                weight,
                side,
                depth: bone.depth,
                x,
                y,
                spread: x - root_x,
            })
        })
        .collect()
}

fn side_rank(side: Option<Side>, prefer: Side) -> u8 {
    match side {
        Some(value) if value == prefer => 2,
        None => 1,
        _ => 0,
    }
}

fn distance(candidate: &Candidate, from: Option<(f64, f64)>) -> f64 {
    match from {
        Some((x, y)) => ((candidate.x - x).powi(2) + (candidate.y - y).powi(2)).sqrt(),
        None => candidate.spread.abs(),
    }
}

/// Highest-scoring unused candidate, preferring `prefer` on ties.
///
/// The comparison order is deliberate: limb family first (a paw beats a leaf even
/// on the "wrong" side), then the declared side, then chain depth, then the rest
/// position, then the id for determinism.
fn best_candidate(
    pool: &[Candidate],
    used: &HashSet<String>,
    prefer: Side,
    from: Option<(f64, f64)>,
) -> Option<Candidate> {
    let mut candidates: Vec<&Candidate> = pool.iter().filter(|item| !used.contains(&item.id)).collect();
    candidates.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| side_rank(b.side, prefer).cmp(&side_rank(a.side, prefer)))
            .then_with(|| b.depth.cmp(&a.depth))
            .then_with(|| {
                distance(b, from)
                    .partial_cmp(&distance(a, from))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    candidates.first().map(|item| (*item).clone())
}

/* ------------------------------------------------------------------ *
 * Socket construction
 * ------------------------------------------------------------------ */

fn list(value: &'static [&'static str]) -> Vec<String> {
    value.iter().map(|item| item.to_string()).collect()
}

fn bone_socket(
    id: &str,
    bone: &str,
    pos: [f64; 2],
    z: i32,
    accepts: Vec<String>,
    mirror: Option<&str>,
    label: &str,
    source: &'static str,
) -> Socket {
    Socket {
        id: id.to_string(),
        bone: Some(bone.to_string()),
        pos,
        rotation: 0.0,
        scale: 1.0,
        z,
        accepts,
        mirror: mirror.map(str::to_string),
        label: label.to_string(),
        kind: "bone",
        source,
    }
}

fn virtual_socket(id: &str, label: &str) -> Socket {
    Socket {
        id: id.to_string(),
        bone: None,
        pos: [0.0, 0.0],
        rotation: 0.0,
        scale: 1.0,
        z: 200,
        accepts: Vec::new(),
        mirror: None,
        label: label.to_string(),
        kind: "virtual",
        source: "derived",
    }
}

/// Does this bone carry a `neck`/`collar` id? (`family` reports Neck already;
/// this keeps the preferred-id ordering in one place.)
fn neck_bone(bones: &BTreeMap<String, BoneInfo>) -> Option<String> {
    let mut matches: Vec<(&String, usize)> = bones
        .iter()
        .filter(|(id, bone)| {
            let lower = id.to_ascii_lowercase();
            lower.contains("neck") || lower.contains("collar") || family(id, &bone.tags) == Family::Neck
        })
        .map(|(id, bone)| (id, bone.depth))
        .collect();
    matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    matches.first().map(|(id, _)| (*id).clone())
}

fn head_bone(bones: &BTreeMap<String, BoneInfo>) -> Option<String> {
    let mut matches: Vec<(&String, bool, usize)> = bones
        .iter()
        .filter(|(id, bone)| family(id, &bone.tags) == Family::Head)
        .map(|(id, bone)| {
            (
                id,
                id.to_ascii_lowercase().contains("head"),
                bone.depth,
            )
        })
        .collect();
    // Prefer a bone actually named `head*`, then the deepest, then the id.
    matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)).then_with(|| a.0.cmp(b.0)));
    matches.first().map(|(id, _, _)| (*id).clone())
}

fn torso_bone(bones: &BTreeMap<String, BoneInfo>) -> Option<String> {
    let mut matches: Vec<(&String, u8, usize)> = bones
        .iter()
        .filter(|(id, bone)| family(id, &bone.tags) == Family::Torso)
        .map(|(id, bone)| {
            let lower = id.to_ascii_lowercase();
            let rank = if lower.contains("torso") {
                2
            } else if lower.contains("chest") {
                1
            } else {
                0
            };
            (id, rank, bone.depth)
        })
        .collect();
    matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)).then_with(|| a.0.cmp(b.0)));
    matches.first().map(|(id, _, _)| (*id).clone())
}

fn root_bone(bones: &BTreeMap<String, BoneInfo>) -> Option<String> {
    if bones.contains_key("root") {
        return Some("root".to_string());
    }
    let mut roots: Vec<&String> = bones
        .iter()
        .filter(|(_, bone)| bone.parent.is_none())
        .map(|(id, _)| id)
        .collect();
    roots.sort();
    roots.first().map(|id| (*id).clone())
}

/* ------------------------------------------------------------------ *
 * Explicit definitions
 * ------------------------------------------------------------------ */

fn explicit_entries(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        other => {
            log::warn!("sockets: expected an array, found {}", json_kind(other));
            Vec::new()
        }
    }
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn merge_explicit(
    out: &mut Vec<Socket>,
    seen: &mut HashSet<String>,
    value: &Value,
    source: &'static str,
    bones: &BTreeMap<String, BoneInfo>,
) {
    for entry in explicit_entries(value) {
        let Some(object) = entry.as_object() else {
            log::warn!("sockets: an entry is not an object; skipped");
            continue;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            log::warn!("sockets: an entry has no string `id`; skipped");
            continue;
        };
        if !seen.insert(id.to_string()) {
            log::warn!("sockets: duplicate id {id:?} ignored (first definition wins)");
            continue;
        }

        let bone = object.get("bone").and_then(Value::as_str).map(str::trim).filter(|bone| !bone.is_empty());
        if let Some(bone) = bone {
            if !bones.contains_key(bone) {
                log::warn!("sockets: {id:?} points at missing bone {bone:?}; skipped");
                seen.remove(id);
                continue;
            }
        }
        let pos = pair(object.get("pos"));
        let rotation = number(object.get("rotation"), 0.0);
        let scale = number(object.get("scale"), 1.0).abs().max(0.0001);
        let z = object
            .get("z")
            .and_then(Value::as_i64)
            .unwrap_or(if bone.is_some() { 380 } else { 200 })
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let accepts = object
            .get("accepts")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mirror = object
            .get("mirror")
            .and_then(Value::as_str)
            .map(str::to_string);
        let label = object
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        out.push(Socket {
            id: id.to_string(),
            bone: bone.map(str::to_string),
            pos,
            rotation,
            scale,
            z,
            accepts,
            mirror,
            label,
            kind: if bone.is_some() { "bone" } else { "virtual" },
            source,
        });
    }
}

/* ------------------------------------------------------------------ *
 * Resolution
 * ------------------------------------------------------------------ */

/// Resolve the socket table for `rig`, with an optional sidecar document.
///
/// Never fails: an unusable explicit entry is logged and skipped, and the
/// derived sockets fill every gap. A rig with no bones at all returns an empty
/// list (the virtual surfaces have nothing to hang off).
pub fn resolve(rig: &Value, sidecar: Option<&Value>) -> Vec<Socket> {
    let bones = collect_bones(rig);
    if bones.is_empty() {
        return Vec::new();
    }
    let places = bind_pose(&bones);

    let mut out: Vec<Socket> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(value) = sidecar {
        merge_explicit(&mut out, &mut seen, value, "sidecar", &bones);
    }
    if let Some(value) = rig.get("sockets") {
        merge_explicit(&mut out, &mut seen, value, "manifest", &bones);
    }

    // A derived socket must not reuse a bone an explicit one already claimed:
    // two things hanging off one joint is almost always an authoring mistake.
    let mut used: HashSet<String> = out
        .iter()
        .filter_map(|socket| socket.bone.clone())
        .collect();

    let pool = hand_pool(&bones, &places);

    if !seen.contains(HAND_PRIMARY) {
        if let Some(chosen) = best_candidate(&pool, &used, Side::Right, None) {
            used.insert(chosen.id.clone());
            out.push(bone_socket(
                HAND_PRIMARY,
                &chosen.id,
                [0.0, 0.0],
                420,
                list(&["rod", "tool", "bag"]),
                Some(HAND_SECONDARY),
                "Primary hand",
                "derived",
            ));
            seen.insert(HAND_PRIMARY.to_string());
        }
    }

    if !seen.contains(HAND_SECONDARY) {
        // Prefer the mirrored side, and among several candidates the one
        // furthest from the primary hand — that is what makes a two-tentacle rig
        // give you opposing tentacles instead of the same one twice.
        let anchor = out
            .iter()
            .find(|socket| socket.id == HAND_PRIMARY)
            .and_then(|socket| socket.bone.as_ref())
            .and_then(|bone| places.get(bone))
            .copied();
        if let Some(chosen) = best_candidate(&pool, &used, Side::Left, anchor) {
            used.insert(chosen.id.clone());
            out.push(bone_socket(
                HAND_SECONDARY,
                &chosen.id,
                [0.0, 0.0],
                420,
                list(&["rod", "tool", "bag"]),
                Some(HAND_PRIMARY),
                "Secondary hand",
                "derived",
            ));
            seen.insert(HAND_SECONDARY.to_string());
        }
    }

    if !seen.contains(HEAD_HELMET) {
        if let Some(bone) = head_bone(&bones) {
            out.push(bone_socket(
                HEAD_HELMET,
                &bone,
                [0.0, 0.0],
                380,
                list(&["helmet", "hat"]),
                None,
                "Head",
                "derived",
            ));
            seen.insert(HEAD_HELMET.to_string());
        }
    }

    if !seen.contains(NECK_COLLAR) {
        if let Some(bone) = neck_bone(&bones) {
            out.push(bone_socket(
                NECK_COLLAR,
                &bone,
                [0.0, 0.0],
                120,
                list(&["collar", "scarf"]),
                None,
                "Neck",
                "derived",
            ));
            seen.insert(NECK_COLLAR.to_string());
        }
    }

    if let Some(bone) = torso_bone(&bones) {
        if !seen.contains(CHEST_FRONT) {
            out.push(bone_socket(
                CHEST_FRONT,
                &bone,
                [0.0, 0.0],
                340,
                list(&["badge", "lamp"]),
                None,
                "Chest (front)",
                "derived",
            ));
            seen.insert(CHEST_FRONT.to_string());
        }
        if !seen.contains(CHEST_BACK) {
            out.push(bone_socket(
                CHEST_BACK,
                &bone,
                [0.0, 0.0],
                60,
                list(&["bag", "pack"]),
                None,
                "Chest (back)",
                "derived",
            ));
            seen.insert(CHEST_BACK.to_string());
        }
    }

    if !seen.contains(BACK) {
        if let Some(bone) = root_bone(&bones) {
            out.push(bone_socket(
                BACK,
                &bone,
                [0.0, -6.0],
                40,
                list(&["pack", "bag", "tool"]),
                None,
                "Back",
                "derived",
            ));
            seen.insert(BACK.to_string());
        }
    }

    // The two surfaces that are not a bone at all. `stage_ground` resolves from
    // the avatar's feet, `perch_surface` from the window edge it is perched on.
    for (id, label) in [(STAGE_GROUND, "Ground"), (PERCH_SURFACE, "Perch edge")] {
        if seen.insert(id.to_string()) {
            out.push(virtual_socket(id, label));
        }
    }

    out.sort_by_key(|socket| {
        let rank = CANONICAL_ORDER
            .iter()
            .position(|known| *known == socket.id)
            .unwrap_or(CANONICAL_ORDER.len());
        (rank, socket.id.clone())
    });
    out
}

/// Read a model folder's `sockets.json`, if it has one.
///
/// A malformed sidecar is a warning, never a load failure: the derived table is
/// a complete answer on its own, and the avatar should not go missing because a
/// hand-written file has a trailing comma.
pub fn load_sidecar(dir: &Path) -> Option<Value> {
    let path = dir.join(SIDECAR_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => {
            let entries = value
                .get("sockets")
                .cloned()
                .or_else(|| value.as_array().cloned().map(Value::Array));
            match entries {
                Some(entries) => Some(entries),
                None => {
                    log::warn!(
                        "{}: expected an array or {{\"sockets\": [...]}}; using derived sockets",
                        path.display()
                    );
                    None
                }
            }
        }
        Err(err) => {
            log::warn!(
                "{}: invalid JSON ({err}); using derived sockets",
                path.display()
            );
            None
        }
    }
}

/// The socket with this id, if the rig has one.
pub fn find<'a>(sockets: &'a [Socket], id: &str) -> Option<&'a Socket> {
    sockets.iter().find(|socket| socket.id == id)
}

/// Every socket id the vocabulary knows, for docs and tests.
pub fn canonical_ids() -> &'static [&'static str] {
    CANONICAL_ORDER
}

/* ------------------------------------------------------------------ *
 * Tests
 * ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rig(bones: Value) -> Value {
        json!({ "format": "desktop-buddy/model@1", "name": "test", "bones": bones })
    }

    fn bone(id: &str, parent: Value, pos: [f64; 2]) -> Value {
        json!({ "id": id, "parent": parent, "pos": pos })
    }

    fn generated_rig() -> Value {
        rig(json!([
            bone("root", Value::Null, [0.0, 0.0]),
            bone("pelvis", json!("root"), [0.0, 10.0]),
            bone("torso", json!("pelvis"), [0.0, -50.0]),
            bone("neck", json!("torso"), [0.0, -20.0]),
            bone("head", json!("neck"), [0.0, -18.0]),
            bone("arm_r", json!("torso"), [46.0, -25.0]),
            bone("arm2_r", json!("arm_r"), [24.0, 0.0]),
            bone("arm3_r", json!("arm2_r"), [26.0, 0.0]),
            bone("arm_l", json!("torso"), [-46.0, -25.0]),
            bone("arm2_l", json!("arm_l"), [24.0, 0.0]),
            bone("arm3_l", json!("arm2_l"), [26.0, 0.0]),
        ]))
    }

    fn socket_ids(sockets: &[Socket]) -> Vec<&str> {
        sockets.iter().map(|socket| socket.id.as_str()).collect()
    }

    #[test]
    fn the_distal_arm_bone_is_the_hand_not_the_shoulder() {
        let sockets = resolve(&generated_rig(), None);
        assert_eq!(find(&sockets, HAND_PRIMARY).and_then(|s| s.bone.as_deref()), Some("arm3_r"));
        assert_eq!(find(&sockets, HAND_SECONDARY).and_then(|s| s.bone.as_deref()), Some("arm3_l"));
    }

    #[test]
    fn head_neck_and_chest_are_derived_from_the_bone_vocabulary() {
        let sockets = resolve(&generated_rig(), None);
        assert_eq!(find(&sockets, HEAD_HELMET).and_then(|s| s.bone.as_deref()), Some("head"));
        assert_eq!(find(&sockets, NECK_COLLAR).and_then(|s| s.bone.as_deref()), Some("neck"));
        assert_eq!(find(&sockets, CHEST_FRONT).and_then(|s| s.bone.as_deref()), Some("torso"));
        assert_eq!(find(&sockets, CHEST_BACK).and_then(|s| s.bone.as_deref()), Some("torso"));
        assert_eq!(find(&sockets, BACK).and_then(|s| s.bone.as_deref()), Some("root"));
    }

    #[test]
    fn legs_are_never_mistaken_for_hands() {
        // The nastier half of the stub problem: real legs next to real arms.
        let stub = rig(json!([
            bone("root", Value::Null, [0.0, 0.0]),
            bone("torso", json!("root"), [0.0, -20.0]),
            bone("arm_r", json!("torso"), [18.0, 0.0]),
            bone("arm_l", json!("torso"), [-18.0, 0.0]),
            bone("leg_r", json!("root"), [10.0, 30.0]),
            bone("foot_r", json!("leg_r"), [0.0, 26.0]),
            bone("leg_l", json!("root"), [-10.0, 30.0]),
            bone("foot_l", json!("leg_l"), [0.0, 26.0]),
        ]));
        let sockets = resolve(&stub, None);
        let used: Vec<&str> = sockets
            .iter()
            .filter(|socket| socket.id.starts_with("hand_"))
            .filter_map(|socket| socket.bone.as_deref())
            .collect();
        assert_eq!(used, vec!["arm_r", "arm_l"]);
    }

    #[test]
    fn a_rig_with_no_arms_still_gets_two_distinct_hands() {
        // `xenomorph-7` is exactly this shape: four tentacles, no arms, no head.
        let alien = rig(json!([
            bone("root", Value::Null, [0.0, 0.0]),
            bone("tentacle_1_base", json!("root"), [268.0, 128.0]),
            bone("tentacle_1_mid", json!("tentacle_1_base"), [150.0, 0.0]),
            bone("tentacle_1_tip", json!("tentacle_1_mid"), [150.0, 0.0]),
            bone("tentacle_2_base", json!("root"), [128.0, 268.0]),
            bone("tentacle_2_mid", json!("tentacle_2_base"), [150.0, 0.0]),
            bone("tentacle_2_tip", json!("tentacle_2_mid"), [150.0, 0.0]),
            bone("tentacle_3_base", json!("root"), [-12.0, 128.0]),
            bone("tentacle_3_mid", json!("tentacle_3_base"), [150.0, 0.0]),
            bone("tentacle_3_tip", json!("tentacle_3_mid"), [150.0, 0.0]),
        ]));
        let sockets = resolve(&alien, None);
        let primary = find(&sockets, HAND_PRIMARY).expect("a hand socket must resolve");
        let secondary = find(&sockets, HAND_SECONDARY).expect("a second hand socket must resolve");
        assert!(primary.bone.is_some() && secondary.bone.is_some());
        assert_ne!(primary.bone, secondary.bone, "both hands on one bone is useless");
        assert!(find(&sockets, HEAD_HELMET).is_none(), "no head bone, no helmet socket");
    }

    #[test]
    fn explicit_sockets_win_and_bad_ones_are_skipped() {
        let sidecar = json!([
            { "id": "hand_primary", "bone": "arm2_r", "pos": [3, -1], "rotation": 0.4, "z": 500,
              "accepts": ["rod"], "label": "Right paw" },
            { "id": "ghost", "bone": "does_not_exist" },
            { "id": "tail_grip", "bone": "arm3_l" }
        ]);
        let sockets = resolve(&generated_rig(), Some(&sidecar));
        let primary = find(&sockets, HAND_PRIMARY).unwrap();
        assert_eq!(primary.bone.as_deref(), Some("arm2_r"));
        assert_eq!(primary.source, "sidecar");
        assert_eq!(primary.z, 500);
        assert_eq!(primary.pos, [3.0, -1.0]);
        assert!(find(&sockets, "ghost").is_none(), "a missing bone is skipped, not guessed");
        assert_eq!(find(&sockets, "tail_grip").and_then(|s| s.bone.as_deref()), Some("arm3_l"));
        // The derived secondary still exists, and it may not reuse a bone an
        // explicit socket already claimed.
        let secondary = find(&sockets, HAND_SECONDARY).expect("secondary is still derived");
        assert_eq!(secondary.source, "derived");
        assert_ne!(secondary.bone.as_deref(), Some("arm3_l"));
        assert_ne!(secondary.bone.as_deref(), Some("arm2_r"));
    }

    #[test]
    fn a_manifest_socket_block_is_read_when_there_is_no_sidecar() {
        let mut rig = generated_rig();
        rig["sockets"] = json!([{ "id": "head_helmet", "bone": "arm_r", "label": "Held above" }]);
        let sockets = resolve(&rig, None);
        let helmet = find(&sockets, HEAD_HELMET).unwrap();
        assert_eq!(helmet.bone.as_deref(), Some("arm_r"));
        assert_eq!(helmet.source, "manifest");
    }

    #[test]
    fn duplicate_ids_keep_the_first_definition() {
        let sidecar = json!([
            { "id": "hand_primary", "bone": "arm_r" },
            { "id": "hand_primary", "bone": "arm_l" }
        ]);
        let sockets = resolve(&generated_rig(), Some(&sidecar));
        assert_eq!(find(&sockets, HAND_PRIMARY).and_then(|s| s.bone.as_deref()), Some("arm_r"));
    }

    #[test]
    fn virtual_surfaces_are_always_reported_and_last() {
        let sockets = resolve(&generated_rig(), None);
        let ids = socket_ids(&sockets);
        assert!(ids.contains(&STAGE_GROUND));
        assert!(ids.contains(&PERCH_SURFACE));
        assert_eq!(ids.last().copied(), Some(PERCH_SURFACE));
        for id in [STAGE_GROUND, PERCH_SURFACE] {
            let socket = find(&sockets, id).unwrap();
            assert_eq!(socket.kind, "virtual");
            assert!(socket.bone.is_none());
        }
    }

    #[test]
    fn the_order_is_canonical_with_custom_sockets_last() {
        let sidecar = json!([{ "id": "aaa_custom", "bone": "arm_r" }]);
        let sockets = resolve(&generated_rig(), Some(&sidecar));
        let ids = socket_ids(&sockets);
        assert_eq!(
            ids,
            vec![
                "hand_primary",
                "hand_secondary",
                "head_helmet",
                "neck_collar",
                "chest_front",
                "chest_back",
                "back",
                "stage_ground",
                "perch_surface",
                "aaa_custom",
            ]
        );
    }

    #[test]
    fn a_bone_without_a_bones_array_yields_nothing() {
        assert!(resolve(&json!({ "name": "empty" }), None).is_empty());
        assert!(resolve(&json!({ "bones": [] }), None).is_empty());
    }

    /// The real guarantee: every rig shipped in the tree can hold something.
    /// Generated `buddy-*` creatures are runtime output (gitignored), so CI sees
    /// the hand-authored rigs and this test still proves the vocabulary maps.
    #[test]
    fn every_shipped_model_resolves_a_primary_hand() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../models");
        if !root.is_dir() {
            return;
        }
        let mut checked = 0usize;
        let mut entries: Vec<_> = std::fs::read_dir(&root)
            .expect("read models root")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        entries.sort();
        for dir in entries {
            let manifest = dir.join("model.json");
            if !manifest.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(rig) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if rig.get("bones").and_then(Value::as_array).map(Vec::is_empty).unwrap_or(true) {
                continue;
            }
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            let sidecar = load_sidecar(&dir);
            let sockets = resolve(&rig, sidecar.as_ref());
            let primary = find(&sockets, HAND_PRIMARY)
                .unwrap_or_else(|| panic!("{name}: no hand_primary socket"));
            let bone = primary.bone.as_deref().unwrap_or("");
            assert!(
                rig.get("bones")
                    .and_then(Value::as_array)
                    .map(|bones| bones.iter().any(|b| b.get("id").and_then(Value::as_str) == Some(bone)))
                    .unwrap_or(false),
                "{name}: hand_primary points at {bone:?}, which is not a bone"
            );
            assert!(
                find(&sockets, HAND_SECONDARY).is_some(),
                "{name}: no hand_secondary socket"
            );
            checked += 1;
        }
        assert!(checked >= 5, "expected the shipped rigs to be checked, saw {checked}");
    }
}
