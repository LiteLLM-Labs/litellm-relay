//! Which processes may read a credential from the broker socket: the peer and
//! up to four of its ancestors are checked against code-signing requirements,
//! so a helper spawned by an allowed client (Claude Code runs it through
//! `sh -c`) is served while an arbitrary shell is refused.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))] // the chain walk runs on macOS only; other hosts answer Verdict::Unsupported

use std::iter::successors;

use serde::{Deserialize, Serialize};

pub const MAX_ANCESTORS: usize = 4;
pub const AUDIT_TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AllowedCaller {
    pub identifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

impl AllowedCaller {
    pub fn new(identifier: &str, team_id: Option<&str>) -> Self {
        Self {
            identifier: identifier.to_string(),
            team_id: team_id.map(str::to_string),
        }
    }

    /// The code-signing requirement the entry compiles to. `None` when a value
    /// cannot be quoted into the requirement language, which only a managed
    /// config can produce.
    pub fn requirement(&self) -> Option<String> {
        if !quotable(&self.identifier) || !self.team_id.as_deref().is_none_or(quotable) {
            return None;
        }
        Some(match &self.team_id {
            Some(team) => format!(
                "identifier \"{}\" and anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
                self.identifier, team
            ),
            None => format!("identifier \"{}\"", self.identifier),
        })
    }

    pub fn describe(&self) -> String {
        match &self.team_id {
            Some(team) => format!("{} ({team})", self.identifier),
            None => format!("{} (any team)", self.identifier),
        }
    }
}

fn quotable(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_control())
}

pub fn default_callers() -> Vec<AllowedCaller> {
    vec![
        AllowedCaller::new("com.anthropic.claudefordesktop", Some("Q6L2SF6YDW")),
        AllowedCaller::new("com.anthropic.claudefordesktop.helper", Some("Q6L2SF6YDW")),
        AllowedCaller::new("com.anthropic.claude-code", Some("Q6L2SF6YDW")),
        AllowedCaller::new("codex", Some("2DC432GLL2")),
    ]
}

/// The managed list replaces the built-in one when present, so an admin can
/// both add a client and drop a default.
pub fn effective_callers(managed: Option<&[AllowedCaller]>) -> Vec<AllowedCaller> {
    match managed {
        Some(callers) => callers.to_vec(),
        None => default_callers(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Pid(u32),
    AuditToken([u8; AUDIT_TOKEN_BYTES]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    pub pid: u32,
    pub audit_token: Option<[u8; AUDIT_TOKEN_BYTES]>,
}

pub trait ProcessTable {
    fn parent(&self, pid: u32) -> Option<u32>;
    fn validates(&self, subject: Subject, requirement: &str) -> bool;
    fn describe(&self, subject: Subject) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed {
        caller: AllowedCaller,
        level: usize,
    },
    Refused {
        chain: Vec<String>,
    },
    #[cfg(not(target_os = "macos"))]
    Unsupported,
}

impl Verdict {
    pub fn refusal_message(&self) -> Option<String> {
        match self {
            Verdict::Allowed { .. } => None,
            Verdict::Refused { chain } => Some(format!(
                "caller refused: no allowed client in the process chain [{}]",
                chain.join(" <- ")
            )),
            #[cfg(not(target_os = "macos"))]
            Verdict::Unsupported => {
                Some("caller refused: parent-process signature checks are macOS only".to_string())
            }
        }
    }
}

pub fn walk(table: &dyn ProcessTable, callers: &[AllowedCaller], peer: Peer) -> Verdict {
    let subjects = chain(table, peer);
    let requirements: Vec<(&AllowedCaller, String)> = callers
        .iter()
        .filter_map(|caller| {
            caller
                .requirement()
                .map(|requirement| (caller, requirement))
        })
        .collect();
    let allowed = subjects.iter().enumerate().find_map(|(level, subject)| {
        requirements
            .iter()
            .find(|(_, requirement)| table.validates(*subject, requirement))
            .map(|(caller, _)| Verdict::Allowed {
                caller: (*caller).clone(),
                level,
            })
    });
    allowed.unwrap_or_else(|| Verdict::Refused {
        chain: subjects
            .iter()
            .map(|subject| table.describe(*subject))
            .collect(),
    })
}

fn chain(table: &dyn ProcessTable, peer: Peer) -> Vec<Subject> {
    let first = match peer.audit_token {
        Some(token) => Subject::AuditToken(token),
        None => Subject::Pid(peer.pid),
    };
    let ancestors = successors(table.parent(peer.pid), |pid| table.parent(*pid))
        .take_while(|pid| *pid > 1)
        .take(MAX_ANCESTORS)
        .map(Subject::Pid);
    std::iter::once(first).chain(ancestors).collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, Clone)]
    pub struct Signed {
        pub identifier: &'static str,
        pub team: Option<&'static str>,
    }

    #[derive(Default)]
    pub struct FakeProcessTable {
        processes: HashMap<u32, (u32, Option<Signed>)>,
        tokens: HashMap<[u8; AUDIT_TOKEN_BYTES], Signed>,
    }

    impl FakeProcessTable {
        pub fn with(
            mut self,
            pid: u32,
            parent: u32,
            identifier: &'static str,
            team: Option<&'static str>,
        ) -> Self {
            self.processes
                .insert(pid, (parent, Some(Signed { identifier, team })));
            self
        }

        pub fn unsigned(mut self, pid: u32, parent: u32) -> Self {
            self.processes.insert(pid, (parent, None));
            self
        }

        pub fn token(
            mut self,
            token: [u8; AUDIT_TOKEN_BYTES],
            identifier: &'static str,
            team: Option<&'static str>,
        ) -> Self {
            self.tokens.insert(token, Signed { identifier, team });
            self
        }

        fn signed(&self, subject: Subject) -> Option<&Signed> {
            match subject {
                Subject::Pid(pid) => self
                    .processes
                    .get(&pid)
                    .and_then(|(_, signed)| signed.as_ref()),
                Subject::AuditToken(token) => self.tokens.get(&token),
            }
        }
    }

    impl ProcessTable for FakeProcessTable {
        fn parent(&self, pid: u32) -> Option<u32> {
            self.processes.get(&pid).map(|(parent, _)| *parent)
        }

        fn validates(&self, subject: Subject, requirement: &str) -> bool {
            self.signed(subject).is_some_and(|signed| {
                AllowedCaller::new(signed.identifier, signed.team)
                    .requirement()
                    .is_some_and(|own| own == requirement)
                    || AllowedCaller::new(signed.identifier, None)
                        .requirement()
                        .is_some_and(|own| own == requirement)
            })
        }

        fn describe(&self, subject: Subject) -> String {
            match self.signed(subject) {
                Some(signed) => AllowedCaller::new(signed.identifier, signed.team).describe(),
                None => "unsigned".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::FakeProcessTable, *};

    const CLAUDE_TEAM: &str = "Q6L2SF6YDW";
    const CODEX_TEAM: &str = "2DC432GLL2";

    fn peer(pid: u32) -> Peer {
        Peer {
            pid,
            audit_token: None,
        }
    }

    fn relay_under_shell_under_claude() -> FakeProcessTable {
        FakeProcessTable::default()
            .with(1, 0, "com.apple.xpc.launchd", Some("apple"))
            .with(100, 1, "com.anthropic.claude-code", Some(CLAUDE_TEAM))
            .with(200, 100, "com.apple.sh", Some("apple"))
            .with(300, 200, "ai.litellm.relay", None)
    }

    #[test]
    fn should_allow_relay_under_sh_under_claude_code() {
        let verdict = walk(
            &relay_under_shell_under_claude(),
            &default_callers(),
            peer(300),
        );
        assert_eq!(
            verdict,
            Verdict::Allowed {
                caller: AllowedCaller::new("com.anthropic.claude-code", Some(CLAUDE_TEAM)),
                level: 2
            }
        );
    }

    #[test]
    fn should_allow_relay_under_codex_under_node() {
        let table = FakeProcessTable::default()
            .with(100, 1, "node", None)
            .with(200, 100, "codex", Some(CODEX_TEAM))
            .with(300, 200, "ai.litellm.relay", None);
        let verdict = walk(&table, &default_callers(), peer(300));
        assert!(
            matches!(verdict, Verdict::Allowed { level: 1, .. }),
            "{verdict:?}"
        );
    }

    #[test]
    fn should_refuse_relay_under_zsh_under_terminal_and_name_the_chain() {
        let table = FakeProcessTable::default()
            .with(100, 1, "com.apple.Terminal", Some("apple"))
            .with(200, 100, "com.apple.zsh", Some("apple"))
            .with(300, 200, "ai.litellm.relay", None);
        let verdict = walk(&table, &default_callers(), peer(300));
        let message = verdict.refusal_message().expect("refused");
        assert!(message.contains("ai.litellm.relay (any team) <- com.apple.zsh (apple) <- com.apple.Terminal (apple)"), "{message}");
    }

    #[test]
    fn should_refuse_an_allowed_client_five_levels_up() {
        let table = FakeProcessTable::default()
            .with(10, 1, "com.anthropic.claude-code", Some(CLAUDE_TEAM))
            .with(20, 10, "a", None)
            .with(30, 20, "b", None)
            .with(40, 30, "c", None)
            .with(50, 40, "d", None)
            .with(60, 50, "ai.litellm.relay", None);
        assert!(matches!(
            walk(&table, &default_callers(), peer(60)),
            Verdict::Refused { .. }
        ));
        let four_deep = FakeProcessTable::default()
            .with(10, 1, "com.anthropic.claude-code", Some(CLAUDE_TEAM))
            .with(20, 10, "a", None)
            .with(30, 20, "b", None)
            .with(40, 30, "c", None)
            .with(60, 40, "ai.litellm.relay", None);
        assert!(matches!(
            walk(&four_deep, &default_callers(), peer(60)),
            Verdict::Allowed { level: 4, .. }
        ));
    }

    #[test]
    fn should_refuse_the_right_identifier_signed_by_the_wrong_team() {
        let table = FakeProcessTable::default()
            .with(100, 1, "com.anthropic.claude-code", Some("ATTACKER01"))
            .with(200, 100, "com.apple.sh", Some("apple"))
            .with(300, 200, "ai.litellm.relay", None);
        assert!(matches!(
            walk(&table, &default_callers(), peer(300)),
            Verdict::Refused { .. }
        ));
    }

    #[test]
    fn should_refuse_an_unsigned_process_with_the_allowed_name() {
        let table =
            FakeProcessTable::default()
                .unsigned(100, 1)
                .with(300, 100, "ai.litellm.relay", None);
        let verdict = walk(&table, &default_callers(), peer(300));
        assert!(verdict
            .refusal_message()
            .expect("refused")
            .contains("<- unsigned"));
    }

    #[test]
    fn should_check_the_peer_by_audit_token_rather_than_pid() {
        let token = [7u8; AUDIT_TOKEN_BYTES];
        let table = FakeProcessTable::default()
            .with(300, 1, "com.anthropic.claude-code", Some(CLAUDE_TEAM))
            .token(token, "com.apple.zsh", Some("apple"));
        let verdict = walk(
            &table,
            &default_callers(),
            Peer {
                pid: 300,
                audit_token: Some(token),
            },
        );
        assert!(matches!(verdict, Verdict::Refused { .. }), "{verdict:?}");
    }

    #[test]
    fn should_stop_walking_at_launchd() {
        let table = FakeProcessTable::default()
            .with(1, 0, "com.anthropic.claude-code", Some(CLAUDE_TEAM))
            .with(300, 1, "ai.litellm.relay", None);
        assert!(matches!(
            walk(&table, &default_callers(), peer(300)),
            Verdict::Refused { .. }
        ));
    }

    #[test]
    fn should_let_a_managed_list_replace_the_defaults() {
        let managed = vec![AllowedCaller::new(
            "com.example.gateway-client",
            Some("EXAMPLE123"),
        )];
        let table = FakeProcessTable::default()
            .with(100, 1, "com.example.gateway-client", Some("EXAMPLE123"))
            .with(300, 100, "ai.litellm.relay", None);
        let callers = effective_callers(Some(&managed));
        assert!(matches!(
            walk(&table, &callers, peer(300)),
            Verdict::Allowed { level: 1, .. }
        ));
        assert!(matches!(
            walk(&relay_under_shell_under_claude(), &callers, peer(300)),
            Verdict::Refused { .. }
        ));
        assert_eq!(effective_callers(None), default_callers());
    }

    #[test]
    fn should_match_an_entry_without_a_team_against_any_signer() {
        let managed = vec![AllowedCaller::new("com.example.gateway-client", None)];
        let table = FakeProcessTable::default()
            .with(100, 1, "com.example.gateway-client", Some("WHOEVER0001"))
            .with(300, 100, "ai.litellm.relay", None);
        assert!(matches!(
            walk(&table, &managed, peer(300)),
            Verdict::Allowed { level: 1, .. }
        ));
    }

    #[test]
    fn should_compile_the_requirement_with_and_without_a_team() {
        assert_eq!(
            AllowedCaller::new("codex", Some(CODEX_TEAM)).requirement().as_deref(),
            Some("identifier \"codex\" and anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"")
        );
        assert_eq!(
            AllowedCaller::new("codex", None).requirement().as_deref(),
            Some("identifier \"codex\"")
        );
        assert_eq!(AllowedCaller::new("co\"dex", None).requirement(), None);
        assert_eq!(AllowedCaller::new("codex", Some("")).requirement(), None);
        assert_eq!(AllowedCaller::new("", None).requirement(), None);
    }

    #[test]
    fn should_skip_an_unquotable_managed_entry_instead_of_matching_everything() {
        let managed = vec![
            AllowedCaller::new("bad\"entry", None),
            AllowedCaller::new("ok", None),
        ];
        let table = FakeProcessTable::default().with(100, 1, "ok", None).with(
            300,
            100,
            "ai.litellm.relay",
            None,
        );
        assert!(matches!(
            walk(&table, &managed, peer(300)),
            Verdict::Allowed { level: 1, .. }
        ));
    }
}
