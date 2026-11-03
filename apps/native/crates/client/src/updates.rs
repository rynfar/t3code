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
        if let Err(error) = update_server(client, &server.target_version, &server.self_update).await
        {
            failures.push(format!("{}: {error}", server.label));
        }
    }
    failures
}

trait UpdateTransport {
    fn generation(&self) -> u64;
    fn request(
        &self,
        method: &'static str,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Value, crate::session::RpcError>> + Send;
    fn reconnected(
        &self,
        after: u64,
    ) -> impl std::future::Future<Output = Result<(), crate::session::RpcError>> + Send;
    fn next_ready(
        &self,
        result: &Value,
    ) -> impl std::future::Future<Output = Result<Value, crate::session::RpcError>> + Send;
}

fn handoff_loss(error: &crate::session::RpcError) -> bool {
    match error {
        crate::session::RpcError::Closed => true,
        crate::session::RpcError::Failure(error) => {
            error.tag.is_none() && error.message == "Interrupted" && error.error.is_null()
        }
    }
}

async fn update_with(
    transport: &impl UpdateTransport,
    target: &str,
    capability: &str,
) -> Result<(), String> {
    let generation = transport.generation();
    let result = match transport
        .request("server.updateServer", json!({"targetVersion":target}))
        .await
    {
        Ok(result) => result,
        Err(error) if matches!(capability, "boot-service" | "respawn") && handoff_loss(&error) => {
            transport
                .reconnected(generation)
                .await
                .map_err(|e| e.to_string())?;
            json!({"targetVersion":target,"method":capability})
        }
        Err(error) => return Err(error.to_string()),
    };
    if result["method"] == "desktop-app"
        && let Some(token) = result["desktopUpdateToken"].as_str()
    {
        let generation = transport.generation();
        match transport
            .request("server.commitDesktopUpdate", json!({"requestId":token}))
            .await
        {
            Ok(_) => {}
            Err(error) if handoff_loss(&error) => transport
                .reconnected(generation)
                .await
                .map_err(|e| e.to_string())?,
            Err(error) => return Err(error.to_string()),
        }
        // Match waitForDesktopUpdateTarget: an old-version reconnect retries the same token,
        // with at most three readiness checks, and never consumes the pre-commit connection.
        for attempt in 1..=3 {
            let ready = transport
                .next_ready(&result)
                .await
                .map_err(|e| e.to_string())?;
            if ready["payload"]["environment"]["serverVersion"] == result["targetVersion"] {
                return t3_core::updates::ready_outcome(&result, &ready).unwrap_or_else(|| {
                    Err("The desktop app resumed without installing the prepared update.".into())
                });
            }
            if attempt == 3 {
                break;
            }
            let generation = transport.generation();
            match transport
                .request("server.commitDesktopUpdate", json!({"requestId":token}))
                .await
            {
                Ok(_) => break,
                Err(error) if handoff_loss(&error) => transport
                    .reconnected(generation)
                    .await
                    .map_err(|e| e.to_string())?,
                Err(error) => return Err(error.to_string()),
            }
        }
        return Err("The desktop app resumed without installing the prepared update.".into());
    }
    loop {
        let ready = transport
            .next_ready(&result)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(outcome) = t3_core::updates::ready_outcome(&result, &ready) {
            return outcome;
        }
    }
}

impl UpdateTransport for ClientHandle {
    fn generation(&self) -> u64 {
        *self.connection_generation().borrow()
    }
    async fn request(
        &self,
        method: &'static str,
        input: Value,
    ) -> Result<Value, crate::session::RpcError> {
        self.request(method, input).await
    }
    async fn reconnected(&self, after: u64) -> Result<(), crate::session::RpcError> {
        self.connection_generation()
            .wait_for(|generation| *generation > after)
            .await
            .map(|_| ())
            .map_err(|_| crate::session::RpcError::Closed)
    }
    async fn next_ready(&self, result: &Value) -> Result<Value, crate::session::RpcError> {
        loop {
            let generation = self.generation();
            match self.stream("subscribeServerLifecycle", json!({})).await {
                Ok(mut stream) => {
                    while let Some(event) = stream.receiver.recv().await {
                        match event {
                            Ok(event)
                                if event["type"] == "ready"
                                    && ((result["method"] == "desktop-app"
                                        && result["desktopUpdateToken"].is_string())
                                        || t3_core::updates::ready_outcome(result, &event)
                                            .is_some()) =>
                            {
                                return Ok(event);
                            }
                            Ok(_) => {}
                            Err(error) if handoff_loss(&error) => break,
                            Err(error) => return Err(error),
                        }
                    }
                }
                Err(error) if handoff_loss(&error) => {}
                Err(error) => return Err(error),
            }
            self.reconnected(generation).await?;
        }
    }
}

pub async fn update_server(
    client: &ClientHandle,
    target: &str,
    capability: &str,
) -> Result<(), String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(32 * 60),
        update_with(client, target, capability),
    )
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

#[cfg(test)]
mod handoff_tests;
