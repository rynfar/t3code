//! `GET /api/orchestration/threads/:threadId/history`, as
//! `client-runtime/src/state/threadHistoryHttp.ts` requests it: bearer auth, the orchestration
//! protocol header, compact IDs. Collapsed work (`deferWork`) is not requested. The page is
//! decoded here, on the network runtime.

use std::time::Duration;

use serde_json::Value;
use t3_core::history::{HistoryPage, decode_history_page};

use crate::auth::{Credentials, ORCHESTRATION_PROTOCOL_VERSION};

/// `DEFAULT_THREAD_HISTORY_TIMEOUT_MS`.
const TIMEOUT: Duration = Duration::from_secs(6);
const PROTOCOL_HEADER: &str = "x-t3-orchestration-protocol";
/// `formatHistoryError`'s fallback.
const FALLBACK: &str = "Could not load earlier activity.";

pub fn history_url(http_base: &str, thread_id: &str, cursor: &str) -> Option<url::Url> {
    let mut url = url::Url::parse(http_base).ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().extend(["api", "orchestration", "threads", thread_id, "history"]);
    url.query_pairs_mut().append_pair("cursor", cursor).append_pair("compactIds", "1");
    Some(url)
}

pub async fn fetch_history_page(credentials: &Credentials, thread_id: &str, cursor: &str) -> Result<HistoryPage, String> {
    let url = history_url(&credentials.http_base, thread_id, cursor).ok_or_else(|| FALLBACK.to_owned())?;
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|_| FALLBACK.to_owned())?;
    let response = client
        .get(url)
        .bearer_auth(&credentials.access_token)
        .header(PROTOCOL_HEADER, ORCHESTRATION_PROTOCOL_VERSION.to_string())
        .send()
        .await
        .map_err(describe)?;
    let status = response.status();
    let body = response.bytes().await.map_err(describe)?;
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("This device is no longer authorized. Pair again.".into());
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| FALLBACK.to_owned())?;
    if !status.is_success() {
        let message = value.get("message").and_then(Value::as_str).filter(|m| !m.trim().is_empty());
        return Err(message.unwrap_or(FALLBACK).to_owned());
    }
    decode_history_page(value).map_err(|error| {
        tracing::warn!(%error, "undecodable history page");
        FALLBACK.to_owned()
    })
}

fn describe(error: reqwest::Error) -> String {
    if error.is_timeout() {
        format!("The server did not answer within {}s.", TIMEOUT.as_secs())
    } else if error.is_connect() {
        "Could not reach the server.".into()
    } else {
        FALLBACK.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_url_escapes_the_thread_id_and_keeps_a_base_path() {
        let url = history_url("http://127.0.0.1:3773/", "thread:project:a/b", "c=1").unwrap();
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:3773/api/orchestration/threads/thread:project:a%2Fb/history?cursor=c%3D1&compactIds=1"
        );
        let prefixed = history_url("https://host.example/t3", "t1", "x").unwrap();
        assert_eq!(prefixed.path(), "/t3/api/orchestration/threads/t1/history");
    }
}
