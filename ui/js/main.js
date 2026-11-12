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
  avatar, RT, speech, PRESET_STATES,
  round, num, viewport, updateViewport,
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

import { speak, hideSpeech, updateSpeech } from "./speech.js";

import {
  setClickThrough, setupEventListeners, importFromDialog, importImageAs,
  showChooser, hideChooser, toggleCollapsed, reportState,
  applyApiStatus, refreshApiStatus,
} from "./bridge.js";

import { call } from "./ipc.js";

/* ---------- behaviour (play mode) ---------- */

function stepBlendshapes(dt) {
  const k = Math.min(1, avatar.blendSpeed * dt);
  for (const key of Object.keys(avatar.blendshapes)) {
    const target = avatar.targetBlendshapes[key] || 0;
    avatar.blendshapes[key] += (target - avatar.blendshapes[key]) * k;
  }
}

function startWalk(x, y, notify) {
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
    avatar.idleTimer += dt;
    if (avatar.idleTimer >= avatar.nextIdle && avatar.time > avatar.manualUntil) {
      wander();
      avatar.nextIdle = 2.5 + Math.random() * 3.5;
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
}

/* ---------- main loop ---------- */

let lastFrame = performance.now();

function frame(now) {
  const dt = Math.min(0.05, (now - lastFrame) / 1000);
  lastFrame = now;
  step(dt);
  render();
  reportState(now);
  requestAnimationFrame(frame);
}

/* ---------- avatar interaction detection (play mode) ---------- */

/** Two clicks closer than this (and near each other) are one double-click. */
const DOUBLE_CLICK_MS = 320;
/** Pointer travel above this turns a press into a drag, not a click. */
const DRAG_START_PX = 8;

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
 * Hit-test the avatar's bounding box (the same box that becomes a click-through
 * region) and report click / double_click / drag_end.
 *
 * Rig Edit Mode is excluded: there a press is a gizmo drag and the editor owns
 * it, so the agent would otherwise get a stream of rig-editing clicks.
 */
function attachInteractionDetection(canvas) {
  let pressOrigin = null;
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
      return;
    }
    const point = canvasPoint(canvas, event);
    pressOrigin = overAvatar(point) ? point : null;
  });

  window.addEventListener("mouseup", (event) => {
    const origin = pressOrigin;
    pressOrigin = null;
    if (!origin || RT.rigMode || event.button !== 0) return;

    const point = canvasPoint(canvas, event);
    if (Math.hypot(point.x - origin.x, point.y - origin.y) > DRAG_START_PX) {
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

/* ---------- UI wiring ---------- */

function setupUI() {
  const canvas = document.getElementById("canvas");

  document.getElementById("btn-close").addEventListener("click", () => call("close_window"));
  document.getElementById("btn-minimize").addEventListener("click", () => call("minimize_window"));
  document.getElementById("btn-clickthrough").addEventListener("click", () => setClickThrough(!avatar.clickThrough));
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

  window.addEventListener("keydown", (event) => {
    if (event.target && (event.target.tagName === "INPUT" || event.target.tagName === "SELECT")) return;
    const key = event.key.toLowerCase();
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
      case "escape": if (RT.rigMode) setRigMode(false); else toggleCollapsed(!avatar.collapsed); break;
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
