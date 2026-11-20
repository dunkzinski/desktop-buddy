# desktop-buddy — AI desktop avatar

A transparent, always-on-top 2D avatar that walks around the desktop and can be
driven by an external AI agent over a local HTTP API.

* **Shell / window**: [`src-tauri`](src-tauri) — Tauri 2 + WebView2 (Rust)
* **Renderer / UI**: [`ui/index.html`](ui/index.html) — canvas rig, control panel, titlebar, rig editor
* **Models / rigs**: [`models/`](models) — see [MODEL_FORMAT.md](MODEL_FORMAT.md)
* **HTTP API**: `src-tauri/src/api.rs` — axum 0.7 on its own thread + Tokio runtime

```
┌──────────────────────────────┐        invoke / events        ┌────────────────────────────┐
│ WebView2 page (ui/index.html)│◀─────────────────────────────▶│ Rust (Tauri app)           │
│ • animation, drawing         │                                │ • authoritative state      │
│ • control panel + titlebar   │  report_state / update_regions │ • commands                 │
│ • hit-region publishing      │                                │ • cursor tracker thread    │
└──────────────────────────────┘                                └──────────┬─────────────────┘
                                                                           │ HTTP 127.0.0.1:8765
                                                                           ▼
                                                                  external AI agent (curl, python, …)
```

## Build & run

```bash
cd src-tauri
cargo check          # type-check
cargo build          # dev binary -> target/debug/desktop-buddy.exe
cargo build --release
./target/debug/desktop-buddy.exe
```

Optional: `DESKTOP_BUDDY_API_PORT=9000` overrides the API port (default `8765`; if the
port is taken the next free port in `8765..8774` is used and logged).

## Interaction model

| Area | Clickable? |
| --- | --- |
| Titlebar, control panel, the avatar itself | yes — window is interactive |
| Empty desktop space | no — `WS_EX_TRANSPARENT` is applied, clicks reach the desktop |

The page publishes the rectangles that must stay interactive (`update_hit_regions`)
and a Rust thread polls the **global** cursor every ~35 ms, flipping
`set_ignore_cursor_events` on the window. A global poll is required because a
click-through window stops receiving mouse messages, so the page cannot detect
that the cursor came back on its own.

Click-through arms itself automatically once the page has published its first
region set. If the page fails to load, the window stays fully interactive — the
failure mode is "a normal window", never "an unclickable screen".

The **cursor button** in the titlebar (leftmost) toggles it manually; the state
is also settable over HTTP.

### Avatar interaction (click detection)

While the window is click-through, the avatar's bounding box is one of the
published hit regions, so pointer events over the art reach the page. In play
mode the page watches those events and reports them to Rust through the
`notify_interaction(kind, x, y)` command, which fans them out on
`GET /events` as `interaction` frames:

* `click` — press and release inside the avatar's bounds, no movement
* `double_click` — a second such click within 320 ms and 24 px of the first
  (the first click already reported as `click`)
* `drag_end` — press inside the bounds released after moving more than 8 px

Rig Edit Mode is excluded: there a press belongs to the gizmo editor. The
interaction is a *notification only* — it changes no state, so whatever the
avatar does back (wave, speak, …) is the agent's decision.

### Ask / task (command bar)

You never need a terminal to talk to the avatar:

* **`Enter`** or **`/`** anywhere on the overlay, the **chat icon** in the
  titlebar, the **Ask…** button in the control panel, or **clicking the speech
  bubble** all summon a floating command bar under the avatar.
* **From any application: `Alt+Space`** (change it with `hotkey` in
  `watcher_rules.json`). The app registers the shortcut with Win32
  `RegisterHotKey` on its own thread; when it fires, the overlay drops
  click-through, takes focus and emits `summon-command-bar`, so the bar opens
  with the caret already in the input. If another app owns the shortcut the
  registration fails with a warning and nothing else changes.
* Type a prompt or a task and press **`Enter`** — it goes out as a `user_reply`
  frame (`notify_reply`, the page-side twin of `POST /reply`) and the bubble
  immediately switches to a **Thinking…** card with the `thinking` face while
  the model works.
* **Quick-task chips** under the input fire one canned task each. They come from
  `quick_tasks` in `watcher_rules.json` (via `GET /watcher`), so you can edit
  them without rebuilding: summarise the clipboard, ask what window you are in,
  run a diagnostic, or anything you script. A chip may pin a `profile` and/or set
  `include_screen: true` (the **What's on my screen?** chip does), which asks the
  watcher for a screenshot alongside the text.
* **↺** (in the bar) and **New chat** (control panel) call `reset_session`, i.e.
  `POST /session/reset`: the conversation context is dropped and the next task
  starts a fresh session.
* **`Esc`** closes the bar and the desktop goes back to click-through; the bar
  publishes its own hit region while open, so it stays typeable.

## Window controls

* **Minimize** — titlebar middle button (`minimize_window`).
* **Close (X)** — titlebar right button (`close_window`): closes the window,
  stops the cursor tracker and shuts the HTTP server down gracefully.
* Both are also reachable over HTTP (`/shutdown`).

Keyboard: `1`–`4` expressions, `W` walk, `I` idle, `C` centre, `Space` wave,
`Esc` collapse/expand the panel.

## HTTP API

Base URL: `http://127.0.0.1:8765` (bind address is also shown in the control
panel). All request/response bodies are JSON.

### `GET /health`

```json
{ "ok": true, "service": "desktop-buddy avatar api", "version": "0.2.0", "uptime_secs": 42 }
```

### `GET /state`

```json
{
  "position": [700.0, 500.0],
  "target_position": [700.0, 500.0],
  "state": "idle",
  "expression": "neutral",
  "revision": 3,
  "talking": false,
  "perched": true,
  "perch_rect": { "x": 400.0, "y": 300.0, "width": 800.0, "height": 500.0 },
  "look_at": null
}
```

* `state` ∈ `idle | walking | waving`
* `expression` ∈ `neutral | happy | sad | surprised | thinking | angry`
* `position` is the live animated position in CSS pixels (window-relative, and
  the window covers the primary monitor)
* `revision` increments on every externally requested change — clients can poll
  it to detect "something happened"
* `perched` is `true` while the avatar was planted on a window border by
  [`POST /perch`](#post-perch); `perch_rect` is that window (CSS px). The page
  pauses idle wandering and rests its gaze on the rect's centre while perched
* `look_at` is an explicit gaze target `[x, y]` from `POST /move`, or `null`

### `POST /state`

```json
{ "state": "idle", "expression": "happy" }   // expression optional
```

### `POST /move`

```json
{ "x": 1200, "y": 700, "look_at": [1180, 640] }   // look_at optional
```

Sets the walk target (`state` becomes `walking`); the page animates the walk and
reports the live position back. `look_at` optionally names the point the eyes
should rest on (same CSS-pixel space), which is how one call can both move the
avatar and point it at the thing it is about to talk about. A move clears any
perch and replaces the previous `look_at`.

### `POST /perch`

```json
{ "edge": "top_right", "window_process": "code", "offset": 0 }
```

Walks the avatar to a border of a window and leaves it perched there, watching
that window. `edge` is one of `top_left`, `top_right`, `left`, `right`,
`bottom`:

| `edge` | where the avatar ends up |
| --- | --- |
| `top_left` / `top_right` | standing on the **top** border, at the window's left/right corner — its feet on the edge |
| `left` / `right` | standing just **outside** that side, vertically centred |
| `bottom` | hanging under the **bottom** border |

`window_process` is matched case-insensitively as a substring of a
`visible_windows` entry's `process_name` (`"code"` matches `Code.exe`); without
it the foreground window's `window_rect` is used. `offset` slides the avatar
along the chosen edge (x for top/bottom, y for left/right).

The target is computed from the window rect and how far the avatar's artwork
reaches from its root in each direction (`left`/`right`/`up`/`down`, which the
page reports with `report_state` — the root sits near the hips, so half the
height would leave the feet floating), so the pixels touch the border rather
than the bone root. It is then clamped to the **monitor that window is on**, so
a window flush with that screen's edge cannot push the avatar off it, and a
window on a secondary monitor is not dragged back to the primary one.

The perch is **live**: the resolved window's `HWND`, edge and offset are stored,
and a 60 ms poll re-reads its `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`
rect. Drag or resize the window and the avatar walks along its border with it; a
change under 1 px is ignored so a still window produces no traffic. Minimize or
close the window and the perch is dropped (`perched: false`, `perch_rect: null`,
state `idle`). A user drag of the avatar, the `break_perch` command, or any
`POST /move` ends the perch immediately so the tracker never fights the pointer.

```json
{
  "ok": true,
  "edge": "top_right",
  "window_process": "code",
  "window_rect": { "x": 400.0, "y": 300.0, "width": 800.0, "height": 500.0 },
  "target": { "x": 1130.0, "y": 210.0 },
  "state": { "position": [700.0, 500.0], "state": "walking", "perched": true, "…": "…" }
}
```

The `state` frame it broadcasts carries `perched: true` + `perch_rect`, so the
page stops its idle wandering and keeps its gaze on the window. `POST /move`,
`POST /state`, a drag, or `W`/`C` in the overlay all end the perch. A request
with an unknown `edge`, no matching visible process, or no foreground window is
a **400** with `{"ok": false, "error": "…"}` — the message lists the valid edges
or the processes that *are* visible.

### `POST /expression`

```json
{ "expression": "surprised" }
```

### `POST /wave`

No body. Sets state to `waving`; the page returns to `idle` when the animation ends.

### Models & rigs

See [MODEL_FORMAT.md](MODEL_FORMAT.md) for the manifest schema.

#### `GET /models`

```json
{ "ok": true, "root": "C:\\hermes\\desktop-buddy\\models", "models": [
  { "name": "demo-blob", "label": "Demo Blob (sprite parts)", "path": "...",
    "bones": 7, "parts": 10, "has_rig_override": false, "generated": false,
    "error": null }
] }
```

Broken folders appear with `"error": "<reason>"` instead of failing the request.

The response is served from `models/.model-index.json`, a cache keyed by a
fingerprint of each folder's `model.json`/`rig.json` (size + mtime):

* **warm** — a directory walk plus one `stat` per folder: ~24 ms for 249 models;
* **cold** — every manifest parsed, none of their sprites read: ~280 ms (one
  archive-wide rebuild after the cache is deleted or invalidated);
* **what it replaced** — `load_model` for every folder on every call, sprites
  included: ~250 ms *per call*, which the startup and every `rig-saved` event
  paid.

Only the folder that changed is re-read, so saving a rig or installing a
generated model costs one parse, not the whole archive. The cache is a plain
file (gitignored) and is rebuilt silently if it is missing or corrupt.
`cd src-tauri && cargo run --example model_list_timing` prints those three
numbers against the current `models/`.

`"generated": true` marks a folder whose name is a metamorphosis slug
(`buddy-<archetype>-<stage>[-<attribute>]-<8 hex>`) — the procedural creatures the
plugin writes on every stage change. The endpoint lists them like any other
model (the bridge and the MCP tools still see everything); the desktop model
picker hides them unless they carry a `rig.json` override (i.e. you edited and
saved one) or are the model currently loaded.

#### `GET /model`

```json
{ "ok": true, "name": "demo-blob", "label": "Demo Blob", "dir": "...",
  "rig_path": ".../model.json", "is_override": false }
```

#### `POST /model`

```json
{ "name": "demo-blob" }
```

Switches the live model (the selection is persisted in `models/.state.json`).

#### `POST /import`

Imports a **rig manifest** (`.json`, `.toml`) or a **single image**
(`.png`, `.jpg`/`.jpeg`, `.webp`, `.svg`).

```json
{ "path": "D:/art/mychar/model.json" }                               // manifest -> new model
{ "path": "D:/art/hero.png", "mode": "model", "name": "hero" }       // image -> new character
{ "path": "D:/art/hat.png", "mode": "part", "tag": "accessory" }     // image -> part of current rig
```

* manifest imports copy the manifest plus **only the sprites the rig references**
  (`part.image` and every `part.variants` entry, relative paths preserved) into
  `models/<slug>/` and load it — unrelated files next to the source manifest are
  never copied, so a manifest sitting in `Downloads/` stays a one-folder import
  (`name` overrides the folder name; collisions get `-2`, `-3`, …)
* `mode: "model"` builds a one-bone model whose body is that image
* `mode: "part"` (default for images) copies the sprite into the current
  model's `parts/` folder and returns it **staged**, not attached:

```json
{ "kind": "part", "model": "demo-blob",
  "staged": { "image": "parts/hat.png", "name": "hat.png", "size": [64, 64],
              "data_url": "data:image/png;base64,..." } }
```

The editor attaches a staged image to the selected bone (the response carries
the data URL so the page can render it immediately).

Unknown extensions are rejected with a 400 naming the accepted formats.

#### `GET /parts`

Every part of the loaded rig with runtime patches merged in:

```json
{ "ok": true, "parts": [
  { "bone": "head", "id": "mouth", "tag": "mouth", "image": "parts/mouth_open.png",
    "state": "open", "variants": { "closed": "...", "open": "...", "smile": "..." }, "z": 5 }
] }
```

#### `POST /part`

Patch parts selected by semantic tag or by id. Applies immediately (the page
listens for the `part-patch` event) and is reflected by `GET /parts`:

```json
{ "target": { "tag": "mouth" }, "patch": { "offset": [0, 26], "scale": 1.2 } }
{ "target": { "part": "mouth" }, "patch": { "state": "smile" } }
```

Patch keys: `state` (switches the sprite to that variant), `talk` (animate),
`offset`, `pivot`, `rotation`, `scale`, `z`, `visible`, `color`, `image`.

#### `POST /mouth`

```json
{ "state": "open" }     // closed | open | smile | frown  -> swap the mouth sprite
{ "state": "talk" }     // start the talking animation  ("idle" stops it)
```

Equivalent to `POST /part` with `{"target": {"tag": "mouth"}}`, for the common
"make the avatar speak" case.

#### `GET /rig`

Full rig document plus runtime data:

```json
{ "ok": true, "name": "demo-blob", "rig_path": "...", "is_override": false,
  "rig": { "bones": [ ... ] },
  "pose": { "arm_l": { "rotation": -1.2 } },
  "angles": { "root": 0.0, "arm_l": -1.2 } }
```

`angles` are the live local joint rotations (bind + animation + pose), reported
by the page at ~8 Hz.

#### `POST /rig`

Saves a rig document to `models/<name>/rig.json` (atomic write, validated).
Accepts either a bare rig object or `{ "rig": { ... } }`.

```bash
curl -s localhost:8765/rig | python -c "import json,sys; print(json.load(sys.stdin)['rig'])" > /tmp/rig.json
curl -s -X POST localhost:8765/rig -H 'content-type: application/json' --data @/tmp/rig.json
```

#### `POST /rig/reset`

Deletes `rig.json` and reloads `model.json`.

### Posing individual joints

#### `GET /joints`

```json
{ "ok": true, "model": "demo-blob",
  "rig_path": "...",
  "angles": { "arm_l": 0.37, "foot_l": 0.15 },
  "pose":   { "arm_l": { "rotation": 0.5 } } }
```

#### `POST /pose`

Applies per-bone overrides on top of the animation. Supported keys: `rotation`
(radians, absolute — replaces bind + animation for that bone), `scale`,
`x`/`y` (px offset of the joint). `null` clears one bone.

```bash
curl -s -X POST localhost:8765/pose -H 'content-type: application/json' \
  -d '{"joints": {"arm_l": {"rotation": -1.2}, "head": {"x": 0, "y": -10}}}'
```

#### `DELETE /pose`

Clears every override.

### Custom keyframe animations

Clips live on the rig root (`"animations"` in `model.json` / `rig.json`, see
[MODEL_FORMAT.md](MODEL_FORMAT.md)) and are authored in the timeline bar of Rig
Edit Mode. The page owns playback — these endpoints only tell it what to play.

#### `GET /animations`

Every clip on the loaded rig, sorted by name:

```json
{ "ok": true, "model": "demo-blob", "rig_path": ".../model.json",
  "animations": [
    { "name": "breathe", "duration_ms": 2400.0, "loop": true, "keyframes": 3 },
    { "name": "celebrate", "duration_ms": 1400.0, "loop": false, "keyframes": 5 },
    { "name": "wave", "duration_ms": 1400.0, "loop": false, "keyframes": 6 }
  ] }
```

`duration_ms` is always `> 0`, `keyframes` is the authored keyframe count, and a
model with no `animations` block returns `"animations": []`. The shipped rigs
carry a **reaction library** — `nod`, `shake_head`, `celebrate`, `alert_ping`,
`idle_fidget` — alongside `wave`/`breathe`, so an `openai_compat` model can ask
for `{"animate": "nod"}` with no extra setup.

#### `POST /animate`

```json
{ "name": "wave" }                 // play the authored loop flag
{ "name": "wave", "loop": true }   // override the loop flag for this run
{ "name": null }                   // stop whatever is running
```

`loop` is optional and only overrides this run — the clip's stored flag is not
modified. An unknown `name` is a **400** naming the clips that do exist:

```json
{ "ok": false, "error": "unknown animation \"jump\"; model \"demo-blob\" has [\"breathe\", \"wave\"]" }
```

The response echoes what the page was told to do:

```json
{ "ok": true, "animation": { "action": "play", "name": "wave", "duration_ms": 1400.0,
                             "loop": false, "keyframes": 6 } }
```

A non-looping clip reports itself finished when it reaches its last keyframe: the
avatar returns to `idle`, and the page's next `state` report carries that
transition onto the event stream.

#### `POST /animate/stop`

Body-less stop, equivalent to `POST /animate {"name": null}`:

```json
{ "ok": true, "animation": { "action": "stop", "name": null } }
```

Both spellings emit the `avatar-animate` Tauri event to the page and an
`animate` frame on `GET /events`.

### Expressions & speech

#### `POST /emote`

```json
{ "expression": "happy", "duration_ms": 3000, "revert": "neutral" }
```

Expressions: `neutral`, `happy`, `surprised`, `thinking`, `sad`, `angry`.
With `duration_ms` the avatar reverts to `revert` (default `neutral`) afterwards;
omit it to hold the expression until changed.

#### `POST /speak`

```json
{ "text": "Hello!", "expression": "happy", "duration_ms": 4000, "char_ms": 26 }
```

Shows the speech bubble next to the head (typewriter reveal), sets the
expression and animates the mouth. `duration_ms` is how long the finished text
stays on screen (default 6000, min 500); `char_ms` is the per-character typing
speed (default 26). Text is limited to 2000 characters.

`details` (optional, ≤ 8000 characters) is the long half of a bubble — a code
snippet, a diff, a log tail. It renders behind a **Details ▾** toggle inside the
bubble with a **Copy** button, pauses the auto-dismiss timer while expanded, and
is echoed back as `details_chars`. Prefer a one-line `text` plus `details` over
a paragraph the bubble cannot fit:

```bash
curl -s -X POST localhost:8765/speak -H 'content-type: application/json' \
     -d '{"text":"Build failed: 2 errors","details":"src/lib.rs:41: unused import\nsrc/lib.rs:88: mismatched types"}'
```
```json
{"ok":true,"text":"Build failed: 2 errors","expression":"neutral","duration_ms":6000,"char_ms":26,
 "details":"src/lib.rs:41: unused import\nsrc/lib.rs:88: mismatched types","details_chars":62}
```

#### `POST /clear_speech`

Immediately hides the bubble and stops the mouth animation.

#### `GET /speech`

```json
{ "ok": true, "talking": false, "speech": null }
```

`speech` holds the active payload while the bubble is on screen, then returns to
`null` once it is dismissed.

`GET /state` also carries `talking`, so a client can watch the mouth state
without polling `/speech`.

#### `GET /history`

```json
{
  "ok": true,
  "messages": [
    { "id": 1, "role": "user", "text": "what does the history drawer do?", "ts_ms": 1794621725236 },
    { "id": 2, "role": "assistant", "text": "It keeps the last 50 turns in memory.",
      "details": "fn push_chat() { /* bounded VecDeque */ }",
      "expression": "happy", "ts_ms": 1794621725766 }
  ]
}
```

The session's conversation turns, **oldest first** — the same in-memory log the
page's **History** drawer renders (and the same body the `get_chat_history`
command returns). A `"user"` turn is recorded by every `POST /reply` (and the
command bar / push-to-talk, which share that path); an `"assistant"` turn by
every `POST /speak` (and by every agent reply, which funnels through
`do_speak`), carrying the same `details` and `expression` the bubble had.
`POST /session/reset` empties the list.

The log is bounded to **50 turns** (the oldest is evicted), holds no ids beyond
the per-process counter, is never written to disk, and starts empty on every
launch. `details` and `expression` are omitted when absent.

### `POST /click-through`

```json
{ "enabled": true }
```

### `POST /hit-regions`

```json
{ "regions": [ { "x": 0, "y": 0, "w": 100, "h": 40 } ] }
```

Used by the page; exposed for tooling/debugging.

### `GET /watcher` / `POST /watcher`

The supervised `desktop-buddy-watcher.exe` — the same switch the control panel's
**Watcher** toggle drives.

```bash
curl -s localhost:8765/watcher
```

```json
{
  "ok": true,
  "enabled": true,
  "running": true,
  "pid": 14320,
  "binary_path": "C:\dev\desktop-buddy\src-tauri\target\debug\desktop-buddy-watcher.exe",
  "rules_path": "C:\dev\desktop-buddy\watcher_rules.json",
  "logs": ["[22:23:29] started … (pid 14320, rules …watcher_rules.json)", "…"],
  "quick_tasks": [
    {"label": "Summarize clipboard", "prompt": "Call avatar_get_clipboard and summarise what I copied …"},
    {"label": "What am I looking at?", "prompt": "Call avatar_get_context, then tell me …"}
  ]
}
```

`logs` is a **bounded ring buffer** of the child's stdout+stderr: timestamped
`[HH:MM:SS]` lines, newest last, capped at **200**.

`quick_tasks` is the command bar's chip list, read straight out of
`watcher_rules.json` (`[{"label": "...", "prompt": "..."}]`, capped at 6). Edit
the file, reopen the bar and the chips change — no rebuild. With no `quick_tasks`
array (or an unreadable file) the app falls back to four built-in chips
(clipboard summary, active window, quick diagnostic).

```bash
curl -s -X POST localhost:8765/watcher -H 'content-type: application/json'      -d '{"enabled": false}'      # kill + reap; the supervisor parks
curl -s -X POST localhost:8765/watcher -H 'content-type: application/json'      -d '{"enabled": true}'       # spawn it again, no app restart
```

Both return the same status object as `GET /watcher`. Every start, stop and log
line also goes out as a `watcher` frame on `GET /events` and as the
`watcher-status` Tauri event, so the page never has to poll.

Dispatch is **single-flight with a one-slot pending queue**. A `user_reply` or an
`alert` that arrives while a `--hermes-cmd` turn is still running is not dropped:
it waits in a one-trigger slot (a newer one replaces it, so only the latest
matters — the watcher logs `[watcher] queued pending trigger (1 slot)`) and runs
as soon as that turn's child exits or is killed by the timeout. Routine triggers
(`interaction`, `window_title`, `idle_return`) are dropped while busy, so
background noise never stacks up behind a task.

### `GET /tts` / `POST /tts`

The local **Piper** voice — the same switch the control panel's **Voice** toggle drives.

```bash
curl -s localhost:8765/tts
```

```json
{
  "ok": true,
  "enabled": true,
  "ready": true,
  "reason": null,
  "piper_exe_path": "C:\\hermes\\tools\\piper\\piper.exe",
  "model_path": "C:\\hermes\\tools\\piper\\voices\\en_GB-southern_english_female-low.onnx",
  "espeak_data_path": "C:\\hermes\\tools\\piper\\espeak-ng-data",
  "speaker_id": null,
  "length_scale": 1.0
}`
```

Every speech bubble is read out loud (`lfm3-fast`, `hermes_cli` and any other
caller — it hooks the one `do_speak` path). `ready: false` means `piper.exe`, the
`.onnx` voice model or the espeak-ng data directory was missing at startup;
`reason` names the missing piece and the avatar keeps working silently.

```bash
curl -s -X POST localhost:8765/tts -H 'content-type: application/json' -d '{"enabled": false}'  # mute
curl -s -X POST localhost:8765/tts -H 'content-type: application/json' -d '{"enabled": true}'   # unmute
```

Both return the same object. Muting also stops the line that is playing. The
mute lives in memory only — `watcher_rules.json` is never rewritten, so a
restart returns to the file's `tts.enabled`. The panel drives the same code
through the `get_tts_status` / `set_tts_enabled` commands and gets `tts-status`
events.

### `GET /stt` / `POST /stt`

Push-to-talk voice input. `GET` returns the `stt` settings plus the worker's live
state:

```json
{
  "ok": true,
  "enabled": true,
  "ready": true,
  "state": "idle",
  "reason": null,
  "hotkey": "Alt+V",
  "whisper_exe_path": "C:\\hermes\\tools\\whisper.cpp\\build\\bin\\Release\\whisper-cli.exe",
  "whisper_model_path": "C:\\hermes\\tools\\whisper.cpp\\ggml-base.en.bin",
  "silero_vad_model_path": "C:\\hermes\\tools\\whisper.cpp\\ggml-silero-v6.2.0.bin",
  "vad_threshold": 0.5,
  "language": "en",
  "min_speech_ms": 250,
  "last_transcript": "",
  "last_error": null,
  "sample_rate": 16000
}
```

`state` is `idle`, `listening` (the microphone is open) or `transcribing`
(Whisper is running). `ready: false` means `whisper-cli.exe`, a model or the
microphone was missing at startup; `reason` names the missing piece and the app
keeps working without voice input. `sample_rate` is the rate the device actually
opened at (the clip is resampled to 16 kHz before Whisper sees it).

```bash
curl -s localhost:8765/stt                                                        # status
curl -s -X POST localhost:8765/stt -H 'content-type: application/json' -d '{"enabled": false}'  # mute
```

`POST` takes `{"enabled": bool}` — muting a hold stops it — and returns the same
object. Runtime only: a restart restores `stt.enabled`. Recording itself is
driven by the **Hold to talk** button or the `stt.hotkey` hold; the API can read
and mute but cannot press the button. The transcript is posted to `POST /reply`
as if it had been typed.

### `GET /ai-profiles` / `POST /ai-profile`

Which **brain** answers a watcher trigger: the `ai_profiles` block in
`watcher_rules.json` (an `openai_compat` server such as `llama.cpp` / Ollama, or
the `hermes_cli` agent), plus the live health of the active one.

```bash
curl -s localhost:8765/ai-profiles
```

```json
{
  "ok": true,
  "active_profile": "hermes",
  "active_health": {
    "reachable": true,
    "status": "cli",
    "detail": "hermes found at C:\\Users\\me\\AppData\\Local\\hermes\\hermes.exe"
  },
  "profiles": [
    {"name": "hermes", "label": "Hermes CLI", "kind": "hermes_cli", "model": ""},
    {"name": "lfm3-fast", "label": "LFM3 1.7B (llama.cpp)", "kind": "openai_compat",
     "model": "lfm3-1.7b", "endpoint": "http://127.0.0.1:8080"}
  ]
}
```

`active_health.status` is one of:

| status | meaning |
| --- | --- |
| `online` | an `openai_compat` profile answered a probe (its own /health, then /v1/models, 800 ms cap) |
| `offline` | nothing answered — `detail` names the endpoint and the error |
| `cli` | a `hermes_cli` profile; `reachable` says whether `hermes` resolves on `PATH` |

Switch it:

```bash
curl -s -X POST localhost:8765/ai-profile \
     -H 'content-type: application/json' -d '{"name": "lfm3-fast"}'
```

The switch is persisted to `watcher_rules.json` (`active_profile`, written to a
sibling file and renamed, so the file is never half-written), the new profile is
probed, and the app broadcasts an `ai_profile` frame plus the
`ai-profile-changed` Tauri event. The running `desktop-buddy-watcher.exe` re-reads the
file when it sees the frame, so the **next** trigger uses the new backend — no
restart. An unknown name is a **400** that lists the valid profiles.

Per-trigger overrides: `app_rules[].profile` (a window rule) and
`quick_tasks[].profile` (a command-bar chip, forwarded in the `user_reply`
frame's `profile`). Both fall back to `active_profile` when absent.

### `POST /ai-discover`

Probe configured targets for model servers and list what they serve — the
backend of the AI brain tab's **Discover models** button.

```bash
curl -s -X POST localhost:8765/ai-discover \
     -H 'content-type: application/json' \
     -d '{"hosts": ["127.0.0.1", "192.168.0.36:11434"], "ports": [8001, 9931, 11434]}'
```

The body is optional: with no `hosts` / `ports` arrays the `discovery_hosts`
and `discovery_ports` lists from `watcher_rules.json` are used, so the button
works before a save. A `host:port` entry is probed at that port only; every
other host is crossed with the port list.

```json
{
  "ok": true,
  "summary": "1 server(s) answered on 3 target(s) · 2 served model(s) · 1 local .gguf",
  "elapsed_ms": 41,
  "timeout_ms": 1500,
  "targets": [
    {"host": "127.0.0.1", "port": 8001, "label": ":8001", "loopback": true,
     "reachable": true, "detail": "/v1/models listed model(s)"},
    {"host": "192.168.0.36", "port": 11434, "label": "192.168.0.36:11434", "loopback": false,
     "reachable": false, "detail": "nothing answered (no /v1/models, /models or /api/tags)"}
  ],
  "models": [
    {"id": "qwen3-coder-30b-a3b-colibri", "label": "qwen3-coder-30b-a3b-colibri",
     "endpoint": "http://127.0.0.1:8001/v1", "host": "127.0.0.1", "port": 8001,
     "loopback": true, "source": "openai_compat", "owned_by": "colibri",
     "multimodal": false},
    {"id": "llama3.2:3b", "label": "llama3.2:3b", "endpoint": "http://192.168.0.36:11434/v1",
     "host": "192.168.0.36", "port": 11434, "loopback": false, "source": "ollama",
     "owned_by": "3.2B", "multimodal": false},
    {"id": "LFM3-1.7B-Q8_0", "label": "LFM3-1.7B-Q8_0", "endpoint": "", "port": null,
     "source": "gguf", "owned_by": "", "multimodal": false,
     "path": "C:\\Users\\me\\AppData\\Local\\hermes\\models\\LFM3-1.7B-Q8_0.gguf",
     "size_bytes": 1832140800}
  ],
  "gguf_dirs": ["C:\\Users\\me\\AppData\\Local\\hermes\\models"]
}
```

Each target is asked, in order, for an OpenAI `v1/models` listing, llama.cpp's
root-mounted `models` listing and Ollama's `api/tags`; `source` says which one
answered and `endpoint` is the URL a profile should store (`.../v1`, the prefix
`chat/completions` hangs off). `multimodal: true` means the server itself
reported an image input modality. **`host`** names the machine each model came
from, so a LAN / Tailscale / second-interface hit is distinguishable from
loopback; `label` is the display form (`:8001` for loopback,
`192.168.0.36:11434` otherwise) and `loopback` is the boolean the modal styles
on. Every target is probed **in parallel** with a **1.5 s** budget each
(`DISCOVERY_TIMEOUT_MS`) on a blocking thread, so ten extra targets cost one
budget rather than ten and an offline host costs a spinner tick rather than a
stalled API worker. `.gguf` weights under `%LOCALAPPDATA%\hermes\models` are
listed too (`source: "gguf"`, no endpoint), so an unloaded model is still
visible. `targets` and `ports` carry the same per-target array (the latter kept
for older callers).

Nothing is written: **importing** a model is the modal filling a draft profile
card, and `POST /config` persists it.

### `GET /config` / `POST /config`

The Settings modal's backend, and the reason nobody has to hand-edit
`watcher_rules.json` to move a Piper install.

`GET` returns the **effective** settings — the values the runtime would actually
load, defaults included — plus `paths` (one entry per executable/model the app
spawns, each with `exists`) and `warnings` (keys present in the file that the
loader ignores: a block full of those *looks* configured while the app runs on
defaults, so it is reported instead of hidden).

```bash
curl -s localhost:8765/config | jq '.settings.tts, .paths, .warnings'
```

`POST` takes a **partial** patch and writes it back:

```bash
curl -s -X POST localhost:8765/config -H 'content-type: application/json' -d '{
  "tts":      {"piper_exe_path": "C:\\\\tools\\\\piper\\\\piper.exe"},
  "ai":       {"active_profile": "hermes",
               "profiles": [{"name": "hermes", "timeout_secs": 180}]},
  "survival": {"metabolism": {"hunger_per_min": 0.5}, "notify": false}
}'
```

Only the blocks you send are rewritten, and inside each one only the top-level
value span is replaced (`src-tauri/src/settings.rs`), so `app_rules`,
`quick_tasks`, every `system_prompt` and any key this editor does not own survive
byte-for-byte. Nested objects merge, so a metabolic-rate patch keeps the sibling
rate. The answer is the same body as `GET` plus `applied` (dotted paths) and
`restart_required`.

* **Applied live**: `tts.enabled` / `stt.enabled` (the mute switches), every
  `survival.*` value (the metabolism plugin reads the new config on its next
  tick, so a new drain rate bites within a second), and every `ai.*` or
  `vision.*` change (one `ai_profile` frame is what makes
  `desktop-buddy-watcher.exe` re-read the file — new endpoints, models, capture mode,
  no restart).
* **Needs a restart**: Piper/Whisper **paths** and `stt.hotkey`. Those are read
  once at startup, so they are listed under `restart_required` rather than
  pretended to be live.
* **400s**: unknown key, wrong type, out-of-range number, unknown profile name,
  empty required path, a removal that would leave no profile or strand the
  active/vision selection — the message names the field. Nothing is written on
  failure, and the previous content is kept at `watcher_rules.json.bak`.

#### `ai.*` — add, edit, rename and remove profiles

`POST /config` speaks four verbs for `ai_profiles`, all validated against the
map that **results** from the patch — so one save can switch to a profile it
creates:

```json
{
  "ai": {
    "active_profile": "lfm3-fast",
    "create": [{"name": "tiny", "label": "Tiny 1B", "kind": "openai_compat",
                "endpoint": "http://127.0.0.1:8080/v1", "model": "tiny-1b",
                "timeout_secs": 120, "system_prompt": "…"}],
    "profiles": [{"name": "lfm3-fast", "max_tokens": 8192}],
    "rename": [{"from": "tiny", "to": "tiny-fast"}],
    "remove": ["colibri"]
  },
  "vision": {"profile_name": "lfm3-vl-2b"},
  "discovery_hosts": ["127.0.0.1", "192.168.0.36:11434"],
  "discovery_ports": [8001, 9931, 9932, 11434, 8080, 1234]
}
```

* **`create`** — a whole entry. `name` is the key: a lowercase slug
  (`[a-z0-9][a-z0-9_-]*`, at most 64 chars) that must not already exist.
  `kind` defaults to `openai_compat`, which needs `endpoint` (http/https) and
  `model`; `hermes_cli` needs `cmd`. `timeout_secs` defaults to **120**, and
  when `system_prompt` is absent the entry is created with the JSON-avatar
  contract — the same string `GET /config` returns as
  `settings.ai.default_system_prompt`, so an imported local model answers with
  `{"speech": …}` on its first trigger instead of chatting in prose.
* **`profiles`** — per-entry edits, merged into the stored object, so `api_key`
  and any key the editor does not show survive. `label`, `kind`, `endpoint`,
  `model`, `cmd`, `max_tokens`, `timeout_secs` and `system_prompt` are the
  editable fields.
* **`rename`** — `{from, to}`; `active_profile` and `vision.profile_name` follow
  the rename, so neither reference is left dangling.
* **`remove`** — profile keys. Refused with a **400** (and nothing written) when
  it is the **last** profile, the **active** profile ("answers desktop
  triggers") or the **vision** profile; the message says to switch that
  selection first. The modal refuses the same three cases inline, before sending
  anything.
* **`discovery_ports`** — the top-level port list **Discover models** probes. The
  key is inserted after the opening brace when the file predates it; the array is
  deduplicated and every entry must be a port (1–65535).
* **`discovery_hosts`** — the top-level host list those ports are probed on,
  defaulting to `["127.0.0.1"]` when omitted or empty. Entries are trimmed, lose
  an accidental `http://` / `https://` prefix and any trailing path or slash, and
  are deduplicated case-insensitively. A `host:port` entry (`192.168.0.36:11434`)
  is probed at that port **only** instead of being crossed with every entry in
  `discovery_ports`. Anything that is not a host, IP or `host:port` is a 400 and
  nothing is written.
* **`vision.profile_name`** must name an existing `ai_profiles` entry — a name
  that does not resolve is a 400 rather than a screenshot sent to nothing.


### `GET /soul` / `POST /soul`

`SOUL.md` is the avatar's durable persona — a plain markdown file the user owns.
`desktop-buddy-watcher` reads it at the **start of every turn**, so nothing is cached
and an edit is live on the next reply. `GET` reports the file *and* the persona
that will actually be used:

```bash
curl -s localhost:8765/soul
```

```json
{
  "ok": true,
  "path": "C:\\hermes\\desktop-buddy\\SOUL.md",
  "exists": true,
  "is_default": false,
  "content": "# SOUL.md\n\n…the file exactly as it is on disk…",
  "persona": "# SOUL.md\n\n…the persona the next turn will inject…",
  "max_bytes": 65536
}
```

`content` is the raw file; `persona` is what the next turn will actually inject:
the built-in `DEFAULT_SOUL` starter whenever the file is missing, empty,
unreadable or over the cap — which is exactly what `is_default: true` reports,
with `exists: false` and `content: ""` once the file is gone. Nothing here is a
404: the app seeds `SOUL.md` at startup and **Open SOUL.md** re-seeds it on
demand, but a missing file is a perfectly valid state — the avatar just falls
back to the built-in persona.

```bash
curl -s -X POST localhost:8765/soul -H 'content-type: application/json' \
     -d '{"content": "# SOUL.md\n\nDry, brief, a little odd.\n"}'
```

The body is `{"content": "…"}` and the reply is the same object `GET` returns, so
a client repaints straight from the write. Refusals are the writer's own words
and nothing is written: a missing `content`, a blank one, or anything over
`max_bytes` is a **400**. The write is a temp file plus a rename, so a
half-written persona can never land.

**One resolver, both processes.** `src-tauri/src/soul.rs` is compiled into the
app *and* into `desktop-buddy-watcher` (`#[path = "../soul.rs"]`), so the editor and
the prompt cannot disagree: `DESKTOP_BUDDY_SOUL` → `soul_path` in
`watcher_rules.json` → `<rules dir>/SOUL.md` → `<app config dir>/SOUL.md`.

The Settings modal's **Open SOUL.md** button is the `open_soul_file` command
rather than a second route: it seeds a missing file and hands the path to the
OS's default editor, so the persona can be edited in the user's own editor while
the textarea stays open. `SOUL.md` stays authoritative over the profile's
`expression` hints — the persona wins on voice, the contract wins on shape.

### `GET /survival` / `POST /survival`

The avatar's appetite — hunger and saturation on a 0–100 scale, the metabolic
drain rate and the current hunger tier. Every byte of this lives in the survival
plugin (see `GET /plugins`): its own state, its own tick, its own routes and its
own `survival` block in `watcher_rules.json` (omitting the block uses the
built-in defaults). `enabled: false` keeps the plugin installed but idle — the
bars freeze and the routes keep answering with `"enabled": false` rather than
404-ing.

```bash
curl -s localhost:8765/survival
```

```json
{
  "ok": true,
  "config": {
    "enabled": true,
    "start_hunger": 25.0,
    "start_saturation": 40.0,
    "notify": true,
    "telemetry_secs": 60,
    "metabolism": {"hunger_per_min": 0.5, "saturation_per_min": 0.9}
  },
  "stats": {
    "hunger": 25.0,
    "saturation": 40.0,
    "tier": "peckish",
    "label": "Peckish",
    "expression": "thinking",
    "metabolism": {
      "hunger_per_min": 0.5,
      "saturation_per_min": 0.9,
      "minutes_to_empty": 50.0
    },
    "simulated_secs": 0,
    "meals": 0,
    "last_meal": null,
    "seconds_to_next_tier": 900,
    "seconds_to_starving": 1800,
    "coins": 100
  },
  "hunger": 25.0,
  "saturation": 40.0,
  "tier": "peckish",
  "label": "Peckish",
  "expression": "thinking",
  "coins": 100,
  "persistence": {
    "path": "plugins/survival.state.json",
    "restored": true,
    "dirty": false,
    "saved_ms": 1763000123456,
    "saves": 12
  }
}
```

`tier` is one of `satiated`, `peckish`, `hungry`, `starving`; `label` is the
human wording and `expression` the face the avatar wears in that tier. The
plugin list lives at `GET /plugins` only: the route is answered by a WebAssembly
guest, which cannot read the host's registry from inside a call.
`seconds_to_next_tier` counts down to the next *worse* tier (or `null` when
already starving) and `seconds_to_starving` to the `hungry` floor. `coins` is the
wallet the shop spends and fills — it lives on `stats` and is flattened to the
top level here; see `GET /store`.

`persistence` is how a restart is proven from a shell: `path` is the file the
host writes (`plugins/survival.state.json`, beside the `.wasm`), `restored` says
whether this session began from it, `saved_ms` is the epoch ms of the last
successful save (0 = never) and `saves` counts them. `dirty` is true in the
instant between a change and its write. Everything the avatar owns — the bars,
`meals`, the craving, **the coins and the whole bag** — is in that file, so a
purchase or a cooked meal survives a restart; `ARCHITECTURE.md` documents the
write cadence and the schema.

Override the bars by hand — used by tests, the UI's debug drawer and automation
that wants to force a state:

```bash
curl -s -X POST localhost:8765/survival \
     -H 'content-type: application/json' -d '{"hunger": 12, "saturation": 5}'
```

Both fields are optional (send at least one; neither ⇒ **400**). Values are
clamped to 0–100. A tier change caused here emits the same `survival-threshold`
frame a natural crossing would.

The same endpoint drives the avatar's **craving**, so the dialogue can be
demonstrated without waiting for the interval:

```bash
curl -s -X POST localhost:8765/survival -H 'content-type: application/json' -d '{"craving": "omelette"}'
curl -s -X POST localhost:8765/survival -H 'content-type: application/json' -d '{"craving": {"group": "sweet"}}'
curl -s -X POST localhost:8765/survival -H 'content-type: application/json' -d '{"craving": null}'
```

A food id resolves first, then a food group (`fruit`, `vegetable`, `dairy`,
`protein`, `grain`, `sweet`, `drink`; plurals accepted). Anything else is a
**400**. The response is the full `GET /survival` body, whose `craving` object
is `null` when there is nothing pending:

```json
{
  "kind": "group",
  "target": "dairy",
  "label": "something dairy",
  "started_ms": 1763000000000,
  "duration_secs": 600,
  "ends_ms": 1763000600000,
  "remaining_secs": 600,
  "expired": false
}
```

### `POST /survival/feed`

Eat one item from the catalog (`GET /foods`):

```bash
curl -s -X POST localhost:8765/survival/feed \
     -H 'content-type: application/json' -d '{"food": "apple"}'
```

`id` is accepted as an alias for `food`, so an agent can round-trip the
catalog's `id` field.

A food that can sit in the bag (`apple`, `tomato`, a caught fish — every
`GET /inventory` entry with a `food`) is **spent from the bag**: feeding it is
the same transaction as `POST /inventory/use`, so it costs one of what you hold,
emits `inventory-changed` with `source: "feed"`, and the response gains
`item` / `item_label` / `remaining` / `change`. With none left the answer is a
**409** (`"no Apple in the bag — buy one on the Store tab or catch something
first"`) and neither the bars nor the bag move; a refused meal (`too_full`,
`still_digesting`) consumes nothing either. The shelf meals that have no bag
entry (`cake`, `coffee`, `sandwich`, the crafted dishes) are still eaten as-is.

The response is the `EatOutcome`:

```json
{
  "ok": true,
  "accepted": true,
  "food": "apple",
  "label": "Apple",
  "hunger_before": 12.0,
  "hunger_after": 22.0,
  "saturation_before": 5.0,
  "saturation_after": 14.0,
  "tier_before": "starving",
  "tier_after": "hungry",
  "effects": [],
  "refusal": null,
  "cooldown_remaining_secs": 0,
  "item": "apple",
  "item_label": "Apple",
  "remaining": 1,
  "change": {"item": "apple", "delta": -1, "count": 1, "source": "feed", "…": "…"}
}
```

A refused meal returns **409** with `accepted: false` and `refusal` set to
`too_full` (already satiated) or `still_digesting` (the meal cooldown has not
elapsed; `cooldown_remaining_secs` says how long). An unknown id is a **400**
that lists the valid ids. An accepted meal emits `survival-meal` and, when the
tier changes, `survival-threshold`. Both carry `line` and `expression` — the
sentence the plugin would have said and the face it was written for — so the
watcher can speak it when no model answers.

### `POST /survival/craft`

Cook a recipe. Either name it:

```bash
curl -s -X POST localhost:8765/survival/craft \
     -H 'content-type: application/json' -d '{"recipe": "omelette"}'
```

...or hand over the ingredients and let the recipe book name the dish. Order
does not matter, **counts do**:

```bash
curl -s -X POST localhost:8765/survival/craft \
     -H 'content-type: application/json' -d '{"ingredients": ["egg", "cheese", "egg"]}'
curl -s -X POST localhost:8765/survival/craft \
     -H 'content-type: application/json' -d '{"ingredients": {"noodles": 2, "egg": 1, "mushroom": 1}}'
```

Cooking **draws from the bag**. Every ingredient has to be in the inventory
first — buy it at `GET /store`, or catch/forage it — and it is taken out of the
bag only once the dish has actually been eaten. The response is the usual
`EatOutcome` plus the crafting fields:

```json
{
  "ok": true,
  "accepted": true,
  "food": "omelette",
  "label": "Cheese omelette",
  "kind": "cooked",
  "recipe": "omelette",
  "recipe_label": "Cheese omelette",
  "blurb": "Two eggs and a fistful of cheese. Fast, hot, filling.",
  "cooked_with": [{"food": "egg", "count": 2}, {"food": "cheese", "count": 1}],
  "consumed": [
    {"item": "egg", "label": "Egg", "icon": "🥚", "count": 2, "remaining": 0},
    {"item": "cheese", "label": "Cheese", "icon": "🧀", "count": 1, "remaining": 0}
  ],
  "comfort_secs": 240,
  "hunger_before": 40.0,
  "hunger_after": 66.0,
  "saturation_before": 0.0,
  "saturation_after": 22.1,
  "tier_before": "hungry",
  "tier_after": "peckish",
  "craving_satisfied": null
}
```

An ingredient list that matches no recipe is a **400** that echoes the ids it
was given. A recipe whose ingredients the bag does not hold is a **409**
`missing_ingredients` with a `missing` array of `{item, label, need, held,
short}`, and it costs nothing:

```json
{
  "ok": false,
  "error": "cannot cook Cheese omelette — still need 2 × Egg, 1 × Cheese",
  "refusal": "missing_ingredients",
  "recipe": "omelette",
  "recipe_label": "Cheese omelette",
  "missing": [
    {"item": "egg", "label": "Egg", "need": 2, "held": 0, "short": 2},
    {"item": "cheese", "label": "Cheese", "need": 1, "held": 0, "short": 1}
  ]
}
```

A full or still-digesting avatar is a **409** with the usual `refusal`, and that
refusal consumes nothing either: the ingredients leave the bag only after the
meal was accepted. A cook emits `survival-meal` and `survival-craft` (and
`survival-craving` when the dish settles a craving), plus one `inventory-changed`
per ingredient with `source: "cook"`, so a bag view stays current.
`comfort_secs` is the well-fed window: hunger drains at half rate for that long.

### `GET /foods`

The static food catalog — the ids `POST /survival/feed` accepts, with what each
one restores and any cosmetic side effect:

```bash
curl -s localhost:8765/foods
```

```json
{
  "ok": true,
  "count": 24,
  "raw_count": 12,
  "cooked_count": 12,
  "crafted_count": 6,
  "foods": [
    {
      "id": "apple",
      "label": "Apple",
      "kind": "raw",
      "group": "fruit",
      "group_label": "fruit",
      "nutrition": 12.0,
      "saturation_ratio": 0.5,
      "comfort_secs": 0,
      "crafted_via": null,
      "cooldown_secs": 10,
      "effects": []
    }
  ],
  "recipes": []
}
```

### `GET /recipes`

The recipe book — what `POST /survival/craft` can produce and from what:

```bash
curl -s localhost:8765/recipes
```

```json
{
  "ok": true,
  "count": 6,
  "recipes": [
    {
      "id": "omelette",
      "label": "Cheese omelette",
      "result": "omelette",
      "result_label": "Cheese omelette",
      "blurb": "Two eggs and a fistful of cheese. Fast, hot, filling.",
      "ingredients": [
        {"food": "egg", "count": 2, "held": 2},
        {"food": "cheese", "count": 1, "held": 1}
      ],
      "can_cook": true,
      "missing": []
    }
  ]
}
```

Each recipe also reports the bag: every ingredient line carries `held`, and
`can_cook` is true only when the inventory covers every line. `missing` lists
what is short, in the same `{item, label, need, held, short}` shape the craft
route refuses with — so a Cook button can be disabled honestly instead of
turning a click into a 409. A recipe with no ingredients held reports
`"can_cook": false` and its full `missing` list.

`GET /foods` carries the same enriched array under `recipes`, so a client can
offer crafting from one request.

### `GET /inventory`

The avatar's bag: one entry per slot (empty slots included), the grouped totals,
and the static item catalog. `slots` is what the grid renders; `totals` is the
short "how much of what" a note line needs.

```bash
curl -s localhost:8765/inventory
```

```json
{
  "ok": true,
  "capacity": 20,
  "used": 3,
  "free": 17,
  "full": false,
  "slots": [
    {
      "index": 0,
      "id": "fishing_rod",
      "count": 1,
      "label": "Fishing rod",
      "icon": "🎣",
      "kind": "tool",
      "stack_max": 1,
      "rarity": 2,
      "blurb": "A rod, a line, and a bobber. Required to fish.",
      "food": null,
      "edible": false,
      "sell_price": 30
    },
    {"index": 1, "id": "worm_bait", "count": 5},
    {"index": 3, "id": null, "sell_price": null}
  ],
  "totals": [
    {"id": "fishing_rod", "count": 1, "label": "Fishing rod", "icon": "🎣", "kind": "tool", "slot": 0}
  ],
  "catalog": [
    {"id": "minnow", "label": "Minnow", "icon": "🐟", "kind": "fish", "kind_label": "Fish",
     "stack_max": 20, "blurb": "Small, silver, and mostly bones.", "food": "minnow", "rarity": 1,
     "sell_price": 2}
  ]
}
```

A fresh avatar starts with a fishing rod, five worm bait and two driftwood, so
the Fishing tab is playable before anything is gathered. `capacity` is
`survival.inventory.slots`; an empty slot is
`{"index": n, "id": null, "sell_price": null}`. Every occupied slot **and** every
catalog entry carries `sell_price` — exactly what `POST /store/sell` pays for one
unit of it — so the Bag tab can label its Sell button without a second request.
The counter is wider than the shelf; see *`POST /store/sell`* for the rules.

The catalog's `kind` is one of `fish`, `bait`, `ingredient`, `tool`, `material`
or `junk`, and `food` names the `GET /foods` entry an edible item becomes when it
is used. **Ingredients** (`apple`, `egg`, `cheese`, `tomato`, `mushroom`,
`noodles`, `bread`, `honey`) are food items: using one eats it raw, and
`POST /survival/craft` spends them to cook a recipe.

### `POST /inventory/add`

```bash
curl -s -X POST localhost:8765/inventory/add -H 'content-type: application/json' \
     -d '{"item": "minnow", "count": 3}'
```

`item` (or `id`) is a catalog id, `count` defaults to `1` and is clamped to
1–99. Existing partial stacks are topped up before an empty slot is taken, so
25 minnows are one full slot (20) plus a five-item slot. The reply is the whole
`GET /inventory` body with `added`, `leftover` and `change` added:
`leftover` is the part that did not fit. An unknown id is a **400**; a bag with
no room is a **409**. Both name the item.

An add emits `inventory-changed` with `{item, label, icon, delta, count, source,
inventory}` where `inventory` is the same summary `GET /survival` carries.

### `POST /inventory/remove`

The mirror image — same body, same reply shape, `removed` instead of `added`:

```bash
curl -s -X POST localhost:8765/inventory/remove -H 'content-type: application/json' \
     -d '{"item": "worm_bait", "count": 2}'
```

Asking for more than the bag holds is a **409** (`only 3 worm baits in the bag,
cannot remove 5`) rather than a silent partial removal. Stacks drain from the
last slot first, so earlier slots stay full.

### `POST /inventory/use`

Spend one item. Address it by `item`, `id` or `slot` (the grid's index):

```bash
curl -s -X POST localhost:8765/inventory/use -H 'content-type: application/json' \
     -d '{"slot": 3}'
```

An **edible** item goes through exactly the path `POST /survival/feed` uses, so
a caught trout restores hunger, grants its saturation buffer and starts the
digestion cooldown. The reply is the meal payload plus `item`, `item_label`,
`remaining` and `change`. The item is spent **only if the meal was accepted** —
a full avatar gets a **409** (`refusal: "too_full"`) and keeps the fish. Gear
and bait are a **400**: they can be held, not used.

### `GET /store`

The shop's price list, the player's coin balance, and how much of each entry the
bag already holds — one request is enough to draw the whole shop.

```bash
curl -s localhost:8765/store
```

```json
{
  "ok": true,
  "coins": 89,
  "starting_coins": 100,
  "count": 13,
  "catalog": [
    {
      "id": "worm_bait",
      "label": "Worm bait",
      "icon": "🪱",
      "buy_price": 4,
      "sell_price": 1,
      "category": "bait",
      "category_label": "Bait",
      "owned": 5,
      "stack_max": 64
    },
    {
      "id": "egg",
      "label": "Egg",
      "icon": "🥚",
      "buy_price": 3,
      "sell_price": 1,
      "category": "ingredient",
      "category_label": "Ingredients",
      "owned": 0,
      "stack_max": 12
    }
  ],
  "categories": [
    {"id": "bait", "label": "Bait"},
    {"id": "ingredient", "label": "Ingredients"},
    {"id": "material", "label": "Materials"},
    {"id": "tool", "label": "Tools"}
  ],
  "inventory": {"capacity": 20, "used": 3, "free": 17, "full": false, "totals": []}
}
```

The Survival card's *Store* tab is a client of these three routes — it draws the
shelf from this body and trades through the two POSTs, so what a player sees and
what an agent gets from `curl` cannot diverge.

`coins` is the authoritative wallet: the economy lives in the survival plugin, on
the same `SurvivalStats` the hunger bars do, so it also rides along in
`GET /survival` (`stats.coins`, plus the flattened `coins`) and in every
`survival` telemetry frame. A new session starts with **100** (`starting_coins`),
which is enough for a dozen casts of bait — the shop is usable before the first
fish is caught.

Every catalog entry's `id` is also an id in `GET /inventory`'s `catalog`, so
buying one puts exactly that item in the bag: `stack_max` here is the bag's own
stack limit and `owned` is how many the player already holds (what a sell button
greys itself out on). `category` is one of `bait`, `ingredient`, `material`,
`tool` and always matches the item's own `kind`.

The shelf carries **13** entries: two baits (`worm_bait` 4/1, `glowing_lure`
24/10), the eight cooking ingredients `GET /recipes` uses (`apple` 4/2, `egg`
3/1, `cheese` 5/2, `tomato` 3/1, `mushroom` 3/1, `noodles` 4/1, `bread` 4/2,
`honey` 8/3), two materials (`driftwood` 6/3, `river_weed` 5/2) and the rod
(75/30). An omelette therefore costs 11 coins — two eggs and a cheese — which is
the loop cooking runs on. `sell_price` is always below `buy_price` —
reselling is a loss, which is what makes the shop a sink rather than a coin
printer.

### `POST /store/buy`

```bash
curl -s -X POST localhost:8765/store/buy -H 'content-type: application/json' \
     -d '{"item": "worm_bait", "count": 3}'
```

`item` (or `id`) is a catalog id from `GET /store`; `count` defaults to `1` and
must be an integer from 1 to 99. Anything else is a **400** rather than a silent
clamp: a purchase is the one place where quietly changing the number spends the
player's coins on something they did not ask for.

The purchase is **all-or-nothing**. Both refusals are checked before anything
moves, so a failed buy costs nothing and a successful one cannot half-land:

| status | `refusal` | when |
|---|---|---|
| 200 | — | the goods are in the bag and the coins are gone |
| 400 | — | unknown item id, or a `count` that is not 1–99 |
| 409 | `insufficient_coins` | the tab is larger than the purse |
| 409 | `inventory_full` | the bag cannot take every unit |

```json
{
  "ok": false,
  "error": "not enough coins — Fishing rod x2 costs 150, the purse holds 100",
  "refusal": "insufficient_coins",
  "item": "fishing_rod",
  "count": 2,
  "required": 150,
  "coins": 100,
  "shortfall": 50
}
```

A purchase that lands answers with the trade and the resulting balance:

```json
{
  "ok": true,
  "action": "buy",
  "item": "worm_bait",
  "label": "Worm bait",
  "icon": "🪱",
  "count": 3,
  "requested": 3,
  "unit_price": 4,
  "total": 12,
  "coins": 88,
  "inventory": {"capacity": 20, "used": 3, "free": 17, "full": false, "totals": []}
}
```

`total` is `unit_price × count`, and `count` is what actually landed in the bag.
The same body is published as a `store-transaction` event, so the page and the
SSE bus see the new balance without asking.

### `POST /store/sell`

The mirror image — same body, same reply shape, `action: "sell"`:

```bash
curl -s -X POST localhost:8765/store/sell -H 'content-type: application/json' \
     -d '{"item": "driftwood", "count": 2}'
```

Selling is exact, not best-effort: asking for more than the bag holds is a **409**
(`refusal: "not_enough_items"`, carrying `held` and `count`) instead of a silent
partial sale. The shop **buys the whole item catalog**, not only the thirteen
lines it stocks: a stocked id pays its listed `sell_price`, and every other
catalog item pays a price derived from its `kind` and `rarity` — fish
`2 × rarity` (`minnow` 2 … `ember_eel` 8), ingredients and materials `rarity`,
bait `1 + rarity`, tools `4 × rarity`, and junk a flat **1** coin (an old boot is
still worth clearing out of a full bag). Only an id that is not an item at all is
a **400**. `unit_price` is paid per unit; the reply carries `total` and the new
`coins`, and a `store-transaction` event is emitted with `action: "sell"`.

### `GET /fishing`

The live fishing session. This route also runs the state machine to the current
moment, so it is the precise way to follow a cast: the host tick is 1 s, and the
page polls this at ~120 ms while the line is out.

```bash
curl -s localhost:8765/fishing
```

```json
{
  "ok": true,
  "enabled": true,
  "session": {
    "phase": "reeling",
    "active": true,
    "hook_window_ms": 1400,
    "min_wait_secs": 3,
    "max_wait_secs": 12,
    "hook_key": "Space",
    "reel_key": "Space",
    "mouse_button": 0,
    "reel": {
      "period_ms": 1500,
      "sweet_start": 0.41,
      "sweet_width": 0.22,
      "sweet_end": 0.63,
      "started_ms": 1700000000000,
      "deadline_ms": 1700000003000,
      "elapsed_ms": 375,
      "position": 0.5
    },
    "casts": 1,
    "catches": 0,
    "escapes": 0
  },
  "config": {"enabled": true, "hook_key": "Space", "difficulty": 1.0},
  "inventory": {"capacity": 20, "used": 3, "free": 17, "full": false}
}
```

`phase` is `idle`, `waiting`, `biting` or `reeling`. `reel` is `null` unless the
phase is `reeling`. Note what is **absent**: `bite_at_ms` is never published, so
a client cannot pre-aim at the bite — it learns the fish is on when the phase
flips.

### `POST /fishing/cast`

Puts the line in the water. Needs a `fishing_rod` in the bag
(`fishing.require_rod`), and spends one bait when `fishing.bait_required` is on.

```bash
curl -s -X POST localhost:8765/fishing/cast
```

The wait is drawn here from `fishing.min_wait_secs`–`max_wait_secs` and kept
private; the reply is `{phase: "waiting", casts, min_wait_secs, max_wait_secs,
hook_window_ms, hook_key, reel_key, mouse_button, session, inventory}` and the
`fishing-started` event carries the same. A cast while a session is running, or
without the rod (or bait), is a **409** naming which.

### `POST /fishing/hook`

The reaction press. Legal only while the phase is `biting`:

```bash
curl -s -X POST localhost:8765/fishing/hook
```

Too early (`waiting`), too late (the window lapsed, so the phase is `idle`) or
already hooked (`reeling`) are all **409** with the reason. Success starts the
reel: the reply is `{phase: "reeling", reel, reel_key, mouse_button,
difficulty, casts}` and `fish-hooked` carries the same sweep parameters the page
draws.

### `POST /fishing/reel`

The precision press — one per hook. The plugin recomputes the marker position
from **its own clock** and compares it with the sweet spot; a client cannot
widen the window by lying about where the marker was.

```bash
curl -s -X POST localhost:8765/fishing/reel
```

```json
{
  "caught": true,
  "stored": true,
  "item": "silver_trout",
  "label": "Silver trout",
  "icon": "🎏",
  "junk": false,
  "value": 6,
  "reason": null,
  "position": 0.52,
  "sweet": {"start": 0.41, "end": 0.63},
  "casts": 1,
  "catches": 1,
  "escapes": 0,
  "inventory": {"capacity": 20, "used": 4, "free": 16, "full": false}
}
```

A miss (or letting the two-sweep deadline pass) answers the same shape with
`caught: false` and a `reason`: `missed_the_spot`, `timed_out`, `spooked`,
`cancelled` or `inventory_full`. That last one means the fish was landed but the
bag had no room, so `caught` stays `false` and nothing is stored. Every outcome
emits `fishing-result`, and a stored catch also emits `inventory-changed`. The
frame (not just this reply) carries `species`, `rarity` (1-4), `value` (the
shop's sell price, i.e. what `POST /store/sell` would pay) and `blurb`,
`expression` and `line`; a miss carries `reason`, `expression` and `line`
instead of the species block. Only this frame reaches the watcher's AI brain —
the earlier `fishing-started` / `fishing-bite` / `fish-hooked` frames are the
page's own, and the watcher turns this one into a `fish_caught` or `fish_lost`
trigger (speaking `line` as its offline fallback).

### `POST /fishing/cancel`

Gives up on the current cast and reports it as a `fishing-result` with
`reason: "cancelled"`. Nothing to cancel is a **409**.

## Metamorphosis (the `metamorphosis` plugin)

The `metamorphosis` guest owns one procedurally generated creature: a seed, a
stage, XP, bond and an attribute. Every stage is a complete `desktop-buddy/model@1`
rig it **generates itself** (no sprites, no AI, no network) and writes into
`models/<slug>/model.json` through a host import, which validates it with the
app's own loader before anything lands on disk. `/metamorphose` then hot-swaps the
avatar onto the new rig — the same code path `POST /model` takes — and plays the
`metamorphose` clip.

Stages are ordered `Fresh → In-Training → Rookie → Champion → Ultimate → Mega`.
Each stage **pins its variant** (form, build, marking) in its saved state and each
history entry pins the parent's, so a stage rebuilds identically forever; a child
inherits its parent's body, mutating (12% new form, 16% new build/marking) only
occasionally.
Each transition has three branches — one per attribute, `Seraph`, `Prism` and
`Feral` — and **all three are always on offer**. Bond only *orders* them: a
devoted creature (bond > 0.66) is offered `Seraph` first, a feral one
(bond < 0.33) `Feral` first with `Seraph` last, and the neutral middle keeps the
creature's own attribute in front. Bond and XP seed the branch labels, never the
choice set.

**The attribute is a body plan, not a badge.** It reaches the generator
(`Spec.attribute`), so the three branches are three different creatures even
from one seed:

| attribute | the line grows | reads as |
| --- | --- | --- |
| `Seraph` | wings, a ring of light, feather vanes, a blade crest | haloed, feathered, gentle |
| `Prism` | crystal growths, circuit traces, a visor band, floating bits | crystalline, plated, digital |
| `Feral` | back spines, fangs, toxic drips, clawed hands | fanged, spiked, toxic |

The colours follow too, but only as a tint: a Seraph and a Feral Drake differ
in silhouette first. On top of the archetype and the attribute, the seed picks
one of the family's six structural forms, one of four builds (lean / canonical
/ broad / spare), a marking (none / spots / stripes / patches / bands / dapple) and, just under half the
time, a second accessory from that family's own vocabulary, so two Rookies of
the same archetype are still two individuals. State is persisted as
`plugins/metamorphosis.state.json`, so a restart resumes the same line.

### `GET /creature/status`

Everything the growth system knows, plus the measurements of the live rig.

```bash
curl -s localhost:8765/creature/status
```

```json
{
  "ok": true,
  "creature": { "seed": 1585367844, "archetype": "drake", "seed_hex": "000000005e7d6e64" },
  "stage": "Rookie",
  "tier_index": 2,
  "xp": 64,
  "xp_required": 180,
  "xp_to_next": 116,
  "bond": 0.52,
  "attribute": "Feral",
  "attribute_blend": {
    "dominant": "Feral",
    "weights": { "Seraph": 0.0, "Prism": 0.333333, "Feral": 0.666667 },
    "percent": { "Seraph": 0, "Prism": 33, "Feral": 67 },
    "summary": "67% Feral · 33% Prism",
    "flavour": "fanged, spiked, toxic · crystalline, plated, digital",
    "pure": false,
    "lineage": ["Prism", "Feral"],
    "loud": [
      { "attribute": "Feral", "weight": 0.666667, "percent": 67, "flavour": "fanged, spiked, toxic" },
      { "attribute": "Prism", "weight": 0.333333, "percent": 33, "flavour": "crystalline, plated, digital" }
    ]
  },
  "slug": "buddy-drake-rookie-feral-1f3a9c02",
  "generation": 2,
  "installed": true,
  "can_evolve": false,
  "final_stage": false,
  "can_revert": true,
  "rewind_depth": 1,
  "previous_stage": "In-Training",
  "last_evolved_ms": 1732000123456,
  "rig": { "bones": 19, "parts": 46, "parts_budget": 100, "clips": 3, "min_world_y": -86.4, "symmetry_error": 0.0 },
  "attributes": [ { "value": "Feral", "label": "Feral", "flavour": "fanged, spiked, toxic" } ],
  "archetypes": [ { "value": "beast", "label": "Beast" }, { "value": "drake", "label": "Drake" } ],
  "branches": [
    { "stage": "Champion", "attribute": "Feral", "label": "Drake Champion (Feral)",
      "slug": "buddy-drake-champion-feral-9b21c4d0", "seed": 11166978496, "xp_required": 180,
      "flavour": "fanged, spiked, toxic",
      "blend": { "weights": [0.0, 0.25, 0.75] },
      "blend_summary": "75% Feral · 25% Prism",
      "blended_flavour": "fanged, spiked, toxic · crystalline, plated, digital" }
  ],
  "history": [ { "from": "In-Training", "to": "Rookie", "slug": "buddy-drake-rookie-feral-1f3a9c02",
                 "at_ms": 1732000123456, "from_seed": 1585367844, "from_attribute": "Prism",
                 "to_attribute": "Feral" } ],
  "last_error": null
}
```

`rig.min_world_y` is the highest pixel the current stage paints (world space,
root at `scale: 0.55`); the generator keeps it at or below `-170` so the
artwork never clips the top of the monitor. `rig.symmetry_error` is the worst
left/right disagreement in the skeleton — the format's bound is `1e-6`, and the
generator lands at zero to the last bit of the pose arithmetic. `rig.parts_budget`
is the cap that stage was validated against: `100` for Fresh through Champion,
`150` for Ultimate and Mega. `installed` says the stage's rig has been published
by **this** run (the plugin re-writes it at startup rather than trusting a saved
claim).

**`attribute_blend`** is the creature's whole *lineage*, not just its current
attribute. `lineage` reads oldest-first: the egg's attribute, then the attribute
each transition chose; the weights are the linear recency ramp (the oldest stage
counts `1`, the newest `n`) normalised to sum to `1.0`, so the most recent
choices lead while every earlier stage stays visible. `weights` are those shares,
`percent` the same mix as whole numbers that always total `100`, `summary` the
readout (`"67% Feral · 33% Prism"`), `flavour` every contributor's traits
loudest-first, `loud` the same data as an ordered array, and `dominant` the
attribute with the largest share. The rig is generated from this mix: crystals
and circuit traces from the Prism share, spines/fangs/drips from the Feral share,
halo and feather vanes from the Seraph share — each scaled by its share, and the
skin painted in a weighted blend of the three palettes. `attribute` (top level)
stays the single *current* attribute, which is what names the model folder, so
every older client keeps working. Each branch in `branches` carries its own
`blend` / `blend_summary` / `blended_flavour`: the mix taking that branch would
produce.

### `POST /metamorphose`

Advances one stage. Body is optional:

| field | meaning |
| --- | --- |
| `branch` | index into `status.branches` (default `0`) |
| `force` | `true` skips the XP gate (demo/verification; the default honours it) |

```bash
curl -s -X POST localhost:8765/metamorphose -H 'content-type: application/json' \
  -d '{"branch": 1, "force": true}'
```

```json
{
  "ok": true,
  "from": "Fresh",
  "stage": "In-Training",
  "slug": "buddy-drake-in-training-3c0f22aa",
  "label": "Drake In-Training",
  "attribute": "Prism",
  "attribute_blend": { "dominant": "Prism", "percent": { "Seraph": 0, "Prism": 67, "Feral": 33 },
                       "summary": "67% Prism · 33% Feral", "lineage": ["Prism", "Feral"], "pure": false },
  "blend_summary": "67% Prism · 33% Feral",
  "animation": "metamorphose",
  "bones": 13,
  "parts": 31,
  "min_world_y": -74.2,
  "model_path": "models/buddy-drake-in-training-3c0f22aa/model.json",
  "status": { "stage": "In-Training" }
}
```

An insufficient XP is a **409** (`"not ready: 20 xp of 90 …"`); `Mega` is the
last stage and also a **409**. The reply carries the full post-transition status
so one request repaints a client, plus `attribute_blend` / `blend_summary` — the
lineage mix the new body was actually generated from (the creature it had been,
plus the attribute this branch chose).

### `POST /revert`

Goes **back** one stage (or several). Body is optional:

| field | meaning |
| --- | --- |
| `steps` | how many stages to rewind (default `1`, clamped to the recorded history) |

```bash
curl -s -X POST localhost:8765/revert -H 'content-type: application/json' -d '{"steps": 1}'
```

```json
{
  "ok": true,
  "direction": "out",
  "steps": 1,
  "from": "In-Training",
  "stage": "Fresh",
  "slug": "buddy-beast-fresh-5eed1234",
  "label": "Beast Fresh",
  "attribute": "Prism",
  "attribute_blend": { "dominant": "Prism", "percent": { "Seraph": 0, "Prism": 100, "Feral": 0 },
                       "summary": "100% Prism", "lineage": ["Prism"], "pure": true },
  "blend_summary": "100% Prism",
  "animation": "metamorphose",
  "bones": 9,
  "parts": 27,
  "min_world_y": -49.8,
  "model_path": "models/buddy-beast-fresh-5eed1234/model.json",
  "status": { "stage": "Fresh" }
}
```

Because generation is deterministic and each history entry records the
**parent's** seed and attribute — and both are used when rebuilding, so rewinding
across a change of attribute lands on the creature that was actually there — a
rewind regenerates the *exact* creature the
line had before — same bones, colours and clips, and the same attribute **mix**
(the blend of the truncated lineage, which is the prefix that stage was built
from) — then writes it and hot-swaps the avatar through the same `POST /model`
path as evolution. The reply's `attribute_blend` is therefore the mix the
creature returns *to*, not the one it had a moment ago. XP is kept (a
creature has still grown), so it can evolve straight back. At `Fresh` — or with
no history — the answer is a **409**; `steps: 0` is a **400**.

A history entry written *before* rewind existed has no parent seed, and that seed
is one-way mixed into the child's, so it cannot be recovered: rewinding such a
step is refused with a **409** naming the step, rather than silently landing on
a different creature. Evolving once records it, and the rewind works from then
on.

### Growth fields on `GET /creature/status`

`GET /creature/status` also carries `can_revert` (is there a parent stage),
`rewind_depth` (how many stages of history exist) and `previous_stage` (the
stage a single-step rewind returns to), which is what the card's **◀ Back**
button reads.

A successful transition emits one `creature` event with `event: "metamorphosed"`,
`from`, `stage`, `slug`, `attribute`, `bones`, `parts` and a `line` — the
watcher turns it into a trigger, and with no brain attached the plugin speaks
the line itself. Growing past the threshold emits the same event with
`event: "ready"` exactly once per stage.

### `POST /creature/xp`

Grants growth directly — `{"xp": 40, "bond": 0.2}` (either field alone works).
`bond` is clamped to `-1..1` around the current value. Empty body is a **400**.

### `POST /creature/reset`

Model ids are `buddy-<archetype>-<stage>-<attribute>-<seed>` (`buddy-beast-fresh-feral-00001092`).
The attribute is in the name because it is part of the rig — Seraph and Feral
versions of the same seed are different creatures and must not share a folder.

Starts a new creature, installs its `Fresh` rig **and switches the avatar to it**
(the `POST /model` path), then plays the `metamorphose` clip:
`{"seed": 1234, "archetype": "cyborg", "attribute": "Feral"}`. `archetype` is
one of `beast`, `drake`, `cyborg`, `holy`, `aqua`, `cervid`, `machine`,
`alien`, `avian`, `insect`, `serpent`, `golem` (aliases: `fish`, `deer`,
`robot`, `angel`, `dragon`, `android`, `bird`, `bug`, `snake`, `rock`);
anything else is a **400**. Missing
fields keep the current creature's values (and a missing `seed` uses the clock).

Unlike the install that happens at plugin start-up — which deliberately leaves
the user's avatar alone — this one is an explicit request for a new creature, so
the body on screen changes immediately.

### `GET /plugins`

The plugin registry: every registered plugin, its lifecycle state, tick and
error counters and the last error it raised. Plugins are **WebAssembly modules**
loaded from the `plugins/` directory at startup — the routes below are answered
by a guest instance, not by code in this binary.

```bash
curl -s localhost:8765/plugins
```

```json
{
  "ok": true,
  "ticks": 42,
  "count": 1,
  "active": 1,
  "plugins": [
    {
      "id": "survival",
      "description": "Hunger, saturation and tiers: drains on the wall clock, publishes telemetry and serves the feeding routes.",
      "state": "active",
      "ticks": 42,
      "errors": 0,
      "last_error": null,
      "status": {
        "enabled": true,
        "tier": "peckish",
        "hunger": 24.9,
        "saturation": 39.9,
        "meals": 2,
        "telemetry_secs": 60,
        "frames": 3,
        "inventory_used": 3,
        "inventory_capacity": 20,
        "fishing_phase": "idle",
        "casts": 0,
        "catches": 0,
        "escapes": 0,
        "uptime_ms": 42000
      }
    }
  ]
}
```

`state` is `registered`, `active`, `failed` or `stopped`. A plugin that panics
repeatedly in its tick is parked as `failed` and its `shutdown` hook runs; the
registry keeps the rest running. `GET /plugins` also emits the `plugins` frame
whenever the registry's own status changes.

**The routes are the plugins'.** `/survival`, `/survival/feed`,
`/survival/craft`, `/inventory`, `/inventory/add`, `/inventory/remove`,
`/inventory/use`, `/fishing`, `/fishing/cast`, `/fishing/hook`,
`/fishing/reel`, `/fishing/cancel`, `/foods` and `/recipes` come
from the survival plugin: its `plugin_routes()` export declares them, and the
host maps each one to an axum route that proxies the request into the Wasm
instance. `/plugins` is the host's own introspection route (it answers even with
nothing installed). `api.rs` names none of them: delete `plugins/survival.wasm`
and those endpoints disappear while `/plugins` still answers. The same is true of config blocks —
`GET /config` lists a section only if a plugin claims it.

### `POST /shutdown`

Closes the app (window + API) after flushing the response.

---

## Awareness: desktop context & the event stream

These endpoints exist so an agent can stop polling: one asks "what is the
user doing?", the others push "what did the avatar (or an external monitor)
just do?".

### `GET /context`

A cheap snapshot of the user's desktop. On Windows it reads
`GetForegroundWindow` + `GetWindowTextW` for the focused window title,
`GetWindowThreadProcessId` + `OpenProcess` + `QueryFullProcessImageNameW` for
the executable that owns it, `GetLastInputInfo` + `GetTickCount64` for
machine-wide idle time, `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`
(falling back to `GetWindowRect`) for the focused window's rectangle, and
`EnumWindows` for the front-most visible windows. Every rectangle is converted
from physical pixels to **CSS pixels** measured from the **virtual desktop's**
origin (`SM_XVIRTUALSCREEN`, negative when a monitor sits left of or above the
primary) and divided by the CSS `scale`, so it lines up with the overlay's
coordinate space — the overlay covers every monitor — and with the avatar's own
position. `screen` describes that space in physical pixels;
`physical = css * scale + origin` is the conversion back, which is what the
watcher's GDI capture uses. On other platforms the values are empty/zero (`"window_rect": null`,
`"visible_windows": []`) and `source` says so.

```bash
curl -s localhost:8765/context
```

```json
{
  "ok": true,
  "window_title": "main.rs - visual-studio-code",
  "process_name": "Code.exe",
  "idle_secs": 12,
  "window_rect": { "x": 400.0, "y": 300.0, "width": 800.0, "height": 500.0 },
  "foreground_hwnd": 67672,
  "visible_windows": [
    { "title": "main.rs - visual-studio-code", "process_name": "Code.exe",
      "rect": { "x": 400.0, "y": 300.0, "width": 800.0, "height": 500.0 },
      "hwnd": 67672 },
    { "title": "Inbox - Firefox", "process_name": "firefox.exe",
      "rect": { "x": 1200.0, "y": 60.0, "width": 700.0, "height": 900.0 },
      "hwnd": 88214 }
  ],
  "screen": { "x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0, "scale": 1.0 },
  "avatar_state": {
    "position": [700.0, 500.0],
    "target_position": [700.0, 500.0],
    "state": "idle",
    "expression": "neutral",
    "revision": 7,
    "talking": false
  },
  "recent_events": [
    {"seq": 6, "type": "speech", "ts_ms": 1794405560000, "payload": {"text": "Build finished.", "expression": "happy", "duration_ms": 6000, "char_ms": 26}},
    {"seq": 7, "type": "interaction", "ts_ms": 1794405567002, "payload": {"kind": "double_click", "x": 541, "y": 489, "state": "idle", "expression": "neutral", "position": [760.0, 520.0]}}
  ],
  "source": "win32"
}
```

| Key | Meaning |
| --- | --- |
| `window_title` | title of the foreground window (`""` when there is none) |
| `process_name` | executable that owns the foreground window, e.g. `"Code.exe"` (`""` when it cannot be read — a protected process, or a Linux/macOS backend that cannot resolve the owner) |
| `idle_secs` | seconds since the last keyboard/mouse input anywhere on the machine |
| `window_rect` | bounds of the foreground window in CSS px — `{x, y, width, height}`, or `null` when nothing is focused / the backend cannot probe it. This is the rect `POST /perch` uses when no `window_process` is given |
| `visible_windows` | up to **5** front-most top-level windows (`{title, process_name, rect}`), skipping minimized, cloaked, untitled and the avatar's own window — what an agent can perch on by process name |
| `avatar_state` | identical shape to `GET /state` |
| `recent_events` | the last **15** SSE frames, oldest first, in the same `{seq, type, ts_ms, payload}` envelope `GET /events` pushes — the agent's short-term memory of what just happened |
| `source` | which probe ran: `win32`, `xdotool`, `wayland`, `osascript`, or `unsupported` when none could be used |

The handler queries the OS directly and never touches the avatar state machine,
so it is safe to poll at any rate. `process_name` is best-effort: a protected
process or the desktop/secure window yields `""` rather than an error, and such
a window is left out of `visible_windows` entirely.

### `GET /clipboard`

The clipboard's text (`CF_UNICODETEXT`), capped at **2000 characters**.

```bash
curl -s localhost:8765/clipboard
```

```json
{ "ok": true, "text": "fn main() { println!(\"hi\"); }", "truncated": false }
```

| Key | Meaning |
| --- | --- |
| `text` | the clipboard text, or `""` when the clipboard is empty / not text |
| `truncated` | `true` when the source was longer than 2000 characters and had to be cut |

It always answers `ok: true`: an empty clipboard, a non-text clipboard and a
clipboard another process is holding open all read as `""`. The read is
best-effort (the clipboard is a machine-wide singleton, so the open is retried a
few times) and never blocks the API thread for long.

### `POST /clipboard`

Put text on the clipboard — the write half of the route above, and how an agent
hands over something too big for a bubble.

```bash
curl -s -X POST localhost:8765/clipboard -H 'content-type: application/json' \
     -d '{"text":"fn main() {\n    println!(\"hi\");\n}\n"}'
```

```json
{ "ok": true, "chars": 39, "truncated": false }
```

| Field | Meaning |
| --- | --- |
| `text` | required; the exact text to copy (newlines kept). `""` clears the clipboard |
| `chars` | characters actually written |
| `truncated` | `true` when `text` exceeded the 32 000-character cap and was cut |

Win32 `OpenClipboard` → `EmptyClipboard` → `GlobalAlloc(GMEM_MOVEABLE)` + UTF-16
copy → `SetClipboardData(CF_UNICODETEXT)` → `CloseClipboard`, retried while the
clipboard is busy. If another process holds it open for the whole retry window
the route answers **400** (`the clipboard is held open by another process; try
again`) — the one clipboard case worth retrying rather than ignoring. The same
writer backs the `set_clipboard` Tauri command (the bubble's **Copy** button) and
the `avatar_set_clipboard` MCP tool.

### `POST /reply`

The user's answer, typed into the speech bubble's quick-reply box.

```bash
curl -s -X POST localhost:8765/reply -H 'content-type: application/json' \
     -d '{"text":"yes, do that"}'
```

```json
{ "ok": true, "text": "yes, do that", "subscribers": 1 }
```

It broadcasts a `user_reply` frame on `GET /events` and dismisses the bubble.
`text` must not be blank and is capped at 500 characters (**400** otherwise).
The page-side twin is the `notify_reply` command, so a reply behaves identically
whichever end sent it.

Optional `include_screen: true` rides the same frame and tells
`desktop-buddy-watcher.exe` to attach a screenshot to this turn even when the text
matches none of the `vision.trigger_phrases` phrases — it is what the panel's
**What's on screen?** button and the command bar's matching chip send:

```bash
curl -s -X POST localhost:8765/reply -H 'content-type: application/json' \
     -d '{"text":"what is this error?","include_screen":true}'
```

The watcher resolves the `vision` AI profile by name, captures the focused
window (or the full screen when `capture_mode` says so or the rect is unusable),
downscales to `max_image_dim`, JPEG-encodes it in memory and sends one
OpenAI-shaped multimodal `chat/completions` request. The image is never written
to disk. A disabled block, an unreachable endpoint or a failed capture produces
a spoken apology instead of a stuck **Thinking…** — the text routes are
unaffected.

### `POST /alert`

Let an external monitor — a Suricata instance, a cron job, a build script —
reach the user through the avatar.

```bash
curl -s -X POST localhost:8765/alert -H 'content-type: application/json' \
     -d '{"source":"suricata","summary":"ET SCAN port sweep from 192.168.0.9","severity":"warn"}'
```

```json
{
  "ok": true,
  "alert": { "source": "suricata", "summary": "ET SCAN port sweep from 192.168.0.9", "severity": "warn" },
  "subscribers": 1
}
```

| Field | Meaning |
| --- | --- |
| `source` | who is reporting (required, non-blank) |
| `summary` | one sentence about what happened (required, non-blank) |
| `severity` | `info` \| `warn` \| `error`; defaults to `info`, anything else is **400** |

The route only validates and broadcasts an `alert` frame — it never calls a
model itself. `desktop-buddy-watcher.exe` picks the frame up and wakes Hermes under
its own `alerts.cooldown_secs` throttle, so a `curl` cannot spawn agent turns.

### `POST /session/reset`

Drop the conversation context — the HTTP twin of the command bar's **↺** and the
control panel's **New chat**.

```bash
curl -s -X POST localhost:8765/session/reset
```

```json
{ "ok": true, "reason": "api", "subscribers": 1 }
```

The app owns no conversation state: it broadcasts a `session_reset` frame on
`GET /events` and dismisses the visible bubble, and `desktop-buddy-watcher.exe`
rotates its `{session}` id when it sees the frame (it logs
`session reset (api): desktop-buddy-<ts> -> desktop-buddy-<ts>-2`). The next trigger then
runs in a brand-new Hermes session, so the model starts from a clean context.

### `GET /events` (Server-Sent Events)

A push stream of everything the avatar does: state transitions, speech, model
switches, rig saves, pose updates, part patches, custom animations, pointer
interactions, quick replies and external alerts.

```bash
curl -N localhost:8765/events
```

```
event: hello
data: {"seq":0,"type":"hello","ts_ms":1794405907511,"payload":{"api_url":"http://127.0.0.1:8765","click_through":true,"model":"demo-blob","state":{"position":[1185.9,224.4],"target_position":[1185.9,224.4],"state":"idle","expression":"neutral","revision":33,"talking":false},"subscribers":1}}

event: state
data: {"seq":5,"type":"state","ts_ms":1794405505861,"payload":{"position":[229.1,453.4],"target_position":[900.0,400.0],"state":"walking","expression":"neutral","revision":3,"talking":false}}

event: interaction
data: {"seq":18,"type":"interaction","ts_ms":1794405567002,"payload":{"kind":"double_click","x":541,"y":489,"state":"idle","expression":"neutral","position":[760.0,520.0]}}

: keep-alive
```

Every `data:` line is one JSON object:

| Key | Meaning |
| --- | --- |
| `seq` | monotonic frame counter across all subscribers (`0` for `hello`) |
| `type` | event type — also the SSE `event:` field, so `EventSource` users can `addEventListener("interaction", …)` |
| `ts_ms` | Unix epoch milliseconds |
| `payload` | type-specific object (see below) |

| `type` | Emitted when | `payload` |
| --- | --- | --- |
| `hello` | a client connects (always the first frame) | `{api_url, click_through, model, state, subscribers}` |
| `state` | state / expression / talking changes — from HTTP, a command, or the page's own transitions | full `AvatarState` (same shape as `GET /state`) |
| `speech` | `POST /speak` | `{text, expression, duration_ms, char_ms}`, plus `details` when the bubble carries a drawer |
| `speech_cleared` | `POST /clear_speech` (or the bubble dismissing itself) | `{}` |
| `model` | model loaded, switched, imported or reset | `{name, label, rig_path, is_override, bones, images, reason}` with `reason` ∈ `load` \| `switch` \| `import` \| `reset` |
| `rig_saved` | `POST /rig`, or the editor's **Save rig** | `{rig_path}` |
| `pose` | `POST /pose` / `DELETE /pose` | `{pose}` |
| `part_patch` | `POST /part`, `POST /mouth` | `{target, patch}` |
| `animate` | `POST /animate`, `POST /animate/stop` | `{action, name, duration_ms, loop, keyframes}` with `action` ∈ `play` \| `stop` |
| `interaction` | the user clicks the avatar (§ Avatar interaction) | `{kind, x, y, state, expression, position}` with `kind` ∈ `click` \| `double_click` \| `drag_end` |
| `user_reply` | the user submits the bubble's quick-reply box (`notify_reply`) or `POST /reply` | `{text}` |
| `alert` | `POST /alert` from an external monitor | `{source, summary, severity}` with `severity` ∈ `info` \| `warn` \| `error` |
| `session_reset` | `POST /session/reset`, the command bar's **↺** or the control panel's **New chat** | `{reason}` with `reason` ∈ `ui` \| `api` |
| `ai_profile` | `POST /ai-profile`, the panel's **AI Brain** dropdown, or the `set_ai_profile` command | `{name, active_profile, source}` with `source` ∈ `ui` \| `api` |
| `watcher` | the supervised `desktop-buddy-watcher.exe` starts, stops, or writes a line | full status `{ok, enabled, running, pid, binary_path, rules_path, logs}` on a lifecycle change, or `{kind: "log", line, enabled, running, pid}` for one log line |

Behaviour worth knowing:

* A `: keep-alive` comment is sent every **15 s** of silence, so `curl -N`,
  proxies and idle TCP connections never look dead.
* The broadcast channel holds **64** frames. A subscriber that falls further
  behind is skipped forward (the connection stays open) and resynchronises on
  the next `state` frame.
* The page reports its animated position at 8 Hz, but only *transitions*
  (`idle→walking`, expression changes, talking on/off) are broadcast — position
  echoes never spam the stream.
* Frames are fire-and-forget: with no subscriber the send is a silent no-op.

A minimal consumer:

```python
import json, urllib.request

with urllib.request.urlopen("http://127.0.0.1:8765/events") as stream:
    for raw in stream:
        line = raw.decode().strip()
        if line.startswith("data: "):
            frame = json.loads(line[6:])
            if frame["type"] == "interaction":
                print("user clicked the avatar:", frame["payload"]["kind"])
```

### Errors

`400` with a body like

```json
{ "ok": false, "error": "invalid expression \"clown\"; expected one of [\"neutral\", \"happy\", \"sad\", \"surprised\", \"thinking\", \"angry\"]" }
```

`422` for malformed JSON bodies (serde), `404` for unknown routes.

### Example session

```bash
curl -s -X POST localhost:8765/move -H 'content-type: application/json' -d '{"x":400,"y":300}'
curl -s -X POST localhost:8765/expression -d '{"expression":"happy"}' -H 'content-type: application/json'
curl -s -X POST localhost:8765/wave
curl -s localhost:8765/state
curl -s -X POST localhost:8765/shutdown
```

## Internal (Tauri command) surface

The page talks to Rust through `invoke`; the same mutations are available as
commands. Arguments are snake_case (`rename_all = "snake_case"`). All 46, in the
order they are registered (ARCHITECTURE.md §6 has the full purpose table):

**State, window & interaction** — `get_avatar_state`, `get_runtime_info`,
`set_avatar_position(x, y)`, `break_perch()`, `set_avatar_expression(expression)`,
`set_avatar_state(new_state)`, `trigger_wave`, `report_state(position,
target_position, state, expression, joints?, talking?, speech?)`,
`update_hit_regions(regions)`, `set_click_through(enabled)`,
`notify_interaction(kind, x, y)`, `notify_reply(text)`, `reset_session`,
`close_window`, `minimize_window`.

**Models, rigs & parts** — `list_models`, `get_model`, `load_model(name)`,
`pick_import_file`, `pick_config_path(kind?)` (the Settings modal's
**Browse…** button: `kind` ∈ `executable` \| `model` \| `folder` \| `any`
chooses the dialog's filters and whether it picks a directory; returns the
chosen path or `null`, and it drops the overlay's always-on-top flag while
the dialog is up), `import_asset(path, mode?, name?, tag?)`,
`patch_part(target, patch)`, `set_mouth(state)`, `get_parts`, `save_rig(rig)`,
`reset_rig`, `set_pose(joints)`, `clear_pose`, `get_joints`.

**Expressions & speech** — `emote(expression, duration_ms?, revert?)`,
`speak(text, expression?, duration_ms?, char_ms?, details?)`, `set_clipboard(text)`
(the bubble's **Copy** button; same Win32 writer as `POST /clipboard`),
`clear_speech`, `get_speech`.

**Conversation history** — `get_chat_history` returns the `GET /history` body
(`{ok, messages}`: the current session's turns, oldest first, at most 50). The
drawer hydrates from it on load and then follows the page-local `chat-message` /
`chat-history-cleared` events, so it never polls.

**Watcher** — `get_watcher_status`, `set_watcher_enabled(enabled)`. The switch
and log tail behind the control panel's **Watcher** row and the pop-out console;
`set_watcher_enabled` returns the same status object as `GET /watcher`.

**Voice** — `get_tts_status` (the `GET /tts` object) and `set_tts_enabled(enabled)`
(the **Voice** switch: mute/unmute the local Piper voice, emits `tts-status`,
returns the new status; runtime only, `watcher_rules.json` is never rewritten).

**Push to talk** — `get_stt_status` (the `GET /stt` object), `ptt_start` /
`ptt_stop` (the **Hold to talk** button's press and release; `ptt_start` also cuts
the Piper voice off so the mic does not hear the avatar), and
`set_stt_enabled(enabled)` (mute/unmute, emits `stt-status`). Each returns the live
status object.

**AI brain** — `get_ai_profiles` (the same object as `GET /ai-profiles`, health
probe included) and `set_ai_profile(name)` (the **AI Brain** dropdown; persists,
probes and broadcasts exactly like `POST /ai-profile`, and returns the new
status; an unknown name is an error).

**Persona** — `open_soul_file` (the Settings modal's **Open SOUL.md** button:
resolve the persona path exactly as the watcher does, seed a missing file, hand
it to the OS's default editor — `cmd /C start`, `open`, `xdg-open` — and return
the path; a file that cannot be created or opened is an error string). The
persona *content* is the `GET /soul` / `POST /soul` pair, because it is just a
file the watcher re-reads every turn.

`notify_interaction` takes `kind` ∈ `click | double_click | drag_end` with `x`,
`y` in CSS pixels relative to the window, and returns
`{ok, kind, subscribers}` where `subscribers` is how many `GET /events` clients
received the frame. It is a pure notification — unlike every other command it
does not change `AvatarState`.

`notify_reply(text, profile?)` is the mirror image for the bubble's quick-reply
box: the user's text (≤ 500 characters) becomes a `user_reply` frame, the bubble
is dismissed and `{ok, text, profile, subscribers}` comes back. The optional
`profile` pins that one prompt to a specific AI profile (what a
`quick_tasks[].profile` chip does).
`POST /reply`, and `desktop-buddy-watcher.exe` treats the frame as a direct
instruction that bypasses the interaction cooldowns.

`reset_session` is the **New chat** button: it broadcasts a `session_reset`
frame (HTTP twin `POST /session/reset`), dismisses the bubble and returns
`{ok, reason, subscribers}`. It rotates nothing itself — the watcher owns the
session id.

Events emitted to the page: `avatar-state-changed` (full state after every
command/API mutation), `api-status` (bound address or `offline`),
`model-changed`, `rig-pose`, `rig-saved`, `part-patch`, `emote`, `speak`
(including `details`), `speech-clear`, `tts-status` (`{enabled, ready, reason,…}`
after every voice toggle), `avatar-animate` (play/stop a custom clip:
`{action, name, duration_ms, loop}`), `summon-command-bar` (`{source, hotkey}` —
the global hotkey fired; the page opens the bar and focuses its input).

The same changes are also mirrored onto the HTTP event bus (`GET /events`) —
see *Awareness: desktop context & the event stream* for the frame list.

---

## MCP server (`desktop-buddy-mcp`)

`src-tauri/src/bin/desktop-buddy-mcp.rs` builds a **second binary**,
`src-tauri/target/debug/desktop-buddy-mcp.exe`, that exposes the avatar to any MCP
client (Hermes, Claude Desktop, …) over **stdio JSON-RPC 2.0** as thirteen tools.
It is a thin, stateless bridge: each tool call becomes one HTTP request to the
*running* `desktop-buddy.exe`.

```
MCP client ── stdio JSON-RPC ──▶ desktop-buddy-mcp ── HTTP ──▶ 127.0.0.1:8765
```

Base URL: `DESKTOP_BUDDY_API_URL`, default `http://127.0.0.1:8765`.

| MCP tool | Arguments | Forwards to |
| --- | --- | --- |
| `avatar_speak` | `text` (required), `expression?`, `duration_ms?`, `details?` | `POST /speak` |
| `avatar_emote` | `expression` (required), `duration_ms?` | `POST /emote` |
| `avatar_move` | `x`, `y` (required), `look_at?` (`[x, y]`) | `POST /move` |
| `avatar_perch` | `edge` (required), `window_process?`, `offset?` | `POST /perch` |
| `avatar_wave` | — | `POST /wave` |
| `avatar_pose` | `joints` (required; `{boneId: {rotation?, scale?, x?, y?}}`, `null` clears a bone) | `POST /pose` |
| `avatar_get_context` | — | `GET /context` **and** `GET /state` |
| `avatar_get_clipboard` | — | `GET /clipboard` |
| `avatar_set_clipboard` | `text` (required; `""` clears) | `POST /clipboard` |
| `avatar_list_models` | — | `GET /models` |
| `avatar_switch_model` | `name` (required) | `POST /model` |
| `avatar_list_animations` | — | `GET /animations` |
| `avatar_animate` | `name` (required), `loop?` | `POST /animate` |
| `avatar_switch_ai` | `name` (required; an `ai_profiles` key) | `POST /ai-profile` |

`expression` accepts the same six names as the HTTP API (`neutral`, `happy`,
`sad`, `surprised`, `thinking`, `angry`) and is validated locally before any
request is sent.

`avatar_animate` takes a clip name from `avatar_list_animations` and an optional
`loop` override; `"name": "stop"` (or an explicit `null`) stops playback instead
of starting a clip, and `loop: true` together with a stop is rejected locally.

`avatar_get_context` returns a single text block: a one-line human summary, then
JSON with `window_title`, `process_name`, `idle_secs`, `source`, `window_rect`,
`active_profile`, `ai_profiles`, `active_health`,
`visible_windows`, `avatar_state`, `state_source` (`"state"` when the fresh
`/state` read answered, `"context"` when it fell back to the copy embedded in
`/context`) and `recent_events` (the last 15 SSE frames, oldest first). The
summary names the focused app — e.g. `User is in "main.rs - visual-studio-code"
(Code.exe), idle for 12s.` — gives the foreground rect and the visible windows
as `Code.exe@(400, 300) 800x500 | firefox.exe@(0, 0) 1920x40`, says
`perched on a window` while the avatar is perched, and lists the recent events as
`type: hint` pairs.

`avatar_perch` validates `edge` locally against the five borders before sending
anything, so a typo comes back as an `invalid_arguments` tool result listing the
accepted values instead of an HTTP 400. It is the spatial half of "react to what
the user is looking at": `avatar_get_context` says which window is focused and
where it is, then `avatar_perch` stands the avatar on it and the gaze follows.

`avatar_get_clipboard` returns the `GET /clipboard` body as text:
`{ok: true, text, truncated}`. Read it only when the trigger refers to copied
content; it is the user's data.

### Error behaviour

The process never panics and never closes the transport:

| Situation | Result |
| --- | --- |
| avatar app not running / wrong port | `isError: true`, payload `{"ok":false,"kind":"avatar_unreachable","error":"cannot reach the desktop-buddy avatar at …","hint":"Start the desktop app first: …"}` |
| avatar answered 4xx (validation) | `isError: true`, `kind: "avatar_rejected_request"`, message taken from the API's `error` field |
| request exceeded 10 s | `isError: true`, `kind: "avatar_timeout"` |
| bad argument values | `isError: true`, `kind: "invalid_arguments"` (no HTTP call made) |
| unparseable stdin line | JSON-RPC `-32700`, loop continues |
| unknown tool / unsupported method | JSON-RPC `-32602` / `-32601` |
| stdin EOF (client closed) | exit code 0 |

`tools/call` reports *argument* problems as `isError` tool results (so the model
sees the reason and can retry) and only *structural* problems as JSON-RPC
errors.

### Driving it by hand

```bash
cd src-tauri
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
 '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"avatar_get_context","arguments":{}}}' \
 | ./target/debug/desktop-buddy-mcp.exe
```

`python tools/mcp_smoke_test.py` asserts the whole protocol surface against the
real stdio transport — the tool schemas, the JSON-RPC error codes, and both the
"avatar down" (structured `isError`) and "avatar up" (live calls, animations
included) tool paths.

## Design notes

* The page owns the animation; Rust owns the state that clients read. The page
  pushes its position back at ~8 Hz (`report_state`) — that path never emits an
  event, so there is no feedback loop.
* `revision` guards against the page acting on its own echo: it only applies
  remote target/state/expression when the revision is newer than the last one
  it saw.
* Wander behaviour is suppressed for 12–20 s after any manual action
  (`markManual`), so API/UI commands are not clobbered by the idle walk.
* Locks are always released before emitting events (no lock across await, no
  lock held while the UI thread might re-enter), and poisoning is recovered
  with `lock_or_recover` instead of panicking.
