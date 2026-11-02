//! Headless checks for the Sharing & access, Scheduled tasks, Source control and Worktrees
//! settings pages against a live server.
//!
//! Read-only: the access stream, the task list, source control discovery and worktree settings.
//!
//!   cargo run -p t3-client --example access_probe -- --read
//!
//! Round trips (writes; point `T3_NATIVE_HOME` at an isolated server's home): create a read-only
//! pairing link and revoke it, create a disabled scheduled task and delete it, and flip
//! `newWorktreesStartFromOrigin` and restore it. Pairing credentials are never printed.
//!
//!   T3_NATIVE_HOME=<dev home>/native cargo run -p t3-client --example access_probe -- --allow-write

use std::time::Duration;

use serde_json::{Value, json};
use t3_client::session::StreamHandle;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, start};
use t3_core::server_config::ServerConfigState;
use t3_core::settings::access::{self, AccessState, READ_ONLY_SCOPES};
use t3_core::settings::scheduled_tasks::{self, Draft, method};
use t3_core::settings::source_control::{DISCOVER, decode_discovery};
use t3_core::settings::worktrees::WorktreeChange;
use t3_core::shell::ShellStreamItem;

async fn next(stream: &mut StreamHandle) -> anyhow::Result<Value> {
    match tokio::time::timeout(Duration::from_secs(10), stream.receiver.recv()).await {
        Ok(Some(Ok(value))) => Ok(value),
        Ok(Some(Err(error))) => anyhow::bail!("stream failed: {error}"),
        Ok(None) => anyhow::bail!("stream ended"),
        Err(_) => anyhow::bail!("timed out waiting for the stream"),
    }
}

/// The access stream until `done` holds, printing each event type.
async fn access_until(stream: &mut StreamHandle, state: &mut AccessState, done: impl Fn(&AccessState) -> bool) -> anyhow::Result<()> {
    while !done(state) {
        let item = next(stream).await?;
        println!("access  {} (revision {})", item["type"].as_str().unwrap_or("?"), item["revision"]);
        state.apply(&item);
    }
    Ok(())
}

async fn access(client: &ClientHandle, write: bool) -> anyhow::Result<()> {
    let mut stream = client.stream(access::SUBSCRIBE, json!({})).await?;
    let mut state = AccessState::default();
    access_until(&mut stream, &mut state, |state| state.loaded).await?;
    let now = now_ms();
    println!("access  {}; this device: {:?}", state.summary(now), state.sorted_clients().first().filter(|client| client.current).map(|client| client.title()));
    if !write {
        return Ok(());
    }
    let scopes: Vec<String> = READ_ONLY_SCOPES.iter().map(|scope| (*scope).to_owned()).collect();
    let created = client.post_access("/api/auth/pairing-token", Some(access::pairing_token_body("w16 probe", &scopes))).await.map_err(anyhow::Error::msg)?;
    let id = created["id"].as_str().unwrap_or_default().to_owned();
    let url = access::pairing_url(client.environment(), created["credential"].as_str().unwrap_or_default());
    println!("create  pairing link {id}: {}; url {}", access::expires_label(created["expiresAt"].as_str().unwrap_or_default(), now_ms()), url.map_or("none (loopback server)".to_owned(), |_| "<server>/pair#token=<redacted>".to_owned()));
    access_until(&mut stream, &mut state, |state| state.pairing_links.iter().any(|link| link.id == id)).await?;
    let revoked = client.post_access("/api/auth/pairing-links/revoke", Some(json!({ "id": id }))).await.map_err(anyhow::Error::msg)?;
    println!("revoke  pairing link {id}: {revoked}");
    access_until(&mut stream, &mut state, |state| !state.pairing_links.iter().any(|link| link.id == id)).await?;
    println!("access  {}", state.summary(now_ms()));
    Ok(())
}

async fn tasks(client: &ClientHandle, project_id: Option<String>, config: &ServerConfigState, write: bool) -> anyhow::Result<()> {
    let mut stream = client.stream(method::SUBSCRIBE, json!({})).await?;
    let list = scheduled_tasks::decode_list(&next(&mut stream).await?).unwrap_or_default();
    println!("tasks   {} scheduled tasks", list.len());
    for task in list.iter().take(3) {
        println!("        {:?}: {} \u{00b7} {}", task.title, task.schedule.label(|ms| ms.to_string()), task.state_label(now_ms()));
    }
    let model = config.config.as_ref().and_then(|config| {
        config.settings.default_model_selection.clone().or_else(|| t3_core::providers::project_default_selection(&t3_core::providers::entries(config, None)))
    });
    let (Some(project_id), Some(model), true) = (project_id, model, write) else { return Ok(()) };
    let draft = Draft { title: "w16 probe".into(), prompt: "Probe task; never runs.".into(), enabled: false, project_id, model: Some(model), ..Draft::default() };
    let input = draft.upsert_input(false, |_| None).map_err(|invalid| anyhow::anyhow!(invalid.description))?;
    let reply = client.request(method::UPSERT, input).await?;
    let id = reply["task"]["id"].as_str().unwrap_or_default().to_owned();
    let saved = scheduled_tasks::decode_list(&json!({ "tasks": [reply["task"].clone()] })).unwrap_or_default();
    println!("upsert  {id}: {}", saved.first().map(|task| format!("{} \u{00b7} {}", task.schedule.label(|ms| ms.to_string()), task.state_label(now_ms()))).unwrap_or_default());
    let listed = scheduled_tasks::decode_list(&next(&mut stream).await?).unwrap_or_default();
    println!("stream  {} tasks after upsert", listed.len());
    println!("delete  {id}: {}", client.request(method::DELETE, scheduled_tasks::id_payload(&id)).await?);
    let listed = scheduled_tasks::decode_list(&next(&mut stream).await?).unwrap_or_default();
    println!("stream  {} tasks after delete", listed.len());
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let read_only = std::env::args().any(|arg| arg == "--read");
    if !read_only && !std::env::args().any(|arg| arg == "--allow-write") {
        anyhow::bail!("pass --read, or --allow-write against an isolated dev server");
    }
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection under T3_NATIVE_HOME"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut config = ServerConfigState::default();
    let mut project_id = None;
    while config.config.is_none() || project_id.is_none() {
        match tokio::time::timeout(Duration::from_secs(10), events.recv()).await? {
            Some(ClientEvent::ServerConfig(event)) => {
                config.apply(*event);
            }
            Some(ClientEvent::Shell(ShellStreamItem::Snapshot { snapshot, .. })) => {
                project_id = Some(snapshot.projects.iter().find(|project| project.archived_at.is_none()).map(|project| project.id.clone()));
            }
            Some(_) => {}
            None => anyhow::bail!("client stopped"),
        }
    }
    let settings = &config.config.as_ref().expect("config").settings;
    println!("server  {} ({})", client.environment(), config.config.as_ref().map_or("", |config| config.environment.server_version.as_str()));
    println!("wt      {:?} startFromOrigin={:?} cleanup={:?}", settings.worktrees.branch_naming_mode, settings.new_worktrees_start_from_origin, settings.worktrees.cleanup());

    let discovery = decode_discovery(&client.request(DISCOVER, json!({})).await?);
    for item in discovery.version_control_systems.iter().chain(&discovery.source_control_providers) {
        println!("scm     {} {:?}: {}", item.label, item.version, item.summary());
    }

    access(&client, !read_only).await?;
    tasks(&client, project_id.flatten(), &config, !read_only).await?;

    if !read_only {
        let original = settings.new_worktrees_start_from_origin.unwrap_or(true);
        for value in [!original, original] {
            let payload = WorktreeChange::StartFromOrigin(value).into_server().payload();
            let reply = client.request(t3_core::commands::method::SERVER_UPDATE_SETTINGS, payload.clone()).await?;
            println!("send    {payload} -> newWorktreesStartFromOrigin = {}", reply["newWorktreesStartFromOrigin"]);
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}
