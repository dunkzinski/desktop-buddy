# ARCHITECTURE — char-rig desktop avatar

Technical documentation for the AI desktop avatar in `C:\hermes\char-rig`.
Everything here was read from the current sources; file names, function names,
constants and payloads are quoted from the code, not from memory.

Last verified against: `src-tauri/src/{lib,api,models}.rs`, `ui/index.html`,
`src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `models/*/model.json`,
`src-tauri/src/bin/{char-rig-mcp,char-rig-watcher}.rs`.

---

## 1. Project overview

A 2D character that lives on top of the Windows desktop: a transparent,
always-on-top, full-screen WebView2 window that draws a rigged sprite/shape
avatar and lets it walk around, emote, blink, talk and show speech bubbles.
A local HTTP API lets an external agent (LLM, script, automation) drive it.

### Feature summary

| Area | What exists |
| --- | --- |
| **Desktop windowing** | Transparent, borderless, always-on-top overlay covering the primary monitor; selective click-through so the desktop stays usable; custom titlebar with click-through toggle, minimize and close |
| **UI controls** | Avatar control panel (state readout, expression buttons, Wave / Idle / Walk / Center, New chat, Ask…, walk-speed slider, live position, API URL), model dropdown, Import…, a **chat icon** in the titlebar, and a **Watcher** row: on/off switch, `running`/`off`/`missing` badge and a **Logs ↗** pop-out console |
| **Ask / task from the desktop** | Floating command bar under the avatar — `Enter`/`/`, the titlebar chat icon, the **Ask…** button or a click on the bubble opens it, and a **system-wide hotkey** (`Alt+Space` by default, `hotkey` in `watcher_rules.json`) summons it from any application; type + `Enter` sends a `user_reply`, `Esc` closes it. Quick-task chips come from `quick_tasks` in `watcher_rules.json`, and **↺ / New chat** drops the conversation context (`POST /session/reset`) |
| **Model importing** | Rig manifests as `.json` or `.toml`; single images (`.png`, `.jpg`/`.jpeg`, `.webp`, `.svg`) as a **new character** or as a **part attached to the current rig**; native file picker; clear validation errors |
| **Rig Edit Mode** | Bone/joint/pivot/hitbox overlay; drag joints, drag rotation ring (unlimited multi-turn), wheel rotate / shift+wheel scale; re-parent; add bone, add limb, add part, duplicate/delete; inspector for transforms, limits, z-order and animation channels; undo; save to `rig.json`; reset |
| **Facial system** | Per-part sprite **variants**, 6 expression presets (`neutral`, `happy`, `surprised`, `thinking`, `sad`, `angry`) mapping to eye/mouth variant names, procedural squash/stretch fallback, expression holds with auto-revert |
| **Custom animations** | Optional `animations` block on the rig root: named keyframe clips (`duration_ms`, `loop`, `keyframes[]`) whose per-bone deltas are sampled with smoothstep easing and composed into the transform pass; timeline bar in Rig Edit Mode (clip CRUD, duration, loop, play/stop, 0–100 % scrubber with keyframe ticks, `+ Keyframe` capture) |
| **Life signs** | Auto-blink every 3–6 s (~120 ms); **text-driven lip sync** — the mouth follows the typewriter character by character (vowels wide open, soft consonants mid-open, spaces and punctuation closed) for both sprite variants and the procedural shape; **eye tracking** — pupils ease toward the pointer, the focused command bar, or an idle glance every 2–4 s, clamped to ±4 px so they never leave the socket; **hover response** — a soft head tilt when the cursor enters the avatar's bounds |
| **Speech bubbles** | Head-anchored DOM card, re-anchored to the head **every animation frame** (no lag while the avatar walks), typewriter reveal with caret, word wrap, auto-size, edge clamping / flip, auto-dismiss; the avatar stands still while a bubble is up (automatic wandering is paused — an explicit `/move` or keypress still wins); an optional **quick-reply box** (click the bubble, or just hover) with `Enter` to send and `Esc` to dismiss; an optional **details drawer** — a `details` string renders a monospace, scrollable pre block behind a **Details ▾** toggle plus a **Copy** button, and an open drawer holds the bubble up |
| **Agent API** | 35 local HTTP paths (41 method routes): state, movement, expressions, speech, models, rigs, parts, custom animations, desktop context, clipboard read **and write**, quick replies, external alerts, session reset, watcher control, **AI profile switching + health**, shutdown |
| **Desktop awareness** | `GET /context` — foreground window title, its owning executable (`process_name`), the focused window's `window_rect` plus the top few `visible_windows` (all converted to CSS px), machine-wide idle seconds (Win32 probes, safe defaults elsewhere); `POST /perch` — stand the avatar on or beside a window border and keep its gaze there **and** the last 15 SSE frames as the agent's short-term memory; `GET /clipboard` — the last-copied text, capped at 2000 characters; `POST /clipboard` — put up to 32 000 characters on the clipboard |
| **Event stream** | `GET /events` — Server-Sent Events over a 64-frame `tokio::broadcast` channel: `state`, `speech`, `speech_cleared`, `model`, `rig_saved`, `pose`, `part_patch`, `animate`, `interaction`, `user_reply`, `alert`, `session_reset`, `ai_profile`, `watcher`; 15 s keep-alive, plus a 15-frame history ring buffer served inside `/context` |
| **Interactions** | Play-mode click / double-click / drag on the avatar's bounds → `notify_interaction` → `interaction` frame; a typed command-bar message → `notify_reply` → `user_reply` frame |
| **Sessions** | The watcher owns a mutable `{session}` id (`char-rig-<seed>`, `-2`, `-3`… after each rotation) substituted into `hermes_cmd`; **New chat** / `POST /session/reset` broadcasts `session_reset` and rotates it; `max_session_turns` / `session_idle_reset_secs` rotate automatically |
| **MCP bridge** | `char-rig-mcp.exe` — a second binary exposing 14 tools (`avatar_speak`, `avatar_emote`, `avatar_move`, `avatar_perch`, `avatar_wave`, `avatar_pose`, `avatar_get_context`, `avatar_get_clipboard`, `avatar_set_clipboard`, `avatar_list_models`, `avatar_switch_model`, `avatar_list_animations`, `avatar_animate`, `avatar_switch_ai`) to any MCP client over stdio JSON-RPC 2.0; stateless, forwards each call to the HTTP API and reports a down host as a readable tool error |
| **Desktop watcher** | `char-rig-watcher.exe` — a third binary that subscribes to `GET /events` and polls `GET /context` and only acts when a rule fires: an `interaction` (`double_click`/`drag_end`), a `window_title` change matching a keyword **or a per-app rule**, an `idle_return` after a long idle stretch, a `user_reply` from the command bar (no cooldown — the user asked directly), or an `alert` pushed by `POST /alert` under its own cooldown. Per-trigger cooldowns, an immediate ack (expression or custom clip), an optional webhook, and an optional spawned Hermes command with a one-line prompt on a persistent, rotatable session (`{session}`). Spawned and supervised by the app itself, with an on/off switch (`GET`/`POST /watcher`, the panel toggle), a 200-line piped log tail in the pop-out console, and crash restart. Dispatch is single-flight with a **one-slot pending queue**: a `user_reply`/`alert` arriving mid-turn waits and runs as soon as that turn ends (newest wins), while routine triggers are dropped. Exponential-backoff reconnect; **zero LLM tokens while idle** |
| **Persistence** | `models/<name>/model.json` (shipped), `rig.json` (editor override), `models/.state.json` (selected model) |

### Repository map

| Path | Status | Notes |
| --- | --- | --- |
| `src-tauri/` | **live** | The application (Rust + Tauri 2) |
| `ui/` | **live** | Modular frontend (see structure below) |
| `models/` | **live** | Model folders + `models/.state.json` |
| [`API.md`](API.md), [`MODEL_FORMAT.md`](MODEL_FORMAT.md), [`README.md`](README.md) | **live docs** | Endpoint reference, manifest guide, quick start (this file is the design overview) |
| `watcher_rules.json` | **live config** | Optional rules for `char-rig-watcher.exe` (interaction kinds, title keywords, **per-app `app_rules`**, alert cooldown, idle threshold, cooldowns, ack emote, Hermes command/session, hotkey, webhook); built-in defaults when absent |
| `docs/`, `tools/`, `TASKS.json` | **live** | Screenshots, `verify_docs.py` / `mcp_smoke_test.py` / `cdp-eval.mjs`, and the stage ledger |

The legacy prototypes that used to sit in this tree — `ai-avatar-bridge/`,
`avatar-desktop/`, `avatar-editor/`, `spikes/`, `src-tauri/avatar-renderer.html`
and the empty `src/char_rig/`, `tests/`, `assets/` placeholders — have been
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
    └── main.js         # Entry point: DOM bindings, keyboard shortcuts, gaze targeting (`updateGaze`, `attachGazeTracking`), `window.__avatarDebug`, `requestAnimationFrame` main loop (`step`, `frame` — bubble re-anchor), the ask/task command bar (`openCommandBar`, `sendUserMessage`, quick-task chips, `resetSession`)
```

> **No Node/npm build step.** `src-tauri/tauri.conf.json` serves `../ui` directly into WebView2 (`frontendDist: ../ui`, `"withGlobalTauri": true`). Native ES modules (`<script type="module">`) load at runtime.

> The app is **only** `src-tauri` + `ui` + `models` (+ `docs`/`tools` for
> development). The legacy prototype trees are gone — see the repository map
> above.

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
| `avatar-cursor` | `spawn_cursor_tracker` | until `AppCtx::running` is false | Global cursor poll + click-through toggling |
| `avatar-watcher-log` (x2) | `spawn_log_reader` | until the child closes stdout/stderr | Drain the watcher's pipes into the `AppCtx` ring buffer and echo each line to stderr |
| `avatar-watcher-supervisor` | `supervise_watcher` | until `AppCtx::running` is false | `try_wait`s the supervised `char-rig-watcher.exe` every 250 ms and respawns it after a crash (2 s, doubling to 30 s); parks while the switch is off |
| `avatar-hotkey` | `spawn_hotkey_listener` | until `AppCtx::running` is false | Registers the system-wide hotkey (`RegisterHotKey`) and polls its thread message queue every 120 ms; on `WM_HOTKEY` it disables click-through, focuses the overlay and emits `summon-command-bar`, then `UnregisterHotKey` on the way out |

Nothing on the API side ever touches the GUI thread directly: handlers mutate
shared state and emit Tauri events; the page applies them on its next frame.

### Startup sequence (`lib.rs::run`)

1. `AppCtx::new(api_port())` — port from `CHAR_RIG_API_PORT`, else `8765`.
2. `ModelCtx::new()` — resolves `models_root()` and the saved selection.
3. Create a `tokio::sync::oneshot` pair; store the sender in `AppCtx`.
4. `tauri::Builder` → `manage(AppCtx)`, `manage(ModelCtx)`, register 29 commands.
5. `setup`:
   * Resize/move the window to the primary monitor using `Physical` position/size.
   * `model_ctx.ensure_loaded()` (logs a warning and falls back if a model is broken).
   * `api::start(...)` → binds, spawns the API thread, stores the URL, emits `api-status`.
   * `start_watcher(...)` → resolves `char-rig-watcher.exe` next to the running
     exe and spawns it with `--api-url <bound url>`, `--rules <found>`
     (`watcher_rules_path`: cwd → beside the binaries → parents up to
     `models_root()`'s parent), `stdin(Stdio::null())`, `stdout`/`stderr` piped
     into the log ring buffer, cwd = the exe's directory and
     `CREATE_NO_WINDOW`; skipped entirely when `CHAR_RIG_NO_WATCHER` is
     `1`/`true`. The child goes into `AppCtx::watcher` (switch + child + paths +
     log tail), reader threads start, and the supervisor takes over.
   * `spawn_cursor_tracker(...)`.
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

---

## 3. Codebase map

### `src-tauri/src/lib.rs` (2599 lines)

| Section | Responsibility |
| --- | --- |
| consts | `AVATAR_STATES`, `AVATAR_EXPRESSIONS` (6), `INTERACTION_KINDS` (`click`, `double_click`, `drag_end`), `EVENT_CHANNEL_CAPACITY = 64`, `EVENT_HISTORY_CAPACITY = 15`, `DEFAULT_API_PORT = 8765`, `MAX_SPEECH_CHARS = 2000`, `MAX_SPEECH_DETAILS_CHARS = 8000`, `MAX_CLIPBOARD_CHARS = 2000`, `MAX_CLIPBOARD_WRITE_CHARS = 32000`, `MAX_REPLY_CHARS = 500`, `MAX_QUICK_TASKS = 6`, `DEFAULT_HOTKEY = "Alt+Space"`, `HOTKEY_ID` |
| `AvatarState`, `HitRegion` | serialized shared state; region contains-test |
| `lock_or_recover`, `snapshot` | poisoned-lock recovery (never panic on the UI path) |
| `AppCtx` + impl | avatar state, regions, `api_port`, `speech` slot, `events` broadcast sender, `recent_events: Mutex<VecDeque<Value>>`, `event_seq`, `speech_seen`, `expression_commanded_at`, `click_through`, `running`, `api_url`, `shutdown_tx`, `watcher: Mutex<WatcherState>`; `emit_event` (which also remembers the frame), `recent_events`, `apply_ignore_cursor`, `request_shutdown` (which calls `stop_watcher`), `watcher_status`, `set_watcher_enabled`, `push_watcher_log`, `spawn_watcher` |
| `ModelCtx` + impl | `root`, `current`, `loaded` cache, `pose`, `joints`, `part_patches`, `dialog_open`; `load`, `ensure_loaded`, `invalidate`, `apply_pose`, `parts_snapshot`, `rig_path` |
| `publish` | bump revision + emit `avatar-state-changed` + broadcast `state` |
| `model_event_payload` | compact `{name, label, rig_path, is_override, bones, images, reason}` for `model` frames |
| commands | 29 `#[tauri::command]` fns (see §6) |
| `do_emote`, `do_speak`, `do_clear_speech` | shared logic used by both commands and HTTP routes (each broadcasts its frame) |
| `do_animate` | custom keyframe playback: emits `avatar-animate` to the page **and** an `animate` frame on the SSE bus (`clip = None` stops) |
| `do_notify_interaction` | interaction validation + `interaction` frame; the command is a thin wrapper so it is unit-testable |
| `do_user_reply`, `notify_reply` | quick-reply validation (non-blank, ≤ `MAX_REPLY_CHARS`), bubble dismissal and the `user_reply` frame; the command adds the page-side `speech-clear` event |
| `do_session_reset`, `reset_session` | broadcast `session_reset` (`{reason}`) + dismiss the bubble; the watcher does the actual rotation |
| `quick_tasks`, `default_quick_tasks` | read `quick_tasks` out of `watcher_rules.json` for `GET /watcher` (blank entries skipped, capped at `MAX_QUICK_TASKS`, built-in chips when absent/unreadable) |
| `AiProfileInfo`, `ai_profiles_from_document`, `ai_profiles` | parse `active_profile` + `ai_profiles` out of the rules document (unknown `kind`s skipped, label falls back to the model then the name, a pre-profiles file still reports a synthesised `hermes` / `hermes_cli` entry); `summary()` omits `endpoint` for a CLI profile |
| `health_probe_urls`, `probe_openai_health`, `ai_health`, `find_on_path`, `hermes_on_path` | the liveness probe behind the panel's dot: a GET of the server's /health then /v1/models with an 800 ms cap → `online` / `offline` (plus the endpoint and error in `detail`); a `hermes_cli` profile reports `status: "cli"` with `reachable` = whether `hermes` resolves on `PATH` |
| `ai_status_json`, `set_active_ai_profile`, `with_active_profile`, `write_rules_atomically` | the `GET /ai-profiles` body; the switch (validate against the file → rewrite **only** the `active_profile` string so hand-written formatting and key order survive → write a sibling `.json.tmp` and rename, so the rules file is never half-written → re-parse to prove it → re-read the status); an unknown name is an error that lists the valid ones |
| `do_user_reply_with_profile` | `do_user_reply` plus an optional `profile`, carried in the `user_reply` frame so a chip can pin its own brain |
| `desktop_context` | Win32 `GetForegroundWindow`/`GetWindowTextW`, `GetWindowThreadProcessId`/`OpenProcess`/`QueryFullProcessImageNameW` (the `process_name`), `GetLastInputInfo`/`GetTickCount64`, `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` for `window_rect` and `EnumWindows` for `visible_windows`, behind `GET /context`; safe defaults off Windows |
| `physical_rect_to_logical`, `primary_scale`, `primary_viewport` | physical px → CSS px via `GetDpiForSystem()/96`, degenerate rects rejected, monitor size in CSS px |
| `perch_target`, `PerchEdge`, `PERCH_EDGES`, `AvatarBounds` | pure perch geometry: feet on a top/bottom edge, standing beside a left/right edge, `offset` along the edge, clamp to the monitor — unit-tested without Win32; `AvatarBounds` is the art's extent per direction around the root |
| `tool_perch`, `tool_move` | MCP-side argument validation (`edge`, `look_at`) before any HTTP call |
| `clipboard_text`, `truncate_chars` | Win32 `OpenClipboard`/`GetClipboardData(CF_UNICODETEXT)`/`GlobalLock` read behind `GET /clipboard`, capped at `MAX_CLIPBOARD_CHARS` on a char boundary; every path balances its handle |
| `write_clipboard_text`, `ClipboardWrite` | the Win32 write half: `OpenClipboard` → `EmptyClipboard` → `GlobalAlloc(GMEM_MOVEABLE)` + UTF-16 copy → `SetClipboardData(CF_UNICODETEXT)` → `CloseClipboard`, capped at `MAX_CLIPBOARD_WRITE_CHARS` and reported as `{chars, truncated}`; a failed `SetClipboardData` frees the block instead of leaking it. Drives `POST /clipboard` and the `set_clipboard` command |
| `Hotkey`, `parse_hotkey`, `virtual_key`, `hotkey_spec` | pure parser for the `"Ctrl+Shift+A"` grammar (modifier bits + virtual-key code) and the `hotkey` lookup in `watcher_rules.json`; a bad value is a logged warning, never a panic |
| `summon_command_bar`, `spawn_hotkey_listener` | the `avatar-hotkey` thread: `RegisterHotKey` (+ `MOD_NOREPEAT`), a 120 ms `PeekMessageW` poll, then `set_click_through(false)` + `apply_ignore_cursor(false)` + `window.set_focus()` + the `summon-command-bar` event; `UnregisterHotKey` on `request_shutdown` |
| `cursor_position`, `spawn_cursor_tracker` | Windows cursor poll (stub returning `None` elsewhere) |
| `WatcherState`, `watcher_status`, `set_watcher_enabled`, `push_watcher_log` | on/off switch, live `Child`, resolved `binary`/`rules` paths and a 200-line `VecDeque` log tail; the status JSON feeds `GET /watcher`, `get_watcher_status` and every `watcher` frame (Tauri `watcher-status` + SSE) |
| `watcher_disabled_by`, `watcher_binary_name`, `watcher_binary_path` | `CHAR_RIG_NO_WATCHER` parsing; resolves `char-rig-watcher{EXE_SUFFIX}` next to `current_exe()` |
| `watcher_rules_candidates`, `watcher_rules_path` | rules lookup order: cwd → beside the binaries → parents up to `models_root()`'s parent |
| `clock_stamp`, `stamped`, `push_log_line`, `log_watcher_event`, `spawn_log_reader` | `[HH:MM:SS]` local-time stamps (`GetLocalTime`), bounded ring-buffer append, and the two pipe readers that echo every line to the app's stderr |
| `spawn_watcher`, `supervise_watcher`, `watcher_health`, `start_watcher`, `kill_and_reap` | spawn with `--api-url`, `--rules`, null stdin, piped stdout/stderr, `CREATE_NO_WINDOW`, cwd = exe dir; then `try_wait` polling, crash restart with backoff (parked while off), and a store-under-the-lock handshake that cannot orphan a child during shutdown |
| `api_port`, `run` | startup |
| `tests` | 67 unit tests over the event envelope, sequence numbers, the 15-frame history ring buffer, interaction validation, quick replies (trim, caps, bubble dismissal, `user_reply` frame), `session_reset` (frame + bubble dismissal), `quick_tasks` (defaults, file parse, blank-entry filtering, cap, missing/broken fallback), `truncate_chars`, the clipboard contract (read cap + write cap + char boundary), the desktop-context shape, the hotkey grammar (`parse_hotkey`) and the `hotkey` rules lookup, `model_event_payload`, the `CHAR_RIG_NO_WATCHER` parser, the watcher binary name, `watcher_health`, the log ring buffer, the watcher status keys, the rules lookup order, and the shutdown kill (no orphaned pid) |

### `src-tauri/src/api.rs` (925 lines)

| Section | Responsibility |
| --- | --- |
| `ApiCtx` | `app`, `shared: Arc<AppCtx>`, `model: Arc<ModelCtx>`, `started` |
| `watcher_status`, `set_watcher` | `GET /watcher` / `POST /watcher` → `AppCtx::watcher_status` / `set_watcher_enabled` |
| `ai_profiles`, `switch_ai_profile` | `GET /ai-profiles` / `POST /ai-profile` → `ai_status_json` / `do_set_ai_profile`, both on a `spawn_blocking` thread because the health probe is a blocking 800 ms HTTP GET; `event_name` maps the frame to `ai_profile` |
| `start` | bind, spawn thread, Tokio runtime, axum serve, graceful shutdown |
| `bind_first_available` | port fallback `preferred..preferred+9` |
| `router` | 33 routes |
| request bodies | `SetStateBody`, `MoveBody`, `ExpressionBody`, `ClickThroughBody`, `HitRegionsBody`, `ModelBody`, `RigBody`, `PoseBody`, `ImportBody`, `PartPatchBody`, `MouthBody`, `EmoteBody`, `SpeakBody` (incl. optional `details`), `ReplyBody`, `AlertBody`, `ClipboardBody`, `AnimateBody` |
| `ApiError` | `IntoResponse` → `{"ok":false,"error":…}` + status |
| `context` | `GET /context` — OS probes (title, process, idle, `window_rect`, `visible_windows`), `avatar_state` snapshot and the `recent_events` history |
| `perch` | `POST /perch` — resolve a window rect (by `window_process` or the foreground window), turn `edge` + half-bounds + `offset` into a clamped target, mark the state `perched` and publish a `state` frame |
| `clipboard`, `set_clipboard`, `reply`, `alert` | `GET /clipboard` (never fails: empty text for a non-text clipboard), `POST /clipboard` (`{text}` → `write_clipboard_text`, `{ok, chars, truncated}`; a held-open clipboard is a 400 to retry), `POST /reply` (`user_reply` frame + bubble dismissal) and `POST /alert` (severity validation → `alert` frame) |
| `ALERT_SEVERITIES` | `info` \| `warn` \| `error`, the only values `POST /alert` accepts |
| `events` | `GET /events` — SSE from `BroadcastStream`, `hello` frame, 15 s keep-alive |
| `event_name` | maps a frame `type` onto the SSE `event:` field (`&'static str`) |
| `list_animations`, `animate`, `stop_animation` | `GET /animations` (clip catalogue from the loaded rig) and the two stop/play spellings for `POST /animate` |
| handlers | one per route (see §7) |

### `src-tauri/src/models.rs` (1081 lines)

| Function | Responsibility |
| --- | --- |
| `models_root` | `CHAR_RIG_MODELS` → next to the exe (`models`, `../models`, `../../../models`, `../../../../models`) → compile-time `<repo>/models` |
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
| `tests` | 6 unit tests over the animation schema (opt-in block, `duration_ms`, `t` range, unknown bones, malformed shapes) |

### `src-tauri/src/main.rs` (6 lines)

Calls `avatar_desktop_lib::run()`; sets `windows_subsystem = "windows"` in release builds.

### `src-tauri/src/bin/char-rig-mcp.rs` (1300 lines)

The **second binary** (`char-rig-mcp.exe`), built by the same `cargo build`. It is
independent of the Tauri lib: a stdio MCP server that owns no state and forwards
each tool call to the HTTP API with `reqwest::blocking`.

| Section | Responsibility |
| --- | --- |
| consts | `DEFAULT_API_URL`, `ENV_API_URL` (`CHAR_RIG_API_URL`), `CONNECT_TIMEOUT` 3 s, `REQUEST_TIMEOUT` 10 s, `EXPRESSIONS` (mirrors `AVATAR_EXPRESSIONS`), `SUPPORTED_PROTOCOLS`, `INSTRUCTIONS` |
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

### `src-tauri/src/bin/char-rig-watcher.rs` (3340 lines)

The **third binary** (`char-rig-watcher.exe`), built by the same `cargo build`.
It owns no UI and no state of its own: a headless trigger pump that reads the
avatar's `GET /events` stream and polls `GET /context`, and only reaches out
when a rule fires. With no trigger it spends no tokens — the whole idle cost is
one open SSE response and one small JSON poll every 20 s.

```text
  avatar-desktop.exe ──SSE /events────┐
                     ──GET /context─┴──▶ char-rig-watcher ──▶ POST /emote (ack)
                                                          ──▶ --webhook-url
                                                          ──▶ ai_profile (backend)
                                                                ├─ openai_compat ──▶ POST /v1/chat/completions
                                                                │                   └─▶ /perch + /emote + /speak
                                                                └─ hermes_cli ─────▶ spawn (… {session} …)
```

| Section | Responsibility |
| --- | --- |
| consts | `DEFAULT_API_URL`, `ENV_API_URL` (`CHAR_RIG_API_URL`), `ENV_HERMES_CMD` (`CHAR_RIG_HERMES_CMD`), `DEFAULT_RULES_FILE`, `DEFAULT_CONTEXT_INTERVAL_SECS` 20, `DEFAULT_INTERACTION_COOLDOWN_SECS` 15, `DEFAULT_CONTEXT_COOLDOWN_SECS` 300, `DEFAULT_ALERT_COOLDOWN_SECS` 60, `DEFAULT_IDLE_THRESHOLD_SECS` 900, `DEFAULT_ACK_EMOTE` `thinking`, `ACK_EMOTE_DURATION_MS` 4000, `DEFAULT_HERMES_TIMEOUT_SECS` 120, `FALLBACK_SPEECH_TEXT` / `FALLBACK_SPEECH_EXPRESSION` `sad` / `FALLBACK_SPEECH_DURATION_MS` 4000 (the bubble the watcher posts itself when a dispatch fails), `EXPRESSIONS` (mirrors `AVATAR_EXPRESSIONS`, decides emote vs. clip ack), `HEADLESS_DIRECTIVE`, `CONNECT_TIMEOUT` 3 s, `CONTEXT_TIMEOUT` 5 s, backoff 1 s → 30 s, `TRIGGER_QUEUE` 64 |
| `CliConfig` / `Config` | raw flags vs. resolved runtime config; `Config::resolve` merges **CLI → env → rules file → built-in default** (`hermes_timeout` = `--hermes-timeout` → `hermes_timeout_secs` → 120; no env var) |
| `parse_cli` / `usage` | hand-rolled flag parser (`--flag value` and `--flag=value`), `-h/--help`, exit 2 on bad input; env fallbacks applied after the flags |
| `InteractionRules` / `ContextRules` / `AppRule` / `AlertRules` / `Rules` | serde-defaulted rule model (`app_rules` and `alerts` optional, so the old schema still parses); `Rules::load` (explicit path, else cwd then next to the exe), `from_json`, `interaction_allowed`, `matched_keyword` (case-insensitive substring), `matched_app_rule` (process + title, first match wins, blank matcher = wildcard), `idle_transition` (`was_idle && idle < threshold`) |
| `Cooldowns` | per-key `HashMap<String, Instant>`; `allow` checks **and** stamps, so a suppressed trigger does not extend the window |
| `SseParser` | incremental `text/event-stream` parser: `event:`/`data:` fields, multi-line `data`, `:` keep-alive comments, CRLF, and frames split across arbitrary TCP reads; `message` is the default event name |
| `Trigger` | `{event_type, detail, window_title, idle_secs, kind, keyword, x, y, animation, hint, text, severity}`; `payload(ts_ms)` builds the webhook / dry-run JSON (absent fields omitted), `brief()` builds the one-line situation (what an OpenAI-compatible model gets), `prompt()` is that plus the CLI's `HEADLESS_DIRECTIVE` — quoting a `user_reply`'s text verbatim and appending an `app_rules` `prompt_hint` as `Context:` — and ends with `HEADLESS_DIRECTIVE`: no follow-up questions, **`tool_search(query="avatar_")` as the first tool call of a session** (Hermes defers MCP tools, so `mcp__char_rig__avatar_*` is not callable until the search loads the schemas — a model that skips it retries the "deferred tools must be invoked through tool_call" error until `--hermes-timeout` kills it), the literal `mcp__char_rig__avatar_speak(text, expression?, duration_ms?)` / `avatar_emote(expression, duration_ms?)` (with the six expressions) / `avatar_wave()` / `avatar_animate(name, loop?)` / `avatar_get_context()` / `avatar_get_clipboard()` / `avatar_perch(edge, window_process?, offset?)` signatures, an explicit invitation to chain several calls in one turn, and the **under-180-characters** bubble rule so a bubble never overflows the screen |
| `interaction_trigger` | gate on `interaction_allowed` + cooldown, then format `the user double-clicked the avatar at (x, y)` |
| `user_reply_trigger` | `user_reply` frame → trigger with the user's trimmed text; **no cooldown and no interaction gate** — the user asked directly |
| `alert_trigger` | `alert` frame → trigger with `source`/`summary`/`severity`; honours `alerts.enabled` and the `alert` cooldown |
| `ack_plan` | picks the immediate ack: a matched `app_rules[].animation` (an expression name → `Ack::Emote`, anything else → `Ack::Animate` clip) or the generic `--ack-emote`; `""` disables it |
| `context_triggers` | evaluates one poll: title-change matching `keyword OR app_rule` (the app rule wins when both match and supplies `animation`/`hint`), and the idle→active rule; also maintains `last_title` / `last_idle_secs` / `was_idle`; the **first** poll only sets the baseline (`initialized`) |
| `Shared` | `Mutex`-guarded snapshot: latest `/context`, `initialized`, `last_title`, `last_idle_secs`, `was_idle`, `cooldowns`; `lock` recovers from poisoning |
| `SingleFlight` / `FlightGuard` | one-permit `Arc<AtomicBool>` latch (`compare_exchange`) guarding the whole dispatch, plus a one-slot `Mutex<Option<Trigger>>` pending queue and a `Notify` the guard pings on drop; `offer` returns `Immediate`/`Queued`/`Dropped`, so only `user_reply`/`alert` queue while busy. The guard releases the permit on `Drop`, so a panic in a spawned task cannot wedge the watcher |
| `split_command` / `command_argv` / `spawn_hermes` | quote-aware argv split (no shell), `{prompt}` substitution or append-as-last-arg, then an `async fn` `tokio::process` spawn with `stdin(Stdio::null())` (an interactive prompt reads EOF instead of hanging), `kill_on_drop`, and `tokio::time::timeout(--hermes-timeout)` around the wait — an expired child is `start_kill`ed and reaped. The `FlightGuard` moves into the spawned task, so the permit covers the process lifetime. The task ends by posting the **fallback bubble** for any non-clean ending (see below) |
| `RunOutcome` / `fallback_speech_body` / `fallback_speech_for` / `deliver_fallback` | how the child ended (`Succeeded`, `Failed` = non-zero exit, `TimedOut`, `WaitFailed`, `SpawnFailed`); every outcome but `Succeeded` posts `{"text": "Sorry, I hit a snag or timed out.", "expression": "sad", "duration_ms": 4000}` to `POST /speak` before the permit drops, so a crash, a bad argv or a wedged model (e.g. the deferred-`tool_search` retry loop) can never leave the overlay on "Thinking…". `POST /speak` is the same route `avatar_speak` uses, and an unreachable avatar is logged, never propagated |
| `AiProfile` | the tagged (`#[serde(tag = "kind", rename_all = "snake_case")]`) inference target: `OpenaiCompat { label, endpoint, model, api_key?, max_tokens?, timeout_secs?, system_prompt? }` or `HermesCli { label, cmd, timeout_secs? }`; `kind`, `label`, `endpoint`/`model`, `timeout` and `is_cli` are the only accessors the rest of the file needs |
| `AiRuntime` / `AiShared` | which profiles exist and which is active: `from_rules` synthesises a `hermes` / `hermes_cli` profile from a legacy `hermes_cmd` (and keeps it as the escalation target when the file also carries profiles), `resolved(override)` applies an `app_rules`/chip `profile` then falls back to `active_profile`, `cli_fallback` finds the agent, `set_active` validates. `AiShared` wraps it in a `Mutex` next to the rules path, so an `ai_profile` frame can `reload()` the edited file and swap the runtime without a restart |
| `AiReply` / `AvatarAction` / `truncate_chars` | the JSON contract (`speech`, `expression?`, `perch_edge?`, `window_process?`, `details?`, `escalate?`): `parse` tolerates ```json fences and leading prose, lowercases and validates `expression` against `EXPRESSIONS` and `perch_edge` against `PERCH_EDGES`, drops unknown values, and `actions()` fixes the order perch → emote → speak with the bubble cut to 180 chars (`details` capped at 8000) |
| `InferenceBackend` + two impls | the dispatch seam: `fn dispatch(&self, DispatchCtx) -> Pin<Box<dyn Future<Output = DispatchReport> + Send>>` (boxed futures instead of an `async_trait` dependency). `OpenaiCompatBackend` normalises the endpoint to the server's chat-completions path, **inlines a fresh `/context` snapshot** (and the clipboard when the prompt asks for it — a no-tools model cannot call the MCP tools, and the clipboard is private until the user asks), sends one `response_format: {"type":"json_object"}` request, then drives `/perch` + `/emote` + `/speak`; a request failure posts the sad fallback bubble. `HermesCliBackend` is the old `spawn_hermes` runner. `DispatchReport {backend, kind, actions, escalated, failure}` is what gets logged (and tested) |
| `apply_actions` / `post_fallback_speech` | the HTTP side effects, in `AvatarAction` order, each failure logged and swallowed so one rejected `/perch` cannot abort the `/speak`; `post_fallback_speech` is the shared "unstick Thinking…" bubble |
| `handle_trigger` | dry-run print → take the single-flight permit → the ack (`ack_plan`: `POST /emote` or `POST /animate`) → `POST --webhook-url` → resolve the profile (`trigger.profile` → `active_profile`) → `backend_for(...).dispatch(...)`, then, when the reply said `escalate: true`, a second flight through the `hermes_cli` profile; a trigger that finds the permit taken is **skipped** with a `debug:` line on stderr instead of stacking another model turn |
| `build_client` / `fetch_context` | reqwest client with a 3 s connect timeout; `GET /context` → `ContextSnapshot {window_title, process_name, idle_secs}` (an older avatar without `process_name` still works) |
| `handle_frame` | one SSE frame → at most one trigger; routes `interaction`, `user_reply` and `alert`, ignores `hello`, unparsable data and every other frame type |
| `stream_events` / `sse_loop` | consume `Response::bytes_stream()`, feed the parser, reconnect forever with doubling backoff capped at 30 s (reset on a successful subscribe) |
| `context_loop` | `tokio::time::interval` with `MissedTickBehavior::Delay`; one warning per offline streak, then silent retries |
| `dispatcher` | single consumer of the `mpsc` channel plus the `SingleFlight` latch, so two triggers can never race their side effects and a running child is never duplicated; `select!`s the channel against the latch's `finished` `Notify`, and drains the pending slot as soon as a flight ends |
| `main` | parse → load rules → spawn `sse_loop`, `context_loop`, `dispatcher` → `tokio::select!`; any task ending stops the process |
| `tests` | 86 unit tests: SSE parsing (chunk boundaries, comments, CRLF, multi-line data), rule matching, per-app rule matching (case-insensitive, blank wildcards, `animation`/`prompt_hint` carried onto the trigger), alert parsing/cooldowns/disable switch, the backward-compatible rules schema, cooldown suppression, `user_reply` triggers (no cooldown, quoted text), `handle_frame` routing for `user_reply`/`alert`, `ack_plan` (expression vs. clip vs. none), trigger payloads/prompts, argv splitting, CLI precedence, `--hermes-timeout` parsing + default + `hermes_timeout_secs` precedence, the headless directive (clipboard, context, chaining, 180-char cap) and its tool signatures, the session id/rotation helpers (`{session}` substitution, max-turns, idle reset, reset frames), the single-flight latch and its one-slot pending queue (immediate/queued/dropped offers, latest-wins, the wake-on-drop `Notify`), the deferred-MCP `tool_search` hint (present, ordered before the first tool signature, underscore tool names), the failure recovery (`fallback_speech_for` only for non-clean outcomes, a captured `POST /speak` body from `deliver_fallback`, nothing posted on success, and a real `python -c` child killed at a 1 s timeout that still posts the bubble and releases the permit), plus the AI profiles: the tagged `AiProfile` schema, the synthesised legacy `hermes` profile (with its timeout), active-profile resolution and per-trigger overrides, `set_active` validation, the escalation target, `app_rules`/`quick_tasks` profile pinning, `AiReply::parse` (plain / fenced / padded JSON, unknown enum values dropped), action ordering + the 180-char bubble, `chat_url` normalisation, the request body (`response_format: json_object`, `max_tokens`, system contract), a full chat-completions request → `/perch` → `/emote` → `/speak` run against a captured fake server, the sad fallback bubble on a dead endpoint, the escalating reply, an `ai_profile` frame re-reading the rules file, `Trigger::brief` vs `Trigger::prompt` (the CLI directive never reaches a no-tools model, and the word "clipboard" no longer leaks into an unrelated prompt), the no-tools prompt (`augmented_prompt` inlining a fresh `/context` snapshot, `mentions_clipboard` deciding *before* the request whether the clipboard belongs in it — and never reading it when the user did not ask), plus a guard test that the fake-server helper really answers a plain GET |

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
| `ui/index.html` | Lean semantic HTML shell (titlebar with the chat icon, canvas, control panel, inspector container, import chooser, floating command bar, speech bubble with its quick-reply form, toasts) linking `styles.css` and `<script type="module" src="./js/main.js"></script>`; the control panel carries the **Watcher** row and the **AI Brain** row (`#ai-profile-select` + `#ai-health-badge`) |
| `ui/styles.css` | All CSS rules (titlebar, control panel, rig inspector, import modal, speech bubble + quick reply + thinking state, command bar + chips, the `.ai-row` dropdown and its `.ai-health` dot with `online` / `offline` / `cli` variants, toasts) |
| `ui/js/ipc.js` | **Leaf** Tauri access: `call`, `listen`, `once`, `isTauri`. Reads `window.__TAURI__` lazily on every call (never cached at module scope) |
| `ui/js/state.js` | **Leaf** shared state objects (`avatar`, `RT`, `speech`, `gaze`, `viewport`, `watcher`, `ai` — the last mirroring `{active, profiles, health, checked}` for the **AI Brain** row), constants (`EXPRESSIONS`, `PRESET_STATES`, `PART_TAGS`), helpers, `toast`, `updateStatusUI`, `setExpression`, `snapshot`, `setDirty`, `activeState` (which routes the lip-sync mouth variant through `mouthState`), `blinkActive`, `markManual` |
| `ui/js/rig.js` | Model ingestion (`applyModel`, `boneDefaults`, `rebuildOrder`, `loadImages`, `refreshModelList`), forward kinematics (`animOffset` — the hover head tilt rides on it, `clampRotation`, `animationOffset`, `localRotation`, `computeWorld`), geometry (`partSize`, `sortedDrawables`, `partWorldMatrix` — the one world matrix the renderer and the editor both draw through, `partLocalRect` — how each shape type is anchored inside that matrix, `partBox`), hit-region publishing (`avatarBounds`, `wireRegions` — which also publishes the visible speech bubble and the open command bar so both stay clickable), and rig serialization (`serializeRig`, `serializeAnimations`, `saveRig`, `resetRig`) |
| `ui/js/animator.js` | Custom keyframe clips: catalogue (`animationClips`, `listAnimations`), transport (`playAnimation`, `stopAnimation`, `stopPlayback`, `seekAnimation`, `stepAnimation`), sampling (`sampleAnimation`), authoring (`setAnimationBone`, `captureKeyframe`, `addClip`, `renameClip`, `deleteClip`, `setClipDuration`, `setClipLoop`) and finished listeners (`onAnimationFinished`) |
| `ui/js/renderer.js` | Canvas pipeline: DPR resize (`initRenderer`, `resizeCanvas`), `drawFace`, `drawShape`, `drawPart`, rig overlay gizmos, `render`; `updateGazeDir` turns the smoothed aim point into a clamped pupil vector and `isPupilPart` decides between moving the pupil sprite or the whole eye; the mouth squash reads `speech.mouthSmooth` (the same cadence as the sprite variants) |
| `ui/js/editor.js` | Rig Edit Mode: `hitTest`, mouse/wheel handlers (wheel rotates a held part), inspector (`buildInspector`, `renderInspector`, `updateInspectorValues`), bone/part authoring (`addChildBone`, `deleteBone`, `addPartFromControls`, `addLimbFromControls`, `duplicatePart`, `deletePart`, `snapBoneToPart`, `bindExpressionSprite`, `attachStagedPart`), the animation timeline (`buildTimeline`, `updateTimeline`, `syncTimelinePlayhead`), and `undo` |
| `ui/js/speech.js` | Speech bubble lifecycle (`speak`, `updateSpeech`, `positionBubble`, `hideSpeech`), **text-driven lip sync** (`mouthPoseFor` vowel/consonant/punctuation table, `setMouthFrom`, `stepMouth` smoothing), the quick-reply box (`isReplyOpen`, `openReply`, `closeReply`, `toggleReply`), the **details drawer** (`detailsText`, `toggleDetails`, `detailsOpen`, `copyDetails` — which republishes the hit region so the expanded card stays clickable), the "Thinking…" card (`beginThinking`, `showThinking`, `endThinking`, plus the `THINKING_TIMEOUT_MS` 125 s safety net that clears a stuck card and puts the face back to `neutral`) and the command bar's anchor (`commandBarVisible`, `positionCommandBar`); the hold timer is suspended while the drawer is open, the box is open, the bubble is hovered or an input has focus |
| `ui/js/bridge.js` | Tauri event listeners (`avatar-state-changed`, `emote`, `speak`, `part-patch`, …), `reportState`, `applyRemoteState` (ignores frames that would yank a dragged avatar and never adopts a bare `walking` without its segment), `applyPartPatch`, `applyWatcherStatus` (which also mirrors `quick_tasks` for the command bar), the **AI Brain** layer (`applyAiProfiles` — rebuilds the dropdown only when the profile set changes, so the 12 s health refresh cannot collapse an open select, `renderAiProfiles` for the badge/dot, `refreshAiProfiles`, `setAiProfile`, `aiHealthStatus`, `aiActiveLabel`), click-through (`setClickThrough`), import flow (`importFromDialog`, `importImageAs`, chooser) |
| `ui/js/main.js` | Entry point: DOM bindings, keyboard shortcuts (`Enter`/`/` summon the bar), the `summon-command-bar` hotkey listener (opens the bar and restores click-through when it closes), **gaze targeting** (`attachGazeTracking`, `updateGaze` — pointer > focused command bar > idle glances, plus the eased hover weight), `window.__avatarDebug`, the **AI Brain** select (`setAiProfile` + a toast, with `refreshAiProfiles` every 12 s beside the watcher poll), behaviour loop (`step`, `startWalk`, `wander` — held back while a bubble is up), `requestAnimationFrame` driver (`frame`: `step` → `render` → `positionBubble`), play-mode click / double-click / drag-to-move detection (`attachInteractionDetection`, `clampAvatarPosition`) that feeds `notify_interaction`, and the ask/task surface (`setupCommandBar`, `openCommandBar`, `sendUserMessage`, `renderQuickTasks`, `resetSession`, the bubble's inline reply and its Details/Copy buttons) |

Module graph is a strict DAG (`ipc` / `state` are leaves):
`ipc, state → rig → animator → {renderer, speech, editor, bridge}; editor → bridge; all → main`.
`speech` also imports `ipc` (the clipboard command) and `rig` (`wireRegions`), both
downward edges — `rig` imports neither back, so the DAG stays acyclic.
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
`build.rs` is the standard `tauri_build::build()`.

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
| `windows` 0.58 (`Win32_Foundation`, `Win32_UI_WindowsAndMessaging`, `Win32_System_SystemInformation`, `Win32_UI_Input_KeyboardAndMouse`, `Win32_Graphics_Dwm`, `Win32_UI_HiDpi`) | `GetCursorPos` for the click-through tracker; `GetForegroundWindow`/`GetWindowTextW`/`GetLastInputInfo`/`GetTickCount64`/`DwmGetWindowAttribute`/`EnumWindows`/`GetSystemMetrics`/`GetDpiForSystem` for `GET /context` and `POST /perch` |

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
  "format": "char-rig/model@1",
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
format = "char-rig/model@1"
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
cargo build                 # dev binary -> target/debug/avatar-desktop.exe
./target/debug/avatar-desktop.exe
```

Environment variables: `CHAR_RIG_MODELS` (model root), `CHAR_RIG_API_PORT`
(default 8765). Logs go to stdout and the plugin's log file; startup prints
`avatar api listening on http://127.0.0.1:8765`.

### First run

* The window covers the primary monitor, transparent, always-on-top.
* Top-right titlebar: **rig mode (wrench)**, **click-through (cursor)**, **minimize**, **close**.
* Bottom-right panel: state/mood readout, model dropdown + **Import…** / **Rig edit**,
  expression buttons, Wave/Idle/Walk/Center, a **Watcher** row (switch, badge,
  **Logs ↗**), walk speed, position, API URL.
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

### Life signs: lip sync, gaze and hover

Three small behaviours make the avatar feel alive without any agent input:

* **Lip sync.** While a bubble is up the mouth is driven by the typewriter, not
  by a clock: `speech.js::mouthPoseFor` maps each revealed character to an
  openness — vowels (`a e i o u`) wide open, soft consonants mid-open, spaces
  and `. , ! ? -` closed — and `updateSpeech` eases `speech.mouthSmooth` toward
  it every frame. `state.js::activeState` picks the matching sprite variant
  (`open` / `flat` / `closed`, falling back to whichever exists) and
  `renderer.js` squash-stretches shape mouths by the same number, so the cadence
  survives a rig with no mouth variants at all.
* **Eye tracking.** `main.js::updateGaze` picks a target each frame — a focused
  `#command-input` (the user is typing *at* the avatar) beats the pointer, which
  beats a random idle glance every 2–4 s — smooths it, and `renderer.js` turns
  the offset into a pupil shift clamped to ±4 px (and to 18 % of the part), so a
  pupil can never slide out of its socket. A rig with no pupil sprite moves the
  whole eye by ~55 % of that instead, and the procedural `face` shape moves its
  iris arcs.
* **Hover.** The pointer entering the avatar's art sets `gaze.hover`, which
  `rig.js::animOffset` turns into a ~3° head tilt that eases in and out. It is
  the same maths as the `sway` channel, so it composes with any animation.

### Asking the avatar (command bar)

The avatar is addressable without a terminal. `Enter` or `/` anywhere on the
overlay (while it has keyboard focus), the **chat icon** in the titlebar, the
**Ask…** button in the control panel, or a click on the speech bubble all open a
floating command bar anchored under the avatar:

* **From any application: `Alt+Space`.** The app registers a system-wide hotkey
  (`RegisterHotKey` on the `avatar-hotkey` thread) and, when it fires, disables
  click-through, focuses the overlay and emits `summon-command-bar`; the page
  opens the bar and focuses `#command-input`, so you can start typing the moment
  the shortcut lands. Change it with `hotkey` in `watcher_rules.json`
  (`"Ctrl+Shift+A"`, `"F9"`, `"Win+Shift+Enter"`, …) and restart the app —
  a hotkey is registered once at startup. If another application already owns
  the shortcut, `RegisterHotKey` fails, the app logs a warning and everything
  else keeps working (`Enter`/`/` still open the bar while the overlay has
  focus). Closing the bar restores click-through.
* Type a prompt or a task, `Enter` sends it (`notify_reply` → `user_reply`), and
  the bubble immediately shows a **Thinking…** card with the `thinking` face so
  you can see the task was received before the local model answers. The card can
  never get stuck: the watcher posts a `sad` fallback bubble when the spawned
  agent times out or dies (see `char-rig-watcher.rs`), and `speech.js` keeps a
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

### Watcher console

The **Watcher** row in the control panel drives `char-rig-watcher.exe` without
restarting the app:

| Control | Effect |
| --- | --- |
| switch | `set_watcher_enabled(true/false)` — starts or stops (kills and reaps) the child. The supervisor parks while it is off and resumes when it is on |
| badge | `running` (green, with the pid in the tooltip), `off`, or `missing` (no `char-rig-watcher.exe` beside the app) |
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
keywords and cooldowns it now carries two blocks:

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
  "session_base": "char-rig",
  "hotkey": "Alt+Space",
  "hermes_cmd": "hermes chat --continue {session} --create-if-missing -Q -q"
}
```

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
* **`quick_tasks`** — the command bar's chips: `[{label, prompt}]`, at most 6.
  The app reads them straight from this file and serves them as `quick_tasks` on
  `GET /watcher`, so editing them needs no rebuild — reopen the bar (or it
  refreshes on open) and the chips change. A blank `label`/`prompt` is skipped;
  no array at all falls back to the three built-in chips.
* **`hermes_cmd`** — the shipped value keeps one conversation across triggers:
  `hermes chat --continue {session} --create-if-missing -Q -q`. `{session}` is
  substituted with the watcher's **current** session id (`char-rig-<epoch>`,
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
* **`session_base`** — prefix for generated ids (default `char-rig`).
* **`hotkey`** — the system-wide shortcut that summons the command bar
  (`"Alt+Space"` by default; modifiers `Alt`/`Ctrl`/`Shift`/`Win` plus a letter,
  digit, `Space`, `Enter`, `Tab`, `Esc`, an arrow or `F1`–`F24`). Read by the
  **app**, not the watcher, at startup: change it and restart the overlay. A
  blank or unparsable value keeps `Alt+Space`, and a shortcut another
  application owns is a warning in the log, never a crash.

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
`C` centre · `Space` wave · `R` rig mode · `Esc` close rig mode / collapse panel.

---

## 6. Tauri command surface (page ⇄ Rust)

34 commands, registered in `generate_handler!`. Most take `tauri::State<Arc<AppCtx>>`
and/or `State<Arc<ModelCtx>>`; argument names are snake_case for the annotated ones.

| Command | Purpose |
| --- | --- |
| `get_avatar_state` | snapshot of `AvatarState` |
| `get_runtime_info` | `{api_url, api_port, click_through, avatar_name, version}` — `avatar_name` is the loaded model's label |
| `set_avatar_position(x, y)` | set walk target, mode → `walking` |
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
| `import_asset(path, mode?, name?, tag?)` | import (see §5); broadcasts `model`/`import` when it installs a model |
| `patch_part(target, patch)` | runtime part patch |
| `set_mouth(state)` | mouth convenience wrapper |
| `emote(expression, duration_ms?, revert?)` | expression with hold |
| `speak(text, expression?, duration_ms?, char_ms?, details?)` | bubble + talking; `details` (≤ 8000 chars) becomes the expandable drawer |
| `set_clipboard(text)` | put text on the Windows clipboard (≤ 32 000 chars) → `{ok, chars, truncated}`; the bubble's **Copy** button |
| `clear_speech` / `get_speech` | stop / inspect speech |
| `get_parts` | parts snapshot |
| `save_rig(rig)` / `reset_rig` | persist / discard override (broadcast `rig_saved` / `model`+`reset`) |
| `set_pose(joints)` / `clear_pose` / `get_joints` | runtime joint posing |
| `get_watcher_status` | `{ok, enabled, running, pid, binary_path, rules_path, logs}` — the same JSON as `GET /watcher` |
| `set_watcher_enabled(enabled)` | start/stop `char-rig-watcher.exe` without restarting the app; returns the new status |
| `get_ai_profiles` | the `GET /ai-profiles` object: `{ok, active_profile, active_health, profiles}` (health probe included) |
| `set_ai_profile(name)` | the **AI Brain** dropdown: persists, probes and broadcasts like `POST /ai-profile`, then returns the new status |

Events emitted to the page: `avatar-state-changed`, `api-status`,
`model-changed`, `rig-pose`, `rig-saved`, `part-patch`, `emote`, `speak`,
`speech-clear`, `avatar-animate`, `summon-command-bar` (the global hotkey fired:
`{source, hotkey}` — the page opens the bar and focuses its input).

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
| POST | `/reply` | the user's quick reply (`{"text": "…"}`) → `user_reply` frame |
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
| POST | `/import` | import rig or image |
| GET | `/parts` | parts + tags + variants + runtime patches |
| POST | `/part` | patch parts by tag/id |
| POST | `/mouth` | mouth state / talking |
| POST | `/emote` | expression with optional hold |
| POST | `/speak` | speech bubble + talking |
| POST | `/clear_speech` | hide bubble, stop talking |
| GET | `/speech` | active bubble + talking |
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
{"ok":true,"service":"char-rig avatar api","version":"0.1.0","uptime_secs":396}
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
then clamps to `primary_viewport()` so a window at the screen edge cannot push
the avatar off screen. `offset` slides along the
edge. An invalid edge, no matching window and no focused window are all **400**
`{"ok":false,"error":"…"}` with an actionable message.

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
{"ok":true,"root":"C:\\hermes\\char-rig\\models","models":[
  {"name":"demo-blob","label":"Demo Blob (sprite parts)","bones":7,"parts":11,
   "has_rig_override":false,"error":null,"path":"…\\models\\demo-blob"}]}
```
Image imports in `mode:"part"` return the sprite **staged**, not attached:

```json
{"kind":"part","model":"demo-blob",
 "staged":{"image":"parts/hat.png","name":"hat.png","size":[64,64],"data_url":"data:image/png;base64,…"}}
```

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
 "visible_windows":[{"title":"main.rs - visual-studio-code","process_name":"Code.exe",
                     "rect":{"x":400.0,"y":300.0,"width":800.0,"height":500.0}}],
 "avatar_state":{"position":[700.0,500.0],"target_position":[700.0,500.0],
                 "state":"idle","expression":"neutral","revision":7,"talking":false,
                 "perched":false,"perch_rect":null,"look_at":null},
 "recent_events":[{"seq":6,"type":"speech","ts_ms":1794405560000,"payload":{"text":"Build finished."}}],
 "source":"win32"}
```
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
first. Off Windows the values are empty/`0`/`null` and `source` is
`"unsupported"`.

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
`/reply` takes `{"text"}` (non-blank, ≤ 500 chars), dismisses the bubble and
returns `{ok, text, subscribers}`. `/alert` takes `{source, summary, severity}`
with `severity` ∈ `info | warn | error` (default `info`) and returns
`{ok, alert, subscribers}`. Neither route calls a model: the watcher decides
whether an alert is worth a Hermes turn.

### `/session/reset`

```bash
curl -s -X POST localhost:8765/session/reset   # -> {"ok":true,"reason":"api","subscribers":1}
```
Broadcasts `session_reset` and dismisses the bubble. The app owns no session
state — `char-rig-watcher.exe` rotates its `{session}` id on the frame and logs
`session reset (api): char-rig-<ts> -> char-rig-<ts>-2`; the next task runs in a
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

* **Windows-only click-through and context.** The tracker uses `GetCursorPos`
  and `GET /context` uses `GetForegroundWindow`/`GetLastInputInfo`; on other
  platforms the window stays fully interactive and `/context` returns
  `{"window_title":"","idle_secs":0,"source":"unsupported"}`.
* **Primary monitor only.** The window is sized/positioned to
  `primary_monitor()`, and window rects are scaled by the *primary* monitor's DPI
  factor, so a window on a second monitor with a different scale lands on the
  wrong CSS coordinates. Multi-monitor and mixed-DPI setups are untested.
* **A perch is a point, not a tether.** `POST /perch` computes one target from
  the window rect captured at request time; if the user then moves, resizes or
  maximises that window the avatar stays where it landed (nothing watches the
  rect for changes). It also keeps whatever pose it arrived in — there is no
  sit/hang animation, so `bottom` just places it below the border, and the
  monitor clamp means a window flush with the top edge gets the avatar pushed
  down onto it rather than above it.
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
* **One conversation at a time.** The watcher holds a single session id; there is
  no per-task session and no history UI beyond the chat bubble. The id is
  re-derived at every watcher start (`<prefix>-<timestamp>`), so restarting the
  watcher or the app starts a fresh thread — "New chat" is only needed mid-run.
* **The command bar's hotkey is Windows-only and registered once.** `Enter`/`/`
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
* **One bubble at a time.** No queue, no history; `text` is plain text (max 2000
  chars) and the `details` drawer is plain, monospace text (max 8000 chars, no
  syntax highlighting, no rich formatting); dismissal is page-side, so `/speech`
  follows the page's reports.
* **Expression variants are optional.** Missing variants fall back to
  procedural squash/stretch rather than substituting another sprite.
* **Life signs are cosmetic and page-local.** Blinking only affects parts tagged
  `eye`. Lip sync is an approximation — characters are bucketed into
  vowel / soft-consonant / punctuation shapes, with no visemes, no audio
  analysis and no per-language tuning (it reads English-ish on any rig). Eye
  tracking only sees the pointer while it is over a published interactive
  region (the rest of the overlay is click-through, so the eyes fall back to
  idle glances when you are in another application), and it moves the pupils
  only — there is no head yaw, body turn or blink-on-focus. Hover is the head
  tilt alone.
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
* **Watcher triggers are rule-shaped, not semantic.** `char-rig-watcher.exe`
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
  or configure the avatar (it only observes), and its rules file is looked up
  next to the binary/cwd — a checkout that keeps `watcher_rules.json` at the
  repo root needs `CHAR_RIG_NO_WATCHER=1` plus a manual run, or a copy beside
  the exe.
* **The picker is modal.** While the native dialog is open the overlay drops
  always-on-top; if the app is killed mid-dialog the flag is restored only on
  the next launch.

## 9. Roadmap

1. **Real lip-sync** — viseme timeline + amplitude input from TTS.
2. **Deformation** — 2-bone skinning or a small mesh per part (revive the wgpu
   spike), optional IK targets for legs/arms.
3. **Multi-monitor** — one overlay window per monitor with a shared state.
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
