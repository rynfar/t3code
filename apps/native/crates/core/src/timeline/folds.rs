//! Entry predicates and the turn and attempt folds of `MessagesTimeline.logic.ts`
//! (`deriveTurnFolds`, `deriveSupersededAttemptFolds`, `liveSubagentCardEntryIds`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::activity::ThreadActivity;
use super::presentation::{work_entry_display_indicates_tool_failure, work_log_entry_is_tool_like};
use super::{ChatMessage, EntryKind, Role, SourceActivity, TimelineEntry, Tone, ToolStatus, WorkLogEntry, format_duration, parse_iso_ms};
use crate::turn_item::{TurnItem, TurnItemKind};

const COMPACTION_TEXT: &str = "/compact";

impl TimelineEntry {
    pub(super) fn work(&self) -> Option<&Arc<WorkLogEntry>> {
        match &self.kind {
            EntryKind::Work(entry) => Some(entry),
            _ => None,
        }
    }

    pub(super) fn message(&self) -> Option<&Arc<ChatMessage>> {
        match &self.kind {
            EntryKind::Message(message) => Some(message),
            _ => None,
        }
    }

    pub(super) fn event(&self) -> Option<&Arc<TurnItem>> {
        match &self.kind {
            EntryKind::Event(item) => Some(item),
            _ => None,
        }
    }
}

pub(super) fn is_user(entry: &TimelineEntry) -> bool {
    entry.message().is_some_and(|m| m.role == Role::User)
}

pub(super) fn entry_run_id(entry: &TimelineEntry) -> Option<&str> {
    match &entry.kind {
        EntryKind::Message(m) if m.role == Role::Assistant => m.run_id.as_deref(),
        EntryKind::ProposedPlan(plan) => plan.run_id.as_deref(),
        EntryKind::Work(work) => work.run_id.as_deref(),
        _ => None,
    }
}

pub(super) fn is_steer(entry: &TimelineEntry) -> bool {
    entry.message().is_some_and(|m| m.role == Role::User && m.is_steer())
}

pub(super) fn starts_response(entry: &TimelineEntry) -> bool {
    (is_user(entry) && !is_steer(entry)) || entry.work().is_some_and(|w| w.item_type == Some("notification"))
}

pub(super) fn entry_end(entry: &TimelineEntry) -> &str {
    entry.message().map_or(entry.created_at.as_str(), |m| m.updated_at.as_str())
}

pub(super) fn is_compaction(entry: &TimelineEntry) -> bool {
    entry.work().is_some_and(|w| w.source_activity == Some(SourceActivity::Compaction))
}

pub(super) fn boundary_run_id(entry: &TimelineEntry) -> Option<&str> {
    match &entry.kind {
        EntryKind::Message(m) => m.run_id.as_deref(),
        EntryKind::Work(w) => w.run_id.as_deref(),
        _ => None,
    }
}

pub(super) fn is_persistent_resource(entry: &TimelineEntry) -> bool {
    entry.event().is_some_and(|item| {
        matches!(item.kind, TurnItemKind::Fork { .. } | TurnItemKind::ThreadCreated { .. } | TurnItemKind::SecretRequest { .. } | TurnItemKind::LinkRequest { .. })
    })
}

pub(super) fn is_subagent(item: &TurnItem) -> bool {
    matches!(item.kind, TurnItemKind::Subagent { .. })
}

pub(super) fn is_compaction_command(message: &ChatMessage) -> bool {
    message.role == Role::User && message.text.trim().eq_ignore_ascii_case(COMPACTION_TEXT)
}

pub(super) fn failed_item(entry: &TimelineEntry) -> Option<&TurnItem> {
    let item = match &entry.kind {
        EntryKind::Event(item) => Some(item.as_ref()),
        EntryKind::Work(work) => work.item.as_deref(),
        _ => None,
    }?;
    (matches!(item.kind, TurnItemKind::Error { .. }) && item.status == "failed" && item.parent_item_id.is_none()).then_some(item)
}

pub(super) fn elapsed_ms(start: &str, end: &str) -> Option<f64> {
    Some((parse_iso_ms(end)? - parse_iso_ms(start)?).max(0) as f64)
}

pub(super) fn max_iso<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => match (parse_iso_ms(a), parse_iso_ms(b)) {
            (None, _) => Some(b),
            (_, None) => Some(a),
            (Some(x), Some(y)) => Some(if y > x { b } else { a }),
        },
    }
}

pub(super) fn work_group_id(entry_id: &str) -> String {
    format!("work-group:{entry_id}")
}

pub(super) fn is_active_turn_activity(entry: &WorkLogEntry) -> bool {
    entry.status == Some(ToolStatus::InProgress)
        || (entry.status.is_none() && (entry.source_activity == Some(SourceActivity::TaskProgress) || work_log_entry_is_tool_like(entry)))
}

pub(super) fn is_pending_approval(entry: &WorkLogEntry, pending: &HashSet<String>) -> bool {
    matches!(entry.item.as_ref().map(|i| &i.kind), Some(TurnItemKind::ApprovalRequest { request_id, .. }) if pending.contains(request_id))
}

pub(super) fn breaks_group(entry: &WorkLogEntry) -> bool {
    (entry.item_type == Some("error") && entry.status == Some(ToolStatus::Failed))
        || entry.tone == Tone::Error
        || entry.source_activity == Some(SourceActivity::RuntimeError)
        || matches!(entry.item_type, Some("system_notice" | "notification"))
        || entry.question_answer.is_some()
}

pub(super) fn terminal_assistant_ids(entries: &[Arc<TimelineEntry>]) -> HashSet<String> {
    let mut last_by_key: HashMap<String, String> = HashMap::new();
    let mut unkeyed = 0;
    for message in entries.iter().filter_map(|e| e.message()) {
        if message.role == Role::User {
            unkeyed += 1;
            continue;
        }
        let key = message.run_id.as_ref().map_or_else(|| format!("unkeyed:{unkeyed}"), |run| format!("turn:{run}"));
        last_by_key.insert(key, message.id.clone());
    }
    last_by_key.into_values().collect()
}

pub(super) fn unsettled_run_id(activity: &ThreadActivity) -> Option<String> {
    if let Some(running) = &activity.running_run_id {
        return Some(running.clone());
    }
    let run = activity.latest_run.as_ref()?;
    let settled = run.completed_at.is_some() && !matches!(run.status.as_str(), "running" | "starting" | "waiting");
    (!settled).then(|| run.run_id.clone())
}

pub(super) fn live_subagent_entry_ids(entries: &[Arc<TimelineEntry>]) -> HashSet<String> {
    let mut visible = HashSet::new();
    let mut batch: Vec<&TimelineEntry> = Vec::new();
    let flush = |batch: &mut Vec<&TimelineEntry>, visible: &mut HashSet<String>| {
        if batch.iter().any(|e| e.event().is_some_and(|item| is_subagent(item) && item.is_running())) {
            visible.extend(batch.iter().map(|e| e.id.clone()));
        }
        batch.clear();
    };
    for entry in entries {
        let Some(item) = entry.event().filter(|item| is_subagent(item)) else {
            flush(&mut batch, &mut visible);
            continue;
        };
        if let Some(previous) = batch.last().and_then(|e| e.event())
            && (previous.run_id != item.run_id || previous.provider_turn_id != item.provider_turn_id)
        {
            flush(&mut batch, &mut visible);
        }
        batch.push(entry);
    }
    flush(&mut batch, &mut visible);
    visible
}

pub(super) fn fold_run_id(entry: &TimelineEntry, runless: Option<&str>) -> Option<String> {
    match &entry.kind {
        EntryKind::Work(w) if w.item_type == Some("system_notice") => None,
        EntryKind::Message(m) if m.role == Role::Assistant => m.run_id.as_deref().or(runless).map(str::to_owned),
        EntryKind::Work(w) => w.run_id.as_deref().or(runless).map(str::to_owned),
        EntryKind::Event(item) if is_persistent_resource(entry) || is_subagent(item) => item.run_id.as_deref().or(runless).map(str::to_owned),
        _ => None,
    }
}

pub(super) struct SupersededFold {
    pub(super) attempt_id: String,
    pub(super) created_at: String,
    pub(super) hidden: HashSet<String>,
}

pub(super) fn superseded_attempt_folds(
    entries: &[Arc<TimelineEntry>],
    unfolded: &HashSet<String>,
    live_subagents: &HashSet<String>,
) -> HashMap<String, SupersededFold> {
    let mut order: Vec<String> = Vec::new();
    let mut by_attempt: HashMap<String, Vec<&TimelineEntry>> = HashMap::new();
    for entry in entries {
        let Some(attempt) = entry.attempt.as_ref().filter(|a| a.status == "superseded") else { continue };
        if unfolded.contains(&attempt.run_id)
            || is_user(entry)
            || entry.event().is_some_and(|item| matches!(item.kind, TurnItemKind::Handoff { .. }))
            || is_persistent_resource(entry)
            || live_subagents.contains(&entry.id)
            || entry.work().is_some_and(|w| w.item_type == Some("system_notice"))
        {
            continue;
        }
        let group = by_attempt.entry(attempt.id.clone()).or_default();
        if group.is_empty() {
            order.push(attempt.id.clone());
        }
        group.push(entry);
    }
    let mut folds = HashMap::new();
    for attempt_id in order {
        let Some(group) = by_attempt.get(&attempt_id) else { continue };
        let Some(first) = group.first() else { continue };
        folds.insert(
            first.id.clone(),
            SupersededFold { attempt_id: attempt_id.clone(), created_at: first.created_at.clone(), hidden: group.iter().map(|e| e.id.clone()).collect() },
        );
    }
    folds
}

pub(super) fn active_visual_response_run_ids(entries: &[Arc<TimelineEntry>], unsettled: Option<&str>, is_working: bool) -> HashSet<String> {
    let mut runs = HashSet::new();
    let Some(unsettled) = unsettled else { return runs };
    runs.insert(unsettled.to_owned());
    if !is_working {
        return runs;
    }
    let boundary = entries.iter().rposition(|e| starts_response(e)).map_or(0, |i| i + 1);
    runs.extend(entries[boundary..].iter().filter_map(|e| entry_run_id(e)).map(str::to_owned));
    runs
}

pub(super) struct TurnFold {
    pub(super) run_id: String,
    pub(super) created_at: String,
    pub(super) hidden: HashSet<String>,
    pub(super) members: HashSet<String>,
    pub(super) label: String,
}

pub(super) struct TurnGroup<'a> {
    entries: Vec<&'a TimelineEntry>,
    terminal: Option<&'a TimelineEntry>,
    streaming: bool,
    start: Option<String>,
    anchor: String,
    steers: Vec<(usize, &'a TimelineEntry)>,
}

pub(super) fn turn_folds(
    entries: &[Arc<TimelineEntry>],
    terminal_ids: &HashSet<String>,
    activity: &ThreadActivity,
    unfolded: &HashSet<String>,
    live_subagents: &HashSet<String>,
) -> HashMap<String, TurnFold> {
    let mut interrupted: HashSet<&str> = HashSet::new();
    let mut prompt_index: HashMap<&str, usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if starts_response(entry)
            && let Some(run) = boundary_run_id(entry)
        {
            prompt_index.entry(run).or_insert(index);
        }
        if let Some(item) = entry.event()
            && matches!(item.kind, TurnItemKind::RunInterruptRequest { .. } | TurnItemKind::RunInterruptResult { .. })
            && let Some(run) = item.run_id.as_deref()
        {
            interrupted.insert(run);
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, TurnGroup> = HashMap::new();
    let mut runless_failed: HashSet<String> = HashSet::new();
    let mut runless: Option<String> = None;
    let mut response: Option<String> = None;
    let mut pending: Option<(String, String)> = None;
    for (index, entry) in entries.iter().enumerate() {
        if starts_response(entry) {
            pending = entries.get(index + 1).map(|next| (entry.created_at.clone(), next.id.clone()));
            let boundary = boundary_run_id(entry);
            runless = boundary.is_none().then(|| format!("runless:{}", entry.id));
            response = boundary.map(str::to_owned).or_else(|| runless.clone());
            continue;
        }
        if is_steer(entry) {
            let steer_run = entry.message().and_then(|m| m.run_id.clone()).or_else(|| response.clone());
            if let Some(group) = steer_run.and_then(|run| groups.get_mut(&run))
                && !group.entries.is_empty()
            {
                if group.steers.last().is_some_and(|(start, _)| *start == group.entries.len()) {
                    group.steers.pop();
                }
                group.steers.push((group.entries.len(), entry));
            }
            continue;
        }
        let Some(own) = fold_run_id(entry, runless.as_deref()) else { continue };
        let run_id = match &response {
            Some(response) if prompt_index.get(own.as_str()).is_some_and(|i| *i > index) => response.clone(),
            _ => own,
        };
        if Some(&run_id) == runless.as_ref() && failed_item(entry).is_some() {
            runless_failed.insert(run_id.clone());
        }
        let group = groups.entry(run_id.clone()).or_insert_with(|| {
            order.push(run_id.clone());
            let (start, anchor) = pending.take().map_or((None, entry.id.clone()), |(at, anchor)| (Some(at), anchor));
            TurnGroup { entries: Vec::new(), terminal: None, streaming: false, start, anchor, steers: Vec::new() }
        });
        group.entries.push(entry);
        if let Some(message) = entry.message() {
            if terminal_ids.contains(&message.id) {
                group.terminal = Some(entry);
            }
            group.streaming |= message.streaming;
        }
    }
    let mut folds = HashMap::new();
    for run_id in order {
        let Some(group) = groups.get(&run_id) else { continue };
        if unfolded.contains(&run_id) || interrupted.contains(run_id.as_str()) || runless_failed.contains(&run_id) || group.streaming {
            continue;
        }
        let terminal_index = group.terminal.and_then(|t| group.entries.iter().position(|e| e.id == t.id)).unwrap_or(group.entries.len());
        let mut hidden: HashSet<String> = HashSet::new();
        for (index, entry) in group.entries.iter().enumerate() {
            if group.terminal.is_some_and(|t| t.id == entry.id) {
                continue;
            }
            let compaction = is_compaction(entry);
            let foldable_trailing = entry
                .work()
                .is_some_and(|w| w.status != Some(ToolStatus::InProgress) && !work_entry_display_indicates_tool_failure(w));
            if !compaction && index > terminal_index && !foldable_trailing {
                continue;
            }
            if compaction || is_persistent_resource(entry) || live_subagents.contains(&entry.id) {
                continue;
            }
            if entry.work().is_some_and(|w| w.item_type == Some("notification") || w.question_answer.is_some()) {
                continue;
            }
            hidden.insert(entry.id.clone());
        }
        let hides_work = |entries: &[&TimelineEntry]| entries.iter().any(|e| hidden.contains(&e.id) && !is_compaction(e));
        if hidden.is_empty() || !hides_work(&group.entries) {
            continue;
        }
        let (Some(first), Some(last)) = (group.entries.first(), group.entries.last()) else { continue };
        let latest = activity.latest_run.as_ref().filter(|run| run.run_id == run_id);
        let latest_interrupted = latest.is_some_and(|run| run.status == "interrupted");
        let last_end = entry_end(last);
        let elapsed = match latest.and_then(|run| run.started_at.as_deref().zip(run.completed_at.as_deref())) {
            Some((start, end)) => elapsed_ms(start, end),
            None => elapsed_ms(
                group.start.as_deref().unwrap_or(&first.created_at),
                max_iso(group.terminal.and_then(|t| t.message()).map(|m| m.updated_at.as_str()), Some(last_end)).unwrap_or(last_end),
            ),
        };
        let duration = elapsed.map(format_duration);
        let label = match (latest_interrupted, duration) {
            (true, Some(d)) => format!("You stopped after {d}"),
            (true, None) => "You stopped this response".into(),
            (false, Some(d)) => format!("Worked for {d}"),
            (false, None) => "Worked".into(),
        };
        let segment_hidden = |entries: &[&TimelineEntry]| entries.iter().filter(|e| hidden.contains(&e.id)).map(|e| e.id.clone()).collect();
        let first_segment = &group.entries[..group.steers.first().map_or(group.entries.len(), |(start, _)| *start)];
        folds.insert(
            group.anchor.clone(),
            TurnFold {
                run_id: run_id.clone(),
                created_at: group.start.clone().unwrap_or_else(|| first.created_at.clone()),
                hidden: segment_hidden(first_segment),
                members: first_segment.iter().map(|e| e.id.clone()).collect(),
                label,
            },
        );
        for (index, (start, steer)) in group.steers.iter().enumerate() {
            let end = group.steers.get(index + 1).map_or(group.entries.len(), |(next, _)| *next);
            let segment = &group.entries[*start..end];
            let Some(anchor) = segment.first() else { continue };
            if !hides_work(segment) {
                continue;
            }
            let segment_end = segment.iter().fold(None, |acc, e| max_iso(acc, Some(entry_end(e)))).unwrap_or(&steer.created_at);
            let label = match elapsed_ms(&steer.created_at, segment_end) {
                Some(ms) => format!("Continued for {}", format_duration(ms)),
                None => "Continued".into(),
            };
            folds.insert(
                anchor.id.clone(),
                TurnFold {
                    run_id: format!("{run_id}:steer:{}", steer.id),
                    created_at: steer.created_at.clone(),
                    hidden: segment_hidden(segment),
                    members: segment.iter().map(|e| e.id.clone()).collect(),
                    label,
                },
            );
        }
    }
    folds
}
