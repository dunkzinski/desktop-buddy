/**
 * desktop-buddy — the scavenging walk
 *
 * The **activity** belongs to the survival plugin: how long a run lasts (10-30
 * s), what it finds, and the report it speaks all live in the guest, because a
 * Wasm sandbox has no way to see the desktop. What the guest *cannot* do is move
 * the avatar or make it look like it is digging, so this module answers the one
 * question `main.js` needs:
 *
 *   > where on this screen is there something to rummage around?
 *
 * The answer comes from `GET /context`, not from guesswork: `visible_windows`
 * already carries each open window's rectangle in CSS pixels of the *virtual
 * desktop* — the same space the full-screen overlay (and therefore `avatar.x/y`)
 * lives in — so an anchor can be put on a real window's bottom edge instead of
 * at a hard-coded pair of coordinates. With nothing open, it degrades to a few
 * points around the screen so the walk still reads as searching.
 *
 * Deliberately a leaf-ish module: it imports `ipc` only, takes the viewport as
 * an argument instead of reaching into `state.js`, and never touches
 * `avatar.mode` — `main.js` owns the walk.
 */

import { call } from "./ipc.js";

const DEFAULT_BASE = "http://127.0.0.1:8765";
/** How many windows one run walks to. */
export const MAX_TARGETS = 3;
/** Smallest window worth crossing the screen for, in CSS px. */
const MIN_WINDOW = 140;

/**
 * A short, sayable name for a window: the process stem when it is tidy
 * (`Code.exe` → "Code", `hermes.exe` → "Hermes"), otherwise the last segment of
 * the title (`"main.rs - Code"` → "Code"). The guest quotes this in a comment,
 * so it is capped and stripped of control characters.
 */
export function windowLabel(win) {
  const process = String((win && win.process_name) || "").trim();
  const stem = process.replace(/\.exe$/i, "");
  const title = String((win && win.title) || "").trim();
  const tail = title ? title.split(/\s+[-\u2013\u2014]\s+/).pop().trim() : "";
  const name = stem && stem.length <= 24 ? stem : tail;
  if (!name) return "";
  const capped = name.length > 32 ? `${name.slice(0, 31)}…` : name;
  return capped.charAt(0).toUpperCase() + capped.slice(1);
}

/** The API base the app actually bound (it can slide off 8765). */
async function apiBase() {
  try {
    const info = await call("get_runtime_info");
    const url = info && typeof info.api_url === "string" ? info.api_url : "";
    if (/^https?:\/\//.test(url)) return url;
  } catch (_) {
    // Not in Tauri, or the command is not up yet: fall back to the default.
  }
  return DEFAULT_BASE;
}

/** `GET /context`, or `null` when the API cannot be reached at all. */
async function desktopContext() {
  try {
    const res = await fetch(`${await apiBase()}/context`, { cache: "no-store" });
    if (!res.ok) return null;
    return await res.json();
  } catch (_) {
    return null;
  }
}

/**
 * One anchor per open window: just inside its bottom edge, a little off-centre.
 *
 * The bottom edge is where a desktop buddy stands, and the random horizontal
 * offset keeps two runs from tracing exactly the same line. Rect fields are
 * validated rather than trusted: a `null` rect (a non-Windows backend) or a
 * sliver of a window is skipped instead of producing a NaN walk target.
 */
export function windowAnchors(context, limit = MAX_TARGETS) {
  const windows = context && Array.isArray(context.visible_windows)
    ? context.visible_windows
    : [];
  const anchors = [];
  for (const win of windows) {
    const rect = win && win.rect;
    if (!rect) continue;
    const x = Number(rect.x);
    const y = Number(rect.y);
    const width = Number(rect.width);
    const height = Number(rect.height);
    if (![x, y, width, height].every(Number.isFinite)) continue;
    if (width < MIN_WINDOW || height < MIN_WINDOW) continue;
    anchors.push({
      x: x + width * (0.2 + Math.random() * 0.6),
      y: y + height - 8,
      // The name he may mutter ("the Hermes window"), not the raw title.
      label: windowLabel(win),
    });
    if (anchors.length >= limit) break;
  }
  return anchors;
}

/**
 * Points around the screen, for when nothing is open (or `/context` is down).
 * Still random per run: "around the screen" rather than three fixed spots.
 */
export function fallbackAnchors(viewport, limit = MAX_TARGETS) {
  const width = Math.max(1, Number(viewport && viewport.w) || 0);
  const height = Math.max(1, Number(viewport && viewport.h) || 0);
  const out = [];
  for (let i = 0; i < limit; i += 1) {
    out.push({
      x: width * (0.15 + Math.random() * 0.7),
      y: height * (0.35 + Math.random() * 0.45),
      label: "",
    });
  }
  return out;
}

/**
 * Where to walk for a scavenging run: one anchor per open window, front-most
 * first, or a spread of screen points when there is nothing to rummage around.
 *
 * Never rejects — a dead API must not leave the avatar standing still while the
 * guest's timer runs.
 */
export async function scavengeAnchors(viewport, limit = MAX_TARGETS) {
  const context = await desktopContext();
  const anchors = windowAnchors(context, limit);
  return anchors.length ? anchors : fallbackAnchors(viewport, limit);
}

/**
 * `GET /scavenge`'s session, or `null` when the API cannot be reached.
 *
 * The page polls this to keep the walk honest: it is the same read the card
 * makes twice a second, and finishing a run from a `GET` is idempotent in the
 * guest, so a poll can never double-bank loot.
 */
export async function scavengeSession() {
  try {
    const base = await apiBase();
    const reply = await fetch(`${base}/scavenge`);
    if (!reply.ok) return null;
    const payload = await reply.json();
    return payload && payload.session ? payload.session : null;
  } catch (_) {
    return null;
  }
}

/**
 * Ask the plugin whether he says something about this window.
 *
 * The *cadence and the wording* are the guest's (run cap, gap, chance roll,
 * phrase table); this only supplies the fact the guest cannot see — which
 * window he is standing at. A declined request is a normal 200, so there is
 * nothing to handle; a missed mutter is never worth a console line.
 */
export async function scavengeComment(label) {
  try {
    const base = await apiBase();
    await fetch(`${base}/scavenge/comment`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(label ? { window: label } : {}),
    });
  } catch (_) {
    // Offline, or the run ended between the click and the request: silent.
  }
}
