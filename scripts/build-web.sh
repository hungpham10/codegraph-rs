#!/bin/sh
# Build web UI (SvelteKit SPA) và ghi asset vào crate `codegraph-web` để
# rust-embed nhúng vào binary. Chạy trước `cargo build`/release.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WEB="$ROOT/web"
ASSETS="$ROOT/crates/codegraph-web/assets"

if ! command -v npm >/dev/null 2>&1; then
  echo "npm not found — install Node.js (https://nodejs.org) first." >&2
  exit 1
fi

echo "==> npm ci (web)"
npm --prefix "$WEB" ci --no-audit --no-fund

echo "==> svelte-check"
npm --prefix "$WEB" run check

echo "==> vite build → $ASSETS"
npm --prefix "$WEB" run build

echo "==> done. Assets in $ASSETS"
