# Claude Desktop onboarding

Relay wires the Claude Desktop app (third-party mode) onto your LiteLLM AI Gateway. `relay onboard-claude-desktop` writes the OS-native managed configuration Claude Desktop reads on launch — `/etc/claude-desktop/managed-settings.json` on Linux — so the app boots straight into gateway mode ("Your organization has set up Claude to run through a custom inference gateway. No Claude.ai account needed."). The Gateway must implement the Anthropic Messages API (`POST /v1/messages`), which LiteLLM does.

## Single sign-on (recommended)

Each developer signs in with their corporate account and the resulting OIDC token is sent to the Gateway as the bearer credential, so no provider or gateway key lands on the device.

```bash
sudo relay onboard-claude-desktop \
  --gateway-url https://gateway.yourco.com \
  --oidc-client-id "$CLIENT_ID" \
  --oidc-issuer https://login.yourco.com/v2.0
```

## Static key (proof of concept)

Distribute a shared Gateway key instead of SSO.

```bash
sudo relay onboard-claude-desktop \
  --gateway-url https://gateway.yourco.com \
  --api-key sk-your-gateway-key
```

## Notes

The managed file must be root-owned (Claude Desktop ignores a user-writable one), so run the command with `sudo`. Restart Claude Desktop to pick up the configuration.
