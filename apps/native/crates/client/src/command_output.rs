//! One expanded command row's live output: `orchestration.subscribeCommandOutput`, as the
//! `commandOutput` subscription in `client-runtime/src/state/orchestration.ts` runs it. Frames are
//! decoded and folded (`applyCommandOutputFrame`) here on the network runtime, and the row reads
//! only the newest result from a `watch`, so a slow row never queues frames and holds at most
//! one bounded tail. The server ends the stream once the command settles; a dropped connection
//! resubscribes, and the server starts over with a `replace`. Dropping the handle interrupts it.

use std::time::Duration;

use t3_core::command_output::{CommandOutput, CommandOutputFrame, CommandOutputStatus, CommandOutputView, SUBSCRIBE_COMMAND_OUTPUT, error_message, subscribe_request};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::session::RpcError;
use crate::supervisor::ClientHandle;

pub struct CommandOutputStream {
    pub status: watch::Receiver<CommandOutputStatus>,
    task: JoinHandle<()>,
}

impl Drop for CommandOutputStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn subscribe(runtime: &tokio::runtime::Handle, client: ClientHandle, thread_id: &str, item_id: &str) -> CommandOutputStream {
    let (tx, status) = watch::channel(CommandOutputStatus::default());
    let task = runtime.spawn(run(client, subscribe_request(thread_id, item_id), tx));
    CommandOutputStream { status, task }
}

enum End {
    /// The server finished the stream, or failed it; nothing resubscribes.
    Done,
    /// The connection dropped.
    Lost,
}

async fn run(client: ClientHandle, payload: serde_json::Value, tx: watch::Sender<CommandOutputStatus>) {
    let mut view = CommandOutputView::default();
    let mut attempt = 0u32;
    let fail = |message: &str| tx.send_modify(|status| status.error = Some(error_message(message)));
    loop {
        let end = match client.stream(SUBSCRIBE_COMMAND_OUTPUT, payload.clone()).await {
            Ok(mut stream) => {
                let mut end = End::Done;
                while let Some(first) = stream.receiver.recv().await {
                    // Fold everything already queued before publishing once.
                    let mut batch = vec![first];
                    while let Ok(next) = stream.receiver.try_recv() {
                        batch.push(next);
                    }
                    let mut changed = false;
                    let mut ended = None;
                    for item in batch {
                        match item {
                            Ok(value) => match serde_json::from_value::<CommandOutputFrame>(value) {
                                Ok(frame) => {
                                    attempt = 0;
                                    view.apply(&frame);
                                    changed = true;
                                }
                                Err(error) => tracing::warn!(%error, "undecodable command output frame"),
                            },
                            Err(RpcError::Failure(failure)) => ended = Some(Err(failure.message)),
                            Err(RpcError::Closed) => ended = Some(Ok(())),
                        }
                        if ended.is_some() {
                            break;
                        }
                    }
                    if changed {
                        tx.send_modify(|status| status.output = Some(CommandOutput::from(&view)));
                    }
                    match ended {
                        Some(Err(message)) => {
                            fail(&message);
                            return;
                        }
                        Some(Ok(())) => {
                            end = End::Lost;
                            break;
                        }
                        None if !view.running => return,
                        None => {}
                    }
                }
                end
            }
            Err(RpcError::Failure(failure)) => {
                fail(&failure.message);
                End::Done
            }
            Err(RpcError::Closed) => End::Lost,
        };
        if matches!(end, End::Done) {
            return;
        }
        attempt += 1;
        tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(5)).min(5_000))).await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use t3_core::commands::method;

    use super::*;
    use crate::supervisor::start;
    use crate::test_server::{Connection, FakeServer};

    const WAIT: Duration = Duration::from_secs(10);

    async fn connect(server: &mut FakeServer) -> Connection {
        let mut connection = server.connection().await;
        let config = connection.request(method::SUBSCRIBE_SERVER_CONFIG).await;
        let failure = json!({"_tag": "Failure", "cause": [{"_tag": "Fail", "error": {"message": "no config"}}]});
        connection.send(json!({"_tag": "Exit", "requestId": config["id"], "exit": failure})).await;
        connection.request(method::SUBSCRIBE_SHELL).await;
        connection
    }

    fn frame(kind: &str, text: &str, running: bool) -> Value {
        json!([{ "kind": kind, "text": text, "truncated": false, "running": running }])
    }

    async fn next(status: &mut watch::Receiver<CommandOutputStatus>) -> CommandOutputStatus {
        tokio::time::timeout(WAIT, status.changed()).await.expect("an update").expect("the stream is live");
        status.borrow_and_update().clone()
    }

    fn text(status: &CommandOutputStatus) -> (&str, bool) {
        let output = status.output.as_ref().expect("output");
        (&output.text, output.running)
    }

    #[tokio::test]
    async fn folds_frames_until_the_command_settles_then_stops() {
        let mut server = FakeServer::start().await;
        let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
        let mut connection = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client, "t1", "item-1");
        let request = connection.request(SUBSCRIBE_COMMAND_OUTPUT).await;
        assert_eq!(request["payload"], json!({ "threadId": "t1", "itemId": "item-1" }));

        connection.chunk(&request, frame("replace", "12%\u{1b}[3", true)).await;
        assert_eq!(text(&next(&mut stream.status).await), ("12%", true));
        connection.chunk(&request, frame("append", "2m\r100%\n", true)).await;
        assert_eq!(text(&next(&mut stream.status).await), ("\u{1b}[0;32m100%\u{1b}[0m\n", true));
        connection.chunk(&request, frame("replace", "done\n", false)).await;
        assert_eq!(text(&next(&mut stream.status).await), ("done\n", false));
        // Settled: the task ends without resubscribing, which closes the channel.
        assert!(tokio::time::timeout(WAIT, stream.status.changed()).await.expect("the task ends").is_err());
    }

    #[tokio::test]
    async fn collapsing_interrupts_and_a_lost_connection_resubscribes() {
        let mut server = FakeServer::start().await;
        let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
        let mut first = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client, "t1", "item-1");
        let request = first.request(SUBSCRIBE_COMMAND_OUTPUT).await;
        first.chunk(&request, frame("replace", "before\n", true)).await;
        assert_eq!(text(&next(&mut stream.status).await), ("before\n", true));
        drop(first);

        // The new session starts over with a replace; what was shown stays until it lands.
        let mut second = connect(&mut server).await;
        let request = second.request(SUBSCRIBE_COMMAND_OUTPUT).await;
        assert_eq!(text(&stream.status.borrow()), ("before\n", true));
        second.chunk(&request, frame("replace", "after\n", true)).await;
        assert_eq!(text(&next(&mut stream.status).await), ("after\n", true));

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
    async fn a_server_failure_is_reported_and_not_retried() {
        let mut server = FakeServer::start().await;
        let (client, _events) = start(&tokio::runtime::Handle::current(), server.credentials());
        let mut connection = connect(&mut server).await;
        let mut stream = subscribe(&tokio::runtime::Handle::current(), client, "t1", "item-1");
        let request = connection.request(SUBSCRIBE_COMMAND_OUTPUT).await;
        let error = json!({"_tag": "OrchestrationV2CommandOutputError", "threadId": "t1", "message": "No such command."});
        connection.send(json!({"_tag": "Exit", "requestId": request["id"], "exit": {"_tag": "Failure", "cause": [{"_tag": "Fail", "error": error}]}})).await;
        let status = next(&mut stream.status).await;
        assert_eq!((status.output, status.error.as_deref()), (None, Some("No such command.")));
        assert!(tokio::time::timeout(WAIT, stream.status.changed()).await.expect("the task ends").is_err());
    }
}
