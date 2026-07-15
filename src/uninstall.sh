#!/usr/bin/env bash
set -euo pipefail

RELAY_HOME="${RELAY_HOME:-$HOME/.litellm-relay}"
INSTALL_BIN_DIR_OVERRIDE=""
REMOVE_BIN=1
REMOVE_DATA=0
REMOVE_CA_TRUST=0
REMOVE_PAC_FILE=0
NETWORK_SERVICE=""

usage() {
  cat <<'USAGE'
Uninstall LiteLLM Relay from macOS.

Usage:
  ./src/uninstall.sh [--bin-dir DIR] [--keep-bin] [--remove-ca-trust]
                     [--remove-pac-file] [--remove-data]
                     [--unset-system-proxy "Wi-Fi"]

Options:
  --bin-dir DIR                  Also remove relay shims from DIR
  --keep-bin                     Keep Relay shims and ~/.litellm-relay/bin
  --remove-ca-trust              Remove Relay CA trust from the login keychain
  --remove-pac-file              Remove ~/.litellm-relay/relay.pac
  --remove-data                  Remove ~/.litellm-relay after other cleanup
  --unset-system-proxy SERVICE   Turn off PAC auto-proxy for a network service
  -h, --help                     Show this help

Default behavior stops/removes the LaunchAgent and removes installed command
shims plus the Relay binary. It intentionally preserves logs, config, CA files,
keychain trust, and system proxy settings unless explicit flags are passed.
USAGE
}

require_value() {
  if [[ $# -lt 2 || -z "$2" ]]; then
    echo "$1 requires a value" >&2
    exit 2
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin-dir)
      require_value "$1" "${2:-}"
      INSTALL_BIN_DIR_OVERRIDE="$2"
      shift 2
      ;;
    --keep-bin)
      REMOVE_BIN=0
      shift
      ;;
    --remove-ca-trust)
      REMOVE_CA_TRUST=1
      shift
      ;;
    --remove-pac-file)
      REMOVE_PAC_FILE=1
      shift
      ;;
    --remove-data)
      REMOVE_DATA=1
      REMOVE_PAC_FILE=1
      shift
      ;;
    --unset-system-proxy)
      require_value "$1" "${2:-}"
      NETWORK_SERVICE="$2"
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
  echo "uninstall.sh v0 currently supports macOS only." >&2
  exit 1
fi

PLIST="$HOME/Library/LaunchAgents/ai.litellm.relay.plist"
AUTOCONFIGURE_PLIST="$HOME/Library/LaunchAgents/ai.litellm.relay.autoconfigure.plist"
DESKTOP_DAEMON_LABEL="ai.litellm.relay.autoconfigure-desktop"
DESKTOP_DAEMON_PLIST="/Library/LaunchDaemons/$DESKTOP_DAEMON_LABEL.plist"
RELAY_BINARY="$RELAY_HOME/bin/litellm-relay"

if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi
RELAY_RUNNER="$RELAY_HOME/bin/run-relay"
CA_PATH="$RELAY_HOME/mitm/litellm-relay-ca.pem"
PAC_PATH="$RELAY_HOME/relay.pac"

remove_owned_shim() {
  local shim_path="$1"
  local target

  if [[ ! -e "$shim_path" && ! -L "$shim_path" ]]; then
    return 0
  fi

  if [[ -L "$shim_path" ]]; then
    target="$(readlink "$shim_path")"
    case "$target" in
      "$RELAY_BINARY"|"$RELAY_RUNNER"|"$RELAY_HOME"/bin/*)
        rm -f "$shim_path"
        echo "Removed shim: $shim_path"
        ;;
      *)
        echo "Skipping non-Relay shim: $shim_path -> $target" >&2
        ;;
    esac
    return 0
  fi

  echo "Skipping non-symlink path: $shim_path" >&2
}

remove_relay_bins() {
  local bin_dir
  local -a candidate_dirs=()

  if [[ -n "$INSTALL_BIN_DIR_OVERRIDE" ]]; then
    candidate_dirs+=("$INSTALL_BIN_DIR_OVERRIDE")
  fi
  candidate_dirs+=("/usr/local/bin" "/opt/homebrew/bin" "$HOME/.local/bin")

  for bin_dir in "${candidate_dirs[@]}"; do
    remove_owned_shim "$bin_dir/relay"
    remove_owned_shim "$bin_dir/litellm-relay"
  done

  rm -f "$RELAY_RUNNER" "$RELAY_BINARY"
  rmdir "$RELAY_HOME/bin" >/dev/null 2>&1 || true
}

remove_relay_data() {
  case "$RELAY_HOME" in
    ""|"/"|"$HOME"|"$HOME/")
      echo "refusing to remove unsafe RELAY_HOME: $RELAY_HOME" >&2
      exit 1
      ;;
  esac
  rm -rf "$RELAY_HOME"
}

remove_ca_trust() {
  if [[ ! -f "$CA_PATH" ]]; then
    echo "Relay CA file not found at $CA_PATH; keychain trust was not changed."
    return 0
  fi

  if security remove-trusted-cert -k "$HOME/Library/Keychains/login.keychain-db" "$CA_PATH" >/dev/null 2>&1; then
    echo "Removed Relay CA trust from the login keychain."
  else
    cat >&2 <<WARN
warning: could not remove Relay CA trust automatically.
Check Keychain Access for:
  LiteLLM Relay Local Root CA
WARN
  fi
}

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
rm -f "$PLIST"
launchctl bootout "gui/$(id -u)" "$AUTOCONFIGURE_PLIST" >/dev/null 2>&1 || true
rm -f "$AUTOCONFIGURE_PLIST"
$SUDO launchctl bootout system "$DESKTOP_DAEMON_PLIST" >/dev/null 2>&1 || true
$SUDO rm -f "$DESKTOP_DAEMON_PLIST" >/dev/null 2>&1 || true
# Managed preferences plist Relay wrote for Claude Desktop (macOS). Removing it
# reverts the app to its default (non-gateway) inference on next launch.
CLAUDE_DESKTOP_MANAGED_PLIST="/Library/Managed Preferences/com.anthropic.claudefordesktop.plist"
$SUDO rm -f "$CLAUDE_DESKTOP_MANAGED_PLIST" >/dev/null 2>&1 || true
$SUDO killall -HUP cfprefsd >/dev/null 2>&1 || true

if [[ -n "$NETWORK_SERVICE" ]]; then
  networksetup -setautoproxystate "$NETWORK_SERVICE" off
  echo "Disabled PAC auto-proxy for network service: $NETWORK_SERVICE"
fi

if [[ "$REMOVE_CA_TRUST" == "1" ]]; then
  remove_ca_trust
fi

if [[ "$REMOVE_PAC_FILE" == "1" ]]; then
  rm -f "$PAC_PATH"
fi

if [[ "$REMOVE_BIN" == "1" ]]; then
  remove_relay_bins
fi

if [[ "$REMOVE_DATA" == "1" ]]; then
  remove_relay_data
fi

cat <<DONE
LiteLLM Relay uninstall complete.

Removed:
  LaunchAgent: $PLIST
  LaunchAgent: $AUTOCONFIGURE_PLIST
  LaunchDaemon: $DESKTOP_DAEMON_PLIST
DONE

if [[ "$REMOVE_BIN" == "1" ]]; then
  cat <<DONE
  Relay shims and binary
DONE
else
  cat <<DONE
  Relay shims and binary were preserved because --keep-bin was set
DONE
fi

if [[ "$REMOVE_DATA" == "1" ]]; then
  cat <<DONE
  Relay data: $RELAY_HOME
DONE
else
  cat <<DONE

Preserved:
  Relay data: $RELAY_HOME

Optional cleanup:
  Remove CA trust:       ./src/uninstall.sh --remove-ca-trust
  Remove Relay data:     ./src/uninstall.sh --remove-data
  Disable system PAC:    ./src/uninstall.sh --unset-system-proxy "Wi-Fi"
DONE
fi
