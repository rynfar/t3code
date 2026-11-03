//! Draft rows above the thread list (`web/src/components/sidebar/SidebarDraftBlock.tsx`).

use crate::composer::{
    drafts::{Draft, DraftStore},
    menu::context_references,
};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRow {
    pub project_id: String,
    pub preview: String,
    pub created_at: i64,
}

fn row(project_id: &str, draft: &Draft) -> DraftRow {
    let mut preview = String::new();
    let mut cursor = 0;
    for reference in context_references(&draft.prompt) {
        preview.push_str(&draft.prompt[cursor..reference.start]);
        preview.push_str(reference.label);
        cursor = reference.end;
    }
    preview.push_str(&draft.prompt[cursor..]);
    let preview = preview.trim().split('\n').next().unwrap_or_default().to_owned();
    let preview = if preview.is_empty() {
        let count = draft.attachments.len() + draft.terminals.len() + draft.pending_attachment_count;
        format!("{count} attachment{}", if count == 1 { "" } else { "s" })
    } else {
        preview
    };
    DraftRow { project_id: project_id.into(), preview, created_at: draft.created_at.unwrap_or(draft.updated_at) }
}

#[derive(Default)]
pub struct DraftRows {
    active: Option<String>,
    frozen: Option<DraftRow>,
    pub rows: Vec<DraftRow>,
}

impl DraftRows {
    pub fn update(&mut self, store: &DraftStore, active: Option<&str>, scope: Option<&HashSet<String>>) {
        if self.active.as_deref() != active {
            self.active = active.map(str::to_owned);
            self.frozen = active.and_then(|project| {
                store.get(&crate::composer::drafts::new_thread_key(project)).filter(|draft| draft.has_user_content()).map(|draft| row(project, draft))
            });
        }
        self.rows = store
            .keys()
            .filter_map(|key| {
                let project = key.strip_prefix("draft:")?;
                if scope.is_some_and(|scope| !scope.contains(project)) {
                    return None;
                }
                let draft = store.get(key)?;
                if Some(project) == active {
                    return self.frozen.clone();
                }
                draft.has_user_content().then(|| row(project, draft))
            })
            .collect();
        self.rows.sort_by_key(|row| std::cmp::Reverse(row.created_at));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::drafts::new_thread_key;
    #[test]
    fn rows_skip_empty_and_thread_drafts_follow_scope_and_freeze_the_open_row() {
        let mut store = DraftStore::default();
        let draft = |prompt: &str, at| Draft { prompt: prompt.into(), updated_at: at, ..Default::default() };
        store.set("thread:existing", draft("not a session", 1));
        store.set(&new_thread_key("empty"), Draft { runtime_mode: Some(crate::server_config::RuntimeMode::Auto), ..Default::default() });
        store.set(&new_thread_key("p1"), draft("first\nsecond line", 2));
        store.set(&new_thread_key("p2"), draft("see [Other](t3-context://v1/thread/thread_t2)", 3));
        let pending = Draft { pending_attachment_count: 1, updated_at: 1, ..Default::default() };
        assert_eq!(row("upload", &pending).preview, "1 attachment");
        assert_eq!(row("upload", &Draft { pending_attachment_count: 2, ..pending }).preview, "2 attachments");
        let mut rows = DraftRows::default();
        rows.update(&store, Some("empty"), None);
        assert_eq!(rows.rows.iter().map(|r| r.project_id.as_str()).collect::<Vec<_>>(), ["p2", "p1"]);
        assert_eq!(rows.rows[0].preview, "see Other");
        rows.update(&store, Some("p1"), None);
        assert_eq!(rows.rows[1].preview, "first");
        store.set(&new_thread_key("p1"), draft("changed", 9));
        rows.update(&store, Some("p1"), None);
        assert_eq!(rows.rows[1].preview, "first");
        rows.update(&store, None, Some(&HashSet::from(["p1".into()])));
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0].preview, "changed");
        assert_eq!(rows.rows[0].created_at, 2);
        store.remove(&new_thread_key("p1"));
        rows.update(&store, None, None);
        assert_eq!(rows.rows.len(), 1);
    }
}
