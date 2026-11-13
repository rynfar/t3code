//! `GET /api/orchestration/threads/:threadId/history`, as
//! `client-runtime/src/state/threadHistoryHttp.ts` requests it: bearer auth, the orchestration
//! protocol header, compact IDs. Collapsed work (`deferWork`) is not requested. The page is
//! decoded here, on the network runtime.

use std::time::Duration;

use serde_json::Value;
use t3_core::history::{HistoryPage, decode_history_page};
use t3_core::thread::{ThreadStreamItem, decode_thread_stream_item};

use crate::auth::{Credentials, ORCHESTRATION_PROTOCOL_VERSION};

/// `DEFAULT_THREAD_HISTORY_TIMEOUT_MS`.
const TIMEOUT: Duration = Duration::from_secs(6);
const PROTOCOL_HEADER: &str = "x-t3-orchestration-protocol";
/// `formatHistoryError`'s fallback.
const FALLBACK: &str = "Could not load earlier activity.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryQuery {
    pub cursor: String,
    pub through_entry_id: Option<String>,
    pub view: Option<&'static str>,
    pub defer_work: bool,
}

impl HistoryQuery {
    pub fn older(cursor: impl Into<String>) -> Self {
        Self { cursor: cursor.into(), through_entry_id: None, view: None, defer_work: true }
    }

    pub fn through_entry(cursor: impl Into<String>, entry_id: impl Into<String>) -> Self {
        Self {
            cursor: cursor.into(),
            through_entry_id: Some(entry_id.into()),
            view: Some("conversation"),
            defer_work: false,
        }
    }

    pub fn work(cursor: impl Into<String>) -> Self {
        Self { cursor: cursor.into(), through_entry_id: None, view: Some("work"), defer_work: false }
    }
}

pub fn history_url(http_base: &str, thread_id: &str, cursor: &str) -> Option<url::Url> {
    history_url_with(http_base, thread_id, &HistoryQuery::older(cursor))
}

pub fn history_url_with(http_base: &str, thread_id: &str, query: &HistoryQuery) -> Option<url::Url> {
    let mut url = url::Url::parse(http_base).ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().extend(["api", "orchestration", "threads", thread_id, "history"]);
    let mut pairs = url.query_pairs_mut();
    pairs.append_pair("cursor", &query.cursor);
    if let Some(entry_id) = query.through_entry_id.as_deref() {
        pairs.append_pair("throughEntryId", entry_id);
    }
    if let Some(view) = query.view {
        pairs.append_pair("view", view);
    }
    if query.defer_work {
        pairs.append_pair("deferWork", "1");
    }
    pairs.append_pair("compactIds", "1");
    drop(pairs);
    Some(url)
}

pub fn bounded_snapshot_url(http_base: &str, thread_id: &str) -> Option<url::Url> {
    let mut url = url::Url::parse(http_base).ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().extend(["api", "orchestration", "threads", thread_id, "bounded"]);
    url.query_pairs_mut()
        .append_pair("deferWork", "1")
        .append_pair("compactTurnItems", "1")
        .append_pair("dedupeBodies", "1")
        .append_pair("compactIds", "1");
    Some(url)
}

pub async fn fetch_history_page(credentials: &Credentials, thread_id: &str, cursor: &str) -> Result<HistoryPage, String> {
    fetch_history_page_with(credentials, thread_id, &HistoryQuery::older(cursor)).await
}

pub async fn fetch_history_page_with(credentials: &Credentials, thread_id: &str, query: &HistoryQuery) -> Result<HistoryPage, String> {
    let url = history_url_with(&credentials.http_base, thread_id, query).ok_or_else(|| FALLBACK.to_owned())?;
    let value = fetch_authed_json(credentials, url).await?;
    decode_history_page(value).map_err(|error| {
        tracing::warn!(%error, "undecodable history page");
        FALLBACK.to_owned()
    })
}

pub async fn fetch_bounded_snapshot(credentials: &Credentials, thread_id: &str) -> Result<ThreadStreamItem, String> {
    let url = bounded_snapshot_url(&credentials.http_base, thread_id).ok_or_else(|| FALLBACK.to_owned())?;
    let mut value = fetch_authed_json(credentials, url).await?;
    if let Some(object) = value.as_object_mut()
        && !object.contains_key("kind")
    {
        object.insert("kind".into(), serde_json::json!("snapshot"));
    }
    decode_thread_stream_item(value).map_err(|error| {
        tracing::warn!(%error, "undecodable bounded snapshot");
        FALLBACK.to_owned()
    })
}

async fn fetch_authed_json(credentials: &Credentials, url: url::Url) -> Result<Value, String> {
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
    Ok(value)
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
            "http://127.0.0.1:3773/api/orchestration/threads/thread:project:a%2Fb/history?cursor=c%3D1&deferWork=1&compactIds=1"
        );
        let prefixed = history_url("https://host.example/t3", "t1", "x").unwrap();
        assert_eq!(prefixed.path(), "/t3/api/orchestration/threads/t1/history");
    }
}

#[cfg(test)]
mod bounded_and_work_tests {
    use super::*;

    #[test]
    fn history_url_with_supports_deferred_work_anchor_and_through_entry() {
        let default_url = history_url_with("http://127.0.0.1:3773", "t1", &HistoryQuery::older("c1")).unwrap();
        let pairs: Vec<(String, String)> = default_url.query_pairs().into_owned().collect();
        assert!(pairs.contains(&("cursor".into(), "c1".into())));
        assert!(pairs.contains(&("deferWork".into(), "1".into())));
        assert!(pairs.contains(&("compactIds".into(), "1".into())));
        assert!(!pairs.iter().any(|(k, _)| k == "view"));

        let through_url = history_url_with("http://127.0.0.1:3773", "t1", &HistoryQuery::through_entry("c1", "entry-1")).unwrap();
        let through_pairs: Vec<(String, String)> = through_url.query_pairs().into_owned().collect();
        assert!(through_pairs.contains(&("throughEntryId".into(), "entry-1".into())));
        assert!(through_pairs.contains(&("view".into(), "conversation".into())));
        assert!(!through_pairs.iter().any(|(k, _)| k == "deferWork"));

        let work_url = history_url_with("http://127.0.0.1:3773", "t1", &HistoryQuery::work("w1")).unwrap();
        let work_pairs: Vec<(String, String)> = work_url.query_pairs().into_owned().collect();
        assert!(work_pairs.contains(&("view".into(), "work".into())));
        assert!(!work_pairs.iter().any(|(k, _)| k == "deferWork"));

        let bounded = bounded_snapshot_url("http://127.0.0.1:3773/prefix/", "t/1").unwrap();
        assert_eq!(
            bounded.path(),
            "/prefix/api/orchestration/threads/t%2F1/bounded"
        );
        let bounded_pairs: Vec<(String, String)> = bounded.query_pairs().into_owned().collect();
        assert!(bounded_pairs.contains(&("deferWork".into(), "1".into())));
        assert!(bounded_pairs.contains(&("compactTurnItems".into(), "1".into())));
        assert!(bounded_pairs.contains(&("dedupeBodies".into(), "1".into())));
        assert!(bounded_pairs.contains(&("compactIds".into(), "1".into())));
    }
}
