with_stubs() {
  mkdir -p "$tmp_dir/system"
  touch "$tmp_dir/calls.log" "$tmp_dir/trusted"
  env PATH="$ROOT/scripts/ci/stubs:$PATH" \
    RELAY_SMOKE_LOG="$tmp_dir/calls.log" RELAY_SMOKE_TRUSTED="$tmp_dir/trusted" \
    RELAY_SMOKE_SANDBOX="$tmp_dir" RELAY_SMOKE_SYSTEM="$tmp_dir/system" "$@"
}
