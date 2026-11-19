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

#[test]
fn narrow_window_sheet_mode_and_widths_match_web_breakpoints() {
    assert!(is_sheet_mode(SHEET_BREAKPOINT));
    assert!(is_sheet_mode(760.));
    assert!(!is_sheet_mode(SHEET_BREAKPOINT + 1.));
    // <= 760px: min(88vw, 24rem = 384px)
    assert!((sheet_width(400.) - 352.).abs() < 0.01);
    assert_eq!(sheet_width(600.), 384.);
    assert_eq!(sheet_width(760.), 384.);
    // 761..=980px: min(42vw, 28rem = 448px), min-w-80 = 320px, floored to pixels
    assert_eq!(sheet_width(900.), 378.);
    assert_eq!(sheet_width(980.), 411.);
}

#[test]
fn terminal_panel_tabs_split_activate_close_persist_and_skip_closed_history() {
    let mut panels = RightPanels::default();
    assert!(panels.open_terminal("t", "term-2"));
    assert_eq!(ids(&panels, "t"), ["terminal:term-2"]);
    assert_eq!(panels.panel_terminal_ids("t"), HashSet::from(["term-2".to_owned()]));
    assert!(panels.split_terminal("t", "terminal:term-2", "term-3", SplitDirection::Vertical));
    assert!(panels.split_terminal("t", "terminal:term-2", "term-4", SplitDirection::Horizontal));
    assert!(panels.split_terminal("t", "terminal:term-2", "term-5", SplitDirection::Horizontal));
    // Capped at MAX_TERMINALS_PER_GROUP (4).
    assert!(!panels.split_terminal("t", "terminal:term-2", "term-6", SplitDirection::Horizontal));
    assert_eq!(
        panels.panel_terminal_ids("t"),
        HashSet::from(["term-2".into(), "term-3".into(), "term-4".into(), "term-5".into()])
    );
    assert!(panels.activate_terminal("t", "terminal:term-2", "term-3"));
    // Round-trip persistence keeps valid terminal tabs.
    let saved = serde_json::to_value(&panels).unwrap();
    let restored = RightPanels::from_value(&saved);
    assert_eq!(restored.panel("t").surfaces, panels.panel("t").surfaces);
    // Closing terminals inside a split keeps the tab until the last terminal closes.
    assert!(panels.close_terminal("t", "terminal:term-2", "term-3"));
    assert!(panels.close_terminal("t", "terminal:term-2", "term-4"));
    assert!(panels.close_terminal("t", "terminal:term-2", "term-5"));
    assert!(panels.is_open("t"));
    assert!(panels.close_terminal("t", "terminal:term-2", "term-2"));
    assert!(!panels.is_open("t"));
    assert!(panels.closed_views().is_empty(), "terminal tabs are never remembered for cmd-shift-t");
}

#[test]
fn pull_request_tabs_track_detail_tab_strip_reveal_url_and_reopen() {
    let reveal_url = "https://github.com/owner/repo/pull/42#discussion_r1";
    let target = PullRequestTarget {
        environment_id: None,
        project_id: "proj-1".into(),
        host: Some("github.com".into()),
        repository: "owner/repo".into(),
        number: 42,
        url: Some("https://github.com/owner/repo/pull/42".into()),
        reveal_url: Some(reveal_url.into()),
        detail_tab: Some(PullRequestDetailTab::Summary),
    };
    let expected_id = pull_request_surface_id(None, "proj-1", Some("github.com"), "owner/repo", 42);
    let mut panels = RightPanels::default();
    assert!(panels.open_pull_request("t", target));
    assert_eq!(ids(&panels, "t"), [expected_id.as_str()]);
    assert_eq!(panels.active_surface("t").map(Surface::title).as_deref(), Some("#42"));
    assert!(panels.set_pull_request_detail_tab("t", &expected_id, PullRequestDetailTab::Code));
    // Persistence strips ephemeral revealUrl.
    let saved = serde_json::to_value(&panels).unwrap();
    let restored = RightPanels::from_value(&saved);
    match &restored.panel("t").surfaces[0].kind {
        SurfaceKind::PullRequest { detail_tab, reveal_url, number, .. } => {
            assert_eq!(*number, 42);
            assert_eq!(*detail_tab, Some(PullRequestDetailTab::Code));
            assert_eq!(*reveal_url, None);
        }
        other => panic!("expected pull request surface, got {other:?}"),
    }
    assert!(panels.settle_pull_request_reveal("t", &expected_id, reveal_url));
    assert!(!panels.settle_pull_request_reveal("t", &expected_id, reveal_url));
    panels.close_surface("t", &expected_id);
    assert_eq!(reopen(&mut panels, |_| true), Some("t".into()));
    assert_eq!(ids(&panels, "t"), [expected_id.as_str()]);
}

#[test]
fn folder_trail_and_attachment_tabs_preserve_history_and_survive_workspace_close() {
    assert_eq!(retained_breadcrumb_trail(Some("src/a/b"), "src/a"), "src/a/b");
    assert_eq!(retained_breadcrumb_trail(Some("src/a/b"), "docs"), "docs");
    let mut panels = RightPanels::default();
    panels.open_file("t", Some("src/a/b.rs"), None);
    assert!(panels.navigate_folder_with_trail("t", "file:src/a/b.rs", "src", "src/a", false));
    assert_eq!(panels.active_surface("t").and_then(Surface::folder_trail), Some("src/a"));
    // Attachment tabs survive close_file_surfaces and reopen even without a workspace.
    let attachment = FileAttachment {
        id: "att-1".into(),
        name: "spec.txt".into(),
        mime_type: "text/plain".into(),
        size_bytes: 128,
        url: Some("/api/attachments/att-1".into()),
    };
    assert!(panels.open_attachment("t", attachment));
    assert!(panels.close_file_surfaces("t"));
    assert_eq!(ids(&panels, "t"), ["attachment:att-1"]);
    panels.close_surface("t", "attachment:att-1");
    assert_eq!(panels.reopen_closed(|_| true, true, |_| false), Some(Reopened::Thread("t".into())));
    assert_eq!(ids(&panels, "t"), ["attachment:att-1"]);
    assert!(panels.active_surface("t").is_some_and(Surface::is_attachment));
}

#[test]
fn proactive_panels_coordinate_pr_priority_retargeting_and_diff_thresholds() {
    use crate::diff::{CheckpointFile, TurnDiffSummary};
    let mut panels = RightPanels::default();
    let mut coord = ProactivePanelCoordinator::default();
    let pr = |number: u64| PullRequestTarget {
        environment_id: None,
        project_id: "p1".into(),
        host: None,
        repository: "o/r".into(),
        number,
        url: Some(format!("https://github.com/o/r/pull/{number}")),
        reveal_url: None,
        detail_tab: None,
    };
    let summary = |run_id: &str, files: usize, additions: u64| TurnDiffSummary {
        checkpoint_id: format!("cp-{run_id}"),
        run_id: run_id.into(),
        turn_count: 1,
        status: "ready".into(),
        files: (0..files)
            .map(|idx| CheckpointFile {
                path: format!("f{idx}.rs"),
                kind: "modified".into(),
                additions: if idx == 0 { additions } else { 0 },
                deletions: 0,
            })
            .collect(),
        files_total: None,
        completed_at: "2026-11-19T00:00:00Z".into(),
    };
    let summaries_big = [summary("turn-1", 5, 80)];

    // Disabled: does not open proactively on entering a thread.
    let disabled = coord.evaluate(
        &mut panels,
        ProactiveEvaluationInput {
            thread_id: Some("t-disabled"),
            enabled: false,
            settings_hydrated: true,
            thread_detail_loading: false,
            running_turn_id: None,
            latest_run_id: Some("turn-1"),
            latest_run_completed: true,
            latest_run_settled: true,
            turn_diff_summaries: &summaries_big,
            is_git_repo: Some(true),
            linked_pull_request: Some(pr(7)),
            has_linked_pull_request_detail: true,
            pull_requests_key: Some("o/r#7".into()),
            pull_requests_capability_known: true,
            supports_pull_requests: true,
            pull_requests_surface_available: true,
            visible_pull_request_count: 1,
        },
    );
    assert_eq!(disabled, ProactiveEvaluationOutcome::default());
    assert!(!panels.is_open("t-disabled"));

    // Enabled with linked PR on entering a fresh thread: PR takes priority over Changes.
    let pr_open = coord.evaluate(
        &mut panels,
        ProactiveEvaluationInput {
            thread_id: Some("t"),
            enabled: true,
            settings_hydrated: true,
            thread_detail_loading: false,
            running_turn_id: None,
            latest_run_id: Some("turn-1"),
            latest_run_completed: true,
            latest_run_settled: true,
            turn_diff_summaries: &summaries_big,
            is_git_repo: Some(true),
            linked_pull_request: Some(pr(7)),
            has_linked_pull_request_detail: true,
            pull_requests_key: Some("o/r#7".into()),
            pull_requests_capability_known: true,
            supports_pull_requests: true,
            pull_requests_surface_available: true,
            visible_pull_request_count: 1,
        },
    );
    assert!(pr_open.panels_changed);
    assert!(!pr_open.diff_opened);
    let pr7_id = pull_request_surface_id(None, "p1", None, "o/r", 7);
    assert_eq!(panels.active_surface("t").map(Surface::id), Some(pr7_id.as_str()));

    // Retargets an already-open thread PR even when proactive setting is turned off.
    let retarget = coord.evaluate(
        &mut panels,
        ProactiveEvaluationInput {
            thread_id: Some("t"),
            enabled: false,
            settings_hydrated: true,
            thread_detail_loading: false,
            running_turn_id: None,
            latest_run_id: Some("turn-1"),
            latest_run_completed: true,
            latest_run_settled: true,
            turn_diff_summaries: &summaries_big,
            is_git_repo: Some(true),
            linked_pull_request: Some(pr(8)),
            has_linked_pull_request_detail: true,
            pull_requests_key: Some("o/r#8".into()),
            pull_requests_capability_known: true,
            supports_pull_requests: true,
            pull_requests_surface_available: true,
            visible_pull_request_count: 1,
        },
    );
    assert!(retarget.panels_changed);
    let pr8_id = pull_request_surface_id(None, "p1", None, "o/r", 8);
    assert_eq!(panels.active_surface("t").map(Surface::id), Some(pr8_id.as_str()));

    // Fresh thread without PR: defers until is_git_repo is known.
    let mut diff_panels = RightPanels::default();
    let mut diff_coord = ProactivePanelCoordinator::default();
    let summaries_small = [summary("turn-1", 2, 49)];
    assert_eq!(
        diff_coord.evaluate(
            &mut diff_panels,
            ProactiveEvaluationInput {
                thread_id: Some("t2"),
                enabled: true,
                settings_hydrated: true,
                thread_detail_loading: false,
                running_turn_id: None,
                latest_run_id: Some("turn-1"),
                latest_run_completed: true,
                latest_run_settled: true,
                turn_diff_summaries: &summaries_small,
                is_git_repo: None,
                linked_pull_request: None,
                has_linked_pull_request_detail: false,
                pull_requests_key: None,
                pull_requests_capability_known: true,
                supports_pull_requests: false,
                pull_requests_surface_available: false,
                visible_pull_request_count: 0,
            },
        ),
        ProactiveEvaluationOutcome::default()
    );
    // Below threshold (2 files, 49 lines): stays closed.
    assert_eq!(
        diff_coord.evaluate(
            &mut diff_panels,
            ProactiveEvaluationInput {
                thread_id: Some("t2"),
                enabled: true,
                settings_hydrated: true,
                thread_detail_loading: false,
                running_turn_id: None,
                latest_run_id: Some("turn-1"),
                latest_run_completed: true,
                latest_run_settled: true,
                turn_diff_summaries: &summaries_small,
                is_git_repo: Some(true),
                linked_pull_request: None,
                has_linked_pull_request_detail: false,
                pull_requests_key: None,
                pull_requests_capability_known: true,
                supports_pull_requests: false,
                pull_requests_surface_available: false,
                visible_pull_request_count: 0,
            },
        ),
        ProactiveEvaluationOutcome::default()
    );
    // Next turn runs then settles with >= 50 lines: opens diff once.
    let summaries_next = [summary("turn-2", 1, 50)];
    diff_coord.evaluate(
        &mut diff_panels,
        ProactiveEvaluationInput {
            thread_id: Some("t2"),
            enabled: true,
            settings_hydrated: true,
            thread_detail_loading: false,
            running_turn_id: Some("turn-2"),
            latest_run_id: Some("turn-2"),
            latest_run_completed: false,
            latest_run_settled: false,
            turn_diff_summaries: &summaries_small,
            is_git_repo: Some(true),
            linked_pull_request: None,
            has_linked_pull_request_detail: false,
            pull_requests_key: None,
            pull_requests_capability_known: true,
            supports_pull_requests: false,
            pull_requests_surface_available: false,
            visible_pull_request_count: 0,
        },
    );
    let open_diff = diff_coord.evaluate(
        &mut diff_panels,
        ProactiveEvaluationInput {
            thread_id: Some("t2"),
            enabled: true,
            settings_hydrated: true,
            thread_detail_loading: false,
            running_turn_id: None,
            latest_run_id: Some("turn-2"),
            latest_run_completed: true,
            latest_run_settled: true,
            turn_diff_summaries: &summaries_next,
            is_git_repo: Some(true),
            linked_pull_request: None,
            has_linked_pull_request_detail: false,
            pull_requests_key: None,
            pull_requests_capability_known: true,
            supports_pull_requests: false,
            pull_requests_surface_available: false,
            visible_pull_request_count: 0,
        },
    );
    assert!(open_diff.diff_opened);
    assert_eq!(diff_panels.active_surface("t2"), Some(&Surface::diff()));
    // User closes diff: manual choice prevents reopening for turn-2.
    diff_panels.close_surface("t2", "diff");
    let after_close = diff_coord.evaluate(
        &mut diff_panels,
        ProactiveEvaluationInput {
            thread_id: Some("t2"),
            enabled: true,
            settings_hydrated: true,
            thread_detail_loading: false,
            running_turn_id: None,
            latest_run_id: Some("turn-2"),
            latest_run_completed: true,
            latest_run_settled: true,
            turn_diff_summaries: &summaries_next,
            is_git_repo: Some(true),
            linked_pull_request: None,
            has_linked_pull_request_detail: false,
            pull_requests_key: None,
            pull_requests_capability_known: true,
            supports_pull_requests: false,
            pull_requests_surface_available: false,
            visible_pull_request_count: 0,
        },
    );
    assert_eq!(after_close, ProactiveEvaluationOutcome::default());
}

#[test]
fn reopened_browser_snapshot_falls_back_to_browser_defaults() {
    let idle = session("a", Value::Null);
    let snapshot = BrowserSnapshot::from_session(&idle);
    let defaults = crate::preview::viewport::BrowserDefaults {
        viewport: crate::preview::viewport::ViewportSetting::Preset { preset_id: "iphone".into(), width: 390, height: 844 },
        zoom_factor: 1.25,
        appearance: crate::preview::viewport::AppearancePreference::Dark,
        ..Default::default()
    };
    let payload = snapshot.open_input_with_defaults("t", &defaults);
    assert_eq!(payload["zoomFactor"], 1.25);
    assert_eq!(payload["colorScheme"], "dark");
    assert_eq!(payload["viewport"]["_tag"], "preset");
}

#[test]
fn issue_surface_open_title_persistence_and_reopen() {
    use crate::issues::wire::IssueTrackerKind;

    let mut panels = RightPanels::default();
    let gh = IssueTarget {
        environment_id: Some("env-1".into()),
        project_id: "proj/1".into(),
        tracker: IssueTrackerKind::GitHub,
        host: Some("GitHub.COM".into()),
        scope: "Acme/Web".into(),
        key: "42".into(),
        display_key: None,
        url: Some("https://github.com/acme/web/issues/42".into()),
    };
    assert!(panels.open_issue("t1", gh.clone()));
    let gh_surface = panels.active_surface("t1").unwrap().clone();
    assert_eq!(gh_surface.title(), "#42");
    assert_eq!(
        gh_surface.id(),
        issue_surface_id(
            Some("env-1"),
            "proj/1",
            &IssueTrackerKind::GitHub,
            Some("github.com"),
            "acme/web",
            "42",
        )
    );

    let linear = IssueTarget {
        environment_id: None,
        project_id: "p2".into(),
        tracker: IssueTrackerKind::Linear,
        host: Some("linear.app".into()),
        scope: "ENG".into(),
        key: "ENG-17".into(),
        display_key: Some("ENG-17".into()),
        url: Some("https://linear.app/acme/issue/ENG-17".into()),
    };
    assert!(panels.open_issue("t1", linear));
    let linear_surface = panels.active_surface("t1").unwrap().clone();
    assert_eq!(linear_surface.title(), "ENG-17");
    assert_eq!(panels.panel("t1").surfaces.len(), 2);

    // Reopening the GitHub issue with an explicit displayKey updates it without duplicating the tab.
    let mut gh_updated = gh;
    gh_updated.display_key = Some("#42".into());
    assert!(panels.open_issue("t1", gh_updated));
    assert_eq!(panels.panel("t1").surfaces.len(), 2);
    assert_eq!(panels.active_surface("t1").map(Surface::id), Some(gh_surface.id()));

    // Persistence round-trip + malformed issue entry dropped cleanly.
    let mut persisted = serde_json::to_value(&panels).unwrap();
    persisted["threads"]["t1"]["surfaces"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({ "id": "issue:bad", "kind": "issue", "projectId": "p1" }));
    let restored = RightPanels::from_value(&persisted);
    assert_eq!(restored.panel("t1").surfaces.len(), 2);
    assert_eq!(restored.active_surface("t1").map(Surface::title).as_deref(), Some("#42"));

    // Closing an issue surface remembers it for Cmd-Shift-T reopen.
    let mut panels = restored;
    let gh_id = gh_surface.id().to_owned();
    assert!(panels.close_surface("t1", &gh_id));
    assert_eq!(panels.closed_views().len(), 1);
    assert_eq!(
        panels.reopen_closed(|id| id == "t1", true, |_| true),
        Some(Reopened::Thread("t1".into()))
    );
    assert_eq!(panels.active_surface("t1").map(Surface::id), Some(gh_id.as_str()));
}
