//! Diagnostics-only telemetry demand, ported from `web/src/lib/resourceTelemetryState.ts`.
//! Decode on Tokio and publish only the newest snapshot; dropping the owner interrupts demand.

use crate::{session::RpcError, supervisor::ClientHandle};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use t3_core::resource_telemetry::{self as model, History, HistoryWindow, RetryResult, Snapshot};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feed {
    pub snapshot: Option<Arc<Snapshot>>,
    pub error: Option<String>,
}

pub struct ResourceTelemetryStream {
    pub feed: watch::Receiver<Feed>,
    task: JoinHandle<()>,
}
impl Drop for ResourceTelemetryStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn subscribe(runtime: &tokio::runtime::Handle, client: ClientHandle) -> ResourceTelemetryStream {
    let (tx, feed) = watch::channel(Feed::default());
    let task = runtime.spawn(async move {
        let mut attempt = 0u32;
        loop {
            match client.stream(model::SUBSCRIBE, json!({})).await {
                Ok(mut stream) => {
                    while let Some(item) = stream.receiver.recv().await {
                        match item {
                            Ok(value) => match serde_json::from_value::<Snapshot>(value) {
                                Ok(snapshot) => {
                                    attempt = 0;
                                    tx.send_replace(Feed { snapshot: Some(Arc::new(snapshot)), error: None });
                                }
                                Err(error) => {
                                    tx.send_modify(|feed| feed.error = Some(format!("Could not read resource telemetry: {error}")));
                                }
                            },
                            Err(RpcError::Failure(failure)) => {
                                tx.send_modify(|feed| feed.error = Some(failure.message));
                                if stream.connection_closed() {
                                    break;
                                }
                                return;
                            }
                            Err(RpcError::Closed) => break,
                        }
                    }
                }
                Err(RpcError::Failure(failure)) => {
                    tx.send_modify(|feed| feed.error = Some(failure.message));
                    return;
                }
                Err(RpcError::Closed) => {}
            }
            if tx.is_closed() {
                return;
            }
            attempt += 1;
            tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(5)).min(5_000))).await;
        }
    });
    ResourceTelemetryStream { feed, task }
}

pub struct HistoryRequest {
    task: JoinHandle<Result<Arc<History>, String>>,
}
impl Drop for HistoryRequest {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl HistoryRequest {
    pub async fn result(&mut self) -> Result<Arc<History>, String> {
        (&mut self.task).await.map_err(|error| error.to_string())?
    }
}
pub fn start_history(runtime: &tokio::runtime::Handle, client: ClientHandle, window: HistoryWindow) -> HistoryRequest {
    HistoryRequest { task: runtime.spawn(async move { history(&client, window).await.map(Arc::new) }) }
}

pub async fn history(client: &ClientHandle, window: HistoryWindow) -> Result<History, String> {
    let value = client.request(model::HISTORY, window.payload()).await.map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}
pub async fn retry(client: &ClientHandle) -> Result<RetryResult, String> {
    let value = client.request(model::RETRY, json!({})).await.map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{Connection, FakeServer};
    use t3_core::commands::method;
    const WAIT: Duration = Duration::from_secs(10);

    async fn connect(server: &mut FakeServer) -> Connection {
        let mut c = server.connection().await;
        let config = c.request(method::SUBSCRIBE_SERVER_CONFIG).await;
        c.send(json!({"_tag":"Exit","requestId":config["id"],"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"message":"no config"}}]}})).await;
        c.request(method::SUBSCRIBE_SHELL).await;
        c
    }
    fn snapshot(at: &str) -> serde_json::Value {
        json!({"readAt":at,"sampleIntervalMs":2000,"processes":[],"groups":{},"power":{},"speedLimitPercent":{"_tag":"None"},"attribution":{},"health":{}})
    }
    async fn next(feed: &mut watch::Receiver<Feed>) -> Feed {
        tokio::time::timeout(WAIT, feed.changed()).await.unwrap().unwrap();
        feed.borrow_and_update().clone()
    }
    #[tokio::test]
    async fn decodes_reconnects_and_interrupts_when_the_page_owner_drops() {
        let mut server = FakeServer::start().await;
        let (client, _events) = crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut first = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client);
        let request = first.request(model::SUBSCRIBE).await;
        assert_eq!(request["payload"], json!({}));
        first.chunk(&request, json!([snapshot("first")])).await;
        assert_eq!(next(&mut stream.feed).await.snapshot.unwrap().read_at, "first");
        drop(first);
        let mut second = connect(&mut server).await;
        let request = second.request(model::SUBSCRIBE).await;
        second.chunk(&request, json!([snapshot("second")])).await;
        assert_eq!(next(&mut stream.feed).await.snapshot.unwrap().read_at, "second");
        drop(stream);
        loop {
            let message = second.recv().await;
            if message["_tag"] == "Interrupt" {
                assert_eq!(message["requestId"], request["id"]);
                break;
            }
        }
    }
    #[tokio::test]
    async fn connection_wide_failures_resume_on_the_next_session() {
        let mut server = FakeServer::start().await;
        let (client, _events) = crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut connection = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client);
        let request = connection.request(model::SUBSCRIBE).await;
        connection.chunk(&request, json!([snapshot("before")])).await;
        assert_eq!(next(&mut stream.feed).await.snapshot.unwrap().read_at, "before");
        for failure in [json!({"_tag":"Defect","defect":{"message":"store unavailable"}}), json!({"_tag":"ClientProtocolError","error":{"message":"bad frame"}})] {
            connection.send(failure).await;
            assert!(next(&mut stream.feed).await.error.is_some());
            connection.closed().await;
            drop(connection);
            connection = connect(&mut server).await;
            let request = connection.request(model::SUBSCRIBE).await;
            connection.chunk(&request, json!([snapshot("recovered")])).await;
            let feed = next(&mut stream.feed).await;
            assert_eq!(feed.snapshot.unwrap().read_at, "recovered");
            assert!(feed.error.is_none());
        }
    }

    async fn interrupted(connection: &mut Connection, request: &serde_json::Value) {
        loop {
            let message = connection.recv().await;
            if message["_tag"] == "Interrupt" {
                assert_eq!(message["requestId"], request["id"]);
                return;
            }
        }
    }

    #[tokio::test]
    async fn replacing_and_dropping_history_demand_cancels_pending_rpcs() {
        let mut server = FakeServer::start().await;
        let (client, _events) = crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut connection = connect(&mut server).await;
        let runtime = tokio::runtime::Handle::current();
        let mut owner = Some(start_history(&runtime, client.clone(), model::HISTORY_WINDOWS[1]));
        let request = connection.request(model::HISTORY).await;
        // Period change: the same owner slot is replaced before the new request starts.
        drop(owner.take());
        interrupted(&mut connection, &request).await;
        owner = Some(start_history(&runtime, client.clone(), model::HISTORY_WINDOWS[0]));
        let request = connection.request(model::HISTORY).await;
        assert_eq!(request["payload"]["windowMs"], 300000);
        // Leave, permission loss, and environment change all drop this owner.
        drop(owner.take());
        interrupted(&mut connection, &request).await;
        // The connection is still usable after cancellation.
        let mut last = start_history(&runtime, client, model::HISTORY_WINDOWS[0]);
        let request = connection.request(model::HISTORY).await;
        connection.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":{"readAt":"now","windowMs":300000,"bucketMs":15000,"sampleIntervalMs":2000,"retainedSampleCount":0,"buckets":[],"topProcesses":[],"health":{}}}})).await;
        assert_eq!(last.result().await.unwrap().window_ms, 300000);
    }

    #[tokio::test]
    async fn reports_terminal_errors_and_decodes_both_rpcs() {
        let mut server = FakeServer::start().await;
        let (client, _events) = crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut c = connect(&mut server).await;
        for (cause, expected) in [
            (json!({"_tag":"Fail","error":{"_tag":"EnvironmentAuthorizationError","message":"not allowed"}}), "not allowed"),
            (json!({"_tag":"Die","defect":{"message":"Unknown request tag: subscribeResourceTelemetry"}}), "Unknown request tag: subscribeResourceTelemetry"),
        ] {
            let mut stream = subscribe(&tokio::runtime::Handle::current(), client.clone());
            let request = c.request(model::SUBSCRIBE).await;
            c.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Failure","cause":[cause]}})).await;
            assert_eq!(next(&mut stream.feed).await.error.as_deref(), Some(expected));
            assert!(tokio::time::timeout(WAIT, stream.feed.changed()).await.unwrap().is_err(), "request-local failure must terminate demand");
        }
        let handle = client.clone();
        let task = tokio::spawn(async move { history(&handle, model::HISTORY_WINDOWS[1]).await });
        let request = c.request(model::HISTORY).await;
        assert_eq!(request["payload"], json!({"windowMs":900000,"bucketMs":30000}));
        c.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":{"readAt":"now","windowMs":900000,"bucketMs":30000,"sampleIntervalMs":2000,"retainedSampleCount":0,"buckets":[],"topProcesses":[],"health":{}}}})).await;
        assert_eq!(task.await.unwrap().unwrap().bucket_ms, 30000);
        let task = tokio::spawn(async move { retry(&client).await });
        let request = c.request(model::RETRY).await;
        assert_eq!(request["payload"], json!({}));
        c.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":{"accepted":false,"snapshot":snapshot("retry")}}})).await;
        assert!(!task.await.unwrap().unwrap().accepted);
    }
}
