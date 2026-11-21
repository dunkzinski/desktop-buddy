# Installing desktop-buddy

This document is written for an AI agent that has to install the app on a
machine it does not know, and for the human watching it. Every command here was
run on the revision that ships it; every "expected" value is copied from a real
run, not from reading the source.

**"Install" means: build the app from source and register its MCP bridge with
Hermes.** There is no MSI/NSIS bundle yet (see [What is not
included](#what-is-not-included)).

---

## Platform support

| Platform | Status |
| --- | --- |
| Windows 11 (x64) | **Supported.** Everything below applies. |
| Windows 10 | Should work; WebView2 and the Rust MSVC toolchain are the only OS requirements. Not what CI runs. |
| Linux / WSL | `install.sh` compiles the app, but the desktop features (focused-window context, screen vision, global hotkeys) are Windows implementations. Treat it as "it builds", not "it works". |
| macOS | **Not supported.** Nothing has been built or tested there. `install.sh` refuses to run. |

---

## Requirements

| Tool | Needed for | Check | Install |
| --- | --- | --- | --- |
| Rust (MSVC) 1.85+ | the app | `rustc --version` | `winget install Rustlang.Rust.MSVC` |
| `wasm32-unknown-unknown` target | the two plugin guests | `rustup target list --installed` | `rustup target add wasm32-unknown-unknown` |
| Git | the checkout | `git --version` | `winget install Git.Git` |
| WebView2 runtime | the app window | `Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'` | ships with Windows 11 |
| `winget` | only if a prerequisite is missing | `winget --version` | "App Installer" from the Microsoft Store |

You do **not** need `bash`, `gh`, or an authenticated GitHub account: the
repository is public and the installer drives `cargo` directly.

---

## One command

```powershell
iwr https://raw.githubusercontent.com/dunkzinski/desktop-buddy/main/install.ps1 -UseBasicParsing | iex
```

That form takes the defaults. To pass options you need the file locally:

```powershell
.\install.ps1 -InstallDir D:\tools\desktop-buddy -SkipHermes -SkipTests
```

| Option | Effect |
| --- | --- |
| `-InstallDir <path>` | Where to clone. Default `C:\hermes\desktop-buddy`. |
| `-RepoUrl <url>` | Clone from a fork, or a local path such as `C:/src/desktop-buddy` for an offline test (`git clone` accepts a directory; `file:///C:/...` is misread by Git for Windows as `/C:/...`). |
| `-Branch <name>` | Branch to check out. Default `main`. |
| `-SkipPrereqs` | Do not probe/install Rust, Git or the wasm target. |
| `-SkipHermes` | Build only; do not touch the Hermes config or gateway. |
| `-SkipTests` | Skip `cargo test` (not recommended). |
| `-NoLaunch` | Do not start the app at the end. |
| `-Force` | Re-clone over an existing **clean** checkout. |

The installer refuses to delete a directory that has uncommitted changes, is a
checkout of a different repository, or is not a git checkout at all - so pointing
it at a working copy cannot destroy work.

Exit code `0` means the build succeeded **and** the MCP bridge answered. `1`
means it finished with warnings: read the `!` lines.

---

## What the installer does

1. **Target directory.** Reuses a clean checkout (`git pull --ff-only`) instead
   of clobbering it; refuses a dirty or foreign directory.
2. **Prerequisites.** Probes with `Get-Command`, installs what is missing by
   hardcoded winget id (`Rustlang.Rust.MSVC`, `Git.Git`), re-probes after
   refreshing PATH, and stops with instructions if the shell still cannot see it.
3. **Clone.**
4. **Plugin guests.** `plugins/*.wasm` are build artifacts and are gitignored, so
   a fresh clone has none: each `plugins/<name>/` workspace is built with
   `cargo build --release --target wasm32-unknown-unknown` and the artifact is
   copied to `plugins/<name>.wasm`.
5. **The three binaries.** `cargo build --bins` in `src-tauri`, then each binary
   is checked on disk. The running app is shut down first (`POST /shutdown`), and
   if the Hermes gateway is holding `desktop-buddy-mcp.exe` open it is renamed
   aside (Windows allows renaming a running image, not overwriting it).
6. **Tests.** `cargo test --lib --bins --tests` - see [The test
   suite](#the-test-suite) for why that exact selector.
7. **Hermes registration.** `hermes mcp add desktop-buddy --command <exe> --env
   DESKTOP_BUDDY_API_URL=http://127.0.0.1:8765`, with `y` on stdin. Hermes owns
   the config format; nothing here hand-edits `config.yaml`. The registration is
   then re-read from `hermes mcp list` and the config text.
8. **Gateway restart.** The gateway is the scheduled task `\Hermes_Gateway`
   (its action is `wscript ...\gateway-service\Hermes_Gateway.vbs`). The task is
   queried first - `schtasks` exits non-zero for an unknown name and does not
   throw - then ended and started, with both exit codes checked.
9. **Launch.** Starts `desktop-buddy.exe` and waits for `/health` to answer.
10. **Bridge proof.** Speaks MCP to `desktop-buddy-mcp.exe` over stdio
    (`scripts/mcp-probe.ps1`): `initialize`, `tools/list` (expects 14 tools), then
    a real `tools/call avatar_get_context`.

### Why the last step is not `hermes mcp test`

`hermes mcp test desktop-buddy` spawns its *own* copy of the bridge, so it can be
green while the gateway is still holding a dead child from before the rebuild.
The probe talks to the binary over the same stdio protocol the gateway uses, and
reports exactly what came back.

**The one thing no script can prove** is that the *gateway* picked the server up.
Only a tool call made through the agent's own tool surface does that:

```
mcp__desktop_buddy__avatar_get_context    (Hermes)
```

Call it once after an install. If it answers, the wiring is real.

---

## The registered MCP server

`hermes mcp add` writes this, and this is what `hermes mcp list` should show:

```yaml
mcp_servers:
  desktop-buddy:
    command: C:\hermes\desktop-buddy\src-tauri\target\debug\desktop-buddy-mcp.exe
    env:
      DESKTOP_BUDDY_API_URL: http://127.0.0.1:8765
    enabled: true
```

`DESKTOP_BUDDY_API_URL` is optional; the bridge defaults to
`http://127.0.0.1:8765`. It only matters if the app listens elsewhere.

The bridge serves 14 `avatar_*` tools: `avatar_speak`, `avatar_emote`,
`avatar_move`, `avatar_perch`, `avatar_wave`, `avatar_pose`,
`avatar_get_context`, `avatar_get_clipboard`, `avatar_set_clipboard`,
`avatar_list_models`, `avatar_switch_model`, `avatar_switch_ai`,
`avatar_list_animations`, `avatar_animate`.

The bridge needs the **app running**; with no app, every tool still answers
(that is how you tell "wired but no app" from "not wired").

---

## Verification

```powershell
.\verify-install.ps1                      # read-only, exits 0/1
.\verify-install.ps1 -SkipTests -SkipProbe
```

```bash
./verify-install.sh --dir /path/to/checkout
```

| Check | Proves |
| --- | --- |
| source tree | the checkout is complete (docs, scripts, probe) |
| binaries | all three were built, with sizes |
| plugin guests | each `plugins/<name>.wasm` exists |
| tests | `cargo test --lib --bins --tests` exits 0 |
| Hermes wiring | `desktop-buddy` is a row in `hermes mcp list`; `DESKTOP_BUDDY_API_URL` is in `config.yaml`; `\Hermes_Gateway` exists |
| live API | `/health` returns `ok: true`; `/context` has the expected fields |
| MCP bridge | stdio handshake, 14 tools, `avatar_get_context` answers |

Warnings (app not running, no Hermes, no gateway task) do not fail the run;
anything that means "this install cannot work" does.

### Live API shapes

`curl -s http://127.0.0.1:8765/health`

```json
{"ok":true,"service":"desktop-buddy avatar api","version":"0.2.0","uptime_secs":42}
```

Note `ok`, not `status`.

`curl -s http://127.0.0.1:8765/context` returns, among others: `ok`,
`window_title`, `process_name`, `idle_secs`, `window_rect`, `foreground_hwnd`,
`visible_windows`, `screen`, `avatar_state`, `recent_events`, `source`. The
foreground process is `process_name` (not `focused_window.process`) and the idle
counter is `idle_secs` (not `idle_seconds`).

---

## The test suite

`cargo test --lib --bins --tests` runs five targets:

| Target | Tests |
| --- | --- |
| `src/lib.rs` | 190 passed, 1 ignored |
| `src/main.rs` | 0 (it is a thin `main`) |
| `src/bin/desktop-buddy-mcp.rs` | 12 |
| `src/bin/desktop-buddy-watcher.rs` | 137 |
| `tests/chat_and_cursor_contract.rs` | 3 |

That is **342 passing tests and 1 ignored**; `137` is only the watcher binary's
share. Do not run a bare `cargo test`: the doctest stage needs `rustdoc.exe`,
which this toolchain does not ship, and it exits 1 after every real test has
already passed. `--lib`/`--bins` alone would also skip `tests/`, which is where
the regression tests that read shipped JS and docs live.

---

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| `Access is denied (os error 5)` on the build | the app, the watcher, or the gateway's bridge holds the binary | the installer handles this; by hand: `curl -s -X POST http://127.0.0.1:8765/shutdown`, and rename `desktop-buddy-mcp.exe` to `.stale` if the gateway holds it |
| `0xc0000139` (STATUS_ENTRYPOINT_NOT_FOUND) on a test run | test binary without the app's Windows manifest (comctl32 v6) | already handled in `build.rs`; if it reappears the manifest link was dropped |
| MCP tools never appear | the gateway never restarted, or it holds a dead child | `schtasks /End /TN '\Hermes_Gateway'` then `/Run`, or `hermes gateway restart`; then a real `avatar_get_context` call |
| `hermes mcp add` prints "Cancelled." and removes nothing | it asks "Enable all N tools?" and treats end-of-input as cancel, exiting 0 | feed it an answer: `'y' \| hermes mcp add ...` (the installer does this) |
| `winget` installed Rust but `cargo` is not found | the shell kept its old PATH | open a new terminal, or refresh PATH from the registry |
| The installer stops with "syntax error / unexpected token" after you edit it | the file must stay **pure ASCII**: PowerShell 5.1 decodes a BOM-less file as the ANSI code page, and a non-ASCII glyph becomes a smart quote that ends a string | keep `install.ps1`, `verify-install.ps1` and `scripts/mcp-probe.ps1` ASCII-only |
| Plugins do not load | `plugins/*.wasm` missing (gitignored) | re-run the plugin build step: `.\install.ps1` or `scripts/build-plugin.sh` |
| `build-plugin.sh` fails with "bash: command not found" | `bash` is not on PATH on a fresh Windows install (Git puts `cmd\`, not `bin\`, on PATH) | do not use the shell script on Windows; the installer drives `cargo` directly |

---

## Updating an existing install

```powershell
.\install.ps1            # clean checkout: git pull --ff-only, rebuild, re-verify
```

The installer reuses a clean checkout rather than cloning over it. To rebuild by
hand:

```powershell
cd C:\hermes\desktop-buddy
git pull --ff-only
.\scripts\build-plugin.sh      # or the installer's cargo-only path
cd src-tauri
cargo build --bins
cargo test --lib --bins --tests
```

Then restart the app, restart the gateway, and call `avatar_get_context` again -
a rebuilt bridge only reaches the agent after the gateway respawns it.

---

## Uninstalling

```powershell
hermes mcp remove desktop-buddy     # may ask for confirmation
```

Then delete the checkout. The app keeps its own state under
`%LOCALAPPDATA%\desktop-buddy` (and migrates a pre-rename `%LOCALAPPDATA%\char-rig`
once); nothing outside those two directories and the MCP registration is touched.

---

## What is not included

- **No packaged installer** (MSI/NSIS/exe bootstrap): this builds from source and
  needs a Rust toolchain. Auto-update and code signing are still open.
- **No Linux or macOS desktop**: the OS layer has Windows implementations for
  focused-window context, screen vision and global hotkeys. `install.sh` builds,
  it does not deliver the product.
- **No voice models**: Piper and whisper.cpp are optional; the app runs silently
  without them.

---

## Files

| File | Purpose |
| --- | --- |
| `install.ps1` | Windows: prerequisites, build, Hermes wiring, bridge proof |
| `verify-install.ps1` | Windows: read-only verification, exit 0/1 |
| `install.sh` | Linux / WSL / git-bash: build only |
| `verify-install.sh` | Linux / WSL / git-bash: read-only verification |
| `scripts/build-plugin.sh` | builds both Wasm guests (needs bash) |
| `scripts/mcp-probe.ps1` | the MCP stdio handshake probe the installers use |
| `scripts/` (other) | maintenance helpers; not needed to install |

Licensed Apache-2.0, like the rest of the repository - see [LICENSE](LICENSE).
