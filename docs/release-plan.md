# LiteLLM Relay Release Plan

## Goal

Ship a formal `v0.1.0` release only after the relay has repeatable CI, macOS packaging artifacts, a documented conformance matrix, and a clear scope for what the first release does and does not guarantee.

## Release scope

- macOS local relay binary for pilot deployment.
- PAC-driven CONNECT proxy path.
- Metadata-first privacy defaults.
- Gateway ingest metadata for AI traffic.
- Explicit protocol handling for buffered HTTP/1.1, SSE-like streams, and WebSocket upgrades.
- Installer support for pinned source archives and optional SHA-256 verification.

## Release artifacts

- `litellm-relay-v0.1.0-x86_64-apple-darwin.tar.gz`
- `litellm-relay-v0.1.0-aarch64-apple-darwin.tar.gz`
- SHA-256 files for each archive.
- GitHub release notes with supported deployment assumptions.
- GHCR Docker image: `ghcr.io/litellm-labs/litellm-relay:v0.1.0`

The Docker image is for CI, demos, and Linux proxy experiments. It is not a replacement for macOS device deployment, because root CA trust, LaunchAgent behavior, MDM configuration, and process attribution are host-specific.

## CI gates before tagging

- Ubuntu and macOS Rust tests pass.
- `cargo fmt --all --check` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- Installer scripts pass shell syntax checks.
- Proxy/TLS smoke test passes.
- Streaming and WebSocket protocol regression tests pass.
- Dependency audit passes.
- Docker image builds.

## Manual gates before tagging

- Verify install and uninstall on a disposable macOS user account.
- Verify PAC behavior on a managed network profile.
- Verify Gateway ingest metadata in a real LiteLLM Gateway.
- Verify one representative SSE flow and one WebSocket flow with screenshots.
- Validate behind Netskope or an equivalent enterprise TLS/proxy stack.
- Record known limitations in the release notes.

## Deferred beyond `v0.1.0`

- Signed and notarized `.pkg`.
- Durable updater and rollback.
- Central signed configuration.
- Durable telemetry queue.
- Tamper-resistant privileged helper.
- Full macOS process attribution with PID, bundle ID, audit token, and signing identity.
- HTTP/2, gRPC, HTTP/3, and QUIC support.
