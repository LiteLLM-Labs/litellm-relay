//! Onboarding of AI coding tools onto the Gateway. `idp` and `token` hold the
//! identity concerns shared across tools and `gateway_credential` turns that
//! identity into the Gateway credential the tools send; each tool gets its own
//! folder with a settings writer. See `CLAUDE.md`.

pub mod autoconfigure;
pub mod blocking;
pub mod claude_cli;
pub mod claude_desktop;
pub mod codex;
pub mod credential;
pub mod detect;
pub mod gateway_credential;
pub mod idp;
pub mod token;

pub use autoconfigure::{autoconfigure, AutoConfigureParams};
pub use claude_cli::{onboard, print_token, OnboardParams};
pub use claude_desktop::{onboard_desktop, OnboardDesktopParams};
pub use codex::{onboard as onboard_codex, print_token as print_codex_token, CodexOnboardParams};
