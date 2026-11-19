use serde_json::json;

use super::*;

fn snapshot(status: &str, history: &str) -> AttachEvent {
    serde_json::from_value(json!({
        "type": "snapshot",
        "snapshot": {
            "threadId": "t1", "terminalId": "term-1", "cwd": "/repo", "worktreePath": null,
            "status": status, "pid": 42, "history": history, "exitCode": null, "exitSignal": null,
            "label": "zsh", "updatedAt": "2026-11-02T00:00:00.000Z", "cols": 120, "rows": 30
        }
    }))
    .expect("snapshot decodes")
}

fn summary(terminal_id: &str, running: bool) -> TerminalSummary {
    serde_json::from_value(json!({
        "threadId": "t1", "terminalId": terminal_id, "cwd": "/repo", "worktreePath": null,
        "status": "running", "pid": 1, "exitCode": null, "exitSignal": null,
        "hasRunningSubprocess": running, "label": "", "updatedAt": "2026-11-02T00:00:00.000Z"
    }))
    .expect("summary decodes")
}

#[test]
fn attach_events_decode_per_contract_and_skip_unknown_types() {
    let output: AttachEvent = serde_json::from_value(json!({ "type": "output", "threadId": "t1", "terminalId": "term-1", "data": "hi" })).unwrap();
    assert_eq!(output, AttachEvent::Output { data: "hi".into() });
    let activity: AttachEvent = serde_json::from_value(json!({
        "type": "activity", "threadId": "t1", "terminalId": "term-1", "hasRunningSubprocess": true, "label": "vim", "cols": 80, "rows": 24
    }))
    .unwrap();
    assert_eq!(activity, AttachEvent::Activity { has_running_subprocess: true, label: "vim".into(), cols: Some(80), rows: Some(24) });
    let future: AttachEvent = serde_json::from_value(json!({ "type": "bell", "threadId": "t1", "terminalId": "term-1" })).unwrap();
    assert_eq!(future, AttachEvent::Unknown);
    let SessionSnapshot { status, cols, .. } = match snapshot("starting", "") {
        AttachEvent::Snapshot { snapshot } => snapshot,
        other => panic!("{other:?}"),
    };
    assert_eq!((status, cols), (SessionStatus::Starting, Some(120)));
}

#[test]
fn metadata_upserts_replace_in_place_order_and_removes_drop() {
    let mut terminals = Vec::new();
    let snapshot: MetadataEvent = serde_json::from_value(json!({ "type": "snapshot", "terminals": [
        { "threadId": "t1", "terminalId": "term-1", "cwd": "/r", "worktreePath": null, "status": "running", "pid": 1,
          "exitCode": null, "exitSignal": null, "hasRunningSubprocess": false, "label": "", "updatedAt": "x" }
    ]}))
    .unwrap();
    apply_metadata(&mut terminals, snapshot);
    apply_metadata(&mut terminals, MetadataEvent::Upsert { terminal: summary("term-2", false) });
    apply_metadata(&mut terminals, MetadataEvent::Upsert { terminal: summary("term-1", true) });
    let ids: Vec<&str> = terminals.iter().map(|t| t.terminal_id.as_str()).collect();
    assert_eq!(ids, ["term-2", "term-1"]);
    assert!(terminals[1].has_running_subprocess);
    // The sidebar's per-thread running ids keep metadata order and skip idle shells.
    let mut other = summary("term-9", true);
    other.thread_id = "t2".into();
    terminals.push(other);
    apply_metadata(&mut terminals, MetadataEvent::Upsert { terminal: summary("term-3", true) });
    assert_eq!(running_by_thread(&terminals), std::collections::BTreeMap::from([
        ("t1".to_owned(), vec!["term-1".to_owned(), "term-3".to_owned()]),
        ("t2".to_owned(), vec!["term-9".to_owned()]),
    ]));
    terminals.retain(|t| t.thread_id == "t1" && t.terminal_id != "term-3");
    let remove: MetadataEvent = serde_json::from_value(json!({ "type": "remove", "threadId": "t1", "terminalId": "term-2" })).unwrap();
    apply_metadata(&mut terminals, remove);
    assert_eq!(terminals.len(), 1);
}

#[test]
fn thread_terminals_sort_with_numeric_collation() {
    let terminals = vec![summary("term-10", false), summary("term-9", false), summary("term-2", false)];
    let ids: Vec<&str> = thread_terminals(&terminals, "t1").iter().map(|t| t.terminal_id.as_str()).collect();
    assert_eq!(ids, ["term-2", "term-9", "term-10"]);
    assert!(thread_terminals(&terminals, "other").is_empty());
}

#[test]
fn buffer_resets_on_snapshots_and_appends_output() {
    let mut buffer = BufferState::default();
    assert_eq!(buffer.apply(snapshot("running", "$ ")), OutputOp::Reset("$ ".into()));
    assert_eq!((buffer.status, buffer.version, buffer.lifecycle_version), (BufferStatus::Running, 1, 0));
    assert_eq!(buffer.grid, Some(Grid { cols: 120, rows: 30 }));
    assert_eq!(buffer.apply(AttachEvent::Output { data: "ls\r\n".into() }), OutputOp::Append("ls\r\n".into()));
    // A resubscription's snapshot is a new lifecycle generation.
    buffer.apply(snapshot("running", "$ ls\r\n"));
    assert_eq!(buffer.lifecycle_version, 1);
    assert_eq!(buffer.apply(AttachEvent::Cleared), OutputOp::Reset(String::new()));
    buffer.apply(AttachEvent::Activity { has_running_subprocess: true, label: String::new(), cols: Some(90), rows: Some(20) });
    assert_eq!((buffer.grid, buffer.has_running_subprocess), (Some(Grid { cols: 90, rows: 20 }), true));
    buffer.apply(AttachEvent::Error { message: "boom".into() });
    assert_eq!((buffer.status, buffer.error.as_deref()), (BufferStatus::Error, Some("boom")));
    buffer.apply(AttachEvent::Exited { exit_code: Some(0) });
    assert_eq!((buffer.status, buffer.error.as_deref()), (BufferStatus::Exited, None));
}

#[test]
fn a_reattach_snapshot_starts_a_generation_the_grid_is_replayed_for() {
    let mut buffer = BufferState::default();
    let mut replay = GridReplay::default();
    assert!(!replay.observe(&buffer), "nothing to resize before the first snapshot");
    buffer.apply(snapshot("running", "$ "));
    assert!(replay.observe(&buffer));
    buffer.apply(AttachEvent::Output { data: "ls".into() });
    assert!(!replay.observe(&buffer), "output stays in the same generation");
    // The socket reconnected: the server's new snapshot must hear the measured grid again.
    buffer.apply(snapshot("running", "$ ls"));
    assert_eq!(buffer.generation, 1);
    assert!(replay.observe(&buffer));
    // A shell that stops and comes back through output replays once it runs again.
    buffer.apply(AttachEvent::Closed);
    assert!(!replay.observe(&buffer));
    buffer.apply(AttachEvent::Output { data: "x".into() });
    assert!(replay.observe(&buffer));
    // Leaving the attachment forgets the generation.
    replay.detach();
    assert!(replay.observe(&buffer));
    // A starting shell waits until it runs.
    let mut starting = BufferState::default();
    let mut replay = GridReplay::default();
    starting.apply(snapshot("starting", ""));
    assert!(!replay.observe(&starting));
    // Restarting an already-running shell starts a new generation too.
    let restarted = match snapshot("running", "$ ") {
        AttachEvent::Snapshot { snapshot } => AttachEvent::Restarted { snapshot },
        other => panic!("{other:?}"),
    };
    buffer.apply(restarted);
    assert_eq!(buffer.generation, 2);
    assert!(replay.observe(&buffer));
}

#[test]
fn output_before_a_snapshot_marks_the_session_running() {
    let mut buffer = BufferState::default();
    buffer.apply(AttachEvent::Output { data: "x".into() });
    assert_eq!(buffer.status, BufferStatus::Running);
    let restarted = match snapshot("running", "fresh") {
        AttachEvent::Snapshot { snapshot } => AttachEvent::Restarted { snapshot },
        other => panic!("{other:?}"),
    };
    assert_eq!(buffer.apply(restarted), OutputOp::Reset("fresh".into()));
    assert_eq!(buffer.lifecycle_version, 1);
}

#[test]
fn history_is_trimmed_on_a_character_boundary() {
    assert_eq!(trim_to_bytes("abc".into(), 10), "abc");
    assert_eq!(trim_to_bytes("héllo".into(), 4), "llo");
    assert_eq!(trim_to_bytes("aé".into(), 2), "é");
}

#[test]
fn lifecycle_closes_a_shell_that_exits_while_attached() {
    let mut lifecycle = Lifecycle::default();
    assert_eq!(lifecycle.transition(LifecycleEvent::Status { status: BufferStatus::Closed, version: 0 }), None);
    assert_eq!(lifecycle.transition(LifecycleEvent::Status { status: BufferStatus::Running, version: 1 }), None);
    assert_eq!(
        lifecycle.transition(LifecycleEvent::Status { status: BufferStatus::Exited, version: 2 }),
        Some(LifecycleCommand::Exit)
    );
    assert_eq!(lifecycle.transition(LifecycleEvent::Status { status: BufferStatus::Exited, version: 3 }), None);
}

#[test]
fn lifecycle_reopens_an_ended_shell_once_per_visit() {
    let mut lifecycle = Lifecycle::default();
    assert_eq!(
        lifecycle.transition(LifecycleEvent::Status { status: BufferStatus::Exited, version: 1 }),
        Some(LifecycleCommand::Reopen)
    );
    assert_eq!(lifecycle.transition(LifecycleEvent::Status { status: BufferStatus::Exited, version: 2 }), None);
    lifecycle.transition(LifecycleEvent::ReopenFailed);
    assert_eq!(
        lifecycle.transition(LifecycleEvent::Status { status: BufferStatus::Closed, version: 3 }),
        Some(LifecycleCommand::Reopen)
    );
}

#[test]
fn lifecycle_leaves_history_alone_for_read_only_viewers() {
    let mut observed = Lifecycle::default();
    observed.transition(LifecycleEvent::Observed);
    assert_eq!(observed.transition(LifecycleEvent::Status { status: BufferStatus::Exited, version: 1 }), None);
    let mut revoked = Lifecycle::default();
    revoked.transition(LifecycleEvent::Status { status: BufferStatus::Running, version: 1 });
    revoked.transition(LifecycleEvent::Detached { can_operate: false });
    assert!(revoked.observed && !revoked.was_running);
}

const OPERATE: TerminalAccess = TerminalAccess { read: true, operate: true };
const READ: TerminalAccess = TerminalAccess { read: true, operate: false };

fn status(follow: &mut SessionFollow, status: BufferStatus, version: u64) -> Option<LifecycleCommand> {
    follow.status(status, version)
}

#[test]
fn a_shell_that_exits_across_a_reconnect_is_closed_not_reopened() {
    let mut follow = SessionFollow::default();
    assert_eq!(follow.follow(OPERATE), Some(SessionMode::Attached));
    assert_eq!(status(&mut follow, BufferStatus::Running, 1), None);
    // The transport drops and resubscribes the same attachment.
    assert_eq!(follow.follow(OPERATE), Some(SessionMode::Attached));
    assert_eq!(status(&mut follow, BufferStatus::Exited, 2), Some(LifecycleCommand::Exit));
}

#[test]
fn access_picks_attach_or_observe_and_upgrades_keep_history() {
    assert_eq!(TerminalAccess::default().mode(), None);
    let mut follow = SessionFollow::default();
    assert_eq!(follow.follow(TerminalAccess::default()), None);
    assert_eq!(follow.follow(READ), Some(SessionMode::Observing));
    assert_eq!(status(&mut follow, BufferStatus::Exited, 1), None, "observers never close or reopen");
    assert_eq!(follow.follow(OPERATE), Some(SessionMode::Attached));
    assert_eq!(status(&mut follow, BufferStatus::Exited, 1), None, "a granted viewer keeps the history they read");

    let mut revoked = SessionFollow::default();
    revoked.follow(OPERATE);
    status(&mut revoked, BufferStatus::Running, 1);
    assert_eq!(revoked.follow(READ), Some(SessionMode::Observing));
    assert_eq!(revoked.follow(OPERATE), Some(SessionMode::Attached));
    assert_eq!(status(&mut revoked, BufferStatus::Exited, 2), None);

    let mut fresh = SessionFollow::default();
    fresh.follow(OPERATE);
    assert_eq!(status(&mut fresh, BufferStatus::Exited, 1), Some(LifecycleCommand::Reopen));
}

#[test]
fn typography_follows_the_code_font_until_split() {
    let code = FontSizeRange { default: 13., min: 10., max: 18. };
    let terminal = FontSizeRange { default: 12., min: 8., max: 20. };
    let settings = |value: serde_json::Value| value.as_object().cloned().expect("object");
    let resolve = |value, advanced| TerminalTypography::resolve(&settings(value), advanced, code, terminal);
    assert_eq!(resolve(json!({}), false), TerminalTypography { family: None, size: 13. });
    assert_eq!(resolve(json!({}), true), TerminalTypography { family: None, size: 12. });
    let chosen = json!({
        "fontFamilyCode": "Menlo", "fontSizeCode": 15,
        "fontFamilyTerminal": " \"JetBrains Mono\", monospace", "fontSizeTerminal": 9
    });
    assert_eq!(resolve(chosen.clone(), false), TerminalTypography { family: Some("Menlo".into()), size: 15. });
    assert_eq!(resolve(chosen, true), TerminalTypography { family: Some("JetBrains Mono".into()), size: 9. });
    let invalid = json!({"fontFamilyTerminal": "  ", "fontSizeTerminal": 30, "fontSizeCode": 12.5});
    assert_eq!(resolve(invalid.clone(), true), TerminalTypography { family: None, size: 12. });
    assert_eq!(resolve(invalid, false).size, 13.);
}

#[test]
fn session_grants_follow_permissions_then_legacy_scopes() {
    let session = |value: serde_json::Value| -> crate::auth::AuthSessionState { serde_json::from_value(value).expect("decodes") };
    let current = session(json!({"authenticated": true, "auth": {}, "permissions": ["terminal:read"], "scopes": ["terminal:operate"]}));
    assert_eq!(TerminalAccess::of(Some(&current)), READ);
    let legacy = session(json!({"authenticated": true, "auth": {"policy": "x"}, "scopes": ["terminal:operate"]}));
    assert_eq!(TerminalAccess::of(Some(&legacy)), OPERATE);
    let pinned = session(json!({"authenticated": true, "auth": {"serverUpdateScope": "environment:maintain"}, "scopes": ["terminal:operate"]}));
    assert_eq!(TerminalAccess::of(Some(&pinned)), TerminalAccess { read: false, operate: true });
    let signed_out = session(json!({"authenticated": false, "auth": {}, "permissions": ["terminal:operate", "brand:new"]}));
    assert_eq!(TerminalAccess::of(Some(&signed_out)), TerminalAccess::default());
    assert_eq!(TerminalAccess::of(None), TerminalAccess::default());
}

#[test]
fn labels_follow_terminal_labels_ts() {
    assert_eq!(terminal_label("term-3"), "Terminal 3");
    assert_eq!(terminal_label("terminal-12"), "Terminal 12");
    assert_eq!(terminal_label("TERM-2-0f8fad5b-d9cb-469f-a165-70867728950e"), "Terminal 2");
    assert_eq!(terminal_label("term-2-nope"), "term-2-nope");
    assert_eq!(terminal_label("setup"), "setup");
    assert_eq!(session_label("term-1", Some("  vim ")), "vim");
    assert_eq!(session_label("term-1", Some("   ")), "Terminal 1");
    assert_eq!(next_terminal_id(["term-1", "term-3", "custom"], None), "term-2");
    assert_eq!(next_terminal_id([], Some("abc")), "term-1-abc");
}

#[test]
fn payloads_match_the_contract() {
    let root = LaunchLocation::new("/repo", None);
    assert_eq!(root.env.get("T3CODE_PROJECT_ROOT").map(String::as_str), Some("/repo"));
    let open = open_input("t1", "term-1", &root, Some(Grid::new(80, 24)));
    assert_eq!(open, json!({ "threadId": "t1", "terminalId": "term-1", "cwd": "/repo", "env": { "T3CODE_PROJECT_ROOT": "/repo" }, "cols": 80, "rows": 24 }));
    let worktree = LaunchLocation::new("/repo", Some("/wt"));
    assert_eq!(worktree.cwd, "/wt");
    let attach = attach_input("t1", "term-1", &worktree, None, true);
    assert_eq!(attach["worktreePath"], "/wt");
    assert_eq!(attach["env"]["T3CODE_WORKTREE_PATH"], "/wt");
    assert_eq!(attach["resume"], true);
    assert_eq!(attach["maxHistoryBytes"], MAX_BUFFER_BYTES);
    assert_eq!(attach_input("t1", "term-1", &root, None, false)["maxHistoryBytes"], 512 * 1024);
    assert_eq!(attach_input("t1", "term-1", &root, None, false).get("resume"), None);
    assert_eq!(attach_input("t1", "term-1", &root, None, false)["worktreePath"], json!(null));
    assert_eq!(attach.get("providerInstanceId"), None);
    let setup = LaunchLocation { provider_instance_id: Some("codex-work".to_owned()), ..LaunchLocation::new("/setup", None) };
    for resume in [false, true] {
        let attach = attach_input("onboarding-agent-setup", "onboarding-codex-uuid", &setup, None, resume);
        assert_eq!(attach["providerInstanceId"], "codex-work");
        assert_eq!(attach["cwd"], "/setup");
    }
    assert_eq!(open_input("onboarding-agent-setup", "onboarding-codex-uuid", &setup, None)["providerInstanceId"], "codex-work");
    assert_eq!(close_input("t1", "term-1", true)["deleteHistory"], true);
    assert_eq!(close_input("t1", "term-1", false).get("deleteHistory"), None);
    assert_eq!(resize_input("t1", "term-1", Grid::new(0, 9999)), json!({ "threadId": "t1", "terminalId": "term-1", "cols": 1, "rows": 500 }));
    assert_eq!(write_input("t1", "term-1", "ls\r")["data"], "ls\r");
}

#[test]
fn shared_grid_caps_each_axis() {
    let measured = Grid::new(100, 40);
    assert_eq!(Grid::fit_within(None, measured), measured);
    assert_eq!(Grid::fit_within(Some(Grid::new(80, 50)), measured), Grid::new(80, 40));
}

#[test]
fn drawer_height_clamps_to_the_window() {
    assert_eq!(clamp_drawer_height(280., 1000.), 280.);
    assert_eq!(clamp_drawer_height(100., 1000.), 180.);
    assert_eq!(clamp_drawer_height(900., 1000.), 750.);
    assert_eq!(clamp_drawer_height(500., 200.), 180.);
    assert_eq!(clamp_drawer_height(f32::NAN, 1000.), 280.);
}

#[test]
fn close_confirmation_copy() {
    let (message, detail) = close_confirmation(&["Terminal 1".into()]);
    assert_eq!(message, "Close terminal \"Terminal 1\"?");
    assert_eq!(detail, "This stops the running process and clears its history.");
    let (message, detail) = close_confirmation(&["a".into(), "b".into()]);
    assert_eq!(message, "Close 2 terminals?");
    assert_eq!(detail, "This stops their running processes and clears their histories: \"a\", \"b\".");
}

fn key(name: &str) -> KeyInput<'_> {
    KeyInput { key: name, ..KeyInput::default() }
}

#[test]
fn mac_editing_shortcuts_become_readline_input() {
    assert_eq!(shortcut_data(&KeyInput { alt: true, ..key("left") }), Some("\x1bb"));
    assert_eq!(shortcut_data(&KeyInput { alt: true, ..key("right") }), Some("\x1bf"));
    assert_eq!(shortcut_data(&KeyInput { platform: true, ..key("left") }), Some("\x01"));
    assert_eq!(shortcut_data(&KeyInput { platform: true, ..key("right") }), Some("\x05"));
    assert_eq!(shortcut_data(&KeyInput { platform: true, ..key("backspace") }), Some("\x15"));
    assert_eq!(shortcut_data(&KeyInput { platform: true, ..key("k") }), Some("\x0c"));
    assert_eq!(shortcut_data(&KeyInput { control: true, ..key("l") }), Some("\x0c"));
    assert_eq!(shortcut_data(&KeyInput { alt: true, shift: true, ..key("left") }), None);
    assert_eq!(shortcut_data(&key("left")), None);
}

#[test]
fn keys_encode_like_xterm() {
    assert_eq!(encode_key(&key("enter"), false), Some(b"\r".to_vec()));
    assert_eq!(encode_key(&key("backspace"), false), Some(vec![0x7f]));
    assert_eq!(encode_key(&KeyInput { shift: true, ..key("tab") }, false), Some(b"\x1b[Z".to_vec()));
    assert_eq!(encode_key(&key("up"), false), Some(b"\x1b[A".to_vec()));
    assert_eq!(encode_key(&key("up"), true), Some(b"\x1bOA".to_vec()));
    assert_eq!(encode_key(&KeyInput { shift: true, ..key("left") }, true), Some(b"\x1b[1;2D".to_vec()));
    assert_eq!(encode_key(&KeyInput { control: true, ..key("delete") }, false), Some(b"\x1b[3;5~".to_vec()));
    assert_eq!(encode_key(&key("f1"), false), Some(b"\x1bOP".to_vec()));
    assert_eq!(encode_key(&key("f12"), false), Some(b"\x1b[24~".to_vec()));
    assert_eq!(encode_key(&KeyInput { control: true, key_char: Some("c"), ..key("c") }, false), Some(vec![0x03]));
    assert_eq!(encode_key(&KeyInput { control: true, ..key("[") }, false), Some(vec![0x1b]));
    assert_eq!(encode_key(&KeyInput { control: true, ..key("space") }, false), Some(vec![0x00]));
    assert_eq!(encode_key(&KeyInput { control: true, alt: true, ..key("c") }, false), Some(vec![0x1b, 0x03]));
    assert_eq!(encode_key(&KeyInput { alt: true, ..key("backspace") }, false), Some(vec![0x1b, 0x7f]));
}

#[test]
fn text_and_app_shortcuts_are_not_encoded() {
    assert_eq!(encode_key(&KeyInput { key_char: Some("a"), ..key("a") }, false), None);
    assert_eq!(encode_key(&KeyInput { alt: true, key_char: Some("∫"), ..key("b") }, false), None);
    // macOS Option-layer composed characters stay left to text input (`pingdotgg/t3code#21596`).
    assert_eq!(encode_key(&KeyInput { alt: true, key_char: Some("#"), ..key("3") }, false), None);
    assert_eq!(encode_key(&KeyInput { alt: true, key_char: Some("@"), ..key("l") }, false), None);
    assert_eq!(encode_key(&KeyInput { platform: true, ..key("enter") }, false), None);
    assert_eq!(encode_key(&KeyInput { control: true, ..key("1") }, false), None);
}

#[test]
fn local_shortcuts_match_copy_paste_select_and_scroll_including_non_latin_layouts() {
    use std::collections::HashMap;
    let russian = HashMap::from([('a', 'ф'), ('c', 'с'), ('v', 'м')]);
    // Non-Latin Cmd+C/V/A on macOS and Ctrl+Shift+C/V/A on non-macOS (`pingdotgg/t3code#35210`).
    assert_eq!(
        local_shortcut(&KeyInput { platform: true, ..key("с") }, true, true, false, Some(&russian)),
        Some(TerminalShortcut::Copy { clear_selection: false }),
    );
    assert_eq!(
        local_shortcut(&KeyInput { control: true, shift: true, ..key("с") }, false, true, false, Some(&russian)),
        Some(TerminalShortcut::Copy { clear_selection: false }),
    );
    assert_eq!(
        local_shortcut(&KeyInput { control: true, ..key("с") }, false, true, false, Some(&russian)),
        Some(TerminalShortcut::Copy { clear_selection: true }),
    );
    assert_eq!(
        local_shortcut(&KeyInput { control: true, ..key("c") }, false, false, false, Some(&russian)),
        None,
    );
    assert_eq!(
        local_shortcut(&KeyInput { control: true, shift: true, ..key("м") }, false, false, false, Some(&russian)),
        Some(TerminalShortcut::Paste),
    );
    assert_eq!(
        local_shortcut(&KeyInput { platform: true, ..key("ф") }, true, false, false, Some(&russian)),
        Some(TerminalShortcut::SelectAll),
    );
    assert_eq!(
        local_shortcut(&KeyInput { control: true, shift: true, ..key("ф") }, false, false, false, Some(&russian)),
        Some(TerminalShortcut::SelectAll),
    );
    assert_eq!(
        local_shortcut(&KeyInput { shift: true, ..key("pageup") }, true, false, false, None::<&HashMap<char, char>>),
        Some(TerminalShortcut::ScrollPages(1)),
    );
    assert_eq!(
        local_shortcut(&KeyInput { shift: true, ..key("pageup") }, true, false, true, None::<&HashMap<char, char>>),
        None,
    );
    assert_eq!(
        local_shortcut(&KeyInput { shift: true, platform: true, ..key("home") }, true, false, false, None::<&HashMap<char, char>>),
        Some(TerminalShortcut::ScrollToTop),
    );
}

#[test]
fn pastes_respect_bracketed_mode_and_write_limits() {
    assert_eq!(encode_paste("a\nb\r\nc", false), "a\rb\rc");
    assert_eq!(encode_paste("hi\n", true), "\x1b[200~hi\n\x1b[201~");
    assert_eq!(encode_paste("a\x1b[201~rm", true), "\x1b[200~arm\x1b[201~");
    let long = "é".repeat(20_000);
    let chunks = paste_chunks(&long);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks.concat(), long);
    assert!(paste_chunks("").is_empty());
}

fn ids(layout: &ThreadTerminalLayout) -> Vec<&str> {
    layout.terminal_ids.iter().map(String::as_str).collect()
}

#[test]
fn opening_an_empty_thread_adds_the_default_terminal() {
    let layout = ThreadTerminalLayout::default().with_open(true);
    assert!(layout.terminal_open);
    assert_eq!(ids(&layout), ["term-1"]);
    assert_eq!(layout.active_terminal_group_id, "group-term-1");
    assert!(!layout.with_open(false).terminal_open);
}

#[test]
fn splits_join_the_active_group_after_the_active_terminal() {
    let layout = ThreadTerminalLayout::default()
        .with_new("term-1")
        .with_split("term-2", SplitDirection::Horizontal)
        .with_active("term-1")
        .with_split("term-3", SplitDirection::Vertical);
    assert_eq!(layout.visible_terminal_ids(), ["term-1", "term-3", "term-2"]);
    assert_eq!(layout.split_direction(), SplitDirection::Vertical);
    assert_eq!(layout.active_terminal_id, "term-3");
    let full = layout.with_split("term-4", SplitDirection::Horizontal);
    assert!(full.has_reached_split_limit());
    assert_eq!(full.with_split("term-5", SplitDirection::Horizontal), full);
    let separate = full.with_new("term-5");
    assert_eq!(separate.visible_terminal_ids(), ["term-5"]);
    assert!(separate.shows_group_headers());
}

#[test]
fn closing_picks_a_neighbour_and_the_last_close_resets() {
    let layout = ThreadTerminalLayout::default()
        .with_new("term-1")
        .with_split("term-2", SplitDirection::Horizontal)
        .with_new("term-3")
        .with_active("term-1");
    let closed = layout.with_closed("term-1");
    assert_eq!(closed.active_terminal_id, "term-2");
    assert_eq!(ids(&closed), ["term-2", "term-3"]);
    let closed = closed.with_closed("term-2");
    assert_eq!(closed.active_terminal_id, "term-3");
    assert_eq!(closed.with_closed("term-3"), ThreadTerminalLayout::default());
    assert_eq!(closed.with_closed("missing"), closed);
}

#[test]
fn reconcile_adopts_server_ids_and_keeps_surviving_groups() {
    let layout = ThreadTerminalLayout::default()
        .with_new("term-1")
        .with_split("term-2", SplitDirection::Vertical)
        .with_new("term-3");
    let next = layout.with_reconciled(&["term-2".into(), "term-3".into(), "term-4".into()]);
    assert_eq!(ids(&next), ["term-2", "term-3", "term-4"]);
    assert_eq!(next.terminal_groups[0].terminal_ids, ["term-2"]);
    assert_eq!(next.terminal_groups[0].split_direction, SplitDirection::Vertical);
    assert_eq!(next.terminal_groups.len(), 3);
}

#[test]
fn ensure_can_add_without_activating() {
    let layout = ThreadTerminalLayout::default().with_new("term-1");
    let ensured = layout.with_ensured("term-2", false, Some(false));
    assert_eq!(ensured.active_terminal_id, "term-1");
    assert_eq!(ids(&ensured), ["term-1", "term-2"]);
    let opened = ThreadTerminalLayout::default().with_ensured("setup", true, None);
    assert!(opened.terminal_open);
    assert_eq!(opened.active_terminal_id, "setup");
}

#[test]
fn closed_terminals_stay_gone_while_the_server_list_lags() {
    let mut layouts = TerminalLayouts::default();
    assert!(layouts.new_terminal("t1", "term-1"));
    layouts.new_terminal("t1", "term-2");
    assert!(layouts.close("t1", "term-2"));
    layouts.reconcile("t1", &["term-1".into(), "term-2".into()]);
    assert_eq!(ids(&layouts.get("t1")), ["term-1"]);
    // A new terminal with the same id is wanted again.
    layouts.new_terminal("t1", "term-2");
    layouts.reconcile("t1", &["term-1".into(), "term-2".into()]);
    assert_eq!(ids(&layouts.get("t1")), ["term-1", "term-2"]);
    assert!(!layouts.set_active("t1", "term-2"));
}

#[test]
fn layouts_persist_only_non_default_threads() {
    let mut layouts = TerminalLayouts::default();
    layouts.set_open("t1", true);
    layouts.set_height("t1", 320.);
    layouts.set_open("t2", true);
    layouts.close("t2", "term-1");
    let json = serde_json::to_string(layouts.saved()).unwrap();
    let mut saved: std::collections::HashMap<String, ThreadTerminalLayout> = serde_json::from_str(&json).unwrap();
    assert!(!saved.contains_key("t2"));
    saved.insert("t3".into(), ThreadTerminalLayout::default());
    let restored = TerminalLayouts::from_saved(saved);
    assert_eq!(restored.get("t1"), layouts.get("t1"));
    assert_eq!(restored.get("t1").terminal_height, 320.);
    assert_eq!(restored.saved().len(), 1);
}

#[test]
fn reconcile_and_reuse_rules() {
    let s = |v: &[&str]| v.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    assert!(!should_reconcile(&s(&["b", "a"]), &s(&["a", "b"])));
    assert!(!should_reconcile(&s(&["a"]), &s(&["a", "b"])));
    assert!(should_reconcile(&s(&["a", "c"]), &s(&["a", "b"])));
    assert!(!should_reconcile(&s(&[]), &s(&["a"])));
    assert!(should_reconcile(&s(&["a"]), &s(&[])));
    let (idle, busy) = (summary("term-1", false), summary("term-2", true));
    assert_eq!(pick_existing_terminal(&[&idle, &busy]).as_deref(), Some("term-2"));
    assert_eq!(pick_existing_terminal(&[&idle]).as_deref(), Some("term-1"));
    assert_eq!(pick_existing_terminal(&[]), None);
}

fn found(kind: links::LinkKind, text: &str, start: usize, end: usize) -> links::LinkMatch {
    links::LinkMatch { kind, text: text.to_owned(), start, end }
}

fn path_at(text: &str, start: usize) -> links::LinkMatch {
    found(links::LinkKind::Path, text, start, start + text.len())
}

#[test]
fn links_follow_terminal_links_test_ts() {
    use links::{LinkKind, extract_links, is_url};
    for path in [r"C:\dev\ai-test\dir 1", r"C:\dev\ai-test\dir 1\test", "C:\\", r"\\server\my share\dir 1", "/home/Jane Doe/project", r"C:\dev\folder (draft)\name!"] {
        assert_eq!(extract_links(&format!("PS {path}> ")), vec![path_at(path, 3)], "{path}");
    }
    let prompt = r"PS C:\dev\dir 1\test> ";
    assert_eq!(
        extract_links(&format!("{prompt}echo https://example.com")),
        vec![path_at(r"C:\dev\dir 1\test", 3), found(LinkKind::Url, "https://example.com", prompt.len() + 5, prompt.len() + 24)]
    );
    assert_eq!(extract_links(r"see C:\dev\project for details"), vec![path_at(r"C:\dev\project", 4)]);
    for text in [r"src\index.ts:12", r".\src\index.ts:12:7", r"..\src\index.ts:12", r"C:\Users\Jane\project\index.ts:12", r"\\server\share\index.ts:12"] {
        assert_eq!(extract_links(&format!("error at {text}: failed")), vec![path_at(text, 9)], "{text}");
    }
    for quote in ['"', '\'', '`'] {
        let text = r"C:\Users\Jane Doe\project\index.ts:12:7";
        assert_eq!(extract_links(&format!("see {quote}{text}{quote}.")), vec![path_at(text, 5)]);
        let at_text = "src/my contact@support.vue:12:3";
        assert_eq!(extract_links(&format!("see {quote}{at_text}{quote}.")), vec![path_at(at_text, 5)]);
    }
    for path in [r"src\my folder\index.ts", "./my folder/index.ts", "/home/Jane Doe/index.ts", "/home/Jane Doe/file)", r"\\server\my share\index.ts"] {
        assert_eq!(extract_links(&format!("\"{path}\":12:7: error")), vec![found(LinkKind::Path, &format!("{path}:12:7"), 1, path.len() + 7)], "{path}");
    }
    assert_eq!(extract_links("error: \"something went wrong\""), vec![]);
    assert_eq!(extract_links("error: \"something went wrong.\""), vec![]);
    let (path, url) = (r"C:\Jane Doe\index.ts:12", "https://example.com/docs");
    let line = format!("\"{path}\" {url} src/index.ts");
    assert_eq!(
        extract_links(&line),
        vec![path_at(path, 1), found(LinkKind::Url, url, path.len() + 3, path.len() + 3 + url.len()), path_at("src/index.ts", line.len() - 12)]
    );
    assert!(is_url("HTTPS://example.com/docs") && is_url("Http://example.com"));
    assert!(!is_url("src/components/main.ts") && !is_url("httpsdocs/readme.md"));
    assert_eq!(
        extract_links("(https://example.com/docs), ./src/main.ts:12."),
        vec![found(LinkKind::Url, "https://example.com/docs", 1, 25), path_at("./src/main.ts:12", 28)]
    );
    for text in [
        "src/żółć.ts:12:7",
        "żółć/main.ts:12",
        r"żółć\main.ts:12",
        "src/cafe\u{301}.ts:12",
        "src/𐐀.ts:12",
        "src/app/[id]/page.tsx:3:10",
        "src/routes/(app)/+page.svelte:12:4",
        "src/routes/(app)/+page@.svelte:12:3",
        "src/contact@support.vue:12:3",
        r"src\contact@support.ts:12:3",
        "src/équipe@home.ts:12:3",
        "src/foo$contact@support.ts:12:3",
        "node_modules/.pnpm/react@18.3.1/node_modules/react/index.js:12:5",
        "app/routes/posts.$id.tsx:5",
    ] {
        assert_eq!(extract_links(&format!("see {text} for details")), vec![path_at(text, 4)], "{text}");
    }
    assert_eq!(extract_links("see C:/Users/someone/project/src/file.ts:42 for details"), vec![path_at("C:/Users/someone/project/src/file.ts:42", 4)]);
    assert_eq!(extract_links("GET https://example.test/items/foo:"), vec![found(LinkKind::Url, "https://example.test/items/foo:", 4, 35)]);
    for (line, text) in [
        ("./main.go:10:5: undefined: x", "./main.go:10:5"),
        ("/home/dev/app/src/main.c:10:5: error: expected ';'", "/home/dev/app/src/main.c:10:5"),
        ("C:\\dev\\app\\src\\main.c:10:5: error: expected ';'", "C:\\dev\\app\\src\\main.c:10:5"),
        ("wrote ./out/report.txt:", "./out/report.txt"),
    ] {
        assert_eq!(extract_links(line), vec![path_at(text, line.find(text).unwrap())], "{line}");
    }
    for line in [
        " GET /api/trpc/post.list,user.me?batch=1&input=%7B%220%22%3A%7B%22json%22%3Anull%7D%7D 200 in 35ms",
        "GET /api?+debug 200",
        "Started GET \"/posts?page=2\" for ::1",
        "proxying api/trpc/post.list?batch=1 to the backend",
        "connected to redis://127.0.0.1:6379/0",
        "connected to postgres://localhost:5432/app",
        "uploaded s3://bucket/reports/today.csv",
        "    at main (file:///Users/jane/app/src/index.mjs:3:7)",
        "+redis://localhost:6379/0",
        "-redis://localhost:6379/0",
        "git clone git@github.com:acme/widgets.git",
        "remote: git@gitlab.com:group/sub/project.git",
    ] {
        assert_eq!(extract_links(line), vec![], "{line}");
    }
    let (before, after, url) = ("src/server.ts:12", "/app/bin/server:3:7", "https://example.com/api/users?id=42");
    let line = format!("{before} GET /api/users?id=42 200 {after} {url}");
    let (after_start, url_start) = (line.find(after).unwrap(), line.find(url).unwrap());
    assert_eq!(
        extract_links(&line),
        vec![path_at(before, 0), path_at(after, after_start), found(LinkKind::Url, url, url_start, url_start + url.len())]
    );
    assert_eq!(extract_links("src/db.ts:4 redis://localhost:6379/0 C:/app/src/db.ts:9"), vec![path_at("src/db.ts:4", 0), path_at("C:/app/src/db.ts:9", 37)]);
    assert_eq!(extract_links("wrote /tmp/report?draft"), vec![path_at("/tmp/report?draft", 6)]);
}

#[test]
fn link_targets_resolve_like_resolve_terminal_link_target() {
    use crate::files::position::resolve_path_link_target;
    use links::{extract_links, resolve_link_target};
    let link = &extract_links(r#"error at "src\my folder\index.ts":12:7"#)[0];
    assert_eq!(resolve_path_link_target(&link.text, r"C:\Users\Jane Doe\project"), r"C:\Users\Jane Doe\project\src\my folder\index.ts:12:7");
    let link = &extract_links("/Users/julius/project/main.c:10:5: error: expected ';'")[0];
    assert_eq!(resolve_path_link_target(&link.text, "/Users/julius/project"), "/Users/julius/project/main.c:10:5");
    for (path, target) in [
        ("src/main.ts", "/repo/src/main.ts"),
        ("/repo/src/main.ts", "/repo/src/main.ts"),
        (r"C:\repo\src\main.ts", r"C:\repo\src\main.ts"),
        ("app/(auth)/page.tsx", "/repo/app/(auth)/page.tsx"),
    ] {
        let text = format!("{path}(42,7)");
        let found_links = extract_links(&format!("{text}: error TS2322: Type mismatch"));
        assert_eq!(found_links, vec![found(links::LinkKind::Path, &format!("{path}:42:7"), 0, text.len())], "{path}");
        assert_eq!(resolve_link_target(&found_links[0].text, "/repo"), format!("{target}:42:7"));
    }
    for (line, target) in [
        (r"PS C:\dev\folder (42,7)> ", r"C:\dev\folder (42,7)"),
        ("/tmp/report(42,7)", "/tmp/report(42,7)"),
        ("src/Program.cs(3,10): error CS1002: x", "/repo/src/Program.cs"),
    ] {
        assert_eq!(resolve_link_target(&extract_links(line)[0].text, "/repo"), target, "{line}");
    }
    // OSC 8 file URIs name their file; they are never joined onto the cwd (`pingdotgg/t3code#36025`).
    assert_eq!(resolve_link_target("file:///var/tmp/report.txt", "/workspace/project"), "/var/tmp/report.txt");
    assert_eq!(resolve_link_target("file:///var/tmp/a%20b.txt#L12", "/workspace/project"), "/var/tmp/a b.txt:12");
    assert!(!resolve_link_target("file://remote.invalid/var/tmp/report.txt", "/workspace/project").starts_with("/workspace/project/"));
    assert_eq!(resolve_path_link_target("file:notes.md", "/workspace/project"), "/workspace/project/file:notes.md");
    assert_eq!(resolve_path_link_target("~/notes.md:3", "/Users/jane/project"), "/Users/jane/notes.md:3");
}

fn row(text: &str, continuation: bool, wraps: bool) -> links::RowText {
    let offsets = text.char_indices().map(|(at, _)| at).collect::<Vec<_>>();
    let spacer = vec![false; offsets.len()];
    links::RowText { text: text.to_owned(), offsets, spacer, continuation, wraps }
}

#[test]
fn links_are_found_across_soft_wraps_and_never_truncated() {
    use links::{LinkHit, link_at};
    let rows = [row("prompt> ", false, false), row("see https://example.com/a", false, true), row("/bc?x=1  ", true, false), row("done", false, false)];
    let get = |index: usize| rows.get(index).cloned();
    let hit = LinkHit { text: "https://example.com/a/bc?x=1".into(), start: (1, 4), end: (2, 6) };
    // From the first row and from its continuation alike.
    assert_eq!(link_at(1, 10, get), Some(hit.clone()));
    assert_eq!(link_at(2, 2, get), Some(hit));
    assert_eq!(link_at(1, 1, get), None);
    assert_eq!(link_at(3, 1, get), None);
    // A logical line that began above the viewport, or a link running past its bottom, is cut.
    let cut_above = [row("/bc?x=1", true, false)];
    assert_eq!(link_at(0, 2, |index| cut_above.get(index).cloned()), None);
    let cut_below = [row("see https://example.com/a", false, true)];
    assert_eq!(link_at(0, 8, |index| cut_below.get(index).cloned()), None);
    // A wide character occupies two columns, its spacer hits the character.
    let wide = links::RowText {
        text: "日 src/a.rs".into(),
        offsets: vec![0, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        spacer: vec![false, true, false, false, false, false, false, false, false, false, false],
        continuation: false,
        wraps: false,
    };
    assert_eq!(link_at(0, 5, |index| (index == 0).then(|| wide.clone())), Some(LinkHit { text: "src/a.rs".into(), start: (0, 3), end: (0, 10) }));
}

#[test]
fn emulator_finds_detected_and_osc8_links() {
    use emulator::Emulator;
    let theme = emulator::ThemeColors {
        foreground: emulator::Rgb { r: 255, g: 255, b: 255 },
        background: emulator::Rgb { r: 0, g: 0, b: 0 },
        cursor: emulator::Rgb { r: 255, g: 255, b: 255 },
        ansi: [emulator::Rgb { r: 0, g: 0, b: 0 }; 16],
    };
    let mut term = Emulator::new(Grid::new(20, 4), theme);
    term.feed(b"open src/main.rs:3 now\r\n\x1b]8;;file:///tmp/a.txt\x1b\\report\x1b]8;;\x1b\\ x");
    // The 20-column row soft-wraps `now` onto the next row.
    let detected = term.link_at(0, 7).expect("detected link");
    assert_eq!((detected.text.as_str(), detected.start, detected.end), ("src/main.rs:3", (0, 5), (0, 17)));
    let explicit = term.link_at(2, 3).expect("osc 8 link");
    assert_eq!((explicit.text.as_str(), explicit.start, explicit.end), ("file:///tmp/a.txt", (2, 0), (2, 5)));
    assert_eq!(term.link_at(2, 7), None);
}

#[test]
fn wire_codecs_handle_write_options_snapshot_error_and_metadata_manager_id() {
    let plain = write_input("th-1", "term-1", "ls\n");
    assert_eq!(plain, json!({ "threadId": "th-1", "terminalId": "term-1", "data": "ls\n" }));
    let opts = write_input_with_options("th-1", "term-1", "\u{1b}[?1;2c", Some(true), Some(true));
    assert_eq!(
        opts,
        json!({ "threadId": "th-1", "terminalId": "term-1", "data": "\u{1b}[?1;2c", "binary": true, "reply": true })
    );
    let typed: TerminalWriteInput = serde_json::from_value(opts).unwrap();
    assert_eq!(typed.binary, Some(true));
    assert_eq!(typed.reply, Some(true));

    let failed_snapshot: AttachEvent = serde_json::from_value(json!({
        "type": "snapshot",
        "snapshot": {
            "threadId": "th-1",
            "terminalId": "term-1",
            "status": "error",
            "error": "spawn /bin/zsh ENOENT"
        }
    }))
    .unwrap();
    let mut buffer = BufferState::default();
    buffer.apply(failed_snapshot);
    assert_eq!(buffer.status, BufferStatus::Error);
    assert_eq!(buffer.error.as_deref(), Some("spawn /bin/zsh ENOENT"));

    let meta_with_mgr: MetadataEvent = serde_json::from_value(json!({
        "type": "snapshot",
        "terminals": [],
        "managerId": "mgr-42"
    }))
    .unwrap();
    assert_eq!(meta_with_mgr, MetadataEvent::Snapshot { terminals: vec![], manager_id: Some("mgr-42".into()) });
    let meta_legacy: MetadataEvent = serde_json::from_value(json!({
        "type": "snapshot",
        "terminals": []
    }))
    .unwrap();
    assert_eq!(meta_legacy, MetadataEvent::Snapshot { terminals: vec![], manager_id: None });
}
