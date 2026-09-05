#!/usr/bin/env bash
# Default-deny egress with a hostname allowlist. This is the control that makes
# --dangerously-skip-permissions defensible: the agent can write anything it
# likes inside the container, but an identity key under discordia-data/, a
# LiveKit secret or a session transcript cannot leave to a host nobody put on
# this list.
#
# Runs from postStartCommand, not postCreateCommand: iptables rules live in the
# container's network namespace and are gone after `docker stop`, so a
# create-time-only hook leaves every later session wide open.
#
# Known limitation: hostnames resolve to IPs once, here. crates.io, npm, the
# GitHub asset CDN and the Anthropic API all sit behind CDNs that rotate
# addresses, so a long-lived container can start seeing failures that look like
# outages. Re-run this script (or restart the container) to re-resolve.
set -euo pipefail
IFS=$'\n\t'

[ "$(id -u)" -eq 0 ] || { echo "must run as root (sudo init-firewall.sh)" >&2; exit 1; }

# Every host reached at runtime, grouped by who needs it. A build that starts
# failing on a fresh upstream belongs here, with a note saying which.
ALLOWED_DOMAINS=(
    # Claude Code: the API, plus the OAuth exchange `claude` has to complete
    # against when you log in inside the container.
    api.anthropic.com
    console.anthropic.com
    claude.ai
    statsig.anthropic.com

    # Toolchain and registries.
    static.rust-lang.org
    crates.io
    index.crates.io
    static.crates.io
    registry.npmjs.org

    # GitHub release assets do NOT resolve into the ranges api.github.com/meta
    # publishes — they are served from Azure blob storage. Without these three,
    # three separate things fail mid-download and blame the network: the
    # libwebrtc prebuilt webrtc-sys fetches for the client, the livekit-server
    # release server/build.rs embeds, and every `cargo binstall`.
    objects.githubusercontent.com
    release-assets.githubusercontent.com
    raw.githubusercontent.com

    # Nostr, for DMs and kind 0 profiles. Only the client talks to these and
    # the client has no display in here — they are allowed so a probe or an
    # #[ignore]d test against a real relay works, not because the suite needs
    # them. client/src/nostr/ holds the defaults; blossom.band is media.
    relay.damus.io
    nos.lol
    relay.primal.net
    relay.nostr.band
    blossom.band
)

echo "firewall: resetting"
# The filter table ONLY. Flushing nat destroys the DNAT rules that redirect
# 127.0.0.11 to Docker's embedded resolver, and every name in this script then
# fails to resolve — including, silently, the ones being allowed. mangle is
# untouched for the same reason: nothing here writes to either table.
iptables -F
iptables -X
ipset destroy allowed-domains 2>/dev/null || true

# Resolution and the GitHub metadata fetch below both need working egress, so
# they happen while the policy is still ACCEPT. The DROP flip is the last step.
ipset create allowed-domains hash:net

# GitHub publishes its ranges rather than a stable set of A records; git,
# api.github.com and codeload are separate blocks and all are needed (clone,
# push, `gh pr create`, and the deep_filter git dependency).
echo "firewall: github ranges"
gh_meta="$(curl -fsSL --max-time 20 https://api.github.com/meta)"
echo "$gh_meta" \
    | jq -r '(.web + .api + .git + .packages + .actions)[]? | select(test(":") | not)' \
    | sort -u \
    | while read -r cidr; do ipset add allowed-domains "$cidr" -exist; done

for domain in "${ALLOWED_DOMAINS[@]}"; do
    ips="$(dig +short +time=3 +tries=2 A "$domain" | grep -E '^[0-9.]+$' || true)"
    if [ -z "$ips" ]; then
        # Not fatal. A typo here should not cost a whole session; the failure it
        # causes later is legible, and this line says which name to look at.
        echo "firewall: WARNING $domain did not resolve — traffic to it will be dropped"
        continue
    fi
    while read -r ip; do ipset add allowed-domains "$ip" -exist; done <<< "$ips"
    echo "firewall: $domain -> $(echo "$ips" | tr '\n' ' ')"
done

# The compose network. Read from the kernel's own link-scope route rather than
# hardcoded: Docker allocates the subnet and it differs between machines and
# between projects.
subnet="$(ip -o -f inet route show scope link | awk '$1 != "default" {print $1}' | head -n1)"
[ -n "$subnet" ] || { echo "firewall: could not determine the container subnet" >&2; exit 1; }
ipset add allowed-domains "$subnet" -exist
echo "firewall: local network $subnet"

echo "firewall: applying default deny"
iptables -A INPUT  -i lo -j ACCEPT
iptables -A OUTPUT -o lo -j ACCEPT
# DNS only. No blanket port 22 rule: SSH to anywhere is as good an exfil
# channel as HTTPS to anywhere, and git@github.com already resolves into the
# GitHub ranges added to the ipset above, so push still works.
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT
iptables -A INPUT   -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT  -m state --state ESTABLISHED,RELATED -j ACCEPT
# VS Code / the devcontainer CLI attach inbound from the host network.
iptables -A INPUT -s "$subnet" -j ACCEPT

iptables -P INPUT   DROP
iptables -P FORWARD DROP
iptables -P OUTPUT  DROP
iptables -A OUTPUT -m set --match-set allowed-domains dst -j ACCEPT

# Prove both directions rather than trusting the ruleset. A silent
# misconfiguration here reads as "the firewall is on" while allowing everything.
if curl -fsS --max-time 5 https://example.com >/dev/null 2>&1; then
    echo "firewall: FAILED — example.com is reachable, egress is not restricted" >&2
    exit 1
fi
# Any HTTP status proves the connection was made; 401 is the correct answer
# here and there may be no key to send. Only 000 — no response at all — means
# the allowlist is wrong. `curl -f` would report the 401 as a failure, so this
# reads the status code instead.
code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://api.anthropic.com/v1/models || echo 000)"
if [ "$code" = "000" ]; then
    echo "firewall: FAILED — api.anthropic.com is unreachable, Claude cannot run" >&2
    exit 1
fi
echo "firewall: ok — default deny, $(ipset list allowed-domains | grep -c '^[0-9]') entries allowed"
