# ai_tools

Onboarding for AI coding tools onto the LiteLLM AI Gateway. Each tool is wired so it authenticates with the developer's corporate identity and routes through the Gateway, with no provider API key on the device.

## Layout

Shared identity concerns live at the top level and are reused by every tool. `idp.rs` runs the OIDC authorization code plus PKCE sign-in against the corporate IdP named by `idp.issuer` and `idp.client_id` (discovery document, loopback redirect, code exchange) and the refresh token grant, returning the ID token and refresh token. `token.rs` caches that session under `~/.litellm-relay/identity-token.json`, renews it silently with the refresh token near expiry, and only opens the browser when no refresh is possible and the caller allows one. `gateway_credential.rs` registers Relay with the Gateway's authorization server (`/.well-known/litellm-cli-auth`), exchanges the ID token for a Gateway credential (RFC 8693 token exchange at `/token`), caches it per Gateway and team under `~/.litellm-relay/gateway-credentials.json`, and keeps it fresh with its refresh token. Both caches are written through `system::write_private` and locked through `system::lock_private`, which keep them owner-only and owned by the developer even when the root daemon writes them. `blocking.rs` runs one async HTTP exchange to completion from a synchronous command. None of these modules knows anything about a specific tool.

`detect.rs` decides which tools are installed on the device (pure `PATH`/filesystem inspection via `DetectContext`), and `autoconfigure.rs` drives detection then calls each detected tool's `onboard`, continuing past any single tool's failure. This is what makes Relay opt-out: installing it wires every recognized tool to the Gateway, no per-tool command.

`credential.rs` is the client side of the credential broker in `src/broker/`: `relay credential` asks the running daemon for the bearer over `~/.litellm-relay/broker.sock` and prints it, `relay credential --proxy` asks for the proxy token instead (Claude Code and Codex present it to the daemon's `/v1/*` forwarder in `src/inference.rs` at `local_proxy_url`, which swaps it for the Gateway credential, so those two tools never hold the credential), `relay sign-in` and `relay sign-out` drive the daemon's IdP session from a terminal, and `bearer_plan` tells each writer which bearer source to name in the tool's file. On macOS that is always the broker, which checks the asking process chain's code signature, so no key or token lands in a tool's config; elsewhere the writers keep the on-disk `claude-token` and `codex-token` helpers or a static key. The Claude Code writer refuses a `claude` that is a node script (npm releases up to 2.1.110 ran `cli.js` that way) on macOS before it writes anything, since the daemon would refuse every credential call from that chain; later npm releases ship the native binary and pass. The Claude Desktop writer does not ask `bearer_plan` yet and keeps writing the static key or the in-app OIDC settings into the managed file.

`launch_agent.rs` makes sure that daemon is running. A writer that names `relay credential` calls `require_daemon`, which changes nothing when something already answers on the socket and otherwise installs and starts the `ai.litellm.relay` LaunchAgent (or restarts it when the label is already loaded), then waits for the socket and fails the command when nothing answers. The launchd and socket calls sit behind the `DaemonHost` trait, so tests pass `test_support::FakeHost` and never touch launchd.

Each tool gets its own folder holding only that tool's settings writer, for example `claude_cli/` for Claude Code. Codex and others are added as sibling folders without touching the shared modules or Relay's proxy code.

```
ai_tools/
  mod.rs          shared re-exports and module wiring
  detect.rs       which AI tools are installed (shared)
  autoconfigure.rs detect + onboard every installed tool (shared)
  idp.rs          corporate IdP sign-in and refresh, OIDC code + PKCE (shared)
  token.rs        identity session cache and silent refresh (shared)
  gateway_credential.rs  ID token to Gateway credential exchange and cache (shared)
  blocking.rs     one async HTTP exchange run from a synchronous command (shared)
  credential.rs   `relay credential` / `sign-in` / `sign-out` and the per-host bearer plan (shared)
  claude_cli/     Claude Code settings writer
    mod.rs
  codex/          Codex CLI config writer
  claude_desktop/ Claude Desktop managed settings writer
```

## Adding a tool

Create a folder named after the tool. Ask `credential::bearer_plan` which bearer source the host gets, call `launch_agent::require_daemon` when it is the broker, and write only the tool's own config (its equivalent of `~/.claude/settings.json`), pointing it at the Gateway with the team header and an `apiKeyHelper`-style hook that runs `relay credential` on macOS, or elsewhere the legacy token helper through `gateway_credential::print_bearer` (which resolves the identity token through `token::ensure_token` and exchanges it) rather than talking to `token` or `idp` directly, so sign-in, exchange, caching, and refresh stay in one place. Add the tool's signing identifier and Team ID to `broker::caller::default_callers`, or the daemon refuses it (an entry without a team is ignored, since an identifier alone is satisfied by any ad hoc signature). Wire the new `onboard` command into `src/app.rs`. Then teach `detect.rs` how to recognize the tool (add an `AiTool` variant and its detection evidence) and dispatch it in `autoconfigure.rs` so it is auto-configured on install. Add meaningful tests next to the writer.
