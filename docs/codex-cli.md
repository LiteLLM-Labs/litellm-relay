# Codex CLI onboarding

Relay onboards the OpenAI Codex CLI onto your LiteLLM AI Gateway the same way it onboards Claude Code: it writes Codex's own config so `codex` routes through the Gateway with the developer's corporate identity, and no provider API key touches the device.

Codex reads `~/.codex/config.toml`. Relay defines a custom OpenAI-compatible provider under `[model_providers.<id>]` (pointing `base_url` at the Gateway's `/v1`) and selects it with the top-level `model_provider`/`model` keys. For the credential it uses Codex's command-backed `auth` hook, which runs Relay's token command to fetch the Gateway credential on demand (Codex refreshes it on the `refresh_interval_ms` interval).

> **Gateway must serve the Responses API.** Codex only supports `wire_api = "responses"` (`wire_api = "chat"` is rejected by the CLI), so the provider talks to the Gateway's `POST /v1/responses`. LiteLLM supports the Responses API — make sure it is enabled for the models you expose.

## Step 1: Enable JWT auth on the Gateway (admin, once)

Same as [Claude Code](claude-code.md#gateway-configuration) — turn on JWT auth with `auto_register` so each SSO identity maps to its own virtual key and limits.

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

Leave the `team_id_*` lines out unless your IdP issues a `team_id` claim; the Gateway rejects tokens that lack a configured team claim (see [Claude Code](claude-code.md#gateway-configuration)).

## Step 2: Run `relay onboard-codex` on the device

```bash
relay onboard-codex \
  --gateway-url https://gateway.yourco.com \
  --oidc-issuer https://login.yourco.com \
  --oidc-client-id 00000000-0000-0000-0000-000000000000 \
  --team engineering \
  --model gpt-5-codex
```

The `--oidc-*` flags are the same ones `relay onboard` takes (see [Claude Code](claude-code.md#commands)); the IdP session is shared, so a developer who already signed in for Claude Code is not asked again.

This writes `~/.codex/config.toml`:

```toml
model = "gpt-5-codex"
model_provider = "litellm"

[model_providers.litellm]
name = "LiteLLM AI Gateway"
base_url = "https://gateway.yourco.com/v1"
wire_api = "responses"
http_headers = { x-litellm-team = "engineering" }

[model_providers.litellm.auth]
command = "/usr/local/bin/litellm-relay"
args = ["codex-token"]
timeout_ms = 390000
refresh_interval_ms = 300000
```

There is no API key in the file. `relay codex-token` prints a Gateway credential for the configured team on stdout, which is exactly what Codex's `auth` hook expects: Relay exchanges the developer's IdP sign-in for it at the Gateway's `/token` endpoint and refreshes it before it expires (the exchange is described in [claude-code.md](claude-code.md)). `timeout_ms` raises Codex's five-second ceiling on the helper to the length of a whole browser sign-in, so the first `codex` run can complete the IdP flow instead of being killed mid sign-in. The file is written with `0600` permissions.

## Step 3: Start Codex and sign in

The developer runs `codex` with no key and no exports. On first use Relay opens the corporate IdP sign-in in the browser (OIDC authorization code with PKCE), caches the identity session, exchanges it for a Gateway credential, and hands Codex that credential each time the hook runs, renewing both with their refresh tokens so the browser does not open again when they expire. Spend is tracked per-user in LiteLLM, exactly as with Claude Code.

## Credential alternatives

The `auth` hook is the default and keeps no key on the device. Codex treats `auth`, `env_key`, and `experimental_bearer_token` as mutually exclusive, so Relay writes exactly one.

- `--env-key <VAR>`: Codex reads the bearer key from an environment variable (`env_key = "<VAR>"`) rather than invoking the hook. Populate it with the Gateway credential from your shell profile:

  ```bash
  relay onboard-codex --gateway-url https://gateway.yourco.com --env-key LITELLM_API_KEY
  export LITELLM_API_KEY="$(relay codex-token)"
  ```

- `--api-key <KEY>`: for environments without an IdP, embeds a static gateway key as `experimental_bearer_token` on the provider.

  ```bash
  relay onboard-codex --gateway-url https://gateway.yourco.com --api-key sk-...
  ```

A static key that `autoconfigure` writes, whether passed with `--api-key` or
saved by `litellm-relay setup`, is verified against the Gateway before each
write. The session credential that `setup` stores expires, so a key the Gateway
rejects aborts the run with `Run litellm-relay setup to sign in again` rather
than landing in `config.toml` as `experimental_bearer_token`

## Usage

The developer only runs `codex`. It answers through the Gateway — here it returns a deterministic phrase, and the request lands on the Gateway's Responses API:

![Codex CLI answering through the Gateway](img/codex-cli-answer.png)

## Demo

Claude Desktop and the Codex CLI, both onboarded by Relay and answering through one LiteLLM Gateway with zero developer setup:

[▶ Watch the demo (mp4)](video/claude-desktop-codex-cli-gateway-demo.mp4)
