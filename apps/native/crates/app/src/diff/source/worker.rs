//! Network-side workflow for `review.ts` / `useReviewFilePatches.ts`.
use super::{Loaded, Parsed};
use std::sync::Arc;
use t3_client::supervisor::ClientHandle;
use t3_core::diff::{ReviewModel, ReviewScope, decode_diff_preview};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

#[derive(Debug, Clone)]
pub enum Command {
    File(String),
    Next,
    Retry(String),
}

#[derive(Default)]
struct Projection {
    version: Option<u64>,
    parsed: Option<Arc<Parsed>>,
}

impl Projection {
    fn loaded(&mut self, model: &ReviewModel) -> Loaded {
        if self.version != Some(model.render_version()) {
            let source = model.source.as_ref();
            self.parsed = Some(Parsed::from_patch(model.patch(), source.is_some_and(|s| s.truncated && !model.lazy()), source.and_then(|s| s.totals), source.and_then(|s| s.base_ref.clone()), source.and_then(|s| s.head_ref.clone())));
            self.version = Some(model.render_version());
        }
        Loaded::Git { parsed: self.parsed.as_ref().expect("parsed projection installed").clone(), review: Arc::new(model.clone()), repository: model.repository }
    }
}

pub async fn run(
    client: ClientHandle,
    scope: ReviewScope,
    commit_sha: Option<String>,
    previous: Option<ReviewModel>,
    patch_reads: Arc<tokio::sync::Semaphore>,
    commands: mpsc::Receiver<Command>,
    updates: mpsc::Sender<Result<Loaded, String>>,
) {
    run_with(
        move |method, payload| {
            let client = client.clone();
            async move { client.request(method, payload).await.map_err(|error| error.to_string()) }
        },
        scope,
        commit_sha,
        previous,
        patch_reads,
        commands,
        updates,
    )
    .await;
}

pub(crate) async fn run_with<F, Fut>(
    request: F,
    scope: ReviewScope,
    commit_sha: Option<String>,
    previous: Option<ReviewModel>,
    patch_reads: Arc<tokio::sync::Semaphore>,
    mut commands: mpsc::Receiver<Command>,
    updates: mpsc::Sender<Result<Loaded, String>>,
) where
    F: Fn(&'static str, serde_json::Value) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, String>> + Send,
{
    let mut model = previous.unwrap_or_else(|| ReviewModel::new(scope.clone()).with_commit(commit_sha));
    model.states.retain(|_, state| !state.pending);
    let tracked = !model.has_complete_preview() && model.commit_sha.is_none();
    let (method, payload) = model.request(tracked);
    let preview_received = match request(method, payload).await {
        Ok(value) => {
            model.accept_preview(&value);
            true
        }
        Err(error) => {
            let _ = updates.send(Err(error)).await;
            if model.source.is_none() {
                return;
            }
            false
        }
    };
    let mut metadata = JoinSet::new();
    if model.incomplete && preview_received && model.commit_sha.is_none() {
        let request = request.clone();
        let (method, payload) = model.request(false);
        metadata.spawn(async move { request(method, payload).await });
    }
    let mut patches = JoinSet::new();
    let mut projection = Projection::default();
    let mut publish = true;
    loop {
        if publish {
            for ticket in model.take_requests() {
                let request = request.clone();
                let patch_reads = patch_reads.clone();
                patches.spawn(async move {
                    let _permit = patch_reads.acquire_owned().await.expect("diff patch read semaphore stays open");
                    let (method, payload) = ticket.request();
                    let result = request(method, payload).await.and_then(|value| {
                        decode_diff_preview(&value, ticket.scope.kind)
                            .ok_or_else(|| "Diff no longer available. Refresh the comparison.".into())
                    });
                    (ticket, result)
                });
            }
            if updates.send(Ok(projection.loaded(&model))).await.is_err() {
                return;
            }
        }
        publish = true;
        tokio::select! {
            result = metadata.join_next(), if !metadata.is_empty() => {
                match result {
                    Some(Ok(Ok(value))) => {
                        let revision = model.revision;
                        model.accept_preview(&value);
                        if revision != model.revision {
                            patches.abort_all();
                            while patches.join_next().await.is_some() {}
                        }
                    }
                    Some(Ok(Err(error))) => { let _ = updates.send(Err(error)).await; }
                    _ => return,
                }
            }
            result = patches.join_next(), if !patches.is_empty() => {
                if let Some(Ok((ticket, result))) = result { model.accept_patch(&ticket, result); }
            }
            command = commands.recv() => {
                match command {
                    Some(Command::File(path)) => publish = model.request_file(&path),
                    Some(Command::Next) => publish = model.load_next(),
                    Some(Command::Retry(path)) => publish = model.retry(&path),
                    None => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use t3_core::diff::PreviewSourceKind;

    #[tokio::test]
    async fn provisional_preview_is_visible_before_full_reply_and_survives_its_failure() {
        let scope = ReviewScope {
            environment: "smoke".into(),
            connection: 1,
            cwd: "/repo".into(),
            kind: PreviewSourceKind::WorkingTree,
            base_ref: None,
            ignore_whitespace: true,
        };
        let (release, hold) = tokio::sync::oneshot::channel::<()>();
        let hold = Arc::new(std::sync::Mutex::new(Some(hold)));
        let (send, commands) = mpsc::channel(16);
        let (updates, mut receive) = mpsc::channel(1);
        let task = tokio::spawn(run_with(
            move |_, input| {
                let hold = hold.clone();
                async move {
                    if input.get("file").is_some() {
                        return std::future::pending().await;
                    }
                    if input["trackedOnly"] == true {
                        return Ok(
                            json!({"incomplete":true,"generatedAt":"tracked","sources":[{"kind":"working-tree","patchesDeferred":true,"files":[{"path":"a","additions":1,"deletions":0}]}]}),
                        );
                    }
                    let hold = hold.lock().unwrap().take().unwrap();
                    hold.await.unwrap();
                    Err("full preview failed".into())
                }
            },
            scope,
            None,
            None,
            Arc::new(tokio::sync::Semaphore::new(4)),
            commands,
            updates,
        ));
        let first = receive.recv().await.unwrap().unwrap_or_else(|e| panic!("{e}"));
        let Loaded::Git { review, parsed: original_parsed, .. } = first else { panic!("git preview") };
        assert!(review.incomplete);
        assert_eq!(review.files()[0].path, "a");
        release.send(()).unwrap();
        assert!(matches!(receive.recv().await.unwrap(), Err(error) if error == "full preview failed"));
        let Loaded::Git { review: retained, parsed: retained_parsed, .. } = receive.recv().await.unwrap().unwrap_or_else(|e| panic!("{e}")) else {
            panic!("retained preview")
        };
        assert!(Arc::ptr_eq(&original_parsed, &retained_parsed), "status-only publication reuses the parsed representation");
        assert_eq!(retained.source, review.source);
        assert_eq!(retained.revision, review.revision);
        drop(send);
        task.await.unwrap();
        let (send, commands) = mpsc::channel(16);
        let (updates, mut receive) = mpsc::channel(1);
        let task = tokio::spawn(run_with(
            |_, input| async move {
                if input.get("file").is_some() {
                    Err("file failed after refresh".into())
                } else {
                    Err("metadata refresh failed".into())
                }
            },
            retained.scope.clone(),
            None,
            Some((*retained).clone()),
            Arc::new(tokio::sync::Semaphore::new(4)),
            commands,
            updates,
        ));
        assert!(matches!(receive.recv().await.unwrap(), Err(error) if error == "metadata refresh failed"));
        let Loaded::Git { review: pending, .. } = receive
            .recv()
            .await
            .expect("failed refresh keeps the file reader alive")
            .unwrap_or_else(|e| panic!("{e}"))
        else {
            panic!("retained git")
        };
        assert_eq!(pending.revision, retained.revision);
        let Loaded::Git { review: settled, .. } = receive.recv().await.unwrap().unwrap_or_else(|e| panic!("{e}")) else {
            panic!("file result")
        };
        assert_eq!(
            settled.states.values().next().unwrap().error.as_deref(),
            Some("file failed after refresh")
        );
        drop(send);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn tracked_to_complete_preview_forwards_completion_token_and_keeps_in_flight_patch_reads() {
        let scope = ReviewScope {
            environment: "smoke".into(),
            connection: 1,
            cwd: "/repo".into(),
            kind: PreviewSourceKind::BranchRange,
            base_ref: Some("main".into()),
            ignore_whitespace: true,
        };
        let (send, commands) = mpsc::channel(16);
        let (updates, mut receive) = mpsc::channel(4);
        let file_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_completion_token = Arc::new(std::sync::Mutex::new(None::<String>));
        let (allow_complete, wait_complete) = tokio::sync::oneshot::channel::<()>();
        let wait_complete = Arc::new(std::sync::Mutex::new(Some(wait_complete)));
        let (allow_patch, wait_patch) = tokio::sync::oneshot::channel::<()>();
        let wait_patch = Arc::new(std::sync::Mutex::new(Some(wait_patch)));
        let file_reads_req = file_reads.clone();
        let token_req = seen_completion_token.clone();

        let task = tokio::spawn(run_with(
            move |_, input| {
                let file_reads = file_reads_req.clone();
                let seen_token = token_req.clone();
                let wait_complete = wait_complete.clone();
                let wait_patch = wait_patch.clone();
                async move {
                    if let Some(file) = input.get("file") {
                        let path = file["path"].as_str().unwrap_or_default();
                        if path == "tracked.rs" {
                            file_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let rx = wait_patch.lock().unwrap().take();
                            if let Some(rx) = rx {
                                let _ = rx.await;
                            }
                            return Ok(json!({
                                "cwd": "/repo",
                                "generatedAt": "patch",
                                "incomplete": false,
                                "sources": [{
                                    "kind": "branch-range",
                                    "diff": "diff --git a/tracked.rs b/tracked.rs\n--- a/tracked.rs\n+++ b/tracked.rs\n@@ -1 +1 @@\n-old\n+new\n",
                                    "files": [{ "path": "tracked.rs", "previousPath": null, "changeType": "change", "additions": 1, "deletions": 1 }]
                                }]
                            }));
                        }
                        return Ok(json!({
                            "cwd": "/repo",
                            "generatedAt": "patch-u",
                            "incomplete": false,
                            "sources": [{
                                "kind": "branch-range",
                                "diff": "diff --git a/untracked.rs b/untracked.rs\n--- /dev/null\n+++ b/untracked.rs\n@@ -0,0 +1 @@\n+hi\n",
                                "files": [{ "path": "untracked.rs", "previousPath": null, "changeType": "new", "additions": 1, "deletions": 0 }]
                            }]
                        }));
                    }
                    if input["trackedOnly"] == true {
                        return Ok(json!({
                            "cwd": "/repo",
                            "generatedAt": "tracked",
                            "incomplete": true,
                            "completionToken": "tok-worker",
                            "sources": [{
                                "kind": "branch-range",
                                "baseRef": "main",
                                "headRef": "feat",
                                "mergeBase": "m1",
                                "patchesDeferred": true,
                                "files": [{ "path": "tracked.rs", "previousPath": null, "changeType": "change", "additions": 1, "deletions": 1 }]
                            }]
                        }));
                    }
                    *seen_token.lock().unwrap() = input.get("completionToken").and_then(|v| v.as_str()).map(str::to_owned);
                    let rx = wait_complete.lock().unwrap().take();
                    if let Some(rx) = rx {
                        let _ = rx.await;
                    }
                    Ok(json!({
                        "cwd": "/repo",
                        "generatedAt": "complete",
                        "incomplete": false,
                        "sources": [{
                            "kind": "branch-range",
                            "baseRef": "main",
                            "headRef": "feat",
                            "mergeBase": "m1",
                            "patchesDeferred": true,
                            "files": [
                                { "path": "tracked.rs", "previousPath": null, "changeType": "change", "additions": 1, "deletions": 1 },
                                { "path": "untracked.rs", "previousPath": null, "changeType": "new", "additions": 1, "deletions": 0 }
                            ]
                        }]
                    }))
                }
            },
            scope,
            None,
            None,
            Arc::new(tokio::sync::Semaphore::new(4)),
            commands,
            updates,
        ));

        let Loaded::Git { review: initial, .. } = receive.recv().await.unwrap().unwrap() else { panic!("tracked") };
        assert!(initial.incomplete);
        allow_complete.send(()).unwrap();
        let Loaded::Git { review: after_complete, .. } = receive.recv().await.unwrap().unwrap() else { panic!("complete") };
        assert!(!after_complete.incomplete);
        assert_eq!(seen_completion_token.lock().unwrap().as_deref(), Some("tok-worker"));
        allow_patch.send(()).unwrap();
        let Loaded::Git { review: settled, .. } = receive.recv().await.unwrap().unwrap() else { panic!("settled") };
        assert_eq!(settled.files().len(), 2);
        assert_eq!(file_reads.load(std::sync::atomic::Ordering::SeqCst), 1, "tracked.rs patch must not be aborted and re-fetched when complete preview arrives");
        drop(send);
        task.await.unwrap();
    }
}
