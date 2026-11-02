//! Headless check of the diff panel's requests: opens a thread, lists its checkpointed turns,
//! then fetches the latest turn's diff and the full-thread diff and prints what they parse to.
//!
//!   cargo run -p t3-client --example diff_probe -- --thread <thread-id>
//!   cargo run -p t3-client --example diff_probe -- --thread <thread-id> --turn 2 --cwd /repo

use std::time::Duration;

use serde_json::json;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, ConnectionStatus, start};
use t3_core::diff::{
    self, ParsedPatch, PreviewSourceKind, checkpoint_diff_request, decode_diff_preview, decode_turn_diff,
    diff_preview_request, parse_patch, turn_diff_summaries,
};
use t3_core::shell::ShellState;
use t3_core::thread::ThreadState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let thread_id = flag("--thread").ok_or_else(|| anyhow::anyhow!("pass --thread <id>"))?;
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    client.open_thread(&thread_id);

    let mut shell = ShellState::default();
    let mut thread = ThreadState::default();
    let deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);
    while !thread.synchronized || !shell.synchronized {
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("timed out waiting for the thread snapshot"),
            event = events.recv() => match event {
                None => anyhow::bail!("client stopped"),
                Some(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })) => anyhow::bail!(reason),
                Some(ClientEvent::Thread { item, .. }) => { thread.apply(*item); }
                Some(ClientEvent::Shell(item)) => { shell.apply(item); }
                Some(_) => {}
            }
        }
    }
    let projection = thread.projection.as_ref().ok_or_else(|| anyhow::anyhow!("no projection"))?;
    let summaries = turn_diff_summaries(&projection.checkpoints);
    println!("thread {thread_id}: {} checkpoints, {} turns with diffs", projection.checkpoints.len(), summaries.len());
    for summary in &summaries {
        let totals = summary.totals();
        println!(
            "  turn {} run={} status={} files={} +{} -{}",
            summary.turn_count, summary.run_id, summary.status, totals.count, totals.additions, totals.deletions
        );
    }

    let turn = flag("--turn").and_then(|turn| turn.parse::<u64>().ok()).or(summaries.first().map(|s| s.turn_count));
    if let Some(turn) = turn {
        let (method, payload) = checkpoint_diff_request(&thread_id, turn, true);
        report(&client, &format!("turn {turn} ({method})"), method, payload).await;
        let full = json!({ "threadId": thread_id, "toTurnCount": turn, "ignoreWhitespace": true });
        report(&client, &format!("full thread to turn {turn}"), diff::method::GET_FULL_THREAD_DIFF, full).await;
    }
    // The panel's git scopes read the thread's worktree, else its project root (`DiffPanel.tsx`).
    let cwd = flag("--cwd").or_else(|| {
        let shell_thread = shell.thread(&thread_id)?;
        shell_thread.worktree_path.clone().or_else(|| Some(shell.project(&shell_thread.project_id)?.workspace_root.clone()))
    });
    if let Some(cwd) = cwd {
        println!("git scopes in {cwd}");
        let (method, payload) = diff_preview_request(&cwd, None, true);
        match client.request(method, payload).await {
            Ok(value) => {
                for (label, kind) in [("changes", PreviewSourceKind::BranchRange), ("uncommitted", PreviewSourceKind::WorkingTree)] {
                    match decode_diff_preview(&value, kind) {
                        Some(source) => print_patch(label, &source.diff),
                        None => println!("{label}: no source"),
                    }
                }
            }
            Err(error) => println!("diff preview failed: {error}"),
        }
    }
    Ok(())
}

async fn report(client: &ClientHandle, label: &str, method: &'static str, payload: serde_json::Value) {
    match client.request(method, payload).await {
        Ok(value) => match decode_turn_diff(&value) {
            Ok(patch) => print_patch(label, &patch),
            Err(error) => println!("{label}: {error}"),
        },
        Err(error) => println!("{label}: request failed: {error}"),
    }
}

fn print_patch(label: &str, patch: &str) {
    match parse_patch(patch) {
        ParsedPatch::Empty => println!("{label}: no net changes"),
        ParsedPatch::Raw { reason, text } => println!("{label}: {reason} ({} bytes)", text.len()),
        ParsedPatch::Files(files) => {
            let hunks: usize = files.iter().map(|file| file.hunks.len()).sum();
            let stat = diff::line_stat(&files);
            println!(
                "{label}: {} bytes, {} files, {hunks} hunks, +{} -{}",
                patch.len(),
                files.len(),
                stat.additions,
                stat.deletions
            );
            for file in files.iter().take(12) {
                println!(
                    "    {:<14} {} ({} hunks, +{} -{}{})",
                    file.change.as_str(),
                    file.path(),
                    file.hunks.len(),
                    file.additions,
                    file.deletions,
                    if file.binary { ", binary" } else { "" }
                );
            }
        }
    }
}
