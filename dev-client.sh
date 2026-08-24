#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
if [[ "${FAST:-0}" == "1" ]]; then export LIVEKIT_BUNDLE_SKIP=1; fi
if command -v npx >/dev/null 2>&1; then
  (cd client && npx @tailwindcss/cli -i assets/tailwind.css -o assets/tailwind.out.css --minify)
else
  echo "warn: npx not found — using committed tailwind.out.css" >&2
fi
cargo run -p dioxusfun
