#!/usr/bin/env bash
set -euo pipefail

RELAY_HOME="${RELAY_HOME:-$HOME/.litellm-relay}"
RELAY_PORT="${LITELLM_RELAY_PORT:-4142}"
NETWORK_SERVICE=""

usage() {
  cat <<'USAGE'
Install LiteLLM Relay on macOS.

Usage:
  ./install.sh [--set-system-proxy "Wi-Fi"]

Environment:
  LITELLM_GATEWAY_URL              LiteLLM Gateway URL, default http://127.0.0.1:4000
  LITELLM_GATEWAY_API_KEY          Gateway virtual key for shadow calls
  LITELLM_RELAY_SHADOW_ENABLED     Set to 1 to shadow Notion connection events
  LITELLM_RELAY_SHADOW_MODEL       Model for synthetic shadow calls, default gpt-4o-mini

By default this installs and starts the local Relay LaunchAgent but does not
change system proxy settings. Pass --set-system-proxy "Wi-Fi" for a manual pilot.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --set-system-proxy)
      NETWORK_SERVICE="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "install.sh v0 currently supports macOS only." >&2
  exit 1
fi

mkdir -p "$RELAY_HOME"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -d "$SCRIPT_DIR/src/litellm_relay" ]]; then
  rsync -a --delete "$SCRIPT_DIR/src/" "$RELAY_HOME/src/"
else
  TMP_DIR="$(mktemp -d)"
  trap 'rm -rf "$TMP_DIR"' EXIT
  curl -fsSL "https://github.com/BerriAI/litellm-relay/archive/refs/heads/main.tar.gz" \
    | tar -xz -C "$TMP_DIR"
  rsync -a --delete "$TMP_DIR/litellm-relay-main/src/" "$RELAY_HOME/src/"
fi

cat > "$RELAY_HOME/env" <<ENV
LITELLM_RELAY_HOST=127.0.0.1
LITELLM_RELAY_PORT=$RELAY_PORT
LITELLM_RELAY_LOG_PATH=$RELAY_HOME/relay.log.jsonl
LITELLM_GATEWAY_URL=${LITELLM_GATEWAY_URL:-http://127.0.0.1:4000}
LITELLM_GATEWAY_API_KEY=${LITELLM_GATEWAY_API_KEY:-}
LITELLM_RELAY_SHADOW_ENABLED=${LITELLM_RELAY_SHADOW_ENABLED:-0}
LITELLM_RELAY_SHADOW_MODEL=${LITELLM_RELAY_SHADOW_MODEL:-gpt-4o-mini}
ENV
chmod 600 "$RELAY_HOME/env"

mkdir -p "$RELAY_HOME/bin"
cat > "$RELAY_HOME/bin/run-relay" <<RUNNER
#!/usr/bin/env zsh
set -euo pipefail
set -a
source "$RELAY_HOME/env"
set +a
export PYTHONPATH="$RELAY_HOME/src"
exec /usr/bin/python3 -m litellm_relay.cli serve
RUNNER
chmod 700 "$RELAY_HOME/bin/run-relay"

cat > "$RELAY_HOME/relay.pac" <<PAC
function FindProxyForURL(url, host) {
  var relayProxy = "PROXY 127.0.0.1:$RELAY_PORT";
  var notionDomains = ["notion.so", "notion.com", "api.notion.com", "www.notion.so", "app.notion.com"];
  host = host.toLowerCase();
  for (var i = 0; i < notionDomains.length; i++) {
    var domain = notionDomains[i];
    if (host === domain || dnsDomainIs(host, "." + domain)) {
      return relayProxy;
    }
  }
  return "DIRECT";
}
PAC

PLIST="$HOME/Library/LaunchAgents/ai.litellm.relay.plist"
mkdir -p "$(dirname "$PLIST")"
cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>ai.litellm.relay</string>
  <key>ProgramArguments</key>
  <array>
    <string>$RELAY_HOME/bin/run-relay</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>$RELAY_HOME/launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>$RELAY_HOME/launchd.err.log</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
launchctl enable "gui/$(id -u)/ai.litellm.relay"

if [[ -n "$NETWORK_SERVICE" ]]; then
  networksetup -setautoproxyurl "$NETWORK_SERVICE" "http://127.0.0.1:$RELAY_PORT/proxy.pac"
  networksetup -setautoproxystate "$NETWORK_SERVICE" on
fi

cat <<DONE
LiteLLM Relay installed.

Relay proxy: 127.0.0.1:$RELAY_PORT
PAC URL:     http://127.0.0.1:$RELAY_PORT/proxy.pac
Logs:        $RELAY_HOME/relay.log.jsonl

To route Notion through Relay for a manual pilot:
  networksetup -setautoproxyurl "Wi-Fi" http://127.0.0.1:$RELAY_PORT/proxy.pac
  networksetup -setautoproxystate "Wi-Fi" on

To enable shadow calls through LiteLLM Gateway, set LITELLM_GATEWAY_API_KEY and
LITELLM_RELAY_SHADOW_ENABLED=1 before running install.sh, then restart Relay.
DONE
