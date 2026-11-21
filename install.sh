#!/usr/bin/env bash
# install.sh - build desktop-buddy from source on Linux / WSL / git-bash.
#
# Windows is the supported platform. Run install.ps1 there: it also registers the
# MCP bridge with Hermes, which this script deliberately does not do (it would
# have to hand-edit a YAML file, and `hermes mcp add` is the only safe writer).
#
# On Linux this compiles the portable layers, but the app's OS features are
# Windows implementations (focused-window context, screen vision, global
# hotkeys), so a Linux build is a build - not a supported desktop. Upstream CI
# does not build for Linux either; treat a green run here as "it compiles".
#
#   ./install.sh                      # build into $HOME/desktop-buddy
#   ./install.sh --dir /opt/buddy     # build somewhere else
#   ./install.sh --skip-prereqs       # do not touch rustup/apt
#   ./install.sh --with-system-deps   # apt-get the Tauri build deps (Debian/Ubuntu)
#
# Exit codes: 0 built and verified, 1 a step failed.

set -euo pipefail

INSTALL_DIR="${HOME}/desktop-buddy"
REPO_URL="https://github.com/dunkzinski/desktop-buddy"
BRANCH="main"
SKIP_PREREQS=false
WITH_SYSTEM_DEPS=false

usage() {
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

# Flags and the optional positional directory are accepted in any order; the old
# version read $1 as the directory first, which silently swallowed "--force".
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dir|--install-dir) INSTALL_DIR="${2:?--dir needs a value}"; shift 2 ;;
        --repo) REPO_URL="${2:?--repo needs a value}"; shift 2 ;;
        --branch) BRANCH="${2:?--branch needs a value}"; shift 2 ;;
        --skip-prereqs) SKIP_PREREQS=true; shift ;;
        --with-system-deps) WITH_SYSTEM_DEPS=true; shift ;;
        -h|--help) usage 0 ;;
        -*) echo "unknown option: $1" >&2; usage 1 ;;
        *) INSTALL_DIR="$1"; shift ;;
    esac
done

log()  { printf '\n\033[1;36m=== %s ===\033[0m\n' "$*"; }
step() { printf '  \033[1;32m>\033[0m %s\n' "$*"; }
warn() { printf '  \033[1;33m!\033[0m %s\n' "$*"; }
die()  { printf '  \033[1;31mx\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

case "${OSTYPE:-}" in
    msys*|cygwin*) PLATFORM="windows-bash"; EXE=".exe" ;;
    linux*)        PLATFORM="linux";        EXE="" ;;
    darwin*)       die "macOS is not supported: nothing here has been built or tested on it. Use a Windows machine, or build from source and expect to fix OS-level code yourself." ;;
    *)             die "unknown platform (OSTYPE='${OSTYPE:-}'). Windows: use install.ps1." ;;
esac

log "desktop-buddy build ($PLATFORM)"
step "target: $INSTALL_DIR"

# --- system dependencies (Linux only) --------------------------------------
if [[ "$PLATFORM" == "linux" && "$SKIP_PREREQS" == false ]]; then
    log "System dependencies"
    if [[ "$WITH_SYSTEM_DEPS" == true ]]; then
        have apt-get || die "--with-system-deps needs apt-get (Debian/Ubuntu)"
        step "apt-get install Tauri build dependencies"
        sudo apt-get update
        sudo apt-get install -y --no-install-recommends \
            build-essential curl wget file pkg-config \
            libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
            librsvg2-dev libsoup-3.0-dev javascriptcoregtk-4.1-dev
    elif ! pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
        die "the Tauri build needs webkit2gtk-4.1 and its friends. Re-run with --with-system-deps, or install them yourself:
       sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev"
    else
        step "webkit2gtk-4.1 found"
    fi
fi

# --- toolchain --------------------------------------------------------------
if [[ "$SKIP_PREREQS" == false ]]; then
    log "Rust toolchain"
    if ! have cargo; then
        step "installing Rust via rustup"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
        # shellcheck disable=SC1091
        source "$HOME/.cargo/env"
    fi
    step "$(cargo --version)"
    have rustup || die "rustup is missing; the wasm32 target cannot be added"
    if rustup target list --installed | grep -q '^wasm32-unknown-unknown$'; then
        step "wasm32-unknown-unknown present"
    else
        step "rustup target add wasm32-unknown-unknown"
        rustup target add wasm32-unknown-unknown
    fi
    have git || die "git is missing"
    step "$(git --version)"
fi

# --- checkout ---------------------------------------------------------------
log "Checkout"
if [[ -d "$INSTALL_DIR/.git" ]]; then
    git -C "$INSTALL_DIR" status --porcelain | grep -q . && \
        die "$INSTALL_DIR has uncommitted changes; refusing to update it"
    step "updating the existing checkout"
    git -C "$INSTALL_DIR" fetch origin "$BRANCH"
    git -C "$INSTALL_DIR" checkout "$BRANCH"
    git -C "$INSTALL_DIR" pull --ff-only origin "$BRANCH"
elif [[ -e "$INSTALL_DIR" ]]; then
    die "$INSTALL_DIR exists and is not a git checkout; move it aside or pass --dir"
else
    step "git clone --branch $BRANCH $REPO_URL $INSTALL_DIR"
    git clone --branch "$BRANCH" "$REPO_URL" "$INSTALL_DIR"
fi

cd "$INSTALL_DIR"
[[ -f src-tauri/Cargo.toml ]] || die "$INSTALL_DIR does not look like desktop-buddy"

# --- Wasm guests ------------------------------------------------------------
log "Wasm plugin guests"
step "./scripts/build-plugin.sh (plugins/*.wasm are build artifacts, not in git)"
./scripts/build-plugin.sh

shopt -s nullglob
guests=(plugins/*.wasm)
shopt -u nullglob
(( ${#guests[@]} > 0 )) || die "the plugin build produced no plugins/*.wasm"

# --- app --------------------------------------------------------------------
log "App binaries"
( cd src-tauri && cargo build --bins )

for bin in desktop-buddy desktop-buddy-mcp desktop-buddy-watcher; do
    path="src-tauri/target/debug/${bin}${EXE}"
    [[ -f "$path" ]] || die "missing binary after the build: $path"
    step "$path ($(du -h "$path" | cut -f1))"
done

log "Tests (cargo test --lib --bins --tests)"
( cd src-tauri && cargo test --lib --bins --tests )

# --- what this script does not do ------------------------------------------
log "Hermes wiring"
if [[ "$PLATFORM" == "windows-bash" ]]; then
    warn "not attempted from bash: run ./install.ps1 so 'hermes mcp add' writes the config"
else
    warn "not attempted on Linux: the bridge is a Windows app's MCP server; install on Windows for Hermes"
fi

log "Build complete"
echo
echo "Next:"
echo "  $INSTALL_DIR/src-tauri/target/debug/desktop-buddy${EXE}"
echo "  curl -s http://127.0.0.1:8765/health    # expect {\"ok\":true,...}"
echo "  ./verify-install.sh --dir $INSTALL_DIR"
