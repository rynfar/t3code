//! Proposed-plan text and export helpers.
//! Ports `apps/web/src/proposedPlan.ts`, `packages/shared/src/proposedPlanText.ts`,
//! and the proposed-plan helpers in `apps/web/src/session-logic.ts`.

use crate::thread::ThreadProjection;
use crate::timeline::activity::{plan_title, strip_displayed_plan_markdown as strip_plan_markdown_body};

/// Extract the first markdown heading from `plan_markdown` (`proposedPlanTitle`).
pub fn proposed_plan_title(plan_markdown: &str) -> Option<String> {
    plan_title(plan_markdown)
}

/// Strip the leading title heading and an immediate `Summary` subheading (`stripDisplayedPlanMarkdown`).
pub fn strip_displayed_plan_markdown(plan_markdown: &str) -> String {
    strip_plan_markdown_body(plan_markdown)
}

pub const DEFAULT_COLLAPSED_PLAN_PREVIEW_LINES: usize = 10;

/// Collapsed preview markdown for `ProposedPlanCard` (`buildCollapsedProposedPlanPreviewMarkdown`).
/// Counts only non-whitespace lines toward `max_lines`, trims trailing blank lines, and
/// appends a blank line followed by `...` when remaining non-whitespace content was truncated.
pub fn build_collapsed_proposed_plan_preview_markdown(plan_markdown: &str, max_lines: usize) -> (String, bool) {
    let display = strip_displayed_plan_markdown(plan_markdown);
    let raw_lines: Vec<&str> = display.lines().collect();
    let mut preview_lines: Vec<&str> = Vec::new();
    let mut visible_line_count = 0usize;
    let mut stopped_at = raw_lines.len();

    for (index, line) in raw_lines.iter().enumerate() {
        if preview_lines.is_empty() && line.trim().is_empty() {
            continue;
        }
        preview_lines.push(line);
        if !line.trim().is_empty() {
            visible_line_count += 1;
            if visible_line_count >= max_lines {
                stopped_at = index + 1;
                break;
            }
        }
    }

    while preview_lines.last().is_some_and(|line| line.trim().is_empty()) {
        preview_lines.pop();
    }

    if preview_lines.is_empty() {
        let fallback = proposed_plan_title(plan_markdown).unwrap_or_else(|| "Plan preview unavailable.".to_owned());
        return (fallback, false);
    }

    let was_truncated = raw_lines[stopped_at..].iter().any(|line| !line.trim().is_empty());
    if !was_truncated {
        return (preview_lines.join("\n"), false);
    }

    preview_lines.push("");
    preview_lines.push("...");
    (preview_lines.join("\n"), true)
}

fn sanitize_plan_file_segment(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.to_ascii_lowercase().chars() {
        if matches!(ch, '`' | '\'' | '"' | '.' | ',' | '!' | '?' | '(' | ')' | '[' | ']' | '{' | '}') {
            continue;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "plan".to_owned() } else { out }
}

/// Export filename for downloading or saving a plan (`buildProposedPlanMarkdownFilename`).
pub fn build_proposed_plan_markdown_filename(plan_markdown: &str) -> String {
    let title = proposed_plan_title(plan_markdown);
    format!("{}.md", sanitize_plan_file_segment(title.as_deref().unwrap_or("plan")))
}

/// Normalize markdown for export (`normalizePlanMarkdownForExport`).
pub fn normalize_plan_markdown_for_export(plan_markdown: &str) -> String {
    format!("{}\n", plan_markdown.trim_end())
}

/// `unsentManualContinuationSource` (`packages/shared/src/orchestrationV2ManualContinuation.ts`):
/// returns whether continuing `run_id` has an unsent message that the provider never received.
fn has_unsent_manual_continuation_source(run_id: &str, projection: &ThreadProjection) -> bool {
    let runs: std::collections::HashMap<&str, &crate::thread::Run> =
        projection.runs.iter().map(|run| (run.id.as_str(), run)).collect();
    let accepted_attempts: std::collections::HashSet<&str> = projection
        .provider_turns
        .iter()
        .filter_map(|turn| turn.value.get("runAttemptId").and_then(serde_json::Value::as_str))
        .collect();
    let latest_attempt = |id: &str, matches: &dyn Fn(&crate::thread::RunAttempt) -> bool| -> u64 {
        projection
            .attempts
            .iter()
            .filter(|attempt| attempt.run_id == id && matches(attempt))
            .map(|attempt| attempt.attempt_ordinal)
            .max()
            .unwrap_or(0)
    };
    let mut visited = std::collections::HashSet::<&str>::new();
    let mut source_id = Some(run_id);
    while let Some(id) = source_id {
        if !visited.insert(id) {
            break;
        }
        let Some(source) = runs.get(id).copied() else {
            return false;
        };
        let accepted = latest_attempt(id, &|attempt| accepted_attempts.contains(attempt.id.as_str()));
        // An accepted attempt delivered everything before it; only a later steering restart's message is unsent.
        let restarted = latest_attempt(id, &|attempt| attempt.reason.as_deref() == Some("steering_restart"));
        if restarted > accepted {
            return true;
        }
        if accepted > 0 {
            return false;
        }
        if source.manual_continuation_of_run_id.is_none() {
            return true;
        }
        source_id = source.manual_continuation_of_run_id.as_deref();
    }
    false
}

fn run_provider_session<'a>(
    projection: &'a ThreadProjection,
    run: &crate::thread::Run,
) -> Option<&'a crate::thread::Entity> {
    let session_id = run.provider_thread_id.as_deref().and_then(|pt_id| {
        projection
            .provider_threads
            .iter()
            .find(|thread| thread.id == pt_id)
            .and_then(|thread| thread.value.get("providerSessionId").and_then(serde_json::Value::as_str))
    });
    match session_id {
        Some(sid) => projection.provider_sessions.iter().find(|session| session.id == sid),
        None => projection.provider_sessions.iter().rfind(|session| {
            session.value.get("providerInstanceId").and_then(serde_json::Value::as_str)
                == Some(run.provider_instance_id.as_str())
        }),
    }
}

/// The latest interrupted or usage-limited run that a manual resume continues (`deriveResumableRunId`,
/// `pingdotgg/t3code#39229`).
pub fn resumable_run_id(projection: &ThreadProjection) -> Option<String> {
    let latest_executed = projection
        .runs
        .iter()
        .filter(|run| run.status != "queued" && !(run.status == "cancelled" && run.started_at.is_none()))
        .max_by(|a, b| {
            let end_a = a.completed_at.as_deref().and_then(super::parse_iso_ms).unwrap_or(i64::MAX);
            let end_b = b.completed_at.as_deref().and_then(super::parse_iso_ms).unwrap_or(i64::MAX);
            end_a.cmp(&end_b).then_with(|| a.ordinal.cmp(&b.ordinal))
        })?;
    let resumable = latest_executed.status == "interrupted"
        || (latest_executed.status == "failed"
            && super::status::thread_error(projection)
                .is_some_and(|err| err.class.as_deref() == Some("usage_limit")));
    if !resumable {
        return None;
    }
    // Without native resume the server can only resend a message the provider never received.
    let supports_native_resume = run_provider_session(projection, latest_executed)
        .and_then(|session| session.value.pointer("/capabilities/turns/supportsNativeResume"))
        .and_then(serde_json::Value::as_bool);
    if supports_native_resume == Some(false)
        && !has_unsent_manual_continuation_source(&latest_executed.id, projection)
    {
        return None;
    }
    Some(latest_executed.id.clone())
}

/// Whether the thread currently has a resumable interrupted/usage-limited run (without a
/// pending runtime request blocking continuation), else a held queued run
/// (`canResumeThread`).
pub fn can_resume_thread(projection: &ThreadProjection) -> bool {
    if resumable_run_id(projection).is_none() {
        return projection.runs.iter().any(|run| run.status == "queued" && run.queue_held == Some(true));
    }
    !projection
        .runtime_requests
        .iter()
        .any(|request| request.value.get("status").and_then(serde_json::Value::as_str) == Some("pending"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_displayed_removes_title_and_summary_heading() {
        let md = "# My Plan\n\n## Summary\n\n- Step 1\n- Step 2\n";
        assert_eq!(strip_displayed_plan_markdown(md), "- Step 1\n- Step 2");
        assert_eq!(proposed_plan_title(md).as_deref(), Some("My Plan"));
    }

    #[test]
    fn collapsed_preview_truncates_and_falls_back_to_title() {
        let md = "# Empty Body";
        assert_eq!(build_collapsed_proposed_plan_preview_markdown(md, 8), ("Empty Body".to_owned(), false));

        // Blank lines do not count toward the 8 non-whitespace preview lines.
        let with_blanks = (1..=10).map(|i| format!("line {i}\n\n")).collect::<String>();
        let (preview, truncated) = build_collapsed_proposed_plan_preview_markdown(&with_blanks, 8);
        assert!(truncated);
        assert!(preview.ends_with("\n\n..."));
        assert_eq!(preview.lines().filter(|l| !l.trim().is_empty()).count(), 9);
    }

    #[test]
    fn filename_and_export_sanitize_headings() {
        let md = "# Refactor: Transcript / Messages (Wave 1C)\n\nBody";
        assert_eq!(build_proposed_plan_markdown_filename(md), "refactor-transcript-messages-wave-1c.md");
        assert_eq!(build_proposed_plan_markdown_filename("# It's Bob's v1.2 plan!"), "its-bobs-v12-plan.md");
        assert_eq!(normalize_plan_markdown_for_export("# Hi\n\n  "), "# Hi\n");
    }

    #[test]
    fn can_resume_thread_blocks_when_runtime_request_is_pending() {
        let mut projection: ThreadProjection = serde_json::from_value(serde_json::json!({
            "thread": {
                "id": "th-1",
                "projectId": "pr-1",
                "title": "Resume thread",
                "providerInstanceId": "codex-1",
                "modelSelection": { "instanceId": "codex-1", "model": "gpt-5" },
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "branch": null,
                "worktreePath": null,
                "lineage": {
                    "parentThreadId": null,
                    "relationshipToParent": null,
                    "rootThreadId": "th-1"
                },
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-01T00:03:00Z",
                "archivedAt": null
            },
            "runs": [
                {
                    "id": "run-1",
                    "ordinal": 1,
                    "providerInstanceId": "codex-1",
                    "modelSelection": { "instanceId": "codex-1", "model": "gpt-5" },
                    "userMessageId": "msg-1",
                    "rootNodeId": null,
                    "status": "interrupted",
                    "requestedAt": "2026-01-01T00:00:00Z",
                    "startedAt": "2026-01-01T00:00:01Z",
                    "completedAt": "2026-01-01T00:01:00Z"
                },
                {
                    "id": "run-2",
                    "ordinal": 2,
                    "providerInstanceId": "codex-1",
                    "modelSelection": { "instanceId": "codex-1", "model": "gpt-5" },
                    "userMessageId": "msg-2",
                    "rootNodeId": null,
                    "status": "queued",
                    "queueHeld": true,
                    "requestedAt": "2026-01-01T00:02:00Z",
                    "startedAt": null,
                    "completedAt": null
                }
            ],
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": [],
            "providerThreads": [],
            "providerTurns": [],
            "runtimeRequests": [{ "id": "req-1", "status": "pending", "kind": "command_approval" }],
            "messages": [],
            "plans": [],
            "turnItems": [],
            "checkpointScopes": [],
            "checkpoints": [],
            "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": [],
            "updatedAt": "2026-01-01T00:03:00Z"
        }))
        .expect("valid projection");

        // A pending runtime request blocks continuing the interrupted run even if the queue is held.
        assert!(!can_resume_thread(&projection));
        projection.runtime_requests[0].value["status"] = serde_json::json!("resolved");
        assert!(can_resume_thread(&projection));
    }

    #[test]
    fn resumable_run_id_hides_resume_when_provider_cannot_continue_accepted_run() {
        // Provider with supportsNativeResume=false cannot continue an already-accepted interrupted run (`pingdotgg/t3code#39229`).
        let mut projection: ThreadProjection = serde_json::from_value(serde_json::json!({
            "thread": {
                "id": "t-1", "projectId": "p-1", "title": "T", "providerInstanceId": "claude-1",
                "modelSelection": { "instanceId": "claude-1", "model": "claude-sonnet-4-6" },
                "runtimeMode": "full-access", "interactionMode": "default", "branch": null,
                "worktreePath": null, "activeProviderThreadId": null, "historyOrigin": "native",
                "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t-1", "sideChat": false },
                "forkedFrom": null, "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "archivedAt": null, "settledOverride": null, "settledAt": null, "lastVisitedAt": null, "deletedAt": null
            },
            "runs": [{
                "id": "run-1", "ordinal": 1, "providerInstanceId": "claude-1",
                "modelSelection": { "instanceId": "claude-1", "model": "claude-sonnet-4-6" },
                "providerThreadId": "pt-1", "userMessageId": "msg-1", "rootNodeId": null,
                "status": "interrupted", "requestedAt": "2026-01-01T00:00:00Z",
                "startedAt": "2026-01-01T00:00:01Z", "completedAt": "2026-01-01T00:01:00Z"
            }],
            "attempts": [{
                "id": "att-1", "runId": "run-1", "rootNodeId": "node-1",
                "attemptOrdinal": 1, "reason": "initial", "status": "interrupted", "completedAt": "2026-01-01T00:01:00Z"
            }],
            "nodes": [], "subagents": [],
            "providerSessions": [{
                "id": "ps-1", "providerInstanceId": "claude-1",
                "capabilities": { "turns": { "supportsNativeResume": false } }
            }],
            "providerThreads": [{ "id": "pt-1", "providerSessionId": "ps-1" }],
            "providerTurns": [{ "id": "turn-1", "runAttemptId": "att-1" }],
            "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
            "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
            "visibleTurnItems": [], "updatedAt": "2026-01-01T00:01:00Z"
        }))
        .expect("valid projection");

        assert_eq!(resumable_run_id(&projection), None);

        // If the provider never received the attempt (no providerTurn for att-1), manual continuation can still resend it.
        projection.provider_turns.clear();
        assert_eq!(resumable_run_id(&projection).as_deref(), Some("run-1"));

        // If a later steering_restart attempt wasn't accepted after an accepted attempt, it is resumable again.
        projection.provider_turns.push(crate::thread::Entity {
            id: "turn-1".into(),
            value: serde_json::json!({ "id": "turn-1", "runAttemptId": "att-1" }),
        });
        projection.attempts.push(crate::thread::RunAttempt {
            id: "att-2".into(),
            run_id: "run-1".into(),
            root_node_id: "node-2".into(),
            attempt_ordinal: 2,
            reason: Some("steering_restart".into()),
            status: "interrupted".into(),
            completed_at: Some("2026-01-01T00:01:30Z".into()),
        });
        assert_eq!(resumable_run_id(&projection).as_deref(), Some("run-1"));
    }
}
