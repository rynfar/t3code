use std::sync::Arc;

use super::*;
use crate::composer::attachments::ATTACHMENT_ONLY_PROMPT;
use crate::timeline::{ChatMessage, MessageMeta};

fn message(id: &str, role: Role, text: &str) -> TimelineRow {
    TimelineRow {
        id: id.into(),
        created_at: None,
        continues_work_log: false,
        kind: RowKind::Message {
            message: Arc::new(ChatMessage {
                id: id.into(),
                role,
                text: Arc::from(text),
                run_id: None,
                streaming: false,
                created_at: String::new(),
                updated_at: String::new(),
                meta: MessageMeta::default(),
                item: None,
            }),
            show_assistant_meta: false,
            show_assistant_timestamp: false,
            show_copy: false,
            copy_streaming: false,
            grouped_with_next: false,
            grouped_with_previous: false,
            continues_steer: false,
        },
    }
}

fn working() -> TimelineRow {
    TimelineRow { id: "working".into(), created_at: None, continues_work_log: false, kind: RowKind::Working { compacting: false } }
}

fn item(user: &str, assistant: Option<&str>) -> MinimapItem {
    MinimapItem { id: "x".into(), row_index: 0, user_text: Arc::from(user), assistant_text: assistant.map(Arc::from) }
}

#[test]
fn items_are_user_messages_with_their_turns_final_answer() {
    let rows = vec![
        message("u1", Role::User, "First ask"),
        message("a1", Role::Assistant, "Looking"),
        working(),
        message("a2", Role::Assistant, "Final answer"),
        message("u2", Role::User, &format!("Ultrathink:\n{ATTACHMENT_ONLY_PROMPT}")),
        message("u3", Role::User, ATTACHMENT_ONLY_PROMPT),
        message("a3", Role::Assistant, "Seen"),
    ];
    let items = derive_minimap_items(&rows);
    let summary: Vec<(&str, usize, &str, Option<&str>)> =
        items.iter().map(|item| (item.id.as_str(), item.row_index, &*item.user_text, item.assistant_text.as_deref())).collect();
    assert_eq!(
        summary,
        vec![("u1", 0, "First ask", Some("Final answer")), ("u2", 4, "", None), ("u3", 5, "", Some("Seen"))],
    );
    assert_eq!(resolve_minimap_preview(&items[1]), MinimapPreview { user_text: None, assistant_text: None });
    assert_eq!(resolve_minimap_preview(&items[0]).user_text.as_deref(), Some("First ask"));
}

/// Expectations are `resolveTimelineMinimapPreview`'s output for the same text.
#[test]
fn previews_read_markdown_as_the_web_does() {
    let cases = [
        (
            "# Plan\n\nI **added** `clamp` next to _sum_.\n\n- Reads the manifest\n- Asks first\n\n```ts\nclamp(1, 0, 2);\n```\n\n| Step | Result |\n| --- | --- |\n| Read | 1 file |",
            Some("Plan I added clamp next to sum. Reads the manifest Asks first clamp(1, 0, 2); Step Result Read 1 file"),
        ),
        (
            "Line one  \nline two<br/>after<!-- secret -->\n\n<div class=\"note\">Block <b>bold</b></div>\n\n![diagram](d.png) ~~old~~ new [link](https://x.y) www.example.com",
            Some("Line one line two after Block bold diagram old new link www.example.com"),
        ),
        ("before <!-- unterminated comment", Some("before <!-- unterminated comment")),
        ("a <span title='x > y'>quoted</span> b < c and <3 <P>para</p>", Some("a quoted b < c and <3 para")),
        ("See [^1] here.\n\n[^1]: The note.", Some("See here. The note.")),
        (
            "Started <t3-thread://v2/env-1/thread-1> and [Fix CI](t3-thread://v2/env-1/t2).",
            Some("Started a thread and Fix CI."),
        ),
        (
            "<t3-thread://v1/abc> and <t3-thread://v2/env/> and <https://example.com>",
            Some("a thread and t3-thread://v2/env/ and https://example.com"),
        ),
        ("> quoted **text**\n>\n> more\n\n1. one\n2. two\n\n---\n\nend", Some("quoted text more one two end")),
        ("- [x] done task\n- [ ] open task\n\nText with\ttabs\u{a0}and nbsp", Some("done task open task Text with tabs and nbsp")),
        ("   \n\n  ", None),
    ];
    for (text, expected) in cases {
        assert_eq!(resolve_minimap_preview(&item("ask", Some(text))).assistant_text.as_deref(), expected, "{text:?}");
    }
    let long = "word ".repeat(400);
    let preview = resolve_minimap_preview(&item("ask", Some(&long))).assistant_text.unwrap();
    assert_eq!(preview.split(' ').count(), 300, "only the first 1,500 UTF-16 units are read");
}

#[test]
fn rail_geometry_follows_the_web() {
    assert_eq!(rail_height(2, 500.), 8.);
    assert_eq!(rail_height(101, 500.), 500.);
    assert_eq!(rail_height(5, -40.), 0.);
    assert_eq!((top_percent(0, 5), top_percent(2, 5), top_percent(9, 5), top_percent(0, 1)), (0., 50., 100., 0.));
    let pointer = |count, y| index_from_pointer(count, 100., 40., y);
    assert_eq!([pointer(5, 90.), pointer(5, 104.9), pointer(5, 105.), pointer(5, 130.), pointer(5, 200.)], [Some(0), Some(0), Some(1), Some(3), Some(4)]);
    assert_eq!((pointer(1, 130.), pointer(0, 130.), index_from_pointer(3, 0., 0., 0.)), (Some(0), None, None));
}

#[test]
fn the_visible_range_and_current_item_follow_the_scroll() {
    // Rows 0..6 are 100px tall except row 3, which has no height yet.
    let tops = [0., 100., 200., 300., 300., 400., 500.];
    let at = |index: usize| tops[index];
    let range = |scroll: f32| visible_range(6, scroll, 150., at);
    assert_eq!(range(0.), Some((0, 1)));
    assert_eq!(range(120.), Some((1, 2)));
    assert_eq!(range(250.), Some((2, 4)), "a row with no height counts once its top is in view");
    assert_eq!(range(300.), Some((3, 5)));
    assert_eq!(range(380.), Some((4, 5)));
    assert_eq!(range(600.), None);
    assert_eq!(visible_range(0, 0., 150., at), None);

    assert!(is_row_visible(2, Some((1, 2))) && !is_row_visible(3, Some((1, 2))) && !is_row_visible(0, None));
    let users = [0, 3, 5];
    assert_eq!(current_index(Some((2, 4)), users), Some(1), "the first visible user message");
    assert_eq!(current_index(Some((1, 2)), users), Some(0), "otherwise the turn being read");
    assert_eq!(current_index(Some((6, 8)), users), Some(2));
    assert_eq!(current_index(Some((0, 2)), [4, 6]), None);
    assert_eq!(current_index(None, users), None);
}

#[test]
fn the_gutter_decides_whether_the_rail_shows_and_takes_the_pointer() {
    // (viewport, lane) → (persistent, hit strip, navigation)
    let cases = [
        (832., 736., (true, 36., true)),
        (1000., 736., (true, 40., true)),
        (784., 736., (false, 12., false)),
        (790., 736., (false, 15., true)),
        (760., 736., (false, 0., false)),
        (500., 900., (false, 0., false)),
        (0., 736., (false, 0., false)),
    ];
    for (viewport, lane, expected) in cases {
        let hit = hit_strip_width(viewport, lane);
        assert_eq!((has_persistent_gutter(viewport, lane), hit, navigation_interactive(hit)), expected, "{viewport} / {lane}");
    }
}

#[test]
fn a_jump_anchors_above_the_message_through_measured_rows() {
    let sizes = [0., 30., 10., 10., 80.];
    let size = |index: usize| sizes[index];
    assert_eq!(jump_anchor(2, 24., size), (1, 6., 0.));
    assert_eq!(jump_anchor(4, 24., size), (1, 26., 0.), "walks up through short rows");
    assert_eq!(jump_anchor(1, 24., size), (1, 0., 24.), "an unmeasured row above leaves the rest for after layout");
    assert_eq!(jump_anchor(0, 24., size), (0, 0., 24.));
    assert_eq!(jump_anchor(4, 0., size), (4, 0., 0.));
}
