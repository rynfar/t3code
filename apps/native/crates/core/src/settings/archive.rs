//! Archived threads: `orchestration.getArchivedShellSnapshot` (`OrchestrationV2ArchivedShellSnapshot`
//! in `contracts/src/orchestrationV2.ts`) grouped by project the way
//! `web/src/components/settings/ArchivedThreadsPanel.tsx` groups one environment. Only the fields
//! the panel shows decode; a project or thread that fails to decode is dropped alone.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::shell::Lineage;
use crate::sidebar::tag_filter::ThreadKey;
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
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub lineage: Option<Lineage>,
    #[serde(default)]
    pub delegated_from: Option<Value>,
}

/// Parent-aware tag visibility inputs for one archived snapshot, matching
/// `packages/client-runtime/src/state/archivedThreads.ts` and `tag_filter::hidden_thread_keys`.
pub struct ArchiveVisibility<'a> {
    pub environment_id: &'a str,
    pub hidden_tags: Option<&'a [String]>,
    pub already_hidden: &'a HashSet<ThreadKey>,
    pub loaded_elsewhere: &'a HashSet<ThreadKey>,
    pub hiding_anywhere: bool,
}

fn visibility_parent(environment_id: &str, thread: &ArchivedThread) -> Option<ThreadKey> {
    if let Some(lineage) = &thread.lineage
        && !lineage.is_listed()
        && let Some(parent) = &lineage.parent_thread_id
    {
        return Some(ThreadKey::new(environment_id, parent));
    }
    let parent = thread.delegated_from.as_ref()?;
    Some(ThreadKey::new(parent.get("environmentId")?.as_str()?, parent.get("threadId")?.as_str()?))
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
    groups_filtered(snapshot, None)
}

/// Projects in snapshot order with their archived threads, omitting threads hidden directly by
/// tag or via an unlisted/delegated parent (`archivedThreads.ts` / `tag_filter::hidden_thread_keys`).
pub fn groups_filtered(snapshot: &Value, visibility: Option<&ArchiveVisibility<'_>>) -> Vec<ArchiveGroup> {
    let projects: Vec<ArchivedProject> = decode_each(snapshot.get("projects"));
    let threads: Vec<ArchivedThread> = decode_each(snapshot.get("threads"));
    let hidden_ids: HashSet<String> = match visibility {
        Some(vis) if !vis.already_hidden.is_empty() || vis.hiding_anywhere => {
            let entries: HashMap<ThreadKey, &ArchivedThread> =
                threads.iter().map(|t| (ThreadKey::new(vis.environment_id, &t.id), t)).collect();
            fn is_hidden(
                key: &ThreadKey,
                entries: &HashMap<ThreadKey, &ArchivedThread>,
                known: &mut HashMap<ThreadKey, bool>,
                vis: &ArchiveVisibility<'_>,
            ) -> bool {
                if vis.already_hidden.contains(key) {
                    return true;
                }
                if let Some(hidden) = known.get(key) {
                    return *hidden;
                }
                let Some(thread) = entries.get(key).copied() else {
                    return false;
                };
                let parent = visibility_parent(vis.environment_id, thread);
                let follows = parent.as_ref().is_some_and(|parent| entries.contains_key(parent) || vis.already_hidden.contains(parent));
                let hidden = if follows {
                    known.insert(key.clone(), false);
                    is_hidden(parent.as_ref().unwrap(), entries, known, vis)
                } else if parent.as_ref().is_some_and(|parent| vis.loaded_elsewhere.contains(parent)) {
                    false
                } else if parent.is_some() && vis.hiding_anywhere {
                    true
                } else {
                    vis.hidden_tags.is_some_and(|tags| tags.iter().any(|tag| thread.tags.contains(tag)))
                };
                known.insert(key.clone(), hidden);
                hidden
            }
            let mut known = HashMap::new();
            entries
                .keys()
                .filter(|key| is_hidden(key, &entries, &mut known, vis))
                .map(|key| key.thread_id.clone())
                .collect()
        }
        _ => HashSet::new(),
    };
    let mut by_project: HashMap<String, Vec<ArchivedThread>> = HashMap::new();
    for thread in threads {
        if hidden_ids.contains(&thread.id) {
            continue;
        }
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

#[cfg(test)]
mod filter_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn groups_filtered_follows_contract_lineage_delegation_and_live_hidden_parents() {
        let snapshot = json!({
            "projects": [{ "id": "p1", "title": "Repo" }],
            "threads": [
                {
                    "id": "t-visible",
                    "projectId": "p1",
                    "title": "Visible",
                    "createdAt": "2026-01-01T00:00:00Z",
                    "tags": ["feature"],
                    "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t-visible", "sideChat": false }
                },
                {
                    "id": "t-hidden",
                    "projectId": "p1",
                    "title": "Secret",
                    "createdAt": "2026-01-02T00:00:00Z",
                    "tags": ["private"],
                    "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t-hidden", "sideChat": false }
                },
                {
                    "id": "t-listed-fork",
                    "projectId": "p1",
                    "title": "Listed fork of hidden parent",
                    "createdAt": "2026-01-03T00:00:00Z",
                    "tags": [],
                    "lineage": { "parentThreadId": "t-hidden", "relationshipToParent": "fork", "rootThreadId": "t-hidden", "sideChat": false }
                },
                {
                    "id": "t-subagent",
                    "projectId": "p1",
                    "title": "Unlisted subagent of hidden archived parent",
                    "createdAt": "2026-01-04T00:00:00Z",
                    "tags": [],
                    "lineage": { "parentThreadId": "t-hidden", "relationshipToParent": "subagent", "rootThreadId": "t-hidden", "sideChat": false }
                },
                {
                    "id": "t-live-sidechat",
                    "projectId": "p1",
                    "title": "Side chat of hidden live parent",
                    "createdAt": "2026-01-05T00:00:00Z",
                    "tags": [],
                    "lineage": { "parentThreadId": "live-hidden", "relationshipToParent": null, "rootThreadId": "live-hidden", "sideChat": true }
                },
                {
                    "id": "t-delegated-hidden",
                    "projectId": "p1",
                    "title": "Delegated from hidden live parent",
                    "createdAt": "2026-01-06T00:00:00Z",
                    "tags": [],
                    "delegatedFrom": { "environmentId": "env-a", "threadId": "live-hidden" }
                },
                {
                    "id": "t-delegated-visible",
                    "projectId": "p1",
                    "title": "Delegated from visible live parent",
                    "createdAt": "2026-01-07T00:00:00Z",
                    "tags": [],
                    "delegatedFrom": { "environmentId": "env-a", "threadId": "live-visible" }
                },
                {
                    "id": "t-delegated-orphan",
                    "projectId": "p1",
                    "title": "Delegated from unloaded parent",
                    "createdAt": "2026-01-08T00:00:00Z",
                    "tags": [],
                    "delegatedFrom": { "environmentId": "env-b", "threadId": "missing-parent" }
                }
            ]
        });
        let unfiltered = groups(&snapshot);
        assert_eq!(unfiltered[0].threads.len(), 8);

        let tags = vec!["private".to_owned()];
        let already_hidden = HashSet::from([ThreadKey::new("env-a", "live-hidden")]);
        let loaded_elsewhere = HashSet::from([ThreadKey::new("env-a", "live-visible")]);
        let filtered = groups_filtered(
            &snapshot,
            Some(&ArchiveVisibility {
                environment_id: "env-a",
                hidden_tags: Some(&tags),
                already_hidden: &already_hidden,
                loaded_elsewhere: &loaded_elsewhere,
                hiding_anywhere: true,
            }),
        );
        let ids: Vec<&str> = filtered[0].threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["t-delegated-visible", "t-listed-fork", "t-visible"]);
    }
}
