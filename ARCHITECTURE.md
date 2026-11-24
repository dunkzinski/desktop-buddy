# ARCHITECTURE — desktop-buddy desktop avatar

Technical documentation for the AI desktop avatar in `C:\hermes\desktop-buddy`.
Everything here was read from the current sources; file names, function names,
constants and payloads are quoted from the code, not from memory.

Last verified against: `src-tauri/src/{lib,api,models}.rs`, `ui/index.html`,
`src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `models/*/model.json`,
`src-tauri/src/bin/{desktop-buddy-mcp,desktop-buddy-watcher}.rs`,
`src-tauri/src/bin/capture/mod.rs`, `src-tauri/src/platform/*.rs`.

---

## 1. Project overview

A 2D character that lives on top of the Windows desktop: a transparent,
always-on-top, full-screen WebView2 window that draws a rigged sprite/shape
avatar and lets it walk around, emote, blink, talk and show speech bubbles.
A local HTTP API lets an external agent (LLM, script, automation) drive it.

### Feature summary

| Area | What exists |
| --- | --- |
| **Desktop windowing** | Transparent, borderless, always-on-top overlay covering the **whole virtual desktop** (every monitor); selective click-through so the desktop stays usable; custom titlebar with click-through toggle, minimize and close |
| **UI controls** | Avatar control panel (state/mood readout, a live **Blend** strip showing the creature's recency-weighted attribute mix from `GET /creature/status`, Wave / Walk / Center, New chat, History, Ask…, walk-speed slider, live position, API URL — no manual expression buttons, expressions are commanded by the AI and by watcher acks), model dropdown, Import…, **chat** and **Settings (gear)** icons in the titlebar, a **Watcher** row: on/off switch, `running`/`off`/`missing` badge and a **Logs ↗** pop-out console, a **Voice** row: the Piper mute switch with an `on`/`muted`/`missing` badge, and a **Push to talk** row: the hold-to-talk button with an `idle`/`listening`/`transcribing`/`missing` badge |
| **Ask / task from the desktop** | Floating command bar under the avatar — `Enter`/`/`, the titlebar chat icon, the **Ask…** button or a click on the bubble opens it, and a **system-wide hotkey** (`Alt+Space` by default, `hotkey` in `watcher_rules.json`) summons it from any application; type + `Enter` sends a `user_reply`, `Esc` closes it. Quick-task chips come from `quick_tasks` in `watcher_rules.json`, and **↺ / New chat** drops the conversation context (`POST /session/reset`) |
| **Model importing** | Rig manifests as `.json` or `.toml`; single images (`.png`, `.jpg`/`.jpeg`, `.webp`, `.svg`) as a **new character** or as a **part attached to the current rig**; native file picker; clear validation errors |
| **Rig Edit Mode** | Bone/joint/pivot/hitbox overlay; drag joints, drag rotation ring (unlimited multi-turn), wheel rotate / shift+wheel scale; re-parent; add bone, add limb, add part, duplicate/delete; inspector for transforms, limits, z-order and animation channels; undo; save to `rig.json`; reset |
| **Facial system** | Per-part sprite **variants**, 6 expression presets (`neutral`, `happy`, `surprised`, `thinking`, `sad`, `angry`) mapping to eye/mouth variant names, procedural squash/stretch fallback, expression holds with auto-revert. **Nothing on the panel commands a face**: expressions are chosen by the AI (`POST /emote`, `speak`, the watcher's contract and vision prompts all ask for one that matches the tone/outcome), by watcher acks (an `alert`'s severity picks `sad`/`surprised`, per-app rules and survival frames pick their own), and by page-side events (PTT `transcribing` → `thinking`, fishing caught/lost → `happy`/`sad`, metamorphosis `metamorphosed`/`ready` → `happy`/`surprised`, send failure / model timeout / AI endpoint offline / watcher crash → `sad`/`surprised`). A face with no explicit hold settles back to `neutral` after `EXPRESSION_SETTLE_SECS` once no bubble, thinking card, walk or recording is in progress |
| **Custom animations** | Optional `animations` block on the rig root: named keyframe clips (`duration_ms`, `loop`, `keyframes[]`) whose per-bone deltas are sampled with smoothstep easing and composed into the transform pass; timeline bar in Rig Edit Mode (clip CRUD, duration, loop, play/stop, 0–100 % scrubber with keyframe ticks, `+ Keyframe` capture) |
| **Life signs** | Auto-blink every 3–6 s (~120 ms); **text-driven lip sync** — the mouth follows the typewriter character by character (vowels wide open, soft consonants mid-open, spaces and punctuation closed) for both sprite variants and the procedural shape; **global eye tracking** — the `avatar-cursor` poll publishes a deadbanded `cursor-pos` event (3 CSS px, ~20 Hz), so the pupils follow the **OS cursor anywhere on the virtual desktop**, even while the overlay is click-through and unfocused. Gaze priority: explicit `look_at` (`POST /move`) > focused command bar > perch rectangle > global cursor (for 4 s after it last moved) > idle glance every 2–4 s; pupils are clamped to ±4 px so they never leave the socket. The same eased gaze drives a **head-bone yaw bias** (≤ 0.22 rad, ~13°) that is damped and clamped by the bone's `limits`; **hover response** — an extra soft head tilt when the cursor enters the avatar's bounds |
| **Speech bubbles** | Head-anchored DOM card, re-anchored to the head **every animation frame** (no lag while the avatar walks), typewriter reveal with caret, word wrap, auto-size, edge clamping / flip, auto-dismiss; the avatar stands still while a bubble is up (automatic wandering is paused — an explicit `/move` or keypress still wins); an optional **quick-reply box** (click the bubble, or just hover) with `Enter` to send and `Esc` to dismiss; an optional **details drawer** — a `details` string renders a monospace, scrollable pre block behind a **Details ▾** toggle plus a **Copy** button, and an open drawer holds the bubble up |
| **Conversation history** | The session's turns are kept **in memory** by `AppCtx::push_chat` — a bounded `VecDeque<ChatMessage>` (capacity 50, oldest evicted) recording `{id, role, text, details?, expression?, ts_ms}`. `"user"` turns are recorded in `do_user_reply_with_profile`, `"assistant"` turns in `do_speak` (with their `details` and expression), and `do_session_reset` clears the lot. A **History** drawer (panel **History** button, command-bar **☰**, or `H`) renders the transcript with per-turn timestamps, expandable monospace `details` blocks and **Copy**; it hydrates through `get_chat_history`, appends live from `chat-message`, empties on `chat-history-cleared`, and stays clickable over a click-through desktop by publishing its own hit region while open. `GET /history` exposes the same log to agents. Nothing is persisted — a restart starts a new chat |
| **Local voice** | Every spoken bubble is also **read out loud** by a local **Piper** voice — no network, no API key. `do_speak` is the single hook, so it covers `POST /speak`, the `speak` command, MCP `avatar_speak` and every agent reply, whichever model (`lfm3-fast`, `hermes_cli`, …) produced the words. Text is sanitized first (markdown, fenced code, links, emoji and `<tag>` control markers are not spoken) and an empty result is skipped; `piper.exe` runs with `CREATE_NO_WINDOW` on the FIFO `avatar-tts` worker thread, which synthesizes one temp `.wav`, plays it through **`rodio`** (WASAPI / ALSA / CoreAudio — no OS audio binding in this crate) and deletes it — lines queue instead of overlapping, and the HTTP server never blocks. Configured by the `tts` block of `watcher_rules.json`, with a **Voice** mute switch in the control panel, `GET`/`POST /tts` and the `get_tts_status` / `set_tts_enabled` commands |
| **Push-to-talk voice input** | Hold **Alt+V** anywhere (or the panel's **Hold to talk** button), speak, let go: the mic is captured through **`cpal`** (WASAPI / ALSA / CoreAudio), downmixed to mono and resampled to **16 kHz PCM**, written to a temp `.wav`, and transcribed locally by `whisper-cli.exe` with its built-in **Silero VAD** (`--vad --vad-model --vad-threshold`). The cleaned transcript (no `[BLANK_AUDIO]`, `(music)`, timestamps or control chars) is posted to `POST /reply`, so it wakes the agent exactly like a typed message and the answer is spoken by the Piper voice. One `avatar-ptt` thread owns the whole state machine (`idle` → `listening` → `transcribing`); holding the button cuts the avatar's own voice off first so the mic never transcribes it. Configured by the `stt` block, with `GET`/`POST /stt` and a badge in the panel |
| **Screen vision** | **"What's on screen?"** in the command bar (or the panel's screen button, or simply asking *what's on my screen?*) makes the watcher screenshot the focused window and answer with a **local vision-language model** — no network, no API key. `GET /context` now carries the foreground window's `window_rect`, and `desktop-buddy-watcher` grabs exactly that rectangle with GDI (`GetDC` + `BitBlt` + `GetDIBits`), falls back to the full primary monitor when the rect is missing, zero-sized or the overlay itself is focused, downscales the longest edge to `max_image_dim`, encodes JPEG (`jpeg_quality`) and inlines it as a `data:image/jpeg;base64,…` URL in one OpenAI-shaped `chat/completions` request against the named `vision` AI profile. A failed capture or an unreachable endpoint is one spoken apology, never a crashed or blocked reply. Configured by the `vision` block of `watcher_rules.json` |
| **Audio-driven lip sync** | The Piper clip that is about to play is read back as **16-bit PCM**, its RMS is bucketed at **25 ms** and normalized, and the page indexes that envelope by playback time — so the mouth follows the actual voice instead of the typing speed. `tts-envelope` carries `{bucket_ms, samples}` and is emitted right before playback starts; `tts-envelope-stop` ends it. A muted, missing or already-finished voice silently falls back to the per-character mapping, so a rig without audio still lip-syncs |
| **Live window tether** | `POST /perch` remembers the target's `HWND`, edge and offset, and a 60 ms poll re-reads its DWM frame bounds: the avatar walks along with the window as it is dragged or resized, and drops back to `idle` (clearing `perched`/`perch_rect`) when the window is minimized or closed. A drag of the avatar itself, or any `/move`, breaks the perch immediately |
| **Multi-monitor overlay** | The overlay window is positioned and sized to the **whole virtual desktop** (`SM_XVIRTUALSCREEN` …), and every screen-space value — cursor, window rects, `visible_windows`, the watcher's GDI capture — is expressed relative to that virtual origin, so a monitor to the left of (or above) the primary has positive CSS coordinates instead of negatives. `POST /perch` clamps to the monitor the target window is on |
| **Agent API** | 38 local HTTP paths (46 method routes): state, movement, expressions, speech, **conversation history**, models, rigs, parts, custom animations, desktop context, clipboard read **and write**, quick replies, external alerts, session reset, watcher control, **AI profile switching + health**, shutdown |
| **Desktop awareness** | `GET /context` — foreground window title, its owning executable (`process_name`), the focused window's `window_rect` plus the top few `visible_windows` (all converted to CSS px), machine-wide idle seconds (Win32 probes, safe defaults elsewhere); `POST /perch` — stand the avatar on or beside a window border and keep its gaze there **and** the last 15 SSE frames as the agent's short-term memory; `GET /clipboard` — the last-copied text, capped at 2000 characters; `POST /clipboard` — put up to 32 000 characters on the clipboard |
| **Event stream** | `GET /events` — Server-Sent Events over a 64-frame `tokio::broadcast` channel: `state`, `speech`, `speech_cleared`, `model`, `rig_saved`, `pose`, `part_patch`, `animate`, `interaction`, `user_reply`, `alert`, `session_reset`, `ai_profile`, `watcher`; 15 s keep-alive, plus a 15-frame history ring buffer served inside `/context` |
| **Interactions** | Play-mode click / double-click / drag on the avatar's bounds → `notify_interaction` → `interaction` frame; a typed command-bar message → `notify_reply` → `user_reply` frame |
| **Sessions** | The watcher owns a mutable `{session}` id (`desktop-buddy-<seed>`, `-2`, `-3`… after each rotation) substituted into `hermes_cmd`; **New chat** / `POST /session/reset` broadcasts `session_reset` and rotates it; `max_session_turns` / `session_idle_reset_secs` rotate automatically |
| **MCP bridge** | `desktop-buddy-mcp.exe` — a second binary exposing 14 tools (`avatar_speak`, `avatar_emote`, `avatar_move`, `avatar_perch`, `avatar_wave`, `avatar_pose`, `avatar_get_context`, `avatar_get_clipboard`, `avatar_set_clipboard`, `avatar_list_models`, `avatar_switch_model`, `avatar_list_animations`, `avatar_animate`, `avatar_switch_ai`) to any MCP client over stdio JSON-RPC 2.0; stateless, forwards each call to the HTTP API and reports a down host as a readable tool error |
| **Desktop watcher** | `desktop-buddy-watcher.exe` — a third binary that subscribes to `GET /events` and polls `GET /context` and only acts when a rule fires: an `interaction` (`double_click`/`drag_end`), a `window_title` change matching a keyword **or a per-app rule**, an `idle_return` after a long idle stretch, a `user_reply` from the command bar (no cooldown — the user asked directly), or an `alert` pushed by `POST /alert` under its own cooldown. Per-trigger cooldowns, an immediate ack (expression or custom clip), an optional webhook, and an optional spawned Hermes command with a one-line prompt on a persistent, rotatable session (`{session}`). Spawned and supervised by the app itself, with an on/off switch (`GET`/`POST /watcher`, the panel toggle), a 200-line piped log tail in the pop-out console, and crash restart. Dispatch is single-flight with a **one-slot pending queue**: a `user_reply`/`alert` arriving mid-turn waits and runs as soon as that turn ends (newest wins), while routine triggers are dropped. Exponential-backoff reconnect; **zero LLM tokens while idle** |
| **Persistence** | `models/<name>/model.json` (shipped), `rig.json` (editor override), `models/.state.json` (selected model) |

### Repository map

| Path | Status | Notes |
| --- | --- | --- |
| `src-tauri/` | **live** | The application (Rust + Tauri 2) |
| `ui/` | **live** | Modular frontend (see structure below) |
| `models/` | **live** | Model folders + `models/.state.json` |
| [`API.md`](API.md), [`MODEL_FORMAT.md`](MODEL_FORMAT.md), [`README.md`](README.md) | **live docs** | Endpoint reference, manifest guide, quick start (this file is the design overview) |
| `watcher_rules.json` | **live config** | Optional rules for `desktop-buddy-watcher.exe` (interaction kinds, title keywords, **per-app `app_rules`**, alert cooldown, idle threshold, cooldowns, ack emote, Hermes command/session, hotkey, webhook); built-in defaults when absent |
| `docs/`, `tools/`, `TASKS.json` | **live** | Screenshots, `verify_docs.py` / `mcp_smoke_test.py` / `cdp-eval.mjs`, and the stage ledger |
| `.github/workflows/` | **live** | One `check.yml` job on `windows-latest`: `cargo test --lib --bins` (lib, MCP and watcher targets), then both plugin crate suites via `--manifest-path`, then `verify_docs.py`. Each plugin is its own workspace, so the app's cargo never reaches them; `plugins/*.wasm` are gitignored, so the shipped-plugin test is guarded on presence |

The legacy prototypes that used to sit in this tree — `ai-avatar-bridge/`,
`avatar-desktop/`, `avatar-editor/`, `spikes/`, `src-tauri/avatar-renderer.html`
and the empty `src/desktop_buddy/`, `tests/`, `assets/` placeholders — have been
**deleted**. Nothing outside `src-tauri`, `ui`, `models`, `docs`, `tools` is
part of the app, and no source file references them.

### `ui/` — modular frontend structure (no build step)

```
ui/
├── index.html          # Lean semantic HTML shell (titlebar, canvas, panels, bubble, toasts)
├── styles.css          # All CSS rules (titlebar, control panel, inspector, import modal, speech bubble, toasts)
└── js/
    ├── state.js        # Shared state objects (`avatar`, `RT`, `animation`, `gaze`, `speech`), constants (`EXPRESSION_PRESETS`, `EXPRESSIONS`, `PRESET_STATES`, `PART_TAGS`), the lip-sync mouth mapping helper, undo history helpers
    ├── rig.js          # Model ingestion (`applyModel`, `boneDefaults`, `rebuildOrder`, `loadImages`), skeletal FK (`animOffset` — which also adds the hover head tilt, `clampRotation`, `animationOffset`, `localRotation`, `computeWorld`), rig serialization (`serializeRig`, `serializeAnimations`)
    ├── animator.js     # Custom keyframe clips: `playAnimation`, `stopAnimation`, `sampleAnimation`, `seekAnimation`, `stepAnimation`, `captureKeyframe`, clip CRUD, finished listeners
    ├── renderer.js     # Canvas rendering pipeline (`drawFace`, `drawShape`, `drawPart`, `render`), the gaze→pupil offset (`updateGazeDir`, `isPupilPart`), and the lip-sync mouth squash
    ├── editor.js       # Rig Edit Mode overlay gizmos (`drawEditorOverlay`, `hitTest`, mouse/wheel handlers), inspector rendering/bindings (`buildInspector`, `renderInspector`, `updateInspectorValues`), bone/part authoring (`addChildBone`, `deleteBone`, `addPartFromControls`, `addLimbFromControls`, `duplicatePart`, `deletePart`, `bindExpressionSprite`, `saveRig`, `resetRig`), and the animation timeline (`buildTimeline`, `updateTimeline`, `syncTimelinePlayhead`)
    ├── speech.js       # Speech bubble lifecycle (`speak`, `updateSpeech`, `positionBubble`, `hideSpeech`), text-driven lip sync (`mouthPoseFor`, `setMouthFrom`, `stepMouth`), the "Thinking…" card and the command bar's anchor (`positionCommandBar`)
    ├── bridge.js       # Tauri IPC commands, event listeners (`avatar-state-changed`, `emote`, `speak`, `avatar-animate`, etc.), hit-region publishing (`wireRegions`, `setClickThrough`), state sync (`reportState`, `applyRemoteState`, `applyPartPatch`)
    ├── drag.js         # Floating-panel drag + layout: header-only pointer drag, viewport clamping (multi-monitor overlay), sibling z-order, `localStorage` persistence (`desktop-buddy.panel-layout.v1`), `setupPanelDrag` / `resetPanelLayout`; a drag sets `body.panel-dragging` so `rig.js` publishes a full-viewport region for the gesture
    ├── entities.js     # Prop entities on the canvas: `fetchEntities` / `markDirty` (2 s reconcile + event-driven refetch), `entityDrawables` (z-sorted, per-socket parents, `world` parent for stage-anchored props), `stepEntities`, `clip_for_state` mirroring, the chatty chest API (`chestStatus`, `summonChest`, `openChest`, `closeChest`, `putInChest`, `takeFromChest`, `dismissChest` — refusals thrown with the host's own words), and `chestBounds` / `overChest` for the pod's click region
    ├── chest-card.js   # The **Curiosity pod** card: `render` (state pill, slot bar, one row per item with a Take button), `openChestCard` / `closeChestCard` / `toggleChestCard`, `refreshChestCard` (called by the entity-event listeners, so the card tracks pods moved by plugins or agents); a 2 s poll only while it is open
    ├── behaviour-card.js # The **Brain & body** card: watcher, AI brain, voice, push-to-talk and walk speed. It owns the card only — every control kept its element id when it moved out of the control panel, so the code that drives each behaviour did not move
    ├── gestures.js     # **Procedural gestures**: bone offsets over time, no clips (`GESTURES`, `gestureOffset`, `gestureDuration`). `pack-up` and `reach` today; `rig.js` layers the running one on top of the pose, `main.js` drives the clock
    ├── survival.js     # The **Survival** pop-out card and its four tabs: `setupSurvival` (hydrate from `GET /survival` + `GET /foods` + `GET /inventory` + `GET /store`, follow the `survival` / `survival-meal` / `survival-threshold` / `survival-craft` / `survival-craving` / `inventory-changed` / `store-transaction` / `fishing-*` events, 15 s survival poll floor), `renderSurvival` (tier badge, the two bars and the craving row, from any of the payload shapes), `feed` (the real `POST /survival/feed`), `setCraving` / `clearCraving`, `craft` + `syncCraftButton` (the recipe picker's `can_cook`/`missing`, so a cook the bag cannot pay for is not clickable), `showSurvivalTab` (status / bag / fishing / store), `renderInventory` + `addItem` + `useItem` + `sellPickedItem` (`GET /inventory`, `POST /inventory/add`, `POST /inventory/use`, plus a one-unit `POST /store/sell` straight from the bag at the slot's `sell_price`), `syncInventoryButtons` (Use for edibles, Sell for anything the counter prices), the shop (`renderStore` + `renderWallet` + `pickStoreItem` + `syncStoreButtons` + `trade` over `GET /store`, `POST /store/buy`, `POST /store/sell`), the fishing mini-game (`castLine` / `hookFish` / `reelFish` / `cancelFishing`, a 120 ms `GET /fishing` poll, `startMarker` animating off the plugin's sweep), `toggleSurvivalCard` / `survivalCardOpen`
    ├── metamorphose.js    # The **Metamorphosis** pop-out card: `setupMetamorphose` (hydrate from
    │                  # `GET /creature/status`, follow the `creature` event, 10 s poll), `renderCreature`
    │                  # (stage badge, XP/bond bars, attribute chip, the live rig's bones/parts/top),
    │                  # `renderBlend` (`blendOf` / `summarise`: the lineage's percentage chips, stacked bar
    │                  # and blended traits), `renderBranches` + `pickEvolveBranch` (the three branches, each
    │                  # with the mix it would produce via `branchMix`), `evolve` (the real
    │                  # `POST /metamorphose` — the plugin writes the model and the
    │                  # host swaps the avatar, so nothing here touches the rig), `nudge` (`POST /creature/xp`),
    │                  # `reset` (`POST /creature/reset`), `toggleMetamorphoseCard` / `metamorphoseCardOpen`
    ├── plugins.js      # Plugin-backed UI gate: `setupPluginGate` (one `GET /plugins` read plus the
    │                  # `plugins` event), `applyPluginStatus`, `pluginActive`, `gatePluginUi` — reveals a
    │                  # `data-plugin="<id>"` control-panel button / Settings tab only while that plugin is `active`
    ├── settings.js     # The **Settings** modal over `GET /config` / `POST /config`: `setupSettings` (build the form from one `FIELDS` spec, hydrate from the server, prime at startup), `renderSettings` (values, per-path ✓/✗, stray-key warnings), `collectPatch` (sends **only** what changed, including `ai.create` / `ai.rename` / `ai.remove`, `discovery_ports` and `discovery_hosts`), the AI brain tab's own `renderProfileCards` / `refreshProfileOptions` / `addProfile` / `removeProfile` / `discoverModels` / `importModel` / `renderDiscovery`, `save` (partial `POST /config`, then reports applied vs `restart_required`), `toggleSettings` / `settingsOpen`
    └── main.js         # Entry point: DOM bindings, keyboard shortcuts, gaze targeting (`updateGaze`, `attachGazeTracking`), `window.__avatarDebug`, `requestAnimationFrame` main loop (`step`, `frame` — bubble re-anchor), the ask/task command bar (`openCommandBar`, `sendUserMessage`, quick-task chips, `resetSession`)
```

> **No Node/npm build step.** `src-tauri/tauri.conf.json` serves `../ui` directly into WebView2 (`frontendDist: ../ui`, `"withGlobalTauri": true`). Native ES modules (`<script type="module">`) load at runtime.

> The app is **only** `src-tauri` + `ui` + `models` (+ `docs`/`tools` for
> development). The legacy prototype trees are gone — see the repository map
> above.

### Pop-out cards, and who owns a click

Four cards float over the canvas: **survival** (top left), **metamorphosis**
(beside it), **curiosity pod** and **brain & body** (the right-hand shelf, below
the titlebar — it is opaque there and z-indexed above the cards, so a card at
`top: 14px` on the right loses its header and its close button).

Three rules hold them together:

* **A card is inert to clicks unless it is in the region list.** `wireRegions`
  publishes `[cards…, bubble, avatar, extra sources]`, and `wireRegions` runs
  again on every open/close and on every drag — a card that forgets the call is
  visible but unclickable on a click-through desktop, which reads as a freeze.
* **Moving a control keeps its id.** The behaviour card is the watcher, AI,
  voice, push-to-talk and walk-speed sections lifted straight out of the control
  panel with their markup unchanged; `main.js`, `settings.js` and `plugins.js`
  kept working because they bind by id and never by position.
* **A prop can publish its own region.** `addRegionSource(fn)` (in `rig.js`)
  lets `entities.js` contribute the pod's box without `rig.js` importing it —
  the dependency stays one-way. `main.js` then hit-tests the same box before it
  starts an avatar drag, so clicking the pod opens its card and clicking the
  gremlin still drags it.

---

## 2. System architecture & threading model

```
┌─────────────────────────────────────────────┐
│ WebView2 page  (ui/index.html, embedded)    │
│  • canvas render loop (requestAnimationFrame)│
│  • avatar behaviour: idle wander / walk /    │
│    wave / blink / talk / custom clips        │
│  • rig editor + inspector + speech bubble    │
│  • publishes hit regions, reports state 8 Hz │
│  • click/double-click/drag detection         │
└───────────────┬─────────────────────────────┘
                │  invoke(cmd, args)  ▲  events
                │  (36 Tauri commands)│  (11 event names)
┌───────────────▼─────────────────────┴───────┐
│ Tauri main process (Rust)                    │
│  AppCtx   — avatar state, hit regions,       │
│             speech slot, click-through flags, │
│             SSE broadcast channel (64)        │
│  ModelCtx — current model, rig cache, pose,  │
│             joint angles, part patches        │
│  cursor tracker thread (35 ms poll)          │
└───────────────┬─────────────────────────────┘
                │ Arc<AppCtx> / Arc<ModelCtx>
┌───────────────▼─────────────────────────────┐
│ API thread "avatar-api"                      │
│  private Tokio runtime (2 workers)           │
│  axum 0.7 — 33 routes on 127.0.0.1:8765      │
│  ↳ GET /events fans the broadcast out as SSE │
└─────────────────────────────────────────────┘
```

### Threads

| Thread | Created in | Loop / lifetime | Job |
| --- | --- | --- | --- |
| main (Tauri/wry) | Tauri | event loop for the app's lifetime | Window + WebView2 event pump |
| WebView2 renderer | WebView2 | page lifetime | All rendering/animation/UI logic in JS |
| `avatar-api` | `api::start` | until `oneshot` shutdown fires | Owns a 2-worker Tokio runtime running `axum::serve` |
| `avatar-api-rt` workers | Tokio | runtime lifetime | Request handlers |
| `avatar-cursor` | `spawn_cursor_tracker` | until `AppCtx::running` is false | Global cursor poll: publishes the deadbanded `cursor-pos` gaze event every ≥ 3 CSS px / ≥ 50 ms and toggles `WS_EX_TRANSPARENT` per hit region |
| `avatar-watcher-log` (x2) | `spawn_log_reader` | until the child closes stdout/stderr | Drain the watcher's pipes into the `AppCtx` ring buffer and echo each line to stderr |
| `avatar-watcher-supervisor` | `supervise_watcher` | until `AppCtx::running` is false | `try_wait`s the supervised `desktop-buddy-watcher.exe` every 250 ms and respawns it after a crash (2 s, doubling to 30 s); parks while the switch is off |
| `avatar-tts` | `tts::init` | process lifetime | The only caller of `piper.exe` and of the `rodio` output stream: drains a `SyncSender<String>` FIFO, synthesizes each line into `%TEMP%\desktop-buddy-tts-<pid>-<n>.wav` with `CREATE_NO_WINDOW`, plays it synchronously (`Sink::sleep_until_end`) and deletes it. One thread, so lines queue instead of cutting each other off |
| `avatar-ptt` | `stt::init` | process lifetime | Push-to-talk state machine: polls `platform::key_is_down` for the hold hotkey, drains the `cpal` input stream while held, resamples to 16 kHz mono, writes a temp `.wav`, runs `whisper-cli.exe` (with Silero VAD) and posts the cleaned transcript to `/reply`. One thread, so a hold and a transcription can never overlap |
| `avatar-hotkey` | `spawn_hotkey_listener` | until `AppCtx::running` is false | Registers the system-wide hotkey through `platform::HotkeyListener` (`RegisterHotKey` on Windows, the `global-hotkey` crate on Linux/macOS) and polls it every 25 ms; on a press it disables click-through, focuses the overlay and emits `summon-command-bar`, then releases the shortcut on the way out |

Nothing on the API side ever touches the GUI thread directly: handlers mutate
shared state and emit Tauri events; the page applies them on its next frame.

### Startup sequence (`lib.rs::run`)

1. `AppCtx::new(api_port())` — port from `DESKTOP_BUDDY_API_PORT`, else `8765`.
2. `ModelCtx::new()` — resolves `models_root()` and the saved selection.
3. Create a `tokio::sync::oneshot` pair; store the sender in `AppCtx`.
4. `tauri::Builder` → `manage(AppCtx)`, `manage(ModelCtx)`, register 44 commands.
5. `setup`:
   * Resize/move the window to the **virtual desktop** (`SM_X/Y/CX/CYVIRTUALSCREEN`, converted to physical px; the primary monitor only as a fallback when no virtual metrics exist).
   * `model_ctx.ensure_loaded()` (logs a warning and falls back if a model is broken).
   * `api::start(...)` → binds, spawns the API thread, stores the URL, emits `api-status`.
   * `start_watcher(...)` → resolves `desktop-buddy-watcher.exe` next to the running
     exe and spawns it with `--api-url <bound url>`, `--rules <found>`
     (`watcher_rules_path`: cwd → beside the binaries → parents up to
     `models_root()`'s parent), `stdin(Stdio::null())`, `stdout`/`stderr` piped
     into the log ring buffer, cwd = the exe's directory and
     `CREATE_NO_WINDOW`; skipped entirely when `DESKTOP_BUDDY_NO_WATCHER` is
     `1`/`true`. The child goes into `AppCtx::watcher` (switch + child + paths +
     log tail), reader threads start, and the supervisor takes over.
   * `spawn_cursor_tracker(...)`.
* `stt::SttConfig::load(ctx.ai_rules_path())` → `stt::init(config, handle, api_url)`:
  validates `whisper-cli.exe`, the Whisper model, the Silero VAD model and that
  a microphone exists (one `warn` + `ready: false` otherwise), starts the
  `avatar-ptt` thread and emits `stt-status`.
* `tts::TtsConfig::load(ctx.ai_rules_path())` → `tts::init(...)`: validates
  `piper.exe`, the `.onnx` model and the espeak-ng data directory **once** (a
  missing file is one `warn` line — the avatar stays silent, never crashes),
  starts the `avatar-tts` worker and emits `tts-status`.
6. `app.run(...)` → on `RunEvent::ExitRequested | Exit` call `AppCtx::request_shutdown()`
   (kills and reaps the supervised watcher, stops the tracker, fires the oneshot,
   graceful axum shutdown). The titlebar **X** and `POST /shutdown` take the same
   path, and the call is idempotent because all three can fire for one exit.

### State ownership and data flow

`AvatarState` (in `AppCtx.avatar`) is the API-visible truth:

```rust
pub struct AvatarState {
    pub position: (f64, f64),          // rendered as [x, y]
    pub target_position: (f64, f64),
    pub state: String,                 // idle | walking | waving
    pub expression: String,            // see AVATAR_EXPRESSIONS
    pub revision: u64,                 // bumped on every command/API mutation
    pub talking: bool,                 // reported by the page
}
```

Two directions, deliberately asymmetric:

* **Rust → page**: commands/API mutate state, then `publish()` bumps `revision`,
  emits `avatar-state-changed` with the whole struct, and (for expressions /
  speech) emits a dedicated event.
* **Page → Rust**: `report_state` is invoked at ~8 Hz (throttled to 120 ms) with
  position, target, mode, expression, joint angles, `talking` and bubble
  visibility. It **does not** re-emit the Tauri `avatar-state-changed` event (no
  feedback loop), but it *does* broadcast a `state` frame on the SSE bus when
  `state`/`expression`/`talking` actually changed — that is how page-driven
  transitions (`idle→walking`, mouth start/stop) reach agents. Those frames
  leave `revision` alone: it counts external instructions only.
* **Rust → agents**: every mutation also calls `AppCtx::emit_event(type, payload)`,
  which stamps `{seq, type, ts_ms, payload}` and sends it on the broadcast
  channel behind `GET /events`. Sending with no subscriber returns `0` instead of
  erroring, so the API is never required for the app to work.

Three guards keep this coherent:

| Guard | Where | Why |
| --- | --- | --- |
| `revision` | page `applyRemoteState` | the page only accepts a remote state newer than the last one it applied; its own position echo can't hijack a walk |
| `expression_commanded_at` (900 ms window) | `report_state` | a report that was already in flight when a command landed must not overwrite the fresh expression |
| `speech_seen` | `report_state` | "bubble not visible yet" reports that arrive **before** the `speak` event is handled must not clear the stored payload |

Pose overrides (`POST /pose`), part patches (`POST /part`) and model switching
live in `ModelCtx`; joint angles reported by the page live there too.

### Window transparency and hit-testing

* Config (`tauri.conf.json`): `transparent: true`, `decorations: false`,
  `shadow: false` (no window shadow frame, which on Windows 11 is what draws a
  faint outline along the screen edge), and `strip_dwm_frame` at setup clears
  `DWMWA_BORDER_COLOR` (34, `DWMWA_COLOR_NONE`) + `DWMWA_WINDOW_CORNER_PREFERENCE`
  (33, `DWMWCP_DONOTROUND`) so Windows 11 does not draw its 1 px frame and
  rounded corners either; `ui/styles.css` also zeroes `margin`/`padding`/
  `border`/`outline`/`overflow` on `html`, `body` and `canvas` so WebView2 cannot
  contribute an edge line or a focus ring of its own,
  `alwaysOnTop: true`, `skipTaskbar: true`, `focusable: true`,
  `"withGlobalTauri": true`. No `WS_EX_LAYERED` fiddling — wry handles it.
* The page publishes the rectangles that must stay interactive
  (`update_hit_regions`, refreshed every 200 ms): titlebar, control panel (or
  its launcher button), the avatar's computed bounding box, and — while the rig
  editor is open — the whole viewport plus the inspector. The import chooser and
  bubble are handled explicitly.
* `avatar-cursor` polls `GetCursorPos` every **35 ms**, converts screen→client
  with `outer_position()` and `scale_factor()`, hit-tests the regions and calls
  `window.set_ignore_cursor_events(!inside)`. A *global* poll is required: once
  the window is `WS_EX_TRANSPARENT` it receives no mouse messages, so the page
  could never notice the cursor coming back.
* Safety default: `click_through` starts **false**; the page arms it only after
  publishing its first region set. A broken page therefore leaves a normal,
  fully interactive window instead of an unclickable screen.

### API server

* `api::start` binds `127.0.0.1:<port>` first (trying `port..port+9`), sets it
  non-blocking, then hands it to a Tokio listener inside the `avatar-api` thread.
* `axum::serve(...).with_graceful_shutdown(oneshot)`; the response to
  `/shutdown` is flushed before a 120 ms delayed `app.exit(0)`.
* Errors: our handlers return `{"ok":false,"error":"…"}` with **400**;
  malformed JSON is rejected by axum with a plain-text **422**; unknown routes
  are **404**.
* **CORS.** The page runs on the `tauri://localhost` custom scheme, so every
  in-page call to `127.0.0.1` is cross-origin. `allow_webview_cors` (an
  `axum::middleware::from_fn` layer on the router) answers the `OPTIONS`
  preflight itself and stamps `Access-Control-Allow-Origin: *` plus the
  allow-methods/headers pair on every response — without it a
  `POST /survival/feed` from the survival card dies in the browser before it
  reaches a handler.

### Event bus (SSE)

`AppCtx.events` is a `tokio::sync::broadcast::Sender<String>` with capacity
`EVENT_CHANNEL_CAPACITY = 64`, created in `AppCtx::new`. It is deliberately
separate from Tauri's own emitter: Tauri events only reach the page, the
broadcast channel reaches HTTP clients, and the two carry the same information.

```
publish() / do_speak() / do_clear_speech() / notify_interaction()
model load|switch|import|reset / save_rig / set_pose / patch_part
        │
        ├─► app.emit(<tauri event>)        page applies it next frame
        └─► AppCtx::emit_event(type, json)  ──► broadcast (64) ──► GET /events
                                                   │
                                          (lagged subscriber skips forward)
```

* `emit_event` stamps a monotonic `seq` (`AtomicU64`) and `ts_ms`, then
  `send`s — a send with zero receivers is a no-op, not an error.
* Every frame is also appended to `AppCtx.recent_events`, a bounded
  `VecDeque<Value>` of length `EVENT_HISTORY_CAPACITY = 15` (oldest evicted).
  `GET /context` serves that buffer as `recent_events`, which is how an agent
  that only polls gets short-term memory of what just happened — there is no
  `Last-Event-ID` replay on the wire.
* `GET /events` subscribes on connect, yields a `hello` frame with the current
  snapshot, then adapts the receiver with `tokio_stream::wrappers::BroadcastStream`
  into `Sse<impl Stream<Item = Result<Event, Infallible>>>`. Keep-alive is 15 s.
* The SSE `event:` field carries the frame `type` (see `api::event_name`), so
  `EventSource` consumers can subscribe per-type; unknown types fall back to the
  default name and stay readable from the JSON.
* `report_state` broadcasts only on a real transition, which keeps the stream
  quiet while the avatar merely walks.
* **Page-local events never reach the bus.** `cursor-pos` (gaze),
  `chat-message` and `chat-history-cleared` are emitted with `app.emit` only:
  they are cosmetic UI state, they must not bump `AvatarState.revision`, and an
  agent already learns the same facts from the `user_reply` / `speech` /
  `session_reset` frames. `AppCtx::emit_to_page` is the helper that emits them
  (it no-ops when no window handle is installed, which is what keeps the core
  functions unit-testable).

---

## 3. Codebase map

### `src-tauri/src/lib.rs` (5000 lines)

| Section | Responsibility |
| --- | --- |
| consts | `AVATAR_STATES`, `AVATAR_EXPRESSIONS` (6), `INTERACTION_KINDS` (`click`, `double_click`, `drag_end`), `EVENT_CHANNEL_CAPACITY = 64`, `EVENT_HISTORY_CAPACITY = 15`, `CHAT_HISTORY_CAPACITY = 50`, `CHAT_ROLES` (`user`, `assistant`), `CURSOR_EMIT_DEADBAND = 3.0`, `CURSOR_EMIT_INTERVAL_MS = 50`, `DEFAULT_API_PORT = 8765`, `MAX_SPEECH_CHARS = 2000`, `MAX_SPEECH_DETAILS_CHARS = 8000`, `MAX_CLIPBOARD_CHARS = 2000`, `MAX_CLIPBOARD_WRITE_CHARS = 32000`, `MAX_REPLY_CHARS = 500`, `MAX_QUICK_TASKS = 6`, `DEFAULT_HOTKEY = "Alt+Space"`, `HOTKEY_ID` |
| `AvatarState`, `HitRegion` | serialized shared state; region contains-test |
| `lock_or_recover`, `snapshot` | poisoned-lock recovery (never panic on the UI path) |
| `ChatMessage`, `now_ms`, `chat_history_json` | one conversation turn `{id, role, text, details?, expression?, ts_ms}`; the single `{ok, messages}` body shared by `GET /history` and `get_chat_history` |
| `AppCtx` + impl | avatar state, regions, `api_port`, `speech` slot, `events` broadcast sender, `recent_events: Mutex<VecDeque<Value>>`, `event_seq`, `speech_seen`, `expression_commanded_at`, `click_through`, `running`, `api_url`, `shutdown_tx`, `watcher: Mutex<WatcherState>`, `chat: Mutex<VecDeque<ChatMessage>>`, `chat_seq`, `app: Mutex<Option<AppHandle>>`; `emit_event` (which also remembers the frame), `recent_events`, `emit_to_page` (page-local events, no bus), `push_chat` / `chat_history` / `clear_chat` (the bounded log + `chat-message` / `chat-history-cleared`), `apply_ignore_cursor`, `request_shutdown` (which calls `stop_watcher`), `watcher_status`, `set_watcher_enabled`, `push_watcher_log`, `spawn_watcher` |
| `ModelCtx` + impl | `root`, `current`, `loaded` cache, `pose`, `joints`, `part_patches`, `dialog_open`; `load`, `ensure_loaded`, `invalidate`, `apply_pose`, `parts_snapshot`, `rig_path` |
| `publish` | bump revision + emit `avatar-state-changed` + broadcast `state` |
| `model_event_payload` | compact `{name, label, rig_path, is_override, bones, images, reason}` for `model` frames |
| commands | 44 `#[tauri::command]` fns (see §6) |
| `do_emote`, `do_speak`, `do_speak_opt`, `do_clear_speech` | shared logic used by both commands and HTTP routes (each broadcasts its frame); `do_speak` also calls `tts::enqueue_speech(text)` — the single hook that gives **every** spoken line a voice — and records the `"assistant"` turn in the conversation log; `do_speak_opt` takes `Option<&AppHandle>` so that recording path is unit-testable without a window |
| `do_animate` | custom keyframe playback: emits `avatar-animate` to the page **and** an `animate` frame on the SSE bus (`clip = None` stops) |
| `do_notify_interaction` | interaction validation + `interaction` frame; the command is a thin wrapper so it is unit-testable |
| `do_user_reply`, `notify_reply` | quick-reply validation (non-blank, ≤ `MAX_REPLY_CHARS`), bubble dismissal and the `user_reply` frame; the command adds the page-side `speech-clear` event |
| `do_session_reset`, `reset_session` | broadcast `session_reset` (`{reason}`) + dismiss the bubble + empty the conversation log (`clear_chat` → `chat-history-cleared`); the watcher does the actual rotation |
| `quick_tasks`, `default_quick_tasks` | read `quick_tasks` out of `watcher_rules.json` for `GET /watcher` (blank entries skipped, capped at `MAX_QUICK_TASKS`, built-in chips when absent/unreadable) |
| `AiProfileInfo`, `ai_profiles_from_document`, `ai_profiles` | parse `active_profile` + `ai_profiles` out of the rules document (unknown `kind`s skipped, label falls back to the model then the name, a pre-profiles file still reports a synthesised `hermes` / `hermes_cli` entry); `summary()` omits `endpoint` for a CLI profile |
| `health_probe_urls`, `probe_openai_health`, `ai_health`, `find_on_path`, `hermes_on_path` | the liveness probe behind the panel's dot: a GET of the server's /health then /v1/models with an 800 ms cap → `online` / `offline` (plus the endpoint and error in `detail`); a `hermes_cli` profile reports `status: "cli"` with `reachable` = whether `hermes` resolves on `PATH` |
| `ai_status_json`, `set_active_ai_profile`, `with_active_profile`, `write_rules_atomically` | the `GET /ai-profiles` body; the switch (validate against the file → rewrite **only** the `active_profile` string so hand-written formatting and key order survive → write a sibling `.json.tmp` and rename, so the rules file is never half-written → re-parse to prove it → re-read the status); an unknown name is an error that lists the valid ones |
| `do_user_reply_with_profile` | `do_user_reply` plus an optional `profile` and `include_screen`, both carried in the `user_reply` frame so a chip can pin its own brain and/or ask the watcher for a screenshot; also records the trimmed prompt as a `"user"` turn in the conversation log |
| `desktop_context` | Win32 `GetForegroundWindow`/`GetWindowTextW`, `GetWindowThreadProcessId`/`OpenProcess`/`QueryFullProcessImageNameW` (the `process_name`), `GetLastInputInfo`/`GetTickCount64`, `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` for `window_rect` and `EnumWindows` for `visible_windows`, behind `GET /context`; safe defaults off Windows. The overlay is a full-screen transparent window, so after a drag or a quick-task click *it* owns foreground: `reported_window` drops a foreground whose PID is `std::process::id()` (or whose exe name is `own_process_name`, for an unreadable PID) and reports the front-most other entry from `visible_windows` instead — the same rect an unqualified `POST /perch` perches on — or a blank context when nothing else is on screen |
| `physical_rect_to_logical`, `primary_scale`, `primary_viewport` | physical px → CSS px via `GetDpiForSystem()/96`, degenerate rects rejected, monitor size in CSS px |
| `perch_target`, `PerchEdge`, `PERCH_EDGES`, `AvatarBounds` | pure perch geometry: feet on a top/bottom edge, standing beside a left/right edge, `offset` along the edge, clamp to the monitor — unit-tested without Win32; `AvatarBounds` is the art's extent per direction around the root |
| `tool_perch`, `tool_move` | MCP-side argument validation (`edge`, `look_at`) before any HTTP call |
| `clipboard_text`, `truncate_chars` | Win32 `OpenClipboard`/`GetClipboardData(CF_UNICODETEXT)`/`GlobalLock` read behind `GET /clipboard`, capped at `MAX_CLIPBOARD_CHARS` on a char boundary; every path balances its handle |
| `write_clipboard_text`, `ClipboardWrite` | the Win32 write half: `OpenClipboard` → `EmptyClipboard` → `GlobalAlloc(GMEM_MOVEABLE)` + UTF-16 copy → `SetClipboardData(CF_UNICODETEXT)` → `CloseClipboard`, capped at `MAX_CLIPBOARD_WRITE_CHARS` and reported as `{chars, truncated}`; a failed `SetClipboardData` frees the block instead of leaking it. Drives `POST /clipboard` and the `set_clipboard` command |
| `Hotkey`, `parse_hotkey`, `virtual_key`, `hotkey_spec` | pure parser for the `"Ctrl+Shift+A"` grammar (modifier bits + virtual-key code) and the `hotkey` lookup in `watcher_rules.json`; a bad value is a logged warning, never a panic |
| `summon_command_bar`, `spawn_hotkey_listener` | the `avatar-hotkey` thread: `RegisterHotKey` (+ `MOD_NOREPEAT`), a 120 ms `PeekMessageW` poll, then `set_click_through(false)` + `apply_ignore_cursor(false)` + `window.set_focus()` + the `summon-command-bar` event; `UnregisterHotKey` on `request_shutdown` |
| `cursor_position`, `cursor_emit_due`, `spawn_cursor_tracker` | Windows cursor poll (stub returning `None` elsewhere). The poll publishes `cursor-pos` `{x, y}` (CSS px) whenever the cursor moved ≥ `CURSOR_EMIT_DEADBAND` and ≥ `CURSOR_EMIT_INTERVAL_MS` passed — the pure `cursor_emit_due` predicate is unit-tested — **and** flips `WS_EX_TRANSPARENT` per hit region; emission happens whether or not the window is click-through, as long as it is visible and not minimized |
| `WatcherState`, `watcher_status`, `set_watcher_enabled`, `push_watcher_log` | on/off switch, live `Child`, resolved `binary`/`rules` paths and a 200-line `VecDeque` log tail; the status JSON feeds `GET /watcher`, `get_watcher_status` and every `watcher` frame (Tauri `watcher-status` + SSE) |
| `watcher_disabled_by`, `watcher_binary_name`, `watcher_binary_path` | `DESKTOP_BUDDY_NO_WATCHER` parsing; resolves `desktop-buddy-watcher{EXE_SUFFIX}` next to `current_exe()` |
| `watcher_rules_candidates`, `watcher_rules_path` | rules lookup order: cwd → beside the binaries → parents up to `models_root()`'s parent |
| `clock_stamp`, `stamped`, `push_log_line`, `log_watcher_event`, `spawn_log_reader` | `[HH:MM:SS]` local-time stamps (`GetLocalTime`), bounded ring-buffer append, and the two pipe readers that echo every line to the app's stderr |
| `spawn_watcher`, `supervise_watcher`, `watcher_health`, `start_watcher`, `kill_and_reap` | spawn with `--api-url`, `--rules`, null stdin, piped stdout/stderr, `CREATE_NO_WINDOW`, cwd = exe dir; then `try_wait` polling, crash restart with backoff (parked while off), and a store-under-the-lock handshake that cannot orphan a child during shutdown |
| `api_port`, `run` | startup |
| `tests` | 110 unit tests over the event envelope, `reported_window` / `is_self_foreground` (PID and exe-name matching, the front-most-window fallback and its rect, the blank overlay-alone case), sequence numbers, the 15-frame history ring buffer, interaction validation, quick replies (trim, caps, bubble dismissal, `user_reply` frame), `session_reset` (frame + bubble dismissal), `quick_tasks` (defaults, file parse, blank-entry filtering, cap, missing/broken fallback), `truncate_chars`, the clipboard contract (read cap + write cap + char boundary), the desktop-context shape, the hotkey grammar (`parse_hotkey`) and the `hotkey` rules lookup, `model_event_payload`, the `DESKTOP_BUDDY_NO_WATCHER` parser, the watcher binary name, `watcher_health`, the log ring buffer, the watcher status keys, the rules lookup order, and the shutdown kill (no orphaned pid) |

### `src-tauri/src/platform/` (the OS boundary)

Every OS call in the crate lives here; `lib.rs`, `api.rs`, `stt.rs` and `tts.rs`
are portable Rust that only ever see this facade. Adding a third OS means one
file plus two `cfg` lines in `mod.rs`.

| File | Contents |
| --- | --- |
| `mod.rs` | The facade: `#[cfg(target_os = "windows")] mod win32;` / `#[cfg(any(target_os = "linux", target_os = "macos"))] mod unix;` and the re-exports both backends must provide (`desktop_context`, `clipboard_text`, `write_clipboard_text`, `screen_metrics`, `primary_scale`, `monitor_bounds_for`, `cursor_position`, `tracked_window_rect`, `key_is_down`, `HotkeyListener`) |
| `types.rs` | The shared coordinate space — `DesktopContext`, `WindowRect`, `VisibleWindow`, `ScreenMetrics`, `ClipboardText`/`ClipboardWrite`, `ReportedWindow`, `is_self_foreground`, `reported_window`, `truncate_chars` and the clipboard caps. Pure data, so it compiles everywhere |
| `hotkey.rs` | The `"Ctrl+Shift+A"` grammar (`parse_hotkey`), the portable `Hotkey {modifiers, vk}` and the modifier/virtual-key constants. Pure and unit-tested on every target; only *registration* is OS-specific |
| `win32.rs` | `GetForegroundWindow`/`GetWindowTextW`/`DwmGetWindowAttribute`/`EnumWindows` for `/context`, `CF_UNICODETEXT` for the clipboard, `GetCursorPos`, `RegisterHotKey` + `PeekMessageW`, `GetAsyncKeyState` |
| `unix.rs` | Linux/macOS: `xdotool`/`wmctrl`/`xrandr`/`xprop` and `osascript` for window context, `xclip`/`xsel`/`wl-copy`/`pbcopy` for the clipboard, `rdev` for the global cursor and key state, `global-hotkey` for the shortcut |

`tools/platform-check/` compiles this tree for another target without the Tauri
toolchain (`cargo check --target x86_64-unknown-linux-gnu`), which is how the
Linux and macOS backends are verified from a Windows box.

### `src-tauri/src/stt.rs`

Push-to-talk voice input. One module, one worker thread, one config block.

| Item | Responsibility |
| --- | --- |
| `DEFAULT_WHISPER_EXE_PATH`, `DEFAULT_WHISPER_MODEL_PATH`, `DEFAULT_SILERO_VAD_MODEL_PATH`, `DEFAULT_STT_HOTKEY`, `DEFAULT_VAD_THRESHOLD`, `DEFAULT_STT_LANGUAGE`, `DEFAULT_MIN_SPEECH_MS` | the verified shipped install; `verify_docs.py` pins the rules file against these constants |
| `SttConfig` | the `stt` block (`enabled`, `hotkey`, `whisper_exe_path`, `whisper_model_path`, `silero_vad_model_path`, `vad_threshold`, `language`, `min_speech_ms`), `#[serde(default)]` so a rules file from before voice input still loads |
| `SttConfig::load`, `path_problem`, `threshold`, `language`, `vad_model` | read the block, validate the three files + the microphone once, clamp the threshold to 0.01–0.99, fall back to `en` for a blank language |
| `init`, `status_json`, `start`, `stop`, `set_enabled`, `shutdown`, `is_live` | the process-wide `OnceLock<SttRuntime>`: start the worker, report `{enabled, ready, state, hotkey, last_transcript, last_error, reason, …}`, and the two edges the panel/hotkey drive. `start` also calls `tts::interrupt()` |
| `run_worker` | the `avatar-ptt` state machine: idle (hotkey rising edge via `GetAsyncKeyState`, or `PttCommand::Start`) → `listening` → `transcribing` → post → idle. Stale commands are drained after a transcription so a tap mid-run cannot start a surprise recording |
| `capture_until_stop`, `MicStream` | `cpal` default input device at its own preferred format; the callback downmixes interleaved frames to mono `i16` (`append_mono`), the worker drains the buffer every 15 ms, `resample_pcm16` brings it to 16 kHz, then the `min_speech_ms` gate and a 60 s cap |
| `downmix_pcm16`, `resample_pcm16`, `wav_bytes`, `temp_wav_path` | interleaved 16-bit PCM → mono, linear-interpolation resampling to 16 kHz, a 44-byte-header PCM WAV, and `%TEMP%\desktop-buddy-stt-<pid>-<n>.wav` (deleted right after transcription) |
| `transcribe`, `run_whisper` | write the clip, spawn `whisper-cli.exe` with `-m`, `-f`, `-l`, `-nt`, `-np` and `--vad --vad-model --vad-threshold` when a VAD model is configured, `CREATE_NO_WINDOW`, stdout captured / stderr dropped, then delete the file |
| `sanitize_transcript`, `is_non_speech` | strip `[BLANK_AUDIO]`, `(music)`, `[inaudible]`, timestamp brackets and control characters, collapse whitespace; an empty result is never posted |
| `post_reply` | `POST /reply {"text": …}` against the app's own API (blocking reqwest, 5 s timeout), capped at `MAX_REPLY_CHARS` |

### `src-tauri/src/tts.rs`

The local voice. One module, one worker thread, one config block.

| Item | Responsibility |
| --- | --- |
| `DEFAULT_PIPER_EXE_PATH`, `DEFAULT_PIPER_MODEL_PATH`, `DEFAULT_ESPEAK_DATA_PATH` | the verified shipped install (`C:\hermes\tools\piper\…`); `verify_docs.py` pins the rules file against these constants |
| `TtsConfig` | the `tts` block (`enabled`, `piper_exe_path`, `model_path`, `espeak_data_path`, `speaker_id`, `length_scale`). `#[serde(default)]` on the container means every field is optional, so a rules file from before voice output existed still loads |
| `TtsConfig::load` | read the block out of `watcher_rules.json`; a missing file/block is silent, a malformed one returns one warning and the defaults |
| `TtsConfig::path_problem` | validate `piper.exe`, the `.onnx` and the espeak-ng directory **once** at startup |
| `init`, `status_json`, `set_enabled`, `is_speaking_enabled`, `enqueue_speech` | the process-wide `OnceLock<TtsRuntime>`: start the worker, report `{enabled, ready, reason, paths}`, mute/unmute (mute also purges playback), and the hook `do_speak` calls (sanitize → skip if empty → `try_send`) |
| `run_worker`, `synthesize`, `temp_wav_path` | the `avatar-tts` thread: one `piper.exe` spawn per line (`--model`, `--espeak_data`, `--speaker`, `--length_scale`, `--output_file`, `CREATE_NO_WINDOW`, UTF-8 text on **stdin**), a `> 44`-byte wav check, then delete. Before playing, the wav is read back and its envelope emitted (`tts-envelope`); playback goes through `rodio::Sink` (cpal backend: WASAPI/ALSA/PipeWire/CoreAudio) and `tts-envelope-stop` is sent when the sink drains |
| `set_emitter`, `emit`, `emit_envelope_stop` | the worker has no Tauri handle, so the app installs one emitter at startup and the module emits `tts-envelope` / `tts-envelope-stop` through it (a no-op in tests) |
| `wav_envelope`, `parse_wav`, `sample_at`, `envelope_payload`, `ENVELOPE_BUCKET_MS` | audio-driven lip sync: walk the RIFF chunks, read 8/16-bit PCM (mono or stereo, `WAVE_FORMAT_EXTENSIBLE` included), average the channels, compute the RMS of each **25 ms** bucket and normalize the loudest bucket to `1.0`. Silence, an unreadable file or a non-PCM wav returns an empty envelope, and the page keeps the character-driven mouth |
| `play_file`, `stop_playback` | `rodio`: one `OutputStream` per worker, a `Sink` per line (`append` + `sleep_until_end`, sequential by construction), and `Sink::stop()` for mute/interrupt. The sink is shared with `stop_playback` through an `Arc`, so a cut from another thread also unblocks the worker |
| `sanitize_for_speech`, `strip_code_fences`, `strip_links`, `strip_urls`, `strip_tags`, `is_emoji`, `is_markup_marker`, `truncate_chars` | the "Piper reads clean prose" half: fenced code blocks, links (the label stays, the URL goes), bare URLs, `<tag>` control/expression markers, markdown emphasis and code markers, emoji and control characters are removed; whitespace collapses; an empty result is never spoken |

### `src-tauri/src/soul.rs`

The avatar's durable persona, `SOUL.md`. Dependency-light (std + `serde_json`)
because `desktop-buddy-watcher` includes it directly with `#[path = "../soul.rs"]`,
so the app and the watcher share one resolver and one seed.

| Item | Responsibility |
| --- | --- |
| `DEFAULT_SOUL` | the ~50-line starter: voice, brevity, candour, humour, face/body, screen reactions, anti-patterns. No paths, profiles or secrets — those live in `watcher_rules.json` |
| `MAX_SOUL_BYTES`, `ENV_SOUL_PATH`, `SOUL_FILE_NAME` | 64 KB cap; `DESKTOP_BUDDY_SOUL` override; `SOUL.md` |
| `soul_path` | the one resolution rule: `DESKTOP_BUDDY_SOUL` → `soul_path` in the rules file → `<rules dir>/SOUL.md` → `<app config dir>/SOUL.md` |
| `read_soul`, `load_soul` | read with the size cap; empty/missing/unreadable/oversized all fall back to `DEFAULT_SOUL` |
| `ensure_soul` | seed a missing file with `create_new` (never clobbers an existing or empty file), then return the effective persona |
| `write_soul` | atomic replace (temp file + rename); refuses an empty body or anything over the cap |
| `soul_json` | the `GET /soul` body: `path`, `exists`, `is_default`, `content` (raw file) and `persona` (effective) |

### `src-tauri/src/api.rs` (1425 lines)

| Section | Responsibility |
| --- | --- |
| `ApiCtx` | `app`, `shared: Arc<AppCtx>`, `model: Arc<ModelCtx>`, `started` |
| `watcher_status`, `set_watcher` | `GET /watcher` / `POST /watcher` → `AppCtx::watcher_status` / `set_watcher_enabled` |
| `get_tts`, `set_tts` | `GET /tts` / `POST /tts` → `tts::status_json` / `tts::set_enabled` (plain `Json` handlers: no app handle, no lock) |
| `get_speech`, `history` | `GET /speech` (the stored bubble + `talking`) and `GET /history` (`crate::chat_history_json` — the same bounded conversation log the History drawer renders) |
| `get_stt`, `set_stt` | `GET /stt` / `POST /stt` → `stt::status_json` / `stt::set_enabled` (status + live `idle`/`listening`/`transcribing` state) |
| `ai_profiles`, `switch_ai_profile` | `GET /ai-profiles` / `POST /ai-profile` → `ai_status_json` / `do_set_ai_profile`, both on a `spawn_blocking` thread because the health probe is a blocking 800 ms HTTP GET; `event_name` maps the frame to `ai_profile` |
| `discover_models` | `POST /ai-discover` → `discovery::discover`, on a `spawn_blocking` thread (each `(host, port)` target is a blocking GET with a 1.5 s budget, probed in parallel); an optional `{hosts: […], ports: […]}` body overrides the rules file, otherwise `settings::discovery_hosts` / `settings::discovery_ports` supply the lists and `discovery::targets` crosses them |
| `get_config`, `set_config` | `GET /config` / `POST /config` → `settings::config_json` / `settings::apply_config` (the write runs on `spawn_blocking`), then the live side effects: `tts::set_enabled`, `stt::set_enabled`, `plugins::apply_survival_config`, and one `ai_profile` frame so the watcher re-reads the file |
| `get_soul`, `set_soul` | `GET /soul` / `POST /soul` → `soul::soul_json` / `soul::write_soul` on `spawn_blocking`; `soul_path` resolves the same file the watcher reads. No cache to invalidate — the watcher re-reads on every turn |
| `start` | bind, spawn thread, Tokio runtime, axum serve, graceful shutdown |
| `bind_first_available` | port fallback `preferred..preferred+9` |
| `router` | 41 core routes, then `crate::plugins::routes()` merged in (deduped by path) — **no plugin endpoint is named in this file** |
| request bodies | `SetStateBody`, `MoveBody`, `ExpressionBody`, `ClickThroughBody`, `HitRegionsBody`, `ModelBody`, `RigBody`, `PoseBody`, `ImportBody`, `PartPatchBody`, `MouthBody`, `EmoteBody`, `SpeakBody` (incl. optional `details`), `ReplyBody`, `AlertBody`, `ClipboardBody`, `AnimateBody`, `TtsBody`, `SttBody` |
| `ApiError` | `IntoResponse` → `{"ok":false,"error":…}` + status |
| `context` | `GET /context` — OS probes (title, process, idle, `window_rect`, `visible_windows`, `foreground_hwnd`, `screen`), `avatar_state` snapshot and the `recent_events` history |
| `perch` | `POST /perch` — resolve a window rect (by `window_process` or the foreground window) and its `hwnd`, turn `edge` + painted bounds + `offset` into a target clamped to that window's monitor, mark the state `perched` (+ tracking: `perch_hwnd`, `perch_edge`, `perch_offset`, `perch_bounds`) and publish a `state` frame |
| `clipboard`, `set_clipboard`, `reply`, `alert` | `GET /clipboard` (never fails: empty text for a non-text clipboard), `POST /clipboard` (`{text}` → `write_clipboard_text`, `{ok, chars, truncated}`; a held-open clipboard is a 400 to retry), `POST /reply` (`user_reply` frame + bubble dismissal) and `POST /alert` (severity validation → `alert` frame) |
| `ALERT_SEVERITIES` | `info` \| `warn` \| `error`, the only values `POST /alert` accepts |
| `events` | `GET /events` — SSE from `BroadcastStream`, `hello` frame, 15 s keep-alive |
| `event_name` | maps a frame `type` onto the SSE `event:` field (`&'static str`) |
| `list_animations`, `animate`, `stop_animation` | `GET /animations` (clip catalogue from the loaded rig) and the two stop/play spellings for `POST /animate` |
| handlers | one per route (see §7) |

### `src-tauri/src/plugins/` (the host + the WebAssembly runtime)

Plugins are **not compiled into this binary**. They are `.wasm` files in a
`plugins/` directory, built by their own crates, loaded at startup. No plugin is
named in `lib.rs`, `api.rs` or this directory's registry: delete every `.wasm`
and the app still starts (with no plugins) and still serves `/plugins`. The page
follows the same rule: `ui/js/plugins.js` reads `GET /plugins` and reveals a
`data-plugin="<id>"` control-panel button or Settings tab only while that
plugin's state is `active`, so a missing `.wasm` removes the menu entry too — it
never leaves a button that opens an offline card.

| file | responsibility |
| --- | --- |
| `mod.rs` | the feature-blind host: `Plugin` (`init` / `tick` / `shutdown` + optional `status` / `routes` / `config`), `PluginHost` (`emit`, `notify`, `now_ms`, `rules_path`, `write_model`, `activate_model`, `animate`), `PluginRoute` (a path + an axum `MethodRouter`), `PluginConfig` (`key`, `describe`, `validate`, `apply`, `warnings`), the failure-tolerant registry, and the process-wide `install` / `routes` / `config_specs` / `stop` |
| `wasm_host.rs` | the WebAssembly runtime: scans the plugin directory, compiles each file to a `wasmtime::Module`, instantiates it in **its own `Store`** with a fuel budget, defines the host imports, and wraps the instance in a `WasmPlugin` that implements `Plugin` |
| `manifest.rs` | how the app finds plugins — `wasm_host::load_all(&mut registry)`. It names nothing, so there is nothing to edit when a plugin is added or removed |

#### The guest ABI

JSON in, JSON out, through the guest's own allocator:

```text
alloc(len) -> ptr        the host writes its request into guest memory
dealloc(ptr, len)        either side frees a buffer it owns
plugin_init(ptr, len)    {"config": <the plugin's block or null>}
plugin_tick(dt_ms)       one tick
plugin_status()          the entry in GET /plugins
plugin_routes()          [{"path": "/survival", "methods": ["GET","POST"]}, …]
plugin_http(ptr, len)    {"method","path","query","body"} -> {"status","body"}
plugin_config()          descriptor: {"key","title","describe"}
plugin_config_validate(ptr, len)   {"current","patch"} -> {"ok","text"}
plugin_config_apply(ptr, len)      <block> -> {"live": bool}
plugin_config_warnings(ptr, len)   {"current"} -> {"warnings": […]}
```

A reply packs `(ptr as i64) << 32 | len`; `0` means "no result". The host copies
the bytes out, calls `dealloc`, and parses. A reply with an `error` key is a log
line, never a panic.

The guest may call nine host imports — that list *is* the plugin's capability
surface:

```text
host_emit_event(name, payload)     -> the page and the GET /events bus
host_notify(text, expression)      -> spoken through do_speak
host_now_ms()                      -> the clock (wasm32 has none)
host_log(level, message)           -> the app log
host_state_save(bytes)             -> writes this plugin's save file
host_ai_attached()                 -> 1 when a watcher is on the event bus
host_model_write(slug, manifest)   -> validate + write models/<slug>/model.json
host_model_activate(slug)          -> the POST /model code path (hot swap)
host_animate(clip)                 -> the POST /animate code path
```

**Publishing a rig is a host capability, not a guest one.** A guest has no
filesystem, so a plugin that generates models (see the metamorphosis plugin
below) hands the manifest over instead of writing it:
`PluginHost::write_model` runs `models::validate_rig` and then
`models::install_generated_model`, which writes `models/<slug>/model.json`
through a temp file, *loads it back* with the app's loader, and deletes it again
if it does not read. The slug is restricted to `[a-z0-9-]`, so a manifest can
never escape the models root. `activate_model` is `ModelCtx::load` plus the
`model-changed` frame — literally what `POST /model` does — and `animate` is
`find_animation` plus `do_animate`, what `POST /animate` does. The host stays
feature-blind: it knows "install a rig", "activate a rig" and "play a clip",
never "metamorphose".

**One voice per moment.** Every spontaneous survival line — a tier crossing, a
starvation reminder, a craving that began / lapsed / was satisfied, a meal
notice, a bite, a catch — is published as an event that also carries the `line`
the plugin would have spoken and the `expression` it was written for. The
watcher routes that event to the model, so `host_ai_attached` is what decides who
talks: with a watcher subscribed to `GET /events` the guest says nothing and the
model answers (and the watcher speaks the payload's `line` itself when the model
errors, times out or no profile is configured), while with nobody attached the
guest speaks because it is then the only voice left. `config.notify` stays the
plugin's own switch for those canned lines. This gate is what removed the
double-fire where a hunger crossing produced the canned line *and* an AI answer
to the same event.

#### Persistence: the host owns the bytes

A Wasm guest has no filesystem, so a plugin that has to remember anything gets
one narrow door: `host_state_save` writes, and the last file is handed **back
inside `plugin_init`** (`{"config": …, "state": …, "state_path": …}`). That is
deliberate — the guest never names a path, the host writes atomically, and the
snapshot never has to travel through an HTTP route.

* **Where**: `plugins/<name>.state.json`, beside the `.wasm` it belongs to
  (`state_path_for`). Deleting the `.wasm` deletes the feature; deleting the
  state file starts a fresh session.
* **When**: after every mutating request (the survival dispatcher flushes any
  `POST` before returning), and on a 30 s tick cadence for the continuous hunger
  drift. Actions are user-paced, so that is one small write per click, and a
  kill right after a purchase loses nothing.
* **How**: temp file + rename (`write_state`), capped at 1 MiB, so a crash
  mid-write cannot leave a truncated file that still parses. A corrupt or
  oversized file is logged and ignored; the plugin starts fresh.
* **Schema**: `{"version", "saved_ms", "stats": <SurvivalStats>, "inventory":
  {"capacity", "stacks": [{id, count}]}}`, versioned by
  `plugins/survival/src/persist.rs`. A version this build does not know is
  ignored rather than migrated. The file is never trusted: stats go through
  `SurvivalStats::sanitized` (clamps hunger/saturation, drops an already-lapsed
  craving) and the bag through `Inventory::from_saved` (unknown ids and overflow
  are *reported* as dropped, not silently truncated); the live slot count still
  comes from the config, not from the save.
* `GET /survival` carries `persistence: {path, restored, dirty, saved_ms,
  saves}`, which is how a restart is proven from a shell.

#### Isolation

* **Memory**: one `Store` per plugin, bounds-checked linear memory; the host only
  ever copies through the exported `alloc`/`dealloc` area, capped at 4 MiB per
  message.
* **CPU**: every call gets a fresh `FUEL_PER_CALL` (50 M instructions). A guest
  that spins traps out instead of hanging the ticker.
* **Crashes**: a guest panic (`panic = "abort"`) is a trap → `Err`; the registry
  counts it and parks the plugin after `MAX_CONSECUTIVE_TICK_ERRORS`, running its
  `shutdown` hook. The app keeps running.
* **Loading**: a `.wasm` that will not compile or instantiate is logged and
  skipped. A bad drop-in cannot stop the app from starting.
* **Deadlock guard**: a guest cannot ask for the registry (`plugins::status()`)
  from inside a call — that would re-enter the registry mutex the ticker already
  holds. Anything the host knows (the plugin list, for instance) stays in a host
  route, which is why `GET /survival` no longer embeds `plugins`.

#### Building and dropping a plugin in

```bash
./scripts/build-plugin.sh          # every plugins/<name>/ crate -> plugins/<name>.wasm
# or by hand:
cd plugins/survival
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/desktop_buddy_survival_plugin.wasm ../survival.wasm
```

Then restart `desktop-buddy.exe` — no Rust build of the app, no feature flag.
The plugin's routes appear in the API, its config block appears in the settings
modal, and its entry appears in `GET /plugins`. Delete the `.wasm` to remove it.

#### The metamorphosis plugin (`plugins/metamorphosis/`)

A second guest, and the proof that the host stayed feature-blind: adding it
touched `mod.rs` (three generic capabilities), `wasm_host.rs` (their imports) and
`models.rs` (one write helper) — no route, no type, no branch named
"metamorphosis" anywhere in the app.

| file | responsibility |
| --- | --- |
| `src/rng.rs` | SplitMix64. No dependency, no `getrandom`: `(seed, archetype, tier, attribute)` reproduces the same creature — same bones, same colours — on every machine |
| `src/skeleton.rs` | the `desktop-buddy/model@1` types, the two mirroring formulas (`atan2(sin θ, −cos θ)` for a chain root, `−θ` for a descendant), `computeWorld`, and the painted-bounds pass |
| `src/attribute.rs` | `Seraph` / `Prism` / `Feral`: wire spelling, `branch_order` (bond re-orders the three branches — Seraph leading for a devoted creature, Feral for a feral one — and never drops one), the one-line flavour the UI shows, and `AttributeBlend` — the recency-weighted mix of a whole evolution line (linear step weights `1..n`, normalised) plus its percentages, prominence and blended flavour |
| `src/plan.rs` | The fifteen body plans, each with its own six-form table (`beast_form`, `drake_form`, …), then the attribute's mutations, then the seeded build and the seed's individual layer (`individualise`): head, ears, crest, arms, legs, tail and trunk per archetype, plus proportion multipliers, posture, accessory geometry and the family's secondary-trait vocabulary (`secondary_traits`) |
| `src/generator.rs` | `generate(Spec) -> Manifest`: centerline → tail → right chains → mirror → archetype parts → blended attribute parts → clips → monitor guard → detail trim, plus `validate` / `validate_at(manifest, tier)` and `max_parts(tier)` |
| `src/evolution.rs` | stage/XP/bond/attribute state plus the **pinned variant** each stage keeps (so a later form/build/marking addition cannot move an existing creature), the three-branch table (each branch previews the mix *and* the body it would produce; a child inherits its parent's variant, mutating only 12% of the time for a new form and 16% for a new build/marking), and the transitions both ways (generate → host write → host activate; `revert` regenerates the parent from its recorded seed, attribute **and variant**, and from the *truncated* lineage's blend) |
| `src/host.rs` | typed wrappers over the host imports (`write_model`, `activate_model`, `animate`, `emit`, `notify`, `log`, `save_state`, `now_ms`); the one place the guest talks to the app |
| `src/plugin.rs` | live state: passive XP, the one-shot "ready" notice, the 30 s save, `GET /creature/status`'s body (stage, XP, bond, attribute, rig measurements, branches, history, and the `archetypes` / `attributes` lists the UI builds its pickers from) |
| `src/routes.rs` | the `ROUTES` table + dispatcher |
| `tests/generator_tests.rs` | every tier × archetype × attribute × seed: schema, parents, cycles, limits, FK symmetry, monitor bounds, face tags, z-stack, budget, and the variety rules (three attributes ⇒ three rigs) |

**Three layers of variety, one function.** `generate(Spec)` composes three
independent choices, so the corpus is large without any of it being hand-drawn:

1. **The archetype** (`src/plan.rs`) picks the body plan — head, ears, crest,
   arms, legs, tail and trunk, plus proportion multipliers. Fifteen of them:
   beast, drake, cyborg, holy, aqua, cervid, machine, alien, avian, insect,
   serpent, golem, amphibian, flora and wraith.
2. **The attribute mix** (`with_blend`) reshapes that plan: Seraph grows
   wings, a halo and feather vanes; Prism grows crystals, circuit traces and a
   visor band; Feral grows spines, fangs and claws. Structural, not a tint.
   The mix is the creature's whole lineage, not just its current attribute —
   see *Attribute blend* below. A body has one crest and one pair of hands, so
   those two choices belong to the loudest attribute; everything additive is
   drawn for every attribute above the visibility threshold.
3. **The seed** (`Spec::variant`, twenty-four values) picks one of six *forms* —
   the archetype's own structural silhouettes (a Beast is classic, lupine,
   ursine, sabre, mole or griffin; a Drake is spiked, a wyrm, a wyvern,
   serpentine, a finned tide-drake or a legless hydra; and so on) — one of three
   *builds* (lean, canonical, broad, spare) and one of six markings (none /
   spots / stripes / patches / bands / dapple). A family's last two forms are its *kind swaps* rather
   than another set of proportions: the fifth leaves the family's default
   locomotion or covering behind (a Machine on legs, a hovering Serpent, a Holy
   warden in stone greaves, a Golem with carved arm-cannons), and the sixth
   leaves it again in a different direction (a Beast that flies, a Drake with no
   legs at all, a hovering Machine, a Holy order's winged messenger, an Insect
   with a raised stinging tail).
4. **The seed again, continuously** (`Plan::individualise`): the variant is a
   table, so the seed then deforms the chosen silhouette — every proportion and
   posture angle inside the armature's safe band (`BAND_PROPORTION`,
   `BAND_SCALE`, `BAND_SEGMENT`, `BAND_SNOUT`, the same constants the tests
   assert against), a second family trait just under half the time (drawn only
   from `secondary_traits`, so it can never blur two families together), a
   rerolled marking, a hue drift on the body and a larger one on the accent,
   and — for about one individual in five — one trait (ears, tail or furniture)
   borrowed from a *sibling form* of the same family, so a mole's ears can sit
   on a sabre's skull while the family still reads as one animal.
   Two creatures that share a variant are the same *kind* of animal, not the same
   animal.

A form carries its own ear/crest scale and sweep, overall limb length and the
segment split inside each limb (`arm_segment`/`leg_segment`, a multiply/divide
pair that keeps the reach and moves the joint), arm and leg stance, muzzle
stretch (`snout`), tail length and arc, and one accessory drawn as parts (mane,
dorsal ridge, shoulder spurs, hip fins, jaw frill, second sensor pair, throat
collar, tail blades, shell, pauldrons, tusks, back sail, hip plates, cape,
second horns, sensor ring, shoulder shells). 15
archetypes × 6 forms × 4 builds × 6 markings × 3 attributes is 6,480
silhouettes before a palette is chosen, and the palette follows the attribute as
a tint (a fraction, never a replacement).

Measured with `cargo run --example variety -- 40` (forty seeds per family, at
one tier and attribute): distinct *structural templates* per family went from 12
— the ceiling the twelve-variant table imposed — to about 34 (31–37 depending
on the family), and all forty are unique in proportion and colour. Half of that spread is furniture: the individual
layer draws a second accessory from the family's own vocabulary just under half
the time, and each family's pool holds five or six kinds, so the shells, tusks,
sails, pauldrons and hip plates are worn in every combination the family allows. `examples/digits.rs` dumps one rig per family
by form index so the form table can be rendered as a contact sheet and eyeballed.

**The forms are per-archetype functions, not a template.** `plan_for` composes
`plan(archetype)` → `<archetype>_form(plan, form)` → `with_blend` →
`with_build`, and each of the fifteen form tables in `src/plan.rs` returns the
plan it was handed with only that archetype's numbers changed. Nothing is shared
between two archetypes' forms, so a Beast's mane can never appear on a Drake by
accident, and `plan.rs`'s own tests assert both that each archetype has five
distinct forms and that the fifteen archetypes stay distinct *at every form
index*. `generator_tests.rs` adds the end-to-end version: forty seeds of one
family have to yield at least eighteen distinct templates and forty distinct
bind poses.

**Generation, not authoring.** Each stage is built from tier-scaled dimensions:
bone count, limb segments (forearm / shin / hand / foot), wings (and a second
wing segment), armour plates, tail segments, eyespots, halo and aura grow as the
tier rises, while the part count stays under the stage's budget — `MAX_PARTS`
(100) for Fresh through Champion, 150 for Ultimate and Mega, because a line that
has mixed two or three attributes genuinely has more layered silhouette to draw.
The archetype's signature is present from `Fresh` — a small crest, ears, gills,
treads, coil or folded wing buds — because hiding it until In-Training made every
hatchling the same blob; the tier scales those parts instead of introducing them.
Coordinates are authored in local pixels with the root at `scale: 0.55`, and
`enforce_monitor_budget` verifies `min_world_y >= -170` over every painted corner
— the format's top-clip budget — shifting the pelvis down if a hand-tuned number
ever exceeded it. `trim_details` then drops decoration (`spot`, `stripe`,
`patch_*`, `circuit_*`, `crystal_core`, the form's accessory last, …) until the
rig fits four parts under the cap — ordered by the **attribute's share of the
lineage**, so a minor/older contributor loses its decoration before the
dominant/latest one does. A rich plan loses decoration, never a limb: structural
and limb parts are not in the trim table at all. Forms add parts, never bones: the joint count is
unchanged, so the mirroring rules and the 40-bone ceiling hold by construction.

**Attribute blend.** A line is not one attribute, it is a history.
`Metamorphosis::lineage` returns the egg's attribute followed by the attribute each
transition chose (`HistoryEntry::to_attribute`; a legacy entry without it borrows
the next entry's parent attribute, or the live one), and `AttributeBlend` weights
those stages linearly — the oldest counts `1`, the newest `n` — then normalises.
A creature that has been Prism for four stages and has just become Feral therefore
reads 60/40: Prism still owns the body while the fresh feral spines are plainly
there. `plan::with_blend` gives the single-slot traits (crest, hands) to the
loudest attribute and draws every additive one (halo, wings, crystals, circuits,
spines, fangs, drips) for each attribute above 12%, scaling each group by
`AttributeBlend::prominence` (1.0 at the loudest share, ~0.6 at the faint end) and
gating the *extra* layer of a group (a core crystal, a second circuit run, a mid
spine, an inner halo) behind a 0.9 share — the detail-trimming order uses the
same shares, so a minor contributor loses its ornament before the dominant one
does. The palette is a weighted mix of the three tints, so the skin is neither
pure Prism nor pure Feral. A one-hot blend is exactly the old single-attribute rig
(asserted byte for byte), which is what keeps hatchlings and legacy saves
unchanged.

**Names carry the rig.** A model id is
`buddy-<archetype>-<stage>-<attribute>-<seed>`; the attribute is part of the name
because it is part of the shape, and two attributes of one seed must not share a
`models/<slug>/` folder. Saved games are migrated on load — the slug is
re-derived, never trusted.

**Mirroring is arithmetic, never hand-typed.** Only the centerline
(`root`, `pelvis`, `torso`, `neck`, `head`, `tail`) and the right-side chains
(`arm_r`, `leg_r`, `horn_r`, `wing_r`) are authored; `mirror_chain` produces the
`*_l` twins, remaps each descendant's parent to the **left** twin, and negates
part offsets/rotations because a mirrored bone's local frame has the opposite
handedness. The clips then key `+a` on `_l` and `−a` on `_r`.

**Routes** (declared in `routes.rs`, served through the plugin proxy):

| route | what it does |
| --- | --- |
| `GET /creature/status` | stage, XP, bond, attribute, the recency-weighted `attribute_blend`, the live rig's `bones`/`parts`/`parts_budget`/`min_world_y`/`symmetry_error`, the three branches (each with its projected blend) and the transition history |
| `POST /metamorphose` | `{branch?, force?}` → generate the next stage from the extended lineage, `host_model_write`, `host_model_activate` (the `POST /model` path), `host_animate("metamorphose")`, then one `creature` event; the reply carries the shifted `attribute_blend` |
| `POST /revert` | `{steps?}` → regenerate the **parent** stage from the history entry's seed/attribute (deterministic, so the exact earlier rig comes back), write it, activate it, play the clip, emit `reverted`; `409` at Fresh |
| `POST /creature/xp` | `{xp, bond}` — grant growth directly |
| `POST /creature/reset` | `{seed?, archetype?, attribute?}` — a new creature plus its `Fresh` rig, installed **and activated**; omitted fields inherit the current creature |

State lives in `plugins/metamorphosis.state.json` through `host_state_save`. The
current stage's rig is (re-)published at every `init` — `installed` is a
per-process hint, never a saved claim, because the guest cannot stat a file and
a deleted `models/` folder must not leave the plugin pointing at nothing — but
it is **never activated**, because taking over the user's avatar on startup is
not a plugin's call. An explicit `POST /creature/reset` *does* activate: the user
asked for a new creature, so leaving the old body up until the first evolution
would look like nothing happened.

`tools/verify_docs.py` reads the route table out of the guest sources
(`plugins/*/src/*.rs`, the `ROUTES` const) as well as `api.rs` and
`src/plugins/**`, so the documented endpoints follow the plugin.

### `src-tauri/src/discovery.rs`

The blocking half of **Discover models** (`POST /ai-discover`). Nothing here
writes anything: it probes, and the modal decides what to import.

| Item | Responsibility |
| --- | --- |
| `DISCOVERY_TIMEOUT_MS`, `MAX_GGUF_DEPTH` | 1.5 s per request; the `.gguf` walk depth (3) |
| `port_in_url` | the port a profile endpoint already names (`…:8001/v1` → 8001), so `discovery_ports` can include it |
| `Target`, `split_host_port`, `normalize_host` | one `(host, port)` pair; a `discovery_hosts` entry is cleaned here (trim, no `http(s)://`, no path, dedupe) and `192.168.0.36:11434` keeps its own port. `label` / `is_loopback` / `authority` give the modal its `:8001` vs `host:port` badge and a bracket-correct URL for IPv6 |
| `targets` | cross `discovery_hosts` with `discovery_ports` (a `host:port` entry is one target), preserving order and dropping duplicates |
| `openai_rows`, `ollama_rows` | parse `{data:[…]}` / `{models:[…]}` (OpenAI, llama.cpp, vLLM) and Ollama's `{models:[…]}`; `multimodal` comes from `architecture.input_modalities` or a `clip` family |
| `probe_target` | per target (resolved through `to_socket_addrs`, so hostnames work): `v1/models`, then the root-mounted `models`, then `api/tags` — one `reqwest::blocking` client with `no_proxy`, so a dead target is a fast `None` and a wrong-protocol port is one timeout |
| `gguf_dirs`, `walk_gguf`, `gguf_models` | `.gguf` weights under `%LOCALAPPDATA%\hermes\models` (plus the `USERPROFILE` / `$HOME` fallbacks), reported with `path` and `size_bytes` so an unloaded model is still visible |
| `discover` | the whole `POST /ai-discover` body: `{ok, summary, elapsed_ms, timeout_ms, targets:[{host, port, label, loopback, reachable, detail}], ports:[the same], models:[…], gguf_dirs}`. Targets are probed in parallel threads, one 1.5 s budget each |

Every model row is `{id, label, endpoint, host, port, loopback, source,
owned_by, multimodal}` (`source` is `openai_compat`, `ollama` or `gguf`; a
`.gguf` row also carries `path` / `size_bytes` and a `null` port). `endpoint` is
always the URL a profile should store — `http://<host>:<port>/v1` — because that
is the prefix the watcher's `chat/completions` call hangs off, and `host` is what
lets the modal show a LAN/Tailscale hit differently from loopback. Unit tests
serve a one-shot `TcpListener` and pin the endpoint, the multimodal flag, a
silent target, the host normaliser and the host×port cross product; no model
server is needed.

### `src-tauri/src/models.rs` (1081 lines)

The shipped rigs (`models/demo-blob`, `models/nebulon-9`) carry a baseline
**reaction clip library** next to the originals (`wave`, `breathe`, `scan`,
`alert`): `nod`, `shake_head`, `celebrate`, `alert_ping` and `idle_fidget`. They
are ordinary `animations` entries — validated by `validate_animations` like any
other clip — so `GET /animations` lists them and an `openai_compat` model can ask
for one with `{"animate": "nod"}` without any new plumbing.

| Function | Responsibility |
| --- | --- |
| `models_root` | `DESKTOP_BUDDY_MODELS` → next to the exe (`models`, `../models`, `../../../models`, `../../../../models`) → compile-time `<repo>/models` |
| `load_selection` / `save_selection` | read/write `models/.state.json` (`{"current": "<name>"}`) |
| `validate_rig` | bones array non-empty, unique ids, parents exist, no cycles, every part has `id` and (`image` or `shape`), plus `validate_animations` → `(bones, parts)` |
| `validate_animations` | optional `animations` block: object of clips, `duration_ms > 0`, boolean `loop`, `keyframes` array, every `t` in `0.0..=1.0`, every referenced bone id exists, transform fields finite |
| `animation_summary` / `find_animation` | `GET /animations` entries (`{name, duration_ms, loop, keyframes}`, sorted) and one-clip lookup for `POST /animate` |
| `normalize_rig` | TOML has no null: `parent: "" | "null" | "none"` → `null` |
| `part_images` | collects `part.image` **and** every `variants` path |
| `resolve_inside` | rejects absolute paths and `..` escapes |
| `inline_images` | reads each sprite (≤ `MAX_IMAGE_BYTES` = 16 MiB) → `data:<mime>;base64,…` |
| `load_model` | prefers `rig.json`, falls back to `model.json`, merges identity keys, validates, inlines images |
| `list_models` | per-folder summary; broken folders report `error` instead of failing the list |
| `save_rig` | validate + write `rig.json.tmp` then rename (atomic) |
| `clear_rig` | delete `rig.json` (the "reset") |
| `import_model` | parse `.json`/`.toml`, validate, copy tree into `models/<slug>` (collisions `-2`, `-3`), write a normalised `model.json` |
| `detect_kind` / `AssetKind` | `.json`/`.toml` → `Rig`; `.png/.jpg/.jpeg/.webp/.svg` → `Image`; anything else → error |
| `stage_image` | copy a sprite into `models/<current>/parts/` with a unique name → `{image, name, size, data_url}` |
| `create_model_from_image` | build a one-bone model (`root` + part `body`) from an image |
| `import_asset` | dispatch used by both the command and `POST /import` |
| `tests` | 9 unit tests over the animation schema (opt-in block, `duration_ms`, `t` range, unknown bones, malformed shapes) |

### `src-tauri/src/main.rs` (6 lines)

Calls `desktop_buddy_lib::run()`; sets `windows_subsystem = "windows"` in release builds.

### `src-tauri/src/bin/desktop-buddy-mcp.rs` (1300 lines)

The **second binary** (`desktop-buddy-mcp.exe`), built by the same `cargo build`. It is
independent of the Tauri lib: a stdio MCP server that owns no state and forwards
each tool call to the HTTP API with `reqwest::blocking`.

| Section | Responsibility |
| --- | --- |
| consts | `DEFAULT_API_URL`, `ENV_API_URL` (`DESKTOP_BUDDY_API_URL`), `CONNECT_TIMEOUT` 3 s, `REQUEST_TIMEOUT` 10 s, `EXPRESSIONS` (mirrors `AVATAR_EXPRESSIONS`), `SUPPORTED_PROTOCOLS`, `INSTRUCTIONS` |
| `ApiError` / `Api` | blocking client + error taxonomy: `Unreachable`, `Timeout`, `Transport`, `Http{status,message}`, `Decode`; `get`/`post`/`send`/`classify` |
| argument helpers | `req_str`, `opt_str`, `opt_expression`, `opt_u64`, `opt_bool`, `req_number`, `req_object` — each failure is a model-readable message |
| `tool_content` / `tool_failure` | build the MCP result envelope (`content[]`, `isError`) and the `{ok:false, kind, error, hint, details}` payload |
| `TOOL_NAMES` / `tools()` | the 14 tool definitions with JSON Schemas and descriptions |
| `tool_*` | one function per tool; `tool_get_context` merges `GET /context` + `GET /state` and adds `window_rect`, `visible_windows` (as `Code.exe@(400, 300) 800x500 | …`), the `perched` flag and the `recent_events` history (summarised as `type: hint` pairs), `tool_perch` validates `edge` locally then forwards, `tool_move` passes the optional `look_at`, `tool_get_clipboard` forwards `GET /clipboard`, `tool_set_clipboard` forwards `POST /clipboard` (empty string clears), `tool_speak` passes the optional `details` drawer text through, `tool_animate` maps `stop`/`null` onto `POST /animate {"name": null}`, `tool_switch_ai` validates a non-blank `name` locally then forwards `POST /ai-profile` (the API answers 400 with the valid names), and `tool_get_context` also reports `active_profile`, `ai_profiles` and `active_health` |
| `handle_message` | JSON-RPC dispatch: `initialize`, `ping`, `tools/list`, `tools/call`, notifications, `-32601`/`-32602`/`-32700` |
| `main` | read stdin line-by-line, one JSON frame per line out, flush, clean exit on EOF |
| `tests` | 12 unit tests: the tool table (14 names, each with a schema and a `run_tool` arm), local argument validation (expression, speak, perch, look_at, clipboard, `avatar_switch_ai`), the error envelope, and the context summary helpers |

Protocol notes worth keeping: stdout carries **only** JSON-RPC frames (all
diagnostics go to stderr, or the client's parser breaks), a missing avatar is a
tool *result*, and a bad line is a JSON-RPC error — the loop always survives.

### `src-tauri/src/bin/desktop-buddy-watcher.rs` (3340 lines)

The **third binary** (`desktop-buddy-watcher.exe`), built by the same `cargo build`.
It owns no UI and no state of its own: a headless trigger pump that reads the
avatar's `GET /events` stream and polls `GET /context`, and only reaches out
when a rule fires. With no trigger it spends no tokens — the whole idle cost is
one open SSE response and one small JSON poll every 20 s.

```text
  desktop-buddy.exe ──SSE /events────┐
                     ──GET /context─┴──▶ desktop-buddy-watcher ──▶ POST /emote (ack)
                                                          ──▶ --webhook-url
                                                          ──▶ ai_profile (backend)
                                                                ├─ openai_compat ──▶ POST /v1/chat/completions
                                                                │                   └─▶ /perch + /emote + /speak
                                                                └─ hermes_cli ─────▶ spawn (… {session} …)
```

| Section | Responsibility |
| --- | --- |
| consts | `DEFAULT_API_URL`, `ENV_API_URL` (`DESKTOP_BUDDY_API_URL`), `ENV_HERMES_CMD` (`DESKTOP_BUDDY_HERMES_CMD`), `DEFAULT_RULES_FILE`, `DEFAULT_CONTEXT_INTERVAL_SECS` 20, `DEFAULT_INTERACTION_COOLDOWN_SECS` 15, `DEFAULT_CONTEXT_COOLDOWN_SECS` 300, `DEFAULT_ALERT_COOLDOWN_SECS` 60, `DEFAULT_IDLE_THRESHOLD_SECS` 900, `DEFAULT_ACK_EMOTE` `thinking`, `ACK_EMOTE_DURATION_MS` 4000, `DEFAULT_HERMES_TIMEOUT_SECS` 120, `FALLBACK_SPEECH_TEXT` / `FALLBACK_SPEECH_EXPRESSION` `sad` / `FALLBACK_SPEECH_DURATION_MS` 4000 (the bubble the watcher posts itself when a dispatch fails), `EXPRESSIONS` (mirrors `AVATAR_EXPRESSIONS`, decides emote vs. clip ack), `HEADLESS_DIRECTIVE`, `CONNECT_TIMEOUT` 3 s, `CONTEXT_TIMEOUT` 5 s, backoff 1 s → 30 s, `TRIGGER_QUEUE` 64 |
| `CliConfig` / `Config` | raw flags vs. resolved runtime config; `Config::resolve` merges **CLI → env → rules file → built-in default** (`hermes_timeout` = `--hermes-timeout` → `hermes_timeout_secs` → 120; no env var) |
| `parse_cli` / `usage` | hand-rolled flag parser (`--flag value` and `--flag=value`), `-h/--help`, exit 2 on bad input; env fallbacks applied after the flags |
| `InteractionRules` / `ContextRules` / `AppRule` / `AlertRules` / `Rules` | serde-defaulted rule model (`app_rules` and `alerts` optional, so the old schema still parses); `Rules::load` (explicit path, else cwd then next to the exe), `from_json`, `interaction_allowed`, `matched_keyword` (case-insensitive substring), `matched_app_rule` (process + title, first match wins, blank matcher = wildcard), `idle_transition` (`was_idle && idle < threshold`) |
| `Cooldowns` | per-key `HashMap<String, Instant>`; `allow` checks **and** stamps, so a suppressed trigger does not extend the window |
| `SseParser` | incremental `text/event-stream` parser: `event:`/`data:` fields, multi-line `data`, `:` keep-alive comments, CRLF, and frames split across arbitrary TCP reads; `message` is the default event name |
| `Trigger` | `{event_type, detail, window_title, idle_secs, kind, keyword, x, y, animation, hint, text, severity}`; `payload(ts_ms)` builds the webhook / dry-run JSON (absent fields omitted), `brief()` builds the one-line situation (what an OpenAI-compatible model gets), `prompt()` is that plus the CLI's `HEADLESS_DIRECTIVE` — quoting a `user_reply`'s text verbatim and appending an `app_rules` `prompt_hint` as `Context:` — and ends with `HEADLESS_DIRECTIVE`: no follow-up questions, **`tool_search(query="avatar_")` as the first tool call of a session** (Hermes defers MCP tools, so `mcp__desktop_buddy__avatar_*` is not callable until the search loads the schemas — a model that skips it retries the "deferred tools must be invoked through tool_call" error until `--hermes-timeout` kills it), the literal `mcp__desktop_buddy__avatar_speak(text, expression?, duration_ms?)` / `avatar_emote(expression, duration_ms?)` (with the six expressions) / `avatar_wave()` / `avatar_animate(name, loop?)` / `avatar_get_context()` / `avatar_get_clipboard()` / `avatar_perch(edge, window_process?, offset?)` signatures, an explicit invitation to chain several calls in one turn, and the **under-180-characters** bubble rule so a bubble never overflows the screen |
| `interaction_trigger` | gate on `interaction_allowed` + cooldown, then format `the user double-clicked the avatar at (x, y)` |
| `user_reply_trigger` | `user_reply` frame → trigger with the user's trimmed text; **no cooldown and no interaction gate** — the user asked directly |
| `alert_trigger` | `alert` frame → trigger with `source`/`summary`/`severity`; honours `alerts.enabled` and the `alert` cooldown |
| `ack_plan` | picks the immediate ack: a matched `app_rules[].animation` (an expression name → `Ack::Emote`, anything else → `Ack::Animate` clip), then an `alert`'s severity (`error`/`critical` → `sad`, `warn` → `surprised`), then the generic `--ack-emote`; `""` disables it. The ack always fires **before** the model turn, so the AI's own `expression` replaces it rather than the other way round |
| `context_triggers` | evaluates one poll: title-change matching `keyword OR app_rule` (the app rule wins when both match and supplies `animation`/`hint`), and the idle→active rule; also maintains `last_title` / `last_idle_secs` / `was_idle`; the **first** poll only sets the baseline (`initialized`) |
| `Shared` | `Mutex`-guarded snapshot: latest `/context`, `initialized`, `last_title`, `last_idle_secs`, `was_idle`, `cooldowns`; `lock` recovers from poisoning |
| `SingleFlight` / `FlightGuard` | one-permit `Arc<AtomicBool>` latch (`compare_exchange`) guarding the whole dispatch, plus a one-slot `Mutex<Option<Trigger>>` pending queue and a `Notify` the guard pings on drop; `offer` returns `Immediate`/`Queued`/`Dropped`, so only `user_reply`/`alert` queue while busy. The guard releases the permit on `Drop`, so a panic in a spawned task cannot wedge the watcher |
| `split_command` / `command_argv` / `spawn_hermes` | quote-aware argv split (no shell), `{prompt}` substitution or append-as-last-arg, then an `async fn` `tokio::process` spawn with `stdin(Stdio::null())` (an interactive prompt reads EOF instead of hanging), `kill_on_drop`, and `tokio::time::timeout(--hermes-timeout)` around the wait — an expired child is `start_kill`ed and reaped. The `FlightGuard` moves into the spawned task, so the permit covers the process lifetime. The task ends by posting the **fallback bubble** for any non-clean ending (see below) |
| `RunOutcome` / `fallback_speech_body` / `fallback_speech_for` / `deliver_fallback` | how the child ended (`Succeeded`, `Failed` = non-zero exit, `TimedOut`, `WaitFailed`, `SpawnFailed`); every outcome but `Succeeded` posts `{"text": "Sorry, I hit a snag or timed out.", "expression": "sad", "duration_ms": 4000}` to `POST /speak` before the permit drops, so a crash, a bad argv or a wedged model (e.g. the deferred-`tool_search` retry loop) can never leave the overlay on "Thinking…". `POST /speak` is the same route `avatar_speak` uses, and an unreachable avatar is logged, never propagated |
| `AiProfile` | the tagged (`#[serde(tag = "kind", rename_all = "snake_case")]`) inference target: `OpenaiCompat { label, endpoint, model, api_key?, max_tokens?, timeout_secs?, system_prompt? }` or `HermesCli { label, cmd, timeout_secs? }`; `kind`, `label`, `endpoint`/`model`, `timeout` and `is_cli` are the only accessors the rest of the file needs |
| `AiRuntime` / `AiShared` | which profiles exist and which is active: `from_rules` synthesises a `hermes` / `hermes_cli` profile from a legacy `hermes_cmd` (and keeps it as the escalation target when the file also carries profiles), `resolved(override)` applies an `app_rules`/chip `profile` then falls back to `active_profile`, `cli_fallback` finds the agent, `set_active` validates. `AiShared` wraps it in a `Mutex` next to the rules path, so an `ai_profile` frame can `reload()` the edited file and swap the runtime without a restart |
| `AiReply` / `AvatarAction` / `truncate_chars` | the JSON contract (`speech`, `expression?`, `perch_edge?`, `window_process?`, `animate?`, `wave?`, `details?`, `escalate?`): `parse` tolerates ```json fences and leading prose, lowercases and validates `expression` against `EXPRESSIONS`, perch edges against `PERCH_EDGES`, trims `animate` (blank ignored), and drops unknown values; `actions_with_clips(&clips)` fixes the order perch → emote → wave/animate → speak with the bubble cut to 180 chars (`details` capped at 8000). `wave: true` outranks `animate`; `animate: "wave"` becomes `AvatarAction::Wave` unless the rig lists its own `wave` clip (`has_clip`, case-insensitive) |
| `InferenceBackend` + two impls | the dispatch seam: `fn dispatch(&self, DispatchCtx) -> Pin<Box<dyn Future<Output = DispatchReport> + Send>>` (boxed futures instead of an `async_trait` dependency). `OpenaiCompatBackend` normalises the endpoint to the server's chat-completions path, **inlines a fresh `/context` snapshot** (and the clipboard when the prompt asks for it — a no-tools model cannot call the MCP tools, and the clipboard is private until the user asks), inlines the rig's clip names from `/animations` (so the model knows what `animate` may name), sends one `response_format: {"type":"json_object"}` request, then drives `/perch` + `/emote` + `/wave` or `/animate` + `/speak`; a rejected `animate: "wave"` degrades to `/wave` (a rig without that clip still gestures) and a request failure posts the sad fallback bubble. `HermesCliBackend` is the old `spawn_hermes` runner. `DispatchReport {backend, kind, actions, escalated, failure}` is what gets logged (and tested) |
| `apply_actions` / `post_fallback_speech` | the HTTP side effects, in `AvatarAction` order, each failure logged and swallowed so one rejected `/perch` cannot abort the `/speak`; `post_fallback_speech` is the shared "unstick Thinking…" bubble |
| `VisionRules` / `CaptureMode` / `matches_trigger` / `wants_vision` | the `vision` block of the rules file, `#[serde(default)]` at the container *and* the field level so a pre-vision rules file still loads: `enabled`, `profile_name`, `capture_mode` (`active_window` \| `full_screen`, `as_str()` is what the log and the prompt name), `max_image_dim` (clamped 64–4096), `jpeg_quality` (clamped 1–100), `trigger_phrases` (NFKC-ish normalisation: lowercased, apostrophes dropped, whitespace collapsed, so `what's`/`whats` and curly quotes all match). `wants_vision` is `enabled && (trigger.include_screen || matches_trigger(text))` — routine triggers (`interaction`, `window_title`, `idle_return`) never capture |
| `vision_request_body` | one OpenAI-shaped multimodal body: a system message that keeps the profile's own `system_prompt` (or a built-in "eyes of a small desktop avatar" one) and appends the same JSON reply contract the text backend uses, then a user message whose content is the two-part array `[{"type":"text",…},{"type":"image_url","image_url":{"url":"data:image/jpeg;base64,…"}}]`, with `response_format: json_object` and the profile's `max_tokens` when present |
| `capture_screen` / `screen_bytes` / `rect_from_context` (in `src-tauri/src/bin/capture/mod.rs`) | GDI capture: `GetDC(NULL)` + `CreateCompatibleDC` + `CreateCompatibleBitmap` + `BitBlt`, then `GetDIBits` into a top-down BGRA buffer. `/context` reports CSS pixels, so `rect_from_context` converts back to physical screen pixels with the context's `screen` object (`physical = css * scale + origin`) — a left-hand monitor yields negative physical coordinates, which is exactly what GDI wants. `ActiveWindow` clamps that rect to the virtual screen and *refuses* a rect that is the overlay's own window (the avatar must look at the user's app, not itself), `FullScreen` uses the primary monitor; the buffer is resized with `image` so `max(w, h) <= max_image_dim`, JPEG-encoded in memory, base64-encoded, and the temp buffer is dropped — nothing is left on disk |
| `handle_trigger` | dry-run print → take the single-flight permit → the ack (`ack_plan`: `POST /emote` or `POST /animate`) → `POST --webhook-url` → resolve the profile (`trigger.profile` → `active_profile`) → **when `trigger.wants_vision(&rules.vision)`** take a screenshot, resolve the *named* vision profile (`AiRuntime::named`, no fallback to the active text profile) and send the multimodal body, else → `backend_for(...).dispatch(...)`; then, when the reply said `escalate: true`, a second flight through the `hermes_cli` profile; a trigger that finds the permit taken is **skipped** with a `debug:` line on stderr instead of stacking another model turn |
| `build_client` / `fetch_context` | reqwest client with a 3 s connect timeout; `GET /context` → `ContextSnapshot {window_title, process_name, idle_secs, window_rect}` (an older avatar without `process_name`/`window_rect` still works — a missing rect just means full-screen capture) |
| `handle_frame` | one SSE frame → at most one trigger; routes `interaction`, `user_reply` and `alert`, ignores `hello`, unparsable data and every other frame type |
| `stream_events` / `sse_loop` | consume `Response::bytes_stream()`, feed the parser, reconnect forever with doubling backoff capped at 30 s (reset on a successful subscribe) |
| `context_loop` | `tokio::time::interval` with `MissedTickBehavior::Delay`; one warning per offline streak, then silent retries |
| `dispatcher` | single consumer of the `mpsc` channel plus the `SingleFlight` latch, so two triggers can never race their side effects and a running child is never duplicated; `select!`s the channel against the latch's `finished` `Notify`, and drains the pending slot as soon as a flight ends |
| `main` | parse → load rules → spawn `sse_loop`, `context_loop`, `dispatcher` → `tokio::select!`; any task ending stops the process |
| `tests` | 128 unit tests: SSE parsing (chunk boundaries, comments, CRLF, multi-line data), rule matching, per-app rule matching (case-insensitive, blank wildcards, `animation`/`prompt_hint` carried onto the trigger), alert parsing/cooldowns/disable switch, the backward-compatible rules schema, cooldown suppression, `user_reply` triggers (no cooldown, quoted text), `handle_frame` routing for `user_reply`/`alert`, `ack_plan` (expression vs. clip vs. none), trigger payloads/prompts, argv splitting, CLI precedence, `--hermes-timeout` parsing + default + `hermes_timeout_secs` precedence, the headless directive (clipboard, context, chaining, 180-char cap) and its tool signatures, the session id/rotation helpers (`{session}` substitution, max-turns, idle reset, reset frames), the single-flight latch and its one-slot pending queue (immediate/queued/dropped offers, latest-wins, the wake-on-drop `Notify`), the deferred-MCP `tool_search` hint (present, ordered before the first tool signature, underscore tool names), the failure recovery (`fallback_speech_for` only for non-clean outcomes, a captured `POST /speak` body from `deliver_fallback`, nothing posted on success, and a real `python -c` child killed at a 1 s timeout that still posts the bubble and releases the permit), plus the AI profiles: the tagged `AiProfile` schema, the synthesised legacy `hermes` profile (with its timeout), active-profile resolution and per-trigger overrides, `set_active` validation, the escalation target, `app_rules`/`quick_tasks` profile pinning, `AiReply::parse` (plain / fenced / padded JSON, unknown enum values dropped), action ordering + the 180-char bubble, `chat_url` normalisation, the request body (`response_format: json_object`, `max_tokens`, system contract), a full chat-completions request → `/perch` → `/emote` → `/speak` run against a captured fake server, the sad fallback bubble on a dead endpoint, the escalating reply, an `ai_profile` frame re-reading the rules file, `Trigger::brief` vs `Trigger::prompt` (the CLI directive never reaches a no-tools model, and the word "clipboard" no longer leaks into an unrelated prompt), the no-tools prompt (`augmented_prompt` inlining a fresh `/context` snapshot, `mentions_clipboard` deciding *before* the request whether the clipboard belongs in it — and never reading it when the user did not ask), plus a guard test that the fake-server helper really answers a plain GET, the vision block (`#[serde(default)]` defaults, a rules file with no `vision` key and a partial block, `full_screen` + custom phrases, case/apostrophe/whitespace-insensitive phrase matching, `enabled:false` and routine triggers never capturing, the multimodal body's two-part content and `max_tokens` handling, and `include_screen` on a reply frame), the expression-selection contract (every enum name plus the happy/sad/surprised/thinking/neutral guidance in `AI_REPLY_CONTRACT`, the vision prompt and `HEADLESS_DIRECTIVE`, the survival directive's one-face rule, an AI expression reaching both `Emote` and `Speak`, an invalid one dropped rather than guessed, alert-severity acks with app rules still winning), and the native-gesture path (`animate`/`wave` parsing, gesture-before-speech ordering, `animate: "wave"` resolving to the built-in wave, a real `POST /wave`, a real `POST /animate {"name":"dance","loop":false}`, and a rejected clip falling back to `/wave` without swallowing `/speak`) |

Protocol notes worth keeping: diagnostics go to **stderr** and stdout carries
only `--dry-run` payloads (so the binary stays pipeable), the prompt is always a
single argv entry so it can never be re-interpreted as shell syntax, and the
first `/context` poll is a baseline — the watcher must not fire on the window it
starts with. Dispatch is **single-flight and headless**: at most one trigger's
side effects (including a live `--hermes-cmd` child) run at a time, the child's
stdin is the null device (`Stdio::null()`, so an interactive prompt reads EOF),
and a child that outlives `--hermes-timeout` (120 s by default, `hermes_timeout_secs`
in `watcher_rules.json` to tune it) is killed. **Every dispatch ends visibly**: the
only ending that leaves the bubble alone is a clean exit 0, because that is the
one case where the agent itself had a turn to call `avatar_speak`; a timeout, a
non-zero exit or a failed spawn makes the watcher post its own sad bubble.
The prompt likewise starts with `tool_search(query="avatar_")` because Hermes
**defers MCP tools per session** — the names are not callable until the search
loads their schemas, and a model that retries the raw call instead burns the
whole `--hermes-timeout` without ever answering.

### `ui/` — modular frontend (no build step)

The frontend is split into native ES modules under `ui/` with no bundler:

| File | Responsibility |
| --- | --- |
| `ui/index.html` | Lean semantic HTML shell (titlebar with the chat icon, canvas, control panel, inspector container, import chooser, floating command bar, speech bubble with its quick-reply form, the **History** drawer, toasts) linking `styles.css` and `<script type="module" src="./js/main.js"></script>`; the control panel carries the **Watcher** row, the **AI Brain** row (`#ai-profile-select` + `#ai-health-badge`) and the **History** button |
| `ui/styles.css` | All CSS rules (`html`/`body`/`canvas` reset to zero margin/padding/border/outline/overflow so no edge line or focus ring can appear, titlebar, control panel, rig inspector, import modal, speech bubble + quick reply + thinking state, command bar + chips, the `.ai-row` dropdown and its `.ai-health` dot with `online` / `offline` / `cli` variants, toasts) |
| `ui/js/ipc.js` | **Leaf** Tauri access: `call`, `listen`, `once`, `isTauri`. Reads `window.__TAURI__` lazily on every call (never cached at module scope) |
| `ui/js/plugins.js` | **Leaf** plugin-backed UI gate: `setupPluginGate` (one `GET /plugins` read, retried while the API is still binding, plus a `plugins` event listener), `applyPluginStatus`, `pluginActive` (only `active` counts — a failed or parked plugin does not serve its routes), `pluginStates` and `gatePluginUi`, which shows/hides every `data-plugin="<id>"` element. The Survival / Metamorphose control-panel buttons and the Settings Survival tab ship **hidden** in `index.html`, so a missing `.wasm` removes the option instead of leaving a dead end |
| `ui/js/state.js` | **Leaf** shared state objects (`avatar`, `RT`, `speech`, `gaze`, `viewport`, `watcher`, `ai` — the last mirroring `{active, profiles, health, checked}` for the **AI Brain** row), constants (`EXPRESSIONS`, `PRESET_STATES`, `PART_TAGS`), helpers, `toast`, `updateStatusUI`, `setExpression`, the expression lifecycle (`EXPRESSION_SETTLE_SECS`, `reactExpression` — a program/event reaction with a hold that reverts to neutral, `settleExpression` — the per-frame ease back to neutral, guarded by holds, bubbles, walk/wave and STT state), `snapshot`, `setDirty`, `activeState` (which routes the lip-sync mouth variant through `mouthState`), `blinkActive`, `markManual` |
| `ui/js/rig.js` | Model ingestion (`applyModel`, `boneDefaults`, `rebuildOrder`, `loadImages`, `refreshModelList`), forward kinematics (`animOffset` — the hover head tilt rides on it, and `rummageOffset` is the procedural dig the scavenging activity blends in through `avatar.rummage`, `clampRotation`, `animationOffset`, `localRotation`, `computeWorld` — plus `computeWorldInto(ctx, origin)` so a **prop entity** evaluates the same maths from its own root), geometry (`partSize`, `sortedDrawables`, `partWorldMatrix` — the one world matrix the renderer and the editor both draw through, `partLocalRect` — how each shape type is anchored inside that matrix, `partBox`, `partWorldMatrixIn` for a prop's private world), **sockets** (`socketWorld`, `socketTransformIn`, `socketList`, `socketById`, `makeRigContext` — a private, `lifeSigns: false` context for a prop rig), hit-region publishing (`avatarBounds`, `wireRegions` — which also publishes the visible speech bubble and the open command bar so both stay clickable), and rig serialization (`serializeRig`, `serializeAnimations`, `saveRig`, `resetRig`) |
| `ui/js/entities.js` | **Prop entities** on the canvas: `refresh` (`GET /entities`, coalesced), `stepEntities` (per-entity clock + state→clip, with `animator.js::sampleClip`), `resolveRoot` (avatar socket, `entity:<id>` nesting, the `stage` ground, depth 4 + cycle guard, and the `attach.flip` mirror parity), `entityDrawables` (z-sorted parts in the avatar's own units), `entitySnapshot` (the `__avatarDebug.entities()` accessor, including each root's `mirror`), `markDirty` (fed by the `entity-*` frames) |
| `ui/js/animator.js` | Custom keyframe clips: catalogue (`animationClips`, `listAnimations`), transport (`playAnimation`, `stopAnimation`, `stopPlayback`, `seekAnimation`, `stepAnimation`), sampling (`sampleAnimation`), authoring (`setAnimationBone`, `captureKeyframe`, `addClip`, `renameClip`, `deleteClip`, `setClipDuration`, `setClipLoop`) and finished listeners (`onAnimationFinished`) |
| `ui/js/renderer.js` | Canvas pipeline: DPR resize (`initRenderer`, `resizeCanvas`), `drawFace`, `drawShape`, `drawPart`, rig overlay gizmos, `render`; `updateGazeDir` turns the smoothed aim point into a clamped pupil vector and `isPupilPart` decides between moving the pupil sprite or the whole eye; the mouth squash reads `speech.mouthSmooth` (the same cadence as the sprite variants) |
| `ui/js/editor.js` | Rig Edit Mode: `hitTest`, mouse/wheel handlers (wheel rotates a held part), inspector (`buildInspector`, `renderInspector`, `updateInspectorValues`), bone/part authoring (`addChildBone`, `deleteBone`, `addPartFromControls`, `addLimbFromControls`, `duplicatePart`, `deletePart`, `snapBoneToPart`, `bindExpressionSprite`, `attachStagedPart`), the animation timeline (`buildTimeline`, `updateTimeline`, `syncTimelinePlayhead`), and `undo` |
| `ui/js/speech.js` | Speech bubble lifecycle (`speak`, `updateSpeech`, `positionBubble`, `hideSpeech`), **audio-driven lip sync** (`setAudioEnvelope` / `clearAudioEnvelope` / `hasAudioEnvelope` — `updateSpeech` indexes the Piper RMS envelope by `performance.now() - envelopeAt` and falls back to the character table when the voice is muted, missing or done) on top of the **text-driven** mapping (`mouthPoseFor` vowel/consonant/punctuation table, `setMouthFrom`, `stepMouth` smoothing), the quick-reply box (`isReplyOpen`, `openReply`, `closeReply`, `toggleReply`), the **details drawer** (`detailsText`, `toggleDetails`, `detailsOpen`, `copyDetails` — which republishes the hit region so the expanded card stays clickable), the "Thinking…" card (`beginThinking`, `showThinking`, `endThinking`, plus the `THINKING_TIMEOUT_MS` 125 s safety net that clears a stuck card and puts the face back to `neutral`) and the command bar's anchor (`commandBarVisible`, `positionCommandBar`); the hold timer is suspended while the drawer is open, the box is open, the bubble is hovered or an input has focus |
| `ui/js/bridge.js` | Tauri event listeners (`avatar-state-changed`, `emote`, `speak`, `part-patch`, …), `reportState`, `applyRemoteState` (ignores frames that would yank a dragged avatar and never adopts a bare `walking` without its segment), `applyPartPatch`, `applyWatcherStatus` (which also mirrors `quick_tasks` for the command bar and wears a `surprised` face when a switched-on watcher stops running), `applySttStatus` (`transcribing` raises a `thinking` face — listening is not a mood), the **AI Brain** layer (`applyAiProfiles` — rebuilds the dropdown only when the profile set changes, so the 12 s health refresh cannot collapse an open select, `renderAiProfiles` for the badge/dot, `refreshAiProfiles`, `setAiProfile`, `aiHealthStatus` — a profile that drops from online to offline wears a `sad` face, only on the transition, `aiActiveLabel`), click-through (`setClickThrough`), import flow (`importFromDialog`, `importImageAs`, chooser) |
| `ui/js/settings.js` | The **Settings** modal (`#settings-modal`, three tabs): a declarative `FIELDS` spec drives the whole form (label, dotted `section.key`, input type, Browse… `kind`), `renderSettings` paints the `GET /config` body including the per-path ✓/✗ marks and the *stray key* warnings, `collectPatch` diffs the form against the loaded body so a save sends only what changed, and `save` reports `applied` vs `restart_required` from `POST /config`. The **AI brain** tab is not declarative: profiles are a list, so it keeps a working copy in `ai` (loaded entries plus unsaved drafts flagged `isNew`, a `removed` queue and the two dropdown selections) and repaints cards through `renderProfileCards` / `refreshProfileOptions`; `addProfile` / `removeProfile` (inline guards: last profile, active profile, vision profile) / `importModel` (from `discoverModels` → `POST /ai-discover`) all edit that copy, and `collectPatch` turns it into `ai.create` / `ai.profiles` / `ai.rename` / `ai.remove` |
| `ui/js/main.js` | Entry point: DOM bindings, keyboard shortcuts (`Enter`/`/` summon the bar, `H` toggles the History drawer), the plugin gate (`setupPluginGate` runs before `setupSurvival` / `setupMetamorphose`, which are only set up while their plugin is `active`), the `summon-command-bar` hotkey listener (opens the bar and restores click-through when it closes), **gaze targeting** (`attachGazeTracking` — page `mousemove` plus the global `cursor-pos` stream; `updateGaze` — `look_at` > focused command bar > perch > global cursor (4 s) > idle glances, plus the eased hover weight and the clamped head-bone yaw bias), `window.__avatarDebug`, the **AI Brain** select (`setAiProfile` + a toast, with `refreshAiProfiles` every 12 s beside the watcher poll), behaviour loop (`step`, `beginWalk`, `startWalk` — which bumps `avatar.manualMoves`, so any manual or agent-driven walk outranks the scavenging route — `wander` — held back while a bubble is up — and `stepScavenge` / `beginScavengeWalk` / `nextScavengeLeg` / `endScavengeWalk`, the walk between open windows with a dig at each), `requestAnimationFrame` driver (`frame`: `step` → `render` → `positionBubble`), play-mode click / double-click / drag-to-move detection (`attachInteractionDetection`, `clampAvatarPosition`) that feeds `notify_interaction`, and the ask/task surface (`setupCommandBar`, `openCommandBar`, `sendUserMessage`, `renderQuickTasks`, `resetSession`, the bubble's inline reply and its Details/Copy buttons) |
| `ui/js/history.js` | The **History** drawer: `setupHistory` (toggle buttons, the `chat-message` / `chat-history-cleared` listeners, the load-time `get_chat_history` hydrate), `renderHistory` (rows built with `textContent`, so a quoted diff can never be parsed as HTML), `appendChatMessage` (deduped by `id`, capped at 50, follows the newest turn only when the reader is already at the bottom), `clearChatHistory`, `toggleHistoryDrawer` (which republishes the hit region so the open card stays clickable), `historyOpen`, and the per-row **Details ▾** / **Copy** actions || `ui/js/history.js` | The **History** drawer: `setupHistory` (toggle buttons, the `chat-message` / `chat-history-cleared` listeners, the load-time `get_chat_history` hydrate), `renderHistory` (rows built with `textContent`, so a quoted diff can never be parsed as HTML), `appendChatMessage` (deduped by `id`, capped at 50, follows the newest turn only when the reader is already at the bottom), `clearChatHistory`, `toggleHistoryDrawer` (which republishes the hit region so the open card stays clickable), `historyOpen`, and the per-row **Details ▾** / **Copy** actions |
| `ui/js/survival.js` | The **Survival** card (top-left pop-out, not a panel section) with four tabs. `setupSurvival` hydrates from `GET /survival` + `GET /foods` + `GET /inventory` + `GET /store`, follows the `survival` / `survival-meal` / `survival-threshold` / `survival-craft` / `survival-craving` / `inventory-changed` / `store-transaction` / `fishing-*` events with a 15 s survival poll as the floor, `renderSurvival` flattens every payload shape into the tier badge, the two bars and the craving row, `feed` is a real `POST /survival/feed`, `setCraving` / `clearCraving` drive `POST /survival`, `craft` is a real `POST /survival/craft` fed by the recipe picker, which comes from `GET /recipes` with the bag folded in (`can_cook`, `missing`, per-line `held`) so `syncCraftButton` can disable a cook that could not pay (a 409 `missing_ingredients` / refusal prints the API's own prose). `showSurvivalTab` switches status / bag / fishing / store; `renderInventory` draws one cell per slot from `GET /inventory`, `addItem` / `useItem` / `sellPickedItem` are real `POST /inventory/add`, `POST /inventory/use` and a one-unit `POST /store/sell` (the bag's Sell button, enabled for any slot `GET /inventory` gave a `sell_price`); the store pane draws one row per shelf entry from `GET /store` (`renderStore`, grouped by category, with the wallet and each entry's `owned` count), keeps Buy/Sell honest against the purse and the bag (`syncStoreButtons`) and trades for real through `trade` — a 409 `insufficient_coins` / `inventory_full` / `not_enough_items` prints the API's own prose; the fishing pane runs `castLine` / `hookFish` / `reelFish` / `cancelFishing` against the plugin, polls `GET /fishing` at 120 ms so the bobber beats the 1 s host tick, and animates the marker from the published sweep. `toggleSurvivalCard` / `survivalCardOpen` drive the card and Esc chain; scavenging is the Status pane's 🧹 **Scavenge** button (`startScavenging` → `POST /scavenge/start`, `cancelScavenging` → `POST /scavenge/cancel`), a 500 ms `GET /scavenge` poll (`ensureScavengePoll` / `pollScavenging`) that counts the chip down and can be what completes the run, and `applyScavengeResult`, which prints the findings (what was *stored*, the coins, a rare find) in the note line |
| `ui/js/scavenge.js` | The **scavenging walk**'s geography: `scavengeAnchors` reads `GET /context` and turns the front-most `visible_windows` into one anchor just inside each window's bottom edge (validating the rect, skipping a sliver), `windowAnchors` is that pure mapping, and `fallbackAnchors` spreads a few points around the screen when nothing is open or the API is down. It moves nothing itself — `main.js` owns the walk — and takes the viewport as an argument, so it stays a leaf over `ipc.js` |
| `ui/js/metamorphose.js` | The **Metamorphosis** card (a second top-left pop-out, beside Survival). `setupMetamorphose` resolves the API base, hydrates from `GET /creature/status`, follows the plugin's `creature` event (`ready` / `metamorphosed` / `reset`) and keeps a 10 s poll as the floor so a stopped plugin shows as `offline` instead of freezing the card. `renderCreature` paints the stage badge (green when evolvable, gold at Mega), the XP and bond bars, the archetype/attribute/seed chips and the live rig's `bones` / `parts` / `min_world_y`, `renderBlend` turns `status.attribute_blend` into the lineage readout — one percentage chip and one stacked-bar slice per attribute (a plugin too old to send the block falls back to a one-hot mix from `attribute`), plus the blended flavour line — and `renderBranches` draws one row per offered branch (`pickEvolveBranch` selects it) with the mix that branch would produce (`branchMix` over `branch.blend` / `blend_summary`, so taking it visibly shifts the percentages) and `syncEvolveButton` disables Evolve with the exact XP shortfall (and `syncBackButton` labels **◀ Back** with the stage it returns to — a real `POST /revert`) — the button is a real `POST /metamorphose`, whose reply (`from → stage`, bones, parts, `model_path`) lands in the card's note while the host hot-swaps the avatar; `nudge` is `POST /creature/xp` and `reset` is `POST /creature/reset` (archetype + optional seed). `toggleMetamorphoseCard` / `metamorphoseCardOpen` drive the card and the Esc chain. A 404 on the status route renders the card as offline with the build step |

Module graph is a strict DAG (`ipc` / `state` are leaves):
`ipc, state → rig → animator → {renderer, speech, editor, bridge}; editor → bridge; all → main`.
`speech` also imports `ipc` (the clipboard command) and `rig` (`wireRegions`), both
downward edges — `rig` imports neither back, so the DAG stays acyclic.
`history` imports `state`, `ipc` and `rig` only; `main` passes it the page's
`resetSession` as a callback instead of importing `main` back.
No module writes to another module's namespace object; no `export let` placeholders.
The only mutable cross-module surfaces are the plain objects owned by `state.js`:
`viewport` (canvas size) and `animation` (the sampled clip offsets `rig.js` folds
into the transform pass, written only by `animator.js`). That indirection is what
keeps `rig.js` from importing the animator (which imports it) while still letting
`localRotation` / `computeWorld` see the current clip pose.
Add a rule: a module may only import *downward* in that list, and geometry helpers
belong in `rig.js` so the renderer, hit-tester and click-through sync stay in sync.

`src-tauri/tauri.conf.json` embeds `../ui` directly (`frontendDist: ../ui`) with `"withGlobalTauri": true` — no Vite/Webpack, no build step.

### `src-tauri/tauri.conf.json`

`frontendDist: ../ui` (embedded at compile time), `withGlobalTauri: true`, one
window `main` with the flags listed in §2.

`capabilities/default.json` grants `core:default` to the `main` window.
`build.rs` calls `tauri_build::build()` and then links the generated
`resource.lib` into the **test** binaries (`cargo:rustc-link-arg-tests`) plus
`#[link(name = "resource")]` for the lib unit-test harness: those binaries link
wry, which imports comctl32 v6-only symbols, and without the app's
`Microsoft.Windows.Common-Controls` manifest the loader resolves comctl32 v5 and
the process dies before `main` with `STATUS_ENTRYPOINT_NOT_FOUND`
(`cargo test` would report nothing but `0xc0000139`).

### Dependencies (`Cargo.toml`)

| Crate | Why |
| --- | --- |
| `tauri` 2 (`tauri-runtime-wry`) | window, WebView2, IPC, events |
| `tauri-plugin-log` 2 (level Info) | file + stdout logging |
| `serde` / `serde_json` | state serialization, rig JSON |
| `log` | logging facade |
| `tokio` 1 (`rt`, `rt-multi-thread`, `macros`, `sync`, `net`, `time`, `process`) | API runtime, `oneshot` shutdown, `broadcast` event bus; `process` drives the watcher's `--hermes-cmd` child so its wait can be wrapped in `timeout` and killed |
| `axum` 0.7 | HTTP API (incl. `response::sse`) |
| `tokio-stream` 0.1 (`sync`) | `BroadcastStream` adapter feeding the SSE response |
| `reqwest` 0.12 (`json`, `blocking`, `stream`, **no TLS**) | HTTP client for the MCP bridge binary (blocking) and the watcher (`stream`, for the SSE body and `/emote` + webhook POSTs); localhost-only, so no TLS backend is compiled in |
| `base64` 0.22 | inlining sprites as data URLs |
| `rfd` 0.15 | native file picker |
| `toml` 0.8 | TOML rig manifests |
| `image` 0.25 (`png`, `jpeg`, `webp`, no defaults) | image dimension probing |
| `rodio` 0.19 (`wav`) | audio playback for the Piper voice — WASAPI / ALSA / PulseAudio / PipeWire / CoreAudio through `cpal`, so `tts.rs` has no OS audio code |
| `cpal` 0.15 | microphone capture for push-to-talk (`stt.rs`), replacing winmm `waveIn` |
| `global-hotkey` 0.6 (Linux/macOS only) | the system-wide command-bar shortcut on X11 and macOS |
| `rdev` 0.5 (Linux/macOS only) | global cursor position and held-key state for gaze tracking and push-to-talk |
| `windows` 0.58 (`Win32_Foundation`, `Win32_UI_WindowsAndMessaging`, `Win32_System_SystemInformation`, `Win32_UI_Input_KeyboardAndMouse`, `Win32_Graphics_Dwm`, `Win32_UI_HiDpi`, `Win32_System_DataExchange`, `Win32_System_Memory`, `Win32_System_Ole`, `Win32_System_Threading`, `Win32_Graphics_Gdi`) | the Windows backend in `src/platform/win32.rs`: `GetCursorPos`, `GetForegroundWindow`/`GetWindowTextW`/`GetLastInputInfo`/`GetTickCount64`/`DwmGetWindowAttribute`/`EnumWindows`/`GetSystemMetrics`/`GetDpiForSystem`, the clipboard, `RegisterHotKey` and `GetAsyncKeyState` |

---

## 4. Data models & rig schema

### Folder layout

```
models/
  .state.json                  { "current": "demo-blob" }
  xenomorph-7/
    model.json                 13 bones, 14 shape parts (no textures)
  demo-blob/
    model.json                 7 bones, 11 parts
    rig.json                   optional editor override (wins over model.json)
    parts/{body,eye,pupil,arm,foot,mouth_closed,mouth_open,mouth_smile}.png
```

`rig.json` is written **only** by the editor; `model.json` is never modified by
the app. "Reset" = delete `rig.json`.

### Rig document

```jsonc
{
  "format": "desktop-buddy/model@1",
  "name": "demo-blob",
  "label": "Demo Blob (sprite parts)",
  "notes": "free-form",
  "bones": [ /* ... */ ]
}
```

#### Bone fields

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | unique; joints are addressed by this in `/pose`, `/joints` |
| `parent` | string \| null | another bone id, or `null` for a root |
| `pos` | `[x, y]` | local offset from the parent joint, in px, rotated by the parent |
| `rotation` | number | bind rotation, **radians** |
| `scale` | number | local scale multiplier |
| `length` | number | px; draws the rotation handle, used by `+ Bone` |
| `z` | number | draw order (lower first) |
| `limits` | `{min,max}` *optional* | rotation clamp in radians; **absent/non-finite = free multi-turn** |
| `anim` | object *optional* | `walk`, `sway`, `wave`, `bob`, `phase` (see below) |
| `parts` | array | sprites/shapes rigidly attached to this bone |

#### Part fields

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | unique within the bone |
| `image` | string *optional* | path relative to the model folder (no `..`, no absolute) |
| `shape` | string *optional* | `ellipse` \| `circle` \| `capsule` \| `face` (alternative to `image`) |
| `color` | string *optional* | fill for shape parts |
| `size` | `[w,h]` *optional* | px; omitted for images → natural size is adopted |
| `pivot` | `[x,y]` | normalised anchor inside the image (0..1) |
| `offset` | `[x,y]` | px offset from the bone origin |
| `rotation`, `scale` | number | relative to the bone |
| `z` | number | order within the bone |
| `tag` | string *optional* | semantic tag: `head`, `torso`, `arm_left`, `arm_right`, `leg_left`, `leg_right`, `eye`, `mouth`, `accessory`, `custom` |
| `state` | string *optional* | active variant name |
| `variants` | object *optional* | `{ "open": "parts/mouth_open.png", … }` |

#### Animation channels (`anim`)

Summed per bone, then clamped by `limits`:

| Channel | Contribution |
| --- | --- |
| `walk` | `sin(2π·walkPhase + phase) · walk` — only while walking |
| `sway` | `sin(t·1.3 + phase) · sway` — always |
| `wave` | `sin(wavePhase + phase) · wave` — only while waving |
| `bob` | vertical offset in px: `sin(t·1.8 + phase) · bob` |
| `phase` | radians; stagger limbs with ±π |

#### Custom keyframe clips (`animations`)

An **optional** object on the rig root. Rigs without it load exactly as before.

```jsonc
"animations": {
  "wave": {
    "duration_ms": 1400,
    "loop": false,
    "keyframes": [
      { "t": 0.0, "bones": { "arm_r": { "rotation": 0.0,  "x": 0, "y": 0, "scale": 1.0 } } },
      { "t": 0.4, "bones": { "arm_r": { "rotation": -2.5, "x": 0, "y": 0, "scale": 1.0 } } },
      { "t": 1.0, "bones": { "arm_r": { "rotation": 0.0,  "x": 0, "y": 0, "scale": 1.0 } } }
    ]
  }
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `duration_ms` | number | clip length; **must be > 0** |
| `loop` | bool *optional* | restart at the end instead of finishing (default `false`) |
| `keyframes` | array | one entry per pose; order does not matter (the sampler sorts by `t`) |
| `keyframes[].t` | number | normalised time in **`0.0..=1.0`** |
| `keyframes[].bones` | object | `{ "<bone_id>": { rotation, x, y, scale } }`; ids must exist in `bones` |

Every per-bone value is a **delta with an identity default**, so a clip composes
with the rig instead of replacing it:

| Key | Applied as | Identity |
| --- | --- | --- |
| `rotation` | radians **added** to the bone's bind rotation (then clamped by `limits`) | `0.0` |
| `x`, `y` | px **added** to the bone's `pos` in the parent's rotated frame | `0.0` |
| `scale` | **multiplier** on the bone's scale | `1.0` |

**Interpolation.** The sampler finds the two keyframes surrounding the playhead,
takes the normalised distance between them, smoothsteps it (`p²(3−2p)`) and
lerps each of the four values. A bone missing from one side of the pair is
interpolated from its identity there, so partial keyframes are legal. Before the
first keyframe and after the last one the clip holds that keyframe. `anim`
channels still add on top and `POST /pose` overrides still win, which is why the
order in `localRotation` is *bind → channels → clip → pose*.

Playback lives in the page (`ui/js/animator.js`); Rust only validates the data
and forwards `POST /animate` as the `avatar-animate` event. A non-looping clip
that reaches `t = 1` stops, sets `animation.finished`, and returns the avatar to
`idle` — that transition is what agents see as the next `state` frame.

#### Expression mapping

`PRESET_STATES` in `ui/index.html` maps a preset to a variant name per tag:

| preset | `eye` | `mouth` |
| --- | --- | --- |
| `neutral` | `open` | `closed` |
| `happy` | `happy` | `smile` |
| `surprised` | `wide` | `open` |
| `thinking` | `look_up` | `flat` |
| `sad` | `sad` | `frown` |
| `angry` | `angry` | `frown` |

Resolution order at draw time: **blink override → talk override → expression
state → authored `state`**; if the resulting variant is missing, the part falls
back to procedural animation (mouth scale, eye Y-squash).

### Worked example (verbatim from `models/demo-blob/model.json`)

```json
{
  "id": "head",
  "parent": "root",
  "pos": [0, -60],
  "rotation": 0.0,
  "scale": 1.0,
  "length": 0.0,
  "z": 30,
  "anim": { "sway": 0.06, "walk": 0.12 },
  "parts": [
    { "id": "eye_l", "image": "parts/eye.png", "size": [34, 34],
      "pivot": [0.5, 0.5], "offset": [-28, -14], "rotation": 0, "z": 1, "tag": "eye" },
    { "id": "mouth", "tag": "mouth",
      "image": "parts/mouth_closed.png", "size": [128, 20],
      "pivot": [0.5, 0.5], "offset": [0, 22], "rotation": 0, "scale": 1, "z": 5,
      "state": "closed",
      "variants": {
        "closed": "parts/mouth_closed.png",
        "open":   "parts/mouth_open.png",
        "smile":  "parts/mouth_smile.png"
      } }
  ]
}
```

TOML manifests are accepted too and normalised to JSON on install; TOML has no
null, so a root bone omits `parent` (or uses `""` / `"none"`):

```toml
format = "desktop-buddy/model@1"
name = "tomlhero"

[[bones]]
id = "root"
pos = [0, 0]

[[bones.parts]]
id = "body"
shape = "ellipse"
color = "#f0a020"
size = [120, 120]
```

### Validation rules (enforced on load, import and save)

* `bones` must be a non-empty array; ids unique; every `parent` must exist; no cycles.
* Every part needs `id` and either `image` or `shape`.
* Image paths must be relative, inside the model folder, and exist; ≤ 16 MiB each.
* `animations` (optional): `duration_ms > 0`, `loop` boolean, `keyframes` array,
  every `t` in `0.0..=1.0`, every referenced bone id present, transform values finite.
* `GET /models` reports broken folders via `error` instead of failing.

---

## 5. User guide

### Build & run

```bash
cd src-tauri
cargo check                 # type-check
cargo build                 # dev binary -> target/debug/desktop-buddy.exe
./target/debug/desktop-buddy.exe
```

Environment variables: `DESKTOP_BUDDY_MODELS` (model root), `DESKTOP_BUDDY_API_PORT`
(default 8765). Logs go to stdout and the plugin's log file; startup prints
`avatar api listening on http://127.0.0.1:8765`.

### First run

* The window covers the primary monitor, transparent, always-on-top.
* Top-right titlebar: **ask/task (chat)**, **rig mode (wrench)**, **settings
  (gear)** and **click-through (cursor)**, then **minimize** / **close**. The
  gear opens the same Settings modal as the panel button, so settings stay
  reachable while the control panel is collapsed.
* Bottom-right panel: a **Blend** strip (the creature's live recency-weighted
  attribute mix, e.g. `57% Feral · 43% Prism`, hidden until a creature exists),
  state/mood readout, model dropdown + **Import…** / **Rig edit**,
  Wave/Walk/Center, a **Watcher** row (switch, badge,
  **Logs ↗**), walk speed, position, API URL. There are no manual expression
  buttons — expressions come from `POST /emote` / `speak` and from watcher acks,
  and the status line only reports the current mood. The dropdown lists the
  models you keep: hand-authored rigs, plus any procedural creature you edited and saved
  (`has_rig_override`). Everything the metamorphosis plugin writes per stage is
  hidden — except the model currently loaded, which stays listed and is tagged
  `· generated` so the control never goes blank.
* Empty desktop space is click-through once the page publishes its regions; the
  cursor button toggles it manually.
* Close = titlebar **X** (stops the tracker and the API) or `POST /shutdown`.

### Moving the avatar (play mode)

Press anywhere inside the avatar's art and drag: it follows the cursor with the
grab offset preserved (so it does not snap its centre to the pointer), clamped
so the whole art box stays on screen. The drag parks it in `idle`, pauses idle
wandering for the gesture and for 15 s after release, and keeps the whole window
mouse-interactive until the button comes up — otherwise the click-through
tracker would drop the gesture the moment a fast drag outran the art box.
Releasing reports `drag_end` on `GET /events` like any other interaction.

A press that never crosses 8 px is still a `click` (or a `double_click` within
320 ms and 24 px of the last one). Rig Edit Mode is excluded: there the press
belongs to the gizmos.

Two things make the gesture stick: while `avatar.dragging` is set the page
publishes a **full-viewport** hit region (`rig.js`), because the 35 ms cursor
tracker would otherwise flip the overlay click-through the moment a fast drag
outran the art box; and `applyRemoteState` ignores in-flight
`avatar-state-changed` frames while the drag is live, plus a bare `walking`
echo that arrives without the walk segment that goes with it (adopting it used
to leave `step()` with a stale start and snap the avatar to an old target).

### Moving the panels

Floating cards are positioned by CSS defaults and moved by `ui/js/drag.js`.
`setupPanelDrag` attaches a `pointerdown` listener to each panel's header
(`titlebar`, `#control-panel header`, `#inspector header`, the drawer/card
headers and the import chooser's `<h3>`). The listener ignores presses that
start on a control (`button`, `select`, `input`, …), so only the header
background starts a drag. Positions are clamped so the header stays reachable
inside the overlay viewport; a stretch panel (the rig inspector) keeps its
bottom margin. A press anywhere in the panel raises its z-index within the
900–979 stack, below Settings (980) and the transient bubble/command bar
(1050/1060).

`drag.js` persists `(left, top)` per panel in `localStorage`
(`desktop-buddy.panel-layout.v1`), restores it when the panel is visible (a
`MutationObserver` catches panels that open later), and validates the saved
coordinates against the live viewport — a stale or off-screen entry falls back
to the CSS default. The titlebar's **▦** button calls `resetPanelLayout`.

Click-through stays correct because the drag calls `wireRegions` on every
animation frame and on release, and because `body.panel-dragging` makes
`rig.js` publish the whole viewport for the gesture. Without that flag a fast
drag could outrun the 35 ms Rust cursor tracker and lose the pointer.

### Life signs: lip sync, gaze and hover

Four small behaviours make the avatar feel alive without any agent input:

* **Lip sync.** While a bubble is up the mouth is driven by the typewriter, not
  by a clock: `speech.js::mouthPoseFor` maps each revealed character to an
  openness — vowels (`a e i o u`) wide open, soft consonants mid-open, spaces
  and `. , ! ? -` closed — and `updateSpeech` eases `speech.mouthSmooth` toward
  it every frame. `state.js::activeState` picks the matching sprite variant
  (`open` / `flat` / `closed`, falling back to whichever exists) and
  `renderer.js` squash-stretches shape mouths by the same number, so the cadence
  survives a rig with no mouth variants at all.
* **Global eye tracking.** The eyes follow the **operating system's cursor**,
  anywhere on the virtual desktop — over another application, with the overlay
  click-through and unfocused — because the page only receives `mousemove` over
  its own hit regions. The `avatar-cursor` thread already polls `GetCursorPos`
  every 35 ms for the click-through state machine; it now also emits
  `cursor-pos {x, y}` (CSS px, throttled to a 3 px deadband and ≥ 50 ms apart,
  i.e. ~20 Hz) and the page stores it in `gaze.cursorX/Y`. `updateGaze` picks the
  target in this order: an explicit `look_at` (`POST /move`), a focused
  `#command-input`, the perch rectangle while perched, the global cursor for 4 s
  after it last moved, then a random idle glance every 2–4 s. `renderer.js`
  turns the smoothed offset into a pupil shift clamped to ±4 px (and to 18 % of
  the part), so a pupil can never slide out of its socket; a rig with no pupil
  sprite moves the whole eye by ~55 % of that instead.
* **Head-bone turn.** The same eased gaze also biases the **head bone**: a yaw
  of up to 0.22 rad (~13°, `atan`-free — `dx / 900` clamped to ±1) that is damped
  at ~4/s and added in `rig.js::animOffset`. Because `localRotation` clamps the
  whole sum against `bone.limits`, a rig that declares a neck range can never be
  turned past it — set `limits: {min: -0.05, max: 0.05}` on a head bone and the
  head stops at ±2.9° however far away the cursor is.
* **Hover.** The pointer entering the avatar's art sets `gaze.hover`, which
  `rig.js::animOffset` turns into an extra ~3° head tilt that eases in and out.
  It is the same maths as the `sway` channel, so it composes with any animation.

A parked cursor is not tracked forever: after 4 s without movement the gaze
falls back to idle glances until the mouse moves again (that is also why a page
reload with a motionless cursor starts out glancing — the first `cursor-pos`
only arrives on the next movement).

### Asking the avatar (command bar)

The avatar is addressable without a terminal. `Enter` or `/` anywhere on the
overlay (while it has keyboard focus), the **chat icon** in the titlebar, the
**Ask…** button in the control panel, or a click on the speech bubble all open a
floating command bar anchored under the avatar:

* **From any application: `Alt+Space`.** The app registers a system-wide hotkey
  (`RegisterHotKey` on the `avatar-hotkey` thread) and, when it fires, disables
  click-through, focuses the overlay and emits `summon-command-bar`; the page
  opens the bar and focuses `#command-input`, so you can start typing the moment
  the shortcut lands. Typing before the Alt key comes back up is delivered as a
  *system* key (`WM_SYSCHAR`), which produces no DOM `input` event at all, so
  the bar's input re-inserts that character itself (`insertAtCaret`) instead of
  letting the first letter of a fast prompt be dropped. Change it with
  `hotkey` in `watcher_rules.json`
  (`"Ctrl+Shift+A"`, `"F9"`, `"Win+Shift+Enter"`, …) and restart the app —
  a hotkey is registered once at startup. If another application already owns
  the shortcut, `RegisterHotKey` fails, the app logs a warning and everything
  else keeps working (`Enter`/`/` still open the bar while the overlay has
  focus). Closing the bar restores click-through.
* Type a prompt or a task, `Enter` sends it (`notify_reply` → `user_reply`), and
  the bubble immediately shows a **Thinking…** card with the `thinking` face so
  you can see the task was received before the local model answers. The same
  card covers the *watcher's* own triggers: every one of them acks with
  `POST /emote {"expression": "thinking"}`, and `bridge.js` turns that emote into
  `showThinkingForAck()` — except when a real answer is already on screen, which
  must not be replaced by a spinner. So `drag_end`, `idle_return`,
  `window_title` and `alert` show the wait too, and all of them inherit the same
  safety timer.
  never get stuck: the watcher posts a `sad` fallback bubble when the spawned
  agent times out or dies (see `desktop-buddy-watcher.rs`), and `speech.js` keeps a
  `THINKING_TIMEOUT_MS` (125 s) timer as the last resort — it clears the card, sets
  the expression back to `neutral` and toasts, so "Thinking…" always ends.
* The **chips** under the input are one-click tasks (`quick_tasks` in
  `watcher_rules.json`, see below) — clipboard summary, "what am I looking at",
  diagnostics, or anything you script. A click sends the chip's prompt and
  closes the bar.
* **↺** in the bar and **New chat** in the control panel drop the conversation
  context: `reset_session` → `session_reset` → the watcher rotates its
  `{session}` id, so the next task starts a fresh thread.
* `Esc` closes the bar. While it is open the page publishes its rectangle
  (`rig.js::wireRegions`) so it stays clickable over the click-through desktop;
  closing it returns the region set to just the avatar/titlebar/panel and the
  desktop becomes click-through again.

The bubble's own inline reply box (hover it) is the same `notify_reply` path in
miniature, and its auto-dismiss timer is paused while the pointer is over the
bubble or either input has focus.

While a bubble is up the avatar **stands still**: `step()` zeroes the idle timer
instead of starting a wander, so the card (and the person reading it) never
drifts across the screen. An explicit `POST /move`, a quick-task chip's task or a
keypress still wins — only the automatic wander is held back. The card itself is
re-anchored **every animation frame** (`frame()` → `render()` →
`positionBubble()`), so a walk that was already in progress when the agent spoke
cannot detach it: the tail stays 30 px above the head no matter how fast the
avatar moves.

### Reading long output (details drawer + clipboard)

A bubble is a glance, not a page: the headless prompt asks Hermes for under 180
characters. Anything longer travels in `details`:

* `POST /speak {"text": "3 failures in parser.rs", "details": "…full log…"}` (or
  the MCP `avatar_speak` `details` argument) renders a compact **Details ▾**
  toggle and a **Copy** button inside `#speech-bubble`.
* **Details ▾** expands a scrollable, monospace `<pre>` under the summary,
  re-anchors the bubble above the head, republishes the hit region (so the fresh
  pixels are clickable immediately, not after the 200 ms publish) and **pauses
  the auto-dismiss timer** while it is open.
* **Copy** puts the full text on the clipboard through the `set_clipboard`
  command — the same Win32 writer as `POST /clipboard` and
  `avatar_set_clipboard`, so it works without the webview's async-clipboard
  permission. `navigator.clipboard` is only a fallback outside Tauri.
* For output that should not live in a bubble at all, an agent calls
  `avatar_set_clipboard(text)` (MCP) or `POST /clipboard` and speaks one line
  about it.

### Conversation history (the History drawer)

A bubble is transient by design, so the session's turns are also kept in a log:

* **Open it** with the **History** button in the control panel, the **☰** button
  in the command bar, or the `H` key. `Esc` closes it again (before the rig mode
  or the panel collapse), and its bounding box is published as a hit region only
  while it is open — so it is clickable over a click-through desktop and gives
  the desktop back the moment it closes.
* **What it holds.** Every prompt you send (`POST /reply`, the command bar,
  push-to-talk) and every line the avatar speaks (`POST /speak`, an agent reply)
  is recorded as `{id, role, text, details?, expression?, ts_ms}` — the same
  `details` block the bubble carried becomes an expandable monospace row with its
  own **Copy** button. The log is **bounded to 50 turns** (oldest evicted) and
  lives only in memory: a restart or **New chat** starts a fresh one.
* **Live updates.** The drawer hydrates from the `get_chat_history` command,
  appends from the `chat-message` event and empties on `chat-history-cleared`
  (which `do_session_reset` emits, so **New chat** clears the transcript
  everywhere at once — the drawer's own **New chat ↺** button runs the same
  reset). It auto-scrolls to the newest turn, but never yanks you away from a
  message you scrolled back to.
* **For agents.** `GET /history` returns the same list as
  `{"ok": true, "messages": [...]}`, so a script can read what was actually said
  instead of guessing from bubbles that already dismissed themselves. `role` is
  `"user"` or `"assistant"`.

### Survival card (hunger, saturation, feeding, cooking, the bag, fishing, scavenging, the shop)

The avatar has two bars and a mood about them. The numbers live in the survival
plugin (`plugins/survival/src/model.rs`) and are drained by that plugin's own
tick, so they keep moving whether or not the card is on screen.

* **Open it** with the **Survival** button in the control panel; `Esc` or the
  **✕** closes it. The button is gated on the plugin: it is hidden (and the
  Settings **Survival** tab with it) unless `GET /plugins` lists `survival` as
  `active`, so deleting `plugins/survival.wasm` removes the way in rather than
  offering a card that only says "offline". It is a **pop-out card pinned top-left**, deliberately not a
  panel section — the control panel is already tall, and the bars are something
  you glance at rather than leave open. Its box is published as a hit region
  only while it is open, so the selects and buttons work over a click-through
  desktop.
* **What it shows.** A tier badge (`satiated` / `peckish` / `hungry` /
  `starving`, coloured), the **Hunger** bar and the **Saturation** bar — which
  drains first and shields hunger, so it buys more time than its number suggests
  — and the **craving row** (`Apple · 10m left`, or `—` when nothing is pending).
  The card has four **tabs** — *Status*, *Bag*, *Fishing* and *Store* — because a
  second floating card would fight the first one for the same corner. The tab
  strip's coin badge carries the wallet, so the balance is visible from every
  pane.
* **Feed** really is a `POST /survival/feed` from the page: it sends
  `{"food": "<id>"}` and renders whatever comes back. A refusal (409: full, or
  still digesting) prints the API's own prose in the note line instead of
  pretending the click worked. The picker is filled from `GET /foods` and is
  **grouped** into `Ingredients` (raw) and `Meals & drinks`; crafted dishes are
  deliberately left out, so an omelette has to be cooked, not eaten from the
  shelf. A food that lives in the bag shows `own N` (`· none in bag` at zero) and
  feeding it **spends one**, exactly like Bag → Use — the plugin's `eat_held` is
  shared by both routes, so the note can print `· N left` and no meal comes free
  from an empty bag. Shelf meals with no bag entry (`cake`, `coffee`, `sandwich`)
  are eaten as-is.
* **Crave** sends `POST /survival {"craving": "<id>"}` and the row fills in;
  **Clear** sends `{"craving": null}`. This is the manual version of the
  craving the plugin rolls on its own, and it is how you demonstrate the
  proactive dialogue without waiting for the interval.
* **Cook** is a real `POST /survival/craft {"recipe": "<id>"}` driven by the
  recipe picker from `GET /recipes`. Cooking spends ingredients **from the bag**,
  so the picker is honest about it: an option whose pantry is short reads
  `Cheese omelette · Egg×2 (own 0) + Cheese (own 0) · missing`, its tooltip lists
  the shortfall, and `syncCraftButton` disables Craft until `can_cook` is true
  (title: *"still needs 2 × Egg, 1 × Cheese — buy them on the Store tab"*). A
  successful cook prints the dish, the tier change, the comfort window and what
  left the pantry (`used 2 × Egg, 1 × Cheese`); a refusal prints the API's own
  prose. The dish is eaten in the same call and its `comfort_secs` window halves
  the hunger drain for that long. Feeding is the other path: the Status picker
  shows `own N` per raw ingredient too, and `POST /survival/feed` now takes that
  item out of the bag through the same `eat_held` the Bag tab's Use button uses —
  a food you do not hold is a 409, not a free snack.
* **Where the numbers come from.** `GET /survival` on open/start, the
  `survival` telemetry event, a `GET /survival` re-read on `survival-meal`,
  `survival-threshold`, `survival-craft` and `survival-craving`, plus a 15 s poll
  as the floor when telemetry is disabled.
* **Spontaneous speech.** The plugin does not wait for the card: when a craving
  begins or lapses, or when the tick crosses into `hungry` / `starving`, it calls
  `host_notify` and the avatar says the line out loud. The card only renders the
  state; the talking happens in the guest tick.
* **Bag tab.** A slot grid straight from `GET /inventory`: the icon, the stack
  count and a tooltip from the item's `blurb`; empty slots are drawn too, so
  `used/capacity` means something. Clicking a slot selects it (`Use` is only
  enabled for an edible item, `Sell` for anything the counter prices — which is
  everything) and the row beneath shows its details, `sells for N` included; the
  picker and count box are a real `POST /inventory/add`, `Use` is a real
  `POST /inventory/use` that eats one through the same meal path as Feed, and
  **Sell** is a real one-unit `POST /store/sell` that prints
  `Sold 1 × Silver trout for 6 coins · 106 coins left` in the note line. The
  tab badge tracks `inventory-changed`, so a fish caught while the tab is
  hidden still lands in the count.
* **Fishing tab.** The entry point is the 🎣 **Go fishing** button on *Status*
  (or the tab itself). One button does all three phases — **Cast**, then
  **Strike!**, then **Reel!** — and it is the same action the scene click and
  the configured keybind (`hook_key` / `reel_key`) fire. The plugin is the
  authority: casting starts a hidden wait, the bobber only dips when a
  `GET /fishing` (or its SSE frame) reports `biting`, the strike is a real
  `POST /fishing/hook`, and the reel marker is animated purely from the sweep
  parameters the plugin published — while the *press* is judged by the plugin's
  own clock, so a doctored client cannot widen the sweet spot. A landed fish
  goes into the bag (visible on the Bag tab); a miss prints why it got away.
* **Scavenge.** The 🧹 **Scavenge** button on *Status* is a real
  `POST /scavenge/start`, and it is the one control that can never be greyed out
  for *lack* of anything: no rod, bait, coin or food is checked or spent. The
  chip beside it counts the run down from a 500 ms `GET /scavenge` poll (`idle`,
  then `rummaging · 12s`), **Stop** is a real `POST /scavenge/cancel`, and when
  the run ends the note line spells the findings out —
  `Found 2 × Driftwood, +5 coins` — or says the bag was too full to take them.
  The *walking* is not a request: the page follows the plugin's own
  `scavenging-started` frame, reads `GET /context` for the open windows and moves
  the avatar to each one while it digs (see the scavenging section of the plugin
  chapter), so the card can be closed and the avatar still rummages. On the way he
  may **mutter** about the window he is at — "Let me look behind the Hermes
  window." — offered to the plugin once per window and said only when its chance
  roll and its 8 s / two-per-run limits allow, never over an open bubble.
  His own **fidgeting must not cancel his activity**: the idle wander goes
  through `startWalk`, which counts as the user taking the avatar, so `step()`
  only wanders while `!scavengeWalk.active`, and a leg's `markManual` covers the
  rest of the run instead of a flat 15 s. (Without both, a long run whose last
  leg landed early stood him up mid-dig and let him idle while the card went on
  counting down.)
  The walk is **self-healing**, because two frames are thin glue for a 30 s
  animation: `reconcileScavenge()` re-reads `GET /scavenge` every 2 s (the call
  the card already makes, and completion from a `GET` is idempotent), restarts a
  walk whose `scavenging-started` was missed and ends one whose
  `scavenging-finished` was; a `scavenging-started` that arrives while a walk is
  still winding down replaces it instead of being ignored. `frame()` runs its
  body inside a `try`: `requestAnimationFrame` is queued **outside** the guard,
  so one bad frame costs a frame and a `[avatar] frame failed:` line instead of
  freezing the avatar until the app restarts — `__avatarDebug.snapshot()
  .frameErrors` counts them, and anything above zero is a bug to chase.
* **Store tab.** The shop, drawn from `GET /store`: the wallet in the pane head
  and on the tab badge, then one row per shelf entry (13 of them, four shelves,
  in a scrolling list) grouped by category, each
  with its icon, `own N`, and both prices (`buy` amber, `sell` green; a row the
  purse cannot afford is dimmed). Clicking a row selects it, the **Qty** box
  (1–99) sets the size, and **Buy** / **Sell** are real `POST /store/buy` and
  `POST /store/sell` calls — the note line prints the trade (`Bought 3 × Worm
  bait for 12 coins · 94 left`) or the plugin's refusal verbatim, and the pane
  re-reads `GET /store` afterwards because the reply carries the balance but not
  the new `owned` counts. Buy/Sell are greyed out only when the trade is
  *certain* to be refused (empty purse, not enough held); everything else is the
  server's decision. A `store-transaction` frame from anywhere — this pane, an
  agent's `curl` — repaints the shelf and the bag.
* **For agents.** `POST /survival {"hunger": 12}` moves the bars by hand, the
  same as clicking Feed does in the UI; `POST /fishing/cast`, then `POST /fishing/hook`, then `POST /fishing/reel`
  play the mini-game from a script.

### Settings (the ⚙ modal)

Everything that used to be "open `watcher_rules.json` and find the path" is a
form now. **Settings ⚙** in the control panel opens a modal with three tabs —
*Voice & paths*, *AI brain*, *Survival* — over `GET /config` / `POST /config`.

* **Voice & paths**: the Piper executable, voice `.onnx`, `espeak-ng-data`
  folder, Whisper executable/model and the Silero VAD model, each with a
  **Browse…** button (a native dialog through `pick_config_path`, which drops the
  overlay's always-on-top flag while it is up) and a ✓/✗ next to the field
  telling you whether that path exists right now. Speaking speed, the
  push-to-talk hotkey, VAD threshold, language and the screen-vision capture mode
  live here too.
* **AI brain**: two routing dropdowns — **Answers desktop triggers**
  (`active_profile`) and **Vision / screen capture** (`vision.profile_name`) —
  both rebuilt from the profile list whenever a card is added, renamed or
  removed, so a new profile is selectable before it is saved. Below them:
  **Discovery hosts** (comma separated IPs/hostnames, `127.0.0.1` as the
  placeholder, saved as the top-level `discovery_hosts`, each entry optionally
  `host:port`), **Discovery ports** (comma separated, saved as
  `discovery_ports`, probed on every host) and **Discover models**, which probes
  each host+port for an OpenAI `v1/models` listing, llama.cpp's root-mounted
  `models` listing and Ollama's `api/tags` (plus the `.gguf` files under
  `%LOCALAPPDATA%\hermes\models`) and lists every model it found with its
  endpoint and whether it is already configured. Status chips name the host for
  non-loopback targets (`192.168.0.36:11434 · offline`), so a remote hit is
  never mistaken for a local one. **+ Import** turns a row into a
  draft card pre-filled with the endpoint, model id, `timeout_secs: 120` and the
  JSON-avatar system prompt. Each card edits the key, label, kind, endpoint (or
  `cmd`), model, `max_tokens`, timeout and the **system prompt** textarea, and
  **+ Add Profile** creates one from scratch. **Remove** is refused inline — and
  again by the server — for the last profile, the active profile and the vision
  profile, with a warning naming the dropdown to switch first.
* **Survival**: the hunger system switch, tier notifications, both drain rates,
  the telemetry interval and the starting values. The section comes from the
  plugin itself (`PluginConfig::describe`/`validate`/`apply`) — the editor has no
  survival code — and the change is applied **live** on the plugin's next tick.
* **Saving** sends only the fields you changed. The server validates each block
  with the same struct the runtime loads it from, writes just the affected
  blocks (leaving every other byte alone, with a `.bak` beside the file) and
  answers `applied` + `restart_required`. New Piper/Whisper paths and the
  hold-to-talk hotkey are read once at startup, so they are honestly reported as
  needing a restart rather than silently ignored.
* **Warnings**: if the file carries keys the loader ignores (the classic
  `metabolic_drain_rate` instead of `metabolism.hunger_per_min`), the modal shows
  them in an amber box — that mismatch is exactly how a block ends up looking
  configured while the app runs on defaults.

### Watcher console

The **Watcher** row in the control panel drives `desktop-buddy-watcher.exe` without
restarting the app:

| Control | Effect |
| --- | --- |
| switch | `set_watcher_enabled(true/false)` — starts or stops (kills and reaps) the child. The supervisor parks while it is off and resumes when it is on |
| badge | `running` (green, with the pid in the tooltip), `off`, or `missing` (no `desktop-buddy-watcher.exe` beside the app) |
| **Logs ↗** | opens the pop-out **Watcher console** |

The console (`#watcher-drawer`, fixed above the panel) shows the live tail of the
child's **stdout + stderr** — timestamped `[HH:MM:SS]` lines, newest last, capped
at 200 — plus the `rules_path` the watcher was started with, a **Clear** button
(log view only; the Rust ring buffer keeps its copy) and a **Test ping** button.
Test ping fires `notify_interaction("double_click", …)` at the avatar's current
position, so one click exercises the whole pipeline: page → `interaction` frame on
`GET /events` → watcher rule → ack `/emote` → `--hermes-cmd` → MCP tool call.
`Esc` closes the console before it collapses the panel. The drawer publishes its
own hit region, so its buttons keep working over the click-through desktop.

### Watcher rules (`watcher_rules.json`)

The rules file is plain JSON and every key is optional, so an older file keeps
working and a missing/broken one falls back to the built-in defaults
(`README.md` shows the shipped file). On top of the interaction kinds, title
keywords and cooldowns it now also carries `app_rules`, `alerts`,
`quick_tasks`, the session/rotation keys, `hotkey`, `tts`, `stt`, the `vision`
block and `discovery_hosts` / `discovery_ports`:

```json
{
  "app_rules": [
    {
      "process": "Code.exe",
      "title_contains": "error",
      "animation": "surprised",
      "prompt_hint": "This is a compile error; name the file from the title."
    }
  ],
  "alerts": { "enabled": true, "cooldown_secs": 60 },
  "quick_tasks": [
    { "label": "Summarize clipboard", "prompt": "Call avatar_get_clipboard and summarise what I copied." },
    { "label": "Scan the network", "prompt": "Read the Suricata fast.log and report anything new." }
  ],
  "max_session_turns": 15,
  "session_idle_reset_secs": 1800,
  "session_base": "desktop-buddy",
  "hotkey": "Alt+Space",
  "tts": {
    "enabled": true,
    "piper_exe_path": "C:\\hermes\\tools\\piper\\piper.exe",
    "model_path": "C:\\hermes\\tools\\piper\\voices\\en_GB-southern_english_female-low.onnx",
    "espeak_data_path": "C:\\hermes\\tools\\piper\\espeak-ng-data",
    "speaker_id": null,
    "length_scale": 1.0
  },
  "stt": {
    "enabled": true,
    "hotkey": "Alt+V",
    "whisper_exe_path": "C:\\hermes\\tools\\whisper.cpp\\build\\bin\\Release\\whisper-cli.exe",
    "whisper_model_path": "C:\\hermes\\tools\\whisper.cpp\\ggml-base.en.bin",
    "silero_vad_model_path": "C:\\hermes\\tools\\whisper.cpp\\ggml-silero-v6.2.0.bin",
    "vad_threshold": 0.5,
    "language": "en",
    "min_speech_ms": 250
  },
  "vision": {
    "enabled": true,
    "profile_name": "vision",
    "capture_mode": "active_window",
    "max_image_dim": 1024,
    "jpeg_quality": 85,
    "trigger_phrases": ["what's on my screen", "look at my screen"]
  },
  "hermes_cmd": "hermes chat --continue {session} --create-if-missing -Q -q",
  "discovery_hosts": ["127.0.0.1"],
  "discovery_ports": [8001, 9931, 9932, 11434, 8080, 1234]
}
```

* **`discovery_hosts`** — the hosts **Discover models** probes, defaulting to
  `["127.0.0.1"]` when absent or empty. A LAN box, a Tailscale peer or a second
  interface is just another entry; `192.168.0.36:11434` targets that one port
  instead of being crossed with `discovery_ports`. Entries are cleaned on save
  (trimmed, `http(s)://` and any path dropped, deduplicated) so the file only
  ever holds `host` or `host:port`. The watcher never dials these either.
* **`discovery_ports`** — the ports **Discover models** probes on every
  `discovery_hosts` entry. Absent means the shipped defaults (`8001`, `9931`,
  `9932`, `11434`, `8080`, `1234`); the modal also shows any port an
  `ai_profiles` endpoint already names, so a hand-written profile is
  discoverable without editing the list. The watcher never dials these — only
  the app's `POST /ai-discover` does — so the key is tolerated by an older
  watcher and a file without it loads on defaults.
* **`app_rules`** — evaluated when the focused window *changes*. `process` is
  matched against `/context`'s `process_name` and `title_contains` against the
  window title, both case-insensitive substrings; a blank matcher is a wildcard,
  and the first matching rule wins (so put specific rules first). A matched rule
  can name an `animation` — an expression (`neutral`, `happy`, `sad`,
  `surprised`, `thinking`, `angry`) is posted to `/emote`, anything else is
  treated as a custom keyframe clip and played with `/animate` — and a
  `prompt_hint`, which is appended to the Hermes prompt as `Context: …`.
* **`alerts`** — the throttle for frames pushed by `POST /alert`:
  `cooldown_secs` defaults to `DEFAULT_ALERT_COOLDOWN_SECS` (60 s) and
  `enabled: false` lets the frames pass without waking Hermes.
* **`quick_tasks`** — the command bar's chips: `[{label, prompt, profile?,
  include_screen?}]`, at most 6.
  The app reads them straight from this file and serves them as `quick_tasks` on
  `GET /watcher`, so editing them needs no rebuild — reopen the bar (or it
  refreshes on open) and the chips change. A blank `label`/`prompt` is skipped;
  no array at all falls back to the four built-in chips (clipboard, focused window, screen vision, diagnostic). An optional `profile`
  pins that chip to one AI brain, and `include_screen: true` makes the chip ask
  with a screenshot even when its text does not match a `vision.trigger_phrases`
  phrase.
* **`hermes_cmd`** — the shipped value keeps one conversation across triggers:
  `hermes chat --continue {session} --create-if-missing -Q -q`. `{session}` is
  substituted with the watcher's **current** session id (`desktop-buddy-<epoch>`,
  `-2`, `-3`… after each rotation); `-Q` keeps the banner/spinner out of the
  watcher's log, and `-q` must stay **last** in the string because the watcher
  appends the prompt as the final argv entry. The CLI has no `--session` flag:
  `--continue <name>` (alias `-c`) is the named-session flag, and
  `--create-if-missing` is what makes it safe to point at a thread that does not
  exist yet. A resumed session also keeps its launch policy, so per-run
  overrides (`--run-budget`, `--model`, `-t`) are rejected — configure those in
  `config.yaml`. A template **without** `{session}` still runs, but rotation
  cannot reach the CLI (the watcher logs a warning at startup and `verify_docs`
  flags it).
* **`max_session_turns`** — rotate to a fresh session after this many dispatched
  turns (absent/0 = off). Checked *before* a spawn, so the turn that trips the
  limit already runs in the new conversation.
* **`session_idle_reset_secs`** — rotate after this long without a dispatch
  (absent/0 = off), so a conversation left overnight does not come back stale.
* **`session_base`** — prefix for generated ids (default `desktop-buddy`).
* **`hotkey`** — the system-wide shortcut that summons the command bar
  (`"Alt+Space"` by default; modifiers `Alt`/`Ctrl`/`Shift`/`Win` plus a letter,
  digit, `Space`, `Enter`, `Tab`, `Esc`, an arrow or `F1`–`F24`). Read by the
  **app**, not the watcher, at startup: change it and restart the overlay. A
  blank or unparsable value keeps `Alt+Space`, and a shortcut another
  application owns is a warning in the log, never a crash.
* **`tts`** — the local voice. `enabled` (default `true`) is the master switch;
  `piper_exe_path` (default `C:\hermes\tools\piper\piper.exe`) and
  `model_path` (default `…\voices\en_GB-southern_english_female-low.onnx`) must
  exist on disk; `espeak_data_path` (default
  `C:\hermes\tools\piper\espeak-ng-data`, `null` omits the flag) is passed as
  `--espeak_data` because Windows Piper cannot find its phoneme tables otherwise;
  `speaker_id` (default `null`) adds `--speaker <id>` for a multi-speaker model;
  `length_scale` (default `1.0`, clamped to 0.25–4.0, bigger = slower) adds
  `--length_scale`. Read by the **app** at startup, not the watcher: a missing
  `piper.exe`/`.onnx` logs one warning and the avatar simply stays silent. The
  panel's **Voice** switch / `POST /tts` mutes at runtime without rewriting this
  file, so a restart returns to whatever `enabled` says.
* **`stt`** — push-to-talk voice input. `enabled` (default `true`) is the master
  switch, `hotkey` (default `Alt+V`) is a **hold** shortcut in the same grammar as
  `hotkey`; `whisper_exe_path` / `whisper_model_path` must exist and
  `silero_vad_model_path` (default the shipped Silero v6.2.0 model, `null`
  disables VAD) is passed as `--vad-model`; `vad_threshold` (default `0.5`,
  clamped to 0.01–0.99) is `--vad-threshold`; `language` (default `en`) is `-l`;
  `min_speech_ms` (default `250`) drops accidental taps before Whisper runs. Read
  by the **app** at startup, like `tts`: a missing binary/model/microphone is one
  warning and a `missing` badge, never a crash.
* **`vision`** — screen vision, owned by the **watcher** (it already polls
  `/context`, and it is the side that talks to the AI profiles). `enabled`
  (default `true`) is the master switch; `profile_name` (default `"vision"`)
  must name an `openai_compat` entry in `ai_profiles`, so a screenshot is never
  sent to whatever text model happens to be active; `capture_mode` is
  `"active_window"` (default — the focused window's `window_rect`) or
  `"full_screen"`; `max_image_dim` (default `1024`) caps the longest edge after
  downscaling so the image stays cheap in tokens; `jpeg_quality` (default `85`,
  clamped 1–100) seats the encoder; `trigger_phrases` (default *what's on my
  screen / whats on my screen / what is on my screen / look at my screen / see
  my screen / read my screen / check my screen*) is matched case-insensitively against a typed or
  dictated `user_reply`, so push-to-talk can ask for a look without the panel.
  A disabled block, a missing profile, a failed capture or an unreachable
  endpoint is logged and answered conversationally — never a crash.

### Importing

1. **Import…** opens a native picker (JSON, TOML, PNG, JPG/JPEG, WEBP, SVG).
2. A **rig manifest** is installed as a model and loaded immediately.
3. An **image** opens a chooser:
   * **New character** → new model folder with the sprite as its body.
   * **Attach as part** → copies the sprite into the current model's `parts/`,
     enters rig mode and selects the new part on the selected bone (or the root).
     Press **Save rig** to persist.
4. Errors appear as toasts/JSON with the reason (unknown extension, missing
   file, missing sprite, dangling parent, …).

### Rig Edit Mode

The selection overlay draws through the **same matrix as the renderer**
(`rig.js::partWorldMatrix` + `partLocalRect` — `renderer.js::drawPart` calls the
former, `editor.js::hitTest` and the yellow box call the latter), so the box, the
pivot cross, the click target and `avatarBounds()` all describe the pixels on
screen:

* Images are drawn with their top-left at `-size * pivot`, so their rect is
  `[-w·px, w(1-px)] × [-h·py, h(1-py)]`.
* Shapes are **not** anchored that way: `drawShape` paints centred on the local
  origin (a circle/ellipse therefore sits half a size up-left of the image rect)
  and a capsule always spans `[-h/2, w-h/2]`; `partLocalRect` mirrors each case.
  Without that, a circular body part's selection box sat half a part down-right
  of the sprite (and clicks on the sprite missed it).
* The part offset is multiplied by the full world scale (bone × part), because
  the renderer scales before it translates; the inspector's part drag divides by
  the same factor so a scaled part keeps up with the cursor.

![Rig Edit Mode: inspector on the left, bone overlay on the avatar](docs/rig-editor.png)

Open with the wrench, **Rig edit**, or `R`. Entering parks the avatar mid-screen
and freezes animation.

| Action | Control |
| --- | --- |
| Select | click a joint, a part box, or drag a rotation ring |
| Move bone | drag its joint |
| Rotate bone | drag the ring at the bone tip (**unlimited turns**), or wheel over a selected bone (±0.02 rad) |
| Scale bone | shift+wheel over a selected bone (±0.03), or the `scale` field |
| Rotate **part** | wheel while a part is selected/held (±0.05 rad per tick, snapped to 4 dp) — a **locked** part passes the wheel to its bone instead |
| Scale **part** | shift+wheel while a part is selected/held (±0.03); locked parts scale the bone |
| Move part | drag the part (changes `offset`; moves its **bone** when the part is locked) |
| Lock bone to part | **Lock Bone to Part** in the part inspector — locked drags move the parent bone and keep the part's `offset`, and the wheel drives the bone too; a locked part also wins the press over the bone's rotation ring (which overlaps limb artwork); saved as `"lockBone": true` |
| Snap bone onto part | **Snap Bone to Part** — parent bone lands on the part, part `offset` zeroed, sprite stays put |
| Re-parent | `parent` dropdown (cycles rejected) |
| Add bone | **+ Bone** in the inspector toolbar (no selection needed: child of the selection, else of `root`, else a new `root`) — the new bone is auto-selected so its joint + ring are highlighted; the footer **+ Bone** is equivalent |
| Add part | `add content` → **type** (semantic tag), **source** (capsule/ellipse/model image) → **Add part** |
| Add limb | **+ Limb** — new child bone + capsule part + walk/sway channels |
| Duplicate / delete part | **Dup** / **Del** in the *semantics* block |
| Assign sprite to an expression | select an `eye`/`mouth` part → `expr` preset → **Bind PNG** |
| Preview expressions | preset strip **N H S T Sa A**, or **Preview** |
| Variants | `state` dropdown, `variant` name + **+ Image** |
| Undo | **Undo** / `Ctrl+Z` (80 steps) |
| Save | **Save rig** → `models/<name>/rig.json` (validated, atomic) |
| Reset | **Reset** → deletes `rig.json`, reloads `model.json` |
| Leave | **Done** / `R` / `Esc` |

### Animation timeline (Rig Edit Mode)

A compact timeline bar appears above the editor hint while Rig Edit Mode is open
(it is hidden in play mode). It edits `rig.animations` and is persisted by the
same **Save rig**.

| Control | Effect |
| --- | --- |
| **clip** dropdown | pick the clip to preview/author; selecting one switches the canvas to that clip's pose |
| **clip name** field | type a name, then **Enter** or blur, to rename the selected clip (trimmed; blank or duplicate names are rejected with a toast and the field reverts). With no clip selected it names the next **+ Clip** |
| **+ Clip** | create an empty clip named from the field, or the next free `clip 1`, `clip 2`, … when the field is blank, with `duration_ms: 1200`, `loop: false` |
| **Del Clip** | delete the selected clip |
| **duration** | clip length in ms (clamped to ≥ 1); the playhead is rescaled |
| **loop** | restart at the end instead of finishing |
| **Play / Stop** | preview playback. Stop (and a natural end) keep the same clip selected — a non-looping clip parks the playhead at `0 %` in the editor, so previewing never swaps the clip you are editing |
| 0–100 % scrubber | move the playhead; keyframe positions are drawn as tick marks, the one under the playhead highlighted |
| **+ Keyframe** | store the pose currently on screen at the playhead (creates or replaces that keyframe) |
| **- Keyframe** | delete the keyframe sitting on the playhead |

Authoring workflow: pick or create a clip, scrub to `0 %`, press **+ Keyframe**
to capture the rest pose, scrub to another position, drag/rotate bones (while a
clip is selected, drags edit the *clip pose*, not the bind pose), then press
**+ Keyframe** again. **Save rig** writes the clips into `rig.json`; **Play**
previews them without saving.

The readout shows `percent · ms · keys` plus `playing`, `finished` or `unsaved`
(when the on-screen pose has not been captured yet).

Inspector fields: position, rotation, scale, length, z-order, rotation limits
(blank = free), `walk`/`sway`/`wave`/`bob`/`phase`, and per part: size, pivot,
offset, rotation, z-order, scale, tag, state, variant.

### Play-mode shortcuts

`1` happy · `2` sad · `3` surprised · `4` neutral · `W` wander · `I` idle ·
`C` centre · `Space` wave · `R` rig mode · `H` conversation history ·
`Esc` close the command bar / history drawer / rig mode / collapse the panel.

---

## 6. Tauri command surface (page ⇄ Rust)

46 commands, registered in `generate_handler!`. Most take `tauri::State<Arc<AppCtx>>`
and/or `State<Arc<ModelCtx>>`; argument names are snake_case for the annotated ones.

| Command | Purpose |
| --- | --- |
| `get_avatar_state` | snapshot of `AvatarState` |
| `get_runtime_info` | `{api_url, api_port, click_through, avatar_name, version}` — `avatar_name` is the loaded model's label |
| `set_avatar_position(x, y)` | set walk target, mode → `walking`; clears any perch (a manual move) |
| `break_perch()` | drop the live perch without moving — the page calls it when a drag starts, so the 60 ms tracker stops re-targeting the avatar |
| `set_avatar_expression(expression)` | validated expression |
| `set_avatar_state(new_state)` | `idle`/`walking`/`waving` |
| `trigger_wave` | mode → `waving` |
| `report_state(position, target_position, state, expression, joints?, talking?, speech?)` | page → Rust; no Tauri event emitted, but a `state` frame is broadcast when the transition is a real change |
| `update_hit_regions(regions)` | interactive rectangles |
| `set_click_through(enabled)` | master switch (also applies immediately when disabled) |
| `notify_interaction(kind, x, y)` | pointer hit on the avatar art → `interaction` frame; returns `{ok, kind, subscribers}`, changes no state |
| `notify_reply(text)` | the bubble's quick-reply box was submitted → `user_reply` frame, bubble dismissed; returns `{ok, text, subscribers}`, changes no state beyond `talking` |
| `reset_session` | **New chat** → `session_reset` frame (the watcher rotates its `{session}`), bubble dismissed; returns `{ok, reason, subscribers}` |
| `close_window` / `minimize_window` | window controls (close also requests shutdown) |
| `list_models` / `get_model` / `load_model(name)` | model catalogue + switching (`load_model` broadcasts `model`/`load`) |
| `pick_import_file` | native picker → `{path, kind, name}` |
| `pick_config_path(kind?)` | the Settings modal's **Browse…**: `kind` ∈ `executable` \| `model` \| `folder` \| `any` picks the dialog's filters and whether it returns a directory; resolves to the chosen path or `null` (never validated here — the modal shows `GET /config`'s tick/cross and the write validates with the runtime struct). Drops the overlay's always-on-top flag while the dialog is up, and refuses a second dialog |
| `import_asset(path, mode?, name?, tag?)` | import (see §5); broadcasts `model`/`import` when it installs a model |
| `patch_part(target, patch)` | runtime part patch |
| `set_mouth(state)` | mouth convenience wrapper |
| `emote(expression, duration_ms?, revert?)` | expression with hold |
| `speak(text, expression?, duration_ms?, char_ms?, details?)` | bubble + talking; `details` (≤ 8000 chars) becomes the expandable drawer |
| `set_clipboard(text)` | put text on the Windows clipboard (≤ 32 000 chars) → `{ok, chars, truncated}`; the bubble's **Copy** button |
| `clear_speech` / `get_speech` | stop / inspect speech |
| `get_chat_history` | the session's turns, oldest first: `{ok: true, messages: [{id, role, text, details?, expression?, ts_ms}]}` — the drawer's hydrate call, identical to `GET /history` |
| `get_parts` | parts snapshot |
| `save_rig(rig)` / `reset_rig` | persist / discard override (broadcast `rig_saved` / `model`+`reset`) |
| `set_pose(joints)` / `clear_pose` / `get_joints` | runtime joint posing |
| `get_watcher_status` | `{ok, enabled, running, pid, binary_path, rules_path, logs}` — the same JSON as `GET /watcher` |
| `set_watcher_enabled(enabled)` | start/stop `desktop-buddy-watcher.exe` without restarting the app; returns the new status |
| `get_ai_profiles` | the `GET /ai-profiles` object: `{ok, active_profile, active_health, profiles}` (health probe included) |
| `get_tts_status` | the `GET /tts` object: `{ok, enabled, ready, reason, piper_exe_path, model_path, espeak_data_path, speaker_id, length_scale}` |
| `set_tts_enabled(enabled)` | the **Voice** switch: mute/unmute the local Piper voice (muting also purges the line that is playing), emits `tts-status`, returns the new status. Runtime only — `watcher_rules.json` is never rewritten |
| `get_stt_status` | the `GET /stt` object: settings plus `{state, last_transcript, last_error, ready, reason}` |
| `ptt_start` | begin a push-to-talk hold from the panel button (also interrupts the Piper voice); returns the live status |
| `ptt_stop` | end the hold: the worker stops the mic, runs Whisper and posts the transcript to `/reply`; returns the live status |
| `set_stt_enabled(enabled)` | mute/unmute push-to-talk at runtime, emits `stt-status`, returns the new status |
| `set_ai_profile(name)` | the **AI Brain** dropdown: persists, probes and broadcasts like `POST /ai-profile`, then returns the new status |
| `open_soul_file` | **Open SOUL.md**: `soul::soul_path` (the same resolution rule the watcher uses), `ensure_soul` to seed a missing file, then the OS opener (`cmd /C start` / `open` / `xdg-open`); returns the path. The modal's hand-editing escape hatch beside the textarea — the content itself is the `GET /soul` / `POST /soul` pair |

Events emitted to the page: `avatar-state-changed`, `api-status`,
`model-changed`, `rig-pose`, `rig-saved`, `part-patch`, `emote`, `speak`,
`speech-clear`, `avatar-animate`, `tts-status` (voice ready / muted / unavailable, and why), `stt-status` (mic `idle`/`listening`/`transcribing` plus the last transcript), `summon-command-bar` (the global hotkey fired:
`{source, hotkey}` — the page opens the bar and focuses its input),
`cursor-pos` (the OS cursor moved: `{x, y}` in CSS px, deadbanded to 3 px and
throttled to ~20 Hz — the page's gaze input), `chat-message` (one turn was
recorded: `{id, role, text, details?, expression?, ts_ms}`) and
`chat-history-cleared` (`{count}` — a new chat emptied the log).

The last three are **page-local only**: they are never put on the SSE bus, and
they never touch `AvatarState.revision`.

The same mutations are mirrored onto the HTTP event bus (`GET /events`) as
`state`, `speech`, `speech_cleared`, `model`, `rig_saved`, `pose`,
`part_patch`, `animate`, `interaction` and `user_reply` frames — see §7.

Custom keyframe playback has **no Tauri command**: the page owns the clock, so
`POST /animate` only emits `avatar-animate` with `{action, name, duration_ms,
loop, keyframes}` and the page's `bridge.js` calls `playAnimation` /
`stopAnimation`. That keeps the command surface unchanged.

---

## 7. HTTP API reference

Base URL `http://127.0.0.1:8765` (first free port in `8765..8774`).
All bodies are JSON; `content-type: application/json` is required on POSTs.

Error shape (our handlers, **400**): `{"ok": false, "error": "invalid expression \"x\"; expected one of [\"neutral\", …]"}`.
Malformed bodies are rejected by axum with **422** and a plain-text reason;
unknown routes are **404**.

| Method | Route | Purpose |
| --- | --- | --- |
| GET | `/health` | liveness + uptime |
| GET | `/context` | desktop context: foreground window title, its `process_name`, machine idle time + the last 15 SSE frames |
| GET | `/clipboard` | the clipboard's text, capped at 2000 characters |
| POST | `/clipboard` | put `{"text": "…"}` on the clipboard, capped at 32 000 characters |
| POST | `/reply` | the user's quick reply (`{"text": "…", "include_screen": true?}`) → `user_reply` frame; `include_screen` asks the watcher to attach a screenshot |
| POST | `/alert` | an external monitor's alert (`{"source","summary","severity"}`) → `alert` frame |
| POST | `/session/reset` | drop the conversation context → `session_reset` frame |
| GET | `/events` | Server-Sent Events stream of every state change (§7.3) |
| GET | `/state` | full avatar state |
| POST | `/state` | set state/expression |
| POST | `/move` | walk to `x,y` |
| POST | `/expression` | set expression (no hold) |
| POST | `/wave` | wave once |
| POST | `/click-through` | enable/disable click-through |
| POST | `/hit-regions` | page-owned; rarely needed externally |
| GET | `/models` | model catalogue |
| GET | `/model` | current model |
| POST | `/model` | switch model |
| GET | `/sockets` | semantic attachment points of a rig (`?model=<name>` for a folder that is not loaded) |
| GET | `/entities` | every prop entity, with its generated mini-rig |
| POST | `/entities/spawn` | create a prop (idempotent by id) |
| POST | `/entities/update` | move / re-state / attach / detach a prop |
| POST | `/entities/remove` | delete a prop, detaching its children |
| GET | `/props` | prop catalogue (defs, states, clips, grip sockets) |
| GET | `/chest` | the pod: state, slots, contents |
| POST | `/chest/summon` | spawn or bring back the pod at an anchor (`at`, or beside the avatar) |
| POST | `/chest/open` · `/chest/close` | splay the petals and reveal contents, or fold them and hide them |
| POST | `/chest/dismiss` | send it away; refuses while occupied unless `force` drops the contents on the stage |
| POST | `/chest/put` · `/chest/take` | move an entity into a slot, or out onto the stage |
| POST | `/import` | import rig or image |
| GET | `/parts` | parts + tags + variants + runtime patches |
| POST | `/part` | patch parts by tag/id |
| POST | `/mouth` | mouth state / talking |
| POST | `/emote` | expression with optional hold |
| POST | `/speak` | speech bubble + talking |
| POST | `/clear_speech` | hide bubble, stop talking |
| GET | `/speech` | active bubble + talking |
| GET | `/history` | the session's conversation turns, oldest first (bounded to 50) |
| GET | `/rig` | full rig + angles + pose |
| POST | `/rig` | save rig document |
| POST | `/rig/reset` | delete `rig.json` |
| GET | `/joints` | live joint angles + pose |
| POST | `/pose` | per-bone pose overrides |
| DELETE | `/pose` | clear overrides |
| GET | `/animations` | custom keyframe clips on the loaded rig |
| POST | `/animate` | play a clip (`{"name": "wave", "loop": true}`) or stop (`{"name": null}`) |
| POST | `/animate/stop` | body-less stop |
| POST | `/shutdown` | exit the app gracefully |

### `/health`

```bash
curl -s localhost:8765/health
```
```json
{"ok":true,"service":"desktop-buddy avatar api","version":"0.2.0","uptime_secs":396}
```

### `/state`

```bash
curl -s localhost:8765/state
curl -s -X POST localhost:8765/state -H 'content-type: application/json' \
     -d '{"state":"idle","expression":"happy"}'
```
```json
{"position":[463.27,678.53],"target_position":[463.27,678.53],
 "state":"idle","expression":"happy","revision":36,"talking":false,
 "perched":true,"perch_rect":{"x":400.0,"y":300.0,"width":800.0,"height":500.0},"look_at":null}
```
`revision` increments on every externally requested change — poll it to detect
"something happened" without diffing the whole object. `perched`/`perch_rect`
are the spatial directive set by `POST /perch` (the page pauses idle wandering
and aims the gaze at the rect's centre); `look_at` is the explicit gaze target
from `POST /move`. Both are cleared by a later move, `POST /state` or a drag.

### `/move`

```bash
curl -s -X POST localhost:8765/move -H 'content-type: application/json' \
     -d '{"x":1200,"y":700}'
```
```json
{"ok":true,"state":{"position":[…],"target_position":[1200.0,700.0],
 "state":"walking","expression":"neutral","revision":37,"talking":false}}
```
The page animates the walk (ease-in-out) and reports the live position back, so
`/state` converges on the target and returns to `idle` on arrival. An optional
`"look_at": [x, y]` also sets where the eyes rest while walking, and every move
clears a previous perch.

### `/perch`

```bash
curl -s -X POST localhost:8765/perch -H 'content-type: application/json' \
     -d '{"edge":"top_right","window_process":"code"}'
```
```json
{"ok":true,"edge":"top_right","window_process":"code",
 "window_rect":{"x":400.0,"y":300.0,"width":800.0,"height":500.0},
 "target":{"x":1130.0,"y":210.0},"state":{"…":"…","perched":true}}
```

`edge` ∈ `top_left | top_right | left | right | bottom`; the rect comes from the
first `visible_windows` entry whose `process_name` contains `window_process`
(case-insensitive), or from the foreground `window_rect` when it is omitted. The
target puts the avatar's **painted** bounds on the border — feet on a top/bottom
edge, body just outside a left/right one — using the art's `left`/`right`/`up`/
`down` extents around the root, which the page reports through `report_state`,
then clamps to the **monitor the window is on** (`MonitorFromPoint` →
`GetMonitorInfoW`, `monitor_bounds_for`) so a window at the screen edge cannot
push the avatar off it and a window on a secondary screen is not dragged back to
the primary one. `offset` slides along the edge. An invalid edge, no matching
window and no focused window are all **400** `{"ok":false,"error":"…"}` with an
actionable message.

The perch is **live**: the request stores the resolved window's `HWND`, the edge
and the offset (plus the art bounds frozen at that moment, because the live ones
wobble with the walk animation), and the `avatar-perch` thread re-reads the
window's DWM frame bounds every `PERCH_POLL_MS` (60 ms). A window that moved or
resized by more than `PERCH_FOLLOW_TOLERANCE` (1 px) produces a `state` frame
with the new `target_position`/`perch_rect` and the avatar walks along; a window
that is closed or minimized clears `perched`/`perch_rect` and returns the avatar
to `idle`. A user drag (`break_perch`, called by the page when a press becomes a
drag, and by `drag_end`) or any `/move` drops the perch so the tracker stops
fighting the pointer.

### `/expression`, `/wave`

```bash
curl -s -X POST localhost:8765/expression -H 'content-type: application/json' -d '{"expression":"thinking"}'
curl -s -X POST localhost:8765/wave
```

### `/emote`

```bash
curl -s -X POST localhost:8765/emote -H 'content-type: application/json' \
     -d '{"expression":"happy","duration_ms":3000}'
```
```json
{"ok":true,"expression":"happy","duration_ms":3000}
```
Holds for `duration_ms`, then reverts to `revert` (default `neutral`). Omit
`duration_ms` to hold until changed. Verified timing: a 2000 ms emote reverted at
2.11 s.

### `/speak`

```bash
curl -s -X POST localhost:8765/speak -H 'content-type: application/json' \
     -d '{"text":"Hello!","expression":"happy","duration_ms":4000}'
```
```json
{"ok":true,"text":"Hello!","expression":"happy","duration_ms":4000,"char_ms":26}
```
`text` ≤ 2000 chars (`MAX_SPEECH_CHARS`), `duration_ms` = how long the finished
text stays visible (default 6000, clamped 500..600000), `char_ms` = typing speed
(default 26, clamped 6..200). While the bubble is visible the mouth animates;
the bubble is dismissed by the page and `/speech` then reports `null`.

`details` (optional, ≤ 8000 chars — `MAX_SPEECH_DETAILS_CHARS`) is the long half
of a bubble: a code snippet, a diff, a log tail. It is trimmed, truncated rather
than rejected, echoed in `details_chars`, and renders as a **Details ▾** drawer
with a **Copy** button under the 180-character summary. The drawer pauses the
bubble's auto-dismiss timer while it is open. Use it instead of packing a
paragraph into `text`.

```bash
curl -s -X POST localhost:8765/speak -H 'content-type: application/json' -d '{
  "text": "2 tests failed in parser.rs",
  "expression": "sad",
  "details": "failures:\n  parser::nested   expected 3, got 4\n  parser::trailing expected EOF"
}'
```

### `/clear_speech`, `/speech`

```bash
curl -s -X POST localhost:8765/clear_speech
curl -s localhost:8765/speech
```
```json
{"ok":true,"speech":{"text":"hello","expression":"happy","duration_ms":1200,"char_ms":26},"talking":true}
```

### `/history`

```bash
curl -s localhost:8765/history
```
```json
{"ok":true,"messages":[
  {"id":1,"role":"user","text":"what does the history drawer do?","ts_ms":1794621725236},
  {"id":2,"role":"assistant","text":"It keeps the last 50 turns in memory.","details":"fn push_chat() { /* bounded VecDeque */ }","expression":"happy","ts_ms":1794621725766}
]}
```

The session's turns, oldest first. Every `POST /reply` / command-bar prompt
records a `"user"` turn, every `POST /speak` / agent reply records an
`"assistant"` one (carrying the same `details` and `expression` the bubble had),
and `POST /session/reset` clears the list. It is capped at 50 turns — the oldest
is evicted — and lives only in memory, so a restart starts an empty log. This is
the read side of the page's **History** drawer; `get_chat_history` returns the
identical body.

### `/models`, `/model`, `/import`

```bash
curl -s localhost:8765/models
curl -s -X POST localhost:8765/model -H 'content-type: application/json' -d '{"name":"xenomorph-7"}'
curl -s -X POST localhost:8765/import -H 'content-type: application/json' \
     -d '{"path":"D:/art/hero.png","mode":"model","name":"hero"}'
curl -s -X POST localhost:8765/import -H 'content-type: application/json' \
     -d '{"path":"D:/art/hat.png","mode":"part","tag":"accessory"}'
```
```json
{"ok":true,"root":"C:\\hermes\\desktop-buddy\\models","models":[
  {"name":"demo-blob","label":"Demo Blob (sprite parts)","bones":7,"parts":11,
   "has_rig_override":false,"generated":false,"error":null,"path":"…\\models\\demo-blob"}]}
```
Image imports in `mode:"part"` return the sprite **staged**, not attached:

```json
{"kind":"part","model":"demo-blob",
 "staged":{"image":"parts/hat.png","name":"hat.png","size":[64,64],"data_url":"data:image/png;base64,…"}}
```

### `/sockets`

Where things hang off a rig: `hand_primary`, `head_helmet`, `back`,
`stage_ground`, …

```bash
curl -s localhost:8765/sockets
curl -s "localhost:8765/sockets?model=xenomorph-7"
```
```json
{"ok":true,"model":"demo-blob","bones":7,"count":9,
 "sockets":[{"id":"hand_primary","bone":"arm_r","pos":[0,0],"rotation":0,"scale":1,
             "z":42,"accepts":["rod","tool","bag"],"mirror":"hand_secondary",
             "label":"Primary hand","kind":"bone","source":"derived"}]}
```

The table is resolved **at load time** (`sockets::resolve`) and travels with
`LoadedModel`: the `model-changed` Tauri event carries the whole table to the
page, and the `model` SSE frame reports its **count**.
Precedence per id:
`sockets.json` sidecar → `sockets` in `model.json`/`rig.json` → derivation from
the bones. Derived entries say so (`"source": "derived"`), which is the honest
answer for every generated creature, `xenomorph-7` (four tentacles, no arms) and
every hand-authored rig that predates sockets.

`?model=` is **read-only**: it loads the folder from disk and answers, without
touching the current selection (`ModelCtx::load` would switch the avatar and
rewrite `.state.json`, which a GET must never do).

### `/entities`, `/props`

Props are entities, not inventory rows: `def` + `seed` + `params` generate a
mini-rig, `state` picks its clip, `attach` places it on a named socket.

```bash
curl -s localhost:8765/props
curl -s -X POST localhost:8765/entities/spawn -H 'content-type: application/json' \
     -d '{"def":"fishing_rod","id":"survival:rod","state":"cast",
          "attach":{"parent":"avatar","socket":"hand_primary"}}'
curl -s -X POST localhost:8765/entities/update -H 'content-type: application/json' \
     -d '{"id":"survival:rod","state":"biting"}'
curl -s localhost:8765/entities
```

Layers, and why they are split that way:

* **`entities/defs.rs`** — the static catalogue: states, clips, tags, and how the
  prop grips each host socket (`grips: [("hand_primary", -1.25)]`). A prop with no
  opinion about a socket attaches with rotation 0.
* **`entities/gen.rs`** — `(def, seed, params) → mini-rig` in the *model* JSON
  format, so `models::validate_rig` accepts it and the page draws it with the same
  maths. Structure is drawn before colour (the metamorphosis rule); a test counts
  distinct *structural* tuples over 100 seeds, not palettes.
* **`entities/store.rs`** — identity, state, attachment, ownership, persistence in
  `models/.entities.json` (`version = 1`, capped at 64, never the rig itself).

The rig is **derived, never saved**: an entity persists as identity and comes back
with the same wood, reel and dents. `params` changes drop the cached rig.

Ownership: an entity spawned by a plugin carries `owner: "<id>"`; `update`/`remove`
from *another* plugin is a 400, while the host/user (no actor, i.e. HTTP) outranks
every plugin. The store has its own mutex and never calls into the plugin
registry; a mutation is applied under the lock, the lock is dropped, and only then
is the event emitted — otherwise a listener reading `GET /entities` deadlocks.

On the page, `ui/js/entities.js` resolves attachments and paints:
`avatar` sockets come from `socketWorld` (the avatar's `RT.world`), `entity:<id>`
parents resolve recursively (depth 4, cycle guard), and `stage` is the ground under
the avatar until the chest owns a real anchor. Transforms use
`rig.js::computeWorldInto` with a private context (`makeRigContext`, `lifeSigns:
false`) and clips use `animator.js::sampleClip` — one implementation of each, not
two. Draw order is one merged pass: a prop's `z` is `socket.z + bone.z + part.z`,
the same units as the creature's own `z`, so a held rod draws in front of the torso
and a pack on the back draws behind it.

A **guest** reaches the same store through `host_entities` — one JSON request in,
one JSON reply out (`PLUGINS.md`). `entities/guest.rs` is that handler: it applies
the request against a store, stamps `owner` with the caller's plugin id, and
returns a *summary* (never the rig — a 4 KiB guest buffer cannot hold one, and a
guest cannot draw bones). The events are emitted by the host after the lock is
gone, exactly as on the HTTP path. So a plugin and the API can do the same
things to the same props, with the same rules.

Frames: `entity-spawned` / `entity-updated` / `entity-removed` carry only ids
(plus `revision`); the page marks itself dirty and refetches the roster, coalesced
to one request per burst, with a 2 s reconcile so a dropped frame cannot leave a
stale prop on screen. They are not watcher-routed (no model turn is spent on a rod
being equipped), so they are outside the routed-frame check in `verify_docs.py`.

### `/chest` — one pod, blooming

The chest is an entity, not a subsystem: same store, same events, same ownership.
Three things make it a *chest*:

* **It is a singleton** (`id: "chest"`), so `summon` is idempotent by construction
  and a restart re-opens the same pod rather than growing a second one.
* **Contents are derived** from attachments (`entity:chest` at `chest_interior`),
  not stored in a slot list — so `GET /chest` cannot disagree with what is really
  inside, and a removed pod can never leave a phantom inventory behind.
* **It stands at an absolute anchor** (`attach.parent: "world"` with an `offset`).
  `stage` follows the avatar; `world` does not, which is what lets the gremlin
  walk over to it.

`entities/chest.rs` holds the lifecycle and its refusals; `api.rs` only adapts
requests to it and emits `chest-summoned` / `chest-opened` / `chest-closed` /
`chest-put` / `chest-took` / `chest-dismissed` **after** the store lock is gone.

**The choreography is the page's, the state is the host's** (the same split as
scavenging). `chest-summoned` carries `x`/`y`, so `main.js` walks the gremlin to
the anchor (`beginWalk`, `markManual`, `set_avatar_position`) and, on arrival,
POSTs `/chest/open` once. It stands down — without retrying — if the user drags
the avatar mid-walk, if a scavenging run starts, or after 14 s; a pod left closed
is still a pod you can open from the card.

A new pod seeds its own **trinkets** (`gen.rs::trinket`, owned by `chest`), which
is what makes a summon worth looking at: 3–5 of them, scattered in the well.
`dismiss` treats those as furniture (they leave with the pod) and only your own
items block it.

**The pod has no bottom.** `put` has no capacity check, `GET /chest` reports a
`count` and `"unbounded": true`, and the card shows `∞` instead of a bar — a
progress bar would be a lie about how full something bottomless is. This is a
deliberate choice, not an unfinished limit: the ceiling is a joke, and a joke with
a slot count stops being funny at a predictable moment.

### Rummaging (`POST /chest/find`)

A search is not a lookup. `chest::find` draws one to three **wrong** things from
`defs::JUNK` — rubber duck, anvil, magnifying glass, boot — spawns them as
ordinary stage entities in front of the pod, and only then takes out the thing
asked for. Three rules make it work:

* **Distinct duds.** They are drawn from a *shrinking pool* (`Vec::remove`), not
  by index: `Rng::below` will happily return the same value twice, and two
  identical ducks is a weaker joke than a duck and an anvil.
* **The mess stays.** Duds are `owner: "chest"` with a `world` attachment, which
  is exactly the predicate `POST /chest/tidy` sweeps on — so the rule needs no
  bookkeeping, and no item is ever deleted by a search.
* **The host writes the sentence.** `narrative` comes back ready to speak
  ("Rummaged around — out came Boot — then the Trinket"), so neither the page nor
  the model has to reconstruct what happened from the JSON.

A rummage that finds nothing still spills its duds. That is the point.

**The beat.** Everything is spawned at once, but each dud carries `reveal_at` 300 ms
after the one before (the first waits `FIRST_REVEAL_MS`, so the page can hear about
it), and the find is revealed after the last. The host owns that
timing (it is the thing that knows the order); the page owns the *motion*: an
entity is not drawn before its `reveal_at`, and the first frame after it arms a
throw — a `sin(πt)` arc from the pod's mouth to the dud's own landing spot, with a
spin that settles as it lands (`entities.js::tossOffset`, 460 ms). Nothing is
animated by the host, so a rummage works even if the page is mid-refetch: the
worst case is a dud that lands without a flight.

Each dud spawns in the state **`tumbling`**, and that shared name is the whole
trick: `clip_for_state` turns a state into a clip, so one word from the host gives
the duck a flap, the anvil a tip, the magnifier a wobble and the boot an
end-over-end flip — no per-prop branch anywhere in the rummage code.

**A spawn the page never hears about is drawn two seconds late.** `chest::find`
spawns through the store, not the HTTP route, so it emits its own
`entity-spawned` frames — the first version did not, and the debris arrived after
its own throw had already landed. `refresh()` claims its dirt *before* fetching, so
a spawn landing mid-fetch re-arms the flag instead of waiting for the reconcile.

`chest-find` also tells the gremlin, and that listener is what makes the
performance script-agnostic — a rummage started by `curl` or a plugin makes the
gremlin look at the pod and dig just as well as one started from its card.

### Debris has a lifetime

An entity may carry `expires_at` (absolute host epoch ms); `None` means it stays
until something removes it. Duds get `now + 15 s`. `EntityStore::reap_expired(now)`
removes the due ones — taking `now` as an argument so tests reap the future instead
of sleeping — and detaches their children first, so the reaper can never leave a
dangling parent.

The reaper is the **only** thing in the app that deletes on a timer, and it lives
in `api.rs::housekeeping`, a one-second `tokio` task on the API runtime
(`MissedTickBehavior::Delay`, so a suspended laptop does not fire a burst on wake).
It has to live there rather than in the store because removal has to reach the
page: the store can drop a row, but only the API knows how to emit
`entity-removed` on both the Tauri channel and the SSE bus.

`POST /chest/tidy` is the impatient version of the same rule, and `POST
/entities/spawn` accepts `ttl_ms` so an HTTP caller can make its own debris
without knowing the host clock.

### Tools come out of the pod

`POST /chest/withdraw` (and the `withdraw` guest op) asks the pod to **produce** a
tool: the same rummage and the same throws as `find`, then the tool — on the stage
or straight into a socket. `find` searches the contents; `withdraw` supplies what
you ask for, which is the whole point of a bottomless pod.

It is **idempotent by id** and the tool gets no `expires_at` (it is not debris).

**The pod comes when it is called for.** With no pod on the stage (or a dormant
one) — or one more than 220 px away — `withdraw` moves it *beside the gremlin* — `report_state` feeds
`entities::remember_avatar` at 8 Hz, so the store always knows where he stands —
then opens it. The entity store owns that one piece of stage geometry for exactly
this reason: the page is the only thing that knows where the avatar is, and "put it
next to him" cannot be answered from the prop store alone.

**The tool lands before it is held.** `withdraw` answers `pickup_ms`, and the
plugin waits that long before it attaches the rod: the rod lies on the stage while
the page walks the gremlin to it, and the walk *is* the pick-up. Two guests of the
same idea — the host owns the clock, the page owns the acting.

The pod is asked for a rod **when there is no rod** — the first cast of a session.
When a session ends the rod **rests in his paw for two seconds** (`resting`: tip
down, line slack) and is then **stowed**, not sent back to the pod. The beat matters
— a rod that vanishes on the same frame the fish leaves reads as a glitch — and the
stow matters more: the rod is already his, so a back-to-back cast starts with him
raising it rather than with the pod arriving and a fresh rummage — a landed fish, a lapsed bite, a cancel —
so the next cast fishes it out again. Nothing is created for a cast that is not
happening: the rod is fetched when the phase leaves `Idle`, not at boot, or a pod
would pop in beside a gremlin who is only standing there. The `spawn` fallback
remains for a host without the op (`ok:false` on the guest ABI), which is the rule
for every tool that comes next.

### The lid: announce it like any other prop

`chest-opened` and `chest-closed` were emitted for months and **nothing listened**:
the page only re-renders from `entity-*` frames, so the pod's lid changed state and
kept its old drawing until the two-second reconcile — long after a rummage had ended.
`open` and `close` now emit `entity-updated` as well, which is the frame the page
actually acts on.

With that in place the beat closes itself: `main.js` schedules a `POST /chest/close`
for when the sequence has finished spawning (the last `at_ms`, or `pickup_ms` when a
tool was taken). A pod left open never replays its `open` clip, so this is also what
makes the unfolding visible at all — every pull starts shut.

Measured on the drawn pod: `clip: "open"` with `petal0.rotation = -3.12` (≈ -π)
against `clip: "close"` with `rotation = 0`.

### The bag fills while he digs

A scavenging run rolls its loot **when he sets out**, not when he gets back, and
keeps it in the session (`scavenge::Scavenge::plan`). That is the whole reason: the
bag can only fill with things that exist, so the loot has to exist early enough to
be found one at a time.

`due_units(now)` spreads those items evenly across the rummage window; each beat
sends one `params { fullness }` update (the sack's generator widens it by up to
18%), and the page turns the `entity-updated` frame into a fatter prop. At the end
the sack is `full` — what it *is* — and then `resting`, what it is *doing*, which is
what triggers the packing-up gesture. `full` and `resting` are different words on
purpose; only one of them is a cue.

### Gestures: poses over time, not clips

Some acts are too small and too universal to author: putting a tool away, reaching
down for one. A **clip** is per-rig animation data; a **gesture**
(`ui/js/gestures.js`) is a table of bone *roles* (`arm`, `torso`, `head`, `root`)
that every rig answers, generated or hand-made, evaluated as offsets over `t` 0..1.

It is the same mechanism `rig.js::rummageOffset` already used for scavenging,
lifted into a named thing so more than one caller can ask for one:

* `avatar.gesture` / `gestureT` / `gestureBlend` (in `state.js`) are the channel.
* `main.js::startGesture(name)` starts one; `stepGestures(dt)` advances it; both go
  through `gestureDuration` so a gesture and its host-side beat can be timed to end
  together (`pack-up` is 2 s, the rod's `LINGER_MS` is 2 s).
* `rig.js::animOffset` adds the offset **last**, after the rig's own pose, its
  life-signs and a dig — a deliberate act outranks idle sway, and `localRotation`
  still clamps to the bone's declared limits afterwards.

Triggers are prop states, not def names: **`resting` means "the work is over, I am
about to go"**, and the page answers with `pack-up`. Any tool that adopts the state
gets the performance for free — which is the whole plan for the mining set.

### Picking the pod up

The pod is draggable (`main.js`): a press inside `chestBounds` starts `podDrag`,
a move past `DRAG_START_PX` moves the entity locally (`moveChestLocal`) and the
write goes out **once**, on release (`POST /entities/update`). Letting go without
moving opens the card — so click and drag are the same gesture, distinguished by
distance, the same rule the avatar drag uses. During the gesture `body` carries
`pod-dragging`, which `wireRegions` honours by publishing a full-viewport region:
without it the 35 ms cursor tracker can flip the overlay click-through mid-drag
and drop the pod.

`dormant` means *not on the stage*, and the page enforces it: `entityDrawables`
skips a dormant entity unless its clip is still running, which is what lets
`vanish` play and then vanish rather than blinking out — no scheduler needed.

Its look is generated like a prop's (`entities/gen.rs::chest`): a faceted polygon
shell — five or six sides, three facet styles, optional face, feet, charm — with a
rosette of petals hinged at the rim. Closed, the blades meet in the middle and the
silhouette reads as a die; open, each blade swings 180° about its own bone and
lies flat, and the well inside is lit. `poly` parts (`points` in rig units) are
what make a faceted shell possible; ellipses alone cannot look like a polyhedron.

Two rules worth keeping:

* **A state that names a clip plays that clip**, derived on the host (`store::
  clip_for_state`) on spawn *and* on update — so `{"state":"open"}` is the whole
  protocol, and `GET /entities` is the whole truth.
* **Fit belongs to the attachment** (`attach.scale`): an item put into a pod is
  drawn at 45% of its own scale rather than at gremlin scale.

### The mirror: `attach.flip` is a parity, not a negative scale

`Attachment::flip` was stored and validated from Phase 1 and read by nothing, so a
prop put in the off hand drew mirrored. It is not a transform of the *attachment*
— it does not move the prop, turn it or resize it. It reverses the prop's **own**
artwork, so the page carries it as a parity bit through the transform chain:

* `resolveRoot` (in `ui/js/entities.js`) composes `mirror = base.mirror * (flip ?
  -1 : 1)`, so `x`, `y`, `rot` and `scale` stay exactly what the caller asked for
  and the nudge offset is untouched. A prop hung on a flipped prop inherits the
  parity (`base.mirror` comes from `socketTransformIn`), which is what makes a fish
  on a flipped rod come out on the rod's other side.
* `rig.js::computeWorldInto` propagates it per bone: a child offset is
  `(mirror·x, y)`, and its local rotation is negated (`parent.rot + mirror·θ`),
  because a reflection conjugates every angle inside it. `partWorldMatrixIn` and
  `socketTransformIn` do the same, and the renderer's `ctx.scale(scale · mirror,
  scale)` paints the mirrored artwork.
* The mirror sits **after** the root rotation (`T · R(rot) · S(mirror·s, s)`), so
  the axis it reflects about is the prop's own vertical, not the screen's: a prop
  rotated 1.2 rad mirrors about a line 1.2 rad off vertical. A screen-space flip
  would have been `R(−rot) · S(−1,1) · R(rot)` and would look like a different
  prop as soon as anything rotated it.

Unmirrored rigs are untouched: parity is 1 unless an entity asks for it, so the
avatar, the editor and every existing prop compute the same numbers as before.
`tools/entity-transform.test.mjs` pins both halves (the unmirrored numbers and the
mirrored chain) against the real modules, which is the only automated proof this
lives in `ui/js` at all — `cargo` cannot see it. CI runs it as
**Page transform tests**.

### `/parts`, `/part`, `/mouth`

```bash
curl -s localhost:8765/parts
curl -s -X POST localhost:8765/part -H 'content-type: application/json' \
     -d '{"target":{"tag":"mouth"},"patch":{"state":"smile"}}'
curl -s -X POST localhost:8765/mouth -H 'content-type: application/json' -d '{"state":"talk"}'
```
```json
{"bone":"head","id":"mouth","tag":"mouth","image":"parts/mouth_closed.png",
 "state":"closed","variants":{"closed":"…","open":"…","smile":"…"},"z":5,"shape":null}
```
`/mouth` accepts `open`, `closed`, `smile`, `frown` (sprite swap) and `talk`,
`idle` (animation). Patch keys: `state`, `talk`, `offset`, `pivot`, `rotation`,
`scale`, `z`, `visible`, `color`, `image`.

### `/rig`, `/rig/reset`, `/joints`, `/pose`

```bash
curl -s localhost:8765/rig | python -c "import json,sys;print(json.load(sys.stdin)['rig'])" > rig.json
curl -s -X POST localhost:8765/rig -H 'content-type: application/json' --data @rig.json
curl -s -X POST localhost:8765/rig/reset
curl -s localhost:8765/joints
curl -s -X POST localhost:8765/pose -H 'content-type: application/json' \
     -d '{"joints":{"arm_l":{"rotation":-1.2},"head":{"y":-8}}}'
curl -s -X DELETE localhost:8765/pose
```
`GET /rig` returns `{ok, name, label, rig_path, is_override, rig, pose, angles}`.
`/pose` keys: `rotation` (absolute, rad), `scale`, `x`, `y`; `null` clears one
bone. `GET /joints` returns the live per-bone local rotations plus overrides.

### `/animations`, `/animate`, `/animate/stop`

```bash
curl -s localhost:8765/animations
curl -s -X POST localhost:8765/animate -H 'content-type: application/json' -d '{"name":"wave"}'
curl -s -X POST localhost:8765/animate -H 'content-type: application/json' -d '{"name":"breathe","loop":true}'
curl -s -X POST localhost:8765/animate -H 'content-type: application/json' -d '{"name":null}'
curl -s -X POST localhost:8765/animate/stop
```
```json
{"ok":true,"model":"demo-blob","rig_path":"…\\model.json","animations":[
  {"name":"breathe","duration_ms":2400.0,"loop":true,"keyframes":3},
  {"name":"wave","duration_ms":1400.0,"loop":false,"keyframes":6}]}
```
`POST /animate` validates the name against the loaded rig (**400** with the list
of available clips otherwise), applies the optional `loop` override for that run
only, and answers with the payload it pushed:

```json
{"ok":true,"animation":{"action":"play","name":"wave","duration_ms":1400.0,"loop":false,"keyframes":6}}
```

`{"name":null}` and `/animate/stop` both answer
`{"ok":true,"animation":{"action":"stop","name":null}}`. Each call emits the
Tauri event `avatar-animate` **and** the `animate` SSE frame, so a subscriber can
mirror playback state without polling.

### `/watcher`

```bash
curl -s localhost:8765/watcher                    # status (+ 200-line log tail)
curl -s -X POST localhost:8765/watcher -H 'content-type: application/json' \
     -d '{"enabled": true}'                       # or false
```

`GET` returns `{ok, enabled, running, pid, binary_path, rules_path, logs, quick_tasks}`;
`POST` takes `{"enabled": bool}`, starts or stops the supervised child and
returns the same object. `quick_tasks` is the command bar's chip list read from
the rules file (see §5), so editing `watcher_rules.json` changes the UI without
restarting anything. Start/stop/log changes are pushed as `watcher` SSE
frames and `watcher-status` Tauri events.

### `/tts`

```bash
curl -s localhost:8765/tts                                     # is the voice ready?
curl -s -X POST localhost:8765/tts -H 'content-type: application/json' \
     -d '{"enabled": false}'                                   # mute (true = unmute)
```

`GET` returns `{ok, enabled, ready, reason, piper_exe_path, model_path,
espeak_data_path, speaker_id, length_scale}`. `ready: false` means Piper, the
`.onnx` model or the espeak-ng directory was missing at startup and `reason` says
which; the avatar keeps working, it just has no voice. `POST` takes
`{"enabled": bool}`, mutes/unmutes the `avatar-tts` worker (muting also stops the
line that is playing) and returns the same object. The mute is **runtime only** —
`watcher_rules.json` is hand-edited and never re-serialised, so a restart returns
to `tts.enabled`. The control panel's **Voice** switch drives the same code through
`get_tts_status` / `set_tts_enabled` and receives `tts-status` events.

### `/stt`

```bash
curl -s localhost:8765/stt                                     # settings + live state
curl -s -X POST localhost:8765/stt -H 'content-type: application/json' \
     -d '{"enabled": false}'                                   # mute push-to-talk
```

`GET` returns the `stt` settings plus `{state, last_transcript, last_error,
ready, reason}`. `state` is the worker's live state: `idle`, `listening` (the
microphone is open) or `transcribing` (Whisper is running). `ready: false` means
`whisper-cli.exe`, a model or the microphone was missing at startup and `reason`
says which — the app keeps working, the mic is just dead.

`POST` takes `{"enabled": bool}`, mutes/unmutes the worker (muting a hold stops
it) and returns the same object. Runtime only: `watcher_rules.json` is never
re-serialised, so a restart restores `stt.enabled`. The actual recording is
driven by the **Hold to talk** button or the `stt.hotkey` hold, never by HTTP —
the API can read and mute, not press the button. The panel uses
`get_stt_status` / `ptt_start` / `ptt_stop` / `set_stt_enabled` and receives
`stt-status` events.

### `/ai-discover`

```bash
curl -s -X POST localhost:8765/ai-discover -H 'content-type: application/json' \
     -d '{"hosts": ["127.0.0.1", "192.168.0.36:11434"], "ports": [8001, 11434]}'
```

Probes the configured targets for local (or LAN) model servers and lists what
they serve — the **Discover models** button in the AI brain tab. With no
`hosts` / `ports` arrays the configured `discovery_hosts` and `discovery_ports`
are used, and `discovery::targets` crosses them (`host:port` entries keep their
own port). Each target is asked for an OpenAI `v1/models` listing, llama.cpp's
root-mounted `models` listing and Ollama's `api/tags`, all targets **in
parallel** with a 1.5 s budget each on a `spawn_blocking` thread; `.gguf` weights
under `%LOCALAPPDATA%\hermes\models` are listed as well. The body is `{ok,
summary, elapsed_ms, timeout_ms, targets, ports, models, gguf_dirs}` — the last
two include `host` and `loopback` so the modal can badge remote hits — see
`API.md` for the full shape. Discovery only *reports*: importing a model is the
modal filling a draft card, and `POST /config` writes it.

### `/config`

The Settings modal's backend: one read, one partial write, and the only place
that edits `watcher_rules.json` on the user's behalf.

```bash
curl -s localhost:8765/config | jq '.settings, .paths, .warnings'
curl -s -X POST localhost:8765/config -H 'content-type: application/json' \
     -d '{"survival": {"metabolism": {"hunger_per_min": 0.5}}}'
```

`GET` answers the **effective** settings (defaults applied, exactly what the
runtime loads), `paths` (every executable/model the app spawns, with `exists`) and
`warnings` (keys in the file that the loader ignores — the failure mode where a
block looks configured and the app runs on defaults).

`POST` takes a partial patch and writes only the blocks it mentions, replacing
just that member's byte span (`src-tauri/src/settings.rs`), so `app_rules`,
`quick_tasks`, the `system_prompt`s and every key the editor does not own survive
untouched. The body answers with the fresh `GET` payload plus `applied` (dotted
paths) and `restart_required`.

Live: `tts.enabled`, `stt.enabled`, **every block a plugin owns** (the editor
asks `plugins::config_specs()` and each plugin applies its own section through
`PluginConfig::apply`, reporting live vs restart) and everything under `ai` /
`vision` / `discovery_hosts` / `discovery_ports` (the `ai_profile` frame is what makes
`desktop-buddy-watcher.exe` re-read the file). A Piper/Whisper **path** or
`stt.hotkey` is read once at startup, so it is reported under
`restart_required` instead of being pretended live.

`ai` takes four verbs beyond `active_profile`: `create` (a whole entry, with
`timeout_secs` defaulting to 120 and the JSON-avatar `system_prompt` filled in),
`profiles` (per-entry edits, merged so `api_key` survives), `rename`
(`active_profile` and `vision.profile_name` follow it) and `remove` — refused
for the last profile, the active profile and the vision profile. They are all
resolved against the map that *results* from the patch, so one save can switch
to a profile it creates. `vision.profile_name` must resolve, or the save is a
400 instead of a screenshot sent nowhere. `discovery_hosts` (cleaned to bare
`host` / `host:port` values) and `discovery_ports` are written with
`replace_or_insert`, so a file that predates the key gains it without any other
byte changing.

### `/soul`

`SOUL.md` is the avatar's durable persona — the same architecture as Hermes
Agent's `SOUL.md`: one markdown file the user owns, holding only identity
(voice, speech-bubble brevity, humour, candour, how it reacts to screen events,
style anti-patterns). It is deliberately **not** part of `watcher_rules.json`:
it is prose the user edits, not configuration the app writes.

```bash
curl -s localhost:8765/soul | jq '.path, .exists, .is_default'
curl -s -X POST localhost:8765/soul -H 'content-type: application/json' \
     -d '{"content": "# SOUL.md\n\nDry, brief, a little odd.\n"}'
```

`GET` answers `{path, exists, is_default, content, persona, max_bytes}`.
`content` is the raw file (empty when missing) and `persona` is what the next
turn would actually inject — the file when it has text, the built-in starter
otherwise — so the modal can say "this is the starter" instead of guessing.
`POST` replaces the file atomically (temp file + rename); an empty body is a
400 (delete the file to fall back to the starter) and anything over 64 KB is
refused. There is no cache to invalidate: the watcher re-reads the file on
every turn.

**One resolver, both processes.** `src-tauri/src/soul.rs` is compiled into the
app *and* into `desktop-buddy-watcher` (via `#[path = "../soul.rs"]`), so the editor
and the prompt cannot disagree. Order: `DESKTOP_BUDDY_SOUL` → `soul_path` in
`watcher_rules.json` → `<rules dir>/SOUL.md` → `<app config dir>/SOUL.md`.
Seeding is `create_new` only — a missing file gets the ~50-line starter, an
existing file (even an empty one) is never touched. Reads degrade to the
built-in persona on empty, missing, unreadable or oversized content.

**Prompt slot #1.** `avatar_system_message` (openai_compat) and
`vision_request_body` (screenshots) both put the persona first, ahead of the
role prompt, the JSON action/expression contract and — last — the creature's
runtime block. `hermes_cli` gets the same persona through
`Trigger::prompt_with_persona`, so both backends share one voice. The creature
block comes from `GET /creature/status` (stage, archetype, recency-weighted
`attribute_blend` percentages) and is flavour only: it colours tone and the
chosen `expression`, while `SOUL.md` stays authoritative.

### `/survival`, `/survival/feed`, `/survival/craft`, `/inventory`, `/inventory/add`,
`/inventory/remove`, `/inventory/use`, `/fishing`, `/fishing/cast`, `/fishing/hook`,
`/fishing/reel`, `/fishing/cancel`, `/scavenge`, `/scavenge/start`, `/scavenge/cancel`,
`/scavenge/comment`, `/foods`, `/recipes`, `/store`, `/store/buy`, `/store/sell`, `/plugins`

These endpoints are **contributed by plugins**, not listed in `api.rs`: the
survival plugin returns `/survival`, `/survival/feed`, `/survival/craft`, the
four `/inventory` routes, the five `/fishing` routes, the four `/scavenge`
routes and the three `/store` routes from its `plugin_routes()` export, and the host adds
its own `/plugins`. `GET /survival` returns the appetite
(`config`, `stats`, `craving`, the `coins` wallet and the tier duplicated at the
top level);
`POST /survival` overrides `hunger`/`saturation` (at least one of those or
`craving`, clamped 0–100, **400** if none); `POST /survival/feed` eats one catalog id — a bag food is spent (`item`/`remaining` in the reply), a shelf meal is not — and returns
the `EatOutcome` (**409** when refused as `too_full` or `still_digesting`, **400**
for an unknown id); `GET /foods` lists the static catalog; `GET /plugins` returns
the registry's lifecycle state, tick/error counters and each plugin's `status`.

`POST /survival` also carries the **craving** control: a food id, a food group
(`fruit`, `vegetable`, `dairy`, `protein`, `grain`, `sweet`, `drink`), an object
form (`{"food": ...}` / `{"group": ...}`) or `null` to clear it. Cravings are
what make the avatar proactive: a low-saturation impulse roll or the
`craving_interval_secs` deadline starts one on its own, `craving_duration_secs`
later it lapses, and any matching meal settles it. Each of those moments emits
`survival-craving` with `state` `began` / `lapsed` / `satisfied` **and** speaks a
line through `host_notify`, so tier changes are no longer the only thing the
avatar says without being asked. `POST /survival/craft` cooks a recipe: send
`recipe` (an id from `GET /recipes`) or the `ingredients` themselves
(`["egg","egg","cheese"]` or `{"egg":2,"cheese":1}`) and the matching recipe's
dish is produced and eaten in one step.

Cooking is **paid for out of the bag**: the ingredients are inventory items
(`ItemKind::Ingredient`, one per raw food the recipe book names), the plugin
checks the bag before eating and takes them out only after the meal was
accepted. Two refusals exist and neither spends anything — **409**
`missing_ingredients` (with `{item, label, need, held, short}` per line) and the
meal refusals (`too_full`, `still_digesting`). `GET /recipes` therefore reports
the bag too: each line carries `held`, and every recipe `can_cook` plus a
`missing` list, which is what lets the UI disable Craft honestly. Cooking a dish
never needs a free slot — the finished meal is eaten, never held. Cooked dishes
carry a `comfort_secs` window during which the hunger drain is halved, and
`GET /foods` separates `raw` ingredients from `cooked` dishes via `kind`, `group`
and `crafted_via`.

#### Store & economy

The bag needs somewhere to send its surplus, so the plugin ships a shop. The
wallet is a **stat** — `SurvivalStats::coins`, `STARTING_COINS` = 100 — so it is
serialized with everything else, appears in `GET /survival` and rides along in
every `survival` telemetry frame. A state saved before the economy existed has no
`coins` key at all and its serde default is the *starting balance*, not zero, so
upgrading does not bankrupt anyone. The price list is data:
`plugins/survival/src/store.rs` holds `StoreItem { id, label, icon, buy_price,
sell_price, category }` and `default_catalog()`: two baits, the **eight cooking
ingredients the recipe book uses** (`apple`, `egg`, `cheese`, `tomato`,
`mushroom`, `noodles`, `bread`, `honey` — so a recipe is never a dead end), two
materials and the rod. `category` is one of `bait`, `ingredient`, `material` or
`tool`.

* `GET /store` returns the catalog — each entry with its prices, `category`,
  `owned` count and the bag's `stack_max` — plus `coins`, `starting_coins` and
  the inventory summary.
* `POST /store/buy` is **all-or-nothing**: the purse and the free space are both
  checked before anything moves, so a refused purchase costs nothing and an
  accepted one cannot half-land. It is `Inventory::room_for` that makes the
  second half honest — it predicts what an `add` would accept by running the real
  stacking rule on a throwaway copy, so the shop and the bag can never disagree
  about what "fits" (which is exactly why full slots still allow a topped-up
  stack). `insufficient_coins` and `inventory_full` are **409** with the numbers
  (`required`/`shortfall`, `room`); an unknown id or a `count` outside 1–99 is a
  **400**.
* `POST /store/sell` is exact: more than the bag holds is a **409**
  (`not_enough_items`, with `held`), an id that is not a catalog item at all is a
  **400**, and a successful sale removes the items and credits
  `unit_price × count`.
* **What the shop sells is not what the shop buys.** `sell_value(item_id)` is the
  counter, and it is deliberately wider than `default_catalog()`: a stocked id
  pays its listed `sell_price` (so the shelf and the counter can never disagree
  about the one price both of them know), and every other catalog item is priced
  from its `kind` and `rarity` — fish `2 × rarity` (minnow 2 … ember eel 8),
  ingredients and materials `rarity`, bait `1 + rarity`, tools `4 × rarity`, junk
  a flat **1** coin, because an unsellable row would only ever block a slot.
  `None` therefore means exactly "not an item". `GET /inventory` puts that number
  on every occupied slot and every catalog entry, which is what lets the Bag tab's
  **Sell** button print its own payout (`Sell +6`) with no second request and no
  second copy of the price table.
* every trade publishes `store-transaction` with the action, item, `unit_price`,
  `total` and the resulting balance, so the page and the SSE bus never need to
  re-read `GET /store` to stay current.

Four invariants hold the subsystem together and are pinned in `store.rs`'s
tests: every stocked id exists in the item catalog, the shop's `category` agrees
with the item's `kind`, `sell_price` is always below `buy_price`, and **every
ingredient any recipe names is on the ingredients shelf** (and vice versa — no
dead stock). That spread is what keeps the loop a loop rather than a coin
printer: sell the driftwood and river weed the casts drag up (or the fish you do
not want to eat), buy bait for the next cast, and buy the eggs and cheese that
turn into a cooked meal worth far more hunger than either ingredient alone.

```bash
curl -s localhost:8765/survival
curl -s -X POST localhost:8765/survival -H 'content-type: application/json' -d '{"hunger": 12}'
curl -s -X POST localhost:8765/survival/feed -H 'content-type: application/json' -d '{"food": "apple"}'
curl -s localhost:8765/foods
curl -s localhost:8765/plugins
```

The config is not frozen at startup: `POST /config` hands the validated block to
the plugin's `PluginConfig::apply`, which stores it in the plugin's state and
updates the live stats' drain rates in place, so `GET /survival`'s `config` echo
never disagrees with the numbers being applied.

### The `survival` block

| key | type | meaning |
|---|---|---|
| `enabled` | bool | `false` keeps the plugin installed but idle: no drain, no telemetry, routes still answer |
| `start_hunger` | 0–100 | where a fresh avatar starts on the hunger bar |
| `start_saturation` | 0–hunger | starting saturation (drains first, shields hunger) |
| `metabolism` | object | `hunger_per_min` and `saturation_per_min`, each 0.01–100 |
| `notify` | bool | speak tier changes out loud (events fire regardless) |
| `telemetry_secs` | int | seconds between `survival` frames, `0` disables them, minimum 60 |
| `cravings` | bool | let the avatar develop and announce cravings on its own |
| `craving_below_hunger` | 0–100 | hunger under which a craving may start |
| `craving_below_saturation` | 0–100 | saturation under which an impulse craving may start |
| `craving_interval_secs` | int | longest gap between two spontaneous cravings, 60–86400 |
| `craving_duration_secs` | int | how long one craving lasts before it lapses, 30–3600 |
| `fishing` | object | the mini-game's knob block — keys, timings and difficulty, below |
| `inventory` | object | `slots`, 4–40: how many slots the bag has |
| `scavenge` | object | the zero-cost fallback activity — run window and rare-loot odds, below |

#### The `fishing` block

| key | type | meaning |
|---|---|---|
| `enabled` | bool | `false` refuses every cast; the routes still answer |
| `require_rod` | bool | a cast needs a `fishing_rod` in the bag |
| `bait_required` | bool | a cast spends one bait item (`worm_bait`, `glowing_lure`) |
| `hook_key` | string | the `KeyboardEvent.code` that strikes, default `Space` |
| `reel_key` | string | the `KeyboardEvent.code` that reels, default `Space` |
| `mouse_button` | 0–2 | the mouse button that also works (0 left, 1 middle, 2 right) |
| `min_wait_secs` | int | shortest wait before a bite, 1–120 |
| `max_wait_secs` | int | longest wait before a bite, 1–120, never below the minimum |
| `hook_window_ms` | int | reaction window after the bobber dips, 250–8000 |
| `reel_period_ms` | int | one full 0→1→0 sweep of the marker, 300–6000 |
| `sweet_width` | float | sweet-spot width as a fraction of the bar, 0.04–0.6 |
| `difficulty` | float | 0.5 forgiving – 2.0 brutal; scales the sweep up and the spot down |

`difficulty` multiplies the marker speed and divides the sweet-spot width, and
the `config.fishing` echo in `GET /survival` reports what the numbers actually
play as `effective_period_ms` and `effective_sweet_width`.

#### The `scavenge` block

| key | type | meaning |
|---|---|---|
| `enabled` | bool | `false` refuses every run; `GET /scavenge` still answers |
| `min_secs` | int | shortest run, clamped to 10–30 |
| `max_secs` | int | longest run, clamped to 10–30 and never below `min_secs` |
| `rare_chance_per_mille` | int | per-mille chance of one extra find from the rare table, 0–1000 |
| `comment_chance_per_mille` | int | per-mille chance that a *requested* comment is actually said, 0–1000 (default 300) |

The 10–30 s window is the design promise, not a preference: `MIN_DURATION_SECS`
and `MAX_DURATION_SECS` in `plugins/survival/src/scavenge.rs` clamp it, an
out-of-range block is refused by name at `POST /config`, and
`tools/verify_docs.py` pins the shipped file. The defaults are `10` / `30` /
`180` (an 18 % rare find).

These are the **serde field names**, not prose: `metabolic_drain_rate`,
`starting_hunger`, `starting_saturation`, `notifications` and
`telemetry_interval_secs` are all silently ignored (the struct is
`#[serde(default)]` with no `deny_unknown_fields`), which is how a block ends up
looking configured while the app runs on defaults. `GET /config` reports such
stray keys under `warnings`, and `tools/verify_docs.py` now pins this schema.

#### Scavenging

The fishing loop could dead-end: a cast spends bait, bait costs coins, and an
avatar with no bait and no coins had no action left. Scavenging is the way out —
it requires **no rod, bait, coin or consumable**, so it is the one action that is
always available, and it always pays: one to three ordinary finds, a per-mille
chance of one rare find, and 1–6 coins. Two runs of it are always enough to
afford a bait restock (coins plus what the loot sells for), which is the
invariant the plugin's tests pin.

The split is deliberate, because the two halves have different constraints:

* **The guest owns the rules.** `plugins/survival/src/scavenge.rs` holds the
  phase (`idle` / `rummaging`), the drawn duration, the two weighted loot tables
  (`ORDINARY` — driftwood, river weed, minnow, apple, egg, bread, mushroom, an
  old boot, worm bait — and `RARE` — silver trout, glowing lure, honey, cheese,
  ember eel), the coin roll and the findings sentence. `POST /scavenge/start`
  checks only off / already-out / a-line-in-the-water, never stock.
* **The page owns the movement.** A Wasm guest has no window API, so it cannot
  see the desktop. `scavenging-started` carries the run's real `duration_secs`;
  `ui/js/scavenge.js` reads `GET /context`, takes the front-most
  `visible_windows`, and puts one anchor just inside each window's bottom edge
  (falling back to a spread of screen points when nothing is open). `main.js`
  walks the avatar to each anchor, then runs a **procedural rummaging pose** for
  a beat before moving on — `rig.js::rummageOffset` maps `avatar.rummage`
  (a 0→1 blend) onto the root/pelvis/torso/head/arm/forearm/hand bones every rig
  shares, so no model needs a `rummage` clip. `scavenging-finished`, a manual
  drag, or a `POST /move` ends the walk cleanly (`avatar.manualMoves`); the
  guest's run finishes and reports on its own clock either way.
* **Completion is idempotent.** `advance_scavenge` runs from the tick *and* from
  `GET /scavenge`, and flips the phase to `idle` before publishing, so whichever
  caller first sees the deadline banks the loot exactly once. A completed run
  writes its save immediately (a `GET` never reaches the dispatcher's flush).
  The session is not persisted: a restart mid-run loses an animation, never
  progress, because nothing is banked until the run completes.
* **Ambient muttering, spoken by the guest.** `POST /scavenge/comment` lets the
  page hand over the one fact it has and the guest lacks — the name of the window
  the avatar just reached (`windowLabel()` in `ui/js/scavenge.js` turns
  `Code.exe` / `"main.rs - Code"` into `Code`). The guest owns the cadence (max
  **2 per run**, ≥ **8 s** apart, then a `comment_chance_per_mille` roll, default
  300) and the phrasing, and it publishes `scavenging-comment` **and speaks the
  line even with a brain attached**: nothing happened except the avatar looking
  around, so there is no trigger for the watcher to route — and routing it would
  put a 10-30 s model turn on a mutter. The page only asks when no bubble is up,
  so a comment never talks over the AI's own reply to the same run.
* **A bubble must never end the run.** Real reactions to survival events
  (`survival-meal`, `survival-threshold`) can include a `perch`/`move`, so
  `applyRemoteState` deliberately does *not* count as the user taking over: the
  page's walk holds its dig where the avatar was put and keeps the rummaging
  blend up until the run's own deadline. Only a **user gesture** (a drag, or
  `W` / centre / random) bumps `avatar.manualMoves` and stands the walk down.

Tiers are `satiated`, `peckish`, `hungry`, `starving`. An accepted meal emits
`survival-meal`; a tier crossing (natural or manual) emits `survival-threshold`;
the registry's own status changes emit `plugins`. A cook emits `survival-craft`
and a craving emits `survival-craving`. The bag adds `inventory-changed` (any
add, remove or use, with `source` `add` / `remove` / `use` / `bait` /
`fishing`), and a fishing session emits `fishing-started`, `fishing-bite` (the
bobber dipped), `fish-hooked` (the strike landed, the sweep parameters travel
with it) and `fishing-result` (caught or lost, with the reason). A scavenging run
emits `scavenging-started` (the page's cue to walk and dig, carrying the run's
`duration_secs`) and `scavenging-finished` (the findings, the coins and the
line). See API.md for the full JSON shapes.

Every spontaneous event also carries `line` (the sentence the plugin would have
spoken) and, where it has one, `expression`; `fishing-result` adds `species`,
`rarity`, `value` (the shop's sell price) and `blurb` on a catch. Those
`fishing-*` frames are the **page's** feed — `ui/js/survival.js` draws every
phase from them — and only the end of a cast reaches the AI brain:

* `survival-threshold` / `survival-meal` / `survival-craft` / `survival-craving`
  share **one** cooldown bucket (`survival`, 10 s by default) — a single feed can
  cross a tier and settle a craving inside one tick, and that is one reaction.
* **Fishing is terminal-outcome-only.** `fishing-started`, `fishing-bite` and
  `fish-hooked` are not in any `handle_frame` arm, so casting, waiting, nibbling,
  biting and reeling never reach a model: the rig handles those phases locally
  and the single-flight latch stays free. A `fishing-result` is split into two
  trigger types by its `caught` flag — `fish_caught` (the prize: `label`,
  `species`, `rarity`, `value`, `blurb`, `catches`) and `fish_lost` (the `reason`
  and the `escapes` count) — and both share one 2 s bucket, because two outcomes
  seconds apart are one conversational beat. Routing the intermediates would keep
  a 10-30 s turn permanently in flight on a local model and still narrate each
  moment after it had passed.
* **Scavenging is terminal-outcome-only too.** `scavenging-started` is in no
  `handle_frame` arm — it is the page's animation cue. A `scavenging-finished`
  becomes one trigger (own 5 s bucket) whose detail names what was actually
  *stored* and the coins found; a cancelled run says so and nothing else.
* All of them are `quiet_failure`: a failed dispatch never posts the generic
  "Sorry, I hit a snag" bubble, it speaks the event's own `line`. That line is
  the **offline fallback** — a catch or a lost fish still gets a reaction when the
  watcher/brain is unreachable, errors out or has no profile, and the AI path
  never speaks twice.
* `fish_caught` / `fish_lost` are the routine kinds that queue behind a running
  turn instead of being dropped (one slot, newest wins). A slow local model is
  usually still generating when the next cast finishes, so the newer outcome
  replaces the older one (log line `coalesced the older pending … outcome`) and
  an outcome that has waited longer than `FISHING_OUTCOME_MAX_AGE` (45 s) is
  dropped rather than spoken late. With the single-flight latch that means at
  most one fishing request is ever in flight and only the freshest outcome is
  answered — no pile-up.
* Body events get a timeout **floor**, `SURVIVAL_DISPATCH_TIMEOUT_SECS` (180 s).
  A profile's own `timeout_secs` (60 s for the shipped fast profiles) is a fine
  ceiling for a `user_reply`, but cutting a slow local generation off there fires
  the offline fallback on a brain that was working. `dispatch_timeout_floor`
  returns the floor for every `survival-*` frame and for both fishing outcomes,
  and `None` (honour the profile) for everything else.

### `/shutdown`

```bash
curl -s -X POST localhost:8765/shutdown
```
Response flushes, then the app exits after ~120 ms (graceful API shutdown).

### `/context`

```bash
curl -s localhost:8765/context
```
```json
{"ok":true,"window_title":"main.rs - visual-studio-code","process_name":"Code.exe","idle_secs":12,
 "window_rect":{"x":400.0,"y":300.0,"width":800.0,"height":500.0},
 "foreground_hwnd":67672,
 "visible_windows":[{"title":"main.rs - visual-studio-code","process_name":"Code.exe",
                     "rect":{"x":400.0,"y":300.0,"width":800.0,"height":500.0},
                     "hwnd":67672}],
 "screen":{"x":0.0,"y":0.0,"width":1920.0,"height":1080.0,"scale":1.0},
 "avatar_state":{"position":[700.0,500.0],"target_position":[700.0,500.0],
                 "state":"idle","expression":"neutral","revision":7,"talking":false,
                 "perched":false,"perch_rect":null,"look_at":null},
 "recent_events":[{"seq":6,"type":"speech","ts_ms":1794405560000,"payload":{"text":"Build finished."}}],
 "source":"win32"}
```

`window_rect` and every `visible_windows[].rect` are **CSS pixels measured from
the virtual desktop's origin**, which is what the overlay draws in;
`screen` describes that space in physical pixels (`x`/`y` are
`SM_XVIRTUALSCREEN`, negative with a monitor left of or above the primary, and
`scale` is physical px per CSS px). A client that needs physical screen pixels —
the watcher's GDI capture is the one that does — converts with
`physical = css * scale + origin`. `foreground_hwnd` and the per-window `hwnd`
are the Win32 handles as integers, which is how `POST /perch` follows a window.
`window_title` is `GetForegroundWindow` + `GetWindowTextW` (truncated to 512
UTF-16 units); `process_name` is `GetWindowThreadProcessId` + `OpenProcess` +
`QueryFullProcessImageNameW` reduced to the file name (e.g. `Code.exe`, empty
when the process is protected or absent); `idle_secs` is
`(GetTickCount64() - GetLastInputInfo().dwTime)/1000`. `window_rect` is
`DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` (falling back to
`GetWindowRect`) divided by the primary monitor's DPI scale, so it is in the same
CSS-pixel space as the avatar; `visible_windows` comes from `EnumWindows`
(z-order, front first) keeping visible, non-minimized, non-cloaked, titled
windows owned by another process, capped at `MAX_VISIBLE_WINDOWS` (5).
`recent_events` is the last `EVENT_HISTORY_CAPACITY` (15) SSE frames, oldest
first. The probes live in `src-tauri/src/platform/`: the Windows backend
(`win32.rs`) runs the Win32 calls above, while the Linux/macOS backend
(`unix.rs`) shells out to `xprop`/`wmctrl`/`xdotool` (X11), `osascript` (macOS)
and `xrandr`/`NSScreen`-equivalent metrics. `source` names the probe that ran —
`win32`, `xdotool`, `wayland`, `osascript` — or `unsupported` when none could be
used (no display server, missing CLI tools, or a denied accessibility
permission); in that case the fields are empty/`0`/`null`.

### `/clipboard`

```bash
curl -s localhost:8765/clipboard
```
```json
{"ok":true,"text":"fn main() {}","truncated":false}
```
Win32 `OpenClipboard` → `GetClipboardData(CF_UNICODETEXT)` → `GlobalLock` read,
capped at `MAX_CLIPBOARD_CHARS` (2000) on a char boundary. It always answers
`ok: true`; an empty, non-text or busy clipboard yields `text: ""`.
`truncated` says the source was longer.

The write direction — how an agent hands over a whole file, patch or log:

```bash
curl -s -X POST localhost:8765/clipboard -H 'content-type: application/json' \
     -d '{"text":"fn main() {\n    println!(\"hi\");\n}\n"}'
```
```json
{"ok":true,"chars":39,"truncated":false}
```

`text` is required (an empty string clears the clipboard) and is cut at
`MAX_CLIPBOARD_WRITE_CHARS` (32 000) with `truncated: true` rather than an error.
The implementation is `OpenClipboard` → `EmptyClipboard` →
`GlobalAlloc(GMEM_MOVEABLE)` + UTF-16 copy → `SetClipboardData(CF_UNICODETEXT)` →
`CloseClipboard`, retried on a busy clipboard; if another process holds it open
for the whole retry window the route answers **400**, so a caller can retry.
Same writer as the `set_clipboard` command (the bubble's **Copy** button) and the
MCP `avatar_set_clipboard` tool.

### `/reply` and `/alert`

```bash
curl -s -X POST localhost:8765/reply -H 'content-type: application/json' \
     -d '{"text":"yes, do that"}'                    # -> user_reply frame
curl -s -X POST localhost:8765/alert -H 'content-type: application/json' \
     -d '{"source":"suricata","summary":"port sweep","severity":"warn"}'   # -> alert frame
```
`/reply` takes `{"text", "include_screen"?}` (non-blank, ≤ 500 chars),
dismisses the bubble and returns `{ok, text, subscribers}`. The optional
`include_screen: true` is copied into the `user_reply` frame so the watcher
attaches a screenshot even when the text does not match a
`vision.trigger_phrases` phrase — that is what the panel's **What's on screen?**
button and its chip send. `/alert` takes `{source, summary, severity}`
with `severity` ∈ `info | warn | error` (default `info`) and returns
`{ok, alert, subscribers}`. Neither route calls a model: the watcher decides
whether an alert is worth a Hermes turn.

### `/session/reset`

```bash
curl -s -X POST localhost:8765/session/reset   # -> {"ok":true,"reason":"api","subscribers":1}
```
Broadcasts `session_reset` and dismisses the bubble. The app owns no session
state — `desktop-buddy-watcher.exe` rotates its `{session}` id on the frame and logs
`session reset (api): desktop-buddy-<ts> -> desktop-buddy-<ts>-2`; the next task runs in a
clean Hermes conversation. The UI twins are the command bar's **↺** and the
control panel's **New chat** (both call `reset_session`).

### `/events`

```bash
curl -N localhost:8765/events
```
Server-Sent Events; frames are `{"seq","type","ts_ms","payload"}` and `type` is
also the SSE `event:` name. Event types: `hello` (on connect), `state`,
`speech`, `speech_cleared`, `model`, `rig_saved`, `pose`, `part_patch`,
`animate`, `interaction`, `user_reply`, `alert`, `session_reset`, `watcher`. Keep-alive comment
every 15 s; channel capacity 64 with skip-forward on lag, plus the 15-frame
history served in `/context`. Full payload table in `API.md` §
"Awareness: desktop context & the event stream".

---

## 8. Known limitations

* **Wayland has no window context.** `GET /context` reads the focused window
  through `xdotool`/`wmctrl` on X11 and System Events on macOS; a Wayland
  session reports `"source":"wayland"` with empty values, so window-title
  triggers do not fire there. Voice, chat and the avatar are unaffected.
* **Screen vision is Windows-only.** The watcher's capture is GDI `BitBlt`
  (`src/bin/capture/mod.rs`); on Linux/macOS a vision ask posts the apology
  bubble instead of a screenshot.
* **macOS needs the Accessibility permission** for window titles, the global
  cursor and the hotkey. Without it those probes return empty values and log one
  warning; the app keeps running.
* **One DPI scale for window geometry.** The overlay itself spans the whole
  virtual desktop (`SM_X/Y/CX/CYVIRTUALSCREEN`), and every rect is converted
  relative to that origin, so a monitor to the left of or above the primary maps
  to positive CSS coordinates. What is *not* modelled is **mixed per-monitor
  scaling**: `scale_factor()` / `GetDpiForSystem()` report one scale for the
  whole virtual desktop, so a 150 % monitor next to a 100 % one still lands its
  windows slightly off in the other monitor's CSS space.
* **A perch keeps the pose it arrived in.** Live tethering is handled (see
  above: 60 ms `avatar-perch` tracking with a 1 px tolerance), but there is no
  sit/hang animation, so a `bottom` perch just places the avatar below the
  border, and the monitor clamp means a window flush with the top edge gets the
  avatar pushed down onto it rather than above it.
* **Rigid parts, no meshing.** Every part is transformed by one bone; there is
  no skinning, deformation or per-vertex mesh (the `spikes/` wgpu mesh
  experiment was never integrated).
* **Interaction hit-test is a bounding box.** Clicks are matched against the
  avatar's AABB (the same rectangle used for click-through), not per-part
  pixels, so a click in a transparent corner of a large rig still counts. There
  is no hover state. Dragging moves the avatar's anchor only — there is no
  per-part manipulation outside Rig Edit Mode and no momentum/inertia, and the
  drag is clamped to the window, so it cannot push the avatar onto a second
  monitor.
* **One conversation at a time, one session id.** The watcher holds a single
  session id; there is no per-task session, no branching and no way to switch
  back to a previous thread. The id is re-derived at every watcher start
  (`<prefix>-<timestamp>`), so restarting the watcher or the app starts a fresh
  thread — "New chat" is only needed mid-run.
* **The history is in memory and 50 turns deep.** `GET /history` and the
  **History** drawer cover the current session only: the log is a bounded
  `VecDeque` (50 turns, oldest evicted), is never written to disk, and dies with
  the process or a `POST /session/reset`. There is no search, no paging, no
  export, and no per-turn token/cost accounting.
* **The command bar's hotkey is registered once per process** (`RegisterHotKey`
  on Windows, the `global-hotkey` crate on Linux/macOS; Wayland sessions may
  refuse it). `Enter`/`/`
  only reach the page when the overlay window has focus — which is exactly what
  the `Alt+Space` hotkey arranges by focusing the window first. `RegisterHotKey`
  is Win32, so off Windows the shortcut is skipped with a warning; a hotkey
  another application already owns cannot be taken (the app logs it and keeps
  running), and changing `hotkey` in `watcher_rules.json` needs an app restart
  because the shortcut is registered at startup.
* **`interaction` is one-way.** The UI reports a click and does nothing else;
  any reaction (wave, speak) has to come back from an agent over HTTP. The
  quick-reply box is the other direction: it *sends* text (`user_reply`) but
  renders no reply of its own — the bubble only changes when the agent speaks.
* **`/context` is shallow by design.** It exposes the foreground window title,
  the owning executable's *name*, machine-wide idle seconds, one rectangle and
  the five front-most visible windows with their titles/processes/rects, plus the
  last 15 SSE frames — no per-window activity, no full process list, and no
  clipboard on `/context` itself (`GET /clipboard` is a separate, explicit
  call). The
  clipboard read is text-only (`CF_UNICODETEXT`): images, files and rich
  formats come back empty, and the cap is 2000 characters.
* **SSE is unauthenticated and un-persisted.** Any local process can subscribe;
  there is no replay buffer (`Last-Event-ID` is ignored), so a reconnecting
  client resumes from the next `state` frame — the 15-frame history exposed in
  `/context` is the closest thing to a catch-up.
* **One bubble at a time.** No queue; `text` is plain text (max 2000 chars) and
  the `details` drawer is plain, monospace text (max 8000 chars, no syntax
  highlighting, no rich formatting); dismissal is page-side, so `/speech`
  follows the page's reports. Past turns survive in the **History** drawer (and
  `GET /history`) — with their `details` — but the bubble itself is transient by
  design.
* **Expression variants are optional.** Missing variants fall back to
  procedural squash/stretch rather than substituting another sprite.
* **Life signs are cosmetic and page-local.** Blinking only affects parts tagged
  `eye`. Lip sync reads English-ish on any rig: the Piper RMS envelope
  (`wav_envelope`, 25 ms buckets) drives the mouth while the voice plays, and
  the character table (vowel / soft-consonant / punctuation) is the fallback when
  the voice is muted, missing or done — there are still no visemes and no
  per-language tuning. Eye tracking now sees the **global OS cursor** even
  through a click-through, unfocused overlay (`cursor-pos`), but only on
  Windows (elsewhere it falls back to the page's own pointer) and only after the
  cursor's first movement; a parked cursor hands the gaze back to idle glances
  after 4 s. It moves the pupils plus a clamped head-bone yaw — there is no body
  turn, no blink-on-focus and no saccade modelling.
* **Editor scope.** No viewport scale gizmo (wheel/shift+wheel or the inspector
  instead), no snapping, no IK handles, undo is rig-edits-only (80 steps, no
  redo), and there is no undo for imports or model switches.
* **Animation authoring is linear-only.** Keyframe values are deltas, so a clip
  cannot express absolute poses or per-keyframe easing curves; interpolation is
  smoothstep between adjacent keyframes. Undo (`Ctrl+Z`) covers bone edits, not
  keyframe capture — an uncaptured pose is dropped when the playhead moves, and
  the clips are only persisted by **Save rig**. New clips are named from the
  timeline's name field (falling back to `clip 1`, `clip 2`, …) and can be
  renamed in place from the same field.
* **One override per model.** `rig.json` is a single alternative; there are no
  named rig variants or migration/versioning beyond `format`.
* **Watcher triggers are rule-shaped, not semantic.** `desktop-buddy-watcher.exe`
  reacts to `interaction`, `user_reply` and `alert` frames plus window-title and
  idle changes; `app_rules` match a process name against the title with plain
  case-insensitive substrings (no regex, no path, no other window's context),
  and a burst of triggers inside a cooldown is dropped rather than queued. Two
  kinds are exempt from the cooldown: a `user_reply` bypasses them entirely, and
  a `user_reply`/`alert` that lands while a Hermes turn is running waits in a
  **one-slot pending queue** (the newest replaces the older, so an intermediate
  message is lost by design) and runs when that turn ends. Routine
  `interaction`/`window_title`/`idle_return` triggers are still dropped while
  busy, so background noise never stacks up behind a task. It cannot stop
  or configure the avatar (it only observes). The rules file is looked up from
  the cwd, beside the binary, or in a parent directory up to the
  `models/` root, so a checkout that keeps `watcher_rules.json` at the repo root
  works without copying it or setting `DESKTOP_BUDDY_NO_WATCHER`.
* **The picker is modal.** While the native dialog is open the overlay drops
  always-on-top; if the app is killed mid-dialog the flag is restored only on
  the next launch.

## 9. Roadmap

1. **Real lip-sync** — viseme timeline + amplitude input from TTS.
2. **Deformation** — 2-bone skinning or a small mesh per part (revive the wgpu
   spike), optional IK targets for legs/arms.
3. **Mixed-DPI monitors** — one overlay window per monitor (or per-monitor scale
   factors) so a 150 % display next to a 100 % one maps rects exactly.
4. **Packaging** — finish the bundle config (icons exist), ship an installer,
   store models under `%APPDATA%` when installed.
5. **Testing** — unit tests for `models::validate_rig` / `import_asset`, and a
   headless CDP smoke test for the editor, the timeline and the speech lifecycle.
6. **Asset pipeline niceties** — SVG rasterisation for dimension probing,
   sprite atlases, and a model gallery UI.
7. **Richer awareness** — per-window activity in `/context` (process name,
   focused app class), and a replayable event log (`Last-Event-ID`) for SSE.
8. **Animation depth** — easing curves per segment, additive clip layers, clip
   blending/cross-fade, and keyframe undo (the authoring timeline landed in
   Stage 2.5).
