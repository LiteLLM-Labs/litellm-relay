#!/usr/bin/env bash
#
# Build a deployable macOS installer package (.pkg) for LiteLLM Relay.
#
# The package ships a prebuilt relay binary plus install.sh/uninstall.sh and an
# optional managed config. Its postinstall runs the installer as the console
# user so endpoints do NOT need Rust/cargo. Upload the resulting .pkg to Jamf
# (Packages) or wrap it for Intune (see docs/mdm.md).
#
# Must run on macOS (uses pkgbuild). Run on the build/release host, not on
# employee laptops.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

IDENTIFIER="ai.litellm.relay"
INSTALL_LOCATION="/usr/local/litellm-relay"
VERSION=""
BINARY=""
CONFIG_FILE=""
OUTPUT=""
SIGN_IDENTITY=""

usage() {
  cat <<'USAGE'
Build a macOS .pkg for LiteLLM Relay.

Usage:
  scripts/build-macos-pkg.sh --version 0.1.0 [options]

Options:
  --version VERSION       Package version, e.g. 0.1.0 (required)
  --binary PATH           Prebuilt relay binary to package
                          (default: build with cargo build --release --locked)
  --config-file PATH      Managed config.yaml to bake into the package
                          (see mdm/config.yaml.example)
  --output PATH           Output .pkg path
                          (default: dist/litellm-relay-<version>.pkg)
  --identifier ID         Package identifier (default: ai.litellm.relay)
  --sign "IDENTITY"       Developer ID Installer identity for productsign
  -h, --help              Show this help

Notarization (staple) is a follow-up step handled outside this script.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:-}"; shift 2 ;;
    --binary) BINARY="${2:-}"; shift 2 ;;
    --config-file) CONFIG_FILE="${2:-}"; shift 2 ;;
    --output) OUTPUT="${2:-}"; shift 2 ;;
    --identifier) IDENTIFIER="${2:-}"; shift 2 ;;
    --sign) SIGN_IDENTITY="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "build-macos-pkg.sh must run on macOS (needs pkgbuild)." >&2
  exit 1
fi

if [[ -z "$VERSION" ]]; then
  echo "--version is required (e.g. --version 0.1.0)." >&2
  exit 2
fi

if [[ -n "$CONFIG_FILE" && ! -f "$CONFIG_FILE" ]]; then
  echo "managed config not found: $CONFIG_FILE" >&2
  exit 1
fi

if [[ -z "$BINARY" ]]; then
  echo "Building release binary with cargo..."
  cargo build --release --locked --manifest-path "$REPO_ROOT/Cargo.toml"
  BINARY="$REPO_ROOT/target/release/litellm-relay"
fi

if [[ ! -f "$BINARY" ]]; then
  echo "relay binary not found: $BINARY" >&2
  exit 1
fi

if [[ -z "$OUTPUT" ]]; then
  OUTPUT="$REPO_ROOT/dist/litellm-relay-$VERSION.pkg"
fi
mkdir -p "$(dirname "$OUTPUT")"

STAGE_DIR="$(mktemp -d)"
SCRIPTS_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR" "$SCRIPTS_DIR"' EXIT

# Payload laid down at $INSTALL_LOCATION on the device.
install -m 0755 "$BINARY" "$STAGE_DIR/litellm-relay"
install -m 0755 "$REPO_ROOT/src/install.sh" "$STAGE_DIR/install.sh"
install -m 0755 "$REPO_ROOT/src/uninstall.sh" "$STAGE_DIR/uninstall.sh"
if [[ -n "$CONFIG_FILE" ]]; then
  install -m 0644 "$CONFIG_FILE" "$STAGE_DIR/config.yaml"
fi

# Postinstall runs the installer as the console user.
install -m 0755 "$SCRIPT_DIR/macos-pkg/postinstall" "$SCRIPTS_DIR/postinstall"

echo "Building package..."
UNSIGNED_PKG="$OUTPUT"
if [[ -n "$SIGN_IDENTITY" ]]; then
  UNSIGNED_PKG="$STAGE_DIR/../unsigned.pkg"
fi

pkgbuild \
  --root "$STAGE_DIR" \
  --identifier "$IDENTIFIER" \
  --version "$VERSION" \
  --install-location "$INSTALL_LOCATION" \
  --scripts "$SCRIPTS_DIR" \
  "$UNSIGNED_PKG"

if [[ -n "$SIGN_IDENTITY" ]]; then
  echo "Signing package with: $SIGN_IDENTITY"
  productsign --sign "$SIGN_IDENTITY" "$UNSIGNED_PKG" "$OUTPUT"
  rm -f "$UNSIGNED_PKG"
fi

echo "Built: $OUTPUT"
shasum -a 256 "$OUTPUT"
