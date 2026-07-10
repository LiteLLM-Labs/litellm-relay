# LiteLLM Relay

LiteLLM Relay is a proxy that you can install on all your employees machines to start tracking all AI Usage. Install LiteLLM Relay on all devices it detects AI Usage and routes it to LiteLLM AI Gateway. This enables LiteLLM AI Gateway to become your single pane of glass for all AI Usage in your company. 

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
