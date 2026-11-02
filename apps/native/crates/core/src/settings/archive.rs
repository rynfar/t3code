//! Archived threads: `orchestration.getArchivedShellSnapshot` (`OrchestrationV2ArchivedShellSnapshot`
//! in `contracts/src/orchestrationV2.ts`) grouped by project the way
//! `web/src/components/settings/ArchivedThreadsPanel.tsx` groups one environment. Only the fields
//! the panel shows decode; a project or thread that fails to decode is dropped alone.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::timeline::parse_iso_ms;

pub const GET_ARCHIVED_SHELL_SNAPSHOT: &str = "orchestration.getArchivedShellSnapshot";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedProject {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub created_at: String,
    #[serde(default)]
    pub archived_at: Option<String>,
}

impl ArchivedThread {
    /// When it was archived, falling back to its creation for rows archived before the field.
    pub fn archived_or_created(&self) -> &str {
        self.archived_at.as_deref().unwrap_or(&self.created_at)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArchiveGroup {
    pub project_id: String,
    pub title: String,
    pub threads: Vec<ArchivedThread>,
}

fn decode_each<T: for<'de> Deserialize<'de>>(value: Option<&Value>) -> Vec<T> {
    match value {
        Some(Value::Array(values)) => values.iter().filter_map(|value| serde_json::from_value(value.clone()).ok()).collect(),
        _ => Vec::new(),
    }
}

/// Projects in snapshot order with their archived threads, newest archive first; projects
/// without threads are left out.
pub fn groups(snapshot: &Value) -> Vec<ArchiveGroup> {
    let projects: Vec<ArchivedProject> = decode_each(snapshot.get("projects"));
    let mut by_project: HashMap<String, Vec<ArchivedThread>> = HashMap::new();
    for thread in decode_each::<ArchivedThread>(snapshot.get("threads")) {
        by_project.entry(thread.project_id.clone()).or_default().push(thread);
    }
    projects
        .into_iter()
        .filter_map(|project| {
            let mut project_threads = by_project.remove(&project.id)?;
            project_threads.sort_by(|left, right| {
                right.archived_or_created().cmp(left.archived_or_created()).then(right.id.cmp(&left.id))
            });
            Some(ArchiveGroup { project_id: project.id, title: project.title, threads: project_threads })
        })
        .collect()
}

/// One row of the virtualized archive list: a project heading or one of its threads, by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveEntry {
    Project(usize),
    Thread(usize, usize),
}

pub fn entries(groups: &[ArchiveGroup]) -> Vec<ArchiveEntry> {
    groups
        .iter()
        .enumerate()
        .flat_map(|(group, project)| {
            std::iter::once(ArchiveEntry::Project(group)).chain((0..project.threads.len()).map(move |thread| ArchiveEntry::Thread(group, thread)))
        })
        .collect()
}

/// Drops a thread after it is unarchived or deleted, and its group when that empties it.
pub fn remove_thread(groups: &mut Vec<ArchiveGroup>, thread_id: &str) {
    for group in groups.iter_mut() {
        group.threads.retain(|thread| thread.id != thread_id);
    }
    groups.retain(|group| !group.threads.is_empty());
}

/// `formatRelativeTimeLabel`: "just now", then "5m ago", "3h ago", "2d ago".
pub fn relative_label(iso: &str, now_ms: i64) -> String {
    let Some(at) = parse_iso_ms(iso) else { return String::new() };
    let seconds = (now_ms - at).max(0) / 1000;
    if seconds < 60 {
        return "just now".to_owned();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// The row description: "Archived 2d ago · Created 5d ago".
pub fn description(thread: &ArchivedThread, now_ms: i64) -> String {
    format!(
        "Archived {} \u{00b7} Created {}",
        relative_label(thread.archived_or_created(), now_ms),
        relative_label(&thread.created_at, now_ms)
    )
}

/// The confirm prompt `confirmAndDeleteThread` shows.
pub fn delete_prompt(title: &str) -> (String, &'static str) {
    (format!("Delete thread \"{title}\"?"), "This permanently clears conversation history for this thread.")
}
