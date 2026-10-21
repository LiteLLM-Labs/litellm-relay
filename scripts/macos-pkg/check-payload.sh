#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Check that every entry in a macOS .pkg payload is readable by every user, so
the postinstall can run install.sh as the console user once the payload lands
under /usr/local owned by root. A distribution package (productbuild) is
checked component by component.

Usage:
  scripts/macos-pkg/check-payload.sh PKG

Exits 1 and lists the offending entries with their modes otherwise, and when
a component has no Bom to read the modes from.
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

if [[ -f "$EXPAND_DIR/pkg/Distribution" ]]; then
  COMPONENTS=("$EXPAND_DIR"/pkg/*.pkg)
else
  COMPONENTS=("$EXPAND_DIR/pkg")
fi

for component in "${COMPONENTS[@]}"; do
  if [[ ! -f "$component/Bom" ]]; then
    name="${component#"$EXPAND_DIR/pkg"}"
    echo "cannot check $PKG${name:+ (component ${name#/})}: it has no Bom, so its payload modes are unknown." >&2
    exit 1
  fi
done

private_entries() {
  local component
  for component in "${COMPONENTS[@]}"; do
    lsbom -p fm "$component/Bom" | awk -F '\t' '
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
      }'
  done
}

PRIVATE_ENTRIES="$(private_entries)"

if [[ -n "$PRIVATE_ENTRIES" ]]; then
  cat >&2 <<PRIVATE
The payload of $PKG has entries other users cannot read, so the postinstall's
install.sh run as the console user would be denied:
$PRIVATE_ENTRIES
PRIVATE
  exit 1
fi

echo "Payload of $PKG is readable by every user."
