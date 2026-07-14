# Feedback requested: formal LiteLLM Relay release

We are preparing a formal `v0.1.0` release of LiteLLM Relay and want feedback from teams interested in piloting it.

## Proposed scope

- macOS pilot binary with pinned release artifacts.
- Metadata-first capture defaults.
- Gateway ingest metadata for AI traffic.
- CONNECT/TLS smoke coverage in CI.
- Streaming and WebSocket regression coverage.
- Docker image for CI, demos, and Linux proxy experiments.
- Documented conformance matrix and known limitations.

## Not in the first release

- Signed/notarized `.pkg`.
- Central signed config and updater.
- Durable telemetry queue.
- Tamper-resistant helper.
- Full PID, bundle ID, audit-token, and signing-identity attribution.
- HTTP/2, gRPC, HTTP/3, and QUIC support.

## Feedback requested

Please comment with:

- Whether you would pilot a scoped `v0.1.0`.
- Required MDM or security-review gates.
- Whether Docker images are useful for your testing workflow.
- Enterprise-network constraints such as Netskope or other TLS inspection.
- Any release artifact requirements before deployment.

If there is enough interest, we will prioritize the formal release checklist and publish tagged artifacts.
