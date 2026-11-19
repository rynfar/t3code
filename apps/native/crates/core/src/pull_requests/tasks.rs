//! Task list checkboxes in a pull request description. Ports `taskListMarkerOffset` and
//! `setMarkdownTaskChecked` from `packages/shared/src/markdownTaskList.ts`, and splits the
//! description so each task's checkbox can write its source marker back
//! (`PullRequestMarkdown.tsx`'s `onTaskListChange`). Offsets are bytes into the description.

use std::sync::LazyLock;

use markdown::mdast::Node;
use regex::Regex;

static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\s*(?:[-+*]|\d+[.)])\s+)(\[[ xX]\])").unwrap());

/// `taskListMarkerOffset`: the `[ ]` marker on the first line of the item starting at `start`.
pub fn task_marker_offset(markdown: &str, start: usize) -> Option<usize> {
    let rest = markdown.get(start..)?;
    let line = rest.split('\n').next().unwrap_or_default();
    let marker = MARKER.captures(line)?.get(1)?;
    Some(start + marker.start())
}

/// `setMarkdownTaskChecked`: the body with the marker at `offset` ticked or cleared, unchanged
/// unless a marker is there.
pub fn set_task_checked(markdown: &str, offset: usize, checked: bool) -> String {
    let bytes = markdown.as_bytes();
    let is_marker = bytes.get(offset) == Some(&b'[') && matches!(bytes.get(offset + 1), Some(b' ' | b'x' | b'X')) && bytes.get(offset + 2) == Some(&b']');
    if !is_marker {
        return markdown.to_owned();
    }
    format!("{}{}{}", &markdown[..offset + 1], if checked { 'x' } else { ' ' }, &markdown[offset + 2..])
}

/// One piece of a description that has task items, drawn in order. `depth` is the list nesting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Markdown drawn as it is.
    Markdown { text: String, depth: usize },
    /// A task item's first paragraph, after its checkbox. `marker` is `None` when the item's
    /// marker could not be found in the source, which leaves the checkbox read-only.
    Task { marker: Option<usize>, checked: bool, text: String, depth: usize },
    /// A plain item of a list that also holds tasks: its bullet or number, then its paragraph.
    Item { bullet: String, text: String, depth: usize },
}

fn span(node: &Node) -> Option<(usize, usize, usize)> {
    let position = node.position()?;
    Some((position.start.offset, position.end.offset, position.start.column.saturating_sub(1)))
}

fn has_task(node: &Node) -> bool {
    matches!(node, Node::ListItem(item) if item.checked.is_some()) || node.children().is_some_and(|children| children.iter().any(has_task))
}

/// Source from `start` to `end`, with continuation lines dedented by up to `indent` spaces so a
/// block lifted out of a list item parses as it did inside it.
fn lift(source: &str, start: usize, end: usize, indent: usize) -> String {
    let text = source.get(start..end).unwrap_or_default();
    let mut lines = text.split('\n');
    let mut out = lines.next().unwrap_or_default().to_owned();
    for line in lines {
        let strip = line.bytes().take(indent).take_while(|byte| *byte == b' ').count();
        out.push('\n');
        out.push_str(&line[strip..]);
    }
    out.trim_end().to_owned()
}

struct Walk<'a> {
    source: &'a str,
    out: Vec<Segment>,
}

impl Walk<'_> {
    fn markdown(&mut self, run: &mut Option<(usize, usize, usize)>, depth: usize) {
        if let Some((start, end, indent)) = run.take() {
            let text = lift(self.source, start, end, indent);
            if !text.trim().is_empty() {
                self.out.push(Segment::Markdown { text, depth });
            }
        }
    }

    fn blocks(&mut self, blocks: &[Node], depth: usize) {
        let mut run: Option<(usize, usize, usize)> = None;
        for block in blocks {
            let Some((start, end, indent)) = span(block) else { continue };
            let Node::List(list) = block else {
                run = Some(run.map_or((start, end, indent), |(first, _, indent)| (first, end, indent)));
                continue;
            };
            if !has_task(block) {
                run = Some(run.map_or((start, end, indent), |(first, _, indent)| (first, end, indent)));
                continue;
            }
            self.markdown(&mut run, depth);
            let first_number = list.start.unwrap_or(1);
            for (index, item) in list.children.iter().enumerate() {
                let Node::ListItem(entry) = item else { continue };
                let (rest, text) = match entry.children.split_first() {
                    Some((Node::Paragraph(_), rest)) => (rest, span(&entry.children[0]).map(|(start, end, indent)| lift(self.source, start, end, indent)).unwrap_or_default()),
                    _ => (entry.children.as_slice(), String::new()),
                };
                match entry.checked {
                    Some(checked) => {
                        let marker = span(item).and_then(|(start, _, _)| task_marker_offset(self.source, start));
                        self.out.push(Segment::Task { marker, checked, text, depth });
                    }
                    None => {
                        let bullet = if list.ordered { format!("{}.", first_number as usize + index) } else { "•".to_owned() };
                        self.out.push(Segment::Item { bullet, text, depth });
                    }
                }
                self.blocks(rest, depth + 1);
            }
        }
        self.markdown(&mut run, depth);
    }
}

/// The description in segments when it has task items, so their checkboxes can be ticked; `None`
/// when it has none and draws as one piece of markdown.
pub fn task_segments(source: &str) -> Option<Vec<Segment>> {
    let root = markdown::to_mdast(source, &markdown::ParseOptions::gfm()).ok()?;
    if !has_task(&root) {
        return None;
    }
    let mut walk = Walk { source, out: Vec::new() };
    walk.blocks(root.children()?, 0);
    Some(walk.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_found_on_the_item_line_and_written_in_place() {
        let body = "Intro\n\n- [ ] one\n  1. [X] two\n";
        let one = body.find("- [").unwrap();
        assert_eq!(task_marker_offset(body, one), Some(one + 2));
        let two = body.find("1.").unwrap() - 2;
        assert_eq!(task_marker_offset(body, two), Some(body.find("[X]").unwrap()));
        assert_eq!(task_marker_offset(body, 0), None, "a paragraph has no marker");
        assert_eq!(set_task_checked(body, one + 2, true), "Intro\n\n- [x] one\n  1. [X] two\n");
        assert_eq!(set_task_checked(body, body.find("[X]").unwrap(), false), "Intro\n\n- [ ] one\n  1. [ ] two\n");
        assert_eq!(set_task_checked(body, 1, true), body, "nothing changes away from a marker");
        assert_eq!(set_task_checked(body, 999, true), body);
    }

    #[test]
    fn descriptions_without_tasks_draw_whole() {
        assert_eq!(task_segments("# Title\n\n- a\n- b\n\n```\n- [ ] in code\n```\n"), None);
    }

    #[test]
    fn task_lists_split_out_with_their_markers_and_nested_blocks() {
        let body = "## Plan\n\nSome text.\n\n- [x] Ship **it**\n- [ ] Test\n  continued\n  - [ ] nested\n- plain\n\n1. [ ] numbered\n\nAfter.";
        let segments = task_segments(body).unwrap();
        let ship = body.find("[x]").unwrap();
        let test = body.find("[ ] Test").unwrap();
        let nested = body.find("[ ] nested").unwrap();
        let numbered = body.find("[ ] numbered").unwrap();
        assert_eq!(
            segments,
            vec![
                Segment::Markdown { text: "## Plan\n\nSome text.".into(), depth: 0 },
                Segment::Task { marker: Some(ship), checked: true, text: "Ship **it**".into(), depth: 0 },
                Segment::Task { marker: Some(test), checked: false, text: "Test\ncontinued".into(), depth: 0 },
                Segment::Task { marker: Some(nested), checked: false, text: "nested".into(), depth: 1 },
                Segment::Item { bullet: "•".into(), text: "plain".into(), depth: 0 },
                Segment::Task { marker: Some(numbered), checked: false, text: "numbered".into(), depth: 0 },
                Segment::Markdown { text: "After.".into(), depth: 0 },
            ]
        );
        // Every marker writes back to the source it came from.
        let ticked = set_task_checked(body, nested, true);
        assert!(ticked.contains("- [x] nested"));
        assert_eq!(ticked.len(), body.len());
    }

    #[test]
    fn markers_count_bytes_past_wide_characters() {
        let body = "Ünïcödé 😀\n\n- [ ] ✅ done";
        let Some(segments) = task_segments(body) else { panic!("has a task") };
        let Segment::Task { marker: Some(marker), .. } = segments[1] else { panic!("task") };
        assert_eq!(set_task_checked(body, marker, true), "Ünïcödé 😀\n\n- [x] ✅ done");
    }
}
