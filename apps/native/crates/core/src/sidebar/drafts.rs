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

fn row_with_terminal(session: DraftRef, draft: Option<&Draft>, terminal_label: Option<&str>) -> DraftRow {
    let key = session.key();
    let Some(draft) = draft else {
        return DraftRow {
            key,
            draft: session,
            preview: terminal_label.unwrap_or_default().to_owned(),
            created_at: 0,
        };
    };
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
        if count == 0
            && let Some(label) = terminal_label
        {
            label.to_owned()
        } else {
            format!("{count} attachment{}", if count == 1 { "" } else { "s" })
        }
    } else {
        preview
    };
    DraftRow { key, draft: session, preview, created_at: draft.created_at.unwrap_or(draft.updated_at) }
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
        self.update_draft_with_terminals(store, key, scope, &HashMap::new())
    }

    pub fn update_draft_with_terminals(
        &mut self,
        store: &DraftStore,
        key: &str,
        scope: Option<&HashSet<String>>,
        terminal_labels: &HashMap<String, String>,
    ) -> bool {
        let position = self.positions.get(key).copied();
        let held = position.map(|index| &self.rows[index]);
        let draft = store.get(key);
        let terminal_label = terminal_labels.get(key).map(String::as_str);
        let next = if self.active.as_deref() == Some(key) {
            let next = draft
                .and(self.frozen.as_ref())
                .cloned()
                .or_else(|| {
                    let label = terminal_label?;
                    let session = DraftRef::from_key(key)?;
                    if scope.is_some_and(|scope| !scope.contains(&session.project_id)) {
                        return None;
                    }
                    Some(row_with_terminal(session, draft, Some(label)))
                });
            if held == next.as_ref() {
                return false;
            }
            next
        } else {
            let next = DraftRef::from_key(key).and_then(|session| {
                if scope.is_some_and(|scope| !scope.contains(&session.project_id)) {
                    return None;
                }
                if let Some(draft) = draft.filter(|draft| draft.has_user_content() || terminal_label.is_some()) {
                    Some(row_with_terminal(session, Some(draft), terminal_label))
                } else {
                    terminal_label.map(|label| row_with_terminal(session, None, Some(label)))
                }
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
        self.rows.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.key.cmp(&b.key)));
        self.positions = self.rows.iter().enumerate().map(|(index, row)| (row.key.clone(), index)).collect();
    }

    pub fn update(&mut self, store: &DraftStore, active: Option<&str>, scope: Option<&HashSet<String>>) {
        self.update_with_terminals(store, active, scope, &HashMap::new());
    }

    /// Rebuilds listed draft rows, keeping script-only drafts with running/open terminals
    /// reachable in the sidebar (`pingdotgg/t3code#25272`).
    pub fn update_with_terminals(
        &mut self,
        store: &DraftStore,
        active: Option<&str>,
        scope: Option<&HashSet<String>>,
        terminal_labels: &HashMap<String, String>,
    ) {
        if self.active.as_deref() != active {
            self.active = active.map(str::to_owned);
            self.frozen = active.and_then(|key| {
                let session = DraftRef::from_key(key)?;
                store
                    .get(key)
                    .filter(|draft| draft.has_user_content())
                    .map(|draft| row_with_terminal(session, Some(draft), terminal_labels.get(key).map(String::as_str)))
            });
        }
        let mut seen = HashSet::new();
        let mut rows: Vec<DraftRow> = store
            .session_drafts()
            .filter_map(|(session, draft)| {
                let key = session.key();
                if scope.is_some_and(|scope| !scope.contains(&session.project_id)) {
                    return None;
                }
                seen.insert(key.clone());
                let terminal_label = terminal_labels.get(&key).map(String::as_str);
                if Some(key.as_str()) == active {
                    return self.frozen.clone().or_else(|| {
                        terminal_label.map(|label| row_with_terminal(session, Some(draft), Some(label)))
                    });
                }
                (draft.has_user_content() || terminal_label.is_some())
                    .then(|| row_with_terminal(session, Some(draft), terminal_label))
            })
            .collect();
        for (key, label) in terminal_labels {
            if seen.contains(key) {
                continue;
            }
            let Some(session) = DraftRef::from_key(key) else {
                continue;
            };
            if scope.is_some_and(|scope| !scope.contains(&session.project_id)) {
                continue;
            }
            if Some(key.as_str()) == active && let Some(frozen) = &self.frozen {
                rows.push(frozen.clone());
            } else {
                rows.push(row_with_terminal(session, None, Some(label)));
            }
        }
        self.rows = rows;
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
        assert_eq!(row_with_terminal(session("upload"), Some(&pending), None).preview, "1 attachment");
        assert_eq!(row_with_terminal(session("upload"), Some(&Draft { pending_attachment_count: 2, ..pending }), None).preview, "2 attachments");
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

    #[test]
    fn script_only_drafts_with_running_terminals_stay_listed_with_terminal_preview() {
        let mut store = DraftStore::default();
        let script = DraftRef::new(crate::composer::drafts::DraftId::new("script".into()), "env", "p1");
        let empty = DraftRef::new(crate::composer::drafts::DraftId::new("empty".into()), "env", "p1");
        store.set(&script.key(), Draft { runtime_mode: Some(crate::server_config::RuntimeMode::Auto), updated_at: 5, ..Default::default() });
        store.set(&empty.key(), Draft { runtime_mode: Some(crate::server_config::RuntimeMode::Auto), updated_at: 4, ..Default::default() });

        let orphan = DraftRef::new(crate::composer::drafts::DraftId::new("orphan".into()), "env", "p2");
        let terminals = HashMap::from([
            (script.key(), "Dev server".to_owned()),
            (orphan.key(), "Test runner".to_owned()),
        ]);

        let mut rows = DraftRows::default();
        rows.update_with_terminals(&store, Some(&script.key()), None, &terminals);
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.rows[0].draft, script);
        assert_eq!(rows.rows[0].preview, "Dev server");
        assert_eq!(rows.rows[1].draft, orphan);
        assert_eq!(rows.rows[1].preview, "Test runner");

        // Scoped filter keeps only matching project drafts.
        rows.update_with_terminals(&store, None, Some(&HashSet::from(["p1".into()])), &terminals);
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0].draft, script);
    }
}
