//! Headless check against a live server: pairs (or reuses saved credentials), subscribes the
//! shell and server config, optionally one thread, and prints what arrives.
//!
//!   cargo run -p t3-client --example probe -- --pair 'http://127.0.0.1:3773/pair#token=…'
//!   cargo run -p t3-client --example probe -- --thread <thread-id> --seconds 10
//!   cargo run -p t3-client --example probe -- --thread <thread-id> --send "hello"   # costs a turn
//!   cargo run -p t3-client --example probe -- --thread <thread-id> --history 3     # page earlier history
//!   cargo run -p t3-client --example probe -- --config
//!
//! Launch a thread and drive it. The provider and model are required so a probe can never fall
//! back to a paid default; use the free `devReplay` / `showcase` pair on a dev server:
//!
//!   cargo run -p t3-client --example probe -- --launch <project-id> "go" \
//!     --provider devReplay --model showcase --runtime-mode approval-required \
//!     --approve accept --answer --seconds 40
//!
//! Send as the composer does to a thread: permissions first, then the turn with its model:
//!
//!   cargo run -p t3-client --example probe -- --thread <thread-id> --compose "go on" \
//!     --provider devReplay --model showcase --runtime-mode auto-accept-edits

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use t3_client::supervisor::{ClientEvent, ClientHandle, start};
use t3_client::{auth, store};
use t3_core::commands::{self, ApprovalDecision, LaunchThread, method};
use t3_core::composer::title_seed;
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::requests::{self, DraftAnswer};
use t3_core::server_config::{InteractionMode, RuntimeMode, ServerConfigState};
use t3_core::shell::{ShellState, ShellStreamItem};
use t3_core::thread::{ThreadState, ThreadStreamItem};

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn spawn_dispatch(client: &ClientHandle, label: String, command: serde_json::Value) {
    let client = client.clone();
    tokio::spawn(async move { println!("{label}: {:?}", client.dispatch(command).await.map(|v| v.to_string())) });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let has = |name: &str| args.iter().any(|a| a == name);
    let credentials = match flag("--pair") {
        Some(link) => {
            let link = t3_core::pairing::parse_pairing_link(&link, None)?;
            let credentials = auth::redeem_pairing(&link).await?;
            store::save(&credentials)?;
            println!("paired with {}", credentials.http_base);
            credentials
        }
        None => store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pass --pair <link>"))?,
    };
    let seconds: u64 = flag("--seconds").and_then(|s| s.parse().ok()).unwrap_or(5);
    let approve = flag("--approve")
        .map(|value| {
            ApprovalDecision::parse(&value)
                .ok_or_else(|| anyhow::anyhow!("--approve takes accept, acceptForSession, decline or cancel"))
        })
        .transpose()?;
    let answer = has("--answer");
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut watched = flag("--thread");
    if let Some(thread_id) = &watched {
        client.open_thread(thread_id);
        if let Some(text) = flag("--compose") {
            let (Some(provider), Some(model)) = (flag("--provider"), flag("--model")) else {
                anyhow::bail!("--compose needs --provider and --model (use devReplay / showcase on a dev server)");
            };
            let selection = ModelSelection { instance_id: provider, model, options: None };
            if let Some(mode) = flag("--runtime-mode").as_deref().and_then(RuntimeMode::parse) {
                println!("runtime-mode.set: {:?}", client.dispatch(commands::set_runtime_mode(&new_id(), thread_id, mode)).await);
            }
            let (command_id, message_id, title) = (new_id(), new_id(), title_seed(&text));
            let message = commands::composer_message(&commands::ComposerMessage {
                command_id: &command_id,
                thread_id,
                message_id: &message_id,
                text: &text,
                title_seed: &title,
                model_selection: &selection,
                dispatch: commands::ComposerDispatch::Auto,
            });
            println!("message.dispatch: {:?}", client.dispatch(message).await);
        }
        if let Some(text) = flag("--send") {
            let command = commands::send_message(&new_id(), thread_id, &text, &commands::DispatchMode::QueueAfterActive);
            spawn_dispatch(&client, "dispatch".into(), command);
        }
    }
    if let Some(project_id) = flag("--launch") {
        let text = args
            .iter()
            .position(|a| a == "--launch")
            .and_then(|i| args.get(i + 2))
            .filter(|text| !text.starts_with("--"))
            .cloned()
            .unwrap_or_else(|| "go".into());
        let (Some(provider), Some(model)) = (flag("--provider"), flag("--model")) else {
            anyhow::bail!("--launch needs --provider and --model (use devReplay / showcase on a dev server)");
        };
        let runtime_mode = match flag("--runtime-mode") {
            Some(mode) => RuntimeMode::parse(&mode).ok_or_else(|| anyhow::anyhow!("unknown runtime mode {mode}"))?,
            None => RuntimeMode::ApprovalRequired,
        };
        let thread_id = new_id();
        let selection = ModelSelection { instance_id: provider, model, options: None };
        let (command_id, message_id, title) = (new_id(), new_id(), title_seed(&text));
        let payload = commands::launch_thread(&LaunchThread {
            command_id: &command_id,
            thread_id: &thread_id,
            project_id: &project_id,
            title: &title,
            model_selection: &selection,
            runtime_mode,
            interaction_mode: InteractionMode::Default,
            message_id: &message_id,
            text: &text,
        });
        match client.request(method::LAUNCH_THREAD, payload).await {
            Ok(value) => {
                println!(
                    "launch: threadId={} resumed={} sequence={}",
                    value["threadId"],
                    value["resumed"],
                    value.get("sequence").map(|s| s.to_string()).unwrap_or_default()
                );
                client.open_thread(&thread_id);
                watched = Some(thread_id);
            }
            Err(error) => println!("launch failed: {error}"),
        }
    }
    let mut shell = ShellState::default();
    let mut thread = ThreadState::default();
    let mut config = ServerConfigState::default();
    let mut seen: HashSet<String> = HashSet::new();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            event = events.recv() => match event {
                None => break,
                Some(ClientEvent::Status(status)) => println!("status: {status:?}"),
                Some(ClientEvent::ServerConfig(event)) => {
                    let label: String = format!("{event:?}").chars().take(48).collect();
                    if config.apply(*event) {
                        println!("server config: {label}…");
                    }
                }
                Some(ClientEvent::Shell(item)) => {
                    let label = match &item {
                        ShellStreamItem::Snapshot { snapshot, .. } => format!(
                            "snapshot seq={} projects={} threads={}",
                            snapshot.snapshot_sequence, snapshot.projects.len(), snapshot.threads.len()
                        ),
                        other => format!("{other:?}").chars().take(120).collect(),
                    };
                    shell.apply(item);
                    if !has("--quiet-shell") {
                        println!("shell: {label}");
                    }
                }
                Some(ClientEvent::Thread { thread_id, item }) => {
                    let label = match item.as_ref() {
                        ThreadStreamItem::Snapshot { snapshot_sequence, projection, has_more_history, latest_local_turn_ordinal, .. } => format!(
                            "snapshot seq={snapshot_sequence} items={} visible={} has_more={has_more_history:?} watermark={latest_local_turn_ordinal:?}",
                            projection.turn_items.len(), projection.visible_turn_items.len()
                        ),
                        ThreadStreamItem::Event { sequence, .. } => format!("event seq={sequence}"),
                        other => format!("{other:?}"),
                    };
                    if thread.apply(*item) && !has("--quiet-thread") {
                        println!("thread {thread_id}: {label}");
                    }
                    let Some(projection) = &thread.projection else { continue };
                    let pending = requests::pending_requests(projection);
                    for approval in &pending.approvals {
                        if !seen.insert(approval.request_id.clone()) { continue }
                        let (label, _) = approval.presentation();
                        let options: Vec<&str> = approval.options.iter().map(|o| o.label.as_str()).collect();
                        println!("pending {label}: {:?} live={} options={options:?}", approval.detail, approval.live);
                        if let Some(decision) = approve.filter(|_| approval.live) {
                            let command = commands::respond_to_approval(&new_id(), &thread_id, &approval.request_id, decision, None);
                            spawn_dispatch(&client, format!("respond {}", decision.as_str()), command);
                        }
                    }
                    for input in &pending.user_inputs {
                        if !seen.insert(input.request_id.clone()) { continue }
                        for question in &input.questions {
                            let options: Vec<&str> = question.options.iter().map(|o| o.label.as_str()).collect();
                            println!("pending question [{}] {}: {options:?}", question.header, question.question);
                        }
                        if !answer || !input.can_respond() { continue }
                        let drafts: HashMap<String, DraftAnswer> = input
                            .questions
                            .iter()
                            .filter_map(|q| Some((q.id.clone(), requests::toggle_option(q, None, q.options.first()?.answer_value()))))
                            .collect();
                        match requests::build_answers(&input.questions, &drafts) {
                            Some(answers) => {
                                println!("answering: {}", serde_json::Value::Object(answers.clone()));
                                let command = commands::respond_to_user_input(&new_id(), &thread_id, &input.request_id, answers);
                                spawn_dispatch(&client, "answer".into(), command);
                            }
                            None => println!("cannot answer: a question has no options"),
                        }
                    }
                }
                Some(ClientEvent::StreamError { context, message }) => println!("decode error in {context}: {message}"),
            }
        }
    }
    if let (Some(thread_id), Some(loads)) = (flag("--thread"), flag("--history").and_then(|n| n.parse::<usize>().ok())) {
        for _ in 0..loads {
            let Some(mut cursor) = thread.begin_history_load() else { break };
            loop {
                let started = std::time::Instant::now();
                let step = match client.history_page(&thread_id, &cursor).await {
                    Ok(page) => {
                        let rows = page.items.len();
                        let step = thread.apply_history_page(&cursor, page);
                        println!("history page: {rows} rows in {:?} -> {step:?}", started.elapsed());
                        step
                    }
                    Err(message) => thread.fail_history_load(&cursor, message),
                };
                match step {
                    t3_core::history::HistoryStep::Continue(next) => cursor = next,
                    other => {
                        println!("history load: {other:?}");
                        break;
                    }
                }
            }
        }
        let visible = thread.projection.as_ref().map_or(0, |p| p.visible_turn_items.len());
        println!("history: visible={visible} has_more={} expanded={}", thread.history.has_more_history, thread.history.expanded);
    }
    if let Some(config) = &config.config {
        println!(
            "config: environment={} threadStop={:?} defaultRuntimeMode={:?} favorites={:?}",
            config.environment.label,
            config.environment.capabilities.thread_stop,
            config.settings.default_runtime_mode,
            config.settings.favorites
        );
        for entry in providers::entries(config, None) {
            let models = providers::model_options(&config.settings, entry.provider);
            println!(
                "  provider {} ({}) driver={} enabled={} ready={} status={} models={}",
                entry.display_name,
                entry.instance_id(),
                entry.driver(),
                entry.enabled,
                entry.is_ready(),
                entry.provider.status,
                models.len()
            );
            if has("--config") {
                for model in models.iter().take(6) {
                    let descriptors = model.capabilities.as_ref().map(|c| c.option_descriptors.len()).unwrap_or(0);
                    println!("    model {} ({}) descriptors={descriptors}", providers::model_name(model), model.slug);
                }
            }
        }
    }
    if let Some(snapshot) = &shell.snapshot {
        println!(
            "final: {} projects, {} threads, synchronized={}",
            snapshot.projects.len(),
            snapshot.threads.len(),
            shell.synchronized
        );
        if has("--config") {
            for project in &snapshot.projects {
                println!("  project {} {}", project.id, project.title);
            }
        }
        if let Some(thread) = watched.as_deref().and_then(|id| snapshot.threads.iter().find(|t| t.id == id)) {
            println!(
                "watched: {} \"{}\" status={} runtimeMode={} pending={:?}",
                thread.id,
                thread.title,
                thread.status,
                thread.runtime_mode,
                thread.pending_runtime_request.as_ref().map(|p| &p.kind)
            );
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        // `--settled N`: read N older settled pages below the shell's cut, as the shelf does.
        let mut mounts = t3_core::families::MountedReads::default();
        let mut pages = t3_core::sidebar::older_settled::SettledPages::default();
        let mut shelves = t3_core::sidebar::ShelfState::default();
        let older = snapshot.older_settled.as_ref().and_then(t3_core::sidebar::older_settled::OlderSettled::from_cut);
        if let (Some(wanted), Some(older)) = (flag("--settled").and_then(|n| n.parse::<usize>().ok()), older.as_ref()) {
            println!("older settled: before {} counts {:?}", older.before, older.counts);
            pages.sync(&shell, Some(older));
            shelves.settled_expanded = true;
            while pages.reads.len() < wanted && pages.grow(Some(older), 0, usize::MAX) {
                let Some((generation, before)) = pages.next_request() else { break };
                let payload = t3_core::sidebar::older_settled::page_request(&before, None);
                let started = std::time::Instant::now();
                let page = client
                    .request(t3_core::sidebar::older_settled::GET_OLDER_SETTLED_THREADS, payload)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()));
                let rows: Result<usize, String> = page.as_ref().map(|p: &t3_core::families::FamilyRead| p.threads.len()).map_err(Clone::clone);
                pages.apply_page(generation, &before, page);
                println!("older page: {rows:?} rows in {:?}, frontier {:?}", started.elapsed(), pages.frontier);
            }
            mounts.mount("older-settled", shell.generation, pages.reads.clone());
            shelves.settled_visible = usize::MAX;
        }
        let extras = mounts.extras(&shell);
        let model = t3_core::sidebar::SidebarModel::build(
            &t3_core::sidebar::ModelInput {
                snapshot,
                extra_threads: &extras,
                older_settled: older.as_ref().map(|older| t3_core::sidebar::OlderSettledShelf { older, frontier: pages.listing_frontier(Some(older)) }),
                settings: &Default::default(),
                shelves: &shelves,
                selected_thread_id: None,
                project_scope: None,
                connected: true,
                now,
            },
            &mut Default::default(),
        );
        println!("settled: count={} listed={} hidden={}", model.settled_count, model.listed_settled_count, model.hidden_settled_count);
        for row in model.rows.iter().take(12) {
            match row {
                t3_core::sidebar::SidebarRow::Thread { index, section } => println!("  {section:?}: {}", model.threads[*index].title),
                other => println!("  {other:?}"),
            }
        }
    }
    Ok(())
}
