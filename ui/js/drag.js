/**
 * desktop-buddy — floating panel drag, layout persistence and z-order
 *
 * Every floating card in the overlay is positioned by CSS defaults. This module
 * makes each one draggable by its header, clamps it so the header stays
 * reachable, remembers its position in localStorage, and raises it above its
 * siblings when it is used.
 *
 * Click-through: the overlay is a full-screen transparent window and only the
 * rectangles `rig.js::wireRegions` publishes accept the mouse. A drag therefore
 * republishes regions on every animation frame while the pointer moves, and
 * again synchronously on release. While a drag is in flight the body carries
 * `panel-dragging`, which makes `wireRegions` publish the whole viewport — the
 * same guarantee the avatar drag uses, so the 35 ms Rust cursor tracker can
 * never flip the overlay click-through mid-gesture and steal the pointer.
 *
 * Coordinates are CSS pixels inside the overlay window. The window spans the
 * whole virtual desktop (including monitors left of/above the primary), so its
 * own origin is always 0,0 and dragging across monitors is just a larger
 * `left`/`top`; the clamp keeps the header inside the live viewport.
 */

import { viewport, toast } from "./state.js";
import { wireRegions } from "./rig.js";

/** Bump when the stored shape changes; a stale key is ignored, not migrated. */
const LAYOUT_KEY = "desktop-buddy.panel-layout.v1";

/**
 * The pre-rebrand storage key. Read once and rewritten under `LAYOUT_KEY`, so a
 * renamed build does not snap every pop-out back to its CSS default.
 */
const LEGACY_LAYOUT_KEY = "char-rig.panel-layout.v1";

/** How much of a panel's header must stay inside the viewport. */
const MIN_VISIBLE_W = 120;
const HEADER_REACH = 34;

/** A press only becomes a drag after this many pixels. */
const DRAG_THRESHOLD = 3;

/** Base/ceiling for the floating-panel stack. Settings sits at 980. */
const Z_BASE = 900;
const Z_MAX = 979;

/**
 * The floating panels. `handle` is the only part that starts a drag, so a
 * button, tab, input or scrollbar inside the panel keeps its own behaviour.
 *
 * `stretch` panels are laid out with both `top` and `bottom` (the rig
 * inspector). They keep their bottom margin while being dragged, so the panel
 * never runs off the bottom of the screen.
 */
const PANELS = [
  { id: "titlebar", handle: "#titlebar", stack: false },
  { id: "control-panel", handle: "#control-panel header" },
  { id: "inspector", handle: "#inspector header", stretch: true },
  { id: "watcher-drawer", handle: "#watcher-drawer header" },
  { id: "history-drawer", handle: "#history-drawer header" },
  { id: "survival-card", handle: "#survival-card header" },
  { id: "chest-card", handle: "#chest-card header" },
  { id: "behaviour-card", handle: "#behaviour-card header" },
  { id: "metamorphose-card", handle: "#metamorphose-card header" },
  // A modal-ish chooser: keep its own high layer instead of joining the stack.
  { id: "import-chooser", handle: "#import-chooser h3", stack: false },
];

/** Elements that must never start a drag even when they sit in a header. */
const INTERACTIVE = "button, a, input, select, textarea, label, [data-no-drag]";

let layout = loadLayout();
let dragging = null;
let topZ = Z_BASE;
let regionFrame = 0;
let resizeTimer = 0;
let ready = false;

/* ---------- storage ---------- */

function loadLayout() {
  try {
    let raw = window.localStorage.getItem(LAYOUT_KEY);
    if (!raw) {
      const legacy = window.localStorage.getItem(LEGACY_LAYOUT_KEY);
      if (legacy) {
        window.localStorage.setItem(LAYOUT_KEY, legacy);
        window.localStorage.removeItem(LEGACY_LAYOUT_KEY);
        raw = legacy;
      }
    }
    const parsed = raw ? JSON.parse(raw) : null;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const clean = {};
    for (const [id, pos] of Object.entries(parsed)) {
      if (!pos || typeof pos !== "object") continue;
      const left = Number(pos.left);
      const top = Number(pos.top);
      if (!Number.isFinite(left) || !Number.isFinite(top)) continue;
      clean[id] = { left, top };
    }
    return clean;
  } catch (err) {
    console.warn("[avatar] panel layout ignored:", err);
    return {};
  }
}

function saveLayout() {
  try {
    window.localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
  } catch (err) {
    console.warn("[avatar] could not save panel layout:", err);
  }
}

/* ---------- geometry ---------- */

function viewportSize() {
  return {
    w: viewport.w || window.innerWidth || 0,
    h: viewport.h || window.innerHeight || 0,
  };
}

function clampNumber(value, min, max) {
  return Math.min(Math.max(value, min), max);
}

/**
 * Clamp a panel position so its header stays reachable. The left edge can never
 * go negative in overlay coordinates; the top edge keeps `HEADER_REACH` pixels
 * on screen. A stretch panel's bottom margin is preserved by the caller.
 */
function clampPosition(left, top, width, height, stretch) {
  const { w, h } = viewportSize();
  const maxLeft = Math.max(0, w - Math.min(MIN_VISIBLE_W, width || MIN_VISIBLE_W));
  const maxTop = stretch
    ? Math.max(0, h - HEADER_REACH - 220) // keep a usable body on screen
    : Math.max(0, h - HEADER_REACH);
  return {
    left: clampNumber(left, 0, maxLeft),
    top: clampNumber(top, 0, maxTop),
  };
}

/** Materialise a panel's current CSS position as inline left/top. */
function place(el, left, top, stretch) {
  el.style.left = `${Math.round(left)}px`;
  el.style.top = `${Math.round(top)}px`;
  el.style.right = "auto";
  el.style.bottom = "auto";
  if (stretch) {
    const { h } = viewportSize();
    el.style.height = `${Math.max(180, Math.round(h - top - 14))}px`;
  }
  // The import chooser is centred with a transform; once it is dragged the
  // inline coordinates are the truth.
  if (el.id === "import-chooser") el.style.transform = "none";
}

/** Rendered and interactive: not hidden, not display:none, has real geometry. */
function isRendered(el) {
  if (!el || el.classList.contains("hidden")) return false;
  const style = window.getComputedStyle(el);
  if (style.display === "none" || style.visibility === "hidden" || style.pointerEvents === "none") {
    return false;
  }
  const rect = el.getBoundingClientRect();
  return rect.width >= 1 && rect.height >= 1;
}

/** Rendered *and* intersecting the viewport — what a drag press needs. */
function isVisible(el) {
  if (!isRendered(el)) return false;
  const rect = el.getBoundingClientRect();
  const { w, h } = viewportSize();
  return rect.right > 0 && rect.bottom > 0 && rect.left < w && rect.top < h;
}

/* ---------- z-order ---------- */

function zIndex(el) {
  const value = parseInt(window.getComputedStyle(el).zIndex, 10);
  return Number.isFinite(value) ? value : 0;
}

function stackEntries() {
  return PANELS.filter((entry) => entry.stack !== false);
}

/** Raise one panel above its siblings (never above Settings / the command bar). */
function bringToFront(entry) {
  if (entry.stack === false) return;
  const el = document.getElementById(entry.id);
  if (!el) return;
  // Always raise: two panels can share a default z-index (the inspector and
  // the watcher console both ship at 950), and "already at topZ" is not enough
  // to know the clicked one is actually above its tie.
  el.style.zIndex = String(++topZ);
  if (topZ >= Z_MAX) normalizeStack();
}

/** Re-pack the stack without changing its relative order. */
function normalizeStack() {
  const entries = stackEntries()
    .map((entry) => ({ entry, el: document.getElementById(entry.id) }))
    .filter(({ el }) => el)
    .sort((a, b) => zIndex(a.el) - zIndex(b.el));
  topZ = Z_BASE;
  for (const { el } of entries) el.style.zIndex = String(++topZ);
}

/* ---------- hit regions ---------- */

/** Coalesce region republishes to one per animation frame. */
function scheduleRegions() {
  if (regionFrame) return;
  regionFrame = window.requestAnimationFrame(() => {
    regionFrame = 0;
    wireRegions();
  });
}

/* ---------- drag ---------- */

function onPointerDown(event) {
  // One gesture at a time: a second pointer (touch, pen, a stray mouse button)
  // must not overwrite the active drag and strand its pointerup listener.
  if (dragging) return;
  if (event.button !== 0 && event.pointerType === "mouse") return;
  const handle = event.currentTarget;
  const entry = handle.__panelEntry;
  if (!entry) return;
  if (event.target.closest && event.target.closest(INTERACTIVE)) return;

  const el = document.getElementById(entry.id);
  if (!isVisible(el)) return;

  event.preventDefault();
  // Stop the press from reaching the canvas underneath: a panel gesture must
  // never become an avatar drag or click, even when the panel sits over it.
  event.stopPropagation();

  const rect = el.getBoundingClientRect();
  // The rect is captured before the first move; the element is only converted
  // to inline left/top once the gesture actually becomes a drag, so a plain
  // click on a header never pins a bottom/right-anchored panel in place.

  dragging = {
    entry,
    el,
    pointerId: event.pointerId,
    startX: event.clientX,
    startY: event.clientY,
    left: rect.left,
    top: rect.top,
    width: rect.width,
    height: rect.height,
    moved: false,
  };

  document.body.classList.add("panel-dragging");
  el.classList.add("panel-dragging");
  try {
    el.setPointerCapture(event.pointerId);
  } catch (err) {
    // Pointer capture is an optimisation; the window listeners still work.
  }

  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp);
  window.addEventListener("pointercancel", onPointerUp);
  window.addEventListener("blur", onDragBlur);
  // Publish the interactive viewport before the first move: the Rust tracker
  // runs on its own 35 ms clock and must not see a click-through gap.
  wireRegions();
}

function onPointerMove(event) {
  if (!dragging || event.pointerId !== dragging.pointerId) return;
  const dx = event.clientX - dragging.startX;
  const dy = event.clientY - dragging.startY;
  if (!dragging.moved && Math.hypot(dx, dy) < DRAG_THRESHOLD) return;
  dragging.moved = true;
  event.preventDefault();

  const { entry, el } = dragging;
  const clamped = clampPosition(
    dragging.left + dx,
    dragging.top + dy,
    dragging.width,
    dragging.height,
    entry.stretch,
  );
  place(el, clamped.left, clamped.top, entry.stretch);
  scheduleRegions();
}

/** A drag whose pointerup never arrives (alt-tab, focus stolen) must end. */
function onDragBlur() {
  if (!dragging) return;
  onPointerUp({ pointerId: dragging.pointerId });
}

function onPointerUp(event) {
  if (!dragging || event.pointerId !== dragging.pointerId) return;
  const { entry, el, moved } = dragging;

  try {
    el.releasePointerCapture(event.pointerId);
  } catch (err) {
    // The pointer may already be gone (cancel); nothing to release.
  }
  window.removeEventListener("pointermove", onPointerMove);
  window.removeEventListener("pointerup", onPointerUp);
  window.removeEventListener("pointercancel", onPointerUp);
  window.removeEventListener("blur", onDragBlur);

  document.body.classList.remove("panel-dragging");
  el.classList.remove("panel-dragging");

  if (moved) {
    const rect = el.getBoundingClientRect();
    layout[entry.id] = { left: Math.round(rect.left), top: Math.round(rect.top) };
    saveLayout();
  }
  dragging = null;

  // Publish the final geometry immediately, not on the next timer tick: the
  // released panel must keep the pointer (and its hit box) where it landed.
  wireRegions();
}

/* ---------- load / restore ---------- */

function applyStoredPosition(entry) {
  const pos = layout[entry.id];
  if (!pos) return;
  const el = document.getElementById(entry.id);
  if (!el) return;

  const rect = el.getBoundingClientRect();
  const width = rect.width || MIN_VISIBLE_W;
  const height = rect.height || HEADER_REACH;
  const clamped = clampPosition(pos.left, pos.top, width, height, entry.stretch);

  // A saved position that is fully off-screen (a monitor arrangement that no
  // longer exists, or a stale entry) falls back to the CSS default instead of
  // parking the panel somewhere unreachable.
  const { w, h } = viewportSize();
  const offscreen =
    pos.left + Math.min(MIN_VISIBLE_W, width) <= 0 ||
    pos.top + HEADER_REACH <= 0 ||
    pos.left >= w ||
    pos.top >= h;
  if (offscreen) {
    delete layout[entry.id];
    saveLayout();
    return;
  }
  place(el, clamped.left, clamped.top, entry.stretch);
  // Persist the clamped value so a monitor that shrank does not re-clamp the
  // same panel on every reload.
  if (clamped.left !== pos.left || clamped.top !== pos.top) {
    layout[entry.id] = { left: Math.round(clamped.left), top: Math.round(clamped.top) };
    saveLayout();
  }
}

function applyAll() {
  for (const entry of PANELS) {
    if (isRendered(document.getElementById(entry.id))) applyStoredPosition(entry);
  }
}

function onResize() {
  window.clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(() => {
    for (const entry of PANELS) {
      // Only panels the user actually moved have a saved layout; a default
      // bottom/right-anchored panel must keep following its CSS anchor when the
      // window resizes instead of being pinned by a clamp.
      if (!layout[entry.id]) continue;
      const el = document.getElementById(entry.id);
      if (!isRendered(el)) continue;
      const rect = el.getBoundingClientRect();
      const clamped = clampPosition(rect.left, rect.top, rect.width, rect.height, entry.stretch);
      if (clamped.left !== rect.left || clamped.top !== rect.top) {
        place(el, clamped.left, clamped.top, entry.stretch);
        layout[entry.id] = { left: Math.round(clamped.left), top: Math.round(clamped.top) };
        saveLayout();
      }
    }
    wireRegions();
  }, 120);
}

/* ---------- public API ---------- */

/** Reset every panel to its CSS default position. */
export function resetPanelLayout() {
  layout = {};
  try {
    window.localStorage.removeItem(LAYOUT_KEY);
    window.localStorage.removeItem(LEGACY_LAYOUT_KEY);
  } catch (err) {
    console.warn("[avatar] could not clear panel layout:", err);
  }
  for (const entry of PANELS) {
    const el = document.getElementById(entry.id);
    if (!el) continue;
    for (const prop of ["left", "top", "right", "bottom", "height", "transform", "z-index"]) {
      el.style.removeProperty(prop);
    }
  }
  // Re-seed the stack above the CSS defaults, or the next click would land a
  // panel *below* a sibling that still ships at a higher z-index.
  topZ = Math.max(
    Z_BASE,
    ...stackEntries().map((entry) => {
      const el = document.getElementById(entry.id);
      return el ? zIndex(el) : Z_BASE;
    }),
  );
  wireRegions();
  toast("Panel layout reset");
}

/** Wire the draggable headers, restore saved positions and watch for opens. */
export function setupPanelDrag() {
  if (ready) return;
  ready = true;

  for (const entry of PANELS) {
    const handle = document.querySelector(entry.handle);
    if (!handle) continue;
    handle.__panelEntry = entry;
    handle.classList.add("panel-drag-handle");
    handle.addEventListener("pointerdown", onPointerDown);

    // Any press inside the panel raises it, not just the header — clicking a
    // tab or a button should still bring the card you are using to the front.
    const el = document.getElementById(entry.id);
    if (el && entry.stack !== false) {
      el.addEventListener("pointerdown", () => bringToFront(entry), true);
    }
  }

  // A panel that opens later (survival, metamorphosis, history, the watcher
  // console, the import chooser) gets its saved position when it becomes
  // visible; hidden elements have no measurable geometry at load.
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      const el = record.target;
      if (!el || !el.id) continue;
      const entry = PANELS.find((candidate) => candidate.id === el.id);
      if (!entry) continue;
      // The drag itself toggles a class on the element; never re-apply a saved
      // position in the middle of a gesture.
      if (dragging && dragging.el === el) continue;
      if (!el.classList.contains("hidden")) applyStoredPosition(entry);
    }
  });
  for (const entry of PANELS) {
    const el = document.getElementById(entry.id);
    if (el) observer.observe(el, { attributes: true, attributeFilter: ["class"] });
  }

  // Start the stack above whatever CSS z-index the panels already carry, so a
  // click never *lowers* a panel that ships at a higher default.
  topZ = Math.max(
    Z_BASE,
    ...stackEntries().map((entry) => {
      const el = document.getElementById(entry.id);
      return el ? zIndex(el) : Z_BASE;
    }),
  );

  // Restore the visible panels now (titlebar and control panel at boot).
  applyAll();
  window.addEventListener("resize", onResize);

  const resetButton = document.getElementById("btn-reset-layout");
  if (resetButton) resetButton.addEventListener("click", resetPanelLayout);

  if (window.__avatarDebug) {
    window.__avatarDebug.panelLayout = () => JSON.parse(JSON.stringify(layout));
    window.__avatarDebug.resetPanelLayout = resetPanelLayout;
    window.__avatarDebug.panelDragging = () => document.body.classList.contains("panel-dragging");
  }
}

/** True while a floating panel header owns the pointer. */
export function isPanelDragging() {
  return document.body.classList.contains("panel-dragging");
}
