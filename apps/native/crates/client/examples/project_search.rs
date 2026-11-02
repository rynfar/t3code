//! Headless check of go-to-file and content search: `projects.searchEntries` and
//! `projects.searchContents` against the first project's root, decoded as the palette decodes them.
//!
//!   cargo run -p t3-client --example project_search -- [--query main] [--content fn]

use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::project_search::{ContentOptions, ContentsResult, EntriesResult, SEARCH_CONTENTS, SEARCH_ENTRIES, contents_request, entries_request, file_picker_matches, results_summary};
use t3_core::shell::ShellState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
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
    let project = shell.snapshot.as_ref().and_then(|s| s.projects.first().cloned()).ok_or_else(|| anyhow::anyhow!("no project"))?;
    println!("project {} at {}", project.title, project.workspace_root);
    let query = flag("--query").unwrap_or_default();
    let entries: EntriesResult = serde_json::from_value(client.request(SEARCH_ENTRIES, entries_request(&project.workspace_root, &query)).await?)?;
    let matches = file_picker_matches(&entries.entries, &query);
    println!("searchEntries \"{query}\": {} files (truncated={})", matches.len(), entries.truncated);
    for found in matches.iter().take(5) {
        println!("  {} ({}) highlights={:?}", found.name, found.path, found.name_match_indices);
    }
    let content = flag("--content").unwrap_or_else(|| "fn".to_owned());
    if let Some(payload) = contents_request(&project.workspace_root, &content, ContentOptions::default()) {
        let result: ContentsResult = serde_json::from_value(client.request(SEARCH_CONTENTS, payload).await?)?;
        println!("searchContents \"{content}\": {}", results_summary(&result));
        for found in result.matches.iter().take(3) {
            println!("  {}:{} {}", found.path, found.line_number, found.line_content.trim());
        }
    }
    Ok(())
}
