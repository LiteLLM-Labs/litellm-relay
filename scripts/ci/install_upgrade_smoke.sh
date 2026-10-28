#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
old_pid=""
# shellcheck source=scripts/ci/smoke_stubs.sh
source "$ROOT/scripts/ci/smoke_stubs.sh"

cleanup() {
  if [[ -n "$old_pid" ]]; then
    kill "$old_pid" 2> /dev/null || true
    wait "$old_pid" 2> /dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

fail() {
  echo "install upgrade smoke failed: $*" >&2
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

cat > "$tmp_dir/stub-relay.c" <<'STUB'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
  const char *command = argc > 1 ? argv[1] : "";
#ifdef WITHOUT_CAPTURE_MODE
  if (strcmp(command, "capture-mode") == 0) {
    fprintf(stderr, "error: unrecognized subcommand 'capture-mode'\n");
    return 2;
  }
#endif
  if (argc > 2 && strcmp(argv[2], "--help") == 0) {
    return 0;
  }
  if (strcmp(command, "ca-path") == 0) {
    printf("%s/.litellm-relay/mitm/litellm-relay-ca.pem\n", getenv("HOME"));
    return 0;
  }
  if (strcmp(command, "capture-mode") == 0) {
    puts("metadata");
    return 0;
  }
  if (strcmp(command, "pac") == 0) {
    puts("function FindProxyForURL(url, host) { return \"PROXY 127.0.0.1:4199\"; }");
    return 0;
  }
  if (strcmp(command, "launch-agent") == 0) {
    puts("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    puts("<plist version=\"1.0\"><dict><key>Label</key><string>ai.litellm.relay</string></dict></plist>");
    return 0;
  }
  if (strcmp(command, "autoconfigure") == 0) {
    FILE *log = fopen(getenv("RELAY_SMOKE_LOG"), "a");
    if (log == NULL) {
      return 1;
    }
    fprintf(log, "autoconfigure PATH=%s\n", getenv("PATH"));
    fclose(log);
    return 0;
  }
  if (strcmp(command, "serve") == 0) {
    for (;;) {
      pause();
    }
  }
  return 0;
}
STUB
cc -o "$tmp_dir/stub-relay" "$tmp_dir/stub-relay.c" || fail "could not compile the Mach-O stub"
cc -DWITHOUT_CAPTURE_MODE -o "$tmp_dir/old-relay" "$tmp_dir/stub-relay.c" || fail "could not compile the older stub"
printf 'capture:\n  payloads: false\n' > "$tmp_dir/metadata.yaml"
mkdir -p "$tmp_dir/RelayBarGlass.app/Contents/MacOS"
printf '#!/bin/bash\n' > "$tmp_dir/RelayBarGlass.app/Contents/MacOS/RelayBarGlass"
chmod 755 "$tmp_dir/RelayBarGlass.app/Contents/MacOS/RelayBarGlass"

home="$tmp_dir/home"
relay_home="$home/.litellm-relay"
installed="$relay_home/bin/litellm-relay"
desktop_binary="/Library/PrivilegedHelperTools/ai.litellm.relay.autoconfigure-desktop"
uid="$(id -u)"

run_install() {
  local name="$1" binary="$2"
  mkdir -p "$home"
  : > "$tmp_dir/calls.log"
  with_stubs HOME="$home" RELAY_HOME="$relay_home" RELAY_RELAYBAR_APP="$tmp_dir/RelayBarGlass.app" \
    bash "$ROOT/src/install.sh" --background --skip-setup --prebuilt-binary "$binary" \
      --config-file "$tmp_dir/metadata.yaml" --bin-dir "$home/bin" > "$tmp_dir/$name.out" 2>&1
}

expect_postinstall_shape() {
  local name="$1"
  cp "$tmp_dir/calls.log" "$tmp_dir/$name.calls"
  grep -q "LiteLLM Relay installed." "$tmp_dir/$name.out" || { cat "$tmp_dir/$name.out" >&2; fail "install.sh ($name) did not finish"; }
  expect_line "$tmp_dir/$name.calls" "^launchctl bootstrap gui/$uid $home/Library/LaunchAgents/ai.litellm.relay.plist$"
  expect_line "$tmp_dir/$name.calls" "^launchctl kickstart -k gui/$uid/ai.litellm.relay$"
  expect_line "$tmp_dir/$name.calls" "^launchctl bootstrap gui/$uid $home/Library/LaunchAgents/ai.litellm.relaybar.plist$"
  [[ "$(plutil -extract ProgramArguments.0 raw -o - "$home/Library/LaunchAgents/ai.litellm.relaybar.plist")" \
    == "$tmp_dir/RelayBarGlass.app/Contents/MacOS/RelayBarGlass" ]] || fail "the RelayBar agent ($name) does not start the app"
  refute_line "$tmp_dir/$name.calls" 'add-trusted-cert'
  expect_line "$tmp_dir/$name.calls" "^autoconfigure PATH=$home/.local/bin:"
  [[ "$(plutil -extract EnvironmentVariables.PATH raw -o - "$home/Library/LaunchAgents/ai.litellm.relay.autoconfigure.plist")" \
    == "$home/.local/bin:"* ]] || fail "the auto-configure agent ($name) does not put ~/.local/bin on PATH"
  plutil -extract WatchPaths xml1 -o - "$tmp_dir/system/ai.litellm.relay.autoconfigure-desktop.plist" \
    | grep -qF "<string>$relay_home/config.yaml</string>" || fail "the Claude Desktop daemon ($name) does not watch $relay_home/config.yaml"
  [[ "$(plutil -extract ProgramArguments.0 raw -o - "$tmp_dir/system/ai.litellm.relay.autoconfigure-desktop.plist")" \
    == "$desktop_binary" ]] || fail "the Claude Desktop daemon ($name) does not run the root-owned copy at $desktop_binary"
  for stream in StandardOutPath StandardErrorPath; do
    [[ "$(plutil -extract "$stream" raw -o - "$tmp_dir/system/ai.litellm.relay.autoconfigure-desktop.plist")" \
      == /Library/Logs/ai.litellm.relay.autoconfigure-desktop.* ]] || fail "the Claude Desktop daemon ($name) writes $stream outside /Library/Logs"
  done
  expect_line "$tmp_dir/$name.calls" "^sudo install -m 0755 -o root -g wheel $installed $desktop_binary$"
  cmp -s "$installed" "$tmp_dir/system/$(basename "$desktop_binary")" || fail "the root-owned copy ($name) is not the installed binary"
}

run_install fresh "$tmp_dir/stub-relay" || { cat "$tmp_dir/fresh.out" >&2; fail "install.sh (fresh) exited non-zero"; }
expect_postinstall_shape fresh
first_inode="$(stat -f %i "$installed")"
"$installed" serve &
old_pid=$!
sleep 1
kill -0 "$old_pid" || fail "the first installed binary did not stay running"

run_install upgrade "$tmp_dir/stub-relay" || { cat "$tmp_dir/upgrade.out" >&2; fail "install.sh (upgrade) exited non-zero"; }
expect_postinstall_shape upgrade
second_inode="$(stat -f %i "$installed")"
[[ "$first_inode" != "$second_inode" ]] || fail "the upgrade overwrote the running binary in place (same inode $first_inode)"
mode="$("$installed" capture-mode)" || fail "the upgraded binary does not run (exit $?)"
[[ "$mode" == "metadata" ]] || fail "the upgraded binary answered capture-mode with: $mode"
[[ "$(stat -f %Lp "$installed")" == "700" ]] || fail "the upgraded binary is not mode 700: $(stat -f %Lp "$installed")"
[[ -z "$(find "$relay_home/bin" -name '.litellm-relay.*')" ]] || fail "a staged copy was left behind in $relay_home/bin"

if run_install older "$tmp_dir/old-relay"; then
  fail "install.sh accepted a prebuilt binary with no capture-mode subcommand"
fi
expect_line "$tmp_dir/older.out" "has no 'capture-mode' subcommand"
[[ "$(stat -f %i "$installed")" == "$second_inode" ]] || fail "the rejected older binary replaced the installed one"
[[ "$("$installed" capture-mode)" == "metadata" ]] || fail "the installed binary stopped answering capture-mode after the rejected install"

relay_bin="${LITELLM_RELAY_BIN:-}"
if [[ -z "$relay_bin" ]]; then
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml" || fail "could not build litellm-relay for the re-push case"
  relay_bin="${CARGO_TARGET_DIR:-$ROOT/target}/debug/litellm-relay"
fi
prod_url="https://prod.relay-smoke.invalid"
uat_url="https://uat.relay-smoke.invalid"
cat > "$tmp_dir/environments.yaml" <<YAML
gateway:
  url: $prod_url
environments:
  - name: prod
    url: $prod_url
  - name: uat
    url: $uat_url
capture:
  payloads: false
YAML

gateway_value() {
  awk -v key="$2:" '/^gateway:/ { in_gateway = 1; next } /^[^ ]/ { in_gateway = 0 } in_gateway && $1 == key { print $2 }' "$1" \
    | tr -d "\"'"
}

pick_uat_and_team() {
  awk -v url="$uat_url" '
    /^gateway:/ { print; print "  url: " url; print "  team: picked-team"; in_gateway = 1; next }
    /^[^ ]/ { in_gateway = 0 }
    in_gateway && ($1 == "url:" || $1 == "team:") { next }
    { print }
  ' "$1" > "$1.next"
  mv "$1.next" "$1"
}

repush_install() {
  local name="$1"
  : > "$tmp_dir/calls.log"
  with_stubs HOME="$tmp_dir/repush" RELAY_HOME="$tmp_dir/repush/.litellm-relay" \
    bash "$ROOT/src/install.sh" --skip-setup --skip-autoconfigure --prebuilt-binary "$relay_bin" \
      --config-file "$tmp_dir/environments.yaml" --bin-dir "$tmp_dir/repush/bin" > "$tmp_dir/$name.out" 2>&1 \
    || { cat "$tmp_dir/$name.out" >&2; fail "install.sh ($name) exited non-zero"; }
}

mkdir -p "$tmp_dir/repush"
repush_install first-push
pick_uat_and_team "$tmp_dir/repush/.litellm-relay/config.yaml"
[[ "$(gateway_value "$tmp_dir/repush/.litellm-relay/config.yaml" url)" == "$uat_url" ]] || fail "the smoke could not pick uat"
repush_install re-push
config="$tmp_dir/repush/.litellm-relay/config.yaml"
[[ "$(gateway_value "$config" url)" == "$uat_url" ]] || fail "a re-push of the same managed config reset gateway.url to $(gateway_value "$config" url)"
[[ "$(gateway_value "$config" team)" == "picked-team" ]] || fail "a re-push of the same managed config dropped gateway.team"

echo "install upgrade smoke passed"
