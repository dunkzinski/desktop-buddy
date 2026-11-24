/**
 * desktop-buddy — procedural gestures
 *
 * A gesture is a **shape of bone offsets over time**, not a clip. It exists for the
 * things a rig can do without any animation data at all, which is most of the
 * small business of having hands: putting a rod away, reaching down for something,
 * and whatever the mining tools want next.
 *
 * Why not clips? A clip is per-rig authoring; a gesture is one table of bone
 * *roles* (`arm`, `torso`, `head`, `root`) that every rig answers, including the
 * generated ones. That is the same trick `rig.js::rummageOffset` plays for
 * scavenging, lifted into a named thing so more than one caller can ask for one.
 *
 * Layering: `rig.js` adds these offsets *on top of* the bone's own pose and its
 * life-signs, so a rig that sways still sways while it packs up. Limits are applied
 * afterwards by `localRotation`, so a rig that declares a bone range cannot be
 * puppeted past it.
 *
 * Nothing here imports anything: this module is pure data plus maths, so `rig.js`
 * can sit above it in the module DAG without a cycle.
 */

/** Bones on the avatar's left half mirror the right; `/left/` covers old rigs. */
function sideOf(boneId) {
  return /(^|_)l$|left/.test(boneId) ? -1 : 1;
}

/** 0..1 ramp with flat ends, so a gesture starts and stops without a snap. */
function envelope(t) {
  const clamped = Math.min(1, Math.max(0, t));
  return clamped * clamped * (3 - 2 * clamped);
}

/**
 * The gestures themselves.
 *
 * Each `pose(bone, t, blend)` returns `{ rot, bob }` for one bone at progress `t`
 * (0..1) and strength `blend` (0..1). Both must go to zero at `t = 0` and `t = 1`:
 * a gesture that ends anywhere other than neutral leaves the avatar posed.
 */
export const GESTURES = {
  /**
   * Putting something away: the working arm drops, he leans back off the work and
   * looks at what he is holding, holds it a beat, then lets his arm come back as
   * the prop leaves his hand. 2 s — it is timed against the host's `LINGER_MS`, and
   * the two are meant to end together.
   */
  "pack-up": {
    duration: 2000,
    pose(bone, t) {
      // Down by 30%, held to 80%, back to neutral in the last fifth.
      const up = t < 0.3 ? envelope(t / 0.3) : t > 0.8 ? envelope((1 - t) / 0.2) : 1;
      // A slow settle so the held beat breathes instead of freezing.
      const breathe = Math.sin(Math.min(1, t) * Math.PI * 2) * 0.03;
      const id = String(bone.id || "").toLowerCase();
      const side = sideOf(id);
      // The working arm carries the prop; the other one only comes along a little.
      const weight = side > 0 ? 1 : 0.25;

      if (id === "root" || id === "hips") {
        // Settle onto the back foot as the arm comes down.
        return { rot: 0, bob: -3.5 * up };
      }
      if (id === "torso" || id === "chest" || id === "body") {
        return { rot: -0.12 * up, bob: 0 };
      }
      if (/^head/.test(id)) {
        // Look at the thing in his hand.
        return { rot: 0.16 * up, bob: 0 };
      }
      if (/^forearm/.test(id)) {
        return { rot: side * (0.22 + breathe) * up * weight, bob: 0 };
      }
      if (/^arm/.test(id)) {
        return { rot: side * (0.5 + breathe) * up * weight, bob: 0 };
      }
      if (/^hand/.test(id)) {
        return { rot: side * 0.1 * up * weight, bob: 0 };
      }
      return { rot: 0, bob: 0 };
    },
  },

  /**
   * Reaching down for something on the floor and straightening up with it. The
   * fishing rod uses it when he arrives at it; the mining tools are the reason it
   * is generic — a pickaxe, a lantern and a helmet all want the same reach.
   */
  reach: {
    duration: 900,
    pose(bone, t) {
      // Down fast, a short grab at the bottom, up again.
      const down = t < 0.45 ? envelope(t / 0.45) : t > 0.72 ? envelope((1 - t) / 0.28) : 1;
      const id = String(bone.id || "").toLowerCase();
      const side = sideOf(id);
      const weight = side > 0 ? 1 : 0.3;

      if (id === "root" || id === "hips") {
        // The whole body goes down: a 2D rig has no depth to lean along.
        return { rot: 0, bob: -7 * down };
      }
      if (id === "torso" || id === "chest" || id === "body") {
        return { rot: 0.26 * down, bob: 0 };
      }
      if (/^head/.test(id)) {
        return { rot: 0.22 * down, bob: 0 };
      }
      if (/^forearm/.test(id)) {
        return { rot: side * 0.3 * down * weight, bob: 0 };
      }
      if (/^arm/.test(id)) {
        return { rot: side * 0.55 * down * weight, bob: 0 };
      }
      if (/^hand/.test(id)) {
        return { rot: side * 0.14 * down * weight, bob: 0 };
      }
      return { rot: 0, bob: 0 };
    },
  },
};

/** Gesture names, for callers that want to validate before they animate. */
export const GESTURE_NAMES = Object.keys(GESTURES);

/** How long `name` runs for, in seconds. */
export function gestureDuration(name) {
  const gesture = GESTURES[name];
  return gesture ? gesture.duration / 1000 : 0;
}

/**
 * The offset this gesture puts on one bone.
 *
 * A missing or finished gesture is `{ rot: 0, bob: 0 }` — never `null`, so callers
 * can add it without a check.
 */
export function gestureOffset(name, bone, t, blend = 1) {
  const gesture = GESTURES[name];
  if (!gesture || !bone || !(blend > 0)) return { rot: 0, bob: 0 };
  const progress = Math.min(1, Math.max(0, Number(t) || 0));
  const offset = gesture.pose(bone, progress, blend);
  return {
    rot: (offset && Number.isFinite(offset.rot) ? offset.rot : 0) * blend,
    bob: (offset && Number.isFinite(offset.bob) ? offset.bob : 0) * blend,
  };
}
