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

#[test]
fn only_a_press_that_starts_on_the_rail_scrubs_or_jumps() {
    use RailPointerAction::{Ignore, Jump, Track};
    use RailPointerEvent::{Down, Move, Up};
    let run = |pointer: &mut RailPointer, events: &[RailPointerEvent]| events.iter().map(|event| pointer.handle(*event)).collect::<Vec<_>>();
    let mut pointer = RailPointer::default();
    let hover = Move { on_rail: true, pressed: false };
    let drag_on = Move { on_rail: true, pressed: true };
    let drag_off = Move { on_rail: false, pressed: true };

    assert_eq!(run(&mut pointer, &[hover, Down { on_rail: true }, drag_on, Up { on_rail: true }]), [Track, Ignore, Track, Jump], "click and scrub");
    assert_eq!(run(&mut pointer, &[Down { on_rail: false }, drag_on, Up { on_rail: true }]), [Ignore, Ignore, Ignore], "a drag from elsewhere passes");
    assert_eq!(
        run(&mut pointer, &[Down { on_rail: true }, drag_off, Up { on_rail: false }, Down { on_rail: false }, drag_on, Up { on_rail: true }]),
        [Ignore, Ignore, Ignore, Ignore, Ignore, Ignore],
        "a press released off the rail does not carry into the next drag",
    );
    assert_eq!(
        run(&mut pointer, &[Down { on_rail: true }, Move { on_rail: false, pressed: false }, drag_on, Up { on_rail: true }]),
        [Ignore, Ignore, Ignore, Ignore],
        "a button-free move ends a press whose release was missed",
    );
}

#[test]
fn keyboard_focus_and_navigation_match_the_web_rail() {
    assert_eq!(focus_index(None, Some(3), 6), Some(3));
    assert_eq!(focus_index(Some(1), Some(3), 6), Some(1));
    assert_eq!(focus_index(None, None, 6), Some(0));
    assert_eq!(focus_index(Some(8), Some(9), 0), None);
    for (active, key, expected) in [
        (None, "ArrowDown", KeyboardAction::Preview(1)),
        (Some(0), "ArrowUp", KeyboardAction::Preview(0)),
        (Some(5), "ArrowDown", KeyboardAction::Preview(5)),
        (Some(3), "Home", KeyboardAction::Preview(0)),
        (Some(3), "End", KeyboardAction::Preview(5)),
        (Some(3), "Enter", KeyboardAction::Jump(3)),
        (Some(3), " ", KeyboardAction::Jump(3)),
        (None, "Enter", KeyboardAction::Ignore),
        (Some(3), "Tab", KeyboardAction::Ignore),
    ] {
        assert_eq!(keyboard_action(active, 6, key), expected, "{active:?}, {key}");
    }
    assert_eq!(keyboard_action(Some(0), 0, "End"), KeyboardAction::Ignore);
    assert_eq!(keyboard_action(Some(6), 6, "Enter"), KeyboardAction::Ignore);
}

#[test]
fn preview_hangs_from_the_first_and_last_strips_and_centres_between() {
    assert_eq!(preview_offset(0, 6, 104.), 0.);
    assert_eq!(preview_offset(3, 6, 104.), -52.);
    assert_eq!(preview_offset(5, 6, 104.), -104.);
}

#[test]
fn tab_order_skips_disabled_turn_buttons() {
    use FocusPart::*;
    assert_eq!(tab_target(Previous, false, true, true), Some(Rail));
    assert_eq!(tab_target(Rail, false, true, true), Some(Next));
    assert_eq!(tab_target(Next, true, true, true), Some(Rail));
    assert_eq!(tab_target(Rail, true, true, true), Some(Previous));
    assert_eq!(tab_target(Rail, false, true, false), None);
    assert_eq!(tab_target(Rail, true, false, true), None);
    assert_eq!(tab_target(Previous, true, true, true), None);
    assert_eq!(tab_target(Next, false, true, true), None);
}

#[test]
fn navigation_tooltip_escape_survives_renders_and_delayed_hover_opens() {
    use std::time::Duration;
    let mut tooltip = NavigationTooltips::default();
    tooltip.focus(0, true, true);
    assert_eq!(tooltip.visible(), Some(0));
    assert!(tooltip.escape());
    tooltip.focus(0, true, true);
    assert_eq!(tooltip.visible(), None);
    tooltip.focus(0, false, true);
    tooltip.focus(0, true, true);
    assert_eq!(tooltip.visible(), Some(0));
    tooltip.focus(0, false, true);
    tooltip.hover(0, true, Duration::ZERO);
    tooltip.pointer_move(0, 10., 20., Duration::ZERO);
    assert!(!tooltip.escape(), "a pending label must not consume Escape");
    tooltip.advance(Duration::from_secs(1));
    assert_eq!(tooltip.visible(), None, "a delayed hover open cannot undo Escape");
    assert_eq!(tooltip.deadline(), None);
    let mut closing = NavigationTooltips::default();
    closing.hover(0, true, Duration::ZERO);
    closing.pointer_move(0, 10., 20., Duration::ZERO);
    closing.advance(Duration::from_millis(600));
    closing.hover(0, false, Duration::from_millis(600));
    assert!(!closing.escape(), "an ending label has no Escape dismiss interaction");
    closing.hover(0, true, Duration::from_millis(601));
    assert_eq!(closing.visible(), Some(0), "unconsumed Escape preserves the hover-close reentry rule");
}


#[test]
fn navigation_tooltips_wait_for_each_trigger_without_a_provider_grace_period() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut tips = NavigationTooltips::default();
    tips.hover(0, true, ms(0));
    tips.pointer_move(0, 10., 20., ms(0));
    assert_eq!(tips.deadline(), Some(ms(600)));
    tips.advance(ms(599));
    assert_eq!(tips.visible(), None);
    tips.advance(ms(600));
    assert_eq!(tips.visible(), Some(0));
    tips.hover(0, false, ms(610));
    tips.hover(1, true, ms(610));
    tips.pointer_move(1, 10., 20., ms(610));
    assert_eq!(tips.visible(), None, "TimelineMinimap has no TooltipProvider: the next trigger waits too");
    tips.advance(ms(1209));
    assert_eq!(tips.visible(), None);
    tips.advance(ms(1210));
    assert_eq!(tips.visible(), Some(1));
    assert_eq!(tips.deadline(), None, "settled tooltips schedule no work");
}

#[test]
fn navigation_tooltip_scroll_cancels_pending_and_open_hover_but_preserves_focus() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    for open in [false, true] {
        let mut tips = NavigationTooltips::default();
        tips.hover(0, true, ms(0));
        tips.pointer_move(0, 10., 20., ms(0));
        if open { tips.advance(ms(600)); }
        tips.scroll();
        tips.advance(ms(900));
        assert_eq!(tips.visible(), None, "scroll cancels pending opens as well as visible hover tips");
        assert_eq!(tips.deadline(), None);
        tips.hover(0, false, ms(900));
        tips.hover(0, true, ms(901));
        tips.pointer_move(0, 10., 20., ms(901));
        tips.advance(ms(1501));
        assert_eq!(tips.visible(), Some(0), "leaving and reentering restores hover");
    }
    let mut tips = NavigationTooltips::default();
    tips.focus(0, true, true);
    tips.hover(0, true, ms(0));
    tips.pointer_move(0, 10., 20., ms(0));
    tips.scroll();
    assert_eq!(tips.visible(), Some(0), "the web scroll tracker preserves the focused trigger");
}

#[test]
fn navigation_tooltip_press_and_escape_do_not_reopen_until_a_new_interaction() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    for (focused, pending, escape) in [
        (false, true, false), (false, false, false), (true, false, false),
        (false, true, true), (false, false, true), (true, false, true),
    ] {
        let mut tips = NavigationTooltips::default();
        tips.hover(0, true, ms(0));
        tips.pointer_move(0, 10., 20., ms(0));
        tips.focus(0, focused, true);
        if !pending { tips.advance(ms(600)); }
        if escape { tips.escape(); } else { tips.press(0); }
        tips.advance(ms(1000));
        tips.hover(0, true, ms(1000));
        tips.pointer_move(0, 10., 20., ms(1000));
        tips.focus(0, focused, true);
        assert_eq!(tips.visible(), None);
        assert_eq!(tips.deadline(), None);
        tips.hover(0, false, ms(1001));
        tips.hover(0, true, ms(1002));
        tips.pointer_move(0, 10., 20., ms(1002));
        assert_eq!(tips.visible(), None, "unchanged focus cannot bypass re-arming, escape={escape}");
        tips.advance(ms(1601));
        assert_eq!(tips.visible(), None);
        tips.advance(ms(1602));
        assert_eq!(tips.visible(), Some(0));
        tips.escape();
        tips.advance(ms(2000));
        assert_eq!(tips.visible(), None);
    }
}

#[test]
fn navigation_tooltips_arbitrate_focus_and_hover_without_reopening_the_displaced_tip() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut tips = NavigationTooltips::default();
    tips.focus(0, true, true);
    assert_eq!(tips.visible(), Some(0));
    tips.hover(1, true, ms(1));
    tips.pointer_move(1, 10., 20., ms(1));
    tips.advance(ms(601));
    assert_eq!(tips.visible(), Some(1));
    tips.hover(1, false, ms(602));
    tips.advance(ms(1500));
    assert_eq!(tips.visible(), None, "the still-focused displaced trigger cannot reopen on a render");
    tips.focus(0, false, true);
    tips.focus(0, true, true);
    assert_eq!(tips.visible(), Some(0));
}


#[test]
fn navigation_tooltip_warmup_waits_for_cold_movement_but_ignores_warm_jitter() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut tips = NavigationTooltips::default();
    tips.hover(0, true, ms(0));
    assert_eq!(tips.deadline(), None, "mouseenter alone does not start the rest delay");
    tips.pointer_move(0, 10., 20., ms(10));
    assert_eq!(tips.deadline(), Some(ms(610)), "the first move starts the rest delay");
    tips.pointer_move(0, 10.5, 20., ms(300));
    assert_eq!(tips.deadline(), Some(ms(900)), "even tiny cold movement restarts the wait");
    tips.advance(ms(899));
    assert_eq!(tips.visible(), None);
    tips.advance(ms(900));
    assert_eq!(tips.visible(), Some(0));
    tips.pointer_move(0, 20., 30., ms(1000));
    assert_eq!(tips.deadline(), None, "movement of an open trigger schedules nothing");
    tips.scroll();
    tips.pointer_move(0, 30., 40., ms(1100));
    tips.advance(ms(2000));
    assert_eq!(tips.visible(), None, "movement cannot reopen a dismissed tooltip");
    tips.hover(0, false, ms(2001));
    tips.hover(0, true, ms(2002));
    tips.pointer_move(0, 10., 20., ms(2010));
    tips.pointer_move(0, 10.5, 20., ms(2300));
    assert_eq!(tips.deadline(), Some(ms(2610)), "the active DOM reference ignores small jitter while waiting");
    tips.pointer_move(0, 12., 20., ms(2400));
    assert_eq!(tips.deadline(), Some(ms(3000)));
}

#[test]
fn navigation_tooltip_focus_only_opens_on_a_new_keyboard_focus() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut tips = NavigationTooltips::default();
    tips.focus(0, true, false);
    assert_eq!(tips.visible(), None, "pointer focus does not open a label");
    tips.focus(0, true, true);
    assert_eq!(tips.visible(), None, "modality changes do not synthesize a new focus event");
    tips.focus(0, false, true);
    tips.focus(0, true, true);
    assert_eq!(tips.visible(), Some(0));
    assert!(tips.instant(0), "focus opens instantly");
    tips.hover(0, true, ms(0));
    tips.focus(0, false, true);
    tips.pointer_move(0, 10., 20., ms(1));
    tips.advance(ms(601));
    assert_eq!(tips.visible(), None, "blur blocks mouse-move reopening while hover is retained");
    assert_eq!(tips.deadline(), None);
    tips.hover(0, false, ms(602));
    tips.hover(0, true, ms(603));
    tips.pointer_move(0, 10., 20., ms(603));
    tips.advance(ms(1202));
    assert_eq!(tips.visible(), None);
    tips.advance(ms(1203));
    tips.focus(0, true, false);
    assert_eq!(tips.visible(), Some(0), "pointer focus does not change an already open hover label");
    let mut pending = NavigationTooltips::default();
    pending.hover(0, true, ms(0));
    pending.pointer_move(0, 10., 20., ms(0));
    pending.focus(0, true, false);
    assert_eq!(pending.deadline(), Some(ms(600)), "pointer focus preserves hover rest");
    pending.advance(ms(600));
    assert_eq!(pending.visible(), Some(0));
}

#[test]
fn navigation_tooltip_focus_dismiss_and_arbitration_are_instant() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut tips = NavigationTooltips::default();
    tips.focus(0, true, true);
    assert!(tips.instant(0), "focus opens instantly");
    tips.hover(1, true, ms(0));
    tips.pointer_move(1, 10., 20., ms(0));
    tips.advance(ms(600));
    assert_eq!(tips.visible(), Some(1));
    assert!(tips.instant(0) && tips.instant(1), "the delay-group switch closes and opens instantly");
    tips.hover(1, false, ms(601));
    assert!(!tips.instant(1), "a hover close uses its finite fade");
    tips.hover(1, true, ms(602));
    assert_eq!(tips.visible(), Some(1));
    assert!(!tips.instant(1), "same-trigger closing reentry reverses the fade");
    tips.press(1);
    assert!(tips.instant(1), "a reference press closes instantly");
    tips.focus(0, false, true);
    tips.focus(0, true, true);
    assert!(tips.escape());
    assert_eq!(tips.visible(), None);
    assert!(tips.instant(0), "Escape closes instantly");
}


#[test]
fn navigation_tooltip_reentry_only_skips_warmup_during_its_own_hover_close_transition() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    for (reduced, reentry, expected) in [(false, 749, Some(0)), (false, 750, None), (true, 601, None)] {
        let mut tips = NavigationTooltips::default();
        tips.set_reduced_motion(reduced);
        tips.hover(0, true, ms(0));
        tips.pointer_move(0, 10., 20., ms(0));
        tips.advance(ms(600));
        tips.hover(0, false, ms(600));
        tips.hover(0, true, ms(reentry));
        assert_eq!(tips.visible(), expected, "reentry={reentry}, reduced={reduced}");
    }
    let mut tips = NavigationTooltips::default();
    tips.hover(0, true, ms(0));
    tips.pointer_move(0, 10., 20., ms(0));
    tips.advance(ms(600));
    tips.press(0);
    tips.hover(0, false, ms(601));
    tips.hover(0, true, ms(602));
    tips.pointer_move(0, 10., 20., ms(602));
    assert_eq!(tips.visible(), None, "press dismissal does not get the hover-close grace");
}

#[test]
fn navigation_tooltip_focused_hover_leave_closes_and_reentry_waits() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    let mut tips = NavigationTooltips::default();
    tips.focus(0, true, true);
    tips.hover(0, true, ms(0));
    tips.hover(0, false, ms(1));
    assert_eq!(tips.visible(), None, "hover leave closes even when focus remains");
    tips.hover(0, true, ms(151));
    tips.pointer_move(0, 10., 20., ms(151));
    assert_eq!(tips.visible(), None, "retained focus does not bypass hover rest");
    tips.advance(ms(750));
    assert_eq!(tips.visible(), None);
    tips.advance(ms(751));
    assert_eq!(tips.visible(), Some(0));
    assert!(!tips.instant(0), "a hover open replaces focus instant mode");
}

#[test]
fn navigation_tooltip_hover_dismissal_uses_the_press_escape_instant_reason() {
    use std::time::Duration;
    let ms = Duration::from_millis;
    for escape in [false, true] {
        let mut tips = NavigationTooltips::default();
        tips.hover(0, true, ms(0));
        tips.pointer_move(0, 10., 20., ms(0));
        tips.advance(ms(600));
        assert_eq!(tips.visible(), Some(0));
        assert!(!tips.instant(0));
        if escape { assert!(tips.escape()); } else {
            tips.outside_press();
            assert!(!tips.instant(0));
            tips.press(0);
        }
        assert_eq!(tips.visible(), None);
        assert!(tips.instant(0), "a hover-opened label dismisses instantly, escape={escape}");
        assert_eq!(tips.deadline(), None);
    }
}
