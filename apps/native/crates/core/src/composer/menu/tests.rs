use super::*;
use serde_json::json;

fn trigger(kind: TriggerKind, query: &str, range_start: usize, range_end: usize) -> Option<Trigger> {
    Some(Trigger { kind, query: query.into(), range_start, range_end })
}

fn at_end(text: &str) -> Option<Trigger> {
    detect_trigger(text, text.len())
}

fn skill(name: &str) -> ProviderSkill {
    ProviderSkill { name: name.into(), path: format!("/tmp/{name}/SKILL.md"), enabled: true, ..Default::default() }
}

fn command(name: &str, description: Option<&str>) -> ProviderSlashCommand {
    ProviderSlashCommand { name: name.into(), description: description.map(str::to_owned), input: None }
}

fn labels(items: &[MenuItem]) -> Vec<&str> {
    items.iter().map(|item| item.label.as_str()).collect()
}

#[test]
fn slash_tokens_open_the_command_menu_but_paths_do_not() {
    for (text, cursor) in [("Run /tmp/build.sh", 17), ("/etc/hosts", 10), ("Use /etc/hosts now", "Use /et".len())] {
        assert_eq!(detect_trigger(text, cursor), None, "{text}");
    }
    for prefix in ["Use ", "Use\t", "Use\n  "] {
        let text = format!("{prefix}/skill:review later");
        let cursor = prefix.len() + "/skill:review".len();
        assert_eq!(detect_trigger(&text, cursor), trigger(TriggerKind::SlashCommand, "skill:review", prefix.len(), cursor));
    }
    for text in ["Use/review", "https://example.com/review", "Use /review ", "/model spark"] {
        assert_eq!(at_end(text), None, "{text}");
    }
    assert_eq!(at_end("/"), trigger(TriggerKind::SlashCommand, "", 0, 1));
}

#[test]
fn currency_amounts_are_not_skills() {
    for text in ["It costs $5", "budget $9.99", "only $.99", "about €20", "raise $10k", "raise $10k.", "is it $5?", "($5)"] {
        assert_eq!(at_end(text), None, "{text}");
    }
    assert_eq!(at_end("use $rev"), trigger(TriggerKind::Skill, "rev", 4, 8));
    let text = "try €review";
    assert_eq!(at_end(text), trigger(TriggerKind::Skill, "review", 4, text.len()));
    assert_eq!(at_end("$"), trigger(TriggerKind::Skill, "", 0, 1));
    assert_eq!(at_end("$5e"), trigger(TriggerKind::Skill, "5e", 0, 3));
}

#[test]
fn pull_requests_need_a_token_boundary() {
    let text = "Compare this with #8737";
    assert_eq!(at_end(text), trigger(TriggerKind::PullRequest, "8737", 18, text.len()));
    assert_eq!(at_end("# Heading"), None);
    assert_eq!(at_end("issue#123"), None);
    assert_eq!(at_end("#-x"), None);
}

#[test]
fn mentions_open_mid_text_and_at_multibyte_offsets() {
    let text = "Please inspect @in this sentence";
    let cursor = "Please inspect @".len();
    assert_eq!(detect_trigger(text, cursor), trigger(TriggerKind::Path, "", 15, cursor));
    let text = "héllo @src/ma";
    assert_eq!(at_end(text), trigger(TriggerKind::Path, "src/ma", "héllo ".len(), text.len()));
    // A caret inside a multibyte char snaps back to its start.
    assert_eq!(detect_trigger("@é", 2), trigger(TriggerKind::Path, "", 0, 1));
}

#[test]
fn escape_keeps_its_trigger_closed_until_the_token_changes() {
    let mut state = TriggerState::default();
    assert!(state.set(at_end("@sr")));
    state.dismiss(at_end("@sr"));
    assert_eq!(state.trigger(), None);
    assert!(!state.set(at_end("@src")), "typing on stays dismissed");
    assert!(state.set(at_end("@src @")), "a new token opens");
    state.dismiss(at_end("@src @"));
    state.set(None);
    assert!(state.set(at_end("@src @")), "leaving the token clears the dismissal");
}

#[test]
fn standalone_mode_commands_switch_instead_of_sending() {
    assert_eq!(parse_standalone_slash_command(" /plan "), Some(SlashCommand::Plan));
    assert_eq!(parse_standalone_slash_command("/DEFAULT"), Some(SlashCommand::Default));
    assert_eq!(parse_standalone_slash_command("/plan this"), None);
    assert_eq!(parse_standalone_slash_command("/model"), None);
}

#[test]
fn replacements_swallow_one_following_space() {
    let text = "see @sr please";
    let end = extend_range_for_trailing_space(text, 7, "[src](src) ");
    assert_eq!(end, 8);
    assert_eq!(replace_text_range(text, 4, end, "[src](src) "), ("see [src](src) please".to_owned(), 15));
    assert_eq!(extend_range_for_trailing_space(text, 7, "/model"), 7);
    assert_eq!(replace_text_range("ab", 5, 9, "c"), ("abc".to_owned(), 3));
}

#[test]
fn file_mentions_are_markdown_links_with_encoded_paths() {
    assert_eq!(serialize_file_link("src/main.rs"), "[main.rs](src/main.rs)");
    assert_eq!(serialize_file_link("docs/a b(1)#?.md"), "[a b(1)#?.md](docs/a%20b%281%29%23%3F.md)");
    assert_eq!(serialize_file_link("x/[draft].md"), "[\\[draft\\].md](x/%5Bdraft%5D.md)");
    assert_eq!(serialize_file_link("café/ü.txt"), "[ü.txt](caf%C3%A9/%C3%BC.txt)");
    assert_eq!(file_mention_text("apps"), "[apps](apps) ");
}

#[test]
fn slash_menu_lists_builtins_commands_then_skills() {
    let skills = [skill("review"), ProviderSkill { enabled: false, ..skill("off") }, skill("compact")];
    let commands = [command("compact", Some("Compact the thread")), command("init", None), command("review", None)];
    let options = SlashMenuOptions { plan_mode: true, side_chat: false, compact: true, show_skills: true };
    let items = slash_items(&at_end("/").unwrap(), "codex", &skills, &commands, options);
    // A command named like a visible skill shows as the skill.
    assert_eq!(labels(&items), ["/model", "/plan", "/default", "/init", "/skill:review", "/skill:compact"]);
    let options = SlashMenuOptions { show_skills: false, ..options };
    let items = slash_items(&at_end("/").unwrap(), "codex", &skills, &commands, options);
    assert_eq!(labels(&items), ["/model", "/plan", "/default", "/compact", "/init", "/review"]);
    assert_eq!(items[4].description, "Run provider command");
    let options = SlashMenuOptions { compact: false, plan_mode: false, ..options };
    let items = slash_items(&at_end("/").unwrap(), "codex", &skills, &commands, options);
    assert_eq!(labels(&items), ["/model", "/init", "/review"]);
}

#[test]
fn slash_menu_mid_prompt_offers_only_builtins_and_skills() {
    let options = SlashMenuOptions { plan_mode: true, side_chat: true, compact: true, show_skills: true };
    let items = slash_items(&at_end("fix this /").unwrap(), "codex", &[skill("review")], &[command("init", None)], options);
    assert_eq!(labels(&items), ["/model", "/plan", "/default", "/skill:review"]);
}

#[test]
fn slash_queries_rank_names_before_descriptions() {
    let options = SlashMenuOptions { plan_mode: true, show_skills: true, ..Default::default() };
    let commands = [command("permissions", Some("Manage plan approvals")), command("plugins", None)];
    let items = slash_items(&at_end("/pl").unwrap(), "claude", &[skill("planner")], &commands, options);
    // Equal scores fall back to the kind: commands before skills.
    assert_eq!(labels(&items), ["/plan", "/plugins", "/skill:planner", "/permissions"]);
    let items = slash_items(&at_end("/skill").unwrap(), "claude", &[skill("planner")], &commands, options);
    assert_eq!(labels(&items), ["/skill:planner"]);
    let items = slash_items(&at_end("/skill:pla").unwrap(), "claude", &[skill("planner")], &commands, options);
    assert_eq!(labels(&items), ["/skill:planner"]);
}

#[test]
fn skill_search_dedupes_and_ranks_fuzzy_matches() {
    let skills = [
        ProviderSkill { display_name: Some("Gh Fix Ci".into()), ..skill("gh-fix-ci") },
        ProviderSkill { display_name: Some("Github".into()), ..skill("github") },
        ProviderSkill { display_name: Some("Agent Browser".into()), ..skill("agent-browser") },
    ];
    assert_eq!(search_skills(&skills, "gfc").iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["gh-fix-ci"]);
    for prefix in ["$", "€", "₹", "𑿝"] {
        assert_eq!(search_skills(&[skill("review")], &format!("{prefix}review")).len(), 1, "{prefix}");
    }
    let duplicates = [
        ProviderSkill { path: "/a/.codex/skills/audit/SKILL.md".into(), ..skill("audit") },
        skill("browser"),
        ProviderSkill { path: "/a/.agents/skills/audit/SKILL.md".into(), ..skill("Audit") },
        ProviderSkill { user_invocable: Some(false), ..skill("agent-only") },
    ];
    let paths: Vec<String> = search_skills(&duplicates, "").into_iter().map(|s| s.path).collect();
    assert_eq!(paths, ["/a/.codex/skills/audit/SKILL.md", "/tmp/browser/SKILL.md"]);
}

#[test]
fn skill_rows_carry_their_source_and_description() {
    let skills = [
        ProviderSkill { short_description: Some("Review a diff".into()), scope: Some("repo".into()), ..skill("review") },
        ProviderSkill { scope: Some("user".into()), ..skill("notes") },
        ProviderSkill { path: "/u/.codex/plugins/x/SKILL.md".into(), ..skill("plugin") },
        skill("bare"),
    ];
    let items = skill_items(&at_end("$").unwrap(), "codex", &skills);
    let rows: Vec<(&str, &str)> = items.iter().map(|item| (item.label.as_str(), item.description.as_str())).collect();
    assert_eq!(rows, [("review", "Review a diff"), ("notes", "user skill"), ("plugin", "Run provider skill"), ("bare", "Run provider skill")]);
    let sources: Vec<SkillSource> = skills.iter().map(skill_source).collect();
    assert_eq!(sources, [SkillSource::Repo, SkillSource::Personal, SkillSource::App, SkillSource::Other]);
    assert_eq!(items[0].id, "skill:codex:review");
}

#[test]
fn workspace_snapshots_replace_the_global_lists_while_current() {
    let provider: ServerProvider = serde_json::from_value(json!({
        "instanceId": "claude", "driver": "claudeAgent", "enabled": true, "installed": true, "status": "ready",
        "skills": [{"name": "global", "path": "/g", "enabled": true}, {"name": "bad"}],
        "slashCommands": [{"name": "compact"}, {"description": "missing name"}],
    }))
    .unwrap();
    assert_eq!(provider.skills.len(), 1, "a malformed skill is skipped");
    assert_eq!(provider.slash_commands.len(), 1);
    let reply = json!({"providers": [{"instanceId": "claude", "workspaceSnapshots": [
        {"cwd": "/repo", "checkedAt": "2026-11-02T10:00:00.000Z", "slashCommands": [{"name": "deploy"}], "skills": []}
    ]}]});
    let fetched = refreshed_snapshot(&reply, "claude", "/repo").unwrap();
    assert_eq!(refreshed_snapshot(&reply, "claude", "/other"), None);
    let snapshot = workspace_snapshot(&provider, Some(&fetched), Some("/repo"));
    let (skills, commands) = skills_and_commands(&provider, snapshot);
    assert!(skills.is_empty());
    assert_eq!(commands[0].name, "deploy");
    let now = crate::sidebar::time::parse_iso("2026-11-02T10:01:00.000Z").unwrap();
    assert!(snapshot_is_current(snapshot, now));
    assert!(!snapshot_is_current(snapshot, now + WORKSPACE_SNAPSHOT_TTL_MS));
    assert_eq!(workspace_snapshot(&provider, Some(&fetched), Some("/elsewhere")), None);
    assert_eq!(skills_and_commands(&provider, None).1[0].name, "compact");
}

#[test]
fn path_rows_show_the_name_and_parent() {
    let reply = json!({"entries": [
        {"path": "src/composer/menu.rs", "kind": "file"},
        {"path": "src", "kind": "directory"},
        {"kind": "file"},
    ], "truncated": false});
    let items = path_items(&decode_entries(&reply));
    let rows: Vec<(&str, &str, &str)> = items.iter().map(|i| (i.id.as_str(), i.label.as_str(), i.description.as_str())).collect();
    assert_eq!(rows, [("path:file:src/composer/menu.rs", "menu.rs", "src/composer"), ("path:directory:src", "src", "")]);
    assert_eq!(search_entries_request("/repo", " men "), json!({"cwd": "/repo", "query": "men", "limit": 80}));
}

#[test]
fn thread_mentions_need_a_query_and_skip_the_open_thread() {
    let thread = |id: &str, title: &str| PaletteThread {
        environment_id: "env".into(),
        id: id.into(),
        title: title.into(),
        created_at: "2026-10-01T00:00:00.000Z".into(),
        updated_at: "2026-10-01T00:00:00.000Z".into(),
        ..Default::default()
    };
    let threads = [
        thread("t1", "Fix the composer menu"),
        thread("t2", "Composer drafts"),
        PaletteThread { archived_at: Some("2026-10-02T00:00:00.000Z".into()), ..thread("t3", "Composer archive") },
        PaletteThread { hidden: true, ..thread("t4", "Composer subagent") },
    ];
    assert!(thread_items(&threads, "env", None, "  ", 0).is_empty());
    let items = thread_items(&threads, "env", Some("t2"), "composer", 0);
    assert_eq!(labels(&items), ["Fix the composer menu"]);
    assert_eq!(items[0].id, "thread:env:t1");
    assert_eq!(items[0].description, "Thread");
}

#[test]
fn highlight_follows_the_query_and_wraps() {
    let items = path_items(&[
        PathEntry { path: "a".into(), kind: PathKind::File },
        PathEntry { path: "b".into(), kind: PathKind::File },
        PathEntry { path: "c".into(), kind: PathKind::File },
    ]);
    assert_eq!(active_item(&items, Some("path:file:b"), Some("k"), Some("k")).unwrap().label, "b");
    assert_eq!(active_item(&items, Some("path:file:b"), Some("old"), Some("k")).unwrap().label, "a");
    assert_eq!(active_item(&items, Some("gone"), Some("k"), Some("k")).unwrap().label, "a");
    assert_eq!(nudge(&items, None, true).as_deref(), Some("path:file:a"));
    assert_eq!(nudge(&items, None, false).as_deref(), Some("path:file:c"));
    assert_eq!(nudge(&items, Some("path:file:c"), true).as_deref(), Some("path:file:a"));
    assert_eq!(nudge(&[], None, true), None);
}

#[test]
fn thread_references_use_scoped_context_ids() {
    let context = ThreadContext::new("env-1", "a1b2-c3", "Fix [the] menu\nnow");
    assert_eq!(context.context_id, "thread_a1b2-c3");
    assert_eq!(context.label, "Fix the menu now");
    assert_eq!(context.reference(), "[Fix the menu now](t3-context://v1/thread/thread_a1b2-c3)");
    // Values from the TypeScript `toComposerContextId`.
    assert_eq!(to_context_id("thread_x.y/z"), "thread_x-y-z-a690e65aa67bec29");
    assert_eq!(to_context_id("thread_é🙂"), "thread_-39831a0a381b5bb1");
    assert_eq!(fnv1a64(""), "811c9dc59dc5811c");
    assert_eq!(fnv1a64("a"), "e40c292cdaead7c7");
    assert_eq!(sanitize_context_label("  ", "thread"), "thread");
}

#[test]
fn messages_carry_only_the_threads_they_still_reference() {
    let kept = ThreadContext::new("env", "t1", "Kept");
    let dropped = ThreadContext::new("env", "t2", "Dropped");
    let text = format!("compare {} with this", kept.reference());
    assert_eq!(referenced_context_ids(&text), ["thread_t1"]);
    let context = message_context(&text, &[kept.clone(), dropped]).unwrap();
    assert_eq!(context["version"], 1);
    assert_eq!(context["records"].as_array().unwrap().len(), 1);
    assert_eq!(context["records"][0], json!({
        "version": 1, "kind": "thread", "contextId": "thread_t1", "label": "Kept",
        "environmentId": "env", "threadId": "t1", "title": "Kept",
    }));
    assert_eq!(message_context("no links", &[kept]), None);
}
