# LiteLLM Relay

LiteLLM Relay is a proxy you install on employee machines. It detects AI traffic from tools like Notion AI, Perplexity, and OpenClaw, and routes it to your LiteLLM AI Gateway. This makes the Gateway a single pane of glass for all AI usage in your company (including shadow AI).

<img width="2467" height="1080" alt="relay-hero" src="https://github.com/user-attachments/assets/e766224d-014b-4083-b03e-be11abfb0b4a" />

# Usage 
 1. Install LiteLLM Relay on all your employee devices, using [supported MDM](https://github.com/LiteLLM-Labs/litellm-relay#supported-mdms)
 2. Employees use AI tools as they normally would, such as Notion AI.
    <img width="2200" height="1654" alt="Xnapper-2026-07-09-18 25 01" src="https://github.com/user-attachments/assets/01f59c09-c927-4d04-af37-35ff5b7ec8fb" />
 3. Every request, response, and usage event is captured in LiteLLM.
    <img width="2200" height="1327" alt="Xnapper-2026-07-09-18 47 14" src="https://github.com/user-attachments/assets/dfe69818-ba4d-4874-b386-d3d7a061be39" />

## Claude Code onboarding

Relay also onboards Claude Code onto your LiteLLM AI Gateway with zero manual setup. Employees never receive a provider API key and never export environment variables. Their existing corporate identity authenticates each request, and the Gateway maps that identity to a per-user virtual key with its own budget, model access, and spend tracking.

### v0 flow

1. **Admin (once):** enable JWT auth on the Gateway with `auto_register`, so each SSO identity maps to its own virtual key and limits with no manual key handoff.
2. **Package (Jamf/Intune):** the managed install pulls Claude Code from your internal registry (npm/Homebrew via JFrog) and installs Relay.
3. **Package:** `relay onboard` writes `~/.claude/settings.json`, pointing `ANTHROPIC_BASE_URL` at the Gateway, adding the team header, and wiring an `apiKeyHelper` that supplies the identity token.
4. **Developer:** opens a terminal and runs `claude`. No key, no exports.
5. **Runtime:** Relay signs the developer in through the corporate IdP on first use and hands Claude Code a short-lived bearer token. The Gateway validates it, maps it to the developer's virtual key, enforces budget and limits, logs spend, and forwards upstream.
6. **Offboarding:** remove the identity from the SSO group and its tokens stop validating. No secrets live on the device.

### What the package writes

`relay onboard --gateway-url <gateway> --authorize-url <idp-authorize-url> --team <team>` generates a settings file with no provider key in it:

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

`apiKeyHelper` calls Relay, which returns a cached identity token or triggers a browser sign-in when the token is missing or near expiry. Diagnostics go to stderr so only the token reaches Claude Code on stdout.

### Demo

`relay onboard` wires the settings and prints the next step. Note there is no `ANTHROPIC_API_KEY` on the machine:

![relay onboard writing Claude settings](docs/img/claude-onboard.png)

Starting Claude Code opens the corporate IdP sign-in in the browser. A local mock IdP is shown here; in production this is your Okta, Entra, or Google tenant:

![corporate IdP sign-in](docs/img/claude-idp-signin.png)

After sign-in, Claude Code answers through the Gateway with no key on the device:

![Claude Code answering through the Gateway](docs/img/claude-code-answer.png)

The Gateway auto-registers a per-user virtual key from the SSO identity and tracks spend by user and team:

![auto-registered per-user virtual keys](docs/img/claude-virtual-keys.png)

### Production versus demo IdP

In production, `--authorize-url` points at your corporate IdP's OIDC authorize endpoint. The screenshots above use a local mock IdP for demonstration only; it is not part of a deployment. See [docs/claude-code.md](docs/claude-code.md) for the full onboarding and MDM detail.

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
