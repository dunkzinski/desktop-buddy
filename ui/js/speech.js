/**
 * char-rig — speech bubble lifecycle
 *
 * Depends on: state.js (speech state, avatar, RT.world, viewport), ipc.js (the
 * clipboard command) and rig.js (`wireRegions`, because an expanded details
 * drawer changes the bubble's hit region immediately instead of waiting for the
 * next 200 ms publish). rig.js imports neither speech.js nor ipc.js back, so the
 * module DAG stays acyclic.
 */

import {
  avatar, RT, speech,
  clamp, viewport, updateStatusUI,
} from "./state.js";

import { call } from "./ipc.js";
import { wireRegions } from "./rig.js";

/** Longest `details` block the page will render (mirrors lib.rs). */
const MAX_DETAILS_CHARS = 8000;

/* ---------- text-driven lip sync ---------- */

/**
 * Mouth openness per character, driving both the sprite variant and the
 * procedural squash. Vowels open wide, soft consonants sit mid-open, and spaces
 * / punctuation close the mouth — so the avatar visibly pauses between words
 * instead of flapping at a fixed ~3 Hz.
 */
const VOWELS = "aeiouyàáâäãåèéêëìíîïòóôöõùúûü";
const WIDE = ".!?…";            // sentence/clause ends: a beat with the mouth shut
const CLOSED = " \n\t,;:-–—\"'()[]{}…";
const MID_CONSONANTS = "mbpwfvhlcnrszxjqgdtk";

/**
 * Lip-sync pose for one revealed character.
 *
 * `open` is the 0..1 openness the renderer squashes the mouth by; `variant` is
 * the sprite name `state.js::activeState` prefers (falling back to open/closed
 * when the rig lacks `flat`).
 */
export function mouthPoseFor(char) {
  const ch = String(char || "").toLowerCase();
  if (!ch) return { open: 0, variant: "closed" };
  if (CLOSED.includes(ch) || WIDE.includes(ch)) return { open: 0, variant: "closed" };
  if (VOWELS.includes(ch)) return { open: 0.95, variant: "open" };
  if (MID_CONSONANTS.includes(ch)) return { open: 0.45, variant: "flat" };
  // Digits, other letters and emoji: a neutral half-open beat.
  return { open: 0.55, variant: "flat" };
}

/** Keep `speech.mouthOpen/mouthVariant` in step with the typewriter. */
function setMouthFrom(chars) {
  const char = chars > 0 ? speech.full[chars - 1] : "";
  speech.char = char;
  const pose = mouthPoseFor(char);
  speech.mouthOpen = pose.open;
  speech.mouthVariant = pose.variant;
}

/**
 * Ease the drawn openness toward the character's target. Fast enough to keep
 * up with 26 ms/char typing, slow enough that vowels read as a shape rather
 * than a flicker.
 */
function stepMouth(dt) {
  const k = Math.min(1, dt * 22);
  speech.mouthSmooth += (speech.mouthOpen - speech.mouthSmooth) * k;
  if (speech.mouthSmooth < 0.01) speech.mouthSmooth = 0;
}

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
 * on screen; `opts.details` is the optional long half (code, diff, log) that
 * hides behind a **Details ▾** toggle. The mouth animates while the bubble is
 * visible.
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
  speech.details = String(options.details == null ? "" : options.details).slice(0, MAX_DETAILS_CHARS);
  speech.detailsOpen = false;
  speech.visible = true;
  avatar.talk = true;

  const el = document.getElementById("speech");
  el.classList.remove("hidden", "thinking", "details-open");
  el.classList.add("show");
  document.getElementById("speech-text").textContent = "";
  renderDetails();
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
    // The mouth follows the letter that just appeared, not the clock.
    setMouthFrom(chars);
    // `positionBubble` runs every frame from the main loop (after `step()`
    // and `render()`), so the card can never lag the head.
  }

  if (chars >= speech.full.length) {
    speech.doneFor += dt;
    // Finished talking: shut the mouth, so the hold is a closed-mouth beat.
    if (speech.mouthOpen !== 0) {
      speech.mouthOpen = 0;
      speech.mouthVariant = "closed";
      speech.char = "";
    }
    // An open reply box, a hovered bubble or a focused input outranks the hold
    // timer: the bubble must not vanish while the user is reading or typing.
    if (speech.doneFor * 1000 >= speech.holdMs && !bubbleHeld()) hideSpeechNow();
  }

  stepMouth(dt);
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
  speech.details = "";
  speech.detailsOpen = false;
  speech.char = "";
  speech.mouthOpen = 0;
  speech.mouthSmooth = 0;
  speech.mouthVariant = "closed";
  avatar.talk = false;
  closeReply();
  renderDetails();

  const el = document.getElementById("speech");
  el.classList.remove("show", "thinking", "details-open");
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

/* ---------- details drawer + copy ---------- */

/** The long half of the current bubble (empty when there is none). */
export function detailsText() {
  return speech.details || "";
}

/**
 * Paint the Details/Copy row and the drawer from `speech.details`.
 *
 * The row only exists when the bubble actually carries details, so a plain
 * one-line bubble looks exactly as it did before.
 */
function renderDetails() {
  const has = !!detailsText();
  const actions = document.getElementById("speech-actions");
  const toggle = document.getElementById("speech-details-toggle");
  const copy = document.getElementById("speech-copy");
  const pre = document.getElementById("speech-details");

  if (actions) actions.classList.toggle("hidden", !has);
  if (copy) copy.classList.toggle("hidden", !has);
  if (toggle) {
    toggle.classList.toggle("hidden", !has);
    toggle.textContent = speech.detailsOpen ? "Details ▴" : "Details ▾";
    toggle.title = speech.detailsOpen ? "Hide the full text" : "Show the full code, diff or log";
  }
  if (pre) {
    pre.textContent = has ? detailsText() : "";
    pre.classList.toggle("hidden", !has || !speech.detailsOpen);
  }

  const el = document.getElementById("speech");
  if (el) el.classList.toggle("details-open", has && !!speech.detailsOpen);
}

/**
 * Expand/collapse the drawer. Returns the new open state.
 *
 * Expanding grows the card well past the summary, so the bubble is re-anchored
 * above the head and its hit region is republished *now* — the 200 ms publish
 * in `main.js` would leave a click-through gap over the fresh pixels.
 */
export function toggleDetails() {
  if (!detailsText()) return false;
  speech.detailsOpen = !speech.detailsOpen;
  renderDetails();
  positionBubble();
  wireRegions();
  return speech.detailsOpen;
}

/** Is the drawer open right now? */
export function detailsOpen() {
  return !!speech.detailsOpen && !!detailsText();
}

/**
 * Copy the drawer's text to the clipboard.
 *
 * The Tauri command is preferred: it is the same Win32 writer the MCP tool and
 * `POST /clipboard` use, so it works while the page's async-clipboard permission
 * is not granted. `navigator.clipboard` is the fallback outside Tauri.
 */
export async function copyDetails() {
  const text = detailsText();
  if (!text) return false;

  const result = await call("set_clipboard", { text });
  if (result && !result.__error) return true;

  if (typeof navigator !== "undefined" && navigator.clipboard && navigator.clipboard.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (err) {
      console.error("[avatar] clipboard write failed:", err);
    }
  }
  return false;
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
 * True while the details drawer is expanded (the user is reading code), the
 * quick-reply box is pinned, the pointer is over the bubble, or either text
 * input (bubble reply / command bar) has focus — the ways a user can be
 * mid-sentence when the timer would otherwise fire.
 */
function bubbleHeld() {
  if (speech.detailsOpen || isReplyOpen() || speech.hovered) return true;
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
