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
 *
 * The AI brain tab is the exception to "one declarative spec": profiles are a
 * *list* (add / remove / rename / import), so it keeps a working copy in `ai`
 * and diffs it against the last `GET /config` body. `POST /config` speaks
 * `ai.create` / `ai.profiles` / `ai.rename` / `ai.remove` for those.
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
 *
 * `vision.profile_name` is deliberately *not* here: it lives in the AI brain tab
 * next to `active_profile`, because both are "which brain" questions and the
 * dropdowns have to be rebuilt whenever a profile is added, renamed or removed.
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

/** Profile fields compared on save, per `kind`. */
const PROFILE_FIELDS = {
  openai_compat: ["label", "kind", "endpoint", "model", "max_tokens", "timeout_secs", "system_prompt"],
  hermes_cli: ["label", "kind", "cmd", "timeout_secs"],
};

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

/**
 * The `SOUL.md` editor's working state.
 *
 * It is deliberately separate from `settings.loaded` and from the Save button:
 * the persona lives in its own file and its own endpoint (`GET`/`POST /soul`),
 * so saving it must never resend the config form. `dirty` is what keeps a slow
 * `GET /soul` from overwriting text the user is in the middle of typing.
 */
const soul = {
  loaded: null,
  dirty: false,
};

/**
 * The AI brain tab's working copy.
 *
 * `profiles` is what the cards show (loaded entries plus unsaved drafts, each
 * flagged `isNew`); `removed` queues deletions; `selectedActive` /
 * `selectedVision` are the dropdowns' intent, kept here so a rename can carry
 * the selection along instead of silently falling back to the first profile.
 */
const ai = {
  profiles: [],
  removed: [],
  ports: [],
  hosts: [],
  discovered: null,
  selectedActive: "",
  selectedVision: "",
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

function heading(text) {
  const node = document.createElement("h4");
  node.textContent = text;
  return node;
}

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
  } else if (field.type === "select") {
    input = document.createElement("select");
    for (const [value, text] of field.options) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = text;
      input.appendChild(option);
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

/** A labelled `.set-row` without an input, so a caller can add its own. */
function bareRow(labelText, hint) {
  const row = document.createElement("div");
  row.className = "set-row";
  const label = document.createElement("label");
  label.textContent = labelText;
  if (hint) {
    const span = document.createElement("span");
    span.className = "set-hint";
    span.textContent = hint;
    label.appendChild(span);
  }
  row.appendChild(label);
  return row;
}

function buildForm() {
  const panes = {
    voice: el("settings-pane-voice"),
    ai: el("settings-pane-ai"),
    survival: el("settings-pane-survival"),
    soul: el("settings-pane-soul"),
  };
  if (!panes.voice || !panes.ai || !panes.survival || !panes.soul) return false;

  for (const pane of Object.values(panes)) pane.innerHTML = "";

  // Voice & paths: three labelled groups so the modal reads like a form.
  for (const [title, sections] of [
    ["Voice output", ["tts"]],
    ["Push-to-talk", ["stt"]],
    ["Screen vision", ["vision"]],
  ]) {
    const group = document.createElement("div");
    group.className = "set-group";
    group.appendChild(heading(title));
    for (const field of FIELDS.filter((f) => f.tab === "voice" && sections.includes(f.section))) {
      group.appendChild(fieldRow(field));
    }
    panes.voice.appendChild(group);
  }

  /* AI brain, part 1 — routing. Both dropdowns are rebuilt from the working
     copy whenever the profile list changes (see `refreshProfileOptions`). */
  const routing = document.createElement("div");
  routing.className = "set-group";
  routing.appendChild(heading("Routing"));

  const activeRow = bareRow("Answers desktop triggers");
  const activeSelect = document.createElement("select");
  activeSelect.id = "set-ai-active";
  activeSelect.addEventListener("change", () => {
    ai.selectedActive = activeSelect.value;
  });
  activeRow.appendChild(activeSelect);
  routing.appendChild(activeRow);

  const visionRow = bareRow("Vision / screen capture", "profile screenshots are sent to");
  const visionSelect = document.createElement("select");
  visionSelect.id = "set-ai-vision";
  visionSelect.addEventListener("change", () => {
    ai.selectedVision = visionSelect.value;
  });
  visionRow.appendChild(visionSelect);
  routing.appendChild(visionRow);
  panes.ai.appendChild(routing);

  /* AI brain, part 2 — discovery. */
  const discovery = document.createElement("div");
  discovery.className = "set-group";
  discovery.appendChild(heading("Model discovery"));

  const hostsRow = bareRow(
    "Discovery hosts",
    "comma separated IPs/hostnames · add :port to target just that port",
  );
  const hostsInput = document.createElement("input");
  hostsInput.type = "text";
  hostsInput.id = "set-ai-hosts";
  hostsInput.spellcheck = false;
  hostsInput.placeholder = "127.0.0.1";
  hostsInput.title =
    "Hosts probed by Discover models — LAN, Tailscale or a second interface. An entry may carry its own port (`192.168.0.36:11434`). Saved as `discovery_hosts`";
  hostsRow.appendChild(hostsInput);
  discovery.appendChild(hostsRow);

  const portsRow = bareRow("Discovery ports", "comma separated · probed on every host");
  const portsInput = document.createElement("input");
  portsInput.type = "text";
  portsInput.id = "set-ai-ports";
  portsInput.spellcheck = false;
  portsInput.placeholder = "8001, 9931, 9932, 11434";
  portsInput.title = "Ports probed by Discover models; saved as `discovery_ports`";
  portsRow.appendChild(portsInput);
  const discoverButton = document.createElement("button");
  discoverButton.type = "button";
  discoverButton.className = "mini-btn";
  discoverButton.id = "btn-ai-discover";
  discoverButton.textContent = "Discover models";
  discoverButton.addEventListener("click", discoverModels);
  portsRow.appendChild(discoverButton);
  discovery.appendChild(portsRow);

  const note = document.createElement("p");
  note.className = "set-note";
  note.id = "ai-discovery-note";
  note.textContent =
    "Probes every host+port for OpenAI /v1/models, llama.cpp /models and Ollama /api/tags, then lists local .gguf files.";
  discovery.appendChild(note);

  const results = document.createElement("div");
  results.id = "ai-discovery";
  results.className = "set-discovery hidden";
  discovery.appendChild(results);
  panes.ai.appendChild(discovery);

  /* AI brain, part 3 — one card per profile. */
  const profileGroup = document.createElement("div");
  profileGroup.className = "set-group";
  const profileHead = document.createElement("div");
  profileHead.className = "set-group-head";
  profileHead.appendChild(heading("AI profiles"));
  const addButton = document.createElement("button");
  addButton.type = "button";
  addButton.className = "mini-btn";
  addButton.id = "btn-ai-add";
  addButton.textContent = "+ Add Profile";
  addButton.title = "Add a new ai_profiles entry (saved with Save)";
  addButton.addEventListener("click", () => addProfile());
  profileHead.appendChild(addButton);
  profileGroup.appendChild(profileHead);
  const profiles = document.createElement("div");
  profiles.id = "settings-profiles";
  profileGroup.appendChild(profiles);
  panes.ai.appendChild(profileGroup);

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
    group.appendChild(heading(title));
    for (const field of FIELDS.filter(
      (f) => f.tab === "survival" && (f.group || "metabolism") === groupId,
    )) {
      group.appendChild(fieldRow(field));
    }
    panes.survival.appendChild(group);
  }

  /* SOUL.md: the persona is its own file and its own endpoint, so this pane is
     one textarea plus two buttons — save it here, or open the file and edit it
     in the user's own editor. It deliberately does not participate in the
     config form's Save button. */
  const persona = document.createElement("div");
  persona.className = "set-group";
  persona.appendChild(heading("Persona"));

  const soulNote = document.createElement("p");
  soulNote.className = "set-note";
  soulNote.id = "soul-note";
  soulNote.textContent =
    "SOUL.md is read fresh on every watcher turn, so edits apply without a restart.";
  persona.appendChild(soulNote);

  const soulPath = document.createElement("code");
  soulPath.className = "set-soul-path";
  soulPath.id = "soul-path";
  persona.appendChild(soulPath);

  const editorRow = document.createElement("div");
  editorRow.className = "set-row set-row-area";
  const editorLabel = document.createElement("label");
  editorLabel.textContent = "SOUL.md";
  const editorHint = document.createElement("span");
  editorHint.className = "set-hint";
  editorHint.textContent = "identity, tone, brevity — the first section of the prompt";
  editorLabel.appendChild(editorHint);
  editorRow.appendChild(editorLabel);
  const editor = document.createElement("textarea");
  editor.id = "soul-editor";
  editor.className = "set-soul-editor";
  editor.rows = 16;
  editor.spellcheck = true;
  editor.placeholder = "Loading SOUL.md…";
  editor.addEventListener("input", () => {
    soul.dirty = true;
  });
  editorRow.appendChild(editor);
  persona.appendChild(editorRow);

  const soulActions = document.createElement("div");
  soulActions.className = "set-soul-actions";
  const saveSoul = document.createElement("button");
  saveSoul.type = "button";
  saveSoul.className = "mini-btn primary";
  saveSoul.id = "btn-soul-save";
  saveSoul.textContent = "Save SOUL.md";
  saveSoul.addEventListener("click", saveSoulFile);
  soulActions.appendChild(saveSoul);
  const openSoul = document.createElement("button");
  openSoul.type = "button";
  openSoul.className = "mini-btn";
  openSoul.id = "btn-soul-open";
  openSoul.textContent = "Open SOUL.md";
  openSoul.title = "Open the file in your default editor";
  openSoul.addEventListener("click", openSoulFile);
  soulActions.appendChild(openSoul);
  const reloadSoul = document.createElement("button");
  reloadSoul.type = "button";
  reloadSoul.className = "mini-btn";
  reloadSoul.id = "btn-soul-reload";
  reloadSoul.textContent = "Reload";
  reloadSoul.addEventListener("click", () => {
    soul.dirty = false;
    refreshSoul();
  });
  soulActions.appendChild(reloadSoul);
  persona.appendChild(soulActions);
  panes.soul.appendChild(persona);

  return true;
}

/* ---------- AI profile cards ---------- */

/** The server's own copy of the JSON-avatar prompt, so it is never duplicated. */
function defaultSystemPrompt() {
  const config = (settings.loaded && settings.loaded.settings) || {};
  return (config.ai && config.ai.default_system_prompt) || "";
}

/** Read every card's inputs back into `ai.profiles` (the DOM holds the value). */
function syncCards() {
  for (const card of document.querySelectorAll("#settings-profiles .set-profile")) {
    const index = Number(card.dataset.index);
    const profile = ai.profiles[index];
    if (!profile) continue;
    for (const input of card.querySelectorAll("[data-k]")) {
      const key = input.dataset.k;
      if (input.type === "number") {
        profile[key] = input.value.trim() === "" ? null : Number(input.value);
      } else {
        profile[key] = input.value;
      }
    }
  }
}

function profileInputRow(labelText, key, value, type, hint) {
  const row = bareRow(labelText, hint);
  const input = document.createElement("input");
  input.type = type;
  input.spellcheck = false;
  input.dataset.k = key;
  input.value = value == null ? "" : String(value);
  if (type === "number") {
    input.inputMode = "decimal";
    input.min = "1";
    input.max = key === "max_tokens" ? "1000000" : "3600";
    input.step = "1";
  }
  if (key === "name" || key === "label") input.addEventListener("input", refreshProfileOptions);
  row.appendChild(input);
  return row;
}

function profileCard(profile, index) {
  const card = document.createElement("div");
  card.className = `set-profile${profile.isNew ? " set-profile-new" : ""}`;
  card.dataset.index = String(index);

  const head = document.createElement("div");
  head.className = "set-profile-head";

  const labelInput = document.createElement("input");
  labelInput.type = "text";
  labelInput.className = "set-profile-label";
  labelInput.dataset.k = "label";
  labelInput.spellcheck = false;
  labelInput.placeholder = "Display name";
  labelInput.title = "Shown on this card and in both dropdowns";
  labelInput.value = profile.label || "";
  labelInput.addEventListener("input", refreshProfileOptions);
  head.appendChild(labelInput);

  const kind = document.createElement("code");
  kind.className = "set-profile-kind";
  kind.textContent = profile.isNew ? "new" : profile.kind || "openai_compat";
  head.appendChild(kind);

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "mini-btn set-profile-remove";
  remove.textContent = "Remove";
  remove.title = "Delete this profile from watcher_rules.json";
  remove.addEventListener("click", () => removeProfile(index));
  head.appendChild(remove);
  card.appendChild(head);

  card.appendChild(profileInputRow("Key", "name", profile.name, "text", "JSON key / id"));

  const kindRow = bareRow("Kind");
  const kindSelect = document.createElement("select");
  kindSelect.dataset.k = "kind";
  for (const [value, text] of [
    ["openai_compat", "OpenAI-compatible server"],
    ["hermes_cli", "Hermes CLI command"],
  ]) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = text;
    kindSelect.appendChild(option);
  }
  kindSelect.value = profile.kind || "openai_compat";
  kindSelect.addEventListener("change", () => {
    syncCards();
    ai.profiles[index].kind = kindSelect.value;
    renderProfileCards();
    refreshProfileOptions();
  });
  kindRow.appendChild(kindSelect);
  card.appendChild(kindRow);

  if ((profile.kind || "openai_compat") === "hermes_cli") {
    card.appendChild(profileInputRow("Command", "cmd", profile.cmd, "text", "e.g. hermes chat --continue {session} -Q -q"));
  } else {
    card.appendChild(profileInputRow("Endpoint", "endpoint", profile.endpoint, "text", "http://127.0.0.1:8001/v1"));
    card.appendChild(profileInputRow("Model id", "model", profile.model, "text", "what the server calls it"));
    card.appendChild(profileInputRow("Max tokens", "max_tokens", profile.max_tokens, "number", "optional"));
  }
  card.appendChild(profileInputRow("Timeout", "timeout_secs", profile.timeout_secs, "number", "seconds"));

  if ((profile.kind || "openai_compat") !== "hermes_cli") {
    const promptRow = document.createElement("div");
    promptRow.className = "set-row set-row-area";
    const promptLabel = document.createElement("label");
    promptLabel.textContent = "System prompt";
    const hint = document.createElement("span");
    hint.className = "set-hint";
    hint.textContent = "the JSON avatar contract is pre-filled";
    promptLabel.appendChild(hint);
    promptRow.appendChild(promptLabel);
    const prompt = document.createElement("textarea");
    prompt.rows = 4;
    prompt.spellcheck = false;
    prompt.dataset.k = "system_prompt";
    prompt.value = profile.system_prompt || "";
    promptRow.appendChild(prompt);
    card.appendChild(promptRow);
  }

  const warn = document.createElement("p");
  warn.className = "set-profile-warn hidden";
  card.appendChild(warn);

  return card;
}

/** Repaint every profile card from the working copy. */
function renderProfileCards() {
  const host = el("settings-profiles");
  if (!host) return;
  host.innerHTML = "";
  ai.profiles.forEach((profile, index) => host.appendChild(profileCard(profile, index)));
}

function showWarn(index, text) {
  const warn = document.querySelector(
    `#settings-profiles .set-profile[data-index="${index}"] .set-profile-warn`,
  );
  if (!warn) return;
  warn.textContent = text;
  warn.classList.remove("hidden");
}

/** Rebuild both dropdowns from the cards, keeping the intended selection. */
function refreshProfileOptions() {
  syncCards();
  const active = el("set-ai-active");
  const vision = el("set-ai-vision");
  if (!active || !vision) return;

  const names = ai.profiles.map((profile) => (profile.name || "").trim());
  // A rename moves the selection with the profile instead of losing it: a name
  // that vanished from the list is re-pointed at the same card's new key.
  if (ai.selectedActive && !names.includes(ai.selectedActive)) {
    ai.selectedActive = names[0] || "";
  }
  if (ai.selectedVision && !names.includes(ai.selectedVision)) {
    ai.selectedVision = names[0] || "";
  }

  for (const select of [active, vision]) select.innerHTML = "";
  for (const profile of ai.profiles) {
    const name = (profile.name || "").trim();
    if (!name) continue;
    for (const select of [active, vision]) {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = (profile.label || "").trim() || name;
      select.appendChild(option);
    }
  }
  if (names.includes(ai.selectedActive)) active.value = ai.selectedActive;
  if (names.includes(ai.selectedVision)) vision.value = ai.selectedVision;
  ai.selectedActive = active.value || ai.selectedActive;
  ai.selectedVision = vision.value || ai.selectedVision;
}

function addProfile() {
  syncCards();
  let n = ai.profiles.length + 1;
  let name = `profile-${n}`;
  while (ai.profiles.some((profile) => (profile.name || "").trim() === name)) {
    name = `profile-${++n}`;
  }
  ai.profiles.push({
    name,
    label: "",
    kind: "openai_compat",
    endpoint: `http://127.0.0.1:${ai.ports[0] || 8001}/v1`,
    model: "",
    cmd: "",
    max_tokens: null,
    timeout_secs: 120,
    system_prompt: defaultSystemPrompt(),
    isNew: true,
  });
  renderProfileCards();
  refreshProfileOptions();
  const card = document.querySelector(
    `#settings-profiles .set-profile[data-index="${ai.profiles.length - 1}"]`,
  );
  card?.scrollIntoView({ block: "nearest" });
  card?.querySelector('[data-k="label"]')?.focus();
}

function removeProfile(index) {
  syncCards();
  const profile = ai.profiles[index];
  if (!profile) return;
  const name = (profile.name || "").trim();

  // Guards first, and inline: the server refuses these too, but a modal that
  // round-trips an error for a click it could have predicted is worse UX.
  if (ai.profiles.length <= 1) {
    showWarn(index, "This is the only AI profile — add another one before removing it.");
    return;
  }
  if (name && name === ai.selectedActive) {
    showWarn(
      index,
      "This profile answers desktop triggers. Switch “Answers desktop triggers” to another profile first.",
    );
    return;
  }
  if (name && name === ai.selectedVision) {
    showWarn(
      index,
      "This profile is used for screen capture. Switch “Vision / screen capture” to another profile first.",
    );
    return;
  }

  if (!profile.isNew) ai.removed.push(profile.name);
  ai.profiles.splice(index, 1);
  renderProfileCards();
  refreshProfileOptions();
}

/* ---------- model discovery ---------- */

function parsePorts(text) {
  const ports = [];
  for (const part of String(text || "").split(/[,\s;]+/)) {
    if (!part.trim()) continue;
    const port = Number(part.trim());
    if (!Number.isInteger(port) || port < 1 || port > 65535) continue;
    if (!ports.includes(port)) ports.push(port);
  }
  return ports;
}

/** Clean one host entry: trim, lose an accidental scheme/path, drop junk. */
function normalizeHost(part) {
  let value = String(part || "").trim();
  value = value.replace(/^https?:\/\//i, "");
  value = value.replace(/[/?#].*$/, "");
  value = value.trim();
  if (!value || /\s/.test(value) || value.includes("@")) return "";
  return value;
}

/**
 * Split `host`, `host:port`, `[::1]:8001` into its parts the same way the
 * backend does, so the note can count real targets and a loopback chip can keep
 * its short `:port` form.
 */
function splitHostPort(entry) {
  const text = String(entry || "").trim();
  const bracketed = /^\[([^\]]+)\](?::(\d+))?$/.exec(text);
  if (bracketed) {
    const port = Number(bracketed[2]);
    return { host: bracketed[1], port: Number.isInteger(port) && port > 0 ? port : 0 };
  }
  if ((text.match(/:/g) || []).length === 1) {
    const [host, rawPort] = text.split(":");
    const port = Number(rawPort);
    if (host && Number.isInteger(port) && port >= 1 && port <= 65535) return { host, port };
  }
  return { host: text, port: 0 };
}

function isLoopbackHost(host) {
  const value = String(host || "")
    .toLowerCase()
    .replace(/^\[|\]$/g, "");
  return value === "localhost" || value === "::1" || value === "0.0.0.0" || value.startsWith("127.");
}

/** `:8001` for loopback (the form the modal always used), `host:port` otherwise. */
function targetLabel(host, port) {
  return isLoopbackHost(host) ? `:${port}` : `${host}:${port}`;
}

function parseHosts(text) {
  const hosts = [];
  for (const part of String(text || "").split(/[,\s;]+/)) {
    const host = normalizeHost(part);
    if (!host) continue;
    if (!hosts.some((known) => known.toLowerCase() === host.toLowerCase())) hosts.push(host);
  }
  return hosts;
}

/** How many `(host, port)` pairs the current form would probe. */
function countTargets(hosts, ports) {
  let count = 0;
  for (const host of hosts) {
    const { port } = splitHostPort(host);
    count += port ? 1 : ports.length;
  }
  return count;
}

function portInUrl(url) {
  const match = /^[a-z]+:\/\/([^/?#]*)/i.exec(String(url || ""));
  if (!match) return 0;
  const authority = match[1].split("@").pop();
  const parts = authority.split(":");
  if (parts.length < 2) return 0;
  const port = Number(parts[parts.length - 1]);
  return Number.isInteger(port) ? port : 0;
}

function slugify(text) {
  const slug = String(text || "")
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 40);
  return /^[a-z0-9]/.test(slug) ? slug : "";
}

function uniqueKey(base) {
  const root = base || "model";
  let name = root;
  let n = 2;
  while (ai.profiles.some((profile) => (profile.name || "").trim() === name)) {
    name = `${root}-${n++}`;
  }
  return name;
}

/** The profile (if any) that already points at this discovered model. */
function existingProfileFor(model) {
  for (const profile of ai.profiles) {
    if (profile.model !== model.id) continue;
    if (!model.endpoint) return profile.name; // a .gguf has no endpoint to match
    if (profile.endpoint === model.endpoint) return profile.name;
    if (model.port && portInUrl(profile.endpoint) === Number(model.port)) return profile.name;
  }
  return "";
}

function clearDiscovery() {
  const host = el("ai-discovery");
  const note = el("ai-discovery-note");
  if (host) {
    host.innerHTML = "";
    host.classList.add("hidden");
  }
  if (note) {
    note.textContent =
      "Probes every host+port for OpenAI /v1/models, llama.cpp /models and Ollama /api/tags, then lists local .gguf files.";
  }
}

function renderDiscovery(body) {
  const host = el("ai-discovery");
  const note = el("ai-discovery-note");
  if (!host) return;
  host.innerHTML = "";

  if (!body || body.ok === false) {
    host.classList.add("hidden");
    if (note) note.textContent = `Discovery failed: ${(body && body.reason) || "no response"}`;
    return;
  }
  host.classList.remove("hidden");
  if (note) {
    note.textContent = `${body.summary} · ${body.elapsed_ms} ms (${body.timeout_ms} ms per target)`;
  }

  const ports = document.createElement("div");
  ports.className = "set-discovery-ports";
  for (const entry of body.ports || []) {
    const chip = document.createElement("span");
    chip.className = `set-chip${entry.reachable ? " ok" : ""}`;
    const label = entry.label || targetLabel(entry.host, entry.port);
    chip.textContent = `${label} · ${entry.reachable ? entry.detail : "offline"}`;
    chip.title = entry.host && !isLoopbackHost(entry.host) ? `${entry.host} — ${entry.detail || ""}` : entry.detail || "";
    ports.appendChild(chip);
  }
  host.appendChild(ports);

  const models = body.models || [];
  if (!models.length) {
    const empty = document.createElement("p");
    empty.className = "set-note";
    empty.textContent =
      "Nothing found: no model server answered and no local .gguf files were seen. Start a server, or check the hosts and ports.";
    host.appendChild(empty);
    return;
  }

  const list = document.createElement("div");
  list.className = "set-discovery-list";
  models.forEach((model, index) => {
    const row = document.createElement("div");
    row.className = "set-discovery-row";

    const id = document.createElement("code");
    id.textContent = model.id;
    id.title = model.id;
    row.appendChild(id);

    const meta = document.createElement("span");
    meta.className = "set-discovery-meta";
    const bits = [model.source];
    if (model.endpoint) bits.push(model.endpoint);
    if (model.multimodal) bits.push("vision");
    if (model.path) bits.push(model.path);
    meta.textContent = bits.join(" · ");
    meta.title = meta.textContent;
    row.appendChild(meta);

    const owner = existingProfileFor(model);
    if (owner) {
      const badge = document.createElement("span");
      badge.className = "set-chip ok";
      badge.textContent = `in ${owner}`;
      badge.title = "already configured as this profile";
      row.appendChild(badge);
    } else {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "mini-btn";
      button.textContent = "+ Import";
      button.title = "Add a pre-filled profile card (nothing is written until Save)";
      button.addEventListener("click", () => importModel(index));
      row.appendChild(button);
    }
    list.appendChild(row);
  });
  host.appendChild(list);
}

function importModel(index) {
  const model = ((ai.discovered && ai.discovered.models) || [])[index];
  if (!model) return;
  syncCards();
  ai.profiles.push({
    name: uniqueKey(slugify(model.id)),
    label: model.label || model.id,
    kind: "openai_compat",
    endpoint: model.endpoint || `http://127.0.0.1:${ai.ports[0] || 8001}/v1`,
    model: model.id,
    cmd: "",
    max_tokens: null,
    timeout_secs: 120,
    system_prompt: defaultSystemPrompt(),
    isNew: true,
  });
  renderProfileCards();
  refreshProfileOptions();
  renderDiscovery(ai.discovered); // flips this row to "in <name>"
  const card = document.querySelector(
    `#settings-profiles .set-profile[data-index="${ai.profiles.length - 1}"]`,
  );
  card?.scrollIntoView({ block: "nearest" });
}

async function discoverModels() {
  const button = el("btn-ai-discover");
  const note = el("ai-discovery-note");
  const hostsInput = el("set-ai-hosts");
  const portsInput = el("set-ai-ports");
  const hosts = parseHosts(hostsInput ? hostsInput.value : "");
  const ports = parsePorts(portsInput ? portsInput.value : "");
  if (!hosts.length) {
    if (note) note.textContent = "Add at least one host first (for example 127.0.0.1).";
    return;
  }
  const targetCount = countTargets(hosts, ports);
  if (!targetCount) {
    if (note) note.textContent = "Add at least one port first (for example 8001, 11434), or give a host its own :port.";
    return;
  }

  if (button) {
    button.disabled = true;
    button.textContent = "Probing…";
  }
  if (note) {
    note.textContent = `probing ${targetCount} target(s) on ${hosts.length} host(s)…`;
  }

  try {
    const response = await fetch(`${settings.base}/ai-discover`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ hosts, ports }),
    });
    const body = await response.json().catch(() => ({}));
    ai.discovered = body && typeof body === "object" ? body : { ok: false, reason: "empty reply" };
  } catch (err) {
    ai.discovered = { ok: false, reason: err.message };
  } finally {
    if (button) {
      button.disabled = false;
      button.textContent = "Discover models";
    }
  }
  renderDiscovery(ai.discovered);
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
    } else {
      input.value = value == null ? "" : String(value);
      if (field.type === "text") {
        // Long absolute paths otherwise render scrolled to their tail.
        input.scrollLeft = 0;
        input.title = input.value;
      }
    }
  }

  // AI: the two routing dropdowns, the discovery ports and one card per profile.
  const profiles = (config.ai && config.ai.profiles) || [];
  ai.profiles = profiles.map((profile) => ({
    name: profile.name,
    label: profile.label || profile.name,
    kind: profile.kind || "openai_compat",
    endpoint: profile.endpoint || "",
    model: profile.model || "",
    cmd: profile.cmd || "",
    max_tokens: profile.max_tokens == null ? null : profile.max_tokens,
    timeout_secs: profile.timeout_secs == null ? 120 : profile.timeout_secs,
    system_prompt: profile.system_prompt || "",
    isNew: false,
  }));
  ai.removed = [];
  ai.ports = Array.isArray(config.discovery_ports) ? config.discovery_ports.slice() : [];
  ai.hosts =
    Array.isArray(config.discovery_hosts) && config.discovery_hosts.length
      ? config.discovery_hosts.slice()
      : ["127.0.0.1"];
  const active = (config.ai && config.ai.active_profile) || "";
  ai.selectedActive = active || (ai.profiles[0] && ai.profiles[0].name) || "";
  ai.selectedVision = (config.vision && config.vision.profile_name) || ai.selectedActive;

  const portsInput = el("set-ai-ports");
  if (portsInput) {
    portsInput.value = ai.ports.join(", ");
    portsInput.scrollLeft = 0;
  }
  const hostsInput = el("set-ai-hosts");
  if (hostsInput) {
    hostsInput.value = ai.hosts.join(", ");
    hostsInput.scrollLeft = 0;
  }

  renderProfileCards();
  refreshProfileOptions();
  if (ai.discovered) renderDiscovery(ai.discovered);
  else clearDiscovery();

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
    if (render) {
      renderSettings(body);
      // The persona is its own endpoint; keep the SOUL tab in step with the
      // config refresh without making either wait on the other.
      refreshSoul();
    }
    return body;
  } catch (err) {
    setStatus(`API unreachable: ${err.message}`, "error");
    return null;
  }
}

/* ---------- SOUL.md ---------- */

function soulElements() {
  return {
    editor: el("soul-editor"),
    path: el("soul-path"),
    note: el("soul-note"),
    save: el("btn-soul-save"),
  };
}

/** Paint the persona pane from a `GET /soul` body. */
export function renderSoul(body) {
  const { editor, path, note } = soulElements();
  if (!editor) return;
  const ok = !!body && body.ok !== false;
  soul.loaded = ok ? body : null;

  if (path) {
    path.textContent = ok ? body.path || "" : "";
    path.title = ok ? body.path || "" : "";
  }
  // A slow GET must not overwrite text the user is in the middle of typing.
  if (ok && !soul.dirty) {
    editor.value = body.content || body.persona || "";
  }
  if (note) {
    if (!ok) {
      note.textContent = "SOUL.md unavailable — the built-in persona is in use.";
    } else if (body.is_default) {
      note.textContent =
        "No SOUL.md yet — the built-in starter is in use. Save to write this file.";
    } else {
      note.textContent =
        "Read fresh on every watcher turn, so edits apply without a restart.";
    }
  }
}

/** Fetch the persona; `render` unless the caller wants the raw body. */
export async function refreshSoul(render = true) {
  try {
    const response = await fetch(`${settings.base}/soul`, { cache: "no-store" });
    const body = await response.json().catch(() => ({}));
    if (render) renderSoul(body);
    return body;
  } catch (err) {
    if (render) renderSoul({ ok: false, error: String(err) });
    return null;
  }
}

/** Save the textarea to `POST /soul` — the persona's own endpoint. */
async function saveSoulFile() {
  const { editor, save } = soulElements();
  if (!editor) return;
  const content = editor.value;
  if (!content.trim()) {
    setStatus("SOUL.md cannot be empty — delete the file to fall back to the starter", "error");
    return;
  }
  if (save) save.disabled = true;
  setStatus("saving SOUL.md…", "");
  try {
    const response = await fetch(`${settings.base}/soul`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ content }),
    });
    const body = await response.json().catch(() => ({}));
    if (!response.ok || body.ok === false) {
      const message = body.error || `HTTP ${response.status}`;
      setStatus(message, "error");
      toast(`SOUL.md not saved: ${message}`, true);
      return;
    }
    soul.dirty = false;
    renderSoul(body);
    setStatus("SOUL.md saved — the next watcher turn uses it", "ok");
    toast("SOUL.md saved");
  } catch (err) {
    setStatus(`save failed: ${err}`, "error");
  } finally {
    if (save) save.disabled = false;
  }
}

/** Open the persona file in the user's default editor. */
async function openSoulFile() {
  const result = await call("open_soul_file");
  if (result && result.__error) {
    setStatus(`could not open SOUL.md: ${result.__error}`, "error");
    return;
  }
  if (typeof result === "string" && result) {
    toast(`Opened ${result}`);
    setStatus(`opened ${result}`, "ok");
    return;
  }
  // Not in Tauri (a browser tab): show the path so it can be copied by hand.
  const path = soul.loaded && soul.loaded.path;
  setStatus(path ? `SOUL.md lives at ${path}` : "SOUL.md path unavailable", "warn");
}

/* ---------- diffing + saving ---------- */

function currentValue(field) {
  const input = settings.inputs.get(`${field.section}.${field.key}`);
  if (!input) return undefined;
  if (field.type === "bool") return input.checked;
  if (field.type === "select" && input.value === "") {
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

function sameList(a, b) {
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
  return a.every((value, index) => Number(value) === Number(b[index]));
}

function sameStringList(a, b) {
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
  return a.every((value, index) => String(value).toLowerCase() === String(b[index]).toLowerCase());
}

/** The `ai` half of a save: routing, creates, edits, renames and removals. */
function collectAiPatch(patch, config) {
  syncCards();
  const loaded = (config.ai && config.ai.profiles) || [];
  const before = new Map(loaded.map((profile) => [profile.name, profile]));

  const activeSelect = el("set-ai-active");
  const loadedActive = (config.ai && config.ai.active_profile) || "";
  if (activeSelect && activeSelect.value && activeSelect.value !== loadedActive) {
    patch.ai = patch.ai || {};
    patch.ai.active_profile = activeSelect.value;
  }

  const visionSelect = el("set-ai-vision");
  const loadedVision = (config.vision && config.vision.profile_name) || "";
  if (visionSelect && visionSelect.value && visionSelect.value !== loadedVision) {
    patch.vision = patch.vision || {};
    patch.vision.profile_name = visionSelect.value;
  }

  const creates = [];
  const edits = [];
  const renames = [];

  for (const profile of ai.profiles) {
    const name = (profile.name || "").trim();
    const kind = profile.kind || "openai_compat";

    if (profile.isNew) {
      const entry = { name, label: profile.label || "", kind, timeout_secs: 120 };
      if (profile.timeout_secs != null) entry.timeout_secs = Number(profile.timeout_secs);
      if (kind === "hermes_cli") {
        entry.cmd = profile.cmd || "";
      } else {
        entry.endpoint = profile.endpoint || "";
        entry.model = profile.model || "";
        if (profile.max_tokens != null) entry.max_tokens = Number(profile.max_tokens);
        if (profile.system_prompt) entry.system_prompt = profile.system_prompt;
      }
      creates.push(entry);
      continue;
    }

    const was = before.get(profile.name);
    if (!was) continue;
    if (name && name !== profile.name) renames.push({ from: profile.name, to: name });

    const entry = { name: profile.name };
    let touched = false;
    for (const key of PROFILE_FIELDS[kind] || PROFILE_FIELDS.openai_compat) {
      const now = profile[key] === undefined ? null : profile[key];
      const old = was[key] === undefined ? null : was[key];
      if (same(old, now)) continue;
      // An empty optional number is "leave it alone", not 0 — sending 0 would
      // trip the server's range check and reject the whole save.
      if (key === "max_tokens" && (now === null || now === "")) continue;
      entry[key] = now;
      touched = true;
    }
    if (touched) edits.push(entry);
  }

  if (creates.length) {
    patch.ai = patch.ai || {};
    patch.ai.create = creates;
  }
  if (edits.length) {
    patch.ai = patch.ai || {};
    patch.ai.profiles = edits;
  }
  if (renames.length) {
    patch.ai = patch.ai || {};
    patch.ai.rename = renames;
  }
  if (ai.removed.length) {
    patch.ai = patch.ai || {};
    patch.ai.remove = ai.removed.slice();
  }

  const portsInput = el("set-ai-ports");
  if (portsInput) {
    const ports = parsePorts(portsInput.value);
    if (!sameList(config.discovery_ports || [], ports)) {
      patch.discovery_ports = ports;
    }
  }
  const hostsInput = el("set-ai-hosts");
  if (hostsInput) {
    const hosts = parseHosts(hostsInput.value);
    if (!sameStringList(config.discovery_hosts || [], hosts)) {
      patch.discovery_hosts = hosts;
    }
  }
}

/** Only what the user actually changed, in the shape `POST /config` wants. */
export function collectPatch() {
  const config = (settings.loaded && settings.loaded.settings) || {};
  const patch = {};

  for (const field of FIELDS) {
    const next = currentValue(field);
    if (next === undefined) continue;
    const before = getPath(config[field.section], field.key);
    if (same(before, next)) continue;
    if (!patch[field.section]) patch[field.section] = {};
    setPath(patch[field.section], field.key, next);
  }

  collectAiPatch(patch, config);
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

/** The Settings entry points: the ACTIONS button and the titlebar gear. */
function settingsButtons() {
  return [el("btn-settings"), el("btn-settings-top")].filter(Boolean);
}

export function toggleSettings(force) {
  const node = el("settings-modal");
  if (!node) return false;
  const open = typeof force === "boolean" ? force : node.classList.contains("hidden");
  node.classList.toggle("hidden", !open);
  node.setAttribute("aria-hidden", String(!open));
  // Both entry points mirror the modal state, so the gear lights up whether
  // the modal was opened from the panel or from the overlay header.
  for (const button of settingsButtons()) {
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
  // Opening the SOUL tab is a deliberate look at the file: re-read it, unless
  // the user has unsaved edits in the textarea.
  if (name === "soul" && !soul.dirty) refreshSoul();
}

/** Wire the modal, hydrate once, and follow the file from outside edits too. */
export async function setupSettings() {
  if (settings.ready) return;
  if (!el("settings-modal")) return;
  if (!buildForm()) return;

  for (const button of settingsButtons()) {
    button.addEventListener("click", () => toggleSettings());
  }
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
      ai: () => ({
        profiles: ai.profiles.map((profile) => ({ ...profile })),
        removed: ai.removed.slice(),
        ports: ai.ports.slice(),
        hosts: ai.hosts.slice(),
        active: ai.selectedActive,
        vision: ai.selectedVision,
        discovered: ai.discovered,
      }),
      discover: () => discoverModels(),
      addProfile: () => addProfile(),
    });
    window.__avatarDebug.openSettings = () => toggleSettings(true);
  }
}

/** Registered by main.js so a save can refresh the panel's own badges. */
export function onSettingsSaved(handler) {
  settings.onSaved = handler;
}
