use std::collections::HashMap;

use super::*;

const NOW: i64 = 1_792_713_600_000;
const DAY: i64 = DAY_MS;

fn thread(title: &str) -> ThreadSearchSubject {
    ThreadSearchSubject { title: title.into(), activity_at: Some(NOW - 30 * DAY), ..Default::default() }
}

fn opens(opened_at: i64, score: f64, picks: &[(&str, i64)]) -> Option<ThreadOpenStats> {
    let picks = picks.iter().map(|&(query, at)| ThreadPick { query: query.into(), at }).collect();
    Some(ThreadOpenStats { opened_at, score, picks })
}

fn pr(number: u64, title: Option<&str>) -> ThreadPullRequest {
    ThreadPullRequest { number, title: title.map(String::from), references: Vec::new() }
}

fn range(start: usize, end: usize) -> SearchRange {
    SearchRange { start, end }
}

fn score(query: &str, subject: &ThreadSearchSubject) -> Option<SearchMatch> {
    score_thread_search(query, subject, NOW, ThreadSearchOptions::default())
}

fn rank_titles(query: &str, threads: &[ThreadSearchSubject]) -> Vec<String> {
    let normalized = fold_search_query(query);
    rank_search_items(threads, |subject, typos| {
        score_thread_search(&normalized, subject, NOW, ThreadSearchOptions { typos, picks: true })
    })
    .into_iter()
    .map(|entry| entry.item.title.clone())
    .collect()
}

#[test]
fn ranks_whole_words_over_prefixes_over_newer_mid_word_matches() {
    let threads = [
        ThreadSearchSubject { activity_at: Some(NOW), ..thread("Fix oauth redirect") },
        thread("Authentication cleanup"),
        thread("Share auth across worktrees"),
    ];
    assert_eq!(
        rank_titles("auth", &threads),
        ["Share auth across worktrees", "Authentication cleanup", "Fix oauth redirect"]
    );
}

#[test]
fn treats_camel_case_kebab_path_and_digit_boundaries_as_word_starts() {
    let starts = |title: &str, query: &str| score(query, &thread(title)).map(|m| m.ranges);
    assert_eq!(starts("useThreadSearch hook", "search"), Some(vec![range(9, 15)]));
    assert_eq!(starts("fix/sidebar-perf", "perf"), Some(vec![range(12, 16)]));
    assert_eq!(starts("Upgrade to pnpm12", "12"), Some(vec![range(15, 17)]));
    assert_eq!(starts("Fix JSONSchema parsing", "sch"), Some(vec![range(8, 11)]));
    assert_eq!(starts("Fix HTTPServer crash", "ser"), Some(vec![range(8, 11)]));
    assert_eq!(starts("Rename index.ts", ".ts"), Some(vec![range(12, 15)]));
    assert!(score("githuba", &thread("GitHubActions setup")).is_some_and(|m| m.quality < 1.0));
    assert_eq!(
        rank_titles(".ts", &[thread("Fix parser.tsx"), thread("Rename index.ts")]),
        ["Rename index.ts", "Fix parser.tsx"]
    );
    assert!(score("ci", &thread("Decision log")).is_none());
}

#[test]
fn lets_recent_activity_and_frequent_opens_decide_between_equal_matches() {
    let threads = [
        ThreadSearchSubject { activity_at: Some(NOW - 20 * DAY), ..thread("Goldbug epilogue") },
        ThreadSearchSubject { activity_at: Some(NOW - DAY), ..thread("Goldbug processes") },
    ];
    assert_eq!(rank_titles("goldbug", &threads), ["Goldbug processes", "Goldbug epilogue"]);
    let threads = [
        ThreadSearchSubject { activity_at: Some(NOW - DAY), ..thread("Goldbug processes") },
        ThreadSearchSubject {
            activity_at: Some(NOW - 40 * DAY),
            opens: opens(NOW - DAY, 12.0, &[]),
            ..thread("Goldbug notes")
        },
    ];
    assert_eq!(rank_titles("goldbug", &threads), ["Goldbug notes", "Goldbug processes"]);
}

#[test]
fn puts_the_thread_picked_for_a_query_first_when_that_query_is_typed_again() {
    let threads = [
        ThreadSearchSubject {
            opens: opens(NOW - 5 * DAY, 1.0, &[("rel", NOW - 5 * DAY)]),
            ..thread("Release checklist")
        },
        ThreadSearchSubject { activity_at: Some(NOW), ..thread("Relay pairing") },
    ];
    assert_eq!(rank_titles("rel", &threads), ["Release checklist", "Relay pairing"]);
    let auth = [
        ThreadSearchSubject {
            opens: opens(NOW - DAY, 1.0, &[("auth", NOW - DAY)]),
            ..thread("Fix oauth redirect")
        },
        thread("Share auth across worktrees"),
    ];
    assert_eq!(rank_titles("auth", &auth), ["Share auth across worktrees", "Fix oauth redirect"]);
    assert_eq!(rank_titles("re", &threads), ["Relay pairing", "Release checklist"]);
    assert_eq!(rank_titles("rela", &threads), ["Relay pairing"]);
    let stale = [
        ThreadSearchSubject {
            opens: opens(NOW - 90 * DAY, 1.0, &[("rel", NOW - 90 * DAY)]),
            ..thread("Release checklist")
        },
        ThreadSearchSubject { activity_at: Some(NOW), ..thread("Relay pairing") },
    ];
    assert_eq!(rank_titles("rel", &stale), ["Relay pairing", "Release checklist"]);
    let reopened = [
        ThreadSearchSubject {
            opens: opens(NOW - DAY, 2.0, &[("rel", NOW - 30 * DAY)]),
            ..thread("Release checklist")
        },
        ThreadSearchSubject {
            opens: opens(NOW - DAY, 1.0, &[("rel", NOW - DAY)]),
            ..thread("Relay pairing")
        },
    ];
    assert_eq!(rank_titles("rel", &reopened), ["Relay pairing", "Release checklist"]);
    let repicked = [
        ThreadSearchSubject {
            opens: opens(NOW - DAY, 1.0, &[("deploy", NOW - DAY)]),
            ..thread("Deploy")
        },
        ThreadSearchSubject {
            opens: opens(NOW, 1.0, &[("deploy", NOW)]),
            ..thread("Deploy timeout on Railway")
        },
    ];
    assert_eq!(rank_titles("deploy", &repicked), ["Deploy timeout on Railway", "Deploy"]);
    let exact = [
        ThreadSearchSubject {
            opens: opens(NOW, 2.0, &[("fix browser", NOW), ("fix", NOW - 2 * DAY)]),
            ..thread("Fix browser")
        },
        ThreadSearchSubject {
            opens: opens(NOW - DAY, 1.0, &[("fix", NOW - DAY)]),
            ..thread("Fix sidebar")
        },
    ];
    assert_eq!(rank_titles("fix", &exact), ["Fix sidebar", "Fix browser"]);
    let named = [
        ThreadSearchSubject {
            opens: opens(NOW, 1.0, &[("lakebed perf", NOW)]),
            ..thread("Lakebed perf audit")
        },
        thread("Lakebed"),
    ];
    assert_eq!(rank_titles("lakebed", &named), ["Lakebed", "Lakebed perf audit"]);
}

#[test]
fn finds_a_branch_pasted_with_the_apps_worktree_prefix() {
    let subject =
        ThreadSearchSubject { branch: Some("t3/cmdk-search-v2".into()), ..thread("Cmd+K search") };
    assert!(score(&fold_search_query("t3/cmdk-search-v2"), &subject).is_some());
    assert!(score(&fold_search_query("t3/cmdk"), &subject).is_some());
}

#[test]
fn ranks_a_title_holding_the_words_above_the_branch_prefix_and_project_name() {
    let busy = |subject: ThreadSearchSubject| ThreadSearchSubject {
        activity_at: Some(NOW),
        opens: opens(NOW, 6.0, &[]),
        ..subject
    };
    let threads = [
        busy(ThreadSearchSubject {
            branch: Some("t3/diagnose-connect".into()),
            project_title: Some("t3code".into()),
            ..thread("Diagnose Connect provisioning failures")
        }),
        ThreadSearchSubject {
            project_title: Some("t3code".into()),
            ..thread("T3 Connect Tunnel Lifecycle Review")
        },
    ];
    assert_eq!(rank_titles("t3 connect", &threads)[0], "T3 Connect Tunnel Lifecycle Review");
    let threads = [
        busy(ThreadSearchSubject {
            project_title: Some("t3code".into()),
            branch: Some("t3code/sidebar".into()),
            ..thread("Fix sidebar lag")
        }),
        ThreadSearchSubject {
            project_title: Some("slophouse".into()),
            ..thread("T3 Code vs Gemini Token Volume")
        },
    ];
    assert_eq!(rank_titles("t3code", &threads)[0], "T3 Code vs Gemini Token Volume");
}

#[test]
fn matches_pull_request_numbers_branches_and_projects_with_every_token_somewhere() {
    let subject = ThreadSearchSubject {
        branch: Some("t3/trim-mcp-models".into()),
        project_title: Some("t3code".into()),
        pull_requests: vec![pr(22889, Some("Smaller MCP payloads"))],
        ..thread("Trim model lists")
    };
    for query in ["#22889", "22889", "trim mcp", "payloads t3code", "models trim"] {
        assert!(score(&fold_search_query(query), &subject).is_some(), "{query}");
    }
    assert!(score("trim lakebed", &subject).is_none());
}

#[test]
fn forgives_one_typo_only_when_nothing_matches_exactly() {
    let threads = [thread("Hosting alternatives to Railway"), thread("Trim MCP model lists")];
    assert_eq!(rank_titles("railwya", &threads), ["Hosting alternatives to Railway"]);
    assert_eq!(rank_titles("trim mpc", &threads), ["Trim MCP model lists"]);
    assert_eq!(
        rank_titles("drga", &[thread("Restore drag handles"), thread("Organize indicators")]),
        ["Restore drag handles"]
    );
    let stats = ThreadSearchSubject { pull_requests: vec![pr(22189, None)], ..thread("Stats") };
    assert!(rank_titles("22889", &[stats]).is_empty());
    assert_eq!(
        rank_titles("t3coed", &[thread("t3code perf audit"), thread("Other")]),
        ["t3code perf audit"]
    );
    assert_eq!(rank_titles("oath2", &[thread("Fix oauth2 redirect")]), ["Fix oauth2 redirect"]);
    assert_eq!(
        rank_titles("ilde", &[thread("Typesafe App Builder"), thread("De-risk idle tunnel cleanup")]),
        ["De-risk idle tunnel cleanup", "Typesafe App Builder"]
    );
    assert!(rank_titles("ing", &[thread("Nightly build fix")]).is_empty());
    assert_eq!(rank_titles("srv", &[thread("Server restart"), thread("Other")]), ["Server restart"]);
    assert_eq!(rank_titles("sidb", &[thread("Fix sidebar lag")]), ["Fix sidebar lag"]);
    assert!(rank_titles("cron", &[thread("Config cleanup")]).is_empty());
    assert_eq!(rank_titles("fix the sidebar", &[thread("Fix Sidebar Lag")]), ["Fix Sidebar Lag"]);
    assert!(rank_titles("the", &[thread("Fix sidebar lag")]).is_empty());
    assert_eq!(rank_titles("don't", &[thread("Don’t merge yet")]), ["Don’t merge yet"]);
    assert_eq!(
        rank_titles("githbu", &[thread("GitHub API usage"), thread("Other")]),
        ["GitHub API usage"]
    );
    assert_eq!(
        rank_titles("macso", &[thread("Fix macOS window"), thread("Other")]),
        ["Fix macOS window"]
    );
    assert_eq!(
        rank_titles("cmd enter", &[thread("Fix Cmd+Enter"), thread("Open links external")]),
        ["Fix Cmd+Enter"]
    );
}

#[test]
fn returns_for_a_hash_and_a_space_or_marks_that_fold_to_nothing() {
    assert_eq!(rank_titles("# fix", &[thread("Fix sidebar lag")]), ["Fix sidebar lag"]);
    assert!(rank_titles("#", &[thread("Fix sidebar lag")]).is_empty());
    assert!(rank_titles("´´´", &[thread("Fix sidebar lag")]).is_empty());
    assert!(score_search_terms("# fix", &["Fix sidebar"], false).is_none());
    assert!(find_search_token_ranges("Fix sidebar", "´´´").is_empty());
}

#[test]
fn leaves_out_a_row_whose_words_only_meet_across_unrelated_fields() {
    let scattered = ThreadSearchSubject {
        pull_requests: vec![pr(22645, Some("fix model picker")), pr(22649, Some("CI speedups"))],
        ..thread("eMudhra certificate update request")
    };
    assert!(score("fix ci", &scattered).is_none());
    let lakebed = ThreadSearchSubject {
        project_title: Some("lakebed".into()),
        pull_requests: vec![pr(243, Some("perf: faster cold starts"))],
        ..thread("Weekly notes")
    };
    let found = score("lakebed perf", &lakebed).and_then(|m| m.explain(&lakebed));
    let expected = SearchMatchHint {
        label: "PR",
        text: "#243 perf: faster cold starts".into(),
        ranges: vec![range(5, 9)],
    };
    assert_eq!(found, Some(expected));
}

#[test]
fn says_which_field_matched_when_the_title_does_not() {
    let subject = ThreadSearchSubject {
        branch: Some("t3/cleanup-payloads".into()),
        pull_requests: vec![pr(22889, Some("Smaller MCP payloads"))],
        ..thread("Trim model lists")
    };
    let explain =
        |query: &str| score(&fold_search_query(query), &subject).and_then(|m| m.explain(&subject));
    for query in ["#22889", "pr 22889", "pr22889", "pull request #22889"] {
        let expected = SearchMatchHint {
            label: "PR",
            text: "#22889 Smaller MCP payloads".into(),
            ranges: vec![range(1, 6)],
        };
        assert_eq!(explain(query), Some(expected), "{query}");
    }
    let branch =
        SearchMatchHint { label: "Branch", text: "cleanup-payloads".into(), ranges: vec![range(0, 7)] };
    assert_eq!(explain("cleanup"), Some(branch));
    assert_eq!(explain("trim"), None);
}

#[test]
fn reads_letters_and_numbers_typed_together_without_forgiving_typos_in_numbers() {
    let threads =
        [thread("Upgrade to pnpm 12"), thread("GPT-5 eval"), thread("iOS 26 support"), thread("Other")];
    assert_eq!(rank_titles("pnpm12", &threads), ["Upgrade to pnpm 12"]);
    assert_eq!(rank_titles("gpt5", &threads), ["GPT-5 eval"]);
    assert_eq!(rank_titles("ios26", &threads), ["iOS 26 support"]);
    assert!(rank_titles("pnpm13", &threads).is_empty());
}

#[test]
fn reads_whole_words_typed_together_but_not_scattered_letters() {
    let threads = [
        thread("VS Code extension settings"),
        thread("Fix UI bug in sidebar"),
        thread("I plan to open source all of lakebed in the next few weeks"),
    ];
    assert_eq!(rank_titles("vscode", &threads), ["VS Code extension settings"]);
    assert_eq!(rank_titles("fixuibug", &threads), ["Fix UI bug in sidebar"]);
    assert!(rank_titles("lint", &threads).is_empty());
    assert!(rank_titles("inth", &[thread("Show skill names in thread overview")]).is_empty());
    assert!(rank_titles("tos", &[thread("Move Client State to Server")]).is_empty());
    assert_eq!(rank_titles("t3co", &[thread("T3 Code vs Gemini")]), ["T3 Code vs Gemini"]);
    assert_eq!(rank_titles("\"sidebar lag\"", &[thread("Fix sidebar lag")]), ["Fix sidebar lag"]);
    assert_eq!(rank_titles("cafe\u{301}", &[thread("Café menu")]), ["Café menu"]);
    assert_eq!(rank_titles("gomod", &[thread("Migrate to Go modules")]), ["Migrate to Go modules"]);
    assert_eq!(score("vscode", &thread("Fixvscode and VS Code")).map(|m| m.quality), Some(0.6));
    let threads = [
        thread("VS Code remote support"),
        ThreadSearchSubject {
            branch: Some("vscode".into()),
            activity_at: Some(NOW - 400 * DAY),
            ..thread("Old work")
        },
    ];
    assert!(rank_titles("vscode", &threads).contains(&"VS Code remote support".to_owned()));
}

#[test]
fn stores_picked_queries_without_splitting_an_emoji() {
    assert_eq!(picked_query(&format!("{}🚀", "a".repeat(31))), "a".repeat(31));
    assert_eq!(picked_query("  Upl "), "upl");
    let spaced = "investigate    workspace thread reset bug";
    assert_eq!(picked_query(spaced), picked_query(&fold_search_query(spaced)));
}

#[test]
fn matches_emoji_in_titles() {
    assert_eq!(
        rank_titles("🚀", &[thread("🚀 Launch checklist"), thread("Launch notes")]),
        ["🚀 Launch checklist"]
    );
}

#[test]
fn treats_each_chinese_or_japanese_character_as_a_word_start() {
    assert!(score("搜索", &thread("实现全文搜索")).is_some());
    assert!(score("検索", &thread("新しい検索機能")).is_some());
}

#[test]
fn keeps_a_whole_word_on_a_quiet_thread_above_a_mid_word_hit_on_a_thread_opened_daily() {
    let mut stats = record_thread_open(None, NOW - 60 * DAY, None);
    for day in (1..=59).rev() {
        stats = record_thread_open(Some(&stats), NOW - day * DAY, None);
    }
    let threads = [
        ThreadSearchSubject {
            activity_at: Some(NOW),
            opens: Some(stats),
            ..thread("Fix oauth redirect")
        },
        ThreadSearchSubject {
            activity_at: Some(NOW - 90 * DAY),
            ..thread("Share auth across worktrees")
        },
    ];
    assert_eq!(rank_titles("auth", &threads), ["Share auth across worktrees", "Fix oauth redirect"]);
}

#[test]
fn reads_a_word_and_the_start_of_the_next_typed_together_highlighting_each_part() {
    let threads = [thread("Fix sidebar lag"), thread("Unrelated work")];
    assert_eq!(rank_titles("fixsid", &threads), ["Fix sidebar lag"]);
    assert_eq!(
        rank_titles("fixsid", &[thread("Feature: Fix sidebar lag")]),
        ["Feature: Fix sidebar lag"]
    );
    assert_eq!(score("fixsid", &threads[0]).map(|m| m.ranges), Some(vec![range(0, 3), range(4, 7)]));
    assert!(rank_titles("fsl", &threads).is_empty());
}

#[test]
fn finds_nothing_rather_than_an_unrelated_thread_when_no_word_matches() {
    let threads = [
        "Projectless threads without Git",
        "Concern about connection setup",
        "Simplify worktree thread creation",
        "Start a new conversation",
        "Setting up the sentinel",
        "I plan to open source all of lakebed in the next few weeks",
        "Design notes",
        "Link pull request to thread",
    ]
    .map(thread);
    for query in ["posthog", "cron", "stripe", "gpu", "fxsb", "lint", "deno"] {
        assert!(rank_titles(query, &threads).is_empty(), "{query}");
    }
}

#[test]
fn matches_pieces_of_references_that_hold_a_number_and_environment_names() {
    let subject = ThreadSearchSubject {
        id: Some("thread-5fe6f59c-4976".into()),
        environment_label: Some("BeeBox".into()),
        pull_requests: vec![ThreadPullRequest {
            number: 22889,
            title: None,
            references: vec![
                "pingdotgg/t3code#22889".into(),
                "https://github.com/pingdotgg/t3code/pull/22889".into(),
            ],
        }],
        ..thread("Trim model lists")
    };
    for query in [
        "t3code#22889",
        "github.com/pingdotgg/t3code/pull/22889",
        "https://github.com/pingdotgg/t3code/pull/22889/files",
        "5fe6f59c",
        "beebox",
    ] {
        assert!(score(&fold_search_query(query), &subject).is_some(), "{query}");
    }
    assert!(score("github.com", &subject).is_none());
    for query in ["github.com/pingdotgg/t3code/pull/2288", "t3code#2288"] {
        assert!(score(&fold_search_query(query), &subject).is_none(), "{query}");
    }
}

struct Hit {
    key: &'static str,
    quality: f64,
    hits: Option<u64>,
}

impl MessageSearchHit for Hit {
    fn quality(&self) -> Option<f64> {
        Some(self.quality)
    }
    fn hits(&self) -> Option<u64> {
        self.hits
    }
    fn message_created_at(&self) -> Option<&str> {
        None
    }
}

fn hit(key: &'static str, quality: f64) -> Hit {
    Hit { key, quality, hits: None }
}

fn rank_hits(
    hits: &[Hit],
    subjects: &HashMap<&str, ThreadSearchSubject>,
    query: &str,
    options: MessageRankOptions,
) -> Vec<&'static str> {
    rank_message_matches(hits, |hit| subjects.get(hit.key), query, NOW, options)
        .iter()
        .map(|hit| hit.key)
        .collect()
}

#[test]
fn merges_environments_by_word_match_then_picks_then_quality_and_use() {
    let mut subjects = HashMap::from([
        ("a-word", thread("A word")),
        ("a-word-2", thread("A word too")),
        ("b-prefix", thread("B prefix")),
        ("b-recent", ThreadSearchSubject { activity_at: Some(NOW), ..thread("B recent") }),
        (
            "b-picked",
            ThreadSearchSubject {
                opens: opens(NOW - DAY, 1.0, &[("needle", NOW - DAY)]),
                ..thread("B picked")
            },
        ),
    ]);
    let mut hits = vec![
        hit("a-word", 1.0),
        hit("a-word-2", 1.0),
        hit("b-prefix", 0.8),
        hit("b-recent", 1.0),
        hit("b-picked", 0.8),
    ];
    assert_eq!(
        rank_hits(&hits, &subjects, "needle", MessageRankOptions::default()),
        ["b-picked", "b-recent", "a-word", "a-word-2", "b-prefix"]
    );
    subjects.insert("c-titled", thread("Needle cleanup"));
    hits.insert(0, hit("c-titled", 1.0));
    let options = MessageRankOptions { without_title_matches: true, ..Default::default() };
    assert!(!rank_hits(&hits, &subjects, "needle", options).contains(&"c-titled"));
}

#[test]
fn ranks_a_thread_that_keeps_using_the_word_above_a_passing_mention_of_equal_quality() {
    let subjects = HashMap::from([
        (
            "mention",
            ThreadSearchSubject {
                activity_at: Some(NOW - 3 * DAY),
                opens: opens(NOW, 1.0, &[]),
                ..thread("Mention")
            },
        ),
        ("about", ThreadSearchSubject { activity_at: Some(NOW - 48 * DAY), ..thread("About it") }),
    ]);
    let hits = [
        Hit { key: "mention", quality: 1.0, hits: Some(1) },
        Hit { key: "about", quality: 1.0, hits: Some(21) },
    ];
    assert_eq!(
        rank_hits(&hits, &subjects, "tailscale", MessageRankOptions::default()),
        ["about", "mention"]
    );
}

#[test]
fn keeps_matches_inside_words_below_word_matches_however_used() {
    let busy =
        ThreadSearchSubject { activity_at: Some(NOW), opens: opens(NOW, 20.0, &[]), ..thread("Busy") };
    let subjects = HashMap::from([("busy", busy), ("quiet", thread("Quiet"))]);
    let hits = [hit("busy", 0.3), hit("quiet", 1.0)];
    assert_eq!(rank_hits(&hits, &subjects, "rust", MessageRankOptions::default()), ["quiet", "busy"]);
}

#[test]
fn orders_groups_by_their_best_match_and_keeps_the_given_order_on_ties() {
    let groups = [
        ("actions", vec!["Open settings", "New thread"]),
        ("threads", vec!["Settings sync rewrite", "Thread settings"]),
        ("projects", vec!["Settings app"]),
    ];
    let ranked = rank_search_groups(
        &groups,
        |(_, items)| items,
        |item, typos, _| score_search_terms("settings", &[item], typos),
    );
    let names: Vec<(&str, Vec<&str>)> =
        ranked.iter().map(|g| (g.group.0, g.ranked.iter().map(|entry| *entry.item).collect())).collect();
    assert_eq!(
        names,
        [
            ("actions", vec!["Open settings"]),
            ("threads", vec!["Settings sync rewrite", "Thread settings"]),
            ("projects", vec!["Settings app"]),
        ]
    );
}

#[test]
fn matches_inside_a_word_only_in_thread_titles() {
    assert!(score_search_terms("arch", &["Search project contents"], false).is_none());
    let notes =
        ThreadSearchSubject { branch: Some("research-extension-system".into()), ..thread("Notes") };
    assert!(score("arch", &notes).is_none());
    assert_eq!(score("arch", &thread("Research notes")).map(|m| m.quality), Some(0.3));
}

#[test]
fn forgives_typos_in_names_but_not_in_hidden_keywords() {
    let groups = [
        ("actions", vec![vec!["Open settings", "preferences", "keybindings"]]),
        ("settings", vec![vec!["Keybindings", "General"]]),
    ];
    let ranked = rank_search_groups(
        &groups,
        |(_, items)| items,
        |terms, typos, _| score_search_terms("keybindngs", terms, typos),
    );
    assert_eq!(ranked.iter().map(|g| g.group.0).collect::<Vec<_>>(), ["settings"]);
    assert!(score_search_terms("duckdb", &["Search engine", "duckduckgo"], true).is_none());
    assert!(score_search_terms("rust", &["GitHub sharing", "trusted"], false).is_none());
    assert!(score_search_terms("the ci", &["Context window indicator", "circle"], false).is_none());
}

enum Item {
    Command(Vec<&'static str>),
    Thread(Box<ThreadSearchSubject>),
}

#[test]
fn lists_a_thread_named_for_the_query_above_a_command_with_it_as_a_keyword() {
    let rank = |command: Vec<&'static str>, subject: ThreadSearchSubject| {
        let groups = [
            ("actions", vec![Item::Command(command)]),
            ("threads", vec![Item::Thread(Box::new(subject))]),
        ];
        let ranked = rank_search_groups(
            &groups,
            |(_, items)| items,
            |item, typos, _| match item {
                Item::Command(terms) => score_search_terms("mcp", terms, typos),
                Item::Thread(subject) => {
                    score_thread_search("mcp", subject, NOW, ThreadSearchOptions { typos, picks: true })
                }
            },
        );
        ranked.iter().map(|g| g.group.0).collect::<Vec<_>>()
    };
    let named = || thread("Trim MCP model lists");
    let picked = ThreadSearchSubject { opens: opens(NOW, 1.0, &[("mcp", NOW)]), ..named() };
    assert_eq!(rank(vec!["Restart agent session", "reload", "mcp"], named()), ["threads", "actions"]);
    assert_eq!(rank(vec!["Reload MCP servers"], named()), ["actions", "threads"]);
    assert_eq!(rank(vec!["Reload MCP servers"], picked), ["actions", "threads"]);
}

fn pick(query: &str, at: i64) -> ThreadPick {
    ThreadPick { query: query.into(), at }
}

#[test]
fn decays_the_open_count_merges_quick_reopens_and_keeps_the_latest_picked_queries() {
    let first = record_thread_open(None, NOW, Some("  Upl "));
    assert_eq!(first, ThreadOpenStats { opened_at: NOW, score: 1.0, picks: vec![pick("upl", NOW)] });
    let reopened = record_thread_open(Some(&first), NOW + 60_000, Some("uploadthing"));
    let picks = vec![pick("uploadthing", NOW + 60_000), pick("upl", NOW)];
    assert_eq!(reopened, ThreadOpenStats { opened_at: NOW, score: 1.0, picks });
    let long = "x".repeat(50);
    let later = record_thread_open(Some(&reopened), NOW + SEARCH_RECENCY_HALF_LIFE_MS, Some(&long));
    assert_eq!(later.score, 1.5);
    let queries: Vec<&str> = later.picks.iter().map(|p| p.query.as_str()).collect();
    assert_eq!(queries, ["x".repeat(32).as_str(), "uploadthing", "upl"]);
    let repicked = record_thread_open(Some(&later), later.opened_at + DAY, Some("upl"));
    assert_eq!(
        repicked.picks,
        [
            pick("upl", later.opened_at + DAY),
            pick(&"x".repeat(32), later.opened_at),
            pick("uploadthing", NOW + 60_000)
        ]
    );
}

#[test]
fn drops_picks_that_no_longer_count_when_the_thread_is_opened_again() {
    let stats = opens(NOW - 30 * DAY, 1.0, &[("old", NOW - 30 * DAY)]);
    assert_eq!(record_thread_open(stats.as_ref(), NOW, Some("new")).picks, [pick("new", NOW)]);
}

#[test]
fn does_not_renew_a_querys_pick_when_the_thread_is_picked_for_another_one() {
    let deploy = [
        ThreadSearchSubject {
            opens: opens(NOW, 2.0, &[("scripts", NOW), ("deploy", NOW - 60 * DAY)]),
            ..thread("Deploy notes")
        },
        ThreadSearchSubject {
            opens: opens(NOW - DAY, 1.0, &[("deploy", NOW - DAY)]),
            ..thread("Deploy timeout")
        },
    ];
    assert_eq!(rank_titles("deploy", &deploy), ["Deploy timeout", "Deploy notes"]);
}

#[test]
fn maps_folded_matches_back_to_the_source_text_and_skips_short_tokens() {
    assert_eq!(
        find_search_token_ranges("Le Café résumé ci", "CAFE resume ci"),
        [range(3, 7), range(8, 14)]
    );
}

#[test]
fn highlights_a_three_letter_token_only_where_it_starts_a_word() {
    assert_eq!(find_search_token_ranges("drafts, enableFTS", "fts"), [range(14, 17)]);
    assert_eq!(find_search_token_ranges("Open handlers.ts", ".ts"), [range(13, 16)]);
}

#[test]
fn splits_text_at_char_ranges() {
    let parts = split_search_text("Le Café résumé", &[range(3, 7)]);
    let texts: Vec<(&str, bool, usize)> =
        parts.iter().map(|p| (p.text, p.highlighted, p.start)).collect();
    assert_eq!(texts, [("Le ", false, 0), ("Café", true, 3), (" résumé", false, 7)]);
    assert_eq!(split_search_text("", &[]).len(), 1);
}
