#!/usr/bin/env bash
# Build the macOS .app + .dmg, code-signed if an identity is available.
#
# WHY SIGN AT ALL
# macOS TCC records a Screen Recording / Microphone grant against the app's code
# signature. An ad-hoc signature is derived from the binary, so it changes on
# every build, the grant no longer matches, and — because a record for the bundle
# id already exists — macOS does not re-prompt. ScreenCaptureKit instead fails
# with "the user declined TCCs for application, window, display capture", which
# reads as a permission bug and is a signing one. A real identity's designated
# requirement is the bundle id plus the certificate, so one grant holds.
#
# WHY THE IDENTITY IS NOT IN Dioxus.toml
# It is per-developer. Naming one there breaks `dx bundle` for everyone else and
# for CI, both of which fail hard — dx hands the value to `codesign`, which exits
# 1 with "no identity found". That took out all three pre-release jobs once.
#
# WHY WE SIGN HERE RATHER THAN VIA `dx --codesign`
# That flag signs the inner executable only (its own help: `codesign --force
# --entitlements <file> --sign <id>` against the binary). A bundle needs
# `Contents/_CodeSignature/CodeResources` sealing Info.plist and the resources;
# without it `codesign --verify` reports "invalid Info.plist (plist or signature
# have been modified)" and macOS refuses to launch with "The application
# Discordia can't be opened". Signing the *bundle* path is what produces the
# seal, so that is what this does — and then verifies it, which is the step whose
# absence let a broken bundle ship (`codesign -dvv` prints metadata and passes on
# a bundle that will not open; only `--verify` validates).
#
#   DISCORDIA_SIGNING_IDENTITY="Apple Development: You (TEAMID)" ./bundle-macos.sh
#
# Final artifacts are copied to client/build/macos/ so they are easy to find;
# Dioxus's target directory remains an internal build/cache location.
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

identity="${DISCORDIA_SIGNING_IDENTITY:-}"

# Same regeneration dance as dev-client.sh: CI asserts the committed CSS matches
# what the generator produces, so a bundle built from a stale one disagrees with
# the repo.
if command -v npx >/dev/null 2>&1; then
  (cd client && npx @tailwindcss/cli -i assets/tailwind.css -o assets/tailwind.out.css --minify)
else
  echo "warn: npx not found — using committed tailwind.out.css" >&2
fi

if [[ -n "$identity" ]]; then
  # Validate the entitlements as strict XML before codesign sees them. Worth its
  # own check because the parser that matters is stricter than the obvious one:
  # `plutil -lint` accepts a comment containing `--` (illegal in XML) and reports
  # OK, while codesign's AMFI parser rejects it with "AMFIUnserializeXML: syntax
  # error near line 4" after a full release compile.
  if command -v xmllint >/dev/null 2>&1; then
    xmllint --noout client/Entitlements.plist || {
      echo "error: client/Entitlements.plist is not valid XML (a '--' inside a comment?)" >&2
      exit 1
    }
  fi
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

# Only the .app here. The DMG is built below from the *signed* bundle — letting
# dx make it first would package the unsigned one.
(cd client && dx bundle --release --platform macos --package-types macos)

# The bundle lands under cargo's target dir, which is redirectable
# (`build.target-dir`), so ask cargo rather than assuming ./target.
target=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
app=$(find "${target:-target}/dx" -maxdepth 6 -name 'Discordia.app' -path '*bundle/macos*' 2>/dev/null | head -1)
[[ -n "$app" ]] || { echo "error: built, but no Discordia.app found" >&2; exit 1; }

if [[ -n "$identity" ]]; then
  echo "signing bundle with: $identity"
  # No --deep: there is exactly one Mach-O in here (the main executable), and
  # --deep is the wrong tool the moment that stops being true. `--options
  # runtime` matches the configuration this app has been tested against.
  codesign --force --options runtime \
    --entitlements client/Entitlements.plist \
    --sign "$identity" "$app"

  # The check whose absence shipped an app that could not be opened. `--verify`
  # validates the seal; `-dvv` only prints metadata and would pass regardless.
  codesign --verify --deep --strict --verbose=2 "$app"
  echo "signature verified"
fi

# Rebuild the DMG from whatever the .app now is, mirroring dx's own staging: the
# app plus an /Applications symlink to drag it onto.
output_dir="$PWD/client/build/macos"
output_app="$output_dir/Discordia.app"
dmg="$output_dir/Discordia_0.1.0_aarch64.dmg"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
mkdir -p "$output_dir"
rm -rf "$output_app"
cp -R "$app" "$output_app"
rm -f "$dmg"
hdiutil create -srcfolder "$stage" -volname Discordia -ov -format UDZO "$dmg" >/dev/null
[[ -n "$identity" ]] && codesign --force --sign "$identity" "$dmg" >/dev/null 2>&1 || true

echo
echo "app: $output_app"
echo "dmg: $dmg"
codesign -dvv "$output_app" 2>&1 | grep -E '^Identifier|^Authority|^Signature|flags' || true
