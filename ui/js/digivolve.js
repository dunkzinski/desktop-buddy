/**
 * char-rig — digivolution widget
 *
 * The pop-out card for the `digivolution` plugin: stage, XP, bond, attribute,
 * the recency-weighted attribute mix of the whole lineage, the three evolution
 * branches (each with the mix it would produce), the "Evolve" button, and a
 * new-creature row.
 *
 * It is a *client* of the HTTP API, exactly like the survival card: every number
 * is the one `GET /digimon/status` answers, every button really does the POST,
 * and a refusal from the plugin is shown verbatim instead of being smoothed
 * over. The rig itself is never touched here — `POST /digivolve` writes the new
 * manifest and the host hot-swaps the avatar, which the page already sees as a
 * `model-changed` event.
 *
 * Two update paths, deliberately:
 *   * the `digimon` Tauri event (the plugin emits `ready` / `digivolved` /
 *     `reset`), and
 *   * a slow `GET /digimon/status` poll, because the plugin can be absent
 *     (`plugins/digivolution.wasm` deleted) and a stale card is worse than a
 *     card that says "no creature".
 *
 * Imports `ipc` and `state` only — no cycles, and nothing here needs the rig.
 */

import { call, listen } from "./ipc.js";
import { reactExpression, toast } from "./state.js";

const DEFAULT_BASE = "http://127.0.0.1:8765";
const POLL_MS = 10000;

/**
 * Fallback list only: the plugin reports the archetypes it can actually build
 * (`status.archetypes`), and that is what the picker is filled from. A second
 * hand-kept copy here would go stale the moment a body plan is added.
 */
const ARCHETYPES = [
  ["beast", "Beast"],
  ["drake", "Drake"],
  ["cyborg", "Cyborg"],
  ["holy", "Holy"],
  ["aqua", "Aqua"],
  ["cervid", "Cervid"],
  ["machine", "Machine"],
  ["alien", "Alien"],
];

/** Attributes, with the shape each one grows. */
const ATTRIBUTES = [
  ["Vaccine", "Vaccine · haloed"],
  ["Data", "Data · crystalline"],
  ["Virus", "Virus · fanged"],
];

const digivolve = {
  base: DEFAULT_BASE,
  /** Last `GET /digimon/status` body, or null before the first read. */
  status: null,
  /** Index into `status.branches` the user picked. */
  picked: 0,
  ready: false,
  /** Set when the route is not served at all (plugin missing). */
  missing: false,
  /** Slug of the last rendered stage, so a transition can reset the pick. */
  stageKey: "",
  /** Has the user touched the archetype picker? Then stop syncing it. */
  archetypeTouched: false,
  /** Same, for the attribute picker. */
  attributeTouched: false,
};

const el = (id) => document.getElementById(id);
const card = () => document.getElementById("digivolve-card");

/** Is the pop-out card on screen? */
export function digivolveCardOpen() {
  const node = card();
  return !!node && !node.classList.contains("hidden");
}

/** Open/close the card. `force` mirrors `toggleSurvivalCard`, so Esc and ✕
 *  share one path. */
export function toggleDigivolveCard(force) {
  const node = card();
  if (!node) return false;
  const open = typeof force === "boolean" ? force : node.classList.contains("hidden");
  node.classList.toggle("hidden", !open);
  node.setAttribute("aria-hidden", String(!open));

  const button = el("btn-digivolve");
  if (button) {
    button.classList.toggle("active", open);
    button.setAttribute("aria-pressed", String(open));
  }
  if (open) refreshDigimon();
  return open;
}

/** Resolve the API base the app actually bound (it can slide off 8765). */
async function resolveBase() {
  const info = await call("get_runtime_info");
  const url = info && typeof info.api_url === "string" ? info.api_url : "";
  digivolve.base = /^https?:\/\//.test(url) ? url : DEFAULT_BASE;
}

function setNote(text, isError) {
  const note = el("digivolve-note");
  if (!note) return;
  note.textContent = text || "";
  note.classList.toggle("error", !!isError);
}

/* ------------------------------------------------------------------ *
 * Rendering
 * ------------------------------------------------------------------ */

/** `Vaccine` / `Data` / `Virus` chip class. */
function attributeClass(attribute) {
  const value = String(attribute || "").toLowerCase();
  return value === "vaccine" || value === "data" || value === "virus" ? value : "data";
}

function pct(part, whole) {
  if (!Number.isFinite(part) || !Number.isFinite(whole) || whole <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((part / whole) * 100)));
}

const ATTRIBUTE_ORDER = ["Vaccine", "Data", "Virus"];

/**
 * The lineage mix as `[[attribute, percent], …]`, in a fixed attribute order so
 * the stacked bar's colour order never jumps between polls.
 *
 * `status.attribute_blend` is the plugin's word; a plugin built before the blend
 * existed only reports a single `attribute`, so the fallback is a one-hot mix
 * rather than an empty card.
 */
function blendOf(source) {
  const blend = source && source.attribute_blend;
  if (blend && blend.percent) {
    const rows = ATTRIBUTE_ORDER.map((name) => [name, Number(blend.percent[name]) || 0]);
    const total = rows.reduce((sum, [, value]) => sum + value, 0);
    return total > 0 ? rows.filter(([, value]) => value > 0) : [];
  }
  const attribute = source && source.attribute;
  return attribute ? [[attribute, 100]] : [];
}

/** `57% Virus · 43% Data` — the same words the plugin's `summary` uses. */
function summarise(rows) {
  return rows.map(([attribute, percent]) => `${percent}% ${attribute}`).join(" · ");
}

/** The mix a branch would produce, from its own preview fields. */
function branchMix(branch) {
  if (!branch) return [];
  if (Array.isArray(branch.blend && branch.blend.weights)) {
    const weights = branch.blend.weights;
    const rows = ATTRIBUTE_ORDER.map((name, index) => [name, Math.round((weights[index] || 0) * 100)]);
    const total = rows.reduce((sum, [, value]) => sum + value, 0);
    if (total > 0) {
      // Fold the rounding into the loudest share so the readout says 100%.
      const loudest = rows.reduce((best, row) => (row[1] > best[1] ? row : best), rows[0]);
      loudest[1] += 100 - total;
      return rows.filter(([, value]) => value > 0);
    }
  }
  return branch.attribute ? [[branch.attribute, 100]] : [];
}

/** Human shortfall for the XP gate: `needs 90 XP (have 20)`. */
function shortfall(status) {
  const missing = Math.max(0, Number(status.xp_required || 0) - Number(status.xp || 0));
  return `${missing} XP to go`;
}

function renderCard(status) {
  const stage = el("digi-stage");
  if (stage) {
    stage.textContent = status.final_stage ? `${status.stage} · max` : status.stage;
    stage.className = `digi-stage${status.can_evolve ? " ready" : ""}${status.final_stage ? " max" : ""}`;
    stage.title = `Stage ${Number(status.tier_index) + 1} of 6 · generation ${status.generation}`;
  }

  // The creature line: archetype, attribute, generation, seed.
  const line = el("digi-line");
  if (line) {
    line.innerHTML = "";
    const arche = document.createElement("span");
    arche.className = "digi-chip";
    // The attribute has its own chip right beside this one.
    arche.textContent = status.creature ? status.creature.archetype : "—";
    const attribute = document.createElement("span");
    attribute.className = `digi-chip attr ${attributeClass(status.attribute)}`;
    attribute.textContent = status.attribute || "—";
    const seed = document.createElement("span");
    seed.className = "digi-seed";
    seed.textContent = status.creature ? `seed ${status.creature.seed_hex}` : "";
    seed.title = "The seed every stage of this creature is derived from";
    line.append(arche, attribute, seed);
  }

  renderBlend(status);
  renderHeaderBlend(status);

  // XP: at the last stage there is no next tier, so the bar is simply full.
  const final = !!status.final_stage;
  const whole = final ? 1 : Math.max(1, Number(status.xp_required || 0));
  const have = final ? 1 : Math.min(whole, Number(status.xp || 0));
  const xpBar = el("digi-xp-bar");
  if (xpBar) {
    xpBar.style.width = `${final ? 100 : pct(have, whole)}%`;
    xpBar.classList.toggle("full", !!status.can_evolve);
  }
  const xpValue = el("digi-xp");
  if (xpValue) {
    xpValue.textContent = final
      ? "MAX"
      : `${Math.round(Math.min(have, whole))}/${Math.round(whole)}`;
    xpValue.title = final
      ? "This creature is at its final stage"
      : status.can_evolve
        ? `Ready to digivolve (${Math.round(Number(status.xp || 0))} XP)`
        : shortfall(status);
  }

  const bond = Math.max(0, Math.min(1, Number(status.bond || 0)));
  const bondBar = el("digi-bond-bar");
  if (bondBar) bondBar.style.width = `${Math.round(bond * 100)}%`;
  const bondValue = el("digi-bond");
  if (bondValue) {
    bondValue.textContent = bond.toFixed(2);
    bondValue.title = bond > 0.66
      ? "High bond — branches lean Vaccine"
      : bond < 0.33
        ? "Low bond — branches lean Virus"
        : "Neutral bond — Data sits in the middle";
  }

  // The "new creature" picker follows the live creature until the user picks
  // something, so resetting does not silently change the archetype.
  const attribute = el("digi-attribute");
  if (attribute && Array.isArray(status.attributes) && status.attributes.length) {
    const wanted = status.attributes.map((entry) => entry.value).join(",");
    const have = [...attribute.options].map((option) => option.value).join(",");
    if (wanted !== have) {
      attribute.replaceChildren();
      for (const entry of status.attributes) {
        const option = document.createElement("option");
        option.value = entry.value;
        option.textContent = entry.label;
        option.title = entry.flavour;
        attribute.appendChild(option);
      }
    }
  }
  if (attribute && !digivolve.attributeTouched && status.attribute) {
    attribute.value = status.attribute;
  }

  const archetype = el("digi-archetype");
  if (archetype && Array.isArray(status.archetypes) && status.archetypes.length) {
    // Rebuild only when the list really changed, or the poll would wipe the
    // user's pick every ten seconds.
    const wanted = status.archetypes.map((pair) => pair.value).join(",");
    const have = [...archetype.options].map((option) => option.value).join(",");
    if (wanted !== have) {
      archetype.replaceChildren();
      fillArchetypes(archetype, status.archetypes);
    }
  }
  if (archetype && !digivolve.archetypeTouched && status.creature) {
    archetype.value = status.creature.archetype;
  }

  const meta = el("digi-meta");
  if (meta) {
    const rig = status.rig || {};
    const bits = [];
    if (rig.bones) bits.push(`${rig.bones} bones`);
    if (rig.parts) bits.push(`${rig.parts} parts`);
    if (Number.isFinite(rig.min_world_y)) bits.push(`top ${Math.round(rig.min_world_y)} px`);
    if (!status.installed) bits.push("not installed");
    meta.textContent = bits.join(" · ") || "";
    meta.title = "The live rig this creature generated for its current stage";
  }

  renderBranches(status);
}

/**
 * The lineage mix: a stacked bar (each attribute's width is its share), a chip
 * per attribute with its percentage, and the traits the mix grows.
 */
function renderBlend(status) {
  const rows = blendOf(status);
  const blend = status && status.attribute_blend;

  const bar = el("digi-blend-bar");
  if (bar) {
    bar.innerHTML = "";
    for (const [attribute, percent] of rows) {
      const slice = document.createElement("span");
      slice.className = attributeClass(attribute);
      slice.style.width = `${percent}%`;
      slice.title = `${attribute} ${percent}%`;
      bar.appendChild(slice);
    }
    bar.setAttribute("aria-label", `Attribute mix: ${summarise(rows) || "unknown"}`);
  }

  const chips = el("digi-blend-chips");
  if (chips) {
    chips.innerHTML = "";
    for (const [attribute, percent] of rows) {
      const chip = document.createElement("span");
      chip.className = `digi-chip attr ${attributeClass(attribute)} digi-blend-chip`;
      chip.textContent = `${percent}% ${attribute}`;
      chip.title = blend && blend.lineage && blend.lineage.length
        ? `Evolution line: ${blend.lineage.join(" → ")}`
        : `${attribute} share of this creature's lineage`;
      chips.appendChild(chip);
    }
    // A pure creature still gets one chip, so the readout is never blank.
    if (!rows.length) {
      const chip = document.createElement("span");
      chip.className = "digi-chip digi-blend-chip";
      chip.textContent = "no attribute";
      chips.appendChild(chip);
    }
  }

  const traits = el("digi-blend-traits");
  if (traits) {
    const text = (blend && blend.flavour) || (status && status.attribute) || "";
    traits.textContent = text;
    traits.title = blend && blend.lineage && blend.lineage.length
      ? `Recency-weighted across ${blend.lineage.length} stages — the newest stage counts most`
      : "The traits this creature's attribute grows";
  }

  const block = el("digi-blend");
  if (block) block.dataset.dominant = (blend && blend.dominant) || (status && status.attribute) || "";
}

function renderBranches(status) {
  const list = el("digi-branches");
  if (!list) return;
  const branches = Array.isArray(status.branches) ? status.branches : [];
  list.innerHTML = "";

  if (!branches.length) {
    const empty = document.createElement("div");
    empty.className = "digi-empty";
    empty.textContent = status.final_stage
      ? "Final stage — nothing left to become."
      : "No branches offered.";
    list.appendChild(empty);
    syncEvolveButton(status);
    syncBackButton(status);
    return;
  }

  digivolve.picked = Math.max(0, Math.min(branches.length - 1, digivolve.picked));

  branches.forEach((branch, index) => {
    const row = document.createElement("button");
    row.type = "button";
    row.className = `digi-branch${index === digivolve.picked ? " picked" : ""}`;
    row.dataset.branch = String(index);
    row.setAttribute("role", "radio");
    row.setAttribute("aria-checked", String(index === digivolve.picked));
    const projected = branchMix(branch);
    row.title =
      `Evolve into ${branch.label} (${branch.attribute}) — branch seed ${branch.seed}` +
      (projected.length ? `\nMix after evolving: ${summarise(projected)}` : "");

    const name = document.createElement("span");
    name.className = "digi-branch-name";
    name.textContent = branch.stage || "";

    const attribute = document.createElement("span");
    attribute.className = `digi-chip attr ${attributeClass(branch.attribute)}`;
    attribute.textContent = branch.attribute || "";

    // The branch's own seed suffix is what distinguishes the three rows (the
    // stage and attribute are already labelled); the full slug lives in the
    // tooltip. A 31-character slug would only ever render as an ellipsis here.
    const detail = document.createElement("span");
    detail.className = "digi-branch-detail";
    // What the attribute does to the body, not just which letter it is: the
    // three branches are three different creatures now.
    detail.textContent = branch.flavour
      ? branch.flavour
      : branch.slug
        ? `#${String(branch.slug).split("-").pop()}`
        : "";
    if (branch.slug) detail.title = branch.slug;

    row.append(name, attribute, detail);

    // What the mix becomes if this branch is taken. Precomputed by the plugin
    // (`blend_summary`), so the preview cannot drift from what evolution does.
    if (projected.length) {
      const mix = document.createElement("span");
      mix.className = "digi-branch-mix";
      const text = branch.blend_summary || summarise(projected);
      mix.textContent = `→ ${text}`;
      if (branch.blended_flavour) mix.title = branch.blended_flavour;
      row.append(mix);
    }

    row.addEventListener("click", (event) => {
      event.preventDefault();
      digivolve.picked = index;
      renderBranches(digivolve.status || status);
      syncEvolveButton(digivolve.status || status);
      syncBackButton(digivolve.status || status);
    });
    list.appendChild(row);
  });

  syncEvolveButton(status);
  syncBackButton(status);
}

/** Enable/disable "Back" and say which stage it returns to. */
function syncBackButton(status) {
  const button = el("btn-digi-back");
  if (!button) return;
  const can = !!(status && status.can_dedigivolve);
  const previous = status && status.previous_stage;
  button.disabled = !can;
  button.title = can
    ? `Return to ${previous || "the previous stage"} — POST /dedigivolve ` +
      `(${status.rewind_depth} step${status.rewind_depth === 1 ? "" : "s"} of history; ` +
      "the attribute mix rewinds with it)"
    : "This creature is at its first stage";
  button.textContent = can ? `◀ ${previous || "Back"}` : "◀ Back";
}

function syncEvolveButton(status) {
  const button = el("btn-digivolve-go");
  if (!button) return;
  const branches = (status && status.branches) || [];
  const branch = branches[digivolve.picked];
  const ready = !!(status && status.can_evolve && branch);
  button.disabled = !ready;
  button.textContent = ready
    ? "Evolve"
    : status && status.final_stage
      ? "Final stage"
      : "Not ready";
  button.title = ready
    ? `Digivolve into ${branch.stage} (${branch.attribute}) — POST /digivolve`
    : status && status.final_stage
      ? "Mega is the last stage"
      : status
        ? `${shortfall(status)} — the plugin refuses until then`
        : "No creature yet";
}

/**
 * The control panel's header strip: the same recency-weighted mix, compressed
 * to one summary line and a 4 px stacked bar. `digivolve` already owns the
 * `/digimon/status` poll and the blend maths, so the panel header is filled
 * from here rather than running a second fetch. Hidden whenever there is no
 * creature — a permanent "—" would read as a broken readout.
 */
function renderHeaderBlend(status) {
  const strip = el("blend-readout");
  if (!strip) return;
  const rows = blendOf(status);
  const blend = status && status.attribute_blend;
  if (!rows.length) {
    strip.classList.add("hidden");
    return;
  }
  strip.classList.remove("hidden");
  const summary = summarise(rows);
  strip.title =
    blend && blend.lineage && blend.lineage.length
      ? `Recency-weighted across ${blend.lineage.length} stage(s): ${blend.lineage.join(" → ")}`
      : "Recency-weighted attribute mix across this creature's evolution line";

  const text = el("blend-readout-summary");
  if (text) {
    text.textContent = summary;
    text.title = blend && blend.flavour ? blend.flavour : summary;
  }

  const bar = el("blend-readout-bar");
  if (bar) {
    bar.innerHTML = "";
    for (const [attribute, percent] of rows) {
      const slice = document.createElement("span");
      slice.className = attributeClass(attribute);
      slice.style.width = `${percent}%`;
      slice.title = `${attribute} ${percent}%`;
      bar.appendChild(slice);
    }
    bar.setAttribute("aria-label", `Attribute blend: ${summary}`);
  }
}

/** The whole card when the route is not served (plugin removed). */
function renderMissing(detail) {
  digivolve.missing = true;
  digivolve.status = null;
  el("blend-readout")?.classList.add("hidden");
  const stage = el("digi-stage");
  if (stage) {
    stage.textContent = "offline";
    stage.className = "digi-stage off";
    stage.title = detail || "";
  }
  for (const id of ["digi-xp-bar", "digi-bond-bar"]) {
    const bar = el(id);
    if (bar) bar.style.width = "0%";
  }
  for (const id of ["digi-xp", "digi-bond"]) {
    const node = el(id);
    if (node) node.textContent = "—";
  }
  const line = el("digi-line");
  if (line) line.textContent = "The digivolution plugin is not loaded.";
  const meta = el("digi-meta");
  if (meta) meta.textContent = detail || "plugins/digivolution.wasm is missing";
  const list = el("digi-branches");
  if (list) {
    list.innerHTML = "";
    const empty = document.createElement("div");
    empty.className = "digi-empty";
    empty.textContent = "Build it with ./scripts/build-plugin.sh digivolution, then restart.";
    list.appendChild(empty);
  }
  syncEvolveButton(null);
  syncBackButton(null);
}

/** Fill the archetype picker, preferring the list the plugin reported. */
function fillArchetypes(select, pairs) {
  if (!select || select.options.length) return;
  for (const pair of pairs) {
    const [value, label] = Array.isArray(pair) ? pair : [pair.value, pair.label];
    const option = document.createElement("option");
    option.value = value;
    option.textContent = label;
    select.appendChild(option);
  }
}

/** Public render entry (used by the `digimon` event and the poll). */
export function renderDigimon(status) {
  if (!status || typeof status !== "object" || status.ok === false) {
    renderMissing(status && status.error);
    return;
  }
  digivolve.missing = false;
  const stageKey = String(status.slug || status.stage || "");
  if (stageKey !== digivolve.stageKey) {
    digivolve.stageKey = stageKey;
    digivolve.picked = 0;
  }
  digivolve.status = status;
  renderCard(status);
}

/* ------------------------------------------------------------------ *
 * Reads and actions
 * ------------------------------------------------------------------ */

async function refreshDigimon() {
  try {
    const res = await fetch(`${digivolve.base}/digimon/status`, { cache: "no-store" });
    if (res.status === 404) {
      renderMissing("GET /digimon/status → 404");
      return;
    }
    const body = await res.json();
    renderDigimon(body);
  } catch (err) {
    // The API can be down while the app is starting; the poll retries.
    renderMissing(String(err));
  }
}

/** POST JSON, then re-read. A refusal is shown, never swallowed. */
async function post(path, body, okText) {
  try {
    const res = await fetch(`${digivolve.base}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body || {}),
    });
    const payload = await res.json().catch(() => ({}));
    if (!res.ok || payload.ok === false) {
      setNote(payload.error || `${path} → ${res.status}`, true);
      refreshDigimon();
      return null;
    }
    setNote(typeof okText === "function" ? okText(payload) : okText || "");
    refreshDigimon();
    return payload;
  } catch (err) {
    setNote(String(err), true);
    return null;
  }
}

function evolve() {
  const status = digivolve.status;
  const branch = status && (status.branches || [])[digivolve.picked];
  if (!status || !branch) return;
  post(
    "/digivolve",
    { branch: digivolve.picked },
    (payload) =>
      `${payload.from} → ${payload.stage}: ${payload.bones} bones, ${payload.parts} parts` +
      ` · ${payload.blend_summary || payload.attribute} · ${payload.model_path}`,
  ).then((payload) => {
    if (payload) {
      setNote(
        `${payload.from} → ${payload.stage}: ${payload.bones} bones, ${payload.parts} parts ` +
          `(${payload.blend_summary || payload.attribute})`,
      );
    }
  });
}

/** Go back one stage: `POST /dedigivolve`. */
function rewind() {
  const status = digivolve.status;
  if (!status || !status.can_dedigivolve) return;
  post("/dedigivolve", { steps: 1 }, (payload) =>
    `${payload.from} → ${payload.stage} (back) · ${payload.bones} bones, ${payload.parts} parts` +
    ` · ${payload.blend_summary || payload.attribute}`,
  );
}

/** The growth nudge: `POST /digimon/xp`. Labelled as the API it is. */
function nudge(xp, bond) {
  post("/digimon/xp", { xp, bond }, xp ? `+${xp} XP` : `bond ${bond > 0 ? "+" : ""}${bond}`);
}

function reset() {
  const archetype = el("digi-archetype") ? el("digi-archetype").value : "";
  const attribute = el("digi-attribute") ? el("digi-attribute").value : "";
  const rawSeed = el("digi-seed") ? el("digi-seed").value.trim() : "";
  const body = {};
  if (archetype) body.archetype = archetype;
  if (attribute) body.attribute = attribute;
  if (rawSeed) {
    const seed = Number(rawSeed);
    if (!Number.isFinite(seed) || seed < 0) {
      setNote("seed must be a non-negative integer", true);
      return;
    }
    body.seed = Math.floor(seed);
  }
  digivolve.picked = 0;
  post("/digimon/reset", body, (payload) =>
    `new ${payload.status.creature.archetype} · ${payload.status.stage} · ${payload.status.slug}`,
  );
}

/* ------------------------------------------------------------------ *
 * Setup
 * ------------------------------------------------------------------ */

export async function setupDigivolve() {
  if (digivolve.ready) return;
  if (!card()) return; // the markup is missing — nothing to wire.

  el("btn-digivolve")?.addEventListener("click", (event) => {
    event.preventDefault();
    toggleDigivolveCard();
  });
  el("btn-digi-close")?.addEventListener("click", (event) => {
    event.preventDefault();
    toggleDigivolveCard(false);
  });
  el("btn-digi-reload")?.addEventListener("click", (event) => {
    event.preventDefault();
    refreshDigimon();
  });
  el("btn-digivolve-go")?.addEventListener("click", (event) => {
    event.preventDefault();
    evolve();
  });
  el("btn-digi-back")?.addEventListener("click", (event) => {
    event.preventDefault();
    rewind();
  });
  el("btn-digi-xp")?.addEventListener("click", (event) => {
    event.preventDefault();
    nudge(60, 0);
  });
  el("btn-digi-bond")?.addEventListener("click", (event) => {
    event.preventDefault();
    nudge(0, 0.1);
  });
  el("btn-digi-reset")?.addEventListener("click", (event) => {
    event.preventDefault();
    reset();
  });

  // The archetype picker prefills from the live creature once we know it.
  const archetype = el("digi-archetype");
  archetype?.addEventListener("change", () => {
    digivolve.archetypeTouched = true;
  });
  const attribute = el("digi-attribute");
  attribute?.addEventListener("change", () => {
    digivolve.attributeTouched = true;
  });
  if (attribute && !attribute.options.length) {
    for (const [value, label] of ATTRIBUTES) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      option.title = label;
      attribute.appendChild(option);
    }
  }
  if (archetype && !archetype.options.length) {
    fillArchetypes(archetype, ARCHETYPES);
  }

  await resolveBase();

  // The plugin's own frames: `ready` (XP threshold crossed), `digivolved`,
  // `reset`. Any of them changes the card, so one listener covers all three.
  await listen("digimon", (event) => {
    const payload = event && event.payload ? event.payload : {};
    if (payload.event === "digivolved") {
      setNote(
        `${payload.from} → ${payload.stage} (${payload.blend_summary || payload.attribute})`,
      );
      toast(`Digivolved into ${payload.stage}`);
      // Growth is the good news of this whole plugin — show it on the face.
      reactExpression("happy", 5);
    } else if (payload.event === "dedigivolved") {
      setNote(`${payload.from} → ${payload.stage} (back to an earlier form)`);
      toast(`Back to ${payload.stage}`);
    } else if (payload.event === "ready") {
      setNote(`Growth complete — ${payload.stage} can evolve.`);
      reactExpression("surprised", 4);
    } else if (payload.event === "reset") {
      // A brand-new hatchling is worth a startled look.
      reactExpression("surprised", 4);
    }
    refreshDigimon();
  });
  // A new model means the plugin may have been the one that swapped it.
  await listen("model-changed", (event) => {
    const model = event && event.payload ? event.payload : {};
    if (digivolveCardOpen() || String(model.name || "").startsWith("digi-")) {
      refreshDigimon();
    }
  });

  setInterval(refreshDigimon, POLL_MS);
  digivolve.ready = true;
  await refreshDigimon();
  if (digivolve.status && digivolve.status.creature && archetype) {
    archetype.value = digivolve.status.creature.archetype;
  }

  // CDP surface: read the card's state without a screenshot.
  if (window.__avatarDebug) {
    window.__avatarDebug.digivolve = () => ({
      base: digivolve.base,
      ready: digivolve.ready,
      open: digivolveCardOpen(),
      missing: digivolve.missing,
      stage: digivolve.status ? digivolve.status.stage : null,
      xp: digivolve.status ? digivolve.status.xp : null,
      xp_required: digivolve.status ? digivolve.status.xp_required : null,
      bond: digivolve.status ? digivolve.status.bond : null,
      attribute: digivolve.status ? digivolve.status.attribute : null,
      blend: digivolve.status
        ? (digivolve.status.attribute_blend || {
            dominant: digivolve.status.attribute,
            percent: Object.fromEntries(blendOf(digivolve.status)),
            summary: summarise(blendOf(digivolve.status)),
          })
        : null,
      blendChips: [...document.querySelectorAll("#digi-blend-chips .digi-blend-chip")].map(
        (node) => node.textContent,
      ),
      /** The control panel's compact header readout (null when hidden). */
      panelBlend: (() => {
        const strip = el("blend-readout");
        if (!strip || strip.classList.contains("hidden")) return null;
        return {
          summary: (el("blend-readout-summary") || {}).textContent || "",
          slices: [...document.querySelectorAll("#blend-readout-bar span")].map(
            (node) => `${node.className}:${node.style.width}`,
          ),
        };
      })(),
      can_evolve: digivolve.status ? digivolve.status.can_evolve : false,
      picked: digivolve.picked,
      branches: digivolve.status
        ? (digivolve.status.branches || []).map(
            (b) => `${b.stage}:${b.attribute}:${b.blend_summary || ""}`,
          )
        : [],
      rig: digivolve.status ? digivolve.status.rig : null,
      can_dedigivolve: digivolve.status ? digivolve.status.can_dedigivolve : false,
      previous_stage: digivolve.status ? digivolve.status.previous_stage : null,
      backDisabled: !!((el("btn-digi-back") || {}).disabled),
      backLabel: (el("btn-digi-back") || {}).textContent || "",
      note: (el("digivolve-note") || {}).textContent || "",
      rows: [...document.querySelectorAll(".digi-branch")].length,
      evolveDisabled: !!((el("btn-digivolve-go") || {}).disabled),
    });
  }
}
