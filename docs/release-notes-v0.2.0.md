# desktop-buddy v0.2.0 — the renamed release, and the first one you can just run

A local-first desktop companion: a skeletal 2D avatar that lives on a
transparent, click-through overlay across your virtual desktop, talks through
local Piper TTS, listens through local Whisper, tracks your cursor with its
pupils, and exposes an HTTP/SSE/MCP API so a local model or an agent can drive
it.

**This release renames the project and attaches a portable Windows build.**
v0.1.0 was source-only; every binary in that tag was called `avatar-desktop`.
The project is now `desktop-buddy` in every layer — and because the download
below is the first thing most people will touch, the rename is the breaking
change that earns the minor version.

Dated: 2026-11-20 · `v0.2.0` · 4 commits since `v0.1.0`

---

## Download

**[`desktop-buddy-v0.2.0-windows-x64.zip`](https://github.com/dunkzinski/desktop-buddy/releases/download/v0.2.0/desktop-buddy-v0.2.0-windows-x64.zip)**
— 12.6 MiB, 43 files, Windows 10/11 x64.

Unzip anywhere **you can write to** (Desktop, `C:\tools`, a USB stick) and run
`Start.bat`. The portable tree is deliberate, not lazy: models, plugins, saved
creatures, settings and `SOUL.md` are all resolved and written *next to the
executable*, so extracting into `C:\Program Files` gets you a read-only app that
cannot save its pet. The data-directory split that fixes that is the next
packaging step, not this one.

| in the zip | |
|---|---|
| `desktop-buddy.exe` | overlay, canvas renderer, Wasm host, HTTP/SSE API on `127.0.0.1:8765` |
| `desktop-buddy-watcher.exe` | trigger daemon (spawned and supervised by the app) |
| `desktop-buddy-mcp.exe` | stdio MCP bridge, 14 `avatar_*` tools |
| `plugins/survival.wasm`, `plugins/metamorphosis.wasm` | both guests, prebuilt |
| `models/` | the **15 hand-authored rigs** — generated `buddy-*` pets are *not* shipped: those are somebody's creature, not catalogue |
| `README.md`, `LICENSE`, `NOTICE`, `SOUL.md`, `watcher_rules.json` | the app reads the last two at runtime |
| `Start.bat`, `PORTABLE.txt` | launcher + the layout warning above |

Not bundled, all optional: **WebView2** (ships with Windows 11 — required for the
overlay to render), **Piper** for speech, **whisper.cpp** + a Silero VAD model for
push-to-talk. Without them the app runs fine, silently.

The `ui/` directory is not in the zip because it does not need to be — the
frontend is embedded into `desktop-buddy.exe` at compile time.

---

## What changed since v0.1.0

### The rename, in every layer

Package `desktop-buddy`, lib `desktop_buddy_lib`, binaries `desktop-buddy.exe` /
`desktop-buddy-mcp.exe` / `desktop-buddy-watcher.exe`, env vars
`DESKTOP_BUDDY_*`, session ids `desktop-buddy-<seed>`, MCP server key
`desktop-buddy` (tools are `mcp__desktop_buddy__avatar_*`), rig tag
`desktop-buddy/model@1`.

**If you ran v0.1.0, this is what moving means for you:** the MCP entry in your
client config must be re-keyed and re-pointed at the new `.exe`; the app moves
its old `%LOCALAPPDATA%\char-rig` config directory across once, on first launch.
Five `char-rig` spellings survive on purpose — the legacy rig format tag, the
legacy config-dir name, the legacy panel-layout key and two generator format
tags — because deleting them makes rigs you already generated fail to load.

### A portable build, and the honest scope of it

Three release-profile binaries plus the two guest `.wasm` files, assembled
outside the build tree and smoke-tested from the zip itself before it was
attached (see *Verified*).

### CI, at last

`.github/workflows/check.yml`, one `windows-latest` job: `cargo test --lib
--bins` for the app, then each plugin crate explicitly (each is its own
`[workspace]`, so the app's cargo never reaches them), then
`python tools/verify_docs.py`. Badge in the README. It is a real gate and it is
Windows-only — that limitation is still true and still listed below.

### Two bugs the promo screenshots exposed

Photographing the app for the README turned up two defects, both fixed in this
release:

* `#speech` carried `white-space: pre-wrap`, so every bubble also rendered the
  HTML template's own indentation — four blank lines, ~76 px of dead space under
  every message. The property now sits on `#speech-text`: a message keeps its
  newlines and the bubble hugs its text (132 px → 38 px for the same two-line
  line).
* `catch_notice` hardcoded "a", so a vowel-initial catch read "Caught a Ember
  eel". `article()` / `article_cap()` now pick a/an, with a test walking all
  three variants.

Plus five CDP-captured product shots (hero, fishing, metamorphosis, rig editor,
history) replacing the old full-desktop grab in the README.

---

## Everything that was already there

The capabilities are unchanged from v0.1.0; the numbers below were re-derived
from a passing `verify_docs.py` run on this tag, not copied forward.

* **Overlay and rigs.** Whole-virtual-desktop transparent window, real
  click-through (`GET /hit-regions` reports the avatar's own shapes; everything
  else passes through), `desktop-buddy/model@1` skeletal rigs with bones,
  sprites, expression bindings and keyframe clips, an in-app rig editor (`R`),
  and `POST /perch` that keeps following a target window while you drag or
  resize it.
* **Life signs, voice, vision.** Lip-sync from the actual RMS envelope of the
  generated Piper WAV, pupils that track the OS cursor across monitors,
  push-to-talk (`Alt+V`) through `cpal` → Silero VAD → `whisper-cli`, and screen
  vision that grabs the active window in memory and sends it to any
  `openai_compat` vision server. No cloud keys anywhere.
* **Wasm plugins.** Fuel-metered (`50 M` instructions per call), isolated linear
  memory, panic-trapped. `survival.wasm`: hunger/saturation metabolism, 20-slot
  stack-aware bag, 24 foods, 6 recipes, 13-item shop, four-phase fishing
  mini-game. `metamorphosis.wasm`: 15 body plans × 6 forms × 4 builds = 360
  structural silhouettes (2,160 with markings), six stages, three blending
  attributes, bodies pinned in the save.
* **Watcher.** Single-flight dispatch with a one-slot priority queue, rules on
  window title / per-app override / pointer gesture / idle return / alert /
  plugin event, two backends behind one trait (any `/v1/chat/completions`
  server, or a headless Hermes CLI agent turn), command bar at `Alt+Space`,
  bounded 50-turn history drawer (`H`), and `SOUL.md` injected as the first
  system-message slot on every turn.
* **Two APIs.** 64 HTTP routes, a 15-type SSE stream at `GET /events`, and the
  14-tool MCP bridge.

---

## What's in the box

| | |
|---|---|
| Binaries | 3 — `desktop-buddy`, `desktop-buddy-watcher`, `desktop-buddy-mcp` |
| HTTP routes | 64 (core + plugin-contributed) |
| SSE event types | 15 |
| MCP tools | 14 |
| UI ↔ host commands | 46 |
| Hand-authored rigs | 15 |
| Survival | 24 foods, 6 recipes, 18 item kinds, 13 shop items |
| Metamorphosis | 15 archetypes, 6 stages, 3 attributes |
| Rust in `src-tauri/src` | 28,601 lines |
| Tests | 583 — 339 app (190 lib, 12 MCP, 137 watcher), 175 survival, 69 metamorphosis |
| Licence | Apache-2.0 |

---

## Build from source (any platform the platform layer supports)

```bash
git clone https://github.com/dunkzinski/desktop-buddy.git
cd desktop-buddy

# 1. the plugins — they are .wasm artifacts and gitignored, a fresh clone has none
rustup target add wasm32-unknown-unknown
./scripts/build-plugin.sh

# 2. the app (produces all three binaries)
cd src-tauri
cargo build            # or: cargo build --release --bins
```

Then:

```bash
./target/debug/desktop-buddy.exe        # the overlay + API; spawns the watcher itself
```

Check that it is alive:

```bash
curl -s localhost:8765/health            # {"ok":true,...,"version":"0.2.0"}
curl -s localhost:8765/context           # focused window, rect, idle seconds
curl -s localhost:8765/plugins           # both guests, ticks climbing
curl -N localhost:8765/events            # live SSE stream
curl -s localhost:8765/history           # bounded chat history
curl -s -X POST localhost:8765/speak -H 'content-type: application/json' \
     -d '{"text":"Hello!","expression":"happy","duration_ms":4000}'
```

Register the MCP bridge in your client config to get the 14 `avatar_*` tools:

```yaml
mcp_servers:
  desktop-buddy:
    command: 'C:\hermes\desktop-buddy\src-tauri\target\debug\desktop-buddy-mcp.exe'
    args: []
    env:
      DESKTOP_BUDDY_API_URL: http://127.0.0.1:8765
```

---

## Verified

Every claim above is a command's output on this tag:

* `cargo build --release --bins` → all three binaries.
* `cargo test --lib --bins` → 190 + 12 + 137 passed (1 ignored).
  `cargo test --lib` on `plugins/survival` → 175, on `plugins/metamorphosis` → 69.
* `python tools/verify_docs.py` → "Docs and source agree on routes, commands,
  events, kinds and keys." (64 routes, 46 commands, 15 SSE types, 14 MCP tools).
* The attached zip was **extracted to an empty folder and run from there** — not
  just built: `GET /health` answered `"version":"0.2.0"`, `GET /plugins` reported
  `active: 2`, `GET /models` loaded all 15 rigs from the extracted `models/`, the
  two guests wrote their `plugins/*.state.json` beside the `.wasm`, the overlay
  and control panel rendered, and `POST /shutdown` closed it cleanly.
* `sha256 f5084db6f1d3b9146eb4b9400466b4c89069f64ca30a2092a46f35deeae6e1c1`

---

## Known limitations

* **Windows-first.** Audio I/O, hotkeys, clipboard and active-window tracking
  work on Linux/X11 and macOS through `src-tauri/src/platform/`; **screen
  capture is Windows-only**, and Wayland runs in partial mode (no portable
  focused-window title API).
* **The zip is portable, not installed.** It writes into its own folder by
  design: keep it somewhere writable, and know that deleting `plugins/` deletes
  the saved creature. No installer, no auto-update, **no code signing** — Windows
  SmartScreen will warn about an unsigned binary, because it is unsigned.
* **The HTTP API is unauthenticated on `127.0.0.1`.** Fine for a single-user
  desktop; do not bind it to a network interface.
* **Mixed per-monitor DPI scaling** across a multi-monitor virtual desktop is
  still the main remaining rough edge.
* **CI is Windows-only.** The Linux/macOS platform layer builds, but no job
  exercises it.
* **Vision needs a local multimodal server** you run yourself (the config
  defaults assume `llama.cpp` on `127.0.0.1:9932` with an LFM3-VL model).
  `GET /config` reports which paths exist.

---

## License

**Apache-2.0, across the board.** Code, the `ui/` frontend, both plugin crates,
the hand-authored and generated rigs (with their sprite sheets) under `models/`,
the documentation and the `SOUL.md` persona. See
[LICENSE](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/LICENSE) and
[NOTICE](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/NOTICE).

The dependency graph is permissive — no GPL or AGPL anywhere — so the licence
above is not constrained by anything upstream. Piper and whisper.cpp are MIT and
not distributed here; WebView2 ships under Microsoft's own terms.

---

## Docs

* **[README.md](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/README.md)** — what it is, quick start
* **[ARCHITECTURE.md](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/ARCHITECTURE.md)** — threading model, kinematic math, rig schema
* **[API.md](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/API.md)** — every HTTP/SSE endpoint and payload
* **[MODEL_FORMAT.md](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/MODEL_FORMAT.md)** — rig folder layout and the editor
* **[PLUGINS.md](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/PLUGINS.md)** — the Wasm ABI, both guests, economy tables
* **[CONFIGURATION.md](https://github.com/dunkzinski/desktop-buddy/blob/v0.2.0/CONFIGURATION.md)** — `watcher_rules.json`, watcher flags, `SOUL.md`

---

## Notes on this release

Short cycle: the rename, CI, the screenshot work and the two UI/text bugs it
turned up, and then the packaging work that makes this the first release you can
run without a Rust toolchain. The data-directory split, a real installer
(WiX/NSIS, with the two plugins as optional components), and signing/auto-update
are the next packaging steps — deliberately not pretended into this one.

If something here does not run on your machine, that is worth an issue: the
fastest report is the `GET /context` output, the watcher log, and your GPU/DPI
setup.
