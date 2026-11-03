use super::*;
use crate::composer::menu::context_references;
use serde_json::json;

/// `makeContext` in `web/src/lib/terminalContext.test.ts`.
fn context(id: &str, line_start: u32, line_end: u32, text: &str) -> TerminalContext {
    TerminalContext {
        id: id.into(),
        thread_id: "thread-1".into(),
        created_at: "2026-03-13T12:00:00.000Z".into(),
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start,
        line_end,
        text: text.into(),
    }
}

fn selection(line_start: u32, line_end: u32, text: &str) -> TerminalSelection {
    TerminalSelection { terminal_id: " term-1 ".into(), terminal_label: " Terminal 1 ".into(), line_start, line_end, text: text.into() }
}

#[test]
fn labels_and_links_read_as_the_web_writes_them() {
    let range = context("context-1", 12, 13, "git status\nOn branch main");
    assert_eq!(range.label(), "Terminal 1 lines 12-13");
    assert_eq!(range.line_caption(), "Lines 12–13");
    assert_eq!(range.reference(), "[Terminal 1 lines 12-13](t3-context://v1/terminal/terminal_context-1)");
    let single = context("context-1", 4, 4, "ls");
    assert_eq!(single.label(), "Terminal 1 line 4");
    assert_eq!(single.line_caption(), "Line 4");
    let bracketed = TerminalContext { terminal_label: "[build] logs".into(), ..single };
    assert_eq!(bracketed.reference(), "[build logs line 4](t3-context://v1/terminal/terminal_context-1)");
}

#[test]
fn folds_legacy_producer_ids_consistently_in_records_and_links() {
    let legacy = context("old terminal:one", 12, 13, "git status");
    let reference = legacy.reference();
    let parsed = context_references(&reference);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].context_id, legacy.record()["contextId"].as_str().unwrap());
}

#[test]
fn migrates_legacy_placeholders_in_order_and_drops_extras() {
    let contexts = [context("context-1", 12, 13, "git status")];
    let first = contexts[0].reference();
    assert_eq!(migrate_legacy_placeholders("a \u{FFFC} b \u{FFFC}", &contexts), format!("a {first} b "));
    assert_eq!(migrate_legacy_placeholders("plain", &contexts), "plain");
}

#[test]
fn normalizing_drops_carriage_returns_and_surrounding_blank_lines_only() {
    assert_eq!(normalize_text("\r\n\r\nline one\r\nline two\n\n"), "line one\nline two");
    assert_eq!(normalize_text("\n  indented\n\n  kept\n"), "  indented\n\n  kept");
    assert_eq!(normalize_text("\n\n"), "");
    let blank = |text: &str| context("x", 1, 1, text).is_expired();
    assert!(blank("") && blank("\r\n\n\r\n"));
    assert!(!blank("\r") && !blank("\n x\n"));
}

#[test]
fn a_selection_becomes_a_normalized_excerpt() {
    let added = TerminalContext::new(selection(0, 0, "\r\n$ ls\r\n"), "a b", "thread-1", "2026-11-02T00:00:00.000Z").unwrap();
    assert_eq!((added.terminal_id.as_str(), added.terminal_label.as_str()), ("term-1", "Terminal 1"));
    assert_eq!((added.line_start, added.line_end), (1, 1));
    assert_eq!(&*added.text, "$ ls");
    assert_eq!(added.id, to_context_id("a b"));
    let reversed = TerminalContext::new(selection(9, 3, "x"), "id", "thread-1", "now").unwrap();
    assert_eq!((reversed.line_start, reversed.line_end), (9, 9));
    let unnamed = TerminalSelection { terminal_label: "  ".into(), ..selection(1, 1, "x") };
    assert_eq!(TerminalContext::new(unnamed, "id", "thread-1", "now"), None);
}

#[test]
fn selection_rows_become_one_based_lines() {
    assert_eq!(selection_line_range(0, 0), (1, 1));
    assert_eq!(selection_line_range(41, 44), (42, 45));
    assert_eq!(selection_line_range(7, 2), (8, 8));
}

#[test]
fn the_record_carries_normalized_text_and_a_sanitized_label() {
    let excerpt = context("context-1", 12, 13, "\r\ngit status\r\nOn branch main\n");
    assert_eq!(
        excerpt.record(),
        json!({
            "version": 1,
            "contextId": "terminal_context-1",
            "kind": "terminal",
            "label": "Terminal 1 lines 12-13",
            "terminalId": "default",
            "terminalLabel": "Terminal 1",
            "lineStart": 12,
            "lineEnd": 13,
            "text": "git status\nOn branch main",
        })
    );
}

#[test]
fn links_go_in_at_the_caret_with_one_space_around_them() {
    let link = "[T](t3-context://v1/terminal/terminal_x)";
    assert_eq!(insert_reference("", 0, link), (format!("{link} "), link.len() + 1));
    let (prompt, cursor) = insert_reference("hello", 5, link);
    assert_eq!(prompt, format!("hello {link} "));
    assert_eq!(cursor, prompt.len());
    // A space after the caret is absorbed rather than doubled.
    let (prompt, cursor) = insert_reference("hello world", 5, link);
    assert_eq!(prompt, format!("hello {link} world"));
    assert_eq!(&prompt[cursor..], "world");
    let (prompt, _) = insert_reference("hello world", 6, link);
    assert_eq!(prompt, format!("hello {link} world"));
    let (prompt, _) = insert_reference("line\nnext", 5, link);
    assert_eq!(prompt, format!("line\n{link} next"));
    // A caret inside a character lands before it.
    let (prompt, _) = insert_reference("é", 1, link);
    assert_eq!(prompt, format!("{link} é"));
}

#[test]
fn the_same_lines_of_a_terminal_link_once_while_the_prompt_links_them() {
    let first = context("one", 3, 5, "a\nb\nc");
    let edit = add_terminal_context("hi", 2, &[], &first).unwrap();
    assert_eq!(edit, ReferenceEdit { start: 2, end: 2, text: format!(" {} ", first.reference()) });
    let prompt = edit.apply("hi");
    assert_eq!(edit.cursor(), prompt.len());
    let contexts = vec![first.clone()];
    let again = context("two", 3, 5, "a\nb\nc");
    assert_eq!(add_terminal_context(&prompt, prompt.len(), &contexts, &again), None);
    let other_lines = context("three", 3, 6, "a\nb\nc\nd");
    assert!(add_terminal_context(&prompt, prompt.len(), &contexts, &other_lines).is_some());
    // Once its link is gone, the record is only retained for undo and the lines can come back.
    let (cleared, _) = remove_reference(&prompt, &first.context_id());
    assert_eq!(cleared, "hi");
    assert!(add_terminal_context(&cleared, 0, &contexts, &again).is_some());
}

#[test]
fn removing_a_link_takes_one_space_with_it() {
    let excerpt = context("one", 1, 2, "x");
    let link = excerpt.reference();
    assert_eq!(remove_reference(&format!("see {link} now"), &excerpt.context_id()), ("see now".to_owned(), 4));
    assert_eq!(remove_reference(&format!("see {link}"), &excerpt.context_id()), ("see".to_owned(), 3));
    assert_eq!(remove_reference(&format!("{link} a {link} b"), &excerpt.context_id()), ("a b".to_owned(), 0));
    assert_eq!(remove_reference("untouched", &excerpt.context_id()), ("untouched".to_owned(), 9));
}

#[test]
fn referenced_excerpts_follow_the_prompt_order_once_each() {
    let (a, b, retained) = (context("a", 1, 1, "x"), context("b", 2, 2, "y"), context("c", 3, 3, "z"));
    let prompt = format!("{} then {} and {}", b.reference(), a.reference(), b.reference());
    let contexts = [a.clone(), b.clone(), retained];
    let ids: Vec<&str> = referenced(&prompt, &contexts).iter().map(|context| context.id.as_str()).collect();
    assert_eq!(ids, ["b", "a"]);
}

#[test]
fn a_send_drops_expired_excerpts_and_their_links() {
    let live = context("live", 1, 2, "cargo test");
    let expired = context("gone", 4, 4, "");
    let contexts = [live.clone(), expired.clone()];
    let send = TerminalSend::of(&format!("Explain {} and {}", live.reference(), expired.reference()), &contexts);
    assert_eq!(send.text, format!("Explain {} and", live.reference()));
    assert_eq!(send.terminals, vec![live.clone()]);
    assert_eq!(send.expired, 1);
    assert!(send.prose);
    assert!(send.has_content(0, 0));

    let only_expired = TerminalSend::of(&format!("  {}  ", expired.reference()), &contexts);
    assert_eq!((only_expired.text.as_str(), only_expired.expired, only_expired.prose), ("", 1, false));
    assert!(!only_expired.has_content(0, 0));
    assert!(only_expired.has_content(1, 0));
    assert!(only_expired.has_content(0, 1));

    let only_live = TerminalSend::of(&live.reference(), &contexts);
    assert!(!only_live.prose);
    assert!(only_live.has_content(0, 0));
}

#[test]
fn expired_toasts_read_as_the_web_words_them() {
    assert_eq!(
        expired_toast_copy(1, ExpiredNotice::Empty),
        ("Expired terminal context won't be sent".to_owned(), "Remove it or re-add it to include terminal output.".to_owned())
    );
    assert_eq!(
        expired_toast_copy(2, ExpiredNotice::Omitted),
        (
            "Expired terminal contexts omitted from message".to_owned(),
            "Re-add them if you want that terminal output included.".to_owned()
        )
    );
    assert_eq!(expired_toast_copy(0, ExpiredNotice::Omitted).0, "Expired terminal context omitted from message");
    assert_eq!(
        expired_tooltip("Terminal 1 line 4"),
        "Terminal context expired. Remove and re-add Terminal 1 line 4 to include it in your message."
    );
}

#[test]
fn a_send_in_flight_keeps_records_added_meanwhile() {
    let (sent, added) = (context("sent", 1, 2, "a"), context("added", 3, 3, "b"));
    let thread = ThreadContext::new("env", "t1", "Other");
    let prompt = format!("look at {} and {}", sent.reference(), thread.reference());
    let mut held = vec![sent.clone(), context("deleted", 9, 9, "c")];
    let taken = take_for_send(&mut held, &prompt);
    assert_eq!(taken, std::slice::from_ref(&sent), "the send takes what its text links; the rest go with the history");
    assert!(held.is_empty());
    // Added before the send lands; landing must not take it (it once cleared everything).
    held.push(added.clone());
    assert_eq!(held, std::slice::from_ref(&added));
    // A failed send puts its excerpts back beside it, once.
    restore_unsent(&mut held, taken.clone());
    restore_unsent(&mut held, taken);
    assert_eq!(held, [added, sent]);
    let mut threads = vec![thread.clone()];
    assert_eq!(take_for_send(&mut threads, &prompt), [thread]);
    let machine = super::super::records::environment_record("build", "Build Mac");
    let text = super::super::records::reference(&machine).unwrap();
    let mut records = vec![machine.clone()];
    let sent = take_for_send(&mut records, &text);
    assert!(records.is_empty());
    restore_unsent(&mut records, sent);
    assert_eq!(records, [machine]);
    reset_history(&mut records, "no links");
    assert!(records.is_empty());
    assert!(threads.is_empty());
}

#[test]
fn a_new_prompt_history_drops_unlinked_records() {
    let (kept, dropped) = (context("kept", 1, 1, "a"), context("dropped", 2, 2, "b"));
    let mut held = vec![kept.clone(), dropped];
    reset_history(&mut held, &format!("still {}", kept.reference()));
    assert_eq!(held, [kept]);
    let mut threads = vec![ThreadContext::new("env", "t1", "Other")];
    reset_history(&mut threads, "no links");
    assert!(threads.is_empty());
}
