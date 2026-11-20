# CONFIGURATION.md — `watcher_rules.json`, the watcher CLI, and `SOUL.md`

`watcher_rules.json` is the one hand-editable file behind the whole runtime. It
is read by three parties, each of which understands only its own keys:

| Reader | Owns |
|---|---|
| `desktop-buddy.exe` | `hotkey`, `tts`, `stt`, the command bar's `quick_tasks`, `ai_profiles` / `active_profile` for the panel and health probes, and the settings editor itself |
| `desktop-buddy-watcher.exe` | `interactions`, `context`, `app_rules`, `alerts`, `quick_tasks`, the session keys, `active_profile`, `ai_profiles`, `ack_emote`, `hermes_cmd`, `hermes_timeout_secs`, `webhook_url`, `vision`, `survival` (routing only), `soul_path` |
| the survival guest | the rest of the `survival` block (`metabolism`, `cravings`, `fishing`, `inventory`, …) — see [PLUGINS.md](PLUGINS.md#6-the-survival-guest) |

Every block is `#[serde(default)]`, so a missing key is the built-in default and
an unknown key is **silently ignored**. `tools/verify_docs.py` rejects unknown
top-level keys in the shipped file and pins the documented defaults against the
constants, which is the only guard against "looks configured, runs on defaults".

---

## 1. Where the file is found

**The desktop app** (`watcher_rules_candidates`) tries, in order:

1. `./watcher_rules.json` (the process working directory);
2. `watcher_rules.json` beside `desktop-buddy.exe`;
3. every parent directory of the executable, up to the repo root;
4. the repo root itself (next to `models/`).

**The watcher** tries `--rules <path>` when given, otherwise
`./watcher_rules.json`, then `watcher_rules.json` beside the executable. It
prints `[watcher] rules loaded from …` on startup; a file that fails to parse
prints the reason and falls back to built-in defaults rather than dying.

Both processes therefore land on the same checkout file when the app is launched
from the repo, which is what keeps a settings save and a dispatch consistent.

**`POST /config` never re-serialises the file.** `settings.rs` edits the byte
span of the value it was asked to change (or inserts the key after the opening
brace) and re-parses the result before writing, so hand-written formatting,
key order and one-liners survive. A profile switch does not rewrite the file.

---

## 2. Environment variables

| Variable | Used by | Effect |
|---|---|---|
| `DESKTOP_BUDDY_API_URL` | watcher, MCP bridge | base URL (default `http://127.0.0.1:8765`) |
| `DESKTOP_BUDDY_API_PORT` | app | port to bind instead of `8765` |
| `DESKTOP_BUDDY_HERMES_CMD` | watcher | default `hermes_cmd` when the flag and file are silent |
| `DESKTOP_BUDDY_NO_WATCHER` | app | `1` = do not spawn/supervise `desktop-buddy-watcher.exe` |
| `DESKTOP_BUDDY_MODELS` | app | override the `models/` root |
| `DESKTOP_BUDDY_PLUGINS_DIR` | app | override the `plugins/` directory |
| `DESKTOP_BUDDY_SOUL` | app + watcher | absolute `SOUL.md` path, ahead of `soul_path` |
| `DESKTOP_BUDDY_STT_WAV` | `stt` test only | transcribe a WAV and exit |

---

## 3. `desktop-buddy-watcher` command line

The app spawns the watcher itself; these flags are for running it by hand
(`DESKTOP_BUDDY_NO_WATCHER=1`).

| Flag | Default | Meaning |
|---|---|---|
| `--api-url <URL>` | `http://127.0.0.1:8765` | avatar API base |
| `--rules <PATH>` | `./watcher_rules.json`, then beside the exe | rules file |
| `--context-interval <SECS>` | `20` | how often `GET /context` is polled (must be ≥ 1) |
| `--trigger-single-click` | off | also fire on plain `click` interactions |
| `--ack-emote <EXPR>` | `thinking` | expression shown the moment a trigger fires |
| `--no-ack-emote` | — | same as `--ack-emote ""`: acknowledge with no face |
| `--hermes-cmd <CMD>` | `$DESKTOP_BUDDY_HERMES_CMD` | command spawned with the prompt appended |
| `--hermes-timeout <SECS>` | `120` | kill the spawned command after this long (must be ≥ 1) |
| `--webhook-url <URL>` | — | POST the trigger payload to this endpoint |
| `--dry-run` | off | print trigger payloads on stdout, call nothing |
| `-h`, `--help` | — | usage; stops startup |

`--help` is the authoritative text — `usage()` in `desktop-buddy-watcher.rs` is the
string it prints, and it explains the dispatch contract:

* the prompt is appended as the **last argv entry** (or substituted for
  `{prompt}`); the command is split on whitespace with quote grouping and **no
  shell** is involved, so pipes and redirection need a script;
* the child runs headless (`stdin` is `/dev/null`);
* **one dispatch is in flight at a time**. A `user_reply` or `alert` that arrives
  mid-turn waits in a one-slot queue (newest wins) and is dispatched when the
  turn ends; routine `interaction`, window-title and idle triggers are dropped
  while busy.

A dispatch is delivered in order: `--dry-run` print → ack emote → `webhook_url`
POST → the AI profile.

### What can wake the agent

Every routed frame carries an `event_type`; this is the full set:

| `event_type` | Fires on | Gated by |
|---|---|---|
| `interaction` | a double-click, a drag end, or (with `--trigger-single-click`) a plain click | `interactions.*`, `interactions.cooldown_secs` |
| `window_title` | the focused window's title matched `context.window_keywords`, or an `app_rules` entry matched | `context.cooldown_secs`, `app_rules` |
| `idle_return` | idle time dropped back under `context.idle_threshold_secs` | `context.cooldown_secs` |
| `user_reply` | a prompt from the command bar (`POST /reply`) | none — always runs, and queues while busy |
| `alert` | `POST /alert` (Suricata, CI, anything with a webhook) | `alerts.enabled`, `alerts.cooldown_secs` |
| `fish_caught` | a landed catch (from `fishing-result` with `caught: true`) | its own fishing bucket |
| `fish_lost` | an escape or a full bag (`fishing-result` with `caught: false`) | its own fishing bucket |

Plugin frames (`survival-*`, `fishing-bite`, `fish-hooked`) route through the
same table; see [PLUGINS.md §6](PLUGINS.md#events) for their payloads.

---

## 4. Top-level schema

| Key | Type | Default | Read by |
|---|---|---|---|
| `interactions` | object | see §4.1 | watcher |
| `context` | object | see §4.2 | watcher |
| `app_rules` | array | `[]` | watcher |
| `alerts` | object | `{enabled: true, cooldown_secs: 60}` | watcher |
| `quick_tasks` | array (≤ 6) | 4 built-in chips | app (command bar) |
| `max_session_turns` | integer | absent = disabled | watcher |
| `session_idle_reset_secs` | integer | absent = disabled | watcher |
| `session_base` | string | `desktop-buddy` | watcher |
| `active_profile` | string | `hermes` | watcher, app |
| `ai_profiles` | object | `{}` | watcher, app |
| `ack_emote` | string | `thinking` | watcher |
| `hermes_cmd` | string | `$DESKTOP_BUDDY_HERMES_CMD` | watcher |
| `hermes_timeout_secs` | integer | `120` | watcher |
| `webhook_url` | string or `null` | `null` | watcher |
| `vision` | object | see §4.6 | watcher |
| `survival` | object | see §4.8 | watcher + guest |
| `hotkey` | string | `Alt+Space` | app |
| `tts` | object | see §4.9 | app |
| `stt` | object | see §4.10 | app |
| `discovery_ports` | array of ports | `[8001, 9931, 9932, 11434, 8080, 1234]` | app |
| `discovery_hosts` | array of hosts | `["127.0.0.1"]` | app |
| `soul_path` | string | absent = beside this file | app + watcher |

### 4.1 `interactions`

```json
{ "double_click": true, "drag_end": true, "click": false, "cooldown_secs": 15 }
```

Which pointer gestures on the avatar are worth a turn. `click` defaults to
`false` because a single click is usually "move out of the way";
`--trigger-single-click` turns it on for one run. `cooldown_secs` is the shared
budget, so a drag cannot fire twice.

### 4.2 `context`

```json
{
  "window_keywords": ["error", "failed", "exception"],
  "idle_threshold_secs": 900,
  "cooldown_secs": 300
}
```

* `window_keywords` — case-insensitive substrings that make a window title worth
  a trigger. It fires only when the focused window **changes**, so a live title
  cannot machine-gun the model.
* `idle_threshold_secs` — idle time that counts as "away"; dropping below it is a
  `idle_return` trigger.
* `cooldown_secs` — budget for window/title/idle triggers.

### 4.3 `app_rules`

Per-application overrides, evaluated when the focused window changes:

```json
{
  "process": "Code.exe",
  "title_contains": "error",
  "animation": "surprised",
  "prompt_hint": "This is a compile error: name the file…",
  "profile": "fast-local"
}
```

| Field | Meaning |
|---|---|
| `process` | executable name from `GET /context`'s `process_name` |
| `title_contains` | case-insensitive substring of the window title |
| `animation` | expression name or a clip name, used as the ack animation |
| `prompt_hint` | extra context appended to the prompt |
| `profile` | dispatch this app's triggers to that profile instead of `active_profile` |

Both matchers are optional but **not both** — a rule with neither matches every
window. `verify_docs.py` rejects that, and rejects an `animation` that is
neither an expression nor a clip documented in `ARCHITECTURE.md`.

### 4.4 `alerts`

`{ "enabled": true, "cooldown_secs": 60 }` — the switch and budget for `alert`
frames from `POST /alert`. `enabled: false` lets them pass without a turn.

### 4.5 `quick_tasks`

Chips for the command bar (`Alt+Space`). The **app** reads them (the watcher only
validates the shape), caps the list at 6, and skips an entry with a blank `label`
or `prompt`. A chip may pin its brain and/or ask for a screenshot:

```json
{
  "label": "What's on my screen?",
  "prompt": "Look at my screen and tell me what I am working on…",
  "profile": "vision",
  "include_screen": true
}
```

`profile` is forwarded with the `user_reply` frame; `include_screen` is the
explicit screen-vision request. With no `quick_tasks` array the app falls back to
its four built-in chips.

### 4.6 `vision`

Local screen vision. The capture is in memory only — nothing touches disk.

```json
{
  "enabled": true,
  "profile_name": "vision",
  "capture_mode": "active_window",
  "max_image_dim": 1024,
  "jpeg_quality": 85,
  "trigger_phrases": ["what's on my screen", "look at my screen"]
}
```

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | `false` turns `include_screen` and phrase matches into a normal text answer |
| `profile_name` | `vision` | must name an `openai_compat` profile — the request is OpenAI-shaped |
| `capture_mode` | `active_window` | or `full_screen` |
| `max_image_dim` | `1024` | longest edge, aspect ratio preserved, ≥ 64 |
| `jpeg_quality` | `85` | 1–100 |
| `trigger_phrases` | 7 built-ins | case-insensitive, apostrophe-insensitive, non-empty |

A phrase match or an explicit `include_screen` makes the watcher capture and send
the image instead of text. `profile_name` pointing at a missing or `hermes_cli`
profile is refused at save time.

### 4.7 Sessions and dispatch

```json
{
  "max_session_turns": 15,
  "session_idle_reset_secs": 1800,
  "session_base": "desktop-buddy",
  "hermes_cmd": "hermes chat --continue {session} --create-if-missing -Q -q",
  "hermes_timeout_secs": 120,
  "webhook_url": null
}
```

* `max_session_turns` — rotate to a fresh `{session}` after this many dispatched
  turns. Absent or `0` disables the rule.
* `session_idle_reset_secs` — rotate after this long without a dispatch. Absent
  or `0` disables it.
* `session_base` — prefix for generated session ids.
* `hermes_cmd` — the command template. `{session}` and `{prompt}` are
  substituted; **a template without `{session}` cannot be rotated**, so
  `POST /session/reset` would only reset the watcher's own state and
  `verify_docs.py` refuses it.
* `hermes_timeout_secs` — longest `hermes_cmd` run.
* `webhook_url` — receives the trigger payload (the same JSON `--dry-run`
  prints) before the AI call.

### 4.8 `survival`

One JSON block, two readers: `enabled` and `cooldown_secs` are the watcher's
routing switch and budget for every `survival-*` frame (one shared bucket — a
feed that crosses a tier *and* settles a craving is one reaction, not two);
everything else belongs to the guest. The full schema is in
[PLUGINS.md §6](PLUGINS.md#config-block).

### 4.9 `tts` — Piper

```json
{
  "enabled": true,
  "piper_exe_path": "C:\\hermes\\tools\\piper\\piper.exe",
  "model_path": "C:\\hermes\\tools\\piper\\voices\\en_GB-southern_english_female-low.onnx",
  "espeak_data_path": "C:\\hermes\\tools\\piper\\espeak-ng-data",
  "speaker_id": null,
  "length_scale": 1.0
}
```

`speaker_id` is a non-negative integer or `null` (multi-speaker voices);
`length_scale` is `0.25`–`4.0` (`>1` is slower). A wrong `piper_exe_path` or
`model_path` makes the avatar silently mute, so `GET /config` reports each path
with an exists/not-exists tick.

### 4.10 `stt` — push-to-talk

```json
{
  "enabled": true,
  "hotkey": "Alt+V",
  "whisper_exe_path": "C:\\hermes\\tools\\whisper.cpp\\build\\bin\\Release\\whisper-cli.exe",
  "whisper_model_path": "C:\\hermes\\tools\\whisper.cpp\\ggml-base.en.bin",
  "silero_vad_model_path": "C:\\hermes\\tools\\whisper.cpp\\ggml-silero-v6.2.0.bin",
  "vad_threshold": 0.5,
  "language": "en",
  "min_speech_ms": 250
}
```

`vad_threshold` is 0–1 (higher = stricter about what counts as speech);
`min_speech_ms` drops shorter blips. Holding the key interrupts active TTS first,
so the mic never transcribes the avatar.

### 4.11 `hotkey` — command bar

```json
{ "hotkey": "Alt+Space" }
```

Grammar: `Modifier+…+Key`, case-insensitive, with `Alt`, `Ctrl`/`Control`,
`Shift` and `Win`/`Super`/`Meta` modifiers and a single `A`–`Z`, `0`–`9`,
`F1`–`F24`, or a named key (`Space`, `Enter`/`Return`, `Tab`, `Esc`/`Escape`,
`Backspace`, `Left`, `Up`, `Right`, `Down`). A bare key with no modifier is
legal. Junk (`Alt+`, `Hyper+A`, `Alt+AB`, `F25`) is rejected and falls back to
`Alt+Space` instead of panicking. The hotkey is registered **once at startup**,
so a change needs an app restart; a missing/blank/broken file keeps the default.

### 4.12 `soul_path`

Optional absolute path to the persona file. Absent means "beside
`watcher_rules.json`" (see §6). The panel's SOUL tab edits whatever this
resolves to and never rewrites the key.

---

## 5. `ai_profiles` and `active_profile`

Inference targets, keyed by the name the panel and the rules file use. Entries
are tagged by `kind`:

```json
{
  "active_profile": "hermes",
  "ai_profiles": {
    "local-fast": {
      "kind": "openai_compat",
      "label": "llama.cpp · lfm3-1.7b",
      "endpoint": "http://127.0.0.1:8080/v1",
      "model": "lfm3-1.7b",
      "max_tokens": 512,
      "timeout_secs": 60,
      "system_prompt": "You are a tiny, fast desktop avatar assistant…"
    },
    "hermes": {
      "kind": "hermes_cli",
      "label": "Hermes CLI",
      "cmd": "hermes chat --continue {session} --create-if-missing -Q -q",
      "timeout_secs": 120
    }
  }
}
```

| Kind | Required | Optional |
|---|---|---|
| `openai_compat` | `endpoint`, `model` | `label`, `api_key`, `max_tokens`, `timeout_secs`, `system_prompt` |
| `hermes_cli` | `cmd` | `label`, `timeout_secs` |

* `endpoint` is a base URL (`http://127.0.0.1:8080`) **or** the full
  `/v1/chat/completions` URL; `api_key` is sent as `Authorization: Bearer …`.
* `active_profile` names the default target. Absent (or naming a profile that
  does not exist) means `hermes` when configured, else the first entry.
* `timeout_secs` falls back to `60` for `openai_compat` and `hermes_timeout_secs`
  for `hermes_cli`.
* **At least one `hermes_cli` profile is required.** Without it an
  `openai_compat` model could never escalate a multi-step task, and New chat
  would not reach an agent — `verify_docs.py` refuses the file.

### The two backends behave differently

| | `openai_compat` | `hermes_cli` |
|---|---|---|
| Tools | none — it acts through the reply JSON (`speech`, `expression`, `animate`, `wave`, `escalate`) | real agent turn with MCP tools |
| Prompt | `brief()`: the situation, plus a fresh `GET /context`; `GET /clipboard` **only** when the prompt mentions the clipboard | `prompt()`: `brief()` + the headless directive that names the tools to call |
| Escalation | `"escalate": true` in the reply re-runs the turn on the `hermes_cli` profile | — |
| Health | `GET /health`, then `/v1/models` (800 ms cap) | `reachable` = whether `hermes` resolves on `PATH` |

Never hand a `brief()`-less prompt to an HTTP profile: the directive names
`avatar_get_clipboard`, so every trigger would look like a clipboard question and
a tool-less model would be told to call tools it does not have.

### Discovery

`discovery_ports` and `discovery_hosts` are the probe list behind **Settings ⚙ →
AI brain → Discover**. Ports are validated as 1–65535 with no duplicates; hosts
are a bare hostname, an IP or `host:port` — no scheme, no path. The app adds any
port already used by an `ai_profiles` endpoint to the list it shows, so a
configured profile is always discoverable. `POST /ai-discover` only **reports**;
importing a hit is the modal filling a draft and `POST /config` writing it.

---

## 6. `SOUL.md` — the persona

`SOUL.md` is the avatar's voice: who it is, how it talks, what it refuses to do.
It is deliberately **not** config — no paths, no profiles, no secrets, no tool
signatures. Those live in `watcher_rules.json`.

### Resolution

Both processes call the same resolver (`src-tauri/src/soul.rs`):

1. `$DESKTOP_BUDDY_SOUL`, if set and non-empty;
2. `soul_path` from `watcher_rules.json`;
3. the rules file's own directory (`SOUL.md` beside it);
4. the app config directory.

### Rules that make it safe

* **Read on every turn.** Nothing caches it — an edit takes effect on the next
  dispatch, no restart.
* **Seed once, never clobber.** A missing file is created from `DEFAULT_SOUL`; an
  existing file, even an empty one, is left alone. `SOUL.md` is gitignored for
  exactly this reason: it belongs to the user.
* **Degrade, never fail.** An empty, unreadable or oversized file falls back to
  `DEFAULT_SOUL`, so the avatar always has a persona. `MAX_SOUL_BYTES` is
  64 KiB; the API refuses a larger write rather than truncating it.

### Injection order

`SOUL.md` is **slot #1** of the `openai_compat` system message, ahead of
everything else:

1. `[SOUL.md - your persona. …]` + the file's text;
2. the role line, or the profile's own `system_prompt`;
3. `AI_REPLY_CONTRACT` (the JSON action/expression contract);
4. the creature's runtime state (stage, archetype, attribute blend) — flavour
   only; the persona still owns the voice.

The `hermes_cli` path carries the persona in the prompt the same way. The order
is a contract the tests pin: the persona never drifts into the user message.

---

## 7. `POST /config` — the settings editor's write path

`GET /config` returns the effective settings plus `paths` (exists/not-exists per
tool) and `warnings` (stray keys per block). `POST /config` takes a **partial**
patch and returns `{ok, settings, applied, restart_required}`.

Sections it accepts:

| Section | Owner | Restart on change |
|---|---|---|
| `tts` | app | any key except `enabled` |
| `stt` | app | any key except `enabled` |
| `vision` | app | live |
| `ai` | app | live (emits `ai_profile`) |
| any plugin-owned block (e.g. `survival`) | the guest's `validate`/`apply` | as the guest reports |

A patch for a block nobody owns is a 400 naming the known sections. The `ai`
verbs (`ai.create`, `ai.rename`, `ai.remove`, `ai.profiles`) are resolved against
the map that **results** from the patch, in the order remove → edit → rename →
create, so one save can switch to a profile it creates in the same request.
Removal is refused for the last profile, the active one and the vision one, and
`vision.profile_name` must resolve in the final map.

`GET /config` also serves `settings.ai.default_system_prompt`, so the panel never
keeps a second copy of the default prompt.

---

## 8. Drift checks

`python tools/verify_docs.py` is the gate. For this file's subject matter it
checks:

* every top-level key in the shipped `watcher_rules.json` is known (an unknown
  key is silently ignored at runtime, so it would be invisible otherwise);
* the shipped defaults agree with the constants — `interactions.cooldown_secs`,
  `context.cooldown_secs`, `context.idle_threshold_secs`,
  `context.window_keywords`, `alerts.cooldown_secs`, `ack_emote`,
  `hermes_timeout_secs`, `tts.*`, `stt.*`, `vision.*`, `survival.*`;
* `discovery_ports` / `discovery_hosts` / `quick_tasks` / `ai_profiles` have a
  valid shape (ports in range, no duplicates, ≤ 6 chips, a resolvable
  `active_profile`, a `hermes_cli` profile, required fields per `kind`);
* `hermes_cmd` still contains `{session}`;
* every `DEFAULT_*` constant named in `ARCHITECTURE.md` still exists;
* every watcher flag is documented, and the default ack emote is named.

---

## 9. A minimal file

```json
{
  "interactions": { "double_click": true, "drag_end": true, "click": false, "cooldown_secs": 15 },
  "context": { "window_keywords": ["error", "failed"], "idle_threshold_secs": 900, "cooldown_secs": 300 },
  "ack_emote": "thinking",
  "hotkey": "Alt+Space",
  "active_profile": "hermes",
  "ai_profiles": {
    "hermes": { "kind": "hermes_cli", "cmd": "hermes chat --continue {session} --create-if-missing -Q -q" }
  }
}
```

That is a working configuration: no TTS/STT (silent avatar), no vision, no
plugins configured (the survival guest runs on its own defaults), and every
trigger routed to the Hermes CLI.
