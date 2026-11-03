//! CLI OAuth from server/cloud/CliTokenManager.ts and bearer listing from relay/managedRelay.ts.
//! Relay health/connection endpoints require a JWT-only DPoP exchange; CLI OAuth cannot use it.
use rand::RngCore;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use t3_core::connect::{self, Callback, Token, TokenResponse};
use t3_core::relay::{Discovered, Discovery, Environment};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex, oneshot},
};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}
fn http() -> &'static reqwest::Client {
    static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client")
    })
}
fn random<const N: usize>() -> [u8; N] {
    let mut b = [0; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}
#[derive(Clone)]
pub struct Config {
    pub hosted: String,
    pub relay: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub port: u16,
}
impl Config {
    pub fn load() -> Result<Self, String> {
        let value = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into()).trim().to_owned();
        let relay = value("T3CODE_RELAY_URL", "https://relay.t3.codes");
        let hosted = value("T3CODE_HOSTED_APP_URL", "https://app.t3.codes");
        for origin in [&relay, &hosted] {
            let url = url::Url::parse(origin).map_err(|_| "Invalid T3 Connect origin.")?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("T3 Connect requires a secure absolute HTTPS origin.".into());
            }
        }
        let key = value("T3CODE_CLERK_PUBLISHABLE_KEY", "pk_live_Y2xlcmsudDMuY29kZXMk");
        let origin = connect::clerk_origin(&key).ok_or("Invalid Clerk publishable key.")?;
        let client_id = value("T3CODE_CLERK_CLI_OAUTH_CLIENT_ID", "hzxSgY2cH10sDU2r");
        if client_id.is_empty() {
            return Err("T3 Connect OAuth client ID is not configured.".into());
        }
        Ok(Self {
            relay: relay.trim_end_matches('/').into(),
            hosted,
            token_endpoint: format!("{origin}/oauth/token"),
            client_id,
            port: 34338,
        })
    }
}
struct Account {
    loaded: bool,
    token: Option<Token>,
    generation: u64,
}
pub struct Cloud {
    config: Result<Config, String>,
    account: Arc<Mutex<Account>>,
    home: Option<std::path::PathBuf>,
}
impl Cloud {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            config: Config::load(),
            home: crate::store::base(),
            account: Arc::new(Mutex::new(Account {
                loaded: false,
                token: None,
                generation: 0,
            })),
        })
    }
    fn home(&self) -> Result<&std::path::Path, String> {
        self.home.as_deref().ok_or_else(|| "No native credential storage directory.".into())
    }
    fn config(&self) -> Result<&Config, String> {
        self.config.as_ref().map_err(Clone::clone)
    }
    pub async fn identity(&self) -> Result<Option<String>, String> {
        let mut account = self.account.lock().await;
        if !account.loaded {
            account.token = crate::store::load_cloud_token(self.home()?).map_err(|_| "Could not read the stored T3 Connect credential.")?;
            account.loaded = true;
        }
        Ok(account
            .token
            .as_ref()
            .map(|t| t.identity.clone().unwrap_or_else(|| "T3 Connect account".into())))
    }
    async fn token(&self) -> Result<(String, u64), String> {
        self.identity().await?;
        let account = self.account.clone();
        let config = self.config()?.clone();
        let home = self.home()?.to_owned();
        // Refresh rotation and persistence finish even if the caller cancels its discovery.
        tokio::spawn(async move {
            let mut account = account.lock().await;
            let token = account.token.as_ref().ok_or("Sign in to T3 Connect before loading relay data.")?;
            if token.needs_refresh(now_ms()) {
                let params = [
                    ("grant_type", "refresh_token"),
                    ("refresh_token", token.refresh_token.as_str()),
                    ("client_id", config.client_id.as_str()),
                ];
                let response = http()
                    .post(&config.token_endpoint)
                    .form(&params)
                    .send()
                    .await
                    .map_err(|_| "Could not refresh the T3 Connect credential.")?;
                if !response.status().is_success() {
                    return Err(if connect::permanent_refresh_failure(response.status().as_u16()) {
                        "Could not refresh the T3 Connect credential. Sign out and sign in again."
                    } else {
                        "Could not refresh the T3 Connect credential. Try again."
                    }
                    .into());
                }
                let next = response
                    .json::<TokenResponse>()
                    .await
                    .map_err(|_| "Invalid T3 Connect token response.")?
                    .into_token(Some(token), now_ms())?;
                crate::store::save_cloud_token(&home, &next).map_err(|_| "Could not store the T3 Connect credential.")?;
                account.token = Some(next);
            }
            Ok((account.token.as_ref().expect("signed in").access_token.clone(), account.generation))
        })
        .await
        .map_err(|_| "Could not refresh the T3 Connect credential.".to_owned())?
    }
    async fn assert_account(&self, generation: u64) -> Result<(), String> {
        let account = self.account.lock().await;
        if generation != account.generation || account.token.is_none() {
            return Err("The T3 Connect account changed. Try again.".into());
        }
        Ok(())
    }
    pub async fn sign_out(&self) -> Result<(), String> {
        let mut account = self.account.lock().await;
        account.generation += 1;
        account.token = None;
        account.loaded = true;
        let result = crate::store::clear_cloud_token(self.home()?).map_err(|_| "Could not remove the stored T3 Connect credential.".into());
        drop(account);
        result
    }
    pub async fn sign_in(&self, browser: oneshot::Sender<String>) -> Result<String, String> {
        let config = self.config()?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.port))
            .await
            .map_err(|_| "Could not listen for T3 Connect authorization on 127.0.0.1:34338. Close another sign-in attempt and try again.")?;
        let port = listener.local_addr().map_err(|_| "Could not read the callback listener address.")?.port();
        let generation = self.account.lock().await.generation;
        let verifier = connect::encode(&random::<32>());
        let state = connect::encode(&random::<16>());
        let url = connect::authorize_url(&config.hosted, &state, &connect::challenge(&verifier), port).map_err(|_| "Invalid T3 Connect authorization URL.")?;
        browser.send(url).map_err(|_| "T3 Connect sign-in was cancelled.")?;
        let code = tokio::time::timeout(Duration::from_secs(600), receive_callback(listener, &state))
            .await
            .map_err(|_| "Timed out waiting for T3 Connect authorization.")??;
        let redirect = format!("http://127.0.0.1:{port}/callback");
        let response = http()
            .post(&config.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", &redirect),
                ("client_id", &config.client_id),
                ("code_verifier", &verifier),
            ])
            .send()
            .await
            .map_err(|_| "Could not authorize T3 Connect.")?;
        if !response.status().is_success() {
            return Err("Could not authorize T3 Connect. Try signing in again.".into());
        }
        let token = response
            .json::<TokenResponse>()
            .await
            .map_err(|_| "Invalid T3 Connect token response.")?
            .into_token(None, now_ms())?;
        let identity = token.identity.clone().unwrap_or_else(|| "T3 Connect account".into());
        let mut account = self.account.lock().await;
        if account.generation != generation {
            return Err("T3 Connect sign-in was cancelled.".into());
        }
        crate::store::save_cloud_token(self.home()?, &token).map_err(|_| "Could not store the T3 Connect credential.")?;
        account.token = Some(token);
        account.loaded = true;
        account.generation += 1;
        Ok(identity)
    }
    pub async fn discover(&self) -> Result<Discovery, String> {
        let config = self.config()?;
        let (token, generation) = self.token().await?;
        let response = http()
            .get(format!("{}/v1/client/environments", config.relay))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| "Could not load T3 Connect environments.")?;
        #[derive(Deserialize)]
        struct List {
            environments: Vec<Environment>,
        }
        let list: List = serde_json::from_value(relay_response(response).await?).map_err(|_| "Invalid T3 Connect environment list.")?;
        let environments = list
            .environments
            .into_iter()
            .map(|environment| Discovered {
                environment,
                status: None,
                error: None,
            })
            .collect();
        self.assert_account(generation).await?;
        Ok(Discovery {
            listed: true,
            environments,
            ..Default::default()
        })
    }
}
async fn relay_response(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let value = response.json::<Value>().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(t3_core::relay::error_message(status.as_u16(), &value));
    }
    Ok(value)
}
async fn receive_callback(listener: TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|_| "T3 Connect callback listener stopped.")?;
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            let mut bytes = Vec::new();
            let mut buffer = [0; 1024];
            while bytes.len() < 8192 {
                let n = stream.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Ok::<_, std::io::Error>(bytes)
        })
        .await;
        let Ok(Ok(request)) = request else { continue };
        let text = String::from_utf8_lossy(&request);
        let mut words = text.split_whitespace();
        let method = words.next();
        let target = words.next().unwrap_or("");
        let callback = if method == Some("GET") {
            connect::callback(target, state)
        } else {
            Callback::Invalid
        };
        let (status, body) = match callback {
            Callback::Invalid => ("400 Bad Request", "Invalid T3 Connect authorization callback."),
            Callback::Denied => ("200 OK", "T3 Connect authorization was denied. Return to T3 Code Native."),
            Callback::Code(_) => ("200 OK", "T3 Connect authorized. You can close this window and return to T3 Code Native."),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.write_all(response.as_bytes())).await;
        match callback {
            Callback::Code(code) => return Ok(code),
            Callback::Denied => return Err("T3 Connect authorization was denied in the browser.".into()),
            Callback::Invalid => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::TcpStream;

    struct Fixture {
        base: String,
        requests: tokio::sync::mpsc::UnboundedReceiver<String>,
        replies: tokio::sync::mpsc::UnboundedSender<(u16, Value)>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Fixture {
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let (request_tx, requests) = tokio::sync::mpsc::unbounded_channel();
            let (replies, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<(u16, Value)>();
            let task = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let request = read_request(&mut socket).await;
                    let jwt_only_exchange = request.starts_with("POST /v1/client/dpop-token ");
                    if request_tx.send(request).is_err() {
                        return;
                    }
                    // Api.ts rejects CLI OAuth on this endpoint, unlike bearer account listing.
                    let (status, value) = if jwt_only_exchange {
                        (401, json!({"_tag":"RelayAuthInvalidError","reason":"invalid_bearer"}))
                    } else {
                        let Some(reply) = reply_rx.recv().await else { return };
                        reply
                    };
                    let body = value.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            });
            Self { base, requests, replies, task }
        }
        fn reply(&self, status: u16, value: Value) {
            self.replies.send((status, value)).unwrap();
        }
        async fn request(&mut self) -> String {
            self.requests.recv().await.unwrap()
        }
    }
    async fn read_request(socket: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buf = [0; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
            if let Some(head) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let text = String::from_utf8_lossy(&bytes[..head]);
                let length = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= head + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8(bytes).unwrap()
    }
    fn cloud(fixture: &Fixture) -> Arc<Cloud> {
        Arc::new(Cloud {
            config: Ok(Config {
                hosted: "https://app.t3.codes".into(),
                relay: fixture.base.clone(),
                token_endpoint: format!("{}/oauth/token", fixture.base),
                client_id: "client".into(),
                port: 0,
            }),
            home: Some(std::env::temp_dir().join(format!("t3-connect-test-{}", uuid::Uuid::new_v4()))),
            account: Arc::new(Mutex::new(Account {
                loaded: true,
                token: None,
                generation: 0,
            })),
        })
    }
    fn token(expires: u64) -> Token {
        Token {
            access_token: "fixture-access".into(),
            refresh_token: "fixture-refresh".into(),
            expires_at_epoch_ms: expires,
            identity: Some("Fixture account".into()),
        }
    }
    #[tokio::test]
    async fn browser_callback_requires_state_then_exchanges_pkce_and_clears_private_storage() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        fixture.reply(
            200,
            json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_in":3600,"token_type":"Bearer","future":true}),
        );
        let (tx, rx) = oneshot::channel();
        let login = {
            let c = cloud.clone();
            tokio::spawn(async move { c.sign_in(tx).await })
        };
        let url = url::Url::parse(&rx.await.unwrap()).unwrap();
        let params: std::collections::HashMap<_, _> = url::form_urlencoded::parse(url.fragment().unwrap().as_bytes()).into_owned().collect();
        let callback = format!("http://127.0.0.1:{}/callback", params["port"]);
        assert_eq!(http().get(format!("{callback}?state=wrong&code=bad")).send().await.unwrap().status(), 400);
        assert!(
            http()
                .get(format!("{callback}?state={}&code=fixture-code", params["state"]))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(login.await.unwrap().is_ok());
        let request = fixture.request().await;
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["redirect_uri"], callback);
        assert_eq!(connect::challenge(&form["code_verifier"]), params["challenge"]);
        let stored = crate::store::load_cloud_token(cloud.home().unwrap()).unwrap().unwrap();
        assert_eq!(stored.refresh_token, "fixture-refresh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(cloud.home().unwrap().join("cloud-oauth.json")).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        cloud.sign_out().await.unwrap();
        assert!(cloud.identity().await.unwrap().is_none());
        assert!(!cloud.home().unwrap().join("cloud-oauth.json").exists());
        std::fs::remove_dir_all(cloud.home().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn refresh_rotation_is_serialized_and_survives_a_cancelled_caller() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(0));
        let first = {
            let c = cloud.clone();
            tokio::spawn(async move { c.token().await })
        };
        assert!(fixture.request().await.contains("grant_type=refresh_token"));
        first.abort();
        fixture.reply(
            200,
            json!({"access_token":"rotated","refresh_token":"rotated-refresh","expires_in":3600,"token_type":"Bearer"}),
        );
        // The second reader waits for the first refresh's persistence under the same lock.
        assert_eq!(cloud.token().await.unwrap().0, "rotated");
        let stored = crate::store::load_cloud_token(cloud.home().unwrap()).unwrap().unwrap();
        assert_eq!(stored.refresh_token, "rotated-refresh");
        assert_eq!(stored.identity.as_deref(), Some("Fixture account"));
        cloud.sign_out().await.unwrap();
        std::fs::remove_dir_all(cloud.home().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn oauth_discovery_only_lists_and_never_attempts_jwt_only_health_or_connection_endpoints() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(u64::MAX));
        let environment = json!({"environmentId":"env","label":"Mac","endpoint":{"httpBaseUrl":"https://old.test","wsBaseUrl":"wss://old.test/ws","providerKind":"cloudflare_tunnel"},"future":true});
        fixture.reply(200, json!({"environments":[environment]}));
        let discovery = cloud.discover().await.unwrap();
        let listing = fixture.request().await;
        assert!(listing.starts_with("GET /v1/client/environments"));
        assert!(listing.contains("Bearer fixture-access"));
        assert!(fixture.requests.try_recv().is_err(), "CLI OAuth must not call the JWT-only token exchange");
        assert_eq!(discovery.environments.len(), 1);
        assert!(discovery.environments[0].status.is_none());
        assert!(discovery.environments[0].error.is_none());
        let mut next = environment;
        next["endpoint"]["httpBaseUrl"] = json!("https://reallocated.test");
        next["endpoint"]["wsBaseUrl"] = json!("wss://reallocated.test/ws");
        fixture.reply(200, json!({"environments":[next]}));
        let refreshed = cloud.discover().await.unwrap();
        assert_eq!(refreshed.environments[0].environment.endpoint.http_base_url, "https://reallocated.test");
        assert!(fixture.request().await.starts_with("GET /v1/client/environments"));
        assert!(fixture.requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn discovery_rejects_a_list_that_finishes_after_sign_out() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(u64::MAX));
        let discovery = {
            let cloud = cloud.clone();
            tokio::spawn(async move { cloud.discover().await })
        };
        assert!(fixture.request().await.starts_with("GET /v1/client/environments"));
        cloud.sign_out().await.unwrap();
        fixture.reply(200, json!({"environments":[]}));
        assert!(discovery.await.unwrap().is_err());
        assert!(cloud.identity().await.unwrap().is_none());
    }
}
