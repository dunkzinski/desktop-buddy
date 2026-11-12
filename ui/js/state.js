/**
 * char-rig — shared state, constants, view-model sync
 *
 * LEAF MODULE: this file imports nothing. Everything that used to be a
 * "forward declaration" (`export let foo = () => {}` reassigned from another
 * module) either lives here for real, or lives in the module that owns it.
 *
 * ES module namespace objects are read-only: `import * as m; m.foo = ...`
 * throws a TypeError. That is what killed the previous refactor.
 * The only injected/mutable surface left is the plain object `viewport`.
 */

export const TAU = Math.PI * 2;
export const DEG = Math.PI / 180;

/* ---------- viewport (single source of truth) ---------- */
/* Mutated in place by updateViewport(); readable everywhere as viewport.w/h. */
export const viewport = { w: 0, h: 0 };

export function updateViewport() {
  viewport.w = window.innerWidth;
  viewport.h = window.innerHeight;
  return viewport;
}

/* ---------- avatar runtime state ---------- */
export const avatar = {
  x: 0, y: 0,
  targetX: 0, targetY: 0,
  startX: 0, startY: 0,
  startTime: 0,
  mode: "idle",
  idleTimer: 0,
  nextIdle: 3,
  walkSpeed: 150,
  walkPhase: 0,
  wavePhase: 0,
  time: 0,
  expression: "neutral",
  blendshapes: { mouth_open: 0, eye_wide: 0, brow_up: 0 },
  targetBlendshapes: { mouth_open: 0, eye_wide: 0, brow_up: 0 },
  blendSpeed: 3,
  manualUntil: 0,
  revision: 0,
  // expression / life-signs
  expressionHold: 0,      // seconds remaining before reverting
  expressionRevert: "neutral",
  blinkUntil: 0,
  nextBlink: 3,
  talk: false,
  blinkCount: 0,
  clickThrough: false,
  clickThroughUserSet: false,
  clickThroughArmed: false,
  collapsed: false,
  // true only between the drag threshold being crossed and the pointer release;
  // while set, the whole viewport stays mouse-interactive (see wireRegions).
  dragging: false,
};

/* ---------- watcher (the supervised char-rig-watcher.exe) ---------- */
/**
 * Mirror of `get_watcher_status` / `GET /watcher`. Written only by `bridge.js`
 * (status events, commands and the drawer), read by the panel and the drawer.
 */
export const watcher = {
  enabled: true,
  running: false,
  pid: null,
  /** Resolved binary path ("" when the status says it is missing). */
  binary: "",
  /** Rules file the child was pointed at ("" when none was found). */
  rules: "",
  /** True once a full status (not just a log line) has been applied. */
  checked: false,
  /** Newest-last tail, capped at WATCHER_LOG_LIMIT. */
  logs: [],
  /** Chips for the command bar, from `quick_tasks` in watcher_rules.json. */
  quickTasks: [],
  /** Is the pop-out console open? Kept here so `rig.js` can add its region. */
  drawer: false,
};

/* ---------- rig runtime (model, bones, images, pose, world) ---------- */
export const RT = {
  model: null,
  bones: {},
  order: [],
  boneList: [],
  images: {},
  pose: {},
  world: {},
  rigMode: false,
  selected: null,
  drag: null,
  history: [],
  dirty: false,
};

/* ---------- custom keyframe animation runtime ---------- */
/**
 * Playback state for `rig.animations` clips, owned by `animator.js` and read by
 * `rig.js` (which folds `offsets` into `localRotation` / `computeWorld`).
 *
 * `offsets` is the *composed* view that the transform pass consumes:
 * `sampleAnimation(t)` merged with `draft`. Every value is a delta with an
 * identity default — rotation/x/y are added to the bone's bind transform, scale
 * is multiplied into it — so a clip composes with the rig instead of replacing
 * it.
 */
export const animation = {
  clip: null,       // selected / playing clip name, or null
  durationMs: 0,    // clip length in ms
  loop: false,      // effective loop flag for the current run
  time: 0,          // playhead in ms
  t: 0,             // normalised playhead 0..1
  playing: false,
  finished: false,  // set when a non-looping clip reaches its end
  offsets: {},      // boneId -> { rotation, x, y, scale } consumed by rig.js
  draft: {},        // boneId -> authored override (unsaved keyframe pose)
};


/* ---------- expression data model ---------- */
export const EXPRESSION_PRESETS = ["neutral", "happy", "surprised", "thinking", "sad", "angry"];

/** Procedural fallback: drives shape parts and mouths/eyes without variants. */
export const EXPRESSIONS = {
  neutral:   { mouth_open: 0,    eye_wide: 0,    brow_up: 0,    time: 0.3 },
  happy:     { mouth_open: 0.55, eye_wide: 0.15, brow_up: 0.35, time: 0.3 },
  surprised: { mouth_open: 0.9,  eye_wide: 0.85, brow_up: 0.85, time: 0.2 },
  thinking:  { mouth_open: 0.1,  eye_wide: 0,    brow_up: 0.2,  time: 0.35 },
  sad:       { mouth_open: 0.15, eye_wide: 0,    brow_up: 0.6,  time: 0.5 },
  angry:     { mouth_open: 0.25, eye_wide: 0.2,  brow_up: 0.75, time: 0.3 },
};

/**
 * Which sprite variant each expression should show, per semantic part tag.
 * Keys are the variant names authors pick when binding sprites.
 */
export const PRESET_STATES = {
  eye:   { neutral: "open", happy: "happy", surprised: "wide", thinking: "look_up", sad: "sad", angry: "angry" },
  mouth: { neutral: "closed", happy: "smile", surprised: "open", thinking: "flat", sad: "frown", angry: "frown" },
};

export const PART_TAGS = ["none", "head", "torso", "arm_left", "arm_right", "leg_left",
  "leg_right", "eye", "mouth", "accessory", "custom"];

/* ---------- speech bubble state ---------- */
export const speech = {
  visible: false,
  full: "",
  shown: 0,
  elapsed: 0,
  charMs: 26,
  holdMs: 6000,
  doneFor: 0,
  flipped: false,
  lastLayout: 0,
  /** True while the bubble is a transient "Thinking…" status card. */
  thinking: false,
  /** True while the pointer is over the bubble (the hold timer is paused). */
  hovered: false,
};

/* ---------- helpers ---------- */
export const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
export const round = (v, n) => Math.round(v * Math.pow(10, n)) / Math.pow(10, n);
export const num = (v, fallback) => (typeof v === "number" && Number.isFinite(v) ? v : fallback);

export function getPath(obj, path) {
  return path.split(".").reduce((acc, key) => (acc == null ? acc : acc[key]), obj);
}

export function setPath(obj, path, value) {
  const keys = path.split(".");
  let cursor = obj;
  for (let i = 0; i < keys.length - 1; i += 1) {
    if (cursor[keys[i]] == null || typeof cursor[keys[i]] !== "object") cursor[keys[i]] = {};
    cursor = cursor[keys[i]];
  }
  cursor[keys[keys.length - 1]] = value;
}

/* ---------- sprite variant helpers (shared by renderer + debug surface) ---------- */

export function hasVariant(part, key) {
  return !!(part.variants && typeof part.variants[key] === "string");
}

/** Which sprite state should this part show right now? */
export function activeState(part) {
  if (part.tag === "eye" && blinkActive()) {
    return hasVariant(part, "blink") ? "blink" : "__blink__";
  }
  if (part.tag === "mouth" && avatar.talk) {
    const want = Math.sin(avatar.time * 26) > 0 ? "open" : "closed";
    return hasVariant(part, want) ? want : "__talk__";
  }
  if (part.expressionState && hasVariant(part, part.expressionState)) return part.expressionState;
  if (part.state && hasVariant(part, part.state)) return part.state;
  return null;
}

export function blinkActive() {
  return avatar.time < avatar.blinkUntil;
}

export function markManual(seconds) {
  avatar.manualUntil = avatar.time + (seconds || 12);
}

/* ---------- toast (DOM only, so it belongs to the leaf) ---------- */

let toastTimer = 0;

export function toast(message, isError) {
  const el = document.getElementById("toast");
  if (!el) {
    console.log("[toast]", message);
    return;
  }
  el.textContent = message;
  el.classList.toggle("error", !!isError);
  el.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove("show"), 2400);
}

/* ---------- status panel sync ---------- */

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

/* ---------- expressions ---------- */

/**
 * Set the facial expression. `options.durationMs` reverts to `options.revert`
 * (default neutral) after the hold expires.
 */
export function setExpression(name, options) {
  const preset = EXPRESSIONS[name];
  if (!preset) {
    console.warn("[avatar] unknown expression:", name);
    return;
  }
  avatar.expression = name;
  avatar.targetBlendshapes = {
    mouth_open: preset.mouth_open,
    eye_wide: preset.eye_wide,
    brow_up: preset.brow_up,
  };
  avatar.blendSpeed = 1 / Math.max(0.05, preset.time);

  // drive sprite variants of semantic parts when the rig has them
  for (const bone of RT.boneList) {
    for (const part of bone.parts) {
      const map = PRESET_STATES[part.tag];
      if (!map) continue;
      const wanted = map[name];
      if (wanted && hasVariant(part, wanted)) part.expressionState = wanted;
      else part.expressionState = undefined;
    }
  }

  const holdMs = options && Number.isFinite(options.durationMs) ? options.durationMs : 0;
  avatar.expressionHold = holdMs > 0 ? holdMs / 1000 : 0;
  avatar.expressionRevert = (options && options.revert) || "neutral";
  updateStatusUI();
}

/* ---------- undo history ---------- */

export function snapshot() {
  if (!RT.model) return;
  const plain = RT.boneList.map((bone) => {
    const copy = {};
    for (const key of Object.keys(bone)) if (key !== "children") copy[key] = bone[key];
    return copy;
  });
  RT.history.push(JSON.stringify(plain));
  if (RT.history.length > 80) RT.history.shift();
  setDirty(true);
}

/* ---------- dirty flag ---------- */

export function setDirty(value) {
  RT.dirty = value;
  const el = document.getElementById("rig-state");
  if (!el) return;
  el.textContent = value ? "unsaved" : "saved";
  el.classList.toggle("dirty", value);
}
