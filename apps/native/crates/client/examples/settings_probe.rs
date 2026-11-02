//! Headless checks for the settings panels against a live server.
//!
//! Read-only: decode the archive and the keymap the way the panels do.
//!
//!   cargo run -p t3-client --example settings_probe -- --read
//!
//! Round trip: flip the harmless `sidebarAutoSettleOnMerge` through `server.updateSettings`, wait
//! for the config stream to report it, then restore it. This writes, so it needs `--allow-write`;
//! point `T3_NATIVE_HOME` at an isolated dev server's home.
//!
//!   T3_NATIVE_HOME=<dev home>/native cargo run -p t3-client --example settings_probe -- --allow-write
//!
//! Archive cycle: archive a thread, read the archive as the panel does, unarchive it, read again.
//!
//!   T3_NATIVE_HOME=<dev home>/native cargo run -p t3-client --example settings_probe -- --allow-write --archive-cycle <thread-id>

use std::time::Duration;

use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::commands::method;
use t3_core::server_config::{ServerConfigState, ServerConfigStreamEvent};
use t3_core::settings::{ServerSettingChange, archive, decode_update_reply, keybindings};
use tokio::sync::mpsc::Receiver;

async fn next_settings_value(events: &mut Receiver<ClientEvent>, config: &mut ServerConfigState, want: Option<bool>) -> anyhow::Result<bool> {
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("timed out waiting for the config stream"),
            event = events.recv() => {
                let Some(event) = event else { anyhow::bail!("client stopped") };
                let ClientEvent::ServerConfig(event) = event else { continue };
                let kind = match &*event {
                    ServerConfigStreamEvent::Snapshot { .. } => "snapshot",
                    ServerConfigStreamEvent::SettingsUpdated { .. } => "settingsUpdated",
                    _ => continue,
                };
                config.apply(*event);
                let Some(value) = config.config.as_ref().map(|config| config.settings.threads.sidebar_auto_settle_on_merge) else { continue };
                println!("stream  {kind:<16} sidebarAutoSettleOnMerge = {value}");
                if want.is_none_or(|want| want == value) {
                    return Ok(value);
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let read_only = std::env::args().any(|arg| arg == "--read");
    if !read_only && !std::env::args().any(|arg| arg == "--allow-write") {
        anyhow::bail!("pass --read, or --allow-write against an isolated dev server");
    }
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection under T3_NATIVE_HOME"))?;
    println!("server  {}", credentials.http_base);
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    let original = next_settings_value(&mut events, &mut config, None).await?;

    let archive_cycle = std::env::args().skip_while(|arg| arg != "--archive-cycle").nth(1);
    if let Some(thread_id) = archive_cycle.filter(|_| !read_only) {
        for kind in ["thread.archive", "thread.unarchive"] {
            let command = t3_core::commands::simple_thread_command(kind, &uuid::Uuid::new_v4().to_string(), &thread_id);
            println!("send    {kind} {:?}", client.dispatch(command).await.map(|_| "ok"));
            let snapshot = client.request(archive::GET_ARCHIVED_SHELL_SNAPSHOT, serde_json::json!({})).await?;
            for group in archive::groups(&snapshot) {
                for thread in &group.threads {
                    println!("archive {} / {:?} \u{2014} {}", group.title, thread.title, archive::description(thread, chrono_free_now_ms()));
                }
            }
        }
        return Ok(());
    }

    if read_only {
        let snapshot = client.request(archive::GET_ARCHIVED_SHELL_SNAPSHOT, serde_json::json!({})).await?;
        let groups = archive::groups(&snapshot);
        let threads: usize = groups.iter().map(|group| group.threads.len()).sum();
        let raw = |key: &str| snapshot.get(key).and_then(serde_json::Value::as_array).map_or(0, Vec::len);
        println!("archive {} projects, {threads} archived threads (wire: {} projects, {} threads)", groups.len(), raw("projects"), raw("threads"));
        if let Some(thread) = groups.first().and_then(|group| group.threads.first()) {
            println!("        newest: {:?} ({})", thread.title, archive::description(thread, chrono_free_now_ms()));
        }
        if let Some(config) = config.config.as_ref() {
            let rows = keybindings::rows(&config.keybindings, cfg!(target_os = "macos"));
            println!("keymap  {} commands; first: {:?}", rows.len(), rows.first().map(|row| (&row.label, &row.shortcuts)));
            let threads = &config.settings.threads;
            println!("threads autoSettleAfterDays={:?} snoozePresets={} envMode={:?}", threads.sidebar_auto_settle_after_days, threads.snooze_presets.len(), config.settings.default_thread_env_mode);
        }
        return Ok(());
    }

    for value in [!original, original] {
        let change = ServerSettingChange::AutoSettleOnMerge(value);
        println!("send    {} {}", method::SERVER_UPDATE_SETTINGS, change.payload());
        let reply = client.request(method::SERVER_UPDATE_SETTINGS, change.payload()).await?;
        let saved = decode_update_reply(reply).map(|settings| settings.threads.sidebar_auto_settle_on_merge);
        println!("reply   sidebarAutoSettleOnMerge = {saved:?}");
        next_settings_value(&mut events, &mut config, Some(value)).await?;
    }
    println!("restored sidebarAutoSettleOnMerge = {original}");
    Ok(())
}

fn chrono_free_now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}
