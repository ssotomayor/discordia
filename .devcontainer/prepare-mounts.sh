#!/usr/bin/env bash
# Hand the mounted paths Docker created as root to the container user.
#
# Root-only, hence the dedicated sudoers entry: postCreateCommand runs as
# vscode, and this container has no blanket sudo (see the Dockerfile).
set -euo pipefail

[ "$(id -u)" -eq 0 ] || { echo "must run as root (sudo prepare-mounts.sh)" >&2; exit 1; }

REPO=/Users/tehsoto/Projects/dioxusfun-monorepo

# Docker creates the forwarded SSH agent socket owned root:root, so the
# container user gets "Error connecting to agent: Permission denied" and both
# signing and pushing fail while the socket looks perfectly present. Only the
# two accounts in this image can reach it either way.
if [ -S /ssh-agent ]; then
    chown vscode:vscode /ssh-agent 2>/dev/null || chmod 0666 /ssh-agent
    echo "prepare-mounts: opened /ssh-agent to vscode"
fi

# Docker creates the intermediate directories for a nested mount, and creates
# them root-owned. ~/.claude/projects/<slug> only exists because the session
# store is mounted under it, so without this Claude cannot write its own files
# beside it. Non-recursive, so the read-only mounts and the host's transcripts
# underneath are left exactly as they are.
for dir in \
    /home/vscode/.claude \
    /home/vscode/.claude/projects \
    /home/vscode/.config/gh
do
    [ -d "$dir" ] && chown vscode:vscode "$dir" 2>/dev/null || true
done

# The Dockerfile creates both of these so a fresh volume inherits vscode, but
# Docker seeds a volume once and never revisits it — a volume created before
# that fix stays root's, and cargo fails on its own cache with a permission
# error that names a path nobody chose.
for path in /usr/local/cargo/registry /usr/local/cargo/git; do
    [ -d "$path" ] || continue
    if [ "$(stat -c '%U' "$path")" != "vscode" ]; then
        chown -R vscode:vscode "$path"
        echo "prepare-mounts: claimed $path"
    fi
done

# target/ is a named volume mounted *inside* the bind-mounted workspace. Docker
# cannot inherit ownership for that the way it does for a volume over a path
# that exists in the image — there is no image path there, so it creates it
# root:root and the first cargo invocation fails writing its own output dir.
if [ -d "$REPO/target" ] && [ "$(stat -c '%U' "$REPO/target")" != "vscode" ]; then
    # Not recursive on a warm target dir: this is the empty first-run case, and
    # recursing over 40GB of build output on every start would cost minutes for
    # nothing.
    chown vscode:vscode "$REPO/target"
    echo "prepare-mounts: claimed $REPO/target"
fi
