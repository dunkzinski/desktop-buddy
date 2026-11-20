/**
 * desktop-buddy — survival widget
 *
 * The panel's hunger/saturation readout plus the Feed button. It is a *client*
 * of the HTTP API on purpose: the numbers it shows are the ones `GET /survival`
 * answers, and the button really does `POST /survival/feed`, so the widget can
 * never disagree with what an agent sees.
 *
 * The same card carries four panes — Status, Bag, Fishing and Store — and each
 * one is the same kind of client: the bag is `GET /inventory`, the mini-game is
 * `GET /fishing`, and the shop is `GET /store` with real `POST /store/buy` and
 * `POST /store/sell` trades. Nothing here decides an outcome; it mirrors the
 * plugin and reports its refusals verbatim.
 *
 * Two update paths, deliberately:
 *   * the `survival` / `survival-meal` / `store-transaction` Tauri events
 *     (telemetry plugin and every meal or trade, pushed as they happen), and
 *   * a slow `GET /survival` poll, because the telemetry plugin can be disabled
 *     in `watcher_rules.json` and a stalled widget is worse than a stale one.
 *
 * Imports `ipc` and `state` only — no cycles, and nothing here needs the rig.
 */

import { call, listen } from "./ipc.js";
import { reactExpression, toast } from "./state.js";

const DEFAULT_BASE = "http://127.0.0.1:8765";
const POLL_MS = 15000;
/** How often the fishing pane re-reads `GET /fishing` while a cast is out. The
 *  host tick is 1 s, so a poll is what makes the bobber react promptly. */
const FISHING_POLL_MS = 120;

const survival = {
  base: DEFAULT_BASE,
  /** Catalog from `GET /foods`, so the picker can never send an unknown id. */
  foods: [],
  /** Recipe book from `GET /recipes`, so the Cook row can never send a bad id. */
  recipes: [],
  /** Last `GET /survival` body, for the debug surface and the note text. */
  status: null,
  ready: false,
  /** Which of the card's four panes is showing: status | bag | fishing | store. */
  tab: "status",
  /** Last `GET /inventory` body. */
  inventory: null,
  /** Catalog id of the slot the user picked, or null. */
  picked: null,
  /** Last `GET /store` body: `{coins, catalog, categories, inventory, …}`. */
  store: null,
  /** Catalog id of the shelf row the user picked, or null. */
  storePicked: null,
};

/**
 * The fishing mini-game's client half.
 *
 * The *authority* is the plugin: it decides when the fish bites, whether the
 * press was in the window, and — via its own clock — whether the reel marker
 * was inside the sweet spot. This object only mirrors the session so the pane
 * can draw it, which is why every action below is a POST followed by a re-read
 * rather than a local state transition.
 */
const fishing = {
  session: null,
  config: null,
  timer: 0,
  raf: 0,
  /** `casts-catches-escapes` of the last result spelled out, to dedupe the
   *  note when both the SSE frame and a poll deliver the same outcome. */
  resultKey: "",
  /** Last phase rendered, so a transition only re-animates once. */
  phase: "idle",
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
  if (open) {
    refreshSurvival();
    refreshInventory();
    refreshStore();
    if (survival.tab === "fishing") {
      ensureFishingPoll();
      pollFishing();
    }
  }
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

/** Label for a catalog id, falling back to the raw id. */
function foodLabel(id) {
  const food = (survival.foods || []).find((f) => f.id === id);
  return food ? food.label : id;
}

/**
 * Fill the food picker from the catalog. Keeps the current choice if it survived.
 *
 * Crafted dishes are deliberately left out: they only exist once a recipe has
 * been cooked, so listing them here would let a user eat the omelette without
 * ever gathering the eggs. Raw ingredients and ready-made drinks (coffee) stay,
 * and each raw line shows how many the bag already holds — the same numbers the
 * Cook row and the Store tab work from.
 */
function renderFoods(foods) {
  const select = el("survival-food");
  if (!select) return;
  const previous = select.value;
  const feedable = (foods || []).filter((food) => !food.crafted_via);
  const owned = new Map(
    ((survival.inventory && survival.inventory.totals) || []).map((entry) => [entry.id, entry.count]),
  );
  // Foods that live in the bag are drunk/eaten *from* it (POST /survival/feed
  // spends one), so their option has to say how many are left. The shelf meals
  // (cake, coffee, sandwich …) have no bag entry and are eaten as-is.
  const bagFoods = new Set(
    ((survival.inventory && survival.inventory.catalog) || [])
      .filter((item) => item.food)
      .map((item) => item.food),
  );

  const groups = [
    ["Ingredients", feedable.filter((food) => food.kind === "raw")],
    ["Meals & drinks", feedable.filter((food) => food.kind !== "raw")],
  ];

  select.innerHTML = "";
  for (const [label, items] of groups) {
    if (!items.length) continue;
    const group = document.createElement("optgroup");
    group.label = label;
    for (const food of items) {
      const option = document.createElement("option");
      option.value = food.id;
      const held = owned.get(food.id) || 0;
      const stock = bagFoods.has(food.id) ? (held ? ` · own ${held}` : " · none in bag") : "";
      option.textContent = `${food.label} · +${Math.round(food.nutrition)}${stock}`;
      option.title = food.effects && food.effects.length
        ? `effects: ${food.effects.map((e) => e.expression || e.kind).join(", ")}`
        : `saturation ratio ${food.saturation_ratio}`;
      group.appendChild(option);
    }
    select.appendChild(group);
  }

  if (previous && feedable.some((f) => f.id === previous)) select.value = previous;
  const button = el("btn-feed");
  if (button) button.disabled = !select.value;
  syncFoodButtons();
}

/** Keep the Feed/Crave buttons in step with the food picker. */
function syncFoodButtons() {
  const select = el("survival-food");
  const hasFood = !!(select && select.value);
  const feed = el("btn-feed");
  if (feed) feed.disabled = !hasFood;
  const crave = el("btn-crave");
  if (crave) crave.disabled = !hasFood;
}

/**
 * Fill the recipe picker from `GET /recipes`.
 *
 * The book carries the bag now — `can_cook`, `missing` and a `held` count per
 * line — because cooking draws from the inventory. The option text shows what a
 * recipe still needs, and `syncCraftButton` keeps Craft honest: a recipe whose
 * ingredients are absent is a guaranteed 409 and should not look clickable.
 */
function renderRecipes(recipes) {
  const select = el("survival-recipe");
  if (!select) return;
  const previous = select.value;
  select.innerHTML = "";
  for (const recipe of recipes || []) {
    const option = document.createElement("option");
    option.value = recipe.id;
    const cost = (recipe.ingredients || [])
      .map((ingredient) => {
        const label = foodLabel(ingredient.food);
        const counted = ingredient.count > 1 ? `${label}×${ingredient.count}` : label;
        return Number(ingredient.held) >= Number(ingredient.count)
          ? counted
          : `${counted} (own ${Number(ingredient.held) || 0})`;
      })
      .join(" + ");
    const missing = (recipe.missing || []).length;
    option.textContent = `${recipe.label}${cost ? ` · ${cost}` : ""}${missing ? " · missing" : ""}`;
    option.title = missing
      ? `${recipe.blurb || ""}\nmissing: ${(recipe.missing || [])
          .map((entry) => `${entry.short} × ${entry.label}`)
          .join(", ")}`
      : `${recipe.blurb || recipe.label}\ncookable from the bag`;
    select.appendChild(option);
  }
  if (previous && (recipes || []).some((r) => r.id === previous)) select.value = previous;
  syncCraftButton();
}

/** Craft is only enabled for a recipe the bag can actually pay for. */
function syncCraftButton() {
  const select = el("survival-recipe");
  const button = el("btn-craft");
  if (!select || !button) return;
  const recipe = (survival.recipes || []).find((entry) => entry.id === select.value);
  if (!recipe) {
    button.disabled = true;
    button.title = "Pick a recipe";
    return;
  }
  const cookable = recipe.can_cook === true;
  button.disabled = !cookable;
  const missing = (recipe.missing || [])
    .map((entry) => `${entry.short} × ${entry.label}`)
    .join(", ");
  button.title = cookable
    ? `Cook ${recipe.label} from the bag (POST /survival/craft)`
    : `${recipe.label} still needs ${missing} — buy them on the Store tab`;
}

/**
 * Paint the craving row. `craving` is the object `GET /survival` carries
 * (`{kind, target, label, remaining_secs, …}`) or null when nothing is craved.
 */
function renderCraving(craving) {
  const row = el("survival-craving-row");
  const value = el("survival-craving");
  const clear = el("btn-craving-clear");
  const active = !!(craving && typeof craving === "object" && !craving.expired);
  if (row) row.classList.toggle("active", active);
  if (value) {
    if (active) {
      const remaining = Number(craving.remaining_secs);
      const mins = Number.isFinite(remaining) && remaining > 0
        ? ` · ${Math.ceil(remaining / 60)}m left`
        : "";
      value.textContent = `${craving.label || craving.target || "something"}${mins}`;
      value.title = `${craving.kind === "group" ? "group" : "dish"}: ${craving.target || ""}`;
    } else {
      value.textContent = "—";
      value.title = "nothing in particular";
    }
  }
  if (clear) clear.hidden = !active;
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
    craving: pick("craving"),
    well_fed_secs: pick("well_fed_secs"),
    enabled: raw.enabled,
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
    renderCraving(null);
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
  renderCraving(status.craving);
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

async function refreshRecipes() {
  try {
    const res = await fetch(`${survival.base}/recipes`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const body = await res.json();
    survival.recipes = Array.isArray(body.recipes) ? body.recipes : [];
    renderRecipes(survival.recipes);
  } catch (err) {
    setNote(`Recipe book unreachable: ${err.message}`, true);
  }
}

/**
 * Turn a meal payload into the one-line note both Feed and Craft use.
 * `label` is the dish, `tier_before`/`tier_after` the transition, and a
 * crafted meal also carries `comfort_secs` worth mentioning.
 */
function mealNote(body, fallback) {
  const before = typeof body.tier_before === "string" ? body.tier_before : "";
  const after = typeof body.tier_after === "string" ? body.tier_after : "";
  const gained = Number(body.hunger_after) - Number(body.hunger_before);
  const moving = before && after && before !== after ? ` · ${before} → ${after}` : "";
  const comfort = Number(body.comfort_secs) > 0
    ? ` · well fed ${Math.round(Number(body.comfort_secs) / 60)}m`
    : "";
  const craving = body.craving_satisfied ? ` · craving satisfied` : "";
  // A meal that came out of the bag reports what is left, so the picker's
  // "own N" and the note agree after one click.
  const left = Number.isInteger(body.remaining) ? ` · ${body.remaining} left` : "";
  return `${body.label || fallback} eaten (+${Math.round(gained * 10) / 10} hunger)${moving}${comfort}${craving}${left}`;
}

/** The Craft button: a real `POST /survival/craft` with a recipe id. */
async function craft() {
  const select = el("survival-recipe");
  const button = el("btn-craft");
  const recipe = select ? select.value : "";
  if (!recipe) return;

  if (button) {
    button.disabled = true;
    button.textContent = "Cooking…";
  }
  setNote("");

  try {
    const res = await fetch(`${survival.base}/survival/craft`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ recipe }),
    });
    const body = await res.json().catch(() => ({}));

    if (res.ok) {
      const used = Array.isArray(body.consumed) && body.consumed.length
        ? ` · used ${body.consumed.map((line) => `${line.count} × ${line.label}`).join(", ")}`
        : "";
      const note = `${mealNote(body, recipe)}${used}`;
      setNote(body.blurb ? `${note} — ${body.blurb}` : note);
      toast(`${body.label || recipe} cooked`);
    } else {
      // 409 = refused: missing_ingredients, too_full or still_digesting;
      // 400 = unknown recipe. The plugin's prose is the honest message.
      setNote(body.error || `craft failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setNote(`craft failed: ${err.message}`, true);
  } finally {
    if (button) button.textContent = "Craft";
    // The ingredients left the bag, so the pantry, the picker and the button
    // all have to catch up — the events do it too, this covers a muted plugin.
    refreshSurvival();
    refreshInventory();
    refreshRecipes();
    syncCraftButton();
  }
}

/** Set a craving for the food currently in the picker — `POST /survival {craving}`. */
async function setCraving() {
  const select = el("survival-food");
  const food = select ? select.value : "";
  if (!food) return;
  const label = select.selectedOptions && select.selectedOptions[0]
    ? select.selectedOptions[0].textContent.split(" · ")[0]
    : food;
  setNote("");
  try {
    const res = await fetch(`${survival.base}/survival`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ craving: food }),
    });
    const body = await res.json().catch(() => ({}));
    if (res.ok) {
      setNote(`Now craving ${label}`);
      toast(`Craving ${label}`);
      renderSurvival(body);
    } else {
      setNote(body.error || `craving failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setNote(`craving failed: ${err.message}`, true);
  }
}

/** Clear the current craving — `POST /survival {craving: null}`. */
async function clearCraving() {
  setNote("");
  try {
    const res = await fetch(`${survival.base}/survival`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ craving: null }),
    });
    const body = await res.json().catch(() => ({}));
    if (res.ok) {
      setNote("Craving cleared");
      renderSurvival(body);
    } else {
      setNote(body.error || `clear failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setNote(`clear failed: ${err.message}`, true);
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
      setNote(mealNote(body, food));
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

/**
 * Switch the card's pane. The four panes are one card on purpose: the panel
 * itself is already ~994 px tall, so a second and third floating card would
 * fight for the same corner of the screen.
 */
export function showSurvivalTab(name) {
  const target = ["status", "bag", "fishing", "store"].includes(name) ? name : "status";
  survival.tab = target;
  for (const pane of document.querySelectorAll("[data-survival-pane]")) {
    pane.classList.toggle("hidden", pane.dataset.survivalPane !== target);
  }
  for (const tab of document.querySelectorAll("[data-survival-tab]")) {
    const active = tab.dataset.survivalTab === target;
    tab.classList.toggle("active", active);
    tab.setAttribute("aria-selected", String(active));
  }
  if (target === "bag") refreshInventory();
  if (target === "store") refreshStore();
  if (target === "fishing") {
    // Coming back to a cast that is still out? Pick it up where it is.
    ensureFishingPoll();
    pollFishing();
  } else {
    stopMarker();
  }
  return target;
}

/* ------------------------------------------------------------------ *
 * Inventory
 * ------------------------------------------------------------------ */

/** Fill the "add an item" picker from the catalog, grouped by kind. */
function renderItemPicker(catalog) {
  const select = el("inventory-item");
  if (!select) return;
  const previous = select.value;
  const groups = new Map();
  for (const item of catalog || []) {
    const key = item.kind_label || item.kind || "Items";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(item);
  }
  select.innerHTML = "";
  for (const [label, items] of groups) {
    const group = document.createElement("optgroup");
    group.label = label;
    for (const item of items) {
      const option = document.createElement("option");
      option.value = item.id;
      option.textContent = `${item.icon} ${item.label}`;
      option.title = item.blurb || item.label;
      group.appendChild(option);
    }
    select.appendChild(group);
  }
  if (previous && select.querySelector(`option[value="${CSS.escape(previous)}"]`)) {
    select.value = previous;
  }
}

/**
 * Paint `GET /inventory`: one cell per slot, empty ones included, so the grid
 * is stable and "3 of 20" means something.
 */
function renderInventory(body) {
  if (!body || body.ok !== true) return;
  survival.inventory = body;

  const badge = el("survival-bag-count");
  if (badge) {
    badge.textContent = `${body.used}/${body.capacity}`;
    badge.classList.toggle("full", !!body.full);
    badge.title = body.full ? "the bag is full" : `${body.free} slot(s) free`;
  }
  const label = el("inventory-slots");
  if (label) {
    label.textContent = `${body.used} / ${body.capacity} slots${body.full ? " · full" : ""}`;
  }

  renderItemPicker(body.catalog);

  const grid = el("inventory-grid");
  if (grid) {
    grid.innerHTML = "";
    for (const slot of body.slots || []) {
      const cell = document.createElement("button");
      cell.type = "button";
      cell.className = "inv-slot";
      cell.dataset.index = String(slot.index);
      if (!slot.id) {
        cell.classList.add("empty");
        cell.disabled = true;
        cell.setAttribute("aria-label", `Empty slot ${slot.index + 1}`);
        grid.appendChild(cell);
        continue;
      }
      cell.classList.add("filled", `rarity-${slot.rarity || 1}`);
      if (survival.picked === slot.id) cell.classList.add("selected");
      cell.dataset.item = slot.id;
      cell.title = `${slot.label} ×${slot.count}\n${slot.blurb || ""}`.trim();
      cell.setAttribute("aria-label", `${slot.label}, ${slot.count} of ${slot.stack_max}`);

      const icon = document.createElement("span");
      icon.className = "inv-icon";
      icon.textContent = slot.icon || "•";
      const count = document.createElement("span");
      count.className = "inv-count";
      count.textContent = String(slot.count);
      cell.append(icon, count);

      cell.addEventListener("click", () => pickInventoryItem(slot));
      cell.addEventListener("mouseenter", () => showItemDetail(slot));
      grid.appendChild(cell);
    }
  }

  // A picked item that left the bag (eaten, dropped) must not stay selected.
  if (survival.picked && !(body.totals || []).some((t) => t.id === survival.picked)) {
    survival.picked = null;
  }
  // The Feed picker shows `own N` per ingredient, so it repaints with the bag.
  if (survival.foods.length) renderFoods(survival.foods);
  syncInventoryButtons();
}

function showItemDetail(slot) {
  const detail = el("inventory-detail");
  if (!detail) return;
  if (!slot || !slot.id) {
    detail.textContent = "Hover a slot for its details.";
    return;
  }
  detail.innerHTML = "";
  const name = document.createElement("strong");
  name.textContent = slot.label;
  const stack = slot.stack_max ? ` · stacks to ${slot.stack_max}` : "";
  const edible = slot.edible ? " · edible" : "";
  const price = Number(slot.sell_price) || 0;
  const worth = price > 0 ? ` · sells for ${price}` : "";
  detail.append(
    name,
    document.createTextNode(
      ` ×${slot.count}${stack}${edible}${worth}${slot.blurb ? ` — ${slot.blurb}` : ""}`,
    ),
  );
}

function pickInventoryItem(slot) {
  if (!slot || !slot.id) return;
  survival.picked = survival.picked === slot.id ? null : slot.id;
  showItemDetail(survival.picked ? slot : null);
  // Repaint just the selection class rather than re-fetching the whole bag.
  for (const cell of document.querySelectorAll("#inventory-grid .inv-slot.filled")) {
    cell.classList.toggle("selected", cell.dataset.item === survival.picked);
  }
  syncInventoryButtons();
}

/**
 * Keep the Use and Sell buttons honest about what the API will accept.
 *
 * Sell is enabled for any selected slot the shop has a price for, which — since
 * `store::sell_value` covers the whole item catalog — is every row in the bag,
 * not just the thirteen on the Store tab. The number on the button is the same
 * `unit_price` the trade reply will report.
 */
function syncInventoryButtons() {
  const use = el("btn-inventory-use");
  const sell = el("btn-inventory-sell");
  const totals = (survival.inventory && survival.inventory.totals) || [];
  const held = totals.find((entry) => entry.id === survival.picked);
  const catalog = (survival.inventory && survival.inventory.catalog) || [];
  const definition = catalog.find((entry) => entry.id === survival.picked);

  if (use) {
    const usable = !!(held && definition && definition.food);
    use.disabled = !usable;
    use.title = held
      ? usable
        ? `Eat one ${definition.label} (POST /inventory/use)`
        : `${definition.label} can only be held (POST /inventory/use would be a 400)`
      : "Pick a slot first";
  }

  if (sell) {
    const price = Number(held && definition ? definition.sell_price : 0) || 0;
    const sellable = !!(held && held.count >= 1 && price > 0);
    sell.disabled = !sellable;
    sell.textContent = sellable ? `Sell +${price}` : "Sell";
    sell.title = sellable
      ? `Sell one ${definition.label} for ${price} coin${price === 1 ? "" : "s"} `
        + "(POST /store/sell)"
      : held
        ? "the store will not buy this"
        : "Pick a slot first";
  }

  const add = el("btn-inventory-add");
  if (add) add.disabled = !(el("inventory-item") && el("inventory-item").value);
}

function setInventoryNote(text, isError) {
  const note = el("inventory-note");
  if (!note) return;
  note.textContent = text || "";
  note.classList.toggle("error", !!isError);
}

/** `GET /inventory` — the authoritative read behind the grid. */
async function refreshInventory() {
  try {
    const res = await fetch(`${survival.base}/inventory`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    renderInventory(await res.json());
  } catch (err) {
    setInventoryNote(`Inventory unreachable: ${err.message}`, true);
  }
}

/** `POST /inventory/add` for the picker row. */
async function addItem() {
  const select = el("inventory-item");
  const count = el("inventory-count");
  const item = select ? select.value : "";
  if (!item) return;
  const amount = Math.max(1, Math.min(99, Number(count && count.value) || 1));
  setInventoryNote("");
  try {
    const res = await fetch(`${survival.base}/inventory/add`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ item, count: amount }),
    });
    const body = await res.json().catch(() => ({}));
    if (res.ok) {
      renderInventory(body);
      const dropped = Number(body.leftover) || 0;
      setInventoryNote(
        `Added ${body.added} × ${item}${dropped ? ` (${dropped} did not fit)` : ""}`,
      );
      toast(`${body.added} × ${item} added`);
    } else {
      setInventoryNote(body.error || `add failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setInventoryNote(`add failed: ${err.message}`, true);
  }
}

/** `POST /inventory/use` — eats one edible item through the meal path. */
async function useItem() {
  if (!survival.picked) return;
  setInventoryNote("");
  try {
    const res = await fetch(`${survival.base}/inventory/use`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ item: survival.picked }),
    });
    const body = await res.json().catch(() => ({}));
    if (res.ok) {
      setInventoryNote(`${body.item_label || body.label} eaten — ${body.remaining} left`);
      toast(`${body.item_label || body.label} eaten`);
      refreshInventory();
      refreshSurvival();
    } else {
      // 409 = refused (full / digesting) or not held; 400 = not edible.
      setInventoryNote(body.error || `use failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setInventoryNote(`use failed: ${err.message}`, true);
  }
}

/**
 * `POST /store/sell` straight from the bag — one unit of the picked stack.
 *
 * The counter buys anything in the item catalog, so this is the whole of "turn
 * a catch into coins": the Store tab's shelf is only what the shop *sells*, and
 * the bag is where the player actually stands when the purse is thin. The reply
 * names the payout, so the note line never has to guess the price.
 */
async function sellPickedItem() {
  const item = survival.picked;
  if (!item) return;
  const button = el("btn-inventory-sell");
  const original = button ? button.textContent : "";
  if (button) {
    button.disabled = true;
    button.textContent = "Selling…";
  }
  setInventoryNote("");
  try {
    const res = await fetch(`${survival.base}/store/sell`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ item, count: 1 }),
    });
    const body = await res.json().catch(() => ({}));
    if (res.ok) {
      const total = Number(body.total) || 0;
      const coins = Number(body.coins);
      setInventoryNote(
        `Sold ${body.count} × ${body.label || item} for ${total} coin${total === 1 ? "" : "s"}`
        + `${Number.isFinite(coins) ? ` · ${coins} coins left` : ""}`,
      );
      toast(`🪙 ${body.label || item} sold`);
    } else {
      // 409 = the bag no longer holds that many; 400 = not an item at all.
      setInventoryNote(body.error || `sell failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setInventoryNote(`sell failed: ${err.message}`, true);
  } finally {
    if (button) button.textContent = original;
    // The reply carries the balance but not the slotted grid, and the coin badge
    // lives on the Store tab — re-read both. (`store-transaction` does the same;
    // this keeps the pane honest without SSE.)
    refreshInventory();
    refreshStore();
  }
}

/* ------------------------------------------------------------------ *
 * Store
 * ------------------------------------------------------------------ */

function setStoreNote(text, isError) {
  const note = el("store-note");
  if (!note) return;
  note.textContent = text || "";
  note.classList.toggle("error", !!isError);
}

/** Paint the wallet in the pane head and the coin badge on the tab. */
function renderWallet(coins) {
  const value = Number(coins);
  const known = Number.isFinite(value);
  const badge = el("survival-coins");
  if (badge) {
    badge.textContent = known ? String(value) : "—";
    badge.title = known ? `${value} coin${value === 1 ? "" : "s"}` : "coins unknown";
  }
  const wallet = el("store-wallet");
  if (wallet) {
    wallet.textContent = known ? `🪙 ${value} coins` : "wallet unavailable";
    wallet.classList.toggle("off", !known);
  }
}

/** The selected shelf entry, or null. */
function storeItem() {
  const catalog = (survival.store && survival.store.catalog) || [];
  return catalog.find((entry) => entry.id === survival.storePicked) || null;
}

/** How many units a trade asks for — the input is user-typed, so clamp it. */
function storeCount() {
  const input = el("store-count");
  return Math.max(1, Math.min(99, Number(input && input.value) || 1));
}

function pickStoreItem(id) {
  if (!id) return;
  survival.storePicked = survival.storePicked === id ? null : id;
  for (const row of document.querySelectorAll("#store-list .store-row")) {
    row.classList.toggle("selected", row.dataset.item === survival.storePicked);
  }
  syncStoreButtons();
}

/**
 * Keep Buy/Sell honest about what the plugin will actually accept.
 *
 * Both are still validated server-side — this only stops a click that is
 * *certain* to be refused (an empty purse, a stack the bag does not hold) from
 * turning into a red note.
 */
function syncStoreButtons() {
  const item = storeItem();
  const coins = survival.store ? Number(survival.store.coins) : NaN;
  const count = storeCount();

  const buy = el("btn-store-buy");
  if (buy) {
    const cost = item ? Number(item.buy_price) * count : 0;
    const affordable = !!item && (!Number.isFinite(coins) || coins >= cost);
    buy.disabled = !item || !affordable;
    buy.title = !item
      ? "Pick an item first"
      : affordable
        ? `Buy ${count} × ${item.label} for ${cost} coins (POST /store/buy)`
        : `${count} × ${item.label} costs ${cost}, the purse holds ${coins}`;
  }

  const sell = el("btn-store-sell");
  if (sell) {
    const owned = item ? Number(item.owned) || 0 : 0;
    const can = !!item && owned >= count;
    sell.disabled = !can;
    sell.title = !item
      ? "Pick an item first"
      : can
        ? `Sell ${count} × ${item.label} for ${Number(item.sell_price) * count} coins (POST /store/sell)`
        : `you hold ${owned} ${item.label}, cannot sell ${count}`;
  }
}

/**
 * Paint `GET /store`: the wallet, then one row per shelf entry grouped by its
 * category. `owned` comes straight from the plugin, so the Sell button knows
 * how much the bag really holds without a second request.
 */
function renderStore(body) {
  if (!body || body.ok !== true) return;
  survival.store = body;
  renderWallet(body.coins);

  const list = el("store-list");
  if (!list) return;

  const catalog = Array.isArray(body.catalog) ? body.catalog : [];
  // A picked id that left the shelves (a stock rotation, a failed read) must not
  // stay selected; otherwise default to the first entry so Buy/Sell are live.
  if (!survival.storePicked || !catalog.some((item) => item.id === survival.storePicked)) {
    survival.storePicked = catalog.length ? catalog[0].id : null;
  }

  list.innerHTML = "";
  if (!catalog.length) {
    const empty = document.createElement("div");
    empty.className = "store-empty";
    empty.textContent = "The shop is empty.";
    list.appendChild(empty);
    syncStoreButtons();
    return;
  }

  const groups = new Map();
  for (const category of body.categories || []) groups.set(category.id, []);
  for (const item of catalog) {
    const key = item.category || "material";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(item);
  }

  const coins = Number(body.coins);
  for (const items of groups.values()) {
    if (!items.length) continue;
    const heading = document.createElement("div");
    heading.className = "store-heading";
    heading.textContent = items[0].category_label || items[0].category || "Items";
    list.appendChild(heading);

    for (const item of items) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "store-row";
      row.dataset.item = item.id;
      row.classList.toggle("selected", item.id === survival.storePicked);
      row.classList.toggle("broke", Number.isFinite(coins) && coins < Number(item.buy_price));
      row.title = `${item.label} — buy ${item.buy_price}, sell ${item.sell_price}`
        + `\nyou hold ${item.owned}${item.stack_max ? ` · stacks to ${item.stack_max}` : ""}`;
      row.setAttribute("aria-label", `${item.label}, ${item.owned} owned`);
      row.setAttribute("role", "listitem");

      const icon = document.createElement("span");
      icon.className = "store-icon";
      icon.textContent = item.icon || "•";
      const name = document.createElement("span");
      name.className = "store-name";
      name.textContent = item.label;
      const owned = document.createElement("span");
      owned.className = "store-owned";
      owned.textContent = item.owned ? `own ${item.owned}` : "";

      const prices = document.createElement("span");
      prices.className = "store-prices";
      const buy = document.createElement("span");
      buy.className = "store-buy-price";
      buy.textContent = `buy ${item.buy_price}`;
      const sell = document.createElement("span");
      sell.className = "store-sell-price";
      sell.textContent = `sell ${item.sell_price}`;
      prices.append(buy, sell);

      row.append(icon, name, owned, prices);
      row.addEventListener("click", () => pickStoreItem(item.id));
      list.appendChild(row);
    }
  }
  syncStoreButtons();
}

/** `GET /store` — the authoritative read behind the shelf. */
async function refreshStore() {
  try {
    const res = await fetch(`${survival.base}/store`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    renderStore(await res.json());
  } catch (err) {
    setStoreNote(`Store unreachable: ${err.message}`, true);
  }
}

/**
 * One trade: `POST /store/buy` or `POST /store/sell` for the selected row.
 *
 * The reply carries the new balance but not the new `owned` counts, so the pane
 * re-reads `GET /store` afterwards — and the bag too, because a purchase lands
 * items in it. A refusal (409: empty purse, full bag, not enough held; 400:
 * unknown id) is printed verbatim, which is also how you tell a real bug from a
 * refusal.
 */
async function trade(action) {
  const item = storeItem();
  if (!item) return;
  const buying = action === "buy";
  const count = storeCount();
  const button = el(buying ? "btn-store-buy" : "btn-store-sell");
  const original = button ? button.textContent : "";

  if (button) {
    button.disabled = true;
    button.textContent = buying ? "Buying…" : "Selling…";
  }
  setStoreNote("");

  try {
    const res = await fetch(`${survival.base}/store/${buying ? "buy" : "sell"}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ item: item.id, count }),
    });
    const body = await res.json().catch(() => ({}));
    if (res.ok) {
      const moved = Number(body.count) || count;
      const total = Number(body.total) || 0;
      const coins = Number(body.coins);
      setStoreNote(
        `${buying ? "Bought" : "Sold"} ${moved} × ${body.label || item.label} `
        + `for ${total} coin${total === 1 ? "" : "s"}`
        + `${Number.isFinite(coins) ? ` · ${coins} left` : ""}`,
      );
      toast(`${buying ? "🛒" : "🪙"} ${moved} × ${body.label || item.label}`);
    } else {
      setStoreNote(body.error || `trade failed (HTTP ${res.status})`, true);
    }
  } catch (err) {
    setStoreNote(`trade failed: ${err.message}`, true);
  } finally {
    if (button) {
      button.disabled = false;
      button.textContent = original;
    }
    // The reply carries the balance but not the new `owned` counts, and buying
    // an ingredient changes what the Cook row can pay for — so re-read all
    // three. (The events do the same; this keeps the pane honest without them,
    // e.g. in a plain browser.)
    refreshStore();
    refreshInventory();
    refreshRecipes();
  }
}

/* ------------------------------------------------------------------ *
 * Fishing
 * ------------------------------------------------------------------ */

/** Triangle wave, exactly the plugin's `sweep_position`. */
function sweepPosition(elapsedMs, periodMs) {
  const period = Math.max(1, Number(periodMs) || 1);
  const phase = ((elapsedMs % period) + period) % period / period;
  return phase < 0.5 ? phase * 2 : (1 - phase) * 2;
}

/** Human name for a key code, so the hint can say "Space" not "Space". */
function keyLabel(code) {
  if (!code) return "Space";
  if (code === "Space" || code === "space") return "Space";
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  return code;
}

function setFishingNote(text, isError) {
  const note = el("fishing-note");
  if (!note) return;
  note.textContent = text || "";
  note.classList.toggle("error", !!isError);
}

/** Paint the phase chip on the Status pane, so a bite is visible from there. */
function renderFishingChip(session) {
  const chip = el("survival-fishing-state");
  if (!chip) return;
  const phase = (session && session.phase) || "idle";
  chip.textContent = phase;
  chip.classList.toggle("active", phase !== "idle");
  chip.title = session && session.casts
    ? `${session.casts} cast(s), ${session.catches} caught, ${session.escapes} lost`
    : "no casts yet";
}

function renderFishingStats(session) {
  const stats = el("fish-stats");
  if (!stats) return;
  if (!session) {
    stats.textContent = "";
    return;
  }
  stats.textContent =
    `casts ${session.casts} · caught ${session.catches} · lost ${session.escapes}`;
}

/**
 * Apply one `GET /fishing` (or cast/hook) body: phase, bobber, tension bar and
 * the action button all follow from it.
 */
function applyFishingStatus(body) {
  if (!body || body.ok !== true) return;
  if (body.config) fishing.config = body.config;
  const session = body.session || null;
  fishing.session = session;
  renderFishingChip(session);
  renderFishingStats(session);

  const scene = el("fish-scene");
  const hint = el("fish-hint");
  const action = el("btn-fish-action");
  const cancel = el("btn-fish-cancel");
  const tension = el("fish-tension");
  const phase = (session && session.phase) || "idle";
  const changed = phase !== fishing.phase;
  fishing.phase = phase;

  if (scene) scene.dataset.phase = phase;

  if (hint) {
    const hookKey = keyLabel((fishing.config && fishing.config.hook_key) || "Space");
    const reelKey = keyLabel((fishing.config && fishing.config.reel_key) || "Space");
    if (phase === "waiting") {
      hint.textContent = "Bobber's out. Wait for the dip…";
    } else if (phase === "biting") {
      hint.textContent = `It's biting! Press ${hookKey} now!`;
    } else if (phase === "reeling") {
      hint.textContent = `Press ${reelKey} in the green.`;
    } else {
      hint.textContent = "Cast the line.";
    }
  }

  if (action) {
    action.textContent =
      phase === "biting" ? "Strike!" : phase === "reeling" ? "Reel!" : "Cast";
    // Pressing while the line is merely out does nothing server-side, so the
    // button is disabled rather than producing a pointless 409.
    action.disabled = phase === "waiting";
  }
  if (cancel) cancel.disabled = phase === "idle";

  if (phase === "reeling" && session && session.reel) {
    if (tension) tension.classList.remove("hidden");
    const sweet = el("fish-sweet");
    const reel = session.reel;
    if (sweet) {
      sweet.style.left = `${Math.max(0, Math.min(1, reel.sweet_start)) * 100}%`;
      sweet.style.width = `${Math.max(0, Math.min(1, reel.sweet_width)) * 100}%`;
      sweet.title = `sweet spot ${Math.round(reel.sweet_start * 100)}–${Math.round(
        reel.sweet_end * 100,
      )}%`;
    }
    if (changed || !fishing.raf) startMarker(reel);
  } else {
    stopMarker();
    if (tension) tension.classList.add("hidden");
  }

  // A settled session stops the poll; the next cast restarts it.
  if (!session || !session.active) {
    stopFishingPoll();
  }
}

/** Animate the reel marker off the plugin's own timeline. */
function startMarker(reel) {
  stopMarker();
  const marker = el("fish-marker");
  const tension = el("fish-tension");
  if (!marker) return;
  const startedMs = Number(reel.started_ms) || Date.now();
  const periodMs = Number(reel.period_ms) || 1;
  const deadlineMs = Number(reel.deadline_ms) || startedMs + periodMs * 2;
  // The plugin's clock and `Date.now()` are both epoch ms on this machine; the
  // offset only absorbs the round-trip of the request that delivered `reel`.
  const skew = startedMs - Date.now();

  const tick = () => {
    const elapsed = Math.max(0, Date.now() + skew - startedMs);
    const position = sweepPosition(elapsed, periodMs);
    marker.style.left = `${position * 100}%`;
    if (tension) tension.setAttribute("aria-valuenow", String(Math.round(position * 100)));
    if (Date.now() + skew > deadlineMs) {
      stopMarker();
      return;
    }
    fishing.raf = window.requestAnimationFrame(tick);
  };
  fishing.raf = window.requestAnimationFrame(tick);
}

function stopMarker() {
  if (fishing.raf) {
    window.cancelAnimationFrame(fishing.raf);
    fishing.raf = 0;
  }
}

function ensureFishingPoll() {
  if (fishing.timer) return;
  fishing.timer = window.setInterval(pollFishing, FISHING_POLL_MS);
}

function stopFishingPoll() {
  if (fishing.timer) {
    window.clearInterval(fishing.timer);
    fishing.timer = 0;
  }
}

/** `GET /fishing` — cheap enough to run at 120 ms, and it is what makes the
 *  bobber react faster than the host's 1 s tick. */
async function pollFishing() {
  if (!survivalCardOpen() || survival.tab !== "fishing") return;
  try {
    const res = await fetch(`${survival.base}/fishing`, { cache: "no-store" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    applyFishingStatus(await res.json());
  } catch (err) {
    stopFishingPoll();
    setFishingNote(`Fishing API unreachable: ${err.message}`, true);
  }
}

async function postFishing(path, body) {
  const res = await fetch(`${survival.base}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body || {}),
  });
  const payload = await res.json().catch(() => ({}));
  return { ok: res.ok, status: res.status, payload };
}

/** The one button/key/scene-click action: cast → strike → reel. */
async function pressFishingAction() {
  const phase = (fishing.session && fishing.session.phase) || "idle";
  if (phase === "waiting") return; // nothing to do but wait
  if (phase === "biting") return hookFish();
  if (phase === "reeling") return reelFish();
  return castLine();
}

async function castLine() {
  setFishingNote("");
  const { ok, status, payload } = await postFishing("/fishing/cast");
  if (!ok) {
    setFishingNote(payload.error || `cast failed (HTTP ${status})`, true);
    pollFishing();
    return;
  }
  applyFishingStatus({ ok: true, session: payload.session, config: fishing.config });
  ensureFishingPoll();
}

async function hookFish() {
  const { ok, status, payload } = await postFishing("/fishing/hook");
  if (!ok) {
    setFishingNote(payload.error || `hook failed (HTTP ${status})`, true);
    pollFishing();
    return;
  }
  // The reel payload is not a full status body, so re-read for the session.
  ensureFishingPoll();
  await pollFishing();
  setFishingNote("Hooked — press when the marker is in the green.");
}

async function reelFish() {
  const { ok, status, payload } = await postFishing("/fishing/reel");
  if (!ok) {
    setFishingNote(payload.error || `reel failed (HTTP ${status})`, true);
    pollFishing();
    return;
  }
  applyFishingResult(payload);
  await pollFishing();
}

async function cancelFishing() {
  const { ok, status, payload } = await postFishing("/fishing/cancel");
  if (!ok) {
    setFishingNote(payload.error || `cancel failed (HTTP ${status})`, true);
  } else {
    applyFishingResult(payload);
  }
  await pollFishing();
}

const ESCAPE_PROSE = {
  spooked: "It took the bait and got away.",
  missed_the_spot: "You struck too early — it slipped the hook.",
  timed_out: "The line went slack.",
  cancelled: "Reeled in.",
  inventory_full: "Caught it, but the bag is full.",
};

/** Spell out a `fishing-result` frame once, whoever delivered it. */
function applyFishingResult(payload) {
  if (!payload || typeof payload !== "object") return;
  const key = `${payload.casts}-${payload.catches}-${payload.escapes}-${payload.caught ? 1 : 0}`;
  if (key === fishing.resultKey) return;
  fishing.resultKey = key;

  if (payload.caught) {
    setFishingNote(`Caught a ${payload.label}! It's in the bag.`);
    toast(`${payload.icon || "🐟"} ${payload.label} caught`);
    // The outcome is worth a face even when no AI brain is attached.
    reactExpression("happy", 4);
  } else {
    const reason = ESCAPE_PROSE[payload.reason] || `Nothing landed (${payload.reason || "lost"}).`;
    setFishingNote(reason, payload.reason === "inventory_full");
    reactExpression("sad", 4);
  }
  refreshInventory();
}

/**
 * Keyboard hook. Only the two configured codes act, and only while the fishing
 * pane is the visible one — otherwise Space in the command bar would cast a
 * line. Default is prevented so the page does not scroll.
 */
function onFishingKey(event) {
  const config = fishing.config;
  if (!config || !survivalCardOpen() || survival.tab !== "fishing") return;
  if (event.repeat) return;
  const phase = (fishing.session && fishing.session.phase) || "idle";
  if (phase !== "biting" && phase !== "reeling") return;
  const hook = String(config.hook_key || "").toLowerCase();
  const reel = String(config.reel_key || "").toLowerCase();
  const code = String(event.code || "").toLowerCase();
  if (code !== hook && code !== reel) return;
  event.preventDefault();
  pressFishingAction();
}

/* ------------------------------------------------------------------ */


/** Wire the widget, hydrate it, then follow the events. Safe to call twice. */
export async function setupSurvival() {
  if (survival.ready) return;
  const button = el("btn-feed");
  if (!button) return; // the panel markup is missing — nothing to do.

  button.addEventListener("click", (event) => {
    event.preventDefault();
    feed();
  });

  // The picker drives which actions are legal, so keep the buttons honest.
  el("survival-food")?.addEventListener("change", syncFoodButtons);
  el("btn-crave")?.addEventListener("click", (event) => {
    event.preventDefault();
    setCraving();
  });
  el("btn-craving-clear")?.addEventListener("click", (event) => {
    event.preventDefault();
    clearCraving();
  });
  el("btn-craft")?.addEventListener("click", (event) => {
    event.preventDefault();
    craft();
  });
  el("survival-recipe")?.addEventListener("change", syncCraftButton);

  // The pop-out: the panel button toggles the card, the ✕ and Esc close it.
  el("btn-survival")?.addEventListener("click", () => toggleSurvivalCard());
  el("btn-survival-close")?.addEventListener("click", () => toggleSurvivalCard(false));

  // ---- tabs -------------------------------------------------------------
  for (const tab of document.querySelectorAll("[data-survival-tab]")) {
    tab.addEventListener("click", (event) => {
      event.preventDefault();
      showSurvivalTab(tab.dataset.survivalTab);
    });
  }
  el("btn-open-fishing")?.addEventListener("click", (event) => {
    event.preventDefault();
    showSurvivalTab("fishing");
  });

  // ---- inventory --------------------------------------------------------
  el("btn-inventory-reload")?.addEventListener("click", (event) => {
    event.preventDefault();
    refreshInventory();
  });
  el("btn-inventory-add")?.addEventListener("click", (event) => {
    event.preventDefault();
    addItem();
  });
  el("btn-inventory-use")?.addEventListener("click", (event) => {
    event.preventDefault();
    useItem();
  });
  el("btn-inventory-sell")?.addEventListener("click", (event) => {
    event.preventDefault();
    sellPickedItem();
  });
  el("inventory-item")?.addEventListener("change", syncInventoryButtons);

  // ---- store -------------------------------------------------------------
  el("btn-store-reload")?.addEventListener("click", (event) => {
    event.preventDefault();
    refreshStore();
  });
  el("btn-store-buy")?.addEventListener("click", (event) => {
    event.preventDefault();
    trade("buy");
  });
  el("btn-store-sell")?.addEventListener("click", (event) => {
    event.preventDefault();
    trade("sell");
  });
  el("store-count")?.addEventListener("input", syncStoreButtons);
  el("store-count")?.addEventListener("change", syncStoreButtons);

  // ---- fishing ----------------------------------------------------------
  el("btn-fish-action")?.addEventListener("click", (event) => {
    event.preventDefault();
    pressFishingAction();
  });
  el("btn-fish-cancel")?.addEventListener("click", (event) => {
    event.preventDefault();
    cancelFishing();
  });
  const scene = el("fish-scene");
  if (scene) {
    // A scene click is the configured mouse button, so the whole pond is the
    // hit target: click to cast, click to strike, click to reel.
    scene.addEventListener("pointerdown", (event) => {
      const wanted = Number((fishing.config && fishing.config.mouse_button) ?? 0);
      if (event.button !== wanted) return;
      event.preventDefault();
      pressFishingAction();
    });
  }
  document.addEventListener("keydown", onFishingKey);

  await resolveBase();
  await Promise.all([refreshFoods(), refreshRecipes(), refreshSurvival()]);
  await refreshInventory();
  await refreshStore();

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
  // A cooked dish and a craving change both arrive as their own events; the
  // authoritative read carries the whole picture (bars, tier, craving).
  await listen("survival-craft", () => refreshSurvival());
  await listen("survival-craving", () => refreshSurvival());

  // The bag is pushed too: a catch, an eaten fish and a crafted dish all change
  // it, and the tab is often hidden while they happen. The recipe book's
  // `can_cook` follows the pantry, so it re-reads with the bag.
  await listen("inventory-changed", () => {
    refreshInventory();
    refreshRecipes();
  });

  // A trade moved coins and items; the pane needs both. This is also what keeps
  // a second window (or an agent's curl) in step with the shelf.
  await listen("store-transaction", () => {
    refreshStore();
    refreshInventory();
    refreshRecipes();
  });

  // Fishing: the plugin owns the phase, so every frame just asks it again.
  // `fishing-result` also spells the outcome out, because the poll may already
  // have moved on to `idle` by the time it lands.
  await listen("fishing-started", () => {
    ensureFishingPoll();
    pollFishing();
  });
  await listen("fishing-bite", () => {
    // A bite is worth interrupting for even from the Status tab.
    toast("Something's biting! 🎣");
    pollFishing();
  });
  await listen("fish-hooked", () => {
    ensureFishingPoll();
    pollFishing();
  });
  await listen("fishing-result", (event) => applyFishingResult(event.payload));

  setInterval(refreshSurvival, POLL_MS);
  survival.ready = true;

  // CDP surface: read the live numbers without a screenshot.
  if (window.__avatarDebug) window.__avatarDebug.survival = () => ({
    base: survival.base,
    ready: survival.ready,
    open: survivalCardOpen(),
    tab: survival.tab,
    foods: survival.foods.map((f) => f.id),
    recipes: survival.recipes.map((r) => r.id),
    cookable: survival.recipes.filter((r) => r.can_cook).map((r) => r.id),
    tier: survival.status ? survival.status.tier : null,
    hunger: survival.status ? survival.status.hunger : null,
    saturation: survival.status ? survival.status.saturation : null,
    craving: survival.status && survival.status.craving
      ? survival.status.craving.label || survival.status.craving.target
      : null,
    inventory: survival.inventory
      ? {
          used: survival.inventory.used,
          capacity: survival.inventory.capacity,
          full: survival.inventory.full,
          totals: (survival.inventory.totals || []).map((t) => `${t.id}×${t.count}`),
          picked: survival.picked,
          sellPrices: Object.fromEntries(
            (survival.inventory.catalog || []).map((item) => [item.id, item.sell_price]),
          ),
        }
      : null,
    store: survival.store
      ? {
          coins: survival.store.coins,
          picked: survival.storePicked,
          entries: (survival.store.catalog || []).map((item) => `${item.id}×${item.owned}`),
        }
      : null,
    fishing: {
      phase: fishing.session ? fishing.session.phase : "idle",
      casts: fishing.session ? fishing.session.casts : 0,
      catches: fishing.session ? fishing.session.catches : 0,
      escapes: fishing.session ? fishing.session.escapes : 0,
      polling: !!fishing.timer,
      animating: !!fishing.raf,
      sweet: fishing.session && fishing.session.reel
        ? [fishing.session.reel.sweet_start, fishing.session.reel.sweet_end]
        : null,
      hookKey: fishing.config ? fishing.config.hook_key : null,
    },
  });
}
