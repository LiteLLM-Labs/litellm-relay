# LiteLLM Relay

LiteLLM Relay is a proxy you install on employee machines. It does two things.

First, it manages your AI coding tools. Relay installs and version-manages Claude Code and Codex across every laptop through your MDM, writes their settings, and lets developers sign in with their corporate identity through the LiteLLM AI Gateway, so nobody handles a provider API key.

Second, it captures shadow AI. Relay detects AI traffic from tools like Notion AI, Perplexity, and OpenClaw and routes it to the Gateway, making it a single pane of glass for all AI usage in your company.

<img width="2467" height="1080" alt="relay-hero" src="https://github.com/user-attachments/assets/e766224d-014b-4083-b03e-be11abfb0b4a" />

# Usage 
 1. Install LiteLLM Relay on all your employee devices, using [supported MDM](https://github.com/LiteLLM-Labs/litellm-relay#supported-mdms)
 2. Employees use AI tools as they normally would, such as Notion AI.
    <img width="2200" height="1654" alt="Xnapper-2026-07-09-18 25 01" src="https://github.com/user-attachments/assets/01f59c09-c927-4d04-af37-35ff5b7ec8fb" />
 3. Every request, response, and usage event is captured in LiteLLM.
    <img width="2200" height="1327" alt="Xnapper-2026-07-09-18 47 14" src="https://github.com/user-attachments/assets/dfe69818-ba4d-4874-b386-d3d7a061be39" />

## Tools

Relay onboards each AI tool onto the Gateway with no provider key on the device. Pick a tool below for its setup guide with step-by-step screenshots.

| Tool | Setup guide |
| --- | --- |
| Claude Code CLI | [docs/claude-code.md](docs/claude-code.md) |
| Claude Desktop | [docs/claude-desktop.md](docs/claude-desktop.md) |
| Codex | Coming next |

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

Relay has no tagged release yet, so install it from GitHub. The installer
builds Relay from source, so you need [Rust](https://rustup.rs/) (`cargo`)
installed first. Relay currently supports macOS.

Clone the repository and run the installer:

```bash
git clone https://github.com/LiteLLM-Labs/litellm-relay.git
cd litellm-relay
./src/install.sh
```

Or install directly from `main` without cloning:

```bash
curl -fsSL https://raw.githubusercontent.com/LiteLLM-Labs/litellm-relay/main/src/install.sh | \
  RELAY_ALLOW_UNPINNED_MAIN=1 bash
```

The installer builds the `relay` command, adds it to your `PATH`, and trusts
the local Relay CA so AI app payloads can be captured. Pass `--skip-trust-ca`
to install without trusting the CA.

Then open a new terminal and run:

```bash
relay
```

Once tagged releases are published, production deployments will be able to pin
a version tag and verify the source archive checksum:

```bash
curl -fsSL https://raw.githubusercontent.com/LiteLLM-Labs/litellm-relay/main/src/install.sh | \
  RELAY_VERSION=vX.Y.Z \
  RELAY_SHA256=<release-tarball-sha256> \
  bash
```
