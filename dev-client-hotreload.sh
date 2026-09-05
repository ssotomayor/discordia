#!/usr/bin/env bash
# dx serve, for UI iteration only. dx writes its own Info.plist into the .app it
# runs and ignores [bundle.macos] info_plist_path, so the webview keeps ATS at
# its default and cannot open a cleartext SFU (OPEN 81).
set -euo pipefail
cd "$(dirname "$0")"
if [[ "${FAST:-0}" == "1" ]]; then export LIVEKIT_BUNDLE_SKIP=1; fi

if [[ "$(uname -s)" == "Darwin" ]]; then
  cat <<'EOF'
dx serve — hot reload on, media half-blocked against a ws:// SFU:

  works     voice audio, and publishing your screen   (native SDK, no ATS)
  broken    seeing anyone's screen or camera, and     (webview; ATS refuses
            publishing your own camera                 cleartext — OPEN 81)

A wss:// SFU is unaffected. Full media: ./dev-client.sh

EOF
fi

exec dx serve --package dioxusfun
