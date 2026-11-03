//! Resource monitor smoke transcript. `--retry` restarts the smoke collector, never a process signal.
use std::time::Duration;
use t3_client::{
    resource_telemetry as client, store,
    supervisor::{ClientEvent, start},
};
use t3_core::resource_telemetry as model;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved smoke connection"))?;
    anyhow::ensure!(credentials.http_base == "http://127.0.0.1:14658", "probe is restricted to the smoke server");
    println!("server {}", credentials.http_base);
    let (handle, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if matches!(events.recv().await, Some(ClientEvent::ServerConfig(_))) {
                break;
            }
        }
    })
    .await?;
    let mut stream = client::subscribe(&tokio::runtime::Handle::current(), handle.clone());
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(15), stream.feed.changed()).await??;
        let feed = stream.feed.borrow_and_update().clone();
        let snapshot = feed.snapshot.ok_or_else(|| anyhow::anyhow!("{:?}", feed.error))?;
        println!(
            "subscribeResourceTelemetry native={} processes={} interval={}ms readAt={}",
            snapshot.health.native.status.label(),
            snapshot.processes.len(),
            snapshot.sample_interval_ms,
            snapshot.read_at
        );
    }
    for window in model::HISTORY_WINDOWS {
        let history = client::history(&handle, window).await.map_err(anyhow::Error::msg)?;
        println!(
            "server.getResourceTelemetryHistory {} buckets={} topProcesses={} samples={}",
            window.label,
            history.buckets.len(),
            history.top_processes.len(),
            history.retained_sample_count
        );
    }
    if args.iter().any(|s| s == "--retry") {
        let reply = client::retry(&handle).await.map_err(anyhow::Error::msg)?;
        println!("server.retryResourceTelemetry accepted={} native={}", reply.accepted, reply.snapshot.health.native.status.label());
    }
    if let Some(dir) = args.iter().position(|s| s == "--dump").and_then(|i| args.get(i + 1)) {
        let history = handle.request(model::HISTORY, model::HISTORY_WINDOWS[1].payload()).await?;
        let mut raw = handle.stream(model::SUBSCRIBE, serde_json::json!({})).await?;
        let snapshot = tokio::time::timeout(Duration::from_secs(15), raw.receiver.recv()).await?.ok_or_else(|| anyhow::anyhow!("stream closed"))??;
        std::fs::write(format!("{dir}/live.json"), serde_json::to_vec_pretty(&serde_json::json!({"snapshot":snapshot,"history":history,"error":null}))?)?;
    }
    drop(stream);
    println!("unsubscribeResourceTelemetry (owner dropped; Interrupt verified in client test)");
    Ok(())
}
