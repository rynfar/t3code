//! Headless check for the Usage page and composer meter against a live server.
//!
//! Read-only: decode `server.getUsageSummary` for each period the page offers, merge it as the
//! page does, and pool the config's subscription limits.
//!
//!   T3_NATIVE_HOME=<home>/native cargo run -p t3-client --example usage_probe
//!
//! `--refresh-rates` also calls `server.refreshUsageRates` first, as the page's Refresh does. It
//! makes the server fetch model prices, so point it only at an isolated dev server.
//! `--thread <id>` reads one thread's context window as the composer meter derives it. `--dump <dir>`
//! writes each raw reply there.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::server_config::ServerConfigState;
use t3_core::sidebar::time::FixedZone;
use t3_core::thread::{ThreadState, ThreadStreamItem};
use t3_core::usage::format::{format_tokens, format_usd, make_window, window_periods};
use t3_core::usage::limits::{self, LimitEnvironment};
use t3_core::usage::merge::{AnsweredUsage, merge_usage};
use t3_core::usage::{self, USAGE_CONTRACT_VERSION, UsagePeriod, decode_summary, method};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let refresh_rates = args.iter().any(|arg| arg == "--refresh-rates");
    let thread_id = args.iter().position(|arg| arg == "--thread").and_then(|index| args.get(index + 1)).cloned();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection under T3_NATIVE_HOME"))?;
    println!("server  {}", credentials.http_base);
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    while config.config.is_none() {
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("timed out waiting for the config stream"),
            event = events.recv() => match event {
                Some(ClientEvent::ServerConfig(event)) => { config.apply(*event); }
                Some(_) => {}
                None => anyhow::bail!("client stopped"),
            },
        }
    }
    let Some(config) = config.config.clone() else { anyhow::bail!("no config") };

    if refresh_rates {
        let reply = client.request(method::REFRESH_USAGE_RATES, serde_json::json!({})).await?;
        println!("rates   {reply}");
    }

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
    let zone = FixedZone(0);
    for period in UsagePeriod::ALL {
        let window = make_window(period.days(), now, period.resolution(), "UTC", &zone).with_model_filter(config.settings.usage_model_filter.clone());
        let started = std::time::Instant::now();
        let mut reply = client.request(method::GET_USAGE_SUMMARY, window.payload()).await?;
        let raw_buckets = reply.get("buckets").and_then(serde_json::Value::as_array).map_or(0, Vec::len);
        if let Some(dir) = args.iter().position(|arg| arg == "--dump").and_then(|index| args.get(index + 1)) {
            std::fs::write(format!("{dir}/w12-usage-{}.json", period.days()), reply.to_string())?;
        }
        let mut summary = decode_summary(reply.clone()).map_err(anyhow::Error::msg)?;
        if summary.is_refreshing() {
            reply = client.request(method::GET_USAGE_SUMMARY, window.awaiting_refresh().payload()).await?;
            summary = decode_summary(reply).map_err(anyhow::Error::msg)?;
        }
        let merged = merge_usage(
            &[AnsweredUsage { environment_id: "env".into(), label: "env".into(), summary: Arc::new(summary.clone()) }],
            USAGE_CONTRACT_VERSION,
        );
        let periods = window_periods(&window);
        let series = if period == UsagePeriod::Day { &merged.hourly } else { &merged.daily };
        println!(
            "{:<8} contract={} buckets={}/{raw_buckets} sources={} models={} providers={:?} periods={}/{} cost={} tokens={} sessions={} scan={}ms ({:?})",
            period.label(),
            summary.contract_version,
            summary.buckets.len(),
            summary.sources.len(),
            merged.models.len(),
            usage::providers_with_usage(&merged.providers),
            series.len(),
            periods.len(),
            format_usd(merged.cost_usd),
            format_tokens(merged.total_tokens as f64),
            merged.sessions,
            summary.scan_duration_ms,
            started.elapsed(),
        );
    }

    let environments = [LimitEnvironment {
        environment_id: &config.environment.environment_id,
        label: &config.environment.label,
        providers: &config.providers,
        sources: &config.usage_limit_sources,
    }];
    let pools = limits::collect_limit_pools(limits::collect_limit_accounts(&environments), now);
    println!("limits  {} providers, {} sources, {} pools, notices={:?}", config.providers.len(), config.usage_limit_sources.len(), pools.len(), limits::collect_limit_notices(&environments));
    for pool in &pools {
        let windows: Vec<String> = limits::display_limit_windows(pool)
            .iter()
            .map(|window| format!("{} {}% left pace={:?} accounts={}", window.label, window.remaining_percent, window.pace, window.columns.len()))
            .collect();
        println!("        {}: {}", pool.driver, windows.join("; "));
    }
    let hidden = BTreeSet::new();
    println!("filter  {}", usage::provider_filter_label(&hidden));

    if let Some(thread_id) = thread_id {
        client.open_thread(&thread_id);
        let mut thread = ThreadState::default();
        let deadline = tokio::time::sleep(Duration::from_secs(10));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                event = events.recv() => match event {
                    Some(ClientEvent::Thread { item, .. }) => {
                        let synced = matches!(&*item, ThreadStreamItem::Synchronized);
                        thread.apply(*item);
                        if synced { break; }
                    }
                    Some(_) => {}
                    None => break,
                },
            }
        }
        let snapshot = thread.projection.as_ref().and_then(usage::context_window::context_window_snapshot);
        println!("meter   {:?}", snapshot.map(|snapshot| usage::context_window::meter_label(&snapshot)));
    }
    Ok(())
}
