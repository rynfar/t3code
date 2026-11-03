//! Onboarding PTY smoke check: open on the configured provider instance and pre-type its login
//! command without Enter. `--keep` leaves it for offscreen captures; `--close <id>` deletes it.
//! Always use an isolated `T3_NATIVE_HOME` and a disposable server.

use std::time::Duration;

use t3_client::{
    store,
    supervisor::{ClientEvent, ConnectionStatus, start},
};
use t3_core::onboarding::{self, terminal as setup};
use t3_core::server_config::{ServerConfigState, ServerConfigStreamEvent};
use t3_core::terminal::{TerminalAccess, method};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    let mut connected = false;
    while !(connected && config.config.is_some()) {
        match tokio::time::timeout(Duration::from_secs(15), events.recv()).await? {
            Some(ClientEvent::Status(ConnectionStatus::Connected)) => connected = true,
            Some(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })) => {
                anyhow::bail!(reason)
            }
            Some(ClientEvent::ServerConfig(event)) => {
                if let ServerConfigStreamEvent::Snapshot { .. } = &*event {
                    config.apply(*event);
                }
            }
            Some(_) => {}
            None => anyhow::bail!("client stopped"),
        }
    }
    let mut grants = client.session_state();
    while grants.borrow().is_none() {
        tokio::time::timeout(Duration::from_secs(10), grants.changed()).await??;
    }
    let operate = TerminalAccess::of(grants.borrow().as_ref()).operate;
    anyhow::ensure!(operate, "terminal:operate required");
    if let Some(id) = flag("--close") {
        anyhow::ensure!(
            id.starts_with("onboarding-"),
            "only onboarding terminals may be closed"
        );
        client
            .request(method::CLOSE, setup::close_input(id, operate).unwrap())
            .await?;
        println!("close: {id}, deleteHistory=true: ok");
        return Ok(());
    }
    let config = config.config.unwrap();
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.driver == "codex")
        .ok_or_else(|| anyhow::anyhow!("no Codex instance"))?;
    let os = config
        .environment
        .platform
        .as_ref()
        .map_or("unknown", |platform| platform.os.as_str());
    let command = onboarding::login_command(provider, &config.settings, os);
    anyhow::ensure!(!command.contains(['\r', '\n']), "command must not execute");
    let id = setup::terminal_id(&provider.driver, &uuid::Uuid::new_v4().to_string());
    let opened = client
        .request(
            method::OPEN,
            setup::open_input(&id, &config.cwd, &provider.instance_id),
        )
        .await?;
    println!(
        "open: thread={} terminal={id} cwd={} providerInstanceId={} status={}",
        setup::THREAD_ID,
        config.cwd,
        provider.instance_id,
        opened["status"]
    );
    let result = client
        .request(method::WRITE, setup::write_input(&id, &command))
        .await;
    println!(
        "pre-type: command={command:?}, newline=false, ok={}",
        result.is_ok()
    );
    if result.is_err() || !args.iter().any(|arg| arg == "--keep") {
        client
            .request(
                method::CLOSE,
                setup::close_input(&id, TerminalAccess::of(grants.borrow().as_ref()).operate)
                    .ok_or_else(|| anyhow::anyhow!("permission revoked; left PTY running"))?,
            )
            .await?;
        println!("close: deleteHistory=true: ok");
    } else {
        println!("kept: {id} (close with --close {id})");
    }
    result?;
    Ok(())
}
