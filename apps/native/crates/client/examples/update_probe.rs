//! Read-only update-state/lifecycle transcript; never installs or restarts anything.
use serde_json::json;
use t3_client::{
    store,
    supervisor::{self, ClientEvent},
};
use t3_core::server_config::ServerConfigState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var_os("T3_NATIVE_HOME").is_some(),
        "set an isolated T3_NATIVE_HOME"
    );
    let saved = store::load_environments();
    let credentials = saved
        .active()
        .and_then(|e| e.credentials())
        .ok_or_else(|| anyhow::anyhow!("no paired environment"))?;
    let (client, mut events) = supervisor::start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    let read = async {
        while let Some(event) = events.recv().await {
            if let ClientEvent::ServerConfig(event) = event {
                config.apply(*event);
                if let Some(config) = &config.config {
                    println!(
                        "subscribeServerConfig: version={} providers={} selfUpdate={:?} policy={}",
                        config.environment.server_version,
                        config.providers.len(),
                        config.environment.capabilities.server_self_update,
                        config.managed_policy.is_some()
                    );
                    for provider in &config.providers {
                        println!(
                            "provider={} versionAdvisory={} updateState={} finishedAt={:?}",
                            provider.driver,
                            provider
                                .version_advisory
                                .as_ref()
                                .map(|a| a.status.as_str())
                                .unwrap_or("none"),
                            provider
                                .update_state
                                .as_ref()
                                .map(|a| a.status.as_str())
                                .unwrap_or("none"),
                            provider
                                .update_state
                                .as_ref()
                                .and_then(|s| s.finished_at.as_deref())
                        );
                    }
                    let mut lifecycle =
                        client.stream("subscribeServerLifecycle", json!({})).await?;
                    while let Some(event) = lifecycle.receiver.recv().await {
                        let event = event?;
                        println!(
                            "subscribeServerLifecycle: type={} version={}",
                            event["type"], event["payload"]["environment"]["serverVersion"]
                        );
                        if event["type"] == "ready" {
                            break;
                        }
                    }
                    println!("Mutating requests sent: 0");
                    break;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), read).await??;
    Ok(())
}
