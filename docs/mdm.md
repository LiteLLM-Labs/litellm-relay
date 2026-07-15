# MDM rollout

LiteLLM Relay ships to employee Macs as a signed `.pkg` deployed through your
MDM, plus one configuration profile that points macOS Auto Proxy at Relay's
local PAC URL. Relay is macOS-only today.

Endpoints do **not** need Rust/cargo: the `.pkg` carries a prebuilt binary and
its postinstall installs Relay for the console user (CA trust in the login
keychain + a per-user LaunchAgent). See [`scripts/build-macos-pkg.sh`](../scripts/build-macos-pkg.sh).

Recommended shape, same as other endpoint software: manual pilot on one Mac,
then a small MDM pilot group, then broaden.

## Demo

Recorded walkthrough of the Microsoft Intune admin flow — creating and assigning
the PAC configuration profile and the macOS PKG app-add wizard:
[Intune rollout demo video](https://app.devin.ai/attachments/04fa814f-f780-45da-a771-d690a7df1710/intune-relay-rollout-edited.mp4).

## What gets deployed

| Artifact | Purpose | Source |
| --- | --- | --- |
| `litellm-relay-<version>.pkg` | Prebuilt binary + per-user install | Built by `scripts/build-macos-pkg.sh`, attached to the GitHub Release |
| PAC configuration profile | Points macOS Auto Proxy at `http://127.0.0.1:4142/proxy.pac` | [`mdm/litellm-relay-pac.mobileconfig.example`](../mdm/litellm-relay-pac.mobileconfig.example) |
| Claude Desktop profile *(optional)* | Wires Claude Desktop to the Gateway fleet-wide via managed preferences | [`mdm/litellm-relay-claude-desktop.mobileconfig.example`](../mdm/litellm-relay-claude-desktop.mobileconfig.example) |
| Managed `config.yaml` | Gateway URL, capture/shadow settings | [`mdm/config.yaml.example`](../mdm/config.yaml.example) |

The managed config can be baked into the `.pkg` at build time
(`--config-file`) so no separate config delivery is needed:

```bash
scripts/build-macos-pkg.sh --version 0.1.0 --config-file mdm/config.yaml.example
```

## Build the package

On a macOS build/release host (or via the Release workflow):

```bash
scripts/build-macos-pkg.sh \
  --version 0.1.0 \
  --config-file mdm/config.yaml.example \
  --sign "Developer ID Installer: Your Company (TEAMID)"
```

Output: `dist/litellm-relay-0.1.0.pkg` plus a printed SHA-256. Signing is
optional for a Jamf-only fleet but required for Intune (Gatekeeper). Tagging a
release (`v*`) also builds the `.pkg` per architecture in
[`.github/workflows/release.yml`](../.github/workflows/release.yml).

## Manual pilot (one Mac, no MDM)

```bash
curl -fsSL https://raw.githubusercontent.com/LiteLLM-Labs/litellm-relay/main/src/install.sh \
  | RELAY_ALLOW_UNPINNED_MAIN=1 bash -s -- --set-system-proxy "Wi-Fi"
```

Dashboard is served locally at `http://127.0.0.1:4142/`. Verify capture without
touching system settings:

```bash
curl --cacert "$(relay ca-path)" -x http://127.0.0.1:4142 https://www.notion.so
```

## Jamf Pro

1. **Upload the package.** Settings → Computer Management → Packages → New (or
   upload via Jamf Admin). Upload `litellm-relay-<version>.pkg`.
2. **Create the deploy policy.** Computers → Policies → New. Add a **Packages**
   payload with the Relay package, action **Install**. Trigger: Recurring
   check-in (or Enrollment Complete). Scope to your **pilot Smart Group** first.
3. **Deploy the PAC profile.** Computers → Configuration Profiles → New. Add a
   **Proxies** payload → Automatic Proxy Configuration → URL
   `http://127.0.0.1:4142/proxy.pac`. Alternatively upload
   [`mdm/litellm-relay-pac.mobileconfig.example`](../mdm/litellm-relay-pac.mobileconfig.example)
   via Upload. Scope to the same pilot group.
4. **Verify.** On a pilot Mac: `launchctl list | grep ai.litellm.relay`, open
   `http://127.0.0.1:4142/`, and confirm requests appear in your LiteLLM
   Gateway. In Jamf, confirm the policy shows Completed.
5. **Broaden.** Expand the Smart Group scope to the full fleet.

## Microsoft Intune

1. **Wrap and upload the app.** The macOS LOB app type takes a `.intunemac`
   file — wrap the signed `.pkg` with the
   [Intune App Wrapping Tool for macOS](https://github.com/msintuneappsdk/intune-app-wrapping-tool-macos):
   `./IntuneAppUtil -c litellm-relay-<version>.pkg -o .`. Then Apps → macOS →
   Add → **macOS app (PKG)** / line-of-business app, upload the `.intunemac`.
2. **Assign.** Assign the app as **Required** to your pilot Azure AD group.
3. **Deploy the PAC profile.** Devices → Configuration → Create → macOS →
   **Templates → Custom**, upload
   [`mdm/litellm-relay-pac.mobileconfig.example`](../mdm/litellm-relay-pac.mobileconfig.example).
   Assign to the same pilot group.
4. **Verify.** Monitor the app install status per device in Intune, then check
   `http://127.0.0.1:4142/` and the Gateway on a pilot Mac.
5. **Broaden.** Change the assignment to the full device group.

Use Intune trusted-certificate profiles only when testing a future managed-CA
MITM mode; the default install trusts Relay's CA in the user login keychain.

## Kandji

1. Upload the `.pkg` as a **Custom App**, audit-and-enforce or install-once.
2. Add a **Custom Profile** with the PAC payload above.
3. Use a Certificate Library Item only for a future managed-CA MITM test.

## Offboarding / uninstall

Ship `src/uninstall.sh` (also in the package payload at
`/usr/local/litellm-relay/uninstall.sh`) as a script/policy, and unassign the
PAC profile so macOS stops using Auto Proxy:

```bash
/usr/local/litellm-relay/uninstall.sh --unset-system-proxy "Wi-Fi" --remove-data
```

## Claude Desktop (managed preferences)

Relay's installed root LaunchDaemon already writes the Claude Desktop managed
preferences plist (`/Library/Managed Preferences/com.anthropic.claudefordesktop.plist`)
on each auto-configure pass, so a device that has Relay installed gets Claude
Desktop wired with no extra profile. For fleets that prefer to push it through
MDM directly (or to configure machines before Relay's first pass), deploy a
Custom Settings profile instead:

```bash
relay export-claude-desktop-profile > litellm-relay-claude-desktop.mobileconfig
```

This prints a `com.apple.ManagedClient.preferences` profile (see
[`mdm/litellm-relay-claude-desktop.mobileconfig.example`](../mdm/litellm-relay-claude-desktop.mobileconfig.example))
that forces the inference keys into the same managed-preferences domain the app
reads. Upload it as a **Custom Settings** / **Custom Profile** payload in Jamf,
Intune, or Kandji, scoped to the same group as the PAC profile. On macOS the app
honors inference config only from this managed-preferences source, so the Linux
`/etc/claude-desktop/managed-settings.json` path and user defaults have no effect.

## Notes

macOS has a single Global HTTP Proxy payload per device. Customers already using
a corporate proxy need a coordinated PAC file rather than a second competing
profile.

Using `--api-key` (or `gateway.api_key` in the managed config) writes a static
Gateway key to every device. Prefer per-user browser SSO where your Gateway
supports it.
