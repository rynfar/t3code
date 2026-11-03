//! `client-runtime/src/state/server.ts`'s update handoff: prepare, commit a remote desktop
//! host if necessary, then prove the service trial committed via `subscribeServerLifecycle`.
use crate::supervisor::ClientHandle;
use serde_json::{Value, json};

/// `useUpdateEverything.ts`: finish providers before restarting the servers, even on failure.
pub async fn run(client: &ClientHandle, plan: t3_core::updates::Plan) -> Vec<String> {
    let mut failures = Vec::new();
    for provider in plan.providers {
        if !client
            .session_state()
            .borrow()
            .as_ref()
            .is_some_and(|s| s.grants("providers:manage"))
        {
            failures.push("Provider update failed: no permission".into());
            continue;
        }
        let label = t3_core::providers::driver_label(&provider.driver);
        match client
            .request(
                "server.updateProvider",
                json!({"provider":provider.driver,"instanceId":provider.instance_id}),
            )
            .await
        {
            Ok(response) => {
                if let Some(error) = provider_failure(&response, &provider.instance_id) {
                    failures.push(format!("{label}: {error}"));
                }
            }
            Err(error) => failures.push(format!("{label}: {error}")),
        }
    }
    for server in plan.servers {
        if !client
            .session_state()
            .borrow()
            .as_ref()
            .is_some_and(|s| s.grants("environment:maintain"))
        {
            failures.push(format!("{}: no permission", server.label));
            continue;
        }
        if let Err(error) = update_server(client, &server.target_version).await {
            failures.push(format!("{}: {error}", server.label));
        }
    }
    failures
}

pub async fn update_server(client: &ClientHandle, target: &str) -> Result<(), String> {
    let result = client
        .request("server.updateServer", json!({"targetVersion":target}))
        .await
        .map_err(|e| e.to_string())?;
    if let Some(token) = result["desktopUpdateToken"].as_str() {
        match client
            .request("server.commitDesktopUpdate", json!({"requestId":token}))
            .await
        {
            Ok(_) | Err(crate::session::RpcError::Closed) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let wait = async {
        loop {
            match client.stream("subscribeServerLifecycle", json!({})).await {
                Ok(mut stream) => {
                    while let Some(event) = stream.receiver.recv().await {
                        match event {
                            Ok(event) => {
                                if let Some(outcome) =
                                    t3_core::updates::ready_outcome(&result, &event)
                                {
                                    return outcome;
                                }
                            }
                            Err(crate::session::RpcError::Closed) => break,
                            Err(error) => return Err(error.to_string()),
                        }
                    }
                }
                Err(crate::session::RpcError::Failure(error)) => return Err(error.to_string()),
                Err(_) => {}
            }
            // A disconnected supervisor rejects streams immediately; bound reconnect attempts.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(32 * 60), wait)
        .await
        .map_err(|_| "Timed out waiting for the server to resume after updating.".to_owned())?
}

pub fn provider_failure(response: &Value, instance_id: &str) -> Option<String> {
    let state = response["providers"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["instanceId"] == instance_id))
        .map(|p| &p["updateState"]);
    if state.is_some_and(|s| s["status"] == "succeeded") {
        None
    } else {
        Some(
            state
                .and_then(|s| s["message"].as_str())
                .unwrap_or("Provider update did not finish.")
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_provider_rpc_success_can_contain_a_failed_or_unchanged_update() {
        for status in ["failed", "unchanged", "running", "succeeded"] {
            let response = json!({"providers":[{"instanceId":"codex","updateState":{"status":status,"message":"detail"}}]});
            assert_eq!(
                provider_failure(&response, "codex").is_none(),
                status == "succeeded"
            );
        }
        assert!(provider_failure(&json!({}), "codex").is_some());
    }
}
