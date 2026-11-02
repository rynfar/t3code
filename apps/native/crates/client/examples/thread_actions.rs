//! Headless check of the sidebar's thread actions against a live server: pins a thread, waits
//! for the shell to carry `pinnedAt`, reorders the pin and the Active order, unpins it, then settles and un-settles it, printing the
//! sidebar section the thread lands in after each step.
//!
//!   cargo run -p t3-client --example thread_actions -- [--thread <thread-id>]

use std::time::Duration;

use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, start};
use t3_core::shell::{ShellState, ThreadShell};
use t3_core::sidebar::{ModelInput, Section, SidebarModel};
use t3_core::thread_actions as actions;
use tokio::sync::mpsc::Receiver;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn section(shell: &ShellState, thread_id: &str) -> Option<Section> {
    let snapshot = shell.snapshot.as_ref()?;
    let input = ModelInput {
        snapshot,
        extra_threads: &[],
        older_settled: None,
        settings: &Default::default(),
        shelves: &Default::default(),
        selected_thread_id: Some(thread_id),
        project_scope: None,
        connected: true,
        dragging: None,
        now: now(),
    };
    SidebarModel::build(&input, &mut Default::default()).section_of(thread_id)
}

/// Applies shell events until `done` holds for the thread, or times out.
async fn wait_for(events: &mut Receiver<ClientEvent>, shell: &mut ShellState, thread_id: &str, done: impl Fn(&ThreadShell) -> bool) -> bool {
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    loop {
        if shell.thread(thread_id).is_some_and(&done) {
            return true;
        }
        tokio::select! {
            _ = &mut deadline => return false,
            event = events.recv() => match event {
                Some(ClientEvent::Shell(item)) => { shell.apply(item); }
                Some(_) => {}
                None => return false,
            }
        }
    }
}

async fn step(client: &ClientHandle, label: &str, command: serde_json::Value) -> anyhow::Result<()> {
    let result = client.dispatch(command).await;
    println!("{label}: {}", if result.is_ok() { "accepted".to_owned() } else { format!("{result:?}") });
    result.map(|_| ()).map_err(|error| anyhow::anyhow!("{error}"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    while !shell.synchronized {
        match events.recv().await {
            Some(ClientEvent::Shell(item)) => {
                shell.apply(item);
            }
            Some(_) => {}
            None => anyhow::bail!("connection closed"),
        }
    }
    let thread_id = match flag("--thread") {
        Some(id) => id,
        None => shell
            .snapshot
            .as_ref()
            .and_then(|s| s.threads.iter().find(|t| t.lineage.is_listed() && t.pinned_at.is_none() && t.archived_at.is_none()))
            .map(|t| t.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no unpinned thread to use"))?,
    };
    let title = shell.thread(&thread_id).map(|t| t.title.clone()).unwrap_or_default();
    println!("thread {thread_id} \"{title}\": section {:?}", section(&shell, &thread_id));

    let id = || uuid::Uuid::new_v4().to_string();
    step(&client, "thread.pin", actions::pin(&id(), &thread_id, None)).await?;
    let pinned = wait_for(&mut events, &mut shell, &thread_id, |t| t.pinned_at.is_some()).await;
    println!("  shell pinnedAt={:?} -> section {:?}", shell.thread(&thread_id).and_then(|t| t.pinned_at.clone()), section(&shell, &thread_id));
    anyhow::ensure!(pinned, "pinnedAt never arrived");

    // The drag-and-drop writes: move the pin to a fresh key, then an Active order key.
    step(&client, "thread.pin.reorder", actions::reorder_pin(&id(), &thread_id, "n")).await?;
    let reordered = wait_for(&mut events, &mut shell, &thread_id, |t| t.pin_order_key.as_deref() == Some("n")).await;
    println!("  shell pinOrderKey={:?}", shell.thread(&thread_id).and_then(|t| t.pin_order_key.clone()));
    anyhow::ensure!(reordered, "pin reorder never arrived");

    step(&client, "thread.unpin", actions::unpin(&id(), &thread_id)).await?;
    let unpinned = wait_for(&mut events, &mut shell, &thread_id, |t| t.pinned_at.is_none()).await;
    println!("  shell pinnedAt={:?} -> section {:?}", shell.thread(&thread_id).and_then(|t| t.pinned_at.clone()), section(&shell, &thread_id));
    anyhow::ensure!(unpinned, "unpin never arrived");
    step(&client, "thread.active.reorder", actions::reorder_active(&id(), &thread_id, "m")).await?;
    let ordered = wait_for(&mut events, &mut shell, &thread_id, |t| t.active_order_key.as_deref() == Some("m")).await;
    println!("  shell activeOrderKey={:?}", shell.thread(&thread_id).and_then(|t| t.active_order_key.clone()));
    anyhow::ensure!(ordered, "active reorder never arrived");

    let was_settled = shell.thread(&thread_id).is_some_and(|t| t.is_settled());
    if !was_settled {
        step(&client, "thread.settle", actions::settle(&id(), &thread_id)).await?;
        let settled = wait_for(&mut events, &mut shell, &thread_id, |t| t.is_settled()).await;
        println!("  shell settledOverride={:?} -> section {:?}", shell.thread(&thread_id).and_then(|t| t.settled_override.clone()), section(&shell, &thread_id));
        anyhow::ensure!(settled, "settle never arrived");
        step(&client, "thread.unsettle", actions::unsettle(&id(), &thread_id)).await?;
        let active = wait_for(&mut events, &mut shell, &thread_id, |t| !t.is_settled()).await;
        println!("  shell settledOverride={:?} -> section {:?}", shell.thread(&thread_id).and_then(|t| t.settled_override.clone()), section(&shell, &thread_id));
        anyhow::ensure!(active, "unsettle never arrived");
    }

    let wake = now() + 60 * 60 * 1000;
    step(&client, "thread.snooze", actions::snooze(&id(), &thread_id, wake)).await?;
    let snoozed = wait_for(&mut events, &mut shell, &thread_id, |t| t.snoozed_until.is_some()).await;
    println!("  shell snoozedUntil={:?} -> section {:?}", shell.thread(&thread_id).and_then(|t| t.snoozed_until.clone()), section(&shell, &thread_id));
    anyhow::ensure!(snoozed, "snooze never arrived");
    step(&client, "thread.unsnooze", actions::unsnooze(&id(), &thread_id)).await?;
    let awake = wait_for(&mut events, &mut shell, &thread_id, |t| t.snoozed_until.is_none()).await;
    println!("  shell snoozedUntil={:?} -> section {:?}", shell.thread(&thread_id).and_then(|t| t.snoozed_until.clone()), section(&shell, &thread_id));
    anyhow::ensure!(awake, "unsnooze never arrived");

    let search = client.request("orchestration.searchThreads", serde_json::json!({ "query": title.split_whitespace().next().unwrap_or("go") })).await;
    println!("orchestration.searchThreads: {}", match search {
        Ok(value) => format!("{} message matches", value.get("matches").and_then(|m| m.as_array()).map_or(0, Vec::len)),
        Err(error) => format!("{error}"),
    });
    println!("ok");
    Ok(())
}
