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
  --remove-ca-trust              Remove every Relay CA certificate, and its trust
                                 setting, from the login keychain (a trusted one
                                 needs the account password in the Certificate
                                 Trust Settings sheet, so run this from a Terminal
                                 in the user's GUI session)
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
RELAYBAR_PLIST="$HOME/Library/LaunchAgents/ai.litellm.relaybar.plist"
DESKTOP_DAEMON_LABEL="ai.litellm.relay.autoconfigure-desktop"
DESKTOP_DAEMON_PLIST="/Library/LaunchDaemons/$DESKTOP_DAEMON_LABEL.plist"
CLAUDE_DESKTOP_MANAGED_PLIST="/Library/Managed Preferences/com.anthropic.claudefordesktop.plist"
CLAUDE_DESKTOP_STALE_JSON="/etc/claude-desktop/managed-settings.json"
RELAY_BINARY="$RELAY_HOME/bin/litellm-relay"

if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi
RELAY_RUNNER="$RELAY_HOME/bin/run-relay"
CA_PATH="$RELAY_HOME/mitm/litellm-relay-ca.pem"
CA_LABEL="LiteLLM Relay Local Root CA"
LOGIN_KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
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

relay_ca_entries() {
  local work pem fingerprint state
  work="$(mktemp -d)"
  security find-certificate -a -c "$CA_LABEL" -p "$LOGIN_KEYCHAIN" 2>/dev/null \
    | awk -v dir="$work" '/-----BEGIN CERTIFICATE-----/ { n++ } { print > (dir "/" n ".pem") }'
  for pem in "$work"/*.pem; do
    [[ -f "$pem" ]] || continue
    fingerprint="$(openssl x509 -in "$pem" -noout -fingerprint -sha256 | sed 's/^.*=//; s/://g')"
    if security verify-cert -c "$pem" >/dev/null 2>&1; then
      state="trusted"
    else
      state="untrusted"
    fi
    printf '%s %s\n' "$fingerprint" "$state"
  done
  rm -rf "$work"
}

relay_ca_trust_settings() {
  security dump-trust-settings 2>/dev/null | grep -c "$CA_LABEL" || true
}

remove_ca_trust() {
  local entries fingerprint state left trust_left session
  entries="$(relay_ca_entries)"
  if [[ -z "$entries" && "$(relay_ca_trust_settings)" == "0" ]]; then
    echo "No \"$CA_LABEL\" certificate is in $LOGIN_KEYCHAIN."
    return 0
  fi

  session="$(launchctl managername 2>/dev/null || echo unknown)"
  if [[ "$session" != "Aqua" ]]; then
    {
      echo "warning: the Relay CA was left in $LOGIN_KEYCHAIN."
      echo "This shell is not in the user's GUI session (launchctl managername: $session), the only place macOS reads and changes Certificate Trust Settings, so whether these certificates are trusted cannot be read here, and deleting them here would leave their trust settings behind (SHA-256):"
      sed 's/ [a-z]*$//; s/^/  /' <<< "$entries"
      echo "From a Terminal in that user's session run this uninstaller again, or:"
      echo "  security delete-certificate -c \"$CA_LABEL\" -t \"$LOGIN_KEYCHAIN\""
    } >&2
    return 0
  fi

  while read -r fingerprint state; do
    [[ -n "$fingerprint" ]] || continue
    if [[ "$state" == "trusted" ]]; then
      echo "Removing $CA_LABEL ($fingerprint) and its trust setting; macOS asks for your account password in the Certificate Trust Settings sheet."
      security delete-certificate -Z "$fingerprint" -t "$LOGIN_KEYCHAIN" >/dev/null 2>&1 || true
    else
      security delete-certificate -Z "$fingerprint" "$LOGIN_KEYCHAIN" >/dev/null 2>&1 || true
    fi
  done <<< "$entries"

  trust_left="$(relay_ca_trust_settings)"
  if [[ "$trust_left" != "0" && -f "$CA_PATH" ]]; then
    security remove-trusted-cert "$CA_PATH" >/dev/null 2>&1 || true
    trust_left="$(relay_ca_trust_settings)"
  fi
  left="$(relay_ca_entries)"
  if [[ -z "$left" && "$trust_left" == "0" ]]; then
    echo "Removed every \"$CA_LABEL\" certificate from $LOGIN_KEYCHAIN:"
    sed 's/^/  /' <<< "$entries"
    return 0
  fi

  {
    echo "warning: the Relay CA is not fully removed from this account."
    if [[ -n "$left" ]]; then
      echo "Still in $LOGIN_KEYCHAIN (SHA-256, trust state):"
      sed 's/^/  /' <<< "$left"
    fi
    if [[ "$trust_left" != "0" ]]; then
      echo "User trust settings still name \"$CA_LABEL\" $trust_left time(s); Keychain Access does not list a trust setting whose certificate is gone."
    fi
    cat <<WHY
Removing a trusted root changes the Certificate Trust Settings, which macOS only
allows from the logged-in GUI session after the account password is typed into
the sheet it shows, so the change is denied over ssh, from an MDM script, or
when that sheet was cancelled. From a Terminal in that user's session run:
  security delete-certificate -c "$CA_LABEL" -t "$LOGIN_KEYCHAIN"
WHY
    if [[ "$trust_left" != "0" && -f "$CA_PATH" ]]; then
      echo "and, for a trust setting left without its certificate:"
      echo "  security remove-trusted-cert \"$CA_PATH\""
    fi
  } >&2
}

CLAUDE_DESKTOP_REMOVED=""
remove_claude_desktop_managed_settings() {
  local provider
  provider="$(/usr/bin/plutil -extract inferenceProvider raw -o - "$CLAUDE_DESKTOP_MANAGED_PLIST" 2>/dev/null || true)"
  if [[ "$provider" == "gateway" ]]; then
    $SUDO rm -f "$CLAUDE_DESKTOP_MANAGED_PLIST" >/dev/null 2>&1 || true
    CLAUDE_DESKTOP_REMOVED="$CLAUDE_DESKTOP_MANAGED_PLIST"
  fi
  if grep -q '"inferenceProvider": "gateway"' "$CLAUDE_DESKTOP_STALE_JSON" 2>/dev/null; then
    $SUDO rm -f "$CLAUDE_DESKTOP_STALE_JSON" >/dev/null 2>&1 || true
    $SUDO rmdir "$(dirname "$CLAUDE_DESKTOP_STALE_JSON")" >/dev/null 2>&1 || true
    CLAUDE_DESKTOP_REMOVED="${CLAUDE_DESKTOP_REMOVED:+$CLAUDE_DESKTOP_REMOVED, }$CLAUDE_DESKTOP_STALE_JSON"
  fi
}

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
rm -f "$PLIST"
launchctl bootout "gui/$(id -u)" "$AUTOCONFIGURE_PLIST" >/dev/null 2>&1 || true
rm -f "$AUTOCONFIGURE_PLIST"
launchctl bootout "gui/$(id -u)" "$RELAYBAR_PLIST" >/dev/null 2>&1 || true
rm -f "$RELAYBAR_PLIST"
$SUDO launchctl bootout system "$DESKTOP_DAEMON_PLIST" >/dev/null 2>&1 || true
$SUDO rm -f "$DESKTOP_DAEMON_PLIST" >/dev/null 2>&1 || true
remove_claude_desktop_managed_settings

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
  LaunchAgent: $RELAYBAR_PLIST
  LaunchDaemon: $DESKTOP_DAEMON_PLIST
DONE

if [[ -n "$CLAUDE_DESKTOP_REMOVED" ]]; then
  cat <<DONE
  Claude Desktop managed settings: $CLAUDE_DESKTOP_REMOVED
DONE
fi

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
  Remove the Relay CA:   ./src/uninstall.sh --remove-ca-trust  (from the GUI session)
  Remove Relay data:     ./src/uninstall.sh --remove-data
  Disable system PAC:    ./src/uninstall.sh --unset-system-proxy "Wi-Fi"
DONE
fi
