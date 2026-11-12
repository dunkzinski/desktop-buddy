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
  "revision": 3
}
```

* `state` ∈ `idle | walking | waving`
* `expression` ∈ `neutral | happy | sad | surprised | thinking | angry`
* `position` is the live animated position in CSS pixels (window-relative, and
  the window covers the primary monitor)
* `revision` increments on every externally requested change — clients can poll
  it to detect "something happened"

### `POST /state`

```json
{ "state": "idle", "expression": "happy" }   // expression optional
```

### `POST /move`

```json
{ "x": 1200, "y": 700 }
```

Sets the walk target (`state` becomes `walking`); the page animates the walk and
reports the live position back.

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

* manifest imports copy the folder into `models/<slug>/` and load it
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
  "logs": ["[22:23:29] started … (pid 14320, rules …watcher_rules.json)", "…"]
}
```

`logs` is a **bounded ring buffer** of the child's stdout+stderr: timestamped
`[HH:MM:SS]` lines, newest last, capped at **200**.

```bash
curl -s -X POST localhost:8765/watcher -H 'content-type: application/json'      -d '{"enabled": false}'      # kill + reap; the supervisor parks
curl -s -X POST localhost:8765/watcher -H 'content-type: application/json'      -d '{"enabled": true}'       # spawn it again, no app restart
```

Both return the same status object as `GET /watcher`. Every start, stop and log
line also goes out as a `watcher` frame on `GET /events` and as the
`watcher-status` Tauri event, so the page never has to poll.

### `POST /shutdown`

Closes the app (window + API) after flushing the response.

---

## Awareness: desktop context & the event stream

These two endpoints exist so an agent can stop polling: one asks "what is the
user doing?", the other pushes "what did the avatar do?".

### `GET /context`

A cheap snapshot of the user's desktop. On Windows it reads
`GetForegroundWindow` + `GetWindowTextW` for the focused window title and
`GetLastInputInfo` + `GetTickCount64` for machine-wide idle time; on other
platforms it returns empty/zero values and says so in `source`.

```bash
curl -s localhost:8765/context
```

```json
{
  "ok": true,
  "window_title": "Visual Studio Code",
  "idle_secs": 12,
  "avatar_state": {
    "position": [700.0, 500.0],
    "target_position": [700.0, 500.0],
    "state": "idle",
    "expression": "neutral",
    "revision": 7,
    "talking": false
  },
  "source": "win32"
}
```

| Key | Meaning |
| --- | --- |
| `window_title` | title of the foreground window (`""` when there is none) |
| `idle_secs` | seconds since the last keyboard/mouse input anywhere on the machine |
| `avatar_state` | identical shape to `GET /state` |
| `source` | `win32` when the Win32 probes ran, `unsupported` elsewhere |

The handler queries the OS directly and never touches the avatar state machine,
so it is safe to poll at any rate.

### `GET /events` (Server-Sent Events)

A push stream of everything the avatar does: state transitions, speech, model
switches, rig saves, pose updates, part patches, custom animations, and pointer
interactions.

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
| `speech` | `POST /speak` | `{text, expression, duration_ms, char_ms}` |
| `speech_cleared` | `POST /clear_speech` (or the bubble dismissing itself) | `{}` |
| `model` | model loaded, switched, imported or reset | `{name, label, rig_path, is_override, bones, images, reason}` with `reason` ∈ `load` \| `switch` \| `import` \| `reset` |
| `rig_saved` | `POST /rig`, or the editor's **Save rig** | `{rig_path}` |
| `pose` | `POST /pose` / `DELETE /pose` | `{pose}` |
| `part_patch` | `POST /part`, `POST /mouth` | `{target, patch}` |
| `animate` | `POST /animate`, `POST /animate/stop` | `{action, name, duration_ms, loop, keyframes}` with `action` ∈ `play` \| `stop` |
| `interaction` | the user clicks the avatar (§ Avatar interaction) | `{kind, x, y, state, expression, position}` with `kind` ∈ `click` \| `double_click` \| `drag_end` |
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
commands. Arguments are snake_case (`rename_all = "snake_case"`). All 31, in the
order they are registered (ARCHITECTURE.md §6 has the full purpose table):

**State, window & interaction** — `get_avatar_state`, `get_runtime_info`,
`set_avatar_position(x, y)`, `set_avatar_expression(expression)`,
`set_avatar_state(new_state)`, `trigger_wave`, `report_state(position,
target_position, state, expression, joints?, talking?, speech?)`,
`update_hit_regions(regions)`, `set_click_through(enabled)`,
`notify_interaction(kind, x, y)`, `close_window`, `minimize_window`.

**Models, rigs & parts** — `list_models`, `get_model`, `load_model(name)`,
`pick_import_file`, `import_asset(path, mode?, name?, tag?)`,
`patch_part(target, patch)`, `set_mouth(state)`, `get_parts`, `save_rig(rig)`,
`reset_rig`, `set_pose(joints)`, `clear_pose`, `get_joints`.

**Expressions & speech** — `emote(expression, duration_ms?, revert?)`,
`speak(text, expression?, duration_ms?, char_ms?)`, `clear_speech`,
`get_speech`.

**Watcher** — `get_watcher_status`, `set_watcher_enabled(enabled)`. The switch
and log tail behind the control panel's **Watcher** row and the pop-out console;
`set_watcher_enabled` returns the same status object as `GET /watcher`.

`notify_interaction` takes `kind` ∈ `click | double_click | drag_end` with `x`,
`y` in CSS pixels relative to the window, and returns
`{ok, kind, subscribers}` where `subscribers` is how many `GET /events` clients
received the frame. It is a pure notification — unlike every other command it
does not change `AvatarState`.

Events emitted to the page: `avatar-state-changed` (full state after every
command/API mutation), `api-status` (bound address or `offline`),
`model-changed`, `rig-pose`, `rig-saved`, `part-patch`, `emote`, `speak`,
`speech-clear`, `avatar-animate` (play/stop a custom clip: `{action, name,
duration_ms, loop}`).

The same changes are also mirrored onto the HTTP event bus (`GET /events`) —
see *Awareness: desktop context & the event stream* for the frame list.

---

## MCP server (`char-rig-mcp`)

`src-tauri/src/bin/char-rig-mcp.rs` builds a **second binary**,
`src-tauri/target/debug/char-rig-mcp.exe`, that exposes the avatar to any MCP
client (Hermes, Claude Desktop, …) over **stdio JSON-RPC 2.0** as ten tools. It
is a thin, stateless bridge: each tool call becomes one HTTP request to the
*running* `avatar-desktop.exe`.

```
MCP client ── stdio JSON-RPC ──▶ char-rig-mcp ── HTTP ──▶ 127.0.0.1:8765
```

Base URL: `CHAR_RIG_API_URL`, default `http://127.0.0.1:8765`.

| MCP tool | Arguments | Forwards to |
| --- | --- | --- |
| `avatar_speak` | `text` (required), `expression?`, `duration_ms?` | `POST /speak` |
| `avatar_emote` | `expression` (required), `duration_ms?` | `POST /emote` |
| `avatar_move` | `x`, `y` (required) | `POST /move` |
| `avatar_wave` | — | `POST /wave` |
| `avatar_pose` | `joints` (required; `{boneId: {rotation?, scale?, x?, y?}}`, `null` clears a bone) | `POST /pose` |
| `avatar_get_context` | — | `GET /context` **and** `GET /state` |
| `avatar_list_models` | — | `GET /models` |
| `avatar_switch_model` | `name` (required) | `POST /model` |
| `avatar_list_animations` | — | `GET /animations` |
| `avatar_animate` | `name` (required), `loop?` | `POST /animate` |

`expression` accepts the same six names as the HTTP API (`neutral`, `happy`,
`sad`, `surprised`, `thinking`, `angry`) and is validated locally before any
request is sent.

`avatar_animate` takes a clip name from `avatar_list_animations` and an optional
`loop` override; `"name": "stop"` (or an explicit `null`) stops playback instead
of starting a clip, and `loop: true` together with a stop is rejected locally.

`avatar_get_context` returns a single text block: a one-line human summary, then
JSON with `window_title`, `idle_secs`, `source`, `avatar_state` and
`state_source` (`"state"` when the fresh `/state` read answered, `"context"`
when it fell back to the copy embedded in `/context`).

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
