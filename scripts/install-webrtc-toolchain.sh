#!/usr/bin/env bash
set -euo pipefail

. /etc/os-release
case "${VERSION_CODENAME:-}" in
  noble|resolute|trixie) task_codename="$VERSION_CODENAME" ;;
  *) printf 'Unsupported LLVM repository distribution: %s\n' "${VERSION_CODENAME:-unknown}" >&2; exit 1 ;;
esac

# Fastly Anycast fallback if apt.llvm.org authoritative DNS is failing or unresolvable.
if ! getent hosts apt.llvm.org >/dev/null 2>&1; then
  printf '\n199.232.194.49 apt.llvm.org\n199.232.198.49 apt.llvm.org\n' >> /etc/hosts
fi

apt-get update
apt-get install -y --no-install-recommends ca-certificates curl gnupg
key_tmp="$(mktemp)"
curl --ipv4 --retry 5 --retry-delay 2 --retry-all-errors --retry-connrefused -fsSL \
  https://apt.llvm.org/llvm-snapshot.gpg.key -o "$key_tmp"
gpg --dearmor --yes -o /usr/share/keyrings/discordia-llvm.gpg "$key_tmp"
rm -f "$key_tmp"
printf 'deb [signed-by=/usr/share/keyrings/discordia-llvm.gpg] https://apt.llvm.org/%s/ llvm-toolchain-%s-21 main\n' \
  "$task_codename" "$task_codename" > /etc/apt/sources.list.d/discordia-llvm.list
apt-get update -o Acquire::Retries=3
apt-get install -y --no-install-recommends -o Acquire::Retries=3 clang-21 libclang-21-dev lld-21
clang++-21 --version
