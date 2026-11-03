//! MCP app I/O: `client-runtime/src/state/mcpApps.ts`, contracts `mcpApps.ts`, and
//! `McpAppFrame.tsx`'s tool preflight. All calls run on the network runtime.

use crate::session::RpcError;
use crate::supervisor::ClientHandle;
use serde::Deserialize;
use serde_json::{Value, json};
use t3_core::mcp_apps::{MAX_HTML_BYTES, Operation, Reference};

/// A cached app keeps its environment identity, never an authenticated supervisor handle.
pub struct AppConnection {
    environment: String,
}

impl AppConnection {
    pub fn new(client: &ClientHandle) -> Self {
        Self {
            environment: client.environment().into(),
        }
    }

    pub fn resolve(&self, current: Option<ClientHandle>) -> Option<ClientHandle> {
        current.filter(|client| client.environment() == self.environment)
    }
}

#[derive(Debug, Clone)]
pub struct Target {
    pub thread_id: String,
    pub item_id: String,
}

impl Target {
    fn payload(&self) -> Value {
        json!({"threadId":self.thread_id,"itemId":self.item_id})
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    #[serde(default)]
    pub callable: bool,
    #[serde(default)]
    pub read_only: bool,
    pub tool: Option<Value>,
}

fn request_error(error: RpcError) -> String {
    if let RpcError::Failure(failure) = &error
        && failure.tag.as_deref() == Some("McpAppRequestError")
    {
        let message = match failure.error["reason"].as_str() {
            Some("not-an-app") => "This tool call has no MCP app.",
            Some("provider-unsupported") => "This thread's provider cannot run MCP app requests.",
            Some("session-stopped") => "The app's thread is not running. Send a message in the thread that created it to use the app again.",
            Some("tool-not-callable") => "This app cannot call that tool.",
            Some("unsupported-content") => "Only text and structured content are supported.",
            Some("request-failed") => "The app's MCP server request failed.",
            _ => return error.to_string(),
        };
        return message.into();
    }
    error.to_string()
}

pub async fn tool_info(client: &ClientHandle, target: &Target, name: &str) -> Result<ToolInfo, String> {
    let mut payload = target.payload();
    payload["name"] = json!(name);
    let value = client.request("mcpApps.toolInfo", payload).await.map_err(request_error)?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

pub async fn call_tool(client: &ClientHandle, target: &Target, name: &str, arguments: Value) -> Result<Value, String> {
    let info = tool_info(client, target, name).await?;
    require_read_only(&info)?;
    let mut payload = target.payload();
    payload["name"] = json!(name);
    payload["arguments"] = arguments;
    client.request("mcpApps.callTool", payload).await.map_err(request_error)
}

fn require_read_only(info: &ToolInfo) -> Result<(), String> {
    if !info.callable {
        return Err("This app cannot call that tool.".into());
    }
    if !info.read_only {
        return Err("This tool needs confirmation, which is not available here.".into());
    }
    Ok(())
}

pub async fn read_resource(client: &ClientHandle, target: &Target, uri: &str) -> Result<Value, String> {
    let mut payload = target.payload();
    payload["uri"] = json!(uri);
    client.request("mcpApps.readResource", payload).await.map_err(request_error)
}

pub async fn update_model_context(
    client: &ClientHandle,
    target: &Target,
    conversation_thread_id: &str,
    content: Option<Vec<Value>>,
    structured: Option<serde_json::Map<String, Value>>,
) -> Result<Value, String> {
    let mut payload = target.payload();
    payload["conversationThreadId"] = json!(conversation_thread_id);
    if let Some(content) = content {
        payload["content"] = json!(content);
    }
    if let Some(structured) = structured {
        payload["structuredContent"] = json!(structured);
    }
    client.request("mcpApps.updateModelContext", payload).await.map_err(request_error)
}

pub async fn execute(client: &ClientHandle, target: &Target, operation: Operation) -> Result<Value, String> {
    match operation {
        Operation::CallTool { name, arguments } => call_tool(client, target, &name, arguments).await,
        Operation::ReadResource { uri } => read_resource(client, target, &uri).await,
        Operation::OpenLink { .. } => Err("Links must be opened by the client.".into()),
    }
}

pub async fn load_page(client: &ClientHandle, reference: &Reference) -> Result<String, String> {
    let minted = client
        .request(t3_core::media::CREATE_ASSET_URL, json!({"resource":reference.resource()}))
        .await
        .map_err(request_error)?;
    let asset = t3_core::media::decode_asset_url(&minted)?;
    let bytes = client.asset_bytes(&asset.relative_url, MAX_HTML_BYTES).await?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{supervisor, test_server::FakeServer};

    #[tokio::test]
    async fn tools_preflight_on_source_target_and_refuse_without_calling() {
        let mut server = FakeServer::start().await;
        let (client, _events) = supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut socket = server.connection().await;
        let target = Target {
            thread_id: "source-thread".into(),
            item_id: "source-item".into(),
        };
        for (info, allowed) in [
            (json!({"callable":false,"readOnly":true}), false),
            (json!({"callable":true,"readOnly":false}), false),
            (json!({"callable":true,"readOnly":true,"tool":{"future":1},"newField":1}), true),
        ] {
            let (client, target) = (client.clone(), target.clone());
            let job = tokio::spawn(async move { call_tool(&client, &target, "refresh", json!({"city":"Oslo"})).await });
            let request = socket.request("mcpApps.toolInfo").await;
            assert_eq!(request["payload"], json!({"threadId":"source-thread","itemId":"source-item","name":"refresh"}));
            socket
                .send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":info}}))
                .await;
            if allowed {
                let request = socket.request("mcpApps.callTool").await;
                assert_eq!(request["payload"]["arguments"], json!({"city":"Oslo"}));
                assert_eq!(request["payload"]["threadId"], "source-thread");
                socket.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":{"content":[{"type":"future-content","payload":1}],"extra":1}}})).await;
                assert_eq!(job.await.unwrap().unwrap()["content"][0]["type"], "future-content");
            } else {
                assert!(job.await.unwrap().is_err());
            }
        }
        let (reader, source) = (client.clone(), target.clone());
        let job = tokio::spawn(async move { read_resource(&reader, &source, "ui://weather/dashboard").await });
        let request = socket.request("mcpApps.readResource").await;
        assert_eq!(
            request["payload"],
            json!({"threadId":"source-thread","itemId":"source-item","uri":"ui://weather/dashboard"})
        );
        let contents = json!({"contents":[{"uri":"ui://weather/dashboard","text":"page","future":true}],"future":1});
        socket
            .send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":contents}}))
            .await;
        assert_eq!(job.await.unwrap().unwrap(), contents);
        for content in [Some(vec![json!({"type":"text","text":"Sunny"})]), None] {
            let expected = content.clone();
            let (writer, source) = (client.clone(), target.clone());
            let job = tokio::spawn(async move { update_model_context(&writer, &source, "conversation", content, None).await });
            let request = socket.request("mcpApps.updateModelContext").await;
            assert_eq!(request["payload"]["threadId"], "source-thread");
            assert_eq!(request["payload"]["itemId"], "source-item");
            assert_eq!(request["payload"]["conversationThreadId"], "conversation");
            assert_eq!(request["payload"].get("content").is_some(), expected.is_some());
            if let Some(expected) = expected {
                assert_eq!(request["payload"]["content"], json!(expected));
            }
            socket
                .send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":null}}))
                .await;
            assert!(job.await.unwrap().is_ok());
        }
    }

    #[tokio::test]
    async fn cached_app_uses_replacement_connection_and_refuses_other_or_missing_environments() {
        let mut server = FakeServer::start().await;
        let (old, _old_events) = supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut old_socket = server.connection().await;
        let app = AppConnection::new(&old);
        let mut credentials = server.credentials();
        credentials.access_token = "replacement-token".into();
        let (replacement, _replacement_events) = supervisor::start(&tokio::runtime::Handle::current(), credentials);
        let mut replacement_socket = server.connection().await;
        let target = Target {
            thread_id: "source-thread".into(),
            item_id: "source-item".into(),
        };
        let current = app.resolve(Some(replacement.clone())).unwrap();
        let request = tokio::spawn(async move { read_resource(&current, &target, "ui://weather/dashboard").await });
        let (is_replacement, frame) = tokio::select! {
            frame = old_socket.request("mcpApps.readResource") => (false, frame),
            frame = replacement_socket.request("mcpApps.readResource") => (true, frame),
        };
        assert!(is_replacement, "cached app used its original authenticated connection");
        replacement_socket
            .send(json!({"_tag":"Exit","requestId":frame["id"],"exit":{"_tag":"Success","value":{"contents":[]}}}))
            .await;
        assert!(request.await.unwrap().is_ok());
        drop(old);
        old_socket.closed().await;
        assert!(app.resolve(None).is_none());
        let other = FakeServer::start().await;
        let (unrelated, _events) = supervisor::start(&tokio::runtime::Handle::current(), other.credentials());
        assert!(app.resolve(Some(unrelated)).is_none());
        assert!(app.resolve(Some(replacement)).is_some());
    }

    #[test]
    fn unknown_tool_fields_decode_and_missing_permissions_refuse() {
        let info: ToolInfo = serde_json::from_value(json!({"future":true,"tool":{"future":true}})).unwrap();
        assert!(require_read_only(&info).is_err());
        assert_eq!(info.tool.unwrap(), json!({"future":true}));
        let error = |reason| {
            RpcError::Failure(t3_core::rpc::RpcFailure {
                tag: Some("McpAppRequestError".into()),
                message: "McpAppRequestError".into(),
                error: json!({"reason":reason}),
            })
        };
        assert_eq!(request_error(error("tool-not-callable")), "This app cannot call that tool.");
        assert_eq!(request_error(error("future-reason")), "McpAppRequestError");
    }
}
