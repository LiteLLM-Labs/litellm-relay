# LiteLLM Remote v0

A native SwiftUI iOS app for viewing existing Codex sessions through a small API-only Mac bridge. There is no web frontend or embedded website. LiteLLM SSO opens in the system browser for authentication only.

The bridge connects read-only to the existing Codex control socket. It never restarts, resumes, or sends messages to a coding session. It does not change the installed Relay daemon or coding-client configuration.

## Start the Mac bridge

Use Node.js 22.18 or newer and keep Codex running. Run from this directory.

```sh
mkdir -p .local/tmp .local/npm-cache
TMPDIR="$PWD/.local/tmp" npm ci --cache .local/npm-cache --ignore-scripts
export LITELLM_GATEWAY_URL=https://gateway.example.com
export LITELLM_MODEL_PROVIDER=litellm
REMOTE_HOST=0.0.0.0 npm start
```

The bridge listens on port 4174. `REMOTE_PORT` overrides the port. `CODEX_CONTROL_SOCKET` overrides `~/.codex/app-server-control/app-server-control.sock`.

## Run the iOS app

The checked-in Xcode project builds without package dependencies. To regenerate it, use XcodeGen 2.46.0 with `ios/project.yml`.

Open `ios/LiteLLMRemote.xcodeproj`, select the LiteLLMRemote scheme, and run on an iPhone simulator. Enter `http://127.0.0.1:4174` as the bridge address. Complete LiteLLM SSO from localhost once to pair the Mac with your account. A Mac already paired by this prototype keeps its pairing.

If `LITELLM_API_KEY` is already in the bridge's environment, the simulator also offers the Mac's existing gateway connection. The bridge validates that credential against the gateway before issuing a client session. That shortcut is refused for non-local connections. Phone access requires the paired SSO account.

For a physical iPhone, select your signing team in Xcode and connect an unlocked device with Developer Mode enabled. Enter the Mac's Wi-Fi address in the app, including port 4174. Sign in with the same LiteLLM account, enter the code displayed in the native app on the gateway's authorization page, then return to the app. Select a team if the gateway asks.

This v0 uses a trusted local development network. The Mac and iPhone must share Wi-Fi, and the Mac must remain awake. If the gateway requires Cloudflare Zero Trust, enroll the iPhone's Cloudflare One Agent in the same organization. That provides gateway access, not automatic remote access to the Mac. Cellular access, an encrypted off-LAN session transport, TestFlight distribution, and RelayBar integration are not included.

## Data and access

The first SSO pairing must originate from localhost. Only that gateway account can subsequently sign in from another device. Pairing saves the account subject in `.local/owner.json`, not a credential. Gateway credentials and SSO polling secrets remain on the Mac. Client sessions live in memory for at most eight hours. Signing out revokes the client session, and restarting the bridge signs every client out. The iOS app keeps its session cookie in an ephemeral URLSession and stores only the Mac's address in UserDefaults.

Session reads enforce the configured provider, including direct lookups by ID. The latest twelve turns refresh while the app is active. User messages, assistant messages, command names, and changed-file names are displayed. Hidden reasoning and raw tool outputs are omitted. There are no message, command, approval, or filesystem-write APIs.

## Checks

```sh
npm run check
npm test
xcodebuild -project ios/LiteLLMRemote.xcodeproj -scheme LiteLLMRemote -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath .local/DerivedData test CODE_SIGNING_ALLOWED=NO
```

Bridge tests cover account pairing and isolation, client-session expiry, sign-out, native sign-in, cross-origin requests, unauthenticated reads, provider isolation, and live conversation updates over a local WebSocket. Native tests cover connection-address validation and preserving team IDs and pagination cursors. Fixtures, build output, and package caches stay in this project directory.
