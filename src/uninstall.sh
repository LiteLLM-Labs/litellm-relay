#!/usr/bin/env bash
set -euo pipefail

PLIST="$HOME/Library/LaunchAgents/ai.litellm.relay.plist"
RELAY_HOME="${RELAY_HOME:-$HOME/.litellm-relay}"

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
rm -f "$PLIST"

cat <<DONE
LiteLLM Relay LaunchAgent removed.

Relay data remains at:
  $RELAY_HOME

If you enabled a system PAC manually, disable it with:
  networksetup -setautoproxystate "Wi-Fi" off
DONE

