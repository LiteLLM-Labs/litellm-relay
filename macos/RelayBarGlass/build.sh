#!/bin/bash
# Build RelayBarGlass (dark glass menu bar app) and assemble a .app bundle.
set -euo pipefail
cd "$(dirname "$0")"

swift build -c release

APP="RelayBarGlass.app"
BIN=".build/release/RelayBarGlass"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/RelayBarGlass"
cp Info.plist "$APP/Contents/Info.plist"
codesign --force --sign - "$APP" >/dev/null 2>&1 || true

echo "built $(pwd)/$APP"
