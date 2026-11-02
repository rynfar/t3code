//! Headless check of Settings → Projects against a live server: the writes the page sends, each
//! waited for in the shell or the config stream, then undone. It adds a scratch project at
//! `--dir` (a folder it writes a `t3.json` into), renames, pins, archives and resets its icon,
//! sets and clears its New threads overrides, imports, adds, rebinds, deletes and resets its
//! actions, and removes it. Point `T3_NATIVE_HOME` at an isolated dev server's home.
//!
//!   T3_NATIVE_HOME=<dev home>/native cargo run -p t3-client --example project_settings_probe -- --allow-write --dir <scratch folder>
//!
//! `--keep` stops once the project has two actions and a shortcut, for screenshots; `--remove
//! <project id>` then resets its actions and removes it.

use std::time::Duration;

use serde_json::json;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, start};
use t3_core::commands::method;
use t3_core::keybindings::decode_resolved_keybindings;
use t3_core::project_scripts::command_for_project_script;
use t3_core::projects::{PROJECTS_MUTATE, create_project};
use t3_core::server_config::{ServerConfig, ServerConfigState};
use t3_core::settings::project_actions::{Member, ScriptInput, ScriptsEdit, importable, keybinding_value_for_command, plan_writes, project_file_from_read, shown_scripts, submit_id};
use t3_core::settings::projects::{OverrideEdit, ProjectIcon, ProjectUpdate, RemovalMember, confirm_copy, delete_project, icon_description, override_patch, plan_removal};
use t3_core::shell::{ProjectShell, ShellState};
use t3_core::workspace;
use tokio::sync::mpsc::Receiver;

struct Live {
    events: Receiver<ClientEvent>,
    shell: ShellState,
    config: ServerConfigState,
}

impl Live {
    async fn until(&mut self, what: &str, done: impl Fn(&ShellState, Option<&ServerConfig>) -> bool) -> anyhow::Result<()> {
        let deadline = tokio::time::sleep(Duration::from_secs(10));
        tokio::pin!(deadline);
        loop {
            if done(&self.shell, self.config.config.as_ref()) {
                println!("  ok   {what}");
                return Ok(());
            }
            tokio::select! {
                _ = &mut deadline => anyhow::bail!("timed out waiting for: {what}"),
                event = self.events.recv() => match event {
                    Some(ClientEvent::Shell(item)) => { self.shell.apply(item); }
                    Some(ClientEvent::ServerConfig(event)) => { self.config.apply(*event); }
                    Some(_) => {}
                    None => anyhow::bail!("client stopped"),
                }
            }
        }
    }

    fn config(&self) -> &ServerConfig {
        self.config.config.as_ref().expect("config")
    }

    fn project(&self, id: &str) -> Option<&ProjectShell> {
        self.shell.project(id)
    }
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn member(project: &ProjectShell) -> Member {
    Member { id: project.id.clone(), scripts: project.scripts.iter().filter_map(|script| serde_json::from_value(script.clone()).ok()).collect() }
}

/// Sends an actions save as the page does, every write in order.
async fn save_actions(client: &ClientHandle, live: &Live, project_id: &str, edit: ScriptsEdit) -> anyhow::Result<()> {
    let config = live.config();
    let projects: Vec<Member> = live.shell.snapshot.as_ref().map(|snapshot| snapshot.projects.iter().map(member).collect()).unwrap_or_default();
    let members = vec![member(live.project(project_id).expect("project listed"))];
    let writes = plan_writes(&config.settings, &decode_resolved_keybindings(&config.keybindings), &projects, &members, &edit);
    for write in writes {
        println!("  send {} {}", write.method(), write.payload());
        client.request(write.method(), write.payload().clone()).await?;
    }
    Ok(())
}

async fn add_action(client: &ClientHandle, live: &Live, project_id: &str, input: ScriptInput) -> anyhow::Result<String> {
    let projects: Vec<Member> = live.shell.snapshot.as_ref().map(|snapshot| snapshot.projects.iter().map(member).collect()).unwrap_or_default();
    let members = vec![member(live.project(project_id).expect("project listed"))];
    let script_id = submit_id(&live.config().settings, &projects, &members, None, &input.name, 0);
    save_actions(client, live, project_id, ScriptsEdit::Save { id: script_id.clone(), input, is_new: true }).await?;
    Ok(script_id)
}

fn shown_ids(config: Option<&ServerConfig>, project_id: &str) -> Vec<String> {
    config.map(|config| shown_scripts(&config.settings, project_id).iter().map(|script| script.id.clone()).collect()).unwrap_or_default()
}

fn binding(config: Option<&ServerConfig>, script_id: &str) -> Option<String> {
    let config = config?;
    keybinding_value_for_command(&decode_resolved_keybindings(&config.keybindings), command_for_project_script(script_id).as_deref())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(args.iter().any(|arg| arg == "--allow-write"), "pass --allow-write against an isolated dev server");
    let dir = args.iter().position(|arg| arg == "--dir").and_then(|index| args.get(index + 1)).cloned().ok_or_else(|| anyhow::anyhow!("pass --dir"))?;
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection under T3_NATIVE_HOME"))?;
    println!("server {}", credentials.http_base);
    let (client, events) = start(&tokio::runtime::Handle::current(), credentials);
    let mut live = Live { events, shell: ShellState::default(), config: ServerConfigState::default() };
    live.until("shell and config loaded", |shell, config| shell.synchronized && config.is_some()).await?;
    if let Some(project_id) = args.iter().position(|arg| arg == "--remove").and_then(|index| args.get(index + 1)) {
        let removed = shown_ids(live.config.config.as_ref(), project_id);
        save_actions(&client, &live, project_id, ScriptsEdit::Reset).await?;
        live.until("reset the override", |_, config| config.is_some_and(|config| !config.settings.project_settings_overrides.contains_key(project_id))).await?;
        live.until("shortcuts released", |_, config| removed.iter().all(|script_id| binding(config, script_id).is_none())).await?;
        client.request(PROJECTS_MUTATE, delete_project(&id(), project_id)).await?;
        live.until("project.delete removed it", |shell, _| shell.project(project_id).is_none()).await?;
        return Ok(());
    }
    let keybindings_before = live.config().keybindings.clone();
    std::fs::write(
        format!("{dir}/t3.json"),
        r#"{ // the page imports these
  "defaultThreadEnvMode": "worktree",
  "scripts": [{ "name": "Lint", "command": "echo lint", "icon": "lint" },],
}"#,
    )?;

    let project_id = id();
    client.request(PROJECTS_MUTATE, create_project(&id(), &project_id, "settings-probe", &dir)).await?;
    live.until("project.create listed", |shell, _| shell.project(&project_id).is_some()).await?;
    let pid = project_id.clone();
    let update = |update: ProjectUpdate| client.request(PROJECTS_MUTATE, update.payload(&id(), &pid));

    println!("Name");
    update(ProjectUpdate { title: Some("Settings probe".into()), ..Default::default() }).await?;
    live.until("renamed to \"Settings probe\"", |shell, _| shell.project(&project_id).is_some_and(|p| p.title == "Settings probe")).await?;

    println!("Pin and archive");
    for (pinned, archived) in [(Some(true), None), (Some(false), None), (None, Some(true)), (None, Some(false))] {
        update(ProjectUpdate { pinned, archived, ..Default::default() }).await?;
        let what = format!("pinned={pinned:?} archived={archived:?}");
        live.until(&what, |shell, _| {
            shell.project(&project_id).is_some_and(|p| pinned.is_none_or(|on| p.pinned_at.is_some() == on) && archived.is_none_or(|on| p.archived_at.is_some() == on))
        })
        .await?;
    }

    println!("Project icon");
    update(ProjectUpdate { favicon_path: Some(Some("logo.png".into())), project_icon: Some(None), ..Default::default() }).await?;
    live.until("faviconPath set", |shell, _| shell.project(&project_id).is_some_and(|p| p.favicon_path.as_deref() == Some("logo.png"))).await?;
    let shown = live.project(&project_id).expect("listed");
    println!("  description: {}", icon_description(ProjectIcon::decode(shown.project_icon.as_ref()).as_ref(), shown.favicon_path.as_deref()));
    update(ProjectUpdate::default_icon(false)).await?;
    live.until("icon reset to Automatic", |shell, _| shell.project(&project_id).is_some_and(|p| p.favicon_path.is_none() && p.project_icon.is_none())).await?;

    println!("New threads overrides");
    let only = [Member { id: project_id.clone(), scripts: Vec::new() }];
    let set = override_patch(&live.config().settings, &only, &OverrideEdit::Set { key: "defaultThreadEnvMode".into(), value: json!("worktree") });
    println!("  send {} {set}", method::SERVER_UPDATE_SETTINGS);
    client.request(method::SERVER_UPDATE_SETTINGS, set).await?;
    live.until("workspace override = worktree", |_, config| {
        config.is_some_and(|config| workspace::configured_env_mode(&config.settings, &project_id).is_some_and(|(_, source)| source == workspace::Source::Project))
    })
    .await?;
    let clear = override_patch(&live.config().settings, &only, &OverrideEdit::Clear { keys: vec!["defaultThreadEnvMode".into()] });
    println!("  send {} {clear}", method::SERVER_UPDATE_SETTINGS);
    client.request(method::SERVER_UPDATE_SETTINGS, clear).await?;
    live.until("workspace override cleared", |_, config| config.is_some_and(|config| !config.settings.project_settings_overrides.contains_key(&project_id))).await?;

    println!("Actions");
    let file = project_file_from_read(&client.request(method::PROJECTS_READ_FILE, workspace::project_file_request(&dir)).await?);
    let scripts = shown_scripts(&live.config().settings, &project_id).to_vec();
    let import = importable(file.scripts(), &scripts).first().map(|file| file.import_input()).ok_or_else(|| anyhow::anyhow!("t3.json offers nothing to import: {file:?}"))?;
    let lint = add_action(&client, &live, &project_id, import).await?;
    live.until("imported Lint from t3.json", |_, config| shown_ids(config, &project_id).contains(&lint)).await?;
    let dev_input = ScriptInput { name: "Dev".into(), command: "echo dev".into(), keybinding: Some(Some("mod+shift+9".into())), ..Default::default() };
    let dev = add_action(&client, &live, &project_id, dev_input.clone()).await?;
    live.until("added Dev with mod+shift+9", |_, config| shown_ids(config, &project_id).contains(&dev) && binding(config, &dev).as_deref() == Some("mod+shift+9")).await?;
    if args.iter().any(|arg| arg == "--keep") {
        println!("kept project {project_id}");
        return Ok(());
    }
    let rebind = ScriptInput { keybinding: Some(Some("mod+shift+8".into())), ..dev_input };
    save_actions(&client, &live, &project_id, ScriptsEdit::Save { id: dev.clone(), input: rebind, is_new: false }).await?;
    live.until("rebound Dev to mod+shift+8", |_, config| binding(config, &dev).as_deref() == Some("mod+shift+8")).await?;
    save_actions(&client, &live, &project_id, ScriptsEdit::Delete { id: dev.clone() }).await?;
    live.until("deleted Dev and its shortcut", |_, config| !shown_ids(config, &project_id).contains(&dev) && binding(config, &dev).is_none()).await?;
    save_actions(&client, &live, &project_id, ScriptsEdit::Reset).await?;
    live.until("reset the override", |_, config| config.is_some_and(|config| !config.settings.project_settings_overrides.contains_key(&project_id))).await?;
    live.until("keybindings back as they were", |_, config| config.is_some_and(|config| config.keybindings == keybindings_before)).await?;

    println!("Remove");
    let shown = live.project(&project_id).expect("listed").clone();
    let plan = plan_removal(&[RemovalMember { title: &shown.title, workspace_root: &shown.workspace_root, environment_label: Some("smoke") }], 1, false, &shown.title, 0);
    let (title, description) = confirm_copy(&plan.confirmation);
    println!("  confirm: {title}\n           {}", description.unwrap_or_default().replace('\n', "\n           "));
    client.request(PROJECTS_MUTATE, delete_project(&id(), &project_id)).await?;
    live.until("project.delete removed it", |shell, _| shell.project(&project_id).is_none()).await?;
    std::fs::remove_file(format!("{dir}/t3.json"))?;
    println!("done: every write undone");
    Ok(())
}
