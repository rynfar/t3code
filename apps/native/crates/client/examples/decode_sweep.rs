//! Subscribes to every thread on the saved server and reports decode failures, item counts, and
//! snapshot sizes. Run against real data after any contracts change: a decode error here is a
//! `parity:wire` bug.
//!
//!   T3_NATIVE_HOME=… cargo run -p t3-client --example decode_sweep -- --seconds 20

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::shell::ShellState;
use t3_core::thread::ThreadStreamItem;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let seconds: u64 = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    let mut opened = false;
    let mut snapshots: HashMap<String, (usize, usize)> = HashMap::new();
    let mut errors: BTreeMap<String, String> = BTreeMap::new();
    let mut item_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        let Ok(Some(event)) = tokio::time::timeout(deadline - Instant::now(), events.recv()).await else { break };
        match event {
            ClientEvent::Shell(item) => {
                shell.apply(item);
                if shell.synchronized && !opened {
                    opened = true;
                    for thread in shell.snapshot.iter().flat_map(|s| s.threads.iter()) {
                        client.open_thread(&thread.id);
                    }
                }
            }
            ClientEvent::Thread { thread_id, item } => {
                if let ThreadStreamItem::Snapshot { projection, .. } = *item {
                    for row in &projection.turn_items {
                        let kind = format!("{:?}", row.kind);
                        let name = kind.split([' ', '{', '(']).next().unwrap_or_default().to_owned();
                        *item_kinds.entry(name).or_default() += 1;
                    }
                    snapshots.insert(thread_id, (projection.turn_items.len(), projection.visible_turn_items.len()));
                }
            }
            ClientEvent::StreamError { context, message } => {
                errors.insert(context, message.chars().take(300).collect());
            }
            ClientEvent::Status(_) => {}
        }
        let total = shell.snapshot.as_ref().map(|s| s.threads.len()).unwrap_or(usize::MAX);
        if opened && snapshots.len() + errors.len() >= total {
            break;
        }
    }
    let total = shell.snapshot.as_ref().map(|s| s.threads.len()).unwrap_or(0);
    let items: usize = snapshots.values().map(|(items, _)| items).sum();
    println!("threads: {total}, snapshots decoded: {}, decode errors: {}, turn items: {items}", snapshots.len(), errors.len());
    for (kind, count) in &item_kinds {
        println!("  {kind}: {count}");
    }
    let mut largest: Vec<_> = snapshots.iter().collect();
    largest.sort_by_key(|(_, (items, _))| std::cmp::Reverse(*items));
    for (thread_id, (items, visible)) in largest.into_iter().take(3) {
        println!("  largest: {thread_id} items={items} visible={visible}");
    }
    for (context, message) in &errors {
        println!("ERROR {context}: {message}");
    }
    Ok(())
}
