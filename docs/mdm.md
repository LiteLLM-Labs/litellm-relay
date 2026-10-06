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
| Managed `config.yaml` | Gateway URL, IdP issuer and client id, capture/shadow settings | [`mdm/config.yaml.example`](../mdm/config.yaml.example) |

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

## Credential broker

On macOS the Relay daemon (`relay serve`, the `ai.litellm.relay` LaunchAgent) is
the only thing on the device that holds a Gateway credential. Claude Code,
Claude Desktop, and Codex are wired with `relay credential` as their credential
helper, which asks the daemon over the Unix socket `~/.litellm-relay/broker.sock`
(directory 0700, socket 0600) and prints the bearer it gets back. The proxy
port never serves credentials, and no key, token, or session is written into
Claude Code's settings file, Codex's `config.toml`, or the Claude Desktop
managed file: the developer's IdP session and the Gateway key live in the
daemon's memory and are gone when it stops

Before answering, the daemon checks who is asking. It reads the connecting
process's uid (it has to match the daemon's), its pid, and its audit token, and
walks the peer and up to four of its ancestors, validating each against the
Apple code signature requirement of an allowed client: the signing identifier
plus the Team ID on the leaf certificate. The defaults are Claude Desktop
(`com.anthropic.claudefordesktop` and its `.helper`, team `Q6L2SF6YDW`), Claude
Code (`com.anthropic.claude-code`, team `Q6L2SF6YDW`), and Codex (`codex`, team
`2DC432GLL2`). A shell, a script, or any other process that runs
`relay credential` gets `caller_refused` and the chain it was refused on, and
the refusal is counted in `/api/status`. `credential.allowed_callers` in
`config.yaml` replaces the defaults (see `mdm/config.yaml.example`), so list
every client you keep

With an IdP configured, the first interactive request runs the browser sign-in
from the daemon, exchanges the ID token for a Gateway session credential the
way the older helpers did, and mints a Gateway key scoped to the device's team
through `POST /key/generate` with a 60 minute duration and the alias
`relay-<hostname>-<timestamp>`. The daemon extends that key at its half-life
with `POST /key/update`, replaces an expired one on the next request, and
deletes it on `relay sign-out` and when the daemon stops. Minting needs the
team to allow `/key/generate`, `/key/update`, and `/key/delete` for its members
(`POST /team/permissions_update` on the Gateway); while it does not, the daemon
serves the session credential itself and prints the fix once on stderr. A 401
on a mint or an extension means the Gateway no longer accepts the session
credential (its sealing key rotated), so the daemon exchanges the IdP session
again and mints anew, without a browser while the IdP session is still valid.
Without an IdP, the daemon serves `gateway.api_key` from `config.yaml` to the
same allowed clients, so a static-key rollout gets the caller check too. The
daemon re-reads `config.yaml` on the next request or tick after it changes, so
a re-run of `relay autoconfigure` or `relay onboard` needs no daemon restart,
and a changed IdP or Gateway signs the daemon out while a changed team only
re-mints the key

Claude Desktop runs the helper with `CLAUDE_HELPER_CONTEXT=background` or
`scheduled-task` when nobody is at the keyboard; those requests never open a
browser and answer `signed_out` until a developer runs the app interactively or
`relay sign-in` from a terminal. `relay sign-in` always starts a fresh browser
sign-in and `relay sign-out` deletes the key and forgets the session. The
`broker` block of `/api/status` shows `signed_in`, `user_id`, `team`,
`key_expires_at`, `key_extended_at`, the `source` of the last answer
(`minted_key`, `session_credential`, `identity_token`, or `static_key`), and
`refused_callers`, never a token

Linux hosts have no code-signature check, so there the tools keep the on-disk
`claude-token` and `codex-token` helpers and a static key goes into the tool's
config as before. Those commands and their caches under `~/.litellm-relay`
still exist on macOS too for now and are removed in a follow-up

## Notes

macOS has a single Global HTTP Proxy payload per device. Customers already using
a corporate proxy need a coordinated PAC file rather than a second competing
profile.

Using `--api-key` (or `gateway.api_key` in the managed config) writes a static
Gateway key to every device. With an IdP onboarded, Relay exchanges each
developer's sign-in for their own Gateway credential instead, so the flag is
only needed for Gateways without the authorization server. A push has no
terminal, so it never opens a browser: it uses the identity the developer
already signed in with, renewing it silently with its refresh token, and keeps
a saved Gateway key when there is none. Prefer per-user browser SSO where your
Gateway supports it. The credential check also runs when `--api-key` is
combined with the `--oidc-*` flags, since Codex and Claude Code prefer the
explicit key over the IdP.

The `--oidc-*` flags on `relay autoconfigure` set the IdP for Claude Code,
Codex, and Claude Desktop alike, and Relay saves it under `idp:` in
`config.yaml`. From then on every `autoconfigure` run without `--api-key`, the
LaunchAgent's included, sets Claude Code and Codex up for browser sign-in and
stops writing a saved Gateway key into their config, so a key passed once with
`--api-key` lasts only until the next scheduled run. To put only Claude Desktop
on SSO and keep Claude Code and Codex on a static key, pass the flags to
`relay onboard-claude-desktop` instead

The per-user LaunchAgent re-runs `autoconfigure` at login and on its interval.
Each run checks the stored Gateway credential first. Without an IdP, if the
Gateway rejects it, the run leaves the Codex, Claude Code, and Claude Desktop
configs untouched and exits non-zero, so an expired session shows up in the
LaunchAgent's exit status and in the `credential` block of `/api/status`
instead of being rewritten into the tools every hour. With an IdP saved, a
rejected or unreachable saved key is only a warning on stderr: the run still
exchanges the developer's sign-in for every tool, never writes the refused key,
and exits non-zero only when no tool could be configured. The check also covers
the saved key Claude Desktop falls back to when it is not given OIDC flags, so
an IdP setup cannot copy a rejected key into the desktop app. Claude Desktop
reads its managed file at launch, so a renewed credential reaches an app that
has been running longer than the credential's lifetime only after a restart.

Upgrading a fleet from a Relay that sent the ID token itself: the Gateway now
has to accept the device's `--team` for the signed-in developer, through a
`team_id` claim in the ID token or the developer's team membership on the
Gateway under `fallback_to_db_teams: True`. A team it does not accept stops
the tools with `the gateway refused the token exchange (HTTP 400
invalid_request: subject_token was rejected by the gateway's JWT auth)` where
the older Relay's ID token was accepted and the team ignored, so add the
developers to their teams on the Gateway before the rollout, or push a
`--team` the Gateway accepts. `/.well-known/litellm-cli-auth`, `/register`,
and `/token` are reached without a bearer, so let them through any proxy or
WAF in front of the Gateway unauthenticated; while they are blocked, Claude
Code and Codex send the ID token itself with a notice on stderr, as the older
Relay did, and Claude Desktop keeps its saved key or reports the failure (see
[claude-code.md](claude-code.md#gateway-configuration)). `/api/status` reflects the credential
currently saved in `config.yaml`, so re-running `litellm-relay setup` updates
the dashboard without restarting Relay
