#!/usr/bin/env bash
# Builds the static export and syncs it into the nginx root on the VPS (docs/OPS.md, "The website").
set -euo pipefail
cd "$(dirname "$0")"
host="${SITE_HOST:-root@151.243.137.35}"
root="${SITE_ROOT:-/var/www/discordia.world}"
npm ci --no-audit --no-fund
npm run build
# Symbolic chmod: macOS's openrsync rejects the numeric D755,F644 form.
rsync -az --delete --chmod=Du=rwx,Dgo=rx,Fu=rw,Fgo=r out/ "$host:$root/"
echo "deployed to $host:$root"
