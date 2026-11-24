# Custom models & rig format

Drop a folder into `models/` (or use **Import…** in the control panel), then
switch to it from the model dropdown or `POST /model`.

```text
models/
  xenomorph-7/            shipped, procedural shapes (no textures)
    model.json
  nebulon-9/              shipped alien: procedural shapes, tagged face parts,
    model.json            four keyframe clips (wave/breathe/scan/alert)
  emberwing/              shipped lantern moth: 22-bone shape rig, four
    model.json            wings, ember eyespots, six keyframe clips
  demo-blob/              shipped example, PNG sprite parts
    model.json
    rig.json              <- written by the rig editor (optional override)
    parts/body.png
    parts/eye.png
    parts/pupil.png
    parts/arm.png
    parts/foot.png
  my-character/           your import
    model.json            required: the rig manifest
    parts/…               any PNG/JPG/SVG/WebP the manifest references
```

* `models/` root can be relocated with the `DESKTOP_BUDDY_MODELS` environment
  variable; otherwise the app looks next to the executable and falls back to
  `<repo>/models`.
* `.state.json` (inside `models/`) remembers the last selected model, so the
  choice survives restarts.
* The rig editor writes **`rig.json`**, never `model.json`. `rig.json` wins when
  present, so **Reset** simply deletes it and reloads the shipped manifest.

## Manifest schema (`model.json`)

```json
{
  "format": "desktop-buddy/model@1",
  "name": "demo-blob",
  "label": "Demo Blob",

  "bones": [
    {
      "id": "root",
      "parent": null,
      "pos": [0, 0],
      "rotation": 0.0,
      "scale": 1.0,
      "length": 0,
      "z": 20,
      "limits": { "min": -3.1416, "max": 3.1416 },
      "anim": { "walk": 0, "sway": 0, "wave": 0, "bob": 4, "phase": 0 },
      "parts": [
        {
          "id": "body",
          "image": "parts/body.png",
          "size": [150, 132],
          "pivot": [0.5, 0.55],
          "offset": [0, 0],
          "rotation": 0,
          "scale": 1,
          "z": 0
        }
      ]
    }
  ]
}
```

### Bones

| field | meaning |
| --- | --- |
| `id` | unique string; the editor and the API address joints by it |
| `parent` | another bone `id` (or `null` for a root). Hierarchy drives the transform |
| `pos` | local offset from the parent joint, in pixels (parent-rotated) |
| `rotation` | bind rotation, radians |
| `scale` | local scale multiplier |
| `length` | bone length in px; draws the rotation handle and is used by `+ Bone` |
| `z` | draw order; lower draws first |
| `limits.min` / `limits.max` | rotation clamp in radians, enforced in the editor and in animation |
| `anim` | see animation channels below |
| `parts` | sprites/shapes rigidly attached to this bone |

Several bones may share a parent; the avatar's position is the world position of
the root bone(s). The window origin is the top-left of the primary monitor, and
positions are CSS pixels.

#### Screen-Space Angle Compass

Bone rotation follows screen space where `0` points **right (`+x`)** and `+y`
grows **downward**:

| Direction on Screen | Radians | Degrees | Typical Use |
| --- | --- | --- | --- |
| **Right (`+x`)** | `0.0` | `0°` | Right wing, right foot |
| **Down-Right** | `+1.05` to `+1.36` | `+60°` to `+78°` | Right arm / right leg |
| **Straight Down (`+y`)** | `+1.5708` (`+π/2`) | `+90°` | Spine / hanging tail |
| **Down-Left** | `+1.78` to `+2.09` | `+102°` to `+120°` | Left leg / left arm |
| **Left (`-x`)** | `±3.1416` (`±π`) | `±180°` | Left wing, left foot |
| **Up-Left** | `-2.09` to `-2.50` | `-120°` to `-143°` | Left horn / left antenna |
| **Straight Up (`-y`)** | `-1.5708` (`-π/2`) | `-90°` | Neck / upright crest |
| **Up-Right** | `-0.64` to `-1.05` | `-37°` to `-60°` | Right antenna / right horn |

#### Fitting the monitor

The app walks the root around the screen (its wander margin is 120 px), and it
does **not** clamp the artwork to the screen, so whatever a rig paints above its
root is what gets clipped when the avatar wanders near the top. For reference,
`demo-blob` sits 46 px above its root and `xenomorph-7` 168 px; treat ~170 px as
the practical budget. A taller character should be scaled down on the **root
bone** (`"scale": 0.55`) rather than by renumbering every position and size —
child `pos`, part offsets and part sizes all inherit it.

### Parts

| field | meaning |
| --- | --- |
| `id` | unique within the bone |
| `image` | path relative to the model folder (no `..`, no absolute paths) |
| `shape` | alternative to `image`: `ellipse`, `circle`, `capsule`, `face` |
| `color` | fill colour for `shape` parts |
| `size` | `[w, h]` in px (omit for images and the natural size is adopted) |
| `pivot` | normalised `[x, y]` anchor (0..1). For **images** it is the anchor inside the sprite; for `ellipse`/`circle` it shifts the artwork by `-size · pivot` instead — author shapes with `[0, 0]` (see below) |
| `offset` | px offset from the bone origin, applied before the pivot |
| `rotation` | part rotation relative to its bone, radians |
| `scale` | part scale |
| `z` | order within the bone |
| `lockBone` | *optional* bool: the editor's "Lock Bone to Part" flag — dragging the part moves the parent bone instead of the part's `offset` |

`shape: "face"` draws the procedural eyes/brows/mouth driven by the current
expression, so a custom head can keep the built-in expressions.

#### Where shape artwork actually lands

`pivot` is normalised inside an **image** rect, but shapes are not images: after
the same `-size · pivot` pre-translate the renderer draws them relative to the
part origin, which gives the three cases below. Only the `face` shape behaves
like an image.

| shape | painted area, in the part's own frame | authoring form |
| --- | --- | --- |
| `ellipse` / `circle` | circle of `size` **centred on the part origin**, so the pre-translate moves it to `-size · pivot` | `pivot: [0, 0]` and `offset` = the intended **centre** |
| `capsule` | always spans `[-h/2, w - h/2]` on x and `±h/2` on y; `pivot` cancels out | `pivot: [0, 0.5]` and `offset` = the joint it hangs from |
| `face` | the full `size` rect, exactly like an image | `pivot: [0.5, 0.5]` |

A `[0.5, 0.5]` pivot on an ellipse therefore throws the circle half a size up and
left of where the numbers read — the commonest way a hand-written shape rig ends
up looking scrambled. The editor's selection box already accounts for all three
cases, so a part authored this way still selects where it paints.

### Sockets (attachment points)

A socket is a **named** anchor on the rig: where a tool, bag or helmet hangs.
Props never hard-code a coordinate, they name a socket, so the same fishing rod
can live in a paw, in a chest, or on the ground.

```json
"sockets": [
  { "id": "hand_primary", "bone": "arm3_r", "pos": [2, 0], "rotation": 0.3,
    "scale": 1, "z": 420, "accepts": ["rod", "tool", "bag"],
    "mirror": "hand_secondary", "label": "Right paw" },
  { "id": "tail_grip", "bone": "tail_3", "pos": [0, -6], "z": 300 }
]
```

| field | meaning |
| --- | --- |
| `id` | the socket name. The vocabulary the runtime knows: `hand_primary`, `hand_secondary`, `head_helmet`, `neck_collar`, `chest_front`, `chest_back`, `back`, `stage_ground`, `perch_surface`; anything else is a custom socket a prop can still ask for |
| `bone` | **required** for a bone socket; must exist in `bones`. Omit `bone` for a virtual surface (`stage_ground`, `perch_surface`) |
| `pos` | local offset from the bone joint, in the bone's rotated frame (default `[0, 0]`) |
| `rotation` / `scale` | local transform on top of the bone (default `0` / `1`) |
| `z` | draw-order band. Lower draws behind the body. Conventions: `back` 40, `chest_back` 60, `neck_collar` 120, `chest_front` 340, `head_helmet` 380, hands 420. Default 380 |
| `accepts` | what may attach, matched against a prop's tags. Empty means anything |
| `mirror` | the socket on the other side of the body |
| `label` | human name for the editor / API |

#### Where to put them

1. **`sockets.json`, beside `rig.json`** — the recommended home. It is not the
   file the rig editor rewrites, so it survives editing; the editor's
   serialiser only emits `format/name/label/notes/animations/bones` and would
   drop a `sockets` block out of a saved rig.

   ```json
   { "sockets": [ { "id": "hand_primary", "bone": "arm3_r" } ] }
   ```

   A bare array is accepted too. A malformed file is a warning, never a load
   failure — the derived table below is a complete fallback.
2. **`sockets` inside `model.json` / `rig.json`** — what the procedural
   generator writes. Loads today: the validator accepts unknown root keys.
3. **Nothing at all** — the runtime derives the table from the bones.

Precedence, per socket id: sidecar → manifest → derived. An explicit entry that
names a missing bone is skipped with a warning rather than failing the load.

#### How derivation decides

The shipped rigs do not share a vocabulary: a generated creature is
`arm_r → arm2_r → arm3_r` with the paw on the **distal** bone, `demo-blob` has
`arm_l → arm_l_hand` but a bare `arm_r`, and `xenomorph-7` is four tentacles with
no arms and no head at all. So the resolver scores candidates:

* an arm/tentacle bone beats a wing tip or a plain leaf;
* a declared side (`_r`, `_l`, `left…`, `right…`, or a part tag like
  `leg_left`) beats one inferred from the rest position;
* deeper in its chain beats shallower — the paw, not the shoulder;
* the rest position on the expected side beats the other side;
* the id breaks the final tie, so the result is deterministic.

`head_helmet` wants a `head*` bone (or parts tagged `head`/`eye`/`mouth`/`brow`);
`neck_collar` a `neck`/`collar` bone; `chest_front`/`chest_back` a
`torso`/`chest`/`body` bone; `back` the root. A rig without those bones simply
has no such socket — that is a property of the rig, not an error. Both hands are
always produced when the rig has anything limb-like, because "the other paw" is a
more useful answer than "no such thing".

`GET /sockets` returns the resolved table with a `source` of `"sidecar"`,
`"manifest"` or `"derived"`, so you can always tell what was authored and what
was inferred.

### Animation channels (`anim`)

Each channel is an amplitude that the renderer adds to the bone's rotation
(channels are summed and then clamped by `limits`):

| channel | effect |
| --- | --- |
| `walk` | `sin(2π·walkPhase + phase) · walk` — only while walking |
| `sway` | `sin(t·1.3 + phase) · sway` — always (idle breathing) |
| `wave` | `sin(wavePhase + phase) · wave` — only while waving |
| `bob` | vertical bob in px: `sin(t·1.8 + phase) · bob` |
| `phase` | phase offset in radians; stagger limbs with ±π |

Nothing is hardcoded in the renderer: idle, walking, waving and API-triggered
poses all come from these values, so a new rig animates the moment it loads.

## Custom keyframe animations (`animations`)

Rigs may carry named keyframe clips. The block is **optional** — a manifest
without it loads exactly as before.

```json
{
  "format": "desktop-buddy/model@1",
  "name": "demo-blob",
  "bones": [ … ],
  "animations": {
    "wave": {
      "duration_ms": 1400,
      "loop": false,
      "keyframes": [
        { "t": 0.0, "bones": { "arm_r": { "rotation": 0.0,  "x": 0, "y": 0, "scale": 1.0 } } },
        { "t": 0.4, "bones": { "arm_r": { "rotation": -2.5, "x": 0, "y": 0, "scale": 1.0 } } },
        { "t": 1.0, "bones": { "arm_r": { "rotation": 0.0,  "x": 0, "y": 0, "scale": 1.0 } } }
      ]
    },
    "breathe": {
      "duration_ms": 2400,
      "loop": true,
      "keyframes": [
        { "t": 0.0, "bones": { "root": { "y": 0 }, "head": { "rotation": 0.0 } } },
        { "t": 0.5, "bones": { "root": { "y": 3 }, "head": { "rotation": 0.04 } } },
        { "t": 1.0, "bones": { "root": { "y": 0 }, "head": { "rotation": 0.0 } } }
      ]
    }
  }
}
```

### Clip fields

| field | type | meaning |
| --- | --- | --- |
| `<clip name>` | object | the key of `animations`; this is the name `/animate` and `avatar_animate` take |
| `duration_ms` | number | clip length in milliseconds; **must be > 0** |
| `loop` | boolean *(optional)* | restart at the end instead of finishing; default `false` |
| `keyframes` | array | poses in time order (order does not matter — the sampler sorts by `t`) |
| `keyframes[].t` | number | normalised time, **`0.0 ..= 1.0`** (`0.0` = start, `1.0` = end) |
| `keyframes[].bones` | object | `{ "<bone id>": { rotation, x, y, scale } }`; every id must exist in `bones` |

### Per-bone values are deltas

Every value in `bones` is an **offset from the bone's bind transform**, with an
identity default, so a clip composes with the rig (and with `anim` channels and
`/pose` overrides) instead of replacing it:

| key | applied as | identity (no change) |
| --- | --- | --- |
| `rotation` | radians **added** to the bone's bind `rotation`, then clamped by `limits` | `0.0` |
| `x`, `y` | pixels **added** to the bone's `pos`, in the parent's rotated frame | `0.0` |
| `scale` | **multiplier** on the bone's `scale` | `1.0` |

So `"rotation": 0.5` bends a bone half a radian further than its rest pose, and
`"scale": 1.2` makes it 20 % bigger. A bone may appear in only some keyframes;
the missing side is treated as the identity, and a bone that no keyframe
mentions is simply not animated.

### Interpolation rules

* The sampler finds the two keyframes surrounding the playhead and takes the
  normalised distance `p` between them.
* `p` is **smoothstepped** (`p² · (3 − 2p)`) so motion eases in and out of every
  keyframe, then each of the four values is linearly interpolated with it.
* Before the first keyframe and after the last one the clip **holds** that
  keyframe (no extrapolation).
* Transform order in the page is *bind → `anim` channels → clip delta → `/pose`
  override*, so a pose sent over `POST /pose` always wins.
* A non-looping clip stops at `t = 1`, reports itself **finished** and the
  avatar returns to `idle`. A looping clip wraps back to `t = 0`.

### Validation

Loading, importing or saving a rig rejects, with a readable message:

* `animations` that is not an object of clips
* a clip whose `duration_ms` is missing, non-numeric or `<= 0`
* a `loop` that is not a boolean
* `keyframes` that is not an array
* a keyframe whose `t` is missing, non-numeric or outside `0.0..=1.0`
* a keyframe `bones` that is not an object, or a `rotation`/`x`/`y`/`scale` that
  is not a finite number
* a keyframe that references a bone id that does not exist in `bones`

## Authoring a rig that reads well

Everything below was learned building `models/emberwing` (a 22-bone, 96-part
shape-only moth) and `astral-drake` / `aether-drake` (27-bone, 100+ part
cyber-arcane wyverns), and is the difference between a rig that merely loads
and one that looks deliberate. `label` is what the model dropdown shows;
`notes` is free-form and ignored by the renderer.

### Bone frames and mirroring (Chain Roots vs. Chain Children)

A bone's local frame is `+x` along the bone and `+y` perpendicular to it, so
which way local `+y` points on screen depends on the bone's rotation:

| bone points | rotation | local `+y` on screen |
| --- | --- | --- |
| right | `0` | down |
| down | `+π/2` | left |
| left | `π` | up |
| up | `-π/2` | right |

Because of this, mirroring a limb across the vertical axis requires two
different rotation formulas depending on whether the bone is the root of a
paired limb (attached to a centerline bone like `torso`, `head`, or `root`
whose rotation `≈ 0`) or a descendant bone inside an already-mirrored chain:

* **Chain Root (parent is centerline with `rotation == 0`)**: Negating rotation
  reflects across the horizontal axis instead of the vertical axis (turning an
  up-right horn at `-1.05 rad` into a down-right horn at `+1.05 rad` across the
  eye). Reflect across the vertical Y-axis using `atan2(sin(θ), -cos(θ))` (or
  `π - θ` wrapped to `[-π, π]`).
* **Chain Child (parent is already mirrored)**: Because the parent bone's axis
  is already mirrored, local `+y` is already flipped—so simply negate the
  child's relative rotation (`-θ`).

```python
import math

def mirror_chain_root(bone):
    """Mirror a limb root bone whose parent is on the centerline (rotation == 0)."""
    bone["pos"][0] *= -1
    bone["rotation"] = math.atan2(math.sin(bone["rotation"]), -math.cos(bone["rotation"]))
    for part in bone.get("parts", []):
        part["offset"][1] *= -1
        part["rotation"] *= -1

def mirror_chain_child(bone):
    """Mirror a descendant bone whose parent is already mirrored."""
    bone["pos"][1] *= -1
    bone["rotation"] *= -1
    for part in bone.get("parts", []):
        part["offset"][1] *= -1
        part["rotation"] *= -1
```

A right wing root authored at `-0.32 rad` (up-right) therefore has its left twin
at `-2.8216 rad` (up-left), and a child feather joint bent `+0.22 rad` on the
right bends `-0.22 rad` on the left. In keyframe clips, rotation deltas are
always negated between left and right (`+a` on the left, `-a` on the right).

### `limits` are opt-in, and they clamp the bind pose too

`clampRotation` treats a missing or non-finite bound as free rotation, so a bone
with no `limits` key spins 360°. But if you do declare `limits`, the bind
rotation is clamped as well — the schema example's `±3.1416` silently turns
a wing authored at `π + 0.55` into `π`, while its `-0.55` twin renders as
authored. The rig loads, the offline preview looks right, and the live avatar
has one wing up and one flat.

* Omit `limits` on bones that need to swing past `π` (wings, antennae, tails).
* Or widen them: `"limits": { "min": -5.2, "max": 5.2 }` covers a bind angle
  plus a `±1 rad` clip on top of it.
* Keep tight limits where they are the point (a neck, a knee) — the editor and
  every clip respect them.

### Draw order & standard `z`-stacking conventions

`sortedDrawables` sorts by `bone.z` first and `part.z` only as a tie-break, so
every part of a child bone paints over every part of its parent, whatever
the part `z` values say. A wing "tip" bone at `z: 9` will cover the eyespot you
put on the wing panel at `z: 8` — the spot appears cut in half. Either keep the
detail on the bone that owns the panel, or give the child a lower `z` than the
parent and accept that the tip then draws underneath.

Use these standard `z`-index offsets so back-mounted limbs never overlap the
torso or face silhouette:

| Layer / Attachment | Recommended `z` | Visual Effect |
| --- | --- | --- |
| **Back wings / tails** | `torso.z - 6` (e.g., `12–16`) | Tucks cleanly behind the chest and pelvis |
| **Legs / lower limbs** | `torso.z - 8` (e.g., `11–13`) | Connects smoothly underneath the waist/hips |
| **Torso / chest core** | `20` | Primary body mass |
| **Foreground arms** | `torso.z + 4` (e.g., `24–26`) | Drapes cleanly in front of the torso and wing roots |
| **Horns / ears / dorsal crests** | `head.z - 2` (e.g., `28`) | Base emerges naturally from behind the skull silhouette |
| **Head / face / antennae** | `30–32` | Keeps facial expressions unobstructed |

### Silhouette recipes

* **Build a wing (or any soft mass) from a stack of ellipses, not one**: a big
  round shoulder, a narrower mid panel, a small tip. Overlapping ellipses of the
  same colour merge into one tapering silhouette; a single ellipse reads as a
  blob and a capsule reads as a sausage.
* **Capsule-inside-ellipse margin rule**: Never lay a straight capsule along or
  tangent to a curved ellipse edge. A capsule is a straight line and an ellipse
  edge is an arc, so the two diverge and the capsule pokes out into empty space
  — the classic "stray scratch" across a wing. Structural capsules (wing spars,
  veins, antennae, joint trim) must run along the bone's local `+x` chord and
  stay **inset at least `0.5 × capsule_height + 5 px`** inside the parent
  ellipse's radius.
* **Skip thin bright rim lines**: A 3–4 px highlight in a light colour reads as
  a scratch at avatar scale; use a slightly lighter panel instead.
* **One eyespot = four parts**: a dark ring, an accent disc, a bright core and a
  small offset pip. That is the whole trick behind a face that looks alive.
* **Separate values, not hues**: The body should be clearly lighter than the
  wings (`#46335f` body over `#1a1330` wings) or the whole creature turns into
  one violet mass. Two accent colours are plenty (e.g., ember `#ff7a3d` and gold
  `#ffc857`); a third only works as a tiny highlight.
* **Keep the head clear of the wings**: Attach wings at the thorax, not at the
  head, and leave a gap so the face is never occluded by a wing panel.
* **90–100 parts** is a comfortable budget for a detailed shape rig; the cost is
  authoring time, not runtime.

### Clip amplitudes

* Bind angles are already dramatic. A wing resting at 31° above horizontal only
  needs `+0.6 rad` to look fully flared; past `~0.9 rad` it crosses over the
  head and the pose reads as "hands up" rather than "wings out".
* The sampler smoothsteps between keys, so keys are the extremes — you do
  not need extra keys to soften a move, only to change its rhythm.
* Mirror every wing/leg delta by sign (see above), and stagger limbs with
  `anim.phase` (`±π`) rather than with different amplitudes.
* `scale` on a single part (a lantern, an eye, a core) is the cheapest "pulse"
  in the format: `1.0 → 1.4 → 1.0` over a clip reads as a glow.

### Tags that make a shape-only face work

| tag | what the renderer does with it |
| --- | --- |
| `head` | the bone owning it gets the hover tilt and the gaze yaw bias (`isHeadBone` also matches a bone whose id contains `head`) |
| `pupil` | offset by the gaze vector (also matched by `pupil` in the part id); once any pupil exists, eye parts stop drifting |
| `eye` | squashed to 12 % height for the auto-blink |
| `mouth` | squashed/stretched by `mouth_open` while talking or emoting; two halves also lift/tilt with `mouth_curve` |
| `brow` | rises with `brow_up` and tilts with `brow_angle` (mirrored by `offset.x`) |

A rig with no pupil part still looks alive — the whole eye sprite drifts a
little instead — but tagging real pupils gives the crisp "eyes follow the
cursor" read.

### 1-Shot AI Authoring Checklist

When generating a new rig programmatically or in one pass, follow this order to
avoid coordinate, mirroring, or expression bugs:

1. **Anchor at `root` (`pos: [0, 0]`, `scale: 0.55`)**: Place pelvis around
   `y = +45`, torso around `y = 0` (artwork extending up to `y ≈ -110`), and
   head around `y = -125` (top of skull/horns around `y ≈ -220`, which scales
   to `-121 px` — safely inside the `170 px` top-clip budget).
2. **Author Centerline + Right Side Only (`*_r`), Then Mirror**: Never hand-type
   left-side angles. Build the centerline (`root`, `pelvis`, `torso`, `head`,
   `tail`) and the right-side chains (`arm_r`, `leg_r`, `wing_r`, `horn_r`),
   then generate `*_l` using `mirror_chain_root` on the root joint of each chain
   and `mirror_chain_child` on every descendant joint.
3. **Verify `z`-Layering & Capsule Margins**:
   * Keep back wings/tails below `torso.z`, horns/ears at `head.z - 2`, and
     foreground arms above `torso.z`.
   * Keep all structural capsules inset inside their parent ellipse contours
     so no edges poke past curved silhouettes.
4. **Verify Face Tag Rules**:
   * Split the mouth into `mouth_l` and `mouth_r` (`tag: "mouth"`, `offset.x = ±7`)
     so `mouth_curve` lifts and tilts both corners cleanly.
   * Use `shape: "ellipse"` (never `capsule`) for `brow_l` and `brow_r`
     (`tag: "brow"`).
   * Tag irises/sclerae as `"eye"` and pupils as `"pupil"` (never tag a pupil
     as `"eye"`, or it will squash flat during auto-blink).
5. **Run Forward-Kinematics Verification Before Outputting**:
   * Assert every paired joint satisfies `world_x(bone_l) == -world_x(bone_r)`
     and `world_y(bone_l) == world_y(bone_r)`.
   * Assert `min_world_y >= -170` across all parts so the top of the head/horns
     never clips near the top monitor edge.
   * Assert every parent and keyframe bone ID exists, and no `limits` clamp
     a bind rotation unexpectedly.

### Verifying a new rig without guessing

1. **Offline first.** Re-implement `computeWorld` + `partWorldMatrix` +
   `drawShape` in a throwaway PIL script and render PNGs. The three rules to
   copy exactly: an ellipse is centred on `-size · pivot` (so author with
   `pivot: [0, 0]`), a capsule spans `[-h/2, w - h/2]` on x, and the part
   transform is `translate(bone) · rotate(bone) · scale(bone·part) · translate(offset) · rotate(part.rotation)`. This catches most geometry
   mistakes in seconds.
2. **Then the app.** `GET /models` must report `error: null` with the expected
   bones/parts counts, and `POST /model {"name": "..."}` must return
   `ok: true`.
3. **Prove the mirror numerically, not by eye**: play a clip and sample
   `animationOffset(bone).rotation` for both sides in one `Runtime.evaluate` —
   `wing_u_l` must equal `-wing_u_r` at every sample.
4. **Prove the face**: `POST /emote {"expression": "happy"}` and read
   `avatar.blendshapes` (`mouth_open`, `eye_wide`, `brow_up` non-zero), then move
   the real OS cursor and check `__avatarDebug.gaze()` reports
   `cursorTracking: true` with a non-zero tilt.
5. **Screenshot with the bounds, not the alpha**: read
   `__avatarDebug.bounds()` in the same CDP run as `Page.captureScreenshot` and
   crop with those numbers.

## Rig editor

Open with the wrench button in the titlebar, the **Rig edit** button in the
control panel, or the `R` key. Entering the mode freezes the animation and parks
the avatar in the middle of the screen.

| action | result |
| --- | --- |
| click a joint / part | select it; the inspector fills in |
| drag a joint | move that bone (`pos`) |
| drag the ring at the bone tip | rotate the bone, clamped by `limits` |
| drag a part | change the part's `offset` — or move its bone when the part is locked (see below) |
| wheel over a selected part | rotate the part by 0.05 rad per tick (snapped to 4 decimals), leaving the bone alone |
| shift + wheel over a selected part | scale the part |
| wheel over a selected bone (no part) | rotate the bone by 0.02 rad |
| shift + wheel over a selected bone | scale the bone |
| `Ctrl+Z` / **Undo** | step back through edits (80 levels) |
| turn the wheel / type in the inspector | numeric precision for every field |

**What a press grabs.** A part you already have selected — or one locked to its
bone — owns the press, even where the bone's rotation ring overlaps the artwork
(the ring sits at the bone tip, which for a limb sprite is inside the image).
Without that rule a drag on the sprite's far end would rotate the bone instead of
moving the part. The ring still wins when you press it outside every part box,
or when only a bone is selected.

The inspector exposes position, rotation, scale, length, z-order, rotation
limits, all five animation channels, and — for the selected part — size, pivot,
offset, rotation, z-order and scale. Bones can be re-parented from the dropdown
(cycles are rejected), created (`+ Bone`, a capsule-child of the selection) and
deleted together with their descendants.

### Adding bones

`+ Bone` sits in the toolbar at the top of the inspector, so it is reachable
without selecting anything first (the footer copy still works too):

* with a bone selected it adds a capsule child of that bone;
* with nothing selected it adds a child of `root` (or of the first bone when the
  rig has no root);
* with an empty rig it creates the root bone itself.

Either way the new bone is selected immediately, so its joint and rotation ring
are highlighted on the canvas and the inspector is ready for edits.

### Locking a bone to a part

The part inspector has a bone lock group:

| control | effect |
| --- | --- |
| **Lock Bone to Part** | while checked, the part is welded to its bone: dragging the part moves the parent bone (`pos`, in the parent's rotated frame) and the part keeps its local offset, so sprite and pivot travel together — and the wheel / shift+wheel rotate and scale the bone instead of the sprite. Saved as `"lockBone": true` on the part |
| **Snap Bone to Part** | moves the parent bone so its origin lands exactly on the part, then zeros the part's offset — the bone pivot sits on the sprite without it jumping |

While locked, a press anywhere on the sprite drags the bone (the rotation ring
does not steal it); press the ring outside the artwork, or untick the lock, when
you want to aim the sprite on its own. Both controls are ordinary rig edits:
`Ctrl+Z` steps back, and **Save rig** persists them.

**Save rig** writes `models/<name>/rig.json` atomically (write + rename) after
validating the hierarchy and every referenced image. **Reset** deletes that
override and reloads `model.json`.

## Timeline editor (authoring clips)

While Rig Edit Mode is open a compact timeline bar sits above the editor hint.
It edits `animations` on the rig and is persisted by the same **Save rig**.

| control | what it does |
| --- | --- |
| clip dropdown | choose the clip to preview and author; selecting one applies its pose at 0 % |
| clip name field | type a name and press `Enter` (or click away) to rename the selected clip. Names are trimmed; a blank name or one already in use is rejected with a toast and the field snaps back. With no clip selected the field is the name for the next `+ Clip` |
| `+ Clip` | create an empty clip — named from the field when it is filled in, otherwise the next free `clip 1`, `clip 2`, … — with `duration_ms: 1200` and `loop: false` |
| `Del Clip` | delete the selected clip |
| duration | clip length in ms (`>= 1`); the playhead keeps its percentage |
| loop | sets `loop` on the clip |
| **Play** / **Stop** | preview playback in place. The clip stays selected when playback ends (a non-looping clip parks the playhead back at 0 %), so previewing never switches you to another clip |
| 0–100 % scrubber | move the playhead; keyframe positions are drawn as tick marks, the one under the playhead highlighted |
| `+ Keyframe` | store the pose currently on screen at the playhead (creates the keyframe, or replaces the one already there) |
| `- Keyframe` | remove the keyframe sitting on the playhead |

**Workflow for a two-pose wave:**

1. `+ Clip` — an empty clip is created and selected.
2. With the playhead at 0 %, press `+ Keyframe`. Every bone is captured at
   its current delta (all identity), so the clip starts from the rest pose.
3. Scrub to 40 %.
4. Drag or rotate the arm (wheel to rotate, shift+wheel to scale, drag the ring
   at the bone tip). While a clip is selected, drags edit the clip pose, not
   the bind pose, so the rig itself is untouched.
5. Press `+ Keyframe` — the raised arm is stored at 40 %.
6. Scrub to 100 %, drag the arm back, `+ Keyframe` again.
7. Press **Play** to preview, then **Save rig** to write the clip into
   `rig.json`.

The readout next to **Play** shows `percent · ms · keys`, plus `playing` while
previewing, `finished` when a non-looping clip reached its end, and `unsaved`
when the pose on screen has not been captured yet. An uncaptured pose is
discarded when the playhead moves (or when the clip is re-selected) — keyframe
capture is not part of the `Ctrl+Z` bone undo history.

## Validation & error handling

Loading or importing rejects, with a readable message:

* missing or unparsable JSON
* a rig without a `bones` array, or with duplicate bone ids
* a bone whose `parent` does not exist, or a hierarchy containing a cycle
* a part with neither `image` nor `shape`
* an `animations` block with a `duration_ms <= 0`, a keyframe `t` outside
  `0.0..=1.0`, or a keyframe that references an unknown bone
* an image that is missing, escapes the model folder (`..`), or is larger than 16 MB

`GET /models` reports broken folders with an `error` string instead of failing
the whole list, and the dropdown shows them as `name (broken)`.

## Importing

### Accepted formats

| file | becomes |
| --- | --- |
| `.json` | rig manifest → new model: the manifest plus only the sprites it references, copied into `models/<slug>/` |
| `.toml` | rig manifest (converted to JSON on install) |
| `.png` `.jpg` `.jpeg` `.webp` `.svg` | an image — either a new character or a part |

The picker filters to exactly these types. Anything else is rejected with a
message naming the accepted formats.

A rig import is selective: only the manifest plus the files it names
(`part.image` and every `part.variants` entry, kept at their relative paths)
are copied into `models/<slug>/`. Everything else in the source folder is left
alone, so importing a `model.json` that happens to sit in `Downloads/` or on the
Desktop does not drag that folder — or its gigabytes of unrelated files — into
`models/`. A sprite path that escapes the folder (`../x.png`), is absolute, is
missing, or is larger than 16 MB fails the import before anything is written.

TOML has no `null`, so a root bone simply omits `parent` (or uses `""`/`"none"`):

```toml
format = "desktop-buddy/model@1"
name = "tomlhero"

[[bones]]
id = "root"
pos = [0, 0]

[[bones.parts]]
id = "body"
shape = "ellipse"
color = "#f0a020"
size = [120, 120]
```

### Importing

* **GUI**: **Import…** opens a native file picker (parented to the avatar window,
  which drops its always-on-top flag while the dialog is up so the dialog is
  never hidden). Rigs install immediately; images open a chooser:
  * **New character** — creates `models/<slug>/` containing the sprite as the
    body of a one-bone rig.
  * **Attach as part** — copies the sprite into the current model's `parts/`
    and adds it to the selected bone (or the root) as a new part, then selects
    it so you can position it. Press **Save rig** to persist.
* **API**: `POST /import {"path": ".../model.toml"}` for rigs,
  `{"path": ".../hat.png", "mode": "model"}` or
  `{"path": ".../hat.png", "mode": "part", "tag": "accessory"}` for images.
* **Manual**: copy a folder into `models/` yourself; it appears in the dropdown
  after the next refresh.

## Part tags and sprite variants

Parts may carry a semantic `tag` (`head`, `torso`, `arm_left`, `arm_right`,
`leg_left`, `leg_right`, `eye`, `mouth`, `accessory`, `custom`) so animations
and the API can address them without knowing bone names.

A part can also hold `variants` — alternative sprites switched by name:

```json
{
  "id": "mouth",
  "tag": "mouth",
  "image": "parts/mouth_closed.png",
  "state": "closed",
  "variants": {
    "closed": "parts/mouth_closed.png",
    "open": "parts/mouth_open.png",
    "smile": "parts/mouth_smile.png"
  }
}
```

* every path in `variants` is loaded with the model (validation checks them too)
* `state` picks the live sprite; `POST /part {"target":{"tag":"mouth"},"patch":{"state":"open"}}`
  or `POST /mouth {"state":"open"}` switches it at runtime
* `POST /mouth {"state":"talk"}` animates the mouth part (opening/closing at
  ~3 Hz); expressions also drive mouth parts through the `mouth_open`
  blend shape, so imported mouths react to `happy`/`surprised` etc.

## Expressions, blinking and talking

Facial parts are driven by their `tag` and their `variants`:

| tag | variant used per expression |
| --- | --- |
| `eye` | `neutral`→`open`, `happy`→`happy`, `surprised`→`wide`, `thinking`→`look_up`, `sad`→`sad`, `angry`→`angry`, plus `blink` |
| `mouth` | `neutral`→`closed`, `happy`→`smile`, `surprised`→`open`, `thinking`→`flat`, `sad`→`frown`, `angry`→`frown`, plus `open`/`closed` for talking |

* Missing variants fall back to procedural animation: mouths squash/stretch with
  the expression blend shapes, eyes blink by vertical scaling.
* Tags work on shape parts too — an ellipse tagged `eye` blinks by
  squashing and a shape tagged `mouth` stretches with the expression, so a
  character can be built with no sprites at all (`models/nebulon-9` does exactly
  that: shape eyes with pupils, a shape mouth, plus a third eye).
* Auto-blink runs every 3–6 s for ~120 ms; if the eye part has a `blink`
  variant it is used, otherwise the sprite is squashed.
* Talking alternates `open`/`closed` (or squashes the mouth) while a speech
  bubble is visible.
* Save these mappings by giving the part variants with those names — the editor
  does it for you: select the part, pick an expression in `expr`, press
  **Bind PNG**, and choose the sprite. The preset strip (`N`/`H`/`S`/`T`/`Sa`/`A`) previews
  each expression with one click.

### Procedural expression channels (shape parts, no sprites)

Every expression is a set of seven blendshapes, eased toward the preset over
`time` seconds. Shape parts read them by `tag`, so a rig with no sprites still
acts:

| channel | range | what it does |
| --- | --- | --- |
| `mouth_open` | `0..1` | mouth squash (also driven by the typewriter/voice while talking) |
| `mouth_wide` | `0..1` | mouth widens |
| `mouth_curve` | `-1..1` | `+` = corners up (smile), `−` = corners down (frown) |
| `eye_wide` | `0..1` | eyes grow, pupils dilate |
| `eye_squint` | `0..1` | eyes flatten (happy/sad/angry) |
| `brow_up` | `-1..1` | brows rise (`+`) or drop (`−`) |
| `brow_angle` | `-1..1` | `+` = inner ends down (angry), `−` = inner ends up (sad) |

| tag | response |
| --- | --- |
| `mouth` | squash + widen + corner lift/tilt |
| `eye` | widen/squint, plus the blink squash |
| `brow` | vertical travel + mirrored tilt |
| `pupil` | dilates with `eye_wide` |

Two authoring rules make the mouth and brows read:

* **A mouth needs two halves.** `mouth_curve` lifts and tilts each half by the
  sign of its `offset.x`, so author `mouth_l` / `mouth_r` (or a centre plus two
  corner parts) at `x = ±7`; a single centred ellipse can only tilt.
* **Brows must be ellipses, not capsules.** A capsule's origin sits at its left
  end (`[-h/2, w-h/2]`), so a mirrored pair is not symmetric — the right brow
  ends up shifted outward by `w - h`. Ellipses are centred on their offset,
  so `±x` mirrors exactly. This is the commonest "one eyebrow is funky" bug in
  a hand-written face.

Presets live in `ui/js/state.js::EXPRESSIONS`: `neutral` is all zeros; `happy` is
`mouth_curve 0.9 / eye_squint 0.35 / brow_up 0.3`; `surprised` is
`mouth_open 0.95 / eye_wide 0.9 / brow_up 0.9`; `sad` is
`brow_angle -0.85 / mouth_curve -0.85`; `angry` is
`brow_angle 0.9 / eye_squint 0.55 / brow_up -0.15`.

## Building a character inside the editor

**Add content** (bottom of the inspector) creates new geometry on the selected
bone:

| control | effect |
| --- | --- |
| `type` | semantic tag for the new part (`custom`, `mouth`, `head`, `arm_left`, …) |
| `source` | Capsule / Ellipse placeholder, or Model image (any image already in the model) |
| **Add part** | appends the part to the selected bone, using the next free z-index |
| **+ Limb** | creates a new bone (child of the selection) with a capsule part and a walk/sway animation channel |

Part lifecycle buttons (in the semantics block of a selected part):

* **Dup** — copies the part with a `-2` id and a small offset
* **Del** — removes the part
* **+ Image** — imports an image and stores it as a variant under the name
  typed in `variant` (e.g. type `open`, then pick `mouth_open.png`)
* `state` — switch which variant is displayed

Everything (new limbs, parts, tags, variants, states and image paths) is written
by **Save rig** into `rig.json` and reloads intact on the next start.
