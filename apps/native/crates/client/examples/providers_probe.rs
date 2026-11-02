//! Headless check for Settings → Providers against a live server.
//!
//! Read-only: decode the provider list as the panel does, then read each instance's sign-in state
//! from `provider.auth.subscribe`.
//!
//!   T3_NATIVE_HOME=<home>/native cargo run -p t3-client --example providers_probe
//!
//! `--allow-write` also runs `server.refreshProviders`, then adds a disabled custom instance
//! through the Add provider payload and removes it again. Point it only at an isolated dev server.

use std::time::Duration;

use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::commands::method;
use t3_core::server_config::ServerConfigState;
use t3_core::settings::providers::{self as model, auth};
use tokio::sync::mpsc::Receiver;

const PROBE_ID: &str = "w15-probe";

async fn wait_for(
    events: &mut Receiver<ClientEvent>,
    config: &mut ServerConfigState,
    done: impl Fn(&ServerConfigState) -> bool,
) -> anyhow::Result<()> {
    let deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);
    while !done(config) {
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("timed out waiting for the config stream"),
            event = events.recv() => match event {
                Some(ClientEvent::ServerConfig(event)) => { config.apply(*event); }
                Some(_) => {}
                None => anyhow::bail!("client stopped"),
            },
        }
    }
    Ok(())
}

fn print_rows(config: &ServerConfigState) {
    let Some(server) = config.config.as_ref() else { return };
    for row in model::instance_rows(&server.settings, &server.providers, None) {
        let live = server.provider(&row.instance_id);
        let view = model::instance_view(&row, live, false, false);
        println!(
            "row     {:<14} {:<12} {:?} {:?} \u{2014} {}{} version={:?} advisory={:?} setup={:?}",
            row.instance_id,
            row.driver,
            view.status,
            view.enabled,
            view.headline,
            view.inline_detail.map(|detail| format!(" \u{b7} {detail}")).unwrap_or_default(),
            view.version,
            view.advisory.map(|advisory| advisory.title),
            live.and_then(|live| live.setup.as_ref()).map(|setup| (setup.can_authenticate, setup.can_install)),
        );
    }
    let panel = &server.settings.provider_panel;
    println!("panel   health={}s (default {}s) idleCompaction={}", panel.health_interval_seconds(), panel.default_health_interval_seconds(), panel.idle_compaction());
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let allow_write = std::env::args().any(|arg| arg == "--allow-write");
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection under T3_NATIVE_HOME"))?;
    println!("server  {}", credentials.http_base);
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    wait_for(&mut events, &mut config, |config| config.config.is_some()).await?;
    print_rows(&config);

    let instance_ids: Vec<String> = config.config.iter().flat_map(|server| server.providers.iter().map(|provider| provider.instance_id.clone())).collect();
    for instance_id in instance_ids {
        let state = async {
            let mut stream = client.stream(auth::AUTH_SUBSCRIBE, auth::subscribe_payload(&instance_id)).await.map_err(|error| format!("{error:?}"))?;
            match stream.receiver.recv().await {
                Some(Ok(item)) => auth::decode_auth_state(item.clone()).ok_or_else(|| format!("undecodable {item}")),
                Some(Err(error)) => Err(format!("{error:?}")),
                None => Err("stream ended".to_owned()),
            }
        };
        match tokio::time::timeout(Duration::from_secs(3), state).await {
            Ok(Ok(state)) => println!("auth    {instance_id:<14} phase={} methods={}", state.phase, state.methods.map_or(0, |methods| methods.len())),
            Ok(Err(error)) => println!("auth    {instance_id:<14} {error}"),
            Err(_) => println!("auth    {instance_id:<14} no item within 3s"),
        }
    }

    if !allow_write {
        return Ok(());
    }

    let reply = client.request(model::SERVER_REFRESH_PROVIDERS, model::refresh_payload()).await?;
    let providers = model::decode_providers_reply(reply).ok_or_else(|| anyhow::anyhow!("refresh reply did not decode"))?;
    println!("refresh {} providers", providers.len());

    let mut instance = model::new_instance("opencode", "W15 probe", "#2563eb");
    instance.enabled = Some(false);
    client.request(method::SERVER_UPDATE_SETTINGS, model::create_payload(PROBE_ID, &instance)).await?;
    wait_for(&mut events, &mut config, |config| config.config.as_ref().is_some_and(|server| server.settings.provider_instances.contains_key(PROBE_ID))).await?;
    println!("create  {PROBE_ID} = {:?}", config.config.as_ref().and_then(|server| server.settings.provider_instances.get(PROBE_ID).cloned()));
    print_rows(&config);

    client.request(method::SERVER_UPDATE_SETTINGS, model::remove_payload(PROBE_ID)).await?;
    wait_for(&mut events, &mut config, |config| config.config.as_ref().is_some_and(|server| !server.settings.provider_instances.contains_key(PROBE_ID))).await?;
    println!("remove  {PROBE_ID} gone");
    Ok(())
}
