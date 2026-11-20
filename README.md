# desktop-buddy

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![check](https://github.com/dunkzinski/desktop-buddy/actions/workflows/check.yml/badge.svg)](https://github.com/dunkzinski/desktop-buddy/actions/workflows/check.yml)

**A local-first, cross-platform desktop companion runtime built in Rust + Tauri 2.**

It renders skeletal 2D rigs on a transparent, click-through overlay spanning your virtual desktop, drives lip-sync and cursor gaze from local Piper/Whisper pipelines, sandboxes procedural creature generation and mini-games in WebAssembly, and exposes a zero-idle-token HTTP/SSE/MCP bridge for local LLMs or Hermes Agent.

* **Native Rust core:** Tauri 2 (WebView2) shell, native ES-module canvas renderer, and an `axum` HTTP/SSE API on its own Tokio thread.
* **Data-driven 2D skeletal rigs (`model.json`):** Author bones, sprites, expression bindings, and keyframe clips in-app, then trigger them via UI, HTTP, or MCP.
* **Real-time life signs:** Mouth shapes track actual Piper WAV RMS energy (falling back to phoneme-mapped typing when muted), pupils track the OS cursor across monitors, and `POST /perch` dynamically tracks target window borders as you drag or resize them.
* **Fully local voice & vision:** Local Piper TTS (`rodio`/`cpal`), push-to-talk via Silero VAD + `whisper.cpp`, and in-memory screen capture routed to local vision-language models. No cloud keys required.
* **Sandboxed WebAssembly plugins:** Fuel-metered `wasm32-unknown-unknown` guests with isolated linear memory—shipping with a survival/fishing economy loop and a deterministic procedural creature generator (`2,160` base silhouettes across 15 body plans).
* **Zero-idle-token watcher:** A supervised background daemon subscribes to SSE and polls window context, waking an agent or local model only when rules match.

---

## Quick start

**Requirements:** **Rust** (`1.85+`, per `rust-version` in `src-tauri/Cargo.toml`), the **`wasm32-unknown-unknown`** target for the plugins (`rustup target add wasm32-unknown-unknown`), and **WebView2** (ships with Windows 11). Local voice I/O optionally uses **Piper** and **whisper.cpp**—without them, the app runs fine silently.

```bash
./scripts/build-plugin.sh     # plugins are .wasm artifacts and gitignored — a fresh clone has none
cd src-tauri
cargo build
./target/debug/desktop-buddy.exe
```

`cargo build` produces **three** binaries in `src-tauri/target/debug/`:
1. `desktop-buddy.exe` — the overlay, canvas renderer, Wasm plugin host, and `axum` server (`http://127.0.0.1:8765`).
2. `desktop-buddy-watcher.exe` — the trigger daemon. `desktop-buddy.exe` spawns and supervises it automatically (set `DESKTOP_BUDDY_NO_WATCHER=1` to run it manually).
3. `desktop-buddy-mcp.exe` — the stdio MCP bridge exposing 14 `avatar_*` tools to Hermes or any MCP client.

Verify the running instance:

```bash
curl -s localhost:8765/health
curl -s localhost:8765/context          # focused window, rect, visible windows, idle seconds
curl -N localhost:8765/events           # live SSE push stream
curl -s localhost:8765/history          # bounded chat history (50 turns)
curl -s -X POST localhost:8765/speak -H 'content-type: application/json' \
     -d '{"text":"Hello!","expression":"happy","duration_ms":4000}'
curl -s -X POST localhost:8765/perch -H 'content-type: application/json' \
     -d '{"edge":"top_right","window_process":"Code.exe"}'
```

---

## Key capabilities

### In-app rig & animation editor (`R`)
Toggle **Rig Edit Mode** from the titlebar or press `R` to drag joints, rotate bone tips, scale parts (`Shift+Wheel`), bind expression PNGs, and scrub keyframe clips (`+ Clip`, `+ Keyframe`, loop, duration). Saving writes directly to `models/<name>/rig.json`. You can also import standalone images (`.png`, `.svg`, `.webp`) as new characters or attach them as limbs to an existing rig.

### Local voice, push-to-talk & screen vision
* **Speech output:** Every speech bubble is sanitized and synthesized on a dedicated `avatar-tts` thread via Piper, streaming 25 ms RMS amplitude buckets to the renderer for audio-driven lip-sync.
* **Push-to-talk (`Alt+V`):** Hold to capture mic audio via `cpal`, filter with Silero VAD, and transcribe locally via `whisper-cli`. Starting a hold immediately interrupts active TTS playback so the mic never transcribes the avatar.
* **Local vision:** Press **What's on screen?** (or say/type *"what's on my screen?"*) to grab the active window in memory, downscale/JPEG-encode it without touching disk, and send it to any `openai_compat` vision server (e.g., `llama.cpp`).

### WebAssembly plugin runtime (`plugins/`)
Features outside the core avatar engine live in standalone `wasm32-unknown-unknown` crates loaded dynamically at startup:
* **`survival.wasm`:** Adds hunger/saturation metabolism, a 20-slot stack-aware bag, a four-phase fishing mini-game, cooking recipes, and a 13-item shop economy (`GET /store`, `POST /store/buy`, `POST /store/sell`). Body events route to the AI brain with quiet offline fallbacks.
* **`metamorphosis.wasm`:** A pure-Rust procedural character generator (`Fresh → Mega`). Combines a seed, 12 structural body plans, 5 forms per family, and a recency-weighted `Seraph` / `Prism` / `Feral` lineage to synthesize complete `desktop-buddy/model@1` skeletons with exact kinematic mirroring (`atan2(sin θ, −cos θ)`) and hot-swaps them onto the desktop.

Build or update plugins without recompiling the host app:
```bash
./scripts/build-plugin.sh
```

### Watcher, AI profiles & persona (`SOUL.md`)
* **Command bar (`Alt+Space` / `Enter`):** Summons a clickable prompt input over the click-through overlay with configurable `quick_tasks` chips and a bounded 50-turn **History** drawer (`H`).
* **Multi-brain routing:** Switch live between local `/v1/chat/completions` endpoints (`llama.cpp`, Ollama, LM Studio) and headless `hermes_cli` sessions via **Settings ⚙ → AI brain** (includes LAN/localhost model auto-discovery).
* **Single-flight dispatch:** `desktop-buddy-watcher` enforces single-flight child execution with a 1-slot priority queue for user replies, alerts, and fishing outcomes—preventing token pile-ups while ignoring background window noise.
* **Persona (`SOUL.md`):** A plain markdown file re-read live on every turn to define the avatar's voice and brevity rules independently of runtime config.

---

## Hermes / MCP integration

Register `desktop-buddy-mcp.exe` in your Hermes `config.yaml` to expose `avatar_speak`, `avatar_emote`, `avatar_move`, `avatar_perch`, `avatar_wave`, `avatar_pose`, `avatar_animate`, `avatar_get_context`, `avatar_get_clipboard`, `avatar_set_clipboard`, `avatar_list_models`, `avatar_switch_model`, `avatar_list_animations`, and `avatar_switch_ai`:

```yaml
mcp_servers:
  desktop-buddy:
    command: 'C:\hermes\desktop-buddy\src-tauri\target\debug\desktop-buddy-mcp.exe'
    args: []
    env:
      DESKTOP_BUDDY_API_URL: http://127.0.0.1:8765
```

---

## Cross-platform support

OS calls are isolated behind `src-tauri/src/platform/`:
* **Windows:** Full feature set (Win32 click-through, DWM focus, GDI `BitBlt` screen capture, global hotkeys, WASAPI audio).
* **Linux (X11) & macOS:** Audio I/O (`rodio`/`cpal`), global hotkeys, clipboard, and active-window tracking (`xdotool`/`wmctrl` and `osascript`) work out of the box. Wayland runs in partial mode (no portable focused-window title API), and screen capture currently requires Windows.

Verify cross-platform compilation without a full Tauri GUI toolchain:
```bash
cd tools/platform-check && cargo check --target x86_64-unknown-linux-gnu
```

---

## Project layout

```text
src-tauri/
  src/
    lib.rs               Tauri windows, state, SSE event bus, click-through tracker
    api.rs               axum HTTP/SSE server
    models.rs            Rig storage, validation, image/rig import pipeline
    tts.rs / stt.rs      Local Piper TTS & Whisper + Silero VAD push-to-talk
    settings.rs          Byte-span JSON config editor (GET/POST /config)
    platform/            OS abstraction layer (win32.rs, unix.rs)
    plugins/             Wasmtime/Wasm sandbox host & route proxy
  src/bin/
    desktop-buddy-mcp.rs      Stdio MCP server binary
    desktop-buddy-watcher.rs  SSE + context trigger daemon binary
plugins/                 Guest Wasm crates (survival, metamorphosis) + built .wasm files
ui/                      Zero-build-step ES module frontend (canvas renderer, rig editor, speech UI)
models/                  Skeletal rigs, sprites, and persisted selection state
tools/                   verify_docs.py (source↔docs drift check), cdp-eval.mjs, mcp_smoke_test.py
.github/workflows/       check.yml — CI: cargo test --lib --bins, both plugin crates, verify_docs.py
```

---

## Deep-dive documentation

* **[ARCHITECTURE.md](ARCHITECTURE.md)** — Threading model, kinematic math, rig schema, and known limitations
* **[API.md](API.md)** — Full HTTP/SSE endpoint reference and payload examples
* **[MODEL_FORMAT.md](MODEL_FORMAT.md)** — Rig folder layout, manifest specification, and editor guide
* **[PLUGINS.md](PLUGINS.md)** — Wasm ABI imports/exports, the plugin registry, survival economy tables, and procedural creature generation
* **[CONFIGURATION.md](CONFIGURATION.md)** — Complete `watcher_rules.json` schema, watcher CLI flags, AI profiles, and `SOUL.md` injection rules

---

## Status

Active hobby/prototype build. The HTTP API is unauthenticated on localhost, and mixed per-monitor DPI scaling across multi-monitor virtual desktops is the main remaining rough edge. Check `python tools/verify_docs.py` after modifying routes or events to ensure documentation stays in sync with the Rust source.

---

## License

**Apache License 2.0** — see [LICENSE](LICENSE) and [NOTICE](NOTICE).

It covers the whole repository, not just the code: the Rust sources, the `ui/` frontend, both plugin crates, the hand-authored and generated rigs under `models/`, the documentation and the `SOUL.md` persona. Third-party crates keep their own (permissive) licenses — the resolved set is visible with `cargo metadata`.

Contributions are accepted under the same license (inbound = outbound), per section 5 of the license.
