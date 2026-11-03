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
        let visible = capture(&rows, id, 1, tail, 120., tops(1), Some(row_id)).expect("a disclosure releases tail following");
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
