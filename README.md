# LiteLLM Relay

LiteLLM Relay is a proxy you install on employee machines. It detects AI traffic from tools like Notion AI, Perplexity, and OpenClaw, and routes it to your LiteLLM AI Gateway. This makes the Gateway a single pane of glass for all AI usage in your company (including shadow AI).

<img width="2467" height="1080" alt="relay-hero" src="https://github.com/user-attachments/assets/e766224d-014b-4083-b03e-be11abfb0b4a" />


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
