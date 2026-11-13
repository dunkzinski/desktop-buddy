# char-rig — AI desktop avatar

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
cargo build          # dev binary -> target/debug/avatar-desktop.exe
cargo build --release
./target/debug/avatar-desktop.exe
```

Optional: `CHAR_RIG_API_PORT=9000` overrides the API port (default `8765`; if the
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
  run a diagnostic, or anything you script.
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
{ "ok": true, "service": "char-rig avatar api", "version": "0.1.0", "uptime_secs": 42 }
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
than the bone root. It is then clamped to the primary monitor, so a window flush
with the screen edge cannot push the avatar off screen.

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
{ "ok": true, "root": "C:\\hermes\\char-rig\\models", "models": [
  { "name": "demo-blob", "label": "Demo Blob (sprite parts)", "path": "...",
    "bones": 7, "parts": 10, "has_rig_override": false, "error": null }
] }
```

Broken folders appear with `"error": "<reason>"` instead of failing the request.

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
    { "name": "wave", "duration_ms": 1400.0, "loop": false, "keyframes": 6 }
  ] }
```

`duration_ms` is always `> 0`, `keyframes` is the authored keyframe count, and a
model with no `animations` block returns `"animations": []`.

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

The supervised `char-rig-watcher.exe` — the same switch the control panel's
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
  "binary_path": "C:\dev\char-rig\src-tauri\target\debug\char-rig-watcher.exe",
  "rules_path": "C:\dev\char-rig\watcher_rules.json",
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
array (or an unreadable file) the app falls back to three built-in chips
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
`ai-profile-changed` Tauri event. The running `char-rig-watcher.exe` re-reads the
file when it sees the frame, so the **next** trigger uses the new backend — no
restart. An unknown name is a **400** that lists the valid profiles.

Per-trigger overrides: `app_rules[].profile` (a window rule) and
`quick_tasks[].profile` (a command-bar chip, forwarded in the `user_reply`
frame's `profile`). Both fall back to `active_profile` when absent.

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
from physical pixels to **CSS pixels** with the primary monitor's DPI scale, so
it lines up with the overlay's coordinate space and with the avatar's own
position. On other platforms the values are empty/zero (`"window_rect": null`,
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
  "visible_windows": [
    { "title": "main.rs - visual-studio-code", "process_name": "Code.exe",
      "rect": { "x": 400.0, "y": 300.0, "width": 800.0, "height": 500.0 } },
    { "title": "Inbox - Firefox", "process_name": "firefox.exe",
      "rect": { "x": 1200.0, "y": 60.0, "width": 700.0, "height": 900.0 } }
  ],
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
| `process_name` | executable that owns the foreground window, e.g. `"Code.exe"` (`""` when it cannot be read, and always `""` off Windows) |
| `idle_secs` | seconds since the last keyboard/mouse input anywhere on the machine |
| `window_rect` | bounds of the foreground window in CSS px — `{x, y, width, height}`, or `null` when nothing is focused / off Windows. This is the rect `POST /perch` uses when no `window_process` is given |
| `visible_windows` | up to **5** front-most top-level windows (`{title, process_name, rect}`), skipping minimized, cloaked, untitled and the avatar's own window — what an agent can perch on by process name |
| `avatar_state` | identical shape to `GET /state` |
| `recent_events` | the last **15** SSE frames, oldest first, in the same `{seq, type, ts_ms, payload}` envelope `GET /events` pushes — the agent's short-term memory of what just happened |
| `source` | `win32` when the Win32 probes ran, `unsupported` elsewhere |

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
model itself. `char-rig-watcher.exe` picks the frame up and wakes Hermes under
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
`GET /events` and dismisses the visible bubble, and `char-rig-watcher.exe`
rotates its `{session}` id when it sees the frame (it logs
`session reset (api): char-rig-<ts> -> char-rig-<ts>-2`). The next trigger then
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
| `watcher` | the supervised `char-rig-watcher.exe` starts, stops, or writes a line | full status `{ok, enabled, running, pid, binary_path, rules_path, logs}` on a lifecycle change, or `{kind: "log", line, enabled, running, pid}` for one log line |

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
commands. Arguments are snake_case (`rename_all = "snake_case"`). All 34, in the
order they are registered (ARCHITECTURE.md §6 has the full purpose table):

**State, window & interaction** — `get_avatar_state`, `get_runtime_info`,
`set_avatar_position(x, y)`, `set_avatar_expression(expression)`,
`set_avatar_state(new_state)`, `trigger_wave`, `report_state(position,
target_position, state, expression, joints?, talking?, speech?)`,
`update_hit_regions(regions)`, `set_click_through(enabled)`,
`notify_interaction(kind, x, y)`, `notify_reply(text)`, `reset_session`,
`close_window`, `minimize_window`.

**Models, rigs & parts** — `list_models`, `get_model`, `load_model(name)`,
`pick_import_file`, `import_asset(path, mode?, name?, tag?)`,
`patch_part(target, patch)`, `set_mouth(state)`, `get_parts`, `save_rig(rig)`,
`reset_rig`, `set_pose(joints)`, `clear_pose`, `get_joints`.

**Expressions & speech** — `emote(expression, duration_ms?, revert?)`,
`speak(text, expression?, duration_ms?, char_ms?, details?)`, `set_clipboard(text)`
(the bubble's **Copy** button; same Win32 writer as `POST /clipboard`),
`clear_speech`, `get_speech`.

**Watcher** — `get_watcher_status`, `set_watcher_enabled(enabled)`. The switch
and log tail behind the control panel's **Watcher** row and the pop-out console;
`set_watcher_enabled` returns the same status object as `GET /watcher`.

**AI brain** — `get_ai_profiles` (the same object as `GET /ai-profiles`, health
probe included) and `set_ai_profile(name)` (the **AI Brain** dropdown; persists,
probes and broadcasts exactly like `POST /ai-profile`, and returns the new
status; an unknown name is an error).

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
`POST /reply`, and `char-rig-watcher.exe` treats the frame as a direct
instruction that bypasses the interaction cooldowns.

`reset_session` is the **New chat** button: it broadcasts a `session_reset`
frame (HTTP twin `POST /session/reset`), dismisses the bubble and returns
`{ok, reason, subscribers}`. It rotates nothing itself — the watcher owns the
session id.

Events emitted to the page: `avatar-state-changed` (full state after every
command/API mutation), `api-status` (bound address or `offline`),
`model-changed`, `rig-pose`, `rig-saved`, `part-patch`, `emote`, `speak`
(including `details`), `speech-clear`, `avatar-animate` (play/stop a custom clip:
`{action, name, duration_ms, loop}`), `summon-command-bar` (`{source, hotkey}` —
the global hotkey fired; the page opens the bar and focuses its input).

The same changes are also mirrored onto the HTTP event bus (`GET /events`) —
see *Awareness: desktop context & the event stream* for the frame list.

---

## MCP server (`char-rig-mcp`)

`src-tauri/src/bin/char-rig-mcp.rs` builds a **second binary**,
`src-tauri/target/debug/char-rig-mcp.exe`, that exposes the avatar to any MCP
client (Hermes, Claude Desktop, …) over **stdio JSON-RPC 2.0** as thirteen tools.
It is a thin, stateless bridge: each tool call becomes one HTTP request to the
*running* `avatar-desktop.exe`.

```
MCP client ── stdio JSON-RPC ──▶ char-rig-mcp ── HTTP ──▶ 127.0.0.1:8765
```

Base URL: `CHAR_RIG_API_URL`, default `http://127.0.0.1:8765`.

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
| avatar app not running / wrong port | `isError: true`, payload `{"ok":false,"kind":"avatar_unreachable","error":"cannot reach the char-rig avatar at …","hint":"Start the desktop app first: …"}` |
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
 | ./target/debug/char-rig-mcp.exe
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
