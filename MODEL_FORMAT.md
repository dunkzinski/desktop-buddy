# Custom models & rig format

Drop a folder into `models/` (or use **Import…** in the control panel), then
switch to it from the model dropdown or `POST /model`.

```
models/
  xenomorph-7/            shipped, procedural shapes (no textures)
    model.json
  nebulon-9/              shipped alien: procedural shapes, tagged face parts,
    model.json            four keyframe clips (wave/breathe/scan/alert)
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

* `models/` root can be relocated with the `CHAR_RIG_MODELS` environment
  variable; otherwise the app looks next to the executable and falls back to
  `<repo>/models`.
* `.state.json` (inside `models/`) remembers the last selected model, so the
  choice survives restarts.
* The rig editor writes **`rig.json`**, never `model.json`. `rig.json` wins when
  present, so **Reset** simply deletes it and reloads the shipped manifest.

## Manifest schema (`model.json`)

```json
{
  "format": "char-rig/model@1",
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

Bone rotation follows screen space: `0` points along **+x (right)** and `+π/2`
points **down**, since y grows downward.

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
  "format": "char-rig/model@1",
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

## Rig editor

Open with the **wrench** button in the titlebar, the **Rig edit** button in the
control panel, or the `R` key. Entering the mode freezes the animation and parks
the avatar in the middle of the screen.

| action | result |
| --- | --- |
| click a joint / part | select it; the inspector fills in |
| drag a joint | move that bone (`pos`) |
| drag the ring at the bone tip | rotate the bone, clamped by `limits` |
| drag a part | change the part's `offset` — or move its bone when the part is **locked** (see below) |
| wheel over a selected part | rotate the **part** by 0.05 rad per tick (snapped to 4 decimals), leaving the bone alone |
| shift + wheel over a selected part | scale the **part** |
| wheel over a selected bone (no part) | rotate the bone by 0.02 rad |
| shift + wheel over a selected bone | scale the bone |
| `Ctrl+Z` / Undo | step back through edits (80 levels) |
| turn the wheel / type in the inspector | numeric precision for every field |

**What a press grabs.** A part you already have selected — or one locked to its
bone — owns the press, even where the bone's rotation ring overlaps the artwork
(the ring sits at the bone tip, which for a limb sprite is *inside* the image).
Without that rule a drag on the sprite's far end would rotate the bone instead of
moving the part. The ring still wins when you press it *outside* every part box,
or when only a bone is selected.

The inspector exposes position, rotation, scale, length, z-order, rotation
limits, all five animation channels, and — for the selected part — size, pivot,
offset, rotation, z-order and scale. Bones can be re-parented from the dropdown
(cycles are rejected), created (`+ Bone`, a capsule-child of the selection) and
deleted together with their descendants.

### Adding bones

**+ Bone** sits in the toolbar at the top of the inspector, so it is reachable
without selecting anything first (the footer copy still works too):

* with a bone selected it adds a capsule child of that bone;
* with nothing selected it adds a child of `root` (or of the first bone when the
  rig has no `root`);
* with an **empty rig** it creates the `root` bone itself.

Either way the new bone is selected immediately, so its joint and rotation ring
are highlighted on the canvas and the inspector is ready for edits.

### Locking a bone to a part

The part inspector has a **bone lock** group:

| control | effect |
| --- | --- |
| **Lock Bone to Part** | while checked, the part is welded to its bone: dragging the part moves the **parent bone** (`pos`, in the parent's rotated frame) and the part keeps its local `offset`, so sprite and pivot travel together — and the wheel / shift+wheel rotate and scale the **bone** instead of the sprite. Saved as `"lockBone": true` on the part |
| **Snap Bone to Part** | moves the parent bone so its origin lands exactly on the part, then zeros the part's `offset` — the bone pivot sits on the sprite without it jumping |

While locked, a press anywhere on the sprite drags the bone (the rotation ring
does not steal it); press the ring outside the artwork, or untick the lock, when
you want to aim the sprite on its own. Both controls are ordinary rig edits:
`Ctrl+Z` steps back, and **Save rig** persists them.

**Save rig** writes `models/<name>/rig.json` atomically (write + rename) after
validating the hierarchy and every referenced image. **Reset** deletes that
override and reloads `model.json`.

### Timeline editor (authoring clips)

While Rig Edit Mode is open a compact timeline bar sits above the editor hint.
It edits `animations` on the rig and is persisted by the same **Save rig**.

| control | what it does |
| --- | --- |
| **clip** dropdown | choose the clip to preview and author; selecting one applies its pose at `0 %` |
| **clip name** field | type a name and press **Enter** (or click away) to rename the selected clip. Names are trimmed; a blank name or one already in use is rejected with a toast and the field snaps back. With no clip selected the field is the name for the next **+ Clip** |
| **+ Clip** | create an empty clip — named from the field when it is filled in, otherwise the next free `clip 1`, `clip 2`, … — with `duration_ms: 1200` and `loop: false` |
| **Del Clip** | delete the selected clip |
| **duration** | clip length in ms (`>= 1`); the playhead keeps its percentage |
| **loop** | sets `loop` on the clip |
| **Play / Stop** | preview playback in place. The clip stays selected when playback ends (a non-looping clip parks the playhead back at `0 %`), so previewing never switches you to another clip |
| 0–100 % scrubber | move the playhead; keyframe positions are drawn as tick marks, the one under the playhead highlighted |
| **+ Keyframe** | store the pose currently on screen at the playhead (creates the keyframe, or replaces the one already there) |
| **- Keyframe** | remove the keyframe sitting on the playhead |

**Workflow for a two-pose wave:**

1. **+ Clip** — an empty clip is created and selected.
2. With the playhead at `0 %`, press **+ Keyframe**. Every bone is captured at
   its current delta (all identity), so the clip starts from the rest pose.
3. Scrub to `40 %`.
4. Drag or rotate the arm (wheel to rotate, shift+wheel to scale, drag the ring
   at the bone tip). While a clip is selected, drags edit the **clip pose**, not
   the bind pose, so the rig itself is untouched.
5. Press **+ Keyframe** — the raised arm is stored at `40 %`.
6. Scrub to `100 %`, drag the arm back, **+ Keyframe** again.
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
| `.json` | rig manifest → new model (folder is copied into `models/<slug>/`) |
| `.toml` | rig manifest (converted to JSON on install) |
| `.png` `.jpg` `.jpeg` `.webp` `.svg` | an image — either a new character or a part |

The picker filters to exactly these types. Anything else is rejected with a
message naming the accepted formats.

TOML has no `null`, so a root bone simply omits `parent` (or uses `""`/`"none"`):

```toml
format = "char-rig/model@1"
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

* **GUI:** *Import…* opens a native file picker (parented to the avatar window,
  which drops its always-on-top flag while the dialog is up so the dialog is
  never hidden). Rigs install immediately; **images open a chooser**:

  * **New character** — creates `models/<slug>/` containing the sprite as the
    body of a one-bone rig.
  * **Attach as part** — copies the sprite into the current model's `parts/`
    and adds it to the selected bone (or the root) as a new part, then selects
    it so you can position it. Press **Save rig** to persist.

* **API:** `POST /import {"path": ".../model.toml"}` for rigs,
  `{"path": ".../hat.png", "mode": "model"}` or
  `{"path": ".../hat.png", "mode": "part", "tag": "accessory"}` for images.
* **Manual:** copy a folder into `models/` yourself; it appears in the dropdown
  after the next refresh.

## Part tags and sprite variants

Parts may carry a semantic `tag` (`head`, `torso`, `arm_left`, `arm_right`,
`leg_left`, `leg_right`, `eye`, `mouth`, `accessory`, `custom`) so animations
and the API can address them without knowing bone names.

A part can also hold **variants** — alternative sprites switched by name:

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
  ~3 Hz); expressions also drive `mouth` parts through the `mouth_open`
  blend shape, so imported mouths react to happy/surprised etc.

## Expressions, blinking and talking

Facial parts are driven by their **tag** and their **variants**:

| tag | variant used per expression |
| --- | --- |
| `eye` | `neutral→open`, `happy→happy`, `surprised→wide`, `thinking→look_up`, `sad→sad`, `angry→angry`, plus `blink` |
| `mouth` | `neutral→closed`, `happy→smile`, `surprised→open`, `thinking→flat`, `sad→frown`, `angry→frown`, plus `open`/`closed` for talking |

* Missing variants fall back to procedural animation: mouths squash/stretch with
  the expression blend shapes, eyes blink by vertical scaling.
* Tags work on **shape** parts too — an `ellipse` tagged `eye` blinks by
  squashing and a shape tagged `mouth` stretches with the expression, so a
  character can be built with no sprites at all (`models/nebulon-9` does exactly
  that: shape eyes with pupils, a shape mouth, plus a third eye).
* **Auto-blink** runs every 3–6 s for ~120 ms; if the eye part has a `blink`
  variant it is used, otherwise the sprite is squashed.
* **Talking** alternates `open`/`closed` (or squashes the mouth) while a speech
  bubble is visible.
* Save these mappings by giving the part variants with those names — the editor
  does it for you: select the part, pick an expression in **expr**, press
  **Bind PNG**, and choose the sprite. The preset strip (N/H/S/T/Sa/A) previews
  each expression with one click.

Example:

```json
{
  "id": "mouth", "tag": "mouth",
  "image": "parts/mouth_closed.png", "state": "closed",
  "variants": { "closed": "parts/mouth_closed.png", "open": "parts/mouth_open.png",
                "smile": "parts/mouth_smile.png" }
}
```

## Building a character inside the editor

`Add content` (bottom of the inspector) creates new geometry on the selected
bone:

| control | effect |
| --- | --- |
| **type** | semantic tag for the new part (`custom`, `mouth`, `head`, `arm_left`, …) |
| **source** | `Capsule` / `Ellipse` placeholder, or `Model image` (any image already in the model) |
| **Add part** | appends the part to the selected bone, using the next free z-index |
| **+ Limb** | creates a new bone (child of the selection) with a capsule part and a walk/sway animation channel |

Part lifecycle buttons (in the *semantics* block of a selected part):

* **Dup** — copies the part with a `-2` id and a small offset
* **Del** — removes the part
* **+ Image** — imports an image and stores it as a **variant** under the name
  typed in *variant* (e.g. type `open`, then pick `mouth_open.png`)
* **state** — switch which variant is displayed

Everything (new limbs, parts, tags, variants, states and image paths) is written
by **Save rig** into `rig.json` and reloads intact on the next start.
