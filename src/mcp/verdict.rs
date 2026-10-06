use serde::Serialize;
use serde_json::Value;

const READ_WORDS: [&str; 19] = [
    "get",
    "list",
    "read",
    "search",
    "find",
    "fetch",
    "describe",
    "show",
    "view",
    "lookup",
    "count",
    "status",
    "whoami",
    "ping",
    "help",
    "info",
    "inspect",
    "preview",
    "summarize",
];

const WRITE_WORDS: [&str; 68] = [
    "create", "update", "delete", "remove", "write", "set", "put", "post", "patch", "send", "add",
    "insert", "edit", "modify", "move", "rename", "upload", "run", "execute", "exec", "deploy",
    "merge", "close", "open", "cancel", "approve", "reject", "comment", "reply", "publish", "push",
    "commit", "drop", "kill", "stop", "start", "restart", "trigger", "invite", "assign", "archive",
    "restore", "reset", "revoke", "grant", "share", "pay", "purchase", "order", "book", "schedule",
    "submit", "sync", "import", "install", "enable", "disable", "apply", "fork", "clone",
    "transfer", "purge", "clear", "lock", "unlock", "sign", "rotate", "and",
];

const NAME_SEPARATORS: [char; 5] = ['_', '-', '.', '/', ' '];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Allow,
    Ask,
}

#[derive(Debug, Clone, Copy)]
pub struct Listing<'a> {
    pub gateway_name: &'a str,
    pub own_name: &'a str,
    pub annotations: Option<&'a Value>,
    pub listed_twice: bool,
}

pub fn decide(listing: Listing<'_>, allow: &[String]) -> Verdict {
    if listing.listed_twice {
        return Verdict::Ask;
    }
    if allow.iter().any(|allowed| allowed == listing.gateway_name) {
        return Verdict::Allow;
    }
    if name_reads_only(listing.own_name) && annotations_permit(listing.annotations) {
        return Verdict::Allow;
    }
    Verdict::Ask
}

pub fn name_words(name: &str) -> Vec<String> {
    split_words(name, |character| NAME_SEPARATORS.contains(&character))
}

pub fn text_words(text: &str) -> Vec<String> {
    split_words(text, |character| !character.is_alphanumeric())
}

fn name_reads_only(own_name: &str) -> bool {
    let words = name_words(own_name);
    let starts_with_a_read_word = words
        .first()
        .is_some_and(|first| READ_WORDS.contains(&first.as_str()));
    let has_a_write_word = words
        .iter()
        .any(|word| WRITE_WORDS.contains(&word.as_str()));
    starts_with_a_read_word && !has_a_write_word
}

fn annotations_permit(annotations: Option<&Value>) -> bool {
    let Some(annotations) = annotations else {
        return true;
    };
    let hint_is_absent_or = |hint: &str, permitted: bool| match annotations.get(hint) {
        None | Some(Value::Null) => true,
        Some(Value::Bool(value)) => *value == permitted,
        Some(_) => false,
    };
    annotations.is_object()
        && hint_is_absent_or("readOnlyHint", true)
        && hint_is_absent_or("destructiveHint", false)
}

fn split_words(text: &str, separates: impl Fn(char) -> bool) -> Vec<String> {
    let characters: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    for (index, &character) in characters.iter().enumerate() {
        if separates(character) {
            flush(&mut words, &mut word);
            continue;
        }
        if starts_a_camel_case_word(&characters, index) {
            flush(&mut words, &mut word);
        }
        word.extend(character.to_lowercase());
    }
    flush(&mut words, &mut word);
    words
}

fn starts_a_camel_case_word(characters: &[char], index: usize) -> bool {
    let Some(previous) = index.checked_sub(1).map(|before| characters[before]) else {
        return false;
    };
    if !characters[index].is_uppercase() {
        return false;
    }
    let after_a_lowercase_or_digit = previous.is_lowercase() || previous.is_numeric();
    let ends_an_acronym = previous.is_uppercase()
        && characters
            .get(index + 1)
            .is_some_and(|next| next.is_lowercase());
    after_a_lowercase_or_digit || ends_an_acronym
}

fn flush(words: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn verdict_of(own_name: &str, annotations: Option<Value>) -> Verdict {
        let gateway_name = format!("server-{own_name}");
        decide(
            Listing {
                gateway_name: &gateway_name,
                own_name,
                annotations: annotations.as_ref(),
                listed_twice: false,
            },
            &[],
        )
    }

    #[test]
    fn should_split_names_on_separators_and_camel_case_into_lowercase_words() {
        assert_eq!(name_words("get_user"), ["get", "user"]);
        assert_eq!(name_words("getUserById"), ["get", "user", "by", "id"]);
        assert_eq!(name_words("getHTTPStatus"), ["get", "http", "status"]);
        assert_eq!(
            name_words("list.pull-requests/open now"),
            ["list", "pull", "requests", "open", "now"]
        );
        assert_eq!(name_words("GETDELETE"), ["getdelete"]);
        assert_eq!(name_words("v2List"), ["v2", "list"]);
        assert!(name_words("__--").is_empty());
    }

    #[test]
    fn should_allow_only_names_that_start_with_a_read_word_and_carry_no_write_word() {
        let table: [(&str, Option<Value>, Verdict); 30] = [
            ("get_issue", None, Verdict::Allow),
            ("listPullRequests", None, Verdict::Allow),
            ("search.code", None, Verdict::Allow),
            ("describe table", None, Verdict::Allow),
            ("whoami", None, Verdict::Allow),
            ("get_user_settings", None, Verdict::Allow),
            ("list_orders", None, Verdict::Allow),
            ("summarize/thread", None, Verdict::Allow),
            ("create_issue", None, Verdict::Ask),
            ("get_and_delete", None, Verdict::Ask),
            ("getAndDelete", None, Verdict::Ask),
            ("list_then_remove", None, Verdict::Ask),
            ("get_order", None, Verdict::Ask),
            ("search_and_list", None, Verdict::Ask),
            ("query", None, Verdict::Ask),
            ("export_report", None, Verdict::Ask),
            ("check_status", None, Verdict::Ask),
            ("download_file", None, Verdict::Ask),
            ("generate_summary", None, Verdict::Ask),
            ("issue_get", None, Verdict::Ask),
            ("getter", None, Verdict::Ask),
            ("GETDELETE", None, Verdict::Ask),
            ("", None, Verdict::Ask),
            (
                "get_issue",
                Some(json!({"readOnlyHint": true})),
                Verdict::Allow,
            ),
            (
                "get_issue",
                Some(json!({"readOnlyHint": false})),
                Verdict::Ask,
            ),
            (
                "get_issue",
                Some(json!({"destructiveHint": true})),
                Verdict::Ask,
            ),
            (
                "get_issue",
                Some(json!({"destructiveHint": false, "title": "Get"})),
                Verdict::Allow,
            ),
            (
                "get_issue",
                Some(json!({"readOnlyHint": "yes"})),
                Verdict::Ask,
            ),
            ("get_issue", Some(json!(["readOnlyHint"])), Verdict::Ask),
            (
                "create_issue",
                Some(json!({"readOnlyHint": true, "destructiveHint": false})),
                Verdict::Ask,
            ),
        ];
        for (own_name, annotations, expected) in table {
            assert_eq!(
                verdict_of(own_name, annotations.clone()),
                expected,
                "{own_name} with {annotations:?}"
            );
        }
    }

    #[test]
    fn should_ask_for_every_write_word_even_behind_a_read_word() {
        for write_word in WRITE_WORDS {
            assert_eq!(
                verdict_of(&format!("get_{write_word}_thing"), None),
                Verdict::Ask,
                "{write_word}"
            );
        }
        for read_word in READ_WORDS {
            assert_eq!(
                verdict_of(&format!("{read_word}_thing"), None),
                Verdict::Allow,
                "{read_word}"
            );
        }
    }

    #[test]
    fn should_let_the_allow_list_turn_ask_into_allow_only_on_an_exact_name() {
        let allow = ["github-create_issue".to_string()];
        let listing = |gateway_name: &'static str, own_name: &'static str, listed_twice| Listing {
            gateway_name,
            own_name,
            annotations: None,
            listed_twice,
        };
        assert_eq!(
            decide(
                listing("github-create_issue", "create_issue", false),
                &allow
            ),
            Verdict::Allow
        );
        assert_eq!(
            decide(
                listing("github-Create_Issue", "Create_Issue", false),
                &allow
            ),
            Verdict::Ask
        );
        assert_eq!(
            decide(
                listing("gitlab-create_issue", "create_issue", false),
                &allow
            ),
            Verdict::Ask
        );
        assert_eq!(
            decide(
                listing("github-create_issue_comment", "create_issue_comment", false),
                &allow
            ),
            Verdict::Ask
        );
        assert_eq!(
            decide(listing("github-get_issue", "get_issue", false), &allow),
            Verdict::Allow
        );
    }

    #[test]
    fn should_ask_for_a_name_listed_twice_even_when_it_reads_or_is_allow_listed() {
        let allow = ["github-get_issue".to_string()];
        let twice = Listing {
            gateway_name: "github-get_issue",
            own_name: "get_issue",
            annotations: None,
            listed_twice: true,
        };
        assert_eq!(decide(twice, &allow), Verdict::Ask);
        assert_eq!(decide(twice, &[]), Verdict::Ask);
    }
}
