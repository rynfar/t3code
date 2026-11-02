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
    let visible = capture(&old, id, 1, top, 120., tops(1)).unwrap();
    let new = ids(&["t5", "a5", "t6", "a6", "t7", "a7", "t8", "a8"]);
    // The list itself leaves its scroll top on the control, which would show turn 5.
    assert_eq!(restore(&visible, &new, id, 1, top), Some(Anchor::Below { item: 5, gap: 50. }));
}

#[test]
fn the_last_page_removes_the_header_and_keeps_the_first_row_in_place() {
    let old = ids(&["t7", "a7"]);
    let visible = capture(&old, id, 1, ScrollTop { item: 0, offset: 10. }, 120., tops(1)).unwrap();
    let new = ids(&["t1", "a1", "t7", "a7"]);
    assert_eq!(restore(&visible, &new, id, 0, ScrollTop { item: 0, offset: 0. }), Some(Anchor::Below { item: 2, gap: 50. }));
}

#[test]
fn a_row_cut_off_above_keeps_its_offset_when_rows_land_before_it() {
    let old = ids(&["t7", "a7", "t8"]);
    // 15px into a7.
    let visible = capture(&old, id, 1, ScrollTop { item: 2, offset: 15. }, 120., tops(1)).unwrap();
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
    let visible = capture(&rows, id, 1, ScrollTop { item: 2, offset: 15. }, 120., tops(1)).unwrap();
    assert_eq!(restore(&visible, &rows, id, 1, ScrollTop { item: 2, offset: 0. }), Some(Anchor::At(ScrollTop { item: 2, offset: 15. })));
}

#[test]
fn a_change_that_leaves_the_scroll_top_alone_needs_nothing() {
    // Streaming below the fold.
    let old = ids(&["t7", "a7", "t8"]);
    let top = ScrollTop { item: 2, offset: 15. };
    let visible = capture(&old, id, 1, top, 120., tops(1)).unwrap();
    let new = ids(&["t7", "a7", "t8", "a8"]);
    assert_eq!(restore(&visible, &new, id, 1, top), None);
}

#[test]
fn the_first_row_in_view_that_survives_is_the_anchor() {
    let old = ids(&["g7", "t8", "a8"]);
    let visible = capture(&old, id, 1, ScrollTop { item: 0, offset: 10. }, 200., tops(1)).unwrap();
    // The work group regrouped under a new id; t8 started 90px below the viewport's top.
    let new = ids(&["t6", "g6", "t8", "a8"]);
    assert_eq!(restore(&visible, &new, id, 1, ScrollTop { item: 0, offset: 10. }), Some(Anchor::Below { item: 3, gap: 90. }));
}

#[test]
fn nothing_to_keep_while_following_the_tail_or_before_a_layout() {
    let rows = ids(&["t7", "a7"]);
    // A bottom-aligned list at its end reports the item past the last.
    assert_eq!(capture(&rows, id, 1, ScrollTop { item: 3, offset: 0. }, 120., tops(1)), None);
    assert_eq!(capture(&rows, id, 1, ScrollTop { item: 0, offset: 0. }, 0., tops(1)), None);
    assert_eq!(capture(&[] as &[&str], id, 1, ScrollTop { item: 0, offset: 0. }, 120., tops(1)), None);
}
