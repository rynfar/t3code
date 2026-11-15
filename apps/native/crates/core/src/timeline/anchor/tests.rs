use super::*;

/// A list of 40px rows under a 60px header (the history control): item `ix`'s top.
fn tops(header: usize) -> impl Fn(usize) -> f32 {
    move |ix| if ix < header { ix as f32 * 60. } else { header as f32 * 60. + (ix - header) as f32 * 40. }
}

fn ids(rows: &[&'static str]) -> Vec<&'static str> {
    rows.to_vec()
}

fn id<'a>(row: &'a &str) -> &'a str {
    row
}

#[test]
fn earlier_turns_loaded_under_the_header_keep_the_first_row_in_place() {
    // 10px into the 60px control, so turn 7 starts 50px down.
    let old = ids(&["t7", "a7", "t8", "a8"]);
    let top = ScrollTop { item: 0, offset: 10. };
    let visible = capture(&old, id, 1, top, 120., tops(1), None).unwrap();
    let new = ids(&["t5", "a5", "t6", "a6", "t7", "a7", "t8", "a8"]);
    // The list itself leaves its scroll top on the control, which would show turn 5.
    assert_eq!(restore(&visible, &new, id, 1, top), Some(Anchor::Below { item: 5, gap: 50. }));
}

#[test]
fn the_last_page_removes_the_header_and_keeps_the_first_row_in_place() {
    let old = ids(&["t7", "a7"]);
    let visible = capture(&old, id, 1, ScrollTop { item: 0, offset: 10. }, 120., tops(1), None).unwrap();
    let new = ids(&["t1", "a1", "t7", "a7"]);
    assert_eq!(restore(&visible, &new, id, 0, ScrollTop { item: 0, offset: 0. }), Some(Anchor::Below { item: 2, gap: 50. }));
}

#[test]
fn a_row_cut_off_above_keeps_its_offset_when_rows_land_before_it() {
    let old = ids(&["t7", "a7", "t8"]);
    // 15px into a7.
    let visible = capture(&old, id, 1, ScrollTop { item: 2, offset: 15. }, 120., tops(1), None).unwrap();
    let new = ids(&["t6", "a6", "t7", "a7", "t8"]);
    let anchor = Anchor::At(ScrollTop { item: 4, offset: 15. });
    assert_eq!(restore(&visible, &new, id, 1, ScrollTop { item: 2, offset: 0. }), Some(anchor));
    // Where the list's own splice already shifted it, nothing more to do.
    assert_eq!(restore(&visible, &new, id, 1, ScrollTop { item: 4, offset: 15. }), None);
}

#[test]
fn a_replaced_row_under_the_scroll_top_keeps_its_offset() {
    // The list resets a replaced row's offset to its top; the reader stays 15px into it.
    let rows = ids(&["t7", "a7", "t8"]);
    let visible = capture(&rows, id, 1, ScrollTop { item: 2, offset: 15. }, 120., tops(1), None).unwrap();
    assert_eq!(restore(&visible, &rows, id, 1, ScrollTop { item: 2, offset: 0. }), Some(Anchor::At(ScrollTop { item: 2, offset: 15. })));
}

#[test]
fn a_change_that_leaves_the_scroll_top_alone_needs_nothing() {
    // Streaming below the fold.
    let old = ids(&["t7", "a7", "t8"]);
    let top = ScrollTop { item: 2, offset: 15. };
    let visible = capture(&old, id, 1, top, 120., tops(1), None).unwrap();
    let new = ids(&["t7", "a7", "t8", "a8"]);
    assert_eq!(restore(&visible, &new, id, 1, top), None);
}

#[test]
fn the_first_row_in_view_that_survives_is_the_anchor() {
    let old = ids(&["g7", "t8", "a8"]);
    let visible = capture(&old, id, 1, ScrollTop { item: 0, offset: 10. }, 200., tops(1), None).unwrap();
    // The work group regrouped under a new id; t8 started 90px below the viewport's top.
    let new = ids(&["t6", "g6", "t8", "a8"]);
    assert_eq!(restore(&visible, &new, id, 1, ScrollTop { item: 0, offset: 10. }), Some(Anchor::Below { item: 3, gap: 90. }));
}

#[test]
fn nothing_to_keep_while_following_the_tail_or_before_a_layout() {
    let rows = ids(&["t7", "a7"]);
    // A bottom-aligned list at its end reports the item past the last.
    assert_eq!(capture(&rows, id, 1, ScrollTop { item: 3, offset: 0. }, 120., tops(1), None), None);
    assert_eq!(capture(&rows, id, 1, ScrollTop { item: 0, offset: 0. }, 0., tops(1), None), None);
    assert_eq!(capture(&[] as &[&str], id, 1, ScrollTop { item: 0, offset: 0. }, 120., tops(1), None), None);
}

#[test]
fn a_row_regrouped_above_the_anchor_moves_it_back() {
    // The control stays put while a page merges tools into the first work group: same row count,
    // a new id and height, so turn 8 moved although its index and the scroll top did not.
    let old = ids(&["g7", "t8", "a8"]);
    let top = ScrollTop { item: 0, offset: 10. };
    let visible = capture(&old, id, 1, top, 200., tops(1), None).unwrap();
    let new = ids(&["g6", "t8", "a8"]);
    assert_eq!(restore(&visible, &new, id, 1, top), Some(Anchor::Below { item: 2, gap: 90. }));
}

#[test]
fn a_disclosure_at_the_tail_anchors_its_row_instead_of_the_end() {
    for row_id in ["work-toggle:g", "turn-fold:r", "attempt-fold:a", "work-entry:e"] {
        let rows = ids(&["earlier", row_id, "later"]);
        // A footer follows the data rows; its end is item 5, at 220px.
        let tail = ScrollTop { item: 5, offset: 0. };
        let visible = capture(&rows, id, 1, tail, 120., tops(1), Some(row_id)).expect("a disclosure captures its row while following");
        let expanded = ids(&["earlier", row_id, "detail", "later"]);
        assert_eq!(restore(&visible, &expanded, id, 1, ScrollTop { item: 6, offset: 0. }), Some(Anchor::At(ScrollTop { item: 2, offset: 0. })));
        // Expanding an entry remeasures without changing the data or logical scroll top.
        assert_eq!(restore(&visible, &rows, id, 1, tail), Some(Anchor::At(ScrollTop { item: 2, offset: 0. })));
    }
}

#[test]
fn a_disclosure_uses_the_toggled_row_below_the_first_visible_row() {
    let rows = ids(&["earlier", "group", "later"]);
    let top = ScrollTop { item: 0, offset: 10. };
    let visible = capture(&rows, id, 1, top, 200., tops(1), Some("group")).unwrap();
    assert_eq!(restore(&visible, &rows, id, 1, top), Some(Anchor::At(ScrollTop { item: 2, offset: -90. })));
    assert_eq!(include_preceding_rows(ScrollTop { item: 2, offset: -90. }, |ix| tops(1)(ix + 1) - tops(1)(ix)), top);
}

#[test]
fn a_disclosure_in_a_short_bottom_aligned_transcript_keeps_its_gap() {
    let rows = ids(&["group"]);
    let tail = ScrollTop { item: 2, offset: 0. };
    let visible = capture(&rows, id, 0, tail, 200., tops(0), Some("group")).unwrap();
    assert_eq!(restore(&visible, &rows, id, 0, tail), Some(Anchor::At(ScrollTop { item: 0, offset: -120. })));
    assert_eq!(include_preceding_rows(ScrollTop { item: 1, offset: -160. }, |_| 40.), ScrollTop { item: 0, offset: -120. });
    assert_eq!(capture(&rows, id, 0, tail, 0., tops(0), Some("group")), None);
    assert_eq!(capture(&rows, id, 0, tail, 200., tops(0), Some("missing")), None);
}

#[test]
fn disclosure_settling_rearms_end_follow_and_preserves_append_eligibility() {
    for distance in [-120., 0., 20., 40.] {
        let mut follow = DisclosureFollow::default();
        follow.toggle("group", true);
        let generation = follow.generation().unwrap();
        assert!(!follow.follow_change(distance, 200.), "hold the row through settling");
        assert_eq!(follow.settle(generation, distance, 200.), distance == 0., "only rearm the sentinel without moving the viewport at {distance}");
        assert_eq!(follow.anchor(), None);
        assert_eq!(follow.follow_change(distance, 200.), distance != 0., "near-end eligibility waits for the next change");
    }
    for distance in [41., 100., 200.] {
        let mut follow = DisclosureFollow::default();
        follow.toggle("group", true);
        let generation = follow.generation().unwrap();
        assert!(!follow.settle(generation, distance, 200.), "leave expanded details below their row");
        assert_eq!(follow.anchor(), None);
        assert!(follow.follow_change(distance, 200.), "the next data/item change can follow within one viewport");
    }
}

#[test]
fn disclosure_settling_cannot_override_navigation_or_a_newer_layout() {
    let mut follow = DisclosureFollow::default();
    follow.toggle("run", true);
    let old = follow.generation().unwrap();
    follow.changed();
    assert!(!follow.settle(old, 0., 200.));
    assert_eq!(follow.anchor(), Some("run"));
    let current = follow.generation().unwrap();
    assert!(!follow.settle(current, 201., 200.));
    assert!(!follow.follow_change(201., 200.));
    follow.cancel();
    assert!(!follow.follow_change(0., 200.));
    follow.toggle("mid-transcript", false);
    let current = follow.generation().unwrap();
    assert!(!follow.settle(current, 100., 200.));
    assert!(!follow.follow_change(100., 200.));
    follow.toggle("group", true);
    let current = follow.generation().unwrap();
    follow.cancel();
    assert!(!follow.settle(current, 0., 200.));
    assert!(!follow.follow_change(0., 200.));
}

#[test]
fn pinned_tail_captures_visible_rows_and_holds_position_on_append() {
    let old = ids(&["r0", "r1", "r2", "r3", "r4"]);
    // 1 header (60) + 5 rows (200) = 260; viewport 100 at tail => scrolled = 160 (20px into r2).
    let tail = ScrollTop { item: 6, offset: 0. };
    let visible = capture_pinned(&old, id, 1, tail, 100., tops(1), None).expect("pinned capture holds the visible rows at tail");
    let new = ids(&["r0", "r1", "r2", "r3", "r4", "r5"]);
    assert_eq!(
        restore(&visible, &new, id, 1, ScrollTop { item: 7, offset: 0. }),
        Some(Anchor::At(ScrollTop { item: 3, offset: 20. })),
    );
}

#[test]
fn timeline_follow_hysteresis_and_debounce_pill() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut follow = TimelineFollow::default();
    assert!(follow.is_following());
    assert!(follow.is_at_end());

    // Passive growth while Following keeps Following even past 40px.
    follow.on_passive_distance(false, ms(10));
    assert!(follow.is_following());
    assert_eq!(follow.pill_state(false, true, ms(200)), (false, None));

    // Navigating while still <= 40px enters Released { at_end: true } without showing pill.
    follow.on_user_navigation(ScrollSource::Wheel, true, ms(300));
    assert!(!follow.is_following());
    assert!(follow.is_at_end());
    assert_eq!(follow.pill_state(false, true, ms(500)), (false, None));

    // Scrolling past 40px starts the 150ms reveal timer; hidden while find is open.
    follow.on_user_navigation(ScrollSource::Wheel, false, ms(600));
    assert!(!follow.is_at_end());
    assert_eq!(follow.pill_state(false, true, ms(650)), (false, Some(ms(750))));
    assert_eq!(follow.pill_state(false, true, ms(750)), (true, None));
    assert_eq!(follow.pill_state(true, true, ms(760)), (false, None));
    // Re-hides while find was open; restarts 150ms debounce once find closes while still released.
    assert_eq!(follow.pill_state(false, true, ms(800)), (false, Some(ms(950))));

    // Returning within 40px from away re-engages Following and clears pill.
    follow.on_user_navigation(ScrollSource::Wheel, true, ms(960));
    assert!(follow.is_following());
    assert!(follow.is_at_end());
    assert_eq!(follow.pill_state(false, true, ms(1200)), (false, None));

    // Find/Citation reveals stay Released even when jumping near the end while Released away from end.
    follow.on_user_navigation(ScrollSource::Wheel, false, ms(1320));
    assert!(!follow.is_following());
    follow.on_user_navigation(ScrollSource::Find, true, ms(1330));
    assert!(!follow.is_following());
    follow.on_user_navigation(ScrollSource::Citation, true, ms(1340));
    assert!(!follow.is_following());

    // Scrollbar/EndKey explicitly re-arms Following.
    follow.on_user_navigation(ScrollSource::Scrollbar, false, ms(1350));
    assert!(!follow.is_following());
    follow.on_user_navigation(ScrollSource::EndKey, false, ms(1360));
    assert!(follow.is_following());
}

#[test]
fn new_turn_anchor_only_anchors_first_user_message_and_releases_on_tool_or_settle() {
    let mut follow = TimelineFollow::default();
    let initial = [RunAnchorSnapshot { run_id: "r0", status: "completed", user_message_id: "m0", has_tool_or_reasoning: false }];
    // Initial hydration does not anchor historical turns.
    assert_eq!(follow.observe_runs(&initial, 1, |id| id == "m0"), AnchorEffect::None);

    // Follow-up second user message does NOT anchor (web commit 2274444e92).
    let followup = [
        RunAnchorSnapshot { run_id: "r0", status: "completed", user_message_id: "m0", has_tool_or_reasoning: false },
        RunAnchorSnapshot { run_id: "r1", status: "running", user_message_id: "m1", has_tool_or_reasoning: false },
    ];
    assert_eq!(follow.observe_runs(&followup, 2, |id| id == "m0"), AnchorEffect::None);

    // First user message on a fresh thread anchors, holds while streaming plain text, and releases on tool activity.
    let mut fresh = TimelineFollow::default();
    assert_eq!(fresh.observe_runs(&[], 0, |_| false), AnchorEffect::None);
    let first_start = [RunAnchorSnapshot { run_id: "r1", status: "starting", user_message_id: "m1", has_tool_or_reasoning: false }];
    assert_eq!(
        fresh.observe_runs(&first_start, 1, |id| id == "m1"),
        AnchorEffect::Anchor { user_message_id: "m1".to_owned() },
    );
    assert_eq!(fresh.anchored_user_message_id(), Some("m1"));
    let streaming = [RunAnchorSnapshot { run_id: "r1", status: "running", user_message_id: "m1", has_tool_or_reasoning: false }];
    assert_eq!(
        fresh.observe_runs(&streaming, 1, |id| id == "m1"),
        AnchorEffect::Keep { user_message_id: "m1".to_owned() },
    );
    let tool_started = [RunAnchorSnapshot { run_id: "r1", status: "running", user_message_id: "m1", has_tool_or_reasoning: true }];
    assert_eq!(fresh.observe_runs(&tool_started, 1, |id| id == "m1"), AnchorEffect::ReleaseAndScrollToEnd);
    assert_eq!(fresh.anchored_user_message_id(), None);

    // Draft promotion: initial snapshot already includes the live first run and single user message.
    let mut draft_promoted = TimelineFollow::default();
    assert_eq!(
        draft_promoted.observe_runs(&first_start, 1, |id| id == "m1"),
        AnchorEffect::Anchor { user_message_id: "m1".to_owned() },
    );

    // Opening a populated multi-run thread does not anchor on initial hydration even if latest run is live.
    let mut populated = TimelineFollow::default();
    assert_eq!(populated.observe_runs(&followup, 1, |id| id == "m0"), AnchorEffect::None);
}

#[test]
fn remembered_positions_enforces_lru_cap_and_updates_recency() {
    let mut cache = RememberedPositions::default();
    let make = |offset: f32| SavedThreadPosition {
        row_id: Some("row".to_owned()),
        offset_within_row: offset,
        scroll_top: offset,
        at_end: false,
        expanded_runs: Default::default(),
        expanded_attempts: Default::default(),
        expanded_groups: Default::default(),
    };
    for i in 0..REMEMBERED_POSITIONS_LIMIT {
        cache.save(format!("t{i}"), make(i as f32));
    }
    assert_eq!(cache.len(), REMEMBERED_POSITIONS_LIMIT);
    // Touch t0 so t1 becomes LRU.
    assert_eq!(cache.get("t0").unwrap().offset_within_row, 0.);
    cache.save("t_new", make(999.));
    assert_eq!(cache.len(), REMEMBERED_POSITIONS_LIMIT);
    assert!(cache.get("t0").is_some());
    assert!(cache.get("t1").is_none());
}

#[test]
fn smooth_step_fraction_recovers_exact_ease_out_cubic_curve() {
    let mut pos = 0.0f32;
    let target = 1000.0f32;
    let steps = [0.25f32, 0.5, 0.75, 1.0];
    let mut prev = 0.0f32;
    for t in steps {
        let frac = smooth_step_fraction(prev, t);
        pos += (target - pos) * frac;
        assert!((pos - target * ease_out_cubic(t)).abs() < 1e-2, "t={t}, pos={pos}");
        prev = t;
    }
    assert_eq!(pos, target);
}

#[test]
fn resolve_disclosure_reveal_scroll_delta_matches_web_cases() {
    let reveal = |row_top: f32, row_bottom: f32| {
        resolve_disclosure_reveal_scroll_delta(row_top, row_bottom, 0., 800., 150., 24.)
    };
    assert_eq!(reveal(300., 650.), 0.);
    assert_eq!(reveal(560., 900.), 250.);
    assert_eq!(reveal(600., 2000.), 576.);
    assert_eq!(reveal(-40., 1200.), 0.);
}
