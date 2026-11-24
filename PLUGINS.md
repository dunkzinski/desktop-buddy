# PLUGINS.md — the WebAssembly plugin runtime

Everything outside the core avatar engine (metabolism, the bag, the shop, fishing,
procedural creatures) lives in **WebAssembly guests** that the app loads at
runtime from `plugins/*.wasm`. The host is feature-blind: it knows the ABI, not
the features. Adding a plugin is a directory and a build; removing one is
deleting a file.

```
plugins/
  survival/          guest crate (own Cargo workspace)
  metamorphosis/     guest crate (own Cargo workspace)
  survival.wasm      built artifact — gitignored
  metamorphosis.wasm built artifact — gitignored
  *.state.json       the session each guest saves beside its .wasm — gitignored
src-tauri/src/plugins/
  mod.rs             Plugin / PluginHost / PluginConfig traits, registry, ticker
  manifest.rs        registry filler: scans plugins/ for *.wasm
  wasm_host.rs       wasmtime engine, ABI bridge, loader, fuel, state files
  tests.rs           registry/host contract tests
```

---

## 1. Build, drop in, restart

```bash
./scripts/build-plugin.sh              # every plugin
./scripts/build-plugin.sh survival     # just one
```

The script runs `cargo build --release --target wasm32-unknown-unknown` inside
each `plugins/<name>/` workspace and copies the artifact to
`plugins/<name>.wasm`. Nothing in `src-tauri/` is rebuilt: the host reads the
file at startup.

* **A `.wasm` is invisible to `cargo`.** `cargo build`, `cargo test` and
  `tools/verify_docs.py` all pass with `plugins/` empty. Only a running app (or
  `cargo test --lib loads_the_shipped_plugin`) proves a guest actually loads.
* **Editing a guest changes nothing until you re-run the script.** The host
  loads the artifact, never the source tree.
* `plugins/*.wasm` and `plugins/*.state.json` are gitignored, so a fresh clone
  must build before the features exist, and `git status` is never evidence that
  an artifact was rebuilt.

### Where the host looks

`wasm_host::plugins_dir()` resolves, in order:

1. `$DESKTOP_BUDDY_PLUGINS_DIR` (must be an existing directory);
2. `./plugins` relative to the process working directory;
3. `plugins/` beside the executable;
4. `plugins/` beside the repo root (i.e. next to `models/`).

The first directory that exists wins. Every `*.wasm` in it is loaded, sorted by
file name — which is also `init`, `tick` and `shutdown` order.

---

## 2. The ABI

JSON in, JSON out, over a pair of pointers into the guest's own linear memory.

### Guest exports (the host calls these)

| Export | Signature | Purpose |
|---|---|---|
| `alloc` | `(len: u32) -> u32` | the host writes its request into guest memory |
| `dealloc` | `(ptr: u32, len: u32)` | either side frees a buffer it owns |
| `plugin_init` | `(ptr, len) -> i64` | `{"config", "state", "state_path"}` |
| `plugin_tick` | `(dt_ms: i64) -> i64` | one tick; `null` on success |
| `plugin_status` | `() -> i64` | the guest's own status object |
| `plugin_routes` | `() -> i64` | `[{"path", "methods"}]` |
| `plugin_http` | `(ptr, len) -> i64` | `{"method", "path", "body"}` → `{"status", "body"}` |
| `plugin_config` | `() -> i64` | config descriptor `{"key", "title", "describe"}` |
| `plugin_config_validate` | `(ptr, len) -> i64` | patch → canonical JSON **text** |
| `plugin_config_apply` | `(ptr, len) -> i64` | adopt a validated block (live) |
| `plugin_config_warnings` | `(ptr, len) -> i64` | stray keys the guest ignores |

A `plugin_*` reply packs `(ptr as i64) << 32 | len`. **Zero means "no result"**,
and a body containing `{"error": "…"}` is a failed call — the host turns that
into a plugin error, not a panic.

### Host imports (the whole capability surface)

| Import | Signature | Notes |
|---|---|---|
| `host_emit_event` | `(name_ptr, name_len, payload_ptr, payload_len)` | publishes on the SSE bus **and** to the page |
| `host_notify` | `(text_ptr, text_len, expr_ptr, expr_len)` | speaks a bubble; the expression must be one of `AVATAR_EXPRESSIONS` |
| `host_now_ms` | `() -> i64` | the host's clock (epoch ms) — the guest has no clock of its own |
| `host_log` | `(level: u32, ptr, len)` | 0 debug, 1 info, 2 warn, 3+ error |
| `host_state_save` | `(ptr, len) -> i32` | `0` written; negative refused (guest keeps its dirty flag) |
| `host_ai_attached` | `() -> i32` | `1` when a watcher is subscribed to the event bus |
| `host_model_write` | `(slug_ptr, slug_len, json_ptr, json_len) -> i32` | validates with the app loader, writes `models/<slug>/model.json` |
| `host_model_activate` | `(ptr, len) -> i32` | hot-swap the avatar — `POST /model`'s code path |
| `host_animate` | `(ptr, len) -> i32` | play a clip by name — `POST /animate`'s code path |
| `host_entities` | `(req_ptr, req_len, out_ptr, out_cap) -> i32` | prop entities: one JSON request in, one JSON reply out (see below) |

Everything else is unavailable by construction: a guest has no filesystem, no
network, no environment and no way to reach the app's internals.

### Props: `host_entities`

One request, one reply, both JSON. The reply is written into a buffer **the guest
allocates**; the return value is the number of bytes written, or a negative ABI
status (`-1` no memory, `-2` request over 8 KiB, `-3` not JSON, `-4` reply did not
fit).

```json
{"op":"spawn",  "def":"fishing_rod", "id":"survival:rod", "seed":739311, "state":"stowed"}
{"op":"update", "id":"survival:rod", "state":"cast", "visible":true,
                "attach":{"parent":"avatar","socket":"hand_primary"}}
{"op":"update", "id":"survival:rod", "detach":true, "visible":false}
{"op":"remove", "id":"survival:rod"}
{"op":"get",    "id":"survival:rod", "rig":true}
{"op":"list"}
{"op":"withdraw", "def":"fishing_rod", "id":"survival:rod", "socket":"stage_ground"}
```

* **Ownership is stamped by the host**, never sent: a guest spawns as itself and
  can only update or remove what it owns. Another plugin's prop answers
  `{"ok":false,"error":"entity … is owned by …"}`.
* **A refusal is not an ABI failure.** The call returns a positive byte count and
  `{"ok":false,"error":…}`; only `-1…-4` mean the ABI itself failed. Treat `ok`
  as the answer and log the error, or a refused spawn looks like a crash.
* **Replies are summaries** — `id`, `def`, `state`, `attach`, `visible`, `owner`,
  `traits` — never the rig. A whole rod entity is several KiB, and a guest with a
  4 KiB buffer would get `-4` *after* the spawn had already landed. `get` with
  `"rig": true` returns geometry for a guest that really wants it (and needs the
  buffer for it).
* **`spawn` is idempotent by id+def**: a retry returns `{"created":false}` and the
  existing entity, so a guest that missed its ack cannot make two rods.
* **A state whose name is a clip plays that clip.** `{"state":"biting"}` is all a
  guest needs; the page looks the name up in the entity's own animations, so no
  guest ever ships animation data.
* **`withdraw` asks the curiosity pod to *produce* a tool**: a rummage first (one
  to three junk props thrown onto the stage), then the tool, on the stage or
  straight into a `socket`. It is idempotent by id, answers `pickup_ms` (how long
  until the tool belongs in a hand — wait for it and the pick-up is visible), and it
  **brings the pod with it** — no pod, or a dormant one, is summoned beside the gremlin and opened, so a
  guest never has to arrange furniture first. Keep the `spawn` fallback for a host
  without the op (it answers `ok:false`). `find` (search the contents) lives on HTTP
  only; a guest that needs a tool wants `withdraw`.
* **Scavenging carries a bag the same way.** `start_scavenge` withdraws a `loot_bag`
  (`hand_secondary` — the working paw stays free), then steps its `params.fullness`
  up as each find lands, so the sack visibly fattens during the run. At the end it
  goes `full` and then `resting`, which is the page's cue to pack it away.
* The `survival` plugin uses this for the fishing rod: `plugins/survival/src/props.rs`
  asks the pod (`withdraw`) on every cast and falls back to `spawn` if the host
  refuses; it attaches the rod to `hand_primary`, walks it through
  `waiting → biting → reeling`, holds it on the stage for `pickup_ms` first, rests it
  in paw (`resting`) for a couple of seconds when the session ends, and **then stows
  it** — the entity survives, invisible, so a back-to-back cast raises the rod it
  already has instead of playing the pod ritual again. Nothing is fetched for a cast
  that is not happening. With no entity ABI the calls fail and fishing is unchanged
  — the prop is presentation, never a rule.
* **The catch hangs off the rod, not the gremlin.** `land` spawns a `fish` (or a
  `boot` when the river hands back `old_boot`) attached to `entity:survival:rod` at
  the rod's **own** `line_end` socket — the first nested prop in the game — and
  removes it when the rod's two-second linger ends, so the thing in the bag is one
  he was holding. No `state` is sent: a state the def does not declare is a refusal,
  and each catch prop's own default is the one that knows how it arrives (the fish's
  is `flopping`; the boot just hangs there).

### Isolation

| Limit | Value | Constant |
|---|---|---|
| Instructions per guest call | `50,000,000` | `FUEL_PER_CALL` |
| Largest request/response body | 4 MiB | `MAX_MESSAGE_BYTES` |
| Largest state file the host reads or writes | 1 MiB | `MAX_STATE_BYTES` |
| Tick period | 1 s | `TICK_INTERVAL` |
| Longest tick delta handed to a guest | 900 s | `MAX_TICK_DT` |
| Consecutive tick errors before parking | 3 | `MAX_CONSECUTIVE_TICK_ERRORS` |

* **Memory:** each guest gets its own `Store`, so linear memory, fuel and failure
  are per plugin. wasmtime bounds-checks every access; the host only copies
  through the exported area and rejects out-of-range pointers (an empty string,
  never a panic).
* **CPU:** fuel is metered per call. `Config::consume_fuel(true)` starts a store
  with **zero** fuel, so the host seeds `FUEL_PER_CALL` before `instantiate`
  (which runs the start function and data segments) and resets it before every
  call and before `dealloc`. Forgetting the first seed traps with
  `all fuel consumed by WebAssembly`.
* **Crashes:** a guest `panic = "abort"` becomes a trap. The registry counts it,
  parks the plugin after three consecutive tick errors, and still runs its
  `shutdown` hook. The app keeps running.
* **Loading:** a `.wasm` that fails to compile or instantiate is logged with
  `{err:?}` and skipped — a bad drop-in cannot stop the app from starting.

---

## 3. Lifecycle

```
load_all()          compile each .wasm, instantiate into its own Store
  plugin_config()   asked on a COLD instance, before init: which block is yours?
install(host)       registry filled, init_all() in discovery order
  plugin_init       {config, state, state_path}
spawn_ticker()      one thread, 1 s slices, dt clamped to 900 s
  plugin_tick(dt)   active plugins only
stop()              request_shutdown → shutdown_all()
```

`plugins::install` runs during Tauri `setup`, **before** `api::start`, because
`api::start` builds the axum router exactly once and plugin routes are merged
into it. A router built with an empty registry serves a plugin-less API whose
`/plugins` still answers — the log line `merged N plugin route(s)` is the
evidence to check.

### Registry states

| State | Meaning |
|---|---|
| `registered` | loaded, `init` not yet run |
| `active` | initialised and being ticked |
| `failed` | `init` returned an error or panicked; skipped, never ticked |
| `stopped` | `shutdown` ran |

`GET /plugins` reports:

```json
{
  "ok": true,
  "ticks": 412,
  "count": 2,
  "active": 2,
  "plugins": [
    { "id": "survival", "description": "…", "state": "active",
      "ticks": 412, "errors": 0, "last_error": null, "status": { } }
  ]
}
```

Only `registered`/`active` plugins contribute routes, and only non-`stopped`
plugins contribute config sections. `status` is whatever the guest's
`plugin_status` returned.

### State files

The host owns the bytes: `plugins/survival.wasm` → `plugins/survival.state.json`
(`state_path_for` swaps the extension). `plugin_init` receives the last file as
`{"state": …}` plus the path it will be written back to, and `host_state_save`
writes it atomically. A guest with no state path still works — everything just
stays in memory.

---

## 4. The config bridge

A guest may own one block of `watcher_rules.json`. The host never parses it.

1. `plugin_config()` answers `{"key": "survival", "title": "Survival"}` on a
   **cold** instance — the host needs the key before `plugin_init` so it can
   hand over that section.
2. `GET /config` lists the section because a plugin claims it, and reports the
   guest's own `describe()` output as the *effective* config.
3. `POST /config` hands a patch to `plugin_config_validate(current, patch)`.
   The guest merges one level deep (so a single `metabolism.hunger_per_min` is
   enough), validates, and returns the canonical block as **text** — text so the
   guest keeps its own field order instead of serde's alphabetical one.
4. `plugin_config_apply(block)` adopts it. `Ok(true)` is live; `Ok(false)` means
   "written, but the process read it at startup" and lands in
   `restart_required`.
5. `plugin_config_warnings(block)` reports keys present in the file that the
   guest ignores — the "looks configured, runs on defaults" trap.

A patch naming a block no plugin owns is a 400 listing the known sections.

---

## 5. Writing a plugin

A guest crate is a plain `cdylib` + `rlib` with its own workspace:

```toml
[workspace]

[lib]
crate-type = ["cdylib", "rlib"]   # cdylib = the .wasm, rlib keeps cargo test working

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

`src/host.rs` declares the imports as `extern "C"` on wasm32 and as a recording
stub elsewhere, which is what lets the guest's unit tests run natively
(`cd plugins/survival && cargo test --lib`). Keep the stub's recorder
**thread-local** with one fake clock: a process-global mutex recorder makes
parallel tests race, and a test with a real deadline must advance
`host::set_now_ms` because the tick's `now_ms` never moves by itself.

Pitfalls that cost real time:

* **Never call back into the registry from inside a call.** `plugins::status()`
  takes the registry mutex the ticker already holds, so it deadlocks. That is
  why `GET /survival` no longer embeds a `plugins` array.
* **`plugin_config` must answer on a cold instance** (see §4).
* **A guest's route table is parsed by `tools/verify_docs.py`**, which reads
  `("/path", &["GET", …])` literals out of `plugins/*/src/*.rs`. A new route has
  to appear in `API.md` **and** `ARCHITECTURE.md` or the doc check fails.
* **Adding a config key is four edits**: the guest's `model.rs` (or equivalent),
  its `config.rs` `KNOWN_KEYS` + `check()`, `watcher_rules.json`, and
  `ARCHITECTURE.md` — otherwise `plugin_config_validate` rejects exactly what the
  docs just described.

---

## 6. The `survival` guest

```
plugins/survival/src/
  model.rs      catalog, recipes, stats, tiers, cravings, craft maths
  plugin.rs     init/tick/shutdown, the rules, every host_notify line
  routes.rs     ROUTES table + handlers, EatOutcome/CraftOutcome
  inventory.rs  ITEMS catalog, stack-aware bag, slots
  store.rs      CATALOG, buy/sell maths
  fishing.rs    FishingConfig, phase machine, sweep maths, loot table
  scavenge.rs   ScavengeConfig, the 10-30 s window, both loot tables, lines
  persist.rs    Snapshot shape + version guard
  config.rs     ConfigDescriptor, KNOWN_KEYS, check()
  host.rs       the imports; recording stub off wasm32
```

`GET /plugins` lists it as `survival`; `survival.enabled: false` keeps it
installed but idle (no drain, no telemetry, routes answer `enabled: false`).
The registry is never mutated at runtime, so `GET /plugins` stays honest.

### Stats and tiers

Two bars, 0–100: `hunger` and `saturation`. **Saturation drains first**; hunger
only falls once saturation hits 0, so a snack buys more time than its nutrition
suggests. `Metabolism` is `{hunger_per_min, saturation_per_min}` (default
`1.5` / `2.0`); a resumed machine's delta is clamped at `MAX_TICK_SECS = 900`.

| hunger | tier | expression | meaning |
|---|---|---|---|
| 80–100 | `satiated` | `happy` | refuses more food |
| 50–79.99 | `peckish` | `neutral` | starting to think about lunch |
| 20–49.99 | `hungry` | `thinking` | distracted, mentions food |
| 0–19.99 | `starving` | `sad` | desperate |

Boundaries are `SATIATED_MIN = 80`, `PECKISH_MIN = 50`, `HUNGRY_MIN = 20`;
`HungerTier::of` folds NaN and ±∞ to the ends instead of falling through to
`starving`. `is_critical()` is `hungry | starving`.

### Food

`FoodKind` is `Raw` or `Cooked`. Raw stock is edible but weak; a `Recipe` result
carries a `comfort_secs` window during which hunger drains at
`WELL_FED_DRAIN_FACTOR = 0.5`. **Every crafted meal beats every raw ingredient on
saturation ratio** — a unit test (`worst_crafted > best_raw`) enforces it;
ready-made dishes are exempt because a drink is not a dinner.

24 entries, 12 raw and 12 cooked:

| id | label | kind | group | nutrition | sat. ratio | comfort | cooldown |
|---|---|---|---|---|---|---|---|
| `apple` | Apple | raw | fruit | 12 | 0.50 | — | 10 s |
| `egg` | Egg | raw | protein | 6 | 0.20 | — | 5 s |
| `cheese` | Cheese | raw | dairy | 8 | 0.30 | — | 5 s |
| `tomato` | Tomato | raw | vegetable | 5 | 0.25 | — | 5 s |
| `mushroom` | Mushroom | raw | vegetable | 4 | 0.20 | — | 5 s |
| `noodles` | Dry noodles | raw | grain | 9 | 0.15 | — | 5 s |
| `bread` | Bread | raw | grain | 10 | 0.35 | — | 5 s |
| `honey` | Honey | raw | sweet | 6 | 0.40 | — | 5 s |
| `minnow` | Minnow | raw | protein | 6 | 0.20 | — | 5 s |
| `river_perch` | River perch | raw | protein | 10 | 0.35 | — | 8 s |
| `silver_trout` | Silver trout | raw | protein | 12 | 0.45 | — | 10 s |
| `ember_eel` | Ember eel | raw | protein | 12 | 0.50 | — | 12 s |
| `sandwich` | Sandwich | cooked | grain | 28 | 0.70 | 120 s | 20 s |
| `coffee` | Coffee | cooked | drink | 4 | 0.20 | 60 s | 30 s |
| `cake` | Slice of cake | cooked | sweet | 18 | 0.90 | 90 s | 20 s |
| `chili` | Bowl of chili | cooked | vegetable | 30 | 0.60 | 150 s | 25 s |
| `energy_bar` | Energy bar | cooked | grain | 20 | 0.80 | 60 s | 15 s |
| `soup` | Warm soup | cooked | vegetable | 22 | 0.75 | 180 s | 20 s |
| `omelette` | Cheese omelette | crafted | protein | 26 | 0.85 | 240 s | 25 s |
| `grilled_cheese` | Grilled cheese | crafted | grain | 30 | 0.80 | 240 s | 25 s |
| `pasta` | Tomato pasta | crafted | grain | 34 | 0.85 | 300 s | 30 s |
| `mushroom_stew` | Mushroom stew | crafted | vegetable | 28 | 0.90 | 300 s | 30 s |
| `apple_pie` | Apple pie | crafted | sweet | 24 | 0.90 | 270 s | 30 s |
| `ramen` | Bowl of ramen | crafted | grain | 36 | 0.92 | 330 s | 35 s |

Foods also carry `effects[]` (`soothing`, `energizing`, `spicy`, `sleepy`,
`jittery`) with a duration and magnitude; they are flavour and prompt material,
not stat modifiers.

`find_food` normalises case, spaces and hyphens to underscores, so
`"Energy Bar"`, `energy-bar` and `energy_bar` all resolve. `GET /foods` reports
`count`, `raw_count`, `cooked_count`, `crafted_count`, `recipes` and the catalog
with `{id, label, kind, group, nutrition, saturation_ratio, comfort_secs,
crafted_via}` — `crafted_via` is what the UI filters on to keep crafted dishes
out of the Feed picker.

### Recipes

`POST /survival/craft` takes `{"recipe": "<id>"}` or `{"ingredients": …}`, the
latter in any of three shapes: `["egg","egg","cheese"]`, `{"egg":2,"cheese":1}`
or a bare `"omelette"`. Counts matter and order does not.

| id | result | ingredients | comfort |
|---|---|---|---|
| `omelette` | Cheese omelette | 2 egg, 1 cheese | 240 s |
| `grilled_cheese` | Grilled cheese | 2 bread, 1 cheese | 240 s |
| `pasta` | Tomato pasta | 1 noodles, 2 tomato, 1 cheese | 300 s |
| `mushroom_stew` | Mushroom stew | 2 mushroom, 1 noodles | 300 s |
| `apple_pie` | Apple pie | 2 apple, 1 bread, 1 honey | 270 s |
| `ramen` | Bowl of ramen | 2 noodles, 1 egg, 1 mushroom | 330 s |

Every result id exists in the catalog and every ingredient is `Raw` (both
pinned by tests). Refusals are `too_full` and `still_digesting` — a second cook a
few seconds later is refused on purpose; wait out the per-food cooldown.

### Cravings

A persisted `Craving {kind: food|group, target, ends_at_ms}` on the stats, plus
`craving` and `well_fed_secs` on `GET /survival`.

* Generated by the tick when `cravings` is on and hunger/saturation fall below
  `craving_below_hunger` / `craving_below_saturation`, no more often than
  `craving_interval_secs`.
* A new craving **replaces** the previous one; it lapses after
  `craving_duration_secs` and the plugin says a mourning line.
* Satisfying it sets `craving_satisfied` on the outcome and emits
  `survival-craving` with `state: "satisfied"`.

### Inventory

A slot bag (`survival.inventory.slots`, default 20, clamped 4–40) holding 18
item ids with `stack_max` and `rarity`:

| kind | ids (stack_max / rarity) |
|---|---|
| fish | `minnow` 20/1, `river_perch` 10/2, `silver_trout` 5/3, `ember_eel` 3/4 |
| ingredient | `apple` 12/1, `egg` 12/1, `tomato` 12/1, `noodles` 12/1, `bread` 12/1, `cheese` 12/2, `mushroom` 12/2, `honey` 8/2 |
| bait | `worm_bait` 64/1, `glowing_lure` 8/3 |
| tool | `fishing_rod` 1/2 |
| material | `driftwood` 16/1, `river_weed` 16/1 |
| junk | `old_boot` 2/1 |

A fresh bag starts with `fishing_rod ×1`, `worm_bait ×5`, `driftwood ×2`, and a
purse of `STARTING_COINS = 100`. `count` on add/remove is clamped to
`1..=MAX_ADD` (`99`); `/store/buy` and `/store/sell` reject a bad count with a
400 instead of clamping, because quietly changing the number spends the
player's coins.

### Store

13 stocked items across four shelves (`bait`, `ingredient`, `material`, `tool`).
Sell price is roughly 40% of buy price, so buying to resell never turns a profit.

| id | shelf | buy | sell |
|---|---|---|---|
| `worm_bait` | bait | 4 | 1 |
| `glowing_lure` | bait | 24 | 10 |
| `apple` | ingredient | 4 | 2 |
| `egg` | ingredient | 3 | 1 |
| `cheese` | ingredient | 5 | 2 |
| `tomato` | ingredient | 3 | 1 |
| `mushroom` | ingredient | 3 | 1 |
| `noodles` | ingredient | 4 | 1 |
| `bread` | ingredient | 4 | 2 |
| `honey` | ingredient | 8 | 3 |
| `driftwood` | material | 6 | 3 |
| `river_weed` | material | 5 | 2 |
| `fishing_rod` | tool | 75 | 30 |

The shop also **buys** anything the bag can hold, priced from kind and rarity
when the id is not on the shelf: fish `2 × rarity`, ingredient/material
`rarity`, bait `1 + rarity`, tool `4 × rarity`, junk a flat `1`. A stocked id
always pays its own `sell_price`, so the shelf and the counter cannot disagree.

### Fishing

Four phases: `idle → waiting → biting → reeling`, with `cancel` and escapes back
to `idle`. Timing comes from `survival.fishing`:

| field | default | range | meaning |
|---|---|---|---|
| `enabled` | `true` | — | master switch (`POST /fishing/cast` 409s when off) |
| `require_rod` | `true` | — | the bag must hold a `fishing_rod` |
| `bait_required` | `false` | — | spend one bait per cast |
| `hook_key` / `reel_key` | `"Space"` | non-empty | the two press names the UI shows |
| `mouse_button` | `0` | 0–2 | left / middle / right |
| `min_wait_secs` / `max_wait_secs` | `3` / `12` | 1–120, max ≥ min | the wait is drawn in this span and never published |
| `hook_window_ms` | `1400` | 250–8000 | how long the bobber stays down |
| `reel_period_ms` | `1500` | 300–6000 | one full 0→1→0 sweep |
| `sweet_width` | `0.22` | 0.04–0.6 | width of the scoring window |
| `difficulty` | `1.0` | 0.5–2.0 | divides the period and the sweet width |

`effective_period_ms = reel_period_ms / difficulty` and
`effective_sweet_width = sweet_width / difficulty`, both clamped to their
ranges. The marker is a triangle wave (`sweep_position`); the sweet spot starts
somewhere in `0.04 .. 1 - width - 0.08` so the marker never has to reach an
extreme. The line goes slack after **two** sweeps (`ReelSpec::deadline_for`).

The plugin judges the press against **its own clock**; the page's marker is
decoration. Escapes are `spooked` (the hook window closed), `missed_the_spot`,
`timed_out` and `inventory_full`.

Loot is sampled by cumulative weight over 100:

| item | weight |
|---|---|
| `minnow` | 34 |
| `river_perch` | 26 |
| `silver_trout` | 18 |
| `old_boot` | 10 |
| `ember_eel` | 6 |
| `river_weed` | 6 |

A landed junk item still pays one coin at the counter, so a bad cast clears a
slot for something better.

### Scavenging

The zero-cost fallback: **no rod, no bait, no coins, no food** is checked or
spent, and a run always pays. That is what stops the bait/coin loop from
dead-ending — the one state where fishing is closed and the shop is unaffordable
is now recoverable in under a minute.

| | |
|---|---|
| duration | a random **10–30 s** (`MIN_DURATION_SECS` / `MAX_DURATION_SECS`) |
| ordinary finds | **1–3** (uniform), sampled by cumulative weight |
| rare find | one extra, `rare_chance_per_mille` (default **180**, i.e. 18 %) |
| coins | **1–6**, always banked, even with a full bag |

The ordinary table is the desktop's flotsam — `driftwood` 20, `river_weed` 18,
`minnow` 12, `apple` 12, `egg` 10, `bread` 10, `mushroom` 8, `old_boot` 6 and
`worm_bait` 4 — so a run can hand back the exact thing the loop ran out of, not
just the coins to buy it. The rare table is `silver_trout` 26, `glowing_lure` 22,
`honey` 20, `cheese` 18 and `ember_eel` 14. Every id in both tables is a real
`ITEMS` entry with a positive counter price (a test pins it), because an
unsellable find would not be a way back into the economy.

The phase is `idle` / `rummaging`; there is no intermediate step, because the
walk itself is the page's animation. Completion runs from `plugin_tick` **and**
from `GET /scavenge`, and flips the phase before publishing, so whichever caller
first sees the deadline banks the loot exactly once. The session is **not
persisted** — at most 30 s, and nothing is banked until it completes — so a
restart mid-run loses an animation, never progress.

Refusals are always about state, never stock: the plugin or the activity is
switched off, a run is already out, or a line is in the water. `POST
/fishing/cast` refuses the other way round, so the two activities cannot overlap.

**He mutters while he works.** `POST /scavenge/comment` takes the window name the
page derives from `GET /context` (`{"window": "Hermes"}`) and answers
`{ok, spoke, reason, line, window, comments, session}`. The guest owns the
cadence — at most **2 per run**, at least **8 s** apart, then a
`comment_chance_per_mille` roll (default **300**) — and the phrasing, in four
window-aware variants plus four generic ones for a backend that cannot name a
window (`clean_window` trims a name to 40 chars). A declined request is a
**200** with `spoke: false` and `reason: "declined"` (roll) or `"quiet"` (cap or
gap); only "not out scavenging" is a **409**.

Unlike every other survival line, a comment is **not** routed to the brain and is
spoken by the guest **even with one attached**: nothing happened except the avatar
looking around, so there is no trigger, and a model turn per mutter would still
narrate it after the fact. `notify: false` silences it like any canned line.

### Routes

| route | body | notes |
|---|---|---|
| `GET /survival` | — | `{ok, enabled, config, stats, hunger, saturation, tier, label, expression, craving, well_fed_secs, coins, inventory, fishing, persistence}` — `stats` carries `metabolism`, `meals`, `last_meal`, `seconds_to_next_tier` |
| `POST /survival` | `{hunger?, saturation?, craving?}` | clamped 0–100; `craving: null` clears it; writes stats **directly** |
| `POST /survival/feed` | `{food}` or `{id}` | `EatOutcome`; 409 on refusal |
| `POST /survival/craft` | `{recipe}` or `{ingredients}` | `CraftOutcome`; 409 on refusal |
| `GET /inventory` | — | slots, catalog, wallet |
| `POST /inventory/add` | `{item, count?}` | `id` is a synonym; count clamped 1–99 |
| `POST /inventory/remove` | `{item, count?}` | same |
| `POST /inventory/use` | `{item}` / `{id}` / `{slot}` | eating from the bag emits a meal |
| `GET /fishing` | — | phase, session, config, bag summary |
| `POST /fishing/cast` | — | 409 without a rod, or while already fishing |
| `POST /fishing/hook` | — | only legal in `biting` |
| `POST /fishing/reel` | — | judges the sweep; lands or escapes |
| `POST /fishing/cancel` | — | give up the current cast |
| `GET /scavenge` | — | `{ok, enabled, session, config, inventory, coins}`; advances (and may complete) the run |
| `POST /scavenge/start` | — | 409 only when off, already out, or a line is in the water |
| `POST /scavenge/cancel` | — | come back early; `reason: "cancelled"`, nothing banked |
| `POST /scavenge/comment` | `{window?}` | one line of ambient muttering; 200 even when declined (`spoke: false`) |
| `GET /foods` | — | catalog + recipe book + counts |
| `GET /recipes` | — | `{count, recipes[] {id, label, blurb, ingredients[], result, result_label}}` |
| `GET /store` | — | shelves with `owned` and `stack_max` per row |
| `POST /store/buy` | `{item, count?}` | 409 when the purse or the bag is short |
| `POST /store/sell` | `{item, count?}` | pays `sell_value` |

Every `POST` flushes the guest's save before returning; a `GET` never writes.

### Events

| event | payload | routed by the watcher? |
|---|---|---|
| `survival` | `{stats, source: "telemetry", frame}` | never — telemetry would burn a turn every `telemetry_secs` |
| `survival-threshold` | `from`, `to`, `hunger`, `saturation`, `label`, `expression`, `source`, `line` | yes |
| `survival-meal` | `EatOutcome::to_json()` — `ok`, `accepted`, `food`, `label`, bars before/after, `tier_before/after`, `effects[]`, `refusal`, `cooldown_remaining_secs`, `craving_satisfied` | yes |
| `survival-craft` | the meal payload plus `recipe`, `recipe_label`, `blurb`, `kind`, `comfort_secs`, `consumed[]`, `cooked_with[]` | yes |
| `survival-craving` | `state` (`began`/`satisfied`/`lapsed`), `kind`, `target`, `label`, `hunger`, `saturation`, `expression`, `source` | yes (`lapsed` carries no detail) |
| `inventory-changed` | `{item, label, icon, delta, count, source, inventory}` | no |
| `store-transaction` | `{ok, action, item, label, icon, count, requested, unit_price, total, coins, inventory}` with `action` = `buy` or `sell` | no |
| `fishing-started` | `phase: "waiting"`, wait span, keybinds, session | **no** — a cast turn would still be in flight when the bite lands |
| `fishing-bite` | `phase: "biting"`, `window_ms`, `deadline_ms`, keybinds, `line` | yes |
| `fish-hooked` | `phase: "reeling"`, `reel` spec, keybinds, `difficulty` | yes |
| `fishing-result` | `caught`, `stored`, `item`, `label`, `icon`, `junk`, `species`, `rarity`, `value`, `blurb`, `expression`, `line`, `reason`, `position`, `sweet`, counters | yes — and it queues while busy |
| `scavenging-started` | `phase: "rummaging"`, `started_ms`, `ends_ms`, `duration_secs`, `expression`, `line`, `session` | **no** — it is the page's cue to walk and dig |
| `scavenging-finished` | `reason` (`completed`/`cancelled`), `items[]` (`item`, `label`, `icon`, `count`, `stored`, `rare`, `kind`, `rarity`, `blurb`, `sell_price`), `count`, `stored_count`, `coins`, `rare`, `expression`, `line`, `coins_total`, session counters | yes — own 5 s bucket |
| `scavenging-comment` | `spoke`, `line`, `expression`, `window`, `comments`, `session` | **never** — ambient muttering, and the guest says it itself |

**One voice per moment.** Every spontaneous emit carries `line` and
`expression`, and the guest asks `host_ai_attached()` before speaking: with a
watcher on the bus the model owns the words, and the watcher speaks the event's
own `line` only when the model fails. Without that split a hunger crossing
produced two bubbles for one event.

**A refused meal still emits `survival-meal`** (`accepted: false`) — gate on
`accepted` or the brain reacts to a meal that never happened. All four
`survival-*` kinds share one cooldown key: one feed can emit a meal, a
threshold crossing and a satisfied craving inside one tick, and that is one
reaction, not three.

### Persistence

`plugins/survival.state.json` holds `{version, saved_ms, stats, inventory}`.
`STATE_VERSION` is `1`; a file with any other version is **ignored rather than
migrated** — a fresh session is recoverable, a save read under the wrong schema
is not. Decoding never trusts the contents: stats go through
`SurvivalStats::sanitized` and the bag through `Inventory::from_saved`, which
clamp and report what they dropped.

### Config block

All key names are serde field names — friendly prose names are silently dropped
because the struct has `#[serde(default)]` and no `deny_unknown_fields`. Trust
the `config` echo on `GET /survival`, never the file.

| key | default | notes |
|---|---|---|
| `enabled` | `true` | `false` keeps the plugin installed but idle |
| `start_hunger` | `100.0` | 0–100 |
| `start_saturation` | `60.0` | 0–`start_hunger` |
| `metabolism.hunger_per_min` | `1.5` | 0.01–100 |
| `metabolism.saturation_per_min` | `2.0` | 0.01–100 |
| `notify` | `true` | spontaneous lines through `host_notify` |
| `telemetry_secs` | `60` | `0` off, otherwise ≥ 60 |
| `cravings` | `true` | master switch for cravings |
| `craving_below_hunger` | `70.0` | 0–100 |
| `craving_below_saturation` | `15.0` | 0–100 |
| `craving_interval_secs` | `900` | 60–86400 |
| `craving_duration_secs` | `600` | 30–3600 |
| `fishing.*` | see §Fishing | the nested block merges one level deep |
| `inventory.slots` | `20` | 4–40 |
| `scavenge.enabled` | `true` | master switch; `false` refuses every run |
| `scavenge.min_secs` | `10` | 10–30 |
| `scavenge.max_secs` | `30` | 10–30, never below `min_secs` |
| `scavenge.rare_chance_per_mille` | `180` | 0–1000 |
| `scavenge.comment_chance_per_mille` | `300` | 0–1000; chance a *requested* comment is said |

---

## 7. The `metamorphosis` guest

A deterministic procedural creature generator. From `(seed, archetype, stage)`
it synthesises a complete `desktop-buddy/model@1` skeleton, writes it through
`host_model_write`, and hot-swaps the avatar through `host_model_activate` —
exactly `POST /model`'s path.

```
plugins/metamorphosis/src/
  plan.rs        body plans: plan(), <archetype>_form(), with_element/with_build,
                 individualise(), VARIANT_COUNT
  generator.rs   Archetype, Tier, Dims::for_tier, Features::for_spec, parts, clips
  skeleton.rs    the armature the manifest is built on
  attribute.rs   Seraph / Prism / Feral, what each does to a body, AttributeBlend
  evolution.rs   Metamorphosis state, branches, xp_required, pinning
  plugin.rs      Creature: init/tick, install, evolve, revert, status
  routes.rs      ROUTES table + handlers
  rng.rs         the seeded stream (no OS entropy — determinism is the feature)
  host.rs        the imports; recording stub off wasm32
```

### Variety, by construction

| Layer | Count | What it changes |
|---|---|---|
| Archetypes (body plans) | 15 | `beast`, `drake`, `cyborg`, `holy`, `aqua`, `cervid`, `machine`, `alien`, `avian`, `insect`, `serpent`, `golem`, `amphibian`, `flora`, `wraith` |
| Forms per family | 6 | hand-tuned proportions (the trailing `_ =>` arm is form 4) |
| Builds | 4 | lean / canonical / broad / spare |
| Markings | 6 | surface pattern, `(build + form) mod MARKING_COUNT` |
| Attributes | 3 | Seraph / Prism / Feral — the "element" a body is made of |

`VARIANT_COUNT = BUILD_COUNT × FORM_COUNT = 24`, so one archetype has
**15 × 6 × 4 = 360 structural silhouettes**, and each pairs with a marking
derived from `(build + form) mod 6` — **2,160 base combinations** before the
seed's own individual layer (continuous deformation, a 50% marking re-roll and a
45% second family trait) is applied. A lineage mix then blends up to three
attributes into one body.

**A generator that varies colour first is not varied.** `plan.rs` holds a `Plan`
per archetype (head, ears, crest, arms, legs, tail, trunk, proportion
multipliers) and `generator.rs` only decides how much of that plan exists per
tier; a unit test asserts no two plans share a shape tuple, so a new family
cannot collapse into a recolour.

### Stages

| stage | XP to leave |
|---|---|
| `Fresh` | 40 |
| `In-Training` | 90 |
| `Rookie` | 180 |
| `Champion` | 320 |
| `Ultimate` | 520 |
| `Mega` | — (final) |

Passive XP accrues in `tick` at `1.0 + 0.25 × tier_index` XP per second, so a
Mega takes longer to reach than a hatchling does. `bond` starts at `0.5`, is
clamped 0–1, and only shapes the *order* and labels of the branch table — it can
never take a branch away. `POST /creature/xp` awards `{xp?, bond?}` explicitly.

### Lineage and attribute blend

`Metamorphosis::lineage()` is the hatchling's attribute plus every
`HistoryEntry::to_attribute`, oldest first. `AttributeBlend::from_lineage`
weights that list linearly (`1..n`, newest largest) and normalises, so a stage's
body is its **whole lineage**, not just the current attribute. `percentages()`
always totals 100; `prominence()` is `0.6 + 0.4·w/w_max`, so a one-hot blend is
exactly `1.0` and a pure creature generates byte-identical JSON to a pre-blend
save. `plan::with_blend` gives one-slot traits (crest, hands) to the loudest
attribute and every additive trait to each share above 12%; the part trimmer
drops ornament from the faintest attribute first.

### Pinned bodies

`Spec::variant()` is a seeded draw, so **any** change to `VARIANT_COUNT` would
re-map every existing seed. The save therefore pins the body: `pinned_variant`
on the spec (serde name `variant`), `Metamorphosis::variant` for the live stage,
`HistoryEntry::from_variant` for every ancestor. A rewind restores that exact
body. A save written before pinning derives a variant once from its seed and
pins it on the next write — one visible shift, then stable.

Evolution inherits the parent's variant ~72% of the time, redraws build+marking
in the same form ~16%, and picks a new form ~12%. The draw uses the **child's**
seed and happens in `branches()`, so re-previewing a branch cannot re-roll it,
and the installed rig always equals the previewed one.

### Routes

| route | body | notes |
|---|---|---|
| `GET /creature/status` | — | stage, XP, bond, attribute, `attribute_blend`, slug, generation, `can_evolve`, `can_revert`, `rewind_depth`, rig measurements, `branches[]`, `archetypes[]`, `attributes[]`, `history[]` |
| `POST /metamorphose` | `{branch?, force?}` | 409 `not ready` / `final stage`; reply carries `status` so one request repaints everything |
| `POST /revert` | `{steps}` | 409 with no earlier stage; XP is kept, not refunded |
| `POST /creature/xp` | `{xp?, bond?}` | awards growth; 400 when both are empty |
| `POST /creature/reset` | `{seed?, archetype?, attribute?}` | a fresh creature; also installs and activates it |

The plugin emits one event, `creature`, with `{"event": "ready", "stage", "xp",
"line"}` when growth completes (once per stage — `announced_ready` guards it),
and speaks the line itself only when no brain is attached.

### Generated rigs and the picker

Every stage writes `models/buddy-<archetype>-<stage>[-<attribute>]-<8 hex>/`.
`models::is_generated_name` detects that pattern (prefix **and** an 8-lowercase-hex
final segment), and the UI filters on the `generated` flag — never
`GET /models`, because the MCP bridge and the card need the full list. Anything
with a saved rig override, plus the currently loaded model, is always kept.

### Invariants worth knowing before editing the generator

* **`symmetry_error` is never literally `0.0`** — the FK pass accumulates
  `sin`/`cos`, so a healthy rig reports ~1e-15 and `validate` accepts `< 1e-6`.
  Prove mirroring with the one-hot byte-equality test, not `== 0.0`.
* **The part budget is per tier**: 100 parts for Fresh…Champion, 150 for
  Ultimate/Mega. Callers that know the tier must use `validate_at(manifest,
  tier)`.
* **Every decorative part id must be in `DECORATION`**, or a rich Mega can tip
  over `MAX_PARTS` because the trimmer cannot see it.
* **The centreline is unmirrored and vertical** (root/pelvis/torso/head/tail at
  world `x = 0`), so a torso or neck *tilt* is not available — rotating a bone
  about its own origin is the only safe axis.
* **Proportion bands are a product** (`base × form × build × element`), so size
  a base with the worst-case product in mind.
* **Accessory parts are emitted onto the torso bone** and sort at its z, so a
  part centred on the spine paints *under* the head. Make new furniture clear
  the skull silhouette sideways.

---

## 8. Keeping the docs honest

`python tools/verify_docs.py` collects routes from `api.rs`, from every
`PluginRoute::get|post|get_post|new` literal under `src-tauri/src/plugins/`
(skipping `tests.rs`), and from every `("/path", &[…])` entry in a guest crate's
`ROUTES` table. Those route sets must appear in `API.md` **and**
`ARCHITECTURE.md`, and neither doc may mention a route that does not exist.

So the sequence for a new plugin endpoint is: add it to `ROUTES`, handle it, then
document it in both files — in that order, because the check reads the source.
