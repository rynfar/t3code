//! An in-process fake T3 server for session and supervisor tests: answers the RPC ticket POST and
//! the session state GET, and upgrades `/ws` to a WebSocket that speaks Effect RPC JSON frames,
//! on one port.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::auth::Credentials;

/// Guards every wait so a broken expectation fails instead of hanging the suite.
const WAIT: Duration = Duration::from_secs(10);

pub struct FakeServer {
    addr: SocketAddr,
    connections: mpsc::UnboundedReceiver<Connection>,
    session: Arc<Mutex<Value>>,
}

pub struct Connection {
    pub path: String,
    socket: WebSocketStream<TcpStream>,
}

impl FakeServer {
    pub async fn start() -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let (tx, connections) = mpsc::unbounded_channel();
        let session = Arc::new(Mutex::new(json!({"authenticated": true, "auth": {}, "permissions": ["terminal:operate"]})));
        let served = session.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (tx, session) = (tx.clone(), served.clone());
                tokio::spawn(async move {
                    if let Some(connection) = serve(stream, session).await {
                        let _ = tx.send(connection);
                    }
                });
            }
        });
        FakeServer { addr, connections, session }
    }

    /// What `GET /api/auth/session` answers from now on.
    pub fn set_session(&self, state: Value) {
        *self.session.lock().expect("session") = state;
    }

    pub fn credentials(&self) -> Credentials {
        Credentials { http_base: format!("http://{}", self.addr), access_token: "token".into() }
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}/ws", self.addr)
    }

    pub async fn connection(&mut self) -> Connection {
        tokio::time::timeout(WAIT, self.connections.recv())
            .await
            .expect("a client connects")
            .expect("server is running")
    }
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.ok()?;
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

async fn respond_json(mut stream: TcpStream, body: Value) {
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn serve(mut stream: TcpStream, session: Arc<Mutex<Value>>) -> Option<Connection> {
    let head = read_head(&mut stream).await?;
    let path = head.split_whitespace().nth(1)?.to_owned();
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
        })
    };
    if head.starts_with("POST /api/auth/websocket-ticket") {
        respond_json(stream, json!({ "ticket": "ticket" })).await;
        return None;
    }
    if head.starts_with("GET /api/auth/session") {
        let state = session.lock().expect("session").clone();
        respond_json(stream, state).await;
        return None;
    }
    let accept = derive_accept_key(header("sec-websocket-key")?.as_bytes());
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
    );
    stream.write_all(response.as_bytes()).await.ok()?;
    let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
    Some(Connection { path, socket })
}

impl Connection {
    /// The next client message other than `Ping`.
    pub async fn recv(&mut self) -> Value {
        loop {
            let frame = tokio::time::timeout(WAIT, self.socket.next())
                .await
                .expect("the client sends a message")
                .expect("socket is open")
                .expect("frame decodes");
            let Message::Text(text) = frame else { continue };
            let message: Value = serde_json::from_str(&text).expect("client frames are JSON");
            if message["_tag"] != "Ping" {
                return message;
            }
        }
    }

    /// Requests in arrival order until one has this tag.
    pub async fn request(&mut self, tag: &str) -> Value {
        loop {
            let message = self.recv().await;
            if message["_tag"] == "Request" && message["tag"] == tag {
                return message;
            }
        }
    }

    pub async fn send(&mut self, message: Value) {
        self.socket.send(Message::Text(message.to_string())).await.expect("send");
    }

    pub async fn chunk(&mut self, request: &Value, values: Value) {
        self.send(json!({ "_tag": "Chunk", "requestId": request["id"], "values": values })).await;
    }
}
