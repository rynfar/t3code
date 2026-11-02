//! Headless check of project favicons: lists the shell's projects, then mints and downloads each
//! one's icon and puts it through the in-memory cache.
//!
//!   cargo run -p t3-client --example favicon_probe

use std::time::Duration;

use t3_client::favicon::fetch_project_favicon;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::shell::ShellState;
use t3_core::sidebar::favicon::{FaviconCache, Lookup, project_favicon_resource_key};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    while !shell.synchronized {
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("the shell did not synchronize within 10s"),
            event = events.recv() => match event {
                Some(ClientEvent::Shell(item)) => { shell.apply(item); }
                Some(_) => {}
                None => anyhow::bail!("the client stopped"),
            }
        }
    }

    let projects = shell.snapshot.map(|snapshot| snapshot.projects).unwrap_or_default();
    println!("{} projects on {}", projects.len(), client.environment());
    let mut cache = FaviconCache::default();
    for project in &projects {
        let cwd = &project.workspace_root;
        let key = project_favicon_resource_key(client.environment(), cwd, None);
        match fetch_project_favicon(&client, cwd).await {
            Ok(Some(icon)) => {
                println!("{cwd} -> {:?}, {} bytes, revision {} ({:?})", icon.format, icon.bytes.len(), icon.revision, icon.content_type);
                cache.insert(key.clone(), icon.revision.clone(), icon.bytes, icon.format);
                assert!(matches!(cache.lookup(&key, &icon.revision), Lookup::Hit(_)));
            }
            Ok(None) => {
                cache.remove(&key);
                println!("{cwd} -> none");
            }
            Err(error) => println!("{cwd} -> error: {error}"),
        }
    }
    println!("cache: {} entries, {} bytes", cache.len(), cache.total_bytes());
    Ok(())
}
