/**
 * desktop-buddy — plugin-backed UI gate
 *
 * The app is allowed to start with `plugins/` empty (or with one `.wasm`
 * deleted), and `GET /plugins` is the only honest answer to "is this feature
 * installed?". This module reads that registry once and reveals exactly the
 * chrome whose plugin reported `active`; everything gated ships **hidden** in
 * `index.html`, so a missing plugin can never flash an option that does
 * nothing.
 *
 * Two rules:
 *   * only `active` counts. A plugin that failed `init` (or was parked after
 *     repeated tick errors) does not serve its routes, so its button would be a
 *     dead end — it stays hidden.
 *   * the registry can change while the page is open (the host emits the
 *     `plugins` frame), so the same `applyPluginStatus` runs again on that
 *     event.
 *
 * Imports `ipc` only — no cycles, and nothing here needs the rig.
 */

import { call, listen } from "./ipc.js";

const DEFAULT_BASE = "http://127.0.0.1:8765";
/** The API may still be binding when the page loads; a few quick retries beat
 *  a first paint that permanently hides a feature. */
const MAX_ATTEMPTS = 5;
const RETRY_MS = 400;

/** Plugin id -> lifecycle state (`active` / `failed` / …), last registry read. */
const states = new Map();
let base = DEFAULT_BASE;
let ready = false;

/** Resolve the API base the app actually bound (it can slide off 8765). */
async function resolveBase() {
  const info = await call("get_runtime_info");
  const url = info && typeof info.api_url === "string" ? info.api_url : "";
  base = /^https?:\/\//.test(url) ? url : DEFAULT_BASE;
}

/** `GET /plugins`. Throws on a non-2xx so the retry loop can see it. */
async function readRegistry() {
  const res = await fetch(`${base}/plugins`, { cache: "no-store" });
  if (!res.ok) throw new Error(`GET /plugins answered ${res.status}`);
  return res.json();
}

/**
 * Show/hide every element that declares `data-plugin="<id>"`.
 *
 * Chrome only — buttons, tabs. A card or panel keeps its own `.hidden` logic;
 * gating one here would pop it open the moment its plugin is active.
 */
export function gatePluginUi() {
  for (const node of document.querySelectorAll("[data-plugin]")) {
    const id = node.dataset.plugin;
    node.classList.toggle("hidden", !pluginActive(id));
    node.setAttribute("aria-hidden", String(!pluginActive(id)));
  }
}

/** Is `id` registered and serving right now? */
export function pluginActive(id) {
  return states.get(id) === "active";
}

/** Copy of the last registry read, for the debug surface. */
export function pluginStates() {
  return new Map(states);
}

/**
 * Adopt a `GET /plugins` body (or the `plugins` event payload) and re-gate.
 *
 * Ids missing from the body are treated as absent: a plugin the host does not
 * list is a plugin whose routes do not exist.
 */
export function applyPluginStatus(body) {
  states.clear();
  const list = body && Array.isArray(body.plugins) ? body.plugins : [];
  for (const entry of list) {
    if (entry && typeof entry.id === "string" && entry.id) {
      states.set(entry.id, String(entry.state || ""));
    }
  }
  gatePluginUi();
  return pluginStates();
}

/**
 * Read the registry and gate the chrome. Idempotent: a second call is a no-op
 * once a read has succeeded (the `plugins` event keeps it current after that).
 */
export async function refreshPluginStatus() {
  if (ready) return pluginStates();
  await resolveBase();
  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt += 1) {
    try {
      const body = await readRegistry();
      ready = true;
      return applyPluginStatus(body);
    } catch (err) {
      if (attempt === MAX_ATTEMPTS) throw err;
      await new Promise((resolve) => setTimeout(resolve, RETRY_MS));
    }
  }
  return pluginStates();
}

/**
 * Follow the registry for the page's lifetime: the host emits `plugins` when
 * the set of installed plugins changes, so the gate is not a one-shot read.
 */
export async function setupPluginGate() {
  await listen("plugins", (event) => {
    const payload = event && event.payload ? event.payload : null;
    if (payload) applyPluginStatus(payload);
  });
  try {
    return await refreshPluginStatus();
  } catch (err) {
    console.error("[avatar] plugin registry:", err);
    // Fail closed: an unreadable registry leaves the gated chrome hidden rather
    // than showing options that may not work.
    return pluginStates();
  }
}
