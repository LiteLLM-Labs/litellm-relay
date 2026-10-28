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

bom_uid() {
  lsbom -p fu "$1" | awk -F '\t' -v entry="$2" '$1 == entry { print $2 }'
}

build_component() {
  local name="$1" root_mode="$2" config_mode="$3"
  mkdir -m "$root_mode" "$tmp_dir/$name-root"
  install -m 0755 "$tmp_dir/stub-binary" "$tmp_dir/$name-root/litellm-relay"
  install -m "$config_mode" "$ROOT/mdm/config.yaml.example" "$tmp_dir/$name-root/config.yaml"
  pkgbuild \
    --root "$tmp_dir/$name-root" \
    --identifier ai.litellm.relay.ci \
    --version 0.0.0-ci \
    --install-location /usr/local/litellm-relay \
    "$tmp_dir/$name.pkg" > /dev/null
}

printf '#!/bin/sh\nexit 0\n' > "$tmp_dir/stub-binary"
chmod 755 "$tmp_dir/stub-binary"
app="$tmp_dir/RelayBarGlass.app"
mkdir -p "$app/Contents/MacOS"
cp "$ROOT/macos/RelayBarGlass/Info.plist" "$app/Contents/Info.plist"
install -m 0755 "$tmp_dir/stub-binary" "$app/Contents/MacOS/RelayBarGlass"
relaybar_id="$(plutil -extract CFBundleIdentifier raw "$app/Contents/Info.plist")"

mkdir "$tmp_dir/no-swift"
ln -s /usr/bin/dirname /usr/bin/uname "$tmp_dir/no-swift/"
if PATH="$tmp_dir/no-swift" /bin/bash "$ROOT/scripts/build-macos-pkg.sh" --version 0.0.0-ci \
  --binary "$tmp_dir/stub-binary" --relaybar --output "$tmp_dir/no-swift.pkg" > "$tmp_dir/no-swift.out" 2>&1; then
  fail "build-macos-pkg.sh --relaybar succeeded with no swift on PATH"
fi
grep -q -- '--relaybar needs swift on PATH' "$tmp_dir/no-swift.out" || { cat "$tmp_dir/no-swift.out" >&2; fail "the builder did not say --relaybar needs swift"; }
[[ ! -e "$tmp_dir/no-swift.pkg" ]] || fail "the builder wrote a package without the RelayBar app"

"$ROOT/scripts/build-macos-pkg.sh" \
  --version 0.0.0-ci \
  --binary "$tmp_dir/stub-binary" \
  --config-file "$ROOT/mdm/config.yaml.example" \
  --relaybar-app "$app" \
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
[[ "$config_mode" == "100600" ]] || fail "config.yaml mode is $config_mode, expected 100600"
config_uid="$(bom_uid "$tmp_dir/expanded/Bom" ./config.yaml)"
[[ "$config_uid" == "0" ]] || fail "config.yaml is owned by uid $config_uid, expected root"
[[ -f "$tmp_dir/expanded/Scripts/postinstall" ]] || fail "the package carries no postinstall"
grep -q 'Payload of .* is readable by every user' "$tmp_dir/build.out" || fail "the builder did not check the payload"
app_binary_mode="$(bom_mode "$tmp_dir/expanded/Bom" ./RelayBarGlass.app/Contents/MacOS/RelayBarGlass)"
[[ "$app_binary_mode" == "100755" ]] || fail "the RelayBar app binary mode is '$app_binary_mode', expected 100755"
[[ "$(xmllint --xpath "count(/pkg-info/bundle[@id='$relaybar_id'])" "$tmp_dir/expanded/PackageInfo")" == "1" ]] \
  || fail "PackageInfo does not list the $relaybar_id bundle"
[[ "$(xmllint --xpath "count(/pkg-info/relocate/bundle[@id='$relaybar_id'])" "$tmp_dir/expanded/PackageInfo")" == "0" ]] \
  || fail "the installer may relocate $relaybar_id away from /usr/local/litellm-relay"

build_component private 0700 0600
if "$ROOT/scripts/macos-pkg/check-payload.sh" "$tmp_dir/private.pkg" > "$tmp_dir/check.out" 2>&1; then
  fail "check-payload.sh accepted a payload staged in a 0700 root"
fi
grep -q '^\. 40700 ' "$tmp_dir/check.out" || fail "check-payload.sh did not name the 0700 payload root"
if grep -q '^\./config\.yaml ' "$tmp_dir/check.out"; then
  cat "$tmp_dir/check.out" >&2
  fail "check-payload.sh flagged a root-only 0600 config file"
fi

build_component readable-config 0755 0644
if "$ROOT/scripts/macos-pkg/check-payload.sh" "$tmp_dir/readable-config.pkg" > "$tmp_dir/check-readable.out" 2>&1; then
  fail "check-payload.sh accepted a config.yaml every user can read"
fi
grep -q '^\./config\.yaml 100644 ' "$tmp_dir/check-readable.out" || fail "check-payload.sh did not name the world-readable config file"

productbuild --package "$tmp_dir/private.pkg" "$tmp_dir/private-dist.pkg" > /dev/null
if "$ROOT/scripts/macos-pkg/check-payload.sh" "$tmp_dir/private-dist.pkg" > "$tmp_dir/check-dist.out" 2>&1; then
  fail "check-payload.sh accepted a distribution package whose component has a 0700 root"
fi
grep -q '^\. 40700 ' "$tmp_dir/check-dist.out" || fail "check-payload.sh did not name the 0700 payload root inside the distribution package"

productbuild --package "$tmp_dir/relay.pkg" "$tmp_dir/relay-dist.pkg" > /dev/null
"$ROOT/scripts/macos-pkg/check-payload.sh" "$tmp_dir/relay-dist.pkg" > "$tmp_dir/check-dist-ok.out" 2>&1 \
  || { cat "$tmp_dir/check-dist-ok.out" >&2; fail "check-payload.sh rejected a distribution package with a readable payload"; }

rm "$tmp_dir/expanded/Bom"
pkgutil --flatten "$tmp_dir/expanded" "$tmp_dir/no-bom.pkg"
if "$ROOT/scripts/macos-pkg/check-payload.sh" "$tmp_dir/no-bom.pkg" > "$tmp_dir/check-no-bom.out" 2>&1; then
  fail "check-payload.sh accepted a package with no Bom"
fi
grep -q 'it has no Bom' "$tmp_dir/check-no-bom.out" || fail "check-payload.sh did not say the Bom is missing"

echo "pkg payload smoke passed"
