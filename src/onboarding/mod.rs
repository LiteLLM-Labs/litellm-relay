//! Onboarding of coding tools onto the Gateway. `idp` and `token` hold the
//! identity concerns shared across tools; each tool (Claude Code today, Codex
//! next) gets its own settings writer alongside them.

pub mod claude;
pub mod idp;
pub mod token;

pub use claude::{onboard, print_token, OnboardParams};
