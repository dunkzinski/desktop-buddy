/**
 * char-rig — rig model ingestion, skeletal transforms, geometry, serialization
 *
 * Depends on: state.js (state + helpers) and ipc.js (Tauri commands).
 * Nothing in this module reads from editor/renderer/bridge, so it can be
 * imported by all of them without creating a cycle. Geometry lives here
 * because the renderer, the hit-tester (editor) and the click-through region
 * sync all need the exact same maths.
 */

import {
  avatar, RT, TAU, animation, gaze,
  clamp, num, round,
  setDirty, toast, viewport,
} from "./state.js";

import { call } from "./ipc.js";

/** Head tilt (radians, ~3°) at full hover — the "the cursor is on me" sign. */
const HOVER_HEAD_TILT = 0.05;

/**
 * Is this the head bone? A `head`-tagged part is authoritative; a rig that
 * never tagged its parts still gets the life signs through the bone id.
 */
function isHeadBone(bone) {
  return !!bone
    && (bone.parts.some((part) => part.tag === "head") || /head/i.test(bone.id));
}

/* ---------- model loading ---------- */

export function boneDefaults(bone) {
  bone.pos = Array.isArray(bone.pos) ? [num(bone.pos[0], 0), num(bone.pos[1], 0)] : [0, 0];
  bone.rotation = num(bone.rotation, 0);
  bone.scale = num(bone.scale, 1);
  bone.length = num(bone.length, 0);
  bone.z = num(bone.z, 0);
  // Limits are opt-in: absent constraints mean "free 360° rotation".
  bone.limits = bone.limits && typeof bone.limits === "object" ? bone.limits : {};
  bone.anim = bone.anim || {};
  bone.parts = Array.isArray(bone.parts) ? bone.parts : [];
  for (const part of bone.parts) {
    part.offset = Array.isArray(part.offset) ? part.offset : [0, 0];
    part.pivot = Array.isArray(part.pivot) ? part.pivot : [0.5, 0.5];
    part.rotation = num(part.rotation, 0);
    part.scale = num(part.scale, 1);
    part.z = num(part.z, 0);
    if (Array.isArray(part.size)) part.size = [num(part.size[0], 32), num(part.size[1], 32)];
  }
  return bone;
}

export function rebuildOrder() {
  if (!RT.model || !RT.model.rig) return;
  RT.bones = {};
  RT.order = [];
  RT.boneList = (RT.model.rig.bones || []).map((bone) => boneDefaults(bone));
  for (const bone of RT.boneList) RT.bones[bone.id] = bone;

  const placed = new Set();
  let guard = 0;
  while (RT.order.length < RT.boneList.length && guard < 500) {
    guard += 1;
    for (const bone of RT.boneList) {
      if (placed.has(bone.id)) continue;
      if (!bone.parent || placed.has(bone.parent) || !RT.bones[bone.parent]) {
        RT.order.push(bone.id);
        placed.add(bone.id);
      }
    }
  }
  for (const bone of RT.boneList) if (!placed.has(bone.id)) RT.order.push(bone.id);

  for (const bone of RT.boneList) bone.children = [];
  for (const bone of RT.boneList) {
    if (bone.parent && RT.bones[bone.parent]) RT.bones[bone.parent].children.push(bone.id);
  }
}

function loadImages(model) {
  RT.images = {};
  for (const [path, dataUrl] of Object.entries(model.images || {})) {
    const img = new Image();
    img.onload = () => {
      for (const bone of Object.values(RT.bones)) {
        for (const part of bone.parts) {
          if (part.image === path && !Array.isArray(part.size)) {
            part.size = [img.naturalWidth, img.naturalHeight];
          }
        }
      }
    };
    img.onerror = () => console.error("[avatar] image failed:", path);
    img.src = dataUrl;
    RT.images[path] = img;
  }
}

export function applyModel(model) {
  if (!model || !model.rig) return;
  RT.model = model;
  RT.pose = {};
  RT.selected = null;
  RT.drag = null;
  RT.history = [];
  // A new rig invalidates the clip that was playing against the old one.
  animation.clip = null;
  animation.offsets = {};
  animation.draft = {};
  animation.playing = false;
  animation.finished = false;
  animation.time = 0;
  animation.t = 0;
  rebuildOrder();
  loadImages(model);
  setDirty(false);

  const select = document.getElementById("model-select");
  if (select) {
    // A procedural creature is deliberately absent from the picker (see
    // refreshModelList), but the loaded model still has to be *in* the control
    // or it would sit there showing another rig's name — or nothing at all.
    // So: drop the entry the *previous* model left behind (every digivolution
    // used to add one, and they piled up until the next restart), then add this
    // one if the picker does not already list it.
    for (const option of [...select.options]) {
      if (option.dataset.adHoc === "1") option.remove();
    }
    if (model.name) {
      if (![...select.options].some((option) => option.value === model.name)) {
        const option = document.createElement("option");
        option.value = model.name;
        option.dataset.adHoc = "1";
        // Name only: the control is narrow, and "· generated" pushed the seed
        // (the part that identifies the creature) out of view. The tooltip
        // says the rest.
        option.textContent = model.name;
        option.title =
          "Procedural creature (digivolution) — the picker lists custom models only";
        select.insertBefore(option, select.firstChild);
      }
      select.value = model.name;
    }
  }
  const brand = document.getElementById("brand");
  if (brand) brand.textContent = model.label || model.name || "Avatar";

  for (const bone of RT.boneList) {
    for (const part of bone.parts) {
      if (part.tag === "mouth" && part.variants && !part.state) part.state = Object.keys(part.variants)[0];
    }
  }
  wireRegions();
  console.log("[avatar] model:", model.name, RT.boneList.length, "bones");
  return RT.boneList.length;
}

export async function refreshModelList(selectName) {
  const models = await call("list_models");
  const select = document.getElementById("model-select");
  if (!select) return;
  // The picker is for models you keep: hand-authored rigs, and generated ones
  // you have edited and saved (they carry a rig.json override). Procedural
  // creatures are the digivolution line's business — the Digivolve card names
  // the current one — so listing every stage of every line here only buried
  // the two or three rigs a person actually switches between.
  const kept = (models || []).filter(
    (entry) => !entry.generated || entry.has_rig_override,
  );
  // Whatever is loaded right now has to stay selectable, procedural or not, or
  // the control would show the wrong model (or an empty value).
  if (selectName && !kept.some((entry) => entry.name === selectName)) {
    const current = (models || []).find((entry) => entry.name === selectName);
    if (current) kept.unshift(current);
  }
  select.innerHTML = "";
  for (const entry of kept) {
    const option = document.createElement("option");
    option.value = entry.name;
    if (entry.generated) option.dataset.adHoc = "1";
    option.textContent = entry.error
      ? entry.name + " (broken)"
      : entry.name +
        " · " +
        entry.bones +
        "b/" +
        entry.parts +
        "p" +
        (entry.generated ? " · generated" : "");
    option.title = entry.error || option.textContent;
    select.appendChild(option);
  }
  if (selectName) select.value = selectName;
}

/* ---------- transforms ---------- */

export function animOffset(bone) {
  if (RT.rigMode) return { rot: 0, bob: 0 };
  const anim = bone.anim || {};
  const phase = num(anim.phase, 0);
  let rot = 0;
  if (anim.sway) rot += Math.sin(avatar.time * 1.3 + phase) * anim.sway;
  if (anim.walk && avatar.mode === "walking") {
    rot += Math.sin(avatar.walkPhase * TAU + phase) * anim.walk;
  }
  if (anim.wave && avatar.mode === "waving") {
    rot += Math.sin(avatar.wavePhase + phase) * anim.wave;
  }
  // Hover life-sign: a soft head tilt while the cursor rests on the avatar.
  // `gaze.hover` is eased by main.js, so the tilt eases in and back out.
  // The yaw bias is the global eye-tracking half of the same idea: the head
  // turns a few degrees toward whatever the eyes are following (the OS cursor,
  // a perch, or an explicit `look_at`). Both are added here and clamped by
  // `localRotation` against the bone's `limits`, so a rig that declares a neck
  // range can never be spun past it.
  if (isHeadBone(bone)) {
    rot += gaze.hover * HOVER_HEAD_TILT + num(gaze.tilt, 0);
  }
  const bob = anim.bob ? Math.sin(avatar.time * 1.8 + phase) * anim.bob : 0;
  return { rot, bob };
}

/**
 * Clamp a rotation to the bone's limits. A missing/non-finite bound means the
 * bone may spin freely (multi-turn rotations are preserved).
 */
export function clampRotation(bone, value) {
  const limits = bone && bone.limits ? bone.limits : {};
  const min = Number.isFinite(limits.min) ? limits.min : -Infinity;
  const max = Number.isFinite(limits.max) ? limits.max : Infinity;
  return Math.min(max, Math.max(min, value));
}

/**
 * Sampled custom-animation delta for a bone, or `null` when the active clip does
 * not key it. The clip offsets are written by `animator.js` into
 * `animation.offsets`; reading them here (instead of importing the animator)
 * keeps `rig.js` below the animator in the module DAG.
 */
export function animationOffset(bone) {
  if (!bone || !bone.id || !animation) return null;
  const entry = animation.offsets ? animation.offsets[bone.id] : null;
  return entry && typeof entry === "object" ? entry : null;
}

export function localRotation(bone) {
  const override = RT.pose[bone.id];
  const clip = animationOffset(bone);
  const base = num(bone.rotation, 0) + animOffset(bone).rot + (clip ? num(clip.rotation, 0) : 0);
  const value = override && Number.isFinite(override.rotation) ? override.rotation : base;
  return clampRotation(bone, value);
}

export function localScale(bone) {
  const override = RT.pose[bone.id];
  const clip = animationOffset(bone);
  const base = num(bone.scale, 1) * (clip ? num(clip.scale, 1) : 1);
  return override && Number.isFinite(override.scale) ? override.scale : base;
}

export function computeWorld() {
  RT.world = {};
  for (const id of RT.order) {
    const bone = RT.bones[id];
    if (!bone) continue;
    const parent = bone.parent && RT.world[bone.parent]
      ? RT.world[bone.parent]
      : { x: avatar.x, y: avatar.y, rot: 0, scale: 1 };
    const cos = Math.cos(parent.rot);
    const sin = Math.sin(parent.rot);
    const clip = animationOffset(bone);
    const ox = bone.pos[0] + (clip ? num(clip.x, 0) : 0);
    const oy = bone.pos[1] + (clip ? num(clip.y, 0) : 0);
    const bob = animOffset(bone).bob;
    const override = RT.pose[bone.id] || {};
    const dx = num(override.x, 0);
    const dy = num(override.y, 0);
    RT.world[id] = {
      x: parent.x + (ox * cos - oy * sin) * parent.scale + dx,
      y: parent.y + (ox * sin + oy * cos) * parent.scale + dy + bob,
      rot: parent.rot + localRotation(bone),
      scale: parent.scale * localScale(bone),
    };
  }
}

/* ---------- part geometry ---------- */

export function partSize(part) {
  if (Array.isArray(part.size)) return part.size;
  const img = part.image ? RT.images[part.image] : null;
  if (img && img.naturalWidth) return [img.naturalWidth, img.naturalHeight];
  return [64, 64];
}

export function sortedDrawables() {
  const list = [];
  for (const bone of RT.boneList) {
    for (const part of bone.parts) list.push({ bone, part });
  }
  list.sort((a, b) => (a.bone.z - b.bone.z) || (a.part.z - b.part.z));
  return list;
}

/**
 * The part's world matrix — **the single source of truth** for both the
 * renderer (`drawPart`) and the editor (selection box, pivot cross, hit test).
 *
 * It mirrors `drawPart`'s transform chain exactly:
 *
 * ```text
 * translate(bone.x, bone.y) · rotate(bone.rot) · scale(worldScale * part.scale)
 *                           · translate(part.offset) · rotate(part.rotation)
 * ```
 *
 * Note the offset is multiplied by the *full* part scale (bone × part), because
 * the renderer applies `ctx.scale()` before `ctx.translate(offset)`.
 */
export function partWorldMatrix(bone, part) {
  const w = RT.world[bone.id];
  if (!w) return null;
  const scale = w.scale * num(part.scale, 1);
  const cos = Math.cos(w.rot);
  const sin = Math.sin(w.rot);
  const ox = part.offset[0];
  const oy = part.offset[1];
  return {
    x: w.x + (ox * cos - oy * sin) * scale,
    y: w.y + (ox * sin + oy * cos) * scale,
    rot: w.rot + num(part.rotation, 0),
    scale,
  };
}

/**
 * The rectangle a part actually paints, in its own local frame (before the
 * world matrix above).
 *
 * Images are drawn with their top-left at `-size * pivot`, so that rect is
 * `[-w·px, w(1-px)] × [-h·py, h(1-py)]`. **Shapes are not**: `drawShape` draws
 * centred on the local origin, so after the same `-size * pivot` pre-translate
 * an ellipse/face sits half a size up and left of the image rect, and a capsule
 * always spans `[-h/2, w-h/2]` (its own pivot maths cancels the pre-translate).
 * The selection box has to describe the pixels, not the authoring intent, or it
 * drifts off the sprite — which is exactly the `body`-part bug.
 */
export function partLocalRect(part) {
  const size = partSize(part);
  const w = size[0];
  const h = size[1];
  const px = part.pivot[0];
  const py = part.pivot[1];
  // `drawPart` prefers an image (or an active variant) over `shape`.
  const hasImage = !!part.image || !!(part.variants && Object.keys(part.variants).length);
  if (hasImage || !part.shape) {
    return { x: -w * px, y: -h * py, w, h };
  }
  if (part.shape === "capsule") {
    // Capsules are drawn from a round cap at the pivot to `w - h/2`.
    const r = h / 2;
    return { x: -r, y: -r, w, h };
  }
  // `face` is drawn with an extra `translate(w/2, h/2)`, so it fills the image
  // rect; circles and ellipses are centred on the pre-translated origin.
  if (part.shape === "face") {
    return { x: -w * px, y: -h * py, w, h };
  }
  return { x: -w * px - w / 2, y: -h * py - h / 2, w, h };
}

/** Axis-aligned bounding box of a part's artwork in screen space. */
export function partBox(bone, part) {
  const m = partWorldMatrix(bone, part);
  if (!m) return { x: 0, y: 0, w: 0, h: 0 };
  const rect = partLocalRect(part);
  const cos = Math.cos(m.rot);
  const sin = Math.sin(m.rot);
  const corners = [
    [rect.x, rect.y],
    [rect.x + rect.w, rect.y],
    [rect.x + rect.w, rect.y + rect.h],
    [rect.x, rect.y + rect.h],
  ].map((corner) => [
    m.x + (corner[0] * cos - corner[1] * sin) * m.scale,
    m.y + (corner[0] * sin + corner[1] * cos) * m.scale,
  ]);
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (const corner of corners) {
    minX = Math.min(minX, corner[0]);
    maxX = Math.max(maxX, corner[0]);
    minY = Math.min(minY, corner[1]);
    maxY = Math.max(maxY, corner[1]);
  }
  return { x: minX, y: minY, w: maxX - minX, h: maxY - minY };
}

/* ---------- hit regions / click-through ---------- */

export function avatarBounds() {
  let minX = avatar.x - 70, maxX = avatar.x + 70;
  let minY = avatar.y - 70, maxY = avatar.y + 70;
  for (const bone of RT.boneList) {
    if (!RT.world[bone.id]) continue;
    for (const part of bone.parts) {
      const box = partBox(bone, part);
      if (!Number.isFinite(box.x) || !Number.isFinite(box.w)) continue;
      minX = Math.min(minX, box.x);
      maxX = Math.max(maxX, box.x + box.w);
      minY = Math.min(minY, box.y);
      maxY = Math.max(maxY, box.y + box.h);
    }
  }
  return { x: minX, y: minY, w: maxX - minX, h: maxY - minY };
}

/**
 * Tell the Rust side which screen rectangles should accept mouse input. Called
 * on every UI change, on resize, and on a 200 ms timer (the avatar walks).
 */
export function wireRegions() {
  const regions = [];
  const push = (el) => {
    if (!el) return;
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return;
    regions.push({ x: r.left, y: r.top, w: r.width, h: r.height });
  };

  push(document.getElementById("titlebar"));
  if (avatar.collapsed) push(document.getElementById("panel-toggle"));
  else push(document.getElementById("control-panel"));

  if (RT.rigMode) {
    regions.push({ x: 0, y: 0, w: viewport.w, h: viewport.h });
    push(document.getElementById("inspector"));
  } else if (avatar.dragging) {
    // A play-mode drag keeps the whole window interactive: the 35 ms cursor
    // tracker would otherwise flip the overlay click-through the moment a fast
    // drag outran the avatar box, and the gesture would lose the mouse.
    regions.push({ x: 0, y: 0, w: viewport.w, h: viewport.h });
  } else {
    regions.push(avatarBounds());
  }
  // The pop-out watcher console floats over the click-through desktop; without
  // its own region the buttons under it would never receive a click.
  const drawer = document.getElementById("watcher-drawer");
  if (drawer && !drawer.classList.contains("hidden")) push(drawer);
  const chooser = document.getElementById("import-chooser");
  if (chooser && !chooser.classList.contains("hidden")) push(chooser);
  // The command bar floats over the desktop too: without its own region the
  // user could see the input but never focus it.
  const commandBar = document.getElementById("command-bar");
  if (commandBar && !commandBar.classList.contains("hidden")) push(commandBar);
  // The history drawer is a floating card as well — while it is open its rows,
  // Details toggles and Copy buttons must accept clicks over a click-through
  // desktop. Withdrawing the region when it closes is what re-arms
  // click-through, so it is published only while visible.
  const historyDrawer = document.getElementById("history-drawer");
  if (historyDrawer && !historyDrawer.classList.contains("hidden")) push(historyDrawer);
  // The settings modal is a full-screen sheet: while it is up it must take
  // every click (it is the only thing on screen), so its box is published in
  // place of nothing.
  const settingsModal = document.getElementById("settings-modal");
  if (settingsModal && !settingsModal.classList.contains("hidden")) push(settingsModal);
  // The pop-out cards are floating cards too: their pickers and buttons must
  // accept clicks over the click-through desktop, so each box goes in only
  // while that card is open. Forgetting this makes the card *look* live while
  // every click falls through to whatever is behind it.
  for (const id of ["survival-card", "digivolve-card"]) {
    const card = document.getElementById(id);
    if (card && !card.classList.contains("hidden")) push(card);
  }
  // The speech bubble hosts the quick-reply box: while it is on screen its box
  // must accept clicks, or the user could see the input but not type in it.
  const bubble = document.getElementById("speech");
  if (bubble && !bubble.classList.contains("hidden")) push(bubble);

  call("update_hit_regions", { regions: regions });

  if (!avatar.clickThroughArmed && !avatar.clickThroughUserSet && regions.length > 0) {
    avatar.clickThroughArmed = true;
    avatar.clickThrough = true;
    document.getElementById("btn-clickthrough").classList.add("pinned");
    call("set_click_through", { enabled: true });
  }
}

/* ---------- image cache ---------- */

export function ensureImage(path, dataUrl) {
  if (!path || !dataUrl || RT.images[path]) return RT.images[path];
  const img = new Image();
  img.src = dataUrl;
  RT.images[path] = img;
  return img;
}

/** Apply the part's current variant (if any) to its active image. */
export function applyPartVariant(part) {
  if (!part.variants || !part.state) return;
  const target = part.variants[part.state];
  if (typeof target === "string") part.image = target;
}

/* ---------- rig serialization ---------- */

/**
 * Copy `rig.animations` into the document the editor saves.
 *
 * Always emitted (as `{}` when there are no clips) so saving an edited rig never
 * silently re-inherits clips from `model.json`, and every value is rounded and
 * clamped to what `models::validate_animations` accepts.
 */
export function serializeAnimations() {
  const source = RT.model && RT.model.rig ? RT.model.rig.animations : null;
  const out = {};
  if (!source || typeof source !== "object") return out;

  for (const [name, clip] of Object.entries(source)) {
    if (!clip || typeof clip !== "object") continue;
    const keyframes = Array.isArray(clip.keyframes) ? clip.keyframes : [];
    out[name] = {
      duration_ms: Math.max(1, round(num(clip.duration_ms, 1200), 2)),
      loop: !!clip.loop,
      keyframes: keyframes
        .map((keyframe) => {
          const bones = {};
          const entries = keyframe && keyframe.bones && typeof keyframe.bones === "object"
            ? Object.entries(keyframe.bones)
            : [];
          for (const [boneId, value] of entries) {
            if (!value || typeof value !== "object") continue;
            bones[boneId] = {
              rotation: round(num(value.rotation, 0), 4),
              x: round(num(value.x, 0), 2),
              y: round(num(value.y, 0), 2),
              scale: round(num(value.scale, 1), 3),
            };
          }
          return { t: round(clamp(num(keyframe && keyframe.t, 0), 0, 1), 4), bones: bones };
        })
        .sort((a, b) => a.t - b.t),
    };
  }
  return out;
}

export function serializeRig() {
  return {
    format: "char-rig/model@1",
    name: RT.model ? RT.model.name : null,
    label: RT.model ? RT.model.label : null,
    notes: RT.model && RT.model.rig ? RT.model.rig.notes : null,
    animations: serializeAnimations(),
    bones: RT.boneList.map((bone) => ({
      id: bone.id,
      parent: bone.parent || null,
      pos: [round(bone.pos[0], 2), round(bone.pos[1], 2)],
      rotation: round(num(bone.rotation, 0), 4),
      scale: round(num(bone.scale, 1), 3),
      length: round(num(bone.length, 0), 2),
      z: bone.z,
      limits: {
        min: Number.isFinite(bone.limits.min) ? round(bone.limits.min, 4) : null,
        max: Number.isFinite(bone.limits.max) ? round(bone.limits.max, 4) : null,
      },
      anim: {
        walk: round(num(bone.anim.walk, 0), 3),
        sway: round(num(bone.anim.sway, 0), 3),
        wave: round(num(bone.anim.wave, 0), 3),
        bob: round(num(bone.anim.bob, 0), 3),
        phase: round(num(bone.anim.phase, 0), 3),
      },
      parts: bone.parts.map((part) => {
        const size = partSize(part);
        const out = {
          id: part.id,
          pivot: [round(part.pivot[0], 3), round(part.pivot[1], 3)],
          offset: [round(part.offset[0], 1), round(part.offset[1], 1)],
          rotation: round(num(part.rotation, 0), 4),
          scale: round(num(part.scale, 1), 3),
          z: part.z,
          size: [round(size[0], 1), round(size[1], 1)],
        };
        if (part.image) out.image = part.image;
        if (part.shape) out.shape = part.shape;
        if (part.color) out.color = part.color;
        if (part.tag) out.tag = part.tag;
        if (part.state) out.state = part.state;
        if (part.lockBone) out.lockBone = true;
        if (part.variants && Object.keys(part.variants).length) out.variants = part.variants;
        return out;
      }),
    })),
  };
}

export async function saveRig() {
  if (!RT.model) { toast("Load a model first", true); return; }
  const rig = serializeRig();
  const path = await call("save_rig", { rig });
  if (path) {
    RT.model.rig = rig;
    setDirty(false);
    toast("Rig saved -> " + path);
    refreshModelList(RT.model.name);
  }
}

export async function resetRig() {
  const model = await call("reset_rig");
  if (model) {
    applyModel(model);
    toast("Rig reset to model.json");
    refreshModelList(model.name);
  }
}
