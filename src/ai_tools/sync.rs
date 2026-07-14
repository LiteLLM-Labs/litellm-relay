//! `relay sync`: pull the admin-approved policy from the Gateway and reconcile
//! every managed tool on this device. The policy is one document served from a
//! dedicated YAML file on the Gateway; a single sync brings both Claude Code and
//! Codex to their pinned versions and rewrites their managed settings. Each tool
//! reconciles itself; this module only fetches the policy and reports.

use anyhow::Result;

use crate::ai_tools::claude_cli::{self, CLAUDE_TOOL_LABEL};
use crate::ai_tools::codex::{self, CODEX_TOOL_LABEL};
use crate::ai_tools::managed_config::fetch_managed_config;
use crate::ai_tools::version::{describe, ToolReconcile};
use crate::config::load_settings;

pub async fn sync(gateway_url: Option<String>) -> Result<()> {
    let mut settings = load_settings()?;
    if let Some(gateway_url) = gateway_url {
        settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    }
    let api_key = settings.gateway.api_key.clone().unwrap_or_default();

    let managed = fetch_managed_config(&settings.gateway.url, &api_key).await?;

    let claude = claude_cli::reconcile(&managed.claude_code, &settings)?;
    report(CLAUDE_TOOL_LABEL, &claude);

    let codex = codex::reconcile(&managed.codex, &settings)?;
    report(CODEX_TOOL_LABEL, &codex);

    if let Some(policy_version) = managed.policy_version {
        println!("Applied Gateway policy version {policy_version}.");
    }
    Ok(())
}

fn report(tool: &str, outcome: &ToolReconcile) {
    println!("{}", describe(tool, &outcome.state));
    println!(
        "Wrote {tool} managed settings {}",
        outcome.settings_path.display()
    );
}
