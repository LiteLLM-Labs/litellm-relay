//! Detection of AI coding tools already installed on the device. Relay uses
//! this to auto-configure only the tools a developer actually has, so an admin
//! never has to enumerate tools per machine — install Relay and every AI tool
//! it recognizes is wired to the Gateway. Detection is pure filesystem/`PATH`
//! inspection through [`DetectContext`], so it is host-agnostic and testable.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::system::home_dir;

/// An AI coding tool Relay knows how to onboard onto the Gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiTool {
    /// Claude Code CLI (`~/.claude/settings.json`).
    ClaudeCode,
    /// Claude Desktop app (`/etc/claude-desktop/managed-settings.json`).
    ClaudeDesktop,
    /// Codex — the CLI, the VS Code extension, and the macOS app all read the
    /// same `~/.codex/config.toml`, so one onboard wires every surface.
    Codex,
}

/// Every tool Relay can detect, in the order it reports and configures them.
pub const ALL_TOOLS: [AiTool; 3] = [AiTool::ClaudeCode, AiTool::ClaudeDesktop, AiTool::Codex];

impl AiTool {
    /// Human-readable name used in Relay's output.
    pub fn label(self) -> &'static str {
        match self {
            AiTool::ClaudeCode => "Claude Code CLI",
            AiTool::ClaudeDesktop => "Claude Desktop",
            AiTool::Codex => "Codex (CLI, VS Code, macOS app)",
        }
    }
}

/// Filesystem/`PATH` view used to decide whether a tool is installed. Built
/// from the environment in production and constructed directly in tests so
/// detection never depends on what the host machine happens to have.
#[derive(Debug, Clone)]
pub struct DetectContext {
    pub home: PathBuf,
    pub path_dirs: Vec<PathBuf>,
    pub app_dirs: Vec<PathBuf>,
}

impl DetectContext {
    /// Build the context from the current process environment: the real home
    /// directory, the entries of `PATH`, and the macOS application folders.
    pub fn from_env() -> Self {
        let home = home_dir();
        let path_dirs = env::var_os("PATH")
            .map(|paths| env::split_paths(&paths).collect())
            .unwrap_or_default();
        let app_dirs = vec![PathBuf::from("/Applications"), home.join("Applications")];
        Self {
            home,
            path_dirs,
            app_dirs,
        }
    }

    fn binary_on_path(&self, name: &str) -> Option<PathBuf> {
        self.path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    }

    fn app_bundle(&self, bundle: &str) -> Option<PathBuf> {
        self.app_dirs
            .iter()
            .map(|dir| dir.join(bundle))
            .find(|candidate| candidate.exists())
    }

    fn dir(&self, relative: &str) -> Option<PathBuf> {
        let candidate = self.home.join(relative);
        candidate.is_dir().then_some(candidate)
    }

    /// Look for the OpenAI Codex extension in the VS Code extensions folder.
    /// Extension folders are versioned (`openai.chatgpt-1.2.3`), so match on
    /// the publisher.extension prefix.
    fn vscode_codex_extension(&self) -> Option<PathBuf> {
        let extensions_dir = self.home.join(".vscode").join("extensions");
        let entries = fs::read_dir(&extensions_dir).ok()?;
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("openai.chatgpt")
            {
                return Some(entry.path());
            }
        }
        None
    }
}

/// A tool Relay found on the device, with a short human-readable reason so the
/// operator can see why it was picked up.
#[derive(Debug, Clone)]
pub struct Detection {
    pub tool: AiTool,
    pub evidence: String,
}

fn evidence_at(prefix: &str, path: &Path) -> String {
    format!("{prefix} {}", path.display())
}

/// Detect a single tool, returning why it was recognized or `None` if it is not
/// installed.
pub fn detect(ctx: &DetectContext, tool: AiTool) -> Option<Detection> {
    let evidence = match tool {
        AiTool::ClaudeCode => ctx
            .binary_on_path("claude")
            .map(|path| evidence_at("found `claude` at", &path))
            .or_else(|| ctx.dir(".claude").map(|path| evidence_at("found", &path))),
        AiTool::ClaudeDesktop => ctx
            .app_bundle("Claude.app")
            .map(|path| evidence_at("found", &path))
            .or_else(|| {
                ctx.dir("Library/Application Support/Claude")
                    .map(|path| evidence_at("found", &path))
            }),
        AiTool::Codex => ctx
            .binary_on_path("codex")
            .map(|path| evidence_at("found `codex` at", &path))
            .or_else(|| {
                ctx.app_bundle("Codex.app")
                    .map(|path| evidence_at("found", &path))
            })
            .or_else(|| {
                ctx.vscode_codex_extension()
                    .map(|path| evidence_at("found Codex VS Code extension at", &path))
            })
            .or_else(|| ctx.dir(".codex").map(|path| evidence_at("found", &path))),
    };
    evidence.map(|evidence| Detection { tool, evidence })
}

/// Detect every tool Relay knows about, preserving [`ALL_TOOLS`] order.
pub fn detect_all(ctx: &DetectContext) -> Vec<Detection> {
    ALL_TOOLS
        .iter()
        .filter_map(|&tool| detect(ctx, tool))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_home(home: &Path) -> DetectContext {
        DetectContext {
            home: home.to_path_buf(),
            path_dirs: Vec::new(),
            app_dirs: Vec::new(),
        }
    }

    fn temp_home(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("relay-detect-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn should_detect_nothing_on_an_empty_machine() {
        let home = temp_home("empty");
        let ctx = ctx_with_home(&home);

        assert!(detect_all(&ctx).is_empty());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn should_detect_claude_code_from_config_dir() {
        let home = temp_home("claude");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let ctx = ctx_with_home(&home);

        let detected = detect(&ctx, AiTool::ClaudeCode).expect("claude code detected");
        assert_eq!(detected.tool, AiTool::ClaudeCode);
        assert!(detected.evidence.contains(".claude"));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn should_detect_codex_from_config_dir_and_vscode_extension() {
        let home = temp_home("codex");
        fs::create_dir_all(home.join(".codex")).unwrap();
        let ctx = ctx_with_home(&home);
        assert!(detect(&ctx, AiTool::Codex).is_some());

        let vscode = temp_home("codex-vscode");
        fs::create_dir_all(vscode.join(".vscode/extensions/openai.chatgpt-0.4.9")).unwrap();
        let ctx = ctx_with_home(&vscode);
        let detected = detect(&ctx, AiTool::Codex).expect("codex detected via vscode ext");
        assert!(detected.evidence.contains("VS Code"));

        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&vscode);
    }

    #[test]
    fn should_detect_binary_and_app_bundle() {
        let home = temp_home("bins");
        let bin_dir = home.join("bin");
        let apps_dir = home.join("Applications");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(apps_dir.join("Claude.app")).unwrap();
        fs::write(bin_dir.join("codex"), b"#!/bin/sh\n").unwrap();

        let ctx = DetectContext {
            home: home.clone(),
            path_dirs: vec![bin_dir],
            app_dirs: vec![apps_dir],
        };

        assert!(detect(&ctx, AiTool::Codex).is_some());
        let desktop = detect(&ctx, AiTool::ClaudeDesktop).expect("claude desktop detected");
        assert!(desktop.evidence.contains("Claude.app"));

        let _ = fs::remove_dir_all(&home);
    }
}
