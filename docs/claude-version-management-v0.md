# Admin-controlled Claude Code versions (v0)

Relay already onboards Claude Code onto the Gateway and signs developers in
through their IdP. This story adds the missing half the README already
promises: Relay "version-manages Claude Code across every laptop." Today the
pinned version lives nowhere. `config.yaml` is a static file each MDM pushes
per device, and there is no single control an admin can flip to say "the whole
fleet runs Claude Code X.Y.Z."

This doc scopes a **global control plane on the Gateway** for admins to pin one
approved Claude Code version (plus its managed settings) and have every device
converge on it, with no per-device MDM edits.

## User stories

**Admin (platform / security lead).** As the person accountable for what runs
on managed laptops, I want to define one approved Claude Code version and its
settings in a single place on the Gateway, so that every device runs the exact
build I smoke-tested. When Anthropic ships a new release, I test it, change one
value, and the fleet converges. If it regresses, I revert that value and the
fleet rolls back. I never touch the MDM again after the initial rollout.

**Developer (managed device).** As a developer, I just run `claude`. Relay has
already installed and pinned the approved version and written its settings, so I
never install, upgrade, or downgrade Claude Code myself, and I never pick a
version. If the admin pins a new version, my next `claude` (or the next Relay
sync) is running it.

**Why now (customer signal).** Affirm and AT&T both asked to manage Claude Code
versions, settings, and sign-in centrally through Relay because Relay is the
thing MDM already puts on every device. A customer on this thread said a paved
mechanism to control client versions "alleviates a lot of my concerns because we
can set up some smoke tests before rollouts." Central version control is the
prerequisite for that smoke-test-then-rollout workflow.

## What v0 proves

1. An admin sets one approved Claude Code version (an exact pin) in a central
   policy on the Gateway, with no per-device MDM change.
2. Relay fetches that policy authenticated with the same identity it already
   uses, so only enrolled devices/users receive it.
3. Relay reconciles the installed Claude Code against the pin: it reports the
   current version, and when they differ it installs the approved version from
   the configured internal registry (npm / Homebrew / JFrog).
4. Relay writes Claude Code enterprise **managed settings** so the pinned model
   and Gateway wiring cannot be overridden locally by the developer.
5. Changing the pin on the Gateway and re-syncing moves a device from version A
   to version B, and reverting the pin moves it back (roll-forward and
   rollback), proving the smoke-test-then-rollout loop.
6. Relay's local dashboard surfaces `approved` vs `installed` version and the
   last sync result, so drift is visible.

## Control plane: managed policy on the Gateway

The Gateway is the control plane; it already holds JWT auth, per-user virtual
keys, and team mapping. Add an admin-owned **managed policy** document served to
enrolled devices:

```
GET {gateway_url}/relay/managed-config
Authorization: Bearer <relay identity token>   # same token Relay already mints
```

```jsonc
{
  "claude_code": {
    "channel": "pinned",           // "pinned" (v0) | "latest-approved" (future)
    "version": "1.2.3",            // exact version the fleet must run
    "registry": "npm",             // npm | homebrew | jfrog (internal mirror)
    "package": "@anthropic-ai/claude-code",
    "model": "claude-sonnet-4-5",  // folded into managed settings
    "managed_settings": { }        // optional extra ~/.claude managed keys
  },
  "policy_version": 7,             // monotonic; lets Relay detect changes cheaply
  "updated_by": "admin@yourco.com",
  "updated_at": "2026-07-14T03:00:00Z"
}
```

Admins set this policy through the Gateway (config / admin API / UI — surfaced
in a follow-up); v0 only requires that the endpoint serves an admin-authored
document. The document is per-team where a team is present, falling back to a
Gateway-wide default, so a team can pilot a new version before the fleet.

## Relay behavior

- **`relay onboard`** additionally fetches the managed policy and performs one
  reconcile before handing off, so a freshly enrolled device lands on the
  approved version.
- **`relay sync`** (new): fetch policy, reconcile version, rewrite managed
  settings, record the result. Runs on a schedule via the existing LaunchAgent
  and is safe to run repeatedly (idempotent — no work when already converged).
- **Reconcile** = read installed `claude --version`; if it differs from the pin,
  install the pinned version from the configured registry; then write managed
  settings. Failures are logged and surfaced; a failed install never removes a
  working install.
- **Enforcement** uses Claude Code's enterprise managed-settings file (highest
  precedence) so the pinned model and Gateway wiring cannot be overridden
  locally. The version pin is enforced by installing the exact version from the
  managed registry.
- **Dashboard** gains a Claude Code row: approved version, installed version,
  in-sync / drift, and last sync time + outcome.

## Scope

**In scope for v0**

- Central `claude_code.version` **exact pin** on the Gateway, fetched over the
  authenticated endpoint.
- `relay sync` + reconcile-on-`onboard`, idempotent, with logging.
- Install/upgrade/downgrade from **one** configured registry (npm to start).
- Enterprise managed-settings write for model + Gateway wiring.
- Dashboard version/drift/last-sync surface.
- Roll-forward and rollback by changing the pin.

**Out of scope for v0 (call out explicitly)**

- Admin UI for editing the policy (v0 authors the document server-side; UI is a
  follow-up).
- `latest-approved` channel / auto-promotion windows / staged canary rollouts.
- Codex and other tools (same shape, added as siblings later).
- Signed policy documents and a tamper-resistant privileged helper (tracked in
  the release plan as deferred; version pinning here is best-effort at user
  scope for v0).
- Multi-registry fan-out beyond the single configured registry.

## Acceptance criteria

1. With `claude_code.version: 1.2.3` in policy and a device on `1.1.0`,
   `relay sync` ends with `claude --version` == `1.2.3` and an in-sync
   dashboard row.
2. Changing policy to `1.4.0` and re-syncing moves the device to `1.4.0`;
   reverting to `1.2.3` moves it back. (roll-forward + rollback)
3. A developer editing `~/.claude/settings.json` to change model or base URL is
   overridden by managed settings on the next `claude` run.
4. `relay sync` on an already-converged device performs no install and reports
   in-sync (idempotent).
5. The policy endpoint rejects an unauthenticated request; only enrolled
   identities receive the document.
6. A registry/install failure leaves the previously working Claude Code intact
   and surfaces a clear drift + error state in the dashboard and logs.

## Demo / video plan (reviewer bar)

The recording must show the full admin→fleet loop end to end:

1. Show the Gateway managed policy pinned to version A; show a device on a
   different version (`claude --version`).
2. Run `relay sync`; show it install version A; show `claude --version` now == A
   and the dashboard row flip to in-sync.
3. Run `claude` and answer a prompt through the Gateway (proves onboarding still
   works on the pinned build) and show the request in LiteLLM logs.
4. Change the pin to version B on the Gateway, re-sync, show the device move to
   B — then revert to A and show rollback. This is the smoke-test-then-rollout
   moment.
5. Attempt a local override of the managed model/base URL and show it does not
   take effect.

Annotate each step as a named check so the reviewer can follow the assertions.
