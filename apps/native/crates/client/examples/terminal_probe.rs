//! Headless terminal check against a live server: opens a fresh terminal for a thread, attaches,
//! runs `echo hello && pwd`, prints what comes back, resizes, and closes it (deleting its
//! history). With `--observe` it follows an existing terminal read-only instead (optionally
//! typing `--run` into it) and leaves it open. Uses the saved connection, like `probe`.
//!
//!   cargo run -p t3-client --example terminal_probe -- [--thread <thread-id>] [--seconds 3]
//!   cargo run -p t3-client --example terminal_probe -- --thread <id> --observe term-1 [--run 'ls']

use std::time::Duration;

use t3_client::store;
use t3_client::supervisor::{ClientEvent, ConnectionStatus, start};
use t3_core::shell::ShellState;
use t3_core::terminal::{self, AttachEvent, BufferState, Grid, LaunchLocation, MetadataEvent, OutputOp, method};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let seconds: u64 = flag("--seconds").and_then(|s| s.parse().ok()).unwrap_or(3);
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);

    let mut shell = ShellState::default();
    let mut connected = false;
    while !(connected && shell.synchronized) {
        match tokio::time::timeout(Duration::from_secs(15), events.recv()).await? {
            Some(ClientEvent::Status(ConnectionStatus::Connected)) => connected = true,
            Some(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })) => anyhow::bail!(reason),
            Some(ClientEvent::Shell(item)) => {
                shell.apply(item);
            }
            Some(_) => {}
            None => anyhow::bail!("client stopped"),
        }
    }
    let snapshot = shell.snapshot.as_ref().ok_or_else(|| anyhow::anyhow!("no shell snapshot"))?;
    let thread = match flag("--thread") {
        Some(id) => shell.thread(&id).ok_or_else(|| anyhow::anyhow!("unknown thread {id}"))?,
        None => snapshot.threads.first().ok_or_else(|| anyhow::anyhow!("the server has no threads"))?,
    };
    let project = shell.project(&thread.project_id).ok_or_else(|| anyhow::anyhow!("thread has no project"))?;
    let location = LaunchLocation::new(&project.workspace_root, thread.worktree_path.as_deref());
    let terminal_id = terminal::next_terminal_id([], Some(&uuid::Uuid::new_v4().to_string()));
    let thread_id = thread.id.clone();
    if let Some(terminal_id) = flag("--observe") {
        return observe(&client, &thread_id, &terminal_id, flag("--run"), seconds).await;
    }
    println!("thread {thread_id}\nterminal {terminal_id} ({})\ncwd {}", terminal::terminal_label(&terminal_id), location.cwd);

    let mut metadata = client.stream(method::SUBSCRIBE_METADATA, serde_json::json!({})).await?;
    if let Some(Ok(item)) = metadata.receiver.recv().await {
        match serde_json::from_value::<MetadataEvent>(item)? {
            MetadataEvent::Snapshot { terminals } => println!("metadata: snapshot with {} terminal(s)", terminals.len()),
            other => println!("metadata: {other:?}"),
        }
    }

    let grid = Grid::new(80, 24);
    let opened = client.request(method::OPEN, terminal::open_input(&thread_id, &terminal_id, &location, Some(grid))).await?;
    println!(
        "open: status={} pid={} cols={} rows={}",
        opened["status"], opened["pid"], opened["cols"], opened["rows"]
    );

    let attach = terminal::attach_input(&thread_id, &terminal_id, &location, Some(grid), false);
    let mut stream = client.stream(method::ATTACH, attach).await?;
    let mut buffer = BufferState::default();
    let mut output = String::new();
    let mut wrote = false;
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            item = stream.receiver.recv() => {
                let Some(item) = item else { break };
                let event: AttachEvent = serde_json::from_value(item?)?;
                let kind = event_kind(&event);
                match buffer.apply(event) {
                    OutputOp::Reset(history) => {
                        println!("attach: {kind} status={:?} grid={:?} history={}B", buffer.status, buffer.grid, history.len());
                        output = history;
                    }
                    OutputOp::Append(data) => output.push_str(&data),
                    OutputOp::None => println!("attach: {kind} status={:?} grid={:?}", buffer.status, buffer.grid),
                }
                if !wrote && buffer.version > 0 {
                    wrote = true;
                    client.request(method::WRITE, terminal::write_input(&thread_id, &terminal_id, "echo hello && pwd\r")).await?;
                    println!("write: echo hello && pwd");
                }
            }
        }
    }
    println!("output (control sequences stripped):");
    for line in strip_controls(&output).lines().filter(|line| !line.trim().is_empty()) {
        println!("  | {line}");
    }

    let resized = client.request(method::RESIZE, terminal::resize_input(&thread_id, &terminal_id, Grid::new(100, 30))).await;
    println!("resize 100x30: {}", if resized.is_ok() { "ok".to_owned() } else { format!("{resized:?}") });
    if let Ok(Some(Ok(item))) = tokio::time::timeout(Duration::from_millis(800), stream.receiver.recv()).await {
        let event: AttachEvent = serde_json::from_value(item)?;
        buffer.apply(event.clone());
        println!("attach: {} grid={:?}", event_kind(&event), buffer.grid);
    }

    let closed = client.request(method::CLOSE, terminal::close_input(&thread_id, &terminal_id, true)).await;
    println!("close (deleteHistory): {}", if closed.is_ok() { "ok".to_owned() } else { format!("{closed:?}") });
    let mut saw_closed = false;
    while let Ok(Some(Ok(item))) = tokio::time::timeout(Duration::from_secs(2), stream.receiver.recv()).await {
        let event: AttachEvent = serde_json::from_value(item)?;
        if matches!(event, AttachEvent::Closed | AttachEvent::Exited { .. }) {
            println!("attach: {}", event_kind(&event));
            saw_closed = true;
            break;
        }
    }
    while let Ok(Some(Ok(item))) = tokio::time::timeout(Duration::from_secs(1), metadata.receiver.recv()).await {
        if let Ok(MetadataEvent::Remove { terminal_id: removed, .. }) = serde_json::from_value::<MetadataEvent>(item)
            && removed == terminal_id
        {
            println!("metadata: removed {removed}");
            break;
        }
    }
    if !saw_closed {
        println!("attach: stream ended");
    }
    Ok(())
}

async fn observe(
    client: &t3_client::supervisor::ClientHandle,
    thread_id: &str,
    terminal_id: &str,
    run: Option<String>,
    seconds: u64,
) -> anyhow::Result<()> {
    let mut stream = client.stream(method::OBSERVE, terminal::observe_input(thread_id, terminal_id)).await?;
    let mut buffer = BufferState::default();
    let mut output = String::new();
    let mut ran = run.is_none();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            item = stream.receiver.recv() => {
                let Some(item) = item else { break };
                let event: AttachEvent = serde_json::from_value(item?)?;
                let kind = event_kind(&event);
                match buffer.apply(event) {
                    OutputOp::Reset(history) => {
                        println!("observe: {kind} status={:?} grid={:?} history={}B", buffer.status, buffer.grid, history.len());
                        output = history;
                    }
                    OutputOp::Append(data) => output.push_str(&data),
                    OutputOp::None => println!("observe: {kind} status={:?} grid={:?}", buffer.status, buffer.grid),
                }
                if !ran && buffer.version > 0 && let Some(command) = &run {
                    ran = true;
                    client.request(method::WRITE, terminal::write_input(thread_id, terminal_id, &format!("{command}\r"))).await?;
                    println!("write: {command}");
                }
            }
        }
    }
    let text = strip_controls(&output);
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    println!("output tail ({} bytes total):", output.len());
    for line in &lines[lines.len().saturating_sub(8)..] {
        println!("  | {}", line.chars().take(110).collect::<String>());
    }
    Ok(())
}

fn event_kind(event: &AttachEvent) -> &'static str {
    match event {
        AttachEvent::Snapshot { .. } => "snapshot",
        AttachEvent::Output { .. } => "output",
        AttachEvent::Exited { .. } => "exited",
        AttachEvent::Closed => "closed",
        AttachEvent::Error { .. } => "error",
        AttachEvent::Cleared => "cleared",
        AttachEvent::Restarted { .. } => "restarted",
        AttachEvent::Activity { .. } => "activity",
        AttachEvent::Unknown => "unknown",
    }
}

/// Drops CSI/OSC sequences and carriage returns so the transcript reads as plain text.
fn strip_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.peek() == Some(&'\\')) {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' | '\x07' | '\x08' => {}
            c => out.push(c),
        }
    }
    out
}
