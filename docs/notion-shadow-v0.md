# Notion AI shadowing v0

LiteLLM Relay v0 shadows Notion Mac app traffic at the proxy metadata layer.
It does not decrypt TLS and does not capture Notion prompts, page text, cookies,
workspace IDs, or response bodies.

## What v0 proves

1. A managed Mac can route Notion domains through a local Relay proxy using PAC.
2. Relay can identify Notion destination hosts from `CONNECT` requests.
3. Relay can write redacted JSONL connection events.
4. Relay can optionally send a synthetic shadow event through LiteLLM Gateway so
   Gateway logs prove the event path works end to end.

## Install for a manual pilot

```bash
git clone https://github.com/LiteLLM-Labs/litellm-relay.git
cd litellm-relay

./src/install.sh --set-system-proxy "Wi-Fi" \
  --gateway-url "https://gateway.example.com" \
  --api-key "sk-..."
```

Enable synthetic shadow calls in `~/.litellm-relay/config.yaml` when needed:

```yaml
shadow:
  enabled: true
```

Trigger Notion AI in the Notion Mac app, then inspect:

```bash
tail -f ~/.litellm-relay/relay.log.jsonl
open http://127.0.0.1:4142/
```

Expected redacted event:

```json
{
  "event": "connect",
  "host": "www.notion.so",
  "method": "CONNECT",
  "notion_match": true,
  "port": 443,
  "shadow": {
    "attempted": true,
    "ok": true,
    "status": 200
  }
}
```

You can generate a test row without changing system proxy settings:

```bash
curl -I -x http://127.0.0.1:4142 https://www.notion.so
curl -I -x http://127.0.0.1:4142 https://api.openai.com/v1/models
```

## Why this is not MITM

Notion AI uses Notion private APIs over HTTPS. Routing those private requests
through LiteLLM Gateway as real completions requires:

- a managed enterprise CA trusted by the Mac,
- explicit domain allowlisting,
- a Notion private API adapter,
- Notion-compatible response synthesis.

That is intentionally outside v0. V0 establishes the install, routing, logging,
and Gateway shadow-call path first.
