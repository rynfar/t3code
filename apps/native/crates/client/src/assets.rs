//! Media bytes for the transcript: an image attachment or a host file, signed with
//! `assets.createUrl` and fetched with `asset::fetch_asset`, or an image URL fetched as authored.
//! Runs on the network runtime; decoding happens elsewhere, off the UI thread.

use std::time::Duration;

use t3_core::media::{AssetUrl, MediaSource, decode_asset_url, decode_data_url};

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
            let minted = client.request(t3_core::media::CREATE_ASSET_URL, resource.create_url_request()).await.map_err(|error| {
                tracing::debug!(%error, "assets.createUrl failed");
                UNAVAILABLE.to_owned()
            })?;
            let AssetUrl { relative_url, dimensions } = decode_asset_url(&minted)?;
            let bytes = crate::asset::fetch_asset(client.environment(), &relative_url, MAX_BYTES).await.map_err(|error| {
                tracing::debug!(%error, "asset fetch failed");
                UNAVAILABLE.to_owned()
            })?;
            Ok(MediaBytes { bytes, dimensions })
        }
    }
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
