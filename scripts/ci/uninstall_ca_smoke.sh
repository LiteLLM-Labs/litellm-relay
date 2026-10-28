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
  chmod -R u+w "$tmp_dir" 2> /dev/null || true
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
grep -qF "$ROOT/src/uninstall.sh --remove-data" "$tmp_dir/uninstall.out" || fail "the cleanup hints did not name the uninstall.sh that ran"
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
grep -qx "launchctl bootout gui/$(id -u)/ai.litellm.relaybar" "$tmp_dir/calls.log" \
  || fail "the uninstall did not stop the RelayBar agent"

run_uninstall
grep -q "No \"$label\" certificate is in $keychain" "$tmp_dir/uninstall.out" || fail "a second uninstall did not report the empty keychain"

mint_ca "$current"
mkdir -p "$home/Library/LaunchAgents"
touch "$home/Library/LaunchAgents/ai.litellm.relaybar.plist"
payload="$tmp_dir/payload"
mkdir -p "$payload/RelayBarGlass.app/Contents/MacOS"
: > "$tmp_dir/calls.log"
with_stubs HOME="$tmp_dir/root-home" RELAY_SMOKE_UID=0 RELAY_SMOKE_MANAGERNAME=System \
  RELAY_SMOKE_CONSOLE_USER=relay-smoke RELAY_SMOKE_CONSOLE_HOME="$home" RELAY_SMOKE_CONSOLE_UID=4242 \
  RELAY_PKG_PAYLOAD="$payload" \
  bash "$ROOT/src/uninstall.sh" --remove-ca-trust > "$tmp_dir/uninstall.out" 2>&1 \
  || fail "uninstall.sh run as root exited non-zero"
grep -qx "launchctl bootout gui/4242/ai.litellm.relaybar" "$tmp_dir/calls.log" \
  || fail "an uninstall run as root did not stop the console user's RelayBar agent"
[[ ! -e "$payload" ]] || fail "an uninstall run as root left the package payload with RelayBarGlass.app"
grep -qx "pkgutil --forget ai.litellm.relay" "$tmp_dir/calls.log" || fail "an uninstall run as root kept the package receipt"
grep -q "Package payload: $payload" "$tmp_dir/uninstall.out" || fail "the uninstall did not report the package payload it removed"
[[ ! -e "$home/Library/LaunchAgents/ai.litellm.relaybar.plist" ]] || fail "an uninstall run as root left the console user's RelayBar plist"
grep -q "Relay data: $relay_home" "$tmp_dir/uninstall.out" || fail "an uninstall run as root did not target the console user's Relay data"
grep -q "warning: the Relay CA was left in $keychain" "$tmp_dir/uninstall.out" || fail "an uninstall run as root did not look in the console user's keychain"
grep -q "^  $(sha1_of "$current")\$" "$tmp_dir/uninstall.out" || fail "an uninstall run as root did not list the console user's CA"

as_root() {
  : > "$tmp_dir/calls.log"
  with_stubs HOME="$tmp_dir/root-home" RELAY_SMOKE_UID=0 RELAY_PKG_PAYLOAD="$payload" "$@"
}

as_root_with_console_user() {
  as_root RELAY_SMOKE_CONSOLE_USER=relay-smoke RELAY_SMOKE_CONSOLE_HOME="$home" RELAY_SMOKE_CONSOLE_UID=4242 "$@"
}

mint_ca "$stale"
sha1_of "$stale" > "$tmp_dir/trusted"
as_root_with_console_user bash "$ROOT/src/uninstall.sh" --keep-bin --remove-ca-trust > "$tmp_dir/uninstall.out" 2>&1 \
  || fail "uninstall.sh run as root from the user's GUI session exited non-zero"
[[ "$(relay_ca_count)" == "0" ]] || fail "a root uninstall in the GUI session left $(relay_ca_count) Relay CA certificate(s)"
[[ ! -s "$tmp_dir/trusted" ]] || fail "a root uninstall in the GUI session left the console user's trust setting: $(cat "$tmp_dir/trusted")"
as_user="launchctl asuser 4242 sudo -H -u relay-smoke security"
grep -qx "$as_user delete-certificate -Z $(sha1_of "$stale") -t $keychain" "$tmp_dir/calls.log" \
  || fail "a root uninstall did not delete the console user's trusted CA as that user together with its trust setting"
[[ "$(grep -c '^security ' "$tmp_dir/calls.log")" == "$(grep -c "^$as_user " "$tmp_dir/calls.log")" ]] \
  || fail "a root uninstall ran security outside the console user's session: $(grep '^security ' "$tmp_dir/calls.log")"

mkdir -p "$payload/RelayBarGlass.app/Contents/MacOS"
touch "$home/Library/LaunchAgents/ai.litellm.relaybar.plist"
if as_root RELAY_SMOKE_CONSOLE_USER=loginwindow bash "$ROOT/src/uninstall.sh" --remove-data > "$tmp_dir/uninstall.out" 2>&1; then
  fail "uninstall.sh run as root at the login window exited 0"
fi
grep -q "nobody is logged in at the console" "$tmp_dir/uninstall.out" || fail "the login window refusal did not say why"
grep -q "RELAY_USER_HOME=/Users/<name>" "$tmp_dir/uninstall.out" || fail "the login window refusal did not say how to name a user"
[[ ! -s "$tmp_dir/calls.log" ]] || fail "uninstall.sh run as root at the login window changed something: $(cat "$tmp_dir/calls.log")"
[[ -d "$payload" ]] || fail "uninstall.sh run as root at the login window removed the package payload"
[[ -e "$home/Library/LaunchAgents/ai.litellm.relaybar.plist" ]] || fail "uninstall.sh run as root at the login window removed a user's plist"
[[ -d "$relay_home" ]] || fail "uninstall.sh run as root at the login window removed Relay data"

as_root RELAY_SMOKE_CONSOLE_USER=loginwindow RELAY_USER_HOME="$home" bash "$ROOT/src/uninstall.sh" > "$tmp_dir/uninstall.out" 2>&1 \
  || fail "uninstall.sh run as root with RELAY_USER_HOME exited non-zero"
grep -qx "launchctl bootout gui/$(id -u)/ai.litellm.relaybar" "$tmp_dir/calls.log" \
  || fail "uninstall.sh run as root with RELAY_USER_HOME did not stop that user's RelayBar agent"
[[ ! -e "$home/Library/LaunchAgents/ai.litellm.relaybar.plist" ]] || fail "uninstall.sh run as root with RELAY_USER_HOME left that user's RelayBar plist"
grep -q "Relay data: $relay_home" "$tmp_dir/uninstall.out" || fail "uninstall.sh run as root with RELAY_USER_HOME did not target that user's Relay data"

mkdir -p "$payload/RelayBarGlass.app/Contents/MacOS"
: > "$tmp_dir/calls.log"
with_stubs HOME="$home" RELAY_HOME="$relay_home" RELAY_SMOKE_SUDO_FAILS=1 RELAY_PKG_PAYLOAD="$payload" \
  bash "$ROOT/src/uninstall.sh" --remove-data > "$tmp_dir/uninstall.out" 2>&1 \
  || fail "a user uninstall on a Mac with the package payload exited non-zero"
[[ -d "$payload" ]] || fail "a user uninstall removed the package payload shared with every account"
if grep -q "^sudo .*$payload" "$tmp_dir/calls.log" || grep -q '^pkgutil ' "$tmp_dir/calls.log"; then
  fail "a user uninstall reached for the package payload or its receipt: $(cat "$tmp_dir/calls.log")"
fi
grep -q "The shared package payload at $payload stays.*sudo $payload/uninstall.sh" "$tmp_dir/uninstall.out" \
  || fail "a user uninstall did not say who removes the package payload"
[[ ! -e "$relay_home" ]] || fail "a user uninstall on a Mac with the package payload did not remove Relay data"
grep -q "LiteLLM Relay uninstall complete." "$tmp_dir/uninstall.out" || fail "a user uninstall on a Mac with the package payload printed no summary"

mkdir -p "$relay_home/mitm"
chmod 0555 "$payload"
as_root_with_console_user bash "$ROOT/src/uninstall.sh" --remove-data > "$tmp_dir/uninstall.out" 2>&1 \
  || fail "a root uninstall whose payload removal failed exited non-zero"
chmod 0755 "$payload"
[[ -d "$payload" ]] || fail "the smoke could not make the payload removal fail"
grep -q "warning: could not remove the package payload at $payload" "$tmp_dir/uninstall.out" \
  || fail "a root uninstall did not warn that the payload removal failed"
if grep -q '^pkgutil --forget' "$tmp_dir/calls.log"; then
  fail "a root uninstall forgot the receipt of a payload it could not remove"
fi
[[ ! -e "$relay_home" ]] || fail "a failed payload removal stopped --remove-data"
grep -q "LiteLLM Relay uninstall complete." "$tmp_dir/uninstall.out" || fail "a failed payload removal stopped the summary"

echo "uninstall CA smoke passed"
