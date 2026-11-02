//! Timeline entries to rows: `deriveMessagesTimelineRows`
//! (`apps/web/src/components/chat/MessagesTimeline.logic.ts`). Not ported yet: worktree-setup
//! rows, HTML/MCP-app frames, revert targets, per-turn diff summaries, deferred-work loading,
//! idle-resume compaction titles and the T3 delegate-row filter.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::activity::{ThreadActivity, user_message_attribution};
use super::presentation::{
    single_tool_call_label, summarize_tool_group, tool_group_action, tool_group_summary_kind, work_entry_display_indicates_tool_failure,
    work_entry_indicates_tool_success, work_entry_is_visible_in_group, work_log_entry_is_tool_like, ToolGroupAction,
};
use super::compaction::{Compaction, compacting_label, compaction_title};
use super::folds::{
    FoldInput, active_visual_response_run_ids, breaks_group, complete_superseded_reply_ids, failed_item, is_active_turn_activity,
    is_compaction_command, is_pending_approval, is_subagent, live_subagent_entry_ids, starts_response, superseded_attempt_folds,
    terminal_assistant_ids, turn_folds, unsettled_run_id, work_group_id,
};
use super::presentation::compaction_tokens;
use super::{ChatMessage, EntryKind, Role, RowKind, SourceActivity, TimelineEntry, TimelineRow, Tone, ToolStatus, WorkLogEntry, rows_equal};
use crate::turn_item::{TurnItem, TurnItemKind};

const LIVE_ACTIVITY_ROW_ID: &str = "live-activity-row";

pub struct RowsInput<'a> {
    pub entries: &'a [Arc<TimelineEntry>],
    pub activity: &'a ThreadActivity,
    pub expanded_run_ids: &'a HashSet<String>,
    pub expanded_attempt_ids: &'a HashSet<String>,
    pub expanded_work_group_ids: &'a HashSet<String>,
}

/// Keeps settled copies of superseded "Thinking" entries stable across derives, as the web's
/// `WeakMap` does, so their rows compare equal.
#[derive(Default)]
pub struct RowsCache {
    settled: HashMap<usize, (Arc<TimelineEntry>, Arc<TimelineEntry>)>,
    previous: Option<Previous>,
}

struct Previous {
    entries: Vec<Arc<TimelineEntry>>,
    activity: ThreadActivity,
    expanded: [HashSet<String>; 3],
    rows: Vec<Arc<TimelineRow>>,
}

/// A streamed token changes one assistant message and nothing else, so the previous rows hold
/// with that message swapped in: `replaceStreamingMessageRows`. Patches `previous` in place and
/// reports whether it could.
fn patch_streaming_rows(input: &RowsInput, previous: &mut Previous) -> bool {
    if input.entries.len() != previous.entries.len()
        || *input.activity != previous.activity
        || *input.expanded_run_ids != previous.expanded[0]
        || *input.expanded_attempt_ids != previous.expanded[1]
        || *input.expanded_work_group_ids != previous.expanded[2]
    {
        return false;
    }
    let mut replacements: Vec<(usize, Arc<ChatMessage>)> = Vec::new();
    for (index, (next, before)) in input.entries.iter().zip(&previous.entries).enumerate() {
        if Arc::ptr_eq(next, before) {
            continue;
        }
        let (Some(new), Some(old)) = (next.message(), before.message()) else { return false };
        let streaming_text_only = new.role == Role::Assistant
            && old.role == Role::Assistant
            && new.streaming
            && old.streaming
            && new.id == old.id
            && new.run_id == old.run_id
            && next.id == before.id
            && next.created_at == before.created_at
            && next.attempt == before.attempt;
        if !streaming_text_only {
            return false;
        }
        replacements.push((index, new.clone()));
    }
    for (index, message) in replacements {
        let old = previous.entries[index].message().map(|m| Arc::as_ptr(m) as usize);
        previous.entries[index] = input.entries[index].clone();
        for row in previous.rows.iter_mut() {
            let matches = match &row.kind {
                RowKind::Message { message, .. } | RowKind::AssistantMeta { message, .. } => Some(Arc::as_ptr(message) as usize) == old,
                _ => false,
            };
            if matches {
                let mut patched = (**row).clone();
                if let RowKind::Message { message: slot, .. } | RowKind::AssistantMeta { message: slot, .. } = &mut patched.kind {
                    *slot = message.clone();
                }
                *row = Arc::new(patched);
            }
        }
    }
    true
}

fn settle_superseded_reasoning(entries: &[Arc<TimelineEntry>], cache: &mut RowsCache) -> Vec<Arc<TimelineEntry>> {
    let mut settled = HashMap::new();
    let out = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let thinking = entry.work().filter(|w| w.item_type == Some("reasoning") && w.status == Some(ToolStatus::InProgress));
            let (Some(work), false) = (thinking, index + 1 == entries.len()) else { return entry.clone() };
            let key = Arc::as_ptr(entry) as usize;
            let copy = match cache.settled.remove(&key) {
                Some((_, copy)) => copy,
                None => {
                    let mut work = (**work).clone();
                    work.status = Some(ToolStatus::Completed);
                    work.facts = std::sync::OnceLock::new();
                    Arc::new(TimelineEntry { kind: EntryKind::Work(Arc::new(work)), ..(**entry).clone() })
                }
            };
            settled.insert(key, (entry.clone(), copy.clone()));
            copy
        })
        .collect();
    cache.settled = settled;
    out
}

fn row(id: impl Into<String>, created_at: Option<String>, kind: RowKind) -> TimelineRow {
    TimelineRow { id: id.into(), created_at, continues_work_log: false, kind }
}

fn expanded_group_row(group_id: &str, created_at: &str, entries: Vec<Arc<WorkLogEntry>>) -> TimelineRow {
    row(format!("{group_id}:details"), Some(created_at.to_owned()), RowKind::Work { entries, expanded_group: true, display_label: None })
}

/// `deriveMessagesTimelineRows`. Unchanged rows keep their `Arc` across derives
/// (`computeStableMessagesTimelineRows`), so a view can diff them by pointer.
pub fn derive_rows(input: &RowsInput, cache: &mut RowsCache) -> Vec<Arc<TimelineRow>> {
    if let Some(previous) = cache.previous.as_mut()
        && patch_streaming_rows(input, previous)
    {
        return previous.rows.clone();
    }
    let fresh = derive_all_rows(input, cache);
    let previous_rows: HashMap<&str, &Arc<TimelineRow>> =
        cache.previous.iter().flat_map(|previous| previous.rows.iter()).map(|row| (row.id.as_str(), row)).collect();
    let rows: Vec<Arc<TimelineRow>> = fresh
        .into_iter()
        .map(|row| match previous_rows.get(row.id.as_str()) {
            Some(previous) if rows_equal(previous, &row) => Arc::clone(previous),
            _ => Arc::new(row),
        })
        .collect();
    cache.previous = Some(Previous {
        entries: input.entries.to_vec(),
        activity: input.activity.clone(),
        expanded: [input.expanded_run_ids.clone(), input.expanded_attempt_ids.clone(), input.expanded_work_group_ids.clone()],
        rows: rows.clone(),
    });
    rows
}

struct FoldSetup {
    unsettled: Option<String>,
    terminal_ids: HashSet<String>,
    itemized: HashSet<String>,
    superseded: HashMap<String, super::folds::SupersededFold>,
    active_runs: HashSet<String>,
    runless: bool,
    folds: HashMap<String, super::folds::TurnFold>,
}

fn fold_setup(entries: &[Arc<TimelineEntry>], activity: &ThreadActivity) -> FoldSetup {
    let is_working = activity.is_working;
    let unsettled = unsettled_run_id(activity);
    let terminal_ids = terminal_assistant_ids(entries);
    let mut failed_runs: HashSet<String> = entries.iter().filter_map(|e| failed_item(e)?.run_id.clone()).collect();
    if let Some(run) = activity.latest_run.as_ref().filter(|r| r.status == "failed") {
        failed_runs.insert(run.run_id.clone());
    }
    let (mut usage_limited, mut other_failures) = (HashSet::new(), HashSet::new());
    for item in entries.iter().filter_map(|e| failed_item(e)) {
        if let (Some(run), TurnItemKind::Error { failure, .. }) = (&item.run_id, &item.kind) {
            if failure.class == "usage_limit" { &mut usage_limited } else { &mut other_failures }.insert(run.clone());
        }
    }
    let itemized: HashSet<String> =
        failed_runs.iter().filter(|run| !usage_limited.contains(*run) || other_failures.contains(*run)).cloned().collect();
    let live_subagents = live_subagent_entry_ids(entries);
    let complete_replies = complete_superseded_reply_ids(entries);
    let superseded = superseded_attempt_folds(entries, &failed_runs, &live_subagents, &complete_replies);
    let active_runs = active_visual_response_run_ids(entries, unsettled.as_deref(), is_working);
    let unfolded: HashSet<String> = active_runs.union(&failed_runs).cloned().collect();
    let runless = is_working && activity.runless_work_active;
    let folds = turn_folds(
        entries,
        &FoldInput {
            terminal_ids: &terminal_ids,
            activity,
            unfolded: &unfolded,
            live_subagents: &live_subagents,
            complete_replies: &complete_replies,
            runless_active: runless,
        },
    );
    FoldSetup { unsettled, terminal_ids, itemized, superseded, active_runs, runless, folds }
}

/// The fold hiding each entry, so find can open it: `timelineTurnFoldRunIdsByEntryId`, plus the
/// superseded-attempt folds (entry id to attempt id).
pub fn fold_membership(entries: &[Arc<TimelineEntry>], activity: &ThreadActivity) -> (HashMap<String, String>, HashMap<String, String>) {
    let setup = fold_setup(entries, activity);
    let mut runs = HashMap::new();
    for fold in setup.folds.values() {
        for id in &fold.hidden {
            runs.insert(id.clone(), fold.run_id.clone());
        }
    }
    let mut attempts = HashMap::new();
    for fold in setup.superseded.values() {
        for id in &fold.hidden {
            attempts.insert(id.clone(), fold.attempt_id.clone());
        }
    }
    (runs, attempts)
}

fn derive_all_rows(input: &RowsInput, cache: &mut RowsCache) -> Vec<TimelineRow> {
    let entries = settle_superseded_reasoning(input.entries, cache);
    let entries = entries.as_slice();
    let activity = input.activity;
    let pending = &activity.pending_approval_ids;
    let is_working = activity.is_working;
    let FoldSetup { unsettled, terminal_ids, itemized, superseded, active_runs, runless, folds } = fold_setup(entries, activity);

    let mut collapsed: HashSet<String> = HashSet::new();
    let mut collapsed_members: HashSet<String> = HashSet::new();
    for fold in folds.values().filter(|f| !input.expanded_run_ids.contains(&f.run_id)) {
        collapsed.extend(fold.hidden.iter().cloned());
        collapsed_members.extend(fold.members.iter().cloned());
    }
    let mut collapsed_superseded: HashSet<String> = HashSet::new();
    for fold in superseded.values().filter(|f| !input.expanded_attempt_ids.contains(&f.attempt_id)) {
        collapsed_superseded.extend(fold.hidden.iter().cloned());
    }
    for entry in entries {
        if entry.work().is_some_and(|w| is_pending_approval(w, pending)) {
            collapsed.remove(&entry.id);
            collapsed_superseded.remove(&entry.id);
        }
    }
    let run_is_active = |run: Option<&str>| match run {
        Some(run) => active_runs.contains(run),
        None => runless,
    };
    let in_active_run = |entry: &WorkLogEntry| {
        is_working
            && entry.status == Some(ToolStatus::InProgress)
            && match &entry.run_id {
                Some(_) => entry.run_id == unsettled,
                None => runless,
            }
    };
    let header_index = if is_working { entries.iter().rposition(|e| starts_response(e)).map_or(0, |i| i + 1) } else { entries.len() };

    let mut active_tools: Vec<&Arc<TimelineEntry>> = Vec::new();
    if is_working && (unsettled.is_some() || runless) {
        let mut tail_attempt: Option<Option<&str>> = None;
        for entry in entries[header_index.min(entries.len())..].iter().rev() {
            let Some(work) = entry.work() else { break };
            if work.tone == Tone::Error
                || work.source_activity == Some(SourceActivity::RuntimeError)
                || matches!(work.item_type, Some("system_notice" | "notification"))
                || work.question_answer.is_some()
                || is_pending_approval(work, pending)
                || !run_is_active(work.run_id.as_deref())
                || work.source_activity == Some(SourceActivity::Compaction)
                || collapsed.contains(&entry.id)
                || collapsed_superseded.contains(&entry.id)
                || folds.contains_key(&entry.id)
                || superseded.contains_key(&entry.id)
            {
                break;
            }
            let attempt = entry.attempt.as_ref().map(|a| a.id.as_str());
            match tail_attempt {
                None => tail_attempt = Some(attempt),
                Some(tail) if tail != attempt => break,
                _ => {}
            }
            active_tools.push(entry);
        }
    }
    active_tools.reverse();
    let visible_active: Vec<&Arc<TimelineEntry>> =
        active_tools.iter().copied().filter(|e| e.work().is_some_and(|w| work_entry_is_visible_in_group(w, true))).collect();
    let anchor = active_tools.first().copied();
    let latest_visible = visible_active.last().copied();
    let latest_running = visible_active.iter().rev().copied().find(|e| e.work().is_some_and(|w| is_active_turn_activity(w)));
    let keeps_live = latest_running.is_some()
        || (latest_visible.is_some_and(|e| e.work().is_some_and(|w| work_entry_indicates_tool_success(w))) && pending.is_empty());
    let latest_failed = latest_running.is_none()
        && latest_visible.and_then(|e| e.work()).is_some_and(|w| w.status != Some(ToolStatus::Declined) && work_entry_display_indicates_tool_failure(w));
    let latest_thought = visible_active
        .iter()
        .rev()
        .find_map(|e| e.work().filter(|w| w.item_type == Some("reasoning") && w.detail.as_deref().is_some_and(|d| !d.trim().is_empty())));
    let active_work_row = match (anchor, latest_visible) {
        (Some(anchor), Some(latest)) if !latest_failed => {
            let group_id = work_group_id(&anchor.id);
            let entry = latest_running.unwrap_or(latest).work().cloned();
            entry.map(|entry| {
                row(
                    if keeps_live { LIVE_ACTIVITY_ROW_ID.to_owned() } else { format!("work-live:{}", anchor.id) },
                    Some(anchor.created_at.clone()),
                    RowKind::WorkLive {
                        entry,
                        entries: visible_active.iter().filter_map(|e| e.work().cloned()).collect(),
                        expanded: input.expanded_work_group_ids.contains(&group_id),
                        group_id,
                        active: keeps_live,
                        thought: latest_thought.cloned(),
                    },
                )
            })
        }
        _ => None,
    };
    let placement = latest_visible.map(|e| e.id.clone());
    let active_entry_ids: HashSet<String> =
        if active_work_row.is_some() || latest_failed { active_tools.iter().map(|e| e.id.clone()).collect() } else { HashSet::new() };

    let compaction = Compaction::derive(entries, is_working, unsettled.as_deref(), &in_active_run);

    let mut rows: Vec<TimelineRow> = Vec::new();
    let mut has_activity_row = false;
    let working_row = || {
        compaction
            .working_row()
            .map(|compacting| row("working-indicator-row", activity.active_turn_started_at.clone(), RowKind::Working { compacting }))
    };
    let mut index = 0;
    while index < entries.len() {
        let entry = &entries[index];
        if is_working && index == header_index {
            rows.extend(working_row());
        }
        if Some(&entry.id) == placement.as_ref()
            && let Some(live) = &active_work_row
        {
            rows.push(live.clone());
            if let RowKind::WorkLive { active, expanded, group_id, entries: group, .. } = &live.kind {
                has_activity_row |= *active;
                if *expanded {
                    rows.push(expanded_group_row(group_id, live.created_at.as_deref().unwrap_or_default(), group.clone()));
                }
            }
        }
        if entry.event().is_some_and(|item| {
            matches!(item.kind, TurnItemKind::RunInterruptRequest { .. })
                || (matches!(item.kind, TurnItemKind::RunInterruptResult { .. })
                    && item.run_id.as_ref().is_some_and(|run| compaction.runs.contains_key(run) && compaction.compacted.contains(run)))
        }) {
            index += 1;
            continue;
        }
        if let Some(fold) = folds.get(&entry.id) {
            rows.push(row(
                format!("turn-fold:{}", fold.run_id),
                Some(fold.created_at.clone()),
                RowKind::TurnFold { run_id: fold.run_id.clone(), label: fold.label.clone(), expanded: input.expanded_run_ids.contains(&fold.run_id) },
            ));
        }
        if collapsed.contains(&entry.id) {
            index += 1;
            continue;
        }
        if let Some(fold) = superseded.get(&entry.id) {
            rows.push(row(
                format!("attempt-fold:{}", fold.attempt_id),
                Some(fold.created_at.clone()),
                RowKind::AttemptFold {
                    attempt_id: fold.attempt_id.clone(),
                    label: "Superseded attempt".into(),
                    expanded: input.expanded_attempt_ids.contains(&fold.attempt_id),
                },
            ));
        }
        if collapsed_superseded.contains(&entry.id) || active_entry_ids.contains(&entry.id) {
            index += 1;
            continue;
        }
        match &entry.kind {
            EntryKind::Work(work) if work.source_activity == Some(SourceActivity::Compaction) => {
                let active = in_active_run(work);
                if active && compaction.active_run.is_some() && work.run_id == compaction.active_run {
                    index += 1;
                    continue;
                }
                let (status, before, after, summary) = match work.item.as_ref().map(|i| (&i.status, &i.kind)) {
                    Some((status, TurnItemKind::Compaction { before_token_count, after_token_count, summary })) => {
                        (status.as_str(), *before_token_count, *after_token_count, summary.as_deref())
                    }
                    _ => ("completed", None, None, None),
                };
                let reason = compaction.reason(work.run_id.as_deref());
                let failed = status == "failed";
                rows.push(row(
                    entry.id.clone(),
                    Some(entry.created_at.clone()),
                    RowKind::ContextCompaction {
                        label: match reason.filter(|_| active) {
                            Some(reason) => compacting_label(reason).to_owned(),
                            None => work.label.clone(),
                        },
                        tokens: compaction_tokens(status, before, after),
                        active,
                        title: reason.map(|reason| compaction_title(reason).to_owned()),
                        failed,
                        failure: if failed { summary.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned) } else { None },
                    },
                ));
            }
            EntryKind::Work(work) => {
                if is_pending_approval(work, pending)
                    || work.run_id.as_ref().is_some_and(|run| itemized.contains(run))
                    || breaks_group(work)
                {
                    rows.push(row(entry.id.clone(), Some(entry.created_at.clone()), RowKind::Work { entries: vec![work.clone()], expanded_group: false, display_label: None }));
                    index += 1;
                    continue;
                }
                let mut group: Vec<&Arc<WorkLogEntry>> = vec![work];
                let mut cursor = index + 1;
                while let Some(next) = entries.get(cursor) {
                    let Some(next_work) = next.work() else { break };
                    if next_work.source_activity == Some(SourceActivity::Compaction)
                        || breaks_group(next_work)
                        || is_pending_approval(next_work, pending)
                        || active_entry_ids.contains(&next.id)
                        || collapsed.contains(&next.id)
                        || collapsed_superseded.contains(&next.id)
                        || folds.contains_key(&next.id)
                        || superseded.contains_key(&next.id)
                        || next_work.run_id != work.run_id
                        || next.attempt.as_ref().map(|a| &a.id) != entry.attempt.as_ref().map(|a| &a.id)
                    {
                        break;
                    }
                    group.push(next_work);
                    cursor += 1;
                }
                let visible: Vec<Arc<WorkLogEntry>> =
                    group.into_iter().filter(|w| work_entry_is_visible_in_group(w, in_active_run(w))).cloned().collect();
                if !visible.is_empty() {
                    push_work_group(&mut rows, &mut has_activity_row, entry, visible, &in_active_run, input.expanded_work_group_ids);
                }
                index = cursor;
                continue;
            }
            EntryKind::ProposedPlan(plan) => rows.push(row(entry.id.clone(), Some(entry.created_at.clone()), RowKind::ProposedPlan(plan.clone()))),
            EntryKind::Event(item) => {
                if let Some(previous) = rows.last_mut()
                    && let RowKind::Event { item: previous_item, subagents, .. } = &mut previous.kind
                    && is_subagent(item)
                    && is_subagent(previous_item)
                    && previous_item.run_id == item.run_id
                    && previous_item.provider_turn_id == item.provider_turn_id
                {
                    if subagents.is_empty() {
                        subagents.push(previous_item.clone());
                    }
                    subagents.push(item.clone());
                } else {
                    rows.push(row(entry.id.clone(), Some(entry.created_at.clone()), RowKind::Event { item: item.clone(), subagents: Vec::new(), resource_summary: false }));
                }
            }
            EntryKind::Message(message) => {
                if let Some(run) = message.run_id.as_deref().filter(|_| is_compaction_command(message)) {
                    if !compaction.compacted.contains(run) && compaction.active_run.as_deref() != Some(run) {
                        let pending_compaction = compaction.running_run.as_deref() == Some(run);
                        let label = match (pending_compaction, compaction.reason(Some(run))) {
                            (true, Some(reason)) => compacting_label(reason).to_owned(),
                            (true, None) => "Compacting context".to_owned(),
                            (false, _) => "Context compaction".to_owned(),
                        };
                        rows.push(row(
                            format!("compaction:{run}"),
                            Some(entry.created_at.clone()),
                            RowKind::ContextCompaction {
                                label,
                                tokens: None,
                                active: pending_compaction,
                                title: None,
                                failed: false,
                                failure: None,
                            },
                        ));
                    }
                    index += 1;
                    continue;
                }
                let in_progress = message.role == Role::Assistant && run_is_active(message.run_id.as_deref());
                let terminal = message.role == Role::Assistant && terminal_ids.contains(&message.id);
                let show_meta = terminal && !in_progress;
                rows.push(row(
                    entry.id.clone(),
                    Some(entry.created_at.clone()),
                    RowKind::Message {
                        message: message.clone(),
                        show_assistant_meta: show_meta,
                        show_assistant_timestamp: terminal && in_progress && !message.streaming,
                        show_copy: show_meta,
                        copy_streaming: message.streaming || in_progress,
                        grouped_with_next: false,
                        grouped_with_previous: false,
                        continues_steer: false,
                    },
                ));
            }
        }
        index += 1;
    }

    if is_working && !rows.iter().any(|r| matches!(r.kind, RowKind::Working { .. })) && header_index >= entries.len() {
        rows.extend(working_row());
    }
    let turn_ended = activity.latest_run.as_ref().is_some_and(|run| run.turn_ended);
    if is_working
        && !turn_ended
        && !compaction.active
        && !compaction.only_run_finished
        && (latest_failed || (!has_activity_row && pending.is_empty()))
    {
        match anchor.filter(|_| latest_failed) {
            Some(anchor) => {
                let group_id = work_group_id(&anchor.id);
                let expanded = input.expanded_work_group_ids.contains(&group_id);
                rows.push(row(LIVE_ACTIVITY_ROW_ID, activity.active_turn_started_at.clone(), RowKind::Thinking { group_id: Some(group_id.clone()), expanded }));
                if expanded {
                    rows.push(expanded_group_row(&group_id, &anchor.created_at, visible_active.iter().filter_map(|e| e.work().cloned()).collect()));
                }
            }
            None => rows.push(row(LIVE_ACTIVITY_ROW_ID, activity.active_turn_started_at.clone(), RowKind::Thinking { group_id: None, expanded: false })),
        }
    }
    let rows = attach_trailing_tool_groups(attach_created_thread_summaries(rows, entries, &collapsed_members));
    finalize_grouping(rows)
}

fn push_work_group(
    rows: &mut Vec<TimelineRow>,
    has_activity_row: &mut bool,
    entry: &TimelineEntry,
    visible: Vec<Arc<WorkLogEntry>>,
    in_active_run: &dyn Fn(&WorkLogEntry) -> bool,
    expanded_groups: &HashSet<String>,
) {
    let group_id = work_group_id(&entry.id);
    let expanded = expanded_groups.contains(&group_id);
    let created_at = Some(entry.created_at.clone());
    if let Some(latest) = visible.iter().rev().find(|w| in_active_run(w)) {
        rows.push(row(
            format!("work-live:{}", entry.id),
            created_at,
            RowKind::WorkLive { entry: latest.clone(), entries: visible.clone(), group_id: group_id.clone(), expanded, active: true, thought: None },
        ));
        *has_activity_row = true;
        if expanded {
            rows.push(expanded_group_row(&group_id, &entry.created_at, visible));
        }
        return;
    }
    let refs: Vec<&WorkLogEntry> = visible.iter().map(|w| w.as_ref()).collect();
    if let [single] = refs.as_slice()
        && work_log_entry_is_tool_like(single)
    {
        let label = if tool_group_action(single) == ToolGroupAction::Edit { summarize_tool_group(&refs).0 } else { single_tool_call_label(single) };
        rows.push(row(entry.id.clone(), created_at, RowKind::Work { entries: visible, expanded_group: false, display_label: Some(label) }));
        return;
    }
    let summary_kind = tool_group_summary_kind(&refs);
    let tool_surface = refs.iter().rev().find_map(|w| w.tool_surface.clone());
    let has_failure = refs.iter().rev().find(|w| work_log_entry_is_tool_like(w)).is_some_and(|w| work_entry_display_indicates_tool_failure(w));
    let summary = match refs.as_slice() {
        [single] if !work_log_entry_is_tool_like(single) => single.label.clone(),
        _ => summarize_tool_group(&refs).0,
    };
    rows.push(row(
        format!("work-toggle:{}", entry.id),
        created_at,
        RowKind::WorkToggle {
            run_id: refs.first().and_then(|w| w.run_id.clone()),
            group_id: group_id.clone(),
            entries: visible.clone(),
            hidden_count: refs.len(),
            expanded,
            summary,
            summary_kind,
            tool_surface,
            has_failure,
        },
    ));
    if expanded {
        rows.push(expanded_group_row(&group_id, &entry.created_at, visible));
    }
}

fn attach_created_thread_summaries(rows: Vec<TimelineRow>, entries: &[Arc<TimelineEntry>], collapsed_members: &HashSet<String>) -> Vec<TimelineRow> {
    let mut terminal_index: HashMap<String, usize> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        if let RowKind::Message { message, show_assistant_meta: true, .. } = &row.kind
            && let Some(run) = &message.run_id
        {
            terminal_index.insert(run.clone(), index);
        }
    }
    let mut created: HashMap<String, Vec<(String, String, Arc<TurnItem>)>> = HashMap::new();
    for entry in entries {
        let item = match &entry.kind {
            EntryKind::Work(w) => w.item.clone(),
            EntryKind::Event(item) => Some(item.clone()),
            _ => None,
        };
        if let Some(item) = item.filter(|i| matches!(i.kind, TurnItemKind::ThreadCreated { .. }))
            && let Some(run) = item.run_id.clone()
        {
            created.entry(run).or_default().push((entry.id.clone(), entry.created_at.clone(), item));
        }
    }
    if created.is_empty() {
        return rows;
    }
    let mut out = Vec::with_capacity(rows.len());
    for (index, row_value) in rows.into_iter().enumerate() {
        let created_run = match &row_value.kind {
            RowKind::Work { entries: group, .. } if group.len() == 1 && group[0].item_type == Some("thread_created") => group[0].run_id.clone(),
            _ => None,
        };
        if let Some(terminal) = created_run.and_then(|run| terminal_index.get(&run))
            && (collapsed_members.contains(&row_value.id) || index > *terminal)
        {
            continue;
        }
        let summaries = match &row_value.kind {
            RowKind::Message { message, show_assistant_meta: true, .. } => message.run_id.as_ref().and_then(|run| created.get(run)).cloned(),
            _ => None,
        };
        out.push(row_value);
        for (id, created_at, item) in summaries.unwrap_or_default() {
            out.push(row(format!("summary:{id}"), Some(created_at), RowKind::Event { item, subagents: Vec::new(), resource_summary: true }));
        }
    }
    out
}

fn attach_trailing_tool_groups(rows: Vec<TimelineRow>) -> Vec<TimelineRow> {
    let mut without_meta: HashSet<usize> = HashSet::new();
    let mut meta_after: HashMap<usize, TimelineRow> = HashMap::new();
    for (message_index, current) in rows.iter().enumerate() {
        let RowKind::Message { message, show_assistant_meta: true, show_copy, copy_streaming, .. } = &current.kind else { continue };
        let Some(run_id) = message.run_id.as_deref().filter(|_| message.role == Role::Assistant) else { continue };
        let mut last_trailing = None;
        let mut has_tool_group = false;
        for (index, candidate) in rows.iter().enumerate().skip(message_index + 1) {
            match &candidate.kind {
                RowKind::Message { .. } => break,
                RowKind::WorkToggle { run_id: Some(run), .. } if run == run_id => {
                    has_tool_group = true;
                    last_trailing = Some(index);
                }
                RowKind::Event { item, resource_summary: true, .. } if item.run_id.as_deref() == Some(run_id) => {
                    has_tool_group = true;
                    last_trailing = Some(index);
                }
                RowKind::Work { entries, expanded_group, .. } if entries.iter().any(|w| w.run_id.as_deref() == Some(run_id)) => {
                    if !expanded_group
                        && entries.iter().any(|w| {
                            work_log_entry_is_tool_like(w)
                                || w.item.as_ref().is_some_and(|i| matches!(i.kind, TurnItemKind::Error { .. }) && i.status == "failed")
                        })
                    {
                        has_tool_group = true;
                    }
                    if has_tool_group {
                        last_trailing = Some(index);
                    }
                }
                _ => {}
            }
        }
        let Some(last) = last_trailing else { continue };
        without_meta.insert(message_index);
        meta_after.insert(
            last,
            row(
                format!("assistant-meta:{}", message.id),
                rows[last].created_at.clone().or_else(|| Some(message.updated_at.clone())),
                RowKind::AssistantMeta { message: message.clone(), show_copy: *show_copy, copy_streaming: *copy_streaming },
            ),
        );
    }
    if meta_after.is_empty() {
        return rows;
    }
    let mut out = Vec::with_capacity(rows.len() + meta_after.len());
    for (index, mut current) in rows.into_iter().enumerate() {
        if without_meta.contains(&index)
            && let RowKind::Message { show_assistant_meta, show_copy, .. } = &mut current.kind
        {
            *show_assistant_meta = false;
            *show_copy = false;
        }
        out.push(current);
        if let Some(meta) = meta_after.remove(&index) {
            out.push(meta);
        }
    }
    out
}

fn own_user_message(row: Option<&TimelineRow>) -> Option<&ChatMessage> {
    match row.map(|r| &r.kind) {
        Some(RowKind::Message { message, .. }) if message.role == Role::User && user_message_attribution(message).is_none() => Some(message),
        _ => None,
    }
}

fn finalize_grouping(mut rows: Vec<TimelineRow>) -> Vec<TimelineRow> {
    let flags: Vec<(bool, bool, bool, bool)> = (0..rows.len())
        .map(|index| {
            let current = &rows[index];
            let next = rows.get(index + 1);
            if current.kind.is_work_log() && next.is_some_and(|n| n.kind.is_work_log()) {
                return (true, false, false, false);
            }
            let Some(message) = own_user_message(Some(current)) else { return (false, false, false, false) };
            let previous = index.checked_sub(1).and_then(|i| own_user_message(rows.get(i)));
            let with_next = own_user_message(next).is_some();
            let continues = previous.is_some_and(|p| p.is_steer()) && message.is_steer();
            (false, with_next, previous.is_some(), continues)
        })
        .collect();
    for (current, (continues_log, with_next, with_previous, continues_steer)) in rows.iter_mut().zip(flags) {
        current.continues_work_log = continues_log;
        if let RowKind::Message { grouped_with_next, grouped_with_previous, continues_steer: steer, .. } = &mut current.kind {
            *grouped_with_next = with_next;
            *grouped_with_previous = with_previous;
            *steer = continues_steer;
        }
    }
    rows
}
