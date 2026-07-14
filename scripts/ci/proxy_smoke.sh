#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${LITELLM_RELAY_BIN:-$ROOT/target/debug/litellm-relay}"

if [[ ! -x "$BIN" ]]; then
  cargo build --manifest-path "$ROOT/Cargo.toml"
fi

tmp_dir="$(mktemp -d)"
relay_pid=""
tls_pid=""

cleanup() {
  if [[ -n "$relay_pid" ]]; then
    kill "$relay_pid" >/dev/null 2>&1 || true
    wait "$relay_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n "$tls_pid" ]]; then
    kill "$tls_pid" >/dev/null 2>&1 || true
    wait "$tls_pid" >/dev/null 2>&1 || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

pick_port() {
  python3 - <<'PY'
import socket
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
    s.bind(("127.0.0.1", 0))
    print(s.getsockname()[1])
PY
}

relay_port="$(pick_port)"
upstream_port="$(pick_port)"
home_dir="$tmp_dir/home"
relay_home="$home_dir/.litellm-relay"
mkdir -p "$relay_home"

cat > "$relay_home/config.yaml" <<YAML
relay:
  host: 127.0.0.1
  port: $relay_port
  log_path: $tmp_dir/relay.log.jsonl
  mitm_ca_dir: $tmp_dir/mitm
gateway:
  url: http://127.0.0.1:4000
shadow:
  enabled: false
capture:
  payloads: false
  payload_preview_bytes: 0
  payload_body_bytes: 0
domains:
  notion: []
  ai:
    - api.openai.com
timeouts:
  request_seconds: 2.0
YAML

HOME="$home_dir" "$BIN" serve >"$tmp_dir/relay.out" 2>"$tmp_dir/relay.err" &
relay_pid="$!"

for _ in {1..60}; do
  if curl -fs "http://127.0.0.1:$relay_port/api/status" >/dev/null; then
    break
  fi
  sleep 0.25
done
curl -fsS "http://127.0.0.1:$relay_port/api/status" | grep -q '"runtime":"rust"'
curl -fsS "http://127.0.0.1:$relay_port/proxy.pac" | grep -q "PROXY 127.0.0.1:$relay_port"

openssl req \
  -x509 \
  -newkey rsa:2048 \
  -nodes \
  -sha256 \
  -days 1 \
  -keyout "$tmp_dir/upstream.key" \
  -out "$tmp_dir/upstream.crt" \
  -subj "/CN=localhost" \
  -addext "subjectAltName=IP:127.0.0.1,DNS:localhost" >/dev/null 2>&1

openssl s_server \
  -quiet \
  -accept "$upstream_port" \
  -cert "$tmp_dir/upstream.crt" \
  -key "$tmp_dir/upstream.key" \
  -www >"$tmp_dir/tls.out" 2>"$tmp_dir/tls.err" &
tls_pid="$!"

for _ in {1..60}; do
  if python3 - "$upstream_port" <<'PY' >/dev/null 2>&1
import socket
import sys
with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=0.5):
    pass
PY
  then
    break
  fi
  sleep 0.25
done

curl \
  -fsSk \
  --max-time 10 \
  --proxy "http://127.0.0.1:$relay_port" \
  "https://127.0.0.1:$upstream_port/" | grep -qi "s_server"

curl -fsS "http://127.0.0.1:$relay_port/api/events?limit=50" | grep -q '"event":"connect"'
curl -fsS "http://127.0.0.1:$relay_port/api/events?limit=50" | grep -q '"event":"connect_closed"'

echo "proxy smoke ok: relay=$relay_port upstream=$upstream_port"
