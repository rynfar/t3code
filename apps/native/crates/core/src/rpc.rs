//! Effect RPC over a framed transport with JSON serialization, as `effect/rpc` speaks it.
//! Mirrors `RpcMessage.FromClientEncoded` / `FromServerEncoded`. A server frame may carry one
//! message or a JSON array of them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Str(String),
    Num(u64),
}

impl From<u64> for RequestId {
    fn from(value: u64) -> Self {
        RequestId::Str(value.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "_tag")]
pub enum ClientMessage {
    Request {
        id: RequestId,
        tag: String,
        payload: Value,
        headers: Vec<(String, String)>,
    },
    Ack {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Interrupt {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Ping,
    Eof,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "_tag")]
pub enum ServerMessage {
    Chunk {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        values: Vec<Value>,
    },
    Exit {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        exit: ExitEncoded,
    },
    Defect {
        defect: Value,
    },
    Pong,
    ClientProtocolError {
        error: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "_tag")]
pub enum ExitEncoded {
    Success {
        #[serde(default)]
        value: Value,
    },
    Failure {
        cause: Vec<CauseEncoded>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "_tag")]
pub enum CauseEncoded {
    Fail { error: Value },
    Die { defect: Value },
    Interrupt,
    #[serde(other)]
    Unknown,
}

/// A failed RPC, flattened for display. `tag` is the tagged error's `_tag` when the server sent one.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct RpcFailure {
    pub tag: Option<String>,
    pub message: String,
    pub error: Value,
}

impl ExitEncoded {
    pub fn into_result(self) -> Result<Value, RpcFailure> {
        match self {
            ExitEncoded::Success { value } => Ok(value),
            ExitEncoded::Failure { cause } => Err(failure_from_causes(cause)),
        }
    }
}

fn failure_from_causes(causes: Vec<CauseEncoded>) -> RpcFailure {
    let first = causes.into_iter().find(|cause| !matches!(cause, CauseEncoded::Interrupt));
    match first {
        Some(CauseEncoded::Fail { error }) => {
            let tag = error.get("_tag").and_then(Value::as_str).map(str::to_owned);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| tag.clone())
                .unwrap_or_else(|| error.to_string());
            RpcFailure { tag, message, error }
        }
        Some(CauseEncoded::Die { defect }) => RpcFailure {
            tag: None,
            message: defect
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| defect.to_string()),
            error: defect,
        },
        _ => RpcFailure { tag: None, message: "Interrupted".into(), error: Value::Null },
    }
}

pub fn decode_server_frame(text: &str) -> Result<Vec<ServerMessage>, serde_json::Error> {
    if text.trim_start().starts_with('[') {
        serde_json::from_str(text)
    } else {
        serde_json::from_str(text).map(|message| vec![message])
    }
}

pub fn encode_client_message(message: &ClientMessage) -> String {
    serde_json::to_string(message).expect("client messages always serialize")
}

#[cfg(test)]
mod tests;
