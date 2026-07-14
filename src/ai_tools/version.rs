//! Reconciles an installed AI CLI against its admin-approved version. Reading
//! the installed version and installing the pinned one are separated so `sync`
//! can decide whether any work is needed and stay idempotent. The logic is
//! tool-agnostic: it drives any CLI that reports `--version` and installs from a
//! package registry.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Outcome of comparing an installed CLI to its approved version.
#[derive(Debug, PartialEq, Eq)]
pub enum VersionState {
    /// No version is pinned; Relay leaves the installed CLI untouched.
    Unmanaged,
    /// Installed version already matches the pin.
    InSync { version: String },
    /// Installed version (if any) differed from the pin and was reinstalled.
    Reconciled { from: Option<String>, to: String },
}

/// A tool's reconcile result: the version outcome plus where its managed
/// settings were written.
#[derive(Debug)]
pub struct ToolReconcile {
    pub state: VersionState,
    pub settings_path: PathBuf,
}

/// Returns the installed version of `binary`, or `None` when it is absent or
/// does not report a parseable version.
pub fn installed_version(binary: &str) -> Option<String> {
    let output = Command::new(binary).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_version(&stdout)
}

/// Extracts the first semver-looking token from `--version` output such as
/// `1.2.3 (Claude Code)` or `codex-cli 0.144.3`.
pub fn parse_version(raw: &str) -> Option<String> {
    raw.split_whitespace()
        .find(|token| {
            let mut parts = token.split('.');
            let has_three = parts.clone().count() >= 3;
            has_three
                && parts.all(|part| {
                    !part.is_empty() && part.chars().next().is_some_and(|c| c.is_ascii_digit())
                })
        })
        .map(str::to_string)
}

/// Reconciles the installed version of `binary` to `target`. Returns
/// `Unmanaged` when no version is pinned, `InSync` when it already matches, and
/// `Reconciled` after installing the pinned version. A failed install leaves the
/// previous install intact because the state is only advanced once the install
/// command succeeds.
pub fn reconcile(
    binary: &str,
    registry: &str,
    package: &str,
    target: Option<&str>,
) -> Result<VersionState> {
    let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(VersionState::Unmanaged);
    };

    let current = installed_version(binary);
    if current.as_deref() == Some(target) {
        return Ok(VersionState::InSync {
            version: target.to_string(),
        });
    }

    install_version(registry, package, target)?;
    Ok(VersionState::Reconciled {
        from: current,
        to: target.to_string(),
    })
}

/// Installs an exact version from the configured registry. Only npm is
/// supported in v0; other registries are rejected rather than silently skipped.
pub fn install_version(registry: &str, package: &str, version: &str) -> Result<()> {
    match registry {
        "npm" => install_via_npm(package, version),
        other => bail!("unsupported registry '{other}'; v0 supports 'npm'"),
    }
}

fn install_via_npm(package: &str, version: &str) -> Result<()> {
    let spec = format!("{package}@{version}");
    let status = Command::new("npm")
        .args(["install", "--global", &spec])
        .status()
        .with_context(|| format!("failed to run `npm install --global {spec}`"))?;
    if !status.success() {
        bail!("`npm install --global {spec}` failed with status {status}");
    }
    Ok(())
}

/// One-line, tool-labelled description of a reconcile outcome for the sync log.
pub fn describe(tool: &str, state: &VersionState) -> String {
    match state {
        VersionState::Unmanaged => {
            format!("No {tool} version pinned by the Gateway; leaving install untouched.")
        }
        VersionState::InSync { version } => {
            format!("{tool} {version} matches the approved version; nothing to do.")
        }
        VersionState::Reconciled { from, to } => {
            let from = from.as_deref().unwrap_or("none");
            format!("{tool} reconciled from {from} to approved version {to}.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_version_from_claude_output() {
        assert_eq!(parse_version("1.2.3 (Claude Code)"), Some("1.2.3".into()));
    }

    #[test]
    fn should_parse_codex_cli_output() {
        assert_eq!(parse_version("codex-cli 0.144.3"), Some("0.144.3".into()));
    }

    #[test]
    fn should_parse_bare_version() {
        assert_eq!(parse_version("2.10.0\n"), Some("2.10.0".into()));
    }

    #[test]
    fn should_ignore_non_version_text() {
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn should_reject_non_npm_registry() {
        let err = install_version("homebrew", "pkg", "1.0.0").unwrap_err();
        assert!(err.to_string().contains("unsupported registry"));
    }

    #[test]
    fn should_treat_missing_target_as_unmanaged() {
        assert_eq!(
            reconcile("claude", "npm", "pkg", None).unwrap(),
            VersionState::Unmanaged
        );
    }

    #[test]
    fn should_treat_blank_target_as_unmanaged() {
        assert_eq!(
            reconcile("claude", "npm", "pkg", Some("  ")).unwrap(),
            VersionState::Unmanaged
        );
    }
}
