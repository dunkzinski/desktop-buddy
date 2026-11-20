//! The `desktop-buddy/model@1` shape: typed bones/parts/clips, the mirroring
//! formulas from `MODEL_FORMAT.md`, and a forward-kinematics pass the generator
//! (and the tests) can verify against.
//!
//! Nothing here knows about archetypes or evolution — it is only the format.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// The root bone's scale. Every local pixel below is multiplied by this, which
/// is what keeps a large rig inside the ~170 px top-clip budget.
pub const ROOT_SCALE: f64 = 0.55;
/// Highest world-space pixel a rig may paint (`min_world_y >= -170`).
pub const MONITOR_BUDGET_Y: f64 = -170.0;
/// Rigs are rejected above this many parts; the generator stays well under it.
pub const MAX_PARTS: usize = 100;

/* ------------------------------------------------------------------ *
 * Types
 * ------------------------------------------------------------------ */

/// Per-bone idle animation amplitudes (`anim` block).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Anim {
    pub walk: f64,
    pub sway: f64,
    pub wave: f64,
    pub bob: f64,
    pub phase: f64,
}

impl Default for Anim {
    fn default() -> Self {
        Self {
            walk: 0.0,
            sway: 0.0,
            wave: 0.0,
            bob: 0.0,
            phase: 0.0,
        }
    }
}

impl Anim {
    /// A swaying bone (breathing idle) with an optional phase stagger.
    pub fn sway(amount: f64, phase: f64) -> Self {
        Self {
            sway: amount,
            phase,
            ..Self::default()
        }
    }

    /// A walking limb: `walk` swings it, `bob` lifts the body.
    pub fn walk(amount: f64, phase: f64) -> Self {
        Self {
            walk: amount,
            phase,
            ..Self::default()
        }
    }

    /// Stagger a paired limb opposite its twin.
    pub fn with_phase(mut self, phase: f64) -> Self {
        self.phase = phase;
        self
    }
}

/// Rotation clamp in radians. Omitted means free rotation (`MODEL_FORMAT.md`:
/// `limits` are opt-in and they clamp the bind pose too).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub min: f64,
    pub max: f64,
}

/// One piece of artwork rigidly attached to a bone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub shape: String,
    pub color: String,
    pub size: [f64; 2],
    pub pivot: [f64; 2],
    pub offset: [f64; 2],
    pub rotation: f64,
    pub scale: f64,
    pub z: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

impl Part {
    /// An `ellipse`/`circle` part. `offset` is the intended **centre** and the
    /// pivot must be `[0, 0]` — with any other pivot the renderer shifts the
    /// circle by `-size · pivot` and the numbers stop meaning what they read.
    pub fn ellipse(
        id: &str,
        color: &str,
        size: [f64; 2],
        offset: [f64; 2],
        z: i32,
        tag: Option<&str>,
    ) -> Self {
        Self {
            id: id.to_string(),
            shape: "ellipse".to_string(),
            color: color.to_string(),
            size,
            pivot: [0.0, 0.0],
            offset,
            rotation: 0.0,
            scale: 1.0,
            z,
            tag: tag.map(str::to_string),
        }
    }

    /// A `circle` — an ellipse with equal sides.
    pub fn circle(
        id: &str,
        color: &str,
        diameter: f64,
        offset: [f64; 2],
        z: i32,
        tag: Option<&str>,
    ) -> Self {
        let mut part = Self::ellipse(
            id,
            color,
            [diameter, diameter],
            offset,
            z,
            tag,
        );
        part.shape = "circle".to_string();
        part
    }

    /// A `capsule`. It always spans `[-h/2, w - h/2]` on x and `±h/2` on y, so
    /// the pivot must be `[0, 0.5]` and `offset` is the joint it hangs from.
    pub fn capsule(id: &str, color: &str, size: [f64; 2], offset: [f64; 2], z: i32) -> Self {
        Self {
            id: id.to_string(),
            shape: "capsule".to_string(),
            color: color.to_string(),
            size,
            pivot: [0.0, 0.5],
            offset,
            rotation: 0.0,
            scale: 1.0,
            z,
            tag: None,
        }
    }

    /// Rotate the artwork inside its bone (`rotation` field).
    pub fn turned(mut self, radians: f64) -> Self {
        self.rotation = radians;
        self
    }

    /// The painted rectangle in the part's own frame, before the world matrix.
    /// Mirrors `rig.js::partLocalBox`: an ellipse is centred on `-size · pivot`
    /// (so `[0,0]` means centred on the origin) and a capsule spans
    /// `[-h/2, w-h/2] × ±h/2`.
    pub fn local_box(&self) -> ([f64; 2], [f64; 2]) {
        let (w, h) = (self.size[0], self.size[1]);
        match self.shape.as_str() {
            "capsule" => ([-h / 2.0, -h / 2.0], [w - h / 2.0, h / 2.0]),
            // `face` paints the full rect like an image; everything else is a
            // shape centred on the origin after the `-size · pivot` translate.
            "face" => (
                [-w * self.pivot[0], -h * self.pivot[1]],
                [w * (1.0 - self.pivot[0]), h * (1.0 - self.pivot[1])],
            ),
            _ => ([-w / 2.0, -h / 2.0], [w / 2.0, h / 2.0]),
        }
    }
}

/// One joint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bone {
    pub id: String,
    pub parent: Option<String>,
    pub pos: [f64; 2],
    pub rotation: f64,
    pub scale: f64,
    pub length: f64,
    pub z: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
    pub anim: Anim,
    pub parts: Vec<Part>,
}

impl Bone {
    /// A bone with identity scale/rotation and no animation.
    pub fn new(id: &str, parent: Option<&str>, pos: [f64; 2], z: i32) -> Self {
        Self {
            id: id.to_string(),
            parent: parent.map(str::to_string),
            pos,
            rotation: 0.0,
            scale: 1.0,
            length: 0.0,
            z,
            limits: None,
            anim: Anim::default(),
            parts: Vec::new(),
        }
    }

    pub fn rotated(mut self, radians: f64) -> Self {
        self.rotation = radians;
        self
    }

    pub fn scaled(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    pub fn length(mut self, length: f64) -> Self {
        self.length = length;
        self
    }

    pub fn anim(mut self, anim: Anim) -> Self {
        self.anim = anim;
        self
    }

    /// Declare `limits` **around the bind rotation** rather than guessing: a
    /// clamp that excludes the bind pose silently moves the limb the moment the
    /// rig loads (`MODEL_FORMAT.md` calls this out as the "one wing up" bug).
    pub fn limited(mut self, spread: f64) -> Self {
        let spread = spread.abs().max(0.05);
        self.limits = Some(Limits {
            min: round4(self.rotation - spread),
            max: round4(self.rotation + spread),
        });
        self
    }

    pub fn with_parts(mut self, parts: Vec<Part>) -> Self {
        self.parts = parts;
        self
    }
}

/// A keyframe's per-bone delta: `rotation` is added, `x`/`y` are added to
/// `pos`, `scale` multiplies. Every field is optional (identity when absent).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct BoneDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
}

impl BoneDelta {
    pub fn rotation(radians: f64) -> Self {
        Self {
            rotation: Some(round4(radians)),
            ..Self::default()
        }
    }

    pub fn shifted(x: f64, y: f64) -> Self {
        Self {
            x: Some(round4(x)),
            y: Some(round4(y)),
            ..Self::default()
        }
    }

    pub fn scaled(scale: f64) -> Self {
        Self {
            scale: Some(round4(scale)),
            ..Self::default()
        }
    }

    /// Add a rotation on top (used to fold one bone's delta into a pose).
    pub fn plus_rotation(mut self, radians: f64) -> Self {
        self.rotation = Some(round4(self.rotation.unwrap_or(0.0) + radians));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub t: f64,
    pub bones: BTreeMap<String, BoneDelta>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub duration_ms: u64,
    pub r#loop: bool,
    pub keyframes: Vec<Keyframe>,
}

/// The manifest itself (`model.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub name: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub bones: Vec<Bone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animations: Option<BTreeMap<String, Clip>>,
}

impl Manifest {
    /// A pretty-printed manifest, which is what gets written to disk.
    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn part_count(&self) -> usize {
        self.bones.iter().map(|bone| bone.parts.len()).sum()
    }

    pub fn bone_ids(&self) -> Vec<&str> {
        self.bones.iter().map(|bone| bone.id.as_str()).collect()
    }

    pub fn bone(&self, id: &str) -> Option<&Bone> {
        self.bones.iter().find(|bone| bone.id == id)
    }
}

/* ------------------------------------------------------------------ *
 * Mirroring
 * ------------------------------------------------------------------ */

/// `π − θ` wrapped to `[-π, π]`: reflection across the vertical axis for a chain
/// root whose parent sits on the (unrotated) centerline.
pub fn mirror_root_angle(theta: f64) -> f64 {
    // Deliberately **not** rounded: `atan2` of a 4-decimal bind angle does not
    // land on 4 decimals, and rounding here would leave the left twin a
    // fraction of a pixel off the right one — which is exactly what the
    // forward-kinematics assertion is looking for.
    theta.sin().atan2(-theta.cos())
}

/// Swap a right-side id for its left twin (`arm_r` → `arm_l`).
pub fn left_id(id: &str) -> Option<String> {
    if let Some(stem) = id.strip_suffix("_r") {
        return Some(format!("{stem}_l"));
    }
    None
}

/// `true` for a bone that belongs to a right-side chain.
pub fn is_right(id: &str) -> bool {
    id.ends_with("_r")
}

/// Mirror one authored right-side chain (a contiguous run of bones whose ids end
/// in `_r`, parent-first) into its left twin.
///
/// The two formulas are **not** interchangeable (`MODEL_FORMAT.md`):
///
/// * a chain *root* reflects its `pos.x` and its angle across the vertical axis;
/// * a chain *child* negates `pos.y` and its angle, because its parent's local
///   `+y` is already flipped.
///
/// Part offsets/rotations are mirrored in both cases: every mirrored bone's
/// local frame has the opposite handedness, so leaving them alone would put the
/// artwork on the wrong side of the limb.
pub fn mirror_chain(bones: &mut Vec<Bone>, from: usize) {
    let chain: Vec<Bone> = bones[from..].to_vec();
    for bone in chain {
        let Some(id) = left_id(&bone.id) else {
            continue;
        };
        // Which formula applies is decided by the **original** parent: a bone
        // whose parent is inside the mirrored chain is a chain child; one whose
        // parent is on the unrotated centerline is a chain root.
        let parent_in_chain = is_right(bone.parent.as_deref().unwrap_or(""));

        let mut twin = bone.clone();
        twin.id = id;
        // A descendant's parent must follow it into the mirrored chain —
        // leaving it pointing at the `_r` bone silently transplants the twin
        // onto the right-hand transform (the classic "one arm on backwards").
        twin.parent = bone
            .parent
            .as_deref()
            .map(|parent| left_id(parent).unwrap_or_else(|| parent.to_string()));
        twin.limits = bone.limits.map(|limits| Limits {
            min: round4(-limits.max),
            max: round4(-limits.min),
        });

        if parent_in_chain {
            // chain child: parent is already mirrored
            twin.pos[1] = round4(-twin.pos[1]);
            twin.rotation = round4(-twin.rotation);
        } else {
            // chain root: parent is on the unrotated centerline
            twin.pos[0] = round4(-twin.pos[0]);
            twin.rotation = mirror_root_angle(twin.rotation);
        }

        for part in &mut twin.parts {
            part.offset[1] = round4(-part.offset[1]);
            part.rotation = round4(-part.rotation);
        }
        bones.push(twin);
    }
}

/* ------------------------------------------------------------------ *
 * Forward kinematics
 * ------------------------------------------------------------------ */

/// A resolved transform: the page's `RT.world[bone]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xform {
    pub x: f64,
    pub y: f64,
    pub rot: f64,
    pub scale: f64,
}

impl Default for Xform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            rot: 0.0,
            scale: 1.0,
        }
    }
}

/// Resolve every bone to world space — `computeWorld` from `ui/js/rig.js`:
/// `x = parent.x + (px·cos − py·sin)·parent.scale`, `rot = parent.rot + bone.rot`,
/// `scale = parent.scale · bone.scale`.
pub fn world_transforms(bones: &[Bone]) -> HashMap<String, Xform> {
    let mut world: HashMap<String, Xform> = HashMap::new();
    let mut remaining: Vec<&Bone> = bones.iter().collect();

    while !remaining.is_empty() {
        let mut progressed = false;
        let mut still: Vec<&Bone> = Vec::new();
        for bone in remaining {
            let parent = match bone.parent.as_deref() {
                None => Xform::default(),
                Some(parent) => match world.get(parent) {
                    Some(xform) => *xform,
                    None => {
                        still.push(bone);
                        continue;
                    }
                },
            };
            let cos = parent.rot.cos();
            let sin = parent.rot.sin();
            world.insert(
                bone.id.clone(),
                Xform {
                    x: parent.x + (bone.pos[0] * cos - bone.pos[1] * sin) * parent.scale,
                    y: parent.y + (bone.pos[0] * sin + bone.pos[1] * cos) * parent.scale,
                    rot: parent.rot + bone.rotation,
                    scale: parent.scale * bone.scale,
                },
            );
            progressed = true;
        }
        if !progressed {
            // A dangling parent (the loader rejects it, and the generator never
            // produces one) — resolve what we can and stop instead of looping.
            break;
        }
        remaining = still;
    }
    world
}

/// The four corners a part paints, in world space.
pub fn part_world_corners(world: &Xform, part: &Part) -> [[f64; 2]; 4] {
    let (lo, hi) = part.local_box();
    let scale = world.scale * part.scale;
    let rot = world.rot + part.rotation;
    let cos = rot.cos();
    let sin = rot.sin();
    // `partWorldMatrix`: the offset is scaled by bone·part scale, then rotated
    // by the bone's rotation.
    let ox = part.offset[0];
    let oy = part.offset[1];
    let bx = world.x + (ox * world.rot.cos() - oy * world.rot.sin()) * scale;
    let by = world.y + (ox * world.rot.sin() + oy * world.rot.cos()) * scale;
    [
        [lo[0], lo[1]],
        [hi[0], lo[1]],
        [hi[0], hi[1]],
        [lo[0], hi[1]],
    ]
    .map(|[px, py]| [bx + (px * cos - py * sin) * scale, by + (px * sin + py * cos) * scale])
}

/// Highest point (smallest y) the rig paints, in world space.
pub fn min_world_y(manifest: &Manifest) -> f64 {
    let world = world_transforms(&manifest.bones);
    let mut min = f64::INFINITY;
    for bone in &manifest.bones {
        let Some(xform) = world.get(&bone.id) else {
            continue;
        };
        for part in &bone.parts {
            for corner in part_world_corners(xform, part) {
                min = min.min(corner[1]);
            }
        }
    }
    min
}

/// `true` when every bone id in `parents` exists and the graph is acyclic.
pub fn hierarchy_is_sound(bones: &[Bone]) -> Result<(), String> {
    let mut ids: Vec<&str> = bones.iter().map(|bone| bone.id.as_str()).collect();
    ids.sort_unstable();
    for window in ids.windows(2) {
        if window[0] == window[1] {
            return Err(format!("duplicate bone id {:?}", window[0]));
        }
    }
    for bone in bones {
        if let Some(parent) = bone.parent.as_deref() {
            if !ids.contains(&parent) {
                return Err(format!("bone {:?} references missing parent {parent:?}", bone.id));
            }
        }
    }
    // Walk every chain upward; a repeat means a cycle.
    for bone in bones {
        let mut seen: Vec<&str> = Vec::new();
        let mut cursor = Some(bone.id.as_str());
        while let Some(current) = cursor {
            if seen.contains(&current) {
                return Err(format!("hierarchy contains a cycle at {current:?}"));
            }
            seen.push(current);
            cursor = bones
                .iter()
                .find(|candidate| candidate.id == current)
                .and_then(|candidate| candidate.parent.as_deref());
        }
    }
    Ok(())
}

/// Snap to four decimals so the written JSON is stable and readable (the editor
/// rounds the same way).
pub fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_root_formula_is_a_vertical_reflection() {
        // up-right horn → up-left horn
        let mirrored = mirror_root_angle(-0.32);
        assert!((mirrored - (-2.8216)).abs() < 1e-3, "{mirrored}");
        // straight down stays straight down
        assert!((mirror_root_angle(std::f64::consts::FRAC_PI_2) - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        // pointing right mirrors to pointing left
        assert!((mirror_root_angle(0.0).abs() - std::f64::consts::PI).abs() < 1e-9);
    }

    #[test]
    fn a_mirrored_chain_is_symmetric_in_world_space() {
        let mut bones = vec![
            Bone::new("root", None, [0.0, 0.0], 20).scaled(ROOT_SCALE),
            Bone::new("torso", Some("root"), [0.0, -50.0], 20),
            Bone::new("arm_r", Some("torso"), [46.0, -40.0], 25).rotated(1.05),
            Bone::new("forearm_r", Some("arm_r"), [30.0, 2.0], 25).rotated(0.15),
            Bone::new("wing_r", Some("torso"), [-30.0, -30.0], 14).rotated(-0.32),
        ];
        mirror_chain(&mut bones, 2);
        let world = world_transforms(&bones);
        for (right, left) in [
            ("arm_r", "arm_l"),
            ("forearm_r", "forearm_l"),
            ("wing_r", "wing_l"),
        ] {
            let r = world[right];
            let l = world[left];
            assert!((l.x + r.x).abs() < 1e-9, "{right}/{left} x: {} vs {}", l.x, r.x);
            assert!((l.y - r.y).abs() < 1e-9, "{right}/{left} y: {} vs {}", l.y, r.y);
        }
    }

    #[test]
    fn shape_pivots_follow_the_format() {
        let ellipse = Part::ellipse("e", "#fff", [10.0, 20.0], [1.0, 2.0], 0, None);
        let capsule = Part::capsule("c", "#fff", [30.0, 8.0], [0.0, 0.0], 0);
        assert_eq!(ellipse.pivot, [0.0, 0.0]);
        assert_eq!(capsule.pivot, [0.0, 0.5]);
        assert_eq!(capsule.local_box(), ([-4.0, -4.0], [26.0, 4.0]));
        assert_eq!(ellipse.local_box(), ([-5.0, -10.0], [5.0, 10.0]));
    }

    #[test]
    fn limits_wrap_the_bind_rotation() {
        let bone = Bone::new("neck", None, [0.0, 0.0], 20).rotated(0.4).limited(0.9);
        let limits = bone.limits.expect("limits");
        assert!(limits.min <= bone.rotation && bone.rotation <= limits.max);
    }
}
