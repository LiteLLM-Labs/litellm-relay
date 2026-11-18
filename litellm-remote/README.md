# LiteLLM Remote

Native iOS and Android clients for Claude Code and Codex sessions routed through LiteLLM.

The phone connects to a hosted control service. Each Mac connects outbound to that service. Users do not enter a Mac LAN address or open an inbound port. The Mac must stay awake. Inference still runs through the configured gateway and Relay keeps its credential broker and signed-caller checks.

## Layout

```text
src/remote/             Rust service and Mac connector in the existing Relay binary
litellm-remote/
  android/              Kotlin and Jetpack Compose client
  ios/                  SwiftUI client
  tests/                Rust behavior tests run by cargo test
```

There is no additional Node runtime, package.json, Claude SDK package, web frontend, or separate container image. The existing Relay dashboard and container are unchanged.

## Run

Build from the repository root with `cargo build --locked`. Use that binary for the following commands, or install it as `relay`.

On the service host, set `LITELLM_GATEWAY_URL` to your gateway origin and `REMOTE_PUBLIC_URL` to the HTTPS origin where you will expose the control service. Neither has a built-in value.

```sh
relay remote serve --listen 127.0.0.1:4174
```

Terminate HTTPS at your existing ingress and forward WebSocket upgrades for `/agent`. Keep the listener private to that ingress. State defaults to Relay’s `remote/control.sqlite` file. Use `--state` to choose another path. The SQLite state is for hashed mobile and device tokens, account ownership, model IDs, and request deduplication. It does not store conversation transcripts or gateway inference credentials.

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

## Existing Claude sessions

Finish the current turn and all tools on the Mac. Note the original UUID in `/status`, run `/exit`, then hand over that saved conversation.

```sh
relay remote handoff <session-uuid>
```

The phone continues the saved UUID. It does not transfer in-flight tools, process memory, or a simultaneous terminal attachment. New Claude sessions started from the phone do not need handoff. To return to the terminal, stop the turn, stop the connector, then run `claude --resume <session-uuid>`. Keep one writer per conversation. Codex uses its shared control process and needs no handoff.

## Mobile setup

Users configure their own gateway URL. Route the gateway’s `/.well-known/litellm-remote.json` to the service’s discovery endpoint, or enter the service HTTPS address under Advanced in the app. Discovery reports the configured gateway and service origins. The clients reject a service associated with another gateway.

Both mobile apps require an HTTPS gateway URL, including for localhost. A local HTTP gateway needs a TLS frontend with a certificate trusted by the phone. The control service may use HTTP on loopback for development, but that does not relax the gateway requirement. On a physical phone, localhost refers to the phone, so use a reachable hostname. The connector uses the operating system’s trusted certificate roots for WSS and does not fall back to insecure TLS.

The gateway must support `/sso/cli/start` and `/sso/cli/poll/{login_id}`. Sign-in uses the system browser. Mobile sessions expire after seven days and are stored in iOS Keychain or encrypted with Android Keystore. Signing out revokes the mobile token. Devices and operations are scoped to the authenticated account.

If the gateway or service is private, the phone needs the organization’s approved VPN or WARP configuration too. Installing this app does not enroll a phone in that network.

Open `litellm-remote/ios/LiteLLMRemote.xcodeproj` in Xcode and select your signing team. There are no Swift package dependencies. The Android project opens in Android Studio and uses its pinned, checksum-verified Gradle wrapper with JDK 21 and Android SDK 36. Neither app has been submitted to an app store.

## Behavior and limits

Both agents support history, new sessions, messages, incremental public assistant text, tool activity, one-time approvals, model selection above the composer, and stopping the current turn. Hidden reasoning and raw tool results are omitted. Common credential patterns are redacted. Transcripts remain on the Mac and are read on demand by the app.

The model picker uses the signed-in account’s gateway catalog. Sign in again to refresh access. Changing models or sending another message while a turn is active is refused. New Codex sessions use workspace-write sandboxing and on-request approvals. Resumed Codex sessions preserve their existing permissions. Claude uses its normal settings and default permission mode, without bypass flags.

File approvals require the complete diff. Oversized requests, broad persistent-access grants, and unsupported interactive dialogs must be reviewed on the Mac. Stopping or declining remains possible if Relay changes environments. New input and approval acceptance verify that the agent still routes to the paired gateway, including Relay’s current broker gateway rather than a stale account snapshot.

Mutations carry request IDs and their outcomes are persisted. Uncertain requests are not automatically resent after a disconnect or restart. Refresh history before manually submitting another message if confirmation is lost.

## Checks

```sh
cargo fmt --all --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
xcodebuild -project litellm-remote/ios/LiteLLMRemote.xcodeproj -scheme LiteLLMRemote -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath litellm-remote/.local/DerivedData test
cd litellm-remote/android
./gradlew :app:testDebugUnitTest :app:assembleDebug :app:lintDebug
```

Rust tests exercise persisted ownership and deduplication, gateway SSO and model authorization, outbound WebSocket routing, current Relay environment checks, public-message filtering, shared Codex RPC, and native Claude control messages. Protocol fixtures do not substitute for a live end-to-end check with the installed agents and a real gateway.

The MCP SDK is pinned to the reviewed chrono-optional commit from [upstream PR 1474](https://github.com/modelcontextprotocol/rust-sdk/pull/1474). Default features remain disabled, so chrono is not included. Move back to a published SDK release once that change ships.
