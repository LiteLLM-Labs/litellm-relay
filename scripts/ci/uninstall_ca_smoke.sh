#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
home="$tmp_dir/home"
keychain="$home/Library/Keychains/login.keychain-db"
relay_home="$home/.litellm-relay"
label="LiteLLM Relay Local Root CA"

cleanup() {
  security delete-keychain "$keychain" > /dev/null 2>&1 || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

fail() {
  echo "uninstall CA smoke failed: $*" >&2
  cat "$tmp_dir/uninstall.out" >&2 2> /dev/null || true
  exit 1
}

mint_ca() {
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
    -subj "/CN=$label" -keyout "$1.key" -out "$1" > /dev/null 2>&1
  security import "$1" -k "$keychain" -t cert > /dev/null
}

fingerprint_of() {
  openssl x509 -in "$1" -noout -fingerprint -sha256 | sed 's/^.*=//; s/://g'
}

relay_ca_count() {
  security find-certificate -a -c "$label" -Z "$keychain" 2> /dev/null | grep -c '^SHA-256 hash:' || true
}

run_uninstall() {
  HOME="$home" RELAY_HOME="$relay_home" PATH="$tmp_dir/shims:$PATH" \
    bash "$ROOT/src/uninstall.sh" --keep-bin --remove-ca-trust > "$tmp_dir/uninstall.out" 2>&1 \
    || fail "uninstall.sh exited non-zero"
}

mkdir -p "$home/Library/Keychains" "$relay_home/mitm" "$tmp_dir/shims"
printf '#!/bin/bash\nexec "$@"\n' > "$tmp_dir/shims/sudo"
chmod 755 "$tmp_dir/shims/sudo"
security create-keychain -p relay-ci "$keychain"

mint_ca "$tmp_dir/stale-ca.pem"
mint_ca "$relay_home/mitm/litellm-relay-ca.pem"
[[ "$(relay_ca_count)" == "2" ]] || fail "expected two Relay CA certificates before the uninstall, found $(relay_ca_count)"

run_uninstall
[[ "$(relay_ca_count)" == "0" ]] || fail "the uninstall left $(relay_ca_count) Relay CA certificate(s) in the keychain"
grep -q "Removed every \"$label\" certificate from $keychain" "$tmp_dir/uninstall.out" || fail "the uninstall did not report the removal"
grep -q "$(fingerprint_of "$tmp_dir/stale-ca.pem") untrusted" "$tmp_dir/uninstall.out" || fail "the uninstall did not name the stale CA it removed"
grep -q "$(fingerprint_of "$relay_home/mitm/litellm-relay-ca.pem") untrusted" "$tmp_dir/uninstall.out" || fail "the uninstall did not name the current CA it removed"

run_uninstall
grep -q "No \"$label\" certificate is in $keychain" "$tmp_dir/uninstall.out" || fail "a second uninstall did not report the empty keychain"

echo "uninstall CA smoke passed"
