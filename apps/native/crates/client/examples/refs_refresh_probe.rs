//! Read-only branch refresh evidence against the factory's smoke environment.
use t3_client::{
    store,
    supervisor::{ClientEvent, start},
};
use t3_core::{
    shell::ShellState,
    vcs::{self, refs::RefCache},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved smoke connection"))?;
    anyhow::ensure!(credentials.http_base == "http://127.0.0.1:14658", "use the smoke environment on 14658");
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut shell = ShellState::default();
    while !shell.synchronized {
        let event = events.recv().await.ok_or_else(|| anyhow::anyhow!("connection closed"))?;
        if let ClientEvent::Shell(item) = event {
            shell.apply(item);
        }
    }
    let project = shell
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.projects.first())
        .ok_or_else(|| anyhow::anyhow!("no smoke project"))?;
    let input = vcs::list_refs(&project.workspace_root, None, None, vcs::REF_PAGE_LIMIT);
    let mut cache = RefCache::default();
    cache.settle(vcs::method::VCS_REFRESH_STATUS);
    let request = cache.begin(input);
    println!("vcs.listRefs request: {}", request.input);
    let result = client.request(vcs::method::VCS_LIST_REFS, request.input.clone()).await?;
    let result: vcs::ListRefsResult = serde_json::from_value(result)?;
    anyhow::ensure!(cache.commit(&request), "refresh was superseded");
    println!(
        "reply: isRepo={} refs={} total={} nextCursor={:?}",
        result.is_repo,
        result.refs.len(),
        result.total_count,
        result.next_cursor
    );
    Ok(())
}
