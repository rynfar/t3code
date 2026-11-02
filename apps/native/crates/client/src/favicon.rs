//! Project favicons over the wire: `assets.createUrl` mints a signed URL, as
//! `createProjectFaviconUrlAtomFamily` in `client-runtime/src/state/assets.ts` asks for it, and a
//! bounded GET reads the image, as `createProjectFaviconImageLoader` in
//! `client-runtime/src/projectFaviconCache.ts` does. The signed path is the credential: the asset
//! route in `apps/server/src/http.ts` takes no bearer token. Run it on the network runtime.

use std::time::Duration;

use t3_core::sidebar::favicon::{
    ASSETS_CREATE_URL, AssetCreateUrlResult, FaviconFormat, PROJECT_FAVICON_MAX_SOURCE_BYTES, is_project_favicon_fallback_url, project_favicon_url_request,
    project_favicon_url_revision, sniff_format,
};

use crate::supervisor::ClientHandle;

const TIMEOUT: Duration = Duration::from_secs(10);
const TOO_LARGE: &str = "Project icon is too large to decode.";

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectFaviconUrl {
    pub url: url::Url,
    /// `fileRevision` when the server sends one, else the signed URL's file name.
    pub revision: String,
    pub expires_at: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedFavicon {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub revision: String,
    pub format: FaviconFormat,
}

/// Mints and downloads the icon for the project at `cwd`. `None` when the project has none.
pub async fn fetch_project_favicon(handle: &ClientHandle, cwd: &str) -> Result<Option<FetchedFavicon>, String> {
    match create_project_favicon_url(handle, cwd).await? {
        Some(asset) => download_project_favicon(&asset).await.map(Some),
        None => Ok(None),
    }
}

/// The first half of `fetch_project_favicon`, so a caller holding the icon at `revision` can skip
/// the download.
pub async fn create_project_favicon_url(handle: &ClientHandle, cwd: &str) -> Result<Option<ProjectFaviconUrl>, String> {
    let value = handle.request(ASSETS_CREATE_URL, project_favicon_url_request(cwd)).await.map_err(|error| error.to_string())?;
    let result: AssetCreateUrlResult = serde_json::from_value(value).map_err(|error| format!("Undecodable project icon URL: {error}"))?;
    resolve_project_favicon_url(handle.environment(), result)
}

fn resolve_project_favicon_url(http_base: &str, result: AssetCreateUrlResult) -> Result<Option<ProjectFaviconUrl>, String> {
    if is_project_favicon_fallback_url(&result.relative_url) {
        return Ok(None);
    }
    let url = url::Url::parse(http_base).and_then(|base| base.join(&result.relative_url)).map_err(|error| format!("Invalid project icon URL: {error}"))?;
    let revision = result.file_revision.unwrap_or_else(|| project_favicon_url_revision(&result.relative_url));
    Ok(Some(ProjectFaviconUrl { url, revision, expires_at: result.expires_at }))
}

pub async fn download_project_favicon(asset: &ProjectFaviconUrl) -> Result<FetchedFavicon, String> {
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(describe)?;
    let mut response = client.get(asset.url.clone()).send().await.map_err(describe)?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("Project icon request failed with {}.", status.as_u16()));
    }
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).map(str::to_owned);
    let declared = response.content_length();
    if declared.is_some_and(|length| length > PROJECT_FAVICON_MAX_SOURCE_BYTES as u64) {
        return Err(TOO_LARGE.into());
    }
    let mut bytes = Vec::with_capacity(declared.unwrap_or(0) as usize);
    while let Some(chunk) = response.chunk().await.map_err(describe)? {
        if bytes.len() + chunk.len() > PROJECT_FAVICON_MAX_SOURCE_BYTES {
            return Err(TOO_LARGE.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let format = sniff_format(content_type.as_deref(), asset.url.as_str(), &bytes).ok_or_else(|| "Project icon has no image type.".to_owned())?;
    Ok(FetchedFavicon { bytes, content_type, revision: asset.revision.clone(), format })
}

/// Drops the URL: its path is a bearer token.
fn describe(error: reqwest::Error) -> String {
    if error.is_timeout() {
        format!("The server did not answer within {}s.", TIMEOUT.as_secs())
    } else if error.is_connect() {
        "Could not reach the server.".into()
    } else {
        error.without_url().to_string()
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn created(relative_url: &str, file_revision: Option<&str>) -> AssetCreateUrlResult {
        AssetCreateUrlResult { relative_url: relative_url.into(), expires_at: 1.0, file_revision: file_revision.map(str::to_owned) }
    }

    /// Answers one GET with `head`, then writes `chunk` until the client hangs up, `repeat` times
    /// at most when given.
    async fn serve_once(head: &'static str, chunk: Vec<u8>, repeat: Option<usize>) -> ProjectFaviconUrl {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            if stream.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let mut written = 0;
            while repeat.is_none_or(|repeat| written < repeat) && stream.write_all(&chunk).await.is_ok() {
                written += 1;
            }
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest).await;
        });
        ProjectFaviconUrl { url: url::Url::parse(&format!("http://{addr}/api/assets/token/v1-favicon.ico")).expect("url"), revision: "v1-favicon.ico".into(), expires_at: 1.0 }
    }

    #[test]
    fn resolves_the_signed_url_against_the_server_and_skips_the_fallback() {
        assert_eq!(resolve_project_favicon_url("http://127.0.0.1:3773", created("/api/assets/t/project-favicon-missing", None)), Ok(None));
        let asset = resolve_project_favicon_url("http://127.0.0.1:3773/", created("/api/assets/t.sig/vabc-icon%20x.svg", None)).unwrap().unwrap();
        assert_eq!(asset.url.as_str(), "http://127.0.0.1:3773/api/assets/t.sig/vabc-icon%20x.svg");
        assert_eq!(asset.revision, "vabc-icon%20x.svg");
        let revised = resolve_project_favicon_url("http://127.0.0.1:3773", created("/api/assets/t/vabc-icon.svg", Some("r1"))).unwrap().unwrap();
        assert_eq!(revised.revision, "r1");
    }

    #[tokio::test]
    async fn reads_the_image_and_names_the_format_gpui_decodes() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        let asset = serve_once("HTTP/1.1 200 OK\r\ncontent-type: image/x-icon\r\ncontent-length: 16\r\n\r\n", png.clone(), Some(1)).await;
        let fetched = download_project_favicon(&asset).await.unwrap();
        assert_eq!(fetched, FetchedFavicon { bytes: png, content_type: Some("image/x-icon".into()), revision: "v1-favicon.ico".into(), format: FaviconFormat::Png });

        let missing = serve_once("HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\n\r\n", b"Not Found".to_vec(), Some(1)).await;
        assert_eq!(download_project_favicon(&missing).await, Err("Project icon request failed with 404.".into()));
    }

    #[tokio::test]
    async fn stops_reading_a_response_that_exceeds_the_source_limit() {
        let declared = serve_once("HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: 1099511627776\r\n\r\n", b"x".to_vec(), Some(1)).await;
        assert_eq!(download_project_favicon(&declared).await, Err(TOO_LARGE.into()));

        let mut chunk = format!("{:x}\r\n", 1024 * 1024).into_bytes();
        chunk.extend(std::iter::repeat_n(0, 1024 * 1024));
        chunk.extend_from_slice(b"\r\n");
        let endless = serve_once("HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ntransfer-encoding: chunked\r\n\r\n", chunk, None).await;
        assert_eq!(download_project_favicon(&endless).await, Err(TOO_LARGE.into()));
    }
}
