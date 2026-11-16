use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use t3_core::composer::attachments::{
    self, CREATE_UPLOAD_URL,
    queue::{
        AttachmentUploadState, DraftTarget, MAX_UPLOADS_PER_ENVIRONMENT,
        PersistedAttachmentVerification, ReadyAttachmentUpload, UploadCycleOutcome,
        UploadCycleStep,
    },
};
use t3_core::media::CREATE_ASSET_URL;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use super::{
    AttachmentUploadQueueClient, QueueEvent, UploadPayload, UploadTransferError,
    run_attachment_upload_cycle, upload_bytes_with_progress, verify_persisted_attachment_upload,
};
use crate::auth::Credentials;
use crate::test_server::FakeServer;

async fn read_http_head_and_body(
    mut stream: tokio::net::TcpStream,
) -> Option<(String, Vec<u8>, tokio::net::TcpStream)> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.ok()?;
        head.push(byte[0]);
    }
    let head_str = String::from_utf8(head).ok()?;
    let content_len = head_str
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_len];
    stream.read_exact(&mut body).await.ok()?;
    Some((head_str, body, stream))
}

struct MockWsConn {
    socket: WebSocketStream<tokio::net::TcpStream>,
}

impl MockWsConn {
    async fn recv_request(&mut self, expected_tag: Option<&str>) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.socket.next())
                .await
                .expect("client sends WS frame")
                .expect("ws open")
                .expect("ws ok");
            let Message::Text(text) = frame else {
                continue;
            };
            let msg: Value = serde_json::from_str(&text).expect("valid json frame");
            if msg["_tag"] == "Ping" {
                let _ = self
                    .socket
                    .send(Message::Text(json!({ "_tag": "Pong" }).to_string()))
                    .await;
                continue;
            }
            if msg["_tag"] == "Request"
                && expected_tag.is_none_or(|tag| msg["tag"].as_str() == Some(tag))
            {
                return msg;
            }
        }
    }

    async fn recv_upload_rpc(&mut self) -> Value {
        loop {
            let msg = self.recv_request(None).await;
            let tag = msg["tag"].as_str();
            if tag == Some(CREATE_UPLOAD_URL) || tag == Some(attachments::DELETE) {
                return msg;
            }
        }
    }

    async fn send_json(&mut self, value: Value) {
        self.socket
            .send(Message::Text(value.to_string()))
            .await
            .expect("send ws json");
    }

    async fn respond_exit_success(&mut self, request: &Value, value: Value) {
        self.send_json(json!({
            "_tag": "Exit",
            "requestId": request["id"],
            "exit": { "_tag": "Success", "value": value }
        }))
        .await;
    }

    async fn respond_exit_fail(&mut self, request: &Value, error: Value) {
        self.send_json(json!({
            "_tag": "Exit",
            "requestId": request["id"],
            "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": error }] }
        }))
        .await;
    }
}

struct MockUploadWsServer {
    http_base: String,
    conns: mpsc::UnboundedReceiver<MockWsConn>,
    uploads: mpsc::UnboundedReceiver<(String, Vec<u8>, oneshot::Sender<u16>)>,
}

impl MockUploadWsServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (conn_tx, conn_rx) = mpsc::unbounded_channel();
        let (upload_tx, upload_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let conn_tx = conn_tx.clone();
                let upload_tx = upload_tx.clone();
                tokio::spawn(async move {
                    let Some((head, body, mut stream)) = read_http_head_and_body(stream).await
                    else {
                        return;
                    };
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    if head.starts_with("POST /api/auth/websocket-ticket") {
                        let resp_body = r#"{"ticket":"t"}"#;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp_body}",
                            resp_body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                        return;
                    }
                    if head.starts_with("GET /api/auth/session") {
                        let resp_body = r#"{"authenticated":true,"role":"owner","grants":["orchestration:operate","filesystem:read"]}"#;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp_body}",
                            resp_body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                        return;
                    }
                    if path.starts_with("/up/") {
                        let (gate_tx, gate_rx) = oneshot::channel();
                        let _ = upload_tx.send((path, body, gate_tx));
                        let status = gate_rx.await.unwrap_or(200);
                        let resp = format!(
                            "HTTP/1.1 {status} Status\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}"
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let key = head.lines().find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("sec-websocket-key")
                            .then(|| v.trim().to_owned())
                    });
                    if let Some(key) = key {
                        let accept = derive_accept_key(key.as_bytes());
                        let upgrade = format!(
                            "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
                        );
                        if stream.write_all(upgrade.as_bytes()).await.is_ok() {
                            let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                            let _ = conn_tx.send(MockWsConn { socket });
                        }
                    }
                });
            }
        });

        Self {
            http_base: format!("http://{addr}"),
            conns: conn_rx,
            uploads: upload_rx,
        }
    }

    fn credentials(&self) -> Credentials {
        Credentials {
            http_base: self.http_base.clone(),
            access_token: String::new(),
        }
    }

    async fn accept_ws(&mut self) -> MockWsConn {
        let mut conn = tokio::time::timeout(Duration::from_secs(5), self.conns.recv())
            .await
            .expect("ws accept")
            .expect("server running");
        let sub = conn
            .recv_request(Some(t3_core::commands::method::SUBSCRIBE_SERVER_CONFIG))
            .await;
        conn.respond_exit_fail(&sub, json!({ "message": "no config" }))
            .await;
        conn
    }
}

#[tokio::test]
async fn raw_http_upload_streams_chunks_reports_progress_and_aborts_on_cancel() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_srv = Arc::clone(&received);
    let (done_tx, done_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (head, body, mut stream) = read_http_head_and_body(stream).await.unwrap();
        assert!(head.contains("content-type: image/png") || head.contains("Content-Type: image/png"));
        *received_srv.lock().unwrap() = body;
        stream
            .write_all(b"HTTP/1.1 201 Created\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let _ = done_tx.send(());
    });

    let payload = vec![0xab; 1024];
    let (_tx, mut cancel_rx) = watch::channel(false);
    let mut progress_samples = Vec::new();
    upload_bytes_with_progress(
        &format!("http://{addr}/upload/item-1"),
        "image/png",
        &payload,
        128,
        Duration::from_secs(5),
        &mut cancel_rx,
        |p| progress_samples.push(p),
    )
    .await
    .unwrap();
    done_rx.await.unwrap();

    assert_eq!(*received.lock().unwrap(), payload);
    assert_eq!(progress_samples.first().copied(), Some(0.0));
    assert_eq!(progress_samples.last().copied(), Some(1.0));
    assert!(progress_samples.len() >= 9);
    assert!(progress_samples.windows(2).all(|w| w[0] <= w[1]));

    // Pre-cancelled signal aborts immediately without touching network.
    let (abort_tx, mut abort_rx) = watch::channel(true);
    drop(abort_tx);
    let err = upload_bytes_with_progress(
        &format!("http://{addr}/upload/never"),
        "text/plain",
        b"hello",
        4,
        Duration::from_secs(5),
        &mut abort_rx,
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err, UploadTransferError::Cancelled);
}

#[tokio::test]
async fn verify_persisted_attachment_maps_verified_missing_and_transient_failed() {
    let mut server = FakeServer::start().await;
    let (client, _events) =
        crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
    let mut conn = server.connection().await;
    let config = conn
        .request(t3_core::commands::method::SUBSCRIBE_SERVER_CONFIG)
        .await;
    conn.send(json!({
        "_tag": "Exit",
        "requestId": config["id"],
        "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": { "message": "no config" } }] }
    }))
    .await;

    let client_ok = client.clone();
    let t_ok = tokio::spawn(async move { verify_persisted_attachment_upload(&client_ok, "att-ok").await });
    let req = conn.request(CREATE_ASSET_URL).await;
    assert_eq!(req["payload"]["resource"]["attachmentId"], "att-ok");
    conn.send(json!({
        "_tag": "Exit",
        "requestId": req["id"],
        "exit": { "_tag": "Success", "value": { "relativeUrl": "/asset/att-ok", "expiresAt": 9999.0 } }
    }))
    .await;
    assert_eq!(t_ok.await.unwrap(), PersistedAttachmentVerification::Verified);

    let client_missing = client.clone();
    let t_missing =
        tokio::spawn(async move { verify_persisted_attachment_upload(&client_missing, "att-gone").await });
    let req = conn.request(CREATE_ASSET_URL).await;
    conn.send(json!({
        "_tag": "Exit",
        "requestId": req["id"],
        "exit": {
            "_tag": "Failure",
            "cause": [{
                "_tag": "Fail",
                "error": { "_tag": "AssetAttachmentNotFoundError", "message": "Attachment not found" }
            }]
        }
    }))
    .await;
    assert_eq!(
        t_missing.await.unwrap(),
        PersistedAttachmentVerification::Missing {
            reason: "Attachment not found".into()
        }
    );

    let client_fail = client.clone();
    let t_fail = tokio::spawn(async move { verify_persisted_attachment_upload(&client_fail, "att-tmp").await });
    let req = conn.request(CREATE_ASSET_URL).await;
    conn.send(json!({
        "_tag": "Exit",
        "requestId": req["id"],
        "exit": {
            "_tag": "Failure",
            "cause": [{ "_tag": "Fail", "error": { "message": "temporary upstream error" } }]
        }
    }))
    .await;
    assert!(matches!(
        t_fail.await.unwrap(),
        PersistedAttachmentVerification::Failed { reason } if reason == "temporary upstream error"
    ));
}

#[tokio::test]
async fn upload_cycle_deletes_minted_id_when_cancelled_after_mint_and_keeps_failed_mint() {
    let mut server = MockUploadWsServer::start().await;
    let (client, _events) =
        crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
    let mut conn = server.accept_ws().await;

    // 1. Cancel inside `on_minted` triggers immediate `attachments.delete`.
    let (_cancel_tx, mut cancel_rx) = watch::channel(false);
    let client_cancel = client.clone();
    let cancel_task = tokio::spawn(async move {
        run_attachment_upload_cycle(
            &client_cancel,
            json!({ "name": "a.txt" }),
            "text/plain",
            b"abc",
            64,
            Duration::from_secs(5),
            &mut cancel_rx,
            |_| false,
            |_| {},
        )
        .await
    });
    let req = conn.recv_request(Some(CREATE_UPLOAD_URL)).await;
    conn.respond_exit_success(&req, json!({ "attachmentId": "mint-cancelled", "relativeUrl": "/up/1" }))
        .await;

    let del = conn.recv_request(Some(attachments::DELETE)).await;
    assert_eq!(del["payload"]["attachmentId"], "mint-cancelled");
    conn.respond_exit_success(&del, Value::Null).await;
    assert_eq!(
        cancel_task.await.unwrap(),
        UploadCycleOutcome::Cancelled {
            minted_attachment_id: None
        }
    );

    // 2. Transfer HTTP 413 preserves `attachment_id: Some(...)` without deleting it.
    let (_tx2, mut rx2) = watch::channel(false);
    let client_fail = client.clone();
    let fail_task = tokio::spawn(async move {
        run_attachment_upload_cycle(
            &client_fail,
            json!({ "name": "big.bin" }),
            "application/octet-stream",
            b"payload",
            64,
            Duration::from_secs(5),
            &mut rx2,
            |_| true,
            |_| {},
        )
        .await
    });
    let req = conn.recv_request(Some(CREATE_UPLOAD_URL)).await;
    conn.respond_exit_success(&req, json!({ "attachmentId": "mint-failed", "relativeUrl": "/up/fail" }))
        .await;
    let (_path, _body, gate) = server.uploads.recv().await.unwrap();
    let _ = gate.send(413);
    assert_eq!(
        fail_task.await.unwrap(),
        UploadCycleOutcome::Failed {
            step: UploadCycleStep::UploadBytes,
            attachment_id: Some("mint-failed".into()),
            error: "Upload rejected (413)".into(),
        }
    );
}

#[tokio::test]
async fn queue_enforces_max_3_per_env_reconnect_retries_and_adopts_on_draft_move() {
    let mut server = MockUploadWsServer::start().await;
    let (client, _client_events) =
        crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
    let mut conn = server.accept_ws().await;

    let (queue, mut events) = AttachmentUploadQueueClient::with_options(
        tokio::runtime::Handle::current(),
        64,
        Duration::from_secs(5),
    );

    for i in 0..4 {
        queue.start_upload(
            format!("job-{i}"),
            DraftTarget {
                draft_id: "new:proj-1".into(),
                file_id: format!("file-{i}"),
            },
            client.clone(),
            UploadPayload {
                request: json!({ "name": format!("f{i}.bin") }),
                mime_type: "application/octet-stream".into(),
                bytes: Some(vec![b'x'; 256]),
                persisted_attachment_id: None,
                current_state: Some(AttachmentUploadState::Ready(ReadyAttachmentUpload {
                    environment_id: client.environment().to_owned(),
                    attachment_id: format!("prev-{i}"),
                })),
            },
        );
    }

    assert_eq!(queue.active_count(client.environment()), MAX_UPLOADS_PER_ENVIRONMENT);
    assert_eq!(queue.queued_count(client.environment()), 1);

    // Mint the first 3 active uploads and hold their HTTP upload gates open.
    let mut http_gates = Vec::new();
    for i in 0..3 {
        let req = conn.recv_request(Some(CREATE_UPLOAD_URL)).await;
        conn.respond_exit_success(
            &req,
            json!({ "attachmentId": format!("srv-{i}"), "relativeUrl": format!("/up/{i}") }),
        )
        .await;
        let (_path, _body, gate) = server.uploads.recv().await.unwrap();
        http_gates.push(gate);
    }

    // Adopt job-3 while queued across a draft move (`new:proj-1` -> `thread:thr-9`).
    assert!(queue.adopt_target(
        "job-3",
        "job-3-moved".into(),
        DraftTarget {
            draft_id: "thread:thr-9".into(),
            file_id: "file-3".into(),
        }
    ));

    // Complete job-0 (200), fail job-1 (500), complete job-2 (200).
    let mut iter = http_gates.into_iter();
    let _ = iter.next().unwrap().send(200);

    let mut saw_delete_prev0 = false;
    let mut saw_mint_3 = false;
    for _ in 0..2 {
        let msg = conn.recv_upload_rpc().await;
        if msg["tag"] == attachments::DELETE {
            assert_eq!(msg["payload"]["attachmentId"], "prev-0");
            saw_delete_prev0 = true;
            conn.respond_exit_success(&msg, Value::Null).await;
        } else {
            assert_eq!(msg["tag"], CREATE_UPLOAD_URL);
            saw_mint_3 = true;
            conn.respond_exit_success(
                &msg,
                json!({ "attachmentId": "srv-3", "relativeUrl": "/up/3" }),
            )
            .await;
        }
    }
    assert!(saw_delete_prev0 && saw_mint_3);

    let _ = iter.next().unwrap().send(500);
    let _ = iter.next().unwrap().send(200);
    let (_path3, _body3, gate3) = server.uploads.recv().await.unwrap();
    let _ = gate3.send(200);

    let mut deleted_prevs = Vec::new();
    while deleted_prevs.len() < 2 {
        let msg = conn.recv_request(Some(attachments::DELETE)).await;
        deleted_prevs.push(msg["payload"]["attachmentId"].as_str().unwrap().to_owned());
        conn.respond_exit_success(&msg, Value::Null).await;
    }
    deleted_prevs.sort();
    assert_eq!(deleted_prevs, vec!["prev-2", "prev-3"]);

    queue
        .await_uploads(&[
            "job-0".into(),
            "job-1".into(),
            "job-2".into(),
            "job-3-moved".into(),
        ])
        .await;

    assert!(matches!(
        queue.state("job-1"),
        Some(AttachmentUploadState::Failed {
            attachment_id: Some(ref id),
            ..
        }) if id == "srv-1"
    ));

    // Force a real WebSocket disconnect + supervisor reconnect so `connection_generation` advances.
    let _ = conn.socket.close(None).await;
    drop(conn);
    let mut conn2 = server.accept_ws().await;

    let mut retry_deleted_srv1 = false;
    let mut retry_minted = false;
    for _ in 0..2 {
        let msg = conn2.recv_upload_rpc().await;
        if msg["tag"] == attachments::DELETE {
            assert_eq!(msg["payload"]["attachmentId"], "srv-1");
            retry_deleted_srv1 = true;
            conn2.respond_exit_success(&msg, Value::Null).await;
        } else {
            assert_eq!(msg["tag"], CREATE_UPLOAD_URL);
            retry_minted = true;
            conn2
                .respond_exit_success(
                    &msg,
                    json!({ "attachmentId": "srv-1-retry", "relativeUrl": "/up/1-retry" }),
                )
                .await;
        }
    }
    assert!(retry_deleted_srv1 && retry_minted);
    let (_p, _b, retry_gate) = server.uploads.recv().await.unwrap();
    let _ = retry_gate.send(200);

    let del_prev1 = conn2.recv_request(Some(attachments::DELETE)).await;
    assert_eq!(del_prev1["payload"]["attachmentId"], "prev-1");
    conn2.respond_exit_success(&del_prev1, Value::Null).await;

    queue.await_uploads(&["job-1".into()]).await;
    assert_eq!(
        queue
            .state("job-1")
            .and_then(|s| s.ready_attachment_id().map(str::to_owned)),
        Some("srv-1-retry".to_owned())
    );
    assert_eq!(
        queue
            .state("job-3-moved")
            .and_then(|s| s.ready_attachment_id().map(str::to_owned)),
        Some("srv-3".to_owned())
    );

    // Releasing the moved draft deletes srv-3 on the server.
    queue.release_draft("thread:thr-9");
    let del3 = conn2.recv_request(Some(attachments::DELETE)).await;
    assert_eq!(del3["payload"]["attachmentId"], "srv-3");
    conn2.respond_exit_success(&del3, Value::Null).await;

    let mut saw_progress = false;
    while let Ok(ev) = events.try_recv() {
        if matches!(ev, QueueEvent::Progress { .. }) {
            saw_progress = true;
        }
    }
    assert!(saw_progress);
}

#[tokio::test]
async fn real_ws_fake_server_completes_queued_upload_and_takes_owned_asset() {
    let mut server = FakeServer::start().await;
    let (client, _client_events) =
        crate::supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
    let mut connection = server.connection().await;
    let config = connection
        .request(t3_core::commands::method::SUBSCRIBE_SERVER_CONFIG)
        .await;
    connection
        .send(json!({
            "_tag": "Exit",
            "requestId": config["id"],
            "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": { "message": "no config" } }] }
        }))
        .await;

    let (queue, _) = AttachmentUploadQueueClient::new(tokio::runtime::Handle::current());
    queue.start_upload(
        "ws-job".into(),
        DraftTarget {
            draft_id: "thread:t1".into(),
            file_id: "f1".into(),
        },
        client,
        UploadPayload {
            request: json!({ "name": "notes.txt", "mimeType": "text/plain", "sizeBytes": 12 }),
            mime_type: "text/plain".into(),
            bytes: Some(b"hello world!".to_vec()),
            persisted_attachment_id: None,
            current_state: None,
        },
    );

    let req = connection.request(CREATE_UPLOAD_URL).await;
    connection
        .send(json!({
            "_tag": "Exit",
            "requestId": req["id"],
            "exit": {
                "_tag": "Success",
                "value": { "attachmentId": "ws-att-1", "relativeUrl": "/test-copy-destination" }
            }
        }))
        .await;

    queue.await_uploads(&["ws-job".into()]).await;
    let owned = queue.take_uploaded_owned("ws-job").expect("owned upload");
    drop(owned);

    let deleted = connection.request(attachments::DELETE).await;
    assert_eq!(deleted["payload"]["attachmentId"], "ws-att-1");
    connection
        .send(json!({
            "_tag": "Exit",
            "requestId": deleted["id"],
            "exit": { "_tag": "Success", "value": null }
        }))
        .await;
}
