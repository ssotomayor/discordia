#!/usr/bin/env bash
#
# launch_user.sh <username> [extra cargo/binary args...]
#
# Launches an isolated Discordia (dioxusfun) instance with its own identity +
# session, so you can simulate several users on one machine. Each <username>
# maps to a separate config directory via DIOXUSFUN_CONFIG_DIR, which the app
# uses instead of ~/Library/Application Support/dioxusfun.
#
# Examples:
#   ./launch_user.sh alice          # first window, fresh identity for "alice"
#   ./launch_user.sh bob            # second window, separate identity
#
# Reset a user:   rm -rf "$(./launch_user.sh --dir alice)"   (or just delete the dir printed below)
#
# Base dir for the per-user state (override if you like):
#   DIOXUSFUN_DEV_BASE=~/.dioxusfun-users ./launch_user.sh alice

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_DIR="${DIOXUSFUN_DEV_BASE:-/tmp/dioxusfun-users}"

if [[ $# -lt 1 || "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  echo "usage: $(basename "$0") <username> [extra args...]" >&2
  echo "       runs an isolated instance; config dir: ${BASE_DIR}/<username>" >&2
  exit 1
fi

USERNAME="$1"
shift

# Keep the directory name filesystem-safe.
SAFE_USER="$(printf '%s' "$USERNAME" | tr -c 'A-Za-z0-9._-' '_')"
CONFIG_DIR="${BASE_DIR}/${SAFE_USER}"

# `--dir <username>` just prints the resolved config dir (useful for resets).
if [[ "$USERNAME" == "--dir" ]]; then
  SAFE2="$(printf '%s' "${1:?usage: --dir <username>}" | tr -c 'A-Za-z0-9._-' '_')"
  echo "${BASE_DIR}/${SAFE2}"
  exit 0
fi

mkdir -p "$CONFIG_DIR"

echo "▶ launching Discordia as '${USERNAME}'"
echo "  config dir: ${CONFIG_DIR}"

# Build once up front so two concurrent launches don't thrash the build (the
# second invocation just waits on cargo's lock and finds it already built),
# then run the prebuilt binary with the isolated config dir.
( cd "$REPO_DIR" && cargo build -p dioxusfun )

DIOXUSFUN_CONFIG_DIR="$CONFIG_DIR" exec "$REPO_DIR/target/debug/Discordia" "$@"
