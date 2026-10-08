#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

fail() {
  echo "pkg payload smoke failed: $*" >&2
  exit 1
}

bom_mode() {
  lsbom -p fm "$1" | awk -F '\t' -v entry="$2" '$1 == entry { print $2 }'
}

printf '#!/bin/sh\nexit 0\n' > "$tmp_dir/stub-binary"
chmod 755 "$tmp_dir/stub-binary"

"$ROOT/scripts/build-macos-pkg.sh" \
  --version 0.0.0-ci \
  --binary "$tmp_dir/stub-binary" \
  --config-file "$ROOT/mdm/config.yaml.example" \
  --output "$tmp_dir/relay.pkg" > "$tmp_dir/build.out" 2>&1 || {
  cat "$tmp_dir/build.out" >&2
  fail "build-macos-pkg.sh exited non-zero"
}

pkgutil --expand "$tmp_dir/relay.pkg" "$tmp_dir/expanded"
root_mode="$(bom_mode "$tmp_dir/expanded/Bom" .)"
[[ "$root_mode" == "40755" ]] || fail "payload root mode is $root_mode, expected 40755"
installer_mode="$(bom_mode "$tmp_dir/expanded/Bom" ./install.sh)"
[[ "$installer_mode" == "100755" ]] || fail "install.sh mode is $installer_mode, expected 100755"
config_mode="$(bom_mode "$tmp_dir/expanded/Bom" ./config.yaml)"
[[ "$config_mode" == "100644" ]] || fail "config.yaml mode is $config_mode, expected 100644"
[[ -f "$tmp_dir/expanded/Scripts/postinstall" ]] || fail "the package carries no postinstall"
grep -q 'Payload of .* is readable by every user' "$tmp_dir/build.out" || fail "the builder did not check the payload"

mkdir -m 0700 "$tmp_dir/private-root"
install -m 0755 "$tmp_dir/stub-binary" "$tmp_dir/private-root/litellm-relay"
install -m 0600 "$ROOT/mdm/config.yaml.example" "$tmp_dir/private-root/config.yaml"
pkgbuild \
  --root "$tmp_dir/private-root" \
  --identifier ai.litellm.relay.ci \
  --version 0.0.0-ci \
  --install-location /usr/local/litellm-relay \
  "$tmp_dir/private.pkg" > /dev/null

if "$ROOT/scripts/macos-pkg/check-payload.sh" "$tmp_dir/private.pkg" > "$tmp_dir/check.out" 2>&1; then
  fail "check-payload.sh accepted a payload staged in a 0700 root"
fi
grep -q '^\. 40700 ' "$tmp_dir/check.out" || fail "check-payload.sh did not name the 0700 payload root"
grep -q '^\./config\.yaml 100600 ' "$tmp_dir/check.out" || fail "check-payload.sh did not name the 0600 config file"

echo "pkg payload smoke passed"
