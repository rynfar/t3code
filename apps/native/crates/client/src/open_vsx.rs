//! Network transport for `web/src/openVsxThemes.ts`; parsing lives in core.
use futures::future::join_all;
use std::time::Duration;
use t3_core::themes::{
    ThemeDefinition,
    open_vsx::{self, Extension, MAX_JSON_BYTES, MAX_SEARCH_BYTES, MAX_VSIX_BYTES},
};
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
    tokio::task::spawn_blocking(move || open_vsx::extract(&e, &manifest, &bytes, &checksum))
        .await
        .map_err(|e| e.to_string())?
}
