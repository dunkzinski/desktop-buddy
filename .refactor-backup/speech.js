/**
 * char-rig — speech bubble lifecycle
 * Depends on: state.js
 */

import {
  avatar, RT, TAU, speech,
  clamp, round,
} from "./state.js";

import { computeWorld } from "./rig.js";

/* ---------- speech bubble ---------- */

function bubbleAnchor() {
  let bone = RT.boneList.find((candidate) => candidate.parts.some((part) => part.tag === "head"));
  if (!bone) bone = RT.boneList.find((candidate) => /head/i.test(candidate.id));
  if (!bone) bone = RT.boneList.find((candidate) => candidate.parts.some((part) => part.tag === "mouth"));
  const world = bone ? RT.world[bone.id] : null;
  return world ? { x: world.x, y: world.y } : { x: avatar.x, y: avatar.y - 70 };
}

export function positionBubble() {
  if (!speech.visible) return;
  const el = document.getElementById("speech");
  const tail = document.getElementById("speech-tail");
  const rect = el.getBoundingClientRect();
  const width = Math.max(80, rect.width);
  const height = Math.max(30, rect.height);
  const anchor = bubbleAnchor();

  let top = anchor.y - height - 30;
  let flipped = false;
  if (top < 8) {
    top = anchor.y + 46;
    flipped = true;
  }
  const left = clamp(anchor.x - width / 2, 8, Math.max(8, vw - width - 8));
  el.style.left = Math.round(left) + "px";
  el.style.top = Math.round(top) + "px";
  el.style.maxWidth = Math.min(330, Math.max(120, vw * 0.5)) + "px";

  tail.style.left = Math.round(clamp(anchor.x - left, 16, width - 16)) + "px";
  tail.style.marginLeft = "-8px";
  tail.className = flipped ? "above" : "below";
  speech.flipped = flipped;
}

/**
 * Show a speech bubble. `opts.durationMs` is how long the finished text stays
 * on screen; the mouth animates while the bubble is visible.
 */
export function speak(text, opts) {
  const options = opts || {};
  const body = String(text == null ? "" : text).slice(0, 2000);
  if (!body.trim()) {
    hideSpeech();
    return;
  }
  speech.full = body;
  speech.shown = -1;
  speech.elapsed = 0;
  speech.doneFor = 0;
  speech.charMs = clamp(Number(options.charMs) || 26, 6, 200);
  speech.holdMs = Number.isFinite(options.durationMs) ? Math.max(500, options.durationMs) : 6000;
  speech.visible = true;
  avatar.talk = true;

  const el = document.getElementById("speech");
  el.classList.remove("hidden");
  el.classList.add("show");
  document.getElementById("speech-text").textContent = "";
  positionBubble();
  updateSpeech(0);
}

export function updateSpeech(dt) {
  if (!speech.visible) return;
  speech.elapsed += dt;
  const chars = Math.min(speech.full.length, Math.floor((speech.elapsed * 1000) / speech.charMs));
  const textEl = document.getElementById("speech-text");

  if (chars !== speech.shown) {
    speech.shown = chars;
    textEl.textContent = speech.full.slice(0, chars);
    if (chars < speech.full.length) {
      const caret = document.createElement("span");
      caret.className = "caret";
      textEl.appendChild(caret);
    }
    positionBubble();
  }

  if (chars >= speech.full.length) {
    speech.doneFor += dt;
    if (speech.doneFor * 1000 >= speech.holdMs) hideSpeech();
    return;
  }

  // keep the bubble glued to the head while the avatar walks around
  speech.lastLayout += dt;
  if (speech.lastLayout > 0.1) {
    speech.lastLayout = 0;
    positionBubble();
  }
}

export function hideSpeech() {
  const wasVisible = speech.visible;
  speech.visible = false;
  speech.full = "";
  speech.shown = -1;
  speech.elapsed = 0;
  speech.doneFor = 0;
  avatar.talk = false;

  const el = document.getElementById("speech");
  el.classList.remove("show");
  el.classList.add("hidden");
  if (wasVisible) updateStatusUI();
}

/* ---------- forward declarations for cross-module calls ---------- */
export let vw = 0, vh = 0;
export let updateStatusUI = () => {};