/**
 * char-rig — entry point
 *
 * Wires the DOM, keyboard shortcuts, the automation/debug surface
 * (`window.__avatarDebug`) and the requestAnimationFrame loop.
 *
 * This module is the only place that knows about every other module; it owns
 * the behaviour loop (idle/walk/wave) that used to be inline in index.html.
 */

import {
  avatar, RT, speech, watcher, gaze, tts, stt, PRESET_STATES, TAU,
  round, num, clamp, viewport, updateViewport,
  setExpression, markManual, toast, updateStatusUI, activeState, blinkActive,
} from "./state.js";

import {
  applyModel, refreshModelList, wireRegions, saveRig, resetRig, avatarBounds,
} from "./rig.js";

import { initRenderer, render, resizeCanvas } from "./renderer.js";

import {
  buildInspector, setRigMode, addChildBone, deleteBone,
  select, undo, bindExpressionSprite,
  onMouseDown, onMouseMove, onMouseUp, onWheel,
  buildTimeline, updateTimeline, syncTimelinePlayhead,
} from "./editor.js";

import {
  stepAnimation, onAnimationFinished, animationSnapshot,
  playAnimation, stopAnimation, sampleAnimation, listAnimations, isPlaying,
} from "./animator.js";

import {
  speak, hideSpeech, updateSpeech, openReply, closeReply,
  beginThinking, showThinking, endThinking, setBubbleHover,
  commandBarVisible, positionCommandBar, positionBubble,
  toggleDetails, copyDetails, detailsOpen,
} from "./speech.js";

import {
  setClickThrough, setupEventListeners, importFromDialog, importImageAs,
  showChooser, hideChooser, toggleCollapsed, reportState,
  applyApiStatus, refreshApiStatus,
  setWatcherEnabled, refreshWatcher, clearWatcherLogs,
  toggleWatcherDrawer, isWatcherDrawerOpen,
  refreshAiProfiles, setAiProfile, aiActiveLabel,
  setTtsEnabled, refreshTts,
  startPtt, stopPtt, refreshStt,
} from "./bridge.js";

import {
  setupHistory, toggleHistoryDrawer, historyOpen,
} from "./history.js";

import { setupSurvival, toggleSurvivalCard, survivalCardOpen } from "./survival.js";

import { setupDigivolve, toggleDigivolveCard, digivolveCardOpen } from "./digivolve.js";

import {
  setupSettings, toggleSettings, settingsOpen, onSettingsSaved,
} from "./settings.js";

import { call, listen } from "./ipc.js";

/* ---------- behaviour (play mode) ---------- */

function stepBlendshapes(dt) {
  const k = Math.min(1, avatar.blendSpeed * dt);
  for (const key of Object.keys(avatar.blendshapes)) {
    const target = avatar.targetBlendshapes[key] || 0;
    avatar.blendshapes[key] += (target - avatar.blendshapes[key]) * k;
  }
}

function startWalk(x, y, notify) {
  // A manual walk (W, centre, random) ends a perch; an agent's `POST /perch`
  // arrives through applyRemoteState, which sets the target directly.
  avatar.perched = false;
  avatar.perchRect = null;
  avatar.lookAt = null;
  avatar.targetX = x;
  avatar.targetY = y;
  avatar.startX = avatar.x;
  avatar.startY = avatar.y;
  avatar.startTime = avatar.time;
  avatar.mode = "walking";
  avatar.walkPhase = 0;
  avatar.idleTimer = 0;
  setExpression("surprised");
  if (notify !== false) {
    markManual(15);
    call("set_avatar_position", { x: x, y: y });
  }
}

function wander() {
  const margin = 120;
  startWalk(margin + Math.random() * Math.max(1, viewport.w - margin * 2),
    margin + Math.random() * Math.max(1, viewport.h - margin * 2));
}

/* ---------- gaze / eye tracking ---------- */

/** How long a motionless pointer keeps the gaze before idle glances resume. */
const GAZE_POINTER_HOLD = 2.5;
/** Idle saccade interval, seconds. */
const GLANCE_MIN = 2;
const GLANCE_MAX = 4;
/**
 * How long the **global OS cursor** keeps the gaze after its last movement.
 *
 * Longer than [`GAZE_POINTER_HOLD`] on purpose: the eyes should keep resting on
 * the cursor while the user reads something on the far side of the screen, and
 * only fall back to idle glances once it has genuinely been parked.
 */
const GAZE_CURSOR_HOLD = 4;
/** Maximum head-bone yaw bias toward the gaze target (radians, ~13°). */
const GAZE_HEAD_YAW = 0.22;
/** Cursor distance (CSS px) at which that bias reaches its clamp. */
const GAZE_HEAD_REFERENCE = 900;

/** The head bone's world point — the same origin `renderer.js` aims from. */
function headPoint() {
  let bone = RT.boneList.find((candidate) => candidate.parts.some((part) => part.tag === "head"));
  if (!bone) bone = RT.boneList.find((candidate) => /head/i.test(candidate.id));
  const world = bone ? RT.world[bone.id] : null;
  return world ? { x: world.x, y: world.y } : { x: avatar.x, y: avatar.y - 60 };
}

/** Is this screen point inside the avatar art (the click-through region)? */
function pointInAvatar(x, y) {
  if (RT.rigMode || !RT.model) return false;
  const box = avatarBounds();
  return x >= box.x && x <= box.x + box.w && y >= box.y && y <= box.y + box.h;
}

/** Centre of the command bar, or null while it is closed. */
function commandBarCenter() {
  const bar = document.getElementById("command-bar");
  if (!bar || bar.classList.contains("hidden")) return null;
  const rect = bar.getBoundingClientRect();
  return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
}

/**
 * Track the pointer where the page actually receives events, and the **global
 * OS cursor** everywhere.
 *
 * The overlay is click-through outside its hit regions, so `mousemove` only
 * exists over the avatar, the panels and the cards. Rust fills that gap: it
 * polls `GetCursorPos` every 35 ms anyway (for the click-through state machine)
 * and now publishes a deadbanded `cursor-pos` event, so the eyes follow the
 * cursor over any application — focused or not, click-through or not.
 */
function attachGazeTracking() {
  window.addEventListener("mousemove", (event) => {
    gaze.pointerX = event.clientX;
    gaze.pointerY = event.clientY;
    gaze.lastMove = avatar.time;
  });
  // Leaving the window (or the overlay) ends pointer tracking immediately.
  window.addEventListener("mouseleave", () => {
    gaze.lastMove = -100;
  });

  // Global eye tracking. Payload is `{x, y}` in CSS px of the virtual desktop,
  // already throttled by Rust (3 px deadband, ~20 Hz), so this is cheap and
  // needs no filtering of its own beyond a sanity check.
  listen("cursor-pos", (event) => {
    const payload = event && event.payload ? event.payload : {};
    const x = Number(payload.x);
    const y = Number(payload.y);
    if (!Number.isFinite(x) || !Number.isFinite(y)) return;
    gaze.cursorX = x;
    gaze.cursorY = y;
    gaze.cursorAt = avatar.time;
  }).catch((err) => console.error("[avatar] cursor-pos listener failed:", err));
}

/**
 * Decide where the eyes should look and smooth toward it.
 *
 * Priority:
 *   1. an explicit `state.look_at` (`POST /move`) — the agent asked for it;
 *   2. the focused command bar (the user is typing *at* the avatar);
 *   3. the perch rectangle, while perched on a window;
 *   4. the global OS cursor, for [`GAZE_CURSOR_HOLD`] after it last moved;
 *   5. the page-local pointer (same idea, when `cursor-pos` is unavailable);
 *   6. an idle glance every 2–4 s.
 *
 * The smoothing happens here (with `dt`) rather than in the renderer, so the
 * pupils ease instead of snapping between saccades. The same eased target also
 * feeds a small, clamped head-bone yaw bias — the head turns *a little* toward
 * whatever the eyes are following.
 */
function updateGaze(dt) {
  const active = document.activeElement;
  const typing = !!active && active.id === "command-input";
  const bar = typing ? commandBarCenter() : null;
  const pointerFresh = avatar.time - gaze.lastMove < GAZE_POINTER_HOLD;
  const cursorFresh = avatar.time - gaze.cursorAt < GAZE_CURSOR_HOLD;
  // A `look_at` is only usable as `{x, y}`: a malformed frame must not poison
  // the smoothing with NaN (the eyes would stop moving for the rest of the
  // session, because `gaze.ready` stays true).
  const lookAt = avatar.lookAt
    && Number.isFinite(Number(avatar.lookAt.x))
    && Number.isFinite(Number(avatar.lookAt.y))
    ? { x: Number(avatar.lookAt.x), y: Number(avatar.lookAt.y) }
    : null;

  let aim = null;
  let tracking = false;
  let cursorDriven = false;
  if (lookAt) {
    // An agent asked for this point explicitly (`avatar_move`'s `look_at`).
    aim = lookAt;
    tracking = true;
  } else if (bar) {
    // The user is typing at the avatar: watch the box they are typing in.
    aim = bar;
    tracking = true;
  } else if (avatar.perched && avatar.perchRect) {
    // Perched: watch the window we are standing on instead of glancing around.
    const rect = avatar.perchRect;
    aim = {
      x: num(rect.x, 0) + num(rect.width, 0) / 2,
      y: num(rect.y, 0) + num(rect.height, 0) / 2,
    };
    tracking = true;
  } else if (cursorFresh) {
    // The OS cursor, wherever it is — including over another application.
    aim = { x: gaze.cursorX, y: gaze.cursorY };
    tracking = true;
    cursorDriven = true;
  } else if (pointerFresh) {
    // Fallback for a build without the global tracker (no Rust, no event yet).
    aim = { x: gaze.pointerX, y: gaze.pointerY };
    tracking = true;
  }

  if (tracking) {
    // Do not fire a glance the instant the target goes away.
    gaze.nextGlance = Math.max(gaze.nextGlance, avatar.time + 0.8);
  } else {
    if (avatar.time >= gaze.nextGlance) {
      const head = headPoint();
      const angle = Math.random() * TAU;
      const radius = 45 + Math.random() * 95;
      gaze.glanceX = head.x + Math.cos(angle) * radius;
      gaze.glanceY = head.y + Math.sin(angle) * radius * 0.6;
      gaze.nextGlance = avatar.time + GLANCE_MIN + Math.random() * (GLANCE_MAX - GLANCE_MIN);
    }
    aim = { x: gaze.glanceX, y: gaze.glanceY };
  }

  gaze.tracking = tracking;
  gaze.cursorTracking = cursorDriven;
  gaze.targetX = aim.x;
  gaze.targetY = aim.y;

  if (!gaze.ready) {
    // First frame: start where we are looking instead of sliding in from (0,0).
    gaze.ready = true;
    gaze.x = aim.x;
    gaze.y = aim.y;
  } else {
    const k = Math.min(1, dt * 12);
    gaze.x += (gaze.targetX - gaze.x) * k;
    gaze.y += (gaze.targetY - gaze.y) * k;
  }

  // Head-bone yaw bias: a small, clamped turn toward the aim point, damped so
  // the neck follows the eyes instead of snapping. The bias is only a *bias* —
  // `rig.js` adds it to the head bone and `localRotation` clamps the sum
  // against the bone's `limits`, so a rig with a declared neck range can never
  // be turned past it.
  const head = headPoint();
  const yaw = clamp((gaze.targetX - head.x) / GAZE_HEAD_REFERENCE, -1, 1) * GAZE_HEAD_YAW;
  // Idle glances need no bias: the eyes carry that motion, and a head that
  // twitches at every saccade reads as broken.
  gaze.tiltTarget = tracking ? yaw : 0;
  gaze.tilt += (gaze.tiltTarget - gaze.tilt) * Math.min(1, dt * 4);
  if (Math.abs(gaze.tilt) < 0.0005) gaze.tilt = 0;

  // Hover life-sign: ease a 0..1 weight that `rig.js` turns into a head tilt.
  // The pointer has to be on the art *and* recently alive (not a stale reading
  // left behind while the avatar wandered away).
  const over = pointerFresh && pointInAvatar(gaze.pointerX, gaze.pointerY);
  gaze.hoverTarget = over ? 1 : 0;
  gaze.hover += (gaze.hoverTarget - gaze.hover) * Math.min(1, dt * 5);
  if (gaze.hover < 0.002) gaze.hover = 0;
}

function step(dt) {
  avatar.time += dt;
  stepBlendshapes(dt);
  updateSpeech(dt);
  // Custom keyframe clips keep running in Rig Edit Mode (that is the preview).
  stepAnimation(dt);
  if (isPlaying()) syncTimelinePlayhead();
  if (RT.rigMode) return;

  // ---- automatic blinking: 3-6 s apart, ~120 ms long ----
  if (avatar.time > avatar.nextBlink) {
    avatar.blinkUntil = avatar.time + 0.12;
    avatar.nextBlink = avatar.time + 3 + Math.random() * 3;
    avatar.blinkCount += 1;
  }

  // ---- expression hold / auto revert ----
  if (avatar.expressionHold > 0) {
    avatar.expressionHold -= dt;
    if (avatar.expressionHold <= 0) {
      avatar.expressionHold = 0;
      const back = avatar.expressionRevert || "neutral";
      if (back !== avatar.expression) setExpression(back);
    }
  }

  if (avatar.mode === "idle") {
    // While the bubble is up the avatar stands still: talking pins it in place
    // so the card (and the person reading it) never drifts across the screen.
    // An explicit /move or a keypress still overrides this — only the automatic
    // wander below is held back.
    if (speech.visible || avatar.perched) {
      // Talking pins it in place so the bubble never drifts; a perch pins it
      // too, otherwise the avatar would wander off the window it is watching.
      avatar.idleTimer = 0;
    } else {
      avatar.idleTimer += dt;
      if (avatar.idleTimer >= avatar.nextIdle && avatar.time > avatar.manualUntil) {
        wander();
        avatar.nextIdle = 2.5 + Math.random() * 3.5;
      }
    }
  } else if (avatar.mode === "walking") {
    const elapsed = avatar.time - avatar.startTime;
    const dx = avatar.targetX - avatar.startX;
    const dy = avatar.targetY - avatar.startY;
    const distance = Math.hypot(dx, dy);
    const duration = distance / Math.max(1, avatar.walkSpeed);
    if (elapsed >= duration || distance < 1) {
      avatar.x = avatar.targetX;
      avatar.y = avatar.targetY;
      avatar.mode = "idle";
      avatar.idleTimer = 0;
      avatar.walkPhase = 0;
      // a held expression (e.g. /emote) outranks the idle neutral face
      if (avatar.time > avatar.manualUntil && avatar.expressionHold <= 0) setExpression("neutral");
      updateStatusUI();
    } else {
      const t = elapsed / duration;
      const e = t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
      avatar.x = avatar.startX + dx * e;
      avatar.y = avatar.startY + dy * e;
      avatar.walkPhase = avatar.time % 1;
    }
  } else if (avatar.mode === "waving") {
    avatar.wavePhase += dt * 4;
    if (avatar.wavePhase > Math.PI * 4) {
      avatar.mode = "idle";
      avatar.wavePhase = 0;
      updateStatusUI();
    }
  }

  // Gaze runs last: the head bone has moved by now, and the next `render()`
  // turns the smoothed aim point into a pupil offset.
  updateGaze(dt);
}

/* ---------- main loop ---------- */

let lastFrame = performance.now();

function frame(now) {
  const dt = Math.min(0.05, (now - lastFrame) / 1000);
  lastFrame = now;
  step(dt);
  // `render()` refreshes RT.world (computeWorld) before drawing, so the bubble
  // can be re-anchored against this frame's head position instead of the 10 Hz
  // throttle it used to run on.
  render();
  positionBubble();
  reportState(now);
  requestAnimationFrame(frame);
}

/* ---------- avatar interaction detection (play mode) ---------- */

/** Two clicks closer than this (and near each other) are one double-click. */
const DOUBLE_CLICK_MS = 320;
/** Pointer travel above this turns a press into a drag, not a click. */
const DRAG_START_PX = 8;
/** Idle wander stays paused this long after a drag ends, in seconds. */
const DRAG_HOLD_SECS = 15;

/** Report an interaction with the avatar art; Rust fans it out on `GET /events`. */
function notifyInteraction(kind, point) {
  return call("notify_interaction", {
    kind: kind,
    x: Math.round(point.x * 10) / 10,
    y: Math.round(point.y * 10) / 10,
  });
}

function canvasPoint(canvas, event) {
  const rect = canvas.getBoundingClientRect();
  return { x: event.clientX - rect.left, y: event.clientY - rect.top };
}

/**
 * Clamp a proposed avatar anchor so the whole art box stays on screen.
 *
 * `avatarBounds()` is measured from the current anchor, so the box's offsets
 * around the anchor are constant while the pose is held still (a drag parks the
 * avatar in `idle` first, so that holds).
 */
function clampAvatarPosition(x, y) {
  const box = avatarBounds();
  const left = box.x - avatar.x;
  const top = box.y - avatar.y;
  const minX = -left;
  const minY = -top;
  const maxX = viewport.w - (left + box.w);
  const maxY = viewport.h - (top + box.h);
  const lo = (min, max) => Math.min(min, max);
  const hi = (min, max) => Math.max(min, max);
  return {
    x: Math.min(Math.max(x, lo(minX, maxX)), hi(minX, maxX)),
    y: Math.min(Math.max(y, lo(minY, maxY)), hi(minY, maxY)),
  };
}

/**
 * Hit-test the avatar's bounding box (the same box that becomes a click-through
 * region) and report click / double_click / drag_end — and, in play mode, let a
 * press inside that box drag the avatar across the desktop.
 *
 * Rig Edit Mode is excluded: there a press is a gizmo drag and the editor owns
 * it, so the agent would otherwise get a stream of rig-editing clicks.
 */
function attachInteractionDetection(canvas) {
  let pressOrigin = null;
  /** Grab offset while a press inside the avatar could become a drag. */
  let grab = null;
  let lastClick = { t: 0, x: 0, y: 0 };

  const overAvatar = (point) => {
    // RT.world is refreshed by the render loop, so the bounds are current.
    if (RT.rigMode || !RT.model) return false;
    const box = avatarBounds();
    return point.x >= box.x && point.x <= box.x + box.w &&
      point.y >= box.y && point.y <= box.y + box.h;
  };

  canvas.addEventListener("mousedown", (event) => {
    if (event.button !== 0 || RT.rigMode) {
      pressOrigin = null;
      grab = null;
      return;
    }
    const point = canvasPoint(canvas, event);
    pressOrigin = overAvatar(point) ? point : null;
    // The anchor offset is captured at press time, so the avatar keeps its grip
    // under the cursor instead of snapping its centre to the pointer.
    grab = pressOrigin
      ? { dx: avatar.x - point.x, dy: avatar.y - point.y, active: false }
      : null;
  });

  window.addEventListener("mousemove", (event) => {
    if (!grab || !pressOrigin || RT.rigMode) return;
    const point = canvasPoint(canvas, event);

    if (!grab.active) {
      if (Math.hypot(point.x - pressOrigin.x, point.y - pressOrigin.y) <= DRAG_START_PX) return;
      // Past the threshold: this press is a drag, not a click.
      grab.active = true;
      avatar.dragging = true;
      // A drag is a manual move: tell Rust to drop the perch before the 60 ms
      // tracker re-targets the avatar back to the window border.
      call("break_perch").catch(() => {});
      avatar.perched = false;
      avatar.perchRect = null;
      avatar.lookAt = null;
      avatar.mode = "idle";
      avatar.idleTimer = 0;
      avatar.walkPhase = 0;
      avatar.targetX = avatar.x;
      avatar.targetY = avatar.y;
      // Pauses idle wander for the gesture and for DRAG_HOLD_SECS after it.
      markManual(DRAG_HOLD_SECS);
      updateStatusUI();
      wireRegions(); // full-viewport hit region for the rest of the gesture
    }

    const next = clampAvatarPosition(point.x + grab.dx, point.y + grab.dy);
    avatar.x = next.x;
    avatar.y = next.y;
    avatar.targetX = next.x;
    avatar.targetY = next.y;
    // The clickable region has to follow the art, or the press-through window
    // would keep testing the old rectangle.
    wireRegions();
  });

  // A drag whose mouseup never arrives (alt-tab, focus stolen) would leave the
  // overlay permanently interactive; losing focus cancels the gesture.
  window.addEventListener("blur", () => {
    if (grab && grab.active) {
      avatar.dragging = false;
      markManual(DRAG_HOLD_SECS);
      updateStatusUI();
      wireRegions();
    }
    pressOrigin = null;
    grab = null;
  });

  window.addEventListener("mouseup", (event) => {
    const origin = pressOrigin;
    const drag = grab;
    pressOrigin = null;
    grab = null;
    if (!origin || RT.rigMode || event.button !== 0) return;

    const point = canvasPoint(canvas, event);
    const travelled = Math.hypot(point.x - origin.x, point.y - origin.y);
    if ((drag && drag.active) || travelled > DRAG_START_PX) {
      if (avatar.dragging) {
        avatar.dragging = false;
        markManual(DRAG_HOLD_SECS);
        updateStatusUI();
        wireRegions(); // back to the avatar box only
      }
      notifyInteraction("drag_end", point);
      return;
    }

    const now = performance.now();
    const isDouble = now - lastClick.t < DOUBLE_CLICK_MS &&
      Math.hypot(point.x - lastClick.x, point.y - lastClick.y) < 24;
    notifyInteraction(isDouble ? "double_click" : "click", point);
    // A completed double-click resets the chain so a third click starts over.
    lastClick = isDouble ? { t: 0, x: 0, y: 0 } : { t: now, x: point.x, y: point.y };
  });
}

/* ---------- ask / task (command bar + bubble reply) ---------- */

/** Chips used when `watcher_rules.json` has no `quick_tasks` (mirrors lib.rs). */
const DEFAULT_QUICK_TASKS = [
  {
    label: "Summarize clipboard",
    prompt: "Call avatar_get_clipboard and summarise what I copied in two sentences: say what it is " +
      "(code, error, notes, link) and the one thing I most likely want to do with it.",
  },
  {
    label: "What am I looking at?",
    prompt: "Call avatar_get_context, then tell me which window and process I am in and what I am " +
      "probably doing. Offer one concrete next step. Keep it under 180 characters.",
  },
  {
    label: "What's on my screen?",
    prompt: "Look at my screen and tell me what I am working on, naming the app and the one thing " +
      "that most needs my attention. If there is an error or a question on screen, quote it.",
    include_screen: true,
  },
  {
    label: "Quick diagnostic",
    prompt: "Run a quick diagnostic from the desktop context: call avatar_get_context and " +
      "avatar_get_clipboard, check idle time, the focused process and anything copied that looks " +
      "like an error, then report only the notable findings (or say everything looks normal).",
  },
];

/**
 * Send a direct message ("ask" or "task") to Rust, which fans it out as a
 * `user_reply` frame for the watcher.
 *
 * The visual feedback is immediate on purpose: a local model can take a while,
 * and the user must see that the task was received. `beginThinking` arms the
 * bubble so the `speech-clear` Rust emits (dismissing the old prompt) becomes a
 * "Thinking…" card instead of a blank screen.
 */
async function sendUserMessage(text, profile, includeScreen) {
  const body = String(text == null ? "" : text).trim();
  if (!body) return false;
  // Empty/absent profile = whatever the panel has active.
  const pinned = typeof profile === "string" && profile.trim() ? profile.trim() : null;
  // Screen vision: the watcher captures a frame and answers from it.
  const screen = includeScreen === true;

  setExpression("thinking");
  markManual(screen ? 60 : 30);
  call("set_avatar_expression", { expression: "thinking" });
  beginThinking(screen ? "Looking at screen…" : "Thinking…");

  const result = await call("notify_reply", {
    text: body,
    profile: pinned,
    include_screen: screen,
  });
  if (result && result.__error) {
    endThinking();
    toast("Send failed: " + result.__error, true);
    return false;
  }
  // No speech-clear arrived (not Tauri, or it was coalesced): show it here.
  if (!speech.thinking) {
    endThinking();
    showThinking(screen ? "Looking at screen…" : "Thinking…");
  }
  toast("Sent");
  return true;
}

/**
 * Set by the global-hotkey summon: Rust turns click-through off so the overlay
 * can take the keyboard, and this flag remembers whether the desktop should go
 * back to click-through once the bar closes.
 */
let resumeClickThrough = false;

/** Open the floating ask/task bar, anchored under the avatar, and focus it. */
async function openCommandBar(focus = true) {
  const bar = document.getElementById("command-bar");
  if (!bar) return;
  bar.classList.remove("hidden");
  positionCommandBar();
  renderQuickTasks();
  if (focus) {
    const input = document.getElementById("command-input");
    if (input) input.focus();
  }
  wireRegions();
  // Refresh the chips from the rules file in the background (no recompile).
  refreshWatcher().then(renderQuickTasks);
}

function closeCommandBar() {
  const bar = document.getElementById("command-bar");
  if (!bar) return;
  bar.classList.add("hidden");
  bar.classList.remove("collapsed");
  const input = document.getElementById("command-input");
  if (input) {
    input.value = "";
    if (document.activeElement === input) input.blur();
  }
  wireRegions();
  // A global-hotkey summon turns click-through off so the window can take the
  // keyboard. Put the desktop back once the bar is dismissed, but only if it was
  // the armed default — a user who switched click-through off keeps it off.
  if (resumeClickThrough) {
    resumeClickThrough = false;
    call("set_click_through", { enabled: true });
  }
}

function toggleCommandBar() {
  return commandBarVisible() ? (closeCommandBar(), false) : (openCommandBar(), true);
}

/** Render the quick-task chips (rules file → GET /watcher → watcher.quickTasks). */
function renderQuickTasks() {
  const host = document.getElementById("command-chips");
  if (!host) return;
  const tasks = Array.isArray(watcher.quickTasks) && watcher.quickTasks.length
    ? watcher.quickTasks
    : DEFAULT_QUICK_TASKS;
  host.innerHTML = "";
  for (const task of tasks) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip";
    chip.textContent = task.label;
    chip.title = task.prompt;
    if (task.profile) chip.dataset.profile = task.profile;
    if (task.include_screen) chip.dataset.screen = "true";
    chip.addEventListener("click", () =>
      runQuickTask(task.prompt, task.profile, task.include_screen));
    host.appendChild(chip);
  }
}

/** A chip is one click → one task: close the bar and send it. */
function runQuickTask(prompt, profile, includeScreen) {
  closeCommandBar();
  sendUserMessage(prompt, profile, includeScreen);
}

/** Wipe the conversation context (the watcher rotates its session id). */
async function resetSession() {
  closeCommandBar();
  // `endThinking` clears the "waiting" guard so the bubble really goes away;
  // a thinking card deliberately ignores stray clears.
  endThinking();
  hideSpeech();
  const result = await call("reset_session");
  if (result && result.__error) {
    toast("Reset failed: " + result.__error, true);
  } else {
    toast("New chat — context reset");
  }
  wireRegions();
}

/** Wire the bubble's optional input and the bubble's own hover/click behaviour. */
function setupQuickReply() {
  const bubble = document.getElementById("speech");
  const form = document.getElementById("speech-reply");
  const input = document.getElementById("speech-reply-input");
  if (!bubble || !form || !input) return;

  // Clicking the bubble summons the full command bar (chips + new chat), except
  // on the Details/Copy row — those are the bubble's own controls.
  bubble.addEventListener("click", (event) => {
    if (form.contains(event.target)) return;
    if (event.target.closest && event.target.closest("#speech-actions")) return;
    openCommandBar();
  });

  // Details ▾ expands the code/log drawer; Copy puts the full text on the
  // clipboard (same Win32 writer as POST /clipboard and avatar_set_clipboard).
  const detailsButton = document.getElementById("speech-details-toggle");
  const copyButton = document.getElementById("speech-copy");
  if (detailsButton) {
    detailsButton.addEventListener("click", (event) => {
      event.stopPropagation();
      toggleDetails();
    });
  }
  if (copyButton) {
    copyButton.addEventListener("click", async (event) => {
      event.stopPropagation();
      const copied = await copyDetails();
      toast(copied ? "Copied full text" : "Copy failed", !copied);
    });
  }

  // Hovering the bubble pauses its auto-dismiss timer.
  bubble.addEventListener("mouseenter", () => setBubbleHover(true));
  bubble.addEventListener("mouseleave", () => setBubbleHover(false));

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const text = input.value.trim();
    if (!text) {
      closeReply();
      wireRegions();
      return;
    }
    input.value = "";
    sendUserMessage(text);
  });

  input.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      // Keep Esc from also collapsing the panel (the global handler ignores
      // INPUT targets, but stop it here so the intent is explicit).
      event.preventDefault();
      event.stopPropagation();
      closeReply();
      wireRegions();
    }
  });
}

/** Wire the floating ask/task bar (input, send, new chat, close). */
function setupCommandBar() {
  const bar = document.getElementById("command-bar");
  const form = document.getElementById("command-form");
  const input = document.getElementById("command-input");
  if (!bar || !form || !input) return;

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const text = input.value.trim();
    if (!text) {
      closeCommandBar();
      return;
    }
    input.value = "";
    closeCommandBar();
    sendUserMessage(text);
  });

  input.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeCommandBar();
    }
  });

  document.getElementById("command-close").addEventListener("click", closeCommandBar);
  document.getElementById("command-newchat").addEventListener("click", resetSession);

  // The system-wide hotkey (Rust: `summon-command-bar`). Rust has already
  // disabled click-through and focused the window, so all that is left is to
  // open the bar and put the caret in the input — type straight away from any
  // application. The flag restores click-through when the bar closes.
  listen("summon-command-bar", () => {
    resumeClickThrough = avatar.clickThrough;
    openCommandBar();
  }).catch((err) => console.error("[avatar] summon listener failed:", err));

  // Keep the bar glued under the avatar while it wanders, and re-publish the
  // region when it moves.
  setInterval(() => {
    if (!commandBarVisible()) return;
    positionCommandBar();
    wireRegions();
  }, 250);
}

/* ---------- UI wiring ---------- */

function setupUI() {
  const canvas = document.getElementById("canvas");

  document.getElementById("btn-close").addEventListener("click", () => call("close_window"));
  document.getElementById("btn-minimize").addEventListener("click", () => call("minimize_window"));
  document.getElementById("btn-clickthrough").addEventListener("click", () => setClickThrough(!avatar.clickThrough));
  // Ask / task: titlebar icon, control-panel button and the floating bar itself.
  document.getElementById("btn-chat").addEventListener("click", () => openCommandBar());
  document.getElementById("btn-ask").addEventListener("click", () => openCommandBar());
  // Screen vision: one click captures the focused window and asks the local
  // vision model. Same path as the "What's on my screen?" chip.
  document.getElementById("btn-screen").addEventListener("click", () => {
    sendUserMessage(
      "Look at my screen and tell me what I am working on, naming the app and the one thing that " +
        "most needs my attention. If there is an error or a question on screen, quote it.",
      null,
      true,
    );
  });
  document.getElementById("btn-new-chat").addEventListener("click", resetSession);
  document.getElementById("btn-rig").addEventListener("click", () => setRigMode(!RT.rigMode));
  document.getElementById("btn-rig-panel").addEventListener("click", () => setRigMode(true));
  document.getElementById("btn-exit-rig").addEventListener("click", () => setRigMode(false));

  document.querySelectorAll("[data-expression]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      setExpression(btn.dataset.expression);
      markManual(20);
      await call("set_avatar_expression", { expression: btn.dataset.expression });
    });
  });

  document.getElementById("btn-wave").addEventListener("click", async () => {
    avatar.mode = "waving";
    avatar.wavePhase = 0;
    markManual(15);
    updateStatusUI();
    await call("trigger_wave");
  });

  document.getElementById("btn-idle").addEventListener("click", async () => {
    avatar.mode = "idle";
    avatar.idleTimer = 0;
    avatar.targetX = avatar.x;
    avatar.targetY = avatar.y;
    markManual(20);
    setExpression("neutral");
    updateStatusUI();
    await call("set_avatar_state", { new_state: "idle" });
  });

  document.getElementById("btn-walk-random").addEventListener("click", wander);
  document.getElementById("btn-center").addEventListener("click", () => startWalk(viewport.w / 2, viewport.h / 2));

  const speed = document.getElementById("walk-speed");
  const speedValue = document.getElementById("walk-speed-value");
  speed.addEventListener("input", () => {
    avatar.walkSpeed = Number(speed.value);
    speedValue.textContent = avatar.walkSpeed + " px/s";
  });

  document.getElementById("btn-collapse").addEventListener("click", () => toggleCollapsed(true));
  document.getElementById("panel-toggle").addEventListener("click", () => toggleCollapsed(false));

  // ---- watcher switch + pop-out console ----
  document.getElementById("watcher-toggle").addEventListener("click", async () => {
    await setWatcherEnabled(!watcher.enabled);
  });
  document.getElementById("btn-watcher-logs").addEventListener("click", () => {
    toggleWatcherDrawer();
  });
  document.getElementById("btn-watcher-close").addEventListener("click", () => {
    toggleWatcherDrawer(false);
  });
  document.getElementById("btn-watcher-clear").addEventListener("click", clearWatcherLogs);
  // ---- Voice (local Piper TTS): mute / unmute every spoken bubble ----
  document.getElementById("tts-toggle").addEventListener("click", async () => {
    const status = await setTtsEnabled(!tts.enabled);
    if (status && !status.__error) {
      toast(tts.enabled ? "Voice on" : "Voice muted");
    }
  });

  // ---- Push to talk: hold the mic button (or hold Alt+V anywhere) ----
  // `pttHeld` guards against a pointerup arriving without a pointerdown (and
  // against the window listener double-stopping a release on the button).
  let pttHeld = false;
  const pttButton = document.getElementById("btn-ptt");
  const releasePtt = async () => {
    if (!pttHeld) return;
    pttHeld = false;
    await stopPtt();
  };
  pttButton.addEventListener("pointerdown", async (event) => {
    if (event.button !== 0 || pttHeld || !stt.ready || !stt.enabled) return;
    event.preventDefault();
    pttHeld = true;
    const status = await startPtt();
    // The mic refused to open: do not wait for a release that cannot help.
    if (status && status.state === "idle" && status.last_error) {
      pttHeld = false;
      toast("Microphone: " + status.last_error, true);
    }
  });
  pttButton.addEventListener("pointerup", releasePtt);
  pttButton.addEventListener("pointercancel", releasePtt);
  // A release outside the button must still end the hold.
  window.addEventListener("pointerup", releasePtt);
  document.getElementById("btn-watcher-ping").addEventListener("click", async () => {
    // One click drives the whole pipeline: the page reports an interaction,
    // Rust fans it out on GET /events, the watcher wakes and Hermes answers over
    // MCP. Watch the console for the trigger line and the spawn.
    await refreshWatcher();
    await notifyInteraction("double_click", { x: avatar.x, y: avatar.y });
    toast("Test ping sent (double_click)");
  });

  // ---- AI Brain: profile dropdown + health badge ----
  document.getElementById("ai-profile-select").addEventListener("change", async (event) => {
    const name = event.target.value;
    if (!name) return;
    const status = await setAiProfile(name);
    if (!status) {
      toast("Could not switch the AI brain", true);
      await refreshAiProfiles();
      return;
    }
    const health = (status.active_health && status.active_health.status) || "unknown";
    toast(`AI brain: ${aiActiveLabel()} (${health})`);
  });

  document.getElementById("model-select").addEventListener("change", async (event) => {
    const model = await call("load_model", { name: event.target.value });
    if (model && !model.__error) {
      applyModel(model);
      setExpression("neutral");
      toast("Loaded model " + model.name);
    }
  });

  document.getElementById("btn-import").addEventListener("click", importFromDialog);
  document.getElementById("chooser-model").addEventListener("click", () => importImageAs("model"));
  document.getElementById("chooser-part").addEventListener("click", () => importImageAs("part"));
  document.getElementById("chooser-cancel").addEventListener("click", hideChooser);

  document.getElementById("btn-save-rig").addEventListener("click", saveRig);
  document.getElementById("btn-reset-rig").addEventListener("click", resetRig);
  document.getElementById("btn-undo").addEventListener("click", undo);
  document.getElementById("btn-add-bone").addEventListener("click", addChildBone);
  document.getElementById("btn-del-bone").addEventListener("click", deleteBone);

  canvas.addEventListener("mousedown", onMouseDown);
  window.addEventListener("mousemove", onMouseMove);
  window.addEventListener("mouseup", onMouseUp);
  canvas.addEventListener("wheel", onWheel, { passive: false });
  canvas.addEventListener("contextmenu", (event) => {
    if (RT.rigMode) event.preventDefault();
  });

  attachInteractionDetection(canvas);
  attachGazeTracking();
  setupQuickReply();
  setupCommandBar();

  window.addEventListener("keydown", (event) => {
    if (event.target && (event.target.tagName === "INPUT" || event.target.tagName === "SELECT")) return;
    const key = event.key.toLowerCase();
    // Enter or "/" anywhere on the overlay summons the ask/task bar — the
    // fastest way to give the avatar a task without opening a terminal.
    if (key === "enter" || key === "/") {
      event.preventDefault();
      openCommandBar();
      return;
    }
    if ((event.ctrlKey || event.metaKey) && key === "z") {
      event.preventDefault();
      undo();
      return;
    }
    switch (key) {
      case "1": setExpression("happy"); markManual(20); call("set_avatar_expression", { expression: "happy" }); break;
      case "2": setExpression("sad"); markManual(20); call("set_avatar_expression", { expression: "sad" }); break;
      case "3": setExpression("surprised"); markManual(20); call("set_avatar_expression", { expression: "surprised" }); break;
      case "4": setExpression("neutral"); markManual(20); call("set_avatar_expression", { expression: "neutral" }); break;
      case "w": wander(); break;
      case "i":
        avatar.mode = "idle"; avatar.idleTimer = 0;
        avatar.targetX = avatar.x; avatar.targetY = avatar.y;
        markManual(20);
        call("set_avatar_state", { new_state: "idle" }); updateStatusUI();
        break;
      case "c": startWalk(viewport.w / 2, viewport.h / 2); break;
      case " ":
        avatar.mode = "waving"; avatar.wavePhase = 0; markManual(15); updateStatusUI();
        call("trigger_wave");
        event.preventDefault();
        break;
      case "r": setRigMode(!RT.rigMode); break;
      // The conversation log: same toggle as the panel/command-bar buttons.
      case "h": toggleHistoryDrawer(); break;
      case "escape":
        if (settingsOpen()) toggleSettings(false);
        else if (commandBarVisible()) closeCommandBar();
        else if (historyOpen()) toggleHistoryDrawer(false);
        else if (digivolveCardOpen()) toggleDigivolveCard(false);
        else if (survivalCardOpen()) toggleSurvivalCard(false);
        else if (RT.rigMode) setRigMode(false);
        else if (isWatcherDrawerOpen()) toggleWatcherDrawer(false);
        else toggleCollapsed(!avatar.collapsed); break;
      default: break;
    }
  });
}

/* ---------- automation / debug surface ---------- */

/**
 * Installed before the async init so CDP test scripts can poll for it.
 * Every member is a live function reading the current module state.
 */
function installDebugSurface() {
  window.__avatarDebug = {
    ready: false,
    setExpression: setExpression,
    speak: speak,
    hideSpeech: hideSpeech,
    /** Open the bubble's quick-reply box (CDP tests can drive it from here). */
    openReply: () => openReply(),
    /** Is the quick-reply box pinned open right now? */
    replyBox: () => {
      const box = document.getElementById("speech");
      return !!box && box.classList.contains("reply-open");
    },
    /** Ask/task bar: open it, close it, or read its state. */
    openCommandBar: () => openCommandBar(),
    closeCommandBar: () => closeCommandBar(),
    commandBar: () => ({
      open: commandBarVisible(),
      focused: (document.activeElement || {}).id || null,
      chips: Array.from(document.querySelectorAll("#command-chips .chip")).map((chip) => chip.textContent),
    }),
    /** Send a message exactly as the bar does (Enter in the input). */
    ask: (text) => sendUserMessage(text),
    /** One-click New chat. */
    newChat: () => resetSession(),
    thinking: () => ({
      thinking: speech.thinking,
      visible: speech.visible,
      text: speech.visible ? speech.full : "",
    }),
    /** Local voice (Piper): enabled / ready / why it is silent. */
    tts: () => ({ enabled: tts.enabled, ready: tts.ready, reason: tts.reason, model: tts.model }),
    /** Push to talk: live state, readiness, hotkey and the last transcript. */
    stt: () => ({
      enabled: stt.enabled,
      ready: stt.ready,
      state: stt.state,
      hotkey: stt.hotkey,
      transcript: stt.transcript,
      reason: stt.reason,
    }),
    /** Details drawer: read it, expand it, copy it (CDP can drive all three). */
    details: () => ({
      open: detailsOpen(),
      chars: speech.details ? speech.details.length : 0,
      text: speech.details || "",
    }),
    toggleDetails: () => toggleDetails(),
    copyDetails: () => copyDetails(),
    /** Eye tracking: where the pupils are aiming and how far the hover eased. */
    gaze: () => ({
      x: round(gaze.x, 1), y: round(gaze.y, 1),
      targetX: round(gaze.targetX, 1), targetY: round(gaze.targetY, 1),
      tracking: gaze.tracking,
      hover: round(gaze.hover, 3),
      glanceAt: round(gaze.nextGlance, 2),
      // Global eye tracking (Rust `cursor-pos`): where the OS cursor is, how
      // long ago it last moved, and the eased head yaw it is producing.
      cursorX: round(gaze.cursorX, 1), cursorY: round(gaze.cursorY, 1),
      cursorAt: round(gaze.cursorAt, 2),
      cursorFresh: avatar.time - gaze.cursorAt < GAZE_CURSOR_HOLD,
      cursorTracking: gaze.cursorTracking,
      tilt: round(gaze.tilt, 4), tiltTarget: round(gaze.tiltTarget, 4),
    }),
    /** Perch/gaze directives the page is currently obeying (see `POST /perch`). */
    perched: () => ({
      perched: !!avatar.perched,
      rect: avatar.perchRect || null,
      lookAt: avatar.lookAt || null,
    }),
    /** Lip sync: current character, the openness it produced, and whether a
     *  Piper RMS envelope is driving it (audio-driven) or not (text-driven). */
    mouth: () => ({
      char: speech.char,
      open: round(speech.mouthOpen, 3),
      smooth: round(speech.mouthSmooth, 3),
      variant: speech.mouthVariant,
      envelope: speech.envelope ? speech.envelope.length : 0,
      bucketMs: speech.envelope ? speech.envelopeBucketMs : 0,
    }),
    /** Bubble geometry vs. the head anchor — the per-frame tracking check. */
    bubble: () => {
      const el = document.getElementById("speech");
      if (!el || el.classList.contains("hidden")) return null;
      const rect = el.getBoundingClientRect();
      const anchor = headPoint();
      return {
        left: round(rect.left, 1), top: round(rect.top, 1),
        anchorX: round(anchor.x, 1), anchorY: round(anchor.y, 1),
        /** Head-to-bubble gap: a constant ~30 px while tracking, drifting when stale. */
        anchorGap: round(anchor.y - (rect.top + rect.height), 1),
      };
    },
    selectBone: (id, partId) => select(id, partId || null),
    undo: undo,
    /** Same path the canvas uses: notify Rust of a pointer interaction. */
    interaction: (kind, x, y) => notifyInteraction(kind, { x: x, y: y }),
    /** Current avatar bounding box (the click-through region for the art). */
    bounds: () => avatarBounds(),

    /**
     * Bind `path` as the sprite for `preset` on the first part with `tag`
     * (same code path as the editor's Bind PNG button, minus the file dialog).
     */
    bindExpression: async (path, tag, preset) => {
      const part = RT.boneList.reduce((found, bone) => found || bone.parts.find((entry) => entry.tag === tag), null);
      const map = PRESET_STATES[tag];
      if (!part || !map) return "no matching part or tag";
      const key = map[preset];
      if (!key) return "unknown preset";
      const outcome = await call("import_asset", { path: path, mode: "part" });
      if (!outcome || !outcome.staged) return "staging failed";
      const rigModule = await import("./rig.js");
      rigModule.ensureImage(outcome.staged.image, outcome.staged.data_url);
      const stateModule = await import("./state.js");
      stateModule.snapshot();
      bindExpressionSprite(part, key, outcome.staged);
      return key;
    },

    bones: () => RT.boneList.map((bone) => ({
      id: bone.id,
      parent: bone.parent,
      pos: bone.pos,
      rotation: round(num(bone.rotation, 0), 4),
      length: bone.length,
      world: RT.world[bone.id]
        ? { x: round(RT.world[bone.id].x, 1), y: round(RT.world[bone.id].y, 1),
            rot: round(RT.world[bone.id].rot, 4), scale: round(RT.world[bone.id].scale, 3) }
        : null,
    })),

    handle: (id) => {
      const bone = RT.bones[id];
      const world = RT.world[id];
      if (!bone || !world) return null;
      return {
        x: round(world.x + Math.cos(world.rot) * num(bone.length, 0) * world.scale, 1),
        y: round(world.y + Math.sin(world.rot) * num(bone.length, 0) * world.scale, 1),
      };
    },

    snapshot: () => ({
      expression: avatar.expression,
      talking: avatar.talk,
      blinking: blinkActive(),
      blinks: avatar.blinkCount,
      speech: speech.visible ? { text: speech.full, shown: speech.shown } : null,
      expressionHold: round(avatar.expressionHold, 2),
      states: RT.boneList.reduce((acc, bone) => {
        for (const part of bone.parts) {
          if (part.tag) acc[bone.id + "/" + part.id] = activeState(part);
        }
        return acc;
      }, {}),
    }),

    viewport: () => ({ w: viewport.w, h: viewport.h }),
    position: () => ({ x: round(avatar.x, 1), y: round(avatar.y, 1), mode: avatar.mode }),

    /** API line + dot state, for CDP tests and for agents that want the truth. */
    api: () => ({
      text: (document.getElementById("api-url") || {}).textContent || "",
      dot: (document.getElementById("api-dot") || {}).className || "",
      title: (document.getElementById("api-dot") || {}).title || "",
    }),

    /** Live editor drag state, or null — lets CDP tests see what a press hit. */
    drag: () => (RT.drag ? JSON.parse(JSON.stringify(RT.drag)) : null),
    /** Current inspector selection (`{bone, part}`) or null. */
    selection: () => (RT.selected ? { bone: RT.selected.bone, part: RT.selected.part || null } : null),
    /** Every part with its editable fields, including the bone lock flag. */
    parts: () => RT.boneList.reduce((acc, bone) => {
      for (const part of bone.parts) {
        acc.push({
          bone: bone.id,
          id: part.id,
          offset: [part.offset[0], part.offset[1]],
          rotation: num(part.rotation, 0),
          scale: num(part.scale, 1),
          lockBone: !!part.lockBone,
        });
      }
      return acc;
    }, []),

    /** Custom keyframe animation surface (mirrors animator.js). */
    animations: () => listAnimations(),
    playAnimation: (name, loop) => {
      const clip = playAnimation(name, loop);
      updateTimeline();
      return clip ? animationSnapshot() : null;
    },
    stopAnimation: () => {
      stopAnimation();
      updateTimeline();
      return animationSnapshot();
    },
    sampleAnimation: (t, name) => sampleAnimation(t, name),
    animation: () => animationSnapshot(),
  };
}

/* ---------- init ---------- */

async function init() {
  const canvas = document.getElementById("canvas");
  updateViewport();
  initRenderer(canvas);

  window.addEventListener("resize", () => {
    resizeCanvas();
    wireRegions();
  });

  setExpression("neutral");
  setupUI();
  // The conversation log: hydrate from Rust, then follow `chat-message` /
  // `chat-history-cleared`. Its **New chat** button reuses the page's own
  // `resetSession`, so both entry points rotate the session identically.
  setupHistory({ onNewChat: resetSession });
  buildInspector();
  buildTimeline();
  updateStatusUI();

  // A non-looping clip that reaches its end returns the avatar to idle: the
  // "animation finished" state the state machine and the timeline both show.
  onAnimationFinished(() => {
    avatar.mode = "idle";
    avatar.idleTimer = 0;
    updateStatusUI();
    updateTimeline();
    console.log("[avatar] animation finished");
  });

  await setupEventListeners();

  await refreshModelList();
  const model = await call("get_model");
  if (model && !model.__error) {
    const bones = applyModel(model) || 0;
    console.log("[avatar] ready with", bones, "bones");
  }
  updateTimeline();
  const info = await call("get_runtime_info");
  if (info) applyApiStatus(info);
  // The dot must not lie if the server dies without an event.
  setInterval(() => { refreshApiStatus(); }, 4000);

  // The supervised watcher's switch, badge, log drawer and command-bar chips.
  refreshWatcher().then(renderQuickTasks);
  // Local Piper voice: is it ready, and is the speaker switch on?
  refreshTts();
  // Microphone / Whisper: ready, and what is it doing right now?
  refreshStt();
  // Hunger / saturation / tier, hydrated over HTTP from `GET /survival` and the
  // `GET /foods` catalog. Must not hold up the first paint if the API is slow.
  setupSurvival().catch((err) => console.error("[avatar] survival widget:", err));
  // Stage / growth / evolution branches over `GET /digimon/status`. Same rule as
  // survival: a slow API must not hold up the first paint.
  setupDigivolve().catch((err) => console.error("[avatar] digivolve widget:", err));
  // Settings: the modal over `GET/POST /config`. A save can change a path, an
  // AI endpoint or the survival rates, so the panel's own badges are refreshed
  // from the server rather than trusted to update themselves.
  onSettingsSaved(() => {
    refreshTts();
    refreshStt();
    refreshAiProfiles();
    refreshWatcher();
  });
  setupSettings().catch((err) => console.error("[avatar] settings:", err));
  // AI profiles + the live health dot. The probe is capped at 800 ms in Rust,
  // so a dead endpoint cannot make this interval pile up.
  refreshAiProfiles();
  setInterval(() => { refreshAiProfiles(); }, 12000);

  if (window.__avatarDebug) window.__avatarDebug.ready = true;

  setInterval(() => wireRegions(), 200);
  setInterval(() => updateStatusUI(), 250);

  wireRegions();
  render();
  requestAnimationFrame(frame);
}

installDebugSurface();
init().catch((err) => {
  console.error("[avatar] init failed:", err);
  toast("Init failed: " + (err && err.message ? err.message : err), true);
});
