# char-rig — AI desktop avatar

A 2D character that lives on your Windows desktop. It walks around a transparent
always-on-top overlay, blinks, emotes, talks with a speech bubble — and exposes a
local HTTP API so an AI agent can drive it.

![The avatar on the desktop](docs/screenshot.png)

* Rust + Tauri 2 (WebView2) shell, canvas renderer, axum HTTP API
* Data-driven rigs (`model.json`), editable in-app, with sprite parts + variants
* Custom keyframe animations: author clips in the rig editor, play them from the UI, the HTTP API or an MCP agent
* Life signs: the mouth is driven by the typewriter (vowel / consonant / pause shapes), the eyes follow your cursor, and the head tilts a little when you hover
* Talk to it without a terminal: `Alt+Space` (configurable via `hotkey` in `watcher_rules.json`) summons the command bar from any application
* A **survival game** in a WebAssembly plugin: hunger and saturation, a 20-slot
  bag, a four-phase fishing mini-game, and a coin economy with a shop
* Works alongside your desktop: only the titlebar, panel and avatar catch clicks

---

## Quick start

Requirements: **Rust** (1.77+ toolchain), **WebView2 runtime** (ships with
Windows 11), Windows 10/11. Voice output additionally needs the local **Piper**
install described in [Speaking out loud](#speaking-out-loud-local-voice) — a
build without it runs fine, just silently.

```bash
cd src-tauri
cargo build
./target/debug/avatar-desktop.exe
```

`cargo build` produces **three** binaries in `src-tauri/target/debug/`:
`avatar-desktop.exe` (the app), `char-rig-mcp.exe` (the MCP bridge — see
[Hermes / MCP](#hermes--mcp)) and `char-rig-watcher.exe` (the trigger watcher —
see [Waking Hermes](#waking-hermes-the-watcher)).

The console prints `avatar api listening on http://127.0.0.1:8765`. The overlay
covers the **whole virtual desktop** — every monitor, including one positioned
left of or above the primary — so the avatar can walk onto any of them; empty
desktop space is click-through.

**The watcher comes with it.** Once the API is bound, `avatar-desktop.exe` starts
`char-rig-watcher.exe` from its own directory, pointed at the URL it just bound
(`--api-url http://127.0.0.1:8765`) and at a `watcher_rules.json` it finds beside
the binaries or up to three directories above (the repo root in a checkout), with
no console window of its own and its stderr inherited so its log lines land next
to the app's. A supervisor thread
restarts it after a crash (2 s, backing off to 30 s), and closing the avatar —
titlebar **X**, `POST /shutdown`, or an app exit — kills and reaps it, so no
orphaned watcher is ever left behind. Set `CHAR_RIG_NO_WATCHER=1` to opt out and
run the watcher by hand.

Verify it responds:

```bash
curl -s localhost:8765/health
curl -s localhost:8765/context          # foreground window, its rect, visible windows, idle seconds
curl -N localhost:8765/events           # live push stream (Ctrl-C to stop)
curl -s -X POST localhost:8765/speak -H 'content-type: application/json' \
     -d '{"text":"Hello!","expression":"happy","duration_ms":4000}'
curl -s -X POST localhost:8765/perch -H 'content-type: application/json' \
     -d '{"edge":"top_right","window_process":"code"}'
curl -s localhost:8765/tts               # local Piper voice: ready / muted / why
curl -s -X POST localhost:8765/tts -H 'content-type: application/json' -d '{"enabled": false}'
```

Run the unit tests with `cd src-tauri && cargo test`.

### Optional environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `CHAR_RIG_API_PORT` | `8765` | API port (next free port used if busy) |
| `CHAR_RIG_MODELS` | `<repo>/models` | model root directory |
| `CHAR_RIG_NO_WATCHER` | *(unset)* | `1`/`true` stops `avatar-desktop` from spawning and supervising `char-rig-watcher.exe` — use it when running the watcher manually in a terminal |
| `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` | *(unset)* | passed straight to WebView2; set to `--remote-debugging-port=9222` to attach DevTools/CDP to the running page |

### Developer tools

| Script | Purpose |
| --- | --- |
| `python tools/verify_docs.py` | diffs every HTTP route, Tauri command, SSE event type, `/context` key and interaction kind in the markdown against `src-tauri/src/*.rs` and `ui/js/*.js`; exits non-zero on drift |
| `node tools/cdp-eval.mjs <wsUrl> <expr>` | evaluates an expression inside the running page (see the row above for the port); used to drive `window.__avatarDebug` without a mouse — e.g. `node tools/cdp-eval.mjs ws://127.0.0.1:9222/devtools/page/<id> 'window.__avatarDebug.interaction("click", 400, 300)'` |
| `python tools/mcp_smoke_test.py` | drives `char-rig-mcp.exe` over stdio exactly like an MCP client: tool schemas, JSON-RPC error codes, and both the "avatar down" and "avatar up" tool paths (animations included) |

---

## Using it

| Titlebar | |
| --- | --- |
| 🛠 wrench | toggle **Rig Edit Mode** (`R`) |
| ↖ cursor | toggle click-through on empty space |
| — | minimize |
| ✕ | close (stops the API too) |

| Control panel | |
| --- | --- |
| Model dropdown + **Import…** | switch models, import a rig or an image |
| Expression buttons | neutral / happy / sad / surprised |
| Wave / Idle / Walk / Center | triggers, with a walk-speed slider |
| **New chat** | wipe the conversation context (`POST /session/reset`) and empty the History drawer |
| **History** | open the session's conversation log (see [Reading the conversation back](#reading-the-conversation-back)) |
| **Ask…** | open the command bar (same as `Enter` / `/`) |
| **Watcher** switch + badge + **Logs ↗** | start/stop `char-rig-watcher.exe` and open the pop-out console (see [Waking Hermes](#waking-hermes-the-watcher)) |
| **Voice** switch + badge | mute/unmute the local Piper voice; the badge says `on`, `muted` or `missing` (see [Speaking out loud](#speaking-out-loud-local-voice)) |
| **Hold to talk** button + badge | press and hold to talk to the avatar, release to send; the badge says `idle`, `listening`, `transcribing` or `missing` (see [Talking to it](#talking-to-it-push-to-talk)) |
| **What's on screen?** button | screenshot the focused window and ask the local vision model about it (see [Seeing your screen](#seeing-your-screen-local-vision)) |
| API line | live API URL |

**Importing:** pick a `.json`/`.toml` rig, or an image (`.png`, `.jpg`,
`.jpeg`, `.webp`, `.svg`) — images can become a **new character** or be
**attached as a part** to the current rig.

**Building in the editor:** in Rig Edit Mode drag joints to move, drag the ring
at the bone tip to rotate (free multi-turn), shift+wheel to scale. `Add content`
creates new parts/limbs with a semantic tag (`head`, `eye`, `mouth`, `arm_left`,
…); bind sprites to expressions with **Bind PNG**; **Save rig** writes
`models/<name>/rig.json`, **Reset** drops it.

**Animating:** the timeline bar at the bottom of Rig Edit Mode edits named
keyframe clips — `+ Clip`, `Del Clip`, `Duration (ms)`, `Loop`, `Play / Stop`,
a `0..100%` scrubber with keyframe ticks, and `+ Keyframe` to capture the pose on
screen at the playhead. Clips are saved by the same **Save rig** and played over
HTTP (`POST /animate`) or by an agent (`avatar_animate`).

Play-mode shortcuts: `1`–`4` expressions · `W` wander · `I` idle · `C` centre ·
`Space` wave · `R` rig mode · `H` conversation history · `Esc` close.

Clicking or double-clicking the avatar does not make it react on its own — the
page reports the interaction to Rust, which pushes an `interaction` frame on
`GET /events` so your agent can decide what happens (wave back, speak, ignore).

**Talking back:** press `Enter` or `/` (or the chat icon in the titlebar, or
**Ask…** in the control panel, or just click the speech bubble) and a floating
command bar opens under the avatar. Type a prompt or a task, `Enter` sends it,
and the bubble immediately shows **Thinking…** with the `thinking` face while the
model works. Under the input are one-click **chips** (summarise the clipboard,
"what am I looking at", a diagnostic, a network scan…) loaded from
`quick_tasks` in `watcher_rules.json`, and a **↺** button that drops the
conversation context. `Esc` closes the bar. The text becomes a `user_reply` frame
(`notify_reply` / `POST /reply`), the watcher wakes Hermes with your exact words,
and the bubble's box is a published hit region so the input stays clickable while
the rest of the desktop is click-through.

**Moving it:** in play mode, press on the avatar and drag — it follows the
cursor, clamped to the screen, and idle wandering stays paused for 15 s after
you let go (the release is reported as a `drag_end` interaction). Rig Edit Mode
keeps the press for its gizmos, so the two gestures never fight.

---

## Settings (no JSON required)

The **Settings ⚙** button in the control panel opens a modal that edits
`watcher_rules.json` for you — it is the alternative to hunting for a path in a
6000-line config file:

* **Voice & paths** — the Piper executable, voice model, `espeak-ng-data` folder,
  the Whisper executable/model and the Silero VAD model. Every path field has a
  **Browse…** button (a real file dialog) and a **✓ / ✗** telling you whether the
  path exists *right now*. Speaking speed, the hold-to-talk hotkey, VAD threshold
  and language live here too, as does screen-vision's capture mode.
* **AI brain** — which profile answers desktop triggers, plus each HTTP profile's
  endpoint, model id, `max_tokens` and timeout.
* **Survival** — the hunger system switch, tier notifications, both metabolic
  drain rates, the telemetry interval and the starting values.

Saving sends **only what you changed** (`POST /config`), validates each block with
the same struct the runtime loads it from, and answers what applied live versus
what needs a restart. Survival rates and AI/vision settings apply immediately; a
new Piper or Whisper **path** is read once at startup, so it is reported as
`restart_required` instead of being silently ignored. The previous file is kept
as `watcher_rules.json.bak`.

`GET /config` also reports **stray keys** — a block written with friendly names
such as `metabolic_drain_rate` is ignored by the loader (the real key is
`metabolism.hunger_per_min`), so the modal shows an amber warning instead of
pretending those settings work.

Everything the modal writes is still plain JSON, so hand-editing remains fine:
the editor rewrites only the blocks it touches and leaves the rest of the file
byte-for-byte alone.

---

## Plugins (WebAssembly, dropped in at runtime)

Features that are not the avatar engine itself are **WebAssembly plugins**. They
are not compiled into the app: at startup the host scans `plugins/` for `*.wasm`,
instantiates each one in its own sandbox (own linear memory, own instruction
budget) and wires up whatever it contributes.

```text
plugins/
├── survival/            the guest crate (its own Cargo workspace)
│   ├── Cargo.toml       crate-type = ["cdylib", "rlib"], targets wasm32-unknown-unknown
│   └── src/
│       ├── lib.rs       the ABI: alloc/dealloc + plugin_* exports, 4 host imports
│       ├── model.rs     hunger, saturation, tiers, the food catalog (raw
│       │                ingredients vs. cooked dishes), recipes and cravings
│       ├── inventory.rs the item catalog (fish, bait, cooking ingredients,
│       │                gear, material, junk) and the slot-limited,
│       │                stack-aware bag
│       ├── store.rs     the shop: the price list (bait, driftwood, and every
│       │                cooking ingredient the recipes use), buy/sell pricing
│       │                and the shelf categories
│       ├── fishing.rs   the four-phase mini-game: hidden bite, reaction window,
│       │                the triangle-wave reel, the catch table and the prose
│       ├── plugin.rs    the tick loop, telemetry, meal announcements, craving
│       │                rolls, the bag/fishing actions and the spontaneous lines
│       │                spoken via host_notify
│       ├── routes.rs    ROUTES = [("/survival", …), …] + the request dispatcher
│       └── config.rs    the `survival` block of watcher_rules.json (metabolism,
│                        cravings, fishing and inventory)
└── survival.wasm        the built artifact — this is what the app loads
```

The guest talks to the app through exactly four imports (`host_emit_event`,
`host_notify`, `host_now_ms`, `host_log`) and exports JSON-over-memory functions:
`plugin_init`, `plugin_tick`, `plugin_status`, `plugin_routes`, `plugin_http`,
and the config hooks. The host (`src-tauri/src/plugins/wasm_host.rs`) turns
`plugin_routes` into axum routes that proxy each request into the instance.

**Build and install a plugin** (no app rebuild):

```bash
./scripts/build-plugin.sh          # compiles plugins/*/ for wasm32-unknown-unknown
                                   # and copies each artifact to plugins/<name>.wasm
```

Restart `avatar-desktop.exe` and its routes, its config block in the Settings
modal and its entry in `GET /plugins` are live. Delete `plugins/survival.wasm`
and they are gone — the app still starts and still serves `/plugins`.

Isolation is the point: a plugin that traps is parked by the registry (the app
keeps running), every call is fuel-metered so a spinning guest cannot hang the
ticker, and a `.wasm` that will not load is logged and skipped. `survival.enabled:
false` keeps the plugin installed but idle — the bars freeze and the routes keep
answering with `"enabled": false`.

### The economy loop

The survival plugin also runs a small shop, so the bag has somewhere to send its
surplus. A session starts with **100 coins** (`STARTING_COINS`, on the same
`SurvivalStats` the hunger bars live on, so it shows up in `GET /survival` and in
the telemetry frames), `GET /store` lists the price list with the balance and the
player's holdings, `POST /store/buy` spends coins to stock the bag and
`POST /store/sell` hands gathered material back for coins. Every trade is
**all-or-nothing** — the purse and the free space are checked before anything
moves — and publishes a `store-transaction` event so the page sees the new
balance without polling.

* **`GET /store`** — catalog of 13 entries: two baits (`worm_bait` 4/1,
  `glowing_lure` 24/10), the **eight cooking ingredients** every recipe uses
  (`apple` 4/2, `egg` 3/1, `cheese` 5/2, `tomato` 3/1, `mushroom` 3/1,
  `noodles` 4/1, `bread` 4/2, `honey` 8/3), two materials (`driftwood` 6/3,
  `river_weed` 5/2) and the rod (75/30) — each with `buy_price`, `sell_price`,
  `category` (`bait` / `ingredient` / `material` / `tool`), `owned` and
  `stack_max`; plus `coins` and the inventory summary.
* **`POST /store/buy`** — `{"item": "worm_bait", "count": 3}`. A refused buy is
  **409** `insufficient_coins` (with `required` and `shortfall`) or **409**
  `inventory_full` (with `room`), and costs nothing.
* **`POST /store/sell`** — the mirror; selling more than the bag holds is **409**
  `not_enough_items`, and an id the shop does not stock (a caught fish, a boot) is
  **400**.

The Survival card's fourth tab (*Store*) is the front end: it lists the shelves
with both prices and how many the player already holds, and its **Buy** / **Sell**
buttons are those two routes with a **Qty** box, so the loop can be played without
a terminal.

That closes the loop the rest of the feature already had: bait is spent on every
cast, the driftwood and river weed the line drags up are worth coins, and the
coins buy the bait — or the glowing lure — the next cast needs. `sell_price` is
deliberately always below `buy_price`, so reselling is a loss and the shop is a
sink rather than a coin printer.

**Cooking runs through the same pantry.** A recipe is not a magic spell any
more: `POST /survival/craft` spends its ingredients out of the bag, so an
omelette costs two eggs and a cheese that had to be bought (11 coins) or
gathered. `GET /recipes` reports the bag — `can_cook`, `missing`, and `held` per
line — which is what the Cook row uses to grey itself out and to say what is
short, and a cook refuses with **409** `missing_ingredients` (naming every
shortfall) or the usual `too_full` / `still_digesting` without spending a thing.
That makes the shopping trip part of the game rather than a side quest: catch a
fish, sell the driftwood, buy the eggs.

---

## Speaking out loud (local voice)

Every speech bubble — from `lfm3-fast`, from `hermes_cli`, from `POST /speak`,
from the MCP `avatar_speak` tool, from anything — is also **read out loud** by a
local [Piper](https://github.com/rhasspy/piper) voice. No network, no API key,
no cloud. The hook is the single `do_speak` path, so a model can never "talk" in
text without talking out loud.

The shipped setup (change it in the **Settings** modal, or override it in
`watcher_rules.json`):

| Piece | Default |
| --- | --- |
| `piper_exe_path` | `C:\hermes\tools\piper\piper.exe` |
| `model_path` | `C:\hermes\tools\piper\voices\en_GB-southern_english_female-low.onnx` |
| `espeak_data_path` | `C:\hermes\tools\piper\espeak-ng-data` (passed as `--espeak_data`; Windows Piper cannot find its phoneme tables without it) |
| `speaker_id` | `null` (add `--speaker <id>` for a multi-speaker model) |
| `length_scale` | `1.0` (bigger = slower, clamped to 0.25–4.0) |

**What gets spoken** is cleaned first: fenced code blocks, links (the label
survives, the URL does not), bare URLs, `<tags>`/expression markers, markdown
`*`/`_`/`` ` ``/`#`, emoji and control characters are stripped, and whitespace is
collapsed. If nothing readable is left, no synthesis happens at all — so an
emoji-only or code-only bubble never makes a sound.

**How it plays:** `piper.exe` is spawned with `CREATE_NO_WINDOW` (no console
flash) on the dedicated `avatar-tts` thread, which writes the text to its stdin,
waits, then plays the temp `.wav` through [`rodio`](https://docs.rs/rodio) and
deletes it. `rodio` sits on `cpal`, so playback is WASAPI on Windows,
ALSA/PulseAudio/PipeWire on Linux and CoreAudio on macOS from the same code
path — there is no `PlaySoundW` and no OS audio binding in the crate. One thread
means lines **queue** — they never overlap, and the HTTP server / render loop
never waits on the speakers. The queue holds 8 lines; anything past that is
dropped rather than narrating stale bubbles.

**If Piper is missing** (no `piper.exe`, no `.onnx`, or a bad path) the app logs
one warning at startup, the Voice badge says `missing`, and everything else keeps
working — the avatar is just silent. Mute/unmute from the panel, `GET`/`POST
`/tts`, or the `get_tts_status` / `set_tts_enabled` commands; the mute is runtime
only and is never written back to `watcher_rules.json`.

---

## Running on Linux and macOS

The app was written on Windows, and its OS calls used to sit inline in `lib.rs`.
They now live behind one facade — `src-tauri/src/platform/` — so the rest of the
crate is portable Rust:

| Concern | Windows | Linux | macOS |
| --- | --- | --- | --- |
| Audio out (`tts.rs`) | WASAPI via `rodio` | ALSA / PulseAudio / PipeWire via `rodio` | CoreAudio via `rodio` |
| Mic capture (`stt.rs`) | WASAPI via `cpal` | ALSA / PipeWire via `cpal` | CoreAudio via `cpal` |
| Active window | `GetForegroundWindow` + DWM | `xdotool` / `wmctrl` (X11) | `osascript` + System Events |
| Clipboard | `CF_UNICODETEXT` | `xclip` / `xsel`, `wl-copy` on Wayland | `pbcopy` / `pbpaste` |
| Global cursor + key state | `GetCursorPos` / `GetAsyncKeyState` | `rdev` | `rdev` |
| Global hotkey | `RegisterHotKey` | `global-hotkey` (X11 grab) | `global-hotkey` |

What that buys you, honestly:

* **Voice in and out works everywhere** — the two things the refactor was for.
* **Window context, clipboard and the `Alt+Space` bar work on X11 and macOS**
  (macOS needs the Accessibility permission; grant it once to the app).
* **Wayland is a partial session.** No compositor offers a portable "read the
  focused window's title" API, so `GET /context` reports `"source":"wayland"`
  with empty values and window-rule triggers do not fire. Voice, chat and the
  avatar itself are unaffected.
* **Screen vision still needs Windows.** The watcher's capture is GDI `BitBlt`.
* **Click-through uses Tauri's cross-platform API**, so the overlay behaves the
  same, but the always-on-top/focus tweaks that need `WS_EX_*` and DWM are
  Windows-only by nature.

To type-check the platform layer for another OS without a full Tauri toolchain
(no GTK/WebKit, no C cross-compiler):

```bash
cd tools/platform-check
cargo check --target x86_64-unknown-linux-gnu   # or aarch64-apple-darwin
```

It compiles the real `src-tauri/src/platform/` tree for that target, so a
`windows::` import that leaked into portable code fails there instead of on a
user's machine.

---

## Talking to it (push to talk)

The reverse of the voice: hold **`Alt+V`** anywhere on the desktop — or the
**Hold to talk** button in the control panel — speak, and let go. Capture is
`cpal` (WASAPI / ALSA / CoreAudio), so the same code runs on all three
platforms; the device's own rate is downmixed to mono and resampled to 16 kHz. The clip is filtered by
[Silero VAD](https://github.com/snakers4/silero-vad) and transcribed locally by
[whisper.cpp](https://github.com/ggml-org/whisper.cpp); the text is posted to
`POST /reply` exactly as if you had typed it, so the watcher wakes your agent and
the answer is spoken back by the Piper voice. No network, no API key.

The shipped setup (override in the `stt` block of `watcher_rules.json`):

| Piece | Default |
| --- | --- |
| `hotkey` | `Alt+V` (a **hold**, not a toggle; same grammar as `hotkey`) |
| `whisper_exe_path` | `C:\hermes\tools\whisper.cpp\build\bin\Release\whisper-cli.exe` |
| `whisper_model_path` | `C:\hermes\tools\whisper.cpp\ggml-base.en.bin` |
| `silero_vad_model_path` | `C:\hermes\tools\whisper.cpp\ggml-silero-v6.2.0.bin` (`null` disables VAD) |
| `vad_threshold` | `0.5` (Silero speech probability; clamped to 0.01–0.99) |
| `language` | `en` (or `auto`) |
| `min_speech_ms` | `250` — shorter presses are thrown away as accidental taps |

**What happens on a hold:** the panel's badge goes `listening`, the mic opens
through [`cpal`](https://docs.rs/cpal) at the device's native rate and channel
count (stereo is averaged to mono, then resampled to 16 kHz), and the audio is
buffered while you hold. Releasing stops the device immediately; the badge turns
`transcribing`, the clip is written to
`%TEMP%` and handed to `whisper-cli.exe -m … -f … -l … -nt -np --vad --vad-model …
--vad-threshold …` with `CREATE_NO_WINDOW`. Its stdout is cleaned — `[BLANK_AUDIO]`,
`(music)`, `[inaudible]`, timestamps and control characters are stripped — and an
empty result sends nothing. The temp `.wav` is deleted as soon as Whisper exits.

**It will not transcribe the avatar.** Starting a hold first calls the voice's
`interrupt()`, which stops the line playing and drops everything still queued, so
the microphone only ever hears you. One `avatar-ptt` thread owns the whole state
machine, so a hold and a transcription can never overlap, and the HTTP server
never waits on the microphone.

**If anything is missing** (no `whisper-cli.exe`, no model, no microphone) the app
logs one warning at startup, the badge says `missing`, and the tooltip names the
missing piece. Everything else keeps working. `GET`/`POST /stt`, the
`get_stt_status` / `set_stt_enabled` commands and the `ptt_start` / `ptt_stop`
commands expose the same state; muting is runtime only.

```bash
curl -s localhost:8765/stt                      # settings + idle/listening/transcribing
curl -s -X POST localhost:8765/stt -H 'content-type: application/json' -d '{"enabled": false}'
```

---

## Seeing your screen (local vision)

Ask **"what's on my screen?"** — type it, say it with push-to-talk, or press the
control panel's **What's on screen?** button (the command bar has the same chip).
The watcher screenshots the window you are focused on and answers with a local
vision-language model, so the avatar can tell you which app you are in, read an
error out loud, or describe a dialog. No network, no API key.

It is driven by the `vision` block of `watcher_rules.json`:

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | master switch |
| `profile_name` | `vision` | which `ai_profiles` entry answers — must be an `openai_compat` profile |
| `capture_mode` | `active_window` | `active_window` (the focused window's rect) or `full_screen` |
| `max_image_dim` | `1024` | longest edge after downscaling (clamped 64–4096); smaller = faster, cheaper |
| `jpeg_quality` | `85` | JPEG quality (1–100) |
| `trigger_phrases` | *what's on my screen*, *whats on my screen*, *what is on my screen*, *look at my screen*, *see my screen*, *read my screen*, *check my screen* | case-insensitive; matched against every typed or dictated message |

The image never touches the disk: `GetDC`/`BitBlt`/`GetDIBits` grabs the pixels,
the buffer is downscaled and JPEG-encoded in memory, and the bytes ride one
OpenAI-shaped `chat/completions` request as a `data:image/jpeg;base64,…` URL.
When the focused window *is* the avatar's own overlay, the watcher ignores that
rect and captures the primary monitor instead, so it looks at your work rather
than at itself. `include_screen: true` on `POST /reply` (what the button and chip
send) asks for a screenshot even when the text matches no phrase.

**If anything is missing** — vision disabled, the profile absent, the endpoint
down, or the capture failing (locked desktop, no desktop session) — the watcher
logs it and speaks a short apology; ordinary text replies keep working. Point
`ai_profiles.vision` at any multimodal server, for example:

```json
"vision": {
  "kind": "openai_compat",
  "label": "LFM3-VL-2B (llama.cpp)",
  "endpoint": "http://127.0.0.1:9932",
  "model": "lfm3-vl-2b",
  "max_tokens": 512,
  "timeout_secs": 120
}
```

```bash
# ask with an explicit screenshot, exactly like the panel button
curl -s -X POST localhost:8765/reply -H 'content-type: application/json' \
     -d '{"text":"what is this error?","include_screen":true}'
```

---

## Reading the conversation back

A speech bubble is a glance: it auto-dismisses, and the next reply replaces it.
The **History** drawer keeps the session instead. Open it from the control
panel's **History** button, the **☰** button in the command bar, or with `H` —
`Esc` closes it again. It lists every prompt you sent and every line the avatar
spoke, in order, with a timestamp per turn; a turn that carried a `details`
block (a diff, a log tail, a code snippet) offers its own **Details ▾** and
**Copy**, exactly like the bubble did. The drawer stays clickable over the
click-through desktop while it is open and gives the desktop back when it
closes.

The log is **in memory and bounded to 50 turns** (the oldest is evicted), so a
long session cannot grow it without limit. **New chat** clears it together with
the conversation context, and a restart starts a fresh, empty log. Agents can
read the same list over HTTP:

```bash
curl -s localhost:8765/history
# {"ok":true,"messages":[{"id":1,"role":"user","text":"…","ts_ms":…}, …]}
```

## The polish pass

Four details that make the avatar feel less like a widget:

* **The lips follow the voice, not the typing.** When Piper synthesizes a line,
  the wav's RMS is bucketed every 25 ms, normalized, and handed to the page as
  `tts-envelope`; the mouth indexes that envelope by playback time, so it opens
  on the vowels you actually hear. Mute the voice (or run with Piper missing) and
  the mouth quietly falls back to the per-character mapping, so nothing breaks.
* **The eyes follow your real cursor — everywhere.** The overlay is click-through
  almost everywhere, so the page only ever sees the pointer over its own cards.
  Rust's cursor thread (`GetCursorPos`, already polling every 35 ms for
  click-through) therefore publishes a throttled `cursor-pos` event (3 CSS px
  deadband, ~20 Hz), and the pupils track the OS cursor across the whole virtual
  desktop — over another application, with the overlay unfocused. The head bone
  turns a few degrees toward it too, damped and clamped to its `limits`. Park the
  cursor for 4 s and the gaze goes back to idle glances; an explicit
  `POST /move {"look_at":[x,y]}` always wins.
* **A perch is a place, not a snapshot.** `POST /perch` remembers the window
  (`HWND`), the edge and the offset, and re-reads its bounds every 60 ms: drag
  the window and the avatar walks along with it, resize it and the avatar keeps
  its footing, minimize or close it and the avatar returns to `idle` by itself.
  Grabbing the avatar breaks the perch immediately — the tracker never fights
  your cursor.
* **Reaction clips out of the box.** The shipped rigs carry `nod`, `shake_head`,
  `celebrate`, `alert_ping` and `idle_fidget` beside `wave` and `breathe` (see
  `GET /animations`), so a local model can answer with body language:
  `{"speech": "Nice fix!", "animate": "celebrate"}`.

---

## Pushing vs polling

`GET /context` answers *what is the user doing* (foreground window title, the
executable that owns it, the focused window's `window_rect` and the top five
`visible_windows` — all in CSS pixels — machine-wide idle seconds, plus the last
15 SSE frames as a catch-up). `POST /perch` turns that into a place to stand:
`{"edge":"top_right","window_process":"code"}` walks the avatar onto that
window's top border and keeps its gaze on the window while it sits there — and
the perch is **live**, so dragging or resizing that window slides the avatar
along its border, while minimizing or closing it drops the avatar back to
`idle`. `GET /clipboard` reads the last-copied text (2000 chars max).
`GET /events` is a Server-Sent Events stream of *what the avatar did*: `state`,
`speech`, `speech_cleared`, `model`, `rig_saved`, `pose`, `part_patch`,
`interaction`, `user_reply`, `alert`, `session_reset`, `ai_profile`. An agent can therefore block on the stream
instead of polling `/state`:

```bash
curl -N localhost:8765/events
```

---

## Waking Hermes (the watcher)

`char-rig-watcher.exe` is the third binary built by `cargo build`. It is the
piece that turns the avatar's stream into an agent turn — and it costs **zero
LLM tokens while idle**: with nothing to report it only reads a keep-alive SSE
stream and polls `/context` every 20 s.

You normally do not start it yourself: `avatar-desktop.exe` spawns it at startup
and supervises it (see [Quick start](#quick-start)). The panel's **Watcher** row
has the switch (`running` / `off` / `missing`), and **Logs ↗** opens a pop-out
console with the child's live output — timestamped, 200 lines, plus the rules
path it was started with, **Clear** and **Test ping** (which fires a
`double_click` through SSE → watcher → Hermes → MCP in one click). The same two
things are available over HTTP:

```bash
curl -s localhost:8765/watcher                         # {enabled, running, pid, paths, logs}
curl -s -X POST localhost:8765/watcher -H 'content-type: application/json' \
     -d '{"enabled": false}'                           # stop it, no app restart
```

Run it by hand — with the output in your terminal — by setting
`CHAR_RIG_NO_WATCHER=1` on the app, or just start it directly against a running
avatar:

```bash
cd src-tauri
./target/debug/char-rig-watcher.exe --dry-run        # print triggers, call nothing
./target/debug/char-rig-watcher.exe \
  --hermes-cmd "hermes run {prompt}"                 # wake Hermes on a trigger
```

It subscribes to `GET /events` and polls `GET /context`, and fires only when a
rule matches:

| Trigger | Fires when | Default cooldown |
| --- | --- | --- |
| `interaction` | the avatar is double-clicked or a drag ends (plain `click` only with `--trigger-single-click`) | 15 s |
| `window_title` | the focused window *changes* to a title containing one of `window_keywords` (case-insensitive substring) **or** matching an `app_rules` entry | 300 s |
| `idle_return` | idle seconds drop back below `idle_threshold_secs` after having exceeded it | 300 s |
| `user_reply` | the user types into the speech bubble's quick-reply box (or a client calls `POST /reply`) — **no cooldown**, the user spoke directly | — |
| `alert` | `POST /alert` from an external monitor (Suricata, cron, a build script) | 60 s (`alerts.cooldown_secs`) |

The first `/context` poll is only a baseline: the watcher never fires on the
window it starts with. A trigger then does three things, in order:

1. `POST /emote` an ack — `thinking` by default — so the avatar reacts before
   the agent has finished thinking (`--no-ack-emote` turns this off).
2. `POST` the JSON payload to `--webhook-url`, if set.
3. Spawn `--hermes-cmd` with a one-line prompt carrying `event_type`, `detail`,
   `window_title` and `idle_secs`.

`{prompt}` in the command is substituted; otherwise the prompt is appended as
the last argument. The command line is split on whitespace with `"`/`'` quote
grouping and **no shell** is involved, so the prompt can never be re-read as
shell syntax — use a script if you need pipes or redirection.

Spawning is **single-flight** and headless, because a pile-up of `hermes`
processes is the one failure mode that actually costs money:

* While a dispatch is in flight (a `--hermes-cmd` child still running, or a
  webhook POST still open), only one trigger may wait: a `user_reply` or an
  `alert` is parked in a **one-slot pending queue** (log line
  `[watcher] queued pending trigger (1 slot)`; a newer one replaces the older)
  and runs the moment that turn ends. Routine `interaction`/`window_title`/
  `idle_return` triggers are still **skipped** with a
  `[watcher] debug: dispatch in flight …` line, so background noise never queues
  up behind a task.
* The child is spawned with `stdin` on `/dev/null`, so an interactive prompt
  reads EOF immediately instead of blocking on a keyboard nobody is typing on.
* The prompt ends with a directive telling the agent this is a headless avatar
  trigger: **no** follow-up questions, and it spells out the exact MCP call
  signatures (`mcp__char_rig__avatar_speak(text, expression?, duration_ms?, details?)`,
  `avatar_emote(expression, duration_ms?)` with the six valid expressions,
  `avatar_wave()`, `avatar_animate(name, loop?)`, `avatar_set_clipboard(text)`,
  plus `avatar_get_context()` for the focused window / `process_name` / recent
  events and `avatar_get_clipboard()` for the last-copied text) so the very first
  turn can act without a `tool_describe` round-trip. It also tells the model to
  **chain calls** in one turn (an emote or clip *and* a line of speech), to keep
  the bubble **under 180 characters**, and to put multi-line code, diffs or logs
  in `details` (a scrollable drawer with a Copy button) or on the clipboard. That matters
  most on a local model, where every extra round-trip eats the timeout.
* The wait is wrapped in a timeout (`--hermes-timeout`, default `120` s, also
  `hermes_timeout_secs` in `watcher_rules.json`); a child that outlives it is
  killed and reaped, which also releases the flight permit. The default is
  deliberately generous — a local model usually needs more than a minute to
  finish its turn.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--api-url <URL>` | `$CHAR_RIG_API_URL` or `http://127.0.0.1:8765` | avatar API base |
| `--rules <PATH>` | `./watcher_rules.json`, then next to the exe | rules file (see below) |
| `--context-interval <SECS>` | `20` | how often `GET /context` is polled |
| `--trigger-single-click` | off | also fire on plain `click` interactions |
| `--ack-emote <EXPR>` | `thinking` | expression shown the moment a trigger fires |
| `--no-ack-emote` | — | disable the ack emote entirely |
| `--hermes-cmd <CMD>` | `$CHAR_RIG_HERMES_CMD` | command spawned with the prompt |
| `--hermes-timeout <SECS>` | `120` | kill a spawned `--hermes-cmd` after this long |
| `--webhook-url <URL>` | *(none)* | `POST` the trigger payload here |
| `--dry-run` | off | print payloads on stdout and call nothing |

Precedence is **CLI flag → environment → `watcher_rules.json` → built-in
default** (`hermes_timeout_secs` follows the same order, minus an env var). If
the avatar is not running (or restarts), the watcher reconnects with exponential
backoff from 1 s up to 30 s; a failing context poll logs once and keeps trying.
Diagnostics go to stderr; stdout carries only `--dry-run` payloads, so it stays
pipe-friendly.

`watcher_rules.json` (repo root) tunes the rules; a missing or invalid file
falls back to the built-in defaults shown here:

```json
{
  "interactions": {
    "double_click": true,
    "drag_end": true,
    "click": false,
    "cooldown_secs": 15
  },
  "context": {
    "window_keywords": ["error", "failed", "exception"],
    "idle_threshold_secs": 900,
    "cooldown_secs": 300
  },
  "app_rules": [
    {
      "process": "Code.exe",
      "title_contains": "error",
      "animation": "surprised",
      "prompt_hint": "This is a compile/build error in the user's editor."
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
  "ack_emote": "thinking",
  "hermes_cmd": "hermes chat --continue {session} --create-if-missing -Q -q",
  "hermes_timeout_secs": 120,
  "active_profile": "hermes",
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
  "ai_profiles": {
    "lfm3-fast": {
      "kind": "openai_compat",
      "label": "LFM3 1.7B (llama.cpp)",
      "endpoint": "http://127.0.0.1:8080",
      "model": "lfm3-1.7b",
      "max_tokens": 320,
      "timeout_secs": 45
    },
    "hermes": {
      "kind": "hermes_cli",
      "label": "Hermes CLI",
      "cmd": "hermes chat --continue {session} --create-if-missing -Q -q"
    }
  },
  "webhook_url": null
}
```

`app_rules` entries match the focused window on `process` (`/context`'s
`process_name`, e.g. `Code.exe`) and `title_contains` — both case-insensitive
substrings, blank = wildcard, first match wins. A match supplies the ack
`animation` (an expression name emotes, anything else is played as a keyframe
clip) and a `prompt_hint` that is appended to the Hermes prompt as `Context: …`.
`alerts` throttles `POST /alert` frames from outside (network monitor, cron).

`quick_tasks` is the command bar's chips (`[{label, prompt, profile?,
include_screen?}]`, at most 6) — edit the file, reopen the bar, and the buttons
change, no rebuild. `include_screen: true` makes a chip ask with a screenshot.
`max_session_turns` and `session_idle_reset_secs` rotate the conversation
automatically (after N turns / after a long break), and `session_base` is the id
prefix.

`tts` is the local voice (see [Speaking out loud](#speaking-out-loud-local-voice)):
`enabled`, `piper_exe_path`, `model_path`, `espeak_data_path`, `speaker_id` and
`length_scale`. It is read by the **app** at startup, so changing it needs an app
restart — the panel's switch is the runtime mute.

`vision` is screen vision (see [Seeing your screen](#seeing-your-screen-local-vision)):
`enabled`, `profile_name`, `capture_mode`, `max_image_dim`, `jpeg_quality` and
`trigger_phrases`. It is read by the **watcher**, which reloads the file on an
`ai_profile` frame, so an edited block takes effect without a rebuild.

### Choosing the brain (`ai_profiles`)

A trigger does not have to go to `hermes_cmd`. `ai_profiles` names one or more
inference targets, and `active_profile` says which one answers:

* **`openai_compat`** — any `/v1/chat/completions` server (`llama.cpp`, Ollama,
  LM Studio): `{kind, endpoint, model, api_key?, max_tokens?, timeout_secs?,
  system_prompt?}`. Because the model has no MCP tools — and is deliberately
  never shown the CLI's tool directive — the watcher inlines a fresh
  `GET /context` snapshot and the rig's clip names from `GET /animations` (and
  the clipboard, only when the user's message asks about it), then sends **one**
  request with `response_format: {"type":"json_object"}`, parses
  `{speech, expression?, perch_edge?, window_process?, animate?, wave?, details?,
  escalate?}` and performs the actions itself (`POST /perch` → `POST /emote` →
  `POST /wave` or `POST /animate` → `POST /speak`, bubble cut to 180 chars,
  `details` forwarded). `animate` names one of the rig's clips; `animate: "wave"`
  with no custom `wave` clip — or `wave: true` — uses the built-in `POST /wave`,
  so sprite and procedural models both gesture. `escalate: true` (multi-step or
  external-tool work only) hands the same prompt to the CLI profile; a dead or
  slow server gets the sad fallback bubble instead of a stuck **Thinking…**.
* **`hermes_cli`** — the existing agent runner (`{kind, label, cmd,
  timeout_secs?}`), with `{session}` rotation and the headless directive.

`app_rules[].profile` and `quick_tasks[].profile` pin one trigger to a specific
brain (a fast model for editor noise, the agent for anything with tools). The
panel's **AI Brain** row switches the active profile live — `POST /ai-profile`
persists it to this file and broadcasts an `ai_profile` frame, so the running
watcher adopts it on the **next trigger** without a restart:

```bash
curl -s localhost:8765/ai-profiles                     # profiles + active_health
curl -s -X POST localhost:8765/ai-profile \
     -H 'content-type: application/json' -d '{"name":"lfm3-fast"}'
```

`active_health` is probed on demand (`GET /health`, then `/v1/models`, 800 ms
cap) and reports `online`, `offline` (with the endpoint/error) or `cli` for the
agent path.

The shipped `hermes_cmd` keeps one conversation across triggers and is
**rotatable**: `hermes chat --continue {session} --create-if-missing -Q -q`.
`{session}` becomes the watcher's current id (`char-rig-<epoch>`, then `-2`,
`-3`… after each **New chat**/auto-rotation), `-Q` keeps its banner out of the
watcher's log, and `-q` must stay **last** because the prompt is appended as the
final argv entry. Per-run overrides such as `--run-budget`, `--model` or `-t`
are rejected on a resumed session — put them in `config.yaml` instead.

---

## Hermes / MCP

`cargo build` also produces a second binary, **`char-rig-mcp.exe`** — a stdio MCP
server that exposes the avatar to Hermes (or any MCP client) as fourteen tools:
`avatar_speak`, `avatar_emote`, `avatar_move`, `avatar_perch`, `avatar_wave`,
`avatar_pose`, `avatar_get_context`, `avatar_get_clipboard`,
`avatar_set_clipboard`, `avatar_list_models`, `avatar_switch_model`,
`avatar_list_animations`, `avatar_animate`, `avatar_switch_ai`.
`avatar_get_context` carries the window title, the owning `process_name`, the
foreground `window_rect`, the top five `visible_windows`, idle seconds and the
last 15 events; `avatar_get_clipboard` returns the last-copied text (2000 chars
max) and `avatar_set_clipboard` puts up to 32000 characters back on it — the way
to hand over a full snippet, patch or log.
`avatar_perch(edge, window_process?, offset?)` walks the avatar onto or beside a
window's border (`top_left`, `top_right`, `left`, `right`, `bottom`) and keeps
its gaze on that window, so it can stand next to the editor whose error it is
explaining or on the app an alert came from; `avatar_move` takes an optional
`look_at: [x, y]` to point the eyes at something specific.
`avatar_speak` takes an optional `details` string (8000 chars) that the bubble
renders behind a **Details ▾** toggle with its own **Copy** button.

It is a stateless bridge: each tool call is one HTTP request to the running
`avatar-desktop.exe`, so **start the avatar first**. If it is not running, the
tool returns a readable `isError` result (with a hint) instead of breaking the
session.

Add this to Hermes' `config.yaml` (Windows paths use forward slashes or a
single-quoted literal; both work):

```yaml
mcp_servers:
  char-rig:
    command: 'C:\hermes\char-rig\src-tauri\target\debug\char-rig-mcp.exe'
    args: []
    env:
      CHAR_RIG_API_URL: http://127.0.0.1:8765
```

Then restart Hermes (or reconnect the server) — the thirteen `avatar_*` tools show
up in the tool list. Verify the bridge on its own with:

```bash
python tools/mcp_smoke_test.py
```

which drives the real stdio protocol and asserts the tool schemas, the error
path when the avatar is down, and every tool against a live avatar.

---

## Documentation

| File | Contents |
| --- | --- |
| **[ARCHITECTURE.md](ARCHITECTURE.md)** | System design, threading model, codebase map, rig schema, full API reference, limitations & roadmap |
| [API.md](API.md) | Endpoint reference with payloads and `curl` examples |
| [MODEL_FORMAT.md](MODEL_FORMAT.md) | Model folder layout, manifest schema, editor guide |

---

## Project layout

```
src-tauri/            Rust application (Tauri 2)
  src/lib.rs          windows, state, commands, event bus (SSE), desktop probes, click-through tracker, startup
  src/api.rs          axum HTTP API (own thread + Tokio runtime)
  src/models.rs       model/rig storage, validation, import pipeline
  src/tts.rs          local Piper voice: config, text sanitizing, `avatar-tts` playback thread
  src/stt.rs          push-to-talk: `stt` config, cpal capture (mono downmix), 16 kHz resample, whisper-cli + Silero VAD, `avatar-ptt` worker
  src/platform/       the OS boundary: types, hotkey grammar, win32.rs, unix.rs (Linux/macOS)
  src/settings.rs     GET/POST /config: reads and edits watcher_rules.json by byte span
  src/plugins/
    mod.rs            feature-blind plugin host: Plugin trait, PluginHost, PluginRoute, PluginConfig, registry
    wasm_host.rs      the WebAssembly runtime: scans plugins/*.wasm, sandboxes each instance, proxies routes
    manifest.rs       how the app finds plugins (calls wasm_host::load_all — names nothing)
plugins/              guest plugin crates + the .wasm the app loads at runtime
scripts/build-plugin.sh   builds every plugin for wasm32-unknown-unknown
  src/bin/
    char-rig-mcp.rs   second binary: stdio MCP server exposing the avatar_* tools over HTTP
    char-rig-watcher.rs  third binary: SSE + /context watcher that wakes Hermes on triggers
    capture/mod.rs    Windows GDI screen capture, rect clamping, downscale + in-memory JPEG
  tauri.conf.json     transparent always-on-top window, frontendDist = ../ui
ui/                   modular frontend (native ES modules, no build step)
  index.html          lean HTML shell (titlebar, canvas, panels, bubble, toasts)
  styles.css          all CSS rules
  js/
    ipc.js            Tauri access (call/listen/isTauri), read lazily from window.__TAURI__
    state.js          shared state & constants (avatar, RT, speech, gaze, viewport, EXPRESSIONS)
    rig.js            model ingestion, skeletal FK (incl. clip offsets + hover head tilt), part geometry, hit regions, serialization
    animator.js       custom keyframe clips: sampling, playback, authoring, finished state
    renderer.js       canvas rendering pipeline, gaze→pupil offset, lip-sync mouth squash
    editor.js         rig editor gizmos, inspector, bone/part authoring, undo
    speech.js         speech bubble lifecycle, text-driven lip sync, quick-reply box, details drawer
    bridge.js         Tauri events, state reports, import flow, click-through
    main.js           entry point: DOM bindings, shortcuts, gaze targeting, __avatarDebug, main loop, click detection
models/               rigs + sprites (+ .state.json remembering the selection)
watcher_rules.json    optional trigger rules for char-rig-watcher.exe (built-in defaults when absent)
docs/                 screenshots
tools/                verify_docs.py (docs↔source diff), cdp-eval.mjs (evaluate JS in the running page)
```

The legacy prototypes (`ai-avatar-bridge/`, `avatar-desktop/`, `avatar-editor/`,
`spikes/`, the unused `src-tauri/avatar-renderer.html` and the empty
`src/char_rig/`, `tests/` and `assets/` placeholders) have been **deleted** —
the tree above is the whole app.

## Status

Prototype / hobby project. Click-through and desktop context are portable
(Win32 on Windows, `xprop`/`xdotool`/`osascript` on Linux/macOS),
unauthenticated localhost API. The overlay spans the **whole virtual desktop**
(mixed per-monitor DPI scaling is the remaining rough edge). Live events over
SSE; polling `/state` still works and is still the simplest read path. See
*Known limitations* in [ARCHITECTURE.md](ARCHITECTURE.md) before depending on it.
