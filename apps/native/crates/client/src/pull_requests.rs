//! `POST /api/pull-requests/diff`, as `client-runtime/src/state/pullRequestDiffHttp.ts` sends it:
//! bearer auth, a 60s timeout, and the server's tagged errors passed back for the page to word.

use std::time::Duration;

use serde_json::Value;

use crate::auth::Credentials;

/// `DEFAULT_PULL_REQUEST_DIFF_TIMEOUT_MS`.
const TIMEOUT: Duration = Duration::from_secs(60);
const FALLBACK: &str = "Could not load the diff.";

/// A failed read: the error body's `_tag` and fields when the server sent one.
#[derive(Debug, Clone)]
pub struct HttpFailure {
    pub tag: Option<String>,
    pub error: Value,
    pub message: String,
}

pub fn diff_url(http_base: &str) -> Option<url::Url> {
    let mut url = url::Url::parse(http_base).ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().extend(["api", "pull-requests", "diff"]);
    Some(url)
}

pub async fn fetch_diff(credentials: &Credentials, payload: Value) -> Result<Value, HttpFailure> {
    let failure = |message: &str| HttpFailure { tag: None, error: Value::Null, message: message.to_owned() };
    let url = diff_url(&credentials.http_base).ok_or_else(|| failure(FALLBACK))?;
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|_| failure(FALLBACK))?;
    let request = credentials.authorize(client.post(url.as_str()), "POST", url.as_str()).await.map_err(|message| failure(&message))?;
    let response = request.json(&payload).send().await.map_err(|error| {
        if error.is_timeout() {
            failure(&format!("The server did not answer within {}s.", TIMEOUT.as_secs()))
        } else if error.is_connect() {
            failure("Could not reach the server.")
        } else {
            failure(FALLBACK)
        }
    })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|_| failure(FALLBACK))?;
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(value);
    }
    if status.as_u16() == 401 {
        return Err(failure("This environment session is no longer valid. Pair again."));
    }
    let tag = value.get("_tag").and_then(Value::as_str).map(str::to_owned);
    Err(HttpFailure { tag, error: value, message: FALLBACK.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_url_keeps_a_base_path() {
        assert_eq!(diff_url("http://127.0.0.1:3773/").unwrap().as_str(), "http://127.0.0.1:3773/api/pull-requests/diff");
        assert_eq!(diff_url("https://host/t3").unwrap().as_str(), "https://host/t3/api/pull-requests/diff");
    }
}
