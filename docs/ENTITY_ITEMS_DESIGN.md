# Procedural items, sockets and the chest — implementation plan

Status: **proposal, nothing implemented yet.** Companion to `MODEL_FORMAT.md`
(rig schema), `PLUGINS.md` (Wasm ABI) and `ARCHITECTURE.md` (lifecycle).

This document is grounded in the tree as it is today. Every claim about current
behaviour cites the symbol it came from; anything proposed is marked *proposed*.

---

## 1. What the codebase already gives us (audit)

### 1.1 Rig evaluation and rendering

| Seam | Where | What it does today |
|---|---|---|
| Bone defaults + parenting | `ui/js/rig.js::boneDefaults`, `rebuildOrder` | flattens `bones[]`, resolves a topological `RT.order`, builds `children` |
| World transforms | `rig.js::computeWorld` | `RT.world[id] = {x, y, rot, scale}`, root parent is `{x: avatar.x, y: avatar.y, rot: 0, scale: 1}` |
| Rotation stack | `rig.js::localRotation` | bind `rotation` + `anim` channels (`animOffset`) + clip delta (`animationOffset`) + `/pose` override, clamped by `limits` |
| Part matrix | `rig.js::partWorldMatrix` | **the single source of truth** for renderer, editor selection box and hit test |
| Paint order | `rig.js::sortedDrawables` | sorts by `bone.z` then `part.z` |
| Drawing | `ui/js/renderer.js::drawPart` | image (from `RT.images`) **or** `shape` ∈ `capsule`/`ellipse`/`circle`/`face`; `part.visible === false` skips |
| Clips | `ui/js/animator.js` → `animation.offsets` | keyframe sampler writes per-bone deltas; `rig.js` only reads them |
| Procedural pose | `rig.js::rummageOffset` | scavenging layers on top of `anim` channels |
| Load | `src-tauri/src/models.rs::load_model` (:552) | reads `rig.json` override else `model.json`, validates, inlines images into a data-URL map |
| Validation | `models.rs::validate_rig` (:347) | checks bones/parts/animations; **ignores unknown root keys** (it is a `serde_json::Value` walk, not a `deny_unknown_fields` struct) |
| Editor round-trip | `ui/js/rig.js::serializeRig` | emits only `format/name/label/notes/animations/bones` — **any new root key is dropped on save** |

Three consequences that shape the design:

1. A "mini-rig" can literally be the same bone/part JSON, so the renderer needs
   no new code path — only a second root origin.
2. Sockets must **not** live only inside `model.json`: the editor would silently
   delete them the first time someone drags a part. A sidecar (`sockets.json`,
   beside the existing `rig.json`) is the safe home; `model.json` may also carry
   them for generated rigs, which the editor never rewrites.
3. Generated rigs have no `hand` bone — `root, pelvis, torso, neck, head, tail,
   tail2, tail3, arm_r, arm2_r, arm3_r, arm_l, …` (sampled from
   `models/buddy-alien-champion-feral-14ac15fd/model.json`). Socket resolution
   therefore has to understand *chains*, not fixed ids. The shipped default,
   `xenomorph-7`, is worse: thirteen bones, four tentacles, **no arms and no
   head**, so a "find the bone called hand" rule would leave the default model
   with no attachment points at all.

### 1.2 Plugin host and ABI

| Seam | Where | Notes |
|---|---|---|
| Host imports | `src-tauri/src/plugins/wasm_host.rs::build_linker` (:650) | exactly **9**: `host_emit_event`, `host_notify`, `host_now_ms`, `host_ai_attached`, `host_state_save`, `host_log`, `host_model_write`, `host_model_activate`, `host_animate` |
| Guest exports | `plugins/survival/src/lib.rs` | 11: `alloc/dealloc`, `plugin_init/tick/status/routes/http`, `plugin_config{,_validate,_apply,_warnings}` |
| Reply packing | `write_host` | `(ptr as i64) << 32 | len`, 0 = no result |
| Limits | `wasm_host.rs` | `FUEL_PER_CALL` 50 M, `MAX_MESSAGE_BYTES` 4 MiB, state file 1 MiB, 1 s tick, `MAX_TICK_DT` 900 s |
| Capability trait | `src-tauri/src/plugins/mod.rs::PluginHost` | `emit / notify / ai_attached / rules_path / now_ms / write_model / activate_model / animate` |
| Real host | `src-tauri/src/lib.rs::AppHost` (:2170) | `emit` → `ctx.emit_event` (SSE bus, `{seq,type,ts_ms,payload}`) **and** `app.emit` (page) |
| Registry | `plugins/mod.rs` | one ticker thread, `Mutex` per registry, parking after 3 consecutive tick errors |
| Routes | `src-tauri/src/api.rs::router` (:360) | **41 core routes**, built once; plugin routes merged at :415; `PluginRoute` from `plugins/survival/src/routes.rs::ROUTES` (21 entries) |
| Guest route handling | `plugin_http` → `routes::handle` | one JSON in / JSON out hop, no streaming |

A guest has **no network, no filesystem, no clock** — so every capability it
needs must be an import. Adding `host_entities` is the whole cost of this
feature on the sandbox side.

### 1.3 Activities today (what "equipping a rod" must hook into)

* **Fishing** (`plugins/survival/src/fishing.rs`): pure, clock-free four-phase
  state machine `Idle → Waiting → Biting → Reeling → Idle`; `FishingConfig`
  carries `require_rod` / `bait_required`; loot is rolled in the guest.
* **Scavenging** (`plugins/survival/src/scavenge.rs`): guest owns duration, loot
  and comment cadence; the page owns motion (`ui/js/scavenge.js` +
  `main.js::beginScavengeWalk/stepScavenge/reconcileScavenge`).
* **Inventory** (`plugins/survival/src/inventory.rs`): `ItemDefinition { id,
  label, icon: &'static str /* one emoji */, kind, stack_max, blurb, food,
  rarity }`. Items are data + an emoji — there is no visual object anywhere.
* **Events** already emitted: `fishing-*`, `scavenging-*`, consumed by the page
  via Tauri `listen` and by the watcher via SSE.

So today a "fishing rod" is a row in a bag and a `require_rod` boolean. Nothing
exists between "inventory item" and "thing drawn in the world" — that gap is the
feature.

### 1.4 CI / docs gates that will bite

* `tools/verify_docs.py` requires **every** core `.route()` and every guest
  `ROUTES` entry to appear in `API.md` **and** `ARCHITECTURE.md`, and rejects
  documents that mention routes that do not exist.
* Every frame `type` routed by the watcher must appear in `ARCHITECTURE.md`.
* `watcher_rules.json` unknown top-level keys are rejected; new keys need a
  `known` set entry + `CONFIGURATION.md`.
* `event_name()` in `api.rs` (:660) maps a frame `type` to a `&'static str` for
  the SSE `event:` field; unmapped types fall back to `message` (readable from
  the JSON body, but `EventSource.addEventListener("entity-updated")` will not
  fire).
* CI = `cargo test --lib --bins` + each plugin crate's `cargo test --lib` +
  `verify_docs.py`. There is **no JavaScript unit-test harness**; `ui/js` is
  verified by the `chat_and_cursor_contract` integration test (which greps the
  source for pinned constants) and by live CDP probes.

---

## 2. Design

### 2.1 Three layers, one schema

```
  PropDef   (static, Rust, testable)      →  mini-rig (bones/parts/animations/sockets)
  Entity    (dynamic, host-owned, saved)  →  def + seed + params + state + attachment
  Socket    (per rig, resolved host-side) →  named semantic anchor on a bone
```

**PropDef** (`src-tauri/src/entities/defs.rs`, *proposed*)

```rust
pub struct PropDef {
    pub id: &'static str,              // "fishing_rod", "loot_bag", "chest", "lantern"
    pub label: &'static str,
    pub category: PropCategory,        // Tool | Container | Apparel | Furniture
    pub builder: BuilderId,            // which generator function
    pub tags: &'static [&'static str], // matched against a socket's `accepts`
    pub states: &'static [&'static str],
    pub default_state: &'static str,
    pub animations: &'static [&'static str],
    pub own_sockets: &'static [&'static str], // e.g. helmet -> ["lamp"]
}
```

A unit test asserts: ids unique, `default_state ∈ states`, every `animations`
entry exists in every seed's generated rig, and each generated rig passes
`models::validate_rig` (so a prop can never install a rig the app would reject).

**Entity** (`src-tauri/src/entities/store.rs`, *proposed*)

```rust
pub struct Entity {
    pub id: String,                    // "ent-1a2b3c4d" or a stable "survival:rod"
    pub def: String,
    pub seed: u64,
    pub params: serde_json::Value,     // variant overrides over seed defaults
    pub traits: BTreeMap<String, f64>, // gameplay numbers, same seed as the look
    pub state: String,
    pub state_since_ms: u64,
    pub anim: Option<EntityAnim>,      // { clip, started_ms, speed }
    pub attach: Option<Attachment>,    // { parent, socket, flip }
    pub visible: bool,
    pub owner: Option<String>,         // plugin id, for ownership + cleanup
}

pub enum AttachParent { Avatar, Stage, Entity(String) }
```

Store: `Mutex<EntityStore>` behind a `OnceLock`, **never** touching the plugin
registry mutex. Persistence: `models/.entities.json` (gitignored like
`.state.json` / `.model-index.json`), `STATE_VERSION`, `MAX_ENTITIES` (e.g. 64),
size cap. Caps are refusals, not truncations.

**Socket** (`src-tauri/src/sockets.rs`, *proposed*)

```json
{
  "id": "hand_primary", "bone": "arm3_r",
  "pos": [4, -2], "rotation": 0.35, "scale": 1.0,
  "z": 420, "accepts": ["rod", "tool", "bag"],
  "mirror": "hand_secondary", "label": "Right claw"
}
```

Resolution order, host-side, at `load_model` time:

1. `<model>/sockets.json` sidecar (survives rig overrides and editor saves).
2. a `sockets` array inside `model.json` / `rig.json` (generated rigs write it;
   `validate_rig` already tolerates the key).
3. heuristics from bone ids — distal chain member → `hand_primary` /
   `hand_secondary`; `/head/i` → `head_helmet`; `neck` → `neck_collar`;
   `torso`/`chest` → `chest_back` / `chest_front`; `root` → `back`;
   plus two virtual sockets: `stage_ground` (at the avatar's feet) and
   `perch_surface` (the `POST /perch` rect, when perched).

The resolved list is added to `LoadedModel` as a new `sockets` field, so it
reaches the page with `/model` and the `model-changed` event and the page never
guesses. `GET /sockets` exposes the same table for agents and tests.

**Socket world transform** reuses `partWorldMatrix`'s maths verbatim (a socket
is a virtual part on its bone), which keeps one transform formula in the project:

```
socket.world = parent.world ∘ translate(pos) ∘ rotate(rotation) ∘ scale(scale * parent.scale)
```

An attached entity's root parent **is** that transform; its own bones then
evaluate exactly as `computeWorld()` does today with the avatar as root. Nested
attachments (helmet → `lamp` socket → lantern) resolve recursively with a depth
cap of 4 and a visited-set cycle guard. A dangling socket must degrade to
"unattached, last known position, logged once" — never throw, because an
exception in `frame()` freezes the avatar (`main.js` wraps it in try/catch and
counts `frameErrors`).

### 2.2 Procedural generation

`fn build(def, seed, params) -> Value` — deterministic, pure, unit-testable,
with its own small PRNG (the host cannot use `plugins/metamorphosis/src/rng.rs`;
copying the 30-line generator is cheaper than a shared crate).

The metamorphosis lesson applies unchanged: **vary shape first, colour last.**
Each prop has a `Plan` of structural axes and a palette:

| prop | structural axes | palette / wear | states |
|---|---|---|---|
| `fishing_rod` | length, segment count, grip style, reel present, charm, line length | wood/bamboo/carbon, wrap colour, tarnish | `stowed`, `equipped`, `cast`, `waiting`, `biting`, `reeling`, `broken` |
| `loot_bag` | body shape, flap style, strap, patch count, bulge (from fullness) | burlap/leather/hide, patch colours | `stowed`, `carried`, `full`, `torn` |
| `lantern` | frame style, glass shape, handle arc, cage bars | brass/iron/copper, flame tint | `unlit`, `lit`, `swinging`, `broken` |
| `pickaxe` | haft length, head style, head span, wear notches | iron/steel/mythril, haft wood | `stowed`, `equipped`, `swing`, `chipped` |
| `mining_helmet` | dome shape, brim, lamp mount, rivets | leather/steel, lamp glass | `stowed`, `worn`, `lit` |
| `chest` | size class, band count, corner brackets, lid curve, lock style, slot count (4/6/8) | wood species, iron/brass, weathering | `closed`, `opening`, `open`, `closing`, `locked` |

Number-of-axes discipline: a unit test generates 100 seeds per def and asserts
≥ 90 distinct **shape tuples** (structural axes only, palette excluded) — that is
the test the first metamorphosis generator lacked.

Visual vocabulary: everything above is expressible with the two existing
primitives — `capsule` (hafts, straps, bands, rods) and `ellipse` (bodies,
glass, heads, latches). `color` is an arbitrary CSS string, so `rgba(...)` gives
lantern glow, bag shadow and chest weathering with no schema change. The chest
wants one addition: **`shape: "rect"`** (3 lines in `renderer.js::drawShape`,
one case in `rig.js::partLocalRect`, documented in `MODEL_FORMAT.md` §Parts).

Example — a generated rod (abridged; anchors only, numbers are illustrative):

```json
{ "bones": [
    { "id": "root",   "parent": null,   "pos": [0, 0] },
    { "id": "grip",   "parent": "root", "pos": [0, 0],   "rotation": 0.35,
      "parts": [{ "id": "wrap", "shape": "capsule", "size": [22, 9],
                  "offset": [11, 0], "pivot": [0, 0.5], "color": "#6b4a2f" }] },
    { "id": "shaft1", "parent": "grip", "pos": [22, 0],  "rotation": -0.1,
      "parts": [{ "id": "s1", "shape": "capsule", "size": [46, 4], "color": "#c9a227" }] },
    { "id": "shaft2", "parent": "shaft1", "pos": [46, 0], "parts": [ … ] },
    { "id": "tip",    "parent": "shaft2", "pos": [44, 0], "parts": [ … ] },
    { "id": "line",   "parent": "tip",   "pos": [0, 0],  "rotation": 1.4, "parts": [ … ] },
    { "id": "bobber", "parent": "line",  "pos": [0, 34], "parts": [ … ] } ],
  "animations": {
    "cast":    { "duration_ms": 520, "loop": false, "keyframes": [ … ] },
    "waiting": { "duration_ms": 2400, "loop": true,  "keyframes": [ … ] },
    "reeling": { "duration_ms": 900,  "loop": true,  "keyframes": [ … ] } },
  "sockets": [{ "id": "grip_point", "bone": "grip", "pos": [0, 0] }] }
```

Note the state → clip mapping: the page does not know what a rod is. It asks the
entity for `state`, plays the clip of the same name if the mini-rig has one, and
draws. That is what lets a *future* plugin ship a trident with zero page changes.

### 2.3 The chest

A `Container` prop with its own rig:

* bones: `root` → `base`, `lid` (hinge at the back edge), `latch`, `band_*`,
  `foot_*`, `innards` (an inner shadow ellipse).
* sockets: `chest_lid` (on `lid`, high z), `chest_top`, `chest_side_l/r`,
  `chest_front`, and `slot_0..slot_n` on `base` with a z **between** the body
  and the lid so items read as inside; `lid` has the highest z so closing hides
  them naturally.
* states and clips:

| state | clip | meaning |
|---|---|---|
| `appearing` | `appear` (300 ms, non-loop) | summon: rise + scale from 0, dust puff, no interactions accepted |
| `closed` | — | waiting; the summon choreography ends here if nobody walks up |
| `opening` | `open` (350 ms, non-loop) | lid rotates on its hinge bone |
| `open` | `idle_open` (2.4 s, loop) | faint lid breath + inner shadow |
| `closing` | `close` (300 ms, non-loop) | lid returns |
| `dismissing` | `vanish` (260 ms, non-loop) | sink + fade (scale to 0) |
| `dormant` | — | invisible, **kept in the store** so its contents survive |

**Decision — the chest is a summoned stage prop.** It is not permanently on
screen: it appears when something needs it (the user, an agent, or a guest that
wants to stow something), the gremlin walks to it, opens the lid, and it goes
away again. The states above make that a small state machine rather than a
special case.

Summon anchor: `avatar` + offset (`+95 px` on the walking axis, on the ground
line), flipped to the other side when it would fall outside the current
monitor's work area; a persisted `chest_anchor` is the fallback when even the
flipped spot is off-screen. The anchor is resolved in the host (which already
owns the avatar position and the monitor list) and published in the
`chest-summoned` payload, so the page never has to guess where to walk.

**Dismissal never orphans contents.** `POST /chest/dismiss` is refused with a
409 naming the occupied slots while anything is inside; once empty (or with
`force:true`, which moves every entity to `stage_ground` first) the chest goes
`closing → dismissing → dormant`. `dormant` keeps the entity and its entity id,
so a later summon reuses the same chest — same seed, same wood, same dents.

Auto-dismiss: `dismiss_after_ms` in the chest config (default 20 s after the
last interaction: a put, a take, or the walk-up). The timer resets on any
interaction and never fires mid-`put`.

Contents are child entities with `attach.parent = chest.id`. A **transactional**
surface keeps bag counts and entity instances from drifting apart:

```
GET  /chest                    { entity, state, capacity, slots: [{index, socket, entity|null}] }
POST /chest/summon             { x?, y?, by? } -> spawns/appears, emits `chest-summoned`
POST /chest/open | /chest/close
POST /chest/dismiss            { force? }
POST /chest/put { entity_id | def, slot? }   chest_interior <- a hand/stage entity
POST /chest/take { slot, attach? }           chest_interior -> hand / stage
```

`put`/`take` mutate the entity and (when the entity carries an `item_id`) the
guest's bag in the same handler, mirroring the existing `emit_inventory`
discipline: *one mutation path per economy action*.

### 2.4 Authority split (the one real trade-off)

The survival guest already owns the economy (counts, coins, hunger). Entities
are a host concept. Two options:

**(A) Guest = economy, host = entities — CHOSEN.** The bag holds counts;
the chest holds entity instances; an entity may carry `item_id` when it
corresponds to a bag stack (`{"def":"fishing_rod","item_id":"fishing_rod"}`), and
decoration entities carry none. `put`/`take` are the only places both stores
change, in one handler. Pros: one authority per fact, the chest renders with no
brain, plugins that know nothing about survival can spawn props. Cons: two
stores to keep honest; a crash between the two writes could desync (mitigated by
writing the entity store first, then `host_state_save`, and reconciling on init).

**(B) Entities are pure presentation.** The guest's chest contents are the truth
and the host mirrors them. Pros: single store. Cons: the host cannot render the
chest before the guest has ever ticked, a second plugin cannot put anything in
the chest without the survival guest knowing, and the "items are first-class
entities" requirement is only half met. *Rejected for those reasons; kept here
so the trade-off is on record.*

### 2.5 ABI: one new import

```text
host_entities(req_ptr, req_len, out_ptr, out_cap) -> i32    // bytes written, <0 = error
```

The reply goes **into a guest-owned buffer** because ids are strings and the
existing packed-`(ptr<<32)|len` return would require the host to allocate inside
guest memory (the ABI hands buffers the other way). `out_cap` bounds the copy;
a too-small buffer returns `-needed` so a guest can retry with a bigger one.
Ownership rules are therefore identical to `host_state_save`.

Request ops (one dispatch table, validated by a pure function):

| `op` | request | reply |
|---|---|---|
| `spawn` | `{def, id?, seed?, params?, traits?, state?, attach?}` | `{id, seed}` |
| `get` | `{id}` | entity (no rig — guests do not render) |
| `list` | `{def?, owner?, attached?}` | `{entities: [...]}` |
| `update` | `{id, state?, anim?, attach?, params?, visible?}` | `{ok:true}` |
| `detach` | `{id}` | `{ok:true}` |
| `remove` | `{id}` | `{ok:true}` |
| `chest` | `{}` | `{entity, state, capacity, slots}` |

Isolation rules, stated as invariants:

* **Ownership**: `spawn` records `owner = caller.data().id`; `update`/`detach`/
  `remove` on an entity owned by another plugin returns
  `{ok:false,error:"not owned"}`. Reading is unrestricted.
* **No lock nesting**: the entity store is mutated under its own lock, cloned,
  the lock dropped, *then* `host.emit` runs. The ticker holds the registry lock
  while a guest calls `host_entities`; the store never calls into `plugins::*`,
  so the two mutexes never nest in the opposite order.
* **Fuel/size unchanged**: one import call costs the guest normal fuel; the host
  caps the request at `MAX_MESSAGE_BYTES` and `out_cap` at the same.
* **No re-entrant registry access**: the same reason `GET /survival` stopped
  embedding `plugins` — the ticker holds that lock.

### 2.6 HTTP surface (core, `api.rs`)

```
GET  /entities                  render payload (id, def, seed, params, traits, state,
                                anim, attach, rig, images) + revision
POST /entities/spawn            { def, seed?, params?, state?, attach?, owner? }
POST /entities/update           { id, state?, anim?, attach?, params?, visible? }
POST /entities/remove           { id }
GET  /sockets                   ?model=<slug>, defaults to the loaded rig
GET  /props                     catalog: defs, categories, states, sockets, param ranges
GET  /chest
POST /chest/summon | /chest/open | /chest/close | /chest/dismiss
POST /chest/put | /chest/take
```

Twelve routes. Each must appear in `API.md` **and** `ARCHITECTURE.md` or
`verify_docs.py` fails; `GET /props` pays for itself by making the system
discoverable to agents and to the future mining plugin's config UI.

`GET /entities` returns rigs, so keep mini-rigs small (a rod is ~8 bones / ~10
parts). Events carry **ids only** and the page refetches, coalesced — the same
pattern `reconcileScavenge()` already uses.

### 2.7 Events

| type | payload | consumer |
|---|---|---|
| `entity-spawned` | `{id, def}` | page (refetch), agents |
| `entity-updated` | `{id, state, attach}` | page (refetch), agents |
| `entity-removed` | `{id}` | page |
| `chest-summoned` | `{id, x, y, arrive_ms}` | page: walk to it, then `POST /chest/open` |
| `chest` | `{entity, state, open, count}` | card UI, mining plugin later |
| `chest-dismissed` | `{id}` | page: drop any walk toward it |

Add matching arms to `event_name()` so the SSE `event:` field is filterable, and
document every type in `ARCHITECTURE.md` (machine-checked for watcher-routed
types). Page-side: one `entities-changed` dirty flag, refetch at most every
100 ms, plus a 2 s reconcile — a dropped frame can never leave a stale rod.

### 2.8 Page changes

1. `ui/js/rig.js` — factor the transform maths out of module globals:
   `makeRigCtx(rig)` → `{bones, order, world}` and
   `computeWorldInto(ctx, origin)`. `computeWorld()` becomes a three-line wrapper
   over `RT`/`avatar`, so editor, hit-test and region code are untouched.
2. `ui/js/entities.js` (new) — entity cache, `resolveAttachment(entity)` →
   origin transform (recursive, capped), `entityDrawables(entity)` reusing
   `partWorldMatrix`, state→clip selection.
3. `ui/js/renderer.js::render` — merge avatar and entity drawables into one
   z-sorted list (entries `{ctx, bone, part, z}` where an entity's z is its
   socket `z` + local z). Behind-sockets (`back`, `head_helmet`) then draw behind
   the body and `hand_primary` in front, for free.
4. `ui/js/main.js` — step entity clip time alongside `avatar.time`; SSE listeners;
   `entity` fields in `reportState`'s debug snapshot for probes.
5. **Activity arbitration** (`ui/js/state.js` + `main.js`) — the chest walk needs
   the same protection the scavenging walk learned the hard way (an idle fidget
   must not cancel an activity, and only a *user* gesture bumps `manualMoves`).
   Add one explicit field, `avatar.activity ∈ {none, scavenge, chest}`, and gate
   the idle wander on `activity === "none"` instead of on a growing set of
   booleans. Both walks take the lock, both release it on completion, and a
   third activity is then impossible to strand.
6. `ui/js/rig.js::wireRegions` — a chest card (if any) **must** be pushed in
   `wireRegions()`, or it looks live while every click falls through the
   click-through desktop (the existing comment at `rig.js:546-553`).

---

## 3. Activity wiring

### The chest choreography (summon → walk → open → dismiss)

Nothing about this is special-cased in the renderer; it is the scavenging
pattern (guest/host owns the *state*, the page owns the *motion*) applied to a
prop:

```
caller (user / agent / guest)
  POST /chest/summon
    host: anchor = avatar + offset (flipped/clamped to the monitor), spawn or
          wake the chest entity, state `appearing`, emit `chest-summoned{id,x,y}`
  page: listen("chest-summoned") -> take `activity="chest"`, walk to (x, y)
        (same startWalk + hold machinery as scavenge; hold covers the whole walk)
  page: on arrival -> POST /chest/open
    host: state `opening` -> `open`, emit `chest`, arm `dismiss_after_ms`
  page: play clip `open` from the entity state (no rod-specific code anywhere)
  ... put / take interactions ...
  host: idle timer -> /chest/dismiss (refused while occupied, 409 names slots)
    state `closing` -> `dismissing` -> `dormant`, emit `chest-dismissed`
```

Two failure paths that must be designed in, not discovered:

* the avatar is dragged mid-walk → the walk stands down (a user gesture outranks
  it), the chest stays `closed` for the remainder of its idle timer and then
  dismisses. It never sits there forever, and it never teleports the avatar back.
* the page misses `chest-summoned` → `GET /chest` reports `state:"closed"`; the
  page reconciles every 2 s (like `reconcileScavenge()`) and, if it is idle,
  walks over then. Idempotent: `open` on an already-`open` chest is a 200 with
  `changed:false`.

A guest that wants to stow loot (bag full after a scavenging run) calls
`host_entities{op:"chest"}` to check the state, `{op:"spawn"}` the item into a
slot if the chest is open, and otherwise emits its own `chest` request — it does
not walk anywhere, because a sandbox has no legs.

### Fishing (guest changes: `fishing.rs` + `plugin.rs`)

```
cast    -> host_entities{op:spawn, id:"survival:rod", def:"fishing_rod",
                         state:"cast", attach:{parent:"avatar",socket:"hand_primary"}}
        (idempotent: if it already exists, `update` instead)
waiting -> update{state:"waiting"}      (clip `waiting` loops: tip sway)
biting  -> update{state:"biting"}       (clip `biting`: tip jerk)
hook    -> update{state:"reeling"}      (clip `reeling`: bend)
cancel  -> update{state:"stowed", attach:{parent:"entity:survival:chest", socket:"slot_3"}}
```

`require_rod: false` still spawns a rod — a `params:{crude:true}` "found stick"
variant — because the config governs the *inventory check*, not whether the
avatar visibly holds something.

### Scavenging

`start` → spawn/attach `loot_bag` at `hand_secondary`, `state:"carried"`;
completion → `state:"full"` + a 2 s `settle` clip, then `stowed` into a free
chest slot (or the stage when the chest is full).

### Mining (future plugin, no core change)

```
spawn lantern  -> attach {parent:"entity:mining:helmet", socket:"lamp"}
spawn helmet   -> attach {parent:"avatar", socket:"head_helmet"}
spawn pickaxe  -> attach {parent:"avatar", socket:"hand_primary"}
states: lantern lit/unlit, pickaxe swing (clip), helmet worn/lit
```

The **only** core prerequisites are nested sockets (§2.1) and per-entity
state→clip animation (§2.7) — both in Phase 1–2, which is why the mining
roadmap costs a plugin and a doc section, not a redesign.

### No brain attached

Entities are host-owned and event-driven, so `host_ai_attached() == false`
changes nothing: the rod still appears, animates and stows, and the guest's own
`host_notify` line is the voice. Proof recipe: `POST /watcher {"enabled":false}`,
no SSE reader, cast, then `GET /entities` shows `state:"cast"`.

---

## 4. Phased build plan

Every phase ends with the same gate:

```bash
cd src-tauri && cargo build && cargo test --lib --bins
cargo test --test chat_and_cursor_contract      # app shut down first
cd ../plugins/survival && cargo test --lib
cd ../.. && python tools/verify_docs.py
```

| Phase | Scope | Tests that must exist before it is "done" |
|---|---|---|
| **0 — seams, no behaviour change** ✅ | `rig.js` rig-ctx extraction + `computeWorldInto`; `sockets.rs` + `LoadedModel.sockets` + `GET /sockets`; `sockets.json` sidecar loading | socket resolution on `xenomorph-7` (tentacles, no arms), a generated-shape rig (heuristic chain), a legs-named-like-arms stub; a sweep over every shipped `models/*/model.json` asserting `hand_primary` resolves; A/B `__avatarDebug.snapshot()` + screenshot over CDP proves the extraction changed nothing |
| **1 — entity store + one prop** ✅ | `entities/{defs,gen,rng,store}.rs`; persistence, caps, ownership; `fishing_rod` + `loot_bag` builders; `GET/POST /entities*`, `GET /props`; `entities.js` + renderer merge | store unit tests (cap refusal, owner scoping, save/restore round-trip, dangling attachment); determinism test (same seed ⇒ identical rig); variety test ≥60 distinct shape tuples per 100 seeds; `validate_rig` on every generated prop; live: spawn a rod on `hand_primary`, screenshot, drag the avatar and watch it follow |
| **2 — ABI + survival rod** ✅ | `host_entities` in `build_linker`; guest `host.rs` wrapper + thread-local recorder; guest `props.rs`; fishing state→rod wiring | guest tests assert the exact call sequence (one spawn, then updates in phase order, no spawn on cancel); live: cast with watcher off, screenshots at waiting/biting/reeling; `GET /entities` proves state transitions |
| **3 — the chest** ✅ | `chest` def, `shape:"rect"`, slots, `appear/open/idle_open/close/vanish` clips; `summon/open/close/dismiss/put/take` routes; summon anchor (flip + clamp to monitor); `dismiss_after_ms`; the walk-to choreography + `avatar.activity` lock; **seeded decorative loot** (5–8 items, no economy); chest card (with `wireRegions` entry) | capacity/slot allocation; put↔take leaves bag counts and entity list consistent; interior items draw under the lid and vanish when closed; dismiss refuses while occupied (409 names slots) and `force:true` parking everything on `stage_ground`; restart mid-`opening` restores `closed`; a drag mid-walk stands the walk down without teleporting; a missed `chest-summoned` recovers within ~2 s; the same chest re-summons with the same seed and its contents |
| **4 — scavenge bag + loot→entities** | `loot_bag` def, start/stow wiring, loot that yields a tool creates a seeded entity | 30 s live run with screenshots at dig + carry; a looted tool survives restart with the same seed/params; bag and chest never both claim one item |
| **5 — mining-ready hardening** | nested sockets + depth/cycle guard; `accepts` validation; metamorphosis generator writes explicit `sockets`; `head_helmet`/`hand_secondary` verified on generated rigs | lantern-on-helmet-on-avatar resolves; a self-referential attach is refused at spawn with a 400; resolution stays under ~1 ms for a 3-deep chain |
| **6 — docs + MCP** | `API.md`, `ARCHITECTURE.md`, `PLUGINS.md` (ABI table + import), `MODEL_FORMAT.md` (sockets, `rect`), `CONFIGURATION.md` if a key is added; MCP tools + the `expected 14` route assertion | `verify_docs.py` green; MCP tool list updated deliberately (the count assertion is a doc gate) |

Deliberate sequencing choice: **chest after the rod.** The rod alone proves
generation, sockets, rendering and the ABI end to end with one prop; the chest
then reuses all four and only adds containers. Doing the chest first would mean
debugging a container and a renderer refactor at the same time.

---

## 5. Risks and mitigations

| Risk | Mitigation |
|---|---|
| `rig.js` extraction regresses the editor/hit test | `computeWorld()` stays byte-identical in behaviour; A/B snapshot + screenshot over CDP; integration suite pins constants |
| An entity path throws inside `render()` and freezes the avatar | every entity call inside the existing try/catch; dangling socket degrades to "unattached + log once"; `frameErrors` asserted 0 in the live probe |
| Duplicate authority (bag count vs chest entity) | one mutation path per action (`put`/`take` only); entity carries `item_id`; reconcile on init |
| Unbounded growth / runaway guest | `MAX_ENTITIES`, `MAX_MESSAGE_BYTES`, 1 MiB state file, ownership scoping, fuel unchanged |
| Editor silently deletes sockets | `sockets.json` sidecar is authoritative; `model.json` sockets are generator-only output |
| Nested attachment cycle | depth cap 4 + visited set; refused at spawn with a readable 400 |
| SSE clients cannot filter new events | add `event_name()` arms and document every type in `ARCHITECTURE.md` |
| Docs drift | every phase's routes/events land in the same commit as the code, then `verify_docs.py` |

---

## 6. Decisions taken (2026-11-23)

1. **The chest is a summoned stage prop.** It appears on demand, the gremlin
   walks to it and opens the lid, and it dismisses itself (or on request). It is
   never permanently on screen. Implemented as the state machine in §2.3 plus
   the choreography in §3.
2. **Guest owns economy, host owns entities** (§2.4 A). `put`/`take` are the
   only places both change, in one handler.
3. **Phase 3 ships seeded decorative loot**; Phase 4 replaces decoration with
   real, earned loot (an entity is only "real" when it carries an `item_id`
   whose count lives in the guest's bag).

Still to be settled during implementation, not before it:

* whether the chest card is a full card or a couple of buttons on the survival
  card (a card is more discoverable; buttons are cheaper and cannot be missed by
  the click-through regions);
* the exact summon offset per walking direction — proven by screenshot, not by
  arithmetic;
* whether `dismiss_after_ms` is a `survival` config key or a core `chest` block
  (it is core behaviour, but the only caller today is survival; a core block
  needs the `watcher_rules.json` known-key set + `CONFIGURATION.md` rows).

---

## 7. Phase 0 — landed

What shipped, and what the codebase taught us on the way.

| File | Change |
|---|---|
| `src-tauri/src/sockets.rs` (new) | `Socket`, `resolve()`, `load_sidecar()`, `find`, `canonical_ids`; 11 unit tests including a sweep over every shipped `models/*/model.json` |
| `src-tauri/src/models.rs` | `LoadedModel.sockets`, populated in `load_model` (sidecar read + resolve), logged |
| `src-tauri/src/lib.rs` | `pub mod sockets`; `model_event_payload` reports a `sockets` **count** |
| `src-tauri/src/api.rs` | `GET /sockets` (`?model=` is read-only) |
| `ui/js/state.js` | `RT.sockets` |
| `ui/js/rig.js` | `computeWorldInto(ctx, origin)` + `computeWorld()` as its wrapper; `localRotation`/`localScale`/`animationOffset` take an optional rig context; `socketWorld`, `socketList`, `socketById`; `applyModel` adopts `model.sockets` |
| `ui/js/main.js` | `__avatarDebug.sockets()` → every socket with its live world transform |
| `API.md`, `ARCHITECTURE.md`, `MODEL_FORMAT.md` | route table + reference section + "Sockets (attachment points)" |

The derivation had to grow one rule beyond the plan: **tentacles and leaves**.
`xenomorph-7` — the shipped default — has no arms and no head, so with only
"arm chain" matching it would have had zero attachment points. The pool now
scores arm > tentacle > wing/leaf, prefers a declared side, then chain depth,
then rest position, then id; a one-armed rig gets the next best candidate as its
secondary hand rather than a missing socket. The legs-named-`arm` hazard is
covered by a test, and the sweep test is what proves the vocabulary maps onto
*every* shipped rig rather than the one that was in front of us.

Two facts that differ from the skill's older notes (measured, not remembered):
`models/patchkit` now names its legs `leg_r`/`foot_r`, and it is **untracked**,
so it cannot be a test fixture; the tests build synthetic rigs in code and sweep
the shipped ones instead.

Deferred to Phase 1 by design: `computeWorldInto` still reads the avatar's life
signs (`animOffset`) and, unless `ctx.clips`/`ctx.pose` are supplied, the shared
pose/clip state. A prop entity will pass its own `clips` and `pose`; the
life-sign layer needs one more switch (`ctx.lifeSigns = false`) so a held rod
does not sway with the avatar's breathing — that is written down here so it is
not discovered as a mystery wobble later.

### Verification (what was actually run)

| Check | Result |
|---|---|
| `cargo test --lib --bins` | exit 0 — 201 lib + 12 mcp + 138 watcher tests |
| `cargo test --test chat_and_cursor_contract` | 2 passed (app shut down) |
| `python tools/verify_docs.py` | green, `/sockets` documented in both route tables |
| Live A/B, idle | `computeWorld()` vs a verbatim copy of the old loop: **maxDiff 0** over all bones × x/y/rot/scale |
| Live A/B, animated | same comparison over 6 states with walk + sway + rummage + `RT.pose` + clip offsets: **maxDiff 0** |
| `frameErrors` | 0 across both probes |
| Screenshot | avatar renders normally at (960, 540), panel intact |
| `GET /sockets` live | `xenomorph-7` → `tentacle_1_tip` / `tentacle_4_tip` (no arms, still two hands); `demo-blob` → `arm_r` / `arm_l_hand`; `nebulon-9` → `hand_r` / `hand_l`; `sir-clank` → `forearm_r` / `forearm_l` |
| `?model=` read-only | current model unchanged after four such queries |

### Found on the way (not this feature, not fixed)

`ui/js/main.js::step()`'s walk branch does not clamp `t = elapsed / duration`.
A negative `elapsed` (any code that moves `avatar.time` backwards) makes the
easing factor huge and throws the avatar hundreds of thousands of pixels away.
A CDP probe that reset `avatar.time` reproduced it at `(454436, 340558)` in half
a second. Fix is one line —
`const t = Math.min(1, Math.max(0, elapsed / duration))` — and it belongs in its
own change with its own probe, not smuggled into a "no behaviour change" phase.

---

## 8. Phase 1 — landed

Props are real: a `fishing_rod` generated from a seed, held in a generated
gremlin's paw, animating through `cast → waiting → biting → reeling`.

| File | What |
|---|---|
| `src-tauri/src/entities/rng.rs` | xorshift64* PRNG (stable across runs; `seed 0` is not a fixed point) |
| `src-tauri/src/entities/defs.rs` | `PropDef` + `PROPS` (`fishing_rod`, `loot_bag`), per-socket grip rotations, `catalogue()` for `GET /props` |
| `src-tauri/src/entities/gen.rs` | deterministic builders → model-format mini-rigs (bones/parts/clips/sockets) + `traits()` |
| `src-tauri/src/entities/store.rs` | `Entity`, `Attachment`, spawn/update/remove, ownership, caps, `models/.entities.json`, rig cache |
| `src-tauri/src/entities/mod.rs` | `roster()`, `commit()` — mutate, persist, return the revision |
| `src-tauri/src/api.rs` | `GET /entities`, `POST /entities/{spawn,update,remove}`, `GET /props`; `entity-*` frames on SSE + page |
| `ui/js/entities.js` | roster cache, state→clip, attachment resolution, drawables |
| `ui/js/rig.js` | `makeRigContext`, `partWorldMatrixIn`, `socketTransformIn`, `animOffset(bone, ctx)` with `lifeSigns` |
| `ui/js/animator.js` | `sampleClip(clip, t)` extracted (one interpolation implementation) |
| `ui/js/renderer.js`, `ui/js/main.js` | merged z pass; entity step + listeners + `__avatarDebug.entities()` |

Three things the implementation changed about the plan:

1. **Socket `z` units.** Phase 0 shipped bands of 40–420, which are meaningless
   next to a rig whose own `z` runs −8…31: every prop would have drawn on top of
   the head. Measured the generated rigs and moved the vocabulary to rig units
   (`back` 4 … `hand_primary` 42). A prop now interleaves with the body.
2. **Grip is part of the prop definition.** `attach.rotation` defaults from
   `PropDef::grips` (`hand_primary: -1.25`), so a rod is held across the body
   rather than lying along the forearm. Without it the rod pointed down-right at
   the arm's rest angle.
3. **A state that names a clip plays it.** No `anim` is required: `{"state":
   "biting"}` starts `biting`, because the page looks the state up in the
   entity's own `animations`. That is what keeps the page from knowing what a
   rod is.

Known limits, deliberately not fixed in this phase:

* `attach.flip` is stored and validated but not rendered (mirroring a prop for
  the off hand is Phase 5, with the nested-socket work).
* `stage` resolves to the ground under the avatar; the chest will own a real
  anchor in Phase 3.
* `avatarBounds()` (the click-through region) covers the avatar only, so a prop
  that sticks out is not clickable — cosmetic until props become draggable.
* Prop size is authored in rig units: it reads right on a generated gremlin
  (root scale 0.55) and small on `xenomorph-7` (root scale 1.0, ~700 px tall).
  Per-rig fitting is a later nicety, not a correctness bug.

### Verification (what was actually run)

| Check | Result |
|---|---|
| `cargo test --lib entities::` | 25 passed (rng, defs, gen, store) |
| variety test | ≥60 distinct **structural** tuples per 100 seeds, per def |
| `validate_rig` on props | every generated rod/bag is a rig the loader accepts |
| Live, generated gremlin `buddy-alien-fresh-prism-0b5590cd` | `POST /entities/spawn` → rod at `hand_primary` (11 bones, clips `cast/waiting/biting/reeling`), 16 drawables, `frameErrors` 0 |
| State sweep | `waiting → biting → reeling → stowed` each switched the clip; `stowed` → `clip: null` |
| Hot-swap rigs | switched to `buddy-beast-fresh-prism-f6e986b0` (`hand_primary@arm_r`) and `xenomorph-7` (`hand_primary@tentacle_1_tip`); the rod followed both with no reload, `frameErrors` 0 |
| Remove | `POST /entities/remove` → page `count: 0`, save file `{"version":1,"entities":[]}` |

---

## 9. Phase 2 — landed

The rod is no longer placed by hand: the survival guest spawns it, puts it in the
paw, walks it through the cast, and puts it away — over the ABI, with no HTTP call
from the plugin and no knowledge of rigs in the page.

| File | What |
|---|---|
| `src-tauri/src/entities/guest.rs` | the ABI: `spawn`/`update`/`remove`/`get`/`list` as JSON; ownership stamped from the caller id; **summaries, not rigs** |
| `src-tauri/src/plugins/wasm_host.rs` | `host_entities(req, req_len, out, out_cap) -> i32`, writing into the guest's buffer and emitting the `entity-*` frames after the store lock is gone |
| `plugins/survival/src/host.rs` | import + `entities(&Value) -> Option<Value>` over a 4 KiB buffer, and a native recorder that keeps a **fake host** so the plugin's tests see the world it built |
| `plugins/survival/src/props.rs` | `ensure_rod` / `hold(state)` / `put_away` / `discard_rod` — the only place that knows which prop survival spawns and where it grips it |
| `plugins/survival/src/plugin.rs` | `sync_rod()` — idempotent phase→state mapping, called from `advance_fishing`, `cast`, `hook`, `land`, `escape`; backs off 10 s if the host has no ABI and never spends a call when the phase has not changed |
| `src-tauri/src/sockets.rs` | **grip placement**: the hand socket is derived from the limb's artwork (the far end of the outermost part), not from the bone's joint |

### The grip bug (user-visible, now measured)

"the item is more in its elbow/shoulder" was exactly right, and the cause was
structural: a socket at the bone's origin is at the *start* of that bone. On a
three-segment arm that is the wrist; on `buddy-beast`, whose whole arm is one bone
with a 46-unit capsule, it is the **shoulder**. Generated creatures have no
dedicated `hand` bone at all.

`grip_point()` now reads the part geometry (`capsule` spans `[-h/2, w-h/2]`, an
ellipse is centred on its offset — the same rules the renderer uses) and anchors
half a thickness inside the outermost part:

| Rig | bone | old `pos` | new `pos` | reads as |
|---|---|---|---|---|
| `buddy-beast-…` (one-bone arm) | `arm_r` | `[0, 0]` — shoulder | `[31.03, 0]` — 80% out | paw |
| `buddy-alien-…` (two-bone arm) | `arm2_r` | `[0, 0]` — elbow | `[24.19, 0]` — the sucker pad | paw |
| `xenomorph-7` (tentacles) | `tentacle_1_tip` | `[0, 0]` | unchanged (no parts) | tip |

A sweep test asserts, for every shipped model, that a derived hand grip lands
**on** the limb's artwork and never past its end.

### Two bugs the ABI work found

1. **The reply carried the rig.** A spawn's reply included the generated
   mini-rig; at ~4 KiB it did not fit the guest's buffer, so `host_entities`
   returned `-4` *after* the spawn had already landed. The guest read that as a
   refusal, never reached `put_away`, and left the rod visible with no state
   transition. Replies are summaries now; `get` takes `"rig": true` when a caller
   really wants geometry. `props::accepted` logs the reply when it is refused,
   and the guest logs any negative status instead of failing silently.
2. **Stow ran before the phase flipped.** `land()` stowed at the top, while
   `fishing.phase()` was still `Reeling`, so a catch left the rod in `reeling`
   until the next tick. The stow now happens after `record()`/`finish()`, inside
   the same call that announces the result — a test reels on the sweet spot and
   asserts the rod is `stowed` and detached when the call returns.

### Verification (what was actually run)

| Check | Result |
|---|---|
| `cargo test --lib entities::` (host) | 36 passed (ownership, refusals, summarised replies, the reply-fits-a-small-buffer guard) |
| `cargo test --lib` (survival guest) | 218 passed, incl. 6 new: cast→paw, bite, hook→reeling immediately, catch→stowed immediately, no-rod-no-entity, and "no ABI call per tick" |
| Live, watcher **off**, generated `buddy-alien-mega-prism-…` | `stowed/invisible` at rest → `waiting/paw` on cast → `biting/paw` at the bite → `reeling/paw` on hook → `stowed/detached` on the catch; page `frameErrors` 0 |
| Live catch | `POST /fishing/reel` on the sweet spot → `caught: true, Silver trout`, rod stowed in the same call, `river_perch` in the bag |
| Restart | the rod comes back from `models/.entities.json` as `stowed`; the plugin re-spawns idempotently (`created: false`) |

Still open, deliberately: `attach.flip` is not rendered; the bag is not a prop yet
(it is the next activity, not this phase); the chest is Phase 3.

---

## 10. Phase 3 — landed: the pod

The brief was "more whimsical than a chest". The answer is a **seed-pod whose lid
is a flower**: a faceted polygon shell (5–6 sides, three facet styles, an optional
face, feet, a charm) with a rosette of petals hinged at the rim. Closed, the blades
meet in the middle and the silhouette reads as a die; open, each blade swings 180°
about its own bone and lies flat, and the lit well inside is where your things are.
The "d12" idea survives as the shell; the whimsy is that it is a creature you keep
things in — and it breathes: the appear/close clips twitch the blades with a little
seeded jitter, because a perfectly synchronised rosette reads as a machine part.

| File | What |
|---|---|
| `src-tauri/src/entities/gen.rs::chest` | the generator: shell, facets, well, face, feet, petals, four clips, three sockets |
| `src-tauri/src/entities/defs.rs::CHEST` | `dormant`/`closed`/`open`, clips `appear`/`open`/`close`/`vanish`, sockets `chest_interior`/`chest_lid`/`chest_side` |
| `src-tauri/src/entities/chest.rs` | the lifecycle: summon / open / close / dismiss / put / take, slots, refusals, 8 tests |
| `src-tauri/src/api.rs` | the seven routes, events emitted after the store lock |
| `ui/js/renderer.js` · `rig.js` | the **`poly` shape**: `points` in rig units, bbox cached in a `WeakMap` (never written back into the rig JSON) |
| `ui/js/entities.js` | `world` anchors, `attach.offset`, `attach.scale`, and one clock for every entity |

### Three primitives, and why each was necessary

1. **`poly` parts.** An ellipse stack can look like a potato; it cannot look like a
   polyhedron. `points` are geometry, `size` is the bbox so bounds/hit-tests/editor
   keep working, and `validate_rig` refuses a `poly` with fewer than three finite
   points — a silent empty shape is the worst failure mode for a generator.
2. **`world` anchors** (`attach.parent: "world"` + `offset`). `stage` follows the
   avatar; a summoned pod must not. This is what lets the gremlin walk to it later,
   and what makes `dismiss` → `summon` put it somewhere new.
3. **`attach.scale`.** Contents are drawn at 45% while inside, so a rod sits *in*
   the pod instead of through it.

### Bugs the build found (all real, all fixed)

| Symptom | Cause |
|---|---|
| every entity clip stuck on its first frame (petals would not splay, the rod never moved) | `stepEntities()` was handed the page's **relative** frame clock while clip start times are **epoch ms**; `t` was hugely negative and clamped to 0. Entities now use `Date.now()` for clip timing and `lastFetch` too — that one was also refetching every frame |
| the rosette read as a pinwheel | the petal hinge was `θ + π` instead of `θ + π/2`: rotating a bone by `a` sends local +y to `(-sin a, cos a)`, so "inward" is a quarter turn, not a half |
| `open` worked on a dismissed pod; `summon` refused to move a closed one | states were doing double duty as animations. States are now what the pod *is* (`dormant`/`closed`/`open`), clips are what it is *doing*, and an **open** pod is never teleported by a summon |
| contents drawn at full size through the pod | no per-attachment fit — `attach.scale` |
| the page kept serving old JS after an edit | `frontendDist` is **embedded into the binary**: a `ui/js` change needs `cargo build`, not a restart. This cost the most time of anything here |

### Verification (what was actually run)

| Check | Result |
|---|---|
| `cargo test --lib entities::` | 45 passed (8 chest: singleton summon, move-vs-leave-alone, open reveals / close hides, closed refuses put, capacity refusal, take lands beside, dismiss names slots and force drops them) |
| variety | 100 seeds produce ≥60 distinct structures (petal count, facet style, face, feet, charm, radius) |
| `validate_rig` | every generated pod is a rig the loader accepts, with its four advertised clips |
| Live: `summon` | created, `closed`, capacity 7, anchor = avatar + (110, 62) |
| Live: `open` → `put survival:rod` | contents `1 / 7`, rod attached at `chest_interior`, scale 0.45, drawn inside the pod |
| Live: `close` → `dismiss` | 400 naming `survival:rod`; `take` → rod back on the stage at `world`, `dismiss` → `dormant` |
| Live: re-`summon` | the same pod returns at the new anchor, `created: false` |

### Deferred, deliberately

The **walk-to choreography** (gremlin walks over before it opens) and the **chest
card** (a panel with the slots, plus a `wireRegions` entry so the pod is
clickable) are the next slice; they are page work and touch movement, so they get
their own change. Also deferred: **seeded decorative loot** on first summon, and a
real `vanish` animation (a dismiss is currently instant — hiding is `visible:
false`, there is no scheduler to run the clip and then hide).

---

## 11. Phase 4 — landed: the walk, the trinkets, the vanish

The part that makes the pod feel alive rather than scripted.

| Piece | What |
|---|---|
| `ui/js/main.js` | `beginChestWalk` / `stepChestWalk`: on `chest-summoned`, walk to the anchor and open the pod on arrival |
| `ui/js/entities.js` | the chest API (`summonChest`, `openChest`, `putInChest`, …), the `dormant` draw rule, `entry.clipT` |
| `src-tauri/src/entities/gen.rs::trinket` | a faceted find: six silhouettes (pebble, crystal, seed, bead, shard, shell), a twinkling glint bone |
| `src-tauri/src/entities/chest.rs` | seeds 3–5 trinkets on a *new* pod; dismiss treats the pod's own things as furniture |

### The choreography

`chest-summoned` carries `x`/`y`, so no round trip is needed: the page walks the
gremlin to `(x - 96, y - 24)`, then POSTs `/chest/open` **once** on arrival. It
stands down — and does not retry — when the user drags the avatar mid-walk
(`avatar.manualMoves` moves), when a scavenging run starts, or after 14 s. A pod
left closed is still a pod the user can open themselves, so giving up quietly is
the right failure.

### Loot: a pod you want to open

A summons that produces an empty chest is furniture. A new pod now rolls 3–5
trinkets from its own seed, scattered across the well (not stacked on a point),
always leaving two slots free. They are owned by `chest`, which is the whole trick
behind dismiss: the pod's own things leave with it, and only the items *you* put
in can block a dismissal.

### Vanish without a scheduler

`dormant` means "not on the stage", and the page enforces it: `entityDrawables`
skips a dormant entity **unless its clip is still running** (`entry.clipT < 1`).
Dismiss sets `state: dormant` + `clip: vanish` + `visible: true`; the clip plays
for 360 ms, then nothing draws. No timer, no follow-up call, and a page reloaded
mid-vanish resolves to "finished, hidden" from the persisted `started_ms`.

### Verification

| Check | Result |
|---|---|
| `cargo test --lib entities::` | 47 passed (loot seeds and leaves two slots, trinkets are pod property, a moved pod does not re-roll, dismiss with only pod trinkets succeeds, a put item blocks and force drops it) |
| Live: summon at `[1250, 700]` from `(766, 537)` | gremlin walks 766→1154 over ~2.5 s, then the pod opens itself (`state: open`) |
| Live: contents | `seeded: 3`; the page reports three `trinket` entities visible at the pod's interior |
| Live: dismiss | `state: dormant`, `clip: vanish`; `stats.drawables` drops to 0 after the clip |
| Visual | three distinguishable faceted stones in the well beside the gremlin |

### Still open

The **chest card** (a panel with the slots, a Take button each, and a click region
so the pod is clickable) is the remaining slice of the original plan. Everything
it would call already exists on the page (`chestStatus`, `putInChest`,
`takeFromChest`, `dismissChest`), so it is UI work, not plumbing.

---

## 12. The pod has no bottom, and it can be picked up

Two corrections to the design above, both from watching it work.

### Capacity was the wrong idea

The pod shipped with 5–8 slots. That made it a *container* — and a container is a
place you manage, which is not what a gremlin's curiosity pod is for. Its job is
to be **absurdly bigger inside than out**, so the ceiling came out: `put` has no
check, `GET /chest` reports `count` and `"unbounded": true`, and the card shows
`∞`.

### The rummage

`POST /chest/find` is a search that overshoots. Ask for an axe and one to three
wrong things come out first — a rubber duck, an anvil, a magnifying glass, a boot
(`defs::JUNK`) — each a real entity with a real mini-rig, dropped on the stage in
front of the pod. Then, if it is in there, the axe.

Three details that make the gag land:

| Detail | Why |
|---|---|
| **Distinct duds** | drawn from a shrinking pool, not by index |
| **The mess stays** | `owner: "chest"` + `world` attach = exactly what `tidy` sweeps |
| **The host writes the sentence** | `narrative` is ready to speak; neither page nor model reconstructs it |

Junk is generated like everything else — `gen.rs::{rubber_duck, anvil,
magnifying_glass, boot}` — so the gag needs no special-case artwork, and a future
mining plugin can add a pickaxe to the pool with one entry.

### Movable furniture

The pod drags: press, move, release (one write). A press that does not move opens
the card. Same threshold rule as the avatar drag, and `body.pod-dragging` keeps
the window interactive for the gesture so the cursor tracker cannot drop it.

### 12.1 Debris expires

The mess is funny for about fifteen seconds and then it is just clutter. Entities
carry an optional `expires_at`; duds set it, everything else leaves it unset, and
`api.rs::housekeeping` sweeps once a second.

The rule that matters: **an unset lifetime is never reaped.** Adding a TTL to the
entity record is only safe if the default deletes nothing — otherwise a future
change to a spawn path silently starts eating a user's props.

`tidy` and the reaper share the same definition of debris (`owner: "chest"` plus a
`world` attachment), differing only in patience.

### 12.2 The pull, one at a time

The first rummage arrived as a pile on one frame, which is not a rummage — it is a
spawn. Three changes turned it into a bit:

| Piece | Where |
|---|---|
| `reveal_at` per dud, 300 ms apart, the find after the last | `chest.rs::find` |
| "do not draw before reveal; on reveal, throw it" | `entities.js::tossOffset` |
| the gremlin looks at the pod and digs for the length of the sequence | `main.js` on `chest-find` |

The throw is page-side and purely cosmetic: the host never knows an item was in
the air. That keeps the store honest (a dud's position is its landing spot from the
moment it exists) and means a missed frame costs a flight, never a lost item.

The arc is `sin(πt)`: zero at the mouth, zero at the landing spot, peak in
between — an object leaves the pod, rises, and lands where the host said. Spin is
`(1 - t)` in a direction taken from the id, so no two duds tumble identically.

### 12.3 Every prop tumbles in its own way

The arc alone made the items *travel*; it did not make them *thrown*. So each junk
prop now ships a `tumbling` clip and the host throws every dud in that one state:

| prop | tumbling |
|---|---|
| rubber duck | head flaps, body squash-stretches |
| anvil | tips once, lands with a heavy squash |
| magnifying glass | wobbles on its free-swinging handle |
| boot | a full end-over-end flip, dead stop |

The state name is shared and the clips are not — that is `clip_for_state` doing the
work it was built for, and it means the rummage code never mentions a prop.

The scatter also widened: a fan of ~24° per dud out to 120–250 px, stepped by how
much debris is already on the stage, so a second rummage lands beside the first
instead of on it.

### 12.4 Tools come out of the pod

The roadmap promised a fishing pole that appears when the gremlin fishes and a
lantern/pickaxe/helmet for mining later. What was missing was **where they come
from**: a prop that materialises in a paw is a costume change, not an object.

`POST /chest/withdraw` (and the `withdraw` guest op) makes the pod the supply
point: rummage first — junk, thrown, one at a time — then the tool. The survival
plugin now asks for its rod instead of spawning it, so the first cast of a session
starts with the gremlin digging in its pod.

Three rules make it a beat rather than a dependency:

1. **The pod comes when it is called for.** Absent or dormant, `withdraw` summons it
   beside the gremlin (110 px right, 62 px below) and opens it. The store learns
   where he stands from `report_state` — the page is the only thing that knows.
2. **The rod stows — after a beat.** At the end of a session — landed fish, lapsed
   bite, cancel — the rod first **rests** in his paw for two seconds (`resting`: tip
   down, line slack, a slow breath), and is then **stowed**: the entity survives,
   invisible, and the next cast just raises it.

   It used to go back in the pod, which made every cast a rummage — and back-to-back
   fishing then played the whole pod-appears-rod-is-fished-out ritual on every cast.
   The pod sequence belongs to the **first** rod of a session, when there is no rod
   to raise. The linger still makes the ending legible; the stow is what makes the
   next beginning quick.
3. **Nothing is fetched for a cast that is not happening, and nothing is fetched
   twice.** The rod is made when the phase leaves `Idle`, never at boot, or a pod would pop in beside a gremlin who is
   only standing there.

And one for old hosts: if the op is refused outright, `spawn` still works, so
fishing degrades to a rod in the hand with no theatre.

The same shape serves mining later: `withdraw {def:"lantern", socket:"head_lamp"}`,
`withdraw {def:"pickaxe", socket:"hand_primary"}` — no new host code, and the gag
happens automatically.

### 12.5 Gestures, so props have hands to come out of

A prop that appears in a paw without the avatar *doing* anything is a sprite swap.
The rigs have no spare animation data — the catalogue is `breathe`, `wave`,
`metamorphose` — and the generated ones will never get bespoke clips for "put the
pickaxe away".

So gestures are procedural: a table of bone roles plus an envelope over `t`, in
`ui/js/gestures.js`. Two exist:

| gesture | reads as | used by |
|---|---|---|
| `pack-up` | arm drops, he leans off the work, looks at what he is holding, lets go | the rod's `resting` state — and any tool that adopts it |
| `reach` | he bends, takes what is on the floor, straightens up | arriving at a withdrawn tool |

They compose with everything else rather than replacing it: `rig.js` adds the
offset on top of the pose, the life-signs and a scavenging dig, then clamps to the
bone's declared limits, so a rig that sways still sways while it packs up and a rig
with an arm range cannot be puppeted past it.

The mining set is the test of the abstraction: `reach` already covers picking a
pickaxe off the stage, and `pack-up` covers putting a lantern away — both without a
single line of new animation.

### 12.6 The loot bag: supply, carry, fill, return

Scavenging used to be a number: he wandered, then reported items. Now it has
props, and the props carry the story:

1. **Out of the pod.** `start_scavenge` withdraws a `loot_bag` — the pod pops in,
   junk flies, the sack lands, he reaches down and takes it (`reach`).
2. **In the off hand.** `hand_secondary`, because the working paw has to stay free
   to dig. `LOOT_BAG` already declared that grip, so the strap lands in the fingers.
3. **It fills as he digs.** The run rolls its loot at the *start* and keeps it in the
   session; each find's beat sends one `params.fullness` step, and the generator
   widens the sack up to 18%. Measured on the same seed: `54×36` empty →
   `58.9×39.2` half → `63.7×42.5` full.
4. **Then it goes back.** `full` for a couple of seconds while the packing-up
   gesture plays, then the entity is removed and the next run fishes it out again.

The lesson generalises: **an activity that has props needs a beat schedule**, and
the schedule belongs to the thing that owns the numbers (the guest), not the page.
The page only knows how to draw a fatter sack.

### 12.7 The lid is part of the gag

A pod that stays open never unfolds again: the clip is bound to the state, and a
state that does not change plays nothing. So the rummage owns the whole lid:

1. **Open it on the way in.** `find` and `withdraw` unfold a shut pod instead of
   refusing; the lid swings wide, the junk comes out.
2. **Announce the lid.** `open`/`close` emit `entity-updated`, the frame the page
   draws from. (They had their own events for months, and nothing listened — the pod
   only caught up on the two-second reconcile, which is why it appeared not to open.)
3. **Shut it on the way out.** The page closes the pod once the sequence has finished
   spawning — after the last item's beat, and after `pickup_ms` when a tool was taken.
   Next pull starts from a closed lid, so the unfold is always visible.

The general rule this settles: **a prop's state transition must be announced in the
frame the renderer listens to**, not in a bespoke event. `chest-opened` is still
emitted for anything that wants the semantic one, but presentation rides on
`entity-updated`.
