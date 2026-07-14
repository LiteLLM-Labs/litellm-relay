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
RELAY_TRUST_CA="${RELAY_TRUST_CA:-1}"
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
  --skip-trust-ca                 Install without adding the Relay CA to login keychain
  --background                    Configure Gateway auth and start the LaunchAgent
  --set-system-proxy "Wi-Fi"      Route the named macOS network service through Relay
  --gateway-url URL               Gateway URL for non-interactive setup
  --api-key KEY                   Gateway key for non-interactive setup
  --bin-dir DIR                   Install relay shims into DIR

When run from a checked-out repository, this builds the local source tree.
When run as a standalone remote script, pass RELAY_VERSION/--version or
RELAY_SOURCE_URL/--source-url. Mutable main.tar.gz installs require the explicit
RELAY_ALLOW_UNPINNED_MAIN=1 or --allow-unpinned-main opt-in.

By default this installs the relay command and trusts the Relay local CA in your
login keychain so AI app payloads can be captured. Then run:

  relay

The relay command opens the interactive setup wizard when needed and then starts
the foreground terminal trace view.

Pass --background to also configure Gateway SSO and start the Relay LaunchAgent.
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
  RELAY_TRUST_CA=0              Same as --skip-trust-ca
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

if [[ "$RELAY_TRUST_CA" != "0" && "$RELAY_TRUST_CA" != "1" ]]; then
  echo "RELAY_TRUST_CA must be 0 or 1." >&2
  exit 2
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
  mkdir -p "$RELAY_HOME/bin"
  cp "$RELAY_PREBUILT_BINARY" "$RELAY_HOME/bin/litellm-relay"
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
  mkdir -p "$RELAY_HOME/bin"
  cp "$BUILD_DIR/target/release/litellm-relay" "$RELAY_HOME/bin/litellm-relay"
fi
chmod 700 "$RELAY_HOME/bin/litellm-relay"

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
mkdir -p "$INSTALL_BIN_DIR"
ln -sf "$RELAY_HOME/bin/litellm-relay" "$INSTALL_BIN_DIR/relay"
ln -sf "$RELAY_HOME/bin/litellm-relay" "$INSTALL_BIN_DIR/litellm-relay"
install_path_entry "$INSTALL_BIN_DIR"

CA_PATH="$("$RELAY_HOME/bin/litellm-relay" ca-path)"
if [[ "$RELAY_TRUST_CA" == "1" ]]; then
  security add-trusted-cert -r trustRoot -k "$HOME/Library/Keychains/login.keychain-db" "$CA_PATH" >/dev/null 2>&1 || {
    cat >&2 <<WARN
warning: could not add the Relay CA to the login keychain.
Payload capture requires trusting this certificate:
  $CA_PATH
WARN
  }
else
  cat >&2 <<WARN
Skipping Relay CA trust because RELAY_TRUST_CA=0 or --skip-trust-ca was set.
Payload capture requires trusting this certificate later:
  $CA_PATH
WARN
fi

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

if [[ "$RELAY_SKIP_SETUP" == "1" ]]; then
  SKIP_SETUP=1
fi

if [[ "$SKIP_SETUP" == "1" ]]; then
  echo "Skipping interactive gateway setup (--skip-setup)."
  if [[ ! -f "$RELAY_HOME/config.yaml" ]]; then
    cat >&2 <<WARN
warning: --skip-setup was set but $RELAY_HOME/config.yaml does not exist.
Seed a managed config with --config-file so Relay can reach your Gateway.
WARN
  fi
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

mkdir -p "$RELAY_HOME/bin"
cat > "$RELAY_HOME/bin/run-relay" <<RUNNER
#!/usr/bin/env zsh
set -euo pipefail
exec "$RELAY_HOME/bin/litellm-relay" serve
RUNNER
chmod 700 "$RELAY_HOME/bin/run-relay"

"$RELAY_HOME/bin/litellm-relay" pac > "$RELAY_HOME/relay.pac"
RELAY_PORT="$(sed -n 's/.*PROXY 127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$RELAY_HOME/relay.pac" | head -n 1)"
if [[ -z "$RELAY_PORT" ]]; then
  RELAY_PORT="4142"
fi

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

Gateway auth and Relay settings are saved in $RELAY_HOME/config.yaml.
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
