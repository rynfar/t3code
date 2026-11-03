//! Diff comments from `web/src/reviewCommentContext.ts` and the review-comment records/links
//! from `web/src/lib/composerContextRecords.ts`. Native patches are partial: indexes count
//! hunk lines only, as Pierre does before file hydration.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::context::{Linked, format_reference};
use super::menu::{context_references, kind_scoped_context_id, sanitize_context_label};
use crate::diff::{DiffLine, FileDiff, LineKind};

pub const KIND: &str = "review-comment";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Additions,
    Deletions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub start: u32,
    pub side: Side,
    pub end: u32,
    pub end_side: Side,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub id: String,
    pub section_id: String,
    pub section_title: String,
    pub file_path: String,
    pub start_index: usize,
    pub end_index: usize,
    pub range_label: String,
    pub text: Arc<str>,
    pub diff: Arc<str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
}

fn number(line: &DiffLine, side: Side) -> Option<u32> {
    match side {
        Side::Additions => line.new_line,
        Side::Deletions => line.old_line,
    }
}

fn point(line: &DiffLine) -> (u32, Side) {
    if let Some(n) = line.new_line { (n, Side::Additions) } else { (line.old_line.unwrap_or(0), Side::Deletions) }
}

fn marker(line: &DiffLine) -> char {
    match line.kind {
        LineKind::Context => ' ',
        LineKind::Addition => '+',
        LineKind::Deletion => '-',
    }
}

pub fn selection_indexes(file: &FileDiff, selection: Selection) -> Option<(usize, usize)> {
    let index = |number_to_find, side| {
        let find = |side| file.hunks.iter().flat_map(|hunk| &hunk.lines).position(|line| number(line, side) == Some(number_to_find));
        find(side).or_else(|| {
            find(match side {
                Side::Additions => Side::Deletions,
                Side::Deletions => Side::Additions,
            })
        })
    };
    Some((index(selection.start, selection.side)?, index(selection.end, selection.end_side)?))
}

pub fn extend_selection(file: &FileDiff, previous: Selection, end: u32, end_side: Side) -> Selection {
    let target = Selection { start: end, side: end_side, end, end_side };
    let Some((start_index, end_index)) = selection_indexes(file, previous) else { return target };
    let Some((target_index, _)) = selection_indexes(file, target) else { return previous };
    let use_start = if start_index <= end_index { target_index >= start_index } else { target_index <= start_index };
    let (start, side) = if use_start { (previous.start, previous.side) } else { (previous.end, previous.end_side) };
    Selection { start, side, end, end_side }
}

pub fn build_diff_comment(
    id: String,
    section_id: String,
    section_title: String,
    file: &FileDiff,
    selection: Selection,
    text: &str,
) -> Option<ReviewComment> {
    let lines: Vec<_> = file.hunks.iter().flat_map(|hunk| &hunk.lines).collect();
    let (first, last) = selection_indexes(file, selection)?;
    let (start_index, end_index) = (first.min(last), first.max(last));
    let selected = &lines[start_index..=end_index];
    let range = |side| {
        let mut numbers = selected.iter().filter_map(|line| number(line, side));
        let first = numbers.next();
        (first.unwrap_or(0), first.map_or(0, |_| 1 + numbers.count()))
    };
    let (old_start, old_count) = range(Side::Deletions);
    let (new_start, new_count) = range(Side::Additions);
    let first = selected[0];
    let last = selected[selected.len() - 1];
    let first_number = point(first).0;
    let last_number = point(last).0;
    let mark = if first.kind != LineKind::Context && selected.iter().all(|line| line.kind == first.kind) {
        marker(first).to_string()
    } else {
        String::new()
    };
    let range_label =
        if first_number == last_number { format!("{mark}{first_number}") } else { format!("{mark}{first_number} to {mark}{last_number}") };
    let mut diff = format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@");
    for line in selected {
        diff.push('\n');
        diff.push(marker(line));
        diff.push_str(&line.source_text);
    }
    Some(ReviewComment {
        id,
        section_id,
        section_title,
        file_path: file.path().into(),
        start_index,
        end_index,
        range_label,
        text: text.trim().into(),
        diff: diff.into(),
        fence_language: Some("diff".into()),
        selection: Some(selection),
    })
}

impl ReviewComment {
    pub fn context_id(&self) -> String {
        kind_scoped_context_id(KIND, &self.id)
    }
    pub fn label(&self) -> String {
        let range = &self.range_label;
        let normalized = range
            .chars()
            .next()
            .filter(|c| *c == '+' || *c == '-')
            .and_then(|sign| {
                let parts: Vec<_> = range[1..].split(&format!(" to {sign}")).collect();
                (parts.len() <= 2 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))).then(|| {
                    let mut label = format!("L{}", parts[0]);
                    if parts.len() == 2 {
                        label.push_str(&format!(" to L{}", parts[1]));
                    }
                    if sign == '-' {
                        label.push_str(" (before)");
                    }
                    label
                })
            })
            .unwrap_or_else(|| range.clone());
        format!("{} {normalized}", self.file_path.rsplit(['/', '\\']).next().unwrap_or(&self.file_path))
    }
    pub fn reference(&self) -> String {
        format_reference(KIND, &self.context_id(), &self.label())
    }
    pub fn record(&self) -> Value {
        let mut record = json!({ "version":1, "contextId":self.context_id(), "kind":KIND, "label":sanitize_context_label(&self.label(), KIND), "sectionId":self.section_id, "sectionTitle":self.section_title, "filePath":self.file_path, "startIndex":self.start_index, "endIndex":self.end_index, "rangeLabel":self.range_label, "text":clamp_text(&self.text, 16_000), "diff":clamp_text(&self.diff, 32_000) });
        if let Some(language) = &self.fence_language {
            record["fenceLanguage"] = json!(language);
        }
        record
    }
    pub fn from_record(record: &Value) -> Option<Self> {
        if record.get("kind")?.as_str()? != KIND || record.get("version")?.as_u64()? != 1 {
            return None;
        }
        let mut value = record.clone();
        let id = value.get("contextId")?.as_str()?.strip_prefix("review-comment_").unwrap_or(value.get("contextId")?.as_str()?).to_owned();
        value["id"] = json!(id);
        serde_json::from_value(value).ok()
    }
    pub fn restore_selection(&self, file: &FileDiff) -> Option<Selection> {
        if self.selection.is_some() {
            return self.selection;
        }
        let lines: Vec<_> = file.hunks.iter().flat_map(|hunk| &hunk.lines).collect();
        let (start, side) = point(lines.get(self.start_index)?);
        let (end, end_side) = point(lines.get(self.end_index)?);
        Some(Selection { start, side, end, end_side })
    }
}

impl Linked for ReviewComment {
    fn link_id(&self) -> String {
        self.context_id()
    }
}

pub fn referenced<'a>(prompt: &str, comments: &'a [ReviewComment]) -> Vec<&'a ReviewComment> {
    let mut found: Vec<&ReviewComment> = Vec::new();
    for reference in context_references(prompt).into_iter().filter(|reference| reference.kind == KIND) {
        if let Some(comment) = comments.iter().find(|comment| comment.context_id() == reference.context_id)
            && !found.iter().any(|seen| seen.id == comment.id)
        {
            found.push(comment);
        }
    }
    found
}

fn clamp_text(value: &str, max: usize) -> String {
    const MARKER: &str = "\n… truncated …";
    if value.encode_utf16().count() <= max {
        return value.into();
    }
    let units: Vec<_> = value.encode_utf16().take(max - MARKER.encode_utf16().count()).collect();
    format!("{}{MARKER}", String::from_utf16_lossy(&units))
}

pub fn format_fence(language: &str, contents: &str) -> String {
    let longest = contents.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    format!("{fence}{language}\n{}\n{fence}", contents.trim_end())
}

pub fn is_submit_shortcut(key: &str, meta: bool, ctrl: bool, value: &str, pending: bool) -> bool {
    !pending && (meta || ctrl) && key == "enter" && !value.trim().is_empty()
}

#[cfg(test)]
mod tests;
