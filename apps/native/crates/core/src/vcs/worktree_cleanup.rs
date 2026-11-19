//! Whether deleting a thread may offer to delete its worktree too
//! (`apps/web/src/worktreeCleanup.ts`). The display name is `worktree_display_name` in
//! `sidebar::thread`.

use crate::sidebar::grouping::normalize_project_path;

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
}
