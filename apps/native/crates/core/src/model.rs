use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `ModelSelection` on the wire. Older servers send `provider` instead of `instanceId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    #[serde(alias = "provider")]
    pub instance_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}
