//! `GET` of a signed asset URL (`assets.createUrl`'s `relativeUrl`), as the web's `<img src>`
//! loads it: the signature in the path is the credential. The body is read with a byte cap so a
//! huge file never lands in memory whole.

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);
const FALLBACK: &str = "Unable to load workspace image.";

pub fn asset_url(http_base: &str, relative_url: &str) -> Option<url::Url> {
    if !relative_url.starts_with('/') || relative_url.starts_with("//") {
        return None;
    }
    url::Url::parse(http_base).ok()?.join(relative_url).ok()
}

pub async fn fetch_asset(http_base: &str, relative_url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let url = asset_url(http_base, relative_url).ok_or_else(|| FALLBACK.to_owned())?;
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|_| FALLBACK.to_owned())?;
    let mut response = client.get(url).send().await.map_err(|_| FALLBACK.to_owned())?;
    if !response.status().is_success() {
        return Err(FALLBACK.to_owned());
    }
    let too_large = || format!("The image is larger than {} MB.", max_bytes / (1024 * 1024));
    if response.content_length().is_some_and(|length| length > max_bytes as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| FALLBACK.to_owned())? {
        if body.len() + chunk.len() > max_bytes {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::asset_url;

    #[test]
    fn asset_urls_stay_on_the_server() {
        let url = asset_url("http://127.0.0.1:3773", "/api/assets/abc/a%20b.png?x=1").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:3773/api/assets/abc/a%20b.png?x=1");
        assert_eq!(asset_url("http://h:1/base/", "/api/assets/t/f").unwrap().as_str(), "http://h:1/api/assets/t/f");
        assert!(asset_url("http://h:1", "//evil.example/x").is_none());
        assert!(asset_url("http://h:1", "https://evil.example/x").is_none());
    }
}
