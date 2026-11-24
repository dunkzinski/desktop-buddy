# Phase 5 — kickoff prompt (paste into a fresh context)

> **Status: shipped.** All three items landed in `c78d857` (flip), `3b32688` (the
> catch on a nested socket) and `27ecca5` (per-rig fitting); `5b111ca` closed the
> design doc's open list. What shipped, and the numbers it was judged by, is in
> `docs/ENTITY_ITEMS_DESIGN.md` §14.1–14.3. This prompt is kept **verbatim** as the
> record of the brief — it is not a work plan any more, and re-running it would
> rebuild what exists.

You're picking up **Phase 5** of the procedural-props work in `C:\hermes\desktop-buddy` — a Tauri desktop avatar (gremlin) with a Wasm plugin host, Rust backend + vanilla-JS canvas frontend, no frameworks.

## Read first, in this order

1. `skill_view(name='desktop-buddy-project')` — the working skill: build/run/test posture, plus the traps we already paid for (`ui/` is compile-time embedded so a CDP reload lies to you; the page draws from `entity-updated` and ignores bespoke events; a guest reply without `ok` reads as a refusal; session props stow, they don't respawn). Follow its references: `references/entities-and-props.md`.
2. `docs/ENTITY_ITEMS_DESIGN.md` §8 (the attach model and its known limits), §12 (everything the pod became), §13 (**where Phase 4 ended and what Phase 5 is**).
3. `git log --oneline -12`. Every commit message explains a decision and carries measured numbers; they're the fastest account of current behaviour.

## State you're inheriting

- Working tree clean at the tip of `main` (Phase 4's close-out is the commit that added this file). Phase 4 is code-complete (pod, rummage, withdraw, tumbles, gestures, loot bag, lid).
- Green: 269 lib + 12 + 138 bin, 2 contract, 222 survival, `python tools/verify_docs.py`.
- Build & run recipe is in the skill. API `127.0.0.1:8765`, CDP `9222`.

## The charter: three items, in this order

**1. Render `attach.flip`.** It's stored and validated (`store.rs:55`) on every entity, and **nothing reads it** — a prop mirrored into the off hand draws unmirrored. The page resolves attachments in `ui/js/entities.js::resolveRoot`; the flip belongs in the composed transform there (mirror across the socket's local axis, not the screen's), and it must not disturb `rotation`, `scale` or the nudge offset. Acceptance: two identical props on the same socket, one flipped, visibly mirrored — proven with a screenshot plus the drawn pose.

**2. Nested sockets — a socket on a *prop*, not on the avatar.** What already exists: every prop def declares `own_sockets` (`defs.rs`), several generators emit real sockets (`fishing_rod` → `grip_point`/`line_end`, the pod → `chest_interior`/`chest_lid`/`chest_side`, anvil → `anvil_horn`, magnifier → `lens`), the host accepts `parent: "entity:<id>"`, and `resolveRoot` walks `entity:` parents with a depth limit and a graceful `null` on a dangling socket.

**Prove that first before building on it** — my last check could not settle it: I spawned `{"def":"trinket","attach":{"parent":"entity:survival:rod","socket":"line_end"}}` while the rod was in his paw, and the screenshot was ambiguous about whether the gem drew at the tip. Use a prop with an unmistakable silhouette (anvil on `line_end`) and confirm by pixels, not by the API replying ok. If it does draw, the item is: make nesting *usable* (a fish that hangs on the line when landed, an item that can go into the pod's `chest_interior` through the generic path rather than the chest's bespoke scatter) and stop the two socket vocabularies (`defs::own_sockets` vs the generator's `sockets`) from drifting — a test that every declared socket exists in the generated rig is the cheap version.

**3. Per-rig prop fitting.** Prop rigs are authored in rig units and read right on a generated gremlin (root scale 0.55) but small on `xenomorph-7` (scale 1.0, ~700 px tall). Decide whether fit is inherited from the host bone's world scale or authored per prop, state the rule in the doc, and make `attach.scale` mean one thing.

## Working rules (non-negotiable, they're this project's conventions)

- **One scoped commit per item.** Commit before starting the next. Conventional-commit subject, then a body that says *why*, with the measured before/after facts (numbers, timings, state traces) — not a list of files.
- **Proof is automated test + live CDP probe + screenshot + your own vision on that screenshot.** Never claim a visual change without looking at the pixels. The probe recipe is in the skill (`__avatarDebug.entities()` returns `{entities: [...]}`, each entry with `clip`/`state`/`pose` — read *drawn* pose, not just state).
- `python tools/verify_docs.py` stays green; update `API.md` / `PLUGINS.md` / `ARCHITECTURE.md` / `docs/ENTITY_ITEMS_DESIGN.md` when behaviour changes, and record what you discovered in the design doc as a numbered subsection (§12.x is where that pattern lives).
- UI lives in `ui/` and is embedded at compile time: `cargo build`, restart the exe. A CDP `Page.reload` will show you stale modules and cost you an hour.
- Shut the app (`POST /shutdown`) before `cargo test --test chat_and_cursor_contract`.
- If a skill instruction turns out wrong or missing, patch the skill before you finish.

## Deliver

Item by item: what shipped, the evidence (test names, probe output, screenshot paths, measured numbers), and anything you deliberately left open with the reason. If you disagree with this charter after reading the code — say so and propose the alternative instead of building the wrong thing.
