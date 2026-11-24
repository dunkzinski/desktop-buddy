//! The **brain & body** card: the systems you flick on and off mid-session.
//!
//! Watcher, AI brain, voice, push-to-talk and walk speed used to live in the
//! control panel, which made a panel that is mostly *chat* also the place you go
//! to mute the avatar. They keep their element ids, so the code that owns each
//! behaviour (`main.js`, `settings.js`, `plugins.js`) did not have to move — this
//! module only owns the card.

import { wireRegions } from "./rig.js";

let card = null;
let toggle = null;
let visible = false;

export function isBehaviourCardOpen() {
  return visible;
}

export function openBehaviourCard() {
  if (!card) return;
  visible = true;
  card.classList.remove("hidden");
  card.setAttribute("aria-hidden", "false");
  toggle?.classList.add("active");
  toggle?.setAttribute("aria-pressed", "true");
  wireRegions();
}

export function closeBehaviourCard() {
  if (!card || !visible) return;
  visible = false;
  card.classList.add("hidden");
  card.setAttribute("aria-hidden", "true");
  toggle?.classList.remove("active");
  toggle?.setAttribute("aria-pressed", "false");
  wireRegions();
}

export function toggleBehaviourCard() {
  if (visible) closeBehaviourCard();
  else openBehaviourCard();
}

export function initBehaviourCard() {
  card = document.getElementById("behaviour-card");
  if (!card) {
    console.warn("[avatar] behaviour card markup is missing");
    return;
  }
  toggle = document.getElementById("btn-behaviour");
  toggle?.addEventListener("click", () => toggleBehaviourCard());
  document.getElementById("btn-behaviour-close")?.addEventListener("click", closeBehaviourCard);

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && visible) {
      closeBehaviourCard();
      event.stopPropagation();
    }
  });
}
