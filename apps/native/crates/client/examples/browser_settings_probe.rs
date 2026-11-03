//! `BrowserSettingsPanel`'s agent access round trip on the isolated smoke server, then restore.
//! T3_NATIVE_HOME=<worktree>/.t3/native cargo run -p t3-client --example browser_settings_probe -- --allow-write

use std::time::Duration;

use t3_client::supervisor::{ClientEvent, start};
use t3_core::commands::method;
use t3_core::server_config::{ServerConfigState, ServerConfigStreamEvent};
use t3_core::settings::{ServerSettingChange, decode_update_reply};
use tokio::sync::mpsc::Receiver;

async fn next(events: &mut Receiver<ClientEvent>, config: &mut ServerConfigState, want: Option<bool>) -> anyhow::Result<bool> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.recv().await {
            let ClientEvent::ServerConfig(event) = event else { continue };
            if !matches!(&*event, ServerConfigStreamEvent::Snapshot { .. } | ServerConfigStreamEvent::SettingsUpdated { .. }) { continue; }
            config.apply(*event);
            let Some(value) = config.config.as_ref().map(|config| config.settings.enable_agent_browser_access.unwrap_or(true)) else { continue };
            println!("stream enableAgentBrowserAccess={value}");
            if want.is_none_or(|want| want == value) { return Ok(value); }
        }
        anyhow::bail!("client stopped")
    }).await?
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let credentials = t3_client::store::load().ok_or_else(|| anyhow::anyhow!("no isolated saved connection"))?;
    anyhow::ensure!(std::env::args().any(|arg| arg == "--allow-write") && credentials.http_base == "http://127.0.0.1:14658", "this probe writes only to the smoke server, with --allow-write");
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    let original = next(&mut events, &mut config, None).await?;
    let result = async {
        let payload = ServerSettingChange::AgentBrowserAccess(!original).payload();
        println!("send {} {payload}", method::SERVER_UPDATE_SETTINGS);
        let reply = client.request(method::SERVER_UPDATE_SETTINGS, payload).await?;
        let saved = decode_update_reply(reply).ok_or_else(|| anyhow::anyhow!("invalid settings reply"))?;
        anyhow::ensure!(saved.enable_agent_browser_access == Some(!original), "save did not match");
        println!("reply enableAgentBrowserAccess={}", !original);
        next(&mut events, &mut config, Some(!original)).await?;
        anyhow::Ok(())
    }.await;
    let reply = client.request(method::SERVER_UPDATE_SETTINGS, ServerSettingChange::AgentBrowserAccess(original).payload()).await?;
    let saved = decode_update_reply(reply).ok_or_else(|| anyhow::anyhow!("invalid restore reply"))?;
    anyhow::ensure!(saved.enable_agent_browser_access == Some(original), "restore did not match");
    next(&mut events, &mut config, Some(original)).await?;
    println!("restored enableAgentBrowserAccess={original}");
    result
}
