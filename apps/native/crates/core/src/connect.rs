//! Ports shared/connectAuth.ts, relayAuth.ts and server/cloud/CliTokenManager.ts.
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn authorize_url(hosted: &str, state: &str, challenge: &str, port: u16) -> Result<String, url::ParseError> {
    let mut url = url::Url::parse(hosted)?.join("/connect")?;
    let fragment = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("state", state)
        .append_pair("challenge", challenge)
        .append_pair("port", &port.to_string())
        .finish();
    url.set_fragment(Some(&fragment));
    Ok(url.into())
}
pub fn clerk_origin(key: &str) -> Option<String> {
    let encoded = key.strip_prefix("pk_live_").or_else(|| key.strip_prefix("pk_test_"))?;
    let decoded = String::from_utf8(STANDARD.decode(encoded).ok()?).ok()?;
    let host = decoded.strip_suffix('$').unwrap_or(&decoded);
    if host.is_empty() || host.contains(['/', '?', '#', '@', ':']) {
        return None;
    }
    let url = url::Url::parse(&format!("https://{host}")).ok()?;
    Some(url.origin().ascii_serialization())
}
#[derive(Debug, PartialEq, Eq)]
pub enum Callback {
    Invalid,
    Denied,
    Code(String),
}
pub fn callback(target: &str, expected_state: &str) -> Callback {
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Invalid;
    };
    let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
    if url.path() != "/callback" || params.get("state").map(|s| s.as_ref()) != Some(expected_state) {
        return Callback::Invalid;
    }
    if params.contains_key("error") {
        return Callback::Denied;
    }
    match params.get("code").filter(|s| !s.is_empty()) {
        Some(code) => Callback::Code(code.to_string()),
        None => Callback::Invalid,
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_epoch_ms: u64,
    pub identity: Option<String>,
}
impl Token {
    pub fn needs_refresh(&self, now: u64) -> bool {
        self.expires_at_epoch_ms <= now.saturating_add(300_000)
    }
}
#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_in: u64,
    pub token_type: String,
}
impl TokenResponse {
    pub fn into_token(self, previous: Option<&Token>, now: u64) -> Result<Token, &'static str> {
        if self.access_token.is_empty() || !self.token_type.eq_ignore_ascii_case("bearer") || self.expires_in == 0 {
            return Err("Invalid T3 Connect token response.");
        }
        let identity = self
            .id_token
            .as_deref()
            .and_then(identity)
            .or_else(|| previous.and_then(|p| p.identity.clone()));
        Ok(Token {
            access_token: self.access_token,
            refresh_token: self.refresh_token.or_else(|| previous.map(|p| p.refresh_token.clone())).unwrap_or_default(),
            expires_at_epoch_ms: now.saturating_add(self.expires_in.saturating_mul(1000)),
            identity,
        })
    }
}
fn identity(jwt: &str) -> Option<String> {
    let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(jwt.split('.').nth(1)?).ok()?).ok()?;
    ["email", "preferred_username", "sub"]
        .into_iter()
        .find_map(|k| claims[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
}
pub fn permanent_refresh_failure(status: u16) -> bool {
    (400..500).contains(&status) && status != 408 && status != 429
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_and_fragment_match_the_cli() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(
            authorize_url("https://app.t3.codes", "s", "c", 34338).unwrap(),
            "https://app.t3.codes/connect#state=s&challenge=c&port=34338"
        );
        assert_eq!(callback("/callback?state=s&code=c", "s"), Callback::Code("c".into()));
        for path in ["/callback?state=other&code=c", "/callback?state=s", "/wrong?state=s&code=c"] {
            assert_eq!(callback(path, "s"), Callback::Invalid);
        }
        assert_eq!(callback("/callback?state=s&error=access_denied", "s"), Callback::Denied);
        assert_eq!(clerk_origin("pk_live_Y2xlcmsudDMuY29kZXMk").as_deref(), Some("https://clerk.t3.codes"));
        for key in ["pk_live_", "wrong", "pk_live_aHR0cHM6Ly9iYWQ="] {
            assert_eq!(clerk_origin(key), None);
        }
    }
    #[test]
    fn token_decoding_preserves_rotation_and_identity_without_exposing_claims_as_auth() {
        let payload = encode(br#"{"email":"trevor@example.test","sub":"user"}"#);
        let response = serde_json::json!({"access_token":"access","refresh_token":"refresh","id_token":format!("a.{payload}.b"),"expires_in":3600,"token_type":"Bearer","future":true});
        let token = serde_json::from_value::<TokenResponse>(response).unwrap().into_token(None, 1000).unwrap();
        assert_eq!(token.identity.as_deref(), Some("trevor@example.test"));
        let next = serde_json::from_str::<TokenResponse>(r#"{"access_token":"next","expires_in":3600,"token_type":"Bearer"}"#)
            .unwrap()
            .into_token(Some(&token), 2000)
            .unwrap();
        assert_eq!(
            (next.refresh_token.as_str(), next.identity.as_deref()),
            ("refresh", Some("trevor@example.test"))
        );
        assert!(!token.needs_refresh(1001));
        assert!(token.needs_refresh(3_301_000));
        for status in [400, 401, 403] {
            assert!(permanent_refresh_failure(status));
        }
        for status in [408, 429, 500] {
            assert!(!permanent_refresh_failure(status));
        }
        assert!(serde_json::from_str::<TokenResponse>(r#"{"access_token":"a"}"#).is_err());
    }
}
