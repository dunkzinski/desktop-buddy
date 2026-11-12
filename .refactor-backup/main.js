/**
 * char-rig — entry point
 * Initializes DOM bindings, keyboard shortcuts, window.__avatarDebug,
 * and the requestAnimationFrame main loop.
 * Depends on: state.js, rig.js, renderer.js, editor.js, speech.js, bridge.js
 */

import {
  avatar, RT,
  setExpression, markManual,
} from "./state.js";

import { applyModel, refreshModelList, call as rigCall } from "./rig.js";
import { initRenderer } from "./renderer.js";
import { buildInspector, setRigMode, addChildBone, deleteBone, addPartFromControls, addLimbFromControls, duplicatePart, deletePart, saveRig, resetRig, select, onMouseDown, onMouseMove, onMouseUp, onWheel } from "./editor.js";
import { speak, hideSpeech, updateSpeech, positionBubble } from "./speech.js";
import { call, wireRegions, setClickThrough, setupEventListeners, importFromDialog, importImageAs, showChooser, hideChooser, toggleCollapsed, updateStatusUI, applyRemoteState, applyPartPatch } from "./bridge.js";

/* ---------- patch cross-module forward declarations ---------- */

// state.js -> rig.js
import * as stateModule from "./state.js";
stateModule.rebuildOrder = () => import("./rig.js").then(m => m.rebuildOrder());
stateModule.renderInspector = () => import("./editor.js").then(m => m.renderInspector());
stateModule.toast = (msg, err) => console.log("[toast]", msg, err);
stateModule.wireRegions = wireRegions;

// state.js -> editor.js
stateModule.sortedDrawables = () => import("./rig.js").then(m => m.sortedDrawables());
stateModule.clampRotation = (bone, value) => import("./rig.js").then(m => m.clampRotation(bone, value));

// rig.js -> editor.js
import * as rigModule from "./rig.js";
rigModule.renderInspector = () => import("./editor.js").then(m => m.renderInspector());
rigModule.wireRegions = wireRegions;
rigModule.toast = (msg, err) => console.log("[toast]", msg, err);
rigModule.call = call;

// editor.js -> bridge.js
import * as editorModule from "./editor.js";
editorModule.canvas = document.getElementById("canvas");
editorModule.vw = 0;
editorModule.vh = 0;
editorModule.wireRegions = wireRegions;
editorModule.call = call;
editorModule.setExpression = setExpression;
editorModule.sortedDrawables = () => import("./rig.js").then(m => m.sortedDrawables());
editorModule.clampRotation = (bone, value) => import("./rig.js").then(m => m.clampRotation(bone, value));

// speech.js -> bridge.js
import * as speechModule from "./speech.js";
speechModule.vw = 0;
speechModule.vh = 0;
speechModule.updateStatusUI = updateStatusUI;

// bridge.js -> other modules
import * as bridgeModule from "./bridge.js";
bridgeModule.vw = 0;
bridgeModule.vh = 0;
bridgeModule.canvas = document.getElementById("canvas");
bridgeModule.renderInspector = () => import("./editor.js").then(m => m.renderInspector());
bridgeModule.updateInspectorValues = () => import("./editor.js").then(m => m.updateInspectorValues());
bridgeModule.setExpression = setExpression;
bridgeModule.rebuildAddImageOptions = () => import("./rig.js").then(m => m.rebuildAddImageOptions());
bridgeModule.sortedDrawables = () => import("./rig.js").then(m => m.sortedDrawables());
bridgeModule.computeWorld = () => import("./rig.js").then(m => m.computeWorld());

/* ---------- behaviour (play mode) ---------- */

function hasVariant(part, key) {
  return !!(part.variants && typeof part.variants[key] === "string");
}

function activeState(part) {
  if (part.tag === "eye" && avatar.time < avatar.blinkUntil) {
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

function blinkActive() {
  return avatar.time < avatar.blinkUntil;
}

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
  startWalk(margin + Math.random() * Math.max(1, vw - margin * 2),
    margin + Math.random() * Math.max(1, vh - margin * 2));
}

function step(dt) {
  avatar.time += dt;
  stepBlendshapes(dt);
  updateSpeech(dt);
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
  import("./renderer.js").then(m => m.render());
  reportState(now);
  requestAnimationFrame(frame);
}

function reportState(now) {
  if (!window.__TAURI__?.core?.invoke) return;
  if (now - lastReport < 120) return;
  lastReport = now;
  call("report_state", {
    position: [avatar.x, avatar.y],
    target_position: [avatar.targetX, avatar.targetY],
    state: avatar.mode,
    expression: avatar.expression,
    joints: jointAngles(),
    talking: avatar.talk,
    speech: speech.visible,
  });
}

function jointAngles() {
  const out = {};
  for (const id of RT.order) {
    const bone = RT.bones[id];
    if (bone) out[id] = round(localRotation(bone), 4);
  }
  return out;
}

function localRotation(bone) {
  const override = RT.pose[bone.id];
  const base = num(bone.rotation, 0) + animOffset(bone).rot;
  const value = override && Number.isFinite(override.rotation) ? override.rotation : base;
  return clampRotation(bone, value);
}

function animOffset(bone) {
  if (RT.rigMode) return { rot: 0, bob: 0 };
  const anim = bone.anim || {};
  const phase = num(anim.phase, 0);
  let rot = 0;
  if (anim.sway) rot += Math.sin(avatar.time * 1.3 + phase) * anim.sway;
  if (anim.walk && avatar.mode === "walking") {
    rot += Math.sin(avatar.walkPhase * Math.PI * 2 + phase) * anim.walk;
  }
  if (anim.wave && avatar.mode === "waving") {
    rot += Math.sin(avatar.wavePhase + phase) * anim.wave;
  }
  const bob = anim.bob ? Math.sin(avatar.time * 1.8 + phase) * anim.bob : 0;
  return { rot, bob };
}

function clampRotation(bone, value) {
  const limits = bone && bone.limits ? bone.limits : {};
  const min = Number.isFinite(limits.min) ? limits.min : -Infinity;
  const max = Number.isFinite(limits.max) ? limits.max : Infinity;
  return Math.min(max, Math.max(min, value));
}

let lastReport = 0;

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
  document.getElementById("btn-center").addEventListener("click", () => startWalk(vw / 2, vh / 2));

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
    if (model) {
      applyModel(model);
      toast("Loaded model " + model.name);
    }
  });

  document.getElementById("btn-import").addEventListener("click", importFromDialog);
  document.getElementById("chooser-model").addEventListener("click", () => importImageAs("model"));
  document.getElementById("chooser-part").addEventListener("click", () => importImageAs("part"));
  document.getElementById("chooser-cancel").addEventListener("click", hideChooser);

  document.getElementById("btn-save-rig").addEventListener("click", saveRig);
  document.getElementById("btn-reset-rig").addEventListener("click", resetRig);
  document.getElementById("btn-undo").addEventListener("click", () => import("./state.js").then(m => m.undo()));
  document.getElementById("btn-add-bone").addEventListener("click", addChildBone);
  document.getElementById("btn-del-bone").addEventListener("click", deleteBone);

  canvas.addEventListener("mousedown", onMouseDown);
  window.addEventListener("mousemove", onMouseMove);
  window.addEventListener("mouseup", onMouseUp);
  canvas.addEventListener("wheel", onWheel, { passive: false });
  canvas.addEventListener("contextmenu", (event) => {
    if (RT.rigMode) event.preventDefault();
  });

  window.addEventListener("keydown", (event) => {
    if (event.target && (event.target.tagName === "INPUT" || event.target.tagName === "SELECT")) return;
    const key = event.key.toLowerCase();
    if ((event.ctrlKey || event.metaKey) && key === "z") {
      event.preventDefault();
      import("./state.js").then(m => m.undo());
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
      case "c": startWalk(vw / 2, vh / 2); break;
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

/* ---------- init ---------- */

async function init() {
  const canvas = document.getElementById("canvas");
  initRenderer(canvas);

  // Sync viewport dimensions across modules
  const updateViewport = () => {
    vw = window.innerWidth;
    vh = window.innerHeight;
    // Update all modules
    import("./state.js").then(m => { m.vw = vw; m.vh = vh; });
    import("./rig.js").then(m => { m.vw = vw; m.vh = vh; });
    import("./renderer.js").then(m => { m.vw = vw; m.vh = vh; });
    import("./editor.js").then(m => { m.vw = vw; m.vh = vh; });
    import("./speech.js").then(m => { m.vw = vw; m.vh = vh; });
    import("./bridge.js").then(m => { m.vw = vw; m.vh = vh; });
  };
  updateViewport();
  window.addEventListener("resize", () => {
    updateViewport();
    wireRegions();
  });

  setExpression("neutral");
  setupUI();
  buildInspector();
  updateStatusUI();

  await setupEventListeners();

  await refreshModelList();
  const model = await call("get_model");
  if (model) applyModel(model);
  const info = await call("get_runtime_info");
  if (info && info.api_url) document.getElementById("api-url").textContent = info.api_url;

  // automation / debugging surface (also used by the CDP test scripts)
  window.__avatarDebug = {
    setExpression: setExpression,
    speak: speak,
    hideSpeech: hideSpeech,
    selectBone: (id, partId) => select(id, partId || null),
    /**
     * Bind `path` as the sprite for `preset` on the first part with `tag`
     * (same code path as the editor's Bind PNG button, minus the file dialog).
     */
    bindExpression: async (path, tag, preset) => {
      const part = RT.boneList.reduce((found, bone) => found || bone.parts.find((entry) => entry.tag === tag), null);
      const map = (await import("./state.js")).PRESET_STATES[tag];
      if (!part || !map) return "no matching part or tag";
      const key = map[preset];
      if (!key) return "unknown preset";
      const outcome = await call("import_asset", { path: path, mode: "part" });
      if (!outcome || !outcome.staged) return "staging failed";
      (await import("./rig.js")).ensureImage(outcome.staged.image, outcome.staged.data_url);
      (await import("./state.js")).snapshot();
      (await import("./editor.js")).bindExpressionSprite(part, key, outcome.staged);
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
  };

  setInterval(wireRegions, 200);
  setInterval(updateStatusUI, 250);
  wireRegions();
  requestAnimationFrame(frame);
}

// Helpers from state.js (re-exported locally for init)
import { round, num } from "./state.js";

init();