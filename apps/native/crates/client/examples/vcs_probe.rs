//! Headless check of the Git workflow against a live server: follows a project's live status
//! (`subscribeVcsStatus`), refreshes it, lists refs, then creates a throwaway branch, switches
//! back to the original branch, and prints what the status stream reports after each step along
//! with the quick action the thread details card would offer. Use a scratch repository; the
//! throwaway branch stays behind for `git branch -D`.
//!
//!   cargo run -p t3-client --example vcs_probe -- [--project <title> | --add-project <dir>] [--branch <name>]
//!
//! `--add-project` registers the folder as a project for the run and removes it afterwards.
//! `--commit <message>` also commits the working tree with `git.runStackedAction`, printing its
//! progress until the server ends the stream, before the branch steps.

use std::time::Duration;

use t3_client::session::StreamHandle;
use t3_client::store;
use t3_client::supervisor::{ClientEvent, ClientHandle, start};
use t3_core::git_actions::{self, ActionProgress, Operation, ProgressEvent, RunRequest, StackedAction, StackedActionRun, build_menu_items, resolve_quick_action, terminology};
use t3_core::shell::ShellState;
use t3_core::vcs::{self, ListRefsResult, RefNameResult, VcsStatus, VcsStatusEvent, apply_status_event, changes_totals, method, ref_badge};

fn describe(status: &VcsStatus) -> String {
    let (insertions, deletions) = changes_totals(status);
    let quick = resolve_quick_action(Some(status), false, status.local.is_default_ref, status.local.has_primary_remote);
    let menu: Vec<String> = build_menu_items(Some(status), false, status.local.has_primary_remote)
        .into_iter()
        .map(|item| format!("{}{}", item.label, if item.disabled { " (disabled)" } else { "" }))
        .collect();
    format!(
        "branch={} default={} dirty={} files={} changes=+{insertions}/-{deletions} upstream={} ahead={} behind={} | quick action: {}{} | menu: {}",
        status.local.ref_name.as_deref().unwrap_or("(detached)"),
        status.local.is_default_ref,
        status.local.has_working_tree_changes,
        status.local.working_tree.files.len(),
        status.remote.has_upstream,
        status.remote.ahead_count,
        status.remote.behind_count,
        quick.label,
        quick.disabled_reason().map(|reason| format!(" ({reason})")).unwrap_or_default(),
        menu.join(", "),
    )
}

/// Applies status events until `done` holds, or times out.
async fn wait_status(stream: &mut StreamHandle, status: &mut Option<VcsStatus>, done: impl Fn(&VcsStatus) -> bool) -> anyhow::Result<()> {
    let deadline = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(deadline);
    loop {
        if status.as_ref().is_some_and(&done) {
            return Ok(());
        }
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("timed out waiting for the status stream"),
            item = stream.receiver.recv() => {
                let value = item.ok_or_else(|| anyhow::anyhow!("status stream ended"))?.map_err(|error| anyhow::anyhow!("{error}"))?;
                let event: VcsStatusEvent = serde_json::from_value(value)?;
                let tag = match &event {
                    VcsStatusEvent::Snapshot { .. } => "snapshot",
                    VcsStatusEvent::LocalUpdated { .. } => "localUpdated",
                    VcsStatusEvent::RemoteUpdated { .. } => "remoteUpdated",
                    VcsStatusEvent::Unknown => "unknown",
                };
                *status = apply_status_event(status.take(), event);
                if let Some(current) = status.as_ref() {
                    println!("  status {tag}: {}", describe(current));
                }
            }
        }
    }
}

async fn request<T: serde::de::DeserializeOwned>(client: &ClientHandle, rpc: &'static str, payload: serde_json::Value) -> anyhow::Result<T> {
    let value = client.request(rpc, payload).await.map_err(|error| anyhow::anyhow!("{rpc}: {error}"))?;
    Ok(serde_json::from_value(value)?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no saved connection; pair with the probe first"))?;
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
    let added = flag("--add-project");
    if let Some(dir) = added.as_deref() {
        let title = dir.trim_end_matches('/').rsplit('/').next().unwrap_or(dir).to_owned();
        let command = serde_json::json!({ "type": "project.create", "commandId": uuid::Uuid::new_v4().to_string(), "projectId": uuid::Uuid::new_v4().to_string(), "title": title, "workspaceRoot": dir });
        client.request("projects.mutate", command).await.map_err(|error| anyhow::anyhow!("projects.mutate: {error}"))?;
        while !shell.snapshot.as_ref().is_some_and(|snapshot| snapshot.projects.iter().any(|project| project.workspace_root == dir)) {
            if let Some(ClientEvent::Shell(item)) = events.recv().await {
                shell.apply(item);
            }
        }
    }
    let wanted = flag("--project");
    let project = shell
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.projects.iter().find(|project| match (added.as_deref(), wanted.as_ref()) {
            (Some(dir), _) => project.workspace_root == dir,
            (None, Some(title)) => &project.title == title,
            (None, None) => project.archived_at.is_none(),
        }))
        .ok_or_else(|| anyhow::anyhow!("no project"))?
        .clone();
    let cwd = project.workspace_root.clone();
    println!("project {:?} at {cwd}", project.title);

    let mut stream = client.stream(method::SUBSCRIBE_VCS_STATUS, vcs::status_subscription(&cwd)).await.map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut status = None;
    println!("subscribeVcsStatus:");
    wait_status(&mut stream, &mut status, |_| true).await?;
    let original = status.as_ref().and_then(|status| status.local.ref_name.clone()).ok_or_else(|| anyhow::anyhow!("detached HEAD"))?;

    let refreshed: VcsStatus = request(&client, method::VCS_REFRESH_STATUS, vcs::refresh_status(&cwd, true)).await?;
    println!("vcs.refreshStatus (localOnly): {}", describe(&refreshed));

    let refs: ListRefsResult = request(&client, method::VCS_LIST_REFS, vcs::list_refs(&cwd, None, None, vcs::REF_PAGE_LIMIT)).await?;
    println!("vcs.listRefs: {} of {} refs, next cursor {:?}", refs.refs.len(), refs.total_count, refs.next_cursor);
    for item in refs.refs.iter().take(8) {
        println!("  {}{}", item.name, ref_badge(item, Some(&cwd)).map(|badge| format!("  [{badge}]")).unwrap_or_default());
    }

    if let Some(message) = flag("--commit") {
        let action_id = uuid::Uuid::new_v4().to_string();
        let request = RunRequest { commit_message: Some(&message), ..RunRequest::default() };
        let payload = git_actions::run_stacked_action(&action_id, &cwd, StackedAction::Commit, &request);
        let mut progress = ActionProgress::begin(Operation::RunChangeRequest, &action_id, 0);
        let mut run = client.stream(method::GIT_RUN_STACKED_ACTION, payload).await.map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut outcome = StackedActionRun::new(&action_id, StackedAction::Commit);
        let mut stream_error = None;
        println!("git.runStackedAction commit:");
        // Read to the end, as the app does: the server links the PR and refreshes status after
        // `action_finished`, and dropping the stream would interrupt that.
        while let Some(item) = run.receiver.recv().await {
            let event: ProgressEvent = match item {
                Ok(value) => serde_json::from_value(value)?,
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            progress.apply(&event, 0);
            outcome.observe(&event);
            match &event {
                ProgressEvent::ActionFinished { .. } => println!("  action_finished; waiting for the server to end the stream"),
                other => println!("  {:?} -> row shows {:?}", other, progress.presentation().status),
            }
        }
        let result = outcome.settle(stream_error).map_err(|message| anyhow::anyhow!(message))?;
        println!("  stream ended: {:?}", result.summary(terminology(status.as_ref().and_then(VcsStatus::provider_kind))));
        wait_status(&mut stream, &mut status, |status| !status.local.has_working_tree_changes).await?;
    }

    let branch = flag("--branch").unwrap_or_else(|| format!("w8-probe-{}", uuid::Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>()));
    let created: RefNameResult = request(&client, method::VCS_CREATE_REF, vcs::create_ref(&cwd, &vcs::sanitize_new_ref_name(&branch))).await?;
    println!("vcs.createRef {branch:?} (switchRef): refName={:?}", created.ref_name);
    wait_status(&mut stream, &mut status, |status| status.local.ref_name.as_deref() == Some(branch.as_str())).await?;

    let refs: ListRefsResult = request(&client, method::VCS_LIST_REFS, vcs::list_refs(&cwd, Some(&branch), None, 10)).await?;
    println!("vcs.listRefs query={branch:?}: {:?}", refs.refs.iter().map(|item| (item.name.as_str(), ref_badge(item, Some(&cwd)))).collect::<Vec<_>>());

    let switched: RefNameResult = request(&client, method::VCS_SWITCH_REF, vcs::switch_ref(&cwd, &original)).await?;
    println!("vcs.switchRef {original:?}: refName={:?}", switched.ref_name);
    wait_status(&mut stream, &mut status, |status| status.local.ref_name.as_deref() == Some(original.as_str())).await?;
    if added.is_some() {
        let command = serde_json::json!({ "type": "project.delete", "commandId": uuid::Uuid::new_v4().to_string(), "projectId": project.id });
        client.request("projects.mutate", command).await.map_err(|error| anyhow::anyhow!("projects.mutate: {error}"))?;
        println!("removed the project");
    }
    println!("done; delete the throwaway branch with: git -C {cwd} branch -D {branch}");
    Ok(())
}
