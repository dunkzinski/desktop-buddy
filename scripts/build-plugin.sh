#!/usr/bin/env bash
# Build every guest plugin and drop its artifact into ./plugins/.
#
# Each plugin under plugins/<name>/ is its own Cargo workspace (see its
# Cargo.toml), so the app crate never links it. This script is the only build
# step: after it runs, restarting the app is enough to pick the plugin up.
#
#   ./scripts/build-plugin.sh              # every plugin
#   ./scripts/build-plugin.sh survival      # just one
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="wasm32-unknown-unknown"
ONLY="${1:-}"

cd "$ROOT"

mapfile -t DIRS < <(find plugins -mindepth 1 -maxdepth 1 -type d | sort)
if [ "${#DIRS[@]}" -eq 0 ]; then
  echo "no plugin crates found under plugins/" >&2
  exit 1
fi

for dir in "${DIRS[@]}"; do
  name="$(basename "$dir")"
  if [ -n "$ONLY" ] && [ "$ONLY" != "$name" ]; then
    continue
  fi

  echo "==> building $name ($TARGET)"
  ( cd "$dir" && cargo build --release --target "$TARGET" )

  artifact="$(find "$dir/target/$TARGET/release" -maxdepth 1 -name '*.wasm' | head -n 1)"
  if [ -z "$artifact" ]; then
    echo "!! $name produced no .wasm" >&2
    exit 1
  fi

  cp "$artifact" "plugins/$name.wasm"
  echo "    -> plugins/$name.wasm ($(wc -c < "plugins/$name.wasm") bytes)"
done

echo
echo "done. restart avatar-desktop.exe (no rebuild) to load the plugins."
