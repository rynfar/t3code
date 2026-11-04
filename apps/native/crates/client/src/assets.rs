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

pub async fn fetch_media(client: &ClientHandle, source: &MediaSource) -> Result<MediaBytes, String> {
    match source {
        MediaSource::Direct(uri) if uri.len() > 5 && uri[..5].eq_ignore_ascii_case("data:") => {
            decode_data_url(uri).map(|bytes| MediaBytes { bytes, dimensions: None }).ok_or_else(|| UNAVAILABLE.to_owned())
        }
        MediaSource::Direct(uri) => Ok(MediaBytes { bytes: get(uri).await?, dimensions: None }),
        MediaSource::Asset(resource) => {
            let minted = mint_media_url(client, resource).await?;
            let mut media = fetch_signed_media(client, &minted.relative_url).await?;
            media.dimensions = minted.dimensions;
            Ok(media)
        }
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

/// Uses an already connected local environment only for web's three missing-host errors.
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
    if !t3_core::media::allows_local_media_fallback(resource, tag, true) { return Err(UNAVAILABLE.into()); }
    if let Some(local) = local.await.filter(|local| local.environment() != client.environment())
        && let Ok(mut asset) = mint_media_url(&local, resource).await
        && let Some(url) = t3_core::media::resolve_asset_url(local.environment(), &asset.relative_url)
    {
        asset.relative_url = url;
        return Ok(asset);
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
