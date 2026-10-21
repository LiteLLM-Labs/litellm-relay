#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
label="LiteLLM Relay Local Root CA"
earlier_keychain="$tmp_dir/earlier/Library/Keychains/login.keychain-db"
# shellcheck source=scripts/ci/smoke_stubs.sh
source "$ROOT/scripts/ci/smoke_stubs.sh"

cleanup() {
  /usr/bin/security delete-keychain "$earlier_keychain" > /dev/null 2>&1 || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

fail() {
  echo "install CA trust smoke failed: $*" >&2
  exit 1
}

expect_line() {
  grep -q -- "$2" "$1" || { cat "$1" >&2; fail "$1 lacks: $2"; }
}

refute_line() {
  if grep -q -- "$2" "$1"; then
    cat "$1" >&2
    fail "$1 must not contain: $2"
  fi
}

mint_ca() {
  mkdir -p "$(dirname "$1")"
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
    -subj "/CN=$label" -keyout "$1.key" -out "$1" > /dev/null 2>&1
}

sha1_of() {
  openssl x509 -in "$1" -noout -fingerprint -sha1 | sed 's/^.*=//; s/://g'
}

cat > "$tmp_dir/stub-relay" <<'STUB'
#!/bin/bash
if [[ "${2:-}" == "--help" ]]; then
  exit 0
fi
case "$1" in
  ca-path)
    ca="$HOME/.litellm-relay/mitm/litellm-relay-ca.pem"
    if [[ ! -f "$ca" ]]; then
      mkdir -p "$(dirname "$ca")"
      openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
        -subj "/CN=LiteLLM Relay Local Root CA" -keyout "$ca.key" -out "$ca" > /dev/null 2>&1
    fi
    printf '%s\n' "$ca"
    ;;
  capture-mode)
    if grep -q 'payloads: true' "$HOME/.litellm-relay/config.yaml" 2> /dev/null; then
      echo payloads
    else
      echo metadata
    fi
    ;;
esac
STUB
chmod 755 "$tmp_dir/stub-relay"
printf 'capture:\n  payloads: false\n' > "$tmp_dir/metadata.yaml"
printf 'capture:\n  payloads: true\n' > "$tmp_dir/payloads.yaml"

run_install() {
  local name="$1"
  shift
  local home="$tmp_dir/$name"
  mkdir -p "$home"
  : > "$tmp_dir/calls.log"
  with_stubs HOME="$home" RELAY_HOME="$home/.litellm-relay" \
    bash "$ROOT/src/install.sh" --prebuilt-binary "$tmp_dir/stub-relay" --skip-autoconfigure --bin-dir "$home/bin" "$@" \
    > "$tmp_dir/$name.out" 2>&1 || { cat "$tmp_dir/$name.out" >&2; fail "install.sh ($name) exited non-zero"; }
  cp "$tmp_dir/calls.log" "$tmp_dir/$name.calls"
}

run_install managed-metadata --config-file "$tmp_dir/metadata.yaml" --skip-setup
expect_line "$tmp_dir/managed-metadata.out" 'Skipping Relay CA trust: this managed install keeps payload capture off'
expect_line "$tmp_dir/managed-metadata.out" 'Relay CA:    .* (not trusted)'
refute_line "$tmp_dir/managed-metadata.calls" 'add-trusted-cert'
expect_line "$tmp_dir/managed-metadata.calls" "^launchctl kickstart -k gui/$(id -u)/ai.litellm.relay$"

run_install config-file-only --config-file "$tmp_dir/metadata.yaml"
expect_line "$tmp_dir/config-file-only.out" 'Skipping Relay CA trust: this managed install keeps payload capture off'
refute_line "$tmp_dir/config-file-only.calls" 'add-trusted-cert'

run_install skip-setup-only --skip-setup
expect_line "$tmp_dir/skip-setup-only.out" 'Skipping Relay CA trust: this managed install keeps payload capture off'
refute_line "$tmp_dir/skip-setup-only.calls" 'add-trusted-cert'

run_install managed-payloads --config-file "$tmp_dir/payloads.yaml" --skip-setup
expect_line "$tmp_dir/managed-payloads.out" 'Certificate Trust Settings sheet'
expect_line "$tmp_dir/managed-payloads.out" 'Relay CA:    .* (trusted in the login keychain)'
expect_line "$tmp_dir/managed-payloads.calls" "add-trusted-cert -r trustRoot -k $tmp_dir/managed-payloads/Library/Keychains/login.keychain-db $tmp_dir/managed-payloads/.litellm-relay/mitm/litellm-relay-ca.pem"

mint_ca "$tmp_dir/pre-trusted/.litellm-relay/mitm/litellm-relay-ca.pem"
sha1_of "$tmp_dir/pre-trusted/.litellm-relay/mitm/litellm-relay-ca.pem" >> "$tmp_dir/trusted"
run_install pre-trusted --config-file "$tmp_dir/payloads.yaml" --skip-setup
expect_line "$tmp_dir/pre-trusted.out" 'Relay CA:    .* (already trusted)'
refute_line "$tmp_dir/pre-trusted.out" 'Certificate Trust Settings sheet'
refute_line "$tmp_dir/pre-trusted.calls" 'add-trusted-cert'

RELAY_TRUST_CA=1 run_install managed-opt-in --config-file "$tmp_dir/metadata.yaml" --skip-setup
expect_line "$tmp_dir/managed-opt-in.calls" 'add-trusted-cert'

run_install managed-skip --config-file "$tmp_dir/payloads.yaml" --skip-setup --skip-trust-ca
expect_line "$tmp_dir/managed-skip.out" 'RELAY_TRUST_CA=0 or --skip-trust-ca'
refute_line "$tmp_dir/managed-skip.calls" 'add-trusted-cert'

run_install interactive
expect_line "$tmp_dir/interactive.out" 'Certificate Trust Settings sheet'
expect_line "$tmp_dir/interactive.calls" 'add-trusted-cert'

mkdir -p "$(dirname "$earlier_keychain")"
/usr/bin/security create-keychain -p relay-ci "$earlier_keychain"
mint_ca "$tmp_dir/earlier-ca.pem"
/usr/bin/security import "$tmp_dir/earlier-ca.pem" -k "$earlier_keychain" -t cert > /dev/null
earlier_sha1="$(sha1_of "$tmp_dir/earlier-ca.pem")"
printf '%s\n' "$earlier_sha1" >> "$tmp_dir/trusted"

run_install earlier --config-file "$tmp_dir/metadata.yaml" --skip-setup
expect_line "$tmp_dir/earlier.out" 'Relay CA:    .* (still trusted in the login keychain from an earlier install)'
expect_line "$tmp_dir/earlier.out" "security delete-certificate -Z $earlier_sha1 -t \"$earlier_keychain\""
refute_line "$tmp_dir/earlier.calls" 'add-trusted-cert'

RELAY_SMOKE_MANAGERNAME=System run_install earlier --config-file "$tmp_dir/metadata.yaml" --skip-setup
expect_line "$tmp_dir/earlier.out" "Relay CA:    .* (not trusted by this install; an earlier install's trust cannot be read outside the user's GUI session)"

echo "install CA trust smoke passed"
