#!/usr/bin/env bash

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

SAFE_USER="$(printf '%s' "$USERNAME" | tr -c 'A-Za-z0-9._-' '_')"
CONFIG_DIR="${BASE_DIR}/${SAFE_USER}"

if [[ "$USERNAME" == "--dir" ]]; then
  SAFE2="$(printf '%s' "${1:?usage: --dir <username>}" | tr -c 'A-Za-z0-9._-' '_')"
  echo "${BASE_DIR}/${SAFE2}"
  exit 0
fi

mkdir -p "$CONFIG_DIR"

echo "▶ launching Discordia as '${USERNAME}'"
echo "  config dir: ${CONFIG_DIR}"

( cd "$REPO_DIR" && cargo build -p dioxusfun )

DIOXUSFUN_CONFIG_DIR="$CONFIG_DIR" exec "$REPO_DIR/target/debug/Discordia" "$@"
