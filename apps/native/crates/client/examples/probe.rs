//! Headless check against a live server: pairs (or reuses saved credentials), subscribes the
//! shell, optionally one thread, and prints what arrives.
//!
//!   cargo run -p t3-client --example probe -- --pair 'http://127.0.0.1:3773/pair#token=…'
//!   cargo run -p t3-client --example probe -- --thread <thread-id> --seconds 10
//!   cargo run -p t3-client --example probe -- --thread <thread-id> --send "hello"   # costs a turn

use std::time::Duration;

use t3_client::supervisor::{ClientEvent, start};
use t3_client::{auth, store};
use t3_core::shell::{ShellState, ShellStreamItem};
use t3_core::thread::{ThreadState, ThreadStreamItem};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
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
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    if let Some(thread_id) = flag("--thread") {
        client.open_thread(&thread_id);
        if let Some(text) = flag("--send") {
            let command_id = uuid::Uuid::new_v4().to_string();
            let command = t3_core::commands::send_message(
                &command_id,
                &thread_id,
                &text,
                &t3_core::commands::DispatchMode::QueueAfterActive,
            );
            let client = client.clone();
            tokio::spawn(async move { println!("dispatch: {:?}", client.dispatch(command).await) });
        }
    }
    let mut shell = ShellState::default();
    let mut thread = ThreadState::default();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            event = events.recv() => match event {
                None => break,
                Some(ClientEvent::Status(status)) => println!("status: {status:?}"),
                Some(ClientEvent::Shell(item)) => {
                    let label = match &item {
                        ShellStreamItem::Snapshot { snapshot, .. } => format!(
                            "snapshot seq={} projects={} threads={}",
                            snapshot.snapshot_sequence, snapshot.projects.len(), snapshot.threads.len()
                        ),
                        other => format!("{other:?}").chars().take(120).collect(),
                    };
                    shell.apply(item);
                    println!("shell: {label}");
                }
                Some(ClientEvent::Thread { thread_id, item }) => {
                    let label = match item.as_ref() {
                        ThreadStreamItem::Snapshot { snapshot_sequence, projection, .. } => format!(
                            "snapshot seq={snapshot_sequence} items={} visible={}",
                            projection.turn_items.len(), projection.visible_turn_items.len()
                        ),
                        ThreadStreamItem::Event { sequence, .. } => format!("event seq={sequence}"),
                        other => format!("{other:?}"),
                    };
                    thread.apply(*item);
                    println!("thread {thread_id}: {label}");
                }
                Some(ClientEvent::StreamError { context, message }) => println!("decode error in {context}: {message}"),
            }
        }
    }
    if let Some(snapshot) = &shell.snapshot {
        println!("final: {} projects, {} threads, synchronized={}", snapshot.projects.len(), snapshot.threads.len(), shell.synchronized);
        for row in t3_core::sidebar::sidebar_rows(snapshot).iter().take(12) {
            println!("  {row:?}");
        }
    }
    Ok(())
}
