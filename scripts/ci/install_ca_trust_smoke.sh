#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

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

mkdir -p "$tmp_dir/shims"
cat > "$tmp_dir/shims/security" <<'SHIM'
#!/bin/bash
printf '%s\n' "$*" >> "$SECURITY_CALLS"
SHIM
cat > "$tmp_dir/stub-relay" <<'STUB'
#!/bin/bash
case "$1" in
  ca-path)
    printf '%s/.litellm-relay/mitm/litellm-relay-ca.pem\n' "$HOME"
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
chmod 755 "$tmp_dir/shims/security" "$tmp_dir/stub-relay"
printf 'capture:\n  payloads: false\n' > "$tmp_dir/metadata.yaml"
printf 'capture:\n  payloads: true\n' > "$tmp_dir/payloads.yaml"

run_install() {
  local name="$1"
  shift
  local home="$tmp_dir/$name"
  mkdir -p "$home"
  SECURITY_CALLS="$tmp_dir/$name.security" HOME="$home" RELAY_HOME="$home/.litellm-relay" PATH="$tmp_dir/shims:$PATH" \
    bash "$ROOT/src/install.sh" --prebuilt-binary "$tmp_dir/stub-relay" --skip-autoconfigure --bin-dir "$home/bin" "$@" \
    > "$tmp_dir/$name.out" 2>&1 || { cat "$tmp_dir/$name.out" >&2; fail "install.sh ($name) exited non-zero"; }
  touch "$tmp_dir/$name.security"
}

run_install managed-metadata --config-file "$tmp_dir/metadata.yaml" --skip-setup
expect_line "$tmp_dir/managed-metadata.out" 'Skipping Relay CA trust: this managed install keeps payload capture off'
expect_line "$tmp_dir/managed-metadata.out" 'Relay CA:    .* (not trusted)'
refute_line "$tmp_dir/managed-metadata.security" 'add-trusted-cert'

run_install managed-payloads --config-file "$tmp_dir/payloads.yaml" --skip-setup
expect_line "$tmp_dir/managed-payloads.out" 'Certificate Trust Settings sheet'
expect_line "$tmp_dir/managed-payloads.out" 'Relay CA:    .* (trusted in the login keychain)'
expect_line "$tmp_dir/managed-payloads.security" "add-trusted-cert -r trustRoot -k $tmp_dir/managed-payloads/Library/Keychains/login.keychain-db $tmp_dir/managed-payloads/.litellm-relay/mitm/litellm-relay-ca.pem"

RELAY_TRUST_CA=1 run_install managed-opt-in --config-file "$tmp_dir/metadata.yaml" --skip-setup
expect_line "$tmp_dir/managed-opt-in.security" 'add-trusted-cert'

run_install managed-skip --config-file "$tmp_dir/payloads.yaml" --skip-setup --skip-trust-ca
expect_line "$tmp_dir/managed-skip.out" 'RELAY_TRUST_CA=0 or --skip-trust-ca'
refute_line "$tmp_dir/managed-skip.security" 'add-trusted-cert'

run_install interactive
expect_line "$tmp_dir/interactive.out" 'Certificate Trust Settings sheet'
expect_line "$tmp_dir/interactive.security" 'add-trusted-cert'

echo "install CA trust smoke passed"
