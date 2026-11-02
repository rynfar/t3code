//! Headless checks for Settings → Secrets, Maintenance and Diagnostics against a live server.
//!
//! Read-only: print the raw replies the pages decode, then what the pages make of them.
//!
//!   T3_NATIVE_HOME=<home>/native cargo run -p t3-client --example maintenance_probe -- --raw
//!   T3_NATIVE_HOME=<home>/native cargo run -p t3-client --example maintenance_probe -- --read
//!
//! Round trips, each undone before it ends. They write settings, so they need `--allow-write`;
//! point `T3_NATIVE_HOME` at an isolated dev server's home, never one with real data.
//!
//! `--secrets`: add a 1Password provider the way the dialog saves it, rename it, remove it.
//! `--maintenance`: flip provider update checks, pick a background profile, set log retention,
//! then put each back. With `--run-cleanup` it also runs Delete now while log retention is at
//! its 3650-day maximum, so nothing younger than ten years can go.
//! `--telemetry`: test an unreachable OTLP endpoint, save one, then clear it.
//! `--hold-secret` adds a 1Password provider and leaves it for a screenshot; `--drop-secret`
//! removes it again.

use std::time::Duration;

use serde_json::{Value, json};
use t3_client::store;
use t3_client::supervisor::{ClientHandle, start};
use t3_core::commands::method;
use t3_core::server_config::ServerSettings;
use t3_core::settings::decode_update_reply;
use t3_core::settings::diagnostics::{self, OtlpSignal};
use t3_core::settings::maintenance::{self, MaintenanceChange, Retention};
use t3_core::settings::providers::background::Profile;
use t3_core::settings::secrets::{self, Draft, Target};

fn clip(value: &Value, max: usize) -> String {
    let text = value.to_string();
    if text.len() <= max { text } else { format!("{}… ({} bytes)", &text[..max], text.len()) }
}

async fn settings(client: &ClientHandle) -> anyhow::Result<ServerSettings> {
    let config = client.request("server.getConfig", json!({})).await?;
    Ok(serde_json::from_value(config["settings"].clone())?)
}

/// Sends one `server.updateSettings` payload and decodes the settings it answers with.
async fn save(client: &ClientHandle, payload: Value) -> anyhow::Result<ServerSettings> {
    println!("send    {}", clip(&payload, 300));
    let reply = client.request(method::SERVER_UPDATE_SETTINGS, payload).await?;
    decode_update_reply(reply).ok_or_else(|| anyhow::anyhow!("reply is not settings"))
}

async fn read(client: &ClientHandle) -> anyhow::Result<()> {
    let config = client.request("server.getConfig", json!({})).await?;
    let settings: ServerSettings = serde_json::from_value(config["settings"].clone())?;
    let panel = &settings.provider_panel;
    println!("secrets  capability={} providers={:?}", config["secretProviderSettings"], settings.secret_providers.ids());
    println!("updates  providerUpdateChecks={} continueThreads={}", settings.maintenance.enable_provider_update_checks, settings.maintenance.continue_threads_after_server_update);
    println!("activity option={:?} reset={} \"{}\"", maintenance::profile_option(panel), maintenance::background_customized(panel), maintenance::background_description(panel));
    println!("storage  {:?} hasRules={}", Retention::of(&settings), maintenance::has_cleanup_rules(&settings));
    let mut report = client.stream(maintenance::GET_STORAGE_CLEANUP_REPORT, json!({})).await?;
    if let Ok(Some(Ok(item))) = tokio::time::timeout(Duration::from_secs(5), report.receiver.recv()).await {
        match maintenance::decode_report(&item) {
            Some(report) => println!("lastRun  {} | {}", report.counts(), report.description(now_ms())),
            None => println!("lastRun  none ({item})"),
        }
    }
    if let Ok(reply) = client.request(diagnostics::GET_PROCESS_DIAGNOSTICS, json!({})).await {
        let decoded = diagnostics::decode_process_diagnostics(&reply);
        println!("process  {:?}", decoded.map(|data| (data.server_pid, data.processes.len(), data.error)));
    }
    let window = diagnostics::HISTORY_WINDOWS[diagnostics::DEFAULT_HISTORY_WINDOW];
    if let Ok(reply) = client.request(diagnostics::GET_PROCESS_RESOURCE_HISTORY, window.payload()).await {
        let decoded = diagnostics::decode_resource_history(&reply);
        println!("history  {:?}", decoded.map(|data| (data.buckets.len(), data.top_processes.len(), data.interval_label(), data.error)));
    }
    if let Ok(reply) = client.request(diagnostics::GET_TRACE_DIAGNOSTICS, json!({})).await
        && let Some(data) = diagnostics::decode_trace_diagnostics(&reply)
    {
        println!(
            "trace    spans={} failures={} slow={} latest={} common={} slowest={} logs={} top={} error={:?}",
            data.record_count,
            data.failure_count,
            data.slow_span_count,
            data.latest_failures.len(),
            data.common_failures.len(),
            data.slowest_spans.len(),
            data.latest_warning_and_error_logs.len(),
            data.top_spans_by_count.len(),
            data.error_text()
        );
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

async fn secrets_cycle(client: &ClientHandle) -> anyhow::Result<()> {
    let before = settings(client).await?.secret_providers.ids();
    let draft = Draft { name: "Native Probe".into(), account: "probe.example".into() };
    let issues = secrets::provider_from_draft(&Draft { name: "!!!".into(), account: "has space".into() }, &before, &Target::Add).unwrap_err();
    println!("invalid {issues:?}");
    let (id, config) = secrets::provider_from_draft(&draft, &before, &Target::Add).map_err(|issues| anyhow::anyhow!("{issues:?}"))?;
    let added = save(client, secrets::save_payload(&id, config)).await?;
    println!("added   {:?} -> {:?}", added.secret_providers.ids(), added.secret_providers.get(&id).map(secrets::classify));
    let duplicate = secrets::provider_from_draft(&draft, &added.secret_providers.ids(), &Target::Add).unwrap_err();
    println!("again   {duplicate:?}");
    let (_, renamed) =
        secrets::provider_from_draft(&Draft { name: "Native Probe Renamed".into(), account: "probe.example".into() }, &added.secret_providers.ids(), &Target::Edit(id.clone()))
            .map_err(|issues| anyhow::anyhow!("{issues:?}"))?;
    let edited = save(client, secrets::save_payload(&id, renamed)).await?;
    println!("edited  {:?}", edited.secret_providers.get(&id).map(|provider| provider.label.clone()));
    let removed = save(client, secrets::remove_payload(&id)).await?;
    println!("removed {:?} (before {before:?})", removed.secret_providers.ids());
    anyhow::ensure!(removed.secret_providers.ids() == before, "secret providers were not restored");
    Ok(())
}

async fn maintenance_cycle(client: &ClientHandle, run_cleanup: bool) -> anyhow::Result<()> {
    let original = settings(client).await?;
    let checks = original.maintenance.enable_provider_update_checks;
    let flipped = save(client, MaintenanceChange::ProviderUpdateChecks(!checks).payload()).await?;
    println!("updates {} -> {}", checks, flipped.maintenance.enable_provider_update_checks);
    let profile = save(client, maintenance::background_profile_payload(Profile::BatterySaver)).await?;
    println!("profile {:?}", maintenance::profile_option(&profile.provider_panel));
    let custom = save(client, maintenance::override_payload(&profile.provider_panel, maintenance::GIT_FETCH.change(45))).await?;
    println!("custom  {:?} \"{}\"", maintenance::profile_option(&custom.provider_panel), maintenance::background_description(&custom.provider_panel));
    let logs = save(client, MaintenanceChange::LogsAfterDays(Some(3650)).payload()).await?;
    println!("logs    {:?}", Retention::of(&logs));
    if run_cleanup {
        let reply = client.request(maintenance::RUN_STORAGE_CLEANUP, json!({})).await?;
        let report = maintenance::decode_report(&reply).ok_or_else(|| anyhow::anyhow!("no report: {reply}"))?;
        println!("cleanup {} | {} | failures {}", report.counts(), report.description(now_ms()), report.failures().count());
    }
    // Put everything back as it was.
    save(client, MaintenanceChange::ProviderUpdateChecks(checks).payload()).await?;
    let original_activity = &original.provider_panel.background_activity;
    let mut restore_activity = json!({ "patch": { "backgroundActivity": {
        "schemaVersion": 1,
        "profile": original_activity.profile.clone().unwrap_or_else(|| "balanced".into()),
        "overrides": original_activity.overrides,
    } } });
    if let Some(base) = &original_activity.base_profile {
        restore_activity["patch"]["backgroundActivity"]["baseProfile"] = json!(base);
    }
    save(client, restore_activity).await?;
    let restored = save(client, MaintenanceChange::LogsAfterDays(Retention::of(&original).logs_after_days).payload()).await?;
    println!(
        "restore updates={} activity={:?} storage={:?}",
        restored.maintenance.enable_provider_update_checks,
        maintenance::profile_option(&restored.provider_panel),
        Retention::of(&restored)
    );
    anyhow::ensure!(restored.maintenance == original.maintenance, "updates not restored");
    anyhow::ensure!(restored.provider_panel.background_activity == original.provider_panel.background_activity, "background activity not restored");
    anyhow::ensure!(Retention::of(&restored) == Retention::of(&original), "retention not restored");
    Ok(())
}

async fn telemetry_cycle(client: &ClientHandle) -> anyhow::Result<()> {
    let original = settings(client).await?.observability;
    let check = client.request(diagnostics::CHECK_OTLP_ENDPOINT, OtlpSignal::Traces.check_payload("http://127.0.0.1:9/v1/traces")).await?;
    let result = diagnostics::decode_check(&check);
    let status = diagnostics::endpoint_status("http://127.0.0.1:9/v1/traces", Some(&diagnostics::EndpointCheck { url: "http://127.0.0.1:9/v1/traces".into(), result }));
    println!("check   {check} -> {status:?}");
    let values = vec![
        (OtlpSignal::Traces, "http://127.0.0.1:9/v1/traces".to_owned()),
        (OtlpSignal::Metrics, original.otlp_metrics_url.clone()),
        (OtlpSignal::Logs, original.otlp_logs_url.clone()),
    ];
    let saved = save(client, diagnostics::telemetry_save_payload(&values, &original)).await?;
    println!("saved   {:?}", saved.observability);
    let back = vec![(OtlpSignal::Traces, original.otlp_traces_url.clone())];
    let restored = save(client, diagnostics::telemetry_save_payload(&back, &saved.observability)).await?;
    println!("restore {:?}", restored.observability);
    anyhow::ensure!(restored.observability == original, "telemetry endpoints not restored");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection under T3_NATIVE_HOME"))?;
    println!("server  {}", credentials.http_base);
    let (client, _events) = start(&tokio::runtime::Handle::current(), credentials);
    if has("--raw") {
        let config = client.request("server.getConfig", json!({})).await?;
        println!("observability          {}", clip(&config["observability"], 2000));
        println!("secretProviderSettings {}", config["secretProviderSettings"]);
        for key in ["secretProviders", "storageCleanup", "backgroundActivity", "enableProviderUpdateChecks", "continueThreadsAfterServerUpdate", "observability"] {
            println!("settings.{key:<26} {}", clip(&config["settings"][key], 600));
        }
        for (rpc, payload) in [
            (diagnostics::GET_PROCESS_DIAGNOSTICS, json!({})),
            (diagnostics::GET_PROCESS_RESOURCE_HISTORY, json!({ "windowMs": 900_000, "bucketMs": 60_000 })),
            (diagnostics::GET_TRACE_DIAGNOSTICS, json!({})),
        ] {
            match client.request(rpc, payload).await {
                Ok(reply) => println!("{rpc:<34} {}", clip(&reply, 1500)),
                Err(error) => println!("{rpc:<34} error {error}"),
            }
        }
    }
    if has("--read") {
        read(&client).await?;
    }
    let writes = has("--secrets") || has("--maintenance") || has("--telemetry") || has("--hold-secret") || has("--drop-secret");
    if writes && !has("--allow-write") {
        anyhow::bail!("round trips write settings: pass --allow-write against an isolated dev server");
    }
    if has("--secrets") {
        secrets_cycle(&client).await?;
    }
    if has("--maintenance") {
        maintenance_cycle(&client, has("--run-cleanup")).await?;
    }
    if has("--telemetry") {
        telemetry_cycle(&client).await?;
    }
    if has("--hold-secret") {
        let config = json!({ "kind": secrets::ONE_PASSWORD, "label": "Native Probe", "config": { "account": "probe.example" } });
        println!("held    {:?}", save(&client, secrets::save_payload("native-probe", config)).await?.secret_providers.ids());
    }
    if has("--drop-secret") {
        println!("dropped {:?}", save(&client, secrets::remove_payload("native-probe")).await?.secret_providers.ids());
    }
    Ok(())
}
