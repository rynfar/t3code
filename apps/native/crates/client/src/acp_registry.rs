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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        supervisor,
        test_server::{Connection, FakeServer},
    };
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
