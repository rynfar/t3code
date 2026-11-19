//! Whether deleting a thread may offer to delete its worktree too
//! (`apps/web/src/worktreeCleanup.ts`). The display name is `worktree_display_name` in
//! `sidebar::thread`.

use std::collections::HashSet;

use crate::server_config::ServerSettings;
use crate::sidebar::grouping::normalize_project_path;
use crate::sidebar::thread::worktree_display_name;

/// `getOrphanedWorktreePathForThread`: the worktree only `thread_id` links to, trimmed. `threads`
/// are the surviving threads as `(id, worktreePath)`, including the one being deleted.
///
/// Threads can store one worktree as `/repo/wt` and `/repo/wt/`, and confirming deletion
/// force-removes the checkout for every thread on it, so paths compare normalized.
pub fn orphaned_worktree_path<'a>(threads: &[(&str, Option<&'a str>)], thread_id: &str) -> Option<&'a str> {
    let (_, worktree_path) = threads.iter().find(|(id, _)| *id == thread_id)?;
    let target = worktree_path.map(str::trim).filter(|path| !path.is_empty())?;
    let key = normalize_project_path(target);
    let shared = threads.iter().any(|(id, path)| *id != thread_id && path.is_some_and(|path| normalize_project_path(path) == key));
    (!shared).then_some(target)
}

/// `formatWorktreePathForDisplay` (`worktreeCleanup.ts`): the trailing segment, else the path.
pub fn format_worktree_path_for_display(path: &str) -> String {
    worktree_display_name(path).to_owned()
}

/// `resolveWorktreeCleanup(settings, projectId).worktreeOnDelete`: project override if present,
/// else environment policy. When true, the server cleans up the orphaned worktree automatically on
/// delete so the client does not ask.
pub fn automatic_worktree_cleanup_on_delete(settings: &ServerSettings, project_id: &str) -> bool {
    if let Some(override_entry) = settings.project_settings_overrides.get(project_id)
        && let Some(policy) = override_entry.raw.get("worktreeCleanup")
        && let Ok(parsed) = serde_json::from_value::<crate::settings::worktrees::WorktreeSettings>(serde_json::json!({ "worktreeCleanup": policy }))
        && parsed.worktree_cleanup.is_some()
    {
        return parsed.cleanup().worktree_on_delete;
    }
    settings.worktrees.cleanup().worktree_on_delete
}

/// Whether `orchestration.getWorktreeThreadIds` names any thread still using the worktree besides
/// `thread_id` and any bulk-deleted `doomed` threads (`useThreadActions.ts:524`).
pub fn has_other_worktree_occupants(occupant_ids: &[String], thread_id: &str, doomed: &HashSet<String>) -> bool {
    occupant_ids.iter().any(|id| id != thread_id && !doomed.contains(id))
}

/// Confirmation dialog body matching `useThreadActions.ts:557-562`.
pub fn worktree_delete_prompt_body(display_worktree_path: &str) -> String {
    format!("This thread is the only one linked to this worktree:\n{display_worktree_path}\n\nDelete the worktree too?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_another_thread_links_to_is_not_orphaned() {
        let threads = [("thread-1", Some("/tmp/repo/worktrees/feature-a")), ("thread-2", Some("/tmp/repo/worktrees/feature-a"))];
        assert_eq!(orphaned_worktree_path(&threads, "thread-1"), None);
    }

    #[test]
    fn spellings_of_one_worktree_are_the_same_worktree() {
        for (target, other) in [
            ("/tmp/repo/worktrees/feature-a", "/tmp/repo/worktrees/feature-a/"),
            ("/tmp/repo/worktrees/feature-a", "/tmp/repo/worktrees/feature-a//"),
            ("C:\\repo\\worktrees\\feature-a", "c:/repo/worktrees/feature-a/"),
        ] {
            let threads = [("thread-1", Some(target)), ("thread-2", Some(other))];
            assert_eq!(orphaned_worktree_path(&threads, "thread-1"), None, "{target} vs {other}");
            assert_eq!(orphaned_worktree_path(&threads, "thread-2"), None, "{other} vs {target}");
        }
    }

    #[test]
    fn case_sensitive_posix_worktrees_stay_apart() {
        let threads = [("thread-1", Some("/tmp/repo/worktrees/Feature")), ("thread-2", Some("/tmp/repo/worktrees/feature"))];
        assert_eq!(orphaned_worktree_path(&threads, "thread-1"), Some("/tmp/repo/worktrees/Feature"));
    }

    #[test]
    fn threads_on_other_worktrees_or_none_leave_it_orphaned() {
        let threads = [("thread-1", Some(" /tmp/repo/worktrees/feature-a ")), ("thread-2", Some("/tmp/repo/worktrees/feature-b")), ("thread-3", None)];
        assert_eq!(orphaned_worktree_path(&threads, "thread-1"), Some("/tmp/repo/worktrees/feature-a"));
        assert_eq!(orphaned_worktree_path(&threads, "thread-3"), None);
        assert_eq!(orphaned_worktree_path(&threads, "missing"), None);
        assert_eq!(orphaned_worktree_path(&[("thread-1", Some("  "))], "thread-1"), None);
    }

    #[test]
    fn automatic_cleanup_honors_project_override_and_occupant_filter() {
        let mut settings: ServerSettings = serde_json::from_value(serde_json::json!({
            "storageCleanup": { "worktreeOnDelete": true },
            "projectSettingsOverrides": {
                "proj-ask": { "worktreeCleanup": { "mode": "custom", "rules": { "worktreeOnDelete": false } } },
                "proj-off": { "worktreeCleanup": { "mode": "off" } },
                "proj-inherit": { "worktreeCleanup": { "mode": "inherit" } }
            }
        }))
        .unwrap();
        assert!(automatic_worktree_cleanup_on_delete(&settings, "proj-default"));
        assert!(automatic_worktree_cleanup_on_delete(&settings, "proj-inherit"));
        assert!(!automatic_worktree_cleanup_on_delete(&settings, "proj-ask"));
        assert!(!automatic_worktree_cleanup_on_delete(&settings, "proj-off"));
        settings.worktrees = serde_json::from_value(serde_json::json!({ "storageCleanup": { "worktreeOnDelete": false } })).unwrap();
        assert!(!automatic_worktree_cleanup_on_delete(&settings, "proj-default"));

        let doomed: HashSet<String> = ["t-2".into()].into_iter().collect();
        assert!(!has_other_worktree_occupants(&["t-1".into(), "t-2".into()], "t-1", &doomed));
        assert!(has_other_worktree_occupants(&["t-1".into(), "t-3".into()], "t-1", &doomed));
        assert_eq!(
            worktree_delete_prompt_body("feature-a"),
            "This thread is the only one linked to this worktree:\nfeature-a\n\nDelete the worktree too?"
        );
    }
}
