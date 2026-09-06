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

# The mark the app draws with, refreshed from the one place the artwork lives. SwiftPM will not
# follow a symlink into a resource bundle, so this is a copy, and copying it on every build is what
# stops it going stale.
#
# The PDF is the one that matters: it is vector, so Core Graphics rasterises it at the size and the
# subpixel position it is actually drawn at, and every resampling defect this app had disappears
# rather than being minimised. The PNG stays beside it as the fallback for a build where the vector
# is missing.
for asset in pdf png; do
  if [ -f "$ROOT/branding/logo/logo.$asset" ]; then
    cp "$ROOT/branding/logo/logo.$asset" "$ROOT/app/Sources/LokiApp/Resources/loki-mark.$asset"
  fi
done

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

# SwiftPM emits resources as a sibling bundle beside the binary, and copying only the executable
# leaves `Bundle.module` with nothing to find. That fails at runtime rather than at build time,
# which is exactly the kind of silence this script exists to prevent elsewhere.
BIN_DIR="$(dirname "$BIN")"
for resource in "$BIN_DIR"/*.bundle; do
  [ -e "$resource" ] || continue
  cp -R "$resource" "$BUNDLE/Contents/Resources/"
done

# One source of truth. The same file is embedded into the executable at link time by
# Package.swift, so the bundle and the bare binary cannot disagree about LSUIElement.
cp "$ROOT/app/Resources/Info.plist" "$BUNDLE/Contents/Info.plist"

# The Dock icon, built from the same artwork the app draws its own mark from, so the two cannot
# drift. `branding/logo/` is the only place the logo lives.
ICON_SRC="$ROOT/branding/logo/logo.pdf"
[ -f "$ICON_SRC" ] || ICON_SRC="$ROOT/branding/logo/logo.png"
if [ -f "$ICON_SRC" ]; then
  echo "==> app icon"
  WORK="$(mktemp -d)"
  ICONSET="$WORK/Loki.iconset"
  mkdir -p "$ICONSET"

  # Each icon size is rasterised from the source at that size, rather than one large raster being
  # scaled down to the rest. For a vector source that is the difference between an analytic render
  # at 16 points and a resample of a 1024 point bitmap, and 16 is the size where it shows.
  # `sips` cannot read PDF, so this goes through AppKit, which is also what the app itself uses.
  cat > "$WORK/rasterise.swift" <<'RASTERISE'
import AppKit
let source = URL(fileURLWithPath: CommandLine.arguments[1])
let pixels = Int(CommandLine.arguments[2])!
let out = URL(fileURLWithPath: CommandLine.arguments[3])
guard let art = NSImage(contentsOf: source) else { exit(1) }
art.size = NSSize(width: pixels, height: pixels)
guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
) else { exit(1) }
rep.size = NSSize(width: pixels, height: pixels)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current?.imageInterpolation = .high
art.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels))
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: out)
RASTERISE

  for size in 16 32 128 256 512; do
    swift "$WORK/rasterise.swift" "$ICON_SRC" "$size" "$ICONSET/icon_${size}x${size}.png"
    swift "$WORK/rasterise.swift" "$ICON_SRC" "$((size * 2))" \
      "$ICONSET/icon_${size}x${size}@2x.png"
  done
  iconutil -c icns "$ICONSET" -o "$BUNDLE/Contents/Resources/Loki.icns"
  rm -rf "$WORK"
else
  echo "warning: no artwork in branding/logo, so the app keeps the default icon" >&2
fi

echo "==> ad-hoc signing"
codesign --force --sign - "$BUNDLE"

echo
echo "Built $BUNDLE"
echo "Run it with: open $BUNDLE"
