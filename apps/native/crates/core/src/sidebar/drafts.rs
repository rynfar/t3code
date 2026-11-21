//! Draft rows above the thread list (`web/src/components/sidebar/SidebarDraftBlock.tsx`).

use crate::composer::{
    drafts::{Draft, DraftRef, DraftStore},
    menu::context_references,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRow {
    pub draft: DraftRef,
    pub key: String,
    pub preview: String,
    pub created_at: i64,
}

fn row(session: DraftRef, draft: &Draft) -> DraftRow {
    let mut preview = String::new();
    let mut cursor = 0;
    for reference in context_references(&draft.prompt) {
        preview.push_str(&draft.prompt[cursor..reference.start]);
        preview.push_str(reference.label);
        cursor = reference.end;
    }
    preview.push_str(&draft.prompt[cursor..]);
    let preview = crate::assistant_citations::to_plain_text(&preview);
    let preview = preview.trim().split('\n').next().unwrap_or_default().to_owned();
    let preview = if preview.is_empty() {
        let count = draft.attachments.len() + draft.terminals.len() + draft.pending_attachment_count;
        format!("{count} attachment{}", if count == 1 { "" } else { "s" })
    } else {
        preview
    };
    DraftRow { key: session.key(), draft: session, preview, created_at: draft.created_at.unwrap_or(draft.updated_at) }
}

#[derive(Default)]
pub struct DraftRows {
    active: Option<String>,
    frozen: Option<DraftRow>,
    pub rows: Vec<DraftRow>,
    positions: HashMap<String, usize>,
}

impl DraftRows {
    pub fn update_draft(&mut self, store: &DraftStore, key: &str, scope: Option<&HashSet<String>>) -> bool {
        let position = self.positions.get(key).copied();
        let held = position.map(|index| &self.rows[index]);
        let draft = store.get(key);
        let next = if self.active.as_deref() == Some(key) {
            let next = draft.and(self.frozen.as_ref());
            if held.is_some() == next.is_some() {
                return false;
            }
            next.cloned()
        } else {
            let next = draft.filter(|draft| draft.has_user_content()).and_then(|draft| {
                let session = DraftRef::from_key(key)?;
                if scope.is_some_and(|scope| !scope.contains(&session.project_id)) {
                    return None;
                }
                Some(row(session, draft))
            });
            if held == next.as_ref() {
                return false;
            }
            next
        };
        match (position, next) {
            (Some(index), Some(next)) => self.rows[index] = next,
            (Some(index), None) => {
                self.rows.remove(index);
            }
            (None, Some(next)) => self.rows.push(next),
            (None, None) => return false,
        }
        self.index_rows();
        true
    }

    fn index_rows(&mut self) {
        self.rows.sort_by_key(|row| std::cmp::Reverse(row.created_at));
        self.positions = self.rows.iter().enumerate().map(|(index, row)| (row.key.clone(), index)).collect();
    }

    pub fn update(&mut self, store: &DraftStore, active: Option<&str>, scope: Option<&HashSet<String>>) {
        if self.active.as_deref() != active {
            self.active = active.map(str::to_owned);
            self.frozen = active.and_then(|key| {
                let session = DraftRef::from_key(key)?;
                store.get(key).filter(|draft| draft.has_user_content()).map(|draft| row(session, draft))
            });
        }
        self.rows = store
            .session_drafts()
            .filter_map(|(session, draft)| {
                if scope.is_some_and(|scope| !scope.contains(&session.project_id)) {
                    return None;
                }
                if Some(session.key().as_str()) == active {
                    return self.frozen.clone();
                }
                draft.has_user_content().then(|| row(session, draft))
            })
            .collect();
        self.index_rows();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(id: &str) -> DraftRef { DraftRef::legacy(id) }
    fn new_thread_key(id: &str) -> String { session(id).key() }
    #[test]
    fn rows_skip_empty_and_thread_drafts_follow_scope_and_freeze_the_open_row() {
        let mut store = DraftStore::default();
        let draft = |prompt: &str, at| Draft { prompt: prompt.into(), updated_at: at, ..Default::default() };
        store.set("thread:existing", draft("not a session", 1));
        store.set(&new_thread_key("empty"), Draft { runtime_mode: Some(crate::server_config::RuntimeMode::Auto), ..Default::default() });
        store.set(&new_thread_key("p1"), draft("first\nsecond line", 2));
        store.set(&new_thread_key("p2"), draft("see [Other](t3-context://v1/thread/thread_t2)", 3));
        store.set(
            &new_thread_key("p3"),
            draft(
                "[Assistant quote](t3-citation://v1/env/thread/message?suffix=&prefix=&end=11&start=0&text=hello%20world) review this",
                4,
            ),
        );
        let pending = Draft { pending_attachment_count: 1, updated_at: 1, ..Default::default() };
        assert_eq!(row(session("upload"), &pending).preview, "1 attachment");
        assert_eq!(row(session("upload"), &Draft { pending_attachment_count: 2, ..pending }).preview, "2 attachments");
        let mut rows = DraftRows::default();
        rows.update(&store, Some(&new_thread_key("empty")), None);
        assert_eq!(rows.rows.iter().map(|r| r.draft.project_id.as_str()).collect::<Vec<_>>(), ["p3", "p2", "p1"]);
        assert_eq!(rows.rows[0].preview, "hello world review this");
        assert_eq!(rows.rows[1].preview, "see Other");
        rows.update(&store, Some(&new_thread_key("p1")), None);
        assert_eq!(rows.rows[2].preview, "first");
        store.set(&new_thread_key("p1"), draft("changed", 9));
        let rows_ptr = rows.rows.as_ptr();
        let preview_ptr = rows.rows[2].preview.as_ptr();
        assert!(!rows.update_draft(&store, &new_thread_key("p1"), None));
        assert_eq!(rows.rows.as_ptr(), rows_ptr, "a frozen active edit must not rebuild the row vector");
        assert_eq!(rows.rows[2].preview.as_ptr(), preview_ptr, "a frozen preview must not be rebuilt");
        assert_eq!(rows.rows[2].preview, "first");
        store.remove(&new_thread_key("p1"));
        assert!(rows.update_draft(&store, &new_thread_key("p1"), None));
        assert_eq!(rows.rows.len(), 2);
        store.set(&new_thread_key("p1"), draft("changed", 2));
        assert!(rows.update_draft(&store, &new_thread_key("p1"), None));
        assert_eq!(rows.rows[2].preview, "first", "Undo restores the frozen preview");
        store.set(&new_thread_key("p2"), draft("inactive edit", 10));
        assert!(rows.update_draft(&store, &new_thread_key("p2"), None));
        assert_eq!(rows.rows[1].preview, "inactive edit");
        assert!(!rows.update_draft(&store, &new_thread_key("p2"), None));
        rows.update(&store, None, Some(&HashSet::from(["p1".into()])));
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0].preview, "changed");
        assert_eq!(rows.rows[0].created_at, 2);
        store.remove(&new_thread_key("p1"));
        rows.update(&store, None, None);
        assert_eq!(rows.rows.len(), 2);
    }
    #[test]
    fn same_project_drafts_have_independent_rows_and_active_previews() {
        let first = DraftRef::new(crate::composer::drafts::DraftId::new("first".into()), "env", "project");
        let second = DraftRef::new(crate::composer::drafts::DraftId::new("second".into()), "env", "project");
        let draft = |prompt: &str, at| Draft { prompt: prompt.into(), updated_at: at, ..Default::default() };
        let mut store = DraftStore::default();
        store.set(&first.key(), draft("first prompt", 1));
        store.set(&second.key(), draft("second prompt", 2));
        let mut rows = DraftRows::default();
        rows.update(&store, Some(&first.key()), Some(&HashSet::from(["project".into()])));
        assert_eq!(rows.rows.iter().map(|row| (&row.draft, row.preview.as_str())).collect::<Vec<_>>(),
            [(&second, "second prompt"), (&first, "first prompt")]);

        store.set(&first.key(), draft("still editing", 3));
        assert!(!rows.update_draft(&store, &first.key(), None));
        store.remove(&second.key());
        assert!(rows.update_draft(&store, &second.key(), None));
        assert_eq!(rows.rows[0].preview, "first prompt");
        assert_eq!(rows.rows.len(), 1);
    }

}
