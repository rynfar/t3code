//! Ref refresh I/O for `client-runtime/src/state/vcs.ts` and the ref-affecting commands'
//! `onSettled` hooks in `state/vcs.ts`, `state/git.ts`, `state/sourceControl.ts`, and the
//! stacked-action finalizer in `state/vcsAction.ts`.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use t3_core::rpc::RpcFailure;
use t3_core::vcs::{ListRefsResult, VcsBranchStatus, VcsStatusEvent, branch_fallback_subscription, branch_status_from_status_event, branch_subscription, method, refs::RefCache};
use tokio::sync::watch;

use crate::session::{RpcError, StreamHandle};
use crate::supervisor::ClientHandle;

#[derive(Clone)]
pub(crate) struct RefState {
    cache: Arc<Mutex<RefCache>>,
    revision: watch::Sender<u64>,
}

impl Default for RefState {
    fn default() -> Self {
        Self {
            cache: Arc::default(),
            revision: watch::channel(0).0,
        }
    }
}

impl RefState {
    fn lock(&self) -> MutexGuard<'_, RefCache> {
        self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn on_settled(&self, method: &'static str) -> Option<OnSettled> {
        t3_core::vcs::refs::invalidates_refs(method).then(|| OnSettled { state: self.clone(), method })
    }
}

pub(crate) struct OnSettled {
    state: RefState,
    method: &'static str,
}

impl Drop for OnSettled {
    fn drop(&mut self) {
        let mut cache = self.state.lock();
        if cache.settle(self.method) {
            self.state.revision.send_replace(cache.revision());
        }
    }
}

/// A `subscribeVcsBranch` stream that falls back to `subscribeVcsStatus` (`includeRemote: false`)
/// on servers without `capabilities.vcsBranchStatus` (`client-runtime/src/rpc/client.ts`).
pub struct BranchStream {
    client: ClientHandle,
    cwd: String,
    stream: StreamHandle,
    fallback: bool,
    allow_fallback: bool,
}

impl BranchStream {
    /// Returns `None` when the underlying stream ends (`RpcError::Closed` or server completion).
    pub async fn recv(&mut self) -> Option<Result<VcsBranchStatus, RpcError>> {
        loop {
            let item = self.stream.receiver.recv().await?;
            if self.fallback {
                match item {
                    Ok(value) => {
                        let Ok(event) = serde_json::from_value::<VcsStatusEvent>(value) else {
                            continue;
                        };
                        if let Some(status) = branch_status_from_status_event(event) {
                            return Some(Ok(status));
                        }
                    }
                    Err(error) => return Some(Err(error)),
                }
            } else {
                match item {
                    Ok(value) => match serde_json::from_value::<VcsBranchStatus>(value) {
                        Ok(status) if status.is_repo.is_some() || !self.allow_fallback => return Some(Ok(status)),
                        Ok(_) => {
                            if let Err(error) = self.switch_to_status_fallback().await {
                                return Some(Err(error));
                            }
                        }
                        Err(error) => return Some(Err(RpcError::Failure(RpcFailure { tag: None, message: error.to_string(), error: Value::Null }))),
                    },
                    Err(RpcError::Failure(_)) if self.allow_fallback => {
                        if let Err(error) = self.switch_to_status_fallback().await {
                            return Some(Err(error));
                        }
                    }
                    Err(error) => return Some(Err(error)),
                }
            }
        }
    }

    async fn switch_to_status_fallback(&mut self) -> Result<(), RpcError> {
        self.fallback = true;
        self.allow_fallback = false;
        self.stream = self.client.stream(method::SUBSCRIBE_VCS_STATUS, branch_fallback_subscription(&self.cwd)).await?;
        Ok(())
    }
}

impl ClientHandle {
    pub fn refs_revision(&self) -> watch::Receiver<u64> {
        self.refs.revision.subscribe()
    }

    /// `subscribeVcsBranch` (`client-runtime/src/rpc/client.ts`): subscribes to lightweight branch
    /// updates when `vcs_branch_status` is supported, or falls back to `subscribeVcsStatus` with
    /// `includeRemote: false` when `Some(false)` or when `None` and the server rejects the branch
    /// stream or omits `isRepo`.
    pub async fn open_branch_stream(&self, cwd: &str, vcs_branch_status: Option<bool>) -> Result<BranchStream, RpcError> {
        if vcs_branch_status == Some(false) {
            let stream = self.stream(method::SUBSCRIBE_VCS_STATUS, branch_fallback_subscription(cwd)).await?;
            return Ok(BranchStream {
                client: self.clone(),
                cwd: cwd.to_owned(),
                stream,
                fallback: true,
                allow_fallback: false,
            });
        }
        let allow_fallback = vcs_branch_status.is_none();
        match self.stream(method::SUBSCRIBE_VCS_BRANCH, branch_subscription(cwd)).await {
            Ok(stream) => Ok(BranchStream {
                client: self.clone(),
                cwd: cwd.to_owned(),
                stream,
                fallback: false,
                allow_fallback,
            }),
            Err(RpcError::Failure(_)) if allow_fallback => {
                let stream = self.stream(method::SUBSCRIBE_VCS_STATUS, branch_fallback_subscription(cwd)).await?;
                Ok(BranchStream {
                    client: self.clone(),
                    cwd: cwd.to_owned(),
                    stream,
                    fallback: true,
                    allow_fallback: false,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// `refreshVcsBranch` (`client-runtime/src/rpc/client.ts`): asks the server to recheck a
    /// checkout's ref, which `subscribeVcsBranch` then delivers. Older hosts without
    /// `capabilities.vcsBranchStatus` retain their existing status stream without per-row wake scans.
    pub async fn refresh_branch(&self, cwd: &str, vcs_branch_status: bool) -> Result<(), String> {
        if !vcs_branch_status {
            return Ok(());
        }
        self.request(method::VCS_REFRESH_BRANCH, branch_subscription(cwd)).await.map(drop).map_err(|error| error.to_string())
    }

    /// `None` means invalidation superseded this response; it must never reach the picker.
    pub async fn list_refs(&self, input: Value) -> Result<Option<ListRefsResult>, String> {
        let generation = *self.connection_generation().borrow();
        let request = self.refs.lock().begin(input);
        let result = self.request(method::VCS_LIST_REFS, request.input.clone()).await;
        if *self.connection_generation().borrow() != generation || self.refs.lock().revision() != request.revision() {
            return Ok(None);
        }
        let value = result.map_err(|error| error.to_string())?;
        let refs = serde_json::from_value::<ListRefsResult>(value).map_err(|error| error.to_string())?;
        Ok(self.refs.lock().commit(&request).then_some(refs))
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use serde_json::{Value, json};
    use t3_core::auth::AuthSessionState;
    use t3_core::rpc::RpcFailure;
    use t3_core::vcs::{VcsBranchStatus, method};

    use crate::session::{RpcError, StreamHandle};
    use crate::supervisor::ClientFixture;

    fn session() -> AuthSessionState {
        serde_json::from_value(json!({ "authenticated": true })).expect("session")
    }

    #[tokio::test]
    async fn refresh_branch_runs_only_when_capability_is_enabled() {
        let (client, mut fixture) = ClientFixture::handle(session());
        client.refresh_branch("/repo", false).await.unwrap();
        assert!(fixture.take_commands().is_empty());

        let refresh = tokio::spawn(async move { client.refresh_branch("/repo", true).await });
        let (rpc, payload, reply) = fixture.next_request().await;
        assert_eq!(rpc, method::VCS_REFRESH_BRANCH);
        assert_eq!(payload, json!({ "cwd": "/repo" }));
        let _ = reply.send(Ok(json!(null)));
        refresh.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn branch_stream_uses_status_fallback_when_unsupported_and_drops_remote_updates() {
        let (client, mut fixture) = ClientFixture::handle(session());
        let handle = tokio::spawn(async move { client.open_branch_stream("/repo", Some(false)).await });
        tokio::task::yield_now().await;
        let (_, mut streams) = fixture.take_requests();
        assert_eq!(streams.len(), 1);
        let (rpc, reply) = streams.pop().unwrap();
        assert_eq!(rpc, method::SUBSCRIBE_VCS_STATUS);
        let (stream_handle, tx) = StreamHandle::fixture();
        let _ = reply.send(Ok(stream_handle));

        let mut stream = handle.await.unwrap().unwrap();
        tx.send(Ok(json!({ "_tag": "remoteUpdated", "remote": null }))).await.unwrap();
        tx.send(Ok(json!({ "_tag": "localUpdated", "local": { "isRepo": true, "refName": "feat/x" } }))).await.unwrap();
        assert_eq!(stream.recv().await.unwrap().unwrap(), VcsBranchStatus { ref_name: Some("feat/x".into()), is_repo: Some(true) });
    }

    #[tokio::test]
    async fn branch_stream_falls_back_when_server_rejects_subscribe_vcs_branch() {
        let (client, mut fixture) = ClientFixture::handle(session());
        let open = tokio::spawn(async move { client.open_branch_stream("/repo", None).await });
        tokio::task::yield_now().await;
        let (_, mut streams) = fixture.take_requests();
        assert_eq!(streams.len(), 1);
        let (rpc, reply) = streams.pop().unwrap();
        assert_eq!(rpc, method::SUBSCRIBE_VCS_BRANCH);
        let _ = reply.send(Err(RpcError::Failure(RpcFailure { tag: None, message: "unsupported".into(), error: Value::Null })));

        tokio::task::yield_now().await;
        let (_, mut streams) = fixture.take_requests();
        assert_eq!(streams.len(), 1);
        let (rpc, reply) = streams.pop().unwrap();
        assert_eq!(rpc, method::SUBSCRIBE_VCS_STATUS);
        let (stream_handle, tx) = StreamHandle::fixture();
        let _ = reply.send(Ok(stream_handle));

        let mut stream = open.await.unwrap().unwrap();
        tx.send(Ok(json!({ "_tag": "snapshot", "local": { "isRepo": false, "refName": null }, "remote": null }))).await.unwrap();
        assert_eq!(stream.recv().await.unwrap().unwrap(), VcsBranchStatus { ref_name: None, is_repo: Some(false) });
    }

    #[tokio::test]
    async fn branch_stream_falls_back_when_is_repo_is_omitted() {
        let (client, mut fixture) = ClientFixture::handle(session());
        let open = tokio::spawn(async move { client.open_branch_stream("/repo", None).await });
        tokio::task::yield_now().await;
        let (_, mut streams) = fixture.take_requests();
        assert_eq!(streams.len(), 1);
        let (rpc, reply) = streams.pop().unwrap();
        assert_eq!(rpc, method::SUBSCRIBE_VCS_BRANCH);
        let (branch_handle, branch_tx) = StreamHandle::fixture();
        let _ = reply.send(Ok(branch_handle));

        let mut stream = open.await.unwrap().unwrap();
        branch_tx.send(Ok(json!({ "refName": "main" }))).await.unwrap();
        let recv = tokio::spawn(async move {
            let item = stream.recv().await;
            (item, stream)
        });
        tokio::task::yield_now().await;
        let (_, mut streams) = fixture.take_requests();
        assert_eq!(streams.len(), 1);
        let (rpc, reply) = streams.pop().unwrap();
        assert_eq!(rpc, method::SUBSCRIBE_VCS_STATUS);
        let (status_handle, status_tx) = StreamHandle::fixture();
        let _ = reply.send(Ok(status_handle));
        status_tx.send(Ok(json!({ "_tag": "snapshot", "local": { "isRepo": true, "refName": "main" }, "remote": null }))).await.unwrap();

        let (item, _stream) = recv.await.unwrap();
        assert_eq!(item.unwrap().unwrap(), VcsBranchStatus { ref_name: Some("main".into()), is_repo: Some(true) });
    }
}
