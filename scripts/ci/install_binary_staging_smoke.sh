#!/bin/bash
# Proves install.sh replaces an installed binary that is still running without
# breaking either copy: the old process keeps its file (new inode for the new
# binary), and the new file executes. Copying over the old inode in place is
# what left every `relay` call "Killed: 9" on macOS and the installer at 137.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck disable=SC1090
source <(sed -n '/^install_relay_binary()/,/^}/p; /^detect_network_service()/,/^}/p' "$REPO_ROOT/src/install.sh")

WORK="$(mktemp -d)"
trap 'kill "${OLD_PID:-}" 2>/dev/null || true; rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"
TARGET="$WORK/bin/litellm-relay"
# A stand-in daemon built here, not a copied Apple platform binary: macOS
# kills a relocated platform binary when its file is replaced, which would
# fail this test for a reason unrelated to Relay's ad hoc signed binary.
cat > "$WORK/idle.c" <<'C'
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  if (argc > 1 && strcmp(argv[1], "--help") == 0) { puts("idle"); return 0; }
  for (;;) pause();
}
C
cc -o "$WORK/idle" "$WORK/idle.c"

install_relay_binary "$WORK/idle" "$TARGET"
OLD_INODE="$(stat -f %i "$TARGET" 2>/dev/null || stat -c %i "$TARGET")"
"$TARGET" &
OLD_PID=$!
sleep 1

install_relay_binary "$WORK/idle" "$TARGET"
NEW_INODE="$(stat -f %i "$TARGET" 2>/dev/null || stat -c %i "$TARGET")"
[[ "$NEW_INODE" != "$OLD_INODE" ]] || { echo "FAIL: the binary was overwritten in place (inode $OLD_INODE)"; exit 1; }
kill -0 "$OLD_PID" || { echo "FAIL: the running old binary died during the upgrade"; exit 1; }
"$TARGET" --help >/dev/null || { echo "FAIL: the freshly installed binary does not execute (exit $?)"; exit 1; }
MODE="$(stat -f %Lp "$TARGET" 2>/dev/null || stat -c %a "$TARGET")"
[[ "$MODE" == "700" ]] || { echo "FAIL: installed binary mode is $MODE, expected 700"; exit 1; }
[[ -z "$(ls -A "$WORK/bin" | grep -v '^litellm-relay$')" ]] || { echo "FAIL: staging file left behind"; ls -la "$WORK/bin"; exit 1; }
echo "ok: staged binary replace keeps pid $OLD_PID alive and the new file (inode $NEW_INODE) runs"

if [[ "$(uname -s)" == "Darwin" ]]; then
  SERVICE="$(detect_network_service)"
  [[ -n "$SERVICE" ]] || { echo "FAIL: detect_network_service printed nothing"; exit 1; }
  networksetup -listallnetworkservices | grep -Fxq "$SERVICE" || { echo "FAIL: '$SERVICE' is not a network service on this Mac"; exit 1; }
  echo "ok: detect_network_service -> $SERVICE"
fi
