#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Check that every entry in a macOS .pkg payload is readable by every user, so
the postinstall can run install.sh as the console user once the payload lands
under /usr/local owned by root.

Usage:
  scripts/macos-pkg/check-payload.sh PKG

Exits 1 and lists the offending entries with their modes otherwise.
USAGE
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  "")
    usage >&2
    exit 2
    ;;
esac

PKG="$1"
if [[ ! -f "$PKG" ]]; then
  echo "package not found: $PKG" >&2
  exit 1
fi

EXPAND_DIR="$(mktemp -d)"
trap 'rm -rf "$EXPAND_DIR"' EXIT
pkgutil --expand "$PKG" "$EXPAND_DIR/pkg"

PRIVATE_ENTRIES="$(lsbom -p fm "$EXPAND_DIR/pkg/Bom" | awk -F '\t' '
  {
    mode = $2
    kind = substr(mode, 1, length(mode) - 4)
    others = substr(mode, length(mode), 1)
    if (kind == "4" && others != "5" && others != "7") {
      print $1 " " mode " (directory other users cannot read and enter)"
    }
    if (kind == "10" && others < "4") {
      print $1 " " mode " (file other users cannot read)"
    }
  }')"

if [[ -n "$PRIVATE_ENTRIES" ]]; then
  cat >&2 <<PRIVATE
The payload of $PKG has entries other users cannot read, so the postinstall's
install.sh run as the console user would be denied:
$PRIVATE_ENTRIES
PRIVATE
  exit 1
fi

echo "Payload of $PKG is readable by every user."
