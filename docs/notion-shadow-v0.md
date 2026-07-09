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
git clone https://github.com/BerriAI/litellm-relay.git
cd litellm-relay

export LITELLM_GATEWAY_URL="https://gateway.example.com"
export LITELLM_GATEWAY_API_KEY="sk-..."
export LITELLM_RELAY_SHADOW_ENABLED=1

./install.sh --set-system-proxy "Wi-Fi"
```

Trigger Notion AI in the Notion Mac app, then inspect:

```bash
tail -f ~/.litellm-relay/relay.log.jsonl
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

## Why this is not MITM

Notion AI uses Notion private APIs over HTTPS. Routing those private requests
through LiteLLM Gateway as real completions requires:

- a managed enterprise CA trusted by the Mac,
- explicit domain allowlisting,
- a Notion private API adapter,
- Notion-compatible response synthesis.

That is intentionally outside v0. V0 establishes the install, routing, logging,
and Gateway shadow-call path first.
