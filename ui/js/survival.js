/**
 * char-rig — survival widget
 *
 * The panel's hunger/saturation readout plus the Feed button. It is a *client*
 * of the HTTP API on purpose: the numbers it shows are the ones `GET /survival`
 * answers, and the button really does `POST /survival/feed`, so the widget can
 * never disagree with what an agent sees.
 *
 * Two update paths, deliberately:
 *   * the `survival` / `survival-meal` Tauri events (telemetry plugin and every
 *     meal, pushed as they happen), and
 *   * a slow `GET /survival` poll, because the telemetry plugin can be disabled
 *     in `watcher_rules.json` and a stalled widget is worse than a stale one.
 *
 * Imports `ipc` and `state` only — no cycles, and nothing here needs the rig.
 */

import { call, listen } from "./ipc.js";
import { toast } from "./state.js";

const DEFAULT_BASE = "http://127.0.0.1:8765";
const POLL_MS = 15000;

const survival = {
  base: DEFAULT_BASE,
  /** Catalog from `GET /foods`, so the picker can never send an unknown id. */
  foods: [],
  /** Last `GET /survival` body, for the debug surface and the note text. */
  status: null,
  ready: false,
};

const el = (id) => document.getElementById(id);

const card = () => document.getElementById("survival-card");

/** Is the pop-out card on screen? */
export function survivalCardOpen() {
  const node = card();
  return !!node && !node.classList.contains("hidden");
}

/**
 * Open/close the pop-out card. `force` mirrors `toggleHistoryDrawer(false)`, so
 * the Esc chain and the ✕ button share one code path.
 *
 * The panel's Survival button carries the pressed state — it is the only way
 * back in once the card is closed.
 */
export function toggleSurvivalCard(force) {
  const node = card();
  if (!node) return false;
  const open = typeof force === "boolean" ? force : node.classList.contains("hidden");
  node.classList.toggle("hidden", !open);
  node.setAttribute("aria-hidden", String(!open));

  const button = el("btn-survival");
  if (button) {
    button.classList.toggle("active", open);
    button.setAttribute("aria-pressed", String(open));
  }
  // Opened long after the last poll? Then the numbers on show must be re-read.
  if (open) refreshSurvival();
  return open;
}

/** Resolve the API base the app actually bound (it can slide off 8765). */
async function resolveBase() {
  const info = await call("get_runtime_info");
  const url = info && typeof info.api_url === "string" ? info.api_url : "";
  survival.base = /^https?:\/\//.test(url) ? url : DEFAULT_BASE;
}

function setNote(text, isError) {
  const note = el("survival-note");
  if (!note) return;
  note.textContent = text || "";
  note.classList.toggle("error", !!isError);
}

/** Fill the food picker from the catalog. Keeps the current choice if it survived. */
function renderFoods(foods) {
  const select = el("survival-food");
  if (!select) return;
  const previous = select.value;
  select.innerHTML = "";
  for (const food of foods || []) {
    const option = document.createElement("option");
    option.value = food.id;
    option.textContent = `${food.label} · +${Math.round(food.nutrition)}`;
    option.title = food.effects && food.effects.length
      ? `effects: ${food.effects.map((e) => e.expression || e.kind).join(", ")}`
      : `saturation ratio ${food.saturation_ratio}`;
    select.appendChild(option);
  }
  if (previous && (foods || []).some((f) => f.id === previous)) select.value = previous;
  const button = el("btn-feed");
  if (button) button.disabled = !select.value;
}

function paintBar(kind, value) {
  const bar = el(`survival-${kind}-bar`);
  const label = el(`survival-${kind}`);
  const text = Number.isFinite(value) ? `${Math.round(value * 10) / 10}` : "—";
  if (label) label.textContent = text;
  if (bar) {
    bar.style.width = `${Number.isFinite(value) ? Math.max(0, Math.min(100, value)) : 0}%`;
    bar.parentElement?.setAttribute("aria-valuenow", text);
  }
}

/**
 * Flatten the three shapes this widget can receive into one:
 *
 *   * `GET /survival`     — flat `hunger`/`saturation`/`tier` *plus* `stats`
 *   * `survival` event    — `{stats: {...}, source}` (telemetry plugin)
 *   * `survival-threshold`— flat numbers with `to` instead of `tier`
 *
 * Without this the telemetry frame paints `NaN` and the bars go empty, which
 * reads as a starving avatar rather than a parsing bug.
 */
function normalize(raw) {
  if (!raw || typeof raw !== "object") return null;
  if (raw.ok === false) return raw; // `GET /survival` before install
  const stats = raw.stats && typeof raw.stats === "object" ? raw.stats : raw;
  const pick = (key) => (raw[key] !== undefined ? raw[key] : stats[key]);
  const hunger = pick("hunger");
  if (hunger === undefined) return null;
  return {
    ok: true,
    hunger,
    saturation: pick("saturation"),
    tier: pick("tier") !== undefined ? pick("tier") : raw.to,
    label: pick("label"),
    expression: pick("expression"),
  };
}

/**
 * Paint one status frame. Accepts any of the three shapes above; `ok: false`
 * means the plugin never initialised, so say so instead of drawing zeroes that
 * look like a starving avatar.
 */
export function renderSurvival(raw) {
  const status = normalize(raw);
  survival.status = status;
  const tier = el("survival-tier");

  if (!status || status.ok === false) {
    if (tier) {
      tier.textContent = "off";
      tier.className = "survival-tier off";
      tier.title = (status && status.reason) || "survival is not running";
    }
    paintBar("hunger", NaN);
    paintBar("saturation", NaN);
    return;
  }

  const hunger = Number(status.hunger);
  const saturation = Number(status.saturation);
  const tierName = typeof status.tier === "string" ? status.tier : "";

  if (tier) {
    tier.textContent = tierName || "—";
    tier.className = `survival-tier ${tierName || "off"}`;
    tier.title = `${status.label || tierName}: hunger ${Math.round(hunger)} (${status.expression || "neutral"})`;
  }

  paintBar("hunger", hunger);
  paintBar("saturation", saturation);
}

/** `GET /survival` — the authoritative read. */
async function refreshSurvival() {
  try {
    const res = await fetch(`${survival.base}/survival`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    renderSurvival(await res.json());
  } catch (err) {
    setNote(`API unreachable: ${err.message}`, true);
  }
}

async function refreshFoods() {
  try {
    const res = await fetch(`${survival.base}/foods`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const body = await res.json();
    survival.foods = Array.isArray(body.foods) ? body.foods : [];
    renderFoods(survival.foods);
  } catch (err) {
    setNote(`Food catalog unreachable: ${err.message}`, true);
  }
}

/**
 * The Feed button: a real `POST /survival/feed`.
 *
 * 202/200 is a meal, 409 is a *refusal* (full, or still digesting) and carries
 * usable prose in `error`, 400 is an unknown id — each one is surfaced, because
 * a silent no-op after clicking a button is the worst outcome here.
 */
async function feed() {
  const select = el("survival-food");
  const button = el("btn-feed");
  const food = select ? select.value : "";
  if (!food) return;

  if (button) {
    button.disabled = true;
    button.textContent = "Feeding…";
  }
  setNote("");

  try {
    const res = await fetch(`${survival.base}/survival/feed`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ food }),
    });
    const body = await res.json().catch(() => ({}));

    if (res.ok) {
      const before = typeof body.tier_before === "string" ? body.tier_before : "";
      const after = typeof body.tier_after === "string" ? body.tier_after : "";
      const gained = Number(body.hunger_after) - Number(body.hunger_before);
      const moving = before && after && before !== after ? ` · ${before} → ${after}` : "";
      setNote(`${body.label || food} eaten (+${Math.round(gained * 10) / 10} hunger)${moving}`);
      toast(`${body.label || food} eaten`);
    } else {
      // 409 = refused (too_full / still_digesting), 400 = unknown id.
      setNote(body.error || `feed failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setNote(`feed failed: ${err.message}`, true);
  } finally {
    if (button) {
      button.disabled = !food;
      button.textContent = "Feed";
    }
    // The events repaint us within a tick; this covers a muted telemetry plugin.
    refreshSurvival();
  }
}

/** Wire the widget, hydrate it, then follow the events. Safe to call twice. */
export async function setupSurvival() {
  if (survival.ready) return;
  const button = el("btn-feed");
  if (!button) return; // the panel markup is missing — nothing to do.

  button.addEventListener("click", (event) => {
    event.preventDefault();
    feed();
  });

  // The pop-out: the panel button toggles the card, the ✕ and Esc close it.
  el("btn-survival")?.addEventListener("click", () => toggleSurvivalCard());
  el("btn-survival-close")?.addEventListener("click", () => toggleSurvivalCard(false));

  await resolveBase();
  await Promise.all([refreshFoods(), refreshSurvival()]);

  // Pushed updates: the telemetry frame and every meal outcome.
  await listen("survival", (event) => renderSurvival(event.payload));
  // A tier crossing carries a tier name in `to`, not the full stats — take the
  // authoritative read rather than repainting from a partial frame.
  await listen("survival-threshold", () => refreshSurvival());
  await listen("survival-meal", (event) => {
    const payload = event.payload || {};
    if (payload.accepted === false) return; // the button already explained it
    // The meal payload carries no full stats — ask for the authoritative ones.
    refreshSurvival();
  });

  setInterval(refreshSurvival, POLL_MS);
  survival.ready = true;

  // CDP surface: read the live numbers without a screenshot.
  if (window.__avatarDebug) window.__avatarDebug.survival = () => ({
    base: survival.base,
    ready: survival.ready,
    open: survivalCardOpen(),
    foods: survival.foods.map((f) => f.id),
    tier: survival.status ? survival.status.tier : null,
    hunger: survival.status ? survival.status.hunger : null,
    saturation: survival.status ? survival.status.saturation : null,
  });
}
