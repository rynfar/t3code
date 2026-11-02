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
    assert_eq!(attach_input("t1", "term-1", &root, None, false).get("resume"), None);
    assert_eq!(attach_input("t1", "term-1", &root, None, false)["worktreePath"], json!(null));
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
    assert_eq!(encode_key(&KeyInput { platform: true, ..key("enter") }, false), None);
    assert_eq!(encode_key(&KeyInput { control: true, ..key("1") }, false), None);
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
