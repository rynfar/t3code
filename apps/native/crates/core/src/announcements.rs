//! Ports `web/src/lib/announce.ts`, `chat/threadActivityAnnouncement.ts`, and
//! `components/backgroundThreadAnnouncement.ts`. Time is supplied by the host.
use crate::{shell::ThreadShell, sidebar::thread::SubagentRequest, thread::ThreadProjection};
use std::collections::HashMap;
pub const FOCUS_SETTLE_MS: u64 = 250;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Priority {
    #[default]
    Polite,
    Assertive,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Announcement {
    pub message: String,
    pub priority: Priority,
}
struct Pending {
    announcement: Announcement,
    source: Option<&'static str>,
}
pub struct Announcer<F = ()> {
    pending: Vec<Pending>,
    due: Option<u64>,
    focus: Option<F>,
}
impl<F> Default for Announcer<F> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            due: None,
            focus: None,
        }
    }
}
impl<F: PartialEq> Announcer<F> {
    pub fn announce(
        &mut self,
        message: String,
        source: Option<&'static str>,
        priority: Priority,
        now: u64,
    ) {
        if let Some(source) = source {
            self.cancel(source);
        }
        if self
            .pending
            .last()
            .is_none_or(|entry| entry.announcement.message != message)
        {
            self.pending.push(Pending {
                announcement: Announcement { message, priority },
                source,
            });
        }
        self.due = Some(now.saturating_add(FOCUS_SETTLE_MS));
    }
    pub fn cancel(&mut self, source: &str) {
        self.pending.retain(|entry| entry.source != Some(source));
    }
    pub fn focus_changed(&mut self, focus: Option<F>, now: u64) -> bool {
        if self.focus == focus {
            return false;
        }
        self.focus = focus;
        if self.pending.is_empty() {
            return false;
        }
        self.due = Some(now.saturating_add(FOCUS_SETTLE_MS));
        true
    }
    pub fn due(&self) -> Option<u64> {
        self.due.filter(|_| !self.pending.is_empty())
    }
    pub fn flush(&mut self, now: u64) -> Option<Announcement> {
        if now < self.due()? {
            return None;
        }
        let priority = if self
            .pending
            .iter()
            .any(|entry| entry.announcement.priority == Priority::Assertive)
        {
            Priority::Assertive
        } else {
            Priority::Polite
        };
        let message = self
            .pending
            .drain(..)
            .map(|entry| entry.announcement.message)
            .collect::<Vec<_>>()
            .join(". ");
        self.due = None;
        Some(Announcement { message, priority })
    }
}
pub fn outcome(status: &str) -> Option<&'static str> {
    match status {
        "completed" => Some("Done"),
        "failed" => Some("Failed"),
        "interrupted" | "cancelled" => Some("Stopped"),
        _ => None,
    }
}
pub fn detail_behind(detail: Option<(&str, &str)>, shell: Option<(&str, &str)>) -> bool {
    shell.is_some_and(|shell| detail != Some(shell))
}
pub fn latest_executed_run(runs: &[crate::thread::Run]) -> Option<&crate::thread::Run> {
    runs.iter()
        .filter(|r| r.status != "queued" && !(r.status == "cancelled" && r.started_at.is_none()))
        .max_by_key(|r| {
            (
                r.completed_at
                    .as_deref()
                    .and_then(crate::timeline::parse_iso_ms)
                    .unwrap_or(i64::MAX),
                r.ordinal,
            )
        })
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Working,
    Approval,
    Input,
    #[default]
    Idle,
}
impl Status {
    fn request_text(self) -> Option<&'static str> {
        match self {
            Self::Approval => Some("Needs approval"),
            Self::Input => Some("Needs your input"),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Activity {
    pub status: Status,
    pub request_id: Option<String>,
    pub run_id: Option<String>,
    pub run_status: String,
}
#[derive(Debug, PartialEq, Eq)]
pub struct ActivityRef<'a> {
    pub status: Status,
    pub request_id: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub run_status: &'a str,
}

pub fn open_activity<'a>(
    p: &'a ThreadProjection,
    shell: Option<&'a ThreadShell>,
    child: Option<SubagentRequest>,
) -> Option<ActivityRef<'a>> {
    let latest = crate::timeline::presented_latest_run(p);
    let activity_run = p
        .runs
        .iter()
        .filter(|r| ["preparing", "starting", "running", "waiting"].contains(&r.status.as_str()))
        .max_by_key(|r| r.ordinal)
        .or(latest);
    let shell_run = shell.and_then(|s| {
        s.latest_run_id.as_deref().map(|id| {
            (
                id,
                if s.status == "idle" {
                    "completed"
                } else {
                    s.status.as_str()
                },
            )
        })
    });
    if detail_behind(
        latest.map(|r| (r.id.as_str(), r.status.as_str())),
        shell_run,
    ) {
        return None;
    }
    let run = if activity_run.is_some_and(|r| r.status == "cancelled" && r.started_at.is_none()) {
        latest_executed_run(&p.runs)
    } else {
        activity_run
    };
    let pending = |r: &&crate::thread::Entity| r.value["status"] == "pending";
    let approval = p.runtime_requests.iter().filter(pending).find(|r| {
        !matches!(
            r.value["kind"].as_str(),
            Some("user_input" | "auth_refresh" | "dynamic_tool_call")
        )
    });
    let input = p
        .runtime_requests
        .iter()
        .filter(pending)
        .find(|r| r.value["kind"] == "user_input");
    let (status, request) = if approval.is_some() {
        (Status::Approval, approval)
    } else if child == Some(SubagentRequest::Approval) {
        (Status::Approval, None)
    } else if input.is_some()
        || shell.is_some_and(|s| {
            s.pending_runtime_request
                .as_ref()
                .is_some_and(|r| r.kind == "user_input")
        })
    {
        (Status::Input, input)
    } else if child == Some(SubagentRequest::UserInput) {
        (Status::Input, None)
    } else {
        let runless = p.thread.lineage.relationship_to_parent.as_deref() == Some("subagent")
            && p.nodes
                .iter()
                .rev()
                .find(|n| n.value["kind"] == "root_turn" && n.value["runId"].is_null())
                .is_some_and(|n| {
                    matches!(
                        n.value["status"].as_str(),
                        Some("pending" | "running" | "waiting")
                    )
                });
        let working = runless
            || activity_run.is_some_and(|r| {
                ["preparing", "starting", "running", "waiting"].contains(&r.status.as_str())
            });
        (
            if working {
                Status::Working
            } else {
                Status::Idle
            },
            None,
        )
    };
    Some(ActivityRef {
        status,
        request_id: request.map(|r| r.id.as_str()),
        run_id: run.map(|r| r.id.as_str()),
        run_status: run.map_or("", |r| r.status.as_str()),
    })
}
#[derive(Clone, Debug)]
pub struct ThreadMemory {
    snapshot: Activity,
    turn: Option<String>,
    settled: Option<String>,
    idle: Option<String>,
}
pub fn next_thread(
    previous: Option<ThreadMemory>,
    next: Activity,
) -> (ThreadMemory, Option<&'static str>) {
    let idle = next.status == Status::Idle;
    let idle_run =
        if idle && !["queued", "preparing", "starting"].contains(&next.run_status.as_str()) {
            next.run_id.clone()
        } else {
            previous.as_ref().and_then(|p| p.idle.clone())
        };
    let new_run = (!idle
        && ["preparing", "starting", "running", "waiting"].contains(&next.run_status.as_str())
        && next.run_id != idle_run)
        .then(|| next.run_id.clone())
        .flatten();
    let Some(previous) = previous else {
        return (
            ThreadMemory {
                settled: outcome(&next.run_status).and(next.run_id.clone()),
                turn: if idle {
                    None
                } else {
                    Some(new_run.unwrap_or_else(|| "unknown".into()))
                },
                snapshot: next,
                idle: idle_run,
            },
            None,
        );
    };
    let mut memory = ThreadMemory {
        snapshot: next.clone(),
        idle: idle_run,
        ..previous.clone()
    };
    if !idle {
        let started = previous.turn.is_none()
            || (new_run.is_some()
                && previous.turn.as_deref() != Some("unknown")
                && new_run != previous.turn);
        if started || previous.turn.as_deref() == Some("unknown") {
            memory.turn = Some(new_run.unwrap_or_else(|| "unknown".into()));
        }
        let text = match next.status.request_text() {
            Some(_)
                if previous.snapshot.status == next.status
                    && previous.snapshot.request_id == next.request_id =>
            {
                None
            }
            Some(text) => Some(text),
            None if started => Some("Working"),
            _ => None,
        };
        return (memory, text);
    }
    let text = outcome(&next.run_status);
    if previous.turn.is_none() {
        if text.is_some() {
            memory.settled = next.run_id;
        }
        return (memory, None);
    }
    if next.run_status == "rolled_back" {
        memory.turn = None;
        memory.settled = next.run_id;
        return (memory, None);
    }
    if text.is_none() {
        return (memory, None);
    }
    if next.run_id == previous.settled {
        memory.turn = None;
        return (memory, None);
    }
    if previous.turn.as_deref() == Some("unknown") && next.run_id == previous.idle {
        memory.settled = next.run_id;
        return (memory, None);
    }
    memory.turn = None;
    memory.settled = next.run_id;
    (memory, text)
}
#[derive(Default)]
pub struct Background {
    previous: HashMap<String, BackgroundMemory>,
    epoch: u64,
}
struct BackgroundMemory {
    seen: u64,
    revision: Option<u64>,
    request: Status,
    request_id: Option<String>,
    settled: Option<String>,
}
impl Background {
    /// Streaming text leaves announcement snapshots untouched.
    pub fn observe(
        &mut self,
        shell: &crate::shell::ShellState,
        open: Option<&str>,
        now: i64,
    ) -> Option<String> {
        use crate::sidebar::{
            settled::effective_snoozed,
            thread::{SubagentRequest, pending_subagent_requests, present_thread},
        };
        let threads = &shell.snapshot.as_ref()?.threads;
        let requests = pending_subagent_requests(threads);
        self.epoch = self.epoch.wrapping_add(1);
        let mut messages = Vec::new();
        for raw in threads
            .iter()
            .filter(|raw| raw.archived_at.is_none() && raw.lineage.is_listed())
        {
            let revision = shell.revision(&raw.id);
            let sub = requests.get(&raw.id).copied();
            if let Some(before) = self.previous.get_mut(&raw.id) {
                before.seen = self.epoch;
            }
            let before = self.previous.get(&raw.id);
            let request = raw.pending_runtime_request.as_ref();
            let own_approval = request.is_some_and(|r| {
                !["user_input", "auth_refresh", "dynamic_tool_call"].contains(&r.kind.as_str())
            });
            let (status, request_id) = if own_approval {
                (Status::Approval, request.map(|r| r.id.as_str()))
            } else if sub == Some(SubagentRequest::Approval) {
                (Status::Approval, None)
            } else if request.is_some_and(|r| r.kind == "user_input") {
                (Status::Input, request.map(|r| r.id.as_str()))
            } else if sub == Some(SubagentRequest::UserInput) {
                (Status::Input, None)
            } else {
                (Status::Idle, None)
            };
            if before.is_some_and(|b| {
                b.revision == revision
                    && b.request == status
                    && b.request_id.as_deref() == request_id
            }) {
                continue;
            }
            let thread = present_thread(raw, sub);
            let run = thread.latest_run.as_ref();
            let run_id = run.map(|r| r.run_id.as_str());
            let run_status = run.map_or("", |r| r.status.as_str());
            let result = outcome(run_status);
            if let Some(before) =
                before.filter(|_| open != Some(raw.id.as_str()) && !effective_snoozed(&thread, now))
            {
                if (status != before.request || request_id != before.request_id.as_deref())
                    && let Some(text) = status.request_text()
                {
                    messages.push(format!("{text}: {}", raw.title));
                }
                let another = raw
                    .active_run_id
                    .as_deref()
                    .is_some_and(|id| Some(id) != run_id);
                if let Some(text) =
                    result.filter(|_| run_id != before.settled.as_deref() && !another)
                {
                    messages.push(format!("{text}: {}", raw.title));
                }
            }
            let settled = if result.is_some() || run_status == "rolled_back" {
                run_id.map(str::to_owned)
            } else {
                before.and_then(|b| b.settled.clone())
            };
            self.previous.insert(
                raw.id.clone(),
                BackgroundMemory {
                    seen: self.epoch,
                    revision,
                    request: status,
                    request_id: request_id.map(str::to_owned),
                    settled,
                },
            );
        }
        self.previous.retain(|_, memory| memory.seen == self.epoch);
        (!messages.is_empty()).then(|| messages.join(". "))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_settles_dedupes_replaces_cancels_and_repeats() {
        let mut q = Announcer::default();
        q.announce("Settled 1".into(), Some("undo"), Priority::Polite, 0);
        q.announce("Opening in Zed".into(), None, Priority::Polite, 10);
        q.announce("Settled 2".into(), Some("undo"), Priority::Polite, 20);
        q.focus_changed(Some("changed"), 200);
        assert!(q.flush(449).is_none());
        assert_eq!(q.flush(450).unwrap().message, "Opening in Zed. Settled 2");
        q.announce("Working".into(), None, Priority::Polite, 500);
        q.announce("Working".into(), None, Priority::Polite, 510);
        q.announce("Stale".into(), Some("thread"), Priority::Polite, 520);
        q.cancel("thread");
        assert_eq!(q.flush(770).unwrap().message, "Working");
        q.announce("Working".into(), None, Priority::Assertive, 800);
        assert_eq!(
            q.flush(1050).unwrap(),
            Announcement {
                message: "Working".into(),
                priority: Priority::Assertive
            }
        );
    }
    fn activity(status: Status, run: &str, state: &str) -> Activity {
        Activity {
            status,
            run_id: Some(run.into()),
            run_status: state.into(),
            ..Default::default()
        }
    }
    #[test]
    fn thread_baseline_request_ids_and_delayed_end() {
        let (m, text) = next_thread(None, activity(Status::Idle, "old", "completed"));
        assert_eq!(text, None);
        let (m, text) = next_thread(Some(m), activity(Status::Working, "new", "running"));
        assert_eq!(text, Some("Working"));
        let mut approval = activity(Status::Approval, "new", "waiting");
        approval.request_id = Some("a".into());
        let (m, text) = next_thread(Some(m), approval.clone());
        assert_eq!(text, Some("Needs approval"));
        let (m, text) = next_thread(Some(m), approval.clone());
        assert_eq!(text, None);
        approval.request_id = Some("b".into());
        let (m, text) = next_thread(Some(m), approval);
        assert_eq!(text, Some("Needs approval"));
        let (m, text) = next_thread(Some(m), activity(Status::Idle, "new", "running"));
        assert_eq!(text, None);
        let (m, text) = next_thread(Some(m), activity(Status::Idle, "new", "completed"));
        assert_eq!(text, Some("Done"));
        let (_, text) = next_thread(Some(m), activity(Status::Idle, "queued", "cancelled"));
        assert_eq!(text, None);
    }
}

#[cfg(test)]
mod background_tests {
    use super::*;
    use crate::shell::{ShellSnapshot, ShellStreamItem, ThreadShell};
    use serde_json::{Value, json};
    pub(super) fn row(id: &str, state: &str, extra: Value) -> ThreadShell {
        let mut v = json!({"id":id,"projectId":"p","title":id,"providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"gpt"},"runtimeMode":"auto","interactionMode":"default","branch":null,"worktreePath":null,"lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":id},"latestRunId":"r1","activeRunId":null,"status":state,"pendingRuntimeRequest":null,"latestVisibleMessage":null,"latestUserMessageAt":null,"hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0,"createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z","archivedAt":null,"settledOverride":null,"settledAt":null,"deletedAt":null});
        for (k, value) in extra.as_object().into_iter().flatten() {
            v[k] = value.clone();
        }
        serde_json::from_value(v).unwrap()
    }
    fn observe(
        watcher: &mut Background,
        shell: &mut crate::shell::ShellState,
        threads: Vec<ThreadShell>,
        open: Option<&str>,
    ) -> Option<String> {
        shell.apply(ShellStreamItem::Snapshot {
            snapshot: Box::new(ShellSnapshot {
                threads,
                ..Default::default()
            }),
            resolved_repository_identity_roots: None,
        });
        watcher.observe(shell, open, 1_000)
    }
    #[test]
    fn silent_baselines_request_replacement_and_once_per_ending() {
        let (mut w, mut shell) = (Background::default(), crate::shell::ShellState::default());
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row("a", "running", json!({}))],
                None
            ),
            None
        );
        let request = |id| {
            row(
                "a",
                "waiting",
                json!({"pendingRuntimeRequest":{"id":id,"kind":"command_approval"}}),
            )
        };
        assert_eq!(
            observe(&mut w, &mut shell, vec![request("1")], None).as_deref(),
            Some("Needs approval: a")
        );
        assert_eq!(observe(&mut w, &mut shell, vec![request("1")], None), None);
        assert_eq!(
            observe(&mut w, &mut shell, vec![request("2")], None).as_deref(),
            Some("Needs approval: a")
        );
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row("a", "completed", json!({}))],
                None
            )
            .as_deref(),
            Some("Done: a")
        );
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row("a", "completed", json!({}))],
                None
            ),
            None
        );
        assert_eq!(
            observe(&mut w, &mut shell, vec![request("3")], Some("a")),
            None
        );
        assert_eq!(observe(&mut w, &mut shell, vec![request("3")], None), None);
    }
    #[test]
    fn skips_archived_children_snoozed_and_cancelled_queue_while_another_run_works() {
        let (mut w, mut shell) = (Background::default(), crate::shell::ShellState::default());
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row("a", "running", json!({}))],
                None
            ),
            None
        );
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row(
                    "a",
                    "cancelled",
                    json!({"latestRunId":"queued","activeRunId":"r1"})
                )],
                None
            ),
            None
        );
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row(
                    "a",
                    "completed",
                    json!({"archivedAt":"2026-11-01T00:00:00Z"})
                )],
                None
            ),
            None
        );
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row(
                    "child",
                    "running",
                    json!({"lineage":{"parentThreadId":"a","relationshipToParent":"subagent","rootThreadId":"a"}})
                )],
                None
            ),
            None
        );
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row(
                    "s",
                    "running",
                    json!({"snoozedUntil":"2099-11-01T00:00:00Z"})
                )],
                None
            ),
            None
        );
        assert_eq!(
            observe(
                &mut w,
                &mut shell,
                vec![row(
                    "s",
                    "completed",
                    json!({"snoozedUntil":"2099-11-01T00:00:00Z"})
                )],
                None
            ),
            None
        );
    }
}

#[cfg(test)]
mod settling_tests {
    use super::*;
    #[test]
    fn cancelled_queued_runs_do_not_replace_the_last_executed_run() {
        let run = |id: &str,
                   ordinal,
                   status: &str,
                   started: Option<&str>,
                   completed: Option<&str>| {
            serde_json::from_value::<crate::thread::Run>(serde_json::json!({
            "id":id,"threadId":"t","ordinal":ordinal,"providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"gpt"},"userMessageId":"m","rootNodeId":null,"status":status,
            "requestedAt":"2026-11-01T00:00:00Z","startedAt":started,"completedAt":completed
        })).unwrap()
        };
        let runs = vec![
            run(
                "executed",
                1,
                "completed",
                Some("2026-11-01T00:00:00Z"),
                Some("2026-11-01T00:01:00Z"),
            ),
            run("queued", 2, "cancelled", None, Some("2026-11-01T00:02:00Z")),
        ];
        assert_eq!(
            latest_executed_run(&runs).map(|r| r.id.as_str()),
            Some("executed")
        );
        assert!(latest_executed_run(&runs[1..]).is_none());
    }
    #[test]
    fn stale_details_and_unknown_turns_do_not_replay_history() {
        assert!(detail_behind(
            Some(("r", "running")),
            Some(("r", "completed"))
        ));
        assert!(detail_behind(None, Some(("r", "running"))));
        assert!(!detail_behind(
            Some(("r", "running")),
            Some(("r", "running"))
        ));
        let activity = |status, run: &str, run_status: &str| Activity {
            status,
            run_id: Some(run.into()),
            run_status: run_status.into(),
            ..Default::default()
        };
        let (m, _) = next_thread(None, activity(Status::Idle, "old", "running"));
        let (m, text) = next_thread(Some(m), activity(Status::Working, "old", "running"));
        assert_eq!(text, Some("Working"));
        let (m, text) = next_thread(Some(m), activity(Status::Idle, "old", "completed"));
        assert_eq!(text, None);
        let (m, text) = next_thread(Some(m), activity(Status::Working, "new", "starting"));
        assert_eq!(text, None);
        let (m, text) = next_thread(Some(m), activity(Status::Idle, "new", "failed"));
        assert_eq!(text, Some("Failed"));
        let (_, text) = next_thread(Some(m), activity(Status::Idle, "new", "failed"));
        assert_eq!(text, None);
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use serde_json::{Value, json};
    fn projection(runs: Value) -> ThreadProjection {
        serde_json::from_value(json!({
            "thread":{"id":"a","projectId":"p","title":"a","providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"gpt"},"runtimeMode":"auto","interactionMode":"default","branch":null,"worktreePath":null,"lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":"a"},"createdAt":"2026-11-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z","archivedAt":null},
            "runs":runs,"attempts":[],"nodes":[],"subagents":[],"providerSessions":[],"providerThreads":[],"providerTurns":[],"runtimeRequests":[],"messages":[],"plans":[],"turnItems":[],"checkpointScopes":[],"checkpoints":[],"contextHandoffs":[],"contextTransfers":[],"visibleTurnItems":[],"updatedAt":"2026-11-01T00:00:00Z"
        })).unwrap()
    }
    fn run(id: &str, ordinal: u64, status: &str) -> Value {
        json!({"id":id,"ordinal":ordinal,"providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"gpt"},"userMessageId":"m","rootNodeId":null,"status":status,"requestedAt":"2026-11-01T00:00:00Z","startedAt":"2026-11-01T00:00:00Z","completedAt":null})
    }
    #[test]
    fn held_queues_leave_approval_input_and_completion_visible() {
        let mut held = run("held", 2, "queued");
        held["queueHeld"] = json!(true);
        let mut p = projection(json!([run("r1", 1, "running"), held]));
        let mut shell = super::background_tests::row("a", "running", json!({}));
        let snapshot = open_activity(&p, Some(&shell), None)
            .expect("held queue is not the shell's latest run");
        let (mut memory, _) = next_thread(
            None,
            Activity {
                status: snapshot.status,
                run_id: snapshot.run_id.map(str::to_owned),
                run_status: snapshot.run_status.into(),
                ..Default::default()
            },
        );
        for (kind, expected, text) in [
            ("command_approval", Status::Approval, "Needs approval"),
            ("user_input", Status::Input, "Needs your input"),
        ] {
            p.runtime_requests = vec![
                serde_json::from_value(json!({"id":kind,"status":"pending","kind":kind})).unwrap(),
            ];
            let snapshot = open_activity(&p, Some(&shell), None).unwrap();
            assert_eq!(snapshot.status, expected);
            let (next, announcement) = next_thread(
                Some(memory),
                Activity {
                    status: snapshot.status,
                    request_id: snapshot.request_id.map(str::to_owned),
                    run_id: snapshot.run_id.map(str::to_owned),
                    run_status: snapshot.run_status.into(),
                },
            );
            assert_eq!(announcement, Some(text));
            memory = next;
        }
        p.runtime_requests.clear();
        p.runs[0].status = "completed".into();
        shell.status = "completed".into();
        let snapshot = open_activity(&p, Some(&shell), None).unwrap();
        assert_eq!(snapshot.run_id, Some("r1"));
        let (_, text) = next_thread(
            Some(memory),
            Activity {
                status: snapshot.status,
                run_id: snapshot.run_id.map(str::to_owned),
                run_status: snapshot.run_status.into(),
                ..Default::default()
            },
        );
        assert_eq!(text, Some("Done"));
    }
    #[test]
    fn shell_only_secret_and_link_inputs_announce_without_runtime_requests() {
        let p = projection(json!([run("r1", 1, "running")]));
        let initial = super::background_tests::row("a", "running", json!({}));
        let activity = open_activity(&p, Some(&initial), None).unwrap();
        let (memory, _) = next_thread(
            None,
            Activity {
                status: activity.status,
                run_id: activity.run_id.map(str::to_owned),
                run_status: activity.run_status.into(),
                ..Default::default()
            },
        );
        for id in ["secret-card", "link-card"] {
            let shell = super::background_tests::row(
                "a",
                "running",
                json!({"pendingRuntimeRequest":{"id":id,"kind":"user_input","blocking":true}}),
            );
            let activity = open_activity(&p, Some(&shell), None).unwrap();
            assert_eq!(activity.status, Status::Input);
            assert_eq!(
                activity.request_id, None,
                "web's open-thread request id comes from runtime requests"
            );
            let (_, text) = next_thread(
                Some(memory.clone()),
                Activity {
                    status: activity.status,
                    run_id: activity.run_id.map(str::to_owned),
                    run_status: activity.run_status.into(),
                    ..Default::default()
                },
            );
            assert_eq!(text, Some("Needs your input"));
        }
    }
    #[test]
    fn background_secret_and_link_requests_keep_the_synthetic_request_id() {
        use crate::shell::{ShellSnapshot, ShellState, ShellStreamItem};
        let mut shell = ShellState::default();
        let mut watcher = Background::default();
        let mut observe = |extra| {
            shell.apply(ShellStreamItem::Snapshot {
                snapshot: Box::new(ShellSnapshot {
                    threads: vec![super::background_tests::row("a", "running", extra)],
                    ..Default::default()
                }),
                resolved_repository_identity_roots: None,
            });
            watcher.observe(&shell, None, 0)
        };
        assert_eq!(observe(json!({})), None);
        for id in ["secret-card", "link-card"] {
            let snapshot =
                json!({"pendingRuntimeRequest":{"id":id,"kind":"user_input","blocking":true}});
            assert_eq!(
                observe(snapshot.clone()).as_deref(),
                Some("Needs your input: a")
            );
            assert_eq!(observe(snapshot), None);
        }
    }
    #[test]
    fn focus_that_returns_to_its_start_still_restarts_the_settle_delay() {
        let mut q = Announcer::default();
        q.focus_changed(Some("a"), 0);
        q.announce("Done".into(), None, Priority::Polite, 0);
        q.focus_changed(Some("b"), 100);
        q.focus_changed(Some("a"), 200);
        assert!(q.flush(250).is_none());
        assert!(q.flush(449).is_none());
        assert_eq!(q.flush(450).unwrap().message, "Done");
    }
}
