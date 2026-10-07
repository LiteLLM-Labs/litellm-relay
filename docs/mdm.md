# MDM rollout

LiteLLM Relay ships to employee Macs as a signed `.pkg` deployed through your
MDM, plus one configuration profile that points macOS Auto Proxy at Relay's
local PAC URL. Relay is macOS-only today.

Endpoints do **not** need Rust/cargo: the `.pkg` carries a prebuilt binary and
its postinstall installs Relay for the console user (CA trust in the login
keychain + a per-user LaunchAgent). See [`scripts/build-macos-pkg.sh`](../scripts/build-macos-pkg.sh).

Recommended shape, same as other endpoint software: manual pilot on one Mac,
then a small MDM pilot group, then broaden.

## Demo

Recorded walkthrough of the Microsoft Intune admin flow — creating and assigning
the PAC configuration profile and the macOS PKG app-add wizard:
[Intune rollout demo video](https://app.devin.ai/attachments/04fa814f-f780-45da-a771-d690a7df1710/intune-relay-rollout-edited.mp4).

## What gets deployed

| Artifact | Purpose | Source |
| --- | --- | --- |
| `litellm-relay-<version>.pkg` | Prebuilt binary + per-user install | Built by `scripts/build-macos-pkg.sh`, attached to the GitHub Release |
| PAC configuration profile | Points macOS Auto Proxy at `http://127.0.0.1:4142/proxy.pac` | [`mdm/litellm-relay-pac.mobileconfig.example`](../mdm/litellm-relay-pac.mobileconfig.example) |
| Managed `config.yaml` | Gateway URL, IdP issuer and client id, capture/shadow settings | [`mdm/config.yaml.example`](../mdm/config.yaml.example) |
| `RelayBarGlass.app` (optional) | Menu bar app: sign-in state, key countdown, team and environment pickers, budget, MCP servers | Built into the `.pkg` by `scripts/build-macos-pkg.sh --relaybar`, installed at `/usr/local/litellm-relay/RelayBarGlass.app` |

The managed config can be baked into the `.pkg` at build time
(`--config-file`) so no separate config delivery is needed:

```bash
scripts/build-macos-pkg.sh --version 0.1.0 --config-file mdm/config.yaml.example
```

## Build the package

On a macOS build/release host (or via the Release workflow):

```bash
scripts/build-macos-pkg.sh \
  --version 0.1.0 \
  --config-file mdm/config.yaml.example \
  --sign "Developer ID Installer: Your Company (TEAMID)"
```

Output: `dist/litellm-relay-0.1.0.pkg` plus a printed SHA-256. Signing is
optional for a Jamf-only fleet but required for Intune (Gatekeeper). Tagging a
release (`v*`) also builds the `.pkg` per architecture in
[`.github/workflows/release.yml`](../.github/workflows/release.yml).

Add `--relaybar` (or set `RELAY_PKG_RELAYBAR=1`) to ship the RelayBar menu
bar app in the same package. The build host needs `swift` on PATH, since the
flag runs `macos/RelayBarGlass/build.sh` and copies the resulting
`RelayBarGlass.app` into the payload at
`/usr/local/litellm-relay/RelayBarGlass.app`; without `swift` the script
prints a one-line skip and builds the package without the app. When the app is
present, `install.sh` writes a second per-user LaunchAgent,
`~/Library/LaunchAgents/ai.litellm.relaybar.plist` (label
`ai.litellm.relaybar`), so the tray starts at login next to the daemon. The
tray reads `relay.port` from the managed `config.yaml` (default 4142) and
polls `http://127.0.0.1:<port>/api/status`; it holds no credential and
persists nothing of its own.

## Manual pilot (one Mac, no MDM)

```bash
curl -fsSL https://raw.githubusercontent.com/LiteLLM-Labs/litellm-relay/main/src/install.sh \
  | RELAY_ALLOW_UNPINNED_MAIN=1 bash -s -- --set-system-proxy "Wi-Fi"
```

Dashboard is served locally at `http://127.0.0.1:4142/`. Verify capture without
touching system settings:

```bash
curl --cacert "$(relay ca-path)" -x http://127.0.0.1:4142 https://www.notion.so
```

## Jamf Pro

1. **Upload the package.** Settings → Computer Management → Packages → New (or
   upload via Jamf Admin). Upload `litellm-relay-<version>.pkg`.
2. **Create the deploy policy.** Computers → Policies → New. Add a **Packages**
   payload with the Relay package, action **Install**. Trigger: Recurring
   check-in (or Enrollment Complete). Scope to your **pilot Smart Group** first.
3. **Deploy the PAC profile.** Computers → Configuration Profiles → New. Add a
   **Proxies** payload → Automatic Proxy Configuration → URL
   `http://127.0.0.1:4142/proxy.pac`. Alternatively upload
   [`mdm/litellm-relay-pac.mobileconfig.example`](../mdm/litellm-relay-pac.mobileconfig.example)
   via Upload. Scope to the same pilot group.
4. **Verify.** On a pilot Mac: `launchctl list | grep ai.litellm.relay`, open
   `http://127.0.0.1:4142/`, and confirm requests appear in your LiteLLM
   Gateway. In Jamf, confirm the policy shows Completed.
5. **Broaden.** Expand the Smart Group scope to the full fleet.

## Microsoft Intune

1. **Wrap and upload the app.** The macOS LOB app type takes a `.intunemac`
   file — wrap the signed `.pkg` with the
   [Intune App Wrapping Tool for macOS](https://github.com/msintuneappsdk/intune-app-wrapping-tool-macos):
   `./IntuneAppUtil -c litellm-relay-<version>.pkg -o .`. Then Apps → macOS →
   Add → **macOS app (PKG)** / line-of-business app, upload the `.intunemac`.
2. **Assign.** Assign the app as **Required** to your pilot Azure AD group.
3. **Deploy the PAC profile.** Devices → Configuration → Create → macOS →
   **Templates → Custom**, upload
   [`mdm/litellm-relay-pac.mobileconfig.example`](../mdm/litellm-relay-pac.mobileconfig.example).
   Assign to the same pilot group.
4. **Verify.** Monitor the app install status per device in Intune, then check
   `http://127.0.0.1:4142/` and the Gateway on a pilot Mac.
5. **Broaden.** Change the assignment to the full device group.

Use Intune trusted-certificate profiles only when testing a future managed-CA
MITM mode; the default install trusts Relay's CA in the user login keychain.

## Kandji

1. Upload the `.pkg` as a **Custom App**, audit-and-enforce or install-once.
2. Add a **Custom Profile** with the PAC payload above.
3. Use a Certificate Library Item only for a future managed-CA MITM test.

## Offboarding / uninstall

Ship `src/uninstall.sh` (also in the package payload at
`/usr/local/litellm-relay/uninstall.sh`) as a script/policy, and unassign the
PAC profile so macOS stops using Auto Proxy:

```bash
/usr/local/litellm-relay/uninstall.sh --unset-system-proxy "Wi-Fi" --remove-data
```

The uninstaller also boots out and removes the RelayBar LaunchAgent
(`~/Library/LaunchAgents/ai.litellm.relaybar.plist`) when the package shipped
the menu bar app.

## Credential broker

On macOS the Relay daemon (`relay serve`, the `ai.litellm.relay` LaunchAgent)
is the only thing on the device that holds a Gateway credential. Claude Code
and Codex are wired with `relay credential --proxy` as their credential helper,
which asks the daemon over the Unix socket `~/.litellm-relay/broker.sock`
(directory 0700, socket 0600) and prints the proxy token it gets back, and both
send their requests to the local inference proxy described below, which swaps
that token for the Gateway credential. Claude Desktop is wired with
`relay credential` over the same socket and gets the Gateway bearer itself,
because the app sends its requests to the Gateway directly; it keeps that
bearer in memory only. The proxy port never serves credentials, and no key,
token, or session is written into Claude Code's settings file, Codex's
`config.toml`, or the Claude Desktop managed file: the developer's IdP session
and the Gateway key live in the daemon's memory and are gone when it stops

Every command that writes the `relay credential` helper into a tool (`relay
onboard`, `relay onboard-codex`, `relay onboard-claude-desktop`, `relay
autoconfigure`, and the setup wizard) makes sure that daemon is running before
it returns. When something already answers on the socket, a foreground `relay
serve` or an agent your MDM loaded, the command changes nothing. Otherwise it
writes `~/Library/LaunchAgents/ai.litellm.relay.plist` (the plist `relay
launch-agent` prints, which `install.sh --background` installs too), bootstraps
it into the user's `gui/<uid>` domain, and waits up to 15 seconds for the socket
to answer. When the label is already loaded but nothing answers within 3
seconds, it restarts that agent with `launchctl kickstart -k` and leaves its
plist alone. A start
that fails prints one line and the command exits non-zero. The tool file it
wrote keeps pointing at the helper, which refuses until a daemon answers, and
Relay never falls back to a key or token on disk

The agent pins `HOME` to the home directory the command ran with, the same way
the Claude Desktop LaunchDaemon does, so the daemon reads the `~/.litellm-relay`
the tool files point at even when the command ran through `sudo` or with a
different `HOME`. The label exists once per login session, so a second Relay
home cannot get its own agent while another one holds `ai.litellm.relay`.
`relay credential` never starts the agent: launchd keeps it alive and starts it
at login, and a daemon someone stopped on purpose stays stopped until an
onboard command (a scheduled auto-configure pass included) or `relay serve` runs. While the agent is loaded or a daemon
answers, a second `relay serve` exits with an error (so a terminal never races
the agent for the socket), and while a daemon answers `relay` prints where the
dashboard is instead of opening the trace view. To stop the agent run `launchctl bootout
gui/$(id -u)/ai.litellm.relay`, and delete the plist to keep it from loading at
the next login. On an `install.sh --background` install the
`ai.litellm.relay.autoconfigure` agent runs those onboard commands every
`RELAY_AUTOCONFIGURE_INTERVAL` seconds (3600 by default), so it brings the
daemon back on its next pass unless you boot it out as well (`launchctl bootout
gui/$(id -u)/ai.litellm.relay.autoconfigure`) and delete its plist

Before answering, the daemon checks who is asking. It reads the connecting
process's uid (it has to match the daemon's), its pid, and its audit token, and
walks the peer and up to four of its ancestors, validating each against the
Apple code signature requirement of an allowed client: the signing identifier
plus the Team ID on the leaf certificate. The defaults are Claude Desktop
(`com.anthropic.claudefordesktop` and its `.helper`, team `Q6L2SF6YDW`), Claude
Code (`com.anthropic.claude-code`, team `Q6L2SF6YDW`), and Codex (`codex`, team
`2DC432GLL2`). A shell, a script, or any other process that runs
`relay credential` gets `caller_refused` and the chain it was refused on, and
the refusal is counted in `/api/status`. `credential.allowed_callers` in
`config.yaml` replaces the defaults (see `mdm/config.yaml.example`), so list
every client you keep, each with its `team_id`: an identifier alone is
satisfied by any ad hoc signature (`codesign -s - -i <identifier>` on any
binary), so the daemon ignores an entry without a team and says so on stderr.
npm releases of Claude Code up to 2.1.110 ran `cli.js` as a script under
`node`, which carries the Node.js Foundation's signature and not Anthropic's,
so the daemon refuses that chain; `relay onboard` refuses to wire a `claude`
that is a node script before writing anything and names the native installer
(`curl -fsSL https://claude.ai/install.sh | bash`). It looks for `claude` on
PATH and then in `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, and
nvm's `versions/node/*/bin` (newest version first), so the autoconfigure agent's bare PATH does not
skip the check. Later npm releases
hard-link Anthropic's native binary into the package, and the daemon accepts
that build like the installer's

With an IdP configured, the first interactive request runs the browser sign-in
from the daemon, exchanges the ID token for a Gateway session credential the
way the older helpers did, and mints a Gateway key scoped to the device's team
(`claude.team`, or `codex.team` when no Claude Code team is set; one key serves
both clients, so give them the same team or expect spend under the Claude Code
one) through `POST /key/generate` with a 60 minute duration and the alias
`relay-<hostname>-<timestamp>`. The daemon extends that key at its half-life
with `POST /key/update`, replaces an expired one on the next request, and
deletes it on `relay sign-out` and when the daemon stops. Minting needs the
team to allow `/key/generate`, `/key/update`, and `/key/delete` for its members
(`POST /team/permissions_update` on the Gateway); while it does not, the daemon
serves the session credential itself and prints the fix once on stderr. A 401
on a mint or an extension means the Gateway no longer accepts the session
credential (its sealing key rotated), so the daemon exchanges the IdP session
again and mints anew, without a browser while the IdP session is still valid.
Without an IdP, the daemon serves `gateway.api_key` from `config.yaml` to the
same allowed clients, so a static-key rollout gets the caller check too. The
daemon re-reads `config.yaml` on the next request or tick after it changes, so
a re-run of `relay autoconfigure` or `relay onboard` needs no daemon restart,
and a changed IdP signs the daemon out while a changed Gateway or team keeps
the IdP session, deletes the key on the Gateway it leaves, and exchanges again
on the next request without a browser. A changed `credential.allowed_callers`
list applies from the next request and leaves the key in place, so a client
taken off the list is refused the next time it runs the helper

Claude Desktop runs the helper with `CLAUDE_HELPER_CONTEXT=background` or
`scheduled-task` when nobody is at the keyboard; those requests never open a
browser and answer `signed_out` until a developer runs the app interactively or
`relay sign-in` from a terminal. `relay sign-in` always starts a fresh browser
sign-in and `relay sign-out` deletes the key and forgets the session.
`relay switch-team <team>` and `relay switch-environment <name>` move the
daemon to another team or Gateway (see below) and print the outcome as one
JSON line: `team`, `environment`, `gateway_url`, `key_expires_at`, and
`source` on stdout when the switch went through, or
`{"refused": "<reason>", "message": "..."}` on stderr with exit code 1 when it
did not. The `broker` block of `/api/status` shows `signed_in`, `user_id`,
`display_name` (from the ID token's `name`, `preferred_username`, or `email`),
`team`, `environment`, `gateway_url`, `key_expires_at`, `key_extended_at`, the
`source` of the last answer (`minted_key`, `session_credential`,
`identity_token`, or `static_key`), and `refused_callers`, never a token

The daemon also keeps an `account` block there. Every 60 seconds it asks
`GET <gateway.url>/health/liveliness` without a credential, and every 300
seconds, plus once after a sign-in and after a switch, it reads
`GET /user/info` and `GET /team/info?team_id=<team>` with the signed-in user's
credential. The block shows `user` (`id`, `email`), `teams` (the `id` and
`alias` of every team the user may attribute spend to, null until the first
read), `teams_error`, `team` (the current team's `id`, `alias`, `spend`,
`max_budget`, and `budget_reset_at` as the Gateway reports them, null when no
team is selected), `budget_error`, `gateway` (`url`, `reachable`, `checked_at`,
`error`), and `polled_at`. When `/user/info` fails or lists nothing, `teams`
falls back to the current team alone and `teams_error` says why; when
`/team/info` refuses the team, the budget comes from the `/user/info` entry and
`budget_error` says why. `relay recheck` runs both reads and the probe now and
prints the block as one JSON line, or `{"refused": "daemon_unavailable",
"message": "..."}` on stderr with exit code 1 when the daemon is not running.
The `environments` block of `/api/status` shows the `current` environment name
and the `available` entries (`name`, `url`) from `config.yaml`. A
`relay switch-team` to a team missing from a known list answers `unknown_team`
with the known ids and changes nothing; with no list the Gateway decides

### Environments and teams

`environments` in `config.yaml` lists the Gateways a developer may switch
between, each with a `name`, a `url`, and an optional `team` that is that
Gateway's default team. The entry whose `url` equals `gateway.url` is the
current one, and every entry shares the `idp` section, so one sign-in serves
them all. The team the daemon mints keys for is `gateway.team` when set, else
the current environment's `team`, else `claude.team`, else `codex.team`. A
switch (`relay switch-team`, `relay switch-environment`, the MCP tools of the
same names, or RelayBar) writes
`gateway.url` and `gateway.team` back into `config.yaml`, where an environment
switch clears `gateway.team` so the new environment's default team applies,
deletes the key on the Gateway it leaves, keeps the IdP session, and exchanges
and mints again on the new Gateway or team without a browser. A switch the new
Gateway refuses writes the previous values back and answers `switch_failed`
with the Gateway's reason; a name missing from the list answers
`unknown_environment` with the configured names. Without an `environments`
list the daemon serves the one `gateway.url`, and `relay switch-team` still
works against it

## Local inference proxy

Claude Code and Codex never receive the Gateway credential at all. On macOS
`relay autoconfigure` points them at the daemon itself, `http://127.0.0.1:4142`
(`ANTHROPIC_BASE_URL` for Claude Code, `base_url` with `/v1` for Codex; the port
follows `relay.port`), and wires their helper as `relay credential --proxy`.
That helper goes through the same socket and the same caller check as
`relay credential`, signs in the same way, and prints a proxy token: a random
value that only the running daemon accepts and the Gateway has never seen. The
daemon forwards every request under `/v1/` to `gateway.url` with the same path
and query, replaces the proxy token with the in-memory Gateway credential in
whichever of `Authorization` and `x-api-key` the client sent, and streams the
answer back as it arrives, so requests show on the Gateway under the signed-in
user exactly as they do with `relay credential`

A request with no proxy token, a wrong one, or two headers that disagree gets a
401 before anything is sent to the Gateway, and so does every request after
`relay sign-out`, a daemon restart, or a change to `credential.allowed_callers`,
since each of those ends the token. Claude Code and Codex answer a 401 by
running their helper again, which goes back through the caller check and signs
in if needed. The
proxy never opens a browser on its own. Connections that do not come from the
device itself get a 403 even when `relay.host` is not a loopback address, and
paths outside `/v1/` are not forwarded. A Gateway that cannot be reached answers
502 from the daemon; every Gateway answer, errors included, passes through
unchanged

Claude Desktop keeps talking to the Gateway directly with `relay credential`,
because its Cowork VM cannot reach the Mac's loopback address

Linux hosts have no code-signature check, so there the tools keep the on-disk
`claude-token` and `codex-token` helpers and a static key goes into the tool's
config as before. Those commands and their caches under `~/.litellm-relay`
still exist on macOS too for now and are removed in a follow-up

## MCP relay

Claude Code and Codex reach the Gateway's MCP tools through `relay mcp`, a
stdio MCP server that the onboarding commands register in each client as the
server `litellm`. It holds no credential and no catalog: every tool call opens
one connection to the daemon's second socket, `~/.litellm-relay/mcp.sock`,
which runs the same caller check as `broker.sock` on the asking process chain
(the client is the parent of `relay mcp`), and the daemon does the rest with
the signed-in user's credential against `<gateway.url>/mcp`. Calls therefore
show on the Logs page under that user, and a client that is not an allowed
caller gets `caller_refused` and nothing else

The server exposes six tools. `search_tools(query)` answers from the daemon's
in-memory catalog with at most five tool names from the servers the user
activated, plus the names of inactive servers that have matching tools.
`describe_tool(name)` returns the tool's description, input schema, upstream
annotations, and its verdict. `call_tool(name, arguments)` runs the tool on the
Gateway and returns its result unchanged. `activate_server(server)` makes a
server's tools available for the rest of the signed-in session.
`switch_team(team)` and `switch_environment(environment)` move every client on
the device to another team or Gateway, the same as `relay switch-team` and
`relay switch-environment`: called without an argument they answer the current
selection (`team`, `environment`, `gateway_url`, `teams`, `teams_error`, and
`environments`) and ask nothing, and with one they run only after the user
confirms the switch and answer the switch outcome plus the same selection. A
team switch refetches the catalog at once so team-scoped servers show on the
next call. The catalog is fetched when a session signs in and every five
minutes after that, and dropped on sign-out

Every catalog entry carries a verdict. A tool is allow only when its name says
it reads (it starts with a word such as get, list, read, or search, contains no
write word such as create, delete, or send, and its upstream annotations do not
claim otherwise); everything else is ask. An allow tool runs with no prompt.
An ask tool, every `activate_server`, and every switch with an argument runs
only after the user confirms it in the client: the daemon sends one question
over the connection, `relay mcp` turns it into an MCP elicitation (a request of
its own on a 2025 connection, an `input_required` tool result the client
answers by retrying the call on a 2026-07-28 one), and the client shows its
own dialog. Claude
Code shows a yes/no prompt naming the server, the tool, and the arguments, in
its default and bypass modes alike, and answers cancel when it runs headless
(`claude -p`). Codex shows a True/False dialog, where False and Esc both
refuse, and refuses without a dialog under `--ask-for-approval never` or its
bypass flag. Nothing the model passes as an argument counts as a confirmation.
A call that runs waits at most 300 seconds for the Gateway

Three keys in the managed config tune this. `mcp.allow` lists exact,
case-sensitive Gateway tool names (`<server>-<tool>`) that run with no prompt
even though their names do not show they read; it has no wildcards and never
hides or denies a tool. `mcp.max_concurrent_calls` caps the tool calls in
flight across every client on the device, 16 when the key is missing or 0; a
call beyond the cap waits its turn. `mcp.tool_prefix_separator` is the text
the Gateway puts between a server's name and a tool's own name, `-` when the
key is missing or empty: the daemon groups tools into servers and reads a
tool's own name by splitting its Gateway name at the first occurrence, so set
the key to the Gateway's `MCP_TOOL_PREFIX_SEPARATOR` when that was changed. A
name the separator does not split lands on the server `ungrouped`, where no
tool runs without a prompt unless `mcp.allow` names it

On the broker plan (macOS) the writers register the server in each client and
keep the client's own per-tool prompt out of the way, so the daemon's question
is the one gate. Claude Code gets `mcpServers.litellm` (`type` `stdio`,
`command` the Relay executable, `args` `["mcp"]`) in `~/.claude.json`, which
is created as `{}` when missing, and the six rules `mcp__litellm__search_tools`,
`mcp__litellm__describe_tool`, `mcp__litellm__call_tool`,
`mcp__litellm__activate_server`, `mcp__litellm__switch_team`, and
`mcp__litellm__switch_environment` in `permissions.allow` of Claude Code's user
settings file (`settings.json` under `~/.claude`); other servers and rules are
kept. Codex gets `[mcp_servers.litellm]` with `command`, `args = ["mcp"]`, and
`default_tools_approval_mode = "approve"` in `~/.codex/config.toml`, next to
any other `[mcp_servers.*]` table. Both files stay owner-only. Off the broker
plan nothing about MCP is written, and Claude Desktop is left alone in this
step

## Notes

macOS has a single Global HTTP Proxy payload per device. Customers already using
a corporate proxy need a coordinated PAC file rather than a second competing
profile.

Using `--api-key` (or `gateway.api_key` in the managed config) writes a static
Gateway key to every device. With an IdP onboarded, Relay exchanges each
developer's sign-in for their own Gateway credential instead, so the flag is
only needed for Gateways without the authorization server. A push has no
terminal, so it never opens a browser: it uses the identity the developer
already signed in with, renewing it silently with its refresh token, and keeps
a saved Gateway key when there is none. Prefer per-user browser SSO where your
Gateway supports it. The credential check also runs when `--api-key` is
combined with the `--oidc-*` flags, since Codex and Claude Code prefer the
explicit key over the IdP.

The `--oidc-*` flags on `relay autoconfigure` set the IdP for Claude Code,
Codex, and Claude Desktop alike, and Relay saves it under `idp:` in
`config.yaml`. From then on every `autoconfigure` run without `--api-key`, the
LaunchAgent's included, sets Claude Code and Codex up for browser sign-in and
stops writing a saved Gateway key into their config, so a key passed once with
`--api-key` lasts only until the next scheduled run. To put only Claude Desktop
on SSO and keep Claude Code and Codex on a static key, pass the flags to
`relay onboard-claude-desktop` instead

The per-user LaunchAgent re-runs `autoconfigure` at login and on its interval.
Each run checks the stored Gateway credential first. Without an IdP, if the
Gateway rejects it, the run leaves the Codex, Claude Code, and Claude Desktop
configs untouched and exits non-zero, so an expired session shows up in the
LaunchAgent's exit status and in the `credential` block of `/api/status`
instead of being rewritten into the tools every hour. With an IdP saved, a
rejected or unreachable saved key is only a warning on stderr: the run still
exchanges the developer's sign-in for every tool, never writes the refused key,
and exits non-zero only when no tool could be configured. The check also covers
the saved key Claude Desktop falls back to when it is not given OIDC flags, so
an IdP setup cannot copy a rejected key into the desktop app. Claude Desktop
reads its managed file at launch, so a renewed credential reaches an app that
has been running longer than the credential's lifetime only after a restart.

Upgrading a fleet from a Relay that sent the ID token itself: the Gateway now
has to accept the device's `--team` for the signed-in developer, through a
`team_id` claim in the ID token or the developer's team membership on the
Gateway under `fallback_to_db_teams: True`. A team it does not accept stops
the tools with `the gateway refused the token exchange (HTTP 400
invalid_request: subject_token was rejected by the gateway's JWT auth)` where
the older Relay's ID token was accepted and the team ignored, so add the
developers to their teams on the Gateway before the rollout, or push a
`--team` the Gateway accepts. `/.well-known/litellm-cli-auth`, `/register`,
and `/token` are reached without a bearer, so let them through any proxy or
WAF in front of the Gateway unauthenticated; while they are blocked, Claude
Code and Codex send the ID token itself with a notice on stderr, as the older
Relay did, and Claude Desktop keeps its saved key or reports the failure (see
[claude-code.md](claude-code.md#gateway-configuration)). `/api/status` reflects the credential
currently saved in `config.yaml`, so re-running `litellm-relay setup` updates
the dashboard without restarting Relay
