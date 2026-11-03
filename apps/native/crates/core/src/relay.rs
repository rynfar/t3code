//! Account-list models and listing errors from client-runtime/relay/{managedRelay,errorPresentation}.ts.
use serde::Deserialize;
use serde_json::Value;

pub const CONNECTION_UNAVAILABLE: &str = "Connecting through T3 Connect is not supported with native sign-in yet. Use a pairing link or SSH.";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub http_base_url: String,
    pub ws_base_url: String,
    pub provider_kind: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub environment_id: String,
    pub label: String,
    pub endpoint: Endpoint,
}
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub listed: bool,
    pub refreshing: bool,
    pub environments: Vec<Environment>,
}
pub fn error_message(status: u16, error: &Value) -> String {
    let message = match error["_tag"].as_str().unwrap_or("") {
        "RelayAuthInvalidError" => match error["reason"].as_str() {
            Some("missing_bearer" | "invalid_bearer") => "Relay rejected the cloud session token.".into(),
            _ => "Relay rejected the authenticated request.".into(),
        },
        "RelayInternalError" => format!("Relay encountered an internal error ({}).", error["reason"].as_str().unwrap_or("unknown")),
        _ if status == 504 => {
            "Relay timed out before it finished the request (HTTP 504). Try again in a few minutes. If it keeps failing, copy the trace ID when you report it."
                .into()
        }
        _ => format!("Relay returned an unexpected HTTP {status}."),
    };
    match error["traceId"].as_str().filter(|s| !s.is_empty()) {
        Some(trace) => format!("{message} Trace ID: {trace}"),
        None => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listing_errors_preserve_diagnostics_and_accept_unknown_errors() {
        assert_eq!(
            error_message(401, &json!({"_tag":"RelayAuthInvalidError","reason":"invalid_bearer","traceId":"trace"})),
            "Relay rejected the cloud session token. Trace ID: trace"
        );
        assert_eq!(
            error_message(500, &json!({"_tag":"RelayInternalError","reason":"upstream_unavailable"})),
            "Relay encountered an internal error (upstream_unavailable)."
        );
        assert!(error_message(504, &Value::Null).contains("HTTP 504"));
        assert_eq!(error_message(503, &json!({"_tag":"FutureError"})), "Relay returned an unexpected HTTP 503.");
    }
}
