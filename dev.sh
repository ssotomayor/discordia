#!/usr/bin/env bash
# Open iTerm with three vertical panes running:
#   1) rendezvous (left)
#   2) gateway server (middle)
#   3) Dioxus client (right)
#
# Usage:  ./dev.sh
#
# Set FAST=1 to skip the bundled-LiveKit build for faster iteration when
# you don't care about voice end-to-end:
#   FAST=1 ./dev.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_PREFIX=""
if [[ "${FAST:-0}" == "1" ]]; then
  ENV_PREFIX="LIVEKIT_BUNDLE_SKIP=1 "
fi

osascript <<APPLESCRIPT
tell application "iTerm"
  activate
  set newWindow to (create window with default profile)
  tell current session of newWindow
    set name to "rendezvous"
    write text "cd '$ROOT' && ${ENV_PREFIX}cargo run -p dioxusfun-rendezvous"
    set serverSession to (split vertically with default profile)
  end tell
  tell serverSession
    set name to "server"
    write text "cd '$ROOT' && ${ENV_PREFIX}cargo run -p dioxusfun-server"
    set clientSession to (split vertically with default profile)
  end tell
  tell clientSession
    set name to "client"
    write text "cd '$ROOT' && ${ENV_PREFIX}cargo run -p dioxusfun"
  end tell
end tell
APPLESCRIPT
