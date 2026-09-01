#!/usr/bin/env bash
#
# Signs, notarizes and staples an app bundle.
#
# Hardened runtime is required for notarization. The entitlements alongside it re-enable the two
# things the runtime blocks that Loki genuinely needs.
#
# Usage: scripts/notarize.sh build/Loki.app
#
# Environment: IDENTITY, APPLE_ID, TEAM_ID, APP_PASSWORD.

set -euo pipefail

BUNDLE="${1:?usage: notarize.sh <path to .app>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENTITLEMENTS="$ROOT/app/Resources/Loki.entitlements"

for var in IDENTITY APPLE_ID TEAM_ID APP_PASSWORD; do
  if [ -z "${!var:-}" ]; then
    echo "$var is not set" >&2
    exit 1
  fi
done

echo "==> signing"
codesign --force --deep --options runtime --timestamp \
  --entitlements "$ENTITLEMENTS" \
  --sign "$IDENTITY" "$BUNDLE"
codesign --verify --strict --verbose=2 "$BUNDLE"

echo "==> submitting for notarization"
ZIP="$(dirname "$BUNDLE")/notarize.zip"
ditto -c -k --keepParent "$BUNDLE" "$ZIP"
xcrun notarytool submit "$ZIP" \
  --apple-id "$APPLE_ID" \
  --team-id "$TEAM_ID" \
  --password "$APP_PASSWORD" \
  --wait
rm -f "$ZIP"

echo "==> stapling"
xcrun stapler staple "$BUNDLE"
xcrun stapler validate "$BUNDLE"

echo
echo "Notarized $BUNDLE"
