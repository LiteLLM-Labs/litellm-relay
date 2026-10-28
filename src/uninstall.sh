#!/usr/bin/env bash
set -euo pipefail

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
  --keep-bin                     Keep Relay shims, ~/.litellm-relay/bin and, in a
                                 root run, the package payload at
                                 /usr/local/litellm-relay
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
shims and the Relay binary. It intentionally preserves logs, config, CA files,
keychain trust, and system proxy settings unless explicit flags are passed.

Run as root (sudo, or an MDM script), it acts on the user logged in at the
console, or on the user whose home RELAY_USER_HOME names, and refuses when it
has neither. Only a root run removes the package payload a .pkg shared with
every account on the Mac, and forgets its receipt.
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

console_user() {
  local user
  user="$(stat -f%Su /dev/console 2>/dev/null || true)"
  case "$user" in
    ""|root|loginwindow|_mbsetupuser) return 1 ;;
  esac
  printf '%s\n' "$user"
}

refuse_without_user() {
  cat >&2 <<REFUSE
uninstall.sh is running as root and nobody is logged in at the console, so it
cannot tell whose Relay to remove. Nothing was changed.
Name that user's home directory with RELAY_USER_HOME and run it again with the
same options, for example:
  sudo RELAY_USER_HOME=/Users/<name> $0
REFUSE
  exit 1
}

CALLER_UID="$(id -u)"
RELAY_USER=""
RELAY_USER_UID="$CALLER_UID"
if [[ "$CALLER_UID" -ne 0 ]]; then
  RELAY_USER_HOME="$HOME"
elif [[ -n "${RELAY_USER_HOME:-}" ]]; then
  if [[ ! -d "$RELAY_USER_HOME" ]]; then
    echo "RELAY_USER_HOME is not a directory: $RELAY_USER_HOME" >&2
    exit 1
  fi
  RELAY_USER="$(stat -f%Su "$RELAY_USER_HOME")"
  RELAY_USER_UID="$(stat -f%u "$RELAY_USER_HOME")"
elif RELAY_USER="$(console_user)"; then
  RELAY_USER_HOME="$(dscl . -read "/Users/$RELAY_USER" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p')"
  RELAY_USER_HOME="${RELAY_USER_HOME:-/Users/$RELAY_USER}"
  RELAY_USER_UID="$(id -u "$RELAY_USER")"
else
  refuse_without_user
fi
RELAY_HOME="${RELAY_HOME:-$RELAY_USER_HOME/.litellm-relay}"

RELAY_LABEL="ai.litellm.relay"
AUTOCONFIGURE_LABEL="ai.litellm.relay.autoconfigure"
RELAYBAR_LABEL="ai.litellm.relaybar"
PLIST="$RELAY_USER_HOME/Library/LaunchAgents/$RELAY_LABEL.plist"
AUTOCONFIGURE_PLIST="$RELAY_USER_HOME/Library/LaunchAgents/$AUTOCONFIGURE_LABEL.plist"
RELAYBAR_PLIST="$RELAY_USER_HOME/Library/LaunchAgents/$RELAYBAR_LABEL.plist"
PKG_ID="ai.litellm.relay"
PKG_PAYLOAD="${RELAY_PKG_PAYLOAD:-/usr/local/litellm-relay}"
PKG_PAYLOAD_REMOVED=0
DESKTOP_DAEMON_LABEL="ai.litellm.relay.autoconfigure-desktop"
DESKTOP_DAEMON_PLIST="/Library/LaunchDaemons/$DESKTOP_DAEMON_LABEL.plist"
DESKTOP_DAEMON_BINARY="/Library/PrivilegedHelperTools/$DESKTOP_DAEMON_LABEL"
DESKTOP_DAEMON_LOG="/Library/Logs/$DESKTOP_DAEMON_LABEL"
CLAUDE_DESKTOP_MANAGED_PLIST="/Library/Managed Preferences/com.anthropic.claudefordesktop.plist"
CLAUDE_DESKTOP_STALE_JSON="/etc/claude-desktop/managed-settings.json"
RELAY_BINARY="$RELAY_HOME/bin/litellm-relay"

if [[ "$CALLER_UID" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi
RELAY_RUNNER="$RELAY_HOME/bin/run-relay"
CA_PATH="$RELAY_HOME/mitm/litellm-relay-ca.pem"
CA_LABEL="LiteLLM Relay Local Root CA"

LOGIN_KEYCHAIN="$RELAY_USER_HOME/Library/Keychains/login.keychain-db"
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
  candidate_dirs+=("/usr/local/bin" "/opt/homebrew/bin" "$RELAY_USER_HOME/.local/bin")

  for bin_dir in "${candidate_dirs[@]}"; do
    remove_owned_shim "$bin_dir/relay"
    remove_owned_shim "$bin_dir/litellm-relay"
  done

  rm -f "$RELAY_RUNNER" "$RELAY_BINARY"
  rmdir "$RELAY_HOME/bin" >/dev/null 2>&1 || true
}

as_relay_user() {
  if [[ "$CALLER_UID" -eq 0 && "$RELAY_USER_UID" -ne 0 ]]; then
    launchctl asuser "$RELAY_USER_UID" sudo -H -u "$RELAY_USER" "$@"
  else
    "$@"
  fi
}

remove_pkg_payload() {
  [[ -d "$PKG_PAYLOAD" ]] || return 0
  if [[ "$CALLER_UID" -ne 0 ]]; then
    echo "The shared package payload at $PKG_PAYLOAD stays for the other accounts on this Mac; an admin or MDM removes it with: sudo $PKG_PAYLOAD/uninstall.sh"
    return 0
  fi
  if ! rm -rf "$PKG_PAYLOAD"; then
    echo "warning: could not remove the package payload at $PKG_PAYLOAD, so its receipt $PKG_ID was kept; the rest of the uninstall carried on." >&2
    return 0
  fi
  pkgutil --forget "$PKG_ID" >/dev/null 2>&1 || true
  PKG_PAYLOAD_REMOVED=1
}

remove_relay_data() {
  case "$RELAY_HOME" in
    ""|"/"|"$HOME"|"$HOME/"|"$RELAY_USER_HOME"|"$RELAY_USER_HOME/")
      echo "refusing to remove unsafe RELAY_HOME: $RELAY_HOME" >&2
      exit 1
      ;;
  esac
  rm -rf "$RELAY_HOME"
}

relay_ca_entries() {
  local work pem state
  work="$(mktemp -d)"
  as_relay_user security find-certificate -a -c "$CA_LABEL" -Z -p "$LOGIN_KEYCHAIN" 2>/dev/null \
    | awk -v dir="$work" '/^SHA-1 hash: / { file = dir "/" $3 ".pem"; next } /^SHA-256 hash: / { next } file { print > file }'
  for pem in "$work"/*.pem; do
    [[ -f "$pem" ]] || continue
    if as_relay_user security verify-cert -c "$pem" >/dev/null 2>&1; then
      state="trusted"
    else
      state="untrusted"
    fi
    printf '%s %s\n' "$(basename "$pem" .pem)" "$state"
  done
  rm -rf "$work"
}

print_delete_commands() {
  local sha1 state
  while read -r sha1 state; do
    [[ -n "$sha1" ]] || continue
    echo "  security delete-certificate -Z $sha1 -t \"$LOGIN_KEYCHAIN\""
  done <<< "$1"
}

relay_ca_trust_settings() {
  as_relay_user security dump-trust-settings 2>/dev/null | grep -c "$CA_LABEL" || true
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
      echo "This shell is not in the user's GUI session (launchctl managername: $session), the only place macOS reads and changes Certificate Trust Settings, so whether these certificates are trusted cannot be read here, and deleting them here would leave their trust settings behind (SHA-1):"
      sed 's/ [a-z]*$//; s/^/  /' <<< "$entries"
      echo "From a Terminal in that user's session run:"
      print_delete_commands "$entries"
    } >&2
    return 0
  fi

  while read -r fingerprint state; do
    [[ -n "$fingerprint" ]] || continue
    if [[ "$state" == "trusted" ]]; then
      echo "Removing $CA_LABEL ($fingerprint) and its trust setting; macOS asks for your account password in the Certificate Trust Settings sheet."
      as_relay_user security delete-certificate -Z "$fingerprint" -t "$LOGIN_KEYCHAIN" >/dev/null 2>&1 || true
    else
      as_relay_user security delete-certificate -Z "$fingerprint" "$LOGIN_KEYCHAIN" >/dev/null 2>&1 || true
    fi
  done <<< "$entries"

  trust_left="$(relay_ca_trust_settings)"
  if [[ "$trust_left" != "0" && -f "$CA_PATH" ]]; then
    as_relay_user security remove-trusted-cert "$CA_PATH" >/dev/null 2>&1 || true
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
      echo "Still in $LOGIN_KEYCHAIN (SHA-1, trust state):"
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
WHY
    print_delete_commands "$left"
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

launchctl bootout "gui/$RELAY_USER_UID/$RELAY_LABEL" >/dev/null 2>&1 || true
rm -f "$PLIST"
launchctl bootout "gui/$RELAY_USER_UID/$AUTOCONFIGURE_LABEL" >/dev/null 2>&1 || true
rm -f "$AUTOCONFIGURE_PLIST"
launchctl bootout "gui/$RELAY_USER_UID/$RELAYBAR_LABEL" >/dev/null 2>&1 || true
rm -f "$RELAYBAR_PLIST"
$SUDO launchctl bootout "system/$DESKTOP_DAEMON_LABEL" >/dev/null 2>&1 || true
$SUDO rm -f "$DESKTOP_DAEMON_PLIST" "$DESKTOP_DAEMON_BINARY" "$DESKTOP_DAEMON_LOG.out.log" "$DESKTOP_DAEMON_LOG.err.log" >/dev/null 2>&1 || true
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
  remove_pkg_payload
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
  LaunchDaemon: $DESKTOP_DAEMON_PLIST and $DESKTOP_DAEMON_BINARY
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
  if [[ "$PKG_PAYLOAD_REMOVED" == "1" ]]; then
    cat <<DONE
  Package payload: $PKG_PAYLOAD (receipt $PKG_ID forgotten)
DONE
  fi
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
  Remove the Relay CA:   $0 --remove-ca-trust  (from the GUI session)
  Remove Relay data:     $0 --remove-data
  Disable system PAC:    $0 --unset-system-proxy "Wi-Fi"
DONE
fi
