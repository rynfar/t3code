//! Pull request writes as `createPullRequestEnvironmentAtoms` runs them: one at a time in the
//! order they were asked for (`serialPerEnvironment`; this client holds one environment), a
//! merge's preparation inside its own turn, and one pending action per pull request that the
//! page's quick actions and the detail panel share (`usePullRequestActionRunner`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use gpui::{App, BorrowAppContext, Global};
use serde_json::{Value, json};
use t3_client::session::RpcError;
use t3_client::supervisor::ClientHandle;
use t3_core::pull_requests::detail::resolve_merge_method;
use t3_core::pull_requests::wire::{Action, Detail, MergeMethod, PrState, Stack, decode};
use t3_core::pull_requests::{PrRef, action_input, failure_message, method};
use tokio::sync::{mpsc, oneshot};

use super::{NOT_CONNECTED, Read};
use crate::app_state::AppState;

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(crate) struct Mutations {
    lane: mpsc::UnboundedSender<Job>,
    runtime: tokio::runtime::Handle,
    pending: HashMap<String, Action>,
}

impl Global for Mutations {}

pub(crate) fn init(runtime: tokio::runtime::Handle, cx: &mut App) {
    let (lane, mut jobs) = mpsc::unbounded_channel::<Job>();
    runtime.spawn(async move {
        while let Some(job) = jobs.recv().await {
            job.await;
        }
    });
    cx.set_global(Mutations { lane, runtime, pending: HashMap::new() });
}

/// The action pending on this pull request, from either surface.
pub(crate) fn pending(reference: &PrRef, cx: &App) -> Option<Action> {
    cx.try_global::<Mutations>()?.pending.get(&reference.key()).copied()
}

/// Claims the pull request for `action`; false while another action on it is pending.
pub(crate) fn begin(reference: &PrRef, action: Action, cx: &mut App) -> bool {
    if !cx.has_global::<Mutations>() || pending(reference, cx).is_some() {
        return false;
    }
    let key = reference.key();
    cx.update_global::<Mutations, _>(|mutations, _| {
        mutations.pending.insert(key, action);
    });
    true
}

pub(crate) fn finish(reference: &PrRef, cx: &mut App) {
    let key = reference.key();
    cx.update_global::<Mutations, _>(|mutations, _| {
        mutations.pending.remove(&key);
    });
}

/// Queues a write behind every one asked for before it.
fn queue<T: Send + 'static>(cx: &App, work: impl Future<Output = Result<T, String>> + Send + 'static) -> Read<T> {
    let mutations = cx.try_global::<Mutations>()?;
    let (reply, answer) = oneshot::channel();
    mutations
        .lane
        .send(Box::pin(async move {
            let _ = reply.send(work.await);
        }))
        .ok()?;
    Some(mutations.runtime.spawn(async move { answer.await.unwrap_or_else(|_| Err(NOT_CONNECTED.to_owned())) }))
}

async fn request(client: &ClientHandle, method: &'static str, payload: Value) -> Result<Value, String> {
    match client.request(method, payload).await {
        Ok(value) => Ok(value),
        Err(RpcError::Failure(failure)) => Err(failure_message(failure.tag.as_deref(), &failure.error, &failure.message)),
        Err(RpcError::Closed) => Err(NOT_CONNECTED.to_owned()),
    }
}

/// `pullRequests.runAction` with the input as built.
pub(crate) fn run_action(state: &AppState, input: Value, cx: &App) -> Read<()> {
    let client = state.client_handle()?;
    queue(cx, async move { request(&client, method::RUN_ACTION, input).await.map(|_| ()) })
}

/// The merge method picks in force when the merge was asked for.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MergePicks {
    pub current: Option<MergeMethod>,
    pub project_default: Option<MergeMethod>,
    pub last: Option<MergeMethod>,
}

/// A merge of one pull request (`runAction` with `resolveMergeMethod`): the detail is re-read
/// fresh and the merge refused unless the pull request is still open, ready, allowed, and in no
/// stack where the host runs stack actions; then the method is chosen.
pub(crate) fn merge(state: &AppState, reference: PrRef, picks: MergePicks, cx: &App) -> Read<()> {
    let client = state.client_handle()?;
    queue(cx, async move {
        let mut fresh = reference.payload();
        fresh["allowStale"] = json!(false);
        let detail: Detail = decode(request(&client, method::DETAIL, fresh.clone()).await?)?;
        if detail.state != PrState::Open || detail.is_draft || !detail.can(Action::Merge) {
            return Err("This pull request cannot be merged.".to_owned());
        }
        if detail.capabilities.stack_actions == Some(true) {
            let stack: Option<Stack> = decode(request(&client, method::STACK, fresh).await?)?;
            if stack.is_some() {
                return Err("Open this pull request to merge its stack.".to_owned());
            }
        }
        let allowed = detail.allowed_merge_methods();
        if allowed.is_empty() {
            return Err("No merge method is available for this repository.".to_owned());
        }
        let method = resolve_merge_method(&allowed, picks.current, picks.project_default, picks.last);
        request(&client, method::RUN_ACTION, action_input(&reference, Action::Merge, Some(method))).await.map(|_| ())
    })
}
