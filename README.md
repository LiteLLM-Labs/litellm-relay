# LiteLLM Relay

Detect shadow AI from every data source in your company.

LiteLLM Relay brings the AI tools your team already uses into LiteLLM AI
Gateway. Desktop apps, browser AI, coding tools, agents, MCP clients, and direct
LLM API calls can all be routed through one gateway instead of living in
separate, untracked places.

Make LiteLLM AI Gateway your single pane of glass for AI usage across the org.
Once Relay is active on employee devices, AI traffic is routed through LiteLLM AI
Gateway, where it can be logged, governed, and audited in one place.

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

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/src/install.sh | bash
```

To install and immediately route Notion traffic on a pilot Mac:

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/src/install.sh \
  | bash -s -- --set-system-proxy "Wi-Fi"
```

Setup asks for your LiteLLM Gateway URL, opens browser SSO, saves the local
Relay credential, and starts the service. For headless installs, set
`LITELLM_GATEWAY_API_KEY` before running the installer.

## Usage

Start Relay:

```bash
litellm-relay
```

Open the local dashboard:

```text
http://127.0.0.1:4142/
```

Test the proxy:

```bash
curl --cacert ~/.litellm-relay/mitm/litellm-relay-ca.pem \
  -x http://127.0.0.1:4142 https://api.openai.com/v1/models
```

## Development

```bash
cd ui && pnpm install && pnpm build
cargo run -- serve
cargo test
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

The dashboard source lives in `ui/`. `pnpm build` writes the embedded static
bundle to `src/static/dashboard/`.

## Docs

- [Notion AI shadowing v0](docs/notion-shadow-v0.md)
- [MDM rollout](docs/mdm.md)
- [Dashboard source](ui/src/App.tsx)
