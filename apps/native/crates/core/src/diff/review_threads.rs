//! Inline review threads and resolved-conversation avatars on diff lines (`pingdotgg/t3code#34644`),
//! porting `InlineReviewThreads` (`apps/web/src/components/pullRequest/PullRequestReviewAnnotation.tsx`)
//! and stacked context-line anchoring (`apps/web/src/components/pullRequest/PullRequestCodeTab.tsx`).

use std::collections::{HashMap, HashSet};

use crate::composer::review::Side;

use super::{FileDiff, Layout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewActor {
    pub login: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineReviewComment {
    pub id: String,
    pub author: Option<ReviewActor>,
    pub body: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineReviewThread {
    pub id: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub is_resolved: bool,
    pub comments: Vec<InlineReviewComment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAvatar {
    pub login: String,
    pub avatar_url: Option<String>,
    pub initial: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAvatarCluster {
    pub avatars: Vec<ResolvedAvatar>,
    pub resolved_count: usize,
    pub expanded: bool,
    pub label: String,
}

/// Maps a review thread anchor for `layout`: stacked view draws each context row once, so a
/// `Deletions` (`left`) anchor on a context line shares the `Additions` (`right`) line's group
/// (`PullRequestCodeTab.tsx:644-656`).
pub fn resolve_inline_review_anchor(file: &FileDiff, layout: Layout, side: Side, line: u32) -> (Side, u32) {
    if side == Side::Deletions && layout != Layout::Split {
        for hunk in file.hunks.iter() {
            for diff_line in &hunk.lines {
                if diff_line.old_line == Some(line)
                    && let Some(new_line) = diff_line.new_line
                {
                    return (Side::Additions, new_line);
                }
            }
        }
    }
    (side, line)
}

fn actor_initial(login: &str) -> String {
    login
        .trim()
        .chars()
        .next()
        .map(|ch| ch.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned())
}

/// Tracks open/resolved inline review conversations and which resolved ones are expanded.
#[derive(Debug, Clone, Default)]
pub struct InlineReviewThreadsState {
    threads: Vec<InlineReviewThread>,
    /// Conversations that have been seen open or explicitly toggled open by the reader.
    shown: HashSet<String>,
}

impl InlineReviewThreadsState {
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    pub fn threads(&self) -> &[InlineReviewThread] {
        &self.threads
    }

    /// Replaces the thread list while keeping conversations previously seen open shown even if
    /// they were just resolved (`InlineReviewThreads` in `#34644`).
    pub fn set_threads(&mut self, threads: Vec<InlineReviewThread>) {
        for thread in &threads {
            if !thread.is_resolved {
                self.shown.insert(thread.id.clone());
            }
        }
        self.threads = threads;
    }

    pub fn clear(&mut self) {
        self.threads.clear();
        self.shown.clear();
    }

    /// Matching threads for `(file, side, line)` after stacked context-line anchoring.
    pub fn threads_at<'a>(
        &'a self,
        file: &FileDiff,
        layout: Layout,
        side: Side,
        line: u32,
    ) -> Vec<&'a InlineReviewThread> {
        let path = file.path();
        self.threads
            .iter()
            .filter(|thread| {
                thread.path == path && resolve_inline_review_anchor(file, layout, thread.side, thread.line) == (side, line)
            })
            .collect()
    }

    /// Returns the folded/expanded resolved-conversation avatar cluster for a diff line, if any
    /// resolved conversations sit on `(file, side, line)`.
    pub fn resolved_cluster(
        &self,
        file: &FileDiff,
        layout: Layout,
        side: Side,
        line: u32,
    ) -> Option<ResolvedAvatarCluster> {
        let matching = self.threads_at(file, layout, side, line);
        let resolved: Vec<&InlineReviewThread> = matching.into_iter().filter(|t| t.is_resolved).collect();
        if resolved.is_empty() {
            return None;
        }
        let expanded = resolved.iter().all(|t| self.shown.contains(&t.id));
        let mut seen = HashMap::<String, Option<String>>::new();
        let mut order = Vec::<String>::new();
        for comment in resolved.iter().flat_map(|t| &t.comments) {
            let login = comment.author.as_ref().map(|a| a.login.clone()).unwrap_or_else(|| "ghost".to_owned());
            let avatar_url = comment.author.as_ref().and_then(|a| a.avatar_url.clone());
            if !seen.contains_key(&login) {
                order.push(login.clone());
            }
            seen.insert(login, avatar_url);
        }
        if order.is_empty() {
            order.push("ghost".to_owned());
            seen.insert("ghost".to_owned(), None);
        }
        let avatars: Vec<ResolvedAvatar> = order
            .into_iter()
            .take(3)
            .map(|login| {
                let avatar_url = seen.get(&login).cloned().flatten();
                let initial = actor_initial(&login);
                ResolvedAvatar { login, avatar_url, initial }
            })
            .collect();
        let resolved_count = resolved.len();
        let label = if resolved_count == 1 {
            "Resolved conversation".to_owned()
        } else {
            format!("{resolved_count} resolved conversations")
        };
        Some(ResolvedAvatarCluster { avatars, resolved_count, expanded, label })
    }

    /// Toggles all resolved conversations on `(file, side, line)`: clicking when collapsed expands
    /// them all; clicking when all are expanded folds the resolved ones while keeping open ones.
    pub fn toggle_resolved(&mut self, file: &FileDiff, layout: Layout, side: Side, line: u32) -> bool {
        let matching_ids: Vec<(String, bool)> = self
            .threads_at(file, layout, side, line)
            .into_iter()
            .map(|t| (t.id.clone(), t.is_resolved))
            .collect();
        let resolved_ids: Vec<&str> = matching_ids.iter().filter(|(_, r)| *r).map(|(id, _)| id.as_str()).collect();
        if resolved_ids.is_empty() {
            return false;
        }
        let all_expanded = resolved_ids.iter().all(|id| self.shown.contains(*id));
        if all_expanded {
            for id in resolved_ids {
                self.shown.remove(id);
            }
        } else {
            for (id, _) in &matching_ids {
                self.shown.insert(id.clone());
            }
        }
        true
    }

    /// Threads that should draw inline cards below `(file, side, line)`: every open thread plus
    /// every resolved thread currently in `shown`.
    pub fn visible_threads<'a>(
        &'a self,
        file: &FileDiff,
        layout: Layout,
        side: Side,
        line: u32,
    ) -> Vec<&'a InlineReviewThread> {
        self.threads_at(file, layout, side, line)
            .into_iter()
            .filter(|thread| !thread.is_resolved || self.shown.contains(&thread.id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{ParsedPatch, parse_patch};

    fn sample_file() -> FileDiff {
        let patch = "\
--- a/app.rs
+++ b/app.rs
@@ -10,3 +20,3 @@
 context_line
-old_line
+new_line
 trailing_context
";
        let ParsedPatch::Files(mut files) = parse_patch(patch) else { panic!("files") };
        files.remove(0)
    }

    fn thread(id: &str, side: Side, line: u32, resolved: bool, authors: &[&str]) -> InlineReviewThread {
        InlineReviewThread {
            id: id.into(),
            path: "app.rs".into(),
            side,
            line,
            is_resolved: resolved,
            comments: authors
                .iter()
                .enumerate()
                .map(|(i, login)| InlineReviewComment {
                    id: format!("{id}-c{i}"),
                    author: Some(ReviewActor { login: (*login).into(), avatar_url: None }),
                    body: format!("comment {i}"),
                    created_at: None,
                })
                .collect(),
        }
    }

    #[test]
    fn stacked_left_anchor_on_context_line_maps_to_new_side() {
        let file = sample_file();
        assert_eq!(resolve_inline_review_anchor(&file, Layout::Stacked, Side::Deletions, 10), (Side::Additions, 20));
        assert_eq!(resolve_inline_review_anchor(&file, Layout::Split, Side::Deletions, 10), (Side::Deletions, 10));
        assert_eq!(resolve_inline_review_anchor(&file, Layout::Stacked, Side::Deletions, 11), (Side::Deletions, 11));
    }

    #[test]
    fn resolved_threads_fold_into_max_three_avatars_and_toggle_cleanly() {
        let file = sample_file();
        let mut state = InlineReviewThreadsState::default();
        state.set_threads(vec![
            thread("t1", Side::Additions, 21, true, &["alice", "bob"]),
            thread("t2", Side::Additions, 21, true, &["carol", "dave"]),
        ]);
        assert!(state.visible_threads(&file, Layout::Stacked, Side::Additions, 21).is_empty());
        let cluster = state.resolved_cluster(&file, Layout::Stacked, Side::Additions, 21).expect("cluster");
        assert_eq!(cluster.resolved_count, 2);
        assert!(!cluster.expanded);
        assert_eq!(cluster.label, "2 resolved conversations");
        assert_eq!(cluster.avatars.len(), 3);
        assert_eq!(cluster.avatars.iter().map(|a| a.login.as_str()).collect::<Vec<_>>(), vec!["alice", "bob", "carol"]);

        assert!(state.toggle_resolved(&file, Layout::Stacked, Side::Additions, 21));
        assert_eq!(state.visible_threads(&file, Layout::Stacked, Side::Additions, 21).len(), 2);
        assert!(state.resolved_cluster(&file, Layout::Stacked, Side::Additions, 21).unwrap().expanded);

        assert!(state.toggle_resolved(&file, Layout::Stacked, Side::Additions, 21));
        assert!(state.visible_threads(&file, Layout::Stacked, Side::Additions, 21).is_empty());
    }

    #[test]
    fn conversation_seen_open_stays_shown_once_resolved() {
        let file = sample_file();
        let mut state = InlineReviewThreadsState::default();
        state.set_threads(vec![thread("t1", Side::Additions, 21, false, &["alice"])]);
        assert_eq!(state.visible_threads(&file, Layout::Stacked, Side::Additions, 21).len(), 1);

        state.set_threads(vec![thread("t1", Side::Additions, 21, true, &["alice"])]);
        assert_eq!(state.visible_threads(&file, Layout::Stacked, Side::Additions, 21).len(), 1);
        assert!(state.resolved_cluster(&file, Layout::Stacked, Side::Additions, 21).unwrap().expanded);
    }
}
