//! Where a scope's patch comes from (`useCheckpointDiff` and the `review.getDiffPreview` query in
//! `components/DiffPanel.tsx`): the request identity, and the fetch that runs and parses it on
//! the network runtime.

use std::sync::Arc;

use t3_core::diff::{
    PreviewSourceKind, checkpoint_diff_request, decode_diff_preview, decode_turn_diff, diff_preview_request,
    preview_is_repository,
};

use super::view::Parsed;
use crate::app_state::AppState;

/// A patch's identity. The environment is part of it, so a cache never serves one server's
/// patches after pairing with another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKey {
    /// `review.getDiffPreview` answers both git scopes. `revision` bumps on refresh and when
    /// the agent may have changed the workspace.
    Git { environment: String, cwd: String, base_ref: Option<String>, ignore_whitespace: bool, revision: u64 },
    Turn { environment: String, thread_id: String, turn_count: u64, ignore_whitespace: bool },
}

impl RequestKey {
    /// A refresh of the same scope keeps showing the old patch until the new one lands.
    pub fn same_source(&self, other: &RequestKey) -> bool {
        match (self, other) {
            (
                RequestKey::Git { environment, cwd, base_ref, ignore_whitespace, .. },
                RequestKey::Git { environment: e, cwd: c, base_ref: b, ignore_whitespace: w, .. },
            ) => environment == e && cwd == c && base_ref == b && ignore_whitespace == w,
            _ => self == other,
        }
    }
}

#[derive(Clone)]
pub enum Loaded {
    /// `repository` is false when the server found no VCS at the cwd (`sources: []`).
    Git { branch: Option<Arc<Parsed>>, unstaged: Option<Arc<Parsed>>, repository: bool },
    Turn(Arc<Parsed>),
}

pub fn fetch(state: &AppState, key: &RequestKey) -> Option<tokio::task::JoinHandle<Result<Loaded, String>>> {
    match key {
        RequestKey::Git { cwd, base_ref, ignore_whitespace, .. } => {
            let (method, payload) = diff_preview_request(cwd, base_ref.as_deref(), *ignore_whitespace);
            state.request(method, payload, |value| {
                let source = |kind| {
                    decode_diff_preview(&value, kind)
                        .map(|source| Parsed::new(&source.diff, source.truncated, source.totals, source.base_ref, source.head_ref))
                };
                Ok(Loaded::Git {
                    branch: source(PreviewSourceKind::BranchRange),
                    unstaged: source(PreviewSourceKind::WorkingTree),
                    repository: preview_is_repository(&value),
                })
            })
        }
        RequestKey::Turn { thread_id, turn_count, ignore_whitespace, .. } => {
            let (method, payload) = checkpoint_diff_request(thread_id, *turn_count, *ignore_whitespace);
            state.request(method, payload, |value| {
                decode_turn_diff(&value).map(|patch| Loaded::Turn(Parsed::new(&patch, false, None, None, None)))
            })
        }
    }
}
