use std::collections::HashSet;

use serde_json::json;

use super::*;
use crate::thread::Entity;

fn files(patch: &str) -> Vec<FileDiff> {
    match parse_patch(patch) {
        ParsedPatch::Files(files) => files,
        other => panic!("expected files, got {other:?}"),
    }
}

const MODIFIED: &str = "diff --git a/src/main.rs b/src/main.rs
index 3b18e51..a2c4f1e 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main() {
 use std::io;
-fn old() {}
+fn new() {}
+fn added() {}

 fn keep() {}
@@ -20,3 +21,3 @@ impl Foo {
 a
-b
+c
 d
";

#[test]
fn parses_a_modified_file_with_line_numbers_and_stats() {
    let files = files(MODIFIED);
    assert_eq!(files.len(), 1);
    let file = &files[0];
    assert_eq!(file.path(), "src/main.rs");
    assert_eq!(file.change, ChangeKind::Modified);
    assert_eq!((file.additions, file.deletions), (3, 2));
    assert_eq!(file.hunks.len(), 2);
    let hunk = &file.hunks[0];
    assert_eq!((hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count), (1, 4, 1, 5));
    assert_eq!(hunk.context, "fn main() {");
    let summary: Vec<_> = hunk.lines.iter().map(|line| (line.kind, line.old_line, line.new_line, &*line.text)).collect();
    assert_eq!(
        summary,
        vec![
            (LineKind::Context, Some(1), Some(1), "use std::io;"),
            (LineKind::Deletion, Some(2), None, "fn old() {}"),
            (LineKind::Addition, None, Some(2), "fn new() {}"),
            (LineKind::Addition, None, Some(3), "fn added() {}"),
            (LineKind::Context, Some(3), Some(4), ""),
            (LineKind::Context, Some(4), Some(5), "fn keep() {}"),
        ]
    );
    assert_eq!(file.hunks[1].lines[2].new_line, Some(22));
    assert_eq!(line_stat(&files), LineStat { additions: 3, deletions: 2 });
}

#[test]
fn parses_new_deleted_and_renamed_files() {
    let patch = "diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index e69de29..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.rs b/new name.rs
similarity index 100%
rename from old name.rs
rename to new name.rs
diff --git a/a.rs b/b.rs
similarity index 80%
rename from a.rs
rename to b.rs
index 1..2 100644
--- a/a.rs
+++ b/b.rs
@@ -1 +1 @@
-x
+y
";
    let files = files(patch);
    let kinds: Vec<_> = files.iter().map(|file| (file.change, file.old_path.as_deref(), file.new_path.as_deref())).collect();
    assert_eq!(
        kinds,
        vec![
            (ChangeKind::New, None, Some("new.txt")),
            (ChangeKind::Deleted, Some("gone.txt"), None),
            (ChangeKind::RenamePure, Some("old name.rs"), Some("new name.rs")),
            (ChangeKind::RenameChanged, Some("a.rs"), Some("b.rs")),
        ]
    );
    assert_eq!(files[1].path(), "gone.txt");
    assert_eq!(files[1].hunks[0].lines[0].old_line, Some(1));
    assert!(files[2].hunks.is_empty());
    assert_ne!(files[0].key(), files[1].key());
}

#[test]
fn marks_binary_files_and_keeps_parsing_after_them() {
    let patch = "diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..1234567
Binary files /dev/null and b/logo.png differ
diff --git a/icon.bin b/icon.bin
index 1111111..2222222 100644
GIT binary patch
literal 12
TcmZ?wbhEHbRA*pdU}gXS0RRBv

literal 0
HcmV?d00001

diff --git a/x b/x
--- a/x
+++ b/x
@@ -1 +1 @@
-1
+2
";
    let files = files(patch);
    assert_eq!(files.len(), 3);
    assert!(files[0].binary && files[1].binary && !files[2].binary);
    assert_eq!(files[0].change, ChangeKind::New);
    assert_eq!(files[0].path(), "logo.png");
    assert_eq!(files[1].change, ChangeKind::Modified);
    assert_eq!((files[2].additions, files[2].deletions), (1, 1));
    let rows = build_rows(&files, &HashSet::new(), Layout::Stacked);
    assert_eq!(rows[1], DiffRow::Notice { file: 0 });
    assert_eq!(rows[3], DiffRow::Notice { file: 1 });
}

#[test]
fn marks_lines_without_a_trailing_newline() {
    let patch = "--- a/f
+++ b/f
@@ -1,2 +1,2 @@
 keep
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
    let files = files(patch);
    let lines = &files[0].hunks[0].lines;
    assert_eq!(lines.len(), 3);
    assert!(!lines[0].no_newline_at_eof);
    assert!(lines[1].no_newline_at_eof && lines[2].no_newline_at_eof);
    assert_eq!(files[0].change, ChangeKind::Modified);
    // Pierre paints a "No newline at end of file" row under each such line.
    assert_eq!(
        build_rows(&files, &HashSet::new(), Layout::Stacked),
        vec![
            DiffRow::FileHeader { file: 0 },
            DiffRow::Line { file: 0, hunk: 0, line: 0 },
            DiffRow::Line { file: 0, hunk: 0, line: 1 },
            DiffRow::NoNewline { file: 0, hunk: 0, line: 1 },
            DiffRow::Line { file: 0, hunk: 0, line: 2 },
            DiffRow::NoNewline { file: 0, hunk: 0, line: 2 },
        ]
    );
    assert_eq!(
        build_rows(&files, &HashSet::new(), Layout::Split)[2..],
        [
            DiffRow::Split { file: 0, hunk: 0, left: Some(1), right: Some(2) },
            DiffRow::SplitNoNewline { file: 0, hunk: 0, left: Some(1), right: Some(2) },
        ]
    );
}

#[test]
fn keeps_carriage_returns_in_content_but_not_in_headers() {
    let patch = "diff --git a/w.txt b/w.txt\r\n--- a/w.txt\r\n+++ b/w.txt\r\n@@ -1,2 +1,2 @@ ctx\r\n keep\r\n-x\r\n+x\n";
    let files = files(patch);
    assert_eq!(files[0].path(), "w.txt");
    assert_eq!(files[0].hunks[0].context, "ctx");
    let texts: Vec<_> = files[0].hunks[0].lines.iter().map(|line| (line.kind, &*line.text)).collect();
    assert_eq!(texts, vec![(LineKind::Context, "keep\r"), (LineKind::Deletion, "x\r"), (LineKind::Addition, "x")]);
}

#[test]
fn combined_diffs_fall_back_to_raw_text() {
    let patch = "diff --cc file.txt\nindex 1,2..3\n--- a/file.txt\n+++ b/file.txt\n@@@ -1,1 -1,1 +1,1 @@@\n- a\n -b\n++c\n";
    assert!(matches!(parse_patch(patch), ParsedPatch::Raw { .. }));
}

#[test]
fn collapsed_and_binary_files_build_only_their_header_rows() {
    let parsed = files(MODIFIED);
    assert_eq!(file_rows(3, &parsed[0], true, Layout::Split), vec![DiffRow::FileHeader { file: 3 }]);
    assert_eq!(file_rows(0, &parsed[0], false, Layout::Stacked).len(), 12);
}

#[test]
fn tolerates_empty_hunks_missing_counts_and_stripped_blank_context() {
    let patch = "diff --git a/f b/f
--- a/f
+++ b/f
@@ -0,0 +0,0 @@
@@ -3,2 +3,2 @@
-x

+y
@@ -9 +9 @@
-p
+q
";
    let files = files(patch);
    let hunks = &files[0].hunks;
    assert_eq!(hunks.len(), 3);
    assert!(hunks[0].lines.is_empty());
    let lines: Vec<_> = hunks[1].lines.iter().map(|line| (line.kind, line.old_line, line.new_line, &*line.text)).collect();
    assert_eq!(
        lines,
        vec![
            (LineKind::Deletion, Some(3), None, "x"),
            (LineKind::Context, Some(4), Some(3), ""),
            (LineKind::Addition, None, Some(4), "y"),
        ]
    );
    assert_eq!((hunks[2].old_count, hunks[2].new_count, hunks[2].lines.len()), (1, 1, 2));
    assert_eq!((files[0].additions, files[0].deletions), (2, 2));
}

#[test]
fn decodes_quoted_paths_and_paths_with_spaces() {
    let patch = "diff --git \"a/caf\\303\\251 \\\"x\\\".txt\" \"b/caf\\303\\251 \\\"x\\\".txt\"
index 1..2 100644
--- \"a/caf\\303\\251 \\\"x\\\".txt\"
+++ \"b/caf\\303\\251 \\\"x\\\".txt\"
@@ -1 +1 @@
-a
+b
diff --git a/my file.txt b/my file.txt
old mode 100644
new mode 100755
";
    let files = files(patch);
    assert_eq!(files[0].path(), "café \"x\".txt");
    assert_eq!(files[1].path(), "my file.txt");
    assert_eq!(files[1].change, ChangeKind::Modified);
    assert!(files[1].hunks.is_empty());
}

#[test]
fn parses_plain_unified_diffs_with_timestamps_and_expands_tabs() {
    let patch = "--- f.c\t2024-01-01 00:00:00
+++ f.c\t2024-01-02 00:00:00
@@ -1 +1 @@
-\tint x;
+\tint y;\tz
";
    let files = files(patch);
    assert_eq!(files[0].path(), "f.c");
    assert_eq!(&*files[0].hunks[0].lines[1].text, "  int y;  z");
}

#[test]
fn falls_back_to_raw_text_and_treats_blank_as_empty() {
    assert_eq!(parse_patch("  \n"), ParsedPatch::Empty);
    match parse_patch("not a diff at all") {
        ParsedPatch::Raw { text, reason } => {
            assert_eq!(text, "not a diff at all");
            assert!(reason.contains("Unsupported"));
        }
        other => panic!("expected raw, got {other:?}"),
    }
}

#[test]
fn builds_stacked_rows_with_separators_and_collapsed_files() {
    let files = files(MODIFIED);
    let rows = build_rows(&files, &HashSet::new(), Layout::Stacked);
    assert_eq!(rows[0], DiffRow::FileHeader { file: 0 });
    assert_eq!(rows[1], DiffRow::Line { file: 0, hunk: 0, line: 0 });
    // Lines 6..=20 of the new file sit between the hunks.
    assert_eq!(rows[7], DiffRow::Separator { file: 0, hunk: 1, hidden: 15 });
    assert_eq!(rows.len(), 1 + 6 + 1 + 4);
    assert_eq!(separator_label(15), "15 unmodified lines");
    assert_eq!(separator_label(1), "1 unmodified line");
    let collapsed: HashSet<String> = [files[0].key()].into();
    assert_eq!(build_rows(&files, &collapsed, Layout::Stacked), vec![DiffRow::FileHeader { file: 0 }]);
}

#[test]
fn pairs_deletions_with_additions_in_split_rows() {
    let files = files(MODIFIED);
    let rows = build_rows(&files, &HashSet::new(), Layout::Split);
    let first: Vec<_> = rows.iter().skip(1).take(5).copied().collect();
    assert_eq!(
        first,
        vec![
            DiffRow::Split { file: 0, hunk: 0, left: Some(0), right: Some(0) },
            DiffRow::Split { file: 0, hunk: 0, left: Some(1), right: Some(2) },
            DiffRow::Split { file: 0, hunk: 0, left: None, right: Some(3) },
            DiffRow::Split { file: 0, hunk: 0, left: Some(4), right: Some(4) },
            DiffRow::Split { file: 0, hunk: 0, left: Some(5), right: Some(5) },
        ]
    );
    assert!(rows.iter().all(|row| row.file() == 0));
}

#[test]
fn separators_use_the_old_side_for_deleted_files() {
    let patch = "--- a/f\n+++ /dev/null\n@@ -5,1 +0,0 @@\n-x\n";
    let rows = build_rows(&files(patch), &HashSet::new(), Layout::Stacked);
    assert_eq!(rows[1], DiffRow::Separator { file: 0, hunk: 0, hidden: 4 });
}

#[test]
fn file_tree_merges_single_child_directories_in_patch_order() {
    let patch = ["src/app/z.rs", "src/app/a.rs", "README.md", "src/lib.rs", "docs/guide/intro.md"]
        .iter()
        .map(|path| format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-a\n+b\n"))
        .collect::<String>();
    let files = files(&patch);
    let rows = file_tree_rows(&files, &HashSet::new());
    let labels: Vec<_> = rows.iter().map(|row| (row.depth, row.label.as_str())).collect();
    assert_eq!(
        labels,
        vec![(0, "src"), (1, "app"), (2, "z.rs"), (2, "a.rs"), (1, "lib.rs"), (0, "README.md"), (0, "docs/guide"), (1, "intro.md")]
    );
    let collapsed: HashSet<String> = ["src/app/".to_owned()].into();
    let rows = file_tree_rows(&files, &collapsed);
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[1].kind, TreeRowKind::Directory { path: "src/app/".into(), expanded: false });
    assert_eq!(rows[2].kind, TreeRowKind::File { file: 3, change: ChangeKind::Modified });
}

fn checkpoint(id: &str, run: Option<&str>, ordinal: Option<u64>, captured: &str) -> Entity {
    Entity {
        id: id.into(),
        value: json!({
            "id": id, "threadId": "t", "scopeId": "s", "runId": run, "nodeId": "n",
            "parentCheckpointId": null, "ordinalWithinScope": 0, "appRunOrdinal": ordinal,
            "ref": "refs/x", "status": "ready", "capturedAt": captured,
            "files": [{ "path": "a.rs", "kind": "modified", "additions": 2, "deletions": 1 }],
        }),
    }
}

#[test]
fn turn_summaries_skip_non_app_checkpoints_and_sort_newest_first() {
    let checkpoints = vec![
        checkpoint("c1", Some("r1"), Some(1), "2026-01-01T00:00:00Z"),
        checkpoint("sub", Some("r1"), None, "2026-01-01T00:00:01Z"),
        checkpoint("c3", Some("r3"), Some(3), "2026-01-03T00:00:00Z"),
        checkpoint("orphan", None, Some(4), "2026-01-04T00:00:00Z"),
        checkpoint("c2", Some("r2"), Some(2), "2026-01-02T00:00:00Z"),
    ];
    let summaries = turn_diff_summaries(&checkpoints);
    let order: Vec<_> = summaries.iter().map(|summary| (summary.run_id.as_str(), summary.turn_count)).collect();
    assert_eq!(order, vec![("r3", 3), ("r2", 2), ("r1", 1)]);
    assert_eq!(summaries[0].totals(), FileTotals { count: 1, additions: 2, deletions: 1 });
}

#[test]
fn turn_one_uses_the_full_thread_diff() {
    let (method, payload) = checkpoint_diff_request("t1", 1, true);
    assert_eq!(method, "orchestration.getFullThreadDiff");
    assert_eq!(payload, json!({ "threadId": "t1", "toTurnCount": 1, "ignoreWhitespace": true }));
    let (method, payload) = checkpoint_diff_request("t1", 4, false);
    assert_eq!(method, "orchestration.getTurnDiff");
    assert_eq!(payload, json!({ "threadId": "t1", "fromTurnCount": 3, "toTurnCount": 4, "ignoreWhitespace": false }));
    assert_eq!(decode_turn_diff(&json!({ "diff": "x" })), Ok("x".to_owned()));
    assert!(decode_turn_diff(&json!({})).is_err());
}

#[test]
fn diff_preview_picks_the_requested_source() {
    let (method, payload) = diff_preview_request("/repo", Some("main"), true);
    assert_eq!(method, "review.getDiffPreview");
    assert_eq!(payload, json!({ "cwd": "/repo", "baseRef": "main", "ignoreWhitespace": true }));
    let result = json!({
        "cwd": "/repo",
        "sources": [
            { "kind": "working-tree", "diff": "w", "truncated": false, "baseRef": null, "headRef": null },
            { "kind": "branch-range", "diff": "b", "truncated": true, "baseRef": "main", "headRef": "feat",
              "files": [{ "path": "a", "previousPath": null, "additions": 3, "deletions": 1 },
                        { "path": "b", "previousPath": null, "additions": 2, "deletions": 0 }] },
        ],
    });
    let branch = decode_diff_preview(&result, PreviewSourceKind::BranchRange).expect("branch source");
    assert_eq!((branch.diff.as_str(), branch.truncated, branch.totals), ("b", true, Some((5, 1))));
    assert_eq!(branch.head_ref.as_deref(), Some("feat"));
    let tree = decode_diff_preview(&result, PreviewSourceKind::WorkingTree).expect("working tree");
    assert_eq!((tree.diff.as_str(), tree.totals), ("w", None));
}

#[test]
fn selection_counts_reveals_and_reconciles_to_the_latest_turn() {
    let first = DiffSelection::turn(&DiffSelection::default(), "r1", Some(" src/a.rs "));
    assert_eq!(first, DiffSelection::Turn { turn_id: "r1".into(), file_path: Some("src/a.rs".into()), reveal_request_id: 1 });
    let again = DiffSelection::turn(&first, "r1", Some(""));
    assert_eq!(again, DiffSelection::Turn { turn_id: "r1".into(), file_path: None, reveal_request_id: 2 });

    let branch = DiffSelection::Branch { base_ref: Some("dev".into()) };
    assert_eq!(DiffSelection::git(&branch, None, false), branch);
    assert_eq!(DiffSelection::git(&first, Some("dev"), false), branch);
    assert_eq!(DiffSelection::git(&branch, None, true), DiffSelection::Unstaged);

    let summaries = turn_diff_summaries(&[
        checkpoint("c2", Some("r2"), Some(2), "b"),
        checkpoint("c3", Some("r3"), Some(3), "c"),
    ]);
    let mut selection = first.clone();
    assert!(selection.reconcile(&summaries));
    assert!(matches!(&selection, DiffSelection::Turn { turn_id, .. } if turn_id == "r3"));
    assert!(!selection.reconcile(&summaries));
    assert_eq!(DiffScope::resolve(&selection, &summaries), DiffScope::Turn { summary_index: 0, latest: true });
    let older = DiffSelection::turn(&selection, "r2", None);
    let scope = DiffScope::resolve(&older, &summaries);
    assert_eq!(scope.label(&summaries), "Turn 2");
    assert_eq!(DiffScope::resolve(&older, &[]), DiffScope::NoTurns);
    assert_eq!(DiffScope::resolve(&DiffSelection::default(), &[]).label(&[]), "Changes");
}

#[test]
fn selection_round_trips_through_json() {
    let selection = DiffSelection::Turn { turn_id: "r".into(), file_path: None, reveal_request_id: 3 };
    let text = serde_json::to_string(&selection).expect("encode");
    assert_eq!(text, r#"{"kind":"turn","turnId":"r","filePath":null,"revealRequestId":3}"#);
    assert_eq!(serde_json::from_str::<DiffSelection>(&text).expect("decode"), selection);
}

#[test]
fn compact_counts_match_the_web() {
    let cases = [(999, "999"), (1000, "1k"), (1500, "1.5k"), (12_345, "12k"), (999_499, "999k"), (999_500, "1m"), (2_500_000, "2.5m")];
    for (value, expected) in cases {
        assert_eq!(compact_count(value), expected, "{value}");
    }
}

#[test]
fn preview_without_sources_means_no_repository() {
    assert!(!preview_is_repository(&json!({ "cwd": "/x", "sources": [] })));
    assert!(preview_is_repository(&json!({ "sources": [{ "kind": "working-tree", "diff": "" }] })));
}

fn tool_item(id: &str, kind: &str, status: &str) -> crate::turn_item::TurnItem {
    let body = match kind {
        "command_execution" => json!({ "input": "ls" }),
        "file_change" => json!({ "fileName": "a.rs" }),
        _ => json!({ "text": "hi", "messageId": "msg", "streaming": false }),
    };
    let mut value = json!({ "id": id, "threadId": "t", "runId": "r", "nodeId": null, "parentItemId": null,
        "ordinal": 1, "status": status, "title": null, "startedAt": null, "completedAt": null, "type": kind });
    if let (Some(object), Some(extra)) = (value.as_object_mut(), body.as_object()) {
        object.extend(extra.clone());
    }
    serde_json::from_value(value).expect("turn item")
}

#[test]
fn workspace_mutation_id_tracks_finished_file_tools_and_checkpoints() {
    let items = [
        tool_item("c1", "command_execution", "completed"),
        tool_item("f1", "file_change", "running"),
        tool_item("m1", "assistant_message", "completed"),
    ];
    let id = workspace_mutation_id(items.iter(), None).expect("id");
    assert!(id.contains("c1") && !id.contains("f1"));
    let done = [tool_item("c1", "command_execution", "completed"), tool_item("f1", "file_change", "completed")];
    assert_ne!(workspace_mutation_id(done.iter(), None), Some(id.clone()));
    assert_ne!(workspace_mutation_id(items.iter(), Some("2026-01-01T00:00:00Z")), Some(id));
    assert_eq!(workspace_mutation_id(items[2..].iter(), None), None);
}

fn file_change(file_name: &str, diff_str: Option<&str>, changes: &[(&str, &str, Option<&str>)]) -> crate::turn_item::TurnItemKind {
    crate::turn_item::TurnItemKind::FileChange {
        file_name: file_name.into(),
        additions: None,
        deletions: None,
        diff_str: diff_str.map(str::to_owned),
        old_str: None,
        new_str: None,
        changes: (!changes.is_empty()).then(|| {
            changes
                .iter()
                .map(|(operation, path, old_path)| crate::turn_item::FileChangeDetail {
                    operation: (*operation).into(),
                    path: (*path).into(),
                    old_path: old_path.map(str::to_owned),
                    file_type: None,
                    mime_type: None,
                })
                .collect()
        }),
    }
}

fn recorded(kind: &crate::turn_item::TurnItemKind) -> ToolCallDiff {
    tool_call_diff(kind).expect("a file change")
}

/// The text a file's `side` lines hold, newlines included, as Pierre's line arrays join.
fn side_text(file: &FileDiff, side: LineKind) -> (usize, String) {
    let lines: Vec<&DiffLine> = file.hunks.iter().flat_map(|hunk| &hunk.lines).filter(|line| line.kind == side || line.kind == LineKind::Context).collect();
    let text = lines.iter().map(|line| format!("{}{}", line.text, if line.no_newline_at_eof { "" } else { "\n" })).collect();
    (lines.len(), text)
}

#[test]
fn tool_call_hunks_get_file_headers_including_renamed_paths() {
    for (changes, path) in [(vec![], "old.ts"), (vec![("update", "new.ts", Some("old.ts"))], "new.ts")] {
        let patch = recorded(&file_change("old.ts", Some("@@ -1 +1 @@\n-before\n+after\n"), &changes)).patch.expect("patch");
        let files = files(&patch);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path(), path);
        assert!(side_text(&files[0], LineKind::Addition).1.contains("after"));
        assert!(side_text(&files[0], LineKind::Deletion).1.contains("before"));
    }
}

#[test]
fn tool_call_added_and_deleted_contents_become_one_sided_hunks() {
    let contents = [
        ("first\nsecond", ""),
        ("@@ -0,0 +1 @@\n+content", "\n"),
        ("@@ -1 +0,0 @@\n-content", "\n"),
        ("first\nsecond", "\n"),
        ("--- notes\nsecond", "\n"),
        ("diff --git notes\nsecond", "\n"),
        ("--- a/embedded.ts\n+++ b/embedded.ts\n@@ -1 +1 @@\n-before\n+after", "\n"),
        ("diff --git a/embedded.ts b/embedded.ts\n--- a/embedded.ts\n+++ b/embedded.ts\n@@ -1 +1 @@\n-before\n+after", "\n"),
    ];
    for operation in ["add", "delete"] {
        for (content, ending) in contents {
            let recorded_content = format!("{content}{ending}");
            let kind = file_change("file with spaces.ts", Some(&recorded_content), &[(operation, "file with spaces.ts", None)]);
            let files = files(&recorded(&kind).patch.expect("patch"));
            let file = &files[0];
            assert_eq!(file.path(), "file with spaces.ts", "{operation} {content:?}");
            let lines = content.split('\n').count();
            let (added, deleted) = (side_text(file, LineKind::Addition), side_text(file, LineKind::Deletion));
            assert_eq!(added.0, if operation == "add" { lines } else { 0 }, "{operation} {content:?}");
            assert_eq!(deleted.0, if operation == "delete" { lines } else { 0 }, "{operation} {content:?}");
            assert_eq!(if operation == "add" { added.1 } else { deleted.1 }, recorded_content, "{operation} {content:?}");
        }
    }
}

#[test]
fn tool_call_content_without_a_diff_is_kept_out_of_the_patch() {
    for (operation, label) in [("add", "Recorded content after"), ("delete", "Recorded content before")] {
        let diff = recorded(&file_change("empty.ts", Some(""), &[(operation, "empty.ts", None)]));
        assert_eq!(diff.content, vec![RecordedContent { label, text: String::new() }]);
        assert_eq!(diff.patch.as_deref(), Some(""));
    }
    let mut kind = file_change("a.ts", None, &[]);
    if let crate::turn_item::TurnItemKind::FileChange { old_str, new_str, .. } = &mut kind {
        (*old_str, *new_str) = (Some("before".into()), Some(String::new()));
    }
    assert_eq!(
        recorded(&kind),
        ToolCallDiff {
            patch: None,
            content: vec![
                RecordedContent { label: "Recorded content before", text: "before".into() },
                RecordedContent { label: "Recorded content after", text: String::new() },
            ],
            truncated: false,
            recorded_files: 0,
        }
    );
    assert_eq!(tool_call_diff(&crate::turn_item::TurnItemKind::SystemNotice { message: "x".into() }), None);
}

#[test]
fn tool_call_drops_transport_truncation_markers() {
    let suffix = "\n… output truncated for transport";
    let diff = recorded(&file_change("a.ts", Some(&format!("first\nsecond{suffix}")), &[("add", "a.ts", None)]));
    assert!(diff.truncated);
    let patch = diff.patch.expect("patch");
    assert!(!patch.contains("output truncated"));
    assert_eq!(files(&patch)[0].additions, 2);
    let mut kind = file_change("a.ts", None, &[]);
    if let crate::turn_item::TurnItemKind::FileChange { new_str, .. } = &mut kind {
        *new_str = Some(format!("content{suffix}"));
    }
    let diff = recorded(&kind);
    assert!(diff.truncated);
    assert_eq!(diff.content, vec![RecordedContent { label: "Recorded content after", text: "content".into() }]);
}

#[test]
fn tool_call_keeps_complete_patches_and_unsupported_output() {
    for operation in ["add", "delete"] {
        let add = operation == "add";
        let patch = [
            "diff --git a/a.ts b/a.ts",
            if add { "--- /dev/null" } else { "--- a/a.ts" },
            if add { "+++ b/a.ts" } else { "+++ /dev/null" },
            if add { "@@ -0,0 +1 @@" } else { "@@ -1 +0,0 @@" },
            if add { "+content" } else { "-content" },
            "",
        ]
        .join("\n");
        assert_eq!(recorded(&file_change("a.ts", Some(&patch), &[(operation, "a.ts", None)])).patch, Some(patch));
    }
    for patch in [None, Some(""), Some("provider output"), Some("--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-a\n+b\n")] {
        assert_eq!(recorded(&file_change("a.ts", patch, &[])).patch.as_deref(), patch);
    }
    let two = recorded(&file_change("a.ts", Some("@@ -1 +1 @@\n-a\n+b\n"), &[("update", "a.ts", None), ("update", "b.ts", None)]));
    assert_eq!(two.omitted_files(1), 1);
}

#[test]
fn tool_call_target_follows_the_row_the_thread_shows() {
    let row = |status: &str, kind: &str| crate::thread::ProjectedTurnItem {
        position: 0,
        visibility: crate::thread::Visibility::Local,
        source_thread_id: "t".into(),
        source_item_id: "f1".into(),
        item: std::sync::Arc::new(crate::turn_item::TurnItem { updated_at: "2026-01-02T00:00:00Z".into(), ..tool_item("f1", kind, status) }),
    };
    let target = ToolCallTarget::of(&tool_item("f1", "file_change", "running"));
    assert_eq!(target, ToolCallTarget { thread_id: "t".into(), item_id: "f1".into(), revision: "live".into() });
    assert_eq!(target.resolve(&[row("completed", "file_change")]).revision, "2026-01-02T00:00:00Z");
    let mut parent_row = row("completed", "file_change");
    parent_row.source_thread_id = "parent".into();
    assert_eq!(target.resolve(&[parent_row.clone()]), target, "an identical item id in the parent cannot settle the child");
    let settled = target.resolve(&[parent_row, row("completed", "file_change")]);
    assert_ne!(settled.request(), target.request(), "settlement must invalidate a live detail request");
    assert_eq!(settled.thread_id, target.thread_id);
    assert_eq!(settled.item_id, target.item_id);
    assert_eq!(target.resolve(&[row("completed", "command_execution")]), target);
    assert_eq!(target.resolve(&[]), target);
    assert_eq!(target.request(), json!({ "threadId": "t", "itemId": "f1", "revision": "live" }));

    let selection = DiffSelection::ToolCall(target.clone());
    let text = serde_json::to_string(&selection).expect("encode");
    assert_eq!(text, r#"{"kind":"tool-call","threadId":"t","itemId":"f1","revision":"live"}"#);
    assert_eq!(serde_json::from_str::<DiffSelection>(&text).expect("decode"), selection);
    let scope = DiffScope::resolve(&selection, &[]);
    assert_eq!((scope.clone(), scope.label(&[]).as_str()), (DiffScope::ToolCall(target), "Tool call"));
    assert_eq!(DiffSelection::git(&selection, Some("dev"), false), DiffSelection::Branch { base_ref: Some("dev".into()) });
}

#[test]
fn hiding_whitespace_turns_whitespace_only_edits_into_context() {
    let patch = parse_patch("diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1,4 +1,4 @@\n def f():\n-  return 1\n-x = 1\n+    return 1\n+x = 2\n tail\n");
    let ParsedPatch::Files(files) = hide_whitespace_changes(&patch) else { panic!("files") };
    let lines = &files[0].hunks[0].lines;
    let shape: Vec<(LineKind, &str, Option<u32>, Option<u32>)> = lines.iter().map(|line| (line.kind, &*line.text, line.old_line, line.new_line)).collect();
    assert_eq!(
        shape,
        vec![
            (LineKind::Context, "def f():", Some(1), Some(1)),
            (LineKind::Context, "    return 1", Some(2), Some(2)),
            (LineKind::Deletion, "x = 1", Some(3), None),
            (LineKind::Addition, "x = 2", None, Some(3)),
            (LineKind::Context, "tail", Some(4), Some(4)),
        ]
    );
    assert_eq!((files[0].additions, files[0].deletions), (1, 1));

    let reindent = parse_patch("--- a/b.rs\n+++ b/b.rs\n@@ -1,2 +1,2 @@\n-a\n-\tb\n+  a\n+ b\n");
    let ParsedPatch::Files(files) = hide_whitespace_changes(&reindent) else { panic!("files") };
    assert!(files[0].hunks[0].lines.iter().all(|line| line.kind == LineKind::Context));
    assert_eq!((files[0].additions, files[0].deletions), (0, 0));
    assert_eq!(hide_whitespace_changes(&ParsedPatch::Empty), ParsedPatch::Empty);
}

#[test]
fn session_whitespace_defaults_follow_threads_without_leaking_toolbar_overrides() {
    let mut session = DiffSession::default();
    assert!(session.ignore_whitespace("thread-a", true));
    assert!(!session.ignore_whitespace("thread-b", false), "new threads use the saved default");
    assert!(session.toggle_whitespace("thread-b", false));
    assert!(!session.ignore_whitespace("thread-a", false), "a toolbar override belongs to its thread");
    assert!(session.ignore_whitespace("thread-b", false), "returning to the overridden thread keeps its choice");
    assert!(!session.ignore_whitespace("thread-c", false));
    assert!(session.ignore_whitespace("thread-c", true), "an untouched thread can pick up a new default on reopen");
}

#[test]
fn session_fold_defaults_follow_current_files_until_an_explicit_action() {
    let mut session = DiffSession::default();
    let initial = vec!["a".to_owned(), "b".to_owned()];
    let appended = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
    assert_eq!(session.resolve_folds("scope-a", &initial, true), vec![true, true]);
    assert_eq!(session.resolve_folds("scope-a", &appended, true), vec![true, true, true], "new files inherit the default");
    assert_eq!(session.resolve_folds("scope-a", &appended, false), vec![false, false, false], "untouched scopes follow default changes");
    assert!(session.stored_folds("scope-a").is_none(), "rendering never saves an override");
    session.set_file_fold("scope-a".to_owned(), "b".to_owned(), false, || initial.iter().cloned().collect());
    assert_eq!(session.resolve_folds("scope-a", &appended, true), vec![true, false, false]);
    assert_eq!(session.resolve_folds("scope-a", &appended, false), vec![true, false, false]);
    session.remember_folds("scope-a".to_owned(), HashSet::new());
    assert_eq!(session.resolve_folds("scope-a", &appended, true), vec![false, false, false], "expand-all is an explicit override");
    assert!(session.resolve_folds("scope-b", &[], true).is_empty());
    assert_eq!(session.resolve_folds("scope-b", &appended, true), vec![true, true, true], "an empty loading scope does not pin an empty override");
}
