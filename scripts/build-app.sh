#!/usr/bin/env bash
#
# Builds Loki.app.
#
# Cargo builds the core into a static library, SwiftPM builds the executable and links it,
# then this assembles a .app bundle with LSUIElement so it runs as a menu bar item with no
# Dock icon. No .xcodeproj involved, so the whole project stays text and diffable.
#
# Usage: scripts/build-app.sh [debug|release]

set -euo pipefail

CONFIG="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE="$ROOT/build/Loki.app"

# macOS ships bash 3.2, where an empty array under `set -u` reads as unbound. Use a string.
case "$CONFIG" in
  debug)   CARGO_FLAGS="" ;;
  release) CARGO_FLAGS="--release" ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

# Match SwiftPM's deployment target so the linker does not warn about mismatched object files.
export MACOSX_DEPLOYMENT_TARGET=26.0

echo "==> cargo build ($CONFIG)"
cd "$ROOT"
cargo build -p loki-ffi $CARGO_FLAGS

# SwiftPM searches both target dirs. Create the unused one so the linker does not warn.
mkdir -p "$ROOT/target/debug" "$ROOT/target/release"

echo "==> swift build ($CONFIG)"
cd "$ROOT/app"
swift build -c "$CONFIG"
BIN="$(swift build -c "$CONFIG" --show-bin-path)/LokiApp"

echo "==> assembling $BUNDLE"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$BIN" "$BUNDLE/Contents/MacOS/Loki"

# One source of truth. The same file is embedded into the executable at link time by
# Package.swift, so the bundle and the bare binary cannot disagree about LSUIElement.
cp "$ROOT/app/Resources/Info.plist" "$BUNDLE/Contents/Info.plist"

echo "==> ad-hoc signing"
codesign --force --sign - "$BUNDLE"

echo
echo "Built $BUNDLE"
echo "Run it with: open $BUNDLE"
