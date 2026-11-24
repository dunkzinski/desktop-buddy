//! The chest card: what is in the pod, and the four things you can do to it.
//!
//! The host owns the lifecycle, so this is a view with buttons — no state is kept
//! here beyond "is the card open". Every action re-reads `GET /chest` afterwards
//! rather than guessing what the mutation did, which is why a refused dismiss
//! (the pod still holds *your* things) can show the host's own words.

import {
  chestStatus,
  closeChest,
  dismissChest,
  findInChest,
  openChest,
  summonChest,
  takeFromChest,
  tidyChest,
} from "./entities.js";
import { wireRegions } from "./rig.js";

let card = null;
let els = null;
let visible = false;
let poll = 0;
let busy = false;
/** The last host refusal, kept so a re-render does not blink it away. */
let note = "";
/** How many times `say` has been called, so `act` knows if it was spoken for. */
let said = 0;

const PRETTY_DEF = {
  chest: "Pod",
  trinket: "Trinket",
  fishing_rod: "Fishing rod",
  loot_bag: "Loot bag",
};

/**
 * A display name from the entity's **def**, not its id prefix.
 *
 * An id is `chest:loot0`, whose prefix is the container, not the thing — reading
 * the prefix called every trinket a "pod".
 */
function nameOf(item) {
  const base = PRETTY_DEF[item.def] || item.def || "thing";
  const tail = String(item.id || "").split(":").slice(1).join(":");
  return tail ? `${base} · ${tail}` : base;
}

function say(message, tone = "error") {
  said += 1;
  note = message || "";
  if (!els || !els.note) return;
  els.note.textContent = note;
  els.note.style.color = tone === "ok" ? "#8ec48e" : "#e0a0a0";
}

/** Run an action, then re-read the pod. Errors land in the note, never the console. */
async function act(runController) {
  if (busy) return;
  busy = true;
  const spoken = said;
  if (els) els.buttons.forEach((button) => (button.disabled = true));
  try {
    const result = await runController();
    // Only speak if the controller itself said nothing: a rummage writes a whole
    // sentence ("out came a boot — then the Trinket"), and a generic "nothing to
    // change" used to overwrite it a millisecond later.
    if (said === spoken) {
      say(result && result.changed === false ? "nothing to change" : "", "ok");
    }
  } catch (err) {
    say(String(err && err.message ? err.message : err));
  } finally {
    busy = false;
    if (els) els.buttons.forEach((button) => (button.disabled = false));
    await render();
  }
}

function row(item) {
  const element = document.createElement("div");
  element.className = "chest-row";
  element.setAttribute("role", "listitem");
  const dot = document.createElement("span");
  dot.className = "chest-row-dot";
  // Pod property (its own trinkets) is muted; your things are the accent.
  const mine = item.pod_property !== true;
  dot.style.background = mine ? "#d9c26a" : "#6f9fbf";
  const name = document.createElement("span");
  name.className = "chest-row-name";
  name.textContent = item.label || label(item.id);
  name.title = item.id;
  const tag = document.createElement("span");
  tag.className = "chest-row-tag";
  tag.textContent = mine ? "yours" : "pod's";
  const take = document.createElement("button");
  take.className = "mini-btn";
  take.type = "button";
  take.textContent = "Take";
  take.title = `Lay it on the stage (POST /chest/take {id:"${item.id}"})`;
  take.addEventListener("click", (event) => {
    event.stopPropagation();
    act(() => takeFromChest(item.id));
  });
  element.append(dot, name, tag, take);
  return element;
}

/** Read the pod from the host and paint the card. */
export async function render() {
  if (!visible || !els) return;
  let view = null;
  try {
    view = await chestStatus();
  } catch (err) {
    say(`could not read the pod: ${err}`);
    return;
  }
  if (!view || !view.exists) {
    els.state.textContent = "away";
    els.state.dataset.state = "away";
    els.slots.textContent = "—";
    els.contents.replaceChildren();
    const empty = document.createElement("div");
    empty.className = "chest-empty";
    empty.textContent = "No pod on the stage. Summon one.";
    els.contents.append(empty);
    setVerbEnabled({ summon: true, open: false, fold: false, dismiss: false });
    els.look.disabled = true;
    return;
  }

  const state = view.state || "closed";
  els.state.textContent = state === "dormant" ? "away" : state;
  els.state.dataset.state = state;
  // No capacity to compare against: the pod has no bottom, so the only honest
  // readout is a count.
  const count = Number(view.count) || 0;
  els.slots.textContent = `${count} ${count === 1 ? "thing" : "things"}`;

  els.contents.replaceChildren();
  if (!view.contents || !view.contents.length) {
    const empty = document.createElement("div");
    empty.className = "chest-empty";
    empty.textContent = state === "open" ? "Empty — put something in it." : "Closed. Open it to see inside.";
    els.contents.append(empty);
  } else {
    for (const item of view.contents) {
      els.contents.append(row({ ...item, label: nameOf(item) }));
    }
  }

  const away = state === "dormant";
  setVerbEnabled({
    summon: true,
    open: !away && state !== "open",
    fold: state === "open",
    dismiss: !away,
  });
  // Rummaging needs the lid open; the host refuses it anyway, but a disabled
  // button says so before the click.
  els.look.disabled = away || state !== "open";
}

function setVerbEnabled({ summon, open, fold, dismiss }) {
  if (!els) return;
  els.summon.disabled = !summon;
  els.open.disabled = !open;
  els.fold.disabled = !fold;
  els.dismiss.disabled = !dismiss;
}

export function isChestCardOpen() {
  return visible;
}

export function openChestCard() {
  if (!card) return;
  // The tile in Actions lights up while its card is on screen, the way the
  // survival and metamorphosis tiles do — `aria-pressed` is for the screen reader,
  // `.active` is for the eye, and forgetting one makes the two disagree.
  els?.toggle?.classList.add("active");
  els?.toggle?.setAttribute("aria-pressed", "true");
  if (!visible) {
    visible = true;
    card.classList.remove("hidden");
    card.setAttribute("aria-hidden", "false");
    // A two-second poll keeps slots honest while something else (a plugin, an
    // agent, a walk) is moving things in and out.
    poll = window.setInterval(() => render(), 2000);
    say("");
  }
  wireRegions();
  render();
}

export function closeChestCard() {
  if (!card || !visible) return;
  visible = false;
  els?.toggle?.classList.remove("active");
  els?.toggle?.setAttribute("aria-pressed", "false");
  card.classList.add("hidden");
  card.setAttribute("aria-hidden", "true");
  if (poll) window.clearInterval(poll);
  poll = 0;
  say("");
  wireRegions();
}

export function toggleChestCard() {
  if (visible) closeChestCard();
  else openChestCard();
}

/** Called when an entity event lands, so the card tracks a pod summoned elsewhere. */
export function refreshChestCard() {
  if (visible) render();
}

export function initChestCard() {
  card = document.getElementById("chest-card");
  if (!card) {
    console.warn("[avatar] chest card markup is missing");
    return;
  }
  els = {
    state: document.getElementById("chest-state"),
    slots: document.getElementById("chest-slots"),
    contents: document.getElementById("chest-contents"),
    note: document.getElementById("chest-note"),
    summon: document.getElementById("btn-chest-summon"),
    open: document.getElementById("btn-chest-open"),
    fold: document.getElementById("btn-chest-fold"),
    dismiss: document.getElementById("btn-chest-dismiss"),
    tidy: document.getElementById("btn-chest-tidy"),
    look: document.getElementById("btn-chest-find"),
    buttons: [],
  };
  // Built from the literal above, so a button added in one place cannot be
  // missing here — a `undefined` in this list makes every action throw.
  els.buttons = [els.summon, els.open, els.fold, els.dismiss, els.tidy].filter(Boolean);

  document.getElementById("btn-chest-close").addEventListener("click", closeChestCard);

  const query = document.getElementById("chest-query");
  const look = () => {
    const want = (query.value || "").trim();
    if (!want) {
      query.focus();
      say("say what to look for");
      return;
    }
    act(async () => {
      const body = await findInChest(want);
      // The host writes the sentence — it knows what came out and in what order.
      say(body.narrative || (body.found ? "found it" : "not in there"), "ok");
      query.value = "";
      return body;
    });
  };
  document.getElementById("btn-chest-find").addEventListener("click", look);
  query.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      look();
    }
    // Space must type a space, not reach the global emote shortcuts.
    event.stopPropagation();
  });
  els.tidy.addEventListener("click", () => {
    act(async () => {
      const body = await tidyChest();
      const removed = Number(body.removed) || 0;
      say(removed ? `swept up ${removed} thing${removed === 1 ? "" : "s"}` : "nothing to sweep", "ok");
      return body;
    });
  });
  // The panel button that reveals the card (the pod itself is the other way in).
  const toggle = document.getElementById("btn-chest");
  toggle?.addEventListener("click", () => toggleChestCard());
  els.toggle = toggle;

  els.summon.addEventListener("click", () => {
    // No `at`: the host puts it beside the avatar, and the walk choreography in
    // `main.js` takes over from the `chest-summoned` frame.
    act(async () => {
      const body = await summonChest();
      const seeded = Number(body.seeded) || 0;
      say(seeded ? `summoned with ${seeded} trinket${seeded === 1 ? "" : "s"}` : "summoned", "ok");
      return body;
    });
  });
  els.open.addEventListener("click", () => act(openChest));
  els.fold.addEventListener("click", () => act(closeChest));
  els.dismiss.addEventListener("click", (event) => {
    // Shift-click forces: the pod drops anything you put in on the stage, and
    // nothing is ever deleted.
    const force = event.shiftKey;
    act(async () => {
      try {
        return await dismissChest(force);
      } catch (err) {
        const message = String(err && err.message ? err.message : err);
        if (!force && message.includes("force")) {
          throw new Error(`${message} (shift-click Dismiss to force it)`);
        }
        throw err;
      }
    });
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && visible) {
      closeChestCard();
      event.stopPropagation();
    }
  });
}
