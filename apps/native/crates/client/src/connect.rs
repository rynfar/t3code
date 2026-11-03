//! Native I/O for server/cloud/CliTokenManager.ts and client-runtime/relay/managedRelay.ts.
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use t3_core::connect::{self, Callback, Token, TokenResponse};
use t3_core::relay::{Discovered, Discovery, Endpoint, Environment, ProofKey, Status};
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
struct RelayAccess {
    token: String,
    expires: u64,
    generation: u64,
}
pub struct Cloud {
    config: Result<Config, String>,
    account: Arc<Mutex<Account>>,
    home: Option<std::path::PathBuf>,
    relay_access: Mutex<Option<RelayAccess>>,
    key: ProofKey,
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
            relay_access: Mutex::new(None),
            key: ProofKey::from_bytes(&random()).expect("random P-256 key"),
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
        *self.relay_access.lock().await = None;
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
    pub fn proof(&self, method: &str, url: &str, token: Option<&str>) -> Result<String, String> {
        self.key
            .proof(method, url, token, now_ms() / 1000, &uuid::Uuid::new_v4().to_string())
            .map_err(|_| "Could not create the DPoP proof.".into())
    }
    async fn relay_token(&self) -> Result<(String, u64), String> {
        let config = self.config()?;
        let (subject, generation) = self.token().await?;
        let mut cached = self.relay_access.lock().await;
        if let Some(access) = cached.as_ref().filter(|a| a.generation == generation && a.expires > now_ms() + 5_000) {
            return Ok((access.token.clone(), generation));
        }
        let exchange = format!("{}/v1/client/dpop-token", config.relay);
        let scopes = "environment:status environment:connect";
        let response = http()
            .post(&exchange)
            .header("dpop", self.proof("POST", &exchange, None)?)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
                ("subject_token", &subject),
                ("subject_token_type", "urn:ietf:params:oauth:token-type:jwt"),
                ("requested_token_type", "urn:ietf:params:oauth:token-type:access_token"),
                ("resource", &config.relay),
                ("scope", scopes),
                ("client_id", "t3-web"),
            ])
            .send()
            .await
            .map_err(|_| "Could not reach T3 Connect.")?;
        let access = relay_response(response).await?;
        if access["token_type"] != "DPoP" || !scope_matches(access["scope"].as_str().unwrap_or("")) {
            return Err("Relay returned unexpected access token scopes or type.".into());
        }
        let token = access["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or("Invalid relay token response.")?
            .to_owned();
        let expires = access["expires_in"].as_u64().filter(|e| *e > 0).ok_or("Invalid relay token expiry.")?;
        self.assert_account(generation).await?;
        *cached = Some(RelayAccess {
            token: token.clone(),
            expires: now_ms().saturating_add(expires.saturating_mul(1000)),
            generation,
        });
        Ok((token, generation))
    }
    async fn relay_request(&self, path: &str, payload: Option<Value>) -> Result<Value, String> {
        let url = format!("{}{path}", self.config()?.relay);
        for attempt in 0..2 {
            let (token, generation) = self.relay_token().await?;
            let request = http()
                .post(&url)
                .header("authorization", format!("DPoP {token}"))
                .header("dpop", self.proof("POST", &url, Some(&token))?);
            let request = if let Some(payload) = &payload { request.json(payload) } else { request };
            let response = request.send().await.map_err(|_| "Could not reach T3 Connect.")?;
            let status = response.status();
            let value = response.json::<Value>().await.unwrap_or(Value::Null);
            self.assert_account(generation).await?;
            if !status.is_success() {
                if attempt == 0 && value["_tag"] == "RelayAuthInvalidError" && value["reason"] == "invalid_bearer" {
                    let mut cache = self.relay_access.lock().await;
                    if cache.as_ref().is_some_and(|a| a.token == token) {
                        *cache = None;
                    }
                    continue;
                }
                return Err(t3_core::relay::error_message(status.as_u16(), &value));
            }
            return Ok(value);
        }
        Err("Relay rejected the cloud session token.".into())
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
        let environments = futures::stream::iter(list.environments.into_iter().map(|environment| async move {
            let result = async {
                let value = self.relay_request(&environment_path(&environment.environment_id, "status"), None).await?;
                let status: Status = serde_json::from_value(value).map_err(|_| "Invalid relay status response.")?;
                status.validate(&environment)?;
                Ok::<_, String>(status)
            }
            .await;
            match result {
                Ok(status) => Discovered {
                    environment,
                    status: Some(status),
                    error: None,
                },
                Err(error) => Discovered {
                    environment,
                    status: None,
                    error: Some(error),
                },
            }
        }));
        use futures::StreamExt;
        let environments = environments.buffered(8).collect().await;
        self.assert_account(generation).await?;
        Ok(Discovery {
            listed: true,
            environments,
            ..Default::default()
        })
    }
}
fn environment_path(id: &str, action: &str) -> String {
    let mut url = url::Url::parse("https://relay.invalid/v1/environments/").expect("constant URL");
    url.path_segments_mut().expect("base URL").pop_if_empty().push(id).push(action);
    url.path().into()
}
fn scope_matches(scope: &str) -> bool {
    let set: std::collections::BTreeSet<_> = scope.split_whitespace().collect();
    set == ["environment:status", "environment:connect"].into_iter().collect()
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

pub struct RelayConnection {
    pub cloud: Arc<Cloud>,
    pub environment: Environment,
    token: Mutex<Option<(String, u64, u64)>>,
}
impl std::fmt::Debug for RelayConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayConnection")
    }
}
impl PartialEq for RelayConnection {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Eq for RelayConnection {}
impl RelayConnection {
    pub fn new(cloud: Arc<Cloud>, environment: Environment) -> Arc<Self> {
        Arc::new(Self {
            cloud,
            environment,
            token: Mutex::new(None),
        })
    }
    pub async fn access(&self) -> Result<String, String> {
        let mut cached = self.token.lock().await;
        if let Some((token, expires, generation)) = cached.as_ref() {
            self.cloud.assert_account(*generation).await?;
            if *expires > now_ms() + 5000 {
                return Ok(token.clone());
            }
        }
        let (_, generation) = self.cloud.token().await?;
        let value = self
            .cloud
            .relay_request(
                &environment_path(&self.environment.environment_id, "connect"),
                Some(json!({"clientKeyThumbprint":self.cloud.key.thumbprint()})),
            )
            .await?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Bootstrap {
            environment_id: String,
            endpoint: Endpoint,
            credential: String,
        }
        let bootstrap: Bootstrap = serde_json::from_value(value).map_err(|_| "Invalid relay connection response.")?;
        if bootstrap.environment_id != self.environment.environment_id || bootstrap.endpoint != self.environment.endpoint || !bootstrap.endpoint.is_secure() {
            return Err("Relay returned a different or insecure environment endpoint.".into());
        }
        let base = bootstrap.endpoint.http_base_url.trim_end_matches('/');
        let descriptor = crate::auth::fetch_descriptor(base)
            .await
            .map_err(|_| "Could not read the relay environment descriptor.")?;
        if descriptor.environment_id != self.environment.environment_id {
            return Err("Relay returned a descriptor for a different environment.".into());
        }
        if let Some(mismatch) = t3_core::compatibility::protocol_mismatch(&descriptor) {
            return Err(mismatch.message(&descriptor.label));
        }
        let url = format!("{base}/oauth/token");
        let response = http()
            .post(&url)
            .header("dpop", self.cloud.proof("POST", &url, None)?)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
                ("subject_token", bootstrap.credential.as_str()),
                ("subject_token_type", "urn:t3:params:oauth:token-type:environment-bootstrap"),
                ("requested_token_type", "urn:ietf:params:oauth:token-type:access_token"),
                ("client_label", "T3 Code Native"),
                ("client_device_type", "desktop"),
                ("client_os", crate::auth::client_os()),
            ])
            .send()
            .await
            .map_err(|_| "Could not authorize the relay environment.")?;
        let value = relay_response(response).await?;
        if value["token_type"] != "DPoP" {
            return Err("Environment returned an unexpected token type.".into());
        }
        let token = value["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Invalid environment token response.")?
            .to_owned();
        let expires = value["expires_in"].as_u64().filter(|e| *e > 0).ok_or("Invalid environment token expiry.")?;
        self.cloud.assert_account(generation).await?;
        *cached = Some((token.clone(), now_ms().saturating_add(expires.saturating_mul(1000)), generation));
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                    if request_tx.send(request).is_err() {
                        return;
                    }
                    let Some((status, value)) = reply_rx.recv().await else { return };
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
            relay_access: Mutex::new(None),
            key: ProofKey::from_bytes(&[1; 32]).unwrap(),
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
    async fn discovery_checks_status_with_scoped_dpop_and_drops_results_after_sign_out() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(u64::MAX));
        let environment = json!({"environmentId":"env","label":"Mac","endpoint":{"httpBaseUrl":"https://env.test","wsBaseUrl":"wss://env.test/ws","providerKind":"cloudflare_tunnel"},"future":true});
        fixture.reply(200, json!({"environments":[environment]}));
        fixture.reply(
            200,
            json!({"access_token":"relay-access","token_type":"DPoP","scope":"environment:connect environment:status","expires_in":60}),
        );
        let discover = {
            let c = cloud.clone();
            tokio::spawn(async move { c.discover().await })
        };
        let listing = fixture.request().await;
        assert!(listing.starts_with("GET /v1/client/environments"));
        assert!(listing.contains("Bearer fixture-access"));
        let exchange = fixture.request().await;
        assert!(exchange.starts_with("POST /v1/client/dpop-token"));
        assert!(exchange.contains("dpop:"));
        assert!(exchange.contains("client_id=t3-web"));
        let status = fixture.request().await;
        assert!(status.starts_with("POST /v1/environments/env/status"));
        assert!(status.contains("DPoP relay-access"));
        cloud.sign_out().await.unwrap();
        fixture.reply(200, json!({"environmentId":"env","endpoint":environment["endpoint"],"status":"online"}));
        assert!(discover.await.unwrap().is_err());
        assert!(cloud.identity().await.unwrap().is_none());
    }
    #[tokio::test]
    async fn discovery_preserves_offline_errors_and_rejects_unexpected_scopes() {
        let fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(u64::MAX));
        let endpoint = json!({"httpBaseUrl":"https://env.test","wsBaseUrl":"wss://env.test/ws","providerKind":"cloudflare_tunnel"});
        fixture.reply(200, json!({"environments":[{"environmentId":"env","label":"Mac","endpoint":endpoint}]}));
        fixture.reply(
            200,
            json!({"access_token":"relay-access","token_type":"DPoP","scope":"environment:status environment:connect","expires_in":60}),
        );
        fixture.reply(
            200,
            json!({"environmentId":"env","endpoint":endpoint,"status":"offline","offlineReason":"tunnel_released","traceId":"trace"}),
        );
        let result = cloud.discover().await.unwrap();
        assert!(result.listed);
        assert!(result.environments[0].status.as_ref().unwrap().message().contains("tunnel was removed"));
        *cloud.relay_access.lock().await = None;
        fixture.reply(200, json!({"access_token":"relay-access","token_type":"DPoP","scope":"mobile:registration"}));
        assert!(cloud.relay_request("/unused", None).await.unwrap_err().contains("scopes"));
        assert_eq!(environment_path("a/b", "connect"), "/v1/environments/a%2Fb/connect");
    }
    #[tokio::test]
    async fn cached_relay_access_is_reexchanged_once_after_rejection() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(u64::MAX));
        *cloud.relay_access.lock().await = Some(RelayAccess {
            token: "old-relay".into(),
            expires: u64::MAX,
            generation: 0,
        });
        fixture.reply(401, json!({"_tag":"RelayAuthInvalidError","reason":"invalid_bearer"}));
        fixture.reply(
            200,
            json!({"access_token":"new-relay","expires_in":60,"token_type":"DPoP","scope":"environment:status environment:connect"}),
        );
        fixture.reply(200, json!({"ok":true}));
        assert_eq!(cloud.relay_request("/status", None).await.unwrap(), json!({"ok":true}));
        assert!(fixture.request().await.contains("DPoP old-relay"));
        assert!(fixture.request().await.starts_with("POST /v1/client/dpop-token"));
        assert!(fixture.request().await.contains("DPoP new-relay"));
        fixture.reply(200, json!({"ok":true}));
        cloud.relay_request("/status", None).await.unwrap();
        assert!(fixture.request().await.contains("DPoP new-relay"));
    }
    #[tokio::test]
    async fn relay_environment_requests_and_rpc_upgrade_use_the_bound_key_and_fresh_path_proofs() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(u64::MAX));
        let environment = Environment {
            environment_id: "env".into(),
            label: "Mac".into(),
            endpoint: Endpoint {
                http_base_url: fixture.base.clone(),
                ws_base_url: "wss://socket.test/custom".into(),
                provider_kind: "cloudflare_tunnel".into(),
            },
        };
        let relay = RelayConnection::new(cloud.clone(), environment);
        *relay.token.lock().await = Some(("bound-access".into(), u64::MAX, 0));
        let credentials = crate::auth::Credentials {
            http_base: fixture.base.clone(),
            access_token: String::new(),
            relay: Some(relay),
        };
        fixture.reply(404, json!({}));
        fixture.reply(200, json!({"ticket":"ticket"}));
        assert_eq!(crate::auth::issue_rpc_ticket(&credentials).await.unwrap(), "ticket");
        let scoped = fixture.request().await;
        let fallback = fixture.request().await;
        assert!(scoped.starts_with("POST /api/auth/websocket-ticket/rpc"));
        assert!(fallback.starts_with("POST /api/auth/websocket-ticket "));
        assert!(scoped.contains("DPoP bound-access") && scoped.contains("dpop:"));
        let proof = |request: String| request.lines().find(|l| l.starts_with("dpop:")).unwrap().to_owned();
        assert_ne!(proof(scoped), proof(fallback));
        let url = url::Url::parse(&crate::auth::socket_url(&credentials, "ticket")).unwrap();
        assert_eq!((url.host_str(), url.path()), (Some("socket.test"), "/custom"));
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["connectionMethod"], "relay");
        assert!(!params["dpop"].is_empty());
        assert!(!format!("{credentials:?}").contains("bound-access"));
        cloud.sign_out().await.unwrap();
        assert!(credentials.relay.unwrap().access().await.is_err());
    }
}
