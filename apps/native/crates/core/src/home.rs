//! The home route (`apps/web/src/routes/_chat.index.tsx`): which project a fresh draft opens in
//! (`sortScopedProjectsForSidebar` by `updated_at` in `components/Sidebar.logic.ts`), and the
//! scratch "No project" helpers (`isScratchProject` in `client-runtime/src/state/projects.ts`,
//! `availableScratchWorkspaceRoot` and `projects.ensureScratch` in `useScratchProject.ts`).

use serde_json::Value;

use crate::projects::find_project_by_path;
use crate::shell::{ProjectShell, ThreadShell};
use crate::sidebar::time::{Ms, parse_iso};

pub const PROJECTS_ENSURE_SCRATCH: &str = "projects.ensureScratch";

/// `getLatestUserMessageTimestamp`: shell threads carry no messages, so the latest user message
/// time, else updated, else created.
fn thread_activity(thread: &ThreadShell) -> Option<Ms> {
    thread
        .latest_user_message_at
        .as_deref()
        .and_then(parse_iso)
        .or_else(|| parse_iso(&thread.updated_at))
        .or_else(|| parse_iso(&thread.created_at))
}

/// The project the home page opens a draft in: the first unarchived project in the sidebar's
/// `updated_at` order (newest unarchived thread activity, else the project's own update time),
/// ties by title then id.
pub fn most_recent_project<'a>(projects: &'a [ProjectShell], threads: &[ThreadShell]) -> Option<&'a ProjectShell> {
    let activity = |project: &ProjectShell| {
        let latest = threads.iter().filter(|thread| thread.archived_at.is_none() && thread.project_id == project.id).map(thread_activity).max();
        match latest {
            Some(latest) => latest,
            None => parse_iso(&project.updated_at).or_else(|| parse_iso(&project.created_at)),
        }
    };
    projects
        .iter()
        .filter(|project| project.archived_at.is_none())
        .map(|project| (activity(project), project))
        .min_by(|(left_time, left), (right_time, right)| {
            right_time.cmp(left_time).then_with(|| crate::thread::locale_compare(&left.title, &right.title)).then_with(|| left.id.cmp(&right.id))
        })
        .map(|(_, project)| project)
}

/// `isScratchProject`: the project rooted at the server's scratch workspace.
pub fn is_scratch_project(workspace_root: &str, scratch_root: Option<&str>) -> bool {
    scratch_root.is_some_and(|root| find_project_by_path(&[workspace_root], root, |path| path).is_some())
}

/// The project `projects.ensureScratch` answered with.
pub fn ensure_scratch_project_id(reply: &Value) -> Option<String> {
    reply.get("projectId").and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn project(id: &str, title: &str, updated_at: &str, archived: bool) -> ProjectShell {
        serde_json::from_value(json!({
            "id": id, "title": title, "workspaceRoot": format!("/repo/{id}"),
            "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": updated_at,
            "archivedAt": if archived { json!("2026-01-02T00:00:00.000Z") } else { Value::Null },
        }))
        .unwrap()
    }

    fn thread(id: &str, project_id: &str, latest_user_message_at: Option<&str>, archived: bool) -> ThreadShell {
        serde_json::from_value(json!({
            "id": id, "projectId": project_id, "title": id, "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "runtimeMode": "full-access", "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "status": "idle", "latestUserMessageAt": latest_user_message_at, "hasActionableProposedPlan": false,
            "pendingRuntimeRequest": null, "latestVisibleMessage": null, "itemCount": 0, "visibleItemCount": 0,
            "settledOverride": null, "settledAt": null,
            "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
            "archivedAt": if archived { json!("2026-01-02T00:00:00.000Z") } else { Value::Null },
        }))
        .unwrap()
    }

    #[test]
    fn most_recent_project_follows_thread_activity_then_project_time() {
        let projects = vec![
            project("a", "Alpha", "2026-03-01T00:00:00.000Z", false),
            project("b", "Beta", "2026-01-05T00:00:00.000Z", false),
            project("c", "Gamma", "2026-04-01T00:00:00.000Z", true),
        ];
        // Beta's thread is newer than Alpha's own update; the archived project never wins.
        let threads = vec![thread("t1", "b", Some("2026-03-05T00:00:00.000Z"), false), thread("t2", "c", Some("2026-05-01T00:00:00.000Z"), false)];
        assert_eq!(most_recent_project(&projects, &threads).map(|p| p.id.as_str()), Some("b"));
        // Archived threads do not count, so Beta falls back to its own older update time.
        let archived = vec![thread("t1", "b", Some("2026-03-05T00:00:00.000Z"), true)];
        assert_eq!(most_recent_project(&projects, &archived).map(|p| p.id.as_str()), Some("a"));
        // A project with threads ranks by them even when they are older than its update time.
        let older = vec![thread("t1", "a", Some("2026-01-02T00:00:00.000Z"), false)];
        assert_eq!(most_recent_project(&projects, &older).map(|p| p.id.as_str()), Some("b"));
    }

    #[test]
    fn most_recent_project_breaks_ties_by_title_and_skips_archived() {
        let projects = vec![project("2", "beta", "2026-02-01T00:00:00.000Z", false), project("1", "Alpha", "2026-02-01T00:00:00.000Z", false)];
        assert_eq!(most_recent_project(&projects, &[]).map(|p| p.id.as_str()), Some("1"));
        assert!(most_recent_project(&[project("x", "X", "2026-02-01T00:00:00.000Z", true)], &[]).is_none());
    }

    #[test]
    fn scratch_project_matches_the_normalized_scratch_root() {
        assert!(is_scratch_project("/home/me/.t3/scratch/", Some("/home/me/.t3/scratch")));
        assert!(!is_scratch_project("/home/me/repo", Some("/home/me/.t3/scratch")));
        assert!(!is_scratch_project("/home/me/.t3/scratch", None));
        assert_eq!(ensure_scratch_project_id(&json!({"projectId": "p1"})).as_deref(), Some("p1"));
    }
}
