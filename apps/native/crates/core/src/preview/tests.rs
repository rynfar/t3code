use serde_json::json;

use super::*;

fn snapshot(tab: &str, url: Option<&str>, updated_at: &str) -> SessionSnapshot {
    let nav = match url {
        Some(url) => json!({ "_tag": "Success", "url": url, "title": "" }),
        None => json!({ "_tag": "Idle" }),
    };
    SessionSnapshot::decode(json!({
        "threadId": "thread-1",
        "tabId": tab,
        "navStatus": nav,
        "canGoBack": false,
        "canGoForward": false,
        "runtime": "server",
        "headless": true,
        "updatedAt": updated_at,
    }))
    .unwrap()
}

fn event(kind: &str, tab: &str, revision: u64, extra: serde_json::Value) -> PreviewEvent {
    let mut value = json!({
        "type": kind,
        "threadId": "thread-1",
        "tabId": tab,
        "createdAt": "2026-11-02T00:00:09Z",
        "serverEpoch": "epoch-1",
        "revision": revision,
    });
    value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    PreviewEvent::decode(value).unwrap()
}

fn list(revision: u64, sessions: Vec<SessionSnapshot>) -> ListResult {
    ListResult { sessions, server_epoch: "epoch-1".into(), revision }
}

#[test]
fn decodes_snapshots_and_unknown_statuses_forward_compatibly() {
    let decoded = SessionSnapshot::decode(json!({
        "threadId": "t", "tabId": "a", "updatedAt": "x",
        "navStatus": { "_tag": "LoadFailed", "url": "http://localhost:1/", "title": "", "code": -102, "description": "ERR_CONNECTION_REFUSED" },
        "viewport": { "_tag": "fill" }, "zoomFactor": 1.25, "futureField": 1,
    }))
    .unwrap();
    assert!(matches!(decoded.nav_status, NavStatus::LoadFailed { code: -102, .. }));
    assert!(decoded.fills_panel());
    assert_eq!(decoded.zoom(), 1.25);
    let future = SessionSnapshot::decode(json!({ "threadId": "t", "tabId": "a", "navStatus": { "_tag": "Prerendering" } })).unwrap();
    assert_eq!(future.nav_status, NavStatus::Unknown);
    // A list keeps the sessions it can read.
    let result = ListResult::decode(json!({ "serverEpoch": "e", "revision": 4, "sessions": [{ "tabId": 1 }, { "threadId": "t", "tabId": "b", "navStatus": { "_tag": "Idle" } }] })).unwrap();
    assert_eq!(result.sessions.len(), 1);
    assert_eq!(result.revision, 4);
}

#[test]
fn events_apply_in_revision_order_within_one_server_epoch() {
    let mut state = ThreadPreviews::default();
    assert!(state.reconcile(&list(1, vec![snapshot("a", None, "1")])));
    assert!(state.list_loaded);
    assert_eq!(state.active_tab_id.as_deref(), Some("a"));
    assert!(state.apply_event(&event("navigated", "a", 3, json!({ "snapshot": snapshot("a", Some("https://example.com/"), "3").to_value() }))));
    assert_eq!(state.session("a").unwrap().nav_status.url(), Some("https://example.com/"));
    assert_eq!(state.recently_seen_urls, ["https://example.com/"]);
    // An older revision is stale.
    assert!(!state.apply_event(&event("closed", "a", 2, json!({}))));
    // Opening a tab brings it to the front.
    assert!(state.apply_event(&event("opened", "b", 4, json!({ "snapshot": snapshot("b", None, "4").to_value() }))));
    assert_eq!(state.active_tab_id.as_deref(), Some("b"));
    assert!(state.apply_event(&event(
        "failed",
        "b",
        5,
        json!({ "url": "http://localhost:9/", "title": "", "code": -102, "description": "refused" })
    )));
    assert!(matches!(state.session("b").unwrap().nav_status, NavStatus::LoadFailed { .. }));
    assert!(state.apply_event(&event("closed", "b", 6, json!({}))));
    assert_eq!(state.active_tab_id.as_deref(), Some("a"));
    // Another epoch means the server restarted: the caller lists again.
    let mut restarted = event("closed", "a", 7, json!({}));
    restarted.server_epoch = "epoch-2".into();
    assert!(state.needs_relist(&restarted));
    assert!(!state.apply_event(&restarted));
    assert_eq!(state.sessions.len(), 1);
}

#[test]
fn a_list_reconciles_but_keeps_newer_local_copies_and_skips_closing_tabs() {
    let mut state = ThreadPreviews::default();
    state.reconcile(&list(1, vec![snapshot("a", None, "1"), snapshot("b", None, "1")]));
    state.apply_snapshot(snapshot("a", Some("https://example.com/"), "5"));
    assert!(state.begin_close("b"));
    assert_eq!(state.sessions.len(), 1);
    // A stale list still holding `b` must not resurrect it, and `a` keeps its newer navigation.
    state.reconcile(&list(2, vec![snapshot("a", None, "2"), snapshot("b", None, "2")]));
    assert_eq!(state.sessions.iter().map(|s| s.tab_id.as_str()).collect::<Vec<_>>(), ["a"]);
    assert_eq!(state.session("a").unwrap().nav_status.url(), Some("https://example.com/"));
    // A failed close brings the tab back in front.
    let b = snapshot("b", None, "2");
    assert!(state.cancel_close(Some(b), "b"));
    assert_eq!(state.active_tab_id.as_deref(), Some("b"));
    // An older list revision is ignored.
    assert!(!state.reconcile(&list(1, vec![])));
    // Once the server confirms the close, the suppression ends with the session.
    state.begin_close("a");
    state.reconcile(&list(3, vec![snapshot("b", None, "3")]));
    assert!(state.apply_snapshot(snapshot("a", Some("https://example.com/"), "9")));
}

#[test]
fn panel_tabs_wait_for_the_first_list_and_hide_unrevealed_server_tabs() {
    let mut state = ThreadPreviews::default();
    state.apply_snapshot(snapshot("a", None, "1"));
    assert_eq!(state.panel_tabs(), None);
    let mut hidden = snapshot("h", None, "1");
    hidden.reveal = Some(false);
    state.reconcile(&list(1, vec![snapshot("a", None, "1"), hidden]));
    let (tabs, hidden) = state.panel_tabs().unwrap();
    assert_eq!(tabs, ["a", "h"]);
    assert!(hidden.contains("h") && !hidden.contains("a"));
}

#[test]
fn reveals_are_fresh_only_after_the_baseline_list() {
    let mut revealed = snapshot("agent", Some("https://example.com/"), "2");
    revealed.reveal = Some(true);
    let mut previous = ThreadPreviews::default();
    let mut next = previous.clone();
    next.reconcile(&list(1, vec![revealed.clone()]));
    assert!(next.freshly_revealed(&previous).is_empty());
    previous = next.clone();
    revealed.reveal_request = Some(RevealRequest { id: "r1".into(), force: true });
    next.update_snapshot(revealed.clone());
    assert_eq!(next.freshly_revealed(&previous).len(), 1);
    previous = next.clone();
    assert!(next.freshly_revealed(&previous).is_empty());
}

#[test]
fn tab_titles_and_zoom_steps_follow_the_web() {
    assert_eq!(tab_title(None), "Browser");
    assert_eq!(tab_title(Some(&snapshot("a", None, "1"))), "Browser");
    assert_eq!(tab_title(Some(&snapshot("a", Some("http://localhost:5173/app"), "1"))), "localhost:5173");
    let mut titled = snapshot("a", Some("https://example.com/"), "1");
    titled.nav_status = NavStatus::Success { url: "https://example.com/".into(), title: "Example Domain".into() };
    assert_eq!(tab_title(Some(&titled)), "Example Domain");
    assert!(shows_empty_state(None) && shows_empty_state(Some(&snapshot("a", None, "1"))));
    assert!(!shows_empty_state(Some(&titled)));
    assert_eq!(step_zoom(1.0, 1), 1.1);
    assert_eq!(step_zoom(1.0, -1), 0.9);
    assert_eq!(step_zoom(5.0, 1), 5.0);
    assert_eq!(step_zoom(1.3, 1), 1.1);
    assert_eq!(step_zoom(2.0, 0), 1.0);
    assert_eq!(describe_error("ERR_CONNECTION_REFUSED"), "Connection refused");
    assert_eq!(describe_error(""), "Network error");
    assert_eq!(describe_error("net::ERR_ABORTED"), "net::ERR_ABORTED");
    assert_eq!((error_label(-102, ""), error_label(0, ""), error_label(-3, "ERR_ABORTED")), ("ERR_102".into(), "ERR_FAILED".into(), "ERR_ABORTED".into()));
}

#[test]
fn discovered_servers_sort_by_port() {
    let servers = decode_discovered_servers(&json!({
        "scannedAt": "now",
        "servers": [
            { "host": "127.0.0.1", "port": 5173, "url": "http://127.0.0.1:5173", "processName": "vite", "pid": 4, "terminal": null },
            { "host": "localhost", "port": 3000, "url": "http://localhost:3000", "processName": null, "pid": null, "terminal": null },
            { "host": "bad" },
        ],
    }));
    assert_eq!(servers.iter().map(|server| (server.title(), server.address())).collect::<Vec<_>>(), [("Listening", "localhost:3000".to_owned()), ("vite", "127.0.0.1:5173".to_owned())]);
}

impl SessionSnapshot {
    fn to_value(&self) -> serde_json::Value {
        let nav = match &self.nav_status {
            NavStatus::Success { url, title } => json!({ "_tag": "Success", "url": url, "title": title }),
            _ => json!({ "_tag": "Idle" }),
        };
        json!({
            "threadId": self.thread_id, "tabId": self.tab_id, "navStatus": nav,
            "canGoBack": self.can_go_back, "canGoForward": self.can_go_forward,
            "runtime": self.runtime, "reveal": self.reveal, "updatedAt": self.updated_at,
        })
    }
}

#[test]
fn a_thread_shown_again_catches_its_tabs_up_with_changes_made_while_hidden() {
    let mut panels = crate::right_panel::RightPanels::default();
    let mut threads = PreviewThreads::default();
    let follow = |panels: &mut crate::right_panel::RightPanels, edit: Option<TabsEdit>| {
        if let Some(edit) = edit {
            edit.apply(panels, "thread-1");
        }
    };
    let browser_tabs = |panels: &crate::right_panel::RightPanels| -> Vec<String> {
        panels.panel("thread-1").surfaces.iter().filter_map(|surface| surface.preview_tab().map(str::to_owned)).collect()
    };
    follow(&mut panels, threads.show(Some("thread-1")));
    let (_, relist, edit) = threads.listed("thread-1", &list(1, vec![snapshot("a", Some("https://example.com/"), "1")]));
    assert!(!relist);
    follow(&mut panels, edit);
    assert_eq!(browser_tabs(&panels), ["a"]);
    // Another thread comes into view; another client closes `a` meanwhile.
    assert_eq!(threads.show(Some("thread-2")), None);
    let applied = threads.apply([event("closed", "a", 2, json!({}))]);
    assert_eq!((applied.changed, applied.edit), (vec!["thread-1".to_owned()], None));
    assert_eq!(browser_tabs(&panels), ["a"]);
    // Back on thread-1 the dead tab goes, before any list answers.
    follow(&mut panels, threads.show(Some("thread-1")));
    assert!(browser_tabs(&panels).is_empty());
    // A list that changes nothing still brings lagging tabs in line.
    panels.open_browser("thread-1", Some("a"));
    let (changed, _, edit) = threads.listed("thread-1", &list(2, vec![]));
    assert!(!changed);
    follow(&mut panels, edit);
    assert!(browser_tabs(&panels).is_empty());
}
