//! Cases from `SidebarThreadRow.tsx`'s tooltip, `BranchToolbar.logic.test.ts` and
//! `threadPullRequests.test.ts` (`pullRequestListLines`).

use serde_json::{Value, json};

use super::*;
use crate::server_config::ServerConfig;
use crate::shell::ThreadShell;
use crate::sidebar::thread::present_thread;

fn thread(extra: Value) -> SidebarThread {
    let mut value = json!({
        "id": "t1",
        "projectId": "p1",
        "title": "Thread",
        "createdAt": "2026-01-02T00:00:00Z",
        "updatedAt": "2026-01-02T00:00:00Z",
        "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
        "runtimeMode": "auto",
        "interactionMode": "default",
        "lineage": {"rootThreadId": "t1"},
        "status": "idle",
        "hasActionableProposedPlan": false,
        "itemCount": 0,
        "visibleItemCount": 0
    });
    value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    present_thread(&serde_json::from_value::<ThreadShell>(value).unwrap(), None)
}

fn provider(instance: &str, driver: &str, name: Option<&str>, accent: Option<&str>) -> Value {
    json!({
        "instanceId": instance,
        "driver": driver,
        "displayName": name,
        "accentColor": accent,
        "enabled": true,
        "installed": true,
        "status": "ready",
        "models": [{"slug": "gpt-5", "name": "GPT-5 Codex", "shortName": "GPT-5"}]
    })
}

fn config(providers: Vec<Value>) -> ServerConfig {
    serde_json::from_value(json!({"providers": providers})).unwrap()
}

#[test]
fn model_line_uses_the_trigger_name_and_names_the_instance_only_when_a_badge_tells_it_apart() {
    let single = config(vec![provider("codex", "codex", None, None)]);
    let entries = providers::entries(&single, None);
    let plain = thread(json!({}));
    assert_eq!(model_text(&plain, &entries[0], &entries), "GPT-5");

    let unknown_slug = thread(json!({"modelSelection": {"instanceId": "codex", "model": "o9-preview"}}));
    assert_eq!(model_text(&unknown_slug, &entries[0], &entries), "o9-preview");

    let twins = config(vec![provider("codex", "codex", None, None), provider("codex_work", "codex", Some("Work"), None)]);
    let entries = providers::entries(&twins, None);
    let work = thread(json!({"providerInstanceId": "codex_work", "modelSelection": {"instanceId": "codex_work", "model": "gpt-5"}}));
    let entry = entries.iter().find(|entry| entry.instance_id() == model_instance_id(&work)).unwrap();
    assert_eq!(model_text(&work, entry, &entries), "GPT-5 · Work");

    let accented = config(vec![provider("codex", "codex", Some("Personal"), Some("#ff8800"))]);
    let entries = providers::entries(&accented, None);
    assert_eq!(model_text(&plain, &entries[0], &entries), "GPT-5 · Personal");
}

#[test]
fn runtime_instance_wins_and_earlier_instances_read_as_handoffs_by_name() {
    let twins = config(vec![provider("codex", "codex", None, None), provider("claude", "claudeAgent", None, None)]);
    let entries = providers::entries(&twins, None);
    let handed = thread(json!({
        "providerInstanceId": "claude",
        "providerInstanceHistory": ["codex", "retired_instance", "claude"],
        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
        "latestRunId": "run-1"
    }));
    assert_eq!(model_instance_id(&handed), "claude");
    assert_eq!(handed_off_from(&handed, &entries).as_deref(), Some("Handed off from Codex, retired_instance"));
    assert_eq!(handed_off_from(&thread(json!({"providerInstanceHistory": ["codex"]})), &entries), None);
}

#[test]
fn checkout_warning_applies_only_to_local_threads_on_another_branch() {
    let local = thread(json!({"branch": "feature/a"}));
    assert_eq!(checkout_to_compare(&local, Some("/repo")), Some("/repo"));
    assert!(branch_mismatch(&local, Some("main")));
    assert!(!branch_mismatch(&local, Some("feature/a")));
    assert!(!branch_mismatch(&local, None));

    let worktree = thread(json!({"branch": "feature/a", "worktreePath": "/repo/.t3/wt"}));
    assert_eq!(checkout_to_compare(&worktree, Some("/repo")), None);
    assert!(!branch_mismatch(&worktree, Some("main")));

    let no_branch = thread(json!({}));
    assert_eq!(checkout_to_compare(&no_branch, Some("/repo")), None);
    assert!(!branch_mismatch(&no_branch, Some("main")));
}

#[test]
fn terminal_label_counts_processes() {
    assert_eq!(terminal_process_label(1), "1 terminal process running");
    assert_eq!(terminal_process_label(3), "3 terminal processes running");
}

fn link(number: u64, head: &str, base: &str, updated: &str, source: &str) -> Value {
    json!({
        "host": "github.com",
        "repository": "acme/app",
        "number": number,
        "url": format!("https://github.com/acme/app/pull/{number}"),
        "source": source,
        "linkedAt": "2026-01-01T00:00:00Z",
        "snapshot": {"state": "open", "title": format!("PR {number}"), "headBranch": head, "baseBranch": base, "isDraft": false, "updatedAt": updated}
    })
}

#[test]
fn pull_request_lines_put_the_newest_chain_first_and_label_its_size_once() {
    let links: Vec<ThreadLink> = serde_json::from_value(json!([
        link(1, "solo", "main", "2026-01-01T00:00:00Z", "manual"),
        link(2, "base", "main", "2026-01-02T00:00:00Z", "manual"),
        link(3, "top", "base", "2026-01-05T00:00:00Z", "agent"),
        link(4, "gone", "main", "2026-01-09T00:00:00Z", "stack-dismissed"),
    ]))
    .unwrap();
    let lines = pull_request_list_lines(&links);
    let shape: Vec<(u64, usize, Option<String>)> = lines.iter().map(|line| (line.link.number, line.depth, line.stack_label())).collect();
    assert_eq!(shape, vec![(2, 0, Some("chain · 2".into())), (3, 1, None), (1, 0, None)]);
}
