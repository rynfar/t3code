//! Media bytes for the transcript: an image attachment or a host file, signed with
//! `assets.createUrl` and fetched with `asset::fetch_asset`, or an image URL fetched as authored.
//! Runs on the network runtime; decoding happens elsewhere, off the UI thread.

use std::time::Duration;

use t3_core::media::{AssetResource, AssetUrl, MediaSource, decode_asset_url, decode_data_url};

use crate::supervisor::ClientHandle;

const TIMEOUT: Duration = Duration::from_secs(30);
/// Attachments are capped at 10 MiB on send; this leaves room for host files without letting
/// one link pull an unbounded body into memory.
const MAX_BYTES: usize = 32 * 1024 * 1024;
const UNAVAILABLE: &str = "Image unavailable. The file may have been moved or deleted.";

/// Fetched media: the encoded bytes, and the size the server reported, when it did.
pub struct MediaBytes {
    pub bytes: Vec<u8>,
    pub dimensions: Option<(u32, u32)>,
}

/// Decodes an inline `data:` media URI without requiring an active server client.
pub fn fetch_data_media(uri: &str) -> Result<MediaBytes, String> {
    decode_data_url(uri).map(|bytes| MediaBytes { bytes, dimensions: None }).ok_or_else(|| UNAVAILABLE.to_owned())
}

pub async fn fetch_media(client: &ClientHandle, source: &MediaSource) -> Result<MediaBytes, String> {
    match source {
        MediaSource::Direct(uri) if uri.len() > 5 && uri[..5].eq_ignore_ascii_case("data:") => fetch_data_media(uri),
        MediaSource::Direct(uri) => Ok(MediaBytes { bytes: get(uri).await?, dimensions: None }),
        MediaSource::Asset(resource) => match mint_media_url(client, resource).await {
            Ok(minted) => {
                match fetch_signed_media(client, &minted.relative_url).await {
                    Ok(mut media) => {
                        media.dimensions = minted.dimensions;
                        Ok(media)
                    }
                    Err(error) => match resource.fallback_url() {
                        Some(fallback) => Ok(MediaBytes { bytes: get(fallback).await?, dimensions: None }),
                        None => Err(error),
                    },
                }
            }
            Err(error) => match resource.fallback_url() {
                Some(fallback) => Ok(MediaBytes { bytes: get(fallback).await?, dimensions: None }),
                None => Err(error),
            },
        },
    }
}

/// The mint and fetch are separate so an unchanged file keeps its already decoded image.
pub async fn mint_media_url(client: &ClientHandle, resource: &AssetResource) -> Result<AssetUrl, String> {
    let minted = client.request(t3_core::media::CREATE_ASSET_URL, resource.create_url_request()).await.map_err(|error| {
        tracing::debug!(%error, "assets.createUrl failed");
        UNAVAILABLE.to_owned()
    })?;
    decode_asset_url(&minted)
}

/// Uses an already connected local environment only for web's three missing-host errors, or the
/// direct host URL for `SourceControlMedia` (`fallbackSrc={hostMediaUrl}`).
/// Unknown error tags and failed fallbacks use the same unavailable presentation.
pub async fn mint_media_url_with_fallback(
    client: &ClientHandle,
    resource: &AssetResource,
    local: impl std::future::Future<Output = Option<ClientHandle>>,
) -> Result<AssetUrl, String> {
    let result = client.request(t3_core::media::CREATE_ASSET_URL, resource.create_url_request()).await;
    let error = match result {
        Ok(value) => return decode_asset_url(&value),
        Err(error) => error,
    };
    let tag = match &error { crate::session::RpcError::Failure(error) => error.tag.as_deref(), _ => None };
    if t3_core::media::allows_local_media_fallback(resource, tag, true)
        && let Some(local) = local.await.filter(|local| local.environment() != client.environment())
        && let Ok(mut asset) = mint_media_url(&local, resource).await
        && let Some(url) = t3_core::media::resolve_asset_url(local.environment(), &asset.relative_url)
    {
        asset.relative_url = url;
        return Ok(asset);
    }
    if let Some(fallback) = resource.fallback_url() {
        return Ok(AssetUrl {
            relative_url: fallback.to_owned(),
            dimensions: None,
            expires_at: 0.,
            file_revision: None,
        });
    }
    Err(UNAVAILABLE.into())
}

pub async fn fetch_signed_media(client: &ClientHandle, relative_url: &str) -> Result<MediaBytes, String> {
    if relative_url.starts_with("http://") || relative_url.starts_with("https://") {
        return Ok(MediaBytes { bytes: get(relative_url).await?, dimensions: None });
    }
    let bytes = crate::asset::fetch_asset(client.environment(), relative_url, MAX_BYTES).await.map_err(|error| {
        tracing::debug!(%error, "asset fetch failed");
        UNAVAILABLE.to_owned()
    })?;
    Ok(MediaBytes { bytes, dimensions: None })
}

async fn get(url: &str) -> Result<Vec<u8>, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(UNAVAILABLE.into());
    }
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|error| error.to_string())?;
    let mut response = client.get(url).send().await.map_err(|error| describe(&error))?;
    if !response.status().is_success() {
        return Err(UNAVAILABLE.into());
    }
    if response.content_length().is_some_and(|length| length as usize > MAX_BYTES) {
        return Err("This image is too large to show.".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| describe(&error))? {
        if body.len() + chunk.len() > MAX_BYTES {
            return Err("This image is too large to show.".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() { "The image took too long to load.".into() } else { UNAVAILABLE.into() }
}

/// The blocking worker keeps its permit if the async caller is cancelled.
pub async fn decode_media<T: Send + 'static>(permit: tokio::sync::OwnedSemaphorePermit, decode: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(move || { let _permit = permit; decode() }).await.map_err(|_| "Image unavailable.".to_owned())?
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn cancelled_decode_keeps_its_permit_until_the_worker_finishes() {
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = permits.clone().acquire_owned().await.unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, worker) = std::sync::mpsc::channel();
        let task = tokio::spawn(super::decode_media(permit, move || {
            started.send(()).unwrap();
            worker.recv().unwrap();
            Ok(())
        }));
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let held = permits.available_permits() == 0;
        release.send(()).unwrap();
        assert!(held, "cancelled async callers must not admit another blocking decode");
        let _finished = permits.acquire_owned().await.unwrap();
    }

    #[tokio::test]
    async fn source_control_media_sends_repository_and_falls_back_to_host_url_when_mint_fails() {
        use crate::supervisor;
        use crate::test_server::FakeServer;
        use serde_json::json;
        use t3_core::media::AssetResource;

        let mut server = FakeServer::start().await;
        let (handle, _events) = supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut socket = server.connection().await;
        let config = socket.request(t3_core::commands::method::SUBSCRIBE_SERVER_CONFIG).await;
        socket.chunk(&config, json!([{"type":"snapshot","config":{}}])).await;

        let fallback = "https://raw.githubusercontent.com/owner/repo/main/shot.png";
        let resource = AssetResource::SourceControlMedia {
            cwd: "/repo".into(),
            url: fallback.into(),
            repository: Some("upstream/repo".into()),
        };

        let mint_ok = tokio::spawn({
            let handle = handle.clone();
            let resource = resource.clone();
            async move { super::mint_media_url_with_fallback(&handle, &resource, async { None }).await }
        });
        let req = socket.request(t3_core::media::CREATE_ASSET_URL).await;
        assert_eq!(req["payload"], resource.create_url_request());
        assert_eq!(req["payload"]["resource"]["repository"], "upstream/repo");
        socket
            .send(json!({"_tag":"Exit","requestId":req["id"],"exit":{"_tag":"Success","value":{"relativeUrl":"/api/assets/signed","expiresAt":9999999999.0}}}))
            .await;
        let ok = mint_ok.await.unwrap().unwrap();
        assert_eq!(ok.relative_url, "/api/assets/signed");

        let mint_fail = tokio::spawn({
            let handle = handle.clone();
            let resource = resource.clone();
            async move { super::mint_media_url_with_fallback(&handle, &resource, async { None }).await }
        });
        let req2 = socket.request(t3_core::media::CREATE_ASSET_URL).await;
        socket
            .send(json!({"_tag":"Exit","requestId":req2["id"],"exit":{"_tag":"Failure","cause":{"_tag":"Fail","error":{"_tag":"NotFound","message":"missing"}}}}))
            .await;
        let fallen_back = mint_fail.await.unwrap().unwrap();
        assert_eq!(fallen_back.relative_url, fallback);
    }
}
