/**
 * char-rig — Tauri bridge: IPC commands, event listeners, hit regions, state sync
 * Depends on: state.js, rig.js, editor.js, speech.js
 */

import {
  avatar, RT, TAU,
  clamp, round, num,
  toast, setDirty,
} from "./state.js";

import { call as rigCall, applyModel, rebuildOrder, wireRegions as rigWireRegions, refreshModelList, ensureImage, attachStagedPart, bindExpressionSprite } from "./rig.js";
import { setRigMode, select, addChildBone, deleteBone, addPartFromControls, addLimbFromControls, duplicatePart, deletePart, saveRig, resetRig } from "./editor.js";
import { speak, hideSpeech, updateSpeech, positionBubble } from "./speech.js";

/* ---------- Tauri IPC ---------- */

const TAURI = window.__TAURI__;
const isTauri = typeof TAURI !== "undefined" && TAURI !== null;
const invoke = isTauri && TAURI.core && TAURI.core.invoke ? TAURI.core.invoke : null;
const listen = isTauri && TAURI.event && TAURI.event.listen ? TAURI.event.listen : null;

/**
 * Call a Tauri command with error handling and toast notification.
 */
export async function call(cmd, args) {
  if (!invoke) return null;
  try {
    return await invoke(cmd, args || {});
  } catch (err) {
    console.error("[avatar] command failed:", cmd, err);
    toast(String(err), true);
    return null;
  }
}

/* ---------- hit regions / click-through ---------- */

function avatarBounds() {
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

function partBox(bone, part) {
  const m = partWorldMatrix(bone, part);
  const size = partSize(part);
  const w = size[0];
  const h = size[1];
  const px = part.pivot[0];
  const py = part.pivot[1];
  if (!m) return { x: 0, y: 0, w: 0, h: 0 };
  const cos = Math.cos(m.rot);
  const sin = Math.sin(m.rot);
  const corners = [
    [-w * px, -h * py],
    [w * (1 - px), -h * py],
    [w * (1 - px), h * (1 - py)],
    [-w * px, h * (1 - py)],
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

function partWorldMatrix(bone, part) {
  const w = RT.world[bone.id];
  if (!w) return null;
  const cos = Math.cos(w.rot);
  const sin = Math.sin(w.rot);
  const ox = part.offset[0];
  const oy = part.offset[1];
  return {
    x: w.x + (ox * cos - oy * sin) * w.scale,
    y: w.y + (ox * sin + oy * cos) * w.scale,
    rot: w.rot + num(part.rotation, 0),
    scale: w.scale * num(part.scale, 1),
  };
}

function partSize(part) {
  if (Array.isArray(part.size)) return part.size;
  const img = part.image ? RT.images[part.image] : null;
  if (img && img.naturalWidth) return [img.naturalWidth, img.naturalHeight];
  return [64, 64];
}

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
    regions.push({ x: 0, y: 0, w: vw, h: vh });
    push(document.getElementById("inspector"));
  } else {
    regions.push(avatarBounds());
  }
  if (!document.getElementById("import-chooser").classList.contains("hidden")) {
    push(document.getElementById("import-chooser"));
  }

  call("update_hit_regions", { regions: regions });

  if (!avatar.clickThroughArmed && !avatar.clickThroughUserSet && regions.length > 0) {
    avatar.clickThroughArmed = true;
    avatar.clickThrough = true;
    document.getElementById("btn-clickthrough").classList.add("pinned");
    call("set_click_through", { enabled: true });
  }
}

export async function setClickThrough(enabled) {
  avatar.clickThrough = enabled;
  avatar.clickThroughUserSet = true;
  document.getElementById("btn-clickthrough").classList.toggle("pinned", enabled);
  await call("set_click_through", { enabled: enabled });
  toast(enabled ? "Click-through on empty space: ON" : "Click-through: OFF (whole screen interactive)");
  wireRegions();
}

/* ---------- state sync ---------- */

function jointAngles() {
  const out = {};
  for (const id of RT.order) {
    const bone = RT.bones[id];
    if (bone) out[id] = round(localRotation(bone), 4);
  }
  return out;
}

function localRotation(bone) {
  const override = RT.pose[bone.id];
  const base = num(bone.rotation, 0) + animOffset(bone).rot;
  const value = override && Number.isFinite(override.rotation) ? override.rotation : base;
  return clampRotation(bone, value);
}

function animOffset(bone) {
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

function clampRotation(bone, value) {
  const limits = bone && bone.limits ? bone.limits : {};
  const min = Number.isFinite(limits.min) ? limits.min : -Infinity;
  const max = Number.isFinite(limits.max) ? limits.max : Infinity;
  return Math.min(max, Math.max(min, value));
}

let lastReport = 0;
export function reportState(now) {
  if (!invoke) return;
  if (now - lastReport < 120) return;
  lastReport = now;
  call("report_state", {
    position: [avatar.x, avatar.y],
    target_position: [avatar.targetX, avatar.targetY],
    state: avatar.mode,
    expression: avatar.expression,
    joints: jointAngles(),
    talking: avatar.talk,
    speech: speech.visible,
  });
}

export function applyRemoteState(remote) {
  if (!remote) return;
  const rev = Number(remote.revision || 0);
  if (!Number.isFinite(rev) || rev <= avatar.revision) return;
  avatar.revision = rev;
  markManual(15);

  if (Array.isArray(remote.target_position)) {
    const tx = Number(remote.target_position[0]);
    const ty = Number(remote.target_position[1]);
    if (Number.isFinite(tx) && Number.isFinite(ty) &&
        (Math.abs(tx - avatar.targetX) > 0.5 || Math.abs(ty - avatar.targetY) > 0.5)) {
      avatar.startX = avatar.x;
      avatar.startY = avatar.y;
      avatar.startTime = avatar.time;
      avatar.targetX = tx;
      avatar.targetY = ty;
      avatar.walkPhase = 0;
    }
  }
  if (remote.state && remote.state !== avatar.mode) {
    avatar.mode = remote.state;
    if (avatar.mode === "waving") avatar.wavePhase = 0;
    if (avatar.mode === "idle") avatar.idleTimer = 0;
  }
  if (remote.expression && remote.expression !== avatar.expression) setExpression(remote.expression);
  updateStatusUI();
}

export function applyPartPatch(target, patch) {
  if (!target || !patch) return;
  let touched = 0;
  for (const bone of RT.boneList) {
    for (const part of bone.parts) {
      const byTag = target.tag && part.tag === target.tag;
      const byId = target.part && part.id === target.part;
      if (!byTag && !byId) continue;
      if ("talk" in patch) part.talk = !!patch.talk;
      if ("state" in patch) {
        part.state = patch.state;
        applyPartVariant(part);
      }
      for (const key of ["offset", "rotation", "scale", "z", "pivot", "visible", "color"]) {
        if (key in patch) part[key] = patch[key];
      }
      if ("image" in patch && RT.images[patch.image]) part.image = patch.image;
      touched += 1;
    }
  }
  if (touched) {
    renderInspector();
    console.log("[avatar] patched", touched, "part(s)", JSON.stringify(target));
  } else {
    console.warn("[avatar] part patch matched nothing:", JSON.stringify(target));
  }
}

/* ---------- UI state ---------- */

function markManual(seconds) {
  avatar.manualUntil = avatar.time + (seconds || 12);
}

export function updateStatusUI() {
  const stateEl = document.getElementById("status-state");
  const exprEl = document.getElementById("status-expression");
  if (stateEl) stateEl.textContent = avatar.mode.charAt(0).toUpperCase() + avatar.mode.slice(1);
  if (exprEl) exprEl.textContent = avatar.expression.charAt(0).toUpperCase() + avatar.expression.slice(1);
  const dot = document.getElementById("status-dot");
  if (dot) dot.classList.toggle("busy", avatar.mode !== "idle");
  document.querySelectorAll("[data-expression]").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.expression === avatar.expression);
  });
  const posEl = document.getElementById("position-display");
  if (posEl) {
    posEl.textContent = "pos (" + Math.round(avatar.x) + ", " + Math.round(avatar.y) + ") -> (" +
      Math.round(avatar.targetX) + ", " + Math.round(avatar.targetY) + ")";
  }
}

function toggleCollapsed(collapsed) {
  avatar.collapsed = collapsed;
  document.getElementById("control-panel").classList.toggle("collapsed", collapsed);
  document.getElementById("panel-toggle").classList.toggle("visible", collapsed);
  wireRegions();
}

/* ---------- event listeners (Tauri events) ---------- */

export async function setupEventListeners() {
  if (!isTauri || !listen) return;

  await listen("model-changed", (event) => applyModel(event.payload));
  await listen("rig-pose", (event) => { RT.pose = event.payload || {}; });
  await listen("part-patch", (event) => {
    const payload = event.payload || {};
    applyPartPatch(payload.target, payload.patch);
  });
  await listen("rig-saved", () => refreshModelList(RT.model ? RT.model.name : null));
  await listen("avatar-state-changed", (event) => applyRemoteState(event.payload));
  await listen("api-status", (event) => {
    document.getElementById("api-url").textContent = event.payload || "offline";
  });
  await listen("emote", (event) => {
    const payload = event.payload || {};
    const holdMs = Number(payload.duration_ms) || 0;
    markManual(Math.max(8, holdMs / 1000 + 2));
    setExpression(payload.expression || "neutral", {
      durationMs: payload.duration_ms,
      revert: payload.revert || "neutral",
    });
  });
  await listen("speak", (event) => {
    const payload = event.payload || {};
    markManual(Math.max(10, (Number(payload.duration_ms) || 0) / 1000 + 3));
    if (payload.expression) setExpression(payload.expression);
    speak(payload.text, {
      durationMs: payload.duration_ms,
      charMs: payload.char_ms,
    });
  });
  await listen("speech-clear", () => hideSpeech());
}

/* ---------- import flow ---------- */

let pendingImportPath = null;

export function showChooser(path) {
  pendingImportPath = path;
  document.getElementById("chooser-file").textContent = path.split(/[\\/]/).pop();
  document.getElementById("import-chooser").classList.remove("hidden");
  wireRegions();
}

export function hideChooser() {
  pendingImportPath = null;
  document.getElementById("import-chooser").classList.add("hidden");
  wireRegions();
}

export async function importFromDialog() {
  const picked = await call("pick_import_file");
  if (!picked) { toast("Import cancelled"); return; }
  if (picked.kind === "rig") {
    const outcome = await call("import_asset", { path: picked.path, mode: "model" });
    if (outcome && outcome.name) {
      await refreshModelList(outcome.name);
      toast("Imported model " + outcome.name);
    }
    return;
  }
  showChooser(picked.path);
}

export async function importImageAs(mode) {
  const path = pendingImportPath;
  hideChooser();
  if (!path) return;
  const outcome = await call("import_asset", { path: path, mode: mode });
  if (!outcome) return;
  if (mode === "model" && outcome.name) {
    await refreshModelList(outcome.name);
    toast("Created model " + outcome.name);
    return;
  }
  if (mode === "part" && outcome.staged) attachStagedPart(outcome.staged);
}

/* ---------- forward declarations for cross-module calls ---------- */
export let vw = 0, vh = 0;
export let canvas = null;
export let renderInspector = () => {};
export let updateInspectorValues = () => {};
export let setExpression = (name, opts) => {};
export let rebuildAddImageOptions = () => {};
export let sortedDrawables = () => [];
export let computeWorld = () => {};