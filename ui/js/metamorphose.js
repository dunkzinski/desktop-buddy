/**
 * desktop-buddy — metamorphosis widget
 *
 * The pop-out card for the `metamorphosis` plugin: stage, XP, bond, attribute,
 * the recency-weighted attribute mix of the whole lineage, the three evolution
 * branches (each with the mix it would produce), the "Evolve" button, and a
 * new-creature row.
 *
 * It is a *client* of the HTTP API, exactly like the survival card: every number
 * is the one `GET /creature/status` answers, every button really does the POST,
 * and a refusal from the plugin is shown verbatim instead of being smoothed
 * over. The rig itself is never touched here — `POST /metamorphose` writes the new
 * manifest and the host hot-swaps the avatar, which the page already sees as a
 * `model-changed` event.
 *
 * Two update paths, deliberately:
 *   * the `creature` Tauri event (the plugin emits `ready` / `metamorphosed` /
 *     `reset`), and
 *   * a slow `GET /creature/status` poll, because the plugin can be absent
 *     (`plugins/metamorphosis.wasm` deleted) and a stale card is worse than a
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
  ["Seraph", "Seraph · haloed"],
  ["Prism", "Prism · crystalline"],
  ["Feral", "Feral · fanged"],
];

const metamorphose = {
  base: DEFAULT_BASE,
  /** Last `GET /creature/status` body, or null before the first read. */
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
const card = () => document.getElementById("metamorphose-card");

/** Is the pop-out card on screen? */
export function metamorphoseCardOpen() {
  const node = card();
  return !!node && !node.classList.contains("hidden");
}

/** Open/close the card. `force` mirrors `toggleSurvivalCard`, so Esc and ✕
 *  share one path. */
export function toggleMetamorphoseCard(force) {
  const node = card();
  if (!node) return false;
  const open = typeof force === "boolean" ? force : node.classList.contains("hidden");
  node.classList.toggle("hidden", !open);
  node.setAttribute("aria-hidden", String(!open));

  const button = el("btn-metamorphose");
  if (button) {
    button.classList.toggle("active", open);
    button.setAttribute("aria-pressed", String(open));
  }
  if (open) refreshCreature();
  return open;
}

/** Resolve the API base the app actually bound (it can slide off 8765). */
async function resolveBase() {
  const info = await call("get_runtime_info");
  const url = info && typeof info.api_url === "string" ? info.api_url : "";
  metamorphose.base = /^https?:\/\//.test(url) ? url : DEFAULT_BASE;
}

function setNote(text, isError) {
  const note = el("metamorphose-note");
  if (!note) return;
  note.textContent = text || "";
  note.classList.toggle("error", !!isError);
}

/* ------------------------------------------------------------------ *
 * Rendering
 * ------------------------------------------------------------------ */

/** `Seraph` / `Prism` / `Feral` chip class. */
function attributeClass(attribute) {
  const value = String(attribute || "").toLowerCase();
  return value === "seraph" || value === "prism" || value === "feral" ? value : "prism";
}

function pct(part, whole) {
  if (!Number.isFinite(part) || !Number.isFinite(whole) || whole <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((part / whole) * 100)));
}

const ATTRIBUTE_ORDER = ["Seraph", "Prism", "Feral"];

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

/** `57% Feral · 43% Prism` — the same words the plugin's `summary` uses. */
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
  const stage = el("meta-stage");
  if (stage) {
    stage.textContent = status.final_stage ? `${status.stage} · max` : status.stage;
    stage.className = `meta-stage${status.can_evolve ? " ready" : ""}${status.final_stage ? " max" : ""}`;
    stage.title = `Stage ${Number(status.tier_index) + 1} of 6 · generation ${status.generation}`;
  }

  // The creature line: archetype, attribute, generation, seed.
  const line = el("meta-line");
  if (line) {
    line.innerHTML = "";
    const arche = document.createElement("span");
    arche.className = "meta-chip";
    // The attribute has its own chip right beside this one.
    arche.textContent = status.creature ? status.creature.archetype : "—";
    const attribute = document.createElement("span");
    attribute.className = `meta-chip attr ${attributeClass(status.attribute)}`;
    attribute.textContent = status.attribute || "—";
    const seed = document.createElement("span");
    seed.className = "meta-seed";
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
  const xpBar = el("meta-xp-bar");
  if (xpBar) {
    xpBar.style.width = `${final ? 100 : pct(have, whole)}%`;
    xpBar.classList.toggle("full", !!status.can_evolve);
  }
  const xpValue = el("meta-xp");
  if (xpValue) {
    xpValue.textContent = final
      ? "MAX"
      : `${Math.round(Math.min(have, whole))}/${Math.round(whole)}`;
    xpValue.title = final
      ? "This creature is at its final stage"
      : status.can_evolve
        ? `Ready to metamorphose (${Math.round(Number(status.xp || 0))} XP)`
        : shortfall(status);
  }

  const bond = Math.max(0, Math.min(1, Number(status.bond || 0)));
  const bondBar = el("meta-bond-bar");
  if (bondBar) bondBar.style.width = `${Math.round(bond * 100)}%`;
  const bondValue = el("meta-bond");
  if (bondValue) {
    bondValue.textContent = bond.toFixed(2);
    bondValue.title = bond > 0.66
      ? "High bond — branches lean Seraph"
      : bond < 0.33
        ? "Low bond — branches lean Feral"
        : "Neutral bond — Prism sits in the middle";
  }

  // The "new creature" picker follows the live creature until the user picks
  // something, so resetting does not silently change the archetype.
  const attribute = el("meta-attribute");
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
  if (attribute && !metamorphose.attributeTouched && status.attribute) {
    attribute.value = status.attribute;
  }

  const archetype = el("meta-archetype");
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
  if (archetype && !metamorphose.archetypeTouched && status.creature) {
    archetype.value = status.creature.archetype;
  }

  const meta = el("meta-meta");
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

  const bar = el("meta-blend-bar");
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

  const chips = el("meta-blend-chips");
  if (chips) {
    chips.innerHTML = "";
    for (const [attribute, percent] of rows) {
      const chip = document.createElement("span");
      chip.className = `meta-chip attr ${attributeClass(attribute)} meta-blend-chip`;
      chip.textContent = `${percent}% ${attribute}`;
      chip.title = blend && blend.lineage && blend.lineage.length
        ? `Evolution line: ${blend.lineage.join(" → ")}`
        : `${attribute} share of this creature's lineage`;
      chips.appendChild(chip);
    }
    // A pure creature still gets one chip, so the readout is never blank.
    if (!rows.length) {
      const chip = document.createElement("span");
      chip.className = "meta-chip meta-blend-chip";
      chip.textContent = "no attribute";
      chips.appendChild(chip);
    }
  }

  const traits = el("meta-blend-traits");
  if (traits) {
    const text = (blend && blend.flavour) || (status && status.attribute) || "";
    traits.textContent = text;
    traits.title = blend && blend.lineage && blend.lineage.length
      ? `Recency-weighted across ${blend.lineage.length} stages — the newest stage counts most`
      : "The traits this creature's attribute grows";
  }

  const block = el("meta-blend");
  if (block) block.dataset.dominant = (blend && blend.dominant) || (status && status.attribute) || "";
}

function renderBranches(status) {
  const list = el("meta-branches");
  if (!list) return;
  const branches = Array.isArray(status.branches) ? status.branches : [];
  list.innerHTML = "";

  if (!branches.length) {
    const empty = document.createElement("div");
    empty.className = "meta-empty";
    empty.textContent = status.final_stage
      ? "Final stage — nothing left to become."
      : "No branches offered.";
    list.appendChild(empty);
    syncEvolveButton(status);
    syncBackButton(status);
    return;
  }

  metamorphose.picked = Math.max(0, Math.min(branches.length - 1, metamorphose.picked));

  branches.forEach((branch, index) => {
    const row = document.createElement("button");
    row.type = "button";
    row.className = `meta-branch${index === metamorphose.picked ? " picked" : ""}`;
    row.dataset.branch = String(index);
    row.setAttribute("role", "radio");
    row.setAttribute("aria-checked", String(index === metamorphose.picked));
    const projected = branchMix(branch);
    row.title =
      `Evolve into ${branch.label} (${branch.attribute}) — branch seed ${branch.seed}` +
      (projected.length ? `\nMix after evolving: ${summarise(projected)}` : "");

    const name = document.createElement("span");
    name.className = "meta-branch-name";
    name.textContent = branch.stage || "";

    const attribute = document.createElement("span");
    attribute.className = `meta-chip attr ${attributeClass(branch.attribute)}`;
    attribute.textContent = branch.attribute || "";

    // The branch's own seed suffix is what distinguishes the three rows (the
    // stage and attribute are already labelled); the full slug lives in the
    // tooltip. A 31-character slug would only ever render as an ellipsis here.
    const detail = document.createElement("span");
    detail.className = "meta-branch-detail";
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
      mix.className = "meta-branch-mix";
      const text = branch.blend_summary || summarise(projected);
      mix.textContent = `→ ${text}`;
      if (branch.blended_flavour) mix.title = branch.blended_flavour;
      row.append(mix);
    }

    row.addEventListener("click", (event) => {
      event.preventDefault();
      metamorphose.picked = index;
      renderBranches(metamorphose.status || status);
      syncEvolveButton(metamorphose.status || status);
      syncBackButton(metamorphose.status || status);
    });
    list.appendChild(row);
  });

  syncEvolveButton(status);
  syncBackButton(status);
}

/** Enable/disable "Back" and say which stage it returns to. */
function syncBackButton(status) {
  const button = el("btn-meta-back");
  if (!button) return;
  const can = !!(status && status.can_revert);
  const previous = status && status.previous_stage;
  button.disabled = !can;
  button.title = can
    ? `Return to ${previous || "the previous stage"} — POST /revert ` +
      `(${status.rewind_depth} step${status.rewind_depth === 1 ? "" : "s"} of history; ` +
      "the attribute mix rewinds with it)"
    : "This creature is at its first stage";
  button.textContent = can ? `◀ ${previous || "Back"}` : "◀ Back";
}

function syncEvolveButton(status) {
  const button = el("btn-metamorphose-go");
  if (!button) return;
  const branches = (status && status.branches) || [];
  const branch = branches[metamorphose.picked];
  const ready = !!(status && status.can_evolve && branch);
  button.disabled = !ready;
  button.textContent = ready
    ? "Evolve"
    : status && status.final_stage
      ? "Final stage"
      : "Not ready";
  button.title = ready
    ? `Metamorphose into ${branch.stage} (${branch.attribute}) — POST /metamorphose`
    : status && status.final_stage
      ? "Mega is the last stage"
      : status
        ? `${shortfall(status)} — the plugin refuses until then`
        : "No creature yet";
}

/**
 * The control panel's header strip: the same recency-weighted mix, compressed
 * to one summary line and a 4 px stacked bar. `metamorphose` already owns the
 * `/creature/status` poll and the blend maths, so the panel header is filled
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
  metamorphose.missing = true;
  metamorphose.status = null;
  el("blend-readout")?.classList.add("hidden");
  const stage = el("meta-stage");
  if (stage) {
    stage.textContent = "offline";
    stage.className = "meta-stage off";
    stage.title = detail || "";
  }
  for (const id of ["meta-xp-bar", "meta-bond-bar"]) {
    const bar = el(id);
    if (bar) bar.style.width = "0%";
  }
  for (const id of ["meta-xp", "meta-bond"]) {
    const node = el(id);
    if (node) node.textContent = "—";
  }
  const line = el("meta-line");
  if (line) line.textContent = "The metamorphosis plugin is not loaded.";
  const meta = el("meta-meta");
  if (meta) meta.textContent = detail || "plugins/metamorphosis.wasm is missing";
  const list = el("meta-branches");
  if (list) {
    list.innerHTML = "";
    const empty = document.createElement("div");
    empty.className = "meta-empty";
    empty.textContent = "Build it with ./scripts/build-plugin.sh metamorphosis, then restart.";
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

/** Public render entry (used by the `creature` event and the poll). */
export function renderCreature(status) {
  if (!status || typeof status !== "object" || status.ok === false) {
    renderMissing(status && status.error);
    return;
  }
  metamorphose.missing = false;
  const stageKey = String(status.slug || status.stage || "");
  if (stageKey !== metamorphose.stageKey) {
    metamorphose.stageKey = stageKey;
    metamorphose.picked = 0;
  }
  metamorphose.status = status;
  renderCard(status);
}

/* ------------------------------------------------------------------ *
 * Reads and actions
 * ------------------------------------------------------------------ */

async function refreshCreature() {
  try {
    const res = await fetch(`${metamorphose.base}/creature/status`, { cache: "no-store" });
    if (res.status === 404) {
      renderMissing("GET /creature/status → 404");
      return;
    }
    const body = await res.json();
    renderCreature(body);
  } catch (err) {
    // The API can be down while the app is starting; the poll retries.
    renderMissing(String(err));
  }
}

/** POST JSON, then re-read. A refusal is shown, never swallowed. */
async function post(path, body, okText) {
  try {
    const res = await fetch(`${metamorphose.base}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body || {}),
    });
    const payload = await res.json().catch(() => ({}));
    if (!res.ok || payload.ok === false) {
      setNote(payload.error || `${path} → ${res.status}`, true);
      refreshCreature();
      return null;
    }
    setNote(typeof okText === "function" ? okText(payload) : okText || "");
    refreshCreature();
    return payload;
  } catch (err) {
    setNote(String(err), true);
    return null;
  }
}

function evolve() {
  const status = metamorphose.status;
  const branch = status && (status.branches || [])[metamorphose.picked];
  if (!status || !branch) return;
  post(
    "/metamorphose",
    { branch: metamorphose.picked },
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

/** Go back one stage: `POST /revert`. */
function rewind() {
  const status = metamorphose.status;
  if (!status || !status.can_revert) return;
  post("/revert", { steps: 1 }, (payload) =>
    `${payload.from} → ${payload.stage} (back) · ${payload.bones} bones, ${payload.parts} parts` +
    ` · ${payload.blend_summary || payload.attribute}`,
  );
}

/** The growth nudge: `POST /creature/xp`. Labelled as the API it is. */
function nudge(xp, bond) {
  post("/creature/xp", { xp, bond }, xp ? `+${xp} XP` : `bond ${bond > 0 ? "+" : ""}${bond}`);
}

function reset() {
  const archetype = el("meta-archetype") ? el("meta-archetype").value : "";
  const attribute = el("meta-attribute") ? el("meta-attribute").value : "";
  const rawSeed = el("meta-seed") ? el("meta-seed").value.trim() : "";
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
  metamorphose.picked = 0;
  post("/creature/reset", body, (payload) =>
    `new ${payload.status.creature.archetype} · ${payload.status.stage} · ${payload.status.slug}`,
  );
}

/* ------------------------------------------------------------------ *
 * Setup
 * ------------------------------------------------------------------ */

export async function setupMetamorphose() {
  if (metamorphose.ready) return;
  if (!card()) return; // the markup is missing — nothing to wire.

  el("btn-metamorphose")?.addEventListener("click", (event) => {
    event.preventDefault();
    toggleMetamorphoseCard();
  });
  el("btn-meta-close")?.addEventListener("click", (event) => {
    event.preventDefault();
    toggleMetamorphoseCard(false);
  });
  el("btn-meta-reload")?.addEventListener("click", (event) => {
    event.preventDefault();
    refreshCreature();
  });
  el("btn-metamorphose-go")?.addEventListener("click", (event) => {
    event.preventDefault();
    evolve();
  });
  el("btn-meta-back")?.addEventListener("click", (event) => {
    event.preventDefault();
    rewind();
  });
  el("btn-meta-xp")?.addEventListener("click", (event) => {
    event.preventDefault();
    nudge(60, 0);
  });
  el("btn-meta-bond")?.addEventListener("click", (event) => {
    event.preventDefault();
    nudge(0, 0.1);
  });
  el("btn-meta-reset")?.addEventListener("click", (event) => {
    event.preventDefault();
    reset();
  });

  // The archetype picker prefills from the live creature once we know it.
  const archetype = el("meta-archetype");
  archetype?.addEventListener("change", () => {
    metamorphose.archetypeTouched = true;
  });
  const attribute = el("meta-attribute");
  attribute?.addEventListener("change", () => {
    metamorphose.attributeTouched = true;
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

  // The plugin's own frames: `ready` (XP threshold crossed), `metamorphosed`,
  // `reset`. Any of them changes the card, so one listener covers all three.
  await listen("creature", (event) => {
    const payload = event && event.payload ? event.payload : {};
    if (payload.event === "metamorphosed") {
      setNote(
        `${payload.from} → ${payload.stage} (${payload.blend_summary || payload.attribute})`,
      );
      toast(`Metamorphosed into ${payload.stage}`);
      // Growth is the good news of this whole plugin — show it on the face.
      reactExpression("happy", 5);
    } else if (payload.event === "reverted") {
      setNote(`${payload.from} → ${payload.stage} (back to an earlier form)`);
      toast(`Back to ${payload.stage}`);
    } else if (payload.event === "ready") {
      setNote(`Growth complete — ${payload.stage} can evolve.`);
      reactExpression("surprised", 4);
    } else if (payload.event === "reset") {
      // A brand-new hatchling is worth a startled look.
      reactExpression("surprised", 4);
    }
    refreshCreature();
  });
  // A new model means the plugin may have been the one that swapped it.
  await listen("model-changed", (event) => {
    const model = event && event.payload ? event.payload : {};
    if (metamorphoseCardOpen() || String(model.name || "").startsWith("buddy-")) {
      refreshCreature();
    }
  });

  setInterval(refreshCreature, POLL_MS);
  metamorphose.ready = true;
  await refreshCreature();
  if (metamorphose.status && metamorphose.status.creature && archetype) {
    archetype.value = metamorphose.status.creature.archetype;
  }

  // CDP surface: read the card's state without a screenshot.
  if (window.__avatarDebug) {
    window.__avatarDebug.metamorphose = () => ({
      base: metamorphose.base,
      ready: metamorphose.ready,
      open: metamorphoseCardOpen(),
      missing: metamorphose.missing,
      stage: metamorphose.status ? metamorphose.status.stage : null,
      xp: metamorphose.status ? metamorphose.status.xp : null,
      xp_required: metamorphose.status ? metamorphose.status.xp_required : null,
      bond: metamorphose.status ? metamorphose.status.bond : null,
      attribute: metamorphose.status ? metamorphose.status.attribute : null,
      blend: metamorphose.status
        ? (metamorphose.status.attribute_blend || {
            dominant: metamorphose.status.attribute,
            percent: Object.fromEntries(blendOf(metamorphose.status)),
            summary: summarise(blendOf(metamorphose.status)),
          })
        : null,
      blendChips: [...document.querySelectorAll("#meta-blend-chips .meta-blend-chip")].map(
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
      can_evolve: metamorphose.status ? metamorphose.status.can_evolve : false,
      picked: metamorphose.picked,
      branches: metamorphose.status
        ? (metamorphose.status.branches || []).map(
            (b) => `${b.stage}:${b.attribute}:${b.blend_summary || ""}`,
          )
        : [],
      rig: metamorphose.status ? metamorphose.status.rig : null,
      can_revert: metamorphose.status ? metamorphose.status.can_revert : false,
      previous_stage: metamorphose.status ? metamorphose.status.previous_stage : null,
      backDisabled: !!((el("btn-meta-back") || {}).disabled),
      backLabel: (el("btn-meta-back") || {}).textContent || "",
      note: (el("metamorphose-note") || {}).textContent || "",
      rows: [...document.querySelectorAll(".meta-branch")].length,
      evolveDisabled: !!((el("btn-metamorphose-go") || {}).disabled),
    });
  }
}
