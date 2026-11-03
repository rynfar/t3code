//! Ports client-runtime/relay/{discovery,errorPresentation}.ts and web/cloud/dpop.ts.
use crate::connect::encode;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub http_base_url: String,
    pub ws_base_url: String,
    pub provider_kind: String,
}
impl Endpoint {
    pub fn is_secure(&self) -> bool {
        [(self.http_base_url.as_str(), "https"), (self.ws_base_url.as_str(), "wss")]
            .iter()
            .all(|(s, scheme)| {
                url::Url::parse(s).is_ok_and(|u| {
                    u.scheme() == *scheme
                        && u.host_str().is_some()
                        && u.username().is_empty()
                        && u.password().is_none()
                        && u.query().is_none()
                        && u.fragment().is_none()
                })
            })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub environment_id: String,
    pub label: String,
    pub endpoint: Endpoint,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub enum Availability {
    #[serde(rename = "online")]
    Online,
    #[serde(rename = "offline")]
    Offline,
    #[default]
    #[serde(other)]
    Unknown,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub environment_id: String,
    pub endpoint: Endpoint,
    pub status: Availability,
    pub descriptor: Option<crate::server_config::EnvironmentDescriptor>,
    pub offline_reason: Option<String>,
    pub trace_id: Option<String>,
}
impl Status {
    pub fn validate(&self, environment: &Environment) -> Result<(), &'static str> {
        if self.environment_id != environment.environment_id {
            return Err("Relay returned status for a different environment.");
        }
        if self.endpoint != environment.endpoint {
            return Err("Relay returned status for a different environment endpoint.");
        }
        if self.descriptor.as_ref().is_some_and(|d| d.environment_id != environment.environment_id) {
            return Err("Relay returned a descriptor for a different environment.");
        }
        Ok(())
    }
    pub fn message(&self) -> &'static str {
        if self.offline_reason.as_deref() == Some("tunnel_released") {
            return "Offline for a while, so its T3 Connect tunnel was removed. Start T3 Code on that computer and update it to the latest version to reconnect.";
        }
        match self.status {
            Availability::Online => "Online",
            Availability::Offline => "Offline",
            Availability::Unknown => "Checking…",
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct Discovered {
    pub environment: Environment,
    pub status: Option<Status>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub listed: bool,
    pub refreshing: bool,
    pub environments: Vec<Discovered>,
    pub error: Option<String>,
}
impl Discovery {
    pub fn registration(&self, id: &str) -> &'static str {
        if self.environments.iter().any(|e| e.environment.environment_id == id) {
            "registered"
        } else if self.listed {
            "unregistered"
        } else {
            "unknown"
        }
    }
}
pub fn error_message(status: u16, error: &Value) -> String {
    let reason = error["reason"].as_str().unwrap_or("unknown");
    let message = match error["_tag"].as_str().unwrap_or("") {
        "RelayAuthInvalidError" => match reason {
            "missing_bearer" | "invalid_bearer" => "Relay rejected the cloud session token.".into(),
            "invalid_dpop" => format!("Relay rejected the DPoP proof. {}", match error["dpopFailureReason"].as_str() {
                Some("time_window") => "Hint: Check that automatic date and time is enabled on both devices, then try again.",
                None => "Hint: Try again. If it still fails, clock skew may be the cause; check that automatic date and time is enabled on both devices.",
                _ => "Hint: Try again. If the problem continues, copy the trace ID.",
            }),
            _ => "Relay rejected the authenticated request.".into(),
        },
        "RelayEnvironmentConnectNotAuthorizedError" => match reason {
            "environment_link_not_found" => "Relay has no active link for this environment. If it was deregistered from T3 Connect, link it again on that machine.".into(),
            "managed_endpoint_allocation_not_ready" => "The environment's T3 Connect tunnel is still being set up.".into(),
            "endpoint_provider_not_managed" => "This environment is linked only to publish agent activity, so it cannot accept connections. On that computer, open Settings > Sharing & access and turn T3 Connect on.".into(),
            _ => format!("Relay rejected the environment connection request ({reason})."),
        },
        "RelayEnvironmentEndpointUnavailableError" => format!("Relay could not reach the environment endpoint ({reason})."),
        "RelayEnvironmentEndpointTimedOutError" => "Relay timed out while contacting the environment endpoint.".into(),
        "RelayInternalError" => format!("Relay encountered an internal error ({reason})."),
        _ if status == 504 => "Relay timed out before it finished the request (HTTP 504). Try again in a few minutes. If it keeps failing, copy the trace ID when you report it.".into(),
        _ => format!("Relay returned an unexpected HTTP {status}."),
    };
    match error["traceId"].as_str().filter(|s| !s.is_empty()) {
        Some(trace) => format!("{message} Trace ID: {trace}"),
        None => message,
    }
}

#[derive(Clone)]
pub struct ProofKey(SigningKey);
impl ProofKey {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, p256::ecdsa::Error> {
        SigningKey::from_bytes(bytes.into()).map(Self)
    }
    fn jwk(&self) -> Value {
        let point = self.0.verifying_key().to_encoded_point(false);
        json!({"crv":"P-256","kty":"EC","x":encode(point.x().expect("uncompressed x")),"y":encode(point.y().expect("uncompressed y"))})
    }
    pub fn thumbprint(&self) -> String {
        let j = self.jwk();
        let canonical = format!(
            r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
            j["x"].as_str().unwrap_or_default(),
            j["y"].as_str().unwrap_or_default()
        );
        encode(&Sha256::digest(canonical.as_bytes()))
    }
    pub fn proof(&self, method: &str, url: &str, token: Option<&str>, now: u64, jti: &str) -> Result<String, url::ParseError> {
        let mut url = url::Url::parse(url)?;
        url.set_query(None);
        url.set_fragment(None);
        let header = json!({"typ":"dpop+jwt","alg":"ES256","jwk":self.jwk()});
        let mut claims = json!({"htm":method.to_uppercase(),"htu":url.as_str(),"iat":now,"jti":jti});
        if let Some(token) = token {
            claims["ath"] = json!(encode(&Sha256::digest(token.as_bytes())));
        }
        let input = format!("{}.{}", encode(header.to_string().as_bytes()), encode(claims.to_string().as_bytes()));
        let signature: Signature = self.0.sign(input.as_bytes());
        Ok(format!("{input}.{}", encode(&signature.to_bytes())))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use p256::ecdsa::signature::Verifier;
    #[test]
    fn proofs_bind_the_key_method_url_and_token() {
        let key = ProofKey::from_bytes(&[1; 32]).unwrap();
        let jwt = key.proof("post", "https://relay.test/path?q=1#hash", Some("access"), 100, "id").unwrap();
        let parts: Vec<_> = jwt.split('.').collect();
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            claims,
            json!({"htm":"POST","htu":"https://relay.test/path","iat":100,"jti":"id","ath":encode(&Sha256::digest(b"access"))})
        );
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        assert!(
            key.0
                .verifying_key()
                .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
                .is_ok()
        );
        assert_eq!(key.thumbprint().len(), 43);
    }
    #[test]
    fn discovery_rejects_cross_environment_status_and_accepts_future_fields_and_states() {
        let endpoint = json!({"httpBaseUrl":"https://env.test","wsBaseUrl":"wss://env.test/ws","providerKind":"cloudflare_tunnel"});
        let environment: Environment = serde_json::from_value(json!({"environmentId":"a","label":"Mac","endpoint":endpoint})).unwrap();
        let mut status: Status = serde_json::from_value(json!({"environmentId":"a","endpoint":endpoint,"status":"future","future":true})).unwrap();
        assert!(status.validate(&environment).is_ok());
        assert_eq!(status.message(), "Checking…");
        status.environment_id = "b".into();
        assert!(status.validate(&environment).is_err());
        status.environment_id = "a".into();
        status.endpoint.http_base_url = "https://other.test".into();
        assert!(status.validate(&environment).is_err());
        let mut state = Discovery::default();
        assert_eq!(state.registration("a"), "unknown");
        state.listed = true;
        assert_eq!(state.registration("a"), "unregistered");
        assert!(environment.endpoint.is_secure());
        assert!(
            error_message(
                401,
                &json!({"_tag":"RelayAuthInvalidError","reason":"invalid_dpop","dpopFailureReason":"time_window"})
            )
            .contains("automatic date and time")
        );
        assert!(
            error_message(
                403,
                &json!({"_tag":"RelayEnvironmentConnectNotAuthorizedError","reason":"environment_link_not_found"})
            )
            .contains("link it again")
        );
        assert!(error_message(504, &Value::Null).contains("HTTP 504"));
    }
}
