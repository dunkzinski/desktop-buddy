/**
 * char-rig — the Settings modal
 *
 * One form over `GET /config` / `POST /config`. The point is that nothing here
 * hardcodes a path or asks the user to hand-edit `watcher_rules.json`: the
 * fields come from one declarative spec, the values come from the server, and a
 * save sends **only what changed** as a partial patch, so a stale tab can never
 * clobber a block the user edited in another editor.
 *
 * The server does the validating and the writing (span surgery — see
 * `src-tauri/src/settings.rs`); this module only builds the form, shows the
 * path ticks/crosses and reports what applied, what needs a restart, and what
 * the API refused.
 */

import { call } from "./ipc.js";
import { toast } from "./state.js";

const DEFAULT_BASE = "http://127.0.0.1:8765";

/**
 * The whole editable surface.
 *
 * `section` + `key` (dotted) say where the value lives in `watcher_rules.json`;
 * `pick` turns on the Browse… button (the `kind` goes straight to
 * `pick_config_path`).
 */
const FIELDS = [
  { tab: "voice", section: "tts", key: "enabled", label: "Speak replies out loud", type: "bool" },
  { tab: "voice", section: "tts", key: "piper_exe_path", label: "Piper executable", type: "text", pick: "executable" },
  { tab: "voice", section: "tts", key: "model_path", label: "Piper voice model", hint: ".onnx", type: "text", pick: "model" },
  { tab: "voice", section: "tts", key: "espeak_data_path", label: "espeak-ng data folder", type: "text", pick: "folder" },
  { tab: "voice", section: "tts", key: "length_scale", label: "Speaking speed", hint: "0.25 slow – 4.0 fast", type: "number", min: 0.25, max: 4, step: 0.05 },

  { tab: "voice", section: "stt", key: "enabled", label: "Push-to-talk", type: "bool" },
  { tab: "voice", section: "stt", key: "hotkey", label: "Hold-to-talk hotkey", hint: "e.g. Alt+V", type: "text" },
  { tab: "voice", section: "stt", key: "whisper_exe_path", label: "Whisper executable", type: "text", pick: "executable" },
  { tab: "voice", section: "stt", key: "whisper_model_path", label: "Whisper model", hint: "ggml-*.bin", type: "text", pick: "model" },
  { tab: "voice", section: "stt", key: "silero_vad_model_path", label: "Silero VAD model", hint: "optional", type: "text", pick: "model", nullable: true },
  { tab: "voice", section: "stt", key: "vad_threshold", label: "VAD threshold", type: "number", min: 0, max: 1, step: 0.05 },
  { tab: "voice", section: "stt", key: "language", label: "Language", hint: "ISO code", type: "text" },
  { tab: "voice", section: "stt", key: "min_speech_ms", label: "Drop clips shorter than", hint: "ms", type: "number", min: 50, max: 5000, step: 10 },

  { tab: "voice", section: "vision", key: "enabled", label: "Screen vision", type: "bool" },
  { tab: "voice", section: "vision", key: "capture_mode", label: "Capture", type: "select", options: [["active_window", "Focused window"], ["full_screen", "Whole screen"]] },
  { tab: "voice", section: "vision", key: "profile_name", label: "Vision profile", type: "select-profiles" },
  { tab: "voice", section: "vision", key: "max_image_dim", label: "Longest image side", hint: "px", type: "number", min: 256, max: 4096, step: 64 },
  { tab: "voice", section: "vision", key: "jpeg_quality", label: "JPEG quality", type: "number", min: 1, max: 100, step: 1 },

  { tab: "survival", section: "survival", key: "enabled", label: "Hunger system", type: "bool" },
  { tab: "survival", section: "survival", key: "notify", label: "Say tier changes out loud", type: "bool" },
  { tab: "survival", section: "survival", key: "metabolism.hunger_per_min", label: "Hunger drain", hint: "per minute", type: "number", min: 0.01, max: 100, step: 0.1 },
  { tab: "survival", section: "survival", key: "metabolism.saturation_per_min", label: "Saturation drain", hint: "per minute", type: "number", min: 0.01, max: 100, step: 0.1 },
  { tab: "survival", section: "survival", key: "telemetry_secs", label: "Telemetry interval", hint: "seconds, 0 = off", type: "number", min: 0, max: 86400, step: 30 },
  { tab: "survival", section: "survival", key: "start_hunger", label: "Start hunger", hint: "0–100", type: "number", min: 0, max: 100, step: 1 },
  { tab: "survival", section: "survival", key: "start_saturation", label: "Start saturation", hint: "0–100", type: "number", min: 0, max: 100, step: 1 },

  // The fishing and bag blocks live *inside* `survival`, so they address it as
  // `survival.fishing.*` / `survival.inventory.*`. A bare `section: "fishing"`
  // would read `config.fishing` (undefined, so every box renders blank and each
  // bool reads as checked) and the save would post a `fishing` section the host
  // has no owner for: "no plugin owns 'fishing'". `group` is only the card
  // heading this field is drawn under.
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.enabled", label: "Fishing", type: "bool" },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.hook_key", label: "Hook key", hint: "KeyboardEvent.code, e.g. Space", type: "text" },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.reel_key", label: "Reel key", hint: "KeyboardEvent.code, e.g. Space", type: "text" },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.mouse_button", label: "Mouse button", hint: "0 left · 1 middle · 2 right", type: "number", min: 0, max: 2, step: 1 },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.difficulty", label: "Difficulty", hint: "0.5 forgiving – 2.0 brutal", type: "number", min: 0.5, max: 2, step: 0.05 },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.min_wait_secs", label: "Shortest wait", hint: "seconds before a bite", type: "number", min: 1, max: 120, step: 1 },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.max_wait_secs", label: "Longest wait", hint: "seconds before a bite", type: "number", min: 1, max: 120, step: 1 },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.hook_window_ms", label: "Reaction window", hint: "ms to strike after the dip", type: "number", min: 250, max: 8000, step: 50 },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.reel_period_ms", label: "Marker sweep", hint: "ms per full pass", type: "number", min: 300, max: 6000, step: 50 },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.sweet_width", label: "Sweet spot width", hint: "fraction of the bar", type: "number", min: 0.04, max: 0.6, step: 0.01 },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.require_rod", label: "A cast needs the rod", type: "bool" },
  { tab: "survival", group: "fishing", section: "survival", key: "fishing.bait_required", label: "A cast spends bait", type: "bool" },

  { tab: "survival", group: "bag", section: "survival", key: "inventory.slots", label: "Bag slots", hint: "4–40", type: "number", min: 4, max: 40, step: 1 },
];

const settings = {
  base: DEFAULT_BASE,
  /** Last `GET /config` body — the baseline a save is diffed against. */
  loaded: null,
  /** `key` → input/select element, filled by `buildForm`. */
  inputs: new Map(),
  /** `tts.piper_exe_path` → row element, for the ✓/✗ marker. */
  rows: new Map(),
  ready: false,
  /** Set by `onSettingsSaved`; runs after a successful save. */
  onSaved: null,
};

const el = (id) => document.getElementById(id);

/* ---------- small path helpers ---------- */

function getPath(object, path) {
  return path.split(".").reduce((value, part) => (value == null ? undefined : value[part]), object);
}

function setPath(target, path, value) {
  const parts = path.split(".");
  let cursor = target;
  for (const part of parts.slice(0, -1)) {
    if (typeof cursor[part] !== "object" || cursor[part] === null) cursor[part] = {};
    cursor = cursor[part];
  }
  cursor[parts[parts.length - 1]] = value;
}

async function resolveBase() {
  const info = await call("get_runtime_info");
  const url = info && typeof info.api_url === "string" ? info.api_url : "";
  settings.base = /^https?:\/\//.test(url) ? url : DEFAULT_BASE;
}

function setStatus(text, kind) {
  const node = el("settings-status");
  if (!node) return;
  node.textContent = text || "";
  node.className = `settings-status${kind ? " " + kind : ""}`;
}

/* ---------- the form ---------- */

function fieldRow(field) {
  const row = document.createElement("div");
  row.className = `set-row${field.type === "bool" ? " set-row-check" : ""}`;

  const label = document.createElement("label");
  label.textContent = field.label;
  label.title = [field.section, field.key].join(".");
  if (field.hint) {
    const hint = document.createElement("span");
    hint.className = "set-hint";
    hint.textContent = field.hint;
    label.appendChild(hint);
  }

  let input;
  if (field.type === "bool") {
    input = document.createElement("input");
    input.type = "checkbox";
  } else if (field.type === "select" || field.type === "select-profiles") {
    input = document.createElement("select");
    if (field.type === "select") {
      for (const [value, text] of field.options) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = text;
        input.appendChild(option);
      }
    }
  } else {
    input = document.createElement("input");
    input.type = field.type === "number" ? "number" : "text";
    if (field.type === "number") {
      if (field.min !== undefined) input.min = String(field.min);
      if (field.max !== undefined) input.max = String(field.max);
      if (field.step !== undefined) input.step = String(field.step);
      input.inputMode = "decimal";
    } else {
      input.spellcheck = false;
    }
  }
  input.id = `set-${field.section}-${field.key.replace(/\./g, "-")}`;
  input.dataset.fieldKey = `${field.section}.${field.key}`;

  // The row order is label + input, or input + label for checkboxes.
  if (field.type === "bool") {
    row.appendChild(input);
    row.appendChild(label);
  } else {
    row.appendChild(label);
    row.appendChild(input);
  }

  if (field.pick) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "mini-btn set-browse";
    button.textContent = "Browse…";
    button.title = `Pick a ${field.pick}`;
    button.addEventListener("click", async () => {
      const picked = await call("pick_config_path", { kind: field.pick });
      if (picked && typeof picked === "string") {
        input.value = picked;
        input.scrollLeft = 0;
        input.title = picked;
        input.dispatchEvent(new Event("change"));
        markPath(`${field.section}.${field.key}`, null); // unknown until saved
      }
    });
    row.appendChild(button);
  }

  const mark = document.createElement("span");
  mark.className = "set-path-mark";
  row.appendChild(mark);
  settings.rows.set(`${field.section}.${field.key}`, mark);

  settings.inputs.set(`${field.section}.${field.key}`, input);
  return row;
}

function buildForm() {
  const panes = {
    voice: el("settings-pane-voice"),
    ai: el("settings-pane-ai"),
    survival: el("settings-pane-survival"),
  };
  if (!panes.voice || !panes.ai || !panes.survival) return false;

  for (const pane of Object.values(panes)) pane.innerHTML = "";

  // Voice & paths: three labelled groups so the modal reads like a form.
  for (const [title, sections] of [
    ["Voice output", ["tts"]],
    ["Push-to-talk", ["stt"]],
    ["Screen vision", ["vision"]],
  ]) {
    const group = document.createElement("div");
    group.className = "set-group";
    const heading = document.createElement("h4");
    heading.textContent = title;
    group.appendChild(heading);
    for (const field of FIELDS.filter((f) => f.tab === "voice" && sections.includes(f.section))) {
      group.appendChild(fieldRow(field));
    }
    panes.voice.appendChild(group);
  }

  // AI brain: the active profile plus one card per profile's parameters.
  const aiGroup = document.createElement("div");
  aiGroup.className = "set-group";
  const aiHeading = document.createElement("h4");
  aiHeading.textContent = "Active profile";
  aiGroup.appendChild(aiHeading);
  const activeRow = document.createElement("div");
  activeRow.className = "set-row";
  const activeLabel = document.createElement("label");
  activeLabel.textContent = "Answers desktop triggers";
  activeRow.appendChild(activeLabel);
  const activeSelect = document.createElement("select");
  activeSelect.id = "set-ai-active";
  activeRow.appendChild(activeSelect);
  aiGroup.appendChild(activeRow);
  panes.ai.appendChild(aiGroup);

  const profiles = document.createElement("div");
  profiles.id = "settings-profiles";
  panes.ai.appendChild(profiles);

  // Survival: metabolism first, then the fishing mini-game and the bag — each
  // block is its own group so the pane reads as three short forms. The group is
  // a `FIELDS` property, not the config section: `fishing`/`inventory` are
  // nested inside `survival`, so every field's section is `survival` and only
  // the card heading differs.
  for (const [title, groupId] of [
    ["Metabolism", "metabolism"],
    ["Fishing mini-game", "fishing"],
    ["Bag", "bag"],
  ]) {
    const group = document.createElement("div");
    group.className = "set-group";
    const heading = document.createElement("h4");
    heading.textContent = title;
    group.appendChild(heading);
    for (const field of FIELDS.filter(
      (f) => f.tab === "survival" && (f.group || "metabolism") === groupId,
    )) {
      group.appendChild(fieldRow(field));
    }
    panes.survival.appendChild(group);
  }

  return true;
}

/** One card per AI profile; only the numbers/endpoint are editable here. */
function buildProfiles(list) {
  const host = el("settings-profiles");
  if (!host) return;
  host.innerHTML = "";

  for (const profile of list || []) {
    const card = document.createElement("div");
    card.className = "set-profile";

    const head = document.createElement("div");
    head.className = "set-profile-head";
    const name = document.createElement("strong");
    name.textContent = profile.label || profile.name;
    const kind = document.createElement("code");
    kind.textContent = profile.kind;
    head.appendChild(name);
    head.appendChild(kind);
    card.appendChild(head);

    if (profile.kind === "openai_compat") {
      for (const [key, label, type, extra] of [
        ["endpoint", "Endpoint", "text", {}],
        ["model", "Model id", "text", {}],
        ["max_tokens", "Max tokens", "number", { min: 1, max: 1000000, step: 1 }],
        ["timeout_secs", "Timeout", "number", { min: 1, max: 3600, step: 1 }],
      ]) {
        const row = document.createElement("div");
        row.className = "set-row";
        const fieldLabel = document.createElement("label");
        fieldLabel.textContent = label;
        row.appendChild(fieldLabel);
        const input = document.createElement("input");
        input.type = type;
        input.spellcheck = false;
        if (extra.min !== undefined) input.min = String(extra.min);
        if (extra.max !== undefined) input.max = String(extra.max);
        if (extra.step !== undefined) input.step = String(extra.step);
        input.dataset.profile = profile.name;
        input.dataset.profileKey = key;
        // Fill here, not in `renderSettings`: these inputs are created *after*
        // the FIELDS pass, so an unfilled box would look like a user edit and
        // the next save would blank the endpoint.
        const current = profile[key];
        input.value = current == null ? "" : String(current);
        row.appendChild(input);
        settings.inputs.set(`ai_profiles.${profile.name}.${key}`, input);
        card.appendChild(row);
      }
    } else {
      const note = document.createElement("p");
      note.className = "set-note";
      note.textContent = `Runs a local command: ${profile.cmd || "(no cmd)"}`;
      card.appendChild(note);
    }

    host.appendChild(card);
  }
}

function markPath(key, exists) {
  const mark = settings.rows.get(key);
  if (!mark) return;
  mark.classList.remove("ok", "missing");
  if (exists === true) {
    mark.classList.add("ok");
    mark.textContent = "✓";
    mark.title = "exists";
  } else if (exists === false) {
    mark.classList.add("missing");
    mark.textContent = "✗";
    mark.title = "not found — the runtime will fall back or stay silent";
  } else {
    mark.textContent = "";
    mark.title = "not checked yet";
  }
}

/** Paint the whole form from a `GET /config` body. */
export function renderSettings(body) {
  if (!body || body.ok === false) {
    setStatus((body && body.reason) || "settings unavailable", "error");
    return;
  }
  settings.loaded = body;
  const config = body.settings || {};

  const pathLabel = el("settings-path");
  if (pathLabel) {
    pathLabel.textContent = body.path || "";
    pathLabel.title = body.path || "";
  }

  for (const field of FIELDS) {
    const input = settings.inputs.get(`${field.section}.${field.key}`);
    if (!input) continue;
    const value = getPath(config[field.section], field.key);
    if (field.type === "bool") {
      input.checked = value !== false;
    } else if (field.type === "select-profiles") {
      input.innerHTML = "";
      for (const profile of (config.ai && config.ai.profiles) || []) {
        const option = document.createElement("option");
        option.value = profile.name;
        option.textContent = profile.label || profile.name;
        input.appendChild(option);
      }
      input.value = value == null ? "" : String(value);
    } else {
      input.value = value == null ? "" : String(value);
      if (field.type === "text") {
        // Long absolute paths otherwise render scrolled to their tail.
        input.scrollLeft = 0;
        input.title = input.value;
      }
    }
  }

  // AI: the profile switcher and one card per profile.
  const active = el("set-ai-active");
  if (active) {
    active.innerHTML = "";
    for (const profile of (config.ai && config.ai.profiles) || []) {
      const option = document.createElement("option");
      option.value = profile.name;
      option.textContent = profile.label || profile.name;
      active.appendChild(option);
    }
    active.value = (config.ai && config.ai.active_profile) || "";
  }
  buildProfiles((config.ai && config.ai.profiles) || []);

  for (const entry of body.paths || []) {
    markPath(entry.key, entry.exists === true);
  }

  const warnings = el("settings-warnings");
  if (warnings) {
    const list = body.warnings || [];
    warnings.innerHTML = "";
    warnings.classList.toggle("hidden", list.length === 0);
    for (const text of list) {
      const line = document.createElement("p");
      line.textContent = `⚠ ${text}`;
      warnings.appendChild(line);
    }
  }
}

/** Fetch the live settings; `render` unless the caller wants the raw body. */
export async function refreshSettings(render = true) {
  try {
    const response = await fetch(`${settings.base}/config`, { cache: "no-store" });
    const body = await response.json().catch(() => ({}));
    if (render) renderSettings(body);
    return body;
  } catch (err) {
    setStatus(`API unreachable: ${err.message}`, "error");
    return null;
  }
}

/* ---------- diffing + saving ---------- */

function currentValue(field) {
  const input = settings.inputs.get(`${field.section}.${field.key}`);
  if (!input) return undefined;
  if (field.type === "bool") return input.checked;
  if ((field.type === "select" || field.type === "select-profiles") && input.value === "") {
    return undefined; // no options loaded yet — leave the file alone
  }
  if (field.type === "number") {
    if (input.value.trim() === "") return field.nullable ? null : undefined;
    const value = Number(input.value);
    return Number.isFinite(value) ? value : undefined;
  }
  const text = input.value;
  if (field.nullable && text.trim() === "") return null;
  return text;
}

function same(a, b) {
  if (a === null || a === undefined || b === null || b === undefined) {
    return (a === null || a === undefined) && (b === null || b === undefined);
  }
  if (typeof a === "number" || typeof b === "number") {
    return Number(a) === Number(b);
  }
  return a === b;
}

/** Only what the user actually changed, in the shape `POST /config` wants. */
export function collectPatch() {
  const config = (settings.loaded && settings.loaded.settings) || {};
  const patch = {};

  for (const field of FIELDS) {
    const next = currentValue(field);
    if (next === undefined) continue;
    const before = getPath(config[field.section], field.key);
    const normalized = before == null ? before : before;
    if (same(normalized, next)) continue;
    if (!patch[field.section]) patch[field.section] = {};
    setPath(patch[field.section], field.key, next);
  }

  // AI: active profile + the per-profile parameters.
  const active = el("set-ai-active");
  const wasActive = config.ai && config.ai.active_profile;
  if (active && active.value && active.value !== wasActive) {
    patch.ai = patch.ai || {};
    patch.ai.active_profile = active.value;
  }
  const profiles = (config.ai && config.ai.profiles) || [];
  for (const [mapKey, input] of settings.inputs) {
    if (!mapKey.startsWith("ai_profiles.")) continue;
    const [, name, key] = mapKey.split(".");
    const before = profiles.find((p) => p.name === name);
    if (!before) continue;
    // An empty box is "leave it alone", not 0 — sending 0 would trip the
    // server's range validation and reject the whole save.
    if (input.type === "number" && input.value.trim() === "") continue;
    const next = input.type === "number" ? Number(input.value) : input.value;
    if (input.type === "number" && !Number.isFinite(next)) continue;
    if (same(before[key], next)) continue;
    patch.ai = patch.ai || {};
    patch.ai.profiles = patch.ai.profiles || [];
    let entry = patch.ai.profiles.find((p) => p.name === name);
    if (!entry) {
      entry = { name };
      patch.ai.profiles.push(entry);
    }
    entry[key] = next;
  }

  return patch;
}

async function save() {
  const patch = collectPatch();
  const keys = Object.keys(patch);
  if (!keys.length) {
    setStatus("nothing changed", "");
    return;
  }

  const button = el("btn-settings-save");
  if (button) button.disabled = true;
  setStatus("saving…", "");

  try {
    const response = await fetch(`${settings.base}/config`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    });
    const body = await response.json().catch(() => ({}));

    if (!response.ok || body.ok === false) {
      const message = body.error || `HTTP ${response.status}`;
      setStatus(message, "error");
      toast(`Settings: ${message}`, true);
      return;
    }

    renderSettings(body);
    const applied = (body.applied || []).length;
    const restart = body.restart_required || [];
    setStatus(
      restart.length
        ? `saved ${applied} change(s) · restart to apply: ${restart.join(", ")}`
        : `saved ${applied} change(s) · applied live`,
      restart.length ? "warn" : "ok",
    );
    toast(restart.length ? "Settings saved — restart for the new paths" : "Settings saved");
    settings.onSaved?.();
  } catch (err) {
    setStatus(`save failed: ${err.message}`, "error");
  } finally {
    if (button) button.disabled = false;
  }
}

/* ---------- open / close ---------- */

export function settingsOpen() {
  const node = el("settings-modal");
  return !!node && !node.classList.contains("hidden");
}

export function toggleSettings(force) {
  const node = el("settings-modal");
  if (!node) return false;
  const open = typeof force === "boolean" ? force : node.classList.contains("hidden");
  node.classList.toggle("hidden", !open);
  node.setAttribute("aria-hidden", String(!open));
  const button = el("btn-settings");
  if (button) {
    button.classList.toggle("active", open);
    button.setAttribute("aria-pressed", String(open));
  }
  if (open) {
    setStatus("");
    refreshSettings();
  }
  return open;
}

function selectTab(name) {
  for (const tab of document.querySelectorAll(".settings-tab")) {
    tab.classList.toggle("active", tab.dataset.tab === name);
  }
  for (const pane of document.querySelectorAll(".settings-pane")) {
    pane.classList.toggle("active", pane.dataset.pane === name);
  }
}

/** Wire the modal, hydrate once, and follow the file from outside edits too. */
export async function setupSettings() {
  if (settings.ready) return;
  if (!el("settings-modal")) return;
  if (!buildForm()) return;

  el("btn-settings")?.addEventListener("click", () => toggleSettings());
  el("btn-settings-close")?.addEventListener("click", () => toggleSettings(false));
  el("btn-settings-save")?.addEventListener("click", save);
  el("btn-settings-reload")?.addEventListener("click", () => {
    setStatus("");
    refreshSettings();
  });
  for (const tab of document.querySelectorAll(".settings-tab")) {
    tab.addEventListener("click", () => selectTab(tab.dataset.tab));
  }

  await resolveBase();
  await refreshSettings();
  settings.ready = true;

  if (window.__avatarDebug) {
    window.__avatarDebug.settings = () => ({
      base: settings.base,
      open: settingsOpen(),
      ready: settings.ready,
      path: settings.loaded ? settings.loaded.path : null,
      warnings: settings.loaded ? settings.loaded.warnings : [],
      dirty: Object.keys(collectPatch()).length,
      patch: () => collectPatch(),
    });
    window.__avatarDebug.openSettings = () => toggleSettings(true);
  }
}

/** Registered by main.js so a save can refresh the panel's own badges. */
export function onSettingsSaved(handler) {
  settings.onSaved = handler;
}

