//! `POST /api/pull-requests/diff`, as `client-runtime/src/state/pullRequestDiffHttp.ts` sends it:
//! bearer auth, a 60s timeout, and the server's tagged errors passed back for the page to word.

use std::time::Duration;

use serde_json::Value;

use crate::auth::Credentials;

/// `DEFAULT_PULL_REQUEST_DIFF_TIMEOUT_MS`.
const TIMEOUT: Duration = Duration::from_secs(60);
const FALLBACK: &str = "Could not load the diff.";
/// `PullRequestDiffCredentialRejectedError.message` (`pullRequestDiffHttp.ts`).
pub const INVALID_CREDENTIAL_MESSAGE: &str =
    "This environment session is no longer valid (invalid_credential). Refresh or quit and reopen T3 Code.";

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

/// Maps an HTTP error body (`status`, `value`) into an `HttpFailure`, matching
/// `PullRequestDiffCredentialRejectedError` in `pullRequestDiffHttp.ts`.
pub fn classify_diff_http_error(status: u16, value: Value) -> HttpFailure {
    let tag = value.get("_tag").and_then(Value::as_str).map(str::to_owned);
    if tag.as_deref() == Some("EnvironmentAuthInvalidError")
        && value.get("reason").and_then(Value::as_str) == Some("invalid_credential")
    {
        return HttpFailure {
            tag: Some("PullRequestDiffCredentialRejectedError".to_owned()),
            error: value,
            message: INVALID_CREDENTIAL_MESSAGE.to_owned(),
        };
    }
    if status == 401 {
        return HttpFailure {
            tag,
            error: value,
            message: "This environment session is no longer valid. Pair again.".to_owned(),
        };
    }
    HttpFailure { tag, error: value, message: FALLBACK.to_owned() }
}

pub async fn fetch_diff(credentials: &Credentials, payload: Value) -> Result<Value, HttpFailure> {
    let failure = |message: &str| HttpFailure { tag: None, error: Value::Null, message: message.to_owned() };
    let url = diff_url(&credentials.http_base).ok_or_else(|| failure(FALLBACK))?;
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|_| failure(FALLBACK))?;
    let url_str = url.as_str().to_owned();
    let response = crate::auth::send_authed_request(credentials, "POST", &url_str, || {
        client.post(url.clone()).json(&payload)
    })
    .await
    .map_err(|error| match error {
        crate::auth::AuthedHttpError::Http(err) if err.is_timeout() => {
            failure(&format!("The server did not answer within {}s.", TIMEOUT.as_secs()))
        }
        crate::auth::AuthedHttpError::Http(err) if err.is_connect() => failure("Could not reach the server."),
        crate::auth::AuthedHttpError::Http(_) => failure(FALLBACK),
        crate::auth::AuthedHttpError::Auth(err) if err.is_host_credential_refusal() => {
            failure("This environment session is no longer valid. Pair again.")
        }
        crate::auth::AuthedHttpError::Auth(err) => failure(&err.to_string()),
    })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|_| failure(FALLBACK))?;
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(value);
    }
    Err(classify_diff_http_error(status.as_u16(), value))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn diff_url_keeps_a_base_path() {
        assert_eq!(diff_url("http://127.0.0.1:3773/").unwrap().as_str(), "http://127.0.0.1:3773/api/pull-requests/diff");
        assert_eq!(diff_url("https://host/t3").unwrap().as_str(), "https://host/t3/api/pull-requests/diff");
    }

    #[test]
    fn invalid_credential_error_maps_to_credential_rejected() {
        let err = classify_diff_http_error(
            401,
            json!({ "_tag": "EnvironmentAuthInvalidError", "reason": "invalid_credential", "traceId": "t1" }),
        );
        assert_eq!(err.tag.as_deref(), Some("PullRequestDiffCredentialRejectedError"));
        assert_eq!(err.message, INVALID_CREDENTIAL_MESSAGE);
    }
}
