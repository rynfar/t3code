//! Cases ported from both TypeScript `promptLinks.test.ts` files.
use super::*;
use serde_json::json;
fn repo() -> Repository {
    Repository {
        owner: "pingdotgg".into(),
        name: "t3code".into(),
    }
}
#[test]
fn strips_and_counts_hidden_characters_but_preserves_emoji() {
    assert_eq!(
        sanitize_linked_prompt("a\u{200b}b\u{202e}c\u{feff}rate me\u{e0041}\u{e0042}"),
        LinkedPrompt {
            text: "abcrate me".into(),
            hidden_characters: 5
        }
    );
    assert_eq!(sanitize_linked_prompt("a\u{2028}b\u{2029}c\r\nd\re").text, "a\nb\nc\nd\ne");
    let emoji = "👨\u{200d}👩\u{200d}👧 ❤️";
    assert_eq!(
        sanitize_linked_prompt(emoji),
        LinkedPrompt {
            text: emoji.into(),
            hidden_characters: 0
        }
    );
    for text in ["a\u{200d}b", "👨\u{200d}", "\u{20000}\u{200d}\u{20001}"] {
        assert_eq!(sanitize_linked_prompt(text).hidden_characters, 1);
    }
}
#[test]
fn reads_prompt_repository_model_and_optional_pr_prompt() {
    assert_eq!(
        parse_new_thread_link("?prompt=Rate+me&repo=https://github.com/pingdotgg/t3code.git&provider=Codex&model=gpt-5").unwrap(),
        NewThreadLink::New {
            prompt: LinkedPrompt {
                text: "Rate me".into(),
                hidden_characters: 0
            },
            repository: Some(repo()),
            provider: Some("codex".into()),
            model: Some("gpt-5".into())
        }
    );
    for pr in [
        "pingdotgg/t3code/42",
        "pingdotgg/t3code%2342",
        "https://github.com/pingdotgg/t3code/pull/42/files",
    ] {
        assert_eq!(
            parse_new_thread_link(&format!("?pr={pr}")).unwrap(),
            NewThreadLink::PullRequest {
                pull_request: PullRequest {
                    repository: repo(),
                    number: 42
                },
                prompt: None
            }
        );
    }
    for search in [
        "?repo=a/b",
        "?prompt=%E2%80%8B",
        "?prompt=x&repo=../etc",
        "?pr=a/b/0",
        "?pr=a/b/9007199254740992",
    ] {
        assert!(parse_new_thread_link(search).is_err(), "{search}");
    }
    assert!(parse_thread_link_prompt("?prompt=%E2%80%8B").is_err());
    assert!(parse_new_thread_link(&format!("?prompt={}", "x".repeat(8001))).is_err());
    assert!(parse_new_thread_link(&format!("?prompt={}", "😀".repeat(4001))).is_err());
    assert!(parse_new_thread_link(&format!("?prompt={}", "😀".repeat(4000))).is_ok());
    assert_eq!(parse_thread_link_prompt("?prompt=Fix+it").unwrap().unwrap().text, "Fix it");
    assert_eq!(parse_thread_link_prompt("?panel=diff").unwrap(), None);
    assert_eq!(without_prompt_link_params("?prompt=x&panel=diff&model=y&repo=a%2Fb"), "?panel=diff");
}
#[test]
fn roundtrips_and_maps_only_prompt_routes() {
    let link = build_new_thread_link("Swear count & rating?", Some(&repo()), Some("claudeAgent"), Some("opus"));
    assert!(link.starts_with("https://app.t3.codes/new?"));
    let path = prompt_link_app_path(&link).unwrap();
    assert_eq!(
        parse_new_thread_link(path.split_once('?').unwrap().1).unwrap().prompt().unwrap().text,
        "Swear count & rating?"
    );
    for (link, path) in [
        ("t3code://new?prompt=hi", "/new?prompt=hi"),
        ("t3code-dev://threads/env-1/thread-1", "/env-1/thread-1"),
        ("t3code-native://threads/env-1/thread-1?prompt=go", "/env-1/thread-1?prompt=go"),
        ("https://app.t3.codes/env-1/thread-1?prompt=go", "/env-1/thread-1?prompt=go"),
    ] {
        assert_eq!(prompt_link_app_path(link).as_deref(), Some(path));
    }
    for link in [
        "https://app.t3.codes/settings/general",
        "https://evil.test/new?prompt=hi",
        "t3code://app/index.html",
        "t3code-native://new:99?prompt=x",
        "t3code-native://threads//thread?prompt=x",
        "t3code-native://threads//env/thread?prompt=x",
    ] {
        assert!(prompt_link_app_path(link).is_none(), "{link}");
    }
    assert_eq!(prompt_link_deep_link("/new?prompt=hi").as_deref(), Some("t3code-native://new?prompt=hi"));
    assert_eq!(
        prompt_link_deep_link("/env/thread?prompt=hi").as_deref(),
        Some("t3code-native://threads/env/thread?prompt=hi")
    );
    assert!(prompt_link_for(" ", None, None, None).is_none());
    assert!(prompt_link_for(&"x".repeat(8001), None, None, None).is_none());
    let bootstrap = "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";
    assert!(prompt_link_for(bootstrap, None, None, None).is_none());
    assert!(prompt_link_for(&format!("Ultrathink:\n{bootstrap}"), None, None, None).is_none());
    assert!(!build_new_thread_link("x", None, None, Some("opus")).contains("model"));
}
fn project(id: &str, identity: Value) -> ProjectShell {
    serde_json::from_value(json!({"id":id,"title":id,"workspaceRoot":"/x","repositoryIdentity":identity,"createdAt":"","updatedAt":""})).unwrap()
}
#[test]
fn matches_repository_origin_and_mirrors_in_supplied_order() {
    let projects = vec![
        project(
            "fork",
            json!({"canonicalKey":"github.com/pingdotgg/t3code","origin":{"canonicalKey":"github.com/me/t3code"}}),
        ),
        project("none", Value::Null),
        project("recent", json!({"canonicalKey":"github.com/PingDotGG/T3Code"})),
        project("older", json!({"canonicalKey":"github.com/pingdotgg/t3code"})),
        project("cafe", json!({"canonicalKey":"git.cafe/pingdotgg/t3code"})),
        project("staging", json!({"canonicalKey":"staging.git.cafe/pingdotgg/t3code"})),
        project("gitlab", json!({"canonicalKey":"gitlab.com/pingdotgg/t3code"})),
    ];
    let ids = |r: &Repository| find_prompt_link_projects(&projects, r).iter().map(|p| p.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids(&repo()), vec!["recent", "older", "cafe", "staging"]);
    assert_eq!(
        ids(&Repository {
            owner: "me".into(),
            name: "t3code".into()
        }),
        vec!["fork"]
    );
    assert!(
        ids(&Repository {
            owner: "gitlab".into(),
            name: "x".into()
        })
        .is_empty()
    );
}
fn provider(id: &str, driver: &str, status: &str) -> ServerProvider {
    serde_json::from_value(json!({"instanceId":id,"driver":driver,"enabled":true,"installed":true,"status":status,"models":[{"slug":"small","name":"Small"},{"slug":"big","name":"Big","aliases":["opus"],"isDefault":true}]})).unwrap()
}
#[test]
fn resolves_aliases_prefers_ready_and_reports_fallback() {
    let mut providers = vec![
        provider("broken", "claudeAgent", "error"),
        provider("warning", "claudeAgent", "warning"),
        provider("work", "claudeAgent", "ready"),
    ];
    for (driver, model, expected, exact) in [
        ("claude", Some("small"), "small", true),
        ("claudeagent", Some("opus"), "big", true),
        ("claude", Some("SMALL"), "small", true),
        ("claude", Some("nope"), "big", false),
        ("claude", None, "big", true),
    ] {
        let result = resolve_prompt_link_model_selection(&providers, driver, model).unwrap();
        assert_eq!(result.selection.instance_id, "work");
        assert_eq!(result.selection.model, expected);
        assert_eq!(result.exact, exact);
    }
    assert!(resolve_prompt_link_model_selection(&providers, "cursor", None).is_none());
    providers[2].availability = Some("unavailable".into());
    assert_eq!(
        resolve_prompt_link_model_selection(&providers, "claude", None).unwrap().selection.instance_id,
        "warning"
    );
    providers[1].enabled = false;
    assert!(resolve_prompt_link_model_selection(&providers, "claude", None).is_none());
    let mut codex = provider("codex", "codex", "ready");
    codex.models[0].slug = "gpt-5.4".into();
    assert_eq!(
        resolve_prompt_link_model_selection(&[codex], "codex", Some("5.4")).unwrap().selection.model,
        "gpt-5.4"
    );
}
#[test]
fn newest_visible_pr_thread_never_inherited_side_chat() {
    let base: Value = serde_json::from_str(include_str!("../../tests/conformance/shell.jsonl").lines().next().unwrap()).unwrap();
    let make = |id: &str, updated: &str, side: bool, source: &str| {
        let mut t = base["items"][0]["snapshot"]["threads"][0].clone();
        t["id"] = json!(id);
        t["updatedAt"] = json!(updated);
        t["lineage"]["sideChat"] = json!(side);
        t["pullRequests"] = json!([{"host":"github.com","repository":"acme/web","number":7,"url":"https://github.com/acme/web/pull/7","source":source,"linkedAt":"2026-01-01"}]);
        serde_json::from_value::<ThreadShell>(t).unwrap()
    };
    let threads = vec![
        make("old", "2026-01-01", false, "manual"),
        make("parent", "2026-01-02", false, "manual"),
        make("side-chat", "2026-01-03", true, "manual"),
        make("dismissed", "2026-01-04", false, "stack-dismissed"),
    ];
    let pr = PullRequest {
        repository: Repository {
            owner: "acme".into(),
            name: "web".into(),
        },
        number: 7,
    };
    assert_eq!(find_prompt_link_pull_request_thread(&threads, &pr).unwrap().id, "parent");
}
#[test]
fn replacement_requires_a_different_nonempty_draft() {
    let prompt = sanitize_linked_prompt("go");
    assert!(!needs_draft_replacement(" \n", &prompt));
    assert!(!needs_draft_replacement("go", &prompt));
    assert!(needs_draft_replacement("keep this", &prompt));
}

#[test]
fn earlier_pending_pr_lookups_hold_back_later_threads() {
    use PullRequestLookup::*;
    let laptop = Answer {
        environment: "laptop".into(),
        threads: vec![LinkedThread {
            id: "a".into(),
            archived_at: None,
        }],
    };
    let server = Answer {
        environment: "server".into(),
        threads: vec![LinkedThread {
            id: "b".into(),
            archived_at: None,
        }],
    };
    let none = Answer {
        environment: "laptop".into(),
        threads: vec![],
    };
    assert_eq!(pick_prompt_link_pull_request_thread(&[Pending]), None);
    assert_eq!(pick_prompt_link_pull_request_thread(&[none.clone(), Pending]), None);
    assert_eq!(pick_prompt_link_pull_request_thread(&[Pending, server.clone()]), None);
    assert_eq!(pick_prompt_link_pull_request_thread(&[laptop.clone(), Pending]), Some(Some(("laptop", "a"))));
    assert_eq!(pick_prompt_link_pull_request_thread(&[laptop, server.clone()]), Some(Some(("laptop", "a"))));
    assert_eq!(pick_prompt_link_pull_request_thread(&[none.clone(), server]), Some(Some(("server", "b"))));
    assert_eq!(pick_prompt_link_pull_request_thread(&[none]), Some(None));
    let archived = Answer {
        environment: "laptop".into(),
        threads: vec![LinkedThread {
            id: "archived".into(),
            archived_at: Some("2026-01-01".into()),
        }],
    };
    assert_eq!(pick_prompt_link_pull_request_thread(&[archived, Failed]), Some(None));
}
