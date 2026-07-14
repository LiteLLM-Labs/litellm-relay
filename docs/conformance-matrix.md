# Relay Conformance Matrix

This matrix tracks the minimum gates required before calling LiteLLM Relay production-ready.

| Area | CI status | Current gate | Remaining manual or future gate |
| --- | --- | --- | --- |
| Formatting | Automated | `cargo fmt --all --check` on Ubuntu | None |
| Rust unit tests | Automated | `cargo test` on Ubuntu and macOS | Expand provider/app fixtures as catalog grows |
| Clippy | Automated | `cargo clippy --all-targets -- -D warnings` on Ubuntu and macOS | None |
| macOS deployment basics | Automated | macOS test job plus installer shell syntax/help checks | Signed/notarized package install on managed device |
| Proxy CONNECT/TLS | Automated | `scripts/ci/proxy_smoke.sh` starts Relay and tunnels HTTPS through CONNECT | MITM capture against a controlled HTTPS upstream |
| Streaming and WebSocket handling | Automated | Focused Rust regression tests for SSE and WebSocket upgrade decisions | Browser-level WebSocket and SSE end-to-end screenshots |
| Privacy defaults and redaction | Automated | Unit tests for metadata-only defaults, header redaction, query scrubbing, body suppression | Gateway-side audit of ingested metadata |
| Attribution | Automated | Unit tests for catalog attribution, configured AI domains, unknown process status, Gateway metadata | macOS process identity lookup with bundle ID/PID/signing identity |
| Installer management | Automated | `bash -n`, installer help path, release pinning logic in unit-free shell checks | MDM deployment dry run, rollback, full uninstall on disposable macOS account |
| Security dependencies | Automated | `cargo audit --deny warnings` | Static analysis and secret scanning policy |
| Docker image | Automated | PR CI builds image without push; tag release publishes GHCR image | Runtime support statement for Linux container usage |
| Netskope / enterprise network | Manual | Not covered in CI | Validate PAC/proxy behavior behind Netskope or equivalent enterprise TLS inspection |
| Performance | Manual | Not covered in CI | Throughput/latency benchmark with representative AI traffic |
| Release artifacts | Automated on tags | Release workflow packages macOS tarballs and publishes Docker image | Codesign/notarization and SBOM attachment |

CI is not a substitute for the manual enterprise-network and macOS device-management gates, but it prevents regressions in the core Rust, proxy, privacy, and packaging surfaces before release.
