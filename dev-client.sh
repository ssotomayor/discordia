#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
if [[ "${FAST:-0}" == "1" ]]; then export LIVEKIT_BUNDLE_SKIP=1; fi
dx serve --package dioxusfun
