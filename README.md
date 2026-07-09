# LiteLLM Relay

local relay for AI apps

LiteLLM Relay runs on a machine and sends supported AI app traffic through your
existing LiteLLM Gateway. Use it to bring desktop apps, browser AI tools, coding
tools, and MCP servers under the same Gateway budgets, logs, and policy.


## Features

- Local proxy on `127.0.0.1:4142`
- PAC file for routing selected AI domains through Relay
- Gateway SSO setup during install
- Local dashboard at `http://127.0.0.1:4142/`
- Redacted request/response previews in `~/.litellm-relay/relay.log.jsonl`
- Optional HTTPS capture with a local Relay CA for approved domains
- Metadata-only mode for apps that use certificate pinning or stricter pilots

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
