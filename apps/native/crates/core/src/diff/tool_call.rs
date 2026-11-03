//! One tool call's recorded change in the diff panel: `apps/web/src/lib/toolCallDiff.ts` (turning
//! what a provider recorded into a patch the panel can parse) and `ToolCallDiffTarget` /
//! `resolveToolCallDiffTarget` from `apps/web/src/diffPanelStore.ts`.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{FileDiff, LineKind, ParsedPatch, parse_patch};
use crate::thread::ProjectedTurnItem;
use crate::timeline::detail_revision;
use crate::turn_item::{FileChangeDetail, TurnItem, TurnItemKind};

const TRANSPORT_TRUNCATION: &str = "\n… output truncated for transport";

/// `ToolCallDiffTarget`: the item to fetch, keyed by its `turnItemDetailRevision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallTarget {
    pub thread_id: String,
    pub item_id: String,
    pub revision: String,
}

impl ToolCallTarget {
    /// What `V2ItemInspector`'s "Open diff" sends for a file change.
    pub fn of(item: &TurnItem) -> ToolCallTarget {
        ToolCallTarget { thread_id: item.thread_id.clone(), item_id: item.id.clone(), revision: detail_revision(item) }
    }

    /// `resolveToolCallDiffTarget`: the revision of the row the thread shows now, so a change
    /// that was still running when opened is fetched again once it settles.
    pub fn resolve(&self, items: &[ProjectedTurnItem]) -> ToolCallTarget {
        let row = items.iter().find(|row| row.source_thread_id == self.thread_id && row.source_item_id == self.item_id);
        match row {
            Some(row) if matches!(row.item.kind, TurnItemKind::FileChange { .. }) => {
                ToolCallTarget { revision: detail_revision(&row.item), ..self.clone() }
            }
            _ => self.clone(),
        }
    }

    /// Missing or removed shells may be unloaded or archived; only explicit deletion blocks a read.
    pub fn source_available(&self, shell: Option<&crate::shell::ThreadShell>, state: &crate::thread::ThreadState) -> bool {
        !state.source_deleted() && shell.filter(|thread| thread.id == self.thread_id).is_none_or(|thread| thread.deleted_at.is_none())
    }

    /// `orchestration.getTurnItem`'s input.
    pub fn request(&self) -> Value {
        json!({ "threadId": self.thread_id, "itemId": self.item_id, "revision": self.revision })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedContent {
    pub label: &'static str,
    pub text: String,
}

/// `toolCallDiff`'s result, plus the file count `DiffPanel.tsx` compares against the patch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallDiff {
    pub patch: Option<String>,
    /// Content the provider recorded without a diff, shown when there is no patch to render.
    pub content: Vec<RecordedContent>,
    pub truncated: bool,
    /// `changes.length`: the files the tool call reported changing.
    pub recorded_files: usize,
}

impl ToolCallDiff {
    /// `omittedToolCallFiles`: recorded files the rendered patch does not hold.
    pub fn omitted_files(&self, rendered: usize) -> usize {
        self.recorded_files.saturating_sub(rendered)
    }
}

/// `recordedText`: drops the server's transport truncation marker.
fn recorded_text(value: Option<&str>) -> (Option<&str>, bool) {
    match value.and_then(|value| value.strip_suffix(TRANSPORT_TRUNCATION)) {
        Some(text) => (Some(text), true),
        None => (value, false),
    }
}

/// `toolCallDiff`. `None` for anything but a file change.
pub fn tool_call_diff(kind: &TurnItemKind) -> Option<ToolCallDiff> {
    let TurnItemKind::FileChange { file_name, diff_str, old_str, new_str, changes, .. } = kind else { return None };
    let changes = changes.as_deref().unwrap_or_default();
    let (diff, diff_truncated) = recorded_text(diff_str.as_deref());
    let operation = changes.first().map(|change| change.operation.as_str());
    let empty_content = (diff == Some("")).then_some("");
    let (before, before_truncated) = recorded_text(old_str.as_deref().or(if operation == Some("delete") { empty_content } else { None }));
    let (after, after_truncated) = recorded_text(new_str.as_deref().or(if operation == Some("add") { empty_content } else { None }));
    let content = if diff.is_some_and(|diff| !diff.trim().is_empty()) {
        Vec::new()
    } else {
        [("Recorded content before", before), ("Recorded content after", after)]
            .into_iter()
            .filter_map(|(label, text)| text.map(|text| RecordedContent { label, text: text.to_owned() }))
            .collect()
    };
    Some(ToolCallDiff {
        patch: diff.map(|diff| tool_call_patch(file_name, diff, changes)),
        content,
        truncated: diff_truncated || before_truncated || after_truncated,
        recorded_files: changes.len(),
    })
}

/// `toolCallPatch`: a complete patch stays as recorded; a bare hunk gets file headers; an added
/// or deleted file's recorded contents become one all-added or all-deleted hunk.
fn tool_call_patch(file_name: &str, patch: &str, changes: &[FileChangeDetail]) -> String {
    if patch.is_empty() {
        return String::new();
    }
    let change = changes.first();
    let operation = change.map(|change| change.operation.as_str());
    let raw_file_operation = matches!(operation, Some("add" | "delete"));
    if (patch.starts_with("diff --git ") || patch.starts_with("--- ")) && (!raw_file_operation || is_complete_patch(patch, changes)) {
        return patch.to_owned();
    }
    // Codex can record added/deleted file contents or bare hunks instead of a complete patch.
    let hunk = if raw_file_operation {
        Cow::Owned(contents_hunk(patch, operation == Some("add")))
    } else if patch.trim_start().starts_with("@@ ") {
        Cow::Borrowed(patch)
    } else {
        return patch.to_owned();
    };
    let old_path = change.and_then(|change| change.old_path.as_deref()).unwrap_or(file_name);
    let new_path = change.map_or(file_name, |change| change.path.as_str());
    // `JSON.stringify`, which the patch parser reads as git's quoted form.
    let quote = |path: String| serde_json::to_string(&path).unwrap_or_default();
    let (old, new) = (quote(format!("a/{old_path}")), quote(format!("b/{new_path}")));
    let before = if operation == Some("add") { "/dev/null" } else { old.as_str() };
    let after = if operation == Some("delete") { "/dev/null" } else { new.as_str() };
    format!("diff --git {old} {new}\n--- {before}\n+++ {after}\n{}", hunk.trim_start())
}

/// A recorded add or delete whose patch already covers every file it names, each with hunks
/// and nothing on the side that file never had.
fn is_complete_patch(patch: &str, changes: &[FileChangeDetail]) -> bool {
    let ParsedPatch::Files(files) = parse_patch(patch) else { return false };
    files.iter().all(|file| {
        let path = file.path().replace('\\', "/");
        !file.hunks.is_empty()
            && changes.iter().any(|recorded| {
                let recorded_path = recorded.path.replace('\\', "/");
                let one_sided = match recorded.operation.as_str() {
                    "add" => !has_side(file, LineKind::Deletion),
                    "delete" => !has_side(file, LineKind::Addition),
                    _ => true,
                };
                one_sided && (recorded_path == path || recorded_path.ends_with(&format!("/{path}")))
            })
    })
}

/// Whether the file has any line on the side `kind` belongs to; context lines are on both.
fn has_side(file: &FileDiff, kind: LineKind) -> bool {
    file.hunks.iter().flat_map(|hunk| &hunk.lines).any(|line| line.kind == kind || line.kind == LineKind::Context)
}

fn contents_hunk(contents: &str, added: bool) -> String {
    let mut lines: Vec<&str> = contents.split('\n').collect();
    let final_newline = contents.ends_with('\n');
    if final_newline {
        lines.pop();
    }
    let range = format!("1,{}", lines.len());
    let (old, new) = if added { ("0,0", range.as_str()) } else { (range.as_str(), "0,0") };
    let sign = if added { '+' } else { '-' };
    let body: Vec<String> = lines.iter().map(|line| format!("{sign}{line}")).collect();
    let eof = if final_newline { "" } else { "\\ No newline at end of file\n" };
    format!("@@ -{old} +{new} @@\n{}\n{eof}", body.join("\n"))
}
