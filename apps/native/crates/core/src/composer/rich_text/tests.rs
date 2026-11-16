use super::*;

#[test]
fn literal_underscore_runs_ignore_paths_and_keep_emphasis_alternatives() {
    let text = "check ~~strike~~, `inline_code`, src/foo_bar/baz_qux.rs and _yes_/_no_ and _italic_";
    let doc = parse_composer_document(text);
    let italic_texts: Vec<&str> = doc
        .marks
        .iter()
        .filter(|m| m.mark == InlineFormat::Italic)
        .map(|m| &text[m.content_range.clone()])
        .collect();
    assert_eq!(italic_texts, vec!["yes", "no", "italic"]);
    assert!(doc.marks.iter().any(|m| m.mark == InlineFormat::Strike && &text[m.content_range.clone()] == "strike"));
}

#[test]
fn parses_inline_marks_and_tracks_marker_reveal_state() {
    let text = "hello **bold** and `code`";
    let doc = parse_composer_document(text);
    assert_eq!(doc.marks.len(), 2);
    assert_eq!(doc.marks[0].mark, InlineFormat::Bold);
    assert_eq!(&text[doc.marks[0].open_marker.clone()], "**");
    assert_eq!(&text[doc.marks[0].content_range.clone()], "bold");
    assert_eq!(&text[doc.marks[0].close_marker.clone()], "**");

    // Caret outside `**bold**` collapses markers; caret touching or inside reveals them (#277).
    assert!(!is_marker_revealed(&doc.marks[0].full_range, &(0..0)));
    assert!(is_marker_revealed(&doc.marks[0].full_range, &(6..6)));
    assert!(is_marker_revealed(&doc.marks[0].full_range, &(10..10)));
    assert!(is_marker_revealed(&doc.marks[0].full_range, &(14..14)));
    assert!(!is_marker_revealed(&doc.marks[0].full_range, &(15..15)));
}

#[test]
fn parses_fenced_code_blocks_and_handles_enter_and_indent() {
    let open = "```rust";
    let edit = code_block_enter(open, open.len(), open.len()).unwrap();
    assert_eq!(edit.replacement, "\n\n```");
    assert_eq!(edit.selection_after, 8..8);

    let body = "```rust\n  let x = 1;\n```";
    let indented = code_block_enter(body, 20, 20).unwrap();
    assert_eq!(indented.replacement, "\n  ");

    let tab = code_block_indent(body, 10, 10, false).unwrap();
    assert_eq!(tab.replacement, "  ");
    let untab = code_block_indent(body, 12, 12, true).unwrap();
    assert_eq!(untab.replacement, "let x = 1;");

    let exit = "```rust\ncode\n\n\n```";
    let exit_edit = code_block_enter(exit, 14, 14).unwrap();
    assert_eq!(exit_edit.replacement, "\n```\n");
}

#[test]
fn toggles_inline_formatting_and_unwraps_when_inside() {
    let wrapped = toggle_inline_format("hello world", 6, 11, InlineFormat::Bold);
    assert_eq!(wrapped.replacement, "**world**");
    assert_eq!(wrapped.selection_after, 8..13);

    let text = "hello **world**";
    let unwrapped = toggle_inline_format(text, 10, 10, InlineFormat::Bold);
    assert_eq!(unwrapped.start, 6);
    assert_eq!(unwrapped.end, 15);
    assert_eq!(unwrapped.replacement, "world");
    assert_eq!(unwrapped.selection_after, 8..8);
}
