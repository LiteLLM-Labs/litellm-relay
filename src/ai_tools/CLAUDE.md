# ai_tools

Onboarding for AI coding tools onto the LiteLLM AI Gateway. Each tool is wired so it authenticates with the developer's corporate identity and routes through the Gateway, with no provider API key on the device.

## Layout

Shared identity concerns live at the top level and are reused by every tool. `idp.rs` runs the loopback browser sign-in against the corporate IdP and returns a JWT. `token.rs` caches that JWT under `~/.litellm-relay/` and refreshes it when it is missing or near expiry. Neither module knows anything about a specific tool.

Each tool gets its own folder holding only that tool's settings writer, for example `claude_cli/` for Claude Code. Codex and others are added as sibling folders without touching the shared modules or Relay's proxy code.

```
ai_tools/
  mod.rs          shared re-exports and module wiring
  idp.rs          corporate IdP browser sign-in (shared)
  token.rs        identity token cache and refresh (shared)
  claude_cli/     Claude Code settings writer
    mod.rs
```

## Adding a tool

Create a folder named after the tool. Read the identity token through `token::ensure_token` rather than talking to `idp` directly, so caching and refresh stay in one place. Write only the tool's own config (its equivalent of `~/.claude/settings.json`), pointing it at the Gateway with the team header and an `apiKeyHelper`-style hook that prints the token to stdout and nothing else. Wire the new `onboard` and token commands into `src/app.rs`, and add meaningful tests next to the writer.
