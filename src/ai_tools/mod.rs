//! Onboarding of AI coding tools onto the Gateway. `idp` and `token` hold the
//! identity concerns shared across tools; `version` reconciles any CLI to its
//! pinned version; each tool gets its own folder with a settings writer
//! (`claude_cli`, `codex`). See `CLAUDE.md`.

pub mod claude_cli;
pub mod codex;
pub mod idp;
pub mod managed_config;
pub mod sync;
pub mod token;
pub mod version;

pub use claude_cli::{onboard, print_token, OnboardParams};
pub use sync::sync;
