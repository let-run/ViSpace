#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT=8787
OUTPUT="$ROOT/dist/web"
BUILD=1
SERVE=1
while [ "$#" -gt 0 ]; do
  case "$1" in
    --build-only) SERVE=0 ;;
    --serve-only) BUILD=0 ;;
    --output-dir) shift; OUTPUT="${1:?--output-dir requires a directory}" ;;
    --port) shift; PORT="${1:?--port requires a number}" ;;
    *) echo "Usage: $0 [--build-only | --serve-only] [--port 8787] [--output-dir DIR]"; exit 2 ;;
  esac
  shift
done
if [ "$BUILD" = 1 ]; then
  BINDGEN="$ROOT/.tools/bin/wasm-bindgen"
  if [ ! -x "$BINDGEN" ]; then BINDGEN="$(command -v wasm-bindgen || true)"; fi
  if [ -z "$BINDGEN" ]; then
    echo 'Install the matching packaging tool: cargo install wasm-bindgen-cli --version 0.2.130 --locked'
    exit 1
  fi
  (cd "$ROOT" && cargo build --locked --lib --target wasm32-unknown-unknown --profile web)
  mkdir -p "$OUTPUT/pkg"
  "$BINDGEN" "$ROOT/target/wasm32-unknown-unknown/web/vispace.wasm" --target web --out-dir "$OUTPUT/pkg" --no-typescript
  cp "$ROOT/web/index.html" "$ROOT/web/worker.js" "$ROOT/web/host.js" "$ROOT/web/sync-client.js" "$ROOT/web/network-auth.js" "$ROOT/web/sync-ui.js" "$ROOT/web/storage-client.js" "$ROOT/web/storage-worker.js" "$ROOT/web/workspace-access.js" "$ROOT/web/git-snapshot.js" "$ROOT/web/git-cache-cleanup.js" "$ROOT/web/git-io.js" "$OUTPUT/"
  cp "$ROOT/assets/app-icon.png" "$OUTPUT/app-icon.png"
  mkdir -p "$OUTPUT/sqlite"
  cp "$ROOT/web/sqlite/sqlite3.mjs" "$ROOT/web/sqlite/sqlite3.wasm" "$OUTPUT/sqlite/"
  echo "Browser build: $OUTPUT"
fi
if [ "$SERVE" = 1 ]; then
  exec python3 "$ROOT/script/serve_web.py" --directory "$OUTPUT" --port "$PORT"
fi
