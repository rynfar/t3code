//! ACP registry RPCs used by `AcpRegistrySearchStep.tsx`; cancelling the caller interrupts the RPC.
use crate::{session::RpcError, supervisor::ClientHandle};
use serde_json::json;
use t3_core::settings::providers::{
    auth,
    registry::{self, Agent, Prepared, SearchResult},
};

fn failure(error: RpcError) -> String {
    match error {
        RpcError::Failure(error) => auth::failure_message(&error),
        RpcError::Closed => "The connection closed. Try again once it reconnects.".into(),
    }
}
pub async fn search(client: ClientHandle, query: &str) -> Result<SearchResult, String> {
    let result = client
        .request(registry::SEARCH, registry::search_payload(query))
        .await
        .map_err(failure)?;
    serde_json::from_value(result).map_err(|_| "The registry returned an invalid response.".into())
}
pub async fn prepare(client: ClientHandle, agent: &Agent) -> Result<Prepared, String> {
    let result = client
        .request(registry::PREPARE, json!({"agentId":agent.id}))
        .await
        .map_err(failure)?;
    serde_json::from_value(result).map_err(|_| "The ACP could not be prepared.".into())
}

/// AcpRegistryIcon.tsx: unauthenticated, non-redirecting CDN reads, capped before decoding.
pub async fn icon(url: &str) -> Result<(String, Vec<u8>), String> {
    let url = registry::official_icon(url).ok_or("ACP registry icon URL was not allowed.")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "ACP registry icon request failed.")?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "ACP registry icon request failed.")?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    check_icon_response(
        response.status().as_u16(),
        &content_type,
        response.content_length(),
    )?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "ACP registry icon request failed.")?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_ICON_BYTES {
            return Err("ACP registry icon was too large.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((content_type, bytes))
}
const MAX_ICON_BYTES: usize = 512 * 1024;
fn check_icon_response(status: u16, content_type: &str, length: Option<u64>) -> Result<(), String> {
    if !(200..300).contains(&status) {
        return Err("ACP registry icon request failed.".into());
    }
    if !content_type.starts_with("image/") {
        return Err("ACP registry icon was not an image.".into());
    }
    if length.is_some_and(|n| n > MAX_ICON_BYTES as u64) {
        return Err("ACP registry icon was too large.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        supervisor,
        test_server::{Connection, FakeServer},
    };
    #[test]
    fn icon_response_rejects_redirects_non_images_and_unbounded_content() {
        assert!(check_icon_response(302, "image/svg+xml", Some(10)).is_err());
        assert!(check_icon_response(200, "text/html", Some(10)).is_err());
        assert!(check_icon_response(200, "image/png", Some(MAX_ICON_BYTES as u64 + 1)).is_err());
        assert!(check_icon_response(200, "image/svg+xml", None).is_ok());
    }
    async fn client(
        server: &mut FakeServer,
    ) -> (
        ClientHandle,
        Connection,
        tokio::sync::mpsc::Receiver<supervisor::ClientEvent>,
    ) {
        let (client, events) =
            supervisor::start(&tokio::runtime::Handle::current(), server.credentials());
        let mut socket = server.connection().await;
        let config = socket
            .request(t3_core::commands::method::SUBSCRIBE_SERVER_CONFIG)
            .await;
        socket
            .chunk(&config, json!([{"type":"snapshot","config":{}}]))
            .await;
        (client, socket, events)
    }
    async fn reply(socket: &mut Connection, request: &serde_json::Value, value: serde_json::Value) {
        socket.send(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":value}})).await;
    }
    #[tokio::test]
    async fn search_refresh_prepare_and_cancellation_use_existing_rpcs() {
        let mut server = FakeServer::start().await;
        let (client, mut socket, _events) = client(&mut server).await;
        let rpc = tokio::spawn(search(client.clone(), ""));
        let request = socket.request(registry::SEARCH).await;
        assert_eq!(request["payload"], json!({"query":"","limit":512}));
        reply(&mut socket,&request,json!({"agents":[{"id":"fixture","name":"Fixture","version":"1","description":"Fixture","website":null,"repository":null,"icon":null,"distribution":"binary","extra":true}]})).await;
        let mut agents = rpc.await.unwrap().unwrap().agents;
        let agent = agents.remove(0);
        let selected = agent.clone();
        let rpc = tokio::spawn(async move { prepare(client.clone(), &selected).await });
        let request = socket.request(registry::PREPARE).await;
        assert_eq!(request["payload"], json!({"agentId":"fixture"}));
        reply(
            &mut socket,
            &request,
            json!({"agentId":"canonical","version":"2","distribution":"npx","prepared":true}),
        )
        .await;
        let selected = agent.prepared(rpc.await.unwrap().unwrap()).unwrap();
        assert_eq!(selected.id, "canonical");
        // A hidden search drops its request; the server receives an interrupt rather than finishing it.
        let (client, mut socket, _events) = self::client(&mut server).await;
        let rpc = tokio::spawn(search(client.clone(), "  query  "));
        let request = socket.request(registry::SEARCH).await;
        assert_eq!(request["payload"], json!({"query":"query"}));
        rpc.abort();
        assert!(rpc.await.unwrap_err().is_cancelled());
        loop {
            let frame = socket.recv().await;
            if frame["_tag"] == "Interrupt" && frame["requestId"] == request["id"] {
                break;
            }
        }
        let rpc = tokio::spawn(search(client, "query"));
        let request = socket.request(registry::SEARCH).await;
        reply(&mut socket, &request, json!({"agents":[]})).await;
        assert!(rpc.await.unwrap().unwrap().agents.is_empty());
    }
}
