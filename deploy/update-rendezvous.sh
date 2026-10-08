#!/usr/bin/env bash
# Updates the rendezvous container on the VPS to a published image and turns
# on its TURN relay. Run from a machine that can SSH to the box; the
# devcontainer cannot. Idempotent: rerunning re-applies the same override.
#
#   deploy/update-rendezvous.sh                 # latest master image
#   IMAGE_TAG=0911e1a deploy/update-rendezvous.sh   # pin a short sha
#   DRY_RUN=1 deploy/update-rendezvous.sh       # print the remote steps only
#
# Knobs (env): VPS, SSH_KEY, DEPLOY_DIR, PROJECT, IMAGE_TAG, TURN_HOST, TURN_PORTS.
set -euo pipefail

VPS=${VPS:-root@151.243.137.35}
SSH_KEY=${SSH_KEY:-$HOME/.ssh/ultahost_ed25519}
DEPLOY_DIR=${DEPLOY_DIR:-/opt/discordia}
PROJECT=${PROJECT:-discordia}
IMAGE_TAG=${IMAGE_TAG:-latest}
IMAGE=ghcr.io/ssotomayor/discordia-rendezvous:${IMAGE_TAG}
# What clients dial for the relay. Empty means PUBLIC_HOST from the box's .env,
# which is what every other URL there is built from.
TURN_HOST=${TURN_HOST:-}
TURN_PORTS=${TURN_PORTS:-7710-7809}
TURN_PORT=7702
UFW_RANGE=${TURN_PORTS/-/:}
MARKER="# managed by deploy/update-rendezvous.sh"

ssh_cmd=(ssh -o BatchMode=yes -o ConnectTimeout=15)
[[ -f "$SSH_KEY" ]] && ssh_cmd+=(-i "$SSH_KEY")

remote=$(cat <<REMOTE
set -euo pipefail
cd "$DEPLOY_DIR"
[[ -f .env ]] || { echo "no .env in $DEPLOY_DIR" >&2; exit 2; }
PUBLIC_HOST=\$(sed -n 's/^PUBLIC_HOST=//p' .env | tr -d '"\r')
[[ -n "\$PUBLIC_HOST" ]] || { echo "PUBLIC_HOST missing from .env" >&2; exit 2; }
TURN_HOST="${TURN_HOST}"; TURN_HOST=\${TURN_HOST:-\$PUBLIC_HOST}

echo "== before"
docker compose -p "$PROJECT" ps

OVERRIDE=docker-compose.override.yml
if [[ -f "\$OVERRIDE" ]] && ! grep -qF "$MARKER" "\$OVERRIDE"; then
  echo "\$OVERRIDE exists and was not written by this script; merge by hand:" >&2
  cat "\$OVERRIDE" >&2
  exit 3
fi
[[ -f "\$OVERRIDE" ]] && cp -a "\$OVERRIDE" "\$OVERRIDE.bak.\$(date +%Y%m%d%H%M%S)"

cat > "\$OVERRIDE" <<YAML
$MARKER
# Additive: ports append to the base file, environment merges, image replaces.
services:
  rendezvous:
    image: $IMAGE
    ports:
      - "$TURN_PORT:$TURN_PORT/udp"
      - "$TURN_PORTS:$TURN_PORTS/udp"
    environment:
      DIOXUSFUN_RENDEZVOUS_TURN_URL: turn:\$TURN_HOST:$TURN_PORT?transport=udp
      DIOXUSFUN_RENDEZVOUS_TURN_ADDR: 0.0.0.0:$TURN_PORT
      DIOXUSFUN_RENDEZVOUS_TURN_PORTS: $TURN_PORTS
YAML
echo "== override"
cat "\$OVERRIDE"
docker compose -p "$PROJECT" config --quiet

echo "== firewall"
if command -v ufw >/dev/null && ufw status | grep -q '^Status: active'; then
  ufw allow "$TURN_PORT/udp" >/dev/null
  ufw allow "$UFW_RANGE/udp" >/dev/null
  ufw status | grep -E "$TURN_PORT|$UFW_RANGE" || true
else
  echo "ufw not active: make sure UDP $TURN_PORT and $TURN_PORTS are open (iptables / provider panel)"
fi

echo "== pull + restart rendezvous"
docker compose -p "$PROJECT" pull rendezvous
docker compose -p "$PROJECT" up -d rendezvous

echo "== verify"
for _ in \$(seq 1 20); do
  if docker compose -p "$PROJECT" logs --since 2m rendezvous 2>/dev/null | grep -q 'turn relay listening'; then break; fi
  sleep 1
done
docker compose -p "$PROJECT" logs --since 2m rendezvous | grep -E 'turn relay|rendezvous configured|error' || true
CONFIG=\$(curl -fsS http://127.0.0.1:7700/config)
echo "\$CONFIG"
echo "\$CONFIG" | grep -q '"turn_urls":\[' || { echo "relay not advertised in /config" >&2; exit 4; }
docker compose -p "$PROJECT" ps rendezvous
docker image inspect --format '{{index .RepoDigests 0}}' "$IMAGE" || true
echo "TURN_HOST=\$TURN_HOST"
REMOTE
)

if [[ "${DRY_RUN:-}" == 1 ]]; then
  printf '%s\n' "$remote"
  exit 0
fi

echo "== $VPS"
"${ssh_cmd[@]}" "$VPS" bash -s <<<"$remote"

# A STUN binding request to the relay port proves the UDP path from outside the
# box: the port is published, the firewall lets it through, the relay answers.
probe_host=$TURN_HOST
if [[ -z "$probe_host" ]]; then
  probe_host=$("${ssh_cmd[@]}" "$VPS" "sed -n 's/^PUBLIC_HOST=//p' $DEPLOY_DIR/.env | tr -d '\"\r'")
fi
echo "== STUN probe $probe_host:$TURN_PORT"
python3 - "$probe_host" "$TURN_PORT" <<'PY'
import os, socket, sys
host, port = sys.argv[1], int(sys.argv[2])
req = bytes([0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42]) + os.urandom(12)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(3)
s.sendto(req, (host, port))
try:
    data, _ = s.recvfrom(1500)
except socket.timeout:
    print("no STUN reply: UDP", port, "is not reachable from here (firewall or port publish)")
    sys.exit(5)
ok = len(data) >= 20 and data[0:2] == b"\x01\x01" and data[8:20] == req[8:20]
print("STUN binding reply ok" if ok else "unexpected reply", data[:2].hex())
sys.exit(0 if ok else 5)
PY

cat <<EOF

Done. Roll back with:
  ssh $VPS 'cd $DEPLOY_DIR && rm docker-compose.override.yml && docker compose -p $PROJECT up -d rendezvous'
(or restore the .bak the script left beside it). Hosts need nothing: the next
registration hands them relay credentials and their banner tooltip says so.
EOF
