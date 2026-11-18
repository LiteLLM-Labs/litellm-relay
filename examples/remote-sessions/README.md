# LiteLLM Remote

Native SwiftUI and Kotlin / Jetpack Compose clients for continuing Claude Code and Codex sessions and starting new sessions through a LiteLLM gateway.

The phone connects to a remote-control service deployed alongside your gateway. A connector on each Mac maintains an authenticated outbound WebSocket connection to that service and connects to each tool through its local control interface. There is no Mac HTTP listener, LAN-address setup, port forwarding, web frontend, or embedded website.

Inference runs from the selected agent through the configured LiteLLM gateway. The control service does not proxy inference or store conversation transcripts. The Mac must be awake and connected. This does not provide cloud-hosted coding workers.

## Run the service

Use Node.js 22.18 or newer. Install from this directory. All local build files and caches can stay under `.local`.

```sh
mkdir -p .local/tmp .local/npm-cache
TMPDIR="$PWD/.local/tmp" npm ci --cache .local/npm-cache --ignore-scripts
export LITELLM_GATEWAY_URL=https://your-gateway.example.com
export REMOTE_PUBLIC_URL=https://your-remote-service.example.com
npm start
```

`LITELLM_GATEWAY_URL` is required. There is no built-in gateway default. Each service deployment trusts one administrator-configured gateway, rather than accepting arbitrary sign-in destinations from anonymous clients.

Deploy behind an HTTPS reverse proxy that forwards `/agent` WebSocket upgrades and has an idle timeout longer than 90 seconds. `compose.yaml` provides a single-instance container with persistent state, bound to loopback for the reverse proxy. `/healthz` is the health check. `REMOTE_HOST`, `REMOTE_PORT`, and `REMOTE_STATE_PATH` configure the process. Plain HTTP is accepted only for loopback development addresses in native clients.

The current state store is SQLite for a single service instance. Mount its data directory on persistent storage. It contains device ownership, hashes of device and client credentials, duplicate-request outcomes, and the signed-in account’s model catalog. It contains neither inference keys nor transcripts. Do not run multiple service replicas with separate databases. A shared database and connection routing are required before a multi-instance deployment.

## Connect a Mac

Configure the agents you use for the same gateway. Keep Codex running if you want to continue its existing sessions. Claude Code must be installed and authenticated for gateway inference, for example through Relay’s Claude configuration and API-key helper. Run the connector from this directory.

```sh
export LITELLM_REMOTE_URL=https://your-remote-service.example.com
npm run agent
```

Sign in with LiteLLM SSO and enter the displayed verification code in the gateway authorization page. The connector registers the Mac under the SSO account and saves its device credential in `.local/connector.json` with owner-only permissions. The gateway inference credential returned during SSO is discarded. `npm run agent -- --enroll` starts a new enrollment.

The socket is discovered under `$CODEX_HOME/app-server-control/app-server-control.sock`, or `~/.codex/app-server-control/app-server-control.sock` when `CODEX_HOME` is unset. A managed installation can set `CODEX_CONTROL_SOCKET` for a custom socket, `REMOTE_DEVICE_FILE` for credential storage, and `LITELLM_MODEL_PROVIDER` for another configured provider ID. Phone users never need a socket path.

The connector reconnects with backoff. Removing a device in the app revokes its credential and closes its connection. A device that was revoked must be enrolled again. The connector is currently a standalone process, not yet part of Relay’s Rust daemon or its MDM installer. A managed deployment must supervise it as a user process.

## Hand over an existing Claude session

This is an explicit between-turn handoff, not attachment to an untouched foreground process. On your Mac, let the current turn and its tools finish. Note the session UUID shown by `/status`, then run `/exit`. From this example’s directory, with the same remote-service address and device-credential path used for enrollment, run the following command.

```sh
npm run agent -- --handoff <session-uuid>
```

Keep the normal Mac connector running in another terminal. The handed-over session then appears as ready in the phone app. Relay resumes the original saved session ID through the official Claude Agent SDK. It does not fork the conversation or claim to move running tools, outstanding approvals, or in-memory work. `/bg` is not the handoff command here because Claude’s built-in backgrounding creates a different background session ID.

An active Claude session can appear in the app before handoff, but its transcript and input remain unavailable until you explicitly hand it over. Relay refuses to start another writer when discovery reports the session is still open. New Claude sessions created from the phone are already Relay-managed and need no handoff.

Handoff records are stored next to the enrolled device credential in a device-specific directory, with owner-only permissions. They contain session metadata, not inference credentials or transcripts. `CLAUDE_EXECUTABLE` selects an installed Claude executable when it is not found on `PATH`. If your shell uses a wrapper that rewrites authentication or endpoint variables, select the native Claude executable. `REMOTE_CLAUDE_MODEL` optionally chooses the initial model for new or handed-over Claude sessions. Otherwise the current `opus` alias is used. The connector uses existing Claude credentials or `LITELLM_API_KEY` when explicitly provided. The phone and control service do not receive this key.

To return a Relay-managed Claude session to the terminal, stop its current turn, stop the Mac connector, then run `claude --resume <session-uuid>`. Keep only one process writing that conversation. This v0 does not provide simultaneous terminal and phone attachment for Claude. Codex uses its shared app-server process and does not need this handoff.

## Configure gateway discovery

Users enter their own gateway URL in either app. Route the gateway’s `/.well-known/litellm-remote.json` to the same endpoint on the control service. It returns the administrator-configured `gatewayUrl` and `controlUrl`. The apps verify that the service reports the gateway the user selected.

For installations without discovery, expand Advanced and enter the remote-service HTTPS address. No API key is entered into either native app. Sign-in opens the gateway’s authorization page in the system browser. Phone sessions are saved in iOS Keychain or encrypted with an Android Keystore key. They expire after seven days and are revoked on sign-out.

The sign-in requires the gateway’s `/sso/cli/start` and `/sso/cli/poll/{login_id}` endpoints. Devices and session operations are scoped to the authenticated gateway user. A different account cannot discover or operate another account’s devices.

## Native clients

Open `ios/LiteLLMRemote.xcodeproj` in Xcode and select your signing team. The app has no Swift package dependencies. XcodeGen 2.46.0 regenerates the project from `ios/project.yml`. Use a signed simulator build for Keychain integration tests. Nothing is published to TestFlight or the App Store by building the project.

Open `android` in Android Studio or use its checked-in Gradle wrapper with JDK 21 and Android SDK 36. The Kotlin client includes SSO, device discovery and removal, session creation, conversation history, messaging, Markdown, approvals, and a model dropdown attached above the composer. Its wrapper and distribution are pinned and SHA-256 verified. Build and test with the following command.

```sh
cd android
./gradlew :app:assembleDebug :app:testDebugUnitTest :app:lintDebug
```

The APK is written to `android/app/build/outputs/apk/debug/app-debug.apk`. Debug signing is for development. Google Play and App Store distribution need release signing and their own submission process.

## Session behavior

The active conversation refreshes while the app is open. Codex returns its latest twelve turns and Claude returns its latest 120 visible message blocks. Public assistant deltas are included before a turn finishes saving, so replies appear incrementally. User and assistant messages, command names, and changed-file names are visible. Hidden reasoning and raw tool outputs are omitted, and common key and JWT patterns are redacted.

During SSO, the service reads `/v1/models` with the signed-in gateway credential, caches the model names for that client session, then discards the credential. Codex `model/list` supplies friendly labels, but models outside the signed-in user’s gateway catalog are neither offered nor accepted as explicit selections. Sign in again to refresh the catalog after gateway configuration changes.

The dropdown is attached above the composer. The full catalog is searchable when there are more than twelve choices. Selecting a model applies it through the agent’s supported control API without changing the gateway. Model switching is refused while a reply is active rather than silently ignored. Leaving the selection at the session default preserves its model. A configured model can still be temporarily unavailable at the gateway, in which case the app reports the error without silently falling back.

Codex continuation preserves its permissions. New Codex sessions use workspace-write sandboxing with on-request approvals, and creation and messages check that the project’s provider endpoint still matches the service gateway. Relay’s local inference endpoint is also supported after checking its reported upstream gateway. Command approvals can be answered once in the app. File approvals require the full diff, and requests for continuing access to a folder must be reviewed on the Mac. Unsupported Codex interactive requests fail closed and may need a supported desktop client.

Claude sessions load the project’s normal settings and managed policy and use manual permission handling rather than enabling bypass mode. The configured Claude endpoint is checked before startup, and inference is directed to the connected gateway. Full tool inputs can be reviewed and allowed once or declined. Oversized requests and interactive question or plan-mode dialogs cannot be approved from this v0. Stop the turn and return the session to the Mac for those dialogs. Claude accepts the next phone message after the current turn finishes or is stopped. Codex retains its native active-turn input behavior.

Both agents expose stopping the current turn through the shared API. Stopping work does not delete the conversation. The public API carries opaque namespaced session IDs and common states and capabilities. Tool-specific schemas remain in the Mac connector. There are no generic command-execution, filesystem-write, or arbitrary agent RPC endpoints.

Mutations include a request ID. The service persists their outcomes and does not resend uncertain requests after a disconnect or restart. If confirmation is lost, refresh the conversation before submitting a new message.

## Verification

```sh
npm run check
npm test
xcodebuild -project ios/LiteLLMRemote.xcodeproj -scheme LiteLLMRemote -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath .local/DerivedData test
```

Behavior tests cover Claude handoff consent and persistence, single-writer checks, isolated agent routing, live replies, stopping work, SSO, persisted session expiry and revocation, hashed credentials, account isolation, offline devices, outbound connection routing, duplicate writes, gateway enforcement, first-turn persistence, explicit approvals, model selection, and hidden-output exclusion. Native tests cover URL validation, opaque query values, secure iOS session storage, Android request authentication, message and model encoding, server error propagation, and handoff and approval control availability.
