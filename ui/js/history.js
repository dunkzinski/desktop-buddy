/**
 * desktop-buddy — the History drawer (in-memory conversation log)
 *
 * Depends on: state.js (the mirrored `chat` state and `toast`), ipc.js (the
 * `get_chat_history` / `set_clipboard` commands and the two chat events) and
 * rig.js (`wireRegions`, because the drawer is a floating card that must
 * publish its own hit region while it is open).
 *
 * Nothing here is persisted: Rust owns the log (`GET /history`,
 * `get_chat_history`) and this module only mirrors it. Speech bubbles are
 * transient — the drawer is what keeps a session's turns visible after the
 * bubble has auto-dismissed.
 */

import { chat, toast } from "./state.js";
import { call, listen } from "./ipc.js";
import { wireRegions } from "./rig.js";

/** Mirrors `CHAT_HISTORY_CAPACITY` in lib.rs; the drawer never renders more. */
const CHAT_LIMIT = 50;

/** Longest `details` block a row will render (mirrors speech.js / lib.rs). */
const MAX_DETAILS_CHARS = 8000;

/* ---------- helpers ---------- */

function drawer() {
  return document.getElementById("history-drawer");
}

function listEl() {
  return document.getElementById("history-list");
}

/** `14:03` in the machine's locale — a transcript is read by time of day. */
function formatTime(tsMs) {
  const value = Number(tsMs);
  if (!Number.isFinite(value) || value <= 0) return "";
  try {
    return new Date(value).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch (err) {
    return "";
  }
}

/** Is the list scrolled anywhere near the newest turn? */
function atBottom(list) {
  return list.scrollHeight - list.scrollTop - list.clientHeight < 40;
}

function scrollToNewest(list) {
  list.scrollTop = list.scrollHeight;
}

/* ---------- rendering ---------- */

async function copyText(text) {
  if (!text) return false;
  const result = await call("set_clipboard", { text });
  if (result && !result.__error) return true;
  if (typeof navigator !== "undefined" && navigator.clipboard && navigator.clipboard.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (err) {
      console.error("[avatar] clipboard write failed:", err);
    }
  }
  return false;
}

/**
 * One turn as a DOM node.
 *
 * Everything is built with `textContent` / `createElement` rather than an HTML
 * string: a reply may quote arbitrary code and a diff may contain `<`, which
 * must be shown literally and never parsed.
 */
function messageRow(message) {
  const row = document.createElement("article");
  row.className = "hist-row " + (message.role === "assistant" ? "assistant" : "user");
  row.dataset.id = String(message.id);

  const meta = document.createElement("div");
  meta.className = "hist-meta";
  const who = document.createElement("span");
  who.className = "hist-who";
  who.textContent = message.role === "assistant" ? "Avatar" : "You";
  meta.appendChild(who);
  if (message.expression) {
    const mood = document.createElement("span");
    mood.className = "hist-mood";
    mood.textContent = message.expression;
    meta.appendChild(mood);
  }
  const when = document.createElement("span");
  when.className = "hist-when";
  when.textContent = formatTime(message.ts_ms);
  meta.appendChild(when);
  row.appendChild(meta);

  const body = document.createElement("div");
  body.className = "hist-text";
  body.textContent = String(message.text == null ? "" : message.text);
  row.appendChild(body);

  const details = String(message.details == null ? "" : message.details).slice(0, MAX_DETAILS_CHARS);
  if (details) {
    const expanded = !!chat.expanded[message.id];
    const actions = document.createElement("div");
    actions.className = "hist-actions";

    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "hist-btn";
    toggle.textContent = expanded ? "Details ▴" : "Details ▾";
    toggle.title = expanded ? "Hide the full text" : "Show the full code, diff or log";
    toggle.addEventListener("click", () => {
      chat.expanded[message.id] = !expanded;
      // Expanding a block must not yank the list to the bottom.
      renderHistory({ keepScroll: true });
    });
    actions.appendChild(toggle);

    // The bubble's Copy copies the details block; so does the row's.
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "hist-btn";
    copy.textContent = "Copy";
    copy.title = "Copy the full text to the clipboard";
    copy.addEventListener("click", async () => {
      const ok = await copyText(details);
      toast(ok ? "Copied to clipboard" : "Copy failed", !ok);
    });
    actions.appendChild(copy);
    row.appendChild(actions);

    if (expanded) {
      const pre = document.createElement("pre");
      pre.className = "hist-details";
      pre.textContent = details;
      row.appendChild(pre);
    }
  }

  return row;
}

/**
 * Repaint the whole list from `chat.messages`. Cheap enough at 50 rows, and it
 * keeps the expanded/`Details` state and the DOM in step without diffing.
 *
 * `options.keepScroll` restores the old scroll offset instead of jumping to the
 * newest turn (used when a live message lands while the user is reading back).
 */
export function renderHistory(options) {
  const keepScroll = !!(options && options.keepScroll);
  const list = listEl();
  if (!list) return;
  const previousTop = list.scrollTop;

  list.innerHTML = "";
  if (!chat.messages.length) {
    const empty = document.createElement("div");
    empty.className = "hist-empty";
    empty.id = "history-empty";
    empty.textContent = chat.hydrated
      ? "No messages yet — ask the avatar something."
      : "Loading…";
    list.appendChild(empty);
  } else {
    for (const message of chat.messages) list.appendChild(messageRow(message));
  }

  const count = document.getElementById("history-count");
  if (count) count.textContent = String(chat.messages.length);
  if (keepScroll) list.scrollTop = previousTop;
  else scrollToNewest(list);
}

/* ---------- live data ---------- */

/**
 * Append one turn from the `chat-message` event.
 *
 * Deduped by `id`: the hydrate call and the event stream can race (a message
 * appended just before `get_chat_history` answers arrives twice), and a
 * duplicate row in a transcript is a lie about what was said.
 */
export function appendChatMessage(message) {
  if (!message || typeof message !== "object") return false;
  const id = Number(message.id);
  if (!Number.isFinite(id)) return false;
  if (chat.messages.some((existing) => existing.id === id)) return false;

  const list = listEl();
  const stick = !list || atBottom(list);
  chat.messages.push({
    id,
    role: message.role === "assistant" ? "assistant" : "user",
    text: String(message.text == null ? "" : message.text),
    details: message.details == null ? undefined : String(message.details),
    expression: message.expression == null ? undefined : String(message.expression),
    ts_ms: Number(message.ts_ms) || 0,
  });
  while (chat.messages.length > CHAT_LIMIT) chat.messages.shift();

  if (chat.open) {
    // Follow the conversation only when the user was already at the newest
    // turn: scrolling someone away from a message they are reading is worse
    // than making them scroll.
    renderHistory({ keepScroll: !stick });
  }
  return true;
}

/** A new chat emptied the server-side log: drop the rows. */
export function clearChatHistory(count) {
  chat.messages = [];
  chat.expanded = {};
  chat.hydrated = true;
  renderHistory();
  if (Number(count) > 0 && chat.open) toast("Conversation cleared");
}

/** Fill the drawer from `get_chat_history` (the load-time hydrate). */
export async function hydrateHistory() {
  const body = await call("get_chat_history");
  if (body && !body.__error && Array.isArray(body.messages)) {
    // Keep the local order but trust the server's list (it is the truth).
    chat.messages = body.messages.slice(-CHAT_LIMIT);
  }
  chat.hydrated = true;
  renderHistory();
}

/* ---------- drawer ---------- */

/** Is the drawer on screen? */
export function historyOpen() {
  return !!chat.open;
}

/**
 * Show/hide the drawer. Returns the new state.
 *
 * The hit region is republished immediately: without it the card would be
 * visible but click-through until the next 200 ms publish, and the user could
 * read the transcript but not click Details or Copy.
 */
export function toggleHistoryDrawer(force) {
  const el = drawer();
  if (!el) return false;
  const next = typeof force === "boolean" ? force : !chat.open;

  chat.open = next;
  el.classList.toggle("hidden", !next);
  el.setAttribute("aria-hidden", next ? "false" : "true");

  for (const id of ["btn-history", "command-history"]) {
    const button = document.getElementById(id);
    if (!button) continue;
    button.classList.toggle("active", next);
    button.setAttribute("aria-pressed", next ? "true" : "false");
  }

  if (next) renderHistory();
  wireRegions();
  return next;
}

/* ---------- wiring ---------- */

/**
 * Bind the toggle buttons, the two Tauri events and the load-time hydrate.
 *
 * `onNewChat` is the page's own `resetSession` (passed in by `main.js`): the
 * drawer's **New chat** button must do exactly what the command bar's ↺ does —
 * rotate the watcher's session, drop the bubble and empty this transcript — and
 * importing `main.js` from here would be a cycle.
 */
export function setupHistory(options) {
  const settings = options || {};
  const el = drawer();
  if (!el) return;

  const toggle = () => toggleHistoryDrawer();
  for (const id of ["btn-history", "command-history"]) {
    const button = document.getElementById(id);
    if (button) button.addEventListener("click", toggle);
  }
  const close = document.getElementById("btn-history-close");
  if (close) close.addEventListener("click", () => toggleHistoryDrawer(false));

  const clear = document.getElementById("btn-history-clear");
  if (clear) {
    clear.addEventListener("click", () => {
      if (typeof settings.onNewChat === "function") settings.onNewChat();
    });
  }

  // Live turns: every user prompt and avatar reply, from either end (page
  // command or HTTP route — Rust emits this from the one place that records).
  listen("chat-message", (event) => appendChatMessage(event.payload))
    .catch((err) => console.error("[avatar] chat-message listener failed:", err));
  listen("chat-history-cleared", (event) => {
    const payload = event && event.payload ? event.payload : {};
    clearChatHistory(payload.count);
  }).catch((err) => console.error("[avatar] chat-history-cleared listener failed:", err));

  // Hydrate once on load: the app may already have turns from a previous page
  // reload in this same process (the log lives in Rust, not the page).
  hydrateHistory().catch((err) => console.error("[avatar] history hydrate failed:", err));
}
