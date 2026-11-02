//! Transcript rows. Each `TimelineRow` from `t3_core::timeline` renders here, wrapped with the
//! row spacing of `TimelineRowContent` and `WorkLogBlock` (`MessagesTimeline.tsx`, `WorkLog.tsx`).

mod changed_files;
mod command_output;
mod inspector;
pub mod markdown;
mod media;
mod messages;
mod notices;
mod parts;
mod timer;
mod work;
mod worktree_setup;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{AnyElement, Context, Entity, IntoElement, ListState, ParentElement, Styled, div, px};
use t3_core::timeline::{Role, RowKind, TimelineRow, WorkLogEntry};
use t3_core::turn_item::TurnItem;

use crate::thread_view::ThreadView;
use markdown::MarkdownStore;
pub use command_output::CommandOutputSlot;
pub use notices::history_control;
pub use parts::{Clock, InlineTone, ghost_button, ghost_icon_button, ghost_text_button, inline_button};
pub use timer::{TimerKind, WorkingTimer};
pub use worktree_setup::SetupEnv;

/// Disclosure state the user controls. Folds and groups feed row derivation; the rest only
/// change how a row draws.
#[derive(Default)]
pub struct TranscriptUi {
    pub expanded_runs: HashSet<String>,
    pub expanded_attempts: HashSet<String>,
    pub expanded_groups: HashSet<String>,
    pub expanded_entries: HashSet<String>,
    pub expanded_messages: HashSet<String>,
    pub expanded_plans: HashSet<String>,
    /// Changed-files cards with every folder open, by run.
    pub changed_files_open: HashSet<String>,
    /// Folders opened or closed against that, by run, then folder path.
    pub changed_file_dirs: HashMap<String, HashMap<String, bool>>,
}

/// Withheld output fetched with `orchestration.getTurnItem`, keyed by item id.
pub enum Detail {
    Loading { revision: String },
    Loaded { revision: String, item: Arc<TurnItem> },
    Missing { revision: String },
    Failed { revision: String, message: String },
}

impl Detail {
    pub fn revision(&self) -> &str {
        match self {
            Detail::Loading { revision } | Detail::Loaded { revision, .. } | Detail::Missing { revision } | Detail::Failed { revision, .. } => revision,
        }
    }
}

pub struct RowEnv<'a> {
    pub clock: Clock,
    pub workspace_root: Option<&'a str>,
    pub ui: &'a TranscriptUi,
    pub pending_approvals: &'a HashSet<String>,
    pub window_height: f32,
    pub details: &'a HashMap<String, Detail>,
    /// Scroll state of each open work group's own virtualized list.
    pub group_lists: &'a RefCell<HashMap<String, ListState>>,
    pub working_timer: Option<&'a Entity<WorkingTimer>>,
    /// The entry holding find's active match, tinted where it draws.
    pub find_target: Option<&'a str>,
    /// Each turn's changed files, by the assistant message that ends it.
    pub turn_diffs: &'a HashMap<String, Arc<t3_core::diff::TurnDiffSummary>>,
    /// Fork capability by provider thread, and the runs a fork is being created from.
    pub fork_support: &'a HashMap<String, bool>,
    pub forking: &'a HashSet<String>,
    /// The turn count each user message's "Edit from here" rewinds to.
    pub revert_counts: &'a HashMap<String, u64>,
    /// A rewind is out or a turn is running.
    pub rewind_disabled: bool,
    /// File changes offer their diff. Not in a side chat: the diff panel follows only the open
    /// thread's items, so a side chat's running change would never refetch once it settles.
    pub tool_diffs: bool,
    /// Open command rows' streamed output, by entry; `None` when the server does not stream it
    /// (`commandOutputStreaming`), so rows show the item's own output instead.
    pub command_outputs: Option<&'a HashMap<String, CommandOutputSlot>>,
    pub setup: &'a SetupEnv,
}

/// Space below a row: `TimelineRowContent` and `WorkLogBlock`.
fn bottom_space(row: &TimelineRow) -> f32 {
    match &row.kind {
        RowKind::Work { expanded_group: true, .. } => {
            if row.continues_work_log { 0. } else { 4. }
        }
        RowKind::WorkToggle { expanded: true, .. } | RowKind::WorkLive { expanded: true, .. } | RowKind::Thinking { expanded: true, .. } => 0.,
        kind if kind.is_work_log() => {
            if row.continues_work_log { 0. } else { 8. }
        }
        RowKind::Event { item, .. } if matches!(item.kind, t3_core::turn_item::TurnItemKind::Subagent { .. }) => 0.,
        RowKind::TurnFold { .. } | RowKind::Working { .. } => 6.,
        RowKind::Message { grouped_with_next: true, .. } => 4.,
        RowKind::Message { message, show_assistant_meta: false, .. } if message.role == Role::Assistant => 8.,
        RowKind::Event { .. } | RowKind::AttemptFold { .. } | RowKind::WorktreeSetup { .. } => 8.,
        _ => 16.,
    }
}

pub fn render_row(row: &Arc<TimelineRow>, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let id = row.id.as_str();
    let content = match &row.kind {
        RowKind::Work { .. } => work::work_section(row, env, markdown, cx),
        RowKind::WorkLive { .. } => work::live_row(row, env, cx),
        // `ThinkingTimelineRow` keeps its slot blank while the worktree is prepared.
        RowKind::Thinking { .. } if env.setup.view.preparing => worktree_setup::empty_work_row(id),
        RowKind::Thinking { group_id, .. } => work::thinking_row(id, group_id.as_deref(), cx),
        RowKind::WorkToggle { .. } => work::group_header(row, env, cx),
        RowKind::Working { compacting } => {
            let chip = worktree_setup::background_chip(env, cx);
            notices::working_row(*compacting, env, chip, cx)
        }
        RowKind::WorktreeSetup { snapshot, embedded } => worktree_setup::card("worktree-setup", snapshot, *embedded, env, cx),
        RowKind::TurnFold { run_id, label, expanded } => notices::turn_fold(id, run_id, label, *expanded, row.created_at.as_deref(), env, cx),
        RowKind::AttemptFold { attempt_id, label, expanded } => notices::attempt_fold(id, attempt_id, label, *expanded, cx),
        RowKind::ContextCompaction { label, tokens, title, failed, failure, .. } => {
            notices::compaction_row(label, tokens.as_ref(), title.as_deref(), *failed, failure.as_deref(), cx)
        }
        RowKind::Message { message, .. } if message.role == Role::User => messages::user_row(row, env, markdown, cx),
        RowKind::Message { .. } => messages::assistant_row(row, env, markdown, cx),
        RowKind::AssistantMeta { message, show_copy, copy_streaming } => messages::assistant_meta_row(message, *show_copy, *copy_streaming, env, cx),
        RowKind::Event { item, subagents, resource_summary } => notices::event_row(id, item, subagents, *resource_summary, env, cx),
        RowKind::ProposedPlan(plan) => messages::plan_card(id, plan, env, markdown, cx),
    };
    let found = env.find_target.is_some_and(|target| holds(row, target));
    div()
        .w_full()
        .min_w_0()
        .pb(px(bottom_space(row)))
        .child(if found { found_tint(content, cx) } else { content })
        .into_any_element()
}

/// Whether the row draws `entry_id` itself (an open group tints only the member).
fn holds(row: &TimelineRow, entry_id: &str) -> bool {
    match &row.kind {
        RowKind::Message { message, .. } => message.id == entry_id,
        RowKind::Work { entries, expanded_group: false, .. } | RowKind::WorkLive { entries, .. } | RowKind::WorkToggle { entries, .. } => {
            entries.iter().any(|entry| entry.id == entry_id)
        }
        RowKind::Event { item, subagents, .. } => item.id == entry_id || subagents.iter().any(|member| member.id == entry_id),
        RowKind::ProposedPlan(_) => row.id == entry_id,
        _ => false,
    }
}

/// Find's active match: the row behind it takes the `search_match_background` tint.
fn found_tint(content: AnyElement, cx: &Context<ThreadView>) -> AnyElement {
    div().rounded(px(8.)).bg(crate::theme::hsla(crate::theme::palette(cx).search_match_background).opacity(0.45)).child(content).into_any_element()
}

/// One member of an open work group, drawn by the group's own virtualized list.
pub fn render_group_member(entry: &Arc<WorkLogEntry>, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let content = work::group_member(entry, env, markdown, cx);
    if env.find_target == Some(entry.id.as_str()) { found_tint(content, cx) } else { content }
}
