#!/usr/bin/env bash
set -euo pipefail

RELAY_HOME="${RELAY_HOME:-$HOME/.litellm-relay}"
RELAY_PORT="${LITELLM_RELAY_PORT:-4142}"
NETWORK_SERVICE=""
BACKGROUND_SERVICE=0

usage() {
  cat <<'USAGE'
Install LiteLLM Relay on macOS.

Usage:
  ./src/install.sh [--background] [--set-system-proxy "Wi-Fi"]

Environment:
  LITELLM_RELAY_BIN_DIR           Optional install location for the relay command
  LITELLM_GATEWAY_URL              Optional default LiteLLM Gateway URL
  LITELLM_GATEWAY_API_KEY          Optional non-interactive Gateway key fallback
  LITELLM_RELAY_SHADOW_ENABLED     Set to 1 to shadow Notion connection events
  LITELLM_RELAY_SHADOW_MODEL       Model for synthetic shadow calls, default gpt-4o-mini
  LITELLM_RELAY_CAPTURE_PAYLOADS   Capture request/response previews, default 1

By default this builds the Rust Relay binary, installs the relay command, and
trusts the Relay local CA in your login keychain so AI app payloads can be
captured. Then run:

  relay

The relay command opens the interactive setup wizard when needed and then starts
the foreground terminal trace view.

Pass --background to also configure Gateway SSO and start the Relay LaunchAgent.
Pass --set-system-proxy "Wi-Fi" to route AI apps through the background service.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --background)
      BACKGROUND_SERVICE=1
      shift
      ;;
    --set-system-proxy)
      NETWORK_SERVICE="${2:-}"
      BACKGROUND_SERVICE=1
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

choose_bin_dir() {
  if [[ -n "${LITELLM_RELAY_BIN_DIR:-}" ]]; then
    printf '%s\n' "$LITELLM_RELAY_BIN_DIR"
  elif [[ -d /usr/local/bin && -w /usr/local/bin ]]; then
    printf '%s\n' "/usr/local/bin"
  elif [[ -d /opt/homebrew/bin && -w /opt/homebrew/bin ]]; then
    printf '%s\n' "/opt/homebrew/bin"
  else
    printf '%s\n' "$HOME/.local/bin"
  fi
}

install_path_entry() {
  local bin_dir="$1"
  case ":$PATH:" in
    *":$bin_dir:"*)
      return 0
      ;;
  esac

  local shell_name profile_path
  shell_name="$(basename "${SHELL:-zsh}")"
  case "$shell_name" in
    zsh)
      profile_path="$HOME/.zshrc"
      ;;
    bash)
      profile_path="$HOME/.bashrc"
      ;;
    *)
      profile_path="$HOME/.profile"
      ;;
  esac

  mkdir -p "$(dirname "$profile_path")"
  touch "$profile_path"
  if ! grep -Fqs "$bin_dir" "$profile_path"; then
    {
      printf '\n# LiteLLM Relay\n'
      printf 'export PATH="%s:$PATH"\n' "$bin_dir"
    } >> "$profile_path"
    PATH_UPDATED_PROFILE="$profile_path"
  fi
  export PATH="$bin_dir:$PATH"
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DIR=""
if [[ -f "$SCRIPT_DIR/../Cargo.toml" ]]; then
  BUILD_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
elif [[ -f "$SCRIPT_DIR/Cargo.toml" ]]; then
  BUILD_DIR="$SCRIPT_DIR"
else
  TMP_DIR="$(mktemp -d)"
  trap 'rm -rf "$TMP_DIR"' EXIT
  curl -fsSL "https://github.com/BerriAI/litellm-relay/archive/refs/heads/main.tar.gz" \
    | tar -xz -C "$TMP_DIR"
  BUILD_DIR="$TMP_DIR/litellm-relay-main"
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required to install LiteLLM Relay from source." >&2
  echo "Install Rust from https://rustup.rs/ and rerun install.sh." >&2
  exit 1
fi

cargo build --release --manifest-path "$BUILD_DIR/Cargo.toml"
mkdir -p "$RELAY_HOME/bin"
cp "$BUILD_DIR/target/release/litellm-relay" "$RELAY_HOME/bin/litellm-relay"
chmod 700 "$RELAY_HOME/bin/litellm-relay"

INSTALL_BIN_DIR="$(choose_bin_dir)"
PATH_UPDATED_PROFILE=""
mkdir -p "$INSTALL_BIN_DIR"
ln -sf "$RELAY_HOME/bin/litellm-relay" "$INSTALL_BIN_DIR/relay"
ln -sf "$RELAY_HOME/bin/litellm-relay" "$INSTALL_BIN_DIR/litellm-relay"
install_path_entry "$INSTALL_BIN_DIR"

CA_PATH="$("$RELAY_HOME/bin/litellm-relay" ca-path)"
security add-trusted-cert -r trustRoot -k "$HOME/Library/Keychains/login.keychain-db" "$CA_PATH" >/dev/null 2>&1 || {
  cat >&2 <<WARN
warning: could not add the Relay CA to the login keychain.
Payload capture requires trusting this certificate:
  $CA_PATH
WARN
}

if [[ "$BACKGROUND_SERVICE" != "1" ]]; then
  cat <<DONE
LiteLLM Relay installed.

Command:     $INSTALL_BIN_DIR/relay
Relay CA:    $CA_PATH

Start the interactive setup and live trace view:
  relay
DONE
  if [[ -n "$PATH_UPDATED_PROFILE" ]]; then
    cat <<DONE

I added $INSTALL_BIN_DIR to PATH in:
  $PATH_UPDATED_PROFILE

Open a new terminal before running relay, or run:
  export PATH="$INSTALL_BIN_DIR:\$PATH"
  relay
DONE
  fi
  exit 0
fi

SETUP_ARGS=()
if [[ -n "${LITELLM_GATEWAY_URL:-}" ]]; then
  SETUP_ARGS+=(--gateway-url "$LITELLM_GATEWAY_URL")
fi
if [[ -n "${LITELLM_GATEWAY_API_KEY:-}" ]]; then
  SETUP_ARGS+=(--api-key "$LITELLM_GATEWAY_API_KEY")
fi

"$RELAY_HOME/bin/litellm-relay" setup "${SETUP_ARGS[@]}"

mkdir -p "$RELAY_HOME/bin"
cat > "$RELAY_HOME/bin/run-relay" <<RUNNER
#!/usr/bin/env zsh
set -euo pipefail
set -a
source "$RELAY_HOME/env"
set +a
exec "$RELAY_HOME/bin/litellm-relay" serve
RUNNER
chmod 700 "$RELAY_HOME/bin/run-relay"

set -a
source "$RELAY_HOME/env"
set +a

cat > "$RELAY_HOME/relay.pac" <<PAC
function FindProxyForURL(url, host) {
  var relayProxy = "PROXY 127.0.0.1:$RELAY_PORT";
  var notionDomains = ["notion.so", "notion.com", "api.notion.com", "www.notion.so", "app.notion.com", "api.openai.com", "openai.com", "chatgpt.com", "api.anthropic.com", "anthropic.com", "claude.ai"];
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

Command:     $INSTALL_BIN_DIR/relay
Relay proxy: 127.0.0.1:$RELAY_PORT
Dashboard:   http://127.0.0.1:$RELAY_PORT/
PAC URL:     http://127.0.0.1:$RELAY_PORT/proxy.pac
Relay CA:    $CA_PATH
Logs:        $RELAY_HOME/relay.log.jsonl

To open the interactive terminal view:
  relay

To route Notion through Relay for a manual pilot:
  networksetup -setautoproxyurl "Wi-Fi" http://127.0.0.1:$RELAY_PORT/proxy.pac
  networksetup -setautoproxystate "Wi-Fi" on

To verify interception without changing system settings:
  curl --cacert "$CA_PATH" -x http://127.0.0.1:$RELAY_PORT https://www.notion.so

Gateway auth is saved in $RELAY_HOME/env.
DONE
if [[ -n "$PATH_UPDATED_PROFILE" ]]; then
  cat <<DONE

I added $INSTALL_BIN_DIR to PATH in:
  $PATH_UPDATED_PROFILE

Open a new terminal before running relay, or run:
  export PATH="$INSTALL_BIN_DIR:\$PATH"
  relay
DONE
fi
