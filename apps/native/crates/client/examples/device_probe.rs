//! Headless Device panel check against a live server: prints the first device state
//! `subscribeDeviceState` sends (hub status, hosts, devices, sessions) and what the panel and
//! setup make of it. `--list` also runs `device.list`, the probe the panel runs when it shows (it
//! never installs or starts helpers). `--stream <host>/<device>` opens a device's screen and
//! input for a few seconds. Uses the saved connection, like `probe`.
//!
//!   cargo run -p t3-client --example device_probe -- [--list] [--stream local/<udid>]

use std::time::Duration;

use serde_json::json;
use t3_client::device_stream::{DeviceStreamEvent, DeviceTarget};
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ConnectionStatus, start};
use t3_core::device::{Platform, ServiceState, method};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    loop {
        match tokio::time::timeout(Duration::from_secs(15), events.recv()).await? {
            Some(ClientEvent::Status(ConnectionStatus::Connected)) => break,
            Some(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })) => anyhow::bail!(reason),
            Some(_) => {}
            None => anyhow::bail!("client stopped"),
        }
    }
    let mut feed = client.stream(method::SUBSCRIBE_STATE, json!({})).await?;
    let first = tokio::time::timeout(Duration::from_secs(10), feed.receiver.recv()).await?.ok_or_else(|| anyhow::anyhow!("the device state stream ended"))??;
    print_state("subscribeDeviceState", &ServiceState::decode(first).map_err(anyhow::Error::msg)?);
    if args.iter().any(|a| a == "--list") {
        let listed = ServiceState::decode(client.request(method::LIST, json!({})).await?).map_err(anyhow::Error::msg)?;
        print_state("device.list", &listed);
    }
    if let Some((host_id, device_id)) = flag("--stream").as_deref().and_then(|value| value.split_once('/')) {
        let target = DeviceTarget { host_id: host_id.to_owned(), device_id: device_id.to_owned() };
        let mut stream = client.device_stream(&tokio::runtime::Handle::current(), target);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut frames = 0u32;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => break,
                changed = stream.frames.changed() => {
                    if changed.is_err() { break }
                    let bytes = stream.frames.borrow_and_update().as_ref().map_or(0, |frame| frame.len());
                    frames += 1;
                    if frames <= 3 { println!("frame: {bytes} bytes") }
                }
                event = stream.events.recv() => match event {
                    Some(event @ DeviceStreamEvent::Status(_)) | Some(event @ DeviceStreamEvent::Screen(_)) | Some(event @ DeviceStreamEvent::Input { .. }) => println!("event: {event:?}"),
                    None => break,
                },
            }
        }
        println!("frames received: {frames}");
    }
    Ok(())
}

fn print_state(source: &str, state: &ServiceState) {
    let readiness = state.hub_readiness();
    println!(
        "{source}: revision={} hub={:?} ready={} busy={} onboarded={} agentAccess={} needsSetup={}",
        state.revision,
        state.host_status,
        readiness.ready,
        readiness.busy,
        state.onboarding_completed,
        state.agent_access_enabled,
        state.needs_setup()
    );
    for host in &state.hosts {
        let status = state.host_statuses.get(&host.id).map(|entry| format!("{:?}", entry.status)).unwrap_or_else(|| "-".to_owned());
        let platforms: Vec<String> = host.platforms.iter().map(|entry| format!("{}={}", entry.platform.as_str(), entry.available)).collect();
        println!("  host {} ({:?}, {}): status={status} platforms=[{}]", host.id, host.kind, host.label, platforms.join(", "));
    }
    for (platform, devices) in state.grouped() {
        println!("  {}: {}", platform.group_label(), devices.len());
        for device in devices.iter().take(5) {
            println!("    {} — {}", device.name, state.device_detail(device));
        }
    }
    println!("  sessions={} booting={}", state.sessions.len(), state.booting_devices.len());
    for platform in [Platform::Ios, Platform::Android] {
        let status = state.platform_setup_status(platform);
        println!("  setup {}: ready={} {}", platform.name(), status.ready, status.message);
    }
}
