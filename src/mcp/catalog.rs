use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::{
    upstream::UpstreamTool,
    verdict::{decide, name_words, text_words, Listing, Verdict},
};

pub const UNGROUPED_SERVER: &str = "ungrouped";
pub const SEARCH_RESULT_LIMIT: usize = 5;
const NAME_WORD_SCORE: usize = 3;
const SERVER_WORD_SCORE: usize = 2;
const DESCRIPTION_WORD_SCORE: usize = 1;

const fn servers_start_active() -> bool {
    false
}

pub fn split_gateway_name<'a>(gateway_name: &'a str, separator: &str) -> (&'a str, &'a str) {
    if separator.is_empty() {
        return (UNGROUPED_SERVER, gateway_name);
    }
    gateway_name
        .split_once(separator)
        .unwrap_or((UNGROUPED_SERVER, gateway_name))
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    pub name: String,
    pub server: String,
    own_name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub annotations: Option<Value>,
    pub verdict: Verdict,
    listed_twice: bool,
}

impl CatalogEntry {
    pub fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "server": self.server,
            "description": self.description,
            "input_schema": self.input_schema,
            "verdict": self.verdict,
            "annotations": self.annotations,
        })
    }

    fn grouped(self, separator: &str) -> Self {
        let (server, own_name) = split_gateway_name(&self.name, separator);
        Self {
            server: server.to_string(),
            own_name: own_name.to_string(),
            ..self
        }
    }

    fn decided(self, allow: &[String]) -> Self {
        let verdict = decide(
            Listing {
                gateway_name: &self.name,
                own_name: &self.own_name,
                grouped: self.server != UNGROUPED_SERVER,
                annotations: self.annotations.as_ref(),
                listed_twice: self.listed_twice,
            },
            allow,
        );
        Self { verdict, ..self }
    }

    fn score(&self, query_words: &BTreeSet<String>) -> usize {
        let own_words = name_words(&self.own_name);
        let server_words = name_words(&self.server);
        let description_words = text_words(self.description.as_deref().unwrap_or_default());
        query_words
            .iter()
            .map(|word| {
                NAME_WORD_SCORE * usize::from(own_words.contains(word))
                    + SERVER_WORD_SCORE * usize::from(server_words.contains(word))
                    + DESCRIPTION_WORD_SCORE * usize::from(description_words.contains(word))
            })
            .sum()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveServers {
    activated: BTreeSet<String>,
}

impl ActiveServers {
    pub fn is_active(&self, server: &str) -> bool {
        servers_start_active() || self.activated.contains(server)
    }

    pub fn activate(&mut self, server: &str) {
        self.activated.insert(server.to_string());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub tools: Vec<String>,
    pub inactive_servers: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    entries: BTreeMap<String, CatalogEntry>,
}

impl Catalog {
    pub fn build(tools: Vec<UpstreamTool>, separator: &str, allow: &[String]) -> Self {
        let mut entries: BTreeMap<String, CatalogEntry> = BTreeMap::new();
        for tool in tools {
            if let Some(listed_before) = entries.get_mut(&tool.name) {
                listed_before.listed_twice = true;
                continue;
            }
            let entry = CatalogEntry {
                server: UNGROUPED_SERVER.to_string(),
                own_name: tool.name.clone(),
                name: tool.name.clone(),
                description: tool.description,
                input_schema: tool.input_schema,
                annotations: tool.annotations,
                verdict: Verdict::Ask,
                listed_twice: false,
            };
            entries.insert(tool.name, entry);
        }
        Self { entries }.with_settings(separator, allow)
    }

    pub fn with_settings(self, separator: &str, allow: &[String]) -> Self {
        Self {
            entries: self
                .entries
                .into_iter()
                .map(|(name, entry)| (name, entry.grouped(separator).decided(allow)))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&CatalogEntry> {
        self.entries.get(name)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn tool_counts_by_server(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for entry in self.entries.values() {
            *counts.entry(entry.server.clone()).or_insert(0) += 1;
        }
        counts
    }

    pub fn search(&self, query: &str, active: &ActiveServers) -> SearchResult {
        let query_words: BTreeSet<String> = text_words(query).into_iter().collect();
        let matches: Vec<(usize, &CatalogEntry)> = self
            .entries
            .values()
            .map(|entry| (entry.score(&query_words), entry))
            .filter(|(score, _)| *score > 0)
            .collect();
        let mut on_active_servers: Vec<(usize, &CatalogEntry)> = matches
            .iter()
            .copied()
            .filter(|(_, entry)| active.is_active(&entry.server))
            .collect();
        on_active_servers.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.name.cmp(&right.name))
        });
        let inactive_servers: BTreeSet<String> = matches
            .iter()
            .filter(|(_, entry)| !active.is_active(&entry.server))
            .map(|(_, entry)| entry.server.clone())
            .collect();
        SearchResult {
            tools: on_active_servers
                .into_iter()
                .take(SEARCH_RESULT_LIMIT)
                .map(|(_, entry)| entry.name.clone())
                .collect(),
            inactive_servers: inactive_servers.into_iter().collect(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tool(name: &str, description: &str) -> UpstreamTool {
        UpstreamTool {
            name: name.to_string(),
            description: Some(description.to_string()),
            input_schema: json!({"type": "object"}),
            annotations: None,
        }
    }

    fn active(servers: &[&str]) -> ActiveServers {
        let mut active = ActiveServers::default();
        servers.iter().for_each(|server| active.activate(server));
        active
    }

    #[test]
    fn should_start_every_server_inactive() {
        assert!(!ActiveServers::default().is_active("github"));
        assert!(active(&["github"]).is_active("github"));
        assert!(!active(&["github"]).is_active("jira"));
    }

    #[test]
    fn should_group_a_tool_under_the_text_before_the_first_separator() {
        assert_eq!(
            split_gateway_name("github-get_issue", "-"),
            ("github", "get_issue")
        );
        assert_eq!(
            split_gateway_name("github-get-issue", "-"),
            ("github", "get-issue")
        );
        assert_eq!(split_gateway_name("whoami", "-"), ("ungrouped", "whoami"));
        let catalog = Catalog::build(
            vec![
                tool("github-get_issue", ""),
                tool("github-list", ""),
                tool("whoami", ""),
            ],
            "-",
            &[],
        );
        assert_eq!(
            catalog.tool_counts_by_server(),
            BTreeMap::from([("github".to_string(), 2), ("ungrouped".to_string(), 1)])
        );
    }

    #[test]
    fn should_group_by_the_configured_separator_and_regroup_when_it_changes() {
        let tools = vec![
            tool("github__get_issue", ""),
            tool("github__create_issue", ""),
            tool("search__reindex", ""),
        ];
        assert_eq!(
            split_gateway_name("github__get_issue", "__"),
            ("github", "get_issue")
        );
        assert_eq!(
            split_gateway_name("github__get_issue", ""),
            ("ungrouped", "github__get_issue")
        );
        let by_dash = Catalog::build(tools.clone(), "-", &[]);
        assert_eq!(
            by_dash.tool_counts_by_server(),
            BTreeMap::from([("ungrouped".to_string(), 3)])
        );
        for name in [
            "github__get_issue",
            "github__create_issue",
            "search__reindex",
        ] {
            assert_eq!(
                by_dash.get(name).map(|entry| entry.verdict),
                Some(Verdict::Ask),
                "{name}"
            );
        }
        let regrouped = by_dash.with_settings("__", &[]);
        assert_eq!(
            regrouped.tool_counts_by_server(),
            BTreeMap::from([("github".to_string(), 2), ("search".to_string(), 1)])
        );
        assert_eq!(
            regrouped
                .get("github__get_issue")
                .map(|entry| entry.verdict),
            Some(Verdict::Allow)
        );
        assert_eq!(
            regrouped.get("search__reindex").map(|entry| entry.verdict),
            Some(Verdict::Ask)
        );
        assert_eq!(
            regrouped.search("issue", &active(&["github"])).tools,
            ["github__create_issue", "github__get_issue"]
        );
        assert_eq!(regrouped, Catalog::build(tools, "__", &[]));
    }

    #[test]
    fn should_compute_verdicts_at_build_and_again_when_the_allow_list_changes() {
        let tools = vec![
            tool("github-get_issue", ""),
            tool("github-create_issue", ""),
        ];
        let catalog = Catalog::build(tools, "-", &[]);
        assert_eq!(
            catalog.get("github-get_issue").map(|entry| entry.verdict),
            Some(Verdict::Allow)
        );
        assert_eq!(
            catalog
                .get("github-create_issue")
                .map(|entry| entry.verdict),
            Some(Verdict::Ask)
        );
        let allowed = catalog.with_settings("-", &["github-create_issue".to_string()]);
        assert_eq!(
            allowed
                .get("github-create_issue")
                .map(|entry| entry.verdict),
            Some(Verdict::Allow)
        );
        let tightened = allowed.with_settings("-", &[]);
        assert_eq!(
            tightened
                .get("github-create_issue")
                .map(|entry| entry.verdict),
            Some(Verdict::Ask)
        );
    }

    #[test]
    fn should_decide_the_review_table_from_the_gateway_name() {
        let table: [(&str, &str, Verdict); 17] = [
            ("s-get_and_delete", "s", Verdict::Ask),
            ("s-listThenRemove", "s", Verdict::Ask),
            ("s-search-and-replace", "s", Verdict::Ask),
            ("s-GetUser", "s", Verdict::Allow),
            ("s-read_write", "s", Verdict::Ask),
            ("s-info", "s", Verdict::Allow),
            ("s-status_update", "s", Verdict::Ask),
            ("s-fetchAndRun", "s", Verdict::Ask),
            ("s-describe", "s", Verdict::Allow),
            ("s-get", "s", Verdict::Allow),
            ("s-get_issue2", "s", Verdict::Allow),
            ("s-list.issues", "s", Verdict::Allow),
            ("s-Get-Issue", "s", Verdict::Allow),
            ("github-get-issue", "github", Verdict::Allow),
            ("a-b-c", "a", Verdict::Ask),
            ("Get-Issue", "Get", Verdict::Ask),
            ("search-and-replace", "search", Verdict::Ask),
        ];
        let catalog = Catalog::build(
            table.iter().map(|(name, _, _)| tool(name, "")).collect(),
            "-",
            &[],
        );
        for (name, server, verdict) in table {
            let entry = catalog.get(name).expect(name);
            assert_eq!(
                (entry.server.as_str(), entry.verdict),
                (server, verdict),
                "{name}"
            );
        }
    }

    #[test]
    fn should_keep_a_name_listed_twice_once_and_never_allow_it() {
        let allow = ["github-get_issue".to_string()];
        let catalog = Catalog::build(
            vec![
                tool("github-get_issue", "first"),
                tool("github-get_issue", "second"),
            ],
            "-",
            &allow,
        );
        assert_eq!(catalog.len(), 1);
        let entry = catalog.get("github-get_issue").expect("entry");
        assert_eq!(entry.verdict, Verdict::Ask);
        assert_eq!(entry.description.as_deref(), Some("first"));
        assert_eq!(
            catalog
                .with_settings("-", &allow)
                .get("github-get_issue")
                .map(|entry| entry.verdict),
            Some(Verdict::Ask)
        );
    }

    #[test]
    fn should_describe_a_tool_with_its_schema_verdict_and_annotations_as_sent() {
        let annotations = json!({"readOnlyHint": true, "title": "Get issue"});
        let schema = json!({"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]});
        let catalog = Catalog::build(
            vec![UpstreamTool {
                name: "github-get_issue".to_string(),
                description: Some("Read one issue".to_string()),
                input_schema: schema.clone(),
                annotations: Some(annotations.clone()),
            }],
            "-",
            &[],
        );
        assert_eq!(
            catalog.get("github-get_issue").map(CatalogEntry::describe),
            Some(json!({
                "name": "github-get_issue",
                "server": "github",
                "description": "Read one issue",
                "input_schema": schema,
                "verdict": "allow",
                "annotations": annotations,
            }))
        );
    }

    #[test]
    fn should_rank_by_score_then_name_and_answer_at_most_five_names() {
        let catalog = Catalog::build(
            vec![
                tool("github-list_issues", "List issues in a repository"),
                tool("github-get_issue", "Read one issue. Mentions issues, too"),
                tool("github-search_code", "Search code"),
                tool("github-zeta_issues", ""),
                tool("github-alpha_issues", ""),
                tool("github-beta_issues", ""),
                tool("github-gamma_issues", ""),
                tool("github-issues", "issues"),
                tool("github-unrelated", "Nothing to see"),
            ],
            "-",
            &[],
        );
        let result = catalog.search("Issues, issues!", &active(&["github"]));
        assert_eq!(
            result.tools,
            [
                "github-issues",
                "github-list_issues",
                "github-alpha_issues",
                "github-beta_issues",
                "github-gamma_issues",
            ]
        );
        assert!(result.inactive_servers.is_empty());
        let by_server = catalog.search("github search", &active(&["github"]));
        assert_eq!(by_server.tools[0], "github-search_code");
        assert_eq!(by_server.tools.len(), 5);
        let ranked = Catalog::build(
            vec![
                tool("search-list", "List saved searches"),
                tool("github-search_code", "Find code"),
            ],
            "-",
            &[],
        );
        assert_eq!(
            ranked
                .search("search", &active(&["github", "search"]))
                .tools,
            ["github-search_code", "search-list"]
        );
        assert!(catalog
            .search("payroll", &active(&["github"]))
            .tools
            .is_empty());
        assert!(catalog.search("", &active(&["github"])).tools.is_empty());
    }

    #[test]
    fn should_hide_tools_on_inactive_servers_and_name_those_servers() {
        let catalog = Catalog::build(
            vec![
                tool("github-list_issues", "List issues"),
                tool("jira-searchIssues", "Search issues"),
                tool("linear-list_issues", "List issues"),
                tool("slack-post_message", "Send a message"),
            ],
            "-",
            &[],
        );
        let result = catalog.search("issues", &active(&["github"]));
        assert_eq!(result.tools, ["github-list_issues"]);
        assert_eq!(result.inactive_servers, ["jira", "linear"]);
        let nothing_active = catalog.search("issues", &ActiveServers::default());
        assert!(nothing_active.tools.is_empty());
        assert_eq!(
            nothing_active.inactive_servers,
            ["github", "jira", "linear"]
        );
    }
}
