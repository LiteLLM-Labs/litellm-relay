//! Onboarding of AI coding tools onto the Gateway. `idp` and `token` hold the
//! identity concerns shared across tools; each tool gets its own folder with a
//! settings writer (`claude_cli` today, Codex next). See `CLAUDE.md`.

pub mod claude_cli;
pub mod claude_desktop;
pub mod idp;
pub mod token;

pub use claude_cli::{onboard, print_token, OnboardParams};
pub use claude_desktop::{onboard_desktop, OnboardDesktopParams};
