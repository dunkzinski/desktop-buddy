#!/usr/bin/env bash
# verify-install.sh - read-only health check for a desktop-buddy build.
#
#   ./verify-install.sh                 # verify $HOME/desktop-buddy
#   ./verify-install.sh --dir /opt/buddy
#   ./verify-install.sh --skip-tests
#
# Exit codes: 0 every required check passed, 1 something failed.
#
# Deliberately has no jq dependency (a fresh machine has none): the API fields
# are matched with grep, and the JSON shapes are documented in API.md.
# The MCP handshake check lives in scripts/mcp-probe.ps1 and is driven by
# verify-install.ps1 - there is no bash equivalent here.

set -uo pipefail

INSTALL_DIR="${HOME}/desktop-buddy"
API_URL="http://127.0.0.1:8765"
SKIP_TESTS=false
FAILURES=0
WARNINGS=0

usage() {
    sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dir|--install-dir) INSTALL_DIR="${2:?--dir needs a value}"; shift 2 ;;
        --api) API_URL="${2:?--api needs a value}"; shift 2 ;;
        --skip-tests) SKIP_TESTS=true; shift ;;
        -h|--help) usage 0 ;;
        -*) echo "unknown option: $1" >&2; usage 1 ;;
        *) INSTALL_DIR="$1"; shift ;;
    esac
done

case "${OSTYPE:-}" in
    msys*|cygwin*) EXE=".exe" ;;
    *)             EXE="" ;;
esac

pass() { printf '  \033[1;32m[ OK ]\033[0m %s\n' "$*"; }
fail() { printf '  \033[1;31m[FAIL]\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
warn() { printf '  \033[1;33m[WARN]\033[0m %s\n' "$*"; WARNINGS=$((WARNINGS + 1)); }
info() { printf '         %s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

INSTALL_DIR="${INSTALL_DIR%/}"
SRC_TAURI="$INSTALL_DIR/src-tauri"
DEBUG="$SRC_TAURI/target/debug"

echo "=== desktop-buddy installation verification ==="
echo "install dir: $INSTALL_DIR"
echo

echo "-- source tree"
[[ -d "$INSTALL_DIR/.git" ]] && pass "git checkout present" || fail "no .git at $INSTALL_DIR"
for name in README.md INSTALL.md install.ps1 verify-install.ps1 scripts/build-plugin.sh scripts/mcp-probe.ps1; do
    [[ -f "$INSTALL_DIR/$name" ]] && pass "$name present" || fail "$name missing"
done
[[ -f "$SRC_TAURI/Cargo.toml" ]] && pass "src-tauri/Cargo.toml present" || fail "src-tauri/Cargo.toml missing"

echo
echo "-- binaries"
for bin in desktop-buddy desktop-buddy-mcp desktop-buddy-watcher; do
    path="$DEBUG/${bin}${EXE}"
    if [[ -f "$path" ]]; then
        pass "$(basename "$path") ($(du -h "$path" | cut -f1))"
    else
        fail "$(basename "$path") missing (run ./install.sh or cargo build --bins)"
    fi
done

echo
echo "-- Wasm plugin guests"
plugin_count=0
for dir in "$INSTALL_DIR"/plugins/*/; do
    [[ -f "${dir}Cargo.toml" ]] || continue
    name="$(basename "$dir")"
    wasm="$INSTALL_DIR/plugins/$name.wasm"
    if [[ -f "$wasm" ]]; then
        pass "$name.wasm ($(du -h "$wasm" | cut -f1))"
        plugin_count=$((plugin_count + 1))
    else
        fail "$name.wasm missing (run ./scripts/build-plugin.sh)"
    fi
done
[[ $plugin_count -gt 0 ]] || fail "no plugin crates found under plugins/"

echo
if [[ "$SKIP_TESTS" == true ]]; then
    warn "tests skipped (--skip-tests)"
else
    echo "-- tests (cargo test --lib --bins --tests)"
    if ( cd "$SRC_TAURI" && cargo test --lib --bins --tests ); then
        pass "the whole suite passed (lib + both bins + tests/)"
    else
        fail "cargo test failed (exit $?)"
    fi
fi

echo
echo "-- Hermes wiring"
if ! have hermes; then
    warn "the hermes command is not on PATH; skipping the MCP registration checks"
else
    list_text="$(hermes mcp list 2>&1 || true)"
    if printf '%s' "$list_text" | grep -Eq '^[[:space:]]+desktop-buddy[[:space:]]{2,}'; then
        pass "desktop-buddy is registered (hermes mcp list)"
    else
        fail "desktop-buddy is not registered; run install.ps1"
    fi
fi

echo
echo "-- live API"
health="$(curl -s -m 3 "$API_URL/health" || true)"
if [[ -z "$health" ]]; then
    warn "app not running ($API_URL/health did not answer) - start $DEBUG/desktop-buddy${EXE}"
else
    if printf '%s' "$health" | grep -q '"ok":true'; then
        pass "/health answered ok: true"
        context="$(curl -s -m 3 "$API_URL/context" || true)"
        process_name="$(printf '%s' "$context" | grep -o '"process_name":"[^"]*"' | head -1 | cut -d'"' -f4)"
        if printf '%s' "$context" | grep -q '"process_name"'; then
            pass "/context reports process_name '${process_name:-<none>}'"
        else
            fail "/context has no process_name field (API shape changed)"
        fi
    else
        fail "/health answered but not with ok:true: $health"
    fi
fi

echo
echo "-- MCP bridge"
info "the stdio handshake is checked by verify-install.ps1 (scripts/mcp-probe.ps1);"
info "it is the only proof that does not just spawn a private copy of the bridge"

echo
if [[ $FAILURES -eq 0 ]]; then
    printf '\033[1;32mAll checks passed (%d warning(s)).\033[0m\n' "$WARNINGS"
    exit 0
fi
printf '\033[1;31m%d check(s) failed, %d warning(s).\033[0m\n' "$FAILURES" "$WARNINGS"
exit 1
