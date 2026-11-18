use super::*;

/// Distinct tabs for exercising tab behavior; only the id matters to the model.
fn tab(id: &str) -> Surface {
    Surface { id: id.to_owned(), kind: SurfaceKind::Diff }
}

fn ids(panels: &RightPanels, thread: &str) -> Vec<String> {
    panels.panel(thread).surfaces.iter().map(|surface| surface.id.clone()).collect()
}

/// Reopens with the shell live and every thread in a workspace, returning the restored thread.
fn reopen(panels: &mut RightPanels, thread_exists: impl Fn(&str) -> bool) -> Option<String> {
    match panels.reopen_closed(thread_exists, true, |_| true) {
        Some(Reopened::Thread(thread)) => Some(thread),
        None => None,
        other => panic!("expected a panel tab, got {other:?}"),
    }
}

fn with_tabs(names: &[&str]) -> RightPanels {
    let mut panels = RightPanels::default();
    for name in names {
        panels.open("t", tab(name));
    }
    panels
}

#[test]
fn open_adds_once_activates_and_shows() {
    let mut panels = RightPanels::default();
    assert!(panels.open("t", Surface::diff()));
    assert!(!panels.open("t", Surface::diff()));
    let panel = panels.panel("t");
    assert!(panel.is_open);
    assert_eq!(panel.active_surface_id.as_deref(), Some("diff"));
    assert_eq!(panels.active_surface("t"), Some(&Surface::diff()));
    assert_eq!(panels.panel("other"), &ThreadPanel::default());
}

#[test]
fn toggle_hides_the_active_surface_and_brings_others_forward() {
    let mut panels = with_tabs(&["a", "diff"]);
    panels.activate("t", "a");
    assert!(panels.toggle("t", Surface::diff()));
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("diff"));
    assert!(panels.toggle("t", Surface::diff()));
    assert!(!panels.is_open("t"));
    assert_eq!(panels.active_surface("t"), None);
    assert!(panels.toggle("t", Surface::diff()));
    assert!(panels.is_open("t"));
    assert!(panels.toggle_visibility("t"));
    assert!(!panels.is_open("t"));
}

#[test]
fn closing_the_active_tab_activates_its_neighbor() {
    let mut panels = with_tabs(&["a", "b", "c"]);
    panels.activate("t", "b");
    assert!(panels.close_surface("t", "b"));
    assert_eq!(ids(&panels, "t"), ["a", "c"]);
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("c"));
    panels.close_surface("t", "c");
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("a"));
    panels.close_surface("t", "a");
    assert!(!panels.is_open("t"));
    // An empty, hidden panel is dropped entirely.
    assert!(panels.threads.is_empty());
    assert!(!panels.close_surface("t", "missing"));
}

#[test]
fn closing_others_and_to_the_right() {
    let mut panels = with_tabs(&["a", "b", "c", "d"]);
    panels.activate("t", "d");
    assert!(panels.close_surfaces_to_right("t", "b"));
    assert_eq!(ids(&panels, "t"), ["a", "b"]);
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("b"));
    assert!(!panels.close_surfaces_to_right("t", "b"));
    assert!(panels.close_other_surfaces("t", "a"));
    assert_eq!(ids(&panels, "t"), ["a"]);
    assert!(!panels.close_other_surfaces("t", "a"));
    assert!(panels.close_all_surfaces("t"));
    assert!(!panels.is_open("t"));
}

#[test]
fn move_reorders_tabs() {
    let mut panels = with_tabs(&["a", "b", "c"]);
    assert!(panels.move_surface("t", "a", 2));
    assert_eq!(ids(&panels, "t"), ["b", "c", "a"]);
    assert!(panels.move_surface("t", "a", 0));
    assert!(!panels.move_surface("t", "a", 0));
}

#[test]
fn closed_tabs_reopen_newest_first_with_the_active_tab_last_closed() {
    let mut panels = with_tabs(&["a", "b", "c"]);
    panels.activate("t", "b");
    panels.close_all_surfaces("t");
    let order: Vec<_> = panels.closed_views().iter().filter_map(|view| view.surface().map(Surface::id)).collect();
    assert_eq!(order, ["b", "c", "a"]);
    assert_eq!(reopen(&mut panels, |_| true), Some("t".to_owned()));
    assert_eq!(panels.active_surface("t").map(Surface::id), Some("b"));
    assert_eq!(panels.closed_views().len(), 2);
}

#[test]
fn reopen_skips_missing_threads_and_drops_tabs_already_open() {
    let mut panels = RightPanels::default();
    panels.open("gone", tab("x"));
    panels.close_surface("gone", "x");
    panels.open("t", tab("a"));
    panels.close_surface("t", "a");
    panels.open("t", tab("b"));
    panels.close_surface("t", "b");
    panels.open("t", tab("b"));
    // Newest first: t/b (already open, dropped), t/a (restored), gone/x (kept, thread missing).
    assert_eq!(reopen(&mut panels, |thread| thread == "t"), Some("t".to_owned()));
    assert_eq!(ids(&panels, "t"), ["b", "a"]);
    let left: Vec<_> = panels.closed_views().iter().map(|view| view.thread_id.as_str()).collect();
    assert_eq!(left, ["gone"]);
    assert_eq!(reopen(&mut panels, |thread| thread == "t"), None);
}

#[test]
fn side_chat_tabs_persist_like_the_web_and_skip_deleted_chats_on_reopen() {
    let json = r#"{ "threads": { "t": { "isOpen": true, "activeSurfaceId": "side-chat:s",
        "surfaces": [{ "id": "side-chat:s", "kind": "side-chat", "threadId": "s" }] } } }"#;
    let mut panels: RightPanels = serde_json::from_str(json).expect("decode");
    assert_eq!(panels.active_surface("t"), Some(&Surface::side_chat("s")));
    let round = serde_json::to_string(&panels).expect("encode");
    assert!(round.contains(r#"{"id":"side-chat:s","kind":"side-chat","threadId":"s"}"#), "{round}");
    panels.close_surface("t", "side-chat:s");
    panels.open("t", Surface::side_chat("gone"));
    panels.close_surface("t", "side-chat:gone");
    // Newest first is the deleted side chat; it stays skipped and the live one comes back.
    assert_eq!(reopen(&mut panels, |thread| thread != "gone"), Some("t".to_owned()));
    assert_eq!(ids(&panels, "t"), ["side-chat:s"]);
}

#[test]
fn closed_history_dedupes_and_is_bounded() {
    let mut panels = RightPanels::default();
    for round in 0..2 {
        for index in 0..15 {
            let id = format!("tab-{index}");
            panels.open("t", tab(&id));
            panels.close_surface("t", &id);
        }
        assert_eq!(panels.closed_views().len(), 15, "round {round}");
    }
    for index in 15..30 {
        let id = format!("tab-{index}");
        panels.open("t", tab(&id));
        panels.close_surface("t", &id);
    }
    assert_eq!(panels.closed_views().len(), 20);
    assert_eq!(panels.closed_views()[0].surface().map(Surface::id), Some("tab-29"));
}

#[test]
fn widths_and_maximized_state_are_per_thread() {
    let mut panels = RightPanels::default();
    assert!(panels.set_width("t", 600.));
    assert!(!panels.set_width("t", 600.));
    assert_eq!(panels.width("t"), Some(600.));
    assert_eq!(panels.width("u"), None);
    panels.open("t", Surface::diff());
    assert!(panels.set_maximized("t", true));
    assert!(panels.panel("t").maximized);
    assert!(panels.remove_thread("t"));
    assert_eq!(panels.width("t"), None);
}

#[test]
fn persisted_state_drops_unknown_surfaces() {
    let json = r#"{
        "threads": { "t": { "isOpen": true, "activeSurfaceId": "diff",
            "surfaces": [{ "id": "terminal:1", "kind": "terminal", "terminalIds": ["1"] }, { "id": "diff", "kind": "diff" }] } },
        "widths": { "t": 480.0 },
        "closed": [{ "threadId": "t", "surface": { "id": "terminal:2", "kind": "terminal" } },
                   { "threadId": "t", "surface": { "id": "diff", "kind": "diff" } }]
    }"#;
    let panels: RightPanels = serde_json::from_str(json).expect("decode");
    assert_eq!(ids(&panels, "t"), ["diff"]);
    assert_eq!(panels.width("t"), Some(480.));
    assert_eq!(panels.closed_views().len(), 1);
    let round = serde_json::to_string(&panels).expect("encode");
    assert_eq!(serde_json::from_str::<RightPanels>(&round).expect("re-decode"), panels);
    assert!(round.contains(r#""surfaces":[{"id":"diff","kind":"diff"}]"#));
}

#[test]
fn width_clamps_to_the_window_and_the_thread_column() {
    assert_eq!(clamp_width(DEFAULT_WIDTH, 1280., 1024.), 540.);
    assert_eq!(clamp_width(100., 1280., 1024.), MIN_WIDTH);
    // 70% of 1280 is 896; the thread column keeps 360 of the 1024px row, capping at 664.
    assert_eq!(clamp_width(2000., 1280., 1024.), 664.);
    // A narrow window never inverts the clamp.
    assert_eq!(clamp_width(500., 720., 464.), MIN_WIDTH);
}

#[test]
fn persisted_state_survives_bad_entries_and_repairs_the_active_tab() {
    let json = r#"{
        "threads": {
            "kept": { "isOpen": true, "activeSurfaceId": "terminal:1",
                "surfaces": [{ "id": "terminal:1", "kind": "terminal" }, { "id": "diff", "kind": "diff" }] },
            "hidden": { "isOpen": false, "activeSurfaceId": "gone", "surfaces": [{ "id": "diff", "kind": "diff" }] },
            "broken": "not a panel",
            "emptied": { "isOpen": true, "activeSurfaceId": "terminal:2", "surfaces": [{ "id": "terminal:2", "kind": "terminal" }] }
        },
        "widths": { "kept": 480, "bad": "wide" },
        "closed": "nope"
    }"#;
    let panels: RightPanels = serde_json::from_str(json).expect("decode");
    // The active tab was dropped, so the first remaining one takes over while open.
    assert_eq!(panels.panel("kept").active_surface_id.as_deref(), Some("diff"));
    assert!(panels.is_open("kept"));
    assert_eq!(panels.panel("hidden").active_surface_id, None);
    assert!(!panels.is_open("hidden"));
    assert_eq!(panels.panel("broken"), &ThreadPanel::default());
    assert!(!panels.threads.contains_key("emptied"));
    assert_eq!((panels.width("kept"), panels.width("bad")), (Some(480.), None));
    assert!(panels.closed_views().is_empty());
}

#[test]
fn lenient_maps_keep_good_entries() {
    let value = serde_json::json!({ "a": 1, "b": "x", "c": 3 });
    let map: HashMap<String, u32> = decode_lenient_map(Some(&value));
    assert_eq!(map.len(), 2);
    assert!(decode_lenient_map::<u32>(None).is_empty());
}

fn file_reveal(panels: &RightPanels, thread: &str, id: &str) -> Option<(Option<u64>, u64)> {
    panels.panel(thread).surfaces.iter().find(|surface| surface.id == id).and_then(|surface| match &surface.kind {
        SurfaceKind::File { reveal_line, reveal_request_id, .. } => Some((*reveal_line, *reveal_request_id)),
        _ => None,
    })
}

#[test]
fn open_file_replaces_the_explorer_and_bumps_the_reveal() {
    let mut panels = RightPanels::default();
    assert!(panels.open_file("t", None, None));
    assert_eq!(ids(&panels, "t"), vec!["files"]);
    panels.open("t", Surface::diff());
    assert!(panels.open_file("t", Some("src/a.rs"), Some(12)));
    assert_eq!(ids(&panels, "t"), vec!["diff", "file:src/a.rs"]);
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("file:src/a.rs"));
    assert_eq!(file_reveal(&panels, "t", "file:src/a.rs"), Some((Some(12), 1)));
    panels.activate("t", "diff");
    assert!(panels.open_file("t", Some("src/a.rs"), None));
    assert_eq!(file_reveal(&panels, "t", "file:src/a.rs"), Some((None, 2)));
    assert_eq!(ids(&panels, "t"), vec!["diff", "file:src/a.rs"]);
    assert_eq!(panels.active_surface("t").map(Surface::title).as_deref(), Some("a.rs"));
}

#[test]
fn navigate_folder_moves_in_place_or_beside_the_source() {
    let mut panels = RightPanels::default();
    panels.open_file("t", Some("src/a.rs"), None);
    assert!(panels.navigate_folder("t", "file:src/a.rs", "src", true));
    assert_eq!(ids(&panels, "t"), vec!["file:src/a.rs", "file:src"]);
    assert!(panels.navigate_folder("t", "file:src", "", false));
    assert_eq!(ids(&panels, "t"), vec!["file:src/a.rs", "files"]);
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("files"));
    assert!(panels.close_file_surfaces("t"));
    assert!(!panels.is_open("t"));
}

#[test]
fn persisted_file_tabs_decode_and_normalize_reveal_lines() {
    let value = serde_json::json!({ "threads": { "t": {
        "isOpen": true,
        "activeSurfaceId": "file:a.rs",
        "surfaces": [
            { "id": "file:a.rs", "kind": "file", "relativePath": "a.rs", "revealLine": 3.7, "revealRequestId": 2 },
            { "id": "file:b.rs", "kind": "file", "relativePath": "b.rs", "revealLine": "x" },
            { "id": "files", "kind": "files" },
            { "id": "file:c.rs", "kind": "file" },
        ],
    } } });
    let panels = RightPanels::from_value(&value);
    assert_eq!(ids(&panels, "t"), vec!["file:a.rs", "file:b.rs", "files"]);
    assert_eq!(file_reveal(&panels, "t", "file:a.rs"), Some((Some(3), 2)));
    assert_eq!(file_reveal(&panels, "t", "file:b.rs"), Some((None, 0)));
    let saved = serde_json::to_value(&panels).unwrap();
    assert_eq!(saved["threads"]["t"]["surfaces"][0]["relativePath"], "a.rs");
    assert_eq!(saved["threads"]["t"]["surfaces"][0]["revealRequestId"], 2);
}

#[test]
fn browser_tabs_follow_preview_sessions() {
    let mut panels = RightPanels::default();
    panels.open("t", Surface::diff());
    assert!(panels.open_browser("t", None));
    assert!(panels.open_browser("t", Some("a")));
    assert_eq!(ids(&panels, "t"), ["diff", "browser:a"]);
    assert_eq!(panels.active_surface("t").and_then(Surface::preview_tab), Some("a"));
    let hidden = HashSet::from(["h".to_owned()]);
    // A new session gains a tab, an unrevealed one does not, a closed one loses its tab.
    assert!(panels.reconcile_browser_surfaces("t", &["b".to_owned(), "h".to_owned()], &hidden));
    assert_eq!(ids(&panels, "t"), ["diff", "browser:b"]);
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("browser:b"));
    assert!(!panels.reconcile_browser_surfaces("t", &["b".to_owned(), "h".to_owned()], &hidden));
    // Closing a session tab is not remembered for cmd-shift-t: its session is gone.
    panels.close_surface("t", "browser:b");
    assert!(panels.closed_views().iter().all(|view| view.surface().is_some_and(|surface| surface.preview_tab().is_none())));
    // The last session tab closing on the server hides the panel.
    let mut only = RightPanels::default();
    only.open_browser("t", Some("a"));
    assert!(only.reconcile_browser_surfaces("t", &[], &HashSet::new()));
    assert!(!only.is_open("t"));
}

#[test]
fn browser_tabs_persist_in_the_web_shape() {
    let value = serde_json::to_value(Surface::browser(Some("a"))).unwrap();
    assert_eq!(value, serde_json::json!({ "id": "browser:a", "kind": "preview", "resourceId": "a" }));
    let placeholder: Surface = serde_json::from_value(serde_json::json!({ "id": "browser:new", "kind": "preview", "resourceId": null })).unwrap();
    assert_eq!(placeholder, Surface::browser(None));
}

#[test]
fn an_opened_device_takes_the_placeholder_tab_or_fronts_its_own() {
    use crate::device::Platform;
    let target = |device: &str| DeviceTarget { host_id: "local".into(), device_id: device.into(), platform: Platform::Ios, name: format!("iPhone {device}") };
    let mut panels = RightPanels::default();
    panels.open("t", Surface::diff());
    panels.open("t", Surface::device(None));
    panels.open("t", Surface::files());
    // The placeholder becomes the device's tab in place.
    assert!(panels.open_device("t", target("A 1")));
    assert_eq!(ids(&panels, "t"), ["diff", "device:local:A%201", "files"]);
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("device:local:A%201"));
    assert_eq!(panels.panel("t").active().map(Surface::title).as_deref(), Some("iPhone A 1"));
    // A second list opening the same device drops the list and fronts the existing tab.
    panels.open("t", Surface::device(None));
    panels.activate("t", "diff");
    panels.activate("t", "device");
    assert!(panels.open_device("t", target("A 1")));
    assert_eq!(ids(&panels, "t"), ["diff", "device:local:A%201", "files"]);
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("device:local:A%201"));
    // Without a placeholder the device gets a new tab.
    assert!(panels.open_device("t", target("B")));
    assert_eq!(ids(&panels, "t"), ["diff", "device:local:A%201", "files", "device:local:B"]);
    // Tabs persist and restore with their target.
    let restored = RightPanels::from_value(&serde_json::to_value(&panels).unwrap());
    assert_eq!(restored.panel("t").surfaces, panels.panel("t").surfaces);
    assert_eq!(Surface::device(None).title(), "Device");
    // Dismissing setup over the last tab keeps the panel open on the launcher.
    let mut panels = RightPanels::default();
    panels.open("t", Surface::device(None));
    assert!(panels.close_surface("t", "device") | panels.show("t"));
    assert!(panels.is_open("t") && panels.panel("t").surfaces.is_empty());
}

#[test]
fn tool_diff_keeps_its_source_when_the_side_chat_pane_is_replaced() {
    use crate::diff::{DiffSelection, ToolCallTarget};
    let mut panels = RightPanels::default();
    let target = ToolCallTarget { thread_id: "child".into(), item_id: "f1".into(), revision: "live".into() };
    let selection = DiffSelection::ToolCall(target);
    panels.open("parent", Surface::side_chat("child"));
    assert_eq!(panels.active_thread_source("parent", &selection), Some("child"));
    panels.open("parent", Surface::diff());
    assert_eq!(panels.active_thread_source("parent", &selection), Some("child"), "replacing the child view must retain its stream");
    assert_eq!(panels.active_surface("child"), None, "the diff belongs to the parent panel");
    panels.close_surface("parent", "side-chat:child");
    assert_eq!(panels.active_thread_source("parent", &selection), Some("child"), "the source outlives the child tab too");
    assert_eq!(panels.active_thread_source("parent", &DiffSelection::Unstaged), None);
    let other = DiffSelection::ToolCall(ToolCallTarget { thread_id: "other-child".into(), item_id: "f1".into(), revision: "live".into() });
    assert_eq!(panels.active_thread_source("parent", &other), Some("other-child"));
    assert_eq!(panels.active_thread_source("other", &selection), None);
    panels.hide("parent");
    assert_eq!(panels.active_thread_source("parent", &selection), None);
    panels.show("parent");
    assert_eq!(panels.active_thread_source("parent", &selection), Some("child"));
    panels.close_surface("parent", "diff");
    assert_eq!(panels.active_thread_source("parent", &selection), None);
    panels.remove_thread("parent");
    assert_eq!(panels.active_thread_source("parent", &selection), None);
    assert_eq!(RightPanels::default().active_thread_source("parent", &selection), None);
}

fn session(tab: &str, extra: Value) -> crate::preview::SessionSnapshot {
    let mut value = serde_json::json!({
        "threadId": "t", "tabId": tab, "updatedAt": "",
        "navStatus": { "_tag": "Success", "url": format!("http://localhost:3000/{tab}"), "title": "App" },
    });
    value.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    crate::preview::SessionSnapshot::decode(value).expect("snapshot")
}

fn closed_browser_tabs(panels: &RightPanels) -> Vec<&str> {
    panels
        .closed_views()
        .iter()
        .filter_map(|view| match &view.target {
            ClosedTarget::Browser { snapshot } => Some(snapshot.tab_id.as_str()),
            ClosedTarget::PanelTab { .. } => None,
        })
        .collect()
}

#[test]
fn closed_browser_tabs_reopen_as_a_new_session_with_their_settings() {
    let extra = serde_json::json!({ "viewport": { "_tag": "fixed", "width": 390, "height": 844 }, "profileId": "work", "zoomFactor": 1.25, "colorScheme": "dark" });
    let snapshot = BrowserSnapshot::from_session(&session("a", extra));
    assert_eq!(snapshot.url.as_deref(), Some("http://localhost:3000/a"));
    assert_eq!(
        snapshot.open_input("t"),
        serde_json::json!({ "threadId": "t", "runtime": "server", "url": "http://localhost:3000/a",
            "viewport": { "_tag": "fixed", "width": 390, "height": 844 }, "profileId": "work", "zoomFactor": 1.25, "colorScheme": "dark" })
    );
    // An idle tab reopens blank, with only the settings it had.
    let idle = session("idle", serde_json::json!({ "navStatus": { "_tag": "Idle" } }));
    assert_eq!(BrowserSnapshot::from_session(&idle).open_input("t"), serde_json::json!({ "threadId": "t", "runtime": "server" }));

    let mut panels = RightPanels::default();
    panels.open("t", Surface::diff());
    panels.remember_browser("t", snapshot.clone());
    panels.remember_browser("t", BrowserSnapshot::from_session(&session("a", Value::Null)));
    assert_eq!(closed_browser_tabs(&panels), ["a"], "one entry per session");
    panels.remember_browser("t", snapshot.clone());
    let Some(Reopened::Browser(view)) = panels.reopen_closed(|_| true, true, |_| true) else { panic!("browser") };
    assert_eq!((view.thread_id.as_str(), &view.target), ("t", &ClosedTarget::Browser { snapshot: snapshot.clone() }));
    assert!(panels.closed_views().is_empty());
    // The open failed: the entry goes behind newer ones rather than being lost.
    panels.close_surface("t", "diff");
    panels.defer_closed(view.clone());
    assert_eq!(panels.closed_views().last(), Some(&view));
    assert_eq!(panels.closed_views().len(), 2);
}

#[test]
fn a_browser_tab_whose_close_failed_is_dropped_from_history() {
    let mut panels = RightPanels::default();
    panels.open_browser("t", Some("a"));
    panels.remember_browser("t", BrowserSnapshot::from_session(&session("a", Value::Null)));
    // The server refused the close, so the tab came back while the panel was hidden.
    panels.hide("t");
    assert_eq!(panels.reopen_closed(|_| true, true, |_| true), None);
    assert!(panels.closed_views().is_empty());
}

#[test]
fn incognito_browser_tabs_are_remembered_but_never_saved() {
    let mut panels = RightPanels::default();
    panels.remember_browser("t", BrowserSnapshot::from_session(&session("work", Value::Null)));
    panels.remember_browser("t", BrowserSnapshot::from_session(&session("private", serde_json::json!({ "profileId": INCOGNITO_BROWSER_PROFILE_ID }))));
    assert_eq!(closed_browser_tabs(&panels), ["private", "work"]);
    let saved = serde_json::to_value(&panels).unwrap();
    assert_eq!(saved["closed"].as_array().map(Vec::len), Some(1), "{saved}");
    assert_eq!(saved["closed"][0]["kind"], "browser");
    assert_eq!(saved["closed"][0]["snapshot"]["tabId"], "work");
    let restored = RightPanels::from_value(&saved);
    assert_eq!(closed_browser_tabs(&restored), ["work"]);
    // A hand-edited incognito entry on disk is dropped on load too (`migrate`).
    let mut tampered = saved.clone();
    tampered["closed"][0]["snapshot"]["profileId"] = INCOGNITO_BROWSER_PROFILE_ID.into();
    assert!(RightPanels::from_value(&tampered).closed_views().is_empty());
}

#[test]
fn closed_panel_tabs_persist_with_their_kind_and_older_entries_still_load() {
    let mut panels = with_tabs(&["a"]);
    panels.close_surface("t", "a");
    let saved = serde_json::to_value(&panels).unwrap();
    assert_eq!(saved["closed"][0], serde_json::json!({ "threadId": "t", "kind": "panel-tab", "surface": { "id": "a", "kind": "diff" } }));
    let legacy = serde_json::json!({ "closed": [{ "threadId": "t", "surface": { "id": "diff", "kind": "diff" } }, { "threadId": "t", "kind": "later", "surface": {} }] });
    let restored = RightPanels::from_value(&legacy);
    assert_eq!(restored.closed_views(), [ClosedView::panel_tab("t", Surface::diff())]);
}

#[test]
fn reopen_waits_for_the_shell_before_skipping_missing_threads() {
    let mut panels = RightPanels::default();
    panels.open("t", tab("a"));
    panels.close_surface("t", "a");
    panels.open("loading", tab("b"));
    panels.close_surface("loading", "b");
    // The newest entry's thread may still arrive: nothing older reopens past it.
    assert_eq!(panels.reopen_closed(|thread| thread == "t", false, |_| true), None);
    assert_eq!(panels.closed_views().len(), 2);
    assert_eq!(panels.reopen_closed(|thread| thread == "t", true, |_| true), Some(Reopened::Thread("t".into())));
    assert_eq!(panels.closed_views().len(), 1);
}

#[test]
fn files_reopen_only_with_a_workspace_and_otherwise_wait_at_the_back() {
    let mut panels = RightPanels::default();
    panels.open_file("t", Some("src/a.rs"), Some(4));
    panels.open_file("t", None, None);
    panels.open("t", tab("x"));
    panels.close_all_surfaces("t");
    let order = |panels: &RightPanels| -> Vec<String> { panels.closed_views().iter().filter_map(|view| view.surface().map(|surface| surface.id.clone())).collect() };
    assert_eq!(order(&panels), ["x", "files", "file:src/a.rs"]);
    panels.reopen_closed(|_| true, true, |_| true);
    // No workspace: the explorer is deferred behind the rest instead of dropped, and nothing opens.
    assert_eq!(panels.reopen_closed(|_| true, true, |_| false), None);
    assert_eq!(order(&panels), ["file:src/a.rs", "files"]);
    assert_eq!(ids(&panels, "t"), ["x"]);
    // With one, it reopens through `openFile`: a fresh reveal of its line.
    assert_eq!(panels.reopen_closed(|_| true, true, |_| true), Some(Reopened::Thread("t".into())));
    assert_eq!(file_reveal(&panels, "t", "file:src/a.rs"), Some((Some(4), 1)));
    // The placeholder browser tab reopens as one, a session's panel tab never does.
    panels.open_browser("t", None);
    panels.close_surface("t", "browser:new");
    assert_eq!(panels.reopen_closed(|_| true, true, |_| false), Some(Reopened::Thread("t".into())));
    assert_eq!(panels.panel("t").active_surface_id.as_deref(), Some("browser:new"));
}

#[test]
fn renamed_device_tabs_reopen_in_the_lists_place_with_their_name() {
    use crate::device::Platform;
    let target = DeviceTarget { host_id: "local".into(), device_id: "A".into(), platform: Platform::Android, name: "Pixel".into() };
    let mut panels = RightPanels::default();
    panels.open("t", Surface::device(Some(target.clone())));
    assert!(panels.rename_device("t", "device:local:A", "  Test phone "));
    assert_eq!(panels.panel("t").active().map(Surface::title).as_deref(), Some("Test phone"));
    assert!(panels.rename_device("t", "device:local:A", " "));
    assert_eq!(panels.panel("t").active().map(Surface::title).as_deref(), Some("Pixel"));
    assert!(!panels.rename_device("t", "missing", "x"));
    panels.rename_device("t", "device:local:A", "Test phone");
    panels.close_surface("t", "device:local:A");
    panels.open("t", Surface::diff());
    panels.open("t", Surface::device(None));
    assert_eq!(panels.reopen_closed(|_| true, true, |_| true), Some(Reopened::Thread("t".into())));
    assert_eq!(ids(&panels, "t"), ["diff", "device:local:A"]);
    assert_eq!(panels.panel("t").active().map(Surface::title).as_deref(), Some("Test phone"));
}
