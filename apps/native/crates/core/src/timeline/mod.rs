//! The transcript's row model. Ports the web timeline pipeline: visible turn items become
//! timeline entries (`apps/web/src/session-logic.ts`), entries become rows with work groups,
//! turn folds and live activity (`apps/web/src/components/chat/MessagesTimeline.logic.ts`),
//! labelled by `packages/client-runtime/src/work-log/*`.

mod activity;
mod compaction;
mod detail;
mod entries;
mod changed_files;
mod find;
mod folds;
mod fork;
pub mod minimap;
mod now;
mod presentation;
mod rewind;
mod rows;
mod status;
mod time;
mod tool_activity;
mod user_input;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use serde_json::Value;

use crate::thread::Visibility;
use crate::turn_item::TurnItem;

pub use activity::{
    Attribution, LatestRun, ThreadActivity, derive_activity, plan_title, should_collapse_user_message, strip_displayed_plan_markdown,
    user_message_attribution,
};
pub use detail::{GET_TURN_ITEM, command_output_text, decode_detail, detail_request, detail_revision, needs_detail_fetch, open_command_items};
pub use entries::{EntriesCache, derive_entries, item_type_name, update_entries};
pub use presentation::{
    SummaryKind, ToolGroupAction, WorkIcon, format_workspace_relative_path, live_thought_line, live_work_entry_label, skill_args,
    summary_icon, summarize_tool_group, tool_group_action, work_entry_display_indicates_tool_failure, work_entry_display_label,
    work_entry_icon, work_entry_is_visible_in_group, work_entry_read_output, work_log_entry_is_tool_like,
};
pub use changed_files::{
    ChangedFileRow, DiffStat, TreeNode, changed_file_rows, changed_file_tree, changed_files_label, directory_paths, has_directories, hidden_files_label,
    turn_diffs_by_message,
};
pub use fork::{FORK_FAILED, FORK_LABEL, can_fork, fork_from_run, fork_support};
pub use find::{FindMatch, FindNavigation, FindResult, FindStart, MAX_QUERY_CHARS, SEARCH_THREAD, bar_label, decode_result, format_count, search_request, step_index, step_locally};
pub use rewind::{
    CONFIRM_DESCRIPTION, CONFIRM_TITLE, EDIT_FROM_HERE, FAILED as REWIND_FAILED, INTERRUPT_FIRST, KEEP_CHANGES, NO_ROLLBACK, REVERT_FILES, RevertCheckpoint, RevertInput,
    RewindProgress, RewindWatch, TIMED_OUT as REWIND_TIMED_OUT, TIMEOUT_SECS as REWIND_TIMEOUT_SECS, available_revert_turn_counts, find_revert_checkpoint,
    merge_restored_prompt, recallable_prompt, revert_turn_counts, rewind_progress, rollback_command, watch_rewind,
};
pub use rows::{RowsCache, RowsInput, derive_rows, fold_membership};
pub use folds::unsettled_run_id;
pub use now::{
    BackgroundItem, BackgroundWork, ChildInput, ChildInputs, FORCE_STOP_THREAD, background_work, child_inputs, force_stop_notice, force_stop_request,
    format_subagent_display_title,
};
pub use status::{
    CHATGPT_USAGE_URL, CHATGPT_WORKSPACE_ADMIN_REQUEST, GoalLine, LimitRecovery, ProviderBanner, RunStop, SnoozeRefused, StatusLine, ThreadError, elapsed_since,
    error_banner_message, error_can_expand, goal_line, limit_recovery, presented_latest_run, provider_banner, relative_time_label, run_stop, status_line,
    thread_error, timeline_thread_error,
};
pub use time::{format_day_aware, format_duration, format_short_time, format_tokens, format_upcoming, parse_iso_ms};
pub use tool_activity::{claude_skill_invocation, command_display_text, command_program_name};
pub use user_input::{has_question_answer, question_answer_preview, question_answer_text, question_text_preview};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    Thinking,
    #[default]
    Tool,
    Info,
    Error,
}

/// `WorkLogToolLifecycleStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Idle,
    InProgress,
    Completed,
    Failed,
    Declined,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceActivity {
    Compaction,
    RuntimeWarning,
    RuntimeError,
    TaskProgress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSource {
    pub key: String,
    pub name: String,
    pub kind: String,
}

/// `WorkLogEntry`. `item` is the projected item it came from (`structuredPayload`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkLogEntry {
    pub id: String,
    pub created_at: String,
    pub run_id: Option<String>,
    pub label: String,
    pub detail: Option<String>,
    pub command: Option<String>,
    pub raw_command: Option<String>,
    pub changed_files: Vec<String>,
    pub tone: Tone,
    pub tool_title: Option<String>,
    pub tool_surface: Option<String>,
    pub tool_source: Option<ToolSource>,
    pub viewed_image_path: Option<String>,
    pub source_activity: Option<SourceActivity>,
    pub request_kind: Option<String>,
    pub item_type: Option<&'static str>,
    pub status: Option<ToolStatus>,
    pub question_answer: Option<Value>,
    pub item: Option<Arc<TurnItem>>,
    pub visibility: Option<Visibility>,
    /// Failure and action, computed on first use. Entries are shared across derives, so this
    /// keeps output scans off the per-token path.
    pub(crate) facts: std::sync::OnceLock<presentation::EntryFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageMeta {
    pub input_intent: Option<String>,
    pub created_by: Option<String>,
    pub creation_source: Option<String>,
    pub scheduled_task_id: Option<String>,
    pub sender_thread_id: Option<String>,
    pub idle_compaction: Option<String>,
    pub guest_name: Option<String>,
    pub guest_text: Option<String>,
    pub model_switch: Option<Value>,
}

/// `ChatMessage`, built from a user or assistant turn item.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    pub text: Arc<str>,
    pub run_id: Option<String>,
    pub streaming: bool,
    pub created_at: String,
    pub updated_at: String,
    pub meta: MessageMeta,
    pub item: Option<Arc<TurnItem>>,
}

impl ChatMessage {
    pub fn is_steer(&self) -> bool {
        matches!(self.meta.input_intent.as_deref(), Some("steer" | "promoted_queued_to_steer"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposedPlan {
    pub id: String,
    pub run_id: Option<String>,
    pub markdown: Arc<str>,
    pub status: String,
    pub updated_at: String,
}

/// `TimelineAttempt`: the run attempt an item belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineAttempt {
    pub id: String,
    pub run_id: String,
    pub status: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    Message(Arc<ChatMessage>),
    ProposedPlan(Arc<ProposedPlan>),
    Work(Arc<WorkLogEntry>),
    Event(Arc<TurnItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub id: String,
    pub created_at: String,
    pub attempt: Option<TimelineAttempt>,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionTokens {
    pub before: String,
    pub after: String,
}

/// One rendered transcript row: `MessagesTimelineRow`.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRow {
    pub id: String,
    pub created_at: Option<String>,
    pub continues_work_log: bool,
    pub kind: RowKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    Work {
        entries: Vec<Arc<WorkLogEntry>>,
        expanded_group: bool,
        display_label: Option<String>,
    },
    WorkLive {
        entry: Arc<WorkLogEntry>,
        entries: Vec<Arc<WorkLogEntry>>,
        group_id: String,
        expanded: bool,
        active: bool,
        thought: Option<Arc<WorkLogEntry>>,
    },
    Working {
        compacting: bool,
    },
    Thinking {
        group_id: Option<String>,
        expanded: bool,
    },
    WorkToggle {
        run_id: Option<String>,
        group_id: String,
        /// The members the group holds when opened.
        entries: Vec<Arc<WorkLogEntry>>,
        hidden_count: usize,
        expanded: bool,
        summary: String,
        summary_kind: SummaryKind,
        tool_surface: Option<String>,
        has_failure: bool,
    },
    TurnFold {
        run_id: String,
        label: String,
        expanded: bool,
    },
    AttemptFold {
        attempt_id: String,
        label: String,
        expanded: bool,
    },
    ContextCompaction {
        label: String,
        tokens: Option<CompactionTokens>,
        active: bool,
        title: Option<String>,
        failed: bool,
        failure: Option<String>,
    },
    Message {
        message: Arc<ChatMessage>,
        show_assistant_meta: bool,
        show_assistant_timestamp: bool,
        show_copy: bool,
        copy_streaming: bool,
        grouped_with_next: bool,
        grouped_with_previous: bool,
        continues_steer: bool,
    },
    AssistantMeta {
        message: Arc<ChatMessage>,
        show_copy: bool,
        copy_streaming: bool,
    },
    Event {
        item: Arc<TurnItem>,
        subagents: Vec<Arc<TurnItem>>,
        resource_summary: bool,
    },
    ProposedPlan(Arc<ProposedPlan>),
}

impl RowKind {
    pub fn is_work_log(&self) -> bool {
        matches!(self, RowKind::Work { .. } | RowKind::WorkLive { .. } | RowKind::WorkToggle { .. } | RowKind::Thinking { .. })
    }
}

/// Rows compare by identity of their shared parts, so an unchanged row keeps its measured
/// height: `isRowUnchanged`.
pub fn rows_equal(a: &TimelineRow, b: &TimelineRow) -> bool {
    if a.id != b.id || a.created_at != b.created_at || a.continues_work_log != b.continues_work_log {
        return false;
    }
    let same_entries = |x: &[Arc<WorkLogEntry>], y: &[Arc<WorkLogEntry>]| {
        x.len() == y.len() && x.iter().zip(y).all(|(l, r)| Arc::ptr_eq(l, r))
    };
    match (&a.kind, &b.kind) {
        (RowKind::Work { entries: ea, expanded_group: ga, display_label: la }, RowKind::Work { entries: eb, expanded_group: gb, display_label: lb }) => {
            ga == gb && la == lb && same_entries(ea, eb)
        }
        (
            RowKind::WorkLive { entry: a1, entries: ea, group_id: ga, expanded: xa, active: aa, thought: ta },
            RowKind::WorkLive { entry: b1, entries: eb, group_id: gb, expanded: xb, active: ab, thought: tb },
        ) => {
            Arc::ptr_eq(a1, b1)
                && same_entries(ea, eb)
                && ga == gb
                && xa == xb
                && aa == ab
                && match (ta, tb) {
                    (Some(l), Some(r)) => Arc::ptr_eq(l, r),
                    (None, None) => true,
                    _ => false,
                }
        }
        (
            RowKind::Message { message: ma, show_assistant_meta: a1, show_assistant_timestamp: a2, show_copy: a3, copy_streaming: a4, grouped_with_next: a5, grouped_with_previous: a6, continues_steer: a7 },
            RowKind::Message { message: mb, show_assistant_meta: b1, show_assistant_timestamp: b2, show_copy: b3, copy_streaming: b4, grouped_with_next: b5, grouped_with_previous: b6, continues_steer: b7 },
        ) => Arc::ptr_eq(ma, mb) && (a1, a2, a3, a4, a5, a6, a7) == (b1, b2, b3, b4, b5, b6, b7),
        (
            RowKind::AssistantMeta { message: ma, show_copy: a1, copy_streaming: a2 },
            RowKind::AssistantMeta { message: mb, show_copy: b1, copy_streaming: b2 },
        ) => Arc::ptr_eq(ma, mb) && (a1, a2) == (b1, b2),
        (RowKind::Event { item: ia, subagents: sa, resource_summary: ra }, RowKind::Event { item: ib, subagents: sb, resource_summary: rb }) => {
            Arc::ptr_eq(ia, ib) && ra == rb && sa.len() == sb.len() && sa.iter().zip(sb).all(|(l, r)| Arc::ptr_eq(l, r))
        }
        (RowKind::ProposedPlan(pa), RowKind::ProposedPlan(pb)) => Arc::ptr_eq(pa, pb),
        (
            RowKind::WorkToggle { run_id: ra, group_id: ga, entries: ea, hidden_count: ha, expanded: xa, summary: sa, summary_kind: ka, tool_surface: ta, has_failure: fa },
            RowKind::WorkToggle { run_id: rb, group_id: gb, entries: eb, hidden_count: hb, expanded: xb, summary: sb, summary_kind: kb, tool_surface: tb, has_failure: fb },
        ) => same_entries(ea, eb) && (ra, ga, ha, xa, sa, ka, ta, fa) == (rb, gb, hb, xb, sb, kb, tb, fb),
        _ => a.kind == b.kind,
    }
}
