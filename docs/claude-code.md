# Claude Code onboarding

Relay onboards Claude Code onto your LiteLLM AI Gateway with zero manual setup. Employees never receive a provider API key and never export environment variables. Their corporate identity authenticates each request, and the Gateway maps that identity to a per-user virtual key with its own budget, model access, and spend tracking.

## v0 flow

1. Admin (once): enable JWT auth on the Gateway with `auto_register`, so each SSO identity maps to its own virtual key and limits with no manual key handoff
2. Package (Jamf/Intune): the managed install pulls Claude Code from your internal registry (npm/Homebrew via JFrog) and installs Relay
3. Package: `relay onboard` writes `~/.claude/settings.json`, pointing `ANTHROPIC_BASE_URL` at the Gateway, adding the team header, and wiring an `apiKeyHelper` that supplies the identity token
4. Developer: opens a terminal and runs `claude`; no key, no exports
5. Runtime: Relay signs the developer in through the corporate IdP on first use and hands Claude Code a short-lived bearer token; the Gateway validates it, maps it to the developer's virtual key, enforces budget and limits, logs spend, and forwards upstream
6. Offboarding: remove the identity from the SSO group and its tokens stop validating; no secrets live on the device

## Commands

`relay onboard` wires Claude Code to the Gateway and records the IdP authorize URL, team, and model:

```bash
relay onboard \
  --gateway-url https://gateway.yourco.com \
  --authorize-url https://login.yourco.com/authorize \
  --team engineering \
  --model claude-sonnet-4-5
```

`relay claude-token` is what Claude Code's `apiKeyHelper` calls. It returns a cached identity token, or starts a browser sign-in when the token is missing or within a minute of expiry, and prints only the token to stdout. Diagnostics go to stderr.

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

No provider API key is written to the device. The identity token is cached under `~/.litellm-relay/claude-token.json` with `0600` permissions on Unix.

## Gateway configuration

The headline auth mode is JWT with `auto_register`. The Gateway validates the bearer token against your IdP's JWKS and maps claims to a per-user virtual key and team:

```yaml
general_settings:
  enable_jwt_auth: True
  litellm_jwtauth:
    user_id_jwt_field: "sub"
    user_id_upsert: True
    team_id_jwt_field: "team_id"
    team_id_upsert: True
    virtual_key_claim_field: "email"
    unregistered_jwt_client_behavior: "auto_register"
```

```bash
JWT_PUBLIC_KEY_URL="https://login.yourco.com/.well-known/jwks.json"
JWT_ISSUER="https://login.yourco.com"
JWT_AUDIENCE="litellm-gateway"
```

## Production versus demo IdP

In production, `--authorize-url` points at your corporate IdP's OIDC authorize endpoint (Okta, Entra, Google, and similar). The screenshots in the README use a local mock IdP for demonstration only; it is not part of a deployment.

## MDM rollout

The MDM package installs Claude Code from your internal registry and runs `relay onboard` with your Gateway URL, IdP authorize URL, and default team. Everything else follows the standard Relay rollout in [mdm.md](mdm.md): package the repo, deploy to the pilot scope, then broaden through Jamf or Intune. Because the settings file contains no provider key and the token is fetched at runtime through the IdP, the same package is safe to push fleet-wide.
