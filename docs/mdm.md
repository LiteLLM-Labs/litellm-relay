# MDM rollout

LiteLLM Relay v0 supports the same rollout shape buyers expect for endpoint
software: manual pilot first, then Jamf, Intune, or Kandji.

## Manual pilot

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/src/install.sh | bash
```

To route Notion traffic immediately:

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/src/install.sh \
  | bash -s -- --set-system-proxy "Wi-Fi"
```

The installed dashboard is served locally at:

```text
http://127.0.0.1:4142/
```

## Jamf

1. Package this repo as a signed macOS package.
2. Deploy the package to the pilot scope.
3. Deploy a configuration profile with an Auto Proxy URL:
   `http://127.0.0.1:4142/proxy.pac`.
4. Set environment values through a managed LaunchAgent plist or a future Relay
   enrollment profile:
   - `LITELLM_GATEWAY_URL`
   - `LITELLM_RELAY_SHADOW_ENABLED`
   - `LITELLM_RELAY_SHADOW_MODEL`

## Intune

1. Deploy the package as a macOS line-of-business app.
2. Deploy a custom configuration profile for PAC/system proxy settings.
3. Use Intune trusted certificate profiles only when testing a future MITM mode.

## Kandji

1. Upload the package as a Custom App.
2. Deploy a custom profile with the PAC URL.
3. Use a Certificate Library Item only for a future managed-CA MITM test.

## Notes

macOS has a single Global HTTP Proxy payload per device. Customers already using
a corporate proxy need a coordinated PAC file instead of a second competing
profile.
