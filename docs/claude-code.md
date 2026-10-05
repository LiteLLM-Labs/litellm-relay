# Claude Code onboarding

Relay onboards Claude Code onto your LiteLLM AI Gateway with zero manual setup. Employees never receive a provider API key and never export environment variables. Their corporate identity authenticates each request, and the Gateway maps that identity to a per-user virtual key with its own budget, model access, and spend tracking.

## How it works

An admin enables JWT auth on the Gateway once, and from then on onboarding a device is a single `relay onboard` call. The MDM package (Jamf/Intune) installs Claude Code from your internal registry (npm/Homebrew via JFrog) alongside Relay, then runs `relay onboard`, which writes `~/.claude/settings.json` so Claude Code points at the Gateway and pulls its bearer token from Relay's token helper

When the developer runs `claude`, Relay signs them in through the corporate IdP on first use (OIDC authorization code with PKCE, so the app registration needs no client secret) and hands Claude Code the short-lived ID token. Relay keeps that session alive with the refresh token, so the browser opens once per device, not once per token lifetime. The Gateway validates the token, maps it to the developer's virtual key, enforces budget and limits, logs spend, and forwards upstream. No provider key ever touches the device, and offboarding is removing the identity from the SSO group, after which its tokens stop validating

## Usage

`relay onboard` writes the Claude Code settings pointing at the Gateway:

![relay onboard writing Claude settings](img/claude-onboard.png)

On first use Relay opens the corporate IdP sign-in in the browser (a local mock IdP is shown here; in production this is your org's OIDC tenant, Entra being the one this flow was verified against):

![corporate IdP sign-in](img/claude-idp-signin.png)

After sign-in, Claude Code answers through the Gateway with no key on the device:

![Claude Code answering through the Gateway](img/claude-code-answer.png)

The Gateway auto-registers a per-user virtual key from the SSO identity and tracks spend by user and team:

![auto-registered per-user virtual keys](img/claude-virtual-keys.png)

## Commands

`relay onboard` wires Claude Code to the Gateway and records the IdP issuer and client id, team, and model:

```bash
relay onboard \
  --gateway-url https://gateway.yourco.com \
  --oidc-issuer https://login.yourco.com \
  --oidc-client-id 00000000-0000-0000-0000-000000000000 \
  --team engineering \
  --model claude-sonnet-4-5
```

Relay reads `<issuer>/.well-known/openid-configuration` to find the authorization and token endpoints and requests `openid profile email offline_access` (trimmed to what the IdP advertises; pass `--oidc-scopes` to request an exact set, `openid` is always included). Relay refuses a discovery document whose `issuer` is not the configured one or whose endpoints are not https, so `--oidc-issuer` must be the tenant-specific issuer (for Entra `https://login.microsoftonline.com/<tenant id>/v2.0`, never `common` or `organizations`). The app registration is a public client: the redirect URI is `http://127.0.0.1/callback` (loopback, any port, or a fixed one with `--oidc-redirect-port`), it must issue ID tokens, and it needs no secret. In Entra that is the "Mobile and desktop applications" platform with "Allow public client flows" on; one registration serves Claude Code, Codex, and Claude Desktop, so keep every tool's redirect URI on it.

`relay claude-token` is what Claude Code's `apiKeyHelper` calls. It returns the cached ID token, renews it silently with the refresh token when it is within ten minutes of expiry (a failed renewal keeps serving the current token until a minute before it expires), and only opens the browser when there is no session to refresh. Concurrent calls from several tool processes share one cross-process lock, so a single sign-in or renewal serves all of them. It prints only the token to stdout; diagnostics go to stderr.

## Generated settings

```json
{
  "apiKeyHelper": "relay claude-token",
  "env": {
    "ANTHROPIC_BASE_URL": "https://gateway.yourco.com",
    "ANTHROPIC_CUSTOM_HEADERS": "x-litellm-team: engineering",
    "ANTHROPIC_MODEL": "claude-sonnet-4-5"
  }
}
```

No provider API key is written to the device. The identity session (ID token plus refresh token) is cached under `~/.litellm-relay/identity-token.json` with `0600` permissions on Unix.

## Gateway configuration

The headline auth mode is JWT with `auto_register`. The Gateway validates the ID token against your IdP's JWKS and maps claims to a per-user virtual key and team:

```yaml
general_settings:
  enable_jwt_auth: True
  litellm_jwtauth:
    user_id_jwt_field: "sub"
    user_id_upsert: True
    # team_id_jwt_field: "team_id"  # only when your IdP puts a team_id claim in the ID token
    # team_id_upsert: True
    virtual_key_claim_field: "email"
    unregistered_jwt_client_behavior: "auto_register"
```

A standard ID token carries no `team_id` claim, and the Gateway rejects every token that lacks a claim named in `team_id_jwt_field`, so leave the two `team_id_*` lines out unless your IdP is configured to issue that claim.

```bash
JWT_PUBLIC_KEY_URL="https://login.yourco.com/.well-known/openid-configuration"
JWT_ISSUER="https://login.yourco.com"
JWT_AUDIENCE="00000000-0000-0000-0000-000000000000"
```

`JWT_AUDIENCE` is the client id of the app registration, since an ID token's `aud` is the client it was issued to.

## Production versus demo IdP

In production, `--oidc-issuer` and `--oidc-client-id` name your corporate IdP's OIDC issuer and a public app registration in it. Any OIDC provider that lets a public client complete authorization code plus PKCE and hands it a refresh token without a client secret fits; Entra is the provider this flow was verified against (issuer `https://login.microsoftonline.com/<tenant id>/v2.0`). Google's OAuth clients require a client secret at the token endpoint even for desktop apps, so Google does not fit this flow today. The screenshots in the README use a local mock IdP for demonstration only; it is not part of a deployment.

## MDM rollout

The MDM package installs Claude Code from your internal registry and runs `relay onboard` with your Gateway URL, IdP issuer and client id, and default team. Everything else follows the standard Relay rollout in [mdm.md](mdm.md): package the repo, deploy to the pilot scope, then broaden through Jamf or Intune. Because the settings file contains no provider key and the token is fetched at runtime through the IdP, the same package is safe to push fleet-wide.
