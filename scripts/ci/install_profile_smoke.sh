#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"

cleanup() {
  chflags nouchg "$tmp_dir/locked/.zshrc" 2> /dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

fail() {
  echo "install profile smoke failed: $*" >&2
  exit 1
}

expect_line() {
  grep -q -- "$2" "$1" || { cat "$1" >&2; fail "$1 lacks: $2"; }
}

cat > "$tmp_dir/stub-relay" <<'STUB'
#!/bin/bash
case "$1" in
  ca-path)
    printf '%s/.litellm-relay/mitm/litellm-relay-ca.pem\n' "$HOME"
    ;;
  capture-mode)
    echo metadata
    ;;
esac
STUB
chmod 755 "$tmp_dir/stub-relay"
printf 'capture:\n  payloads: false\n' > "$tmp_dir/metadata.yaml"

run_install() {
  local name="$1"
  local home="$tmp_dir/$name"
  SHELL=/bin/zsh HOME="$home" RELAY_HOME="$home/.litellm-relay" \
    bash "$ROOT/src/install.sh" --prebuilt-binary "$tmp_dir/stub-relay" --skip-autoconfigure --skip-setup \
      --config-file "$tmp_dir/metadata.yaml" --bin-dir "$home/bin" > "$tmp_dir/$name.out" 2>&1 \
    || { cat "$tmp_dir/$name.out" >&2; fail "install.sh ($name) exited non-zero"; }
}

path_lines() {
  grep -c -F "export PATH=\"$1/bin:\$PATH\"" "$1/.zshrc" || true
}

mkdir -p "$tmp_dir/writable"
printf '# mine\n' > "$tmp_dir/writable/.zshrc"
run_install writable
[[ "$(path_lines "$tmp_dir/writable")" == "1" ]] || fail "a writable .zshrc should get the PATH line once, got $(path_lines "$tmp_dir/writable")"
expect_line "$tmp_dir/writable.out" "I added $tmp_dir/writable/bin to PATH in:"
run_install writable
[[ "$(path_lines "$tmp_dir/writable")" == "1" ]] || fail "a second install should not repeat the PATH line, got $(path_lines "$tmp_dir/writable")"

mkdir -p "$tmp_dir/locked"
printf '# locked\n' > "$tmp_dir/locked/.zshrc"
chflags uchg "$tmp_dir/locked/.zshrc"
run_install locked
expect_line "$tmp_dir/locked.out" "warning: $tmp_dir/locked/.zshrc is not writable by $(id -un)"
expect_line "$tmp_dir/locked.out" "export PATH=\"$tmp_dir/locked/bin:\$PATH\""
expect_line "$tmp_dir/locked.out" "PATH was not updated: $tmp_dir/locked/.zshrc is not writable"
[[ "$(cat "$tmp_dir/locked/.zshrc")" == "# locked" ]] || fail "the locked .zshrc changed: $(cat "$tmp_dir/locked/.zshrc")"
[[ -x "$tmp_dir/locked/.litellm-relay/bin/litellm-relay" ]] || fail "the install did not go on past the unwritable profile"
[[ -L "$tmp_dir/locked/bin/relay" ]] || fail "the relay symlink was not created past the unwritable profile"

echo "install profile smoke passed"
