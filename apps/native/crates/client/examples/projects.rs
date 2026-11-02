//! Headless check of the Add project flows: `filesystem.browse`, then `projects.mutate`
//! create, pin, unpin and delete on a scratch folder, waiting for each in the shell.
//!
//!   cargo run -p t3-client --example projects -- --dir <absolute folder to add>

use std::time::Duration;

use t3_client::store;
use t3_client::supervisor::{ClientEvent, start};
use t3_core::projects::{BrowseResult, FILESYSTEM_BROWSE, PROJECTS_MUTATE, browse_request, create_project, filter_browse_entries, infer_project_title, set_project_pinned};
use t3_core::shell::{ProjectShell, ShellState};
use tokio::sync::mpsc::Receiver;

async fn wait_for(events: &mut Receiver<ClientEvent>, shell: &mut ShellState, done: impl Fn(&ShellState) -> bool) -> bool {
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    loop {
        if done(shell) {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.iter().position(|a| a == "--dir").and_then(|i| args.get(i + 1)).cloned().ok_or_else(|| anyhow::anyhow!("pass --dir"))?;
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection"))?;
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    wait_for(&mut events, &mut shell, |shell| shell.synchronized).await;

    let parent = format!("{}/", dir.trim_end_matches('/').rsplit_once('/').map_or("/", |(parent, _)| parent));
    let browse: BrowseResult = serde_json::from_value(client.request(FILESYSTEM_BROWSE, browse_request(&parent, None)).await?)?;
    let leaf = infer_project_title(&dir);
    let visible = filter_browse_entries(&browse.entries, &leaf);
    println!("filesystem.browse {parent}: {} entries, \"{leaf}\" matches {:?}", browse.entries.len(), visible.iter().map(|e| &e.name).collect::<Vec<_>>());

    let id = || uuid::Uuid::new_v4().to_string();
    let project_id = id();
    client.request(PROJECTS_MUTATE, create_project(&id(), &project_id, &leaf, &dir)).await?;
    let listed = |pinned: Option<bool>| {
        let project_id = project_id.clone();
        move |shell: &ShellState| shell.project(&project_id).is_some_and(|p: &ProjectShell| pinned.is_none_or(|pinned| p.pinned_at.is_some() == pinned))
    };
    println!("project.create: listed={}", wait_for(&mut events, &mut shell, listed(None)).await);
    client.request(PROJECTS_MUTATE, set_project_pinned(&id(), &project_id, true)).await?;
    println!("project.update pinned=true: pinnedAt={}", wait_for(&mut events, &mut shell, listed(Some(true))).await);
    client.request(PROJECTS_MUTATE, set_project_pinned(&id(), &project_id, false)).await?;
    println!("project.update pinned=false: pinnedAt cleared={}", wait_for(&mut events, &mut shell, listed(Some(false))).await);
    client.request(PROJECTS_MUTATE, serde_json::json!({ "type": "project.delete", "commandId": id(), "projectId": project_id })).await?;
    let gone = { let project_id = project_id.clone(); move |shell: &ShellState| shell.project(&project_id).is_none() };
    println!("project.delete: removed={}", wait_for(&mut events, &mut shell, gone).await);
    Ok(())
}
