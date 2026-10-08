#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
old_pid=""

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

cat > "$tmp_dir/stub-relay.c" <<'STUB'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
  const char *command = argc > 1 ? argv[1] : "";
  if (strcmp(command, "ca-path") == 0) {
    printf("%s/.litellm-relay/mitm/litellm-relay-ca.pem\n", getenv("HOME"));
    return 0;
  }
  if (strcmp(command, "capture-mode") == 0) {
    puts("metadata");
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
printf 'capture:\n  payloads: false\n' > "$tmp_dir/metadata.yaml"

home="$tmp_dir/home"
installed="$home/.litellm-relay/bin/litellm-relay"

run_install() {
  local name="$1"
  mkdir -p "$home"
  HOME="$home" RELAY_HOME="$home/.litellm-relay" \
    bash "$ROOT/src/install.sh" --prebuilt-binary "$tmp_dir/stub-relay" --skip-autoconfigure --skip-setup \
      --config-file "$tmp_dir/metadata.yaml" --bin-dir "$home/bin" > "$tmp_dir/$name.out" 2>&1 \
    || { cat "$tmp_dir/$name.out" >&2; fail "install.sh ($name) exited non-zero"; }
  grep -q "LiteLLM Relay installed." "$tmp_dir/$name.out" || { cat "$tmp_dir/$name.out" >&2; fail "install.sh ($name) did not finish"; }
}

run_install fresh
first_inode="$(stat -f %i "$installed")"
"$installed" serve &
old_pid=$!
sleep 1
kill -0 "$old_pid" || fail "the first installed binary did not stay running"

run_install upgrade
second_inode="$(stat -f %i "$installed")"
[[ "$first_inode" != "$second_inode" ]] || fail "the upgrade overwrote the running binary in place (same inode $first_inode)"
mode="$("$installed" capture-mode)" || fail "the upgraded binary does not run (exit $?)"
[[ "$mode" == "metadata" ]] || fail "the upgraded binary answered capture-mode with: $mode"
[[ "$(stat -f %Lp "$installed")" == "700" ]] || fail "the upgraded binary is not mode 700: $(stat -f %Lp "$installed")"
[[ -z "$(find "$home/.litellm-relay/bin" -name '.litellm-relay.*')" ]] || fail "a staged copy was left behind in $home/.litellm-relay/bin"

echo "install upgrade smoke passed"
