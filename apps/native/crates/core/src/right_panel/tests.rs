use super::*;

/// Distinct tabs for exercising tab behavior; only the id matters to the model.
fn tab(id: &str) -> Surface {
    Surface { id: id.to_owned(), kind: SurfaceKind::Diff }
}

fn ids(panels: &RightPanels, thread: &str) -> Vec<String> {
    panels.panel(thread).surfaces.iter().map(|surface| surface.id.clone()).collect()
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
    let order: Vec<_> = panels.closed_views().iter().map(|view| view.surface.id.as_str()).collect();
    assert_eq!(order, ["b", "c", "a"]);
    assert_eq!(panels.reopen_closed(|_| true).as_deref(), Some("t"));
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
    assert_eq!(panels.reopen_closed(|thread| thread == "t").as_deref(), Some("t"));
    assert_eq!(ids(&panels, "t"), ["b", "a"]);
    let left: Vec<_> = panels.closed_views().iter().map(|view| view.thread_id.as_str()).collect();
    assert_eq!(left, ["gone"]);
    assert_eq!(panels.reopen_closed(|thread| thread == "t"), None);
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
    assert_eq!(panels.closed_views()[0].surface.id, "tab-29");
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
            "surfaces": [{ "id": "browser:1", "kind": "preview", "resourceId": "1" }, { "id": "diff", "kind": "diff" }] } },
        "widths": { "t": 480.0 },
        "closed": [{ "threadId": "t", "surface": { "id": "files", "kind": "files" } },
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
