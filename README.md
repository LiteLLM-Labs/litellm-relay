# LiteLLM Relay

LiteLLM Relay is a local endpoint agent that helps route and shadow AI app traffic
through LiteLLM Gateway. V0 focuses on macOS manual pilots and MDM-friendly PAC
deployment for Notion Mac app traffic.

## V0 scope

- Starts a local HTTP CONNECT proxy on `127.0.0.1:4142`.
- Serves a local dashboard at `http://127.0.0.1:4142/`.
- Serves a PAC file at `http://127.0.0.1:4142/proxy.pac`.
- Routes known AI domains through Relay when the PAC is installed.
- Logs redacted AI connection metadata to `~/.litellm-relay/relay.log.jsonl`.
- Optionally sends a synthetic shadow event through LiteLLM Gateway for audit correlation.

V0 does **not** decrypt TLS, capture Notion prompts, capture cookies, or rewrite
Notion private APIs. That requires a managed enterprise CA and a Notion-specific
adapter, which is intentionally outside this first OSS cut.

## Manual install

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
curl -I -x http://127.0.0.1:4142 https://www.notion.so
```

Generate a Codex/OpenAI-style intercepted request:

```bash
curl -I -x http://127.0.0.1:4142 https://api.openai.com/v1/models
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
PYTHONPATH=src python3 -m litellm_relay.cli serve
PYTHONPATH=src python3 -m litellm_relay.cli pac
PYTHONPATH=src python3 -m unittest discover -s tests
```

## Docs

- [Notion AI shadowing v0](docs/notion-shadow-v0.md)
- [MDM rollout](docs/mdm.md)
- [Product scope artifact](index.html)
