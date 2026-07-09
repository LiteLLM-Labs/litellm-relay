# LiteLLM Relay

LiteLLM Relay is a local endpoint agent that routes AI app traffic through
LiteLLM Gateway and captures redacted request/response previews on the machine.
V0 focuses on macOS manual pilots and MDM-friendly PAC deployment for Notion Mac
app traffic.

Relay is implemented as a single Rust CLI/runtime. The backend is Rust because
the product sits on the local network path: it needs predictable startup,
low-overhead CONNECT tunneling, explicit TLS handling, and a single distributable
binary for endpoint installs.

## V0 scope

- Starts a local HTTP CONNECT proxy on `127.0.0.1:4142`.
- Serves a local dashboard at `http://127.0.0.1:4142/`.
- Serves a PAC file at `http://127.0.0.1:4142/proxy.pac`.
- Routes known AI domains through Relay when the PAC is installed.
- Generates a local Relay CA and uses it to decrypt configured AI domains.
- Logs redacted AI request/response previews to `~/.litellm-relay/relay.log.jsonl`.
- Optionally sends a synthetic shadow event through LiteLLM Gateway for audit correlation.

V0 does **not** capture cookies or authorization headers. Payload previews are
truncated and headers are redacted. If a specific app uses certificate pinning,
set `LITELLM_RELAY_CAPTURE_PAYLOADS=0` to fall back to metadata-only tunneling
for that pilot.

## CLI setup

Run setup first if you want Relay to send capture metadata to LiteLLM Gateway:

```bash
cargo run -- setup --gateway-url "https://gateway.example.com"
```

The setup command opens the LiteLLM Gateway login/API-key page, asks you to paste
a Relay Gateway key, and writes `~/.litellm-relay/env`. That key is then used for
Relay ingest calls to `/internal/collector/events` and optional synthetic shadow
calls.

## Manual install

The installer builds the Rust binary from source, writes a LaunchAgent, and
trusts the local Relay CA in your login keychain:

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/install.sh | bash
```

To immediately route Notion traffic on a pilot Mac:

```bash
curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/install.sh \
  | bash -s -- --set-system-proxy "Wi-Fi"
```

Open the dashboard:

```bash
open http://127.0.0.1:4142/
```

Generate a test intercepted request:

```bash
curl --cacert ~/.litellm-relay/mitm/litellm-relay-ca.pem \
  -x http://127.0.0.1:4142 https://www.notion.so
```

Generate a Codex/OpenAI-style intercepted request:

```bash
curl --cacert ~/.litellm-relay/mitm/litellm-relay-ca.pem \
  -x http://127.0.0.1:4142 https://api.openai.com/v1/models
```

To enable Gateway shadow calls:

```bash
export LITELLM_GATEWAY_URL="https://gateway.example.com"
export LITELLM_GATEWAY_API_KEY="sk-..."
export LITELLM_RELAY_SHADOW_ENABLED=1

curl -fsSL https://raw.githubusercontent.com/BerriAI/litellm-relay/main/install.sh \
  | bash -s -- --set-system-proxy "Wi-Fi"
```

## Local development

```bash
cargo run -- serve
cargo run -- pac
cargo run -- ca-path
cargo test
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

## Docs

- [Notion AI shadowing v0](docs/notion-shadow-v0.md)
- [MDM rollout](docs/mdm.md)
- [Product scope artifact](index.html)
