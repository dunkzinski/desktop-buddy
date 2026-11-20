/**
 * desktop-buddy — shared state, constants, view-model sync
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
  // Seven channels, all 0..1 except the two signed ones. `brow_angle` and
  // `mouth_curve` are signed: positive = inner brow down / mouth corners up.
  blendshapes: {
    mouth_open: 0, mouth_wide: 0, mouth_curve: 0,
    eye_wide: 0, eye_squint: 0,
    brow_up: 0, brow_angle: 0,
  },
  targetBlendshapes: {
    mouth_open: 0, mouth_wide: 0, mouth_curve: 0,
    eye_wide: 0, eye_squint: 0,
    brow_up: 0, brow_angle: 0,
  },
  blendSpeed: 3,
  manualUntil: 0,
  revision: 0,
  // expression / life-signs
  expressionHold: 0,      // seconds remaining before reverting
  expressionRevert: "neutral",
  // `avatar.time` of the last expression change. A face applied by the AI or by
  // program logic that carries no explicit hold eases back to neutral after
  // EXPRESSION_SETTLE_SECS once the moment is over (see settleExpression).
  expressionSetAt: 0,
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
  // `POST /perch` planted the avatar on a window border: idle wandering pauses
  // and the gaze rests on `perchRect` (window bounds, CSS px) instead of
  // glancing around. Cleared by a drag, W/centre, or any move.
  perched: false,
  perchRect: null,
  // Explicit gaze target from `POST /move {"look_at": [x, y]}`, screen px, or
  // null for "decide for yourself".
  lookAt: null,
};

/* ---------- watcher (the supervised desktop-buddy-watcher.exe) ---------- */
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

/* ---------- AI profiles (which brain answers a trigger) ---------- */
/**
 * Mirror of `get_ai_profiles` / `GET /ai-profiles`: the active profile, every
 * configured one, and the active profile's live health. Written only by
 * `bridge.js`, read by the control panel's **AI Brain** row.
 */
export const ai = {
  /** Active profile name ("" until the first status arrives). */
  active: "",
  /** `[{ name, label, kind, model, endpoint? }]`, in rules-file order. */
  profiles: [],
  /** `{ reachable, status: "online"|"offline"|"cli", detail }`. */
  health: { reachable: false, status: "offline", detail: "" },
  /** True once a real status has been applied (a missing file shows "no profiles"). */
  checked: false,
};

/* ---------- local voice (Piper TTS) ---------- */
/**
 * Mirror of `get_tts_status` / `GET /tts`: the control panel's **Voice** row.
 * `ready` is false when Piper or the `.onnx` model is missing on disk; `reason`
 * then explains why the avatar is silent.
 */
export const tts = {
  enabled: true,
  ready: false,
  reason: "",
  /** Voice model file name, for the badge tooltip. */
  model: "",
  checked: false,
};

/* ---------- push-to-talk voice input (Whisper STT) ---------- */
/**
 * Mirror of `get_stt_status` / `GET /stt`: the panel's **Push to talk** row.
 * `state` is the live worker state (`idle` / `listening` / `transcribing`);
 * `ready` is false when whisper-cli, a model or the microphone is missing, and
 * `reason` then says which.
 */
export const stt = {
  enabled: true,
  ready: false,
  reason: "",
  state: "idle",
  /** Last cleaned transcript (shown in the badge tooltip). */
  transcript: "",
  hotkey: "",
  checked: false,
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
  // `brow_angle` and `mouth_curve` are signed: + = inner brow down / corners up.
  neutral:   { mouth_open: 0,    mouth_wide: 0,    mouth_curve: 0,    eye_wide: 0,    eye_squint: 0,    brow_up: 0,     brow_angle: 0,     time: 0.3 },
  happy:     { mouth_open: 0.45, mouth_wide: 0.35, mouth_curve: 0.9,  eye_wide: 0.1,  eye_squint: 0.35, brow_up: 0.3,   brow_angle: -0.15, time: 0.3 },
  surprised: { mouth_open: 0.95, mouth_wide: 0.1,  mouth_curve: 0,    eye_wide: 0.9,  eye_squint: 0,    brow_up: 0.9,   brow_angle: -0.1,  time: 0.2 },
  thinking:  { mouth_open: 0.08, mouth_wide: 0,    mouth_curve: -0.2, eye_wide: 0,    eye_squint: 0.3,  brow_up: 0.15,  brow_angle: 0.45,  time: 0.35 },
  sad:       { mouth_open: 0.12, mouth_wide: 0,    mouth_curve: -0.85, eye_wide: 0,   eye_squint: 0.45, brow_up: 0.05,  brow_angle: -0.85, time: 0.5 },
  angry:     { mouth_open: 0.3,  mouth_wide: 0.2,  mouth_curve: -0.6, eye_wide: 0.05, eye_squint: 0.55, brow_up: -0.15, brow_angle: 0.9,   time: 0.3 },
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

/* ---------- gaze / eye tracking ---------- */

/**
 * Where the eyes are looking, in screen (CSS) px.
 *
 * Written by `main.js` (pointer, command bar, idle saccades — the things that
 * know about the DOM) and read by `renderer.js`, which turns it into a clamped
 * pupil offset. `hover` is the eased 0..1 "cursor is on me" weight that `rig.js`
 * folds into a small head tilt. It lives here because both sides need it and
 * neither may import the other (module DAG).
 */
export const gaze = {
  /** Smoothed aim point (screen px) — the eyes follow this. */
  x: 0, y: 0,
  /** The raw aim point for this frame (pointer / command bar / glance). */
  targetX: 0, targetY: 0,
  /** True while a real target (pointer or a focused input) drives the gaze. */
  tracking: false,
  /** False until the first `updateGaze` snaps the smoothed point into place. */
  ready: false,
  /** Last pointer position + when it last moved (pointer tracking decays). */
  pointerX: 0, pointerY: 0, lastMove: -100,
  /** Idle glances: when the next one starts, and where it lands. */
  nextGlance: 0, glanceX: 0, glanceY: 0,
  /** Eased hover weight 0..1 (cursor inside the avatar's bounds). */
  hover: 0,
  hoverTarget: 0,
  /**
   * The **global** OS cursor, from Rust's `cursor-pos` event (CSS px in the
   * virtual-desktop space) — the only cursor signal that exists while the
   * overlay is click-through and unfocused. `cursorAt` is the `avatar.time` of
   * the last event, so the gaze can fall back to idle glances once the cursor
   * has sat still for a few seconds.
   */
  cursorX: 0, cursorY: 0, cursorAt: -100,
  /** True while the gaze is being driven by the global cursor. */
  cursorTracking: false,
  /**
   * Eased head-bone yaw bias in radians, folded into the head's rotation by
   * `rig.js::animOffset`. `tiltTarget` is what `updateGaze` wants; the eased
   * value is what is drawn, so the head turns instead of snapping.
   */
  tilt: 0, tiltTarget: 0,
};

/* ---------- conversation history (the History drawer) ---------- */
/**
 * Mirror of the Rust-side in-memory chat log (`get_chat_history` /
 * `GET /history`), plus the drawer's own view state.
 *
 * The Rust side owns the truth: the drawer hydrates once on load, then appends
 * from the `chat-message` event and empties on `chat-history-cleared`. Nothing
 * here is persisted — a restart starts a new, empty chat.
 */
export const chat = {
  /** `[{ id, role: "user"|"assistant", text, details?, expression?, ts_ms }]`, oldest first. */
  messages: [],
  /** Is the drawer on screen? Also decides whether its box is a hit region. */
  open: false,
  /** True once the hydrate call has answered (so an empty list can say so). */
  hydrated: false,
  /** Ids whose `details` block is expanded. */
  expanded: {},
};

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
  /** The long half of the bubble (code/diff/log), shown in a drawer. */
  details: "",
  /** Is the details drawer expanded? An open drawer holds the bubble up. */
  detailsOpen: false,
  /** True while the bubble is a transient "Thinking…" status card. */
  thinking: false,
  /** True while the pointer is over the bubble (the hold timer is paused). */
  hovered: false,
  /* ----- text-driven lip sync (see speech.js) ----- */
  /** Last character revealed by the typewriter ("" when idle). */
  char: "",
  /** Mouth openness the current character asked for, 0..1. */
  mouthOpen: 0,
  /** Eased openness the renderer actually draws (0..1). */
  mouthSmooth: 0,
  /** Sprite variant that matches `mouthOpen`: `open` | `flat` | `closed`. */
  mouthVariant: "closed",
  /**
   * Live Piper RMS envelope (`tts-envelope`): normalized 0..1 samples plus the
   * `performance.now()` timestamp they started at. While this is set it owns
   * `mouthOpen` — the typewriter mapping only takes over when the voice is
   * muted, missing, or the line has finished.
   */
  envelope: null,
  envelopeBucketMs: 25,
  envelopeAt: 0,
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

/**
 * Which mouth sprite matches the lip-sync variant the typewriter asked for?
 *
 * `speech.js` maps each revealed character to `open` (vowels), `flat` (soft
 * consonants) or `closed` (spaces / punctuation) and the renderer squashes the
 * *shape* with the same openness, so a rig without variants still reads as
 * talking. A rig with no `flat` sprite shows its open mouth for the mid-open
 * consonants instead of snapping shut.
 */
function mouthState(part) {
  const want = speech.mouthVariant;
  if (want && hasVariant(part, want)) return want;
  if (want === "flat" && hasVariant(part, "open")) return "open";
  return "__talk__";
}

/** Which sprite state should this part show right now? */
export function activeState(part) {
  if (part.tag === "eye" && blinkActive()) {
    return hasVariant(part, "blink") ? "blink" : "__blink__";
  }
  if (part.tag === "mouth" && avatar.talk) {
    return mouthState(part);
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
  avatar.expressionSetAt = avatar.time;
  // Copy every channel the preset declares (all but `time`), so adding a
  // blendshape to EXPRESSIONS needs no change here.
  const target = {};
  for (const [key, value] of Object.entries(preset)) {
    if (key !== "time" && Number.isFinite(value)) target[key] = value;
  }
  avatar.targetBlendshapes = target;
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

/**
 * How long a program- or AI-chosen face lingers when nothing gave it an explicit
 * hold, before it eases back to neutral. Long enough to read as a reaction (and
 * to survive a slow model turn), short enough that the avatar does not sit
 * frozen in one mood for minutes.
 */
export const EXPRESSION_SETTLE_SECS = 6;

/**
 * Wear `name` for a moment: program- and event-driven reactions go through here
 * so every one of them reverts to neutral instead of leaving a face stuck on.
 * It reuses `setExpression`'s hold machinery, so a `POST /emote` with its own
 * duration (or a `speak` carrying an expression) simply replaces this one.
 */
export function reactExpression(name, seconds) {
  if (!EXPRESSIONS[name]) return false;
  const hold = Number.isFinite(seconds) ? seconds : EXPRESSION_SETTLE_SECS;
  setExpression(name, { durationMs: Math.max(1, hold) * 1000, revert: "neutral" });
  return true;
}

/**
 * Ease an AI/program face back to neutral once the moment is actually over.
 *
 * Called every frame. The guards are what keep it from stealing a face the user
 * is meant to see: an explicit hold wins (it reverts on its own schedule), a
 * bubble or the "Thinking…" card *is* the moment, a walk/wave is still running,
 * and push-to-talk is still listening or transcribing. It only ever calls
 * `setExpression`, the same local path the `/emote` event uses, so `revision` /
 * `expression_commanded_at` / `report_state` semantics are untouched.
 */
export function settleExpression() {
  if (avatar.expression === "neutral") return false;
  if (avatar.expressionHold > 0) return false;
  if (speech.visible) return false;
  if (avatar.mode !== "idle") return false;
  if (stt.state !== "idle") return false;
  if (avatar.time - avatar.expressionSetAt < EXPRESSION_SETTLE_SECS) return false;
  setExpression("neutral");
  return true;
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
