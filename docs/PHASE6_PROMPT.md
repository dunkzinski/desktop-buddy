# Phase 6 — closeout prompt (paste into a fresh context)

> **Status: shipped.** All six charter items are closed: `65070af` (one attach rule for
> spawn and update), `f57a5d7` (`accepts` declared advisory), `2f7fd3c` (the pod's idle
> auto-dismiss), `ad5fd93` (the generator writes no sockets, on purpose), `107bfd0` (no
> MCP entity tools) and `b4841e2` (the 3-deep chain measured at 0.033 ms). What shipped,
> what was decided and the evidence for each are in
> `docs/ENTITY_ITEMS_DESIGN.md` §14.5.1–14.5.6. Kept **verbatim** as the record of the
> brief — it is not a work plan any more, and re-running it would re-open decisions that
> section settles.
>
> Two corrections for anyone reading this as history: the tree was **not** clean when it
> was pasted (items 1 and 2 were already half-done and uncommitted, and the two prompt
> files had been deleted in the index), and the "~1 ms" figure was pessimistic by about
> 30×.

You're picking up the **closeout of §4's Phases 5–6** in `C:\hermes\desktop-buddy` — a
Tauri 2 desktop avatar (the gremlin) with an axum API and Wasm guest plugins, Rust
backend + vanilla-JS canvas frontend, no frameworks.

**Numbering, because the doc has two Phase 5s.** `docs/ENTITY_ITEMS_DESIGN.md` §4
plans Phase 5 = "mining-ready hardening" and Phase 6 = "docs + MCP". The arc in §13/§14
(post-plan: flip, nesting, fitting) is *also* called Phase 5 and is finished. This
prompt is the audit's leftovers from §14.4 — three real items, two corrections, one
decision — not a new feature arc.

## Read first, in this order

1. `skill_view(name='desktop-buddy-project')` — build/run/test posture and the traps
   already paid for (`ui/` is compile-time embedded; the page draws from
   `entity-updated`; a guest reply without `ok` reads as a refusal). Then
   `references/entities-and-props.md` for the prop/attachment rules.
2. `docs/ENTITY_ITEMS_DESIGN.md` **§14.4** (the audit — table of what landed vs not,
   with evidence), then §4 rows 4–6 (what was planned) and §2.4/§2.8 items 5 (what was
   designed differently).
3. `git log --oneline -8` — `ee74833`, `560465c`, `27ecca5`, `3b32688`, `c78d857`.
   Bodies carry the measured numbers; they are the fastest account of current behaviour.

## State you're inheriting

- Working tree clean at `ee74833`; the flip/nesting/fitting arc is complete.
- Green: 270 lib + 12 + 138 bin, 2 contract, 224 survival, 69 metamorphosis, 12 page
  tests (`node --test tools/entity-transform.test.mjs`), `python tools/verify_docs.py`.
- API `127.0.0.1:8765`, CDP `9222`; the app is **down**. Build/run recipe is in the skill.

## The charter, in this order (cheapest and highest-value first)

**1. One attach rule for both routes.**
- *Exists today:* `store.rs::check_attach_target` (:778) is called from `update` only
  (`store.rs:555`). `spawn` never calls it, so `POST /entities/spawn` with
  `attach.parent: "entity:<its own id>"` answers **200** and with a missing parent also
  **200**, while `update` answers 400 (`an entity cannot attach to itself` /
  `attachment parent "x" does not exist`). Verified live, 2026-11-24.
- *Risk:* every internal spawn with an `entity:` parent. `chest.rs::summon` (:213)
  spawns the chest **before** `seed_loot` (:110), so that path is safe; `find`/`take`
  spawn into `world`; the nested tests at `store.rs:1186-1246` spawn parents first.
  Re-check all of them, then flip the rule.
- *Acceptance:* Rust tests — spawn self-attach refused, spawn to a missing parent
  refused, spawn to an existing parent unchanged, a cycle refused on whichever route
  closes it. Live: the four curls (spawn self → 400, spawn missing → 400, spawn valid →
  200, update self → 400). Docs: `API.md` + `PLUGINS.md` (both currently document the
  asymmetry as fact), §14.2 #4 and §14.4's Phase 5 row flip to "unified".

**2. Decide `accepts`, and stop overclaiming either way.**
- *Exists today:* `Socket.accepts` is parsed (`sockets.rs:104`, `:742`), shipped to the
  page (`main.js:1681`, `__avatarDebug.sockets()`), and documented as
  "what may attach, matched against a prop's tags" (`MODEL_FORMAT.md:175`) — and
  **nothing matches it**. A prop's `tags` never meet a socket's `accepts`.
- *Why it is not a one-liner:* `Attachment::validate` is shape-only **by design** and
  cannot see any socket table. For an `entity:` parent the table is in the store's rig
  cache (checkable); for the avatar it lives in the loaded model, which the store does
  not own — so enforcing at store level recreates exactly the asymmetry item 1 removes.
- *Recommended default:* declare it **advisory** (doc comment + `MODEL_FORMAT.md` +
  the `defs.rs` comment), add a test that it survives the socket round-trip, and defer
  enforcement until something owns both tables. If you find a layer that owns both, do
  that instead and prove it with a refusal test.
- *Acceptance:* either the refusal test, or docs that no longer claim a check; state
  which you chose and why in the commit.

**3. `dismiss_after_ms`: ship it or delete it.**
- *Exists today:* only in the design doc (§2.3, §3, §4). Nothing in the code; the pod
  stays until `POST /chest/dismiss`, the card's **Fold**, or a caller.
- *Recommended:* ship as a **const**, not a config key (a key costs the
  `watcher_rules.json` known-set, `CONFIGURATION.md` and a settings row for a behaviour
  nobody has configured yet), and drive it from the existing once-a-second sweeper
  (`api.rs::housekeeping`, :1269, which already reaps `expires_at` debris, §12.1).
  Rules: only an **open** pod, only after the last interaction (put / take / open),
  never mid-`put`, and `dormant` is dismissed exactly once.
- *Acceptance:* chest tests (open + idle past the deadline → `dormant` and one
  `chest-dismissed`; an interaction resets the clock; closed or occupied is untouched),
  plus live: open the pod, wait, watch the state and the frame.

**4. The metamorphosis generator writing `sockets` — probably a bad idea; decide with evidence.**
- *Exists today:* `plugins/metamorphosis/src/generator.rs` emits no `sockets`; those
  rigs get them from the `sockets.rs` heuristics.
- *Why the plan may be wrong:* a generator can only emit `{id, bone, pos}`; §9's grip
  bug proved a socket at a bone's **origin** sits at the wrong end of a limb, and the
  heuristics derive the grip from the artwork. Writing sockets blind would be a
  regression dressed as compliance.
- *Recommended:* do not write them; if you want the plan's intent, have the **host**
  return the resolved socket table from `host_model_write` so a guest can *inspect* it.
  If you do author them, the existing sweep test (hand grip lands on the artwork, never
  past the end) must still pass for every shipped rig.
- *Acceptance:* a doc correction citing §9, or the sweep green with authored sockets.

**5. MCP entity tools — only on request.**
- *Exists today:* `desktop-buddy-mcp` exposes 14 `avatar_*` tools and
  `verify_docs.py` asserts exactly 14 tools and 14 mapped routes (:259, :297).
- *Recommended:* skip unless the user asks for agent-driven props. If you add one, bump
  **both** assertions and document the tool in the same commit — the count is the gate
  that keeps this a decision instead of drift.

**6. The "~1 ms for a 3-deep chain" budget: measure it or drop the claim.**
- *Exists today:* no timing test anywhere in `sockets.rs` or `entities/`.
- *Recommended:* drop the number (a wall-clock assertion in CI is flaky), or make it a
  generous complexity test (100 resolutions of a 3-deep chain under 50 ms). Keep the
  real result on record: a 3-deep chain resolved live as trinket `1944.5, 530.5` → fish
  `1956.7, 546.3` → rod `1827.6, 635.3`, all at scale 0.562.

## Do not rebuild (this is where the budget goes to die)

- `attach.flip`, nested sockets, per-rig fitting — shipped in `c78d857`, `3b32688`,
  `27ecca5` and proven (§14.1–14.3). Re-proving them teaches nothing.
- `item_id` and the transactional put↔bag (§2.4A) — the chest and the bag never name
  the same item; §14.4 calls that vacuous, not missing. Do not add it without a feature
  that needs it.
- The 5–8 slot capacity — removed on purpose (§12). The pod has no bottom; do not
  "restore" slots.
- `avatar.activity` — two walk objects + `avatar.manualMoves` ship and work; only a
  *third* activity would test the plan's idea.

## Working rules (non-negotiable — this project's standing conventions)

- **One scoped commit per item**, committed before the next begins: conventional-commit
  subject, then a body that says *why*, with measured before/after facts — not a file list.
- **Proof = automated test + live probe + your own vision on that screenshot** when the
  item is visible. Read the *drawn* state, not just the API's `ok`.
- `python tools/verify_docs.py` stays green; update `API.md` / `PLUGINS.md` /
  `MODEL_FORMAT.md` / the design doc in the *same* commit, and record discoveries as
  numbered subsections (§14.5 if you need one).
- `ui/` is compile-time embedded: `cargo build --bins` then restart — a CDP reload
  serves stale modules.
- `POST /shutdown` before building or running the contract test.
- If a skill instruction turns out wrong or missing, patch the skill before finishing.

## Deliver

Item by item: what shipped (or what you decided and why), the evidence (test names,
probe output, numbers), and anything left open. **If you disagree with this charter once
you have read the code, say so and propose the alternative** — items 2, 4 and 6 are
explicitly decisions, and the previous context's recommendation is the likeliest thing
in this file to be wrong.
