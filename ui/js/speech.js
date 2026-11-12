/**
 * char-rig — speech bubble lifecycle
 *
 * Depends on: state.js only (speech state, avatar, RT.world, viewport).
 * No rig.js import: the bubble reads RT.world, which the render loop fills.
 */

import {
  avatar, RT, speech,
  clamp, viewport, updateStatusUI,
} from "./state.js";

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
  if (!el || !tail) return;
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
  const left = clamp(anchor.x - width / 2, 8, Math.max(8, viewport.w - width - 8));
  el.style.left = Math.round(left) + "px";
  el.style.top = Math.round(top) + "px";
  el.style.maxWidth = Math.min(330, Math.max(120, viewport.w * 0.5)) + "px";

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
    // An open reply box outranks the hold timer: the bubble must not vanish
    // while the user is typing into it.
    if (speech.doneFor * 1000 >= speech.holdMs && !isReplyOpen()) hideSpeech();
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
  closeReply();

  const el = document.getElementById("speech");
  el.classList.remove("show");
  el.classList.add("hidden");
  if (wasVisible) updateStatusUI();
}

/* ---------- quick reply ---------- */

/**
 * Is the reply box pinned open? (Hover alone shows it without this class.)
 *
 * The box lives inside the bubble so it travels with the avatar; the bubble's
 * bounding box is published as a hit region while visible (`wireRegions`), which
 * is what lets the user click into it while the rest of the desktop stays
 * click-through.
 */
export function isReplyOpen() {
  const el = document.getElementById("speech");
  return !!el && el.classList.contains("reply-open");
}

/** Pin the box open and focus the input. Returns false when there is no bubble. */
export function openReply() {
  const el = document.getElementById("speech");
  if (!el || !speech.visible) return false;
  el.classList.add("reply-open");
  // The box makes the card taller: re-anchor it above the head.
  positionBubble();
  const input = document.getElementById("speech-reply-input");
  if (input) input.focus();
  return true;
}

/** Un-pin the box, drop anything typed and stop capturing keys. */
export function closeReply() {
  const el = document.getElementById("speech");
  if (el) el.classList.remove("reply-open");
  const input = document.getElementById("speech-reply-input");
  if (input) {
    input.value = "";
    if (document.activeElement === input) input.blur();
  }
  if (speech.visible) positionBubble();
}

/** Click on the bubble: open the box, or close it if it was already pinned. */
export function toggleReply() {
  return isReplyOpen() ? (closeReply(), false) : openReply();
}
