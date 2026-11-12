/**
 * char-rig — canvas rendering pipeline
 *
 * Depends on: state.js (state + activeState/hasVariant) and rig.js (geometry).
 * Owns the 2D context and the DPR transform.
 */

import {
  avatar, RT, TAU,
  clamp, num, activeState,
  hasVariant, viewport, updateViewport,
} from "./state.js";

import {
  computeWorld, sortedDrawables, partBox, partWorldMatrix, partSize,
} from "./rig.js";

let canvas = null;
let ctx = null;

/* ---------- drawing ---------- */

function drawFace(w, h) {
  const mouthOpen = avatar.blendshapes.mouth_open || 0;
  const eyeWide = avatar.blendshapes.eye_wide || 0;
  const browUp = avatar.blendshapes.brow_up || 0;
  const scale = w / 116;

  ctx.fillStyle = "rgba(255,255,255,0.10)";
  ctx.beginPath();
  ctx.ellipse(0, -12 * scale, w * 0.34, h * 0.28, 0, 0, TAU);
  ctx.fill();

  const eyeY = -12 * scale - browUp * 10 * scale;
  const eyeR = (11 + eyeWide * 7) * scale;
  ctx.fillStyle = "#ffffff";
  ctx.beginPath();
  ctx.ellipse(-22 * scale, eyeY, eyeR, eyeR * 0.72, 0, 0, TAU);
  ctx.ellipse(22 * scale, eyeY, eyeR, eyeR * 0.72, 0, 0, TAU);
  ctx.fill();

  ctx.fillStyle = "#101018";
  ctx.beginPath();
  ctx.arc(-22 * scale, eyeY + scale, eyeR * 0.42, 0, TAU);
  ctx.arc(22 * scale, eyeY + scale, eyeR * 0.42, 0, TAU);
  ctx.fill();

  ctx.strokeStyle = "#1e2233";
  ctx.lineWidth = 3 * scale;
  ctx.lineCap = "round";
  ctx.beginPath();
  const brow = eyeY - 18 * scale - browUp * 8 * scale;
  ctx.moveTo(-38 * scale, brow + 4 * scale);
  ctx.lineTo(-9 * scale, brow - 2 * scale);
  ctx.moveTo(9 * scale, brow - 2 * scale);
  ctx.lineTo(38 * scale, brow + 4 * scale);
  ctx.stroke();

  const mouthY = 20 * scale;
  const mouthW = (22 + mouthOpen * 30) * scale;
  const mouthH = (7 + mouthOpen * 26) * scale;
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

function drawPart(boneWorld, part) {
  if (part.visible === false) return;
  const size = partSize(part);
  const w = size[0];
  const h = size[1];
  const px = part.pivot[0];
  const py = part.pivot[1];
  ctx.save();
  ctx.translate(boneWorld.x, boneWorld.y);
  ctx.rotate(boneWorld.rot);
  const scale = boneWorld.scale * num(part.scale, 1);
  ctx.scale(scale, scale);
  ctx.translate(part.offset[0], part.offset[1]);
  if (part.rotation) ctx.rotate(part.rotation);

  const state = activeState(part);
  if (part.tag === "mouth") {
    // sprite variant switching is preferred; squash/stretch is the fallback
    const open = avatar.talk ? 0.5 + 0.5 * Math.sin(avatar.time * 26)
      : clamp(avatar.blendshapes.mouth_open || 0, 0, 1);
    ctx.scale(1 + open * 0.18, 1 + open * 0.55);
  } else if (part.tag === "eye" && state === "__blink__") {
    ctx.scale(1, 0.12);
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
  for (const entry of sortedDrawables()) {
    const world = RT.world[entry.bone.id];
    if (!world) continue;
    drawPart(world, entry.part);
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
