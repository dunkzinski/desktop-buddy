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
  speech.thinking = false;
  speech.full = body;
  speech.shown = -1;
  speech.elapsed = 0;
  speech.doneFor = 0;
  speech.charMs = clamp(Number(options.charMs) || 26, 6, 200);
  speech.holdMs = Number.isFinite(options.durationMs) ? Math.max(500, options.durationMs) : 6000;
  speech.visible = true;
  avatar.talk = true;

  const el = document.getElementById("speech");
  el.classList.remove("hidden", "thinking");
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
    // An open reply box, a hovered bubble or a focused input outranks the hold
    // timer: the bubble must not vanish while the user is reading or typing.
    if (speech.doneFor * 1000 >= speech.holdMs && !bubbleHeld()) hideSpeechNow();
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
  // Our own reply submit dismisses the old prompt from Rust (`speech-clear`).
  // That is not a dismissal the user should see: swap in the "Thinking…" card
  // instead, so the overlay reacts the instant the task is sent.
  if (expectingThinking) {
    expectingThinking = false;
    showThinking(thinkingLabel);
    return;
  }
  // A waiting card is only dismissed by an answer (`speak`), the hold timer or
  // `endThinking` — a stray/duplicate clear must not blank the "waiting" state.
  if (speech.thinking) return;
  hideSpeechNow();
}

/** Dismiss the bubble unconditionally (timer, reset, explicit end). */
function hideSpeechNow() {
  const wasVisible = speech.visible;
  speech.visible = false;
  speech.thinking = false;
  speech.full = "";
  speech.shown = -1;
  speech.elapsed = 0;
  speech.doneFor = 0;
  avatar.talk = false;
  closeReply();

  const el = document.getElementById("speech");
  el.classList.remove("show", "thinking");
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
  if (!el || !speech.visible || speech.thinking) return false;
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

/* ---------- "Thinking…" feedback ---------- */

/** Set just before submitting; the next speech-clear becomes the thinking card. */
let expectingThinking = false;
let thinkingLabel = "Thinking…";

/**
 * Mark the next `speech-clear` as our own submit echo.
 *
 * Rust dismisses the old prompt when it accepts a reply, and the page hears that
 * as `speech-clear`. Without this flag the bubble would blink out and the user
 * would be left with nothing until the model answered.
 */
export function beginThinking(label) {
  expectingThinking = true;
  thinkingLabel = String(label || "Thinking…");
}

/** Show the transient "Thinking…" card (safe to call twice). */
export function showThinking(label) {
  const text = String(label || thinkingLabel || "Thinking…");
  speak(text, { durationMs: 120000, charMs: 8 });
  speech.thinking = true;
  const el = document.getElementById("speech");
  if (el) el.classList.add("thinking");
}

/** Cancel a pending or visible thinking card (e.g. the submit failed). */
export function endThinking() {
  expectingThinking = false;
  if (speech.thinking) {
    speech.thinking = false;
    hideSpeech();
  }
}

/* ---------- hover hold ---------- */

/** Pause the bubble's hold timer while the pointer rests on it. */
export function setBubbleHover(hovered) {
  speech.hovered = !!hovered;
}

/**
 * Should the bubble stay up even though its hold expired?
 *
 * True while the quick-reply box is pinned, the pointer is over the bubble, or
 * either text input (bubble reply / command bar) has focus — the three ways a
 * user can be mid-sentence when the timer would otherwise fire.
 */
function bubbleHeld() {
  if (isReplyOpen() || speech.hovered) return true;
  const active = document.activeElement;
  return !!active && (active.id === "speech-reply-input" || active.id === "command-input");
}

/* ---------- command bar geometry ---------- */

/** Is the floating ask/task bar on screen? */
export function commandBarVisible() {
  const el = document.getElementById("command-bar");
  return !!el && !el.classList.contains("hidden");
}

/**
 * Anchor the command bar just below the avatar's head, clamped to the viewport
 * (it flips above the avatar when there is no room underneath).
 *
 * Same anchor as the bubble, so the bar appears where the user is already
 * looking and follows the avatar while it wanders.
 */
export function positionCommandBar() {
  const el = document.getElementById("command-bar");
  if (!el || el.classList.contains("hidden")) return;
  const rect = el.getBoundingClientRect();
  const width = Math.max(220, rect.width || 360);
  const height = Math.max(40, rect.height || 60);
  const anchor = bubbleAnchor();
  const left = clamp(anchor.x - width / 2, 8, Math.max(8, viewport.w - width - 8));
  let top = anchor.y + 52;
  if (top + height > viewport.h - 8) top = Math.max(8, anchor.y - height - 52);
  el.style.left = Math.round(left) + "px";
  el.style.top = Math.round(top) + "px";
}
