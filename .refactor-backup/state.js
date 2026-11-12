/**
 * char-rig — shared state & constants
 * Dependency-free module: other modules import from this.
 */

export const TAU = Math.PI * 2;
export const DEG = Math.PI / 180;

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

/* ---------- expression data model ---------- */
export const EXPRESSION_PRESETS = ["neutral", "happy", "surprised", "thinking", "sad", "angry"];

/** Procedural fallback: drives shape parts and mouths/eyes without variants. */
export const EXPRESSIONS = {
  neutral:   { mouth_open: 0,   eye_wide: 0,   brow_up: 0,   time: 0.3 },
  happy:     { mouth_open: 0.55, eye_wide: 0.15, brow_up: 0.35, time: 0.3 },
  surprised: { mouth_open: 0.9,  eye_wide: 0.85, brow_up: 0.85, time: 0.2 },
  thinking:  { mouth_open: 0.1,  eye_wide: 0,   brow_up: 0.2,  time: 0.35 },
  sad:       { mouth_open: 0.15, eye_wide: 0,   brow_up: 0.6,  time: 0.5 },
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

/* ---------- undo history helpers ---------- */
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

export function undo() {
  const previous = RT.history.pop();
  if (!previous) { toast("Nothing to undo"); return; }
  RT.model.rig.bones = JSON.parse(previous);
  const keep = RT.selected ? RT.selected.bone : null;
  rebuildOrder();
  RT.selected = keep && RT.bones[keep] ? { bone: keep, part: null } : null;
  renderInspector();
  setDirty(RT.history.length > 0);
}

/* ---------- dirty flag ---------- */
export function setDirty(value) {
  RT.dirty = value;
  const el = document.getElementById("rig-state");
  if (!el) return;
  el.textContent = value ? "unsaved" : "saved";
  el.classList.toggle("dirty", value);
}

/* ---------- forward declarations for cross-module calls ---------- */
/* These will be assigned by the modules that define them */
export let rebuildOrder = () => {};
export let renderInspector = () => {};
export let toast = (msg, err) => console.log("[toast]", msg, err);
export let wireRegions = () => {};