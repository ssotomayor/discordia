#!/usr/bin/env bash
# Build the macOS .app + .dmg, code-signed if an identity is available.
#
# Signing is what makes macOS *permissions* survive a rebuild, which is the
# reason this script exists rather than a bare `dx bundle`. TCC records a Screen
# Recording / Microphone grant against the app's code signature; an ad-hoc
# signature is derived from the binary, so it changes on every build, the grant
# no longer matches, and — because a record for the bundle id already exists —
# macOS does not re-prompt. ScreenCaptureKit instead fails with "the user
# declined TCCs for application, window, display capture", which reads as a
# permission bug and is a signing one. A real identity's designated requirement
# is the bundle id plus the certificate, so one grant holds across rebuilds.
#
# The identity is NOT in Dioxus.toml on purpose. It is per-developer, and naming
# one there breaks `dx bundle` for everyone else: dx passes it straight to
# `codesign`, which exits 1 with "no identity found" rather than falling back.
# That took out all three pre-release CI jobs.
#
#   DISCORDIA_SIGNING_IDENTITY="Apple Development: You (TEAMID)" ./bundle-macos.sh
#
# List candidates with `security find-identity -v -p codesigning`. Unset, this
# still produces a working (ad-hoc) bundle — you just re-grant permissions after
# each build.
set -euo pipefail
cd "$(dirname "$0")"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: macOS only — this bundles a .app/.dmg" >&2
  exit 1
fi

# Same regeneration dance as dev-client.sh: CI asserts the committed CSS matches
# what the generator produces, so a bundle built from a stale one is a bundle
# that disagrees with the repo.
if command -v npx >/dev/null 2>&1; then
  (cd client && npx @tailwindcss/cli -i assets/tailwind.css -o assets/tailwind.out.css --minify)
else
  echo "warn: npx not found — using committed tailwind.out.css" >&2
fi

args=(bundle --release --platform macos --package-types macos --package-types dmg)
if [[ -n "${DISCORDIA_SIGNING_IDENTITY:-}" ]]; then
  # `--apple-team-id` is dx's name for the identity passed to `codesign --sign`;
  # it takes the full "Name (TEAMID)" string, not a bare team id.
  args+=(--codesign true --apple-team-id "$DISCORDIA_SIGNING_IDENTITY"
         --apple-entitlements Entitlements.plist)
  echo "signing with: $DISCORDIA_SIGNING_IDENTITY"
else
  cat >&2 <<'WARN'
warn: DISCORDIA_SIGNING_IDENTITY unset — building ad-hoc signed.
      The bundle will run, but macOS will treat it as a different app from your
      last build, so Screen Recording and Microphone grants will not carry over.
      Reset them with:
        tccutil reset ScreenCapture com.discordia.app
        tccutil reset Microphone    com.discordia.app
WARN
fi

(cd client && dx "${args[@]}")

# Report what was actually signed, because "it built" and "it will keep its
# permissions" are different claims and only the signature settles the second.
# The bundle lands under cargo's target dir, which is redirectable
# (`build.target-dir`), so ask cargo instead of assuming ./target.
target=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
app=$(find "${target:-target}/dx" -maxdepth 6 -name 'Discordia.app' -path '*bundle/macos*' 2>/dev/null | head -1)
if [[ -n "$app" ]]; then
  echo
  echo "bundle: $app"
  codesign -dvv "$app" 2>&1 | grep -E '^Identifier|^Authority|^Signature|flags' || true
else
  echo "warn: built, but could not locate the .app to report its signature" >&2
fi
