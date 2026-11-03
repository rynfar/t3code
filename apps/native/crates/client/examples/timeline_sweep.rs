//! Derives transcript rows for every thread on the saved server and reports row kinds, empty
//! labels and derive time. Read-only: it subscribes and never dispatches. Prints counts, never
//! thread content, so it is safe to run against real data.
//!
//!   T3_NATIVE_HOME=… cargo run -p t3-client --example timeline_sweep -- --seconds 30

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::shell::ShellState;
use t3_core::thread::{ThreadState, ThreadStreamItem};
use t3_core::timeline::{EntriesCache, RowKind, RowsCache, RowsInput, derive_activity, derive_entries, derive_rows, work_entry_display_label};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let seconds: u64 = args.iter().position(|a| a == "--seconds").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(30);
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    let mut opened = false;
    let mut done: HashSet<String> = HashSet::new();
    let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut slowest = (Duration::ZERO, 0usize);
    let mut slowest_warm = (Duration::ZERO, 0usize);
    let (mut total_items, mut total_rows, mut empty_labels) = (0usize, 0usize, 0usize);
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
                if !matches!(*item, ThreadStreamItem::Snapshot { .. }) || !done.insert(thread_id) {
                    continue;
                }
                let mut state = ThreadState::default();
                state.apply(*item);
                let Some(projection) = state.projection else { continue };
                let started = Instant::now();
                let mut cache = EntriesCache::default();
                let entries = derive_entries(&mut cache, &projection.visible_turn_items, &projection.attempts, &projection.nodes, &projection.plans);
                let activity = derive_activity(&projection);
                let (none, expanded_runs): (HashSet<String>, HashSet<String>) =
                    (HashSet::new(), projection.runs.iter().map(|run| run.id.clone()).collect());
                let input = RowsInput { entries: &entries, activity: &activity, expanded_run_ids: &expanded_runs, expanded_attempt_ids: &none, expanded_work_group_ids: &none, worktree_setup: None };
                let mut rows_cache = RowsCache::default();
                let rows = derive_rows(&input, &mut rows_cache);
                let elapsed = started.elapsed();
                if elapsed > slowest.0 {
                    slowest = (elapsed, projection.visible_turn_items.len());
                }
                let warm_start = Instant::now();
                let warm_entries = derive_entries(&mut cache, &projection.visible_turn_items, &projection.attempts, &projection.nodes, &projection.plans);
                let warm_input = RowsInput { entries: &warm_entries, ..input };
                let _ = derive_rows(&warm_input, &mut rows_cache);
                let warm = warm_start.elapsed();
                if warm > slowest_warm.0 {
                    slowest_warm = (warm, projection.visible_turn_items.len());
                }
                total_items += projection.visible_turn_items.len();
                total_rows += rows.len();
                for row in &rows {
                    let kind = match &row.kind {
                        RowKind::Work { entries, display_label, .. } => {
                            let label = display_label.clone().unwrap_or_else(|| work_entry_display_label(&entries[0], None));
                            empty_labels += usize::from(label.trim().is_empty());
                            "work"
                        }
                        RowKind::WorkLive { .. } => "work-live",
                        RowKind::Working { .. } => "working",
                        RowKind::Thinking { .. } => "thinking",
                        RowKind::WorkToggle { summary, .. } => {
                            empty_labels += usize::from(summary.trim().is_empty());
                            "work-toggle"
                        }
                        RowKind::TurnFold { .. } => "turn-fold",
                        RowKind::AttemptFold { .. } => "attempt-fold",
                        RowKind::ContextCompaction { .. } => "context-compaction",
                        RowKind::Message { .. } => "message",
                        RowKind::AssistantMeta { .. } => "assistant-meta",
                        RowKind::Event { .. } => "event",
                        RowKind::ProposedPlan(_) => "proposed-plan",
                        RowKind::WorktreeSetup { .. } => "worktree-setup",
                        RowKind::HtmlRender(_) => "html-render",
                    };
                    *kinds.entry(kind).or_default() += 1;
                }
            }
            ClientEvent::StreamError { context, message } => println!("stream error in {context}: {}", message.chars().take(160).collect::<String>()),
            ClientEvent::Status(_) | ClientEvent::ServerConfig(_) | ClientEvent::Moved { .. } => {}
        }
        let total = shell.snapshot.as_ref().map(|s| s.threads.len()).unwrap_or(usize::MAX);
        if opened && done.len() >= total {
            break;
        }
    }
    let per_kind: HashMap<_, _> = kinds.iter().collect();
    println!("threads={} visible_items={total_items} rows={total_rows} empty_labels={empty_labels}", done.len());
    println!("slowest derive: {:?} for {} items", slowest.0, slowest.1);
    println!("slowest warm derive (per streamed update): {:?} for {} items", slowest_warm.0, slowest_warm.1);
    println!("rows by kind: {per_kind:?}");
    Ok(())
}
