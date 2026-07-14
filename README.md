# LiteLLM Relay

LiteLLM Relay is a proxy you install on employee machines. It does two things.

First, it manages your AI coding tools. Relay installs and version-manages Claude Code and Codex across every laptop through your MDM, writes their settings, and lets developers sign in with their corporate identity through the LiteLLM AI Gateway, so nobody handles a provider API key.

Second, it captures shadow AI. Relay detects AI traffic from tools like Notion AI, Perplexity, and OpenClaw and routes it to the Gateway, making it a single pane of glass for all AI usage in your company.

<img width="2467" height="1080" alt="relay-hero" src="https://github.com/user-attachments/assets/e766224d-014b-4083-b03e-be11abfb0b4a" />

# Usage 
 1. Install LiteLLM Relay on all your employee devices, using [supported MDM](https://github.com/LiteLLM-Labs/litellm-relay#supported-mdms)
 2. Employees use AI tools as they normally would, such as Notion AI.
    <img width="2200" height="1654" alt="Xnapper-2026-07-09-18 25 01" src="https://github.com/user-attachments/assets/01f59c09-c927-4d04-af37-35ff5b7ec8fb" />
 3. Every request, response, and usage event is captured in LiteLLM.
    <img width="2200" height="1327" alt="Xnapper-2026-07-09-18 47 14" src="https://github.com/user-attachments/assets/dfe69818-ba4d-4874-b386-d3d7a061be39" />

## Claude Code onboarding with IdP sign-in

Relay onboards Claude Code onto your LiteLLM AI Gateway with no manual setup. Employees never receive a provider API key and never export environment variables. Their existing corporate identity authenticates each request, and the Gateway maps that identity to a per-user virtual key with its own budget, model access, and spend tracking. Codex follows the same pattern — see [Codex onboarding](#codex-onboarding-with-idp-sign-in) below

This is the step-by-step guide for setting it up. See [docs/claude-code.md](docs/claude-code.md) for the full Gateway configuration and MDM detail

### Step 1: Enable JWT auth on the Gateway (admin, once)

Turn on JWT auth with `auto_register` so each SSO identity maps to its own virtual key and limits with no manual key handoff

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

### Step 2: Run `relay onboard` on the device

The MDM package (Jamf/Intune) installs Claude Code from your internal registry and Relay, then runs `relay onboard`. It writes `~/.claude/settings.json` pointing `ANTHROPIC_BASE_URL` at the Gateway, adds the team header, and wires an `apiKeyHelper` that supplies the identity token. Note there is no `ANTHROPIC_API_KEY` on the machine

```bash
relay onboard \
  --gateway-url https://gateway.yourco.com \
  --authorize-url https://login.yourco.com/authorize \
  --team engineering
```

![relay onboard writing Claude settings](docs/img/claude-onboard.png)

### Step 3: Start Claude Code and sign in

The developer runs `claude` with no key and no exports. Relay opens the corporate IdP sign-in in the browser. A local mock IdP is shown here; in production this is your Okta, Entra, or Google tenant, set through `--authorize-url`

![corporate IdP sign-in](docs/img/claude-idp-signin.png)

### Step 4: Use Claude Code through the Gateway

After sign-in, Relay hands Claude Code a short-lived bearer token and Claude Code answers through the Gateway, with no key on the device

![Claude Code answering through the Gateway](docs/img/claude-code-answer.png)

### Step 5: Track spend in LiteLLM

The Gateway auto-registers a per-user virtual key from the SSO identity and tracks spend by user and team. Offboarding is removing the identity from the SSO group, after which its tokens stop validating

![auto-registered per-user virtual keys](docs/img/claude-virtual-keys.png)

## Codex onboarding with IdP sign-in

Relay onboards the OpenAI Codex CLI the same way it onboards Claude Code: it writes Codex's own config so `codex` routes through your LiteLLM AI Gateway with the developer's corporate identity, and no provider API key touches the device. Unlike Claude Desktop, Codex is not deployed via MDM here — onboarding is a config writer plus a token command.

Codex reads `~/.codex/config.toml`. Relay defines a custom OpenAI-compatible provider under `[model_providers.<id>]` (pointing `base_url` at the Gateway's `/v1`) and selects it with the top-level `model_provider`/`model` keys. For the credential it uses Codex's command-backed `auth` hook, which runs Relay's token command to fetch a short-lived identity bearer token on demand (Codex refreshes it on the `refresh_interval_ms` interval).

> **Gateway must serve the Responses API.** Codex only supports `wire_api = "responses"` (`wire_api = "chat"` is rejected by the CLI), so the provider talks to the Gateway's `POST /v1/responses`. LiteLLM supports the Responses API — make sure it is enabled for the models you expose.

### Step 1: Enable JWT auth on the Gateway (admin, once)

Same as Claude Code — see [Step 1 above](#step-1-enable-jwt-auth-on-the-gateway-admin-once).

### Step 2: Run `relay onboard-codex` on the device

```bash
relay onboard-codex \
  --gateway-url https://gateway.yourco.com \
  --authorize-url https://login.yourco.com/authorize \
  --team engineering \
  --model gpt-5-codex
```

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
refresh_interval_ms = 300000
```

There is no API key in the file. `relay codex-token` prints a valid IdP bearer token on stdout, which is exactly what Codex's `auth` hook expects.

### Step 3: Start Codex and sign in

The developer runs `codex` with no key and no exports. On first use Relay opens the corporate IdP sign-in in the browser, caches the identity token, and hands Codex a short-lived bearer token for each request. Spend is tracked per-user in LiteLLM, exactly as with Claude Code.

### Credential alternatives

The `auth` hook is the default and keeps no key on the device. Two alternatives are available; Codex treats `auth`, `env_key`, and `experimental_bearer_token` as mutually exclusive, so Relay writes exactly one.

- `--env-key <VAR>`: Codex reads the bearer key from an environment variable (`env_key = "<VAR>"`) rather than invoking the hook. Populate it with the identity token from your shell profile:

  ```bash
  relay onboard-codex --gateway-url https://gateway.yourco.com --env-key LITELLM_API_KEY
  export LITELLM_API_KEY="$(relay codex-token)"
  ```

- `--api-key <KEY>`: for environments without an IdP, embeds a static gateway key as `experimental_bearer_token` on the provider.

  ```bash
  relay onboard-codex --gateway-url https://gateway.yourco.com --api-key sk-...
  ```

The config file is written with `0600` permissions.

## Supported MDMs

Deploy LiteLLM Relay with your existing device-management process:

- Jamf
- Microsoft Intune
- Kandji
- Mosyle
- VMware Workspace ONE
- Addigy
- Custom shell scripts or internal deployment workflows

## Features

- Detect shadow AI usage across employee devices and company traffic sources
- Route AI traffic through LiteLLM AI Gateway for central visibility
- Log AI activity from desktop apps, browser AI, coding tools, agents, MCP
  clients, and LLM APIs
- Apply one set of Gateway controls for audit, access, provider routing, and
  policy

Relay does not log cookies or authorization headers. Payload previews are
truncated and headers are redacted.

## Install

Production deployments should pin the source tag and verify the source archive:

```bash
curl -fsSL https://raw.githubusercontent.com/LiteLLM-Labs/litellm-relay/main/src/install.sh | \
  RELAY_VERSION=v0.1.0 \
  RELAY_SHA256=<release-tarball-sha256> \
  bash
```

Then open a new terminal and run:

```bash
relay
```

For local development from a checked-out repository:

```bash
./src/install.sh --skip-trust-ca
```
