//! Headless check of the composer menus against a live server: runs the `@` path search and the
//! `/` and `$` lists for a project, then (with `--send`) launches a thread whose first message
//! mentions a file and an existing thread, and prints the user message the server recorded.
//!
//!   cargo run -p t3-client --example composer_probe -- [--query <text>] [--project <id>]
//!       [--provider <instance>] [--send [--image <png>]]

use std::time::Duration;

use serde_json::{Value, json};
use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::commands::{self, LaunchThread, method};
use t3_core::composer::attachments;
use t3_core::composer::menu::{self, SlashMenuOptions, ThreadContext, TriggerKind};
use t3_core::model::ModelSelection;
use t3_core::server_config::{InteractionMode, RuntimeMode, ServerConfigState};
use t3_core::shell::ShellState;
use t3_core::workspace::Strategy;

fn trigger_for(text: &str) -> menu::Trigger {
    menu::detect_trigger(text, text.len()).expect("a trigger")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let query = flag("--query").unwrap_or_else(|| "main".to_owned());
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let (mut shell, mut config) = (ShellState::default(), ServerConfigState::default());
    while !shell.synchronized || config.config.is_none() {
        match tokio::time::timeout(Duration::from_secs(15), events.recv()).await? {
            Some(ClientEvent::Shell(item)) => {
                shell.apply(item);
            }
            Some(ClientEvent::ServerConfig(event)) => {
                config.apply(*event);
            }
            Some(_) => {}
            None => anyhow::bail!("connection closed"),
        }
    }
    let config = config.config.expect("config");
    let snapshot = shell.snapshot.as_ref().expect("shell");
    let project = match flag("--project") {
        Some(id) => snapshot.projects.iter().find(|p| p.id == id),
        None => snapshot.projects.iter().find(|p| p.archived_at.is_none()),
    }
    .ok_or_else(|| anyhow::anyhow!("no project"))?;
    println!("project {} at {}", project.title, project.workspace_root);

    let trigger = trigger_for(&format!("look at @{query}"));
    let reply = client.request(menu::SEARCH_ENTRIES, menu::search_entries_request(&project.workspace_root, &trigger.query)).await?;
    let entries = menu::decode_entries(&reply);
    println!("@{query}: {} entries (truncated: {})", entries.len(), reply["truncated"]);
    for item in menu::path_items(&entries).iter().take(8) {
        println!("  {:<28} {}", item.label, item.description);
    }

    let wanted = flag("--provider");
    let provider = config
        .providers
        .iter()
        .find(|p| p.enabled && wanted.as_ref().is_none_or(|id| p.instance_id == *id))
        .ok_or_else(|| anyhow::anyhow!("no provider"))?;
    let refreshed = client.request(menu::REFRESH_PROVIDERS, menu::refresh_providers_request(&provider.instance_id, &project.workspace_root)).await;
    let fetched = refreshed.ok().and_then(|reply| menu::refreshed_snapshot(&reply, &provider.instance_id, &project.workspace_root));
    let snapshot_for_cwd = menu::workspace_snapshot(provider, fetched.as_ref(), Some(&project.workspace_root));
    let (skills, slash_commands) = menu::skills_and_commands(provider, snapshot_for_cwd);
    println!("{} ({}): {} skills, {} commands, snapshot {}", provider.instance_id, provider.driver, skills.len(), slash_commands.len(), snapshot_for_cwd.is_some());
    let options = SlashMenuOptions { plan_mode: true, show_skills: true, ..Default::default() };
    let slash = menu::slash_items(&trigger_for("/"), &provider.driver, &skills, &slash_commands, options);
    println!("/: {}", slash.iter().map(|item| item.label.as_str()).collect::<Vec<_>>().join("  "));
    let dollar = menu::skill_items(&trigger_for("$"), &provider.driver, &skills);
    println!("$: {}", dollar.iter().map(|item| item.label.as_str()).collect::<Vec<_>>().join("  "));
    assert_eq!(trigger.kind, TriggerKind::Path);

    if !args.iter().any(|a| a == "--send") {
        return Ok(());
    }
    let entry = entries.first().ok_or_else(|| anyhow::anyhow!("no entry to mention"))?;
    let other = snapshot.threads.iter().find(|thread| thread.project_id == project.id);
    let thread_context = other.map(|thread| ThreadContext::new(&config.environment.environment_id, &thread.id, &thread.title));
    let mut text = format!("Please read {}", menu::file_mention_text(&entry.path));
    if let Some(context) = &thread_context {
        text.push_str(&format!("and compare with {} ", context.reference()));
    }
    let text = text.trim_end().to_owned();
    // `--image <png>` uploads it and sends it with the message.
    let mut staged = Vec::new();
    if let Some(path) = flag("--image") {
        let bytes = std::fs::read(&path)?;
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let attachments::Classified::Image { mime_type } = attachments::classify(&name) else { anyhow::bail!("not an image") };
        let mut attachment = attachments::Attachment {
            id: uuid(),
            kind: attachments::AttachmentKind::Image,
            name,
            mime_type: mime_type.to_owned(),
            size_bytes: bytes.len() as u64,
            pasted_text: false,
            upload: attachments::Upload::Uploading,
        };
        let attachment_id = t3_client::upload::upload_attachment(&client, attachment.upload_request(), mime_type, bytes)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        println!("uploaded {} as {attachment_id}", attachment.name);
        attachment.upload = attachments::Upload::Ready { attachment_id };
        staged.push(attachment);
    }
    let context = attachments::message_context(&text, thread_context.as_slice(), &staged);
    let chat_attachments = attachments::chat_attachments(&staged).unwrap_or_default();
    let model = provider.models.first().map(|model| model.slug.clone()).unwrap_or_default();
    let selection = ModelSelection { instance_id: provider.instance_id.clone(), model, options: None };
    let (thread_id, command_id, message_id) = (uuid(), uuid(), uuid());
    let payload = commands::launch_thread(&LaunchThread {
        command_id: &command_id,
        thread_id: &thread_id,
        project_id: &project.id,
        title: "Composer probe",
        model_selection: &selection,
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        workspace: &Strategy::Root,
        message_id: &message_id,
        text: &text,
        context: context.as_ref(),
        attachments: &chat_attachments,
    });
    client.request(method::LAUNCH_THREAD, payload).await?;
    println!("launched {thread_id}");
    client.open_thread(&thread_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(5), events.recv()).await else { break };
        if let ClientEvent::Thread { thread_id: id, item } = event
            && id == thread_id
        {
            let value = serde_json::to_value(format!("{item:?}")).unwrap_or(Value::Null);
            let text = value.as_str().unwrap_or_default();
            if let Some(start) = text.find("UserMessage") {
                println!("recorded: {}", &text[start..text.len().min(start + 2400)]);
                return Ok(());
            }
        }
    }
    println!("no user message seen; {}", json!({ "threadId": thread_id }));
    Ok(())
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
