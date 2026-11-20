/**
 * desktop-buddy — Tauri bridge: outgoing state reports, incoming events, import flow
 *
 * Depends on: state.js, rig.js, speech.js, ipc.js, editor.js.
 * One-way edges only: nothing under state/rig/speech/editor imports this file.
 */

import {
  avatar, RT, speech, watcher, ai, tts, stt,
  round,
  toast, setExpression, markManual, updateStatusUI, reactExpression,
} from "./state.js";

import {
  applyModel, refreshModelList, localRotation, wireRegions, avatarBounds,
} from "./rig.js";

import {
  speak, hideSpeech, showThinkingForAck,
  setAudioEnvelope, clearAudioEnvelope,
} from "./speech.js";

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
    // How far the art reaches from the root, per direction. Rust needs this for
    // `POST /perch` to land the *pixels* on a window border: the root sits near
    // the hips, so half the height would leave the feet floating.
    bounds: (() => {
      const box = avatarBounds();
      if (!box || !(box.w > 0) || !(box.h > 0)) return null;
      return {
        left: Math.max(0, avatar.x - box.x),
        right: Math.max(0, box.x + box.w - avatar.x),
        up: Math.max(0, avatar.y - box.y),
        down: Math.max(0, box.y + box.h - avatar.y),
      };
    })(),
  });
}

/* ---------- incoming: remote state ---------- */

export function applyRemoteState(remote) {
  if (!remote) return;
  const rev = Number(remote.revision || 0);
  if (!Number.isFinite(rev) || rev <= avatar.revision) return;
  avatar.revision = rev;
  markManual(15);

  // Spatial directives ride along with every state frame, so a `POST /perch`
  // is adopted here and the `/move` or `POST /state` that ends it clears it.
  avatar.perched = remote.perched === true;
  avatar.perchRect = remote.perch_rect && typeof remote.perch_rect === "object"
    ? remote.perch_rect
    : null;
  avatar.lookAt = Array.isArray(remote.look_at) &&
    Number.isFinite(Number(remote.look_at[0])) &&
    Number.isFinite(Number(remote.look_at[1]))
    ? { x: Number(remote.look_at[0]), y: Number(remote.look_at[1]) }
    : null;

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
    const expression = payload.expression || "neutral";
    // The watcher acks every trigger with a thinking emote; the card is what
    // makes the wait visible (and arms the THINKING_TIMEOUT_MS safety net) for
    // drag_end / idle_return / window_title / alert, not just the command bar.
    if (expression === "thinking") showThinkingForAck();
    setExpression(expression, {
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
      details: payload.details,
    });
  });
  await listen("speech-clear", () => hideSpeech());
  await listen("watcher-status", (event) => applyWatcherStatus(event.payload));
  await listen("tts-status", (event) => applyTtsStatus(event.payload));
  // Audio-driven lip sync: Piper's RMS envelope for the line that is starting
  // to play, and a stop when it ends (or the voice is cut off).
  await listen("tts-envelope", (event) => {
    const payload = event.payload || {};
    setAudioEnvelope(payload.samples, payload.bucket_ms);
  });
  await listen("tts-envelope-stop", () => clearAudioEnvelope());
  await listen("stt-status", (event) => applySttStatus(event.payload));
  await listen("ai-profile-changed", (event) => applyAiProfiles(event.payload));
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

  await refreshWatcher();
  await refreshTts();
  await refreshStt();
  await refreshAiProfiles();
}

/* ---------- AI Brain (profile dropdown + health badge) ---------- */

/** Status word for the health badge: `online`, `offline` or `cli`. */
export function aiHealthStatus() {
  const status = ai.health && ai.health.status;
  return status === "online" || status === "cli" ? status : "offline";
}

/** Label of the active profile (falls back to its name, then a placeholder). */
export function aiActiveLabel() {
  const profile = ai.profiles.find((entry) => entry.name === ai.active);
  return (profile && (profile.label || profile.name)) || ai.active || "no profile";
}

/**
 * Apply one `GET /ai-profiles` / `set_ai_profile` payload.
 *
 * The dropdown is rebuilt only when the *set* of profiles changed, so an open
 * native select does not collapse every time the health probe refreshes.
 */
export function applyAiProfiles(payload) {
  if (!payload || typeof payload !== "object") return;
  // Read the *old* health before overwriting it: only a real transition is a
  // moment worth a face (`ai.checked` false is the first status, not a drop).
  const wasOnline = ai.checked && aiHealthStatus() !== "offline";
  if (typeof payload.active_profile === "string") ai.active = payload.active_profile;
  if (Array.isArray(payload.profiles)) {
    ai.profiles = payload.profiles.filter((entry) => entry && typeof entry.name === "string");
  }
  if (payload.active_health && typeof payload.active_health === "object") {
    ai.health = payload.active_health;
  }
  ai.checked = true;
  renderAiProfiles();
  // A brain that just stopped answering is a failure the user should see on the
  // face instead of a silent "offline" badge in a panel they may not be looking at.
  if (wasOnline && aiHealthStatus() === "offline") reactExpression("sad", 5);
}

export function renderAiProfiles() {
  const select = document.getElementById("ai-profile-select");
  if (select) {
    const key = ai.profiles.map((entry) => entry.name).join("|");
    if (select.dataset.profiles !== key) {
      select.textContent = "";
      if (!ai.profiles.length) {
        const option = document.createElement("option");
        option.value = "";
        option.textContent = ai.checked ? "no profiles configured" : "loading…";
        select.appendChild(option);
      }
      for (const profile of ai.profiles) {
        const option = document.createElement("option");
        option.value = profile.name;
        option.textContent = profile.label || profile.name;
        option.title =
          profile.kind === "openai_compat"
            ? `openai_compat · ${profile.model || "model?"}${profile.endpoint ? " @ " + profile.endpoint : ""}`
            : "hermes_cli · the local agent (tools, sessions)";
        select.appendChild(option);
      }
      select.dataset.profiles = key;
    }
    if (ai.active) select.value = ai.active;
    select.title = `AI brain: ${aiActiveLabel()} — switching applies to the next trigger`;
  }

  const badge = document.getElementById("ai-health-badge");
  if (badge) {
    const status = aiHealthStatus();
    badge.textContent = status;
    badge.className = "ai-health " + status;
    const detail = (ai.health && ai.health.detail) || "";
    badge.title = detail ? `${aiActiveLabel()}: ${detail}` : aiActiveLabel();
  }
}

/** Reload profiles + health from Rust (cheap: one 800 ms-capped probe). */
export async function refreshAiProfiles() {
  if (!isTauri()) return null;
  const status = await call("get_ai_profiles");
  if (status && !status.__error) applyAiProfiles(status);
  return status;
}

/** Switch the brain. Returns the new status, or null on failure. */
export async function setAiProfile(name) {
  if (!isTauri()) return null;
  const status = await call("set_ai_profile", { name });
  if (!status || status.__error) return null;
  applyAiProfiles(status);
  return status;
}

/* ---------- watcher console (switch, badge, pop-out log) ---------- */

/** Client-side cap; the Rust ring buffer keeps the same 200 lines. */
export const WATCHER_LOG_LIMIT = 200;

/** Badge text: `running`, `missing` or `off`. */
export function watcherBadge() {
  if (watcher.running) return "running";
  if (watcher.checked && !watcher.binary) return "missing";
  return "off";
}

/** Apply one `watcher-status` payload: full status or a single log line. */
export function applyWatcherStatus(payload) {
  if (!payload) return;
  const wasRunning = watcher.running;
  const wasChecked = watcher.checked;
  if (payload.kind === "log" && typeof payload.line === "string") {
    pushWatcherLine(payload.line);
  } else {
    if (Array.isArray(payload.logs)) {
      watcher.logs = payload.logs.slice(-WATCHER_LOG_LIMIT);
    }
    if ("binary_path" in payload) watcher.binary = payload.binary_path || "";
    if ("rules_path" in payload) watcher.rules = payload.rules_path || "";
    if ("pid" in payload) watcher.pid = payload.pid;
    // The command bar's chips live in watcher_rules.json → GET /watcher.
    if (Array.isArray(payload.quick_tasks)) {
      watcher.quickTasks = payload.quick_tasks
        .filter((task) => task && typeof task.label === "string" && typeof task.prompt === "string")
        .map((task) => ({
          label: task.label,
          prompt: task.prompt,
          // A chip may pin its own AI profile (a fast model for a one-liner).
          profile: typeof task.profile === "string" ? task.profile : "",
        }));
    }
    watcher.checked = true;
  }
  if (typeof payload.enabled === "boolean") watcher.enabled = payload.enabled;
  if (typeof payload.running === "boolean") watcher.running = payload.running;
  renderWatcher();
  // The app supervises the watcher, so "switched on but no longer running" is a
  // crash/restart, not a user action: show the surprise rather than staying blank.
  if (wasChecked && wasRunning && !watcher.running && watcher.enabled) {
    reactExpression("surprised", 5);
  }
}

function pushWatcherLine(line) {
  watcher.logs.push(line);
  const overflow = watcher.logs.length - WATCHER_LOG_LIMIT;
  if (overflow > 0) watcher.logs.splice(0, overflow);
}

/** Push the mirrored state into the panel and the drawer. */
export function renderWatcher() {
  const badge = watcherBadge();
  for (const id of ["watcher-badge", "drawer-badge"]) {
    const el = document.getElementById(id);
    if (!el) continue;
    el.textContent = badge;
    el.className = "watcher-badge " + badge;
  }

  const toggle = document.getElementById("watcher-toggle");
  if (toggle) {
    toggle.setAttribute("aria-checked", watcher.enabled ? "true" : "false");
    toggle.title = watcher.running
      ? `Watcher running (pid ${watcher.pid})`
      : watcher.enabled
        ? "Watcher starting…"
        : "Watcher stopped — click to start";
  }

  const rules = document.getElementById("watcher-rules");
  if (rules) {
    rules.textContent = watcher.rules || "no watcher_rules.json found";
    rules.title = watcher.rules || "no watcher_rules.json was found";
  }

  const log = document.getElementById("watcher-log");
  if (log) {
    const pinned = log.scrollTop + log.clientHeight >= log.scrollHeight - 12;
    log.textContent = watcher.logs.length ? watcher.logs.join("\n") : "no output yet";
    if (pinned) log.scrollTop = log.scrollHeight;
  }
}

/** Start or stop the watcher through the Rust command (no app restart). */
export async function setWatcherEnabled(enabled) {
  if (!isTauri()) return null;
  const status = await call("set_watcher_enabled", { enabled: !!enabled });
  applyWatcherStatus(status);
  return status;
}

/** Ask Rust for the current snapshot (switch, pid, paths, log tail). */
export async function refreshWatcher() {
  if (!isTauri()) return null;
  const status = await call("get_watcher_status");
  applyWatcherStatus(status);
  return status;
}

/** Drop the local tail only; Rust's ring buffer keeps its own copy. */
export function clearWatcherLogs() {
  watcher.logs = [];
  renderWatcher();
}

/* ---------- local voice (Piper TTS) ---------- */

/** Apply a `get_tts_status` / `GET /tts` / `tts-status` payload. */
export function applyTtsStatus(payload) {
  if (!payload || typeof payload !== "object" || payload.__error) return;
  if (typeof payload.enabled === "boolean") tts.enabled = payload.enabled;
  if (typeof payload.ready === "boolean") tts.ready = payload.ready;
  tts.reason = typeof payload.reason === "string" ? payload.reason : "";
  if (typeof payload.model_path === "string") {
    const parts = payload.model_path.split(/[\\/]/);
    tts.model = parts[parts.length - 1] || "";
  }
  tts.checked = true;
  renderTts();
}

/** The Voice row: switch state, badge text and why the voice is silent. */
export function renderTts() {
  const toggle = document.getElementById("tts-toggle");
  const badge = document.getElementById("tts-badge");
  const on = tts.enabled && tts.ready;
  if (toggle) {
    toggle.setAttribute("aria-checked", on ? "true" : "false");
    toggle.disabled = !tts.ready;
    toggle.title = !tts.ready
      ? `Voice unavailable: ${tts.reason || "piper was not found"}`
      : on
        ? `Speaking out loud with ${tts.model || "Piper"} — click to mute`
        : "Muted — click to speak bubbles out loud";
  }
  if (badge) {
    badge.textContent = !tts.ready ? "missing" : on ? "on" : "muted";
    badge.className = "watcher-badge " + (!tts.ready ? "missing" : on ? "running" : "off");
    badge.title = !tts.ready ? tts.reason || "" : tts.model || "Piper voice";
  }
}

/** Mute or unmute the voice through Rust (runtime only, not persisted). */
export async function setTtsEnabled(enabled) {
  if (!isTauri()) return null;
  const status = await call("set_tts_enabled", { enabled: !!enabled });
  applyTtsStatus(status);
  return status;
}

/** Ask Rust for the current voice status. */
export async function refreshTts() {
  if (!isTauri()) return null;
  const status = await call("get_tts_status");
  applyTtsStatus(status);
  return status;
}

/* ---------- push-to-talk voice input (Whisper STT) ---------- */

/** Apply a `get_stt_status` / `GET /stt` / `stt-status` payload. */
export function applySttStatus(payload) {
  if (!payload || typeof payload !== "object" || payload.__error) return;
  const before = stt.state;
  if (typeof payload.enabled === "boolean") stt.enabled = payload.enabled;
  if (typeof payload.ready === "boolean") stt.ready = payload.ready;
  if (typeof payload.state === "string") stt.state = payload.state;
  if (typeof payload.last_transcript === "string") stt.transcript = payload.last_transcript;
  if (typeof payload.hotkey === "string") stt.hotkey = payload.hotkey;
  stt.reason = typeof payload.reason === "string" ? payload.reason : "";
  stt.checked = true;
  renderStt();
  reactToStt(before, stt.state);
}

/**
 * The face follows the microphone. Whisper transcribing is a "let me work it
 * out" moment, so the avatar looks thoughtful while the transcript is written;
 * listening itself is not a mood (the user is the one doing the talking), and
 * the transcript goes on to raise its own thinking card via `notify_reply`.
 */
function reactToStt(before, after) {
  if (after === before) return;
  if (after === "transcribing") reactExpression("thinking", 8);
}

/** The Push-to-talk row: badge text, button label and the reason it is off. */
export function renderStt() {
  const button = document.getElementById("btn-ptt");
  const badge = document.getElementById("stt-badge");
  const listening = stt.state === "listening";
  const transcribing = stt.state === "transcribing";
  if (button) {
    button.disabled = !stt.ready || !stt.enabled;
    button.classList.toggle("recording", listening);
    button.classList.toggle("transcribing", transcribing);
    button.textContent = listening ? "Listening…" : transcribing ? "Transcribing…" : "Hold to talk";
    button.title = !stt.ready
      ? `Push to talk unavailable: ${stt.reason || "whisper-cli was not found"}`
      : !stt.enabled
        ? "Push to talk is switched off"
        : `Hold to talk (or hold ${stt.hotkey || "Alt+V"} anywhere). Release to send.`;
  }
  if (badge) {
    badge.textContent = !stt.ready ? "missing" : listening ? "listening" : transcribing ? "transcribing" : "idle";
    badge.className = "watcher-badge " + (!stt.ready ? "missing" : listening ? "running" : transcribing ? "busy" : "off");
    badge.title = !stt.ready
      ? stt.reason || ""
      : stt.transcript
        ? `last: ${stt.transcript}`
        : "microphone idle";
  }
}

/** Begin a hold (pointer down on the button). */
export async function startPtt() {
  if (!isTauri()) return null;
  const status = await call("ptt_start");
  applySttStatus(status);
  return status;
}

/** End a hold (pointer up / leave): Whisper runs, then `/reply` fires. */
export async function stopPtt() {
  if (!isTauri()) return null;
  const status = await call("ptt_stop");
  applySttStatus(status);
  return status;
}

/** Ask Rust for the current microphone status. */
export async function refreshStt() {
  if (!isTauri()) return null;
  const status = await call("get_stt_status");
  applySttStatus(status);
  return status;
}

export function isWatcherDrawerOpen() {
  return watcher.drawer;
}

/** Open/close the pop-out console; keeps its hit region in sync. */
export function toggleWatcherDrawer(force) {
  const drawer = document.getElementById("watcher-drawer");
  if (!drawer) return false;
  const open = force === undefined ? !watcher.drawer : !!force;
  watcher.drawer = open;
  drawer.classList.toggle("hidden", !open);
  document.getElementById("btn-watcher-logs").classList.toggle("active", open);
  // The drawer floats over the click-through desktop: publish its region now,
  // not on the next 200 ms timer, or the first click lands on the window behind.
  wireRegions();
  if (open) refreshWatcher();
  return open;
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
