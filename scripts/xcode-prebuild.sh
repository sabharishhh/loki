#!/usr/bin/env bash
#
# Builds the Rust core before Xcode builds the app.
#
# SwiftPM links `libloki_ffi.a` by path and has no dependency edge to the Rust sources, so Xcode on
# its own happily links yesterday's core against today's Swift. That is a silent wrong build: the
# app runs, and every fix made in the core is missing. It cost a round of "the token counters are
# still zero" against a binary that predated the fix.
#
# Install once, per scheme:
#   Product > Scheme > Edit Scheme > Build > Pre-actions > + > New Run Script Action
#   Provide build settings from: LokiApp
#   Script: "$SRCROOT/../scripts/xcode-prebuild.sh"
#
# A SwiftPM build tool plugin would need no setup and cannot be used: measured, its sandbox refuses
# to write to the workspace target directory, since that sits outside the package. Moving the Rust
# output inside the app package to satisfy it would be the tail wagging the dog.
#
# **Xcode ignores a pre-action's exit code.** So failing is not enough: this removes the archive on
# a failed build, which turns a silent stale link into a loud missing one. A build that will not
# link is a much better outcome than an app that runs and lies.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Xcode sets CONFIGURATION when the pre-action is given build settings. Default to debug, which is
# what the scheme's Build and Run use.
case "${CONFIGURATION:-Debug}" in
  Release) PROFILE="release"; FLAGS="--release" ;;
  *)       PROFILE="debug";   FLAGS="" ;;
esac

ARCHIVE="$ROOT/target/$PROFILE/libloki_ffi.a"

# Xcode's environment is not a login shell, so cargo is not on PATH.
export PATH="$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"

# Match the floor in Package.swift, or the linker warns once per object file in ring and libgit2.
# Also set in .cargo/config.toml; kept here so the script is correct when run on its own.
export MACOSX_DEPLOYMENT_TARGET=26.0

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not on PATH, so the core cannot be built" >&2
  rm -f "$ARCHIVE"
  exit 1
fi

# Run from the repo root so rustup reads `rust-toolchain.toml`. It resolves the toolchain from the
# working directory, never from `--manifest-path`, so a pre-action launched from anywhere else
# silently builds with whatever `stable` happens to be. That is the same class of failure this
# script exists to prevent, one level up: the right core, built by the wrong compiler.
cd "$ROOT" || { echo "error: cannot enter $ROOT" >&2; rm -f "$ARCHIVE"; exit 1; }

echo "building the core: cargo build $FLAGS"
if cargo build -p loki-ffi $FLAGS; then
  exit 0
fi

echo "error: the core did not build. Removing $ARCHIVE so the link fails rather than" >&2
echo "       succeeding against a stale library." >&2
rm -f "$ARCHIVE"
exit 1
