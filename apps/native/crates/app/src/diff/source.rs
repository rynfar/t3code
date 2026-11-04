//! Where a scope's patch comes from (`useCheckpointDiff`, the `review.getDiffPreview` query and
//! `useTurnItemDetail` in `components/DiffPanel.tsx`): the request identity, and the fetch that
//! runs and parses it on the network runtime.

use std::sync::Arc;

use t3_core::diff::{
    PreviewSourceKind, ToolCallDiff, ToolCallTarget, checkpoint_diff_request, decode_turn_diff,
    hide_whitespace_changes, tool_call_diff, ReviewModel,
};
use t3_core::timeline::{GET_TURN_ITEM, decode_detail};

use super::view::Parsed;
mod worker;
pub use worker::{Command, run};
#[cfg(feature = "snapshot")]
pub(super) use worker::run_with;
use crate::app_state::AppState;

/// A patch's identity. The environment is part of it, so a cache never serves one server's
/// patches after pairing with another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKey {
    /// Only the selected Git scope is requested. `revision` bumps on refresh and when
    /// the agent may have changed the workspace.
    Git { kind: PreviewSourceKind, environment: String, cwd: String, base_ref: Option<String>, ignore_whitespace: bool, revision: u64 },
    Turn { environment: String, thread_id: String, turn_count: u64, ignore_whitespace: bool },
    /// `orchestration.getTurnItem`; whitespace is hidden client-side, so it is not part of the key.
    ToolCall { environment: String, target: ToolCallTarget },
}

impl RequestKey {
    /// A refresh of the same scope keeps showing the old patch until the new one lands.
    pub fn same_source(&self, other: &RequestKey) -> bool {
        match (self, other) {
            (
                RequestKey::Git { kind, environment, cwd, base_ref, ignore_whitespace, .. },
                RequestKey::Git { kind: k, environment: e, cwd: c, base_ref: b, ignore_whitespace: w, .. },
            ) => kind == k && environment == e && cwd == c && base_ref == b && ignore_whitespace == w,
            (RequestKey::ToolCall { environment, target }, RequestKey::ToolCall { environment: e, target: t }) => {
                environment == e && target.thread_id == t.thread_id && target.item_id == t.item_id
            }
            _ => self == other,
        }
    }
}

/// Whether `request` still has to be fetched: only an exact answer counts. An older revision of
/// the same source stays on screen meanwhile (`same_source`), so a request made while the panel
/// was hidden is fetched once it shows.
pub fn needs_fetch(request: &RequestKey, loaded: Option<&RequestKey>) -> bool {
    loaded != Some(request)
}

#[derive(Clone)]
pub enum Loaded {
    /// `repository` is false when the server found no VCS at the cwd (`sources: []`).
    Git { parsed: Arc<Parsed>, review: Arc<ReviewModel>, repository: bool },
    Turn(Arc<Parsed>),
    ToolCall(Arc<ToolCall>),
}

/// A fetched tool call: its patch as recorded and with whitespace-only changes hidden, both
/// parsed here so the whitespace toggle only swaps them. No patch when the item is gone, is not
/// a file change, or recorded none.
pub struct ToolCall {
    pub exact: Option<Arc<Parsed>>,
    pub ignoring_whitespace: Option<Arc<Parsed>>,
    /// `toolCallDiff` without its patch.
    pub diff: ToolCallDiff,
}

impl ToolCall {
    pub fn parsed(&self, ignore_whitespace: bool) -> Option<Arc<Parsed>> {
        if ignore_whitespace { self.ignoring_whitespace.clone() } else { self.exact.clone() }
    }
}

pub fn fetch(state: &AppState, key: &RequestKey) -> Option<tokio::task::JoinHandle<Result<Loaded, String>>> {
    if !state.can_read_threads() { return None; }
    match key {
        RequestKey::Git { .. } => None,
        RequestKey::Turn { thread_id, turn_count, ignore_whitespace, .. } => {
            let (method, payload) = checkpoint_diff_request(thread_id, *turn_count, *ignore_whitespace);
            state.request(method, payload, |value| {
                decode_turn_diff(&value).map(|patch| Loaded::Turn(Parsed::new(&patch, false, None, None, None)))
            })
        }
        RequestKey::ToolCall { target, .. } if !target.source_available(state.thread_shell(&target.thread_id), state.thread_state(&target.thread_id)) => None,
        RequestKey::ToolCall { target, .. } => state.request(GET_TURN_ITEM, target.request(), |value| {
            let item = decode_detail(value).map_err(|error| error.to_string())?;
            let mut diff = item.and_then(|item| tool_call_diff(&item.kind)).unwrap_or_default();
            let exact = diff.patch.take().map(|patch| Parsed::new(&patch, diff.truncated, None, None, None));
            let ignoring_whitespace =
                exact.as_ref().map(|exact| Parsed::from_patch(hide_whitespace_changes(&exact.patch), exact.truncated, None, None, None));
            Ok(Loaded::ToolCall(Arc::new(ToolCall { exact, ignoring_whitespace, diff })))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_revision_shows_the_old_patch_but_is_still_fetched() {
        let tool_call = |revision: &str| RequestKey::ToolCall {
            environment: "e".into(),
            target: ToolCallTarget { thread_id: "t".into(), item_id: "f1".into(), revision: revision.into() },
        };
        let (live, settled) = (tool_call("live"), tool_call("2026-01-02T00:00:00Z"));
        assert!(settled.same_source(&live), "the running patch stays on screen while the settled one loads");
        assert!(needs_fetch(&settled, Some(&live)));
        assert!(!needs_fetch(&live, Some(&live)));
        assert!(needs_fetch(&live, None));
        let other_thread = RequestKey::ToolCall {
            environment: "e".into(),
            target: ToolCallTarget { thread_id: "parent".into(), item_id: "f1".into(), revision: "live".into() },
        };
        assert!(!live.same_source(&other_thread));
        assert!(needs_fetch(&live, Some(&other_thread)));
        let other_environment = RequestKey::ToolCall {
            environment: "other".into(),
            target: ToolCallTarget { thread_id: "t".into(), item_id: "f1".into(), revision: "live".into() },
        };
        assert!(!live.same_source(&other_environment));
        assert!(needs_fetch(&live, Some(&other_environment)));

        let git = |revision| RequestKey::Git { kind: PreviewSourceKind::BranchRange, environment: "e".into(), cwd: "/r".into(), base_ref: None, ignore_whitespace: true, revision };
        assert!(git(2).same_source(&git(1)) && needs_fetch(&git(2), Some(&git(1))));
        let RequestKey::Git { environment, cwd, base_ref, ignore_whitespace, revision, .. } = git(1) else { unreachable!() };
        let working = RequestKey::Git { kind: PreviewSourceKind::WorkingTree, environment, cwd, base_ref, ignore_whitespace, revision };
        assert!(!working.same_source(&git(1)), "working-tree metadata cannot satisfy a branch preview");
    }
}
