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

/// `ScreenReaderAnnouncements` (`packages/contracts/src/settings.ts`, `pingdotgg/t3code#33900`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenReaderAnnouncements {
    #[default]
    Status,
    Messages,
    Everything,
}

impl ScreenReaderAnnouncements {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "status" => Some(Self::Status),
            "messages" => Some(Self::Messages),
            "everything" => Some(Self::Everything),
            _ => None,
        }
    }
}

pub fn screen_reader_announcements_setting(
    entries: &serde_json::Map<String, serde_json::Value>,
) -> ScreenReaderAnnouncements {
    entries
        .get("screenReaderAnnouncements")
        .and_then(serde_json::Value::as_str)
        .and_then(ScreenReaderAnnouncements::parse)
        .unwrap_or_default()
}

pub fn screen_reader_window_title_setting(
    entries: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    entries
        .get("screenReaderWindowTitle")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// `ThreadActivityAnnouncer.tsx` timing constants.
pub const ACTIVITY_GAP_MS: u64 = 2_000;
pub const MESSAGE_HOLD_MS_PER_CHARACTER: u64 = 50;
pub const MESSAGE_HOLD_MAX_MS: u64 = 15_000;
const ACTIVITY_MAX_LENGTH: usize = 120;
const MESSAGE_MAX_LENGTH: usize = 2_000;
const MESSAGE_AUTHOR: &str = "T3 Code";

pub fn hold_after(text: &str) -> u64 {
    let char_ms = (text.chars().count() as u64).saturating_mul(MESSAGE_HOLD_MS_PER_CHARACTER);
    char_ms.clamp(ACTIVITY_GAP_MS, MESSAGE_HOLD_MAX_MS)
}

/// `asOneAnnouncement` (`ThreadActivityAnnouncer.tsx`).
pub fn as_one_announcement(texts: &[String]) -> String {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            if index + 1 == texts.len() || text.ends_with(['.', '!', '?', ':', '…']) {
                text.clone()
            } else {
                format!("{text}.")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// First `max_len` scalar values of `text`, or `None` when it fits.
pub fn cut_text(text: &str, max_len: usize) -> Option<String> {
    if text.len() <= max_len {
        return None;
    }
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_len).collect();
    chars.next().is_some().then_some(head)
}

/// `TurnMessage` (`turnDetailAnnouncement.ts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnMessage {
    pub id: String,
    pub text: String,
    pub finished: bool,
    pub fresh: bool,
}

/// `TurnDetail` (`turnDetailAnnouncement.ts`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnDetail {
    pub activity: Option<String>,
    pub messages: Vec<TurnMessage>,
    pub tail_id: Option<String>,
}

/// `TurnDetailMemory` (`turnDetailAnnouncement.ts`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnDetailMemory {
    pub activity: Option<String>,
    pub messages: HashMap<String, bool>,
    pub tail_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnDetailAnnouncement {
    pub memory: TurnDetailMemory,
    pub messages: Vec<String>,
    pub activity: Option<String>,
}

fn plain_markdown_text(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut prev_space = true;
    for ch in markdown.chars() {
        if matches!(ch, '`' | '*' | '_' | '#') {
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_owned()
}

fn with_verb(item_type: &str, label: &str) -> String {
    if item_type == "file_change" {
        return format!("Editing {label}");
    }
    if item_type != "file_search" && item_type != "web_search" {
        return label.to_owned();
    }
    let lower = label.to_ascii_lowercase();
    if lower.starts_with("search ")
        || lower.starts_with("searched ")
        || lower.starts_with("searching ")
    {
        return label.to_owned();
    }
    if item_type == "web_search" {
        format!("Searching the web for {label}")
    } else {
        format!("Searching for {label}")
    }
}

/// Extracts `TurnDetail` for the latest turn directly from a `ThreadProjection`
/// (`turnDetail` in `turnDetailAnnouncement.ts`).
pub fn turn_detail_from_projection(
    projection: &ThreadProjection,
    known_tail_id: Option<&str>,
) -> TurnDetail {
    use crate::turn_item::TurnItemKind;
    use std::collections::HashSet;

    let mut messages = Vec::new();
    let mut continued_runs: HashSet<Option<&str>> = HashSet::new();
    let mut activity: Option<Option<String>> = None;
    let mut fresh = true;
    let items = &projection.visible_turn_items;
    let mut idx = items.len();
    while idx > 0 {
        idx -= 1;
        let row = &items[idx];
        let item = &row.item;
        let id = item.id.as_str();
        if matches!(item.kind, TurnItemKind::UserMessage { .. }) {
            break;
        }
        if known_tail_id == Some(id) {
            fresh = false;
        }
        let run_id = item.run_id.as_deref();
        match &item.kind {
            TurnItemKind::AssistantMessage { message_id, text, streaming, .. } => {
                activity.get_or_insert(None);
                messages.push(TurnMessage {
                    id: message_id.clone(),
                    text: text.clone(),
                    finished: !*streaming && continued_runs.contains(&run_id),
                    fresh,
                });
                continued_runs.insert(run_id);
            }
            TurnItemKind::ProposedPlan { .. } => {
                activity.get_or_insert(None);
                continued_runs.insert(run_id);
            }
            TurnItemKind::Reasoning { .. } => {
                continued_runs.insert(run_id);
                if activity.is_none() {
                    activity = Some(Some("Thinking".to_owned()));
                }
            }
            TurnItemKind::Compaction { .. } => {
                if activity.is_none() {
                    let in_progress = matches!(item.status.as_str(), "pending" | "running" | "waiting" | "in_progress");
                    let label = item.title.clone().unwrap_or_else(|| "Compacting context".to_owned());
                    activity = Some(in_progress.then_some(label));
                }
            }
            TurnItemKind::ApprovalRequest { .. } | TurnItemKind::UserInputRequest { .. } => {
                continued_runs.insert(run_id);
            }
            TurnItemKind::FileChange { file_name, .. } => {
                continued_runs.insert(run_id);
                if activity.is_none() {
                    let failed = matches!(item.status.as_str(), "failed" | "declined" | "stopped" | "cancelled");
                    if failed {
                        activity = Some(None);
                    } else {
                        let raw = item.title.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(file_name.as_str());
                        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                        activity = Some((!collapsed.is_empty()).then(|| {
                            let spoken = with_verb("file_change", &collapsed);
                            match cut_text(&spoken, ACTIVITY_MAX_LENGTH) {
                                Some(head) => format!("{head}…"),
                                None => spoken,
                            }
                        }));
                    }
                }
            }
            TurnItemKind::CommandExecution { input, .. } => {
                continued_runs.insert(run_id);
                if activity.is_none() {
                    let failed = matches!(item.status.as_str(), "failed" | "declined" | "stopped" | "cancelled");
                    if failed {
                        activity = Some(None);
                    } else {
                        let raw = item.title.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(input.as_str());
                        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                        activity = Some((!collapsed.is_empty()).then(|| match cut_text(&collapsed, ACTIVITY_MAX_LENGTH) {
                            Some(head) => format!("{head}…"),
                            None => collapsed,
                        }));
                    }
                }
            }
            TurnItemKind::FileSearch { pattern, .. } => {
                continued_runs.insert(run_id);
                if activity.is_none() {
                    let failed = matches!(item.status.as_str(), "failed" | "declined" | "stopped" | "cancelled");
                    if failed {
                        activity = Some(None);
                    } else {
                        let raw = item
                            .title
                            .as_deref()
                            .filter(|s| !s.trim().is_empty())
                            .or(pattern.as_deref())
                            .unwrap_or("");
                        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                        activity = Some((!collapsed.is_empty()).then(|| {
                            let spoken = with_verb("file_search", &collapsed);
                            match cut_text(&spoken, ACTIVITY_MAX_LENGTH) {
                                Some(head) => format!("{head}…"),
                                None => spoken,
                            }
                        }));
                    }
                }
            }
            TurnItemKind::WebSearch { patterns, .. } => {
                continued_runs.insert(run_id);
                if activity.is_none() {
                    let failed = matches!(item.status.as_str(), "failed" | "declined" | "stopped" | "cancelled");
                    if failed {
                        activity = Some(None);
                    } else {
                        let first_pattern = patterns.as_ref().and_then(|p| p.first()).map(String::as_str);
                        let raw = item
                            .title
                            .as_deref()
                            .filter(|s| !s.trim().is_empty())
                            .or(first_pattern)
                            .unwrap_or("");
                        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                        activity = Some((!collapsed.is_empty()).then(|| {
                            let spoken = with_verb("web_search", &collapsed);
                            match cut_text(&spoken, ACTIVITY_MAX_LENGTH) {
                                Some(head) => format!("{head}…"),
                                None => spoken,
                            }
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    let mut tail_known = !fresh;
    if let Some(known) = known_tail_id {
        while !tail_known && idx > 0 {
            idx -= 1;
            if items[idx].item.id == known {
                tail_known = true;
            }
        }
    }
    if !tail_known {
        for m in &mut messages {
            m.fresh = false;
        }
    }
    messages.reverse();
    TurnDetail {
        activity: activity.flatten(),
        messages,
        tail_id: items.last().map(|e| e.item.id.clone()),
    }
}

/// `nextTurnDetailAnnouncement` (`turnDetailAnnouncement.ts`).
pub fn next_turn_detail_announcement(
    previous: Option<&TurnDetailMemory>,
    next: &TurnDetail,
    level: ScreenReaderAnnouncements,
    status: Status,
) -> TurnDetailAnnouncement {
    let memory = TurnDetailMemory {
        activity: if status == Status::Working { next.activity.clone() } else { None },
        messages: next.messages.iter().map(|m| (m.id.clone(), m.finished)).collect(),
        tail_id: next.tail_id.clone(),
    };
    let Some(previous) = previous else {
        return TurnDetailAnnouncement { memory, messages: Vec::new(), activity: None };
    };
    if level == ScreenReaderAnnouncements::Status || status == Status::Idle {
        return TurnDetailAnnouncement { memory, messages: Vec::new(), activity: None };
    }
    let messages = next
        .messages
        .iter()
        .filter(|m| {
            m.finished
                && previous.messages.get(&m.id) != Some(&true)
                && (m.fresh || previous.messages.contains_key(&m.id))
        })
        .filter_map(|m| {
            let spoken = match cut_text(&m.text, MESSAGE_MAX_LENGTH) {
                Some(cut) => format!("{} More in the thread.", plain_markdown_text(&cut)),
                None => plain_markdown_text(&m.text),
            };
            (!spoken.is_empty()).then(|| format!("{MESSAGE_AUTHOR}: {spoken}"))
        })
        .collect();
    let activity = if level == ScreenReaderAnnouncements::Everything && memory.activity != previous.activity {
        memory.activity.clone()
    } else {
        None
    };
    TurnDetailAnnouncement { memory, messages, activity }
}

/// `resolveWindowTitleThreadStatus` (`Sidebar.logic.ts`, `pingdotgg/t3code#34697`).
pub fn resolve_window_title_thread_status(
    thread: &crate::sidebar::thread::SidebarThread,
) -> Option<&'static str> {
    use crate::sidebar::status::{ThreadStatus, resolve_status};
    let mut without_guest = thread.clone();
    without_guest.pending_guest_message_count = 0;
    match resolve_status(&without_guest) {
        ThreadStatus::Approval => Some("Needs approval"),
        ThreadStatus::Input => Some("Needs your input"),
        ThreadStatus::Preparing | ThreadStatus::Working => Some("Working"),
        ThreadStatus::Waiting => (thread.runtime.as_ref().map(|r| r.status.as_str()) == Some("running")).then_some("Working"),
        _ => None,
    }
}

/// `resolveWindowTitle` (`branding.logic.ts`, `pingdotgg/t3code#34697`).
pub fn resolve_window_title(
    app_display_name: &str,
    project_title: Option<&str>,
    thread_title: Option<&str>,
    screen_reader_status: Option<Option<&str>>,
    desktop: bool,
) -> String {
    let thread = thread_title.map(str::trim).unwrap_or("");
    let status = screen_reader_status.flatten();
    if thread.is_empty() {
        return match status {
            Some(s) => format!("{s}: {app_display_name}"),
            None => app_display_name.to_owned(),
        };
    }
    let project = project_title.map(str::trim).unwrap_or("");
    let context = if project.is_empty() {
        thread.to_owned()
    } else if screen_reader_status.is_some() {
        format!("{thread} - {project}")
    } else {
        format!("{project} / {thread}")
    };
    let title = match status {
        Some(s) => format!("{s}: {context}"),
        None => context,
    };
    if desktop { title } else { format!("{title} - {app_display_name}") }
}

#[cfg(test)]
mod screen_reader_detail_tests {
    use super::*;

    #[test]
    fn turn_detail_announces_finished_messages_and_steps_by_level() {
        let baseline = TurnDetail {
            activity: Some("Thinking".into()),
            messages: vec![TurnMessage { id: "m1".into(), text: "Draft".into(), finished: false, fresh: false }],
            tail_id: Some("m1".into()),
        };
        let first = next_turn_detail_announcement(None, &baseline, ScreenReaderAnnouncements::Everything, Status::Working);
        assert!(first.messages.is_empty());
        assert!(first.activity.is_none());

        let next = TurnDetail {
            activity: Some("Editing src/main.rs".into()),
            messages: vec![TurnMessage { id: "m1".into(), text: "**Fixed** `bug`".into(), finished: true, fresh: false }],
            tail_id: Some("w1".into()),
        };
        let out_messages = next_turn_detail_announcement(Some(&first.memory), &next, ScreenReaderAnnouncements::Messages, Status::Working);
        assert_eq!(out_messages.messages, vec!["T3 Code: Fixed bug"]);
        assert_eq!(out_messages.activity, None);

        let out_everything = next_turn_detail_announcement(Some(&first.memory), &next, ScreenReaderAnnouncements::Everything, Status::Working);
        assert_eq!(out_everything.messages, vec!["T3 Code: Fixed bug"]);
        assert_eq!(out_everything.activity.as_deref(), Some("Editing src/main.rs"));

        let out_status = next_turn_detail_announcement(Some(&first.memory), &next, ScreenReaderAnnouncements::Status, Status::Working);
        assert!(out_status.messages.is_empty());
        assert!(out_status.activity.is_none());
    }

    #[test]
    fn window_title_leads_with_status_and_thread_when_screen_reader_enabled() {
        assert_eq!(
            resolve_window_title("T3 Code", Some("t3code"), Some("Fix bug"), None, true),
            "t3code / Fix bug"
        );
        assert_eq!(
            resolve_window_title("T3 Code", Some("t3code"), Some("Fix bug"), Some(Some("Working")), true),
            "Working: Fix bug - t3code"
        );
        assert_eq!(
            resolve_window_title("T3 Code", Some("t3code"), Some("Fix bug"), Some(None), true),
            "Fix bug - t3code"
        );
        assert_eq!(
            resolve_window_title("T3 Code", None, None, Some(None), true),
            "T3 Code"
        );
        assert_eq!(
            as_one_announcement(&["T3 Code: Ready".into(), "Needs approval".into()]),
            "T3 Code: Ready. Needs approval"
        );
        assert_eq!(hold_after("Hi"), ACTIVITY_GAP_MS);
    }
}
