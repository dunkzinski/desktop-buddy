# char-rig v0.1.0 — first public snapshot

A local-first desktop companion: a skeletal 2D avatar that lives on a
transparent, click-through overlay across your virtual desktop, talks through
local Piper TTS, listens through local Whisper, tracks your cursor with its
pupils, and exposes an HTTP/SSE/MCP API so a local model or an agent can drive
it.

This is the **first tagged snapshot**. It is a working prototype, not a
finished product — the honest rough edges are listed at the bottom. Everything
below is built from source; no installers are attached to this release (see
*Build from source* for why).

Dated: 2026-11-19 · first tag on `main`

---

## Highlights

### The overlay and the rig

* A transparent always-on-top window spanning the **whole virtual desktop**, with
  click-through that is genuinely click-through — the page reports its own hit
  regions (`GET /hit-regions`), and everything else passes the click to the
  desktop behind it.
* `model.json` skeletal rigs (`char-rig/model@1`): bones, sprites, expression
  bindings and keyframe clips. **15 hand-authored rigs** ship in `models/`.
* **In-app rig editor** (`R`): drag joints, rotate bone tips, scale parts with
  `Shift+Wheel`, bind expression PNGs, scrub keyframe clips, and import a
  standalone image as a new character. Saving writes `models/<name>/rig.json`
  directly.
* `POST /perch` tethers the avatar to a target window's border and **keeps
  following it** while you drag or resize — per-monitor DPI aware.

### Life signs, voice and vision (all local, no cloud keys)

* Lip-sync driven by the **actual RMS envelope of the generated Piper WAV**
  (falling back to phoneme-mapped typing when muted).
* Pupils track the OS cursor across monitors.
* **Push-to-talk** (`Alt+V`): `cpal` capture → Silero VAD → `whisper-cli`
  transcription. Starting a hold interrupts TTS so the mic never transcribes the
  avatar.
* **Screen vision**: "What's on my screen?" grabs the active window in memory,
  downscales and JPEG-encodes it without touching disk, and sends it to any
  `openai_compat` vision server (e.g. `llama.cpp`).

### Plugins are real drop-ins (WebAssembly)

Features outside the core live in `wasm32-unknown-unknown` guests loaded at
runtime from `plugins/*.wasm`. The host knows the ABI, not the features:
`./scripts/build-plugin.sh`, restart, done — no host rebuild, no feature flag.
Fuel-metered (`50 M` instructions per call), isolated linear memory, and a
panic is a trap that parks the plugin instead of killing the app.

* **`survival.wasm`** — hunger/saturation metabolism, a 20-slot stack-aware bag
  (18 item kinds), a 24-food catalogue, 6 cooking recipes with comfort windows,
  spontaneous cravings, a 13-item shop with a counter that buys anything, and a
  four-phase fishing mini-game (cast → bite window → tension sweep → catch) with
  a weighted loot table.
* **`metamorphosis.wasm`** — a deterministic procedural creature generator.
  15 body plans × 6 forms × 4 builds = **360 structural silhouettes** (2,160
  with markings), grown through six stages (`Fresh → In-Training → Rookie →
  Champion → Ultimate → Mega`) with three attributes (`Seraph` / `Prism` /
  `Feral`) that reshape the body and blend by lineage weight. Generated rigs are
  hot-swapped onto the desktop and their bodies are **pinned** in the save, so a
  creature never silently becomes a different animal.

### The watcher: zero idle tokens

`char-rig-watcher.exe` sits on the SSE bus and polls window context, waking a
model only when a rule fires — window-title keywords, per-app overrides, pointer
gestures, idle return, external alerts, or a plugin event.

* **Single-flight dispatch** with a one-slot priority queue: a prompt that
  arrives mid-turn waits, background noise is dropped.
* Two backends behind one trait: any **`/v1/chat/completions`** server
  (`llama.cpp`, Ollama, LM Studio) or a headless **Hermes CLI** agent turn with
  real tools. An `openai_compat` model can `"escalate": true` a multi-step task
  up to the agent.
* Command bar (`Alt+Space`) with `quick_tasks` chips, a bounded **50-turn**
  history drawer (`H`), and session rotation by turn count or idle time.
* **`SOUL.md`** — the persona is a plain markdown file, read fresh on every turn
  and injected as slot #1 of the system message, so voice and brevity are tuned
  without touching config.

### Two APIs

* **HTTP/SSE** on `http://127.0.0.1:8765` — 64 routes across the core and both
  plugins, plus a 15-type SSE stream at `GET /events` for live state.
* **MCP** — `char-rig-mcp.exe`, a stdio bridge exposing 14 `avatar_*` tools
  (`avatar_speak`, `avatar_emote`, `avatar_perch`, `avatar_animate`,
  `avatar_get_context`, `avatar_get_clipboard`, …) to Hermes or any MCP client.

---

## What's in the box

| | |
|---|---|
| Binaries | 3 — `avatar-desktop`, `char-rig-watcher`, `char-rig-mcp` |
| HTTP routes | 64 (core + plugin-contributed) |
| SSE event types | 15 |
| MCP tools | 14 |
| UI ↔ host commands | 46 |
| Hand-authored rigs | 15 |
| Survival | 24 foods, 6 recipes, 18 item kinds, 13 shop items |
| Metamorphosis | 15 archetypes, 6 stages, 3 attributes |

---

## Requirements

* **Windows 10/11** (full feature set). Linux/X11 and macOS are supported
  through a platform layer — see *Known limitations*.
* **Rust 1.85+** and the MSVC build tools (Tauri/WebView2 shell).
* **WebView2** runtime (ships with Windows 11).
* The `wasm32-unknown-unknown` target for the plugins:
  `rustup target add wasm32-unknown-unknown`.
* Optional: **Piper** for speech and **whisper.cpp** + a Silero VAD model for
  push-to-talk. Without them the app runs fine, silently.

## Build from source

```bash
git clone https://github.com/dunkzinski/desktop-buddy.git
cd desktop-buddy

# 1. the plugins (they are .wasm artifacts and gitignored — a fresh clone has none)
rustup target add wasm32-unknown-unknown
./scripts/build-plugin.sh

# 2. the app (produces all three binaries)
cd src-tauri
cargo build
```

Then:

```bash
./target/debug/avatar-desktop.exe        # the overlay + API; spawns the watcher itself
```

Check that it is alive:

```bash
curl -s localhost:8765/health
curl -s localhost:8765/context           # focused window, rect, idle seconds
curl -s localhost:8765/plugins           # survival + metamorphosis, ticks climbing
curl -N localhost:8765/events            # live SSE stream
curl -s localhost:8765/history           # bounded chat history
curl -s -X POST localhost:8765/speak -H 'content-type: application/json' \
     -d '{"text":"Hello!","expression":"happy","duration_ms":4000}'
```

Register the MCP bridge in your Hermes `config.yaml` to get the 14 `avatar_*`
tools:

```yaml
mcp_servers:
  char-rig:
    command: 'C:\hermes\char-rig\src-tauri\target\debug\char-rig-mcp.exe'
    args: []
    env:
      CHAR_RIG_API_URL: http://127.0.0.1:8765
```

**Why no installers in this release:** every path the app reads or writes
(`models/`, `plugins/`, `watcher_rules.json`, `SOUL.md`) resolves relative to the
source tree, and the model root has a compile-time fallback to the build
machine's checkout. Under `C:\Program Files` those writes fail, and an install
that cleared `plugins/` would delete the saved creature. A data-directory split
(`%APPDATA%`) and a bundled installer are the next packaging step, not something
to paper over with a bundle today.

---

## Known limitations

* **Windows-first.** Audio I/O, hotkeys, clipboard and active-window tracking
  work on Linux/X11 and macOS through `src-tauri/src/platform/`; **screen
  capture is Windows-only**, and Wayland runs in partial mode (no portable
  focused-window title API).
* **The HTTP API is unauthenticated on `127.0.0.1`.** Fine for a single-user
  desktop; do not bind it to a network interface.
* **Mixed per-monitor DPI scaling** across a multi-monitor virtual desktop is
  the main remaining rough edge.
* **No installer, no auto-update, no code signing** — hence source-only.
* **Vision needs a local multimodal server** you run yourself (the config
  defaults assume `llama.cpp` on `127.0.0.1:9932` with an LFM3-VL model).
  Wizardry with paths there is on you; `GET /config` reports which paths exist.
* **CI is Windows-only.** `.github/workflows/check.yml` runs `cargo test --lib
  --bins` (lib, MCP and watcher targets), both plugin crate suites and
  `python tools/verify_docs.py` on `windows-latest`. The Linux/macOS platform
  layer builds, but no job exercises it.

---

## License

**Apache-2.0, across the board.** Code, the `ui/` frontend, both plugin crates,
the hand-authored and generated rigs (with their sprite sheets) under `models/`,
the documentation and the `SOUL.md` persona. See
[LICENSE](https://github.com/dunkzinski/desktop-buddy/blob/main/LICENSE) and
[NOTICE](https://github.com/dunkzinski/desktop-buddy/blob/main/NOTICE).

The 634-crate dependency graph is permissive — no GPL or AGPL anywhere (MIT,
Apache-2.0, BSD, ISC, Zlib, file-level MPL-2.0, Unicode-3.0) — so the license
above is not constrained by anything upstream. Piper and whisper.cpp are MIT and
not distributed here; WebView2 ships under Microsoft's own terms.

Contributions are inbound = outbound under the same license.

---

## Docs

* **[README.md](https://github.com/dunkzinski/desktop-buddy/blob/main/README.md)** — what it is, quick start
* **[ARCHITECTURE.md](https://github.com/dunkzinski/desktop-buddy/blob/main/ARCHITECTURE.md)** — threading model, kinematic math, rig schema
* **[API.md](https://github.com/dunkzinski/desktop-buddy/blob/main/API.md)** — every HTTP/SSE endpoint and payload
* **[MODEL_FORMAT.md](https://github.com/dunkzinski/desktop-buddy/blob/main/MODEL_FORMAT.md)** — rig folder layout and the editor
* **[PLUGINS.md](https://github.com/dunkzinski/desktop-buddy/blob/main/PLUGINS.md)** — the Wasm ABI, both guests, economy tables
* **[CONFIGURATION.md](https://github.com/dunkzinski/desktop-buddy/blob/main/CONFIGURATION.md)** — `watcher_rules.json`, watcher flags, `SOUL.md`

---

## Notes on this release

First tag. There is no previous release to diff against, so the "changelog" is
the full history: the overlay and renderer, the rig editor, voice and vision,
the watcher, the HTTP/MCP bridge, and then the two plugins — survival first as a
core subsystem, then rebuilt as a genuine drop-in `.wasm`, with metamorphosis
following it into the same host.

If something here does not run on your machine, that is worth an issue: the
fastest way to make this less prototype-shaped is a bug report with the
`GET /context` output and the watcher log.
