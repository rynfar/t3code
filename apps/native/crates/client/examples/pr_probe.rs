//! Headless check of the pull request reads, decoded with `t3_core::pull_requests`: the refresh
//! signal, the list (and its line counts), then one pull request's detail, stack, checks,
//! activity and patch. Read-only.
//!
//!   cargo run -p t3-client --example pr_probe
//!   cargo run -p t3-client --example pr_probe -- --state all --query "label:bug" --number 42

use serde_json::Value;
use t3_client::session::RpcError;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, ConnectionStatus, start};
use t3_core::diff::{ParsedPatch, parse_patch};
use t3_core::pull_requests::list::{ListState, Shown, Sort, shown_groups};
use t3_core::pull_requests::wire::{Activity, Detail, DiffResult, ListResult, ListStats, Stack, decode, decode_checks};
use t3_core::pull_requests::{ListPreferences, PAGE_SIZE, PrRef, failure_message, list_input, method, stats_inputs};

async fn read(client: &ClientHandle, method: &'static str, payload: Value) -> Option<Value> {
    match client.request(method, payload).await {
        Ok(value) => Some(value),
        Err(RpcError::Failure(failure)) => {
            println!("{method}: {}", failure_message(failure.tag.as_deref(), &failure.error, &failure.message));
            None
        }
        Err(error) => {
            println!("{method}: {error}");
            None
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    while let Some(event) = events.recv().await {
        match event {
            ClientEvent::Status(ConnectionStatus::Connected) => break,
            ClientEvent::Status(ConnectionStatus::NeedsPairing { reason }) => anyhow::bail!(reason),
            _ => {}
        }
    }
    match client.stream(method::SUBSCRIBE_REFRESHES, serde_json::json!({})).await {
        Ok(mut stream) => match tokio::time::timeout(std::time::Duration::from_secs(2), stream.receiver.recv()).await {
            Ok(Some(Ok(epoch))) => println!("subscribeRefreshes: epoch {epoch}"),
            Ok(other) => println!("subscribeRefreshes: {other:?}"),
            Err(_) => println!("subscribeRefreshes: no refresh yet (epoch 0)"),
        },
        Err(error) => println!("subscribeRefreshes: {error}"),
    }
    let state: ListState = serde_json::from_value(Value::String(flag("--state").unwrap_or_else(|| "open".into()))).unwrap_or_default();
    let preferences = ListPreferences { state, ..Default::default() };
    let query = flag("--query").unwrap_or_default();
    let Some(value) = read(&client, method::LIST, list_input(&preferences, &query, PAGE_SIZE, None)).await else { return Ok(()) };
    let raw = value["entries"].as_array().map_or(0, Vec::len);
    let list: ListResult = decode(value).map_err(|error| anyhow::anyhow!(error))?;
    let parsed = t3_core::pull_requests::query::parse_query(&query);
    let filters = preferences.menu_filters().overlay(&parsed.filters);
    let groups = shown_groups(&Shown {
        entries: &list.entries,
        viewers: &list.viewers,
        providers: &list.providers,
        involvement: preferences.involvement,
        filters: &filters,
        text: &parsed.text,
        answered: true,
        sort: Sort::Ready,
        state,
    });
    println!(
        "list: {raw} on the wire, {} decoded, {} providers, {} errors, truncated={}, viewers={:?}",
        list.entries.len(),
        list.providers.len(),
        list.errors.len(),
        list.truncated,
        list.viewers
    );
    for group in &groups {
        println!("  group {:?}: {} rows", group.key, group.entries.len());
    }
    for batch in stats_inputs(&list.entries) {
        if let Some(value) = read(&client, method::LIST_STATS, batch).await {
            let stats: ListStats = decode(value).map_err(|error| anyhow::anyhow!(error))?;
            println!("listStats: {} rows measured", stats.stats.len());
        }
    }
    let wanted: Option<u64> = flag("--number").and_then(|n| n.parse().ok());
    let entry = list.entries.iter().find(|entry| wanted.is_none_or(|n| entry.number == n));
    let Some(entry) = entry else { return Ok(()) };
    let reference = PrRef::of(entry);
    println!("pull request {} #{}: {}", entry.repository, entry.number, entry.title);
    if let Some(value) = read(&client, method::DETAIL, reference.payload()).await {
        let detail: Detail = decode(value).map_err(|error| anyhow::anyhow!(error))?;
        let stack = match detail.capabilities.stacks {
            Some(true) => read(&client, method::STACK, reference.payload()).await.map(|value| decode::<Option<Stack>>(value).map_err(|error| error.to_string())),
            _ => Some(Ok(None)),
        };
        let gate = t3_core::pull_requests::detail::merge_gate(&detail, stack.as_ref());
        println!("  stack: {:?} gate={gate:?}", stack.as_ref().map(|stack| stack.as_ref().map(|stack| stack.as_ref().map(|stack| (stack.number, stack.layers.len())))));
        println!(
            "  detail: {:?} draft={} +{} -{} files={} checks={} labels={} reviewers={} primary={:?}",
            detail.state,
            detail.is_draft,
            detail.additions,
            detail.deletions,
            detail.changed_files,
            detail.checks.len(),
            detail.labels.len(),
            detail.reviewers.len(),
            t3_core::pull_requests::detail::primary_control(&detail, gate.stack_owns)
        );
        println!("  checks: {}", t3_core::pull_requests::detail::describe_checks(&detail.checks));
        if let Some(value) = read(&client, method::ACTIVITY, reference.payload()).await {
            let activity: Activity = decode(value).map_err(|error| anyhow::anyhow!(error))?;
            let timeline = t3_core::pull_requests::detail::build_timeline(&detail, &activity, "pull request");
            println!(
                "  activity: {} comments ({} total), {} review threads, {} commits -> {} timeline events",
                activity.comments.len(),
                activity.comment_count,
                activity.review_threads.len(),
                activity.commits.len(),
                timeline.len()
            );
        }
    }
    if let Some(value) = read(&client, method::CHECKS, reference.payload()).await {
        let checks = decode_checks(value).map_err(|error| anyhow::anyhow!(error))?;
        println!("  checks read: {:?}", checks.map(|checks| checks.checks.len()));
    }
    match client.pull_request_diff(reference.payload()).await {
        Ok(value) => {
            let diff: DiffResult = decode(value).map_err(|error| anyhow::anyhow!(error))?;
            let files = match parse_patch(&diff.patch) {
                ParsedPatch::Files(files) => files.len(),
                _ => 0,
            };
            println!("  diff: {} bytes, {files} files, truncated={}, more={}", diff.patch.len(), diff.truncated, diff.next_cursor.is_some());
        }
        Err(failure) => println!("  diff: {}", failure_message(failure.tag.as_deref(), &failure.error, &failure.message)),
    }
    Ok(())
}
