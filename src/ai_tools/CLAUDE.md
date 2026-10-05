# ai_tools

Onboarding for AI coding tools onto the LiteLLM AI Gateway. Each tool is wired so it authenticates with the developer's corporate identity and routes through the Gateway, with no provider API key on the device.

## Layout

Shared identity concerns live at the top level and are reused by every tool. `idp.rs` runs the OIDC authorization code plus PKCE sign-in against the corporate IdP named by `idp.issuer` and `idp.client_id` (discovery document, loopback redirect, code exchange) and the refresh token grant, returning the ID token and refresh token. `token.rs` caches that session under `~/.litellm-relay/identity-token.json`, renews it silently with the refresh token near expiry, and only opens the browser when no refresh is possible. Neither module knows anything about a specific tool.

`detect.rs` decides which tools are installed on the device (pure `PATH`/filesystem inspection via `DetectContext`), and `autoconfigure.rs` drives detection then calls each detected tool's `onboard`, continuing past any single tool's failure. This is what makes Relay opt-out: installing it wires every recognized tool to the Gateway, no per-tool command.

Each tool gets its own folder holding only that tool's settings writer, for example `claude_cli/` for Claude Code. Codex and others are added as sibling folders without touching the shared modules or Relay's proxy code.

```
ai_tools/
  mod.rs          shared re-exports and module wiring
  detect.rs       which AI tools are installed (shared)
  autoconfigure.rs detect + onboard every installed tool (shared)
  idp.rs          corporate IdP sign-in and refresh, OIDC code + PKCE (shared)
  token.rs        identity session cache and silent refresh (shared)
  claude_cli/     Claude Code settings writer
    mod.rs
```

## Adding a tool

Create a folder named after the tool. Read the identity token through `token::ensure_token` rather than talking to `idp` directly, so caching and refresh stay in one place. Write only the tool's own config (its equivalent of `~/.claude/settings.json`), pointing it at the Gateway with the team header and an `apiKeyHelper`-style hook that prints the token to stdout and nothing else. Wire the new `onboard` and token commands into `src/app.rs`. Then teach `detect.rs` how to recognize the tool (add an `AiTool` variant and its detection evidence) and dispatch it in `autoconfigure.rs` so it is auto-configured on install. Add meaningful tests next to the writer.
