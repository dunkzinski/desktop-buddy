/**
 * desktop-buddy — custom keyframe animation: authoring helpers, sampling, playback
 *
 * Depends on: state.js only (`RT` holds the loaded rig, `animation` holds the
 * runtime). It is *below* renderer/editor/bridge in the module DAG, so it never
 * imports them — the timeline UI in `editor.js` and the event bridge in
 * `bridge.js` call into this module, and `rig.js` reads `animation.offsets`
 * directly, which is what keeps the transform pass and the clip sampler from
 * importing each other.
 *
 * Data model (`rig.animations.<clip>` in model.json / rig.json):
 *
 *   { "duration_ms": 1200, "loop": false,
 *     "keyframes": [ { "t": 0.0, "bones": { "arm": { "rotation": 0, "x": 0, "y": 0, "scale": 1 } } } ] }
 *
 * Every per-bone value is a **delta with an identity default**:
 *   rotation  radians added to the bone's bind rotation
 *   x / y     px added to the bone's `pos`
 *   scale     multiplier applied to the bone's scale
 * so a clip composes with the rig (and with `anim` channels and `/pose`
 * overrides) instead of replacing it.
 *
 * Sampling is smoothstep-eased between the two surrounding keyframes; before
 * the first keyframe and after the last one the clip holds that keyframe.
 */

import { RT, animation, clamp, num, round } from "./state.js";

/** Identity delta: the pose a keyframe does not mention. */
export const IDENTITY = Object.freeze({ rotation: 0, x: 0, y: 0, scale: 1 });

const AXES = ["rotation", "x", "y", "scale"];

/* ---------- finished listeners ---------- */

const finishedHandlers = [];

/**
 * Register a callback fired when a non-looping clip reaches its end. Returns the
 * handler so a caller can keep a reference to it.
 */
export function onAnimationFinished(handler) {
  if (typeof handler === "function") finishedHandlers.push(handler);
  return handler;
}

function emitFinished(name) {
  for (const handler of finishedHandlers) {
    try {
      handler(name);
    } catch (err) {
      console.error("[avatar] animation-finished handler failed:", err);
    }
  }
}

/* ---------- clip catalogue ---------- */

/** The raw `animations` object of the loaded rig (never null). */
export function animationClips() {
  const rig = RT.model && RT.model.rig ? RT.model.rig : null;
  const clips = rig ? rig.animations : null;
  return clips && typeof clips === "object" ? clips : {};
}

/** `[{ name, duration_ms, loop, keyframes }]`, sorted by name. */
export function listAnimations() {
  return Object.entries(animationClips())
    .map(([name, clip]) => ({
      name: name,
      duration_ms: Math.max(0, num(clip.duration_ms, 0)),
      loop: !!clip.loop,
      keyframes: Array.isArray(clip.keyframes) ? clip.keyframes.length : 0,
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

export function activeClipName() {
  return animation.clip;
}

export function activeClip() {
  const clips = animationClips();
  return animation.clip && clips[animation.clip] ? clips[animation.clip] : null;
}

export function isPlaying() {
  return animation.playing;
}

/** True while the timeline is authoring a clip inside Rig Edit Mode. */
export function isAuthoring() {
  return !!RT.rigMode && !!activeClip();
}

/** Keyframe times of the active clip, ascending, 0..1. */
export function keyframeTimes() {
  const clip = activeClip();
  if (!clip || !Array.isArray(clip.keyframes)) return [];
  return clip.keyframes.map((keyframe) => clamp(num(keyframe.t, 0), 0, 1)).sort((a, b) => a - b);
}

function sortedKeyframes(clip) {
  const frames = Array.isArray(clip && clip.keyframes) ? clip.keyframes.slice() : [];
  return frames
    .filter((keyframe) => keyframe && typeof keyframe === "object")
    .map((keyframe) => ({
      t: clamp(num(keyframe.t, 0), 0, 1),
      bones: keyframe.bones && typeof keyframe.bones === "object" ? keyframe.bones : {},
    }))
    .sort((a, b) => a.t - b.t);
}

/* ---------- sampling ---------- */

const lerp = (a, b, p) => a + (b - a) * p;
const smoothstep = (p) => p * p * (3 - 2 * p);

function frameTransform(frame, boneId) {
  const value = frame.bones ? frame.bones[boneId] : null;
  if (!value || typeof value !== "object") return IDENTITY;
  return {
    rotation: num(value.rotation, 0),
    x: num(value.x, 0),
    y: num(value.y, 0),
    scale: num(value.scale, 1),
  };
}

/**
 * Interpolated deltas at normalised time `t` for the active clip (or `name`).
 *
 * Returns `{ [boneId]: { rotation, x, y, scale } }`; bones the clip never
 * mentions are absent, which makes `rig.js` fall back to the bind transform.
 */
export function sampleAnimation(t, name) {
  const clips = animationClips();
  const clip = name ? clips[name] : activeClip();
  return sampleClip(clip, t);
}

/**
 * The interpolation rules, for **any** clip object.
 *
 * Same smoothstep easing, same hold-before-first / hold-after-last, same node
 * omissions — `entities.js` uses this for a prop's own mini-rig clips, so a rod's
 * `cast` eases exactly like the avatar's `wave` (one implementation, not two).
 */
export function sampleClip(clip, t) {
  if (!clip) return {};
  const frames = sortedKeyframes(clip);
  if (!frames.length) return {};

  const at = clamp(num(t, 0), 0, 1);
  let before = frames[0];
  let after = frames[0];
  for (let index = 0; index < frames.length; index += 1) {
    if (frames[index].t <= at) {
      before = frames[index];
      after = frames[Math.min(index + 1, frames.length - 1)];
    }
  }

  const span = after.t - before.t;
  const raw = span > 1e-6 ? (at - before.t) / span : 0;
  const p = smoothstep(clamp(raw, 0, 1));

  const ids = new Set([
    ...Object.keys(before.bones || {}),
    ...Object.keys(after.bones || {}),
  ]);

  const out = {};
  for (const boneId of ids) {
    const a = frameTransform(before, boneId);
    const b = frameTransform(after, boneId);
    out[boneId] = {
      rotation: lerp(a.rotation, b.rotation, p),
      x: lerp(a.x, b.x, p),
      y: lerp(a.y, b.y, p),
      scale: lerp(a.scale, b.scale, p),
    };
  }
  return out;
}

/* ---------- composing the offsets rig.js consumes ---------- */

function compose(sampled) {
  const out = {};
  for (const [boneId, value] of Object.entries(sampled)) out[boneId] = { ...value };
  for (const [boneId, value] of Object.entries(animation.draft)) {
    const base = out[boneId] || { ...IDENTITY };
    out[boneId] = {
      rotation: num(value.rotation, base.rotation),
      x: num(value.x, base.x),
      y: num(value.y, base.y),
      scale: num(value.scale, base.scale),
    };
  }
  return out;
}

function applyOffsets() {
  animation.offsets = animation.clip ? compose(sampleAnimation(animation.t)) : {};
  return animation.offsets;
}

/* ---------- transport ---------- */

/** Select a clip and park the playhead at 0 (no playback). */
export function selectClip(name) {
  const clip = animationClips()[name];
  if (!clip) return null;
  animation.clip = name;
  animation.durationMs = Math.max(1, num(clip.duration_ms, 1200));
  animation.loop = !!clip.loop;
  animation.time = 0;
  animation.t = 0;
  animation.playing = false;
  animation.finished = false;
  animation.draft = {};
  applyOffsets();
  return clip;
}

/**
 * Start a clip from the beginning.
 *
 * `loopOverride` (optional) replaces the clip's own `loop` flag for this run —
 * that is what `POST /animate {"name": "wave", "loop": true}` and the timeline's
 * Loop checkbox use. Returns the clip, or `null` when the name is unknown.
 */
export function playAnimation(name, loopOverride) {
  const clip = selectClip(name);
  if (!clip) {
    console.warn("[avatar] no animation clip named", name);
    return null;
  }
  animation.loop = typeof loopOverride === "boolean" ? loopOverride : !!clip.loop;
  animation.playing = true;
  return clip;
}

/** Stop playback and drop every clip offset (the bind pose comes back). */
export function stopAnimation() {
  animation.clip = null;
  animation.durationMs = 0;
  animation.loop = false;
  animation.time = 0;
  animation.t = 0;
  animation.playing = false;
  animation.finished = false;
  animation.draft = {};
  animation.offsets = {};
  return null;
}

/**
 * Stop the transport but **keep the clip selected** (the editor's Play/Stop
 * toggle). The playhead returns to `0` and the clip pose is re-applied, so the
 * clip you were editing is still the clip on screen.
 */
export function stopPlayback() {
  if (!animation.clip) return null;
  animation.playing = false;
  animation.finished = false;
  animation.time = 0;
  animation.t = 0;
  animation.draft = {};
  applyOffsets();
  return animation.clip;
}

/** Preview a clip position without playing (the timeline scrubber). */
export function seekAnimation(t) {
  if (!animation.clip) return null;
  animation.playing = false;
  animation.t = clamp(num(t, 0), 0, 1);
  animation.time = animation.t * animation.durationMs;
  animation.finished = false;
  animation.draft = {};
  applyOffsets();
  return animation.t;
}

/**
 * Advance the playhead by `dt` seconds. Called from the page's frame loop.
 *
 * A non-looping clip that reaches its end stops, sets `animation.finished` and
 * fires the `onAnimationFinished` handlers — that is the "animation finished"
 * state the rest of the page reacts to.
 */
export function stepAnimation(dt) {
  if (!animation.clip || !animation.playing) return animation.t;
  animation.time += Math.max(0, dt) * 1000;

  if (animation.time >= animation.durationMs) {
    if (animation.loop) {
      animation.time %= animation.durationMs;
    } else {
      animation.playing = false;
      if (RT.rigMode) {
        // Editing: keep the same clip selected and park the playhead back at the
        // start so the timeline stays on the clip being authored.
        animation.time = 0;
        animation.t = 0;
        animation.draft = {};
        applyOffsets();
      } else {
        // Play mode: settle back into the bind pose; the clip stays selected so
        // the timeline still shows what ran.
        animation.time = animation.durationMs;
        animation.t = 1;
        animation.offsets = {};
      }
      if (!animation.finished) {
        animation.finished = true;
        emitFinished(animation.clip);
      }
      return animation.t;
    }
  }

  animation.t = clamp(animation.time / animation.durationMs, 0, 1);
  applyOffsets();
  return animation.t;
}

/* ---------- authoring ---------- */

/**
 * Write an authored delta for one bone (the canvas drag handlers call this while
 * a clip is selected in Rig Edit Mode). The change is visible immediately and
 * is committed by `captureKeyframe`.
 */
export function setAnimationBone(boneId, partial) {
  if (!boneId || !partial) return null;
  const current = animation.offsets[boneId] || { ...IDENTITY };
  const next = { ...current };
  for (const axis of AXES) {
    if (Number.isFinite(partial[axis])) next[axis] = partial[axis];
  }
  animation.draft[boneId] = next;
  animation.offsets = { ...animation.offsets, [boneId]: next };
  return next;
}

/** Forget the unsaved authored pose (keeps the clip). */
export function clearAnimationDraft() {
  animation.draft = {};
  applyOffsets();
}

/** True when the current playhead carries an unsaved authored pose. */
export function hasAnimationDraft() {
  return Object.keys(animation.draft).length > 0;
}

/**
 * Store the pose currently on screen at the playhead as a keyframe.
 *
 * Capturing is idempotent: with no authored change it re-stores the sampled
 * pose, so pressing the button twice never drifts. Returns
 * `{ t, bones }` (bone count) or `null` when no clip is selected.
 */
export function captureKeyframe() {
  const clip = activeClip();
  if (!clip) return null;
  if (!Array.isArray(clip.keyframes)) clip.keyframes = [];

  const t = round(animation.t, 4);
  const bones = {};
  for (const [boneId, value] of Object.entries(animation.offsets)) {
    bones[boneId] = {
      rotation: round(num(value.rotation, 0), 4),
      x: round(num(value.x, 0), 2),
      y: round(num(value.y, 0), 2),
      scale: round(num(value.scale, 1), 3),
    };
  }

  const existing = clip.keyframes.find((keyframe) => Math.abs(num(keyframe.t, -1) - t) < 1e-4);
  if (existing) existing.bones = bones;
  else clip.keyframes.push({ t: t, bones: bones });
  clip.keyframes.sort((a, b) => num(a.t, 0) - num(b.t, 0));

  animation.draft = {};
  animation.finished = false;
  applyOffsets();
  return { t: t, bones: Object.keys(bones).length };
}

/** Remove the keyframe sitting on the playhead, if any. */
export function deleteKeyframeAtPlayhead() {
  const clip = activeClip();
  if (!clip || !Array.isArray(clip.keyframes)) return false;
  const t = animation.t;
  const kept = clip.keyframes.filter((keyframe) => Math.abs(num(keyframe.t, -1) - t) >= 1e-4);
  if (kept.length === clip.keyframes.length) return false;
  clip.keyframes = kept;
  animation.draft = {};
  applyOffsets();
  return true;
}

/** Trim and validate a clip name typed by the user (`""` = "no name given"). */
export function normalizeClipName(value) {
  return String(value == null ? "" : value).replace(/\s+/g, " ").trim();
}

/** First free `clip N` name (1, 2, 3, …) in the current rig. */
function nextClipName(clips) {
  let index = 1;
  while (clips["clip " + index]) index += 1;
  return "clip " + index;
}

/**
 * Create an empty clip and select it. Returns the clip name.
 *
 * `name` is used verbatim when non-empty (trimmed); a blank name falls back to
 * the next free `clip 1`, `clip 2`, … key.
 */
export function addClip(name) {
  const rig = RT.model && RT.model.rig ? RT.model.rig : null;
  if (!rig) return null;
  if (!rig.animations || typeof rig.animations !== "object") rig.animations = {};

  const typed = normalizeClipName(name);
  let unique = typed || nextClipName(rig.animations);
  if (rig.animations[unique]) {
    let counter = 2;
    const base = unique;
    while (rig.animations[base + " " + counter]) counter += 1;
    unique = base + " " + counter;
  }

  rig.animations[unique] = { duration_ms: 1200, loop: false, keyframes: [] };
  selectClip(unique);
  return unique;
}

/**
 * Rename the active clip in place, keeping it selected.
 *
 * Rejects blank names and names already used by another clip. Returns
 * `{ok: true, name}` or `{ok: false, error}` so the caller can toast the reason.
 */
export function renameClip(newName, oldName) {
  const rig = RT.model && RT.model.rig ? RT.model.rig : null;
  const clips = rig && rig.animations && typeof rig.animations === "object" ? rig.animations : null;
  const from = oldName || animation.clip;
  if (!clips || !from || !clips[from]) {
    return { ok: false, error: "no clip selected" };
  }

  const name = normalizeClipName(newName);
  if (!name) return { ok: false, error: "a clip needs a name" };
  if (name === from) return { ok: true, name: from, unchanged: true };
  if (clips[name]) return { ok: false, error: "a clip named " + name + " already exists" };

  // Rebuild the object so the renamed key keeps its position in the catalogue.
  const rebuilt = {};
  for (const [key, clip] of Object.entries(clips)) {
    rebuilt[key === from ? name : key] = clip;
  }
  rig.animations = rebuilt;

  if (animation.clip === from) {
    animation.clip = name;
  }
  return { ok: true, name: name };
}

/** Delete a clip (and stop it when it is the active one). */
export function deleteClip(name) {
  const rig = RT.model && RT.model.rig ? RT.model.rig : null;
  const target = name || animation.clip;
  if (!rig || !rig.animations || !target || !rig.animations[target]) return false;
  delete rig.animations[target];
  if (animation.clip === target) stopAnimation();
  return true;
}

export function setClipDuration(ms) {
  const clip = activeClip();
  if (!clip) return null;
  const duration = Math.max(1, round(num(ms, clip.duration_ms), 2));
  clip.duration_ms = duration;
  animation.durationMs = duration;
  animation.time = animation.t * duration;
  return duration;
}

export function setClipLoop(loop) {
  const clip = activeClip();
  if (!clip) return null;
  clip.loop = !!loop;
  animation.loop = !!loop;
  return animation.loop;
}

/** Compact snapshot for the debug surface / CDP tests. */
export function animationSnapshot() {
  return {
    clip: animation.clip,
    duration_ms: animation.durationMs,
    loop: animation.loop,
    t: round(animation.t, 4),
    time_ms: round(animation.time, 1),
    playing: animation.playing,
    finished: animation.finished,
    draft: Object.keys(animation.draft).length,
    keyframes: keyframeTimes(),
    bones: Object.keys(animation.offsets).length,
  };
}
