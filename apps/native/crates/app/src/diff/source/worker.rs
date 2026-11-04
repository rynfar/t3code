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

pub fn loaded(model: &ReviewModel) -> Loaded {
    let source = model.source.as_ref();
    Loaded::Git {
        parsed: Parsed::from_patch(
            model.patch(),
            source.is_some_and(|s| s.truncated && !model.lazy()),
            source.and_then(|s| s.totals),
            source.and_then(|s| s.base_ref.clone()),
            source.and_then(|s| s.head_ref.clone()),
        ),
        review: Arc::new(model.clone()),
        repository: model.repository,
    }
}

pub async fn run(
    client: ClientHandle,
    scope: ReviewScope,
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
    previous: Option<ReviewModel>,
    patch_reads: Arc<tokio::sync::Semaphore>,
    mut commands: mpsc::Receiver<Command>,
    updates: mpsc::Sender<Result<Loaded, String>>,
) where
    F: Fn(&'static str, serde_json::Value) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, String>> + Send,
{
    let mut model = previous.unwrap_or_else(|| ReviewModel::new(scope.clone()));
    model.states.retain(|_, state| !state.pending);
    let tracked = !model.has_complete_preview();
    let (method, payload) = scope.request(tracked);
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
    if model.incomplete && preview_received {
        let request = request.clone();
        let (method, payload) = scope.request(false);
        metadata.spawn(async move { request(method, payload).await });
    }
    let mut patches = JoinSet::new();
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
            if updates.send(Ok(loaded(&model))).await.is_err() {
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
            Arc::new(tokio::sync::Semaphore::new(4)),
            commands,
            updates,
        ));
        let first = receive.recv().await.unwrap().unwrap_or_else(|e| panic!("{e}"));
        let Loaded::Git { review, .. } = first else { panic!("git preview") };
        assert!(review.incomplete);
        assert_eq!(review.files()[0].path, "a");
        release.send(()).unwrap();
        assert!(matches!(receive.recv().await.unwrap(), Err(error) if error == "full preview failed"));
        let Loaded::Git { review: retained, .. } = receive.recv().await.unwrap().unwrap_or_else(|e| panic!("{e}")) else {
            panic!("retained preview")
        };
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
}
