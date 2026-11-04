//! Network transport for `web/src/openVsxThemes.ts`; parsing lives in core.
use futures::future::join_all;
use std::{
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use t3_core::themes::{
    ThemeDefinition,
    open_vsx::{self, Extension, MAX_JSON_BYTES, MAX_SEARCH_BYTES, MAX_VSIX_BYTES},
};
use tokio::sync::Semaphore;
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())
}
async fn capped(
    mut response: reqwest::Response,
    limit: usize,
    unavailable: &str,
    large: &str,
) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        return Err(unavailable.into());
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(large.into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > limit {
            return Err(large.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn get(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    unavailable: &str,
    large: &str,
) -> Result<Vec<u8>, String> {
    let r = client.get(url).send().await.map_err(|e| e.to_string())?;
    capped(r, limit, unavailable, large).await
}
async fn timed<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .map_err(|_| "Open VSX took too long to respond.".to_string())?
}
pub async fn search(query: String, sort: String) -> Result<Vec<Extension>, String> {
    let Some(url) = open_vsx::search_url(&query, &sort) else {
        return Ok(Vec::new());
    };
    let client = client()?;
    let bytes = timed(get(
        &client,
        url.as_str(),
        MAX_SEARCH_BYTES,
        "Open VSX search is unavailable right now.",
        "Open VSX returned an unexpectedly large response.",
    ))
    .await?;
    let ids = open_vsx::identities(&bytes)?;
    let details = join_all(ids.into_iter().map(|url| {
        let c = &client;
        async move {
            timed(async move {
                let bytes = get(
                    c,
                    url.as_str(),
                    MAX_JSON_BYTES,
                    "Open VSX theme details are unavailable.",
                    "Open VSX returned an unexpectedly large detail response.",
                )
                .await?;
                let Some(e) = open_vsx::detail(&bytes)? else {
                    return Ok(None);
                };
                let (manifest, package) = tokio::join!(
                    get(
                        c,
                        &e.manifest_url,
                        MAX_JSON_BYTES,
                        "manifest unavailable",
                        "Open VSX returned an unexpectedly large manifest."
                    ),
                    c.head(&e.vsix_url).send()
                );
                let package = package.map_err(|e| e.to_string())?;
                if !package.status().is_success() {
                    return Ok(None);
                }
                Ok(open_vsx::eligible(&e, &manifest?, package.content_length())?.then_some(e))
            })
            .await
        }
    }))
    .await;
    open_vsx::ranked_results(details)
}
pub async fn download(e: Extension) -> Result<Vec<ThemeDefinition>, String> {
    let c = client()?;
    let manifest = get(
        &c,
        &e.manifest_url,
        MAX_JSON_BYTES,
        "That Open VSX extension has no readable manifest.",
        "That Open VSX extension manifest is too large.",
    )
    .await?;
    let bytes = get(
        &c,
        &e.vsix_url,
        MAX_VSIX_BYTES,
        "That Open VSX theme could not be downloaded.",
        "That theme extension is too large to import safely.",
    )
    .await?;
    let checksum = get(
        &c,
        &e.sha256_url,
        256,
        "That Open VSX theme has no readable checksum.",
        "That Open VSX checksum response is invalid.",
    )
    .await?;
    run_extraction(EXTRACTION.clone(), move |cancelled| {
        open_vsx::extract_cancellable(&e, &manifest, &bytes, &checksum, &|| {
            cancelled.load(Ordering::Acquire)
        })
    })
    .await
}

static EXTRACTION: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
async fn run_extraction<T: Send + 'static>(
    semaphore: Arc<Semaphore>,
    operation: impl FnOnce(Arc<AtomicBool>) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(cancelled.clone());
    let permit = semaphore.acquire_owned().await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        // Aborting the awaiter must not release the lease before its blocking job actually stops.
        let _permit = permit;
        if cancelled.load(Ordering::Acquire) {
            return Err(open_vsx::IMPORT_CANCELLED.into());
        }
        operation(cancelled)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_extraction_keeps_its_lease_until_it_stops_and_queued_work_is_retired() {
        let semaphore = Arc::new(Semaphore::new(1));
        let (started, start) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let (stopped, stop) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(run_extraction(semaphore.clone(), move |cancelled| {
            started.send(cancelled.clone()).unwrap();
            gate.recv().unwrap();
            stopped.send(cancelled.load(Ordering::Acquire)).unwrap();
            Ok(())
        }));
        let cancelled = start.await.unwrap();
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        let cancellation_observed = cancelled.load(Ordering::Acquire);
        let lease_held = semaphore.clone().try_acquire_owned().is_err();
        let queued_started = Arc::new(AtomicBool::new(false));
        let marker = queued_started.clone();
        {
            let mut queued = Box::pin(run_extraction(semaphore.clone(), move |_| {
                marker.store(true, Ordering::Release);
                Ok(())
            }));
            let _ = futures::poll!(&mut queued);
        }
        release.send(()).unwrap();
        let worker_cancelled = stop.await.unwrap();
        run_extraction(semaphore, move |_| Ok(())).await.unwrap();
        assert!(cancellation_observed);
        assert!(worker_cancelled);
        assert!(lease_held);
        assert!(!queued_started.load(Ordering::Acquire));
    }
}
