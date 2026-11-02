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

    if let Some(thread_id) = flag("--queue") {
        return queue_probe(&client, &mut events, &thread_id, provider).await;
    }
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

/// `--queue <thread>`: queues two messages behind a fresh turn, edits the first, cancels the
/// second, and prints the queue as `queue_state` reads it after each step.
async fn queue_probe(
    client: &t3_client::supervisor::ClientHandle,
    events: &mut tokio::sync::mpsc::Receiver<ClientEvent>,
    thread_id: &str,
    provider: &t3_core::server_config::ServerProvider,
) -> anyhow::Result<()> {
    use t3_core::composer::queue;
    use t3_core::thread::ThreadState;
    let model = provider.models.first().map(|model| model.slug.clone()).unwrap_or_default();
    let selection = ModelSelection { instance_id: provider.instance_id.clone(), model, options: None };
    let message = |text: &str, dispatch| {
        let (command_id, message_id) = (uuid(), uuid());
        commands::composer_message(&commands::ComposerMessage {
            command_id: &command_id,
            thread_id,
            message_id: &message_id,
            text,
            title_seed: text,
            model_selection: &selection,
            dispatch,
            context: None,
            attachments: &[],
        })
    };
    client.open_thread(thread_id);
    let mut thread = ThreadState::default();
    let steps = vec![
        message("Probe turn", commands::ComposerDispatch::Auto),
        message("Queued one", commands::ComposerDispatch::Queue),
        message("Queued two", commands::ComposerDispatch::Queue),
    ];
    println!("dispatch: {:?}", client.dispatch_in_order(steps).await.map_err(|(index, error)| format!("{index}: {error}")));
    let print_queue = |thread: &ThreadState, label: &str| -> Vec<String> {
        let state = thread.projection.as_ref().map(queue::queue_state).unwrap_or_default();
        let rows: Vec<String> = state.queued.iter().map(|run| format!("{}={:?}", &run.run_id[..8.min(run.run_id.len())], queue::preview_text(run))).collect();
        println!("{label}: can_send={} held={} queued={rows:?}", state.can_send(), state.is_held);
        state.queued.iter().map(|run| run.run_id.clone()).collect()
    };
    drain(events, &mut thread, 1500).await;
    let queued = print_queue(&thread, "after queueing");
    if let Some(first) = queued.first() {
        let edit = queue::edit_queued_run(&uuid(), thread_id, first, "Queued one, edited");
        println!("edit: {:?}", client.dispatch(edit).await.map(|_| "accepted"));
    }
    if let Some(second) = queued.get(1) {
        let cancel = queue::cancel_queued_run(&uuid(), thread_id, second);
        println!("cancel: {:?}", client.dispatch(cancel).await.map(|_| "accepted"));
    }
    drain(events, &mut thread, 1500).await;
    print_queue(&thread, "after edit and cancel");
    Ok(())
}

async fn drain(events: &mut tokio::sync::mpsc::Receiver<ClientEvent>, thread: &mut t3_core::thread::ThreadState, wait: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
        if let ClientEvent::Thread { item, .. } = event {
            thread.apply(*item);
        }
    }
}

