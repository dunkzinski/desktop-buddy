/**
 * char-rig — rig model ingestion, skeletal transforms, serialization
 * Depends on: state.js
 */

import {
  avatar, RT,
  clamp, round, num, getPath, setPath,
  EXPRESSIONS, PRESET_STATES, PART_TAGS,
  snapshot, setDirty,
} from "./state.js";

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
  RT.history = [];
  rebuildOrder();
  loadImages(model);
  setDirty(false);

  const select = document.getElementById("model-select");
  if (select && model.name) select.value = model.name;
  document.getElementById("brand").textContent = model.label || model.name || "Avatar";
  rebuildAddImageOptions();
  for (const bone of RT.boneList) {
    for (const part of bone.parts) if (part.tag === "mouth" && part.variants && !part.state) part.state = Object.keys(part.variants)[0];
  }
  renderInspector();
  wireRegions();
  console.log("[avatar] model:", model.name, RT.boneList.length, "bones");
}

export async function refreshModelList(selectName) {
  const models = await call("list_models");
  const select = document.getElementById("model-select");
  if (!select) return;
  select.innerHTML = "";
  for (const entry of models || []) {
    const option = document.createElement("option");
    option.value = entry.name;
    option.textContent = entry.error
      ? entry.name + " (broken)"
      : entry.name + " · " + entry.bones + "b/" + entry.parts + "p";
    if (entry.error) option.title = entry.error;
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

export function localRotation(bone) {
  const override = RT.pose[bone.id];
  const base = num(bone.rotation, 0) + animOffset(bone).rot;
  const value = override && Number.isFinite(override.rotation) ? override.rotation : base;
  return clampRotation(bone, value);
}

export function localScale(bone) {
  const override = RT.pose[bone.id];
  const base = num(bone.scale, 1);
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
    const ox = bone.pos[0];
    const oy = bone.pos[1];
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

export function partSize(part) {
  if (Array.isArray(part.size)) return part.size;
  const img = part.image ? RT.images[part.image] : null;
  if (img && img.naturalWidth) return [img.naturalWidth, img.naturalHeight];
  return [64, 64];
}

/* ---------- rig serialization ---------- */

export function serializeRig() {
  return {
    format: "char-rig/model@1",
    name: RT.model ? RT.model.name : null,
    label: RT.model ? RT.model.label : null,
    notes: RT.model && RT.model.rig ? RT.model.rig.notes : null,
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
        if (part.variants && Object.keys(part.variants).length) out.variants = part.variants;
        return out;
      }),
    })),
  };
}

export async function saveRig() {
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

/* ---------- forward declarations for cross-module calls ---------- */
export let renderInspector = () => {};
export let wireRegions = () => {};
export let toast = (msg, err) => console.log("[toast]", msg, err);
export let call = async (cmd, args) => null;