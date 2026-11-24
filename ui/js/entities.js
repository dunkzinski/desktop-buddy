/**
 * desktop-buddy — prop entities on the canvas
 *
 * An item is a **miniature rig** with its own bones, parts, clips and state,
 * attached to a semantic socket. The guest (or an HTTP caller) owns the state;
 * this module answers only: where is it, what clip is it on, and which parts does
 * it paint this frame?
 *
 * Two rules keep it from becoming a second renderer:
 *
 * * transforms come from `rig.js` (`computeWorldInto`, `partWorldMatrixIn`,
 *   `socketTransformIn`) — the same maths as the avatar, just a different root;
 * * clips are sampled by `animator.js::sampleClip` — the same easing rules.
 *
 * The rigs arrive with `GET /entities` (they are generated in Rust from
 * `(def, seed, params)`); this module never generates or guesses geometry.
 *
 * State is fed two ways and both are cheap: SSE/Tauri events only mark it dirty,
 * and a 2 s reconcile covers a dropped frame — the same recovery shape the
 * scavenging walk uses. A missed event can never leave a stale rod on screen.
 */

import { avatar, RT, num, round } from "./state.js";
import { call } from "./ipc.js";
import {
  addRegionSource,
  computeWorldInto,
  makeRigContext,
  partLocalRect,
  partWorldMatrixIn,
  socketTransformIn,
} from "./rig.js";
import { sampleClip } from "./animator.js";

const DEFAULT_BASE = "http://127.0.0.1:8765";
/** How often `GET /entities` is re-read when nothing tells us it changed. */
const RECONCILE_MS = 2000;
/** A burst of events (spawn + attach + state) costs one refetch, not three. */
const REFRESH_DEBOUNCE_MS = 120;
/** Nested attachments (helmet → lamp → lantern) resolve this deep at most. */
const MAX_ATTACH_DEPTH = 4;

/** id -> { entity, ctx, signature } */
const cache = new Map();
let revision = -1;
let dirty = true;
let inflight = false;
let lastFetch = 0;
let lastError = null;
let baseUrl = "";
const stats = { fetches: 0, failures: 0, drawables: 0, refreshedAt: 0 };

/** The API base the app actually bound (it can slide off 8765). */
async function apiBase() {
  if (baseUrl) return baseUrl;
  try {
    const info = await call("get_runtime_info");
    const url = info && typeof info.api_url === "string" ? info.api_url : "";
    if (/^https?:\/\//.test(url)) {
      baseUrl = url.replace(/\/$/, "");
      return baseUrl;
    }
  } catch (_) {
    // Not in Tauri, or the command is not up yet: fall back to the default.
  }
  baseUrl = DEFAULT_BASE;
  return baseUrl;
}

/* ---------- the chest, from the page ---------- */

/**
 * POST one of the chest routes and mark the roster dirty.
 *
 * The page never invents chest state: the host owns the lifecycle, this only
 * asks. A refusal is thrown with the host's own words, because those name the
 * item or the slot (`the chest is full (7 of 7 slots)`).
 */
async function chestPost(path, payload) {
  const base = await apiBase();
  const res = await fetch(`${base}/chest/${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload || {}),
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok || body.ok === false) {
    throw new Error(body.error || `chest/${path} refused (${res.status})`);
  }
  markDirty();
  return body;
}

export function openChest() {
  return chestPost("open");
}

export function closeChest() {
  return chestPost("close");
}

export function summonChest(at) {
  return chestPost("summon", at ? { at } : {});
}

export function dismissChest(force = false) {
  return chestPost("dismiss", { force: !!force });
}

export function putInChest(id) {
  return chestPost("put", { id });
}

export function takeFromChest(id) {
  return chestPost("take", { id });
}

/**
 * Rummage. One to three *wrong* things come out first, then the thing asked for.
 *
 * Not a lookup: the host deliberately overshoots, and `narrative` is the sentence
 * a caller can hand straight to the gremlin's mouth.
 */
export function findInChest(query, seed) {
  return chestPost("find", seed === undefined ? { query } : { query, seed });
}

/** Sweep up what a rummage spat onto the stage. The pod's contents are untouched. */
export function tidyChest() {
  return chestPost("tidy");
}

/** The pod's anchor in stage coordinates, or `null` when there is none. */
export function chestOrigin() {
  for (const [, entry] of cache) {
    const entity = entry && entry.entity;
    if (entity && entity.def === "chest" && entity.attach) {
      const offset = entity.attach.offset;
      if (Array.isArray(offset) && offset.length === 2) {
        return { x: num(offset[0]), y: num(offset[1]) };
      }
    }
  }
  return null;
}

/**
 * Move the pod on screen *before* the host knows: drag feedback.
 *
 * The write goes out once, on release — a `POST` per mousemove would be a stack of
 * writes for a gesture the host does not need to arbitrate.
 */
export function moveChestLocal(x, y) {
  for (const [, entry] of cache) {
    const entity = entry && entry.entity;
    if (entity && entity.def === "chest" && entity.attach) {
      entity.attach.parent = "world";
      entity.attach.socket = "world";
      entity.attach.offset = [Math.round(x), Math.round(y)];
      return true;
    }
  }
  return false;
}

/** Tell the host where the pod was put. */
export async function commitChestMove(x, y) {
  const base = await apiBase();
  const res = await fetch(`${base}/entities/update`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      id: "chest",
      attach: {
        parent: "world",
        socket: "world",
        offset: [Math.round(x), Math.round(y)],
      },
    }),
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok || body.ok === false) {
    throw new Error(body.error || `could not move the pod (${res.status})`);
  }
  markDirty();
  return body;
}

/** The host's view: state, capacity, contents. */
export async function chestStatus() {
  const base = await apiBase();
  const res = await fetch(`${base}/chest`, { cache: "no-store" });
  const body = await res.json().catch(() => ({}));
  return body.chest || null;
}

/**
 * Screen bounds of the pod, or `null` when there is none (or it is dormant).
 *
 * Used for two things: the click-through region the host needs so a click on the
 * pod reaches the page at all, and the hit test that opens its card. Computed
 * from the same part matrices the renderer uses — a second, hand-rolled box for
 * a procedurally generated shape would drift the moment the generator changed.
 */
export function chestBounds() {
  const slots = slotMap();
  let hit = null;
  for (const [id, slot] of slots) {
    if (slot.entry.entity.def === "chest") {
      hit = [id, slot];
      break;
    }
  }
  if (!hit) return null;
  const [id, slot] = hit;
  // `dormant` is off the stage: nothing to click.
  if (slot.entry.entity.state === "dormant") return null;
  const root = resolveRoot(id, slots);
  if (!root) return null;

  const ctx = slot.entry.ctx;
  computeWorldInto(ctx, root);
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const bone of ctx.boneList) {
    if (!ctx.world[bone.id]) continue;
    for (const part of bone.parts) {
      if (part.visible === false) continue;
      const matrix = partWorldMatrixIn(ctx.world, bone, part);
      if (!matrix) continue;
      const rect = partLocalRect(part);
      const cos = Math.cos(matrix.rot);
      const sin = Math.sin(matrix.rot);
      const corners = [
        [rect.x, rect.y],
        [rect.x + rect.w, rect.y],
        [rect.x + rect.w, rect.y + rect.h],
        [rect.x, rect.y + rect.h],
      ];
      for (const [cx, cy] of corners) {
        const mx = matrix.mirror < 0 ? -1 : 1;
        const x = matrix.x + (mx * cx * cos - cy * sin) * matrix.scale;
        const y = matrix.y + (mx * cx * sin + cy * cos) * matrix.scale;
        if (x < minX) minX = x;
        if (y < minY) minY = y;
        if (x > maxX) maxX = x;
        if (y > maxY) maxY = y;
      }
    }
  }
  if (!Number.isFinite(minX) || maxX <= minX) return null;
  return { x: minX, y: minY, w: maxX - minX, h: maxY - minY };
}

/** Is this screen point on the pod? */
export function overChest(point) {
  const box = chestBounds();
  if (!box) return false;
  return point.x >= box.x && point.x <= box.x + box.w &&
    point.y >= box.y && point.y <= box.y + box.h;
}

/** Something changed; refetch as soon as the debounce allows. */
export function markDirty() {
  dirty = true;
}

/** Signature of the *inputs* to the generated rig: identity, not the rig blob. */
function signatureOf(entity) {
  const params = entity && entity.params ? JSON.stringify(entity.params) : "null";
  return `${entity.def}|${entity.seed}|${params}`;
}

/**
 * Read the whole roster. Kept small on purpose: entities are decoration until a
 * guest claims them, so a failure is logged and retried by the reconcile timer,
 * never thrown into the frame loop.
 */
export async function refresh() {
  if (inflight) return;
  inflight = true;
  // Claim this round's dirt *before* the request: a spawn that lands mid-fetch
  // must re-arm the flag, or its entity waits for the two-second reconcile.
  dirty = false;
  try {
    const base = await apiBase();
    const response = await fetch(`${base}/entities`, { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data = await response.json();
    const list = Array.isArray(data.entities) ? data.entities : [];
    const next = new Map();
    for (const entity of list) {
      if (!entity || typeof entity.id !== "string") continue;
      const signature = signatureOf(entity);
      const existing = cache.get(entity.id);
      // Rebuild the rig context only when the *inputs* changed; a state change
      // reuses the bones and only swaps clip offsets.
      const ctx = existing && existing.signature === signature
        ? existing.ctx
        : makeRigContext(entity.rig || {});
      next.set(entity.id, { entity, ctx, signature });
    }
    cache.clear();
    for (const [id, entry] of next) cache.set(id, entry);
    // A throw whose entity is gone must not keep a slot in the flight map.
    pruneThrows(new Set(next.keys()));
    revision = Number(data.revision || 0);
    lastError = null;
    stats.fetches += 1;
    stats.refreshedAt = Date.now();
  } catch (err) {
    stats.failures += 1;
    lastError = String(err && err.message ? err.message : err);
  } finally {
    inflight = false;
    lastFetch = Date.now();
  }
}

/** Load once at startup (the page may boot before anything exists). */
let regionSourceAdded = false;

export function initEntities() {
  if (!regionSourceAdded) {
    // Tell the click map about the pod; `rig.js` publishes it while it exists.
    addRegionSource(chestBounds);
    regionSourceAdded = true;
  }
  refresh();
}

/**
 * Advance every prop's clock and clip. Called once per frame from `main.js`.
 *
 * A state whose name is also a clip plays that clip by itself — that is the
 * property that keeps this module prop-agnostic: the rod has a `cast` state and a
 * `cast` clip, so `POST /entities/update {"state":"cast"}` is all a plugin needs.
 */
/**
 * Advance every entity's clock.
 *
 * `frameMs` is the page's frame clock (a relative `performance.now()`, which is
 * what the avatar's own animation runs on). Clip start times come from the
 * **host** and are epoch milliseconds, so the two must never be subtracted
 * directly: doing that once made every entity clip sit on its first frame (`t`
 * went hugely negative and clamped to 0) while the state names and `clip` fields
 * all looked correct.
 */
export function stepEntities() {
  // One clock for entities: the host's epoch milliseconds. `performance.now()`
  // is the avatar's frame clock and is not comparable with `started_ms`.
  const nowMs = Date.now();
  if (!inflight) {
    const due = dirty
      ? nowMs - lastFetch > REFRESH_DEBOUNCE_MS
      : nowMs - lastFetch > RECONCILE_MS;
    if (due) refresh();
  }

  for (const entry of cache.values()) {
    const entity = entry.entity;
    const ctx = entry.ctx;
    ctx.time = nowMs / 1000;
    const clips = (entity.rig && entity.rig.animations) || {};

    let name = null;
    let started = 0;
    let speed = 1;
    if (entity.anim && typeof entity.anim.clip === "string" && entity.anim.clip) {
      name = entity.anim.clip;
      started = num(entity.anim.started_ms, 0);
      speed = num(entity.anim.speed, 1) || 1;
    } else if (clips[entity.state]) {
      name = entity.state;
      started = num(entity.state_since_ms, 0);
    }

    const clip = name ? clips[name] : null;
    if (!clip) {
      ctx.clips = {};
      entry.clip = null;
      entry.clipT = 1;
      continue;
    }
    const duration = Math.max(1, num(clip.duration_ms, 1000));
    let t = ((nowMs - started) * speed) / duration;
    if (clip.loop) {
      t = ((t % 1) + 1) % 1;
    } else {
      t = Math.min(1, Math.max(0, t));
    }
    ctx.clips = sampleClip(clip, t);
    entry.clip = name;
    // `t` (0..1) decides whether a one-shot clip has finished. A dormant entity
    // is off the stage — except while its `vanish` is still playing.
    entry.clipT = clip.loop ? 0 : t;
  }
}

/* ---------------- attachment resolution ---------------- */

/** One slot per entity per frame, so a cycle is a `null`, not a hang. */
function slotMap() {
  const slots = new Map();
  for (const [id, entry] of cache) slots.set(id, { entry, root: undefined });
  return slots;
}

/**
 * Resolve an entity's root transform, recursively for nested attachments.
 *
 * `slot.root = null` before recursing is the cycle guard: a self- or mutual
 * attachment resolves to "invisible this frame", never a stack overflow. A
 * dangling socket is the same — a prop whose host bone vanished simply does not
 * paint, and nothing in the frame loop throws.
 */
function resolveRoot(id, slots, depth = 0) {
  const slot = slots.get(id);
  if (!slot) return null;
  if (slot.root !== undefined) return slot.root;
  slot.root = null;
  if (depth > MAX_ATTACH_DEPTH) return null;

  const entity = slot.entry.entity;
  const attach = entity && entity.attach;
  if (!attach || typeof attach.socket !== "string") return null;

  let base = null;
  if (attach.parent === "avatar") {
    const socket = (RT.sockets || []).find((entry) => entry && entry.id === attach.socket);
    if (!socket) return null;
    const world = socketTransformIn(RT.world, socket);
    if (!world) return null;
    base = { ...world, z: num(socket.z, 20) };
  } else if (attach.parent === "stage") {
    // The stage is the ground under the avatar: where a dropped prop lands.
    base = { x: avatar.x, y: avatar.y + 62, rot: 0, scale: 1, z: 20 };
  } else if (attach.parent === "world") {
    // Absolute stage coordinates. This is what lets a summoned chest stay where
    // it was put while the gremlin walks to it — `stage` would follow the avatar.
    const at = Array.isArray(attach.offset) ? attach.offset : null;
    if (!at) return null;
    base = { x: num(at[0]), y: num(at[1]), rot: 0, scale: 1, z: 20 };
  } else if (typeof attach.parent === "string" && attach.parent.startsWith("entity:")) {
    const parentId = attach.parent.slice("entity:".length);
    const parentSlot = slots.get(parentId);
    const parentRoot = resolveRoot(parentId, slots, depth + 1);
    if (!parentRoot || !parentSlot) return null;
    const parentCtx = parentSlot.entry.ctx;
    computeWorldInto(parentCtx, parentRoot);
    const parentRig = parentSlot.entry.entity.rig || {};
    const socket = (parentRig.sockets || []).find((entry) => entry && entry.id === attach.socket);
    if (!socket) return null;
    const world = socketTransformIn(parentCtx.world, socket);
    if (!world) return null;
    base = { ...world, z: parentRoot.z + num(socket.z, 0) };
  }
  if (!base) return null;

  // A nudge in the parent's frame (a prop standing off to one side of a socket,
  // or a `world` anchor's own point, which is already absolute and needs none).
  const nudge = attach.parent === "world" ? [0, 0] : (Array.isArray(attach.offset) ? attach.offset : [0, 0]);

  slot.root = {
    x: base.x + num(nudge[0], 0),
    y: base.y + num(nudge[1], 0),
    rot: base.rot + num(attach.rotation, 0),
    // `scale` is a fit multiplier: a rod put inside a pod is drawn at the pod's
    // scale, not at gremlin scale.
    scale: base.scale * num(attach.scale, 1),
    z: base.z,
    // `flip` mirrors the prop's **own** axes, so it multiplies into the parity
    // the socket already had (a prop on a flipped prop is mirrored with it) and
    // it leaves `rot`, `scale` and the nudge alone: the prop is turned around in
    // place, it does not move and it does not shrink.
    mirror: (base.mirror < 0 ? -1 : 1) * (attach.flip ? -1 : 1),
  };
  return slot.root;
}

/* ---------------- drawing ---------------- */

/**
 * Every part of every attached prop, as drawables the renderer can merge with
 * the avatar's own list.
 *
 * `z` is `socket.z + bone.z + part.z`, all in rig units, so a prop held in a paw
 * draws in front of the torso and one hung on the back draws behind it — the same
 * ordering rules as the creature itself.
 */
/** Throws in flight: `id@reveal_at` -> where they started. */
const throws = new Map();
/** How long a dud takes to leave the pod and land. */
const TOSS_MS = 460;

/**
 * Where an entity is on its way out of the pod — or `null` if it is still inside.
 *
 * The host holds each rummage dud back with `reveal_at` and the page does the
 * flying: the first frame after the reveal arms an arc from the pod's mouth to the
 * dud's own landing spot (its `world` offset), and the arc is spent in `TOSS_MS`.
 * Keyed by `id@reveal` because `chest:junk0` is a *recycled* id — it comes back on
 * the next rummage once the debris expires.
 */
function tossOffset(entity, id) {
  const reveal = Number(entity.reveal_at);
  if (!Number.isFinite(reveal) || reveal <= 0) return { dx: 0, dy: 0, spin: 0 };
  const now = Date.now();
  if (now < reveal) return null;

  const key = `${id}@${reveal}`;
  let flight = throws.get(key);
  if (!flight) {
    const pod = chestOrigin();
    const to = Array.isArray(entity.attach && entity.attach.offset)
      ? entity.attach.offset
      : [0, 0];
    if (!pod) return { dx: 0, dy: 0, spin: 0 };
    // Out of the pod's mouth, not its anchor: the well sits above the base.
    flight = { start: reveal, fromX: pod.x, fromY: pod.y - 34, toX: num(to[0]), toY: num(to[1]) };
    throws.set(key, flight);
  }

  const t = Math.min(1, (now - flight.start) / TOSS_MS);
  if (t >= 1) {
    throws.delete(key);
    return { dx: 0, dy: 0, spin: 0 };
  }
  const x = flight.fromX + (flight.toX - flight.fromX) * t;
  // A parabola: off the mouth, up, and down onto the spot. `sin(pi t)` is zero at
  // both ends, so it leaves and lands exactly where it should.
  const lift = Math.sin(Math.PI * t) * 70;
  const y = flight.fromY + (flight.toY - flight.fromY) * t - lift;
  const direction = id.length % 2 === 0 ? 1 : -1;
  return {
    dx: x - flight.toX,
    dy: y - flight.toY,
    // A little rigid spin to sell the flight — the prop's own `tumbling` clip does
    // the personality, so this stays small enough not to fight the boot's flip.
    spin: (1 - t) * 0.32 * direction,
  };
}

/** Drop any throws whose entity is gone (expired mid-air, tidied, removed). */
function pruneThrows(liveIds) {
  for (const key of throws.keys()) {
    if (!liveIds.has(key.split("@")[0])) throws.delete(key);
  }
}

export function entityDrawables() {
  const slots = slotMap();
  const out = [];
  for (const id of slots.keys()) {
    const root = resolveRoot(id, slots);
    if (!root) continue;
    const slot = slots.get(id);
    const entry = slot.entry;
    if (entry.entity.visible === false) continue;
    // `dormant` means "not on the stage": the pod is gone, but a vanish that is
    // still running gets to finish rather than blinking out.
    if (entry.entity.state === "dormant" && !(entry.clip && entry.clipT < 1)) continue;
    // Held back by the host, or mid-throw out of the pod.
    const toss = tossOffset(entry.entity, id);
    if (!toss) continue;
    const flying = toss.dx !== 0 || toss.dy !== 0 || toss.spin !== 0;
    const ctx = entry.ctx;
    computeWorldInto(ctx, root);
    const baseZ = num(root.z, 0);
    for (const bone of ctx.boneList) {
      if (!ctx.world[bone.id]) continue;
      for (const part of bone.parts) {
        if (part.visible === false) continue;
        const matrix = partWorldMatrixIn(ctx.world, bone, part);
        if (!matrix) continue;
        out.push({
          z: baseZ + num(bone.z, 0) + num(part.z, 0),
          bone,
          part,
          matrix: flying
            ? {
                ...matrix,
                x: matrix.x + toss.dx,
                y: matrix.y + toss.dy,
                rot: matrix.rot + toss.spin,
              }
            : matrix,
        });
      }
    }
  }
  stats.drawables = out.length;
  return out;
}

/** Debug view, for CDP probes and for agents that want the truth. */
export function entitySnapshot() {
  const slots = slotMap();
  const entities = [];
  for (const [id, slot] of slots) {
    const root = resolveRoot(id, slots);
    const entity = slot.entry.entity;
    entities.push({
      id,
      def: entity.def,
      state: entity.state,
      clip: slot.entry.clip || null,
      attach: entity.attach || null,
      visible: entity.visible !== false,
      bones: slot.entry.ctx.boneList.length,
      pose: slot.entry.ctx.clips || null,
      root: root
        ? { x: round(root.x, 1), y: round(root.y, 1), rot: round(root.rot, 3), scale: round(root.scale, 3), mirror: root.mirror < 0 ? -1 : 1 }
        : null,
    });
  }
  return {
    revision,
    count: cache.size,
    dirty,
    lastError,
    stats: { ...stats },
    entities,
  };
}
