//! An in-process fake T3 server for session, supervisor and SSH tests: answers the RPC ticket
//! POST, the token exchange, the session state GET and the root page, and upgrades `/ws` to a
//! WebSocket that speaks Effect RPC JSON frames, on one port.

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
    descriptor: Arc<Mutex<Option<Value>>>,
    refusal: Refusal,
    http: HttpRequests,
}

/// What `/ws` answers instead of upgrading, when set.
type Refusal = Arc<Mutex<Option<(u16, Value)>>>;
type HttpRequests = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

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
        let descriptor = Arc::new(Mutex::new(None));
        let described = descriptor.clone();
        let refusal: Refusal = Arc::new(Mutex::new(None));
        let refused = refusal.clone();
        let http=Arc::new(Mutex::new(Vec::new()));
        let requests=http.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (tx, session, descriptor, refusal) = (tx.clone(), served.clone(), described.clone(), refused.clone());
                let http=requests.clone();
                tokio::spawn(async move {
                    if let Some(connection) = serve(stream, session, descriptor, refusal, http).await {
                        let _ = tx.send(connection);
                    }
                });
            }
        });
        FakeServer { addr, connections, session, descriptor, refusal, http }
    }

    /// `/ws` answers this status and JSON body instead of upgrading; `None` upgrades again.
    pub fn refuse_upgrades(&self, refusal: Option<(u16, Value)>) {
        *self.refusal.lock().expect("refusal") = refusal;
    }

    /// What `GET /.well-known/t3/environment` answers from now on; `None` answers 404.
    pub fn set_descriptor(&self, descriptor: Option<Value>) {
        *self.descriptor.lock().expect("descriptor") = descriptor;
    }

    /// What `GET /api/auth/session` answers from now on.
    pub fn set_session(&self, state: Value) {
        *self.session.lock().expect("session") = state;
    }

    pub fn http_requests(&self) -> Vec<(String,Vec<u8>)> {self.http.lock().expect("HTTP requests").clone()}

    pub fn port(&self) -> u16 {
        self.addr.port()
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

async fn respond_json(stream: TcpStream, body: Value) {
    respond_status(stream, 200, body).await;
}

async fn respond_status(mut stream: TcpStream, status: u16, body: Value) {
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn serve(mut stream: TcpStream, session: Arc<Mutex<Value>>, descriptor: Arc<Mutex<Option<Value>>>, refusal: Refusal, http: HttpRequests) -> Option<Connection> {
    let head = read_head(&mut stream).await?;
    let path = head.split_whitespace().nth(1)?.to_owned();
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
        })
    };
    if path=="/test-copy-source" || path=="/test-copy-destination" {
        let mut body=vec![0;header("content-length").and_then(|n|n.parse::<usize>().ok()).unwrap_or(0)];
        stream.read_exact(&mut body).await.ok()?;
        http.lock().expect("HTTP requests").push((path.clone(),body));
        let bytes=if path=="/test-copy-source" {b"source bytes".as_slice()}else{b"{}".as_slice()};
        stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",bytes.len()).as_bytes()).await.ok()?;
        stream.write_all(bytes).await.ok()?; return None;
    }
    if head.starts_with("POST /api/auth/websocket-ticket") {
        respond_json(stream, json!({ "ticket": "ticket" })).await;
        return None;
    }
    if head.starts_with("GET /.well-known/t3/environment") {
        let descriptor = descriptor.lock().expect("descriptor").clone();
        match descriptor {
            Some(descriptor) => respond_json(stream, descriptor).await,
            None => {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
            }
        }
        return None;
    }
    if head.starts_with("POST /oauth/token") {
        respond_json(stream, json!({ "access_token": "ssh-access", "token_type": "Bearer" })).await;
        return None;
    }
    // The page a readiness probe asks for.
    if head.starts_with("GET / ") {
        respond_json(stream, json!({})).await;
        return None;
    }
    if head.starts_with("GET /api/auth/session") {
        let state = session.lock().expect("session").clone();
        respond_json(stream, state).await;
        return None;
    }
    let refused = refusal.lock().expect("refusal").clone();
    if let Some((status, body)) = refused {
        respond_status(stream, status, body).await;
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

    pub async fn closed(&mut self) {
        tokio::time::timeout(WAIT, async {
            while let Some(frame) = self.socket.next().await {
                if matches!(frame, Ok(Message::Close(_)) | Err(_)) { return; }
            }
        }).await.expect("dropping the last handle closes the connection");
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
