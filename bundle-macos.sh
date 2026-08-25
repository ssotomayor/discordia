#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: macOS only — this bundles a .app/.dmg" >&2
  exit 1
fi

# Signed here, not via `dx --codesign`: that flag signs the inner executable
# only, leaving no CodeResources seal, and the identity is per-developer.
identity="${DISCORDIA_SIGNING_IDENTITY:-}"

if [[ -n "$identity" ]]; then
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
      last build, so Screen Recording, Microphone and Camera grants will not
      carry over. Reset them with:
        tccutil reset ScreenCapture com.discordia.app
        tccutil reset Microphone    com.discordia.app
        tccutil reset Camera        com.discordia.app
      Each prints one line per copy of the app registered under that bundle id,
      so several lines is normal and means several copies exist, not an error.
WARN
fi

(cd client && dx bundle --release --platform macos --package-types macos)

target=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
app=$(find "${target:-target}/dx" -maxdepth 6 -name 'Discordia.app' -path '*bundle/macos*' 2>/dev/null | head -1)
[[ -n "$app" ]] || { echo "error: built, but no Discordia.app found" >&2; exit 1; }

if [[ -n "$identity" ]]; then
  echo "signing bundle with: $identity"
  codesign --force --options runtime \
    --entitlements client/Entitlements.plist \
    --sign "$identity" "$app"

  codesign --verify --deep --strict --verbose=2 "$app"
  echo "signature verified"
fi

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
