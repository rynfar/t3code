use super::*;

const NOW: i64 = 1_792_713_600_000;
const ENV: &str = "environment-local";
const PROJECT: &str = "project-1";

fn make_thread(id: &str, title: &str) -> PaletteThread {
    PaletteThread {
        environment_id: ENV.into(),
        id: id.into(),
        project_id: PROJECT.into(),
        title: title.into(),
        created_at: "2026-03-01T00:00:00.000Z".into(),
        updated_at: "2026-03-01T00:00:00.000Z".into(),
        ..Default::default()
    }
}

fn thread_items(threads: &[PaletteThread], project_titles: &[(&str, &str)]) -> Vec<CommandPaletteItem> {
    let titles =
        project_titles.iter().map(|(key, title)| ((*key).to_owned(), (*title).to_owned())).collect();
    build_thread_action_items(threads, None, &titles, |_| None)
}

fn action(value: &str, title: &str, search_terms: &[&str]) -> CommandPaletteItem {
    let search_terms = if search_terms.is_empty() {
        vec![title.into()]
    } else {
        search_terms.iter().map(|t| (*t).into()).collect()
    };
    CommandPaletteItem { value: value.into(), title: title.into(), search_terms, ..Default::default() }
}

fn group(value: &str, label: &str, items: Vec<CommandPaletteItem>) -> CommandPaletteGroup {
    CommandPaletteGroup { value: value.into(), label: label.into(), items }
}

fn search(input: CommandPaletteSearch<'_>) -> Vec<CommandPaletteGroup> {
    filter_command_palette_groups(&CommandPaletteSearch {
        now: if input.now == 0 { NOW } else { input.now },
        ..input
    })
}

fn labels(groups: &[CommandPaletteGroup]) -> Vec<&str> {
    groups.iter().map(|group| group.label.as_str()).collect()
}

fn values(groups: &[CommandPaletteGroup]) -> Vec<&str> {
    groups.iter().flat_map(|group| &group.items).map(|item| item.value.as_str()).collect()
}

#[test]
fn keeps_threads_apart_when_two_environments_share_a_thread_id() {
    let local = make_thread("thread-1", "Thread");
    let remote =
        PaletteThread { environment_id: "remote-copy".into(), ..make_thread("thread-1", "Thread") };
    let titles = HashMap::from([
        (format!("{ENV}:{PROJECT}"), "Local project".to_owned()),
        (format!("remote-copy:{PROJECT}"), "Remote project".to_owned()),
    ]);
    let items =
        build_thread_action_items(&[local, remote], Some(("remote-copy", "thread-1")), &titles, |_| None);
    assert_eq!(items.iter().map(|item| &item.value).collect::<HashSet<_>>().len(), 2);
    let remote_item =
        items.iter().find(|item| item.description.as_deref() == Some("Remote project · Current thread"));
    assert_eq!(remote_item.map(|item| item.value.as_str()), Some("thread:remote-copy:thread-1"));
}

#[test]
fn ranks_thread_title_matches_ahead_of_contextual_project_name_matches() {
    let threads = [
        PaletteThread {
            updated_at: "2026-03-20T00:00:00.000Z".into(),
            ..make_thread("thread-context-match", "Fix navbar spacing")
        },
        PaletteThread {
            created_at: "2026-03-02T00:00:00.000Z".into(),
            updated_at: "2026-03-19T00:00:00.000Z".into(),
            ..make_thread("thread-title-match", "Project kickoff notes")
        },
    ];
    let items = thread_items(&threads, &[("environment-local:project-1", "Project")]);
    let groups = search(CommandPaletteSearch {
        query: "project",
        thread_search_items: &items,
        ..Default::default()
    });
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].value, "threads-search");
    assert_eq!(
        values(&groups),
        ["thread:environment-local:thread-title-match", "thread:environment-local:thread-context-match"]
    );
}

#[test]
fn searches_submenu_options_from_the_root_and_from_a_submenu_with_no_match() {
    let projects =
        vec![action("new-thread-in:t3code", "t3code", &[]), action("new-thread-in:web", "web", &[])];
    let view =
        CommandPaletteView { groups: vec![group("projects", "Projects", projects)], initial_query: None };
    let new_thread_in = CommandPaletteItem {
        kind: CommandPaletteItemKind::Submenu(view.clone()),
        ..action("action:new-thread-in", "New thread in...", &["new thread"])
    };
    let add_project = action("action:add-project", "Add project", &[]);
    let root_groups = vec![group("actions", "Actions", vec![new_thread_in.clone(), add_project.clone()])];
    let threads = thread_items(&[make_thread("thread-1", "Thread about t3code")], &[]);
    let run = |query: &str, is_in_submenu: bool| {
        let nested_items =
            build_nested_search_items(&[new_thread_in.clone(), add_project.clone()], query);
        let groups = search(CommandPaletteSearch {
            active_groups: if is_in_submenu { &view.groups } else { &root_groups },
            query,
            is_in_submenu,
            all_options: Some(CommandPaletteAllOptions {
                root_groups: &root_groups,
                nested_items: &nested_items,
            }),
            thread_search_items: &threads,
            ..Default::default()
        });
        groups
            .iter()
            .map(|group| {
                (
                    group.label.clone(),
                    group.items.iter().map(|item| item.title.clone()).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        run("new thread in t3code", false)[0],
        ("Suggestions".to_owned(), vec!["New thread in t3code".to_owned()])
    );
    assert!(!run("new thread", false).iter().any(|(label, _)| label == "Suggestions"));
    assert_eq!(run("t3", true), [("Projects".to_owned(), vec!["t3code".to_owned()])]);
    assert_eq!(run("add project", true), [("Actions".to_owned(), vec!["Add project".to_owned()])]);
}

#[test]
fn keeps_an_active_thread_first_while_its_title_is_typed_above_settings_and_actions() {
    let threads = [PaletteThread {
        latest_user_message_at: Some("2026-10-11T00:00:00.000Z".into()),
        ..make_thread("thread-1", "Compaction prompt timing and cache age")
    }];
    let items = thread_items(&threads, &[]);
    let actions = [group("actions", "Actions", vec![action("action:copy", "Copy thread ID", &[])])];
    let settings = [
        action("setting:compact", "Compact idle threads", &[]),
        action("setting:providers", "Providers", &["Providers", "compatibility"]),
    ];
    for query in ["comp", "compa", "compac", "compact", "compacti", "compaction"] {
        let groups = search(CommandPaletteSearch {
            active_groups: &actions,
            query,
            settings_search_items: &settings,
            thread_search_items: &items,
            ..Default::default()
        });
        assert_eq!(labels(&groups).first(), Some(&"Threads"), "{query}");
    }
}

#[test]
fn orders_groups_by_their_best_match_so_a_strong_thread_is_not_under_a_weak_action() {
    let items = thread_items(&[make_thread("thread-1", "Upgrade to pnpm 12")], &[]);
    let actions = [group("actions", "Actions", vec![action("action:upload", "Upload file", &[])])];
    let run = |query: &str| {
        let groups = search(CommandPaletteSearch {
            active_groups: &actions,
            query,
            thread_search_items: &items,
            ..Default::default()
        });
        labels(&groups).into_iter().map(String::from).collect::<Vec<_>>()
    };
    assert_eq!(run("upgrade"), ["Threads"]);
    assert_eq!(run("upl"), ["Actions"]);
    let prs = [group(
        "actions",
        "Actions",
        vec![
            action(
                "action:open-prs",
                "Open pull requests",
                &["Open pull requests", "pr", "prs", "review"],
            ),
            action("action:project", "Project settings", &[]),
        ],
    )];
    let groups = search(CommandPaletteSearch { active_groups: &prs, query: "pr", ..Default::default() });
    assert_eq!(values(&groups), ["action:open-prs", "action:project"]);
    assert_eq!(run("up"), ["Actions", "Threads"]);
    assert_eq!(run("pnpm up"), ["Threads"]);
}

#[test]
fn runs_the_command_for_its_whole_name_over_a_thread_with_that_title_and_lists_the_open_thread_last() {
    let threads = [
        make_thread("thread-new", "New thread"),
        make_thread("thread-open", "New thread layout"),
        make_thread("thread-other", "New thread button"),
    ];
    let items =
        build_thread_action_items(&threads, Some((ENV, "thread-open")), &HashMap::new(), |_| None);
    let actions =
        [group("actions", "Actions", vec![action("action:new-thread", "New thread in t3code", &[])])];
    let groups = search(CommandPaletteSearch {
        active_groups: &actions,
        query: "new thread",
        thread_search_items: &items,
        ..Default::default()
    });
    assert_eq!(labels(&groups), ["Actions", "Threads"]);
    assert_eq!(groups[1].items.last().map(|item| item.title.as_str()), Some("New thread layout"));
}

#[test]
fn lists_a_project_above_threads_that_only_hold_its_name_when_the_name_is_typed() {
    let items = thread_items(&[make_thread("thread-1", "Lakebed performance gains")], &[]);
    let projects = [action("project:lakebed", "Lakebed", &["Lakebed", "/code/lakebed"])];
    let run = |query: &str| {
        let groups = search(CommandPaletteSearch {
            query,
            project_search_items: &projects,
            thread_search_items: &items,
            ..Default::default()
        });
        labels(&groups).into_iter().map(String::from).collect::<Vec<_>>()
    };
    assert_eq!(run("lakebed"), ["Projects", "Threads"]);
    assert_eq!(run("lake"), ["Threads", "Projects"]);
}

#[test]
fn keeps_a_command_first_over_a_thread_in_daily_use_that_matches_as_well() {
    let day_before = NOW - 24 * 60 * 60 * 1000;
    let threads = [PaletteThread {
        latest_user_message_at: Some("2026-10-22T00:00:00.000Z".into()),
        opens: Some(ThreadOpenStats { opened_at: day_before, score: 7.0, picks: Vec::new() }),
        ..make_thread("thread-1", "New onboarding flow")
    }];
    let items = thread_items(&threads, &[]);
    let actions = [group(
        "actions",
        "Actions",
        vec![action("action:new-thread", "New thread", &["new thread", "chat"])],
    )];
    let groups = search(CommandPaletteSearch {
        active_groups: &actions,
        query: "new",
        thread_search_items: &items,
        ..Default::default()
    });
    assert_eq!(labels(&groups), ["Actions", "Threads"]);
}

#[test]
fn ranks_an_order_independent_setting_title_match_above_a_split_context_match() {
    let settings = [
        action("setting:context-match", "Context match", &["Pairing settings", "remote backend"]),
        action("setting:remote-pairing", "Remote pairing", &["Remote pairing", "connections"]),
    ];
    let groups = search(CommandPaletteSearch {
        query: "pairing remote",
        settings_search_items: &settings,
        ..Default::default()
    });
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].value, "settings-search");
    assert_eq!(values(&groups), ["setting:remote-pairing", "setting:context-match"]);
}

#[test]
fn searches_actions_only_after_a_leading_angle_bracket() {
    let actions = [group(
        "actions",
        "Actions",
        vec![action("action:new-thread", "New thread in t3code", &["new thread", "chat"])],
    )];
    let groups = search(CommandPaletteSearch {
        active_groups: &actions,
        query: "> new t3code",
        ..Default::default()
    });
    assert_eq!(values(&groups), ["action:new-thread"]);
}

#[test]
fn forgives_a_typo_in_a_setting_name_when_nothing_matches_exactly() {
    let settings =
        [action("setting:keybindings", "Keybindings", &["Keybindings", "General", "shortcuts"])];
    let groups = search(CommandPaletteSearch {
        query: "keybindngs",
        settings_search_items: &settings,
        ..Default::default()
    });
    assert_eq!(values(&groups), ["setting:keybindings"]);
}

#[test]
fn normalizes_case_independently_of_the_host_locale() {
    let settings = [action("setting:version-control", "Version control", &["git"])];
    let groups = search(CommandPaletteSearch {
        query: "GIT",
        settings_search_items: &settings,
        ..Default::default()
    });
    assert_eq!(values(&groups), ["setting:version-control"]);
}

#[test]
fn surfaces_threads_when_the_query_is_their_id_without_outranking_title_matches() {
    let threads = [
        PaletteThread {
            updated_at: "2026-03-05T00:00:00.000Z".into(),
            ..make_thread("thread-alpha-1234", "Unrelated work")
        },
        PaletteThread {
            updated_at: "2026-03-04T00:00:00.000Z".into(),
            ..make_thread("thread-other-9999", "Fix thread-alpha-1234 flakes")
        },
    ];
    let items = thread_items(&threads, &[("environment-local:project-1", "T3 Code")]);
    let groups = search(CommandPaletteSearch {
        query: "  THREAD-ALPHA-1234  ",
        thread_search_items: &items,
        ..Default::default()
    });
    assert_eq!(
        values(&groups),
        ["thread:environment-local:thread-other-9999", "thread:environment-local:thread-alpha-1234"]
    );
}

#[test]
fn hides_subagent_threads_but_keeps_forks() {
    let threads = [
        make_thread("parent", "Parent"),
        PaletteThread { hidden: true, ..make_thread("subagent", "Subagent") },
        make_thread("fork", "Fork"),
    ];
    let mut listed: Vec<String> =
        thread_items(&threads, &[]).into_iter().map(|item| item.value).collect();
    listed.sort();
    assert_eq!(listed, ["thread:environment-local:fork", "thread:environment-local:parent"]);
}

#[test]
fn ranks_word_start_title_matches_above_newer_mid_word_matches() {
    let threads = [
        PaletteThread {
            updated_at: "2026-03-20T00:00:00.000Z".into(),
            ..make_thread("mid-word", "Fix oauth redirect")
        },
        PaletteThread {
            updated_at: "2026-03-02T00:00:00.000Z".into(),
            ..make_thread("word-start", "Clean up auth tokens")
        },
        PaletteThread {
            branch: Some("feature/auth-cleanup".into()),
            updated_at: "2026-03-01T00:00:00.000Z".into(),
            ..make_thread("branch-word-start", "Refactor login")
        },
    ];
    let items = thread_items(&threads, &[]);
    let groups =
        search(CommandPaletteSearch { query: "auth", thread_search_items: &items, ..Default::default() });
    assert_eq!(
        values(&groups[..1]),
        [
            "thread:environment-local:word-start",
            "thread:environment-local:branch-word-start",
            "thread:environment-local:mid-word"
        ]
    );
}

#[test]
fn breaks_ties_between_equal_matches_by_latest_user_message() {
    let threads = [
        PaletteThread {
            updated_at: "2026-03-20T00:00:00.000Z".into(),
            latest_user_message_at: Some("2025-09-01T00:00:00.000Z".into()),
            ..make_thread("prefix-stale", "Auth refactor")
        },
        PaletteThread {
            updated_at: "2026-03-10T00:00:00.000Z".into(),
            latest_user_message_at: Some("2026-03-10T00:00:00.000Z".into()),
            ..make_thread("word-fresh", "Fix auth refresh")
        },
    ];
    let items = thread_items(&threads, &[]);
    let now = parse_iso_ms("2026-03-20T00:00:00.000Z").unwrap_or_default();
    let groups = search(CommandPaletteSearch {
        query: "auth",
        thread_search_items: &items,
        now,
        ..Default::default()
    });
    assert_eq!(
        values(&groups[..1]),
        ["thread:environment-local:word-fresh", "thread:environment-local:prefix-stale"]
    );
}

#[test]
fn finds_linked_threads_from_pr_queries() {
    let linked = PaletteThread {
        pull_requests: vec![ThreadPullRequest {
            number: 10839,
            title: None,
            references: vec![
                "pingdotgg/t3code#10839".into(),
                "https://github.com/pingdotgg/t3code/pull/10839".into(),
            ],
        }],
        ..make_thread("thread-1", "Implementation")
    };
    let items = thread_items(&[linked, make_thread("unrelated", "Other work")], &[]);
    for query in
        ["#10839", "10839", "pingdotgg/t3code#10839", "https://github.com/pingdotgg/t3code/pull/10839"]
    {
        let groups =
            search(CommandPaletteSearch { query, thread_search_items: &items, ..Default::default() });
        let titles: Vec<&str> =
            groups.iter().flat_map(|group| &group.items).map(|item| item.title.as_str()).collect();
        assert_eq!(titles, ["Implementation"], "{query}");
    }
}

#[test]
fn lists_recent_threads_by_last_use_without_the_open_thread() {
    let threads: Vec<PaletteThread> = (0..15)
        .map(|day| PaletteThread {
            updated_at: format!("2026-03-{:02}T00:00:00.000Z", day + 1),
            ..make_thread(&format!("t{day}"), "Thread")
        })
        .collect();
    let items = thread_items(&threads, &[]);
    let recent = recent_thread_items(&items, Some("thread:environment-local:t14"));
    let ids: Vec<&str> =
        recent.iter().map(|item| item.value.trim_start_matches("thread:environment-local:")).collect();
    assert_eq!(ids, ["t13", "t12", "t11", "t10", "t9", "t8", "t7", "t6", "t5", "t4", "t3", "t2"]);
}

#[test]
fn keyboard_order_skips_disabled_rows() {
    let disabled = CommandPaletteItem { disabled: true, ..action("b", "B", &[]) };
    let groups =
        [group("actions", "Actions", vec![action("a", "A", &[]), disabled, action("c", "C", &[])])];
    let rows = build_command_palette_rows(&groups);
    assert_eq!(rows.item_values, ["a", "c"]);
    assert_eq!(rows.row_index_by_item_index, [1, 3]);
    assert!(find_highlighted_command_palette_item(&groups, Some("b")).is_none());
    assert_eq!(
        find_highlighted_command_palette_item(&groups, Some("c")).map(|item| item.title.as_str()),
        Some("C")
    );
}

#[test]
fn restores_each_parent_query_when_backing_out_of_nested_pickers() {
    use CommandPaletteSearchAction::*;
    let picker = CommandPaletteView::default();
    let mut state = create_command_palette_search_state(">new thread");
    state = reduce_command_palette_search_state(state, Push(picker.clone()));
    assert_eq!(state.query, "");
    state = reduce_command_palette_search_state(state, Query("remote project".into()));
    state = reduce_command_palette_search_state(
        state,
        Push(CommandPaletteView { initial_query: Some("~/".into()), ..picker }),
    );
    assert_eq!(state.query, "~/");
    state = reduce_command_palette_search_state(state, Query(String::new()));
    state = reduce_command_palette_search_state(state, Back);
    assert_eq!((state.query.as_str(), state.views.len()), ("remote project", 1));
    state = reduce_command_palette_search_state(state, Back);
    assert_eq!((state.query.as_str(), state.views.len()), (">new thread", 0));
}

#[test]
fn cannot_return_to_an_old_picker_after_a_new_search_replaces_it() {
    use CommandPaletteSearchAction::*;
    let mut state = reduce_command_palette_search_state(
        create_command_palette_search_state("old"),
        Push(CommandPaletteView::default()),
    );
    state = reduce_command_palette_search_state(state, Reset(Some("fresh".into())));
    state = reduce_command_palette_search_state(state, Back);
    assert_eq!((state.query.as_str(), state.views.len()), ("fresh", 0));
}

#[test]
fn parses_rfc3339_timestamps() {
    assert_eq!(parse_iso_ms("2026-10-23T00:00:00.000Z"), Some(NOW));
    assert_eq!(parse_iso_ms("2026-10-23T00:00:00Z"), Some(NOW));
    assert_eq!(parse_iso_ms("2026-10-23T02:00:00.5+02:00"), Some(NOW + 500));
    assert_eq!(parse_iso_ms("2026-10-22T19:30:00.123456-04:30"), Some(NOW + 123));
    assert_eq!(parse_iso_ms("1969-12-31T23:59:59.999Z"), Some(-1));
    for invalid in [
        "",
        "2026-10-23",
        "2026-02-29T00:00:00Z",
        "2026-10-23T24:00:00Z",
        "2026-10-23T00:00:00",
        "2026-10-23T00:00:00.Z",
    ] {
        assert_eq!(parse_iso_ms(invalid), None, "{invalid}");
    }
}
