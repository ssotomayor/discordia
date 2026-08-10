#!/usr/bin/env bash
# Regenerate Tailwind CSS (if npx is available) then run the Dioxus client.
# The committed tailwind.out.css is used as a fallback when Node.js isn't
# installed — cargo build/check don't need it, only regeneration does.
set -euo pipefail
cd "$(dirname "$0")"
if [[ "${FAST:-0}" == "1" ]]; then export LIVEKIT_BUNDLE_SKIP=1; fi
if command -v npx >/dev/null 2>&1; then
  (cd client && npx @tailwindcss/cli -i assets/tailwind.css -o assets/tailwind.out.css --minify)
else
  echo "warn: npx not found — using committed tailwind.out.css" >&2
fi
cargo run -p dioxusfun
