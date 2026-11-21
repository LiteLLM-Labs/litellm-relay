# LiteLLM Remote

Native iOS and Android clients for Claude Code and Codex sessions routed through LiteLLM.

The phone and each paired Mac or always-on Linux host connect outbound to the control service. No inbound port is needed on the coding device. To keep coding when the Mac is off, start new sessions on the Linux host. Inference still runs through the configured gateway.

## Layout

```text
src/remote/             Rust control service and connector in the existing Relay binary
litellm-remote/
  android/              Kotlin and Jetpack Compose client
  ios/                  SwiftUI client
  tests/                Rust behavior tests run by cargo test
```

There is no additional Node runtime, package.json, Claude SDK package, web frontend, or separate container image. The existing Relay dashboard and container are unchanged.

## 1. Deploy the Relay control service

Deploy one control service for the LiteLLM gateway your team uses. The control service handles sign-in, device registration, and session commands. It does not run the agents. The gateway handles SSO and model inference. The Mac or Linux connector runs the agents and opens an outbound WebSocket to the control service.

Before deploying, make sure the gateway serves the Claude and Codex model IDs you intend to use and supports `/sso/cli/start` and `/sso/cli/poll/{login_id}`. Choose a stable HTTPS hostname for the control service. The phones and coding devices must be able to reach that hostname and the gateway. If either is private, arrange approved VPN or private network access first.

Build from the repository root with `cargo build --locked` using Rust 1.89 or newer. Install `target/debug/litellm-relay` as `relay` on the service host, or use an equivalent build for that host's architecture. Run the service as a dedicated account with a writable, persistent state directory. For example, run the following under that account:

```sh
export LITELLM_GATEWAY_URL=https://gateway.example.com
export REMOTE_PUBLIC_URL=https://remote.example.com
install -d -m 0700 "$HOME/.litellm-relay/remote"
relay remote serve --listen 127.0.0.1:4174 \
  --state "$HOME/.litellm-relay/remote/control.sqlite"
```

For an unattended deployment, run that command under your service supervisor with those two environment variables and the same state path on every restart. Terminate HTTPS at your ingress, forward the control service routes, and allow WebSocket upgrades and long-lived connections on `/agent`. Keep port 4174 private to the ingress. The SQLite state holds hashed mobile and device tokens, account ownership, model IDs, and request deduplication. Back it up using a SQLite-aware method. It does not contain conversation transcripts or gateway inference credentials.

On the gateway, forward `/.well-known/litellm-remote.json` to the same path on the control service. This lets both apps find the service from the gateway address. If you cannot configure discovery, users can supply the control service origin under **Advanced**.

```sh
curl -fsS https://remote.example.com/healthz
curl -fsS https://gateway.example.com/.well-known/litellm-remote.json
```

Expect `{"status":"ok"}` from the first command and the intended `gatewayUrl` and `controlUrl` from the second. These checks do not verify SSO, WebSocket forwarding, or agent inference. Complete the rollout checks below before use.

## 2. Connect a Mac (optional)

You can deploy a Linux host without pairing a laptop. Pair a Mac if you also want to use sessions and project files that live on that Mac.

The inference credential broker’s signed-caller check is supported on macOS only. On Linux, configure the CLIs to use the gateway directly with a virtual key, rather than the broker’s loopback inference endpoint. Never bypass the caller check.

On each Mac, install a current Relay build with its inference proxy and credential broker. Onboard Claude Code or Codex through Relay first. Set `LITELLM_REMOTE_URL` to the remote service origin, then run the following commands.

```sh
relay remote pair
relay remote connect
```

Pairing opens gateway SSO and writes an owner-only device credential under Relay’s home directory. Sign in to the same account in the mobile app to see that Mac. Remove a Mac from the app to revoke its device token. Remote control remains opt-in. This version does not add process supervision to RelayBar or the MDM installer.

This companion does not change Relay’s global Codex onboarding defaults. Configure an exact gateway-served model in your managed Relay configuration, or use the existing `relay onboard-codex --model <gateway-model-id>` command when setting up Codex manually.

Codex uses the current user’s shared app-server control socket under `CODEX_HOME`. `remote connect --codex-socket` selects another socket. `--provider` selects the configured Codex provider ID and defaults to `litellm`.

Claude uses the installed native executable and its stream-json control protocol. `--claude-executable` selects a binary. `CLAUDE_CONFIG_DIR` selects its settings and transcript directory. Native CLI settings, tool permissions, and the existing credential helper remain in use. The connector never calls `relay credential` itself or expands the broker allowlist.

For a Relay-managed inference endpoint, inherited gateway keys are removed from the Claude child process. Embedded keys in Claude settings are rejected for that route. For an explicitly configured direct gateway endpoint, existing Claude credentials or an explicitly supplied `LITELLM_API_KEY` can authenticate inference. No credential value belongs in source or command examples.

## 3. Deploy an always-on Linux host for each user

### Provision the VM and install the tools

Provision a separate Linux VM with systemd and persistent storage for each person. Configure it to boot after maintenance and stay running when nobody is logged in. Give the user an unprivileged account and use that account for installation, pairing, and both services. Allow outbound HTTPS to the control service, gateway, and source control. Open no inbound Relay port on the VM. Use your normal restricted administrative access for maintenance.

Install native Claude Code using its supported Linux installer and install Codex using its supported Linux distribution. The Codex release must support `codex app-server daemon start`. Install `git`, `jq`, and the Relay build for the VM's architecture. If installing Relay from source, run these commands from the repository root on the VM under the host account:

```sh
cargo build --locked
install -d -m 0755 "$HOME/.local/bin"
install -m 0755 target/debug/litellm-relay "$HOME/.local/bin/relay"
export PATH="$HOME/.local/bin:$PATH"
command -v relay claude codex git jq
claude --version
codex --version
codex app-server daemon --help
```

The unit examples assume Relay and Codex are installed in `~/.local/bin`. Use the actual absolute executable paths if they differ. Keep the complete Codex installation. Copying the native executable out of its npm package can pass `codex --version` but fail to start the daemon. If Codex was installed with npm, make sure its Node binary is also on the services' explicit `PATH`. Do not rely on an interactive shell or a version manager's shell initialization to set it.

### Configure inference and clone projects

Ask the gateway administrator for a separate LiteLLM virtual key limited to the user's projects and models as appropriate. SSO pairing authorizes remote control, not inference. The virtual key authenticates model calls, not pairing. Put the key in a private `~/.config/litellm-remote/host.env` under the Linux account. Create the file with restricted permissions before adding the key, and do not paste the key into a shell command or commit it.

```sh
install -d -m 0700 "$HOME/.config/litellm-remote" "$HOME/.codex"
touch "$HOME/.config/litellm-remote/host.env"
chmod 0600 "$HOME/.config/litellm-remote/host.env"
```

```ini
LITELLM_API_KEY=<your scoped virtual key>
ANTHROPIC_BASE_URL=https://gateway.example.com
```

For Claude Code, the connector uses `ANTHROPIC_BASE_URL` and passes the virtual key to the Claude child process. Use the same gateway advertised by the control service. If a Claude user or project setting already supplies an endpoint, make sure that setting also points at the paired gateway. Don't reuse a Mac's loopback broker configuration on the VM. In the app, choose a Claude model the gateway serves and the virtual key can use.

For Codex, configure `~/.codex/config.toml` under the same user. Replace the placeholder with the exact model ID exposed by LiteLLM:

```toml
model = "<Codex-capable gateway model ID>"
model_provider = "litellm"

[model_providers.litellm]
name = "LiteLLM"
base_url = "https://gateway.example.com/v1"
wire_api = "responses"
env_key = "LITELLM_API_KEY"
```

Clone only the repos the person needs. Use their own SSH key or approved Git credential helper, not a shared administrator credential, forwarded laptop agent, or an embedded token in the clone URL. Enter the VM's absolute project path in the apps. It is not the Mac's path. Repos, uncommitted files, agent state, and transcripts are not synchronized between machines. Push or fetch Git changes explicitly as needed.

### Pair the Linux connector with gateway SSO

Pair once as the host user, using a name you can recognize in either app. Open the printed sign-in URL on the phone or laptop if the host has no browser. Follow the gateway prompts and use the same gateway SSO account in the mobile apps.

```sh
relay remote pair --url https://remote.example.com --name "Always-on Linux host"
```

Pairing writes the device credential to `~/.litellm-relay/remote/device.json` for this OS user. Do not copy it to another VM. Pairing does not start the connector. If replacing an existing device, remove the old entry through the app so its credential is revoked.

### Supervise Codex and Relay with systemd

Run `mkdir -p ~/.config/systemd/user` and put each unit below in its own file. Keep the same `CODEX_HOME`, user, and gateway key configuration for both services. Codex writes a JSON PID file, so the unit extracts the process ID into the plain file systemd expects.

```ini
# litellm-remote-codex.service
[Unit]
Description=Codex app-server for LiteLLM Remote

[Service]
Type=forking
PIDFile=%t/litellm-remote-codex.pid
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
Environment=CODEX_HOME=%h/.codex
EnvironmentFile=%h/.config/litellm-remote/host.env
ExecStart=/bin/sh -ec '"%h/.local/bin/codex" app-server daemon start; jq -er ".pid" "$$CODEX_HOME/app-server-daemon/daemon.pid" > "%t/litellm-remote-codex.pid"'
ExecStop=%h/.local/bin/codex app-server daemon stop
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

```ini
# litellm-remote-connector.service
[Unit]
Description=LiteLLM Remote connector
Wants=litellm-remote-codex.service
After=litellm-remote-codex.service

[Service]
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
Environment=CODEX_HOME=%h/.codex
EnvironmentFile=%h/.config/litellm-remote/host.env
ExecStart=%h/.local/bin/relay remote connect --codex-socket %h/.codex/app-server-control/app-server-control.sock
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

```sh
loginctl enable-linger "$USER"  # Have the machine administrator enable it if required.
systemctl --user daemon-reload
systemctl --user enable --now litellm-remote-codex.service litellm-remote-connector.service
journalctl --user -u litellm-remote-codex -u litellm-remote-connector -f
```

Lingering lets the user service manager start at boot and stay running after logout. It does not keep a powered-off or suspended VM online. The explicit `--codex-socket` leaves Codex startup to systemd. When running the Linux connector without that option, Relay tries `codex app-server daemon start` on demand if the default socket is missing. Do not run a second connector under the same account or share the Codex socket across users.

```sh
loginctl show-user "$USER" -p Linger
systemctl --user is-active litellm-remote-codex.service litellm-remote-connector.service
test -S "$HOME/.codex/app-server-control/app-server-control.sock"
```

Expect `Linger=yes`, two `active` lines, and a successful socket check. Then confirm the host shows online in the apps.

For an unattended boot check, reboot the VM and confirm the host comes online in the app before anyone logs in as the host user. To test logout, close the host user's login session normally or terminate only that session with `loginctl terminate-session <session-id>`. Do not use `loginctl terminate-user` as a logout test. It stops the user's service manager and both services even if lingering is enabled. To recover without a login or reboot, an administrator can run `sudo systemctl start "user@$(id -u relayuser).service"`, replacing `relayuser` with the host username, then check that the host is online again.

## Existing Claude sessions

Finish the current turn and all tools on the Mac. Run `/exit`, then open the conversation picker.

```sh
relay remote handoff
```

Choose your conversation by its title, project, and last-updated time. You do not need to find or copy a UUID. The phone continues the original saved conversation. Handoff does not transfer in-flight tools, process memory, or a simultaneous terminal attachment. New Claude sessions started from the phone do not need handoff. To return to the terminal, stop the turn, stop the connector, then run `claude --resume` and choose the conversation. Keep one writer per conversation. Codex uses its shared control process and needs no handoff.

## 4. Set up Android and iOS

Use a build that matches your Relay deployment. Open the Android project in Android Studio with JDK 21 and Android SDK 36, or build with the Gradle wrapper under `litellm-remote/android`. Open `litellm-remote/ios/LiteLLMRemote.xcodeproj` in Xcode and select your signing team. The iOS project has no Swift package dependencies. Neither app has been submitted to an app store.

In either app, enter the gateway HTTPS URL and tap **Continue**. If the gateway does not advertise the service, open **Advanced** and enter its HTTPS origin in **Remote-service address**. Tap **Sign in with LiteLLM**, finish sign-in in the browser, and return to the app. Use the same SSO account that paired the connectors. Both clients reject a control service associated with another gateway.

Both mobile apps require an HTTPS gateway URL, including for localhost. A local HTTP gateway needs a TLS frontend with a certificate trusted by the phone. The control service may use HTTP on loopback for development, but that does not relax the gateway requirement. On a physical phone, localhost refers to the phone, so use a reachable hostname. The connector uses the operating system’s trusted certificate roots for WSS and does not fall back to insecure TLS.

The gateway must support `/sso/cli/start` and `/sso/cli/poll/{login_id}`. Sign-in uses the system browser. Mobile sessions expire after seven days and are stored in iOS Keychain or encrypted with Android Keystore. Signing out revokes the mobile token. Devices and operations are scoped to the authenticated account.

If the gateway or service is private, the phone needs the organization’s approved VPN or WARP configuration too. Installing the app does not enroll a phone in that network. If the account has no devices, run `relay remote pair` on the machine that will run sessions, followed by `relay remote connect` or the Linux user service. Pairing is performed on the coding machine, not in the mobile **Devices** tab.

### Android

In **Sessions**, open the device selector and select the online **Linux** host. Tap **New session**, choose **Claude Code** or **Codex**, verify the destination shown on the form, and enter the absolute path to a project on the host. Start the session and send a message. Use **Devices** to check which connectors are online or remove one.

If the selected Mac is offline, **New session** can use an online Linux host. The form names the host and explains that existing sessions stay on the Mac. There is no fallback for an existing session. The app keeps laptop sessions it has already loaded under that Mac, labels them **Offline**, and disables actions on them. Reconnect the Mac to load history the app hasn't seen.

### iOS

In **Sessions**, open the device menu and select the online Linux host by the name you used to pair it. Tap **New session**, choose **Claude Code** or **Codex**, and enter the absolute path to a project on that host. Use **Devices** to check connector status or remove a device. The existing iOS client reads the platform from the shared device list but only enables **New session** for the selected online device. Select the Linux host manually if the Mac is offline.

Automatic new-session fallback, a visible Linux platform label in the session picker, and retention and offline labeling of previously loaded laptop sessions depend on the separate iOS host update. The intended rule is the same on both platforms: only new sessions may fall back to Linux, and existing sessions stay on the device that started them. The control service does not implement a fallback or migrate any session.

## What happens when you walk away

| Event | Session started on the Mac | Session started on the Linux host |
| --- | --- | --- |
| You leave the Mac awake and connected | It remains available while the connector and agent are running. | It continues on the host. |
| The Mac sleeps, shuts down, or loses its network | You cannot control it while its connector is offline. Sleep and shutdown can interrupt running work. A network loss alone does not move or stop the local agent. Reconnect the Mac to continue from saved state. | It continues independently of the Mac as long as the VM and its services stay online. |
| The phone disconnects, sleeps, or closes the app | An accepted turn continues on the Mac if it stays running. | An accepted turn continues on the host. Reopen the app and refresh the original session to see its progress. |
| The connector restarts on the coding device | A Claude child owned by the connector is interrupted. A Codex thread runs in the separate daemon and may continue if that daemon stays up. | The same distinction applies. Saved Claude conversations can be reopened, but in-flight process state cannot be recovered. Restarting Codex itself is different from restarting only the connector. |

A stopped VM can't continue a turn. Boot it and wait for its connector to come back online before trying to resume saved work. Neither app moves a conversation, files, or process state to another machine. After a disconnect or restart, refresh before retrying a send whose result is unclear. Relay does not automatically resend an uncertain command.

## Operate and troubleshoot a Linux host

Before rollout, sign in from each supported mobile app and confirm the paired host is online. Create a session for each agent and send a short prompt. Confirm the gateway serves both requests. Disconnect and reconnect the phone while a turn runs, then reopen that same session. With a paired Mac, put it offline and confirm its own session is not moved as you create a new session on the host. Check the Android fallback destination and follow the iOS procedure above for the build being tested. Restart just the Linux connector during a test window and confirm the device returns online and saved sessions remain on the host.

For an upgrade, use a maintenance window. Finish active turns, confirm saved work, and back up the VM's project and agent data as appropriate. Update Relay and the CLIs as the host user. If you edit a unit, run `systemctl --user daemon-reload` first. Restart only the units you changed. If you replace Codex or rotate its key, restart both units so both see the new configuration. Verify device status, the socket, and one turn in each agent again. Keep the same device credential when upgrading in place. Re-pair only if the credential or service origin changes.

Use separate VMs, virtual keys, Git credentials, and Codex sockets per person. Keep the host environment file and device credential private. Agent tools run with the host account's access, subject to agent permissions. Do not give that account privileged OS access or unrelated credentials. To retire a host, remove the device in the app, revoke its virtual key and Git credentials, and stop and disable the user services.

| Symptom | What to check |
| --- | --- |
| The app cannot find the service | Query the gateway discovery URL, verify it names the right gateway and control origins, or enter the control origin under **Advanced**. Check HTTPS trust and private network access on the phone. |
| Sign-in works but the host is missing | Check that the host was paired to this control service and the same gateway account. Verify the connector is using the same OS user's pairing file. |
| The host shows offline | Check `systemctl --user status litellm-remote-connector.service`, its journal, and HTTPS access to the control service from the VM. If health works but the connector cannot connect, check WebSocket forwarding on `/agent`. If it only fails after logout or reboot, check `Linger=yes` and that both units are enabled. |
| The user service bus is unavailable | Run `systemctl --user` from a real login under the host account, not a root shell. If needed, ask the administrator to enable lingering for that account. |
| Codex cannot connect | Check the Codex unit's journal, its executable and (for npm) Node on the unit's `PATH`, and that both units name the same socket and `CODEX_HOME`. Check `jq` is installed if the unit cannot produce the systemd PID file. |
| Claude rejects the gateway, or inference is denied | Check the effective Claude endpoint in project settings and the host environment. Ensure Codex uses the `litellm` provider and the gateway's `/v1` base URL. Check the exact model ID, virtual key restrictions, and gateway logs without printing credentials. |
| The app rejects the project folder | Use an existing absolute directory on the selected device and confirm the service account can access it. A Mac path will usually not exist on the Linux host. |

## Behavior and limits

Both agents support history, new sessions, messages, incremental public assistant text, tool activity, one-time approvals, model selection above the composer, and stopping the current turn. Hidden reasoning and raw tool results are omitted. Common credential patterns are redacted. Transcripts remain on the device that runs the session and are read on demand by the app.

On iOS, the latest assistant reply shows a short plain-text preview while a turn is active. The conversation displays the formatted reply available from the connector once the turn ends.

An iPhone on a different network can control sessions while the coding device is still connected to the service. If that device loses the connection, iOS keeps the currently open conversation and draft visible, but pauses session controls until it can refresh the conversation. The coding device runs the agents; it cannot take new turns while it is offline.

The model picker uses the signed-in account’s gateway catalog. Sign in again to refresh access. Changing models or sending another message while a turn is active is refused. New Codex sessions use workspace-write sandboxing and on-request approvals. Resumed Codex sessions preserve their existing permissions, with one exception: a thread whose approval policy is `never` would deny mobile tool requests without showing a card, so sending from the phone raises it to `on-request`, still subject to administrator constraints and the unchanged sandbox. Claude uses its normal settings and default permission mode, without bypass flags.

File approvals require the complete diff. Oversized requests, broad persistent-access grants, and unsupported interactive dialogs must be reviewed on the coding device. Stopping or declining remains possible if Relay changes environments. New input and approval acceptance verify that the agent still routes to the paired gateway, including Relay’s current broker gateway rather than a stale account snapshot.

Mutations carry request IDs and their outcomes are persisted. Uncertain requests are not automatically resent after a disconnect or restart. Refresh history before manually submitting another message if confirmation is lost.

## Checks

```sh
cargo fmt --all --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
xcodebuild -project litellm-remote/ios/LiteLLMRemote.xcodeproj -scheme LiteLLMRemote -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath litellm-remote/.local/DerivedData -parallel-testing-enabled NO CODE_SIGN_IDENTITY=- test
cd litellm-remote/android
./gradlew :app:testDebugUnitTest :app:assembleDebug :app:lintDebug
```

Rust tests exercise persisted ownership and deduplication, gateway SSO and model authorization, outbound WebSocket routing, current Relay environment checks, public-message filtering, shared Codex RPC, and native Claude control messages. Protocol fixtures do not substitute for a live end-to-end check with the installed agents and a real gateway.

The iOS UI regression uses a local service fixture to exercise a long Codex reply, Stop, and a follow-up send without requiring gateway or provider credentials.

The MCP SDK is pinned to the reviewed chrono-optional commit from [upstream PR 1474](https://github.com/modelcontextprotocol/rust-sdk/pull/1474). Default features remain disabled, so chrono is not included. Move back to a published SDK release once that change ships.
