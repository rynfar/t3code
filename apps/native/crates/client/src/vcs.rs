//! Ref refresh I/O for `client-runtime/src/state/vcs.ts` and the ref-affecting commands'
//! `onSettled` hooks in `state/vcs.ts`, `state/git.ts`, `state/sourceControl.ts`, and the
//! stacked-action finalizer in `state/vcsAction.ts`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use t3_core::rpc::RpcFailure;
use t3_core::vcs::{
    ListRefsResult, VcsBranchStatus, VcsStatusEvent, branch_fallback_subscription,
    branch_status_from_status_event, branch_subscription, method,
    refs::{RefCache, can_use_offline_refs_cache},
};
use tokio::sync::watch;

use crate::session::{RpcError, StreamHandle};
use crate::store;
use crate::supervisor::ClientHandle;

const MAX_CACHED_CWDS: usize = 64;

fn refs_cache_path_in(base: &std::path::Path, environment_key: &str) -> Option<PathBuf> {
    let key: String = environment_key
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') { ch } else { '_' })
        .take(96)
        .collect();
    if key.is_empty() {
        return None;
    }
    Some(base.join("vcs-refs").join(format!("{key}.json")))
}

fn refs_cache_path(environment_key: &str) -> Option<PathBuf> {
    refs_cache_path_in(&store::base()?, environment_key)
}

fn read_disk_cache_at(path: &std::path::Path) -> Option<BTreeMap<String, ListRefsResult>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn read_disk_cache(environment_key: &str) -> Option<BTreeMap<String, ListRefsResult>> {
    read_disk_cache_at(&refs_cache_path(environment_key)?)
}

fn write_disk_cache_at(path: &std::path::Path, cwd: &str, refs: &ListRefsResult) -> std::io::Result<()> {
    let mut map = read_disk_cache_at(path).unwrap_or_default();
    if !map.contains_key(cwd) && map.len() >= MAX_CACHED_CWDS {
        map.pop_first();
    }
    map.insert(cwd.to_owned(), refs.clone());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let payload = serde_json::to_vec(&map).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, payload)?;
    std::fs::rename(&tmp, path)
}

fn write_disk_cache(environment_key: &str, cwd: &str, refs: &ListRefsResult) -> std::io::Result<()> {
    let Some(path) = refs_cache_path(environment_key) else {
        return Ok(());
    };
    write_disk_cache_at(&path, cwd, refs)
}

fn clear_disk_cache_at(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn clear_disk_cache(environment_key: &str) -> std::io::Result<()> {
    let Some(path) = refs_cache_path(environment_key) else {
        return Ok(());
    };
    clear_disk_cache_at(&path)
}

#[derive(Clone)]
pub(crate) struct RefState {
    cache: Arc<Mutex<RefCache>>,
    persisted_env: Arc<Mutex<Option<String>>>,
    persistence_lock: Arc<Mutex<()>>,
    revision: watch::Sender<u64>,
}

impl Default for RefState {
    fn default() -> Self {
        Self {
            cache: Arc::default(),
            persisted_env: Arc::default(),
            persistence_lock: Arc::default(),
            revision: watch::channel(0).0,
        }
    }
}

impl RefState {
    fn lock(&self) -> MutexGuard<'_, RefCache> {
        self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_environment_key(&self, environment_key: &str) {
        let mut guard = self.persisted_env.lock().unwrap_or_else(|p| p.into_inner());
        if guard.as_deref() != Some(environment_key) {
            *guard = Some(environment_key.to_owned());
        }
    }

    fn environment_key(&self) -> Option<String> {
        self.persisted_env.lock().unwrap_or_else(|p| p.into_inner()).clone()
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
        let _persist_guard = self.state.persistence_lock.lock().unwrap_or_else(|p| p.into_inner());
        let env_key = self.state.environment_key();
        let readable = match env_key.as_deref() {
            Some(key) => clear_disk_cache(key).is_ok(),
            None => true,
        };
        let mut cache = self.state.lock();
        if cache.settle(self.method) {
            cache.set_persisted_cache_readable(readable);
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

    /// Reads the persisted offline ref snapshot for `(environment_key, cwd)` when the cache is
    /// readable (`makeCachedVcsRefsChanges` in `packages/client-runtime/src/state/vcs.ts`).
    pub fn load_cached_refs(&self, environment_key: &str, cwd: &str) -> Option<ListRefsResult> {
        self.refs.set_environment_key(environment_key);
        let _persist_guard = self.refs.persistence_lock.lock().unwrap_or_else(|p| p.into_inner());
        if !self.refs.lock().persisted_cache_readable() {
            return None;
        }
        read_disk_cache(environment_key)?.remove(cwd)
    }

    /// `None` means invalidation superseded this response; it must never reach the picker.
    pub async fn list_refs(&self, input: Value) -> Result<Option<ListRefsResult>, String> {
        self.list_refs_with_cache(None, input).await
    }

    /// Like [`ClientHandle::list_refs`], and persists unfiltered first-page snapshots under
    /// `environment_key` for offline startup / recovery (`commitVcsRefsRefresh`).
    pub async fn list_refs_cached(&self, environment_key: &str, input: Value) -> Result<Option<ListRefsResult>, String> {
        self.refs.set_environment_key(environment_key);
        self.list_refs_with_cache(Some(environment_key), input).await
    }

    async fn list_refs_with_cache(&self, environment_key: Option<&str>, input: Value) -> Result<Option<ListRefsResult>, String> {
        let generation = *self.connection_generation().borrow();
        let persist = environment_key.is_some() && can_use_offline_refs_cache(&input);
        let cwd = input["cwd"].as_str().unwrap_or_default().to_owned();
        let request = self.refs.lock().begin(input);
        let result = self.request(method::VCS_LIST_REFS, request.input.clone()).await;
        if *self.connection_generation().borrow() != generation || self.refs.lock().revision() != request.revision() {
            return Ok(None);
        }
        let value = result.map_err(|error| error.to_string())?;
        let refs = serde_json::from_value::<ListRefsResult>(value).map_err(|error| error.to_string())?;
        let _persist_guard = self.refs.persistence_lock.lock().unwrap_or_else(|p| p.into_inner());
        let (committed, was_readable) = {
            let mut cache = self.refs.lock();
            let readable = cache.persisted_cache_readable();
            (cache.commit(&request), readable)
        };
        if !committed {
            return Ok(None);
        }
        if persist && let Some(env_key) = environment_key {
            let mut readable = was_readable;
            if !readable {
                readable = clear_disk_cache(env_key).is_ok();
            }
            if write_disk_cache(env_key, &cwd, &refs).is_ok() && readable != was_readable {
                let mut cache = self.refs.lock();
                if cache.revision() == request.revision() {
                    cache.set_persisted_cache_readable(readable);
                }
            }
        }
        Ok(Some(refs))
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

    #[test]
    fn offline_ref_disk_cache_roundtrips_and_clears() {
        let dir = std::env::temp_dir().join(format!("t3-vcs-refs-test-{}", uuid::Uuid::new_v4()));
        let path = super::refs_cache_path_in(&dir, "env:local/1").expect("cache path");
        let refs: super::ListRefsResult = serde_json::from_value(json!({
            "refs": [{ "name": "main", "current": true }, { "name": "feat/offline" }],
            "nextCursor": null,
            "totalCount": 2
        }))
        .unwrap();
        super::write_disk_cache_at(&path, "/repo", &refs).unwrap();
        let loaded = super::read_disk_cache_at(&path).expect("loaded");
        assert_eq!(loaded.get("/repo"), Some(&refs));
        super::clear_disk_cache_at(&path).unwrap();
        assert!(super::read_disk_cache_at(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
