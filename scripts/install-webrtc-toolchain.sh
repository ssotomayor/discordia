#!/usr/bin/env bash
set -euo pipefail

. /etc/os-release
case "${VERSION_CODENAME:-}" in
  noble|resolute|trixie) task_codename="$VERSION_CODENAME" ;;
  *) printf 'Unsupported LLVM repository distribution: %s\n' "${VERSION_CODENAME:-unknown}" >&2; exit 1 ;;
esac

apt-get update
apt-get install -y --no-install-recommends ca-certificates curl gnupg
curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key |
  gpg --dearmor --yes -o /usr/share/keyrings/discordia-llvm.gpg
printf 'deb [signed-by=/usr/share/keyrings/discordia-llvm.gpg] https://apt.llvm.org/%s/ llvm-toolchain-%s-21 main\n' \
  "$task_codename" "$task_codename" > /etc/apt/sources.list.d/discordia-llvm.list
apt-get update
apt-get install -y --no-install-recommends clang-21 libclang-21-dev lld-21
clang++-21 --version
