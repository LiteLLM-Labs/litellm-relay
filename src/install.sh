#!/usr/bin/env bash
set -euo pipefail

RELAY_HOME="${RELAY_HOME:-$HOME/.litellm-relay}"
RELAY_VERSION="${RELAY_VERSION:-}"
RELAY_SHA256="${RELAY_SHA256:-}"
RELAY_SOURCE_URL="${RELAY_SOURCE_URL:-}"
RELAY_ALLOW_UNPINNED_MAIN="${RELAY_ALLOW_UNPINNED_MAIN:-0}"
RELAY_PREBUILT_BINARY="${RELAY_PREBUILT_BINARY:-}"
RELAY_MANAGED_CONFIG="${RELAY_MANAGED_CONFIG:-}"
RELAY_SKIP_SETUP="${RELAY_SKIP_SETUP:-0}"
RELAY_AUTOCONFIGURE="${RELAY_AUTOCONFIGURE:-1}"
RELAY_AUTOCONFIGURE_INTERVAL="${RELAY_AUTOCONFIGURE_INTERVAL:-3600}"
RELAY_TRUST_CA="${RELAY_TRUST_CA:-auto}"
RELAY_RELAYBAR_APP="${RELAY_RELAYBAR_APP:-/usr/local/litellm-relay/RelayBarGlass.app}"
RELAYBAR_LABEL="ai.litellm.relaybar"
RELAYBAR_PLIST="$HOME/Library/LaunchAgents/$RELAYBAR_LABEL.plist"
RELAY_PORT="4142"
NETWORK_SERVICE=""
BACKGROUND_SERVICE=0
SETUP_GATEWAY_URL=""
SETUP_API_KEY=""
SKIP_SETUP=0
INSTALL_BIN_DIR_OVERRIDE=""

usage() {
  cat <<'USAGE'
Install LiteLLM Relay on macOS.

Usage:
  ./src/install.sh [--version VERSION] [--sha256 SHA256] [--background]
                   [--set-system-proxy "Wi-Fi"] [--gateway-url URL] [--api-key KEY]

Options:
  --version VERSION               Download and build the named GitHub release tag
  --sha256 SHA256                 Verify the downloaded source archive checksum
  --source-url URL                Download source from an explicit archive URL
  --allow-unpinned-main           Allow remote install from mutable main.tar.gz
  --prebuilt-binary PATH          Install this prebuilt relay binary instead of building
  --config-file PATH              Seed ~/.litellm-relay/config.yaml from this managed file
  --skip-setup                    Skip the interactive gateway setup wizard (managed deploys)
  --skip-autoconfigure            Do not auto-detect and wire installed AI tools to the Gateway
                                  (also disables periodic re-detection of later installs)
  --skip-trust-ca                 Install without adding the Relay CA to the login keychain
  --background                    Configure Gateway auth now, restart the Relay LaunchAgent,
                                  and re-detect AI tools on an interval
  --set-system-proxy "Wi-Fi"      Route the named macOS network service through Relay
  --gateway-url URL               Gateway URL for non-interactive setup
  --api-key KEY                   Gateway key for non-interactive setup
  --bin-dir DIR                   Install relay shims into DIR

When run from a checked-out repository, this builds the local source tree.
When run as a standalone remote script, pass RELAY_VERSION/--version or
RELAY_SOURCE_URL/--source-url. Mutable main.tar.gz installs require the explicit
RELAY_ALLOW_UNPINNED_MAIN=1 or --allow-unpinned-main opt-in.

By default this installs the relay command and trusts the Relay local CA in your
login keychain so AI app payloads can be captured; macOS asks for your account
password in a Certificate Trust Settings sheet for that step. A managed install
(--config-file or --skip-setup, which is what the .pkg postinstall runs) trusts
the CA only when the seeded config sets capture.payloads: true, since nothing
else needs it, and says so in the install log either way. Then run:

  relay

The relay command opens the interactive setup wizard when needed and then starts
the foreground terminal trace view.

Wiring an AI tool (the wizard, relay onboard, relay onboard-codex,
relay onboard-claude-desktop, relay autoconfigure) installs and starts the
ai.litellm.relay LaunchAgent when no Relay daemon is answering, because the
tools ask that daemon for their credential. With the agent running, relay
prints where the dashboard is instead of opening the trace view.

Pass --background to also configure Gateway SSO during the install, restart the
LaunchAgent on the new binary, and re-detect AI tools on an interval.
Pass --set-system-proxy "Wi-Fi" to route AI apps through the background service.

Relay settings are stored in:
  ~/.litellm-relay/config.yaml

Environment:
  RELAY_VERSION                 Same as --version
  RELAY_SHA256                  Same as --sha256
  RELAY_SOURCE_URL              Same as --source-url
  RELAY_ALLOW_UNPINNED_MAIN=1   Same as --allow-unpinned-main
  RELAY_PREBUILT_BINARY         Same as --prebuilt-binary
  RELAY_MANAGED_CONFIG          Same as --config-file
  RELAY_SKIP_SETUP=1            Same as --skip-setup
  RELAY_AUTOCONFIGURE=0         Same as --skip-autoconfigure
  RELAY_AUTOCONFIGURE_INTERVAL  Seconds between periodic re-detection (default 3600)
  RELAY_TRUST_CA=0              Same as --skip-trust-ca
  RELAY_TRUST_CA=1              Trust the Relay CA on a managed install whose
                                config keeps payload capture off
  RELAY_RELAYBAR_APP            RelayBar menu bar app bundle to register at login
                                (default /usr/local/litellm-relay/RelayBarGlass.app;
                                skipped when absent)
USAGE
}

autoconfigure_ai_tools() {
  if [[ "$RELAY_AUTOCONFIGURE" != "1" ]]; then
    return 0
  fi
  if [[ ! -f "$RELAY_HOME/config.yaml" ]]; then
    return 0
  fi
  echo "Auto-configuring installed AI tools to route through the Gateway..."
  "$RELAY_HOME/bin/litellm-relay" autoconfigure || {
    echo "warning: AI tool auto-configuration did not complete." >&2
  }
}

write_relaybar_plist() {
  local plist_path="$1" app_path="$2" relay_home="$3"
  cat > "$plist_path" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>ai.litellm.relaybar</string>
  <key>ProgramArguments</key>
  <array>
    <string>$app_path/Contents/MacOS/RelayBarGlass</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardOutPath</key>
  <string>$relay_home/relaybar.out.log</string>
  <key>StandardErrorPath</key>
  <string>$relay_home/relaybar.err.log</string>
</dict>
</plist>
PLIST
}

install_relaybar_agent() {
  if [[ ! -x "$RELAY_RELAYBAR_APP/Contents/MacOS/RelayBarGlass" ]]; then
    return 0
  fi
  mkdir -p "$(dirname "$RELAYBAR_PLIST")"
  write_relaybar_plist "$RELAYBAR_PLIST" "$RELAY_RELAYBAR_APP" "$RELAY_HOME"
  launchctl bootout "gui/$(id -u)" "$RELAYBAR_PLIST" >/dev/null 2>&1 || true
  if launchctl bootstrap "gui/$(id -u)" "$RELAYBAR_PLIST" && launchctl enable "gui/$(id -u)/$RELAYBAR_LABEL"; then
    echo "Registered the RelayBar menu bar app ($RELAYBAR_LABEL); it starts at login."
  else
    echo "warning: could not start the RelayBar menu bar app; open $RELAY_RELAYBAR_APP to run it." >&2
  fi
}

require_value() {
  if [[ $# -lt 2 || -z "$2" ]]; then
    echo "$1 requires a value" >&2
    exit 2
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      require_value "$1" "${2:-}"
      RELAY_VERSION="$2"
      shift 2
      ;;
    --sha256)
      require_value "$1" "${2:-}"
      RELAY_SHA256="$2"
      shift 2
      ;;
    --source-url)
      require_value "$1" "${2:-}"
      RELAY_SOURCE_URL="$2"
      shift 2
      ;;
    --allow-unpinned-main)
      RELAY_ALLOW_UNPINNED_MAIN=1
      shift
      ;;
    --prebuilt-binary)
      require_value "$1" "${2:-}"
      RELAY_PREBUILT_BINARY="$2"
      shift 2
      ;;
    --config-file)
      require_value "$1" "${2:-}"
      RELAY_MANAGED_CONFIG="$2"
      shift 2
      ;;
    --skip-trust-ca)
      RELAY_TRUST_CA=0
      shift
      ;;
    --skip-setup)
      SKIP_SETUP=1
      shift
      ;;
    --autoconfigure)
      RELAY_AUTOCONFIGURE=1
      shift
      ;;
    --skip-autoconfigure)
      RELAY_AUTOCONFIGURE=0
      shift
      ;;
    --background)
      BACKGROUND_SERVICE=1
      shift
      ;;
    --set-system-proxy)
      require_value "$1" "${2:-}"
      NETWORK_SERVICE="${2:-}"
      BACKGROUND_SERVICE=1
      shift 2
      ;;
    --gateway-url)
      require_value "$1" "${2:-}"
      SETUP_GATEWAY_URL="${2:-}"
      shift 2
      ;;
    --api-key)
      require_value "$1" "${2:-}"
      SETUP_API_KEY="${2:-}"
      shift 2
      ;;
    --bin-dir)
      require_value "$1" "${2:-}"
      INSTALL_BIN_DIR_OVERRIDE="${2:-}"
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

if [[ -n "$RELAY_SHA256" && ! "$RELAY_SHA256" =~ ^[A-Fa-f0-9]{64}$ ]]; then
  echo "RELAY_SHA256 must be a 64-character SHA-256 hex digest." >&2
  exit 2
fi

if [[ "$RELAY_TRUST_CA" != "0" && "$RELAY_TRUST_CA" != "1" && "$RELAY_TRUST_CA" != "auto" ]]; then
  echo "RELAY_TRUST_CA must be 0, 1, or auto." >&2
  exit 2
fi

if [[ "$RELAY_SKIP_SETUP" == "1" ]]; then
  SKIP_SETUP=1
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "install.sh v0 currently supports macOS only." >&2
  exit 1
fi

mkdir -p "$RELAY_HOME"

choose_bin_dir() {
  if [[ -n "$INSTALL_BIN_DIR_OVERRIDE" ]]; then
    printf '%s\n' "$INSTALL_BIN_DIR_OVERRIDE"
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
  if ! touch "$profile_path" 2>/dev/null || [[ ! -w "$profile_path" ]]; then
    PATH_SKIPPED_PROFILE="$profile_path"
    echo "warning: $profile_path is not writable by $(id -un) ($(ls -ld "$profile_path" 2>/dev/null | awk '{print $1, $3 ":" $4}')), so PATH was left alone; add this line to your shell profile:" >&2
    echo "  export PATH=\"$bin_dir:\$PATH\"" >&2
    export PATH="$bin_dir:$PATH"
    return 0
  fi
  if ! grep -Fqs "$bin_dir" "$profile_path"; then
    {
      printf '\n# LiteLLM Relay\n'
      printf 'export PATH="%s:$PATH"\n' "$bin_dir"
    } >> "$profile_path"
    PATH_UPDATED_PROFILE="$profile_path"
  fi
  export PATH="$bin_dir:$PATH"
}

print_path_note() {
  if [[ -n "$PATH_UPDATED_PROFILE" ]]; then
    cat <<DONE

I added $INSTALL_BIN_DIR to PATH in:
  $PATH_UPDATED_PROFILE

Open a new terminal before running relay, or run:
  export PATH="$INSTALL_BIN_DIR:\$PATH"
  relay
DONE
  elif [[ -n "$PATH_SKIPPED_PROFILE" ]]; then
    cat <<DONE

PATH was not updated: $PATH_SKIPPED_PROFILE is not writable by $(id -un).
Add this line to it, or run relay with:
  export PATH="$INSTALL_BIN_DIR:\$PATH"
  relay
DONE
  fi
}

install_relay_binary() {
  local source="$1" target="$RELAY_HOME/bin/litellm-relay" staged
  mkdir -p "$RELAY_HOME/bin"
  staged="$(mktemp "$RELAY_HOME/bin/.litellm-relay.XXXXXX")"
  cp "$source" "$staged"
  chmod 700 "$staged"
  mv -f "$staged" "$target"
}

stop_legacy_python_relay() {
  if ! command -v lsof >/dev/null 2>&1; then
    return 0
  fi

  local pid command stopped
  stopped=0
  while IFS= read -r pid; do
    if [[ -z "$pid" ]]; then
      continue
    fi
    command="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    if [[ "$command" == *"litellm_relay.cli serve"* ]]; then
      if [[ "$stopped" == "0" ]]; then
        echo "Stopping old Python LiteLLM Relay on port $RELAY_PORT..."
        stopped=1
      fi
      kill "$pid" >/dev/null 2>&1 || true
    fi
  done < <(lsof -tiTCP:"$RELAY_PORT" -sTCP:LISTEN 2>/dev/null || true)
}

verify_source_archive() {
  local archive_path="$1"

  if [[ -z "$RELAY_SHA256" ]]; then
    cat >&2 <<WARN
warning: no source archive checksum was provided.
Set RELAY_SHA256 or pass --sha256 to make this install checksum-verified.
WARN
    return 0
  fi

  echo "Verifying source archive SHA-256..." >&2
  local actual_sha=""
  if command -v shasum >/dev/null 2>&1; then
    actual_sha="$(shasum -a 256 "$archive_path" | awk '{print $1}')"
  elif command -v sha256sum >/dev/null 2>&1; then
    actual_sha="$(sha256sum "$archive_path" | awk '{print $1}')"
  else
    echo "shasum or sha256sum is required when RELAY_SHA256 is set." >&2
    exit 1
  fi

  local expected_lc actual_lc
  expected_lc="$(printf '%s' "$RELAY_SHA256" | tr '[:upper:]' '[:lower:]')"
  actual_lc="$(printf '%s' "$actual_sha" | tr '[:upper:]' '[:lower:]')"
  if [[ -z "$actual_lc" || "$actual_lc" != "$expected_lc" ]]; then
    cat >&2 <<MISMATCH
source archive checksum mismatch; aborting install.
  expected: $expected_lc
  actual:   ${actual_lc:-<unavailable>}
MISMATCH
    exit 1
  fi
  echo "Source archive checksum verified." >&2
}

download_source_tree() {
  local tmp_dir="$1"
  local archive_path source_url source_root

  archive_path="$tmp_dir/litellm-relay-source.tar.gz"
  if [[ -n "$RELAY_SOURCE_URL" ]]; then
    source_url="$RELAY_SOURCE_URL"
  elif [[ -n "$RELAY_VERSION" ]]; then
    source_url="https://github.com/LiteLLM-Labs/litellm-relay/archive/refs/tags/$RELAY_VERSION.tar.gz"
  elif [[ "$RELAY_ALLOW_UNPINNED_MAIN" == "1" ]]; then
    source_url="https://github.com/LiteLLM-Labs/litellm-relay/archive/refs/heads/main.tar.gz"
    cat >&2 <<WARN
warning: installing from mutable main because RELAY_ALLOW_UNPINNED_MAIN=1 was set.
Prefer RELAY_VERSION plus RELAY_SHA256 for production deployments.
WARN
  else
    cat >&2 <<ERROR
Remote install requires a pinned source.

Pass one of:
  RELAY_VERSION=vX.Y.Z
  --version vX.Y.Z
  RELAY_SOURCE_URL=https://.../source.tar.gz

For production, also set RELAY_SHA256 or pass --sha256.
To intentionally build mutable main, rerun with --allow-unpinned-main.
ERROR
    exit 2
  fi

  echo "Downloading LiteLLM Relay source: $source_url" >&2
  curl -fsSL "$source_url" -o "$archive_path"
  verify_source_archive "$archive_path"

  source_root="$(tar -tzf "$archive_path" | sed -n '1s#/.*##p')"
  if [[ -z "$source_root" ]]; then
    echo "source archive is empty or invalid." >&2
    exit 1
  fi
  tar -xzf "$archive_path" -C "$tmp_dir"
  if [[ ! -f "$tmp_dir/$source_root/Cargo.toml" ]]; then
    echo "source archive did not contain Cargo.toml at the expected root." >&2
    exit 1
  fi
  printf '%s\n' "$tmp_dir/$source_root"
}

if [[ -n "$RELAY_PREBUILT_BINARY" ]]; then
  if [[ ! -f "$RELAY_PREBUILT_BINARY" ]]; then
    echo "prebuilt binary not found: $RELAY_PREBUILT_BINARY" >&2
    exit 1
  fi
  stop_legacy_python_relay
  echo "Installing prebuilt LiteLLM Relay binary..."
  install_relay_binary "$RELAY_PREBUILT_BINARY"
else
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  BUILD_DIR=""
  if [[ -f "$SCRIPT_DIR/../Cargo.toml" ]]; then
    BUILD_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
  elif [[ -f "$SCRIPT_DIR/Cargo.toml" ]]; then
    BUILD_DIR="$SCRIPT_DIR"
  else
    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TMP_DIR"' EXIT
    BUILD_DIR="$(download_source_tree "$TMP_DIR")" || exit $?
  fi

  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required to install LiteLLM Relay from source." >&2
    echo "Install Rust from https://rustup.rs/ and rerun install.sh." >&2
    exit 1
  fi

  stop_legacy_python_relay

  echo "Building LiteLLM Relay..."
  cargo build --quiet --release --manifest-path "$BUILD_DIR/Cargo.toml"
  install_relay_binary "$BUILD_DIR/target/release/litellm-relay"
fi

if [[ -n "$RELAY_MANAGED_CONFIG" ]]; then
  if [[ ! -f "$RELAY_MANAGED_CONFIG" ]]; then
    echo "managed config file not found: $RELAY_MANAGED_CONFIG" >&2
    exit 1
  fi
  echo "Seeding managed Relay config from $RELAY_MANAGED_CONFIG"
  cp "$RELAY_MANAGED_CONFIG" "$RELAY_HOME/config.yaml"
  chmod 600 "$RELAY_HOME/config.yaml"
fi

INSTALL_BIN_DIR="$(choose_bin_dir)"
PATH_UPDATED_PROFILE=""
PATH_SKIPPED_PROFILE=""
mkdir -p "$INSTALL_BIN_DIR"
ln -sf "$RELAY_HOME/bin/litellm-relay" "$INSTALL_BIN_DIR/relay"
ln -sf "$RELAY_HOME/bin/litellm-relay" "$INSTALL_BIN_DIR/litellm-relay"
install_path_entry "$INSTALL_BIN_DIR"

CA_PATH="$("$RELAY_HOME/bin/litellm-relay" ca-path)"
CAPTURE_MODE="$("$RELAY_HOME/bin/litellm-relay" capture-mode)"
LOGIN_KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
MANAGED_INSTALL=0
if [[ -n "$RELAY_MANAGED_CONFIG" || "$SKIP_SETUP" == "1" ]]; then
  MANAGED_INSTALL=1
fi
TRUST_CA="$RELAY_TRUST_CA"
if [[ "$TRUST_CA" == "auto" ]]; then
  if [[ "$MANAGED_INSTALL" == "1" && "$CAPTURE_MODE" != "payloads" ]]; then
    TRUST_CA=0
  else
    TRUST_CA=1
  fi
fi
CA_TRUST_STATE="not trusted"
if [[ "$TRUST_CA" == "1" ]]; then
  echo "Trusting the Relay CA in the login keychain so payload capture can read AI app traffic; macOS asks for your account password in the Certificate Trust Settings sheet."
  if security add-trusted-cert -r trustRoot -k "$LOGIN_KEYCHAIN" "$CA_PATH" >/dev/null 2>&1; then
    CA_TRUST_STATE="trusted in the login keychain"
  else
    cat >&2 <<WARN
warning: could not add the Relay CA to the login keychain: the Certificate Trust
Settings sheet was cancelled, or no GUI session could show it (over ssh or from
an MDM script the change is denied).
Payload capture requires trusting this certificate; from a Terminal in your
session run:
  security add-trusted-cert -r trustRoot -k "$LOGIN_KEYCHAIN" "$CA_PATH"
WARN
  fi
elif [[ "$RELAY_TRUST_CA" == "0" ]]; then
  cat >&2 <<WARN
Skipping Relay CA trust because RELAY_TRUST_CA=0 or --skip-trust-ca was set.
Payload capture requires trusting this certificate later:
  security add-trusted-cert -r trustRoot -k "$LOGIN_KEYCHAIN" "$CA_PATH"
WARN
else
  cat <<SKIP
Skipping Relay CA trust: this managed install keeps payload capture off
(capture.payloads is not true in the seeded config), so nothing on this device
needs the CA and no keychain password sheet is shown.
To capture payloads later, set capture.payloads: true and trust the CA from a
Terminal in the user's session:
  security add-trusted-cert -r trustRoot -k "$LOGIN_KEYCHAIN" "$CA_PATH"
or install with RELAY_TRUST_CA=1.
SKIP
fi

if [[ "$BACKGROUND_SERVICE" != "1" ]]; then
  autoconfigure_ai_tools
  cat <<DONE
LiteLLM Relay installed.

Command:     $INSTALL_BIN_DIR/relay
Relay CA:    $CA_PATH ($CA_TRUST_STATE)

Start the interactive setup:
  relay

Setup wires your AI tools and keeps Relay running in the background as the
ai.litellm.relay LaunchAgent, which the tools ask for their credential.
DONE
  print_path_note
  exit 0
fi

if [[ "$SKIP_SETUP" == "1" ]]; then
  echo "Skipping interactive gateway setup (--skip-setup)."
  if [[ ! -f "$RELAY_HOME/config.yaml" ]]; then
    cat >&2 <<WARN
warning: --skip-setup was set but $RELAY_HOME/config.yaml does not exist.
Seed a managed config with --config-file so Relay can reach your Gateway.
WARN
  fi
  # `relay setup` runs auto-configuration itself; managed (skip-setup) deploys
  # do not, so wire up detected AI tools here from the seeded config.
  autoconfigure_ai_tools
else
  SETUP_ARGS=()
  if [[ -n "$SETUP_GATEWAY_URL" ]]; then
    SETUP_ARGS+=(--gateway-url "$SETUP_GATEWAY_URL")
  fi
  if [[ -n "$SETUP_API_KEY" ]]; then
    SETUP_ARGS+=(--api-key "$SETUP_API_KEY")
  fi

  "$RELAY_HOME/bin/litellm-relay" setup "${SETUP_ARGS[@]}"
fi

"$RELAY_HOME/bin/litellm-relay" pac > "$RELAY_HOME/relay.pac"
RELAY_PORT="$(sed -n 's/.*PROXY 127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$RELAY_HOME/relay.pac" | head -n 1)"
if [[ -z "$RELAY_PORT" ]]; then
  RELAY_PORT="4142"
fi

PLIST="$HOME/Library/LaunchAgents/ai.litellm.relay.plist"
mkdir -p "$(dirname "$PLIST")"
"$RELAY_HOME/bin/litellm-relay" launch-agent > "$PLIST"

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
launchctl enable "gui/$(id -u)/ai.litellm.relay"
install_relaybar_agent

# Periodic auto-configuration: re-detect installed AI tools on an interval so a
# tool installed after Relay gets wired to the Gateway automatically, with no
# re-run by the developer. Split across two agents by where each tool's config
# lives:
#   - a per-user LaunchAgent for the user-writable tools (Claude Code, Codex)
#   - a root LaunchDaemon for Claude Desktop, whose managed settings live in the
#     root-owned /Library/Managed Preferences and cannot be written as the user;
#     it also watches that directory, since an MDM managed-preferences refresh
#     regenerates it from the installed profiles and drops the plist
AUTOCONFIGURE_PLIST="$HOME/Library/LaunchAgents/ai.litellm.relay.autoconfigure.plist"
DESKTOP_DAEMON_LABEL="ai.litellm.relay.autoconfigure-desktop"
DESKTOP_DAEMON_PLIST="/Library/LaunchDaemons/$DESKTOP_DAEMON_LABEL.plist"

if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

# Register the root LaunchDaemon that re-configures Claude Desktop as root. HOME
# is pinned to the installing user's home so Relay reads that user's config and
# detects user-scoped evidence, while running as root to write the managed file.
install_desktop_daemon() {
  local tmp_plist
  tmp_plist="$(mktemp)"
  cat > "$tmp_plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$DESKTOP_DAEMON_LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$RELAY_HOME/bin/litellm-relay</string>
    <string>autoconfigure</string>
    <string>--only</string>
    <string>claude-desktop</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key>
    <string>$HOME</string>
  </dict>
  <key>WatchPaths</key>
  <array>
    <string>/Library/Managed Preferences</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>StartInterval</key>
  <integer>$RELAY_AUTOCONFIGURE_INTERVAL</integer>
  <key>StandardOutPath</key>
  <string>$RELAY_HOME/autoconfigure-desktop.out.log</string>
  <key>StandardErrorPath</key>
  <string>$RELAY_HOME/autoconfigure-desktop.err.log</string>
</dict>
</plist>
PLIST
  $SUDO install -m 644 -o root -g wheel "$tmp_plist" "$DESKTOP_DAEMON_PLIST" || return 1
  rm -f "$tmp_plist"
  $SUDO launchctl bootout system "$DESKTOP_DAEMON_PLIST" >/dev/null 2>&1 || true
  $SUDO launchctl bootstrap system "$DESKTOP_DAEMON_PLIST" || return 1
  $SUDO launchctl enable "system/$DESKTOP_DAEMON_LABEL" || return 1
}

remove_desktop_daemon() {
  $SUDO launchctl bootout system "$DESKTOP_DAEMON_PLIST" >/dev/null 2>&1 || true
  $SUDO rm -f "$DESKTOP_DAEMON_PLIST" >/dev/null 2>&1 || true
}

if [[ "$RELAY_AUTOCONFIGURE" == "1" ]]; then
  cat > "$AUTOCONFIGURE_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>ai.litellm.relay.autoconfigure</string>
  <key>ProgramArguments</key>
  <array>
    <string>$RELAY_HOME/bin/litellm-relay</string>
    <string>autoconfigure</string>
    <string>--only</string>
    <string>claude-code</string>
    <string>--only</string>
    <string>codex</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>StartInterval</key>
  <integer>$RELAY_AUTOCONFIGURE_INTERVAL</integer>
  <key>StandardOutPath</key>
  <string>$RELAY_HOME/autoconfigure.out.log</string>
  <key>StandardErrorPath</key>
  <string>$RELAY_HOME/autoconfigure.err.log</string>
</dict>
</plist>
PLIST
  launchctl bootout "gui/$(id -u)" "$AUTOCONFIGURE_PLIST" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "$AUTOCONFIGURE_PLIST"
  launchctl enable "gui/$(id -u)/ai.litellm.relay.autoconfigure"

  if install_desktop_daemon; then
    echo "Registered root LaunchDaemon $DESKTOP_DAEMON_LABEL for Claude Desktop."
  else
    echo "warning: could not register the Claude Desktop auto-configure daemon (needs root)." >&2
    echo "         Claude CLI and Codex still auto-configure; run install.sh with sudo to enable Claude Desktop." >&2
  fi
else
  launchctl bootout "gui/$(id -u)" "$AUTOCONFIGURE_PLIST" >/dev/null 2>&1 || true
  rm -f "$AUTOCONFIGURE_PLIST"
  remove_desktop_daemon
fi

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
Relay CA:    $CA_PATH ($CA_TRUST_STATE)
Logs:        $RELAY_HOME/relay.log.jsonl

To open the interactive terminal view:
  relay

To route Notion through Relay for a manual pilot:
  networksetup -setautoproxyurl "Wi-Fi" http://127.0.0.1:$RELAY_PORT/proxy.pac
  networksetup -setautoproxystate "Wi-Fi" on

To verify interception without changing system settings:
  curl --cacert "$CA_PATH" -x http://127.0.0.1:$RELAY_PORT https://www.notion.so

Gateway auth and Relay settings are saved in $RELAY_HOME/config.yaml.
DONE
print_path_note
