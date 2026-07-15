# Instructions for AI agents: install Relay + the Mac menu bar

This is a runbook for an AI coding agent (Claude Code, Codex, Cursor, …) to install
LiteLLM Relay on a **macOS dev machine** and bring up the **menu bar usage app**
(the ✨🚅 top-bar icon) so the developer can see their own AI spend locally.

Run the steps in order. Each step is idempotent; stop and report if a command fails.

## 0. Preconditions

- macOS 13+ (`sw_vers -productVersion`).
- Rust toolchain for the relay binary: `command -v cargo` (else install from https://rustup.rs).
- Swift toolchain for the menu bar app: `swift --version` (Xcode or Command Line Tools).
- A LiteLLM Gateway URL and a key for it (ask the developer, or use their existing
  `~/.litellm-relay/config.yaml`). The key must be allowed to read `/key/info` and
  `/user/daily/activity` for the usage app to show numbers.

## 1. Install the relay CLI

```bash
git clone https://github.com/LiteLLM-Labs/litellm-relay.git
cd litellm-relay
./src/install.sh
```

This builds the `relay` binary, adds it to `PATH`, and trusts the local CA. Verify:

```bash
relay --help
```

## 2. Point Relay at the Gateway

```bash
relay setup --gateway-url https://<your-gateway> --api-key <sk-...>
```

This writes `~/.litellm-relay/config.yaml` (`gateway.url`, `gateway.api_key`). Confirm:

```bash
relay serve   # starts the local proxy on 127.0.0.1:4142 (leave running)
```

The menu bar app reads the gateway URL + key from this file, so nothing else to wire.

## 3. Build and launch the Mac menu bar app

```bash
cd macos/RelayBarGlass
./build.sh
open RelayBarGlass.app
```

Result: a **✨🚅** icon appears in the macOS menu bar. Click it to open the frosted-glass
popover — per-tool tabs (Claude Code, Codex CLI, Cursor, …), each showing this-month
spend, spend/day, model mix, cache-hit/success/$-per-req, and the relay-key budget.

`build.sh` produces an ad-hoc-signed `LSUIElement` app (menu-bar only, no Dock icon).

## 4. Verify it shows real data

The app calls the gateway with the key from `config.yaml`:

```bash
KEY=$(awk '/^gateway:/{g=1} g&&/api_key:/{print $2; exit}' ~/.litellm-relay/config.yaml)
G=$(awk '/^gateway:/{g=1} g&&/url:/{print $2; exit}' ~/.litellm-relay/config.yaml)
curl -sS -H "Authorization: Bearer $KEY" "$G/key/info" | grep -o '"spend":[0-9.]*'
```

If `/key/info` returns spend, the app will populate. Two correctness notes the app already
handles, but which matter if you script against the API yourself:

- **Filter by `api_key`** (singular) on `/user/daily/activity?api_key=<keyHash>&…` — without
  it, totals are account-wide, not the key's.
- **UTC day bucketing** — the gateway buckets usage by UTC date; use the latest UTC day so
  recent traffic isn't dropped by a local-date cutoff.

## 5. (Optional) Set a budget

Give the relay key a monthly budget so the app's budget bar is meaningful:

```bash
curl -sS -X POST "$G/key/update" -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"key":"'"$KEY"'","max_budget":2000,"budget_duration":"30d"}'
```

The popover then shows `spend / $2,000 · % used · resets <date>` under the hero spend.

## Troubleshooting

- **No ✨🚅 icon**: the app is `LSUIElement` (no Dock icon). Re-run `open RelayBarGlass.app`;
  check `pgrep -x RelayBarGlass`. On multi-monitor setups it appears on the active display's menu bar.
- **"Killed" on launch**: ad-hoc signature invalidated by copy — re-sign: `codesign --force -s - RelayBarGlass.app`.
- **All tools show $0 / "Not routed"**: that tool isn't authenticating with the relay key.
  CLI tools route via `~/.claude/settings.json` / `~/.codex/config.toml`; desktop apps use
  their own login and won't appear until routed through the relay key.
- **Numbers look account-wide (too high)**: you're not filtering by `api_key` — see step 4.
