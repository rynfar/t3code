//! Headless check of the file surfaces' requests, read-only: lists a project's root, walks its
//! folders four at a time as expand all does (ignored folders skipped), searches, reads files
//! the way the preview does, and fetches an image through `assets.createUrl`.
//!
//!   cargo run -p t3-client --example files_probe -- [--project <id>] [--walk 200] \
//!     [--search main] [--read src/main.rs]... [--image docs/logo.png] [--thread <id>]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use t3_client::session::RpcError;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, ConnectionStatus, start};
use t3_core::files::{
    CREATE_ASSET_URL, EntryKind, FileTree, LIST_ENTRIES, LineIndex, READ_FILE, ReadFailure, SEARCH_ENTRIES, absolute_path,
    decode_asset_url, decode_listing, decode_read_file, decode_search, image_asset_request, list_entries_request,
    read_file_request, search_entries_request, syntax_language,
};
use t3_core::shell::ShellState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let flags = |name: &str| args.windows(2).filter(|pair| pair[0] == name).map(|pair| pair[1].clone()).collect::<Vec<_>>();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    let deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);
    while !shell.synchronized {
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("timed out waiting for the shell snapshot"),
            event = events.recv() => match event {
                None => anyhow::bail!("client stopped"),
                Some(ClientEvent::Status(ConnectionStatus::NeedsPairing { reason })) => anyhow::bail!(reason),
                Some(ClientEvent::Shell(item)) => { shell.apply(item); }
                Some(_) => {}
            }
        }
    }
    let projects = shell.snapshot.as_ref().map(|snapshot| snapshot.projects.clone()).unwrap_or_default();
    if args.iter().any(|arg| arg == "--projects") {
        for project in &projects {
            let thread = shell.snapshot.as_ref().and_then(|snapshot| snapshot.threads.iter().find(|thread| thread.project_id == project.id));
            println!("{} {} {} thread={:?}", project.id, project.title, project.workspace_root, thread.map(|thread| &thread.id));
        }
        return Ok(());
    }
    let project = match flag("--project") {
        Some(id) => projects.iter().find(|project| project.id == id).cloned(),
        None => projects.first().cloned(),
    }
    .ok_or_else(|| anyhow::anyhow!("no project"))?;
    let cwd = project.workspace_root.clone();
    println!("project {} ({}) at {cwd}", project.title, project.id);

    let started = Instant::now();
    let root = client.request(LIST_ENTRIES, list_entries_request(&cwd, "")).await.map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let listing = decode_listing(&root, "").map_err(|error| anyhow::anyhow!(error))?;
    let ignored = listing.entries.iter().filter(|entry| entry.ignored).count();
    let folders = listing.entries.iter().filter(|entry| entry.kind == EntryKind::Directory).count();
    println!(
        "root: {} entries ({folders} folders, {ignored} ignored, truncated={}) in {:?}",
        listing.entries.len(),
        listing.truncated,
        started.elapsed()
    );

    let mut tree = FileTree::default();
    let generation = tree.begin_load("", true).unwrap_or_default();
    tree.finish_load("", generation, listing.entries);
    let limit: usize = flag("--walk").and_then(|value| value.parse().ok()).unwrap_or(200);
    walk(&client, &cwd, &mut tree, limit).await;
    let rows = tree.rows();
    let files = rows.iter().filter(|row| row.kind == EntryKind::File).count();
    println!("visible rows after expand all: {} ({files} files)", rows.len());
    for row in rows.iter().take(12) {
        println!("  {}{}{}", "  ".repeat(row.depth), row.label, if row.kind == EntryKind::Directory { "/" } else { "" });
    }

    if let Some(query) = flag("--search") {
        let started = Instant::now();
        match client.request(SEARCH_ENTRIES, search_entries_request(&cwd, &query)).await {
            Ok(value) => {
                let found = decode_search(&value).map_err(|error| anyhow::anyhow!(error))?;
                let shown = tree.search_rows(&query, &found.entries);
                println!(
                    "search {query:?}: {} server matches (truncated={}), {} rows shown, in {:?}",
                    found.entries.len(),
                    found.truncated,
                    shown.len(),
                    started.elapsed()
                );
            }
            Err(error) => println!("search {query:?}: {error}"),
        }
    }

    for path in flags("--read") {
        let started = Instant::now();
        match read(&client, &cwd, &path).await {
            Ok(file) => {
                let lines = LineIndex::new(&file.contents);
                println!(
                    "read {path}: {} bytes of {} (truncated={}), {} lines, widest {} cols, grammar {:?}, in {:?}",
                    file.contents.len(),
                    file.byte_length,
                    file.truncated,
                    lines.len(),
                    lines.max_columns,
                    syntax_language(&path),
                    started.elapsed()
                );
            }
            Err(failure) => println!("read {path}: {} ({:?})", failure.describe(), failure.failure),
        }
    }

    if let Some(path) = flag("--image") {
        let thread_id = flag("--thread").unwrap_or_default();
        let request = image_asset_request(&thread_id, &cwd, &absolute_path(&path, &cwd));
        let started = Instant::now();
        match client.request(CREATE_ASSET_URL, request).await.map_err(|error| error.to_string()).and_then(|value| decode_asset_url(&value)) {
            Ok(url) => match client.asset_bytes(&url, 32 * 1024 * 1024).await {
                Ok(bytes) => println!("image {path}: {} bytes, magic {:02x?}, in {:?}", bytes.len(), &bytes[..bytes.len().min(8)], started.elapsed()),
                Err(error) => println!("image {path}: {error}"),
            },
            Err(error) => println!("image {path}: {error}"),
        }
    }
    Ok(())
}

async fn read(client: &ClientHandle, cwd: &str, path: &str) -> Result<t3_core::files::FileContents, ReadFailure> {
    match client.request(READ_FILE, read_file_request(cwd, path)).await {
        Ok(value) => decode_read_file(value).map_err(ReadFailure::message),
        Err(RpcError::Failure(failure)) => Err(ReadFailure::from_error(&failure.error, &failure.message)),
        Err(RpcError::Closed) => Err(ReadFailure::message("Not connected to the server.")),
    }
}

/// Expand all with the explorer's cap of four listings in flight, up to `limit` folders.
async fn walk(client: &ClientHandle, cwd: &str, tree: &mut FileTree, limit: usize) {
    let started = Instant::now();
    let mut queue: VecDeque<String> = tree.expand_all().into();
    let mut listed = 0;
    let mut entries = 0;
    let mut in_flight = tokio::task::JoinSet::new();
    loop {
        while in_flight.len() < 4 && listed < limit {
            let Some(directory) = queue.pop_front() else { break };
            let Some(generation) = tree.begin_load(&directory, false) else { continue };
            listed += 1;
            let client = client.clone();
            let payload = list_entries_request(cwd, &directory);
            in_flight.spawn(async move {
                let result = client.request(LIST_ENTRIES, payload).await.map_err(|error| error.to_string());
                (directory, generation, result)
            });
        }
        let Some(Ok((directory, generation, result))) = in_flight.join_next().await else { break };
        match result.and_then(|value| decode_listing(&value, &directory)) {
            Ok(listing) => {
                entries += listing.entries.len();
                queue.extend(tree.finish_load(&directory, generation, listing.entries));
            }
            Err(error) => tree.fail_load(&directory, generation, error),
        }
    }
    println!("expand all: listed {listed} folders, {entries} entries, {} still queued, in {:?}", queue.len(), started.elapsed());
}
