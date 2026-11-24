/**
 * desktop-buddy — canvas rendering pipeline
 *
 * Depends on: state.js (state + activeState/hasVariant + gaze/speech) and
 * rig.js (geometry). Owns the 2D context and the DPR transform.
 */

import {
  avatar, RT, TAU, speech, gaze,
  clamp, num, activeState,
  hasVariant, viewport, updateViewport,
} from "./state.js";

import {
  computeWorld, sortedDrawables, partBox, partWorldMatrix, partSize,
} from "./rig.js";

import { entityDrawables } from "./entities.js";

let canvas = null;
let ctx = null;

/** Unit-ish gaze vector for this frame, recomputed in `render()`. */
let gazeDir = { x: 0, y: 0 };
/** Does the rig draw a dedicated pupil part? (-eye offset fallback if not) */
let rigHasPupils = false;

/** Longest a pupil may travel from its socket centre, in world px. */
const GAZE_RADIUS = 4;

/**
 * Is this the pupil sprite?
 *
 * The rig format has no `pupil` tag (pupils are authored as `eye` parts), so the
 * **id** is the signal — the shipped rig names them `pupil_l` / `pupil_r`. The
 * image path is deliberately *not* used: `parts/pupil.png` is also the hand
 * sprite on demo-blob, and matching it would make a hand drift with the gaze.
 */
function isPupilPart(part) {
  return part.tag === "pupil" || /pupil/i.test(part.id || "");
}

/**
 * Where the eyes are looking, as a clamped direction.
 *
 * `gaze.x/y` is already smoothed in screen space by `main.js`; this turns it
 * into `dir * strength` where strength saturates once the target is ~140 px
 * away, so a near pointer is a small glance and the far corner is a full look.
 */
function updateGazeDir() {
  let head = RT.boneList.find((bone) => bone.parts.some((part) => part.tag === "head"));
  if (!head) head = RT.boneList.find((bone) => bone.parts.some((part) => part.tag === "eye"));
  const world = head ? RT.world[head.id] : null;
  const hx = world ? world.x : avatar.x;
  const hy = world ? world.y : avatar.y - 60;
  const dx = gaze.x - hx;
  const dy = gaze.y - hy;
  const length = Math.hypot(dx, dy);
  if (!Number.isFinite(length) || length < 0.001) {
    gazeDir = { x: 0, y: 0 };
    return;
  }
  const strength = clamp(length / 140, 0, 1);
  // Vertical travel is subtler than horizontal in a real eye; 0.75 keeps it
  // from looking like the pupils are sliding out of the socket.
  gazeDir = { x: (dx / length) * strength, y: (dy / length) * strength * 0.75 };
}

function drawFace(w, h) {
  const mouthOpen = avatar.talk
    ? clamp(speech.mouthSmooth, 0, 1)
    : (avatar.blendshapes.mouth_open || 0);
  const eyeWide = avatar.blendshapes.eye_wide || 0;
  const eyeSquint = avatar.blendshapes.eye_squint || 0;
  const browUp = avatar.blendshapes.brow_up || 0;
  const browAngle = avatar.blendshapes.brow_angle || 0;
  const mouthCurve = avatar.blendshapes.mouth_curve || 0;
  const mouthWide = avatar.blendshapes.mouth_wide || 0;
  const scale = w / 116;
  // The procedural face has no pupil parts: the iris arcs move with the gaze.
  const gazeX = gazeDir.x * GAZE_RADIUS * 0.85 * scale;
  const gazeY = gazeDir.y * GAZE_RADIUS * 0.85 * scale;

  ctx.fillStyle = "rgba(255,255,255,0.10)";
  ctx.beginPath();
  ctx.ellipse(0, -12 * scale, w * 0.34, h * 0.28, 0, 0, TAU);
  ctx.fill();

  const eyeY = -12 * scale - browUp * 10 * scale;
  const eyeR = (11 + eyeWide * 7) * scale;
  const eyeH = eyeR * (0.72 - clamp(eyeSquint, 0, 1) * 0.34);
  ctx.fillStyle = "#ffffff";
  ctx.beginPath();
  ctx.ellipse(-22 * scale, eyeY, eyeR, eyeH, 0, 0, TAU);
  ctx.ellipse(22 * scale, eyeY, eyeR, eyeH, 0, 0, TAU);
  ctx.fill();

  ctx.fillStyle = "#101018";
  ctx.beginPath();
  ctx.arc(-22 * scale + gazeX, eyeY + scale + gazeY, eyeR * 0.42, 0, TAU);
  ctx.arc(22 * scale + gazeX, eyeY + scale + gazeY, eyeR * 0.42, 0, TAU);
  ctx.fill();

  ctx.strokeStyle = "#1e2233";
  ctx.lineWidth = 3 * scale;
  ctx.lineCap = "round";
  const brow = eyeY - 18 * scale - browUp * 8 * scale;
  // +brow_angle pulls the inner ends down (angry), -brow_angle lifts them (sad).
  const browTilt = clamp(browAngle, -1, 1) * 7 * scale;
  ctx.beginPath();
  ctx.moveTo(-38 * scale, brow + 4 * scale - browTilt);
  ctx.lineTo(-9 * scale, brow - 2 * scale + browTilt);
  ctx.moveTo(9 * scale, brow - 2 * scale + browTilt);
  ctx.lineTo(38 * scale, brow + 4 * scale - browTilt);
  ctx.stroke();

  const mouthY = 20 * scale - clamp(mouthCurve, -1, 1) * 3 * scale;
  const mouthW = (22 + mouthOpen * 30 + mouthWide * 10) * scale;
  const mouthH = (7 + mouthOpen * 26 + Math.abs(mouthCurve) * 3) * scale;
  ctx.fillStyle = "#210a12";
  ctx.beginPath();
  ctx.ellipse(0, mouthY, mouthW, mouthH, 0, 0, TAU);
  ctx.fill();
  if (mouthOpen > 0.25) {
    ctx.fillStyle = "#7d2036";
    ctx.beginPath();
    ctx.ellipse(0, mouthY + mouthH * 0.22, mouthW * 0.72, mouthH * 0.42, 0, 0, TAU);
    ctx.fill();
  }
}

function drawShape(part, w, h, px) {
  const color = part.color || "#9a9ab0";
  if (part.shape === "capsule") {
    const radius = h / 2;
    const pivotX = px * w;
    const left = pivotX - radius;
    const right = pivotX + w - radius;
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.moveTo(left, -radius);
    ctx.lineTo(right, -radius);
    ctx.arc(right, 0, radius, -Math.PI / 2, Math.PI / 2);
    ctx.lineTo(left, radius);
    ctx.arc(left, 0, radius, Math.PI / 2, -Math.PI / 2);
    ctx.closePath();
    ctx.fill();
    ctx.beginPath();
    ctx.arc(pivotX, 0, radius * 0.98, 0, TAU);
    ctx.fill();
    return;
  }
  if (part.shape === "ellipse" || part.shape === "circle") {
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.ellipse(0, 0, w / 2, h / 2, 0, 0, TAU);
    ctx.fill();
    return;
  }
  if (part.shape === "face") drawFace(w, h);
}

function drawPart(bone, part, preview = null) {
  if (part.visible === false) return;
  // ONE matrix for the renderer and the editor (selection box, pivot cross,
  // hit test) — see `rig.js::partWorldMatrix`. A prop entity passes its own
  // matrix (computed against its private world) and uses the same painter.
  const m = preview || partWorldMatrix(bone, part);
  if (!m) return;
  const size = partSize(part);
  const w = size[0];
  const h = size[1];
  const px = part.pivot[0];
  const py = part.pivot[1];
  ctx.save();
  ctx.translate(m.x, m.y);
  ctx.rotate(m.rot);
  const scale = m.scale;
  ctx.scale(scale, scale);

  const state = activeState(part);
  const blend = (key) => (Number.isFinite(avatar.blendshapes[key]) ? avatar.blendshapes[key] : 0);
  if (part.tag === "mouth") {
    // Sprite variants switch per character (state.js::mouthState); this squash
    // is the procedural fallback and follows the same text-driven openness.
    const open = avatar.talk
      ? clamp(speech.mouthSmooth, 0, 1)
      : clamp(blend("mouth_open"), 0, 1);
    const wide = clamp(blend("mouth_wide"), 0, 1);
    const curve = clamp(blend("mouth_curve"), -1, 1);
    ctx.scale(1 + open * 0.18 + wide * 0.3, 1 + open * 0.55 + Math.abs(curve) * 0.12);
    // A mouth authored as two halves (or with corner parts) tilts each side by
    // the curve, so +curve reads as a smile and -curve as a frown.
    const side = Math.sign(part.offset[0] || 0);
    if (side) {
      ctx.translate(0, -curve * h * 1.0);
      ctx.rotate(-side * curve * 0.6);
    } else {
      ctx.rotate(curve * 0.12);
    }
  } else if (part.tag === "eye") {
    const wide = clamp(blend("eye_wide"), 0, 1);
    const squint = clamp(blend("eye_squint"), 0, 1);
    ctx.scale(1 + wide * 0.12, 1 + wide * 0.18 - squint * 0.45);
    if (state === "__blink__") ctx.scale(1, 0.12);
  } else if (part.tag === "brow") {
    // +brow_up lifts the brow, +brow_angle pulls its inner end down (angry);
    // the sign of `offset.x` mirrors the tilt for the other side.
    const up = clamp(blend("brow_up"), -1, 1);
    const angle = clamp(blend("brow_angle"), -1, 1);
    const side = Math.sign(part.offset[0] || 0) || 1;
    ctx.translate(0, -up * h * 0.9);
    ctx.rotate(-side * angle * 0.55);
  }
  if (isPupilPart(part)) {
    const wide = clamp(blend("eye_wide"), 0, 1);
    ctx.scale(1 + wide * 0.25, 1 + wide * 0.25);
  }

  // Gaze: pupils drift inside the socket; a rig without a pupil part moves the
  // whole eye sprite a little instead. The radius is in world px (so it stays
  // natural across rig sizes) and is clamped to a fraction of the part.
  const pupil = isPupilPart(part);
  if (pupil || (part.tag === "eye" && !rigHasPupils)) {
    const worldRadius = Math.min(GAZE_RADIUS, Math.min(w, h) * scale * 0.18);
    const localRadius = worldRadius / Math.max(0.0001, scale);
    const weight = pupil ? 1 : 0.55;
    ctx.translate(gazeDir.x * localRadius * weight, gazeDir.y * localRadius * weight);
  }

  const imagePath = state && hasVariant(part, state) ? part.variants[state] : part.image;
  if (imagePath) {
    const img = RT.images[imagePath];
    if (img && img.complete && img.naturalWidth) {
      ctx.drawImage(img, -w * px, -h * py, w, h);
    }
  } else if (part.shape) {
    ctx.translate(-w * px, -h * py);
    if (part.shape === "face") {
      ctx.translate(w / 2, h / 2);
      drawFace(w, h);
    } else {
      drawShape(part, w, h, px);
    }
  }
  ctx.restore();
}

/* ---------- editor overlay ---------- */

const SELECT_COLOR = "#ffb800";
const BONE_COLOR = "rgba(120, 200, 255, 0.85)";

function drawEditorOverlay() {
  const sel = RT.selected;
  for (const id of RT.order) {
    const bone = RT.bones[id];
    const w = RT.world[id];
    if (!w) continue;
    const isSelected = !!(sel && sel.bone === id);

    for (const childId of bone.children || []) {
      const cw = RT.world[childId];
      if (!cw) continue;
      ctx.strokeStyle = isSelected ? SELECT_COLOR : BONE_COLOR;
      ctx.lineWidth = isSelected ? 2 : 1.25;
      ctx.setLineDash(isSelected ? [] : [4, 4]);
      ctx.beginPath();
      ctx.moveTo(w.x, w.y);
      ctx.lineTo(cw.x, cw.y);
      ctx.stroke();
    }
    ctx.setLineDash([]);

    if (bone.length > 0) {
      const hx = w.x + Math.cos(w.rot) * bone.length * w.scale;
      const hy = w.y + Math.sin(w.rot) * bone.length * w.scale;
      ctx.strokeStyle = isSelected ? SELECT_COLOR : "rgba(120,200,255,0.35)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(w.x, w.y);
      ctx.lineTo(hx, hy);
      ctx.stroke();
      ctx.beginPath();
      ctx.arc(hx, hy, isSelected ? 7 : 4.5, 0, TAU);
      ctx.stroke();
    }

    ctx.fillStyle = isSelected ? SELECT_COLOR : "rgba(16,20,30,0.9)";
    ctx.strokeStyle = isSelected ? "#fff3c4" : BONE_COLOR;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(w.x, w.y, isSelected ? 8 : 6, 0, TAU);
    ctx.fill();
    ctx.stroke();

    if (isSelected) {
      ctx.fillStyle = "#ffd166";
      ctx.font = "600 11px 'Segoe UI', sans-serif";
      ctx.fillText(id, w.x + 12, w.y - 10);
    }
  }

  for (const bone of RT.boneList) {
    if (!RT.world[bone.id]) continue;
    for (const part of bone.parts) {
      const selectedPart = !!(sel && sel.bone === bone.id && sel.part === part.id);
      const box = partBox(bone, part);
      ctx.strokeStyle = selectedPart ? SELECT_COLOR : "rgba(255,255,255,0.22)";
      ctx.setLineDash(selectedPart ? [] : [3, 4]);
      ctx.lineWidth = selectedPart ? 1.6 : 1;
      ctx.strokeRect(box.x, box.y, box.w, box.h);
      ctx.setLineDash([]);

      const m = partWorldMatrix(bone, part);
      if (m) {
        ctx.strokeStyle = selectedPart ? SELECT_COLOR : "rgba(255,255,255,0.35)";
        ctx.beginPath();
        ctx.moveTo(m.x - 5, m.y);
        ctx.lineTo(m.x + 5, m.y);
        ctx.moveTo(m.x, m.y - 5);
        ctx.lineTo(m.x, m.y + 5);
        ctx.stroke();
      }
    }
  }
}

/* ---------- main render pipeline ---------- */

export function render() {
  if (!ctx) return;
  ctx.clearRect(0, 0, viewport.w, viewport.h);
  if (!RT.model) return;
  computeWorld();
  // Both depend on the world transforms, so they run after computeWorld().
  rigHasPupils = RT.boneList.some((bone) => bone.parts.some(isPupilPart));
  updateGazeDir();

  // One z-sorted pass. A prop's `z` is `socket.z + bone.z + part.z` — the same
  // units as the avatar's — so a held rod draws in front of the torso and a pack
  // on the back draws behind it, with no special cases in the painter.
  const drawables = [];
  for (const entry of sortedDrawables()) {
    drawables.push({
      z: num(entry.bone.z, 0) + num(entry.part.z, 0),
      bone: entry.bone,
      part: entry.part,
      matrix: partWorldMatrix(entry.bone, entry.part),
    });
  }
  for (const item of entityDrawables()) drawables.push(item);
  drawables.sort((a, b) => a.z - b.z);
  for (const item of drawables) {
    if (!item.matrix) continue;
    drawPart(item.bone, item.part, item.matrix);
  }

  if (RT.rigMode) drawEditorOverlay();
}

/* ---------- init ---------- */

export function initRenderer(canvasEl) {
  canvas = canvasEl;
  ctx = canvas.getContext("2d");
  resizeCanvas();
}

/** Resize the backing store for the current DPR. Called on init and resize. */
export function resizeCanvas() {
  if (!canvas || !ctx) return;
  updateViewport();
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.max(1, Math.floor(viewport.w * dpr));
  canvas.height = Math.max(1, Math.floor(viewport.h * dpr));
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  if (avatar.x === 0 && avatar.y === 0) {
    avatar.x = viewport.w / 2;
    avatar.y = viewport.h / 2;
    avatar.targetX = avatar.x;
    avatar.targetY = avatar.y;
  }
}
