#!/usr/bin/env bash
# One-time setup after the container is built. Runs before init-firewall.sh, so
# anything here still has open egress.
set -euo pipefail

REPO=/Users/tehsoto/Projects/dioxusfun-monorepo
cd "$REPO"

sudo /usr/local/bin/prepare-mounts.sh

# The bind mount arrives owned by the host uid, which is not this container's.
# Without this git refuses every command with "dubious ownership" — including
# inside each parallel-session worktree, which git treats as its own top level.
git config --global --add safe.directory "$REPO"
for wt in "$REPO"/.claude/worktrees/*/; do
    [ -d "$wt" ] && git config --global --add safe.directory "${wt%/}"
done

# The host global config does not come along, so without this `git commit` in
# here fails on a missing identity.
git config --global user.name "Teh SoTo"
git config --global user.email "sansoto5000@gmail.com"

# Signed commits, via the forwarded SSH agent rather than the host's OpenPGP
# key. GPG cannot be agent-forwarded on macOS, so matching the host's format
# would have meant copying the private keyring into the container; SSH signing
# reaches the same "Verified" state with the key staying on the host.
#
# The key is named by value, read out of the forwarded agent, so no public key
# file is mounted either. `key::` is git's literal-key form.
#
# Selected by comment rather than position: the agent also carries unrelated
# keys (a VPS one sorts first), and signing with the wrong one produces commits
# GitHub reports as Unverified. This has to be the same key registered on
# GitHub as a *Signing Key* — GitHub keeps auth and signing keys separate, and
# an auth-only key verifies nothing.
git config --global gpg.format ssh
git config --global commit.gpgsign true
git config --global tag.gpgsign true

signing_key="$(ssh-add -L 2>/dev/null | grep -F "sansoto5000@gmail.com" | head -n1)"
if [ -n "$signing_key" ]; then
    git config --global user.signingkey "key::$signing_key"

    # Without an allowed-signers file, SSH-signed commits are still signed but
    # `git log --show-signature` and %G? report them as unverified locally —
    # which looks exactly like signing having silently failed. GitHub verifies
    # against the key registered there and is unaffected either way.
    install -d -m 700 ~/.config/git
    printf '%s %s\n' "sansoto5000@gmail.com" "$signing_key" > ~/.config/git/allowed_signers
    git config --global gpg.ssh.allowedSignersFile ~/.config/git/allowed_signers

    echo "post-create: commit signing armed from the forwarded agent"
else
    # Better to say so than to let every commit fail with a message about
    # user.signingKey that says nothing about the agent being the cause.
    echo "post-create: WARNING no matching key in the forwarded SSH agent —"
    echo "  commits will fail to sign. On the host: ssh-add ~/.ssh/id_ed25519"
    git config --global commit.gpgsign false
    git config --global tag.gpgsign false
fi

# Pin GitHub's host keys, so the first push is not an interactive
# "authenticity of host cannot be established" prompt that a non-interactive
# agent would simply hang on.
install -d -m 700 ~/.ssh
ssh-keyscan -t rsa,ecdsa,ed25519 github.com 2>/dev/null >> ~/.ssh/known_hosts
sort -u -o ~/.ssh/known_hosts ~/.ssh/known_hosts

npm install -g @anthropic-ai/claude-code

cat <<'EOF'

Discordia devcontainer ready.

  claude --dangerously-skip-permissions

First run in a session, once each:

  claude   /login            the host's credentials are deliberately not mounted
  gh auth login              the host's token lives in its keyring, not a file

What CI checks, in CI's own two groups (trap 14 — one group misses half):

  cargo fmt --all --check
  cargo clippy -p dioxusfun-protocol -p dioxusfun-server -p dioxusfun-bot -p dioxusfun-rendezvous --all-targets -- -D warnings
  cargo clippy -p dioxusfun -p dioxus-grid-layout --all-targets -- -D warnings
  cargo test --workspace
  cargo audit

Pushing: .git/config is mounted read-only, so `git push -u` fails on the
config write. Use

  git push origin HEAD  &&  gh pr create --head "$(git branch --show-current)"

The client compiles in here but cannot run: no display for wry, no audio
device, and screen capture is Windows/macOS-only. `dx build --package dioxusfun`
still works and is how client/assets/tailwind.css gets regenerated (trap 13).

LIVEKIT_BUNDLE_SKIP=1 is set, so `cargo run -p dioxusfun-server` starts a
gateway with no SFU. Unset it to build a server that embeds one.

Egress is default-deny; a new upstream host needs a line in
.devcontainer/init-firewall.sh and an image rebuild.
EOF
