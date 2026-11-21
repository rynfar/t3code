//! T3 Connect cloud authentication (Clerk Native Frontend API `_is_native=1` + legacy CLI OAuth)
//! and full RFC 9449 DPoP relay client (`packages/client-runtime/src/relay/managedRelay.ts`).
use rand::RngCore;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use t3_core::connect::{
    self, CLERK_NATIVE_CALLBACK_URL, Callback, ClerkSession, EmailSignInChallenge, FirstFactorKind,
    SecondFactorChallenge, SecondFactorKind, SocialProvider, Token, TokenResponse,
};
use t3_core::relay::{
    Discovery, Environment, EnvironmentConnectResponse, EnvironmentStatusResponse,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex, oneshot},
};

use crate::dpop::{self, DpopDeviceKey};

const RELAY_DPOP_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const RELAY_JWT_SUBJECT_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:jwt";
const RELAY_ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const RELAY_DPOP_SCOPE: &str = "environment:connect environment:status";
const TOKEN_REFRESH_SKEW_MS: u64 = 15_000;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
    pub clerk_origin: String,
    pub clerk_jwt_template: String,
    pub relay_client_id: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub port: u16,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let value = |name: &str, default: &str| {
            std::env::var(name)
                .unwrap_or_else(|_| default.into())
                .trim()
                .to_owned()
        };
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
        let clerk_origin = connect::clerk_origin(&key).ok_or("Invalid Clerk publishable key.")?;
        let clerk_jwt_template = value("T3CODE_CLERK_JWT_TEMPLATE", "t3-relay");
        if clerk_jwt_template.is_empty() {
            return Err("T3 Connect Clerk JWT template is not configured.".into());
        }
        let relay_client_id = value("T3CODE_RELAY_CLIENT_ID", "t3-web");
        if relay_client_id != "t3-web" && relay_client_id != "t3-mobile" {
            return Err("T3 Connect relay client ID must be t3-web or t3-mobile.".into());
        }
        let client_id = value("T3CODE_CLERK_CLI_OAUTH_CLIENT_ID", "hzxSgY2cH10sDU2r");
        if client_id.is_empty() {
            return Err("T3 Connect OAuth client ID is not configured.".into());
        }
        Ok(Self {
            relay: relay.trim_end_matches('/').into(),
            hosted,
            token_endpoint: format!("{clerk_origin}/oauth/token"),
            clerk_origin,
            clerk_jwt_template,
            relay_client_id,
            client_id,
            port: 34338,
        })
    }
}

/// Summary of the persisted T3 Connect account state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountState {
    pub identity: Option<String>,
    pub has_clerk_session: bool,
}

/// In-flight native social OAuth sign-in started via Clerk Native Frontend API.
#[derive(Debug, Clone)]
pub struct PendingOAuthSignIn {
    pub redirect_url: String,
    client_token: String,
    generation: u64,
}

/// In-flight email sign-in awaiting first factor (`email_code` or `password`).
#[derive(Debug, Clone)]
pub struct PendingEmailSignIn {
    pub challenge: EmailSignInChallenge,
    client_token: String,
}

/// In-flight 2FA challenge (`totp` or `backup_code`) from Clerk Native Frontend API.
#[derive(Debug, Clone)]
pub struct PendingSecondFactor {
    pub challenge: SecondFactorChallenge,
    client_token: String,
}

#[cfg(feature = "test-support")]
impl PendingSecondFactor {
    pub fn for_test(challenge: SecondFactorChallenge) -> Self {
        Self {
            challenge,
            client_token: "test-token".into(),
        }
    }
}

/// Result of completing a Clerk Native first factor or OAuth callback.
#[derive(Debug, Clone)]
pub enum ClerkAuthOutcome {
    Complete(ClerkSession),
    NeedsSecondFactor(PendingSecondFactor),
}

struct CachedJwt {
    jwt: String,
    expires_at_epoch_ms: u64,
}

struct CachedDpopToken {
    access_token: String,
    expires_at_epoch_ms: u64,
    thumbprint: String,
    scope: String,
}

struct Account {
    loaded: bool,
    clerk: Option<ClerkSession>,
    token: Option<Token>,
    cached_jwt: Option<CachedJwt>,
    cached_dpop: Option<CachedDpopToken>,
    generation: u64,
}

/// Pure client-side Clerk Native Frontend API (`_is_native=1`) HTTP helper.
pub struct ClerkClient {
    origin: String,
    jwt_template: String,
}

impl ClerkClient {
    pub fn new(origin: impl Into<String>, jwt_template: impl Into<String>) -> Self {
        Self {
            origin: origin.into().trim_end_matches('/').to_owned(),
            jwt_template: jwt_template.into(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}?_is_native=1", self.origin)
    }

    fn extract_auth_header(headers: &reqwest::header::HeaderMap) -> Option<String> {
        headers
            .get(reqwest::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    async fn send_json(
        request: reqwest::RequestBuilder,
        current_token: Option<&str>,
    ) -> Result<(Option<String>, reqwest::StatusCode, Value), String> {
        let mut req = request;
        if let Some(token) = current_token.filter(|t| !t.is_empty()) {
            req = req.header(reqwest::header::AUTHORIZATION, token);
        }
        let response = req
            .send()
            .await
            .map_err(|_| "Could not reach T3 Connect authentication service.".to_owned())?;
        let rotated = Self::extract_auth_header(response.headers());
        let status = response.status();
        let value = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok((rotated, status, value))
    }

    pub async fn bootstrap_client(&self, existing_token: Option<&str>) -> Result<(String, Value), String> {
        let (rotated, status, value) = Self::send_json(http().get(self.url("/v1/client")), existing_token).await?;
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("T3 Connect authentication failed (HTTP {}).", status.as_u16())));
        }
        let token = rotated
            .or_else(|| existing_token.filter(|t| !t.is_empty()).map(str::to_owned))
            .ok_or_else(|| "T3 Connect authentication did not return a client token.".to_owned())?;
        Ok((token, value))
    }

    pub async fn fetch_client(
        &self,
        client_token: &str,
        rotating_token_nonce: Option<&str>,
    ) -> Result<(String, Value), String> {
        let mut url = url::Url::parse(&self.url("/v1/client"))
            .map_err(|_| "Invalid Clerk client URL.".to_owned())?;
        if let Some(nonce) = rotating_token_nonce.filter(|n| !n.trim().is_empty()) {
            url.query_pairs_mut().append_pair("rotating_token_nonce", nonce.trim());
        }
        let (rotated, status, value) = Self::send_json(http().get(url), Some(client_token)).await?;
        let next_token = rotated.unwrap_or_else(|| client_token.to_owned());
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("Could not complete T3 Connect sign-in (HTTP {}).", status.as_u16())));
        }
        Ok((next_token, value))
    }

    pub async fn start_oauth(&self, provider: SocialProvider) -> Result<(String, String), String> {
        let (client_token, _) = self.bootstrap_client(None).await?;
        let form = [
            ("strategy", provider.strategy()),
            ("redirect_url", CLERK_NATIVE_CALLBACK_URL),
            ("action_complete_redirect_url", CLERK_NATIVE_CALLBACK_URL),
        ];
        let (rotated, status, value) = Self::send_json(
            http().post(self.url("/v1/client/sign_ins")).form(&form),
            Some(&client_token),
        )
        .await?;
        let next_token = rotated.unwrap_or(client_token);
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("Could not start {} sign-in (HTTP {}).", provider.label(), status.as_u16())));
        }
        let redirect_url = connect::extract_oauth_redirect_url(&value)
            .ok_or_else(|| "T3 Connect did not return an OAuth redirect URL.".to_owned())?;
        Ok((next_token, redirect_url))
    }

    pub async fn transfer_oauth_sign_up(&self, client_token: &str) -> Result<(String, Value), String> {
        let (rotated, status, value) = Self::send_json(
            http().post(self.url("/v1/client/sign_ups")).form(&[("transfer", "true")]),
            Some(client_token),
        )
        .await?;
        let next_token = rotated.unwrap_or_else(|| client_token.to_owned());
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("Could not finish T3 Connect sign-up (HTTP {}).", status.as_u16())));
        }
        Ok((next_token, value))
    }

    pub async fn start_email_sign_in(&self, identifier: &str) -> Result<(String, EmailSignInChallenge), String> {
        let trimmed = identifier.trim();
        if trimmed.is_empty() {
            return Err("Enter your email address to sign in.".to_owned());
        }
        let (client_token, _) = self.bootstrap_client(None).await?;
        let (rotated, status, value) = Self::send_json(
            http().post(self.url("/v1/client/sign_ins")).form(&[("identifier", trimmed)]),
            Some(&client_token),
        )
        .await?;
        let next_token = rotated.unwrap_or(client_token);
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("Could not start email sign-in (HTTP {}).", status.as_u16())));
        }
        let challenge = connect::parse_email_sign_in_challenge(&value, trimmed)?;
        Ok((next_token, challenge))
    }

    pub async fn prepare_email_code(
        &self,
        client_token: &str,
        challenge: &EmailSignInChallenge,
    ) -> Result<String, String> {
        let mut form = vec![("strategy", "email_code".to_owned())];
        if let Some(email_id) = challenge.email_address_id.as_deref() {
            form.push(("email_address_id", email_id.to_owned()));
        }
        let path = format!("/v1/client/sign_ins/{}/prepare_first_factor", challenge.sign_in_id);
        let (rotated, status, value) = Self::send_json(
            http().post(self.url(&path)).form(&form),
            Some(client_token),
        )
        .await?;
        let next_token = rotated.unwrap_or_else(|| client_token.to_owned());
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("Could not send verification code (HTTP {}).", status.as_u16())));
        }
        Ok(next_token)
    }

    pub async fn attempt_first_factor(
        &self,
        client_token: &str,
        sign_in_id: &str,
        kind: FirstFactorKind,
        secret: &str,
    ) -> Result<(String, Value), String> {
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(match kind {
                FirstFactorKind::EmailCode => "Enter the verification code sent to your email.".to_owned(),
                FirstFactorKind::Password => "Enter your password.".to_owned(),
            });
        }
        let form = match kind {
            FirstFactorKind::EmailCode => [("strategy", "email_code"), ("code", secret)],
            FirstFactorKind::Password => [("strategy", "password"), ("password", secret)],
        };
        let path = format!("/v1/client/sign_ins/{sign_in_id}/attempt_first_factor");
        let (rotated, status, value) = Self::send_json(
            http().post(self.url(&path)).form(&form),
            Some(client_token),
        )
        .await?;
        let next_token = rotated.unwrap_or_else(|| client_token.to_owned());
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("Sign-in verification failed (HTTP {}).", status.as_u16())));
        }
        Ok((next_token, value))
    }

    pub async fn attempt_second_factor(
        &self,
        client_token: &str,
        sign_in_id: &str,
        kind: SecondFactorKind,
        code: &str,
    ) -> Result<(String, Value), String> {
        let code = code.trim();
        if code.is_empty() {
            return Err("Enter your two-factor verification code.".to_owned());
        }
        let strategy = match kind {
            SecondFactorKind::Totp => "totp",
            SecondFactorKind::BackupCode => "backup_code",
        };
        let path = format!("/v1/client/sign_ins/{sign_in_id}/attempt_second_factor");
        let (rotated, status, value) = Self::send_json(
            http().post(self.url(&path)).form(&[("strategy", strategy), ("code", code)]),
            Some(client_token),
        )
        .await?;
        let next_token = rotated.unwrap_or_else(|| client_token.to_owned());
        if !status.is_success() {
            return Err(connect::extract_clerk_error(&value)
                .unwrap_or_else(|| format!("Two-factor verification failed (HTTP {}).", status.as_u16())));
        }
        Ok((next_token, value))
    }

    pub async fn mint_jwt(
        &self,
        client_token: &str,
        session_id: &str,
    ) -> (Option<String>, Result<String, String>) {
        let path = format!("/v1/client/sessions/{session_id}/tokens/{}", self.jwt_template);
        let result = Self::send_json(http().post(self.url(&path)), Some(client_token)).await;
        let (rotated, status, value) = match result {
            Ok(parts) => parts,
            Err(error) => return (None, Err(error)),
        };
        if !status.is_success() {
            let msg = if status.as_u16() == 401 || status.as_u16() == 404 {
                "Your T3 Connect session expired. Sign out and sign in again.".to_owned()
            } else {
                connect::extract_clerk_error(&value)
                    .unwrap_or_else(|| format!("Could not mint T3 Connect session token (HTTP {}).", status.as_u16()))
            };
            return (rotated, Err(msg));
        }
        let jwt = value
            .get("jwt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        match jwt {
            Some(jwt) => (rotated, Ok(jwt)),
            None => (rotated, Err("T3 Connect returned an empty session token.".to_owned())),
        }
    }

    pub async fn delete_session(&self, client_token: &str, session_id: &str) {
        let path = format!("/v1/client/sessions/{session_id}");
        let _ = Self::send_json(http().delete(self.url(&path)), Some(client_token)).await;
    }
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
                clerk: None,
                token: None,
                cached_jwt: None,
                cached_dpop: None,
                generation: 0,
            })),
        })
    }

    fn home(&self) -> Result<&std::path::Path, String> {
        self.home
            .as_deref()
            .ok_or_else(|| "No native credential storage directory.".into())
    }

    fn config(&self) -> Result<&Config, String> {
        self.config.as_ref().map_err(Clone::clone)
    }

    pub fn configured(&self) -> bool {
        self.config.is_ok()
    }

    fn clerk_client(config: &Config) -> ClerkClient {
        ClerkClient::new(config.clerk_origin.clone(), config.clerk_jwt_template.clone())
    }

    fn device_key(&self) -> Result<Arc<DpopDeviceKey>, String> {
        match self.home.as_deref() {
            Some(dir) => dpop::device_key_in(dir),
            None => dpop::device_key(),
        }
    }

    async fn ensure_loaded(&self) -> Result<(), String> {
        let mut account = self.account.lock().await;
        if !account.loaded {
            let home = self.home()?;
            account.clerk = crate::store::load_clerk_session(home)
                .map_err(|_| "Could not read the stored T3 Connect session.")?;
            account.token = crate::store::load_cloud_token(home)
                .map_err(|_| "Could not read the stored T3 Connect credential.")?;
            account.loaded = true;
        }
        Ok(())
    }

    pub async fn account_state(&self) -> Result<AccountState, String> {
        self.ensure_loaded().await?;
        let account = self.account.lock().await;
        if let Some(clerk) = &account.clerk {
            return Ok(AccountState {
                identity: Some(
                    clerk
                        .identity
                        .clone()
                        .unwrap_or_else(|| "T3 Connect account".into()),
                ),
                has_clerk_session: true,
            });
        }
        if let Some(token) = &account.token {
            return Ok(AccountState {
                identity: Some(
                    token
                        .identity
                        .clone()
                        .unwrap_or_else(|| "T3 Connect account".into()),
                ),
                has_clerk_session: false,
            });
        }
        Ok(AccountState {
            identity: None,
            has_clerk_session: false,
        })
    }

    pub async fn identity(&self) -> Result<Option<String>, String> {
        Ok(self.account_state().await?.identity)
    }

    pub async fn has_clerk_session(&self) -> Result<bool, String> {
        Ok(self.account_state().await?.has_clerk_session)
    }

    async fn persist_clerk_session(&self, session: ClerkSession) -> Result<ClerkSession, String> {
        let home = self.home()?.to_owned();
        let mut account = self.account.lock().await;
        crate::store::save_clerk_session(&home, &session)
            .map_err(|_| "Could not store the T3 Connect session.")?;
        account.clerk = Some(session.clone());
        account.cached_jwt = None;
        account.cached_dpop = None;
        account.loaded = true;
        account.generation += 1;
        Ok(session)
    }

    /// Path A (step 1): start Clerk Native social OAuth sign-in (`oauth_google` / `oauth_github` /
    /// `oauth_apple` / `oauth_microsoft`) targeting `com.t3tools.t3code://callback`.
    pub async fn start_oauth_sign_in(&self, provider: SocialProvider) -> Result<PendingOAuthSignIn, String> {
        let config = self.config()?.clone();
        let generation = self.account.lock().await.generation;
        let clerk = Self::clerk_client(&config);
        let (client_token, redirect_url) = clerk.start_oauth(provider).await?;
        Ok(PendingOAuthSignIn {
            redirect_url,
            client_token,
            generation,
        })
    }

    /// Path A (step 2): finish Clerk Native social OAuth once `com.t3tools.t3code://callback` arrives.
    pub async fn complete_oauth_sign_in(
        &self,
        pending: PendingOAuthSignIn,
        callback_url: &str,
    ) -> Result<ClerkAuthOutcome, String> {
        let config = self.config()?.clone();
        let nonce = connect::parse_oauth_callback(callback_url)?;
        let clerk = Self::clerk_client(&config);
        let (mut client_token, mut client_json) = clerk
            .fetch_client(&pending.client_token, nonce.as_deref())
            .await?;

        if let Some(challenge) = connect::parse_second_factor_challenge(&client_json) {
            return Ok(ClerkAuthOutcome::NeedsSecondFactor(PendingSecondFactor {
                challenge,
                client_token,
            }));
        }

        if connect::extract_clerk_session(&client_json, &client_token).is_none()
            && connect::is_oauth_transferable(&client_json)
        {
            let (next_token, transfer_json) = clerk.transfer_oauth_sign_up(&client_token).await?;
            client_token = next_token;
            client_json = transfer_json;
        }

        let session = connect::extract_clerk_session(&client_json, &client_token)
            .ok_or_else(|| "T3 Connect sign-in did not establish an active session.".to_owned())?;
        {
            let account = self.account.lock().await;
            if account.generation != pending.generation {
                return Err("T3 Connect sign-in was cancelled.".into());
            }
        }
        let saved = self.persist_clerk_session(session).await?;
        Ok(ClerkAuthOutcome::Complete(saved))
    }

    /// Path B (step 1): start an email sign-in directly inside the Connect card.
    pub async fn start_email_sign_in(&self, identifier: &str) -> Result<PendingEmailSignIn, String> {
        let config = self.config()?.clone();
        let clerk = Self::clerk_client(&config);
        let (client_token, challenge) = clerk.start_email_sign_in(identifier).await?;
        Ok(PendingEmailSignIn {
            challenge,
            client_token,
        })
    }

    /// Path B (step 2a): request a one-time email verification code for an in-flight email sign-in.
    pub async fn prepare_email_code(&self, pending: &mut PendingEmailSignIn) -> Result<(), String> {
        let config = self.config()?.clone();
        let clerk = Self::clerk_client(&config);
        let next_token = clerk
            .prepare_email_code(&pending.client_token, &pending.challenge)
            .await?;
        pending.client_token = next_token;
        Ok(())
    }

    /// Path B (step 2b): verify an email code or password for an in-flight email sign-in.
    pub async fn attempt_email_first_factor(
        &self,
        pending: &PendingEmailSignIn,
        kind: FirstFactorKind,
        secret: &str,
    ) -> Result<ClerkAuthOutcome, String> {
        let config = self.config()?.clone();
        let clerk = Self::clerk_client(&config);
        let (mut client_token, value) = clerk
            .attempt_first_factor(&pending.client_token, &pending.challenge.sign_in_id, kind, secret)
            .await?;
        if let Some(challenge) = connect::parse_second_factor_challenge(&value) {
            return Ok(ClerkAuthOutcome::NeedsSecondFactor(PendingSecondFactor {
                challenge,
                client_token,
            }));
        }
        let session = match connect::extract_clerk_session(&value, &client_token) {
            Some(session) => session,
            None => {
                let (next_token, client_json) = clerk.fetch_client(&client_token, None).await?;
                client_token = next_token;
                connect::extract_clerk_session(&client_json, &client_token)
                    .ok_or_else(|| "T3 Connect sign-in did not establish an active session.".to_owned())?
            }
        };
        let saved = self.persist_clerk_session(session).await?;
        Ok(ClerkAuthOutcome::Complete(saved))
    }

    /// Path A/B (step 3): complete a `needs_second_factor` challenge (`totp` or `backup_code`).
    pub async fn attempt_second_factor(
        &self,
        pending: &PendingSecondFactor,
        kind: SecondFactorKind,
        code: &str,
    ) -> Result<ClerkSession, String> {
        let config = self.config()?.clone();
        let clerk = Self::clerk_client(&config);
        let (mut client_token, value) = clerk
            .attempt_second_factor(&pending.client_token, &pending.challenge.sign_in_id, kind, code)
            .await?;
        let session = match connect::extract_clerk_session(&value, &client_token) {
            Some(session) => session,
            None => {
                let (next_token, client_json) = clerk.fetch_client(&client_token, None).await?;
                client_token = next_token;
                connect::extract_clerk_session(&client_json, &client_token)
                    .ok_or_else(|| "Two-factor verification did not establish an active session.".to_owned())?
            }
        };
        self.persist_clerk_session(session).await
    }

    async fn mint_clerk_jwt(&self, force_refresh: bool) -> Result<(String, u64), String> {
        self.ensure_loaded().await?;
        let account = self.account.clone();
        let config = self.config()?.clone();
        let home = self.home()?.to_owned();
        // Serialized and detached from caller cancellation so rotated client_token is always saved.
        tokio::spawn(async move {
            let mut account = account.lock().await;
            if !force_refresh
                && let Some(cached) = &account.cached_jwt
                && cached.expires_at_epoch_ms > now_ms().saturating_add(TOKEN_REFRESH_SKEW_MS)
            {
                return Ok((cached.jwt.clone(), account.generation));
            }
            let session = account
                .clerk
                .clone()
                .ok_or_else(|| t3_core::relay::LEGACY_OAUTH_NOTICE.to_owned())?;
            let clerk = Self::clerk_client(&config);
            let (rotated, result) = clerk.mint_jwt(&session.client_token, &session.session_id).await;
            if let Some(new_client_token) = rotated
                && let Some(active) = account.clerk.as_mut()
                && active.client_token != new_client_token
            {
                active.client_token = new_client_token;
                let _ = crate::store::save_clerk_session(&home, active);
            }
            let jwt = result?;
            let expires_at_epoch_ms = connect::jwt_exp_epoch_ms(&jwt)
                .unwrap_or_else(|| now_ms().saturating_add(55_000));
            account.cached_jwt = Some(CachedJwt {
                jwt: jwt.clone(),
                expires_at_epoch_ms,
            });
            Ok((jwt, account.generation))
        })
        .await
        .map_err(|_| "Could not mint T3 Connect session token.".to_owned())?
    }

    async fn legacy_oauth_token(&self) -> Result<(String, u64), String> {
        self.ensure_loaded().await?;
        let account = self.account.clone();
        let config = self.config()?.clone();
        let home = self.home()?.to_owned();
        // Refresh rotation and persistence finish even if the caller cancels its discovery.
        tokio::spawn(async move {
            let mut account = account.lock().await;
            let token = account
                .token
                .as_ref()
                .ok_or("Sign in to T3 Connect before loading relay data.")?;
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
                crate::store::save_cloud_token(&home, &next)
                    .map_err(|_| "Could not store the T3 Connect credential.")?;
                account.token = Some(next);
            }
            Ok((
                account.token.as_ref().expect("signed in").access_token.clone(),
                account.generation,
            ))
        })
        .await
        .map_err(|_| "Could not refresh the T3 Connect credential.".to_owned())?
    }

    async fn bearer_token(&self, force_refresh: bool) -> Result<(String, u64, bool), String> {
        self.ensure_loaded().await?;
        let has_clerk = self.account.lock().await.clerk.is_some();
        if has_clerk {
            let (jwt, generation) = self.mint_clerk_jwt(force_refresh).await?;
            Ok((jwt, generation, true))
        } else {
            let (token, generation) = self.legacy_oauth_token().await?;
            Ok((token, generation, false))
        }
    }

    async fn assert_account(&self, generation: u64) -> Result<(), String> {
        let account = self.account.lock().await;
        if generation != account.generation || (account.clerk.is_none() && account.token.is_none()) {
            return Err("The T3 Connect account changed. Try again.".into());
        }
        Ok(())
    }

    pub async fn sign_out(&self) -> Result<(), String> {
        let (clerk_session, config, result) = {
            let mut account = self.account.lock().await;
            account.generation += 1;
            let clerk_session = account.clerk.take();
            account.token = None;
            account.cached_jwt = None;
            account.cached_dpop = None;
            account.loaded = true;
            let clear_res = match self.home() {
                Ok(home) => {
                    let r1 = crate::store::clear_clerk_session(home);
                    let r2 = crate::store::clear_cloud_token(home);
                    r1.and(r2)
                        .map_err(|_| "Could not remove the stored T3 Connect credential.".into())
                }
                Err(err) => Err(err),
            };
            (clerk_session, self.config.clone().ok(), clear_res)
        };
        if let (Some(session), Some(config)) = (clerk_session, config) {
            Self::clerk_client(&config)
                .delete_session(&session.client_token, &session.session_id)
                .await;
        }
        result
    }

    /// Legacy browser PKCE sign-in retained for non-macOS fallback and backward compatibility.
    pub async fn sign_in(&self, browser: oneshot::Sender<String>) -> Result<String, String> {
        let config = self.config()?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.port))
            .await
            .map_err(|_| {
                "Could not listen for T3 Connect authorization on 127.0.0.1:34338. Close another sign-in attempt and try again."
            })?;
        let port = listener
            .local_addr()
            .map_err(|_| "Could not read the callback listener address.")?
            .port();
        let generation = self.account.lock().await.generation;
        let verifier = connect::encode(&random::<32>());
        let state = connect::encode(&random::<16>());
        let url = connect::authorize_url(&config.hosted, &state, &connect::challenge(&verifier), port)
            .map_err(|_| "Invalid T3 Connect authorization URL.")?;
        browser
            .send(url)
            .map_err(|_| "T3 Connect sign-in was cancelled.")?;
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
        let identity = token
            .identity
            .clone()
            .unwrap_or_else(|| "T3 Connect account".into());
        let mut account = self.account.lock().await;
        if account.generation != generation {
            return Err("T3 Connect sign-in was cancelled.".into());
        }
        crate::store::save_cloud_token(self.home()?, &token)
            .map_err(|_| "Could not store the T3 Connect credential.")?;
        account.token = Some(token);
        account.loaded = true;
        account.generation += 1;
        Ok(identity)
    }

    async fn relay_bearer_json(
        &self,
        method: reqwest::Method,
        url: &url::Url,
        network_error: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        for attempt in 0..2 {
            let (token, generation, is_clerk) = self.bearer_token(attempt > 0).await?;
            let mut request = http()
                .request(method.clone(), url.clone())
                .bearer_auth(token);
            if let Some(body) = body {
                if !is_clerk {
                    return Err("Sign in to T3 Connect before enabling this.".to_owned());
                }
                request = request.json(body);
            }
            let response = request.send()
                .await
                .map_err(|_| network_error.to_owned())?;
            let status = response.status();
            let value = response.json::<Value>().await.unwrap_or(Value::Null);
            if !status.is_success() {
                if attempt == 0 && is_clerk && t3_core::relay::is_invalid_bearer_error(status.as_u16(), &value) {
                    self.account.lock().await.cached_jwt = None;
                    continue;
                }
                return Err(t3_core::relay::error_message(status.as_u16(), &value));
            }
            self.assert_account(generation).await?;
            return Ok(value);
        }
        unreachable!("relay bearer request exits within 2 attempts")
    }

    /// Lists environments linked to the signed-in T3 Connect account (`GET {relay}/v1/environments`).
    pub async fn discover(&self) -> Result<Discovery, String> {
        let config = self.config()?;
        let url = url::Url::parse(&format!("{}/v1/environments", config.relay))
            .map_err(|_| "Invalid T3 Connect relay URL.".to_owned())?;
        let value = self.relay_bearer_json(reqwest::Method::GET, &url, "Could not load T3 Connect environments.", None).await?;
        #[derive(Deserialize)]
        struct List {
            environments: Vec<Environment>,
        }
        let list: List =
            serde_json::from_value(value).map_err(|_| "Invalid T3 Connect environment list.")?;
        Ok(Discovery {
            listed: true,
            environments: list.environments,
            ..Default::default()
        })
    }

    /// Removes the canonical environment link from the relay after the host confirms unlink.
    pub async fn unlink_environment(&self, environment_id: &str) -> Result<(), String> {
        if self.config.is_err() || self.identity().await?.is_none() {
            return Ok(());
        }
        let mut url = url::Url::parse(&self.config()?.relay)
            .map_err(|_| "Invalid T3 Connect relay URL.".to_owned())?;
        url.path_segments_mut()
            .map_err(|_| "Invalid T3 Connect relay URL.".to_owned())?
            .clear()
            .extend(["v1", "client", "environment-links", environment_id]);
        let value = self.relay_bearer_json(reqwest::Method::DELETE, &url, "Could not reach T3 Connect relay.", None).await?;
        if value.get("ok").and_then(Value::as_bool).is_none() {
            return Err("T3 Connect returned an invalid unlink response.".to_owned());
        }
        Ok(())
    }

    /// Links the connected host and applies its requested publishing preference
    /// (`apps/web/src/cloud/linkEnvironment.ts`).
    pub async fn link_environment(
        &self,
        credentials: &crate::auth::Credentials,
        environment_id: &str,
        managed_tunnel: bool,
        publish: bool,
    ) -> Result<(), String> {
        let relay_url = &self.config()?.relay;
        let endpoint = url::Url::parse(&credentials.http_base)
            .map_err(|_| "Invalid environment URL.".to_owned())?;
        let port = endpoint.port_or_known_default().ok_or("Invalid environment port.")?;
        let mut socket = endpoint.clone();
        let scheme = match endpoint.scheme() {
            "http" => "ws",
            "https" => "wss",
            _ => return Err("Invalid environment URL.".to_owned()),
        };
        socket.set_scheme(scheme).map_err(|_| "Invalid environment URL.")?;
        let provider = if managed_tunnel { "cloudflare_tunnel" } else { "manual" };
        let mut capabilities = serde_json::json!({
            "notificationsEnabled": true,
            "liveActivitiesEnabled": true,
            "managedTunnelsEnabled": managed_tunnel,
        });
        let challenge = self.post_relay_link("environment-link-challenges", &capabilities).await?;
        let challenge = challenge["challenge"].as_str().filter(|value| !value.is_empty())
            .ok_or("Invalid T3 Connect link challenge.")?;
        let timeout = Duration::from_secs(60);
        let proof = crate::auth::post_access_with_timeout(credentials, "/api/connect/link-proof", Some(serde_json::json!({
            "challenge": challenge,
            "relayIssuer": relay_url,
            "endpoint": {
                "httpBaseUrl": credentials.http_base,
                "wsBaseUrl": socket.as_str().trim_end_matches('/'),
                "providerKind": provider,
            },
            "origin": {"localHttpHost": "127.0.0.1", "localHttpPort": port},
        })), Some(timeout)).await?;
        if proof.as_str().is_none_or(str::is_empty) {
            return Err("Invalid environment link proof.".to_owned());
        }
        capabilities["proof"] = proof;
        let link = self.post_relay_link("environment-links", &capabilities).await?;
        let link: t3_core::relay::EnvironmentLinkResponse = serde_json::from_value(link)
            .map_err(|_| "Invalid T3 Connect link response.".to_owned())?;
        if !link.ok || link.environment_id != environment_id || link.endpoint.provider_kind != provider {
            return Err("Relay returned credentials for a different environment or endpoint provider.".to_owned());
        }
        let applied = crate::auth::post_access_with_timeout(credentials, "/api/connect/relay-config", Some(serde_json::json!({
            "relayUrl": relay_url,
            "relayIssuer": link.relay_issuer,
            "cloudUserId": link.cloud_user_id,
            "environmentCredential": link.environment_credential,
            "cloudMintPublicKey": link.cloud_mint_public_key,
            "endpointRuntime": link.endpoint_runtime,
        })), Some(timeout)).await?;
        if applied["ok"] != true {
            return Err("Could not configure environment relay access.".to_owned());
        }
        crate::auth::post_access(credentials, "/api/connect/preferences", Some(serde_json::json!({
            "publishAgentActivity": publish,
        }))).await?;
        Ok(())
    }

    async fn post_relay_link(&self, path: &str, body: &Value) -> Result<Value, String> {
        let url = url::Url::parse(&format!("{}/v1/client/{path}", self.config()?.relay))
            .map_err(|_| "Invalid T3 Connect relay URL.".to_owned())?;
        self.relay_bearer_json(reqwest::Method::POST, &url, "Could not reach T3 Connect relay.", Some(body)).await
    }

    async fn exchange_dpop_token_internal(
        &self,
        force_refresh: bool,
    ) -> Result<(String, Arc<DpopDeviceKey>, u64), String> {
        self.ensure_loaded().await?;
        let config = self.config()?.clone();
        let key = self.device_key()?;
        let thumbprint = key.thumbprint().to_owned();

        if !force_refresh {
            let account = self.account.lock().await;
            if let Some(cached) = &account.cached_dpop
                && cached.thumbprint == thumbprint
                && cached.scope == RELAY_DPOP_SCOPE
                && cached.expires_at_epoch_ms > now_ms().saturating_add(TOKEN_REFRESH_SKEW_MS)
            {
                return Ok((cached.access_token.clone(), key, account.generation));
            }
        }

        let token_url = format!("{}/v1/client/dpop-token", config.relay);
        for attempt in 0..2 {
            let (clerk_jwt, generation) = self.mint_clerk_jwt(force_refresh || attempt > 0).await?;
            let proof = key.create_proof("POST", &token_url, None)?;
            let response = http()
                .post(&token_url)
                .header("DPoP", proof)
                .form(&[
                    ("grant_type", RELAY_DPOP_GRANT_TYPE),
                    ("subject_token", clerk_jwt.as_str()),
                    ("subject_token_type", RELAY_JWT_SUBJECT_TOKEN_TYPE),
                    ("requested_token_type", RELAY_ACCESS_TOKEN_TYPE),
                    ("resource", config.relay.as_str()),
                    ("scope", RELAY_DPOP_SCOPE),
                    ("client_id", config.relay_client_id.as_str()),
                ])
                .send()
                .await
                .map_err(|_| "Could not exchange T3 Connect relay token.".to_owned())?;
            let status = response.status();
            let value = response.json::<Value>().await.unwrap_or(Value::Null);
            if !status.is_success() {
                if attempt == 0 && t3_core::relay::is_invalid_bearer_error(status.as_u16(), &value) {
                    self.account.lock().await.cached_jwt = None;
                    continue;
                }
                return Err(t3_core::relay::error_message(status.as_u16(), &value));
            }
            #[derive(Deserialize)]
            struct DpopTokenResponse {
                access_token: String,
                token_type: String,
                expires_in: u64,
                #[serde(default)]
                scope: String,
            }
            let parsed: DpopTokenResponse = serde_json::from_value(value)
                .map_err(|_| "Invalid T3 Connect DPoP token response.".to_owned())?;
            if parsed.access_token.is_empty()
                || !parsed.token_type.eq_ignore_ascii_case("DPoP")
                || parsed.expires_in == 0
            {
                return Err("Invalid T3 Connect DPoP token response.".to_owned());
            }
            let expires_at_epoch_ms = now_ms().saturating_add(parsed.expires_in.saturating_mul(1000));
            {
                let mut account = self.account.lock().await;
                if account.generation != generation {
                    return Err("The T3 Connect account changed. Try again.".into());
                }
                account.cached_dpop = Some(CachedDpopToken {
                    access_token: parsed.access_token.clone(),
                    expires_at_epoch_ms,
                    thumbprint: thumbprint.clone(),
                    scope: if parsed.scope.is_empty() {
                        RELAY_DPOP_SCOPE.to_owned()
                    } else {
                        parsed.scope
                    },
                });
            }
            return Ok((parsed.access_token, key, generation));
        }
        unreachable!("exchange loop exits within 2 attempts")
    }

    /// Exchanges the Clerk `t3-relay` JWT for a DPoP-bound relay access token
    /// (`POST {relay}/v1/client/dpop-token`).
    pub async fn exchange_dpop_token(&self) -> Result<String, String> {
        let (access_token, _, _) = self.exchange_dpop_token_internal(false).await?;
        Ok(access_token)
    }

    fn environment_endpoint_url(relay: &str, environment_id: &str, action: &str) -> Result<String, String> {
        let mut url = url::Url::parse(relay).map_err(|_| "Invalid T3 Connect relay URL.".to_owned())?;
        url.path_segments_mut()
            .map_err(|_| "Invalid T3 Connect relay URL.".to_owned())?
            .clear()
            .push("v1")
            .push("environments")
            .push(environment_id)
            .push(action);
        Ok(url.into())
    }

    async fn post_dpop_json(&self, url: &str, body: Option<&Value>) -> Result<(Value, u64), String> {
        for attempt in 0..2 {
            let (access_token, key, generation) = self.exchange_dpop_token_internal(attempt > 0).await?;
            let proof = key.create_proof("POST", url, Some(&access_token))?;
            let mut req = http()
                .post(url)
                .header(reqwest::header::AUTHORIZATION, format!("DPoP {access_token}"))
                .header("DPoP", proof);
            if let Some(payload) = body {
                req = req.json(payload);
            }
            let response = req
                .send()
                .await
                .map_err(|_| "Could not reach T3 Connect relay.".to_owned())?;
            let status = response.status();
            let value = response.json::<Value>().await.unwrap_or(Value::Null);
            if !status.is_success() {
                if attempt == 0 && t3_core::relay::is_invalid_bearer_error(status.as_u16(), &value) {
                    let mut account = self.account.lock().await;
                    if account
                        .cached_dpop
                        .as_ref()
                        .is_some_and(|c| c.access_token == access_token)
                    {
                        account.cached_dpop = None;
                    }
                    continue;
                }
                return Err(t3_core::relay::error_message(status.as_u16(), &value));
            }
            self.assert_account(generation).await?;
            return Ok((value, generation));
        }
        unreachable!("dpop post loop exits within 2 attempts")
    }

    /// Checks live online/offline reachability for a relay environment
    /// (`POST {relay}/v1/environments/{id}/status`).
    pub async fn environment_status(&self, environment_id: &str) -> Result<EnvironmentStatusResponse, String> {
        let config = self.config()?.clone();
        let url = Self::environment_endpoint_url(&config.relay, environment_id, "status")?;
        let (value, _) = self.post_dpop_json(&url, None).await?;
        serde_json::from_value(value).map_err(|_| "Invalid T3 Connect environment status response.".to_owned())
    }

    /// Mints a short-lived DPoP-bound environment connection credential via T3 Connect relay
    /// (`POST {relay}/v1/environments/{id}/connect`).
    pub async fn connect_environment(&self, environment_id: &str) -> Result<EnvironmentConnectResponse, String> {
        let config = self.config()?.clone();
        let key = self.device_key()?;
        let url = Self::environment_endpoint_url(&config.relay, environment_id, "connect")?;
        let body = serde_json::json!({
            "clientProofKeyThumbprint": key.thumbprint(),
        });
        let (value, _) = self.post_dpop_json(&url, Some(&body)).await?;
        let response: EnvironmentConnectResponse = serde_json::from_value(value)
            .map_err(|_| "Invalid T3 Connect environment connection response.".to_owned())?;
        if response.environment_id != environment_id || response.endpoint.provider_kind != "cloudflare_tunnel" {
            return Err("Relay returned credentials for a different environment or endpoint provider.".to_owned());
        }
        Ok(response)
    }

    /// Calls [`Self::connect_environment`] and registers the minted credential in the DPoP store.
    pub async fn connect_and_register_environment(
        &self,
        environment_id: &str,
        seed_token: Option<&str>,
    ) -> Result<EnvironmentConnectResponse, String> {
        let resp = self.connect_environment(environment_id).await?;
        let expires_at_epoch_ms = resp
            .expires_at_epoch_ms()
            .unwrap_or_else(|| dpop::now_epoch_ms().saturating_add(300_000));
        dpop::register_relay_credential_in(
            self.home.clone(),
            &resp.endpoint.http_base_url,
            Some(&resp.endpoint.ws_base_url),
            resp.credential.clone(),
            expires_at_epoch_ms,
            Some(resp.environment_id.clone()),
            seed_token,
        );
        Ok(resp)
    }
}

impl crate::supervisor::ClientHandle {
    pub async fn relay_client_status(&self) -> Result<Value, String> {
        self.request("cloud.getRelayClientStatus", serde_json::json!({})).await
            .map_err(|error| error.to_string())
    }

    pub async fn install_relay_client(&self) -> Result<(), String> {
        let mut stream = self.stream("cloud.installRelayClient", serde_json::json!({})).await
            .map_err(|error| error.to_string())?;
        while let Some(item) = stream.receiver.recv().await {
            let event = item.map_err(|error| error.to_string())?;
            if event["type"] == "complete" {
                return if event["status"]["status"] == "available" {
                    Ok(())
                } else {
                    Err("The relay client is still unavailable after installation.".to_owned())
                };
            }
        }
        Err("The relay client install completed without a final status.".to_owned())
    }
}

async fn receive_callback(listener: TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|_| "T3 Connect callback listener stopped.")?;
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
            Callback::Denied => (
                "200 OK",
                "T3 Connect authorization was denied. Return to T3 Code Native.",
            ),
            Callback::Code(_) => (
                "200 OK",
                "T3 Connect authorized. You can close this window and return to T3 Code Native.",
            ),
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

    struct MockReply {
        status: u16,
        headers: Vec<(String, String)>,
        body: Value,
    }

    impl MockReply {
        fn json(status: u16, body: Value) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body,
            }
        }

        fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
            self.headers.push((name.into(), value.into()));
            self
        }
    }

    struct Fixture {
        base: String,
        requests: tokio::sync::mpsc::UnboundedReceiver<String>,
        replies: tokio::sync::mpsc::UnboundedSender<MockReply>,
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
            let (replies, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<MockReply>();
            let task = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let request = read_request(&mut socket).await;
                    if request_tx.send(request).is_err() {
                        return;
                    }
                    let Some(reply) = reply_rx.recv().await else { return };
                    let body = reply.body.to_string();
                    let mut extra = String::new();
                    for (k, v) in reply.headers {
                        extra.push_str(&format!("{k}: {v}\r\n"));
                    }
                    let response = format!(
                        "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        reply.status,
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            });
            Self {
                base,
                requests,
                replies,
                task,
            }
        }

        fn reply(&self, status: u16, value: Value) {
            self.replies.send(MockReply::json(status, value)).unwrap();
        }

        fn reply_with(&self, reply: MockReply) {
            self.replies.send(reply).unwrap();
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

    fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        let head = request.split("\r\n\r\n").next()?;
        head.lines().skip(1).find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    #[tokio::test]
    async fn publish_only_link_sends_host_proof_and_applies_relay_credentials_before_preferences() {
        let mut relay = Fixture::new().await;
        let mut host = Fixture::new().await;
        let cloud = cloud(&relay);
        cloud.account.lock().await.clerk = Some(ClerkSession {
            client_token: "clerk-client".into(),
            session_id: "sess_link".into(),
            identity: None,
        });
        let jwt = fake_jwt("user_alice", now_ms() / 1000 + 300);
        relay.reply(200, json!({"jwt": jwt}));
        relay.reply(200, json!({"challenge": "challenge", "expiresAt": "2030-01-01T00:00:00Z"}));
        relay.reply(200, json!({
            "ok": true, "environmentId": "env-studio", "cloudUserId": "user_alice",
            "endpoint": {
                "httpBaseUrl": host.base, "wsBaseUrl": host.base.replacen("http", "ws", 1),
                "providerKind": "manual"
            },
            "endpointRuntime": null, "relayIssuer": relay.base,
            "environmentCredential": "relay-credential", "cloudMintPublicKey": "public-key"
        }));
        host.reply(200, json!("signed-host-proof"));
        host.reply(200, json!({"ok": true, "endpointRuntimeStatus": null}));
        host.reply(200, json!({"publishAgentActivity": true}));
        cloud.link_environment(
            &crate::auth::Credentials { http_base: host.base.clone(), access_token: "host-token".into() },
            "env-studio",
            false,
            true,
        ).await.unwrap();

        let body = |request: &str| -> Value {
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
        };
        let _mint = relay.request().await;
        let challenge = relay.request().await;
        assert!(challenge.starts_with("POST /v1/client/environment-link-challenges "));
        assert_eq!(header_value(&challenge, "authorization"), Some(format!("Bearer {jwt}").as_str()));
        assert_eq!(body(&challenge), json!({
            "notificationsEnabled": true, "liveActivitiesEnabled": true, "managedTunnelsEnabled": false
        }));
        let proof = host.request().await;
        assert!(proof.starts_with("POST /api/connect/link-proof "));
        assert_eq!(header_value(&proof, "authorization"), Some("Bearer host-token"));
        assert_eq!(body(&proof), json!({
            "challenge": "challenge", "relayIssuer": relay.base,
            "endpoint": {
                "httpBaseUrl": host.base, "wsBaseUrl": host.base.replacen("http", "ws", 1), "providerKind": "manual"
            },
            "origin": {"localHttpHost": "127.0.0.1", "localHttpPort": url::Url::parse(&host.base).unwrap().port()}
        }));
        let link = relay.request().await;
        assert!(link.starts_with("POST /v1/client/environment-links "));
        assert_eq!(body(&link), json!({
            "proof": "signed-host-proof",
            "notificationsEnabled": true, "liveActivitiesEnabled": true, "managedTunnelsEnabled": false
        }));
        let config = host.request().await;
        assert!(config.starts_with("POST /api/connect/relay-config "));
        assert_eq!(body(&config), json!({
            "relayUrl": relay.base, "relayIssuer": relay.base, "cloudUserId": "user_alice",
            "environmentCredential": "relay-credential", "cloudMintPublicKey": "public-key",
            "endpointRuntime": null
        }));
        let preferences = host.request().await;
        assert!(preferences.starts_with("POST /api/connect/preferences "));
        assert_eq!(body(&preferences), json!({"publishAgentActivity": true}));
    }

    #[tokio::test]
    async fn a_saved_relay_route_falls_back_when_ssh_preparation_fails() {
        let relay = Fixture::new().await;
        let cloud = cloud(&relay);
        let mut host = crate::test_server::FakeServer::start().await;
        let environment_id = uuid::Uuid::new_v4().to_string();
        host.set_descriptor(Some(json!({
            "environmentId": environment_id, "label": "Studio", "serverVersion": "0.1.9",
            "orchestrationProtocolVersion": 2,
        })));
        let host_base = host.credentials().http_base;
        let socket_base = host.ws_url();
        let primary = crate::auth::Credentials { http_base: host_base.clone(), access_token: "ssh-token".into() };
        let expires_at = dpop::now_epoch_ms() + 300_000;
        dpop::put_direct_credential(&host_base, dpop::DpopConnectionCredential::new(
            primary.access_token.clone(), expires_at, Some(environment_id.clone()),
        ));
        cloud.account.lock().await.clerk = Some(ClerkSession {
            client_token: "clerk-client".into(),
            session_id: "sess_fallback".into(),
            identity: None,
        });
        relay.reply(200, json!({"jwt": fake_jwt("user_alice", now_ms() / 1000 + 300)}));
        relay.reply(200, json!({
            "access_token": "dpop-access", "issued_token_type": RELAY_ACCESS_TOKEN_TYPE,
            "token_type": "DPoP", "expires_in": 300, "scope": RELAY_DPOP_SCOPE,
        }));
        let relay_seed = format!("relay-seed-{environment_id}");
        let relay_credential = format!("relay-credential-{environment_id}");
        let expires = "2030-01-01T00:00:00Z";
        relay.reply(200, json!({
            "environmentId": environment_id,
            "endpoint": {"httpBaseUrl": host_base, "wsBaseUrl": socket_base, "providerKind": "cloudflare_tunnel"},
            "credential": relay_credential,
            "expiresAt": expires,
        }));
        let route = crate::routes::CandidateRoute::relay(EnvironmentConnectResponse {
            environment_id: environment_id.clone(),
            endpoint: t3_core::relay::Endpoint {
                http_base_url: host_base.clone(),
                ws_base_url: socket_base,
                provider_kind: "cloudflare_tunnel".into(),
            },
            credential: relay_seed.clone(),
            expires_at: expires.into(),
        });
        let prepare: crate::supervisor::Prepare = Arc::new(|| Box::pin(async {
            Err(crate::supervisor::PrepareError::Transient("SSH is unreachable".into()))
        }));
        let (client, _events) = crate::supervisor::start_with_routes(
            &tokio::runtime::Handle::current(), primary.clone(), vec![route], Some(environment_id),
            Some(prepare), Some(cloud),
        );
        let connected = host.connection().await;
        assert!(connected.path.contains("connectionMethod=relay"));
        assert!(connected.path.contains("dpop="));
        client.connection_generation().wait_for(|generation| *generation > 0).await.unwrap();
        assert_eq!(client.credentials().http_base, host_base);
        assert_eq!(client.credentials().access_token, relay_seed);
        assert_eq!(dpop::find_direct_credential(&host_base, &relay_seed).unwrap().1.access_token, relay_credential);
        assert_eq!(dpop::get_direct_credential(&host_base).unwrap().access_token, primary.access_token);
    }

    fn cloud(fixture: &Fixture) -> Arc<Cloud> {
        Arc::new(Cloud {
            config: Ok(Config {
                hosted: "https://app.t3.codes".into(),
                relay: fixture.base.clone(),
                clerk_origin: fixture.base.clone(),
                clerk_jwt_template: "t3-relay".into(),
                relay_client_id: "t3-web".into(),
                token_endpoint: format!("{}/oauth/token", fixture.base),
                client_id: "client".into(),
                port: 0,
            }),
            home: Some(std::env::temp_dir().join(format!("t3-connect-test-{}", uuid::Uuid::new_v4()))),
            account: Arc::new(Mutex::new(Account {
                loaded: true,
                clerk: None,
                token: None,
                cached_jwt: None,
                cached_dpop: None,
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
    async fn unlink_uses_signed_in_bearer_and_canonical_environment_id() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.unlink_environment("test/id").await.unwrap();
        assert!(fixture.requests.try_recv().is_err());

        cloud.account.lock().await.clerk = Some(ClerkSession {
            client_token: "client-jwt".into(),
            session_id: "session".into(),
            identity: None,
        });
        let jwt = fake_jwt("test", now_ms() / 1000 + 3_600);
        fixture.reply_with(MockReply::json(200, json!({"jwt": jwt})));
        fixture.reply_with(MockReply::json(200, json!({"ok": true})));

        cloud.unlink_environment("test/id").await.unwrap();
        assert!(fixture.request().await.starts_with("POST /v1/client/sessions/session/tokens/t3-relay"));
        let unlink = fixture.request().await;
        assert!(unlink.starts_with("DELETE /v1/client/environment-links/test%2Fid HTTP"), "{unlink}");
        assert_eq!(header_value(&unlink, "authorization"), Some(format!("Bearer {jwt}").as_str()));

        {
            let mut account = cloud.account.lock().await;
            account.clerk = None;
            account.token = Some(token(now_ms() + 3_600_000));
        }
        fixture.reply_with(MockReply::json(200, json!({"ok": false})));
        cloud.unlink_environment("test/id").await.unwrap();
        let unlink = fixture.request().await;
        assert!(unlink.starts_with("DELETE /v1/client/environment-links/test%2Fid HTTP"), "{unlink}");
        assert_eq!(header_value(&unlink, "authorization"), Some("Bearer fixture-access"));
        let _ = std::fs::remove_dir_all(cloud.home().unwrap());
    }

    fn fake_jwt(sub: &str, exp_secs: u64) -> String {
        let header = connect::encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = connect::encode(format!(r#"{{"sub":"{sub}","exp":{exp_secs}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
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
        let params: std::collections::HashMap<_, _> = url::form_urlencoded::parse(url.fragment().unwrap().as_bytes())
            .into_owned()
            .collect();
        let callback = format!("http://127.0.0.1:{}/callback", params["port"]);
        assert_eq!(
            http()
                .get(format!("{callback}?state=wrong&code=bad"))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
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
                std::fs::metadata(cloud.home().unwrap().join("cloud-oauth.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        cloud.sign_out().await.unwrap();
        assert!(cloud.identity().await.unwrap().is_none());
        assert!(!cloud.home().unwrap().join("cloud-oauth.json").exists());
        let _ = std::fs::remove_dir_all(cloud.home().unwrap());
    }

    #[tokio::test]
    async fn refresh_rotation_is_serialized_and_survives_a_cancelled_caller() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(0));
        let first = {
            let c = cloud.clone();
            tokio::spawn(async move { c.legacy_oauth_token().await })
        };
        assert!(fixture.request().await.contains("grant_type=refresh_token"));
        first.abort();
        fixture.reply(
            200,
            json!({"access_token":"rotated","refresh_token":"rotated-refresh","expires_in":3600,"token_type":"Bearer"}),
        );
        // The second reader waits for the first refresh's persistence under the same lock.
        assert_eq!(cloud.legacy_oauth_token().await.unwrap().0, "rotated");
        let stored = crate::store::load_cloud_token(cloud.home().unwrap()).unwrap().unwrap();
        assert_eq!(stored.refresh_token, "rotated-refresh");
        assert_eq!(stored.identity.as_deref(), Some("Fixture account"));
        cloud.sign_out().await.unwrap();
        let _ = std::fs::remove_dir_all(cloud.home().unwrap());
    }

    #[tokio::test]
    async fn discovery_calls_v1_environments_and_rejects_after_sign_out() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        cloud.account.lock().await.token = Some(token(u64::MAX));
        let environment = json!({"environmentId":"env","label":"Mac","endpoint":{"httpBaseUrl":"https://old.test","wsBaseUrl":"wss://old.test/ws","providerKind":"cloudflare_tunnel"},"future":true});
        fixture.reply(200, json!({"environments":[environment]}));
        let discovery = cloud.discover().await.unwrap();
        let listing = fixture.request().await;
        assert!(listing.starts_with("GET /v1/environments "), "{listing}");
        assert!(listing.contains("Bearer fixture-access"));
        assert_eq!(discovery.environments.len(), 1);
        assert_eq!(discovery.environments[0].environment_id, "env");

        let discovery_race = {
            let cloud = cloud.clone();
            tokio::spawn(async move { cloud.discover().await })
        };
        assert!(fixture.request().await.starts_with("GET /v1/environments "));
        cloud.sign_out().await.unwrap();
        fixture.reply(200, json!({"environments":[]}));
        assert!(discovery_race.await.unwrap().is_err());
        assert!(cloud.identity().await.unwrap().is_none());
        let _ = std::fs::remove_dir_all(cloud.home().unwrap());
    }

    #[tokio::test]
    async fn clerk_native_oauth_flow_persists_rotated_client_token_and_deletes_session_on_sign_out() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);

        // 1. Bootstrap GET /v1/client?_is_native=1 -> Authorization header
        fixture.reply_with(MockReply::json(200, json!({"response":{}})).with_header("Authorization", "client-tok-1"));
        // 2. POST /v1/client/sign_ins?_is_native=1 -> rotated header + redirect URL
        fixture.reply_with(
            MockReply::json(
                200,
                json!({
                    "response": {
                        "first_factor_verification": {
                            "external_verification_redirect_url": "https://accounts.google.com/oauth"
                        }
                    }
                }),
            )
            .with_header("Authorization", "client-tok-2"),
        );

        let pending = cloud.start_oauth_sign_in(SocialProvider::Google).await.unwrap();
        assert_eq!(pending.redirect_url, "https://accounts.google.com/oauth");

        let req1 = fixture.request().await;
        assert!(req1.starts_with("GET /v1/client?_is_native=1 "), "{req1}");
        let req2 = fixture.request().await;
        assert!(req2.starts_with("POST /v1/client/sign_ins?_is_native=1 "), "{req2}");
        assert_eq!(header_value(&req2, "authorization"), Some("client-tok-1"));
        let body2 = req2.split("\r\n\r\n").nth(1).unwrap();
        let form2: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body2.as_bytes()).into_owned().collect();
        assert_eq!(form2["strategy"], "oauth_google");
        assert_eq!(form2["redirect_url"], CLERK_NATIVE_CALLBACK_URL);
        assert_eq!(form2["action_complete_redirect_url"], CLERK_NATIVE_CALLBACK_URL);

        // 3. Callback completion GET /v1/client?_is_native=1&rotating_token_nonce=...
        fixture.reply_with(
            MockReply::json(
                200,
                json!({
                    "response": {
                        "last_active_session_id": "sess_oauth_1",
                        "sessions": [{
                            "id": "sess_oauth_1",
                            "status": "active",
                            "user": {
                                "primary_email_address_id": "em_1",
                                "email_addresses": [{"id": "em_1", "email_address": "alice@example.test"}]
                            }
                        }]
                    }
                }),
            )
            .with_header("Authorization", "client-tok-3"),
        );

        let outcome = cloud
            .complete_oauth_sign_in(pending, "com.t3tools.t3code://callback?rotating_token_nonce=nonce-xyz")
            .await
            .unwrap();
        let ClerkAuthOutcome::Complete(session) = outcome else {
            panic!("expected complete session");
        };
        assert_eq!(session.session_id, "sess_oauth_1");
        assert_eq!(session.client_token, "client-tok-3");
        assert_eq!(session.identity.as_deref(), Some("alice@example.test"));

        let req3 = fixture.request().await;
        assert!(
            req3.starts_with("GET /v1/client?_is_native=1&rotating_token_nonce=nonce-xyz "),
            "{req3}"
        );
        assert_eq!(header_value(&req3, "authorization"), Some("client-tok-2"));

        let persisted = crate::store::load_clerk_session(cloud.home().unwrap()).unwrap().unwrap();
        assert_eq!(persisted, session);
        assert!(cloud.has_clerk_session().await.unwrap());

        // 4. Sign out deletes session on Clerk and clears private file.
        fixture.reply(200, json!({"response": {"status": "ended"}}));
        cloud.sign_out().await.unwrap();
        let req4 = fixture.request().await;
        assert!(req4.starts_with("DELETE /v1/client/sessions/sess_oauth_1?_is_native=1 "), "{req4}");
        assert_eq!(header_value(&req4, "authorization"), Some("client-tok-3"));
        assert!(!cloud.home().unwrap().join("cloud-clerk.json").exists());
        assert!(!cloud.has_clerk_session().await.unwrap());
        let _ = std::fs::remove_dir_all(cloud.home().unwrap());
    }

    #[tokio::test]
    async fn clerk_native_email_code_and_second_factor_flow_works() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);

        fixture.reply_with(MockReply::json(200, json!({"response":{}})).with_header("Authorization", "tok-init"));
        fixture.reply_with(
            MockReply::json(
                200,
                json!({
                    "response": {
                        "id": "sia_email_1",
                        "supported_first_factors": [
                            { "strategy": "email_code", "email_address_id": "idn_email_1" },
                            { "strategy": "password" }
                        ]
                    }
                }),
            )
            .with_header("Authorization", "tok-started"),
        );

        let mut pending = cloud.start_email_sign_in("dev@example.test").await.unwrap();
        assert!(pending.challenge.supports_email_code);
        assert!(pending.challenge.supports_password);
        let _ = fixture.request().await;
        let _ = fixture.request().await;

        fixture.reply_with(MockReply::json(200, json!({"response":{"status":"needs_first_factor"}})).with_header("Authorization", "tok-prepared"));
        cloud.prepare_email_code(&mut pending).await.unwrap();
        let prep_req = fixture.request().await;
        assert!(
            prep_req.starts_with("POST /v1/client/sign_ins/sia_email_1/prepare_first_factor?_is_native=1 "),
            "{prep_req}"
        );
        assert_eq!(header_value(&prep_req, "authorization"), Some("tok-started"));

        // First factor requires 2FA.
        fixture.reply_with(
            MockReply::json(
                200,
                json!({
                    "response": {
                        "id": "sia_email_1",
                        "status": "needs_second_factor",
                        "supported_second_factors": [{ "strategy": "totp" }, { "strategy": "backup_code" }]
                    }
                }),
            )
            .with_header("Authorization", "tok-2fa"),
        );
        let outcome = cloud
            .attempt_email_first_factor(&pending, FirstFactorKind::EmailCode, "123456")
            .await
            .unwrap();
        let ClerkAuthOutcome::NeedsSecondFactor(sf) = outcome else {
            panic!("expected 2FA challenge");
        };
        let first_req = fixture.request().await;
        assert!(first_req.contains("strategy=email_code"));
        assert!(first_req.contains("code=123456"));

        // Complete 2FA with TOTP.
        fixture.reply_with(
            MockReply::json(
                200,
                json!({
                    "response": { "status": "complete", "created_session_id": "sess_2fa" },
                    "client": {
                        "last_active_session_id": "sess_2fa",
                        "sessions": [{
                            "id": "sess_2fa",
                            "status": "active",
                            "user": { "username": "dev_user" }
                        }]
                    }
                }),
            )
            .with_header("Authorization", "tok-final"),
        );
        let session = cloud
            .attempt_second_factor(&sf, SecondFactorKind::Totp, "654321")
            .await
            .unwrap();
        assert_eq!(session.session_id, "sess_2fa");
        assert_eq!(session.client_token, "tok-final");
        assert_eq!(session.identity.as_deref(), Some("dev_user"));
        let sf_req = fixture.request().await;
        assert!(
            sf_req.starts_with("POST /v1/client/sign_ins/sia_email_1/attempt_second_factor?_is_native=1 "),
            "{sf_req}"
        );
        assert_eq!(header_value(&sf_req, "authorization"), Some("tok-2fa"));
        assert!(sf_req.contains("strategy=totp"));
        let _ = std::fs::remove_dir_all(cloud.home().unwrap());
    }

    #[tokio::test]
    async fn dpop_exchange_status_retry_and_connect_credential_registration() {
        let mut fixture = Fixture::new().await;
        let cloud = cloud(&fixture);
        let home = cloud.home().unwrap().to_owned();
        let initial_session = ClerkSession {
            client_token: "clerk-tok-1".into(),
            session_id: "sess_dpop".into(),
            identity: Some("alice@example.test".into()),
        };
        crate::store::save_clerk_session(&home, &initial_session).unwrap();
        cloud.account.lock().await.clerk = Some(initial_session);

        let future_exp = (now_ms() / 1000) + 300;
        let jwt1 = fake_jwt("user_alice", future_exp);
        let jwt2 = fake_jwt("user_alice", future_exp + 60);

        // 1. Mint Clerk t3-relay JWT (rotates client_token -> clerk-tok-2)
        fixture.reply_with(MockReply::json(200, json!({"jwt": jwt1})).with_header("Authorization", "clerk-tok-2"));
        // 2. Exchange DPoP token (POST /v1/client/dpop-token)
        fixture.reply(
            200,
            json!({
                "access_token": "dpop-access-1",
                "issued_token_type": RELAY_ACCESS_TOKEN_TYPE,
                "token_type": "DPoP",
                "expires_in": 300,
                "scope": RELAY_DPOP_SCOPE
            }),
        );
        // 3. First /status returns 401 invalid_bearer -> triggers 1x retry!
        fixture.reply(
            401,
            json!({"_tag": "RelayAuthInvalidError", "reason": "invalid_bearer", "traceId": "tr-1"}),
        );
        // 4. Retry forces fresh Clerk JWT mint + DPoP exchange + /status succeeds
        fixture.reply(200, json!({"jwt": jwt2}));
        fixture.reply(
            200,
            json!({
                "access_token": "dpop-access-2",
                "issued_token_type": RELAY_ACCESS_TOKEN_TYPE,
                "token_type": "DPoP",
                "expires_in": 300,
                "scope": RELAY_DPOP_SCOPE
            }),
        );
        fixture.reply(
            200,
            json!({
                "environmentId": "env-studio",
                "endpoint": {
                    "httpBaseUrl": "https://studio.t3.codes",
                    "wsBaseUrl": "wss://studio.t3.codes/ws",
                    "providerKind": "cloudflare_tunnel"
                },
                "status": "online",
                "checkedAt": "2026-11-20T18:00:00.000Z"
            }),
        );

        let status = cloud.environment_status("env-studio").await.unwrap();
        assert_eq!(status.status, t3_core::relay::EnvironmentStatusValue::Online);

        // Verify rotated client_token was persisted to cloud-clerk.json.
        assert_eq!(
            crate::store::load_clerk_session(&home).unwrap().unwrap().client_token,
            "clerk-tok-2"
        );

        // Verify request 1: Clerk JWT mint
        let r1 = fixture.request().await;
        assert!(r1.starts_with("POST /v1/client/sessions/sess_dpop/tokens/t3-relay?_is_native=1 "));
        assert_eq!(header_value(&r1, "authorization"), Some("clerk-tok-1"));

        // Verify request 2: DPoP token exchange (proof with NO ath + exact form fields)
        let r2 = fixture.request().await;
        assert!(r2.starts_with("POST /v1/client/dpop-token "));
        let dpop_exchange_proof = header_value(&r2, "dpop").expect("dpop header on exchange");
        let verified_exchange = dpop::verify_dpop_proof(
            dpop_exchange_proof,
            "POST",
            &format!("{}/v1/client/dpop-token", fixture.base),
            (now_ms() / 1000) as i64,
            None,
            None,
            None,
        )
        .expect("valid RFC 9449 DPoP proof on exchange");
        let exchange_body = r2.split("\r\n\r\n").nth(1).unwrap();
        let exchange_form: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(exchange_body.as_bytes()).into_owned().collect();
        assert_eq!(exchange_form["grant_type"], RELAY_DPOP_GRANT_TYPE);
        assert_eq!(exchange_form["subject_token"], jwt1);
        assert_eq!(exchange_form["subject_token_type"], RELAY_JWT_SUBJECT_TOKEN_TYPE);
        assert_eq!(exchange_form["requested_token_type"], RELAY_ACCESS_TOKEN_TYPE);
        assert_eq!(exchange_form["resource"], fixture.base);
        assert_eq!(exchange_form["scope"], RELAY_DPOP_SCOPE);
        assert_eq!(exchange_form["client_id"], "t3-web");

        // Verify request 3: first /status attempt with dpop-access-1 + ath
        let r3 = fixture.request().await;
        assert!(r3.starts_with("POST /v1/environments/env-studio/status "));
        assert_eq!(header_value(&r3, "authorization"), Some("DPoP dpop-access-1"));
        let status_proof_1 = header_value(&r3, "dpop").expect("dpop header on status");
        dpop::verify_dpop_proof(
            status_proof_1,
            "POST",
            &format!("{}/v1/environments/env-studio/status", fixture.base),
            (now_ms() / 1000) as i64,
            Some(&verified_exchange.thumbprint),
            Some("dpop-access-1"),
            None,
        )
        .expect("valid DPoP proof with ath");

        // Verify retry requests 4, 5, 6 used rotated clerk-tok-2 and fresh dpop-access-2
        let r4 = fixture.request().await;
        assert_eq!(header_value(&r4, "authorization"), Some("clerk-tok-2"));
        let _r5 = fixture.request().await;
        let r6 = fixture.request().await;
        assert_eq!(header_value(&r6, "authorization"), Some("DPoP dpop-access-2"));

        // 7. Connect environment reuses cached dpop-access-2 (no new mint/exchange!) and registers credential.
        fixture.reply(
            200,
            json!({
                "environmentId": "env-studio",
                "endpoint": {
                    "httpBaseUrl": "https://studio.t3.codes",
                    "wsBaseUrl": "wss://ws.studio.t3.codes/ws",
                    "providerKind": "cloudflare_tunnel"
                },
                "credential": "env-cred-live",
                "expiresAt": "2026-11-20T18:30:00.000Z"
            }),
        );
        let connected = cloud
            .connect_and_register_environment("env-studio", None)
            .await
            .unwrap();
        assert_eq!(connected.credential, "env-cred-live");
        assert_eq!(connected.expires_at_epoch_ms(), Some(1_795_199_400_000));

        let r7 = fixture.request().await;
        assert!(r7.starts_with("POST /v1/environments/env-studio/connect "), "{r7}");
        assert_eq!(header_value(&r7, "authorization"), Some("DPoP dpop-access-2"));
        let connect_proof = header_value(&r7, "dpop").expect("dpop header on connect");
        dpop::verify_dpop_proof(
            connect_proof,
            "POST",
            &format!("{}/v1/environments/env-studio/connect", fixture.base),
            (now_ms() / 1000) as i64,
            Some(&verified_exchange.thumbprint),
            Some("dpop-access-2"),
            None,
        )
        .expect("valid DPoP proof with ath on connect");
        let connect_body: Value = serde_json::from_str(r7.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            connect_body["clientProofKeyThumbprint"].as_str(),
            Some(verified_exchange.thumbprint.as_str())
        );

        let reg_http = dpop::get_direct_credential_in(Some(home.clone()), "https://studio.t3.codes")
            .expect("registered http credential");
        assert_eq!(reg_http.access_token, "env-cred-live");
        assert_eq!(reg_http.expires_at_epoch_ms, 1_795_199_400_000);
        let reg_ws = dpop::get_direct_credential_in(Some(home.clone()), "https://ws.studio.t3.codes")
            .expect("registered ws origin credential");
        assert_eq!(reg_ws.access_token, "env-cred-live");

        let _ = std::fs::remove_dir_all(&home);
    }
}
