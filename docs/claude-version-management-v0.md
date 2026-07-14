# Admin-controlled Claude Code and Codex versions (v0)

Relay already onboards AI coding tools onto the Gateway and signs developers in
through their IdP. This adds the missing half the README promises: Relay
"version-manages Claude Code across every laptop", and the same mechanism
covers Codex. Today the approved version lives nowhere central.
`~/.litellm-relay/config.yaml` is a static per-device file each MDM pushes, and
there is no single control an admin can flip to say "the whole fleet runs
Claude Code X.Y.Z and Codex A.B.C."

This scopes a single, central place for admins to pin the approved Claude Code
and Codex versions (plus their managed settings) and have every device converge
on them, with no per-device MDM edits after the initial rollout. Both tools are
managed from one file; Codex is a sibling of Claude Code, not a separate
system.

## User stories

**Admin (platform / security lead).** As the person accountable for what runs
on managed laptops, I want to define one approved Claude Code version and its
settings in a single file, so that every device runs the exact build I
smoke-tested. When Anthropic ships a new release, I test it, change one value,
and the fleet converges on the next sync. If it regresses, I revert that value
and the fleet rolls back. I never touch the MDM again after the initial
rollout.

**Developer (managed device).** As a developer, I just run `claude` or `codex`.
Relay has already installed and pinned the approved versions and written their
managed settings, so I never install, upgrade, or downgrade either tool myself,
and I never pick a version. If the admin pins a new version, my next
`relay sync` is running it.

## Architecture: a dedicated Relay settings file, served by the Gateway

The approved policy lives in its own YAML file, separate from the LiteLLM proxy
`config.yaml`. The Gateway reads that file and serves it to enrolled devices;
Relay pulls it and reconciles the device. There is no database-backed settings
store and no admin API or UI in v0: the admin manages versions by editing one
file.

Gateway side (litellm):

- The file path comes from `LITELLM_RELAY_SETTINGS_PATH` (default
  `relay_settings.yaml`), and is re-read on every request so an edit takes
  effect on the next sync.
- It is served at `GET /relay/managed-config`, behind the Gateway's existing
  auth, so only enrolled identities receive the policy.
- A missing file returns an empty policy (Relay leaves the device untouched); a
  malformed file returns a clear error rather than a wrong policy.

```yaml
# relay_settings.yaml (its own file, NOT the proxy config.yaml)
claude_code:
  channel: pinned
  version: "2.1.206"
  registry: npm
  package: "@anthropic-ai/claude-code"
  model: claude-sonnet-4-5
  managed_settings: {}   # optional extra Claude Code managed-settings keys
codex:
  channel: pinned
  version: "0.144.2"
  registry: npm
  package: "@openai/codex"
  model: gpt-5-codex
policy_version: 7        # monotonic; bump on change
updated_by: admin@yourco.com
updated_at: "2026-07-14T03:00:00Z"
```

## Relay behavior

- **`relay sync`** (new): fetch the policy from the Gateway and reconcile every
  managed tool (Claude Code and Codex) in one pass: install each pinned version,
  rewrite each tool's managed settings, and print the result. Safe to run
  repeatedly; no work when already converged. Runs on a schedule via the
  existing LaunchAgent.
- **Reconcile** reads the tool's `--version`; if it differs from the pin, it
  installs the exact pinned version from the configured registry (npm in v0),
  then writes that tool's managed settings. A failed install never removes a
  working install; the error is surfaced. Each tool writes only its own
  settings (`claude_cli/` and `codex/`).
- **Enforcement.** Claude Code uses its enterprise managed-settings file
  (highest precedence) for the model and Gateway wiring. Codex uses a managed
  `~/.codex/config.toml` pointing at the Gateway. Both are rewritten on every
  sync, so a local edit is corrected on the next reconcile.

## Scope

**In scope for v0**

- Central `claude_code.version` and `codex.version` exact pins in one dedicated
  YAML file, fetched over the authenticated endpoint.
- `relay sync` reconciles both tools in one pass, idempotent, with a printed
  result per tool.
- Install / upgrade / downgrade from one configured registry (npm to start).
- Managed-settings write per tool for model + Gateway wiring.
- Roll-forward and rollback by changing a pin.

**Out of scope for v0**

- Admin UI or API for editing the policy (v0 is a file the admin edits).
- `latest-approved` channel, auto-promotion windows, staged canary rollouts.
- Tools beyond Claude Code and Codex (same shape, added as siblings later).
- Signed policy documents and a tamper-resistant privileged helper (tracked in
  the release plan as deferred; version pinning here is best-effort at user
  scope for v0).
- Multi-registry fan-out beyond the single configured registry.

## Acceptance criteria

1. With `claude_code.version: 2.1.206` and `codex.version: 0.144.2` in the file
   and a device on different versions, `relay sync` ends with
   `claude --version` == `2.1.206` and `codex --version` == `0.144.2`.
2. Changing a pin and re-syncing moves that tool to the new version; reverting
   moves it back (roll-forward + rollback).
3. A developer editing a managed-settings file to change model or base URL is
   corrected on the next `relay sync`.
4. `relay sync` on an already-converged device performs no install and reports
   in-sync per tool (idempotent).
5. The policy endpoint rejects an unauthenticated request.
6. A registry/install failure leaves the previously working install intact and
   surfaces a clear error.

See [managing AI tool versions](managing-ai-tool-versions.md) for the concrete
admin runbook.
