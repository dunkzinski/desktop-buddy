/**
 * char-rig — Tauri bridge: outgoing state reports, incoming events, import flow
 *
 * Depends on: state.js, rig.js, speech.js, ipc.js, editor.js.
 * One-way edges only: nothing under state/rig/speech/editor imports this file.
 */

import {
  avatar, RT, speech,
  round,
  toast, setExpression, markManual, updateStatusUI,
} from "./state.js";

import {
  applyModel, refreshModelList, localRotation, wireRegions,
} from "./rig.js";

import { speak, hideSpeech } from "./speech.js";

import { renderInspector, updateInspectorValues, attachStagedPart, updateTimeline } from "./editor.js";

import { playAnimation, stopAnimation } from "./animator.js";

import { call, listen, isTauri } from "./ipc.js";

/* ---------- outgoing: state report ---------- */

let lastReport = 0;

function jointAngles() {
  const out = {};
  for (const id of RT.order) {
    const bone = RT.bones[id];
    if (bone) out[id] = round(localRotation(bone), 4);
  }
  return out;
}

/** Push the live avatar state to Rust at ~8 Hz. */
export function reportState(now) {
  if (!isTauri()) return;
  if (now - lastReport < 120) return;
  lastReport = now;
  return call("report_state", {
    position: [avatar.x, avatar.y],
    target_position: [avatar.targetX, avatar.targetY],
    state: avatar.mode,
    expression: avatar.expression,
    joints: jointAngles(),
    talking: avatar.talk,
    speech: speech.visible,
  });
}

/* ---------- incoming: remote state ---------- */

export function applyRemoteState(remote) {
  if (!remote) return;
  const rev = Number(remote.revision || 0);
  if (!Number.isFinite(rev) || rev <= avatar.revision) return;
  avatar.revision = rev;
  markManual(15);

  // The pointer owns the avatar while it is being dragged: a frame that was
  // already in flight must not yank it out from under the user's hand. The
  // revision is still recorded, so nothing is replayed after the release.
  if (avatar.dragging) return;

  let walkSegment = false;
  if (Array.isArray(remote.target_position)) {
    const tx = Number(remote.target_position[0]);
    const ty = Number(remote.target_position[1]);
    if (Number.isFinite(tx) && Number.isFinite(ty) &&
        (Math.abs(tx - avatar.targetX) > 0.5 || Math.abs(ty - avatar.targetY) > 0.5)) {
      avatar.startX = avatar.x;
      avatar.startY = avatar.y;
      avatar.startTime = avatar.time;
      avatar.targetX = tx;
      avatar.targetY = ty;
      avatar.walkPhase = 0;
      if (avatar.mode !== "walking") avatar.mode = "walking";
      walkSegment = true;
    }
  }
  if (remote.state && remote.state !== avatar.mode) {
    // Adopting `walking` without the segment that goes with it would leave
    // step() with a stale start (and an old target), which snaps the avatar
    // across the screen — the teleport a drag must never suffer. Rust echoes
    // the page's own walk state, so a bare `walking` is not an instruction.
    if (remote.state !== "walking" || walkSegment) {
      avatar.mode = remote.state;
      if (avatar.mode === "waving") avatar.wavePhase = 0;
      if (avatar.mode === "idle") avatar.idleTimer = 0;
    }
  }
  if (remote.expression && remote.expression !== avatar.expression) setExpression(remote.expression);
  updateStatusUI();
}

export function applyPartPatch(target, patch) {
  if (!target || !patch) return;
  let touched = 0;
  for (const bone of RT.boneList) {
    for (const part of bone.parts) {
      const byTag = target.tag && part.tag === target.tag;
      const byId = target.part && part.id === target.part;
      if (!byTag && !byId) continue;
      if ("talk" in patch) part.talk = !!patch.talk;
      if ("state" in patch) {
        part.state = patch.state;
        applyPartVariantSafe(part);
      }
      for (const key of ["offset", "rotation", "scale", "z", "pivot", "visible", "color"]) {
        if (key in patch) part[key] = patch[key];
      }
      if ("image" in patch && RT.images[patch.image]) part.image = patch.image;
      touched += 1;
    }
  }
  if (touched) {
    renderInspector();
    updateInspectorValues();
    console.log("[avatar] patched", touched, "part(s)", JSON.stringify(target));
  } else {
    console.warn("[avatar] part patch matched nothing:", JSON.stringify(target));
  }
}

/** Local copy so a variant can be activated without pulling geometry in. */
function applyPartVariantSafe(part) {
  if (!part.variants || !part.state) return;
  const target = part.variants[part.state];
  if (typeof target === "string") part.image = target;
}

/* ---------- API status indicator ---------- */

/**
 * Paint the API line and its dot.
 *
 *   red    `offline`   — the HTTP API is not listening
 *   amber  `listening` — bound, but no client is attached
 *   green  `connected` — at least one agent/watcher is subscribed to `/events`
 *
 * Accepts a plain URL string (the `api-status` Tauri event) or the richer
 * `get_runtime_info` payload, which also carries `api_subscribers`.
 */
export function applyApiStatus(status) {
  const info = typeof status === "string" ? { api_url: status } : (status || {});
  const url = typeof info.api_url === "string" ? info.api_url : "";
  const bound = /^https?:\/\//.test(url);
  const subscribers = Number.isFinite(info.api_subscribers) ? info.api_subscribers : 0;

  const label = document.getElementById("api-url");
  if (label) label.textContent = bound ? url : "offline";

  const dot = document.getElementById("api-dot");
  if (!dot) return;
  dot.classList.remove("checking", "listening", "connected", "offline");
  if (!bound) {
    dot.classList.add("offline");
    dot.title = "API offline — the HTTP API is not listening";
  } else if (subscribers > 0) {
    dot.classList.add("connected");
    dot.title = `API connected at ${url} — ${subscribers} client${subscribers === 1 ? "" : "s"} attached`;
  } else {
    dot.classList.add("listening");
    dot.title = `API listening at ${url} — no client attached yet`;
  }
}

/** Re-read the runtime info so the dot tracks a server that died silently. */
export async function refreshApiStatus() {
  const info = await call("get_runtime_info");
  if (info && !info.__error) applyApiStatus(info);
  return info;
}

/* ---------- click-through ---------- */

export async function setClickThrough(enabled) {
  avatar.clickThrough = enabled;
  avatar.clickThroughUserSet = true;
  document.getElementById("btn-clickthrough").classList.toggle("pinned", enabled);
  await call("set_click_through", { enabled: enabled });
  toast(enabled ? "Click-through on empty space: ON" : "Click-through: OFF (whole screen interactive)");
  // re-publish regions with the new mode
  wireRegions();
}

/* ---------- panel collapse ---------- */

export function toggleCollapsed(collapsed) {
  avatar.collapsed = collapsed;
  document.getElementById("control-panel").classList.toggle("collapsed", collapsed);
  document.getElementById("panel-toggle").classList.toggle("visible", collapsed);
}

/* ---------- Tauri event listeners ---------- */

export async function setupEventListeners() {
  if (!isTauri()) {
    console.log("[avatar] not running under Tauri - event bridge disabled");
    return;
  }

  await listen("model-changed", (event) => {
    applyModel(event.payload);
    // a new rig brings new clips; the timeline must not show the old ones
    stopAnimation();
    updateTimeline();
  });
  await listen("rig-pose", (event) => { RT.pose = event.payload || {}; });
  await listen("part-patch", (event) => {
    const payload = event.payload || {};
    applyPartPatch(payload.target, payload.patch);
  });
  await listen("rig-saved", () => refreshModelList(RT.model ? RT.model.name : null));
  await listen("avatar-state-changed", (event) => applyRemoteState(event.payload));
  await listen("api-status", (event) => applyApiStatus(event.payload));
  await listen("emote", (event) => {
    const payload = event.payload || {};
    const holdMs = Number(payload.duration_ms) || 0;
    markManual(Math.max(8, holdMs / 1000 + 2));
    setExpression(payload.expression || "neutral", {
      durationMs: payload.duration_ms,
      revert: payload.revert || "neutral",
    });
  });
  await listen("speak", (event) => {
    const payload = event.payload || {};
    markManual(Math.max(10, (Number(payload.duration_ms) || 0) / 1000 + 3));
    if (payload.expression) setExpression(payload.expression);
    speak(payload.text, {
      durationMs: payload.duration_ms,
      charMs: payload.char_ms,
    });
  });
  await listen("speech-clear", () => hideSpeech());
  await listen("avatar-animate", (event) => {
    const payload = event.payload || {};
    const loop = typeof payload.loop === "boolean" ? payload.loop : undefined;
    if (payload.action === "stop" || !payload.name) {
      stopAnimation();
    } else {
      playAnimation(payload.name, loop);
      markManual(Math.max(4, (Number(payload.duration_ms) || 0) / 1000 + 1));
    }
    updateTimeline();
  });
}

/* ---------- import flow ---------- */

let pendingImportPath = null;

export function showChooser(path) {
  pendingImportPath = path;
  const fileEl = document.getElementById("chooser-file");
  if (fileEl) fileEl.textContent = String(path).split(/[\\/]/).pop();
  document.getElementById("import-chooser").classList.remove("hidden");
}

export function hideChooser() {
  pendingImportPath = null;
  document.getElementById("import-chooser").classList.add("hidden");
}

export async function importFromDialog() {
  const picked = await call("pick_import_file");
  if (!picked || picked.__error) { toast("Import cancelled"); return; }
  if (picked.kind === "rig") {
    const outcome = await call("import_asset", { path: picked.path, mode: "model" });
    if (outcome && outcome.name) {
      await refreshModelList(outcome.name);
      const model = await call("get_model");
      if (model) applyModel(model);
      toast("Imported model " + outcome.name);
    }
    return;
  }
  showChooser(picked.path);
}

/**
 * Run the staged import for the pending file. Returns the raw `import_asset`
 * outcome so the caller (main.js) can route a staged part into the editor.
 */
export async function importImageAs(mode) {
  const path = pendingImportPath;
  hideChooser();
  if (!path) return null;
  const outcome = await call("import_asset", { path: path, mode: mode });
  if (!outcome || outcome.__error) return null;
  if (mode === "model" && outcome.name) {
    await refreshModelList(outcome.name);
    const model = await call("get_model");
    if (model) applyModel(model);
    toast("Created model " + outcome.name);
    return outcome;
  }
  if (mode === "part" && outcome.staged) attachStagedPart(outcome.staged);
  return outcome;
}
