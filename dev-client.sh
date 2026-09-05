#!/usr/bin/env bash
# cargo run, not dx serve: build.rs links Info.plist into the binary, so the
# webview gets the ATS exception a self-hosted ws:// SFU needs (OPEN 81).
set -euo pipefail
cd "$(dirname "$0")"
if [[ "${FAST:-0}" == "1" ]]; then export LIVEKIT_BUNDLE_SKIP=1; fi

echo "client: cargo run — every media path works, no hot reload."
echo "        UI iteration instead: ./dev-client-hotreload.sh"
echo

exec cargo run -p dioxusfun
