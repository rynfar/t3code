use super::*;

fn outdent(value: &str, cursor: usize) -> Option<(String, usize)> {
    outdent_for_tab(value, cursor, cursor).map(|edit| (apply(value, &edit), edit.cursor_after))
}

#[test]
fn nested_bullets_move_to_their_parents_indent() {
    let value = "- a\n  - b";
    assert_eq!(outdent(value, value.len()), Some(("- a\n- b".into(), "- a\n- b".len())));
    assert_eq!(outdent("- a\n\t- b", 8), Some(("- a\n- b".into(), 7)));
}

#[test]
fn top_level_items_and_plain_text_are_left_for_other_handlers() {
    assert_eq!(outdent("- a", 3), None);
    assert_eq!(outdent("plain\n  text", 12), None);
    assert!(outdent_for_tab("- a\n  - b", 4, 8).is_none());
}

#[test]
fn without_a_parent_one_indent_step_comes_off() {
    assert_eq!(outdent("    - b", 7), Some(("  - b".into(), 5)));
}

#[test]
fn ordered_items_continue_the_parent_list_and_renumber_after() {
    let value = "1. a\n   1. b\n2. c\n3. d";
    let (text, cursor) = outdent(value, 12).unwrap();
    assert_eq!(text, "1. a\n2. b\n3. c\n4. d");
    assert_eq!(cursor, 9);
}

#[test]
fn task_items_outdent_like_bullets() {
    let value = "- [ ] a\n  - [x] b";
    assert_eq!(outdent(value, value.len()).map(|(text, _)| text), Some("- [ ] a\n- [x] b".into()));
}
