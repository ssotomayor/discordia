#!/bin/sh
# Installs discordia-server on a Linux x86_64 machine, then runs its setup.
#
#   curl -fsSL https://discordia.world/install.sh | sh
#   curl -fsSL https://discordia.world/install.sh | sh -s -- --docker --role rendezvous
#
# Arguments go to `discordia-server setup`. Rerun to upgrade: an installed
# machine keeps its config and restarts onto the new binary.
# DISCORDIA_VERSION=v0.1.0-pre.123 pins a release.
set -eu

REPO=ssotomayor/discordia
ASSET=discordia-server-linux
BIN=/usr/local/bin/discordia-server
# release-signing.pub at the repository root; a signature that does not verify stops here.
PUBKEY=RWRC4u05WSE3oLs4o2ZKddADDC0QrOba58PqVBVYxog366q5Hk3wrTdM

say() { printf '%s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

[ "$(uname -s)" = Linux ] || die "the server is published for Linux only; elsewhere, build it with cargo"
[ "$(uname -m)" = x86_64 ] || die "the server is published for x86_64 only, this is $(uname -m)"

SUDO=
if [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null || die "run as root, or install sudo"
  SUDO=sudo
fi

command -v curl >/dev/null || die "curl is required"
if ! command -v minisign >/dev/null; then
  if command -v apt-get >/dev/null; then
    say "installing minisign to check the download's signature"
    $SUDO apt-get update -qq && $SUDO apt-get install -y -qq minisign >/dev/null
  elif command -v dnf >/dev/null; then
    $SUDO dnf install -y -q minisign
  else
    die "minisign is required to check the download: https://jedisct1.github.io/minisign/"
  fi
fi

tag=${DISCORDIA_VERSION:-}
if [ -z "$tag" ]; then
  # Every release is a pre-release, so /releases/latest would find none.
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$tag" ] || die "could not find the latest release of $REPO"
fi

tmp=$(mktemp -d)
trap 'rm -f "$tmp/$ASSET" "$tmp/$ASSET.minisig"; rmdir "$tmp"' EXIT
url="https://github.com/$REPO/releases/download/$tag/$ASSET"
say "downloading $tag"
curl -fsSL -o "$tmp/$ASSET" "$url"
curl -fsSL -o "$tmp/$ASSET.minisig" "$url.minisig"
minisign -Vq -P "$PUBKEY" -m "$tmp/$ASSET" || die "signature check failed; nothing was installed"

$SUDO install -m 0755 "$tmp/$ASSET" "$BIN"
say "installed $BIN ($tag)"

if [ $# -eq 0 ] && [ -f /etc/discordia/discordia.toml ]; then
  for unit in discordia-server discordia-rendezvous; do
    if systemctl is-enabled --quiet "$unit" 2>/dev/null; then
      $SUDO systemctl restart "$unit" && say "restarted $unit"
    fi
  done
  say "upgraded; \`sudo discordia-server setup\` changes the answers"
  exit 0
fi

# Under `curl | sh` the script is stdin, so the questions read the terminal.
if [ -r /dev/tty ]; then
  exec $SUDO "$BIN" setup "$@" </dev/tty
fi
exec $SUDO "$BIN" setup "$@"
