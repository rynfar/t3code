//! Transcript rows. Each `TimelineRow` from `t3_core::timeline` renders here, wrapped with the
//! row spacing of `TimelineRowContent` and `WorkLogBlock` (`MessagesTimeline.tsx`, `WorkLog.tsx`).

pub mod markdown;
mod messages;
mod notices;
mod parts;
mod work;

use std::collections::HashSet;

use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use t3_core::timeline::{Role, RowKind, TimelineRow};

use crate::thread_view::ThreadView;
use markdown::MarkdownStore;
pub use parts::Clock;

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
}

pub struct RowEnv<'a> {
    pub clock: Clock,
    pub workspace_root: Option<&'a str>,
    pub ui: &'a TranscriptUi,
    pub pending_approvals: &'a HashSet<String>,
    pub window_height: f32,
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
        RowKind::Event { .. } | RowKind::AttemptFold { .. } => 8.,
        _ => 16.,
    }
}

pub fn render_row(row: &TimelineRow, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let id = row.id.as_str();
    let content = match &row.kind {
        RowKind::Work { entries, expanded_group, display_label } => {
            work::work_section(id, entries, *expanded_group, display_label.as_deref(), env, markdown, cx)
        }
        RowKind::WorkLive { .. } => work::live_row(row, env, cx),
        RowKind::Thinking { group_id, .. } => work::thinking_row(id, group_id.as_deref(), env, cx),
        RowKind::WorkToggle { .. } => work::group_header(row, env, cx),
        RowKind::Working { compacting } => notices::working_row(row.created_at.as_deref(), *compacting, env, cx),
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
    div().w_full().min_w_0().pb(px(bottom_space(row))).child(content).into_any_element()
}
