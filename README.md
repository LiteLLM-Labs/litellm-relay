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

Then open a new terminal and run:

```bash
relay
```

`relay` opens the interactive setup wizard if this is your first run, sends you
through LiteLLM Gateway SSO, and then starts the live terminal trace view. That
is the normal local flow: install once, type `relay`, finish login, and keep the
terminal open while it traces routed AI traffic.

To install and start Relay as a background service for a pilot Mac:

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/src/install.sh \
  | bash -s -- --background
```

To also route Notion traffic through the background service:

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/src/install.sh \
  | bash -s -- --set-system-proxy "Wi-Fi"
```

For headless or MDM installs, pass setup values as installer flags:

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/src/install.sh \
  | bash -s -- --background \
    --gateway-url https://gateway.example.com \
    --api-key sk-...
```

## Usage

Start Relay:

```bash
relay
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

## Configuration

Relay keeps one local settings file:

```text
~/.litellm-relay/config.yaml
```

Example:

```yaml
relay:
  host: 127.0.0.1
  port: 4142
  log_path: /Users/you/.litellm-relay/relay.log.jsonl
  mitm_ca_dir: /Users/you/.litellm-relay/mitm
gateway:
  url: https://gateway.example.com
  api_key: sk-...
shadow:
  enabled: false
  model: gpt-4o-mini
  min_interval_seconds: 60
capture:
  payloads: true
  payload_preview_bytes: 8192
  payload_body_bytes: 262144
domains:
  notion:
    - notion.so
    - notion.com
  ai:
    - api.openai.com
    - chatgpt.com
    - chat.openai.com
    - codex.openai.com
timeouts:
  request_seconds: 10.0
```

Run `relay setup` to update Gateway auth, or edit this file for local port,
capture, shadow, and domain settings.

## Troubleshooting

If `relay` says port `4142` is already in use, stop the old Python relay and
start again:

```bash
pkill -f 'litellm_relay.cli serve'
relay
```

To run on a different port:

```yaml
relay:
  port: 4143
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
