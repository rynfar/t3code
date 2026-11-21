//! Metadata for the Changes row's saved branch comparison.

use gpui::{App, Context, Entity};
use t3_core::diff::{DiffPreviewSource, PreviewSourceKind, ReviewScope, decode_diff_preview};
use t3_core::vcs::BranchChanges;

use super::view::DetailsView;
use crate::app_state::AppState;
use crate::git::GitStore;

#[derive(Default)]
pub(crate) struct Comparison {
    key: Option<Key>,
    pub(crate) preview: Option<DiffPreviewSource>,
    pub(crate) pending: bool,
    generation: u64,
}

#[derive(PartialEq, Eq)]
struct Key {
    scope: ReviewScope,
    refs_revision: u64,
    branch: Option<String>,
    totals: (u64, u64),
    branch_changes: Option<BranchChanges>,
}

fn comparison_key(state: &AppState, git: &GitStore, cx: &App) -> Option<Key> {
    let client = state.client_handle()?;
    if !client
        .session_state()
        .borrow()
        .as_ref()
        .is_none_or(|session| session.grants(t3_core::media::FILESYSTEM_READ_SCOPE))
    {
        return None;
    }
    let status = &git.status.as_ref()?.local;
    if !status.is_repo {
        return None;
    }
    let thread = state.selected_thread.as_ref()?;
    let scope = ReviewScope {
        environment: state.environment()?.to_owned(),
        connection: state.connection_id()?,
        cwd: git.target.as_ref()?.cwd.clone(),
        kind: PreviewSourceKind::BranchRange,
        base_ref: state.panels.branch_base_refs.get(thread).cloned(),
        ignore_whitespace: crate::settings::client_settings(cx).diff_ignore_whitespace(),
    };
    Some(Key {
        scope,
        refs_revision: *client.refs_revision().borrow(),
        branch: status.ref_name.clone(),
        totals: (
            status.working_tree.insertions,
            status.working_tree.deletions,
        ),
        branch_changes: status.branch_changes.clone(),
    })
}

impl Comparison {
    pub(super) fn sync(
        &mut self,
        active: bool,
        state: &Entity<AppState>,
        git: &Entity<GitStore>,
        cx: &mut Context<DetailsView>,
    ) {
        let key = active
            .then(|| comparison_key(state.read(cx), git.read(cx), cx))
            .flatten();
        if self.key == key {
            return;
        }
        if !matches!((&self.key, &key), (Some(before), Some(after)) if before.scope == after.scope)
        {
            self.preview = None;
        }
        self.key = key;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let task = self.key.as_ref().and_then(|key| {
            let (method, payload) = key.scope.request(false);
            state.read(cx).request(method, payload, |value| {
                Ok(decode_diff_preview(&value, PreviewSourceKind::BranchRange))
            })
        });
        self.pending = task.is_some();
        if let Some(task) = task {
            cx.spawn(async move |this, cx| {
                let result = task.await.ok().and_then(Result::ok).flatten();
                let _ = this.update(cx, |this, cx| {
                    if this.comparison.generation == generation {
                        this.comparison.preview = result;
                        this.comparison.pending = false;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }
}
