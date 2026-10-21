#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
home="$tmp_dir/home"
keychain="$home/Library/Keychains/login.keychain-db"
relay_home="$home/.litellm-relay"
label="LiteLLM Relay Local Root CA"
# shellcheck source=scripts/ci/smoke_stubs.sh
source "$ROOT/scripts/ci/smoke_stubs.sh"

cleanup() {
  /usr/bin/security delete-keychain "$keychain" > /dev/null 2>&1 || true
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
  /usr/bin/security import "$1" -k "$keychain" -t cert > /dev/null
}

sha1_of() {
  openssl x509 -in "$1" -noout -fingerprint -sha1 | sed 's/^.*=//; s/://g'
}

relay_ca_count() {
  /usr/bin/security find-certificate -a -c "$label" -Z "$keychain" 2> /dev/null | grep -c '^SHA-1 hash:' || true
}

run_uninstall() {
  : > "$tmp_dir/calls.log"
  with_stubs HOME="$home" RELAY_HOME="$relay_home" "$@" \
    bash "$ROOT/src/uninstall.sh" --keep-bin --remove-ca-trust > "$tmp_dir/uninstall.out" 2>&1 \
    || fail "uninstall.sh exited non-zero"
}

mkdir -p "$home/Library/Keychains" "$relay_home/mitm"
/usr/bin/security create-keychain -p relay-ci "$keychain"
stale="$tmp_dir/stale-ca.pem"
current="$relay_home/mitm/litellm-relay-ca.pem"

mint_ca "$stale"
mint_ca "$current"
[[ "$(relay_ca_count)" == "2" ]] || fail "expected two Relay CA certificates before the uninstall, found $(relay_ca_count)"

run_uninstall RELAY_SMOKE_MANAGERNAME=System
[[ "$(relay_ca_count)" == "2" ]] || fail "an uninstall outside the GUI session should leave both Relay CA certificates, found $(relay_ca_count)"
grep -q "warning: the Relay CA was left in $keychain" "$tmp_dir/uninstall.out" || fail "the uninstall outside the GUI session did not warn"
grep -q "launchctl managername: System" "$tmp_dir/uninstall.out" || fail "the warning did not name the session"
grep -q "^  $(sha1_of "$stale")\$" "$tmp_dir/uninstall.out" || fail "the warning did not list the stale CA"
grep -q "^  $(sha1_of "$current")\$" "$tmp_dir/uninstall.out" || fail "the warning did not list the current CA"
grep '^  security delete-certificate ' "$tmp_dir/uninstall.out" | sed 's/^  //' > "$tmp_dir/fallback.sh"
[[ "$(grep -c . "$tmp_dir/fallback.sh")" == "2" ]] || fail "the warning should give one delete command per CA"
with_stubs bash "$tmp_dir/fallback.sh" > "$tmp_dir/fallback.out" 2>&1 || { cat "$tmp_dir/fallback.out" >&2; fail "the printed delete commands failed"; }
[[ "$(relay_ca_count)" == "0" ]] || fail "the printed delete commands left $(relay_ca_count) Relay CA certificate(s)"

mint_ca "$stale"
mint_ca "$current"
sha1_of "$stale" > "$tmp_dir/trusted"
run_uninstall
[[ "$(relay_ca_count)" == "0" ]] || fail "the uninstall left $(relay_ca_count) Relay CA certificate(s) in the keychain"
grep -q "Removed every \"$label\" certificate from $keychain" "$tmp_dir/uninstall.out" || fail "the uninstall did not report the removal"
grep -q "$(sha1_of "$stale") trusted" "$tmp_dir/uninstall.out" || fail "the uninstall did not name the trusted stale CA it removed"
grep -q "$(sha1_of "$current") untrusted" "$tmp_dir/uninstall.out" || fail "the uninstall did not name the current CA it removed"
grep -qx "security delete-certificate -Z $(sha1_of "$stale") -t $keychain" "$tmp_dir/calls.log" \
  || fail "the trusted stale CA was not deleted together with its trust setting"
[[ ! -s "$tmp_dir/trusted" ]] || fail "a Relay CA trust setting was left behind: $(cat "$tmp_dir/trusted")"
grep -qx "launchctl bootout gui/$(id -u) $home/Library/LaunchAgents/ai.litellm.relaybar.plist" "$tmp_dir/calls.log" \
  || fail "the uninstall did not stop the RelayBar agent"

run_uninstall
grep -q "No \"$label\" certificate is in $keychain" "$tmp_dir/uninstall.out" || fail "a second uninstall did not report the empty keychain"

mint_ca "$current"
run_uninstall HOME="$tmp_dir/root-home" RELAY_SMOKE_UID=0 RELAY_SMOKE_MANAGERNAME=System \
  RELAY_SMOKE_CONSOLE_USER=relay-smoke RELAY_SMOKE_CONSOLE_HOME="$home"
grep -q "warning: the Relay CA was left in $keychain" "$tmp_dir/uninstall.out" || fail "an uninstall run as root did not look in the console user's keychain"
grep -q "^  $(sha1_of "$current")\$" "$tmp_dir/uninstall.out" || fail "an uninstall run as root did not list the console user's CA"

echo "uninstall CA smoke passed"
