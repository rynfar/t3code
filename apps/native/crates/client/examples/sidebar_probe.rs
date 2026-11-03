//! Smoke-only sidebar fixtures. `--setup`, `--hide`, `--show`, then `--cleanup`.
//! Uses Replay (dev) threads without starting any provider turn; restores only the settings it changes.

use serde_json::{Value, json};
use t3_client::{
    store,
    supervisor::{ClientEvent, start},
};
use t3_core::{commands::method, projects, server_config::ServerConfigState, shell::ShellState};

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("set T3_NATIVE_HOME to the smoke home"))?;
    anyhow::ensure!(credentials.http_base == "http://127.0.0.1:14658", "this probe only writes to the factory smoke server");
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    let (mut shell, mut config) = (ShellState::default(), ServerConfigState::default());
    while !shell.synchronized || config.config.is_none() {
        match tokio::time::timeout(std::time::Duration::from_secs(15), events.recv()).await? {
            Some(ClientEvent::Shell(item)) => {
                shell.apply(item);
            }
            Some(ClientEvent::ServerConfig(item)) => {
                config.apply(*item);
            }
            Some(_) => {}
            None => anyhow::bail!("disconnected"),
        }
    }
    let config = config.config.unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/sidebar-94-fixture.json");
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--setup") {
        anyhow::ensure!(!path.exists(), "clean up the previous fixture first");
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.display_name.as_deref() == Some("Replay (dev)") || provider.instance_id.contains("replay"))
            .ok_or_else(|| anyhow::anyhow!("Replay (dev) is unavailable"))?;
        let project = id();
        let threads = [id(), id()];
        let root = path.parent().unwrap().join("sidebar-94-workspace");
        std::fs::create_dir_all(&root)?;
        let fixture = json!({"project":project,"threads":threads,"original":{"hiddenThreadTags":config.settings.hidden_thread_tags,"hideTaggedThreads":config.settings.hide_tagged_threads}});
        std::fs::write(&path, serde_json::to_vec_pretty(&fixture)?)?;
        client.request(projects::PROJECTS_MUTATE, projects::create_project(&id(), &project, "Sidebar parity #94", &root.to_string_lossy())).await?;
        for (thread, title) in threads.iter().zip(["Native #94 tagged thread", "Native #94 visible thread"]) {
            client.dispatch(json!({"type":"thread.create","commandId":id(),"threadId":thread,"projectId":project,"title":title,
                "createdBy":"user","creationSource":"web","modelSelection":{"instanceId":provider.instance_id,"model":provider.models.first().map(|model|model.slug.as_str()).unwrap_or("replay")},
                "runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null})).await?;
        }
        client.dispatch(json!({"type":"thread.tag.set","commandId":id(),"threadId":threads[0],"tag":"native-94-private","tagged":true})).await?;
        println!("created two Replay (dev) threads; tagged one native-94-private; no turns started\nproject={project}\nvisible={}", threads[1]);
    } else {
        let fixture: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        if args.iter().any(|arg| arg == "--hide") {
            client.request(method::SERVER_UPDATE_SETTINGS, json!({"patch":{"hiddenThreadTags":["native-94-private"],"hideTaggedThreads":true}})).await?;
            println!("persisted tag filter enabled");
        } else {
            client.request(method::SERVER_UPDATE_SETTINGS, json!({"patch":fixture["original"]})).await?;
            println!("original tag settings restored");
            if args.iter().any(|arg| arg == "--cleanup") {
                for thread in fixture["threads"].as_array().unwrap() {
                    client.dispatch(json!({"type":"thread.delete","commandId":id(),"threadId":thread})).await?;
                }
                client.request(projects::PROJECTS_MUTATE, json!({"type":"project.delete","commandId":id(),"projectId":fixture["project"]})).await?;
                std::fs::remove_file(path)?;
                println!("removed both test threads and their project");
            }
        }
    }
    Ok(())
}
