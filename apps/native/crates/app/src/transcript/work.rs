//! Work, tool and reasoning rows: `TimelineWorkGroupRows.tsx`, `TimelineWorkEntryRow.tsx` and
//! `TimelinePendingApprovalRow.tsx`, laid out per design system §7.7; an open tool row shows the
//! inspector (`inspector.rs`). Approval actions belong to the composer, so the row shows state only.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListSizingBehavior, ListState, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, list, prelude::FluentBuilder, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::timeline::{
    EntryBody, RowKind, TimelineRow, WorkIcon, WorkLogEntry, answered_questions, detail_revision, entry_body, entry_can_expand, entry_fetches_detail,
    has_question_answer, live_thought_line, live_work_entry_label, question_answer_preview, question_text_preview, summary_icon, work_entry_display_indicates_tool_failure,
    work_entry_display_label, work_entry_icon, work_entry_is_visible_in_group, work_log_entry_is_tool_like, ToolStatus,
};
use t3_core::turn_item::{TurnItem, TurnItemKind};

use super::command_output;
use super::inspector::{Fetch, ItemInspector, Mode, plain_body, question_history};
use super::markdown::{MarkdownStore, prose};
use super::parts::{chevron, focusable, icon_slot, outline_button, timestamp, work_line, work_row_frame};
use super::{Detail, RowEnv};
use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;

pub fn work_icon(icon: WorkIcon) -> Icon {
    match icon {
        WorkIcon::Brain => Icon::WorkBrain,
        WorkIcon::Terminal => Icon::WorkTerminal,
        WorkIcon::Eye => Icon::WorkEye,
        WorkIcon::SquarePen => Icon::WorkSquarePen,
        WorkIcon::Globe => Icon::WorkGlobe,
        WorkIcon::Search => Icon::WorkSearch,
        WorkIcon::Wrench => Icon::WorkWrench,
        WorkIcon::Hammer => Icon::WorkHammer,
        WorkIcon::Bot => Icon::WorkBot,
        WorkIcon::Zap => Icon::WorkZap,
        WorkIcon::Check => Icon::WorkCheck,
        WorkIcon::CircleAlert => Icon::WorkCircleAlert,
        WorkIcon::MessageCircle => Icon::WorkMessageCircle,
        WorkIcon::ListTodo => Icon::WorkListTodo,
        WorkIcon::Code => Icon::WorkCode,
        WorkIcon::Computer => Icon::WorkMonitor,
        WorkIcon::T3 => Icon::WorkMessageSquare,
    }
}

fn pending_approval<'a>(entry: &'a WorkLogEntry, env: &RowEnv) -> Option<&'a WorkLogEntry> {
    match entry.item.as_ref().map(|i| &i.kind) {
        Some(TurnItemKind::ApprovalRequest { request_id, .. }) if env.pending_approvals.contains(request_id) => Some(entry),
        _ => None,
    }
}

/// `WorkGroupSection`: one standalone row per entry, or the open list of a group.
pub fn work_section(row: &Arc<TimelineRow>, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    let RowKind::Work { entries, expanded_group, display_label } = &row.kind else { return div().into_any_element() };
    let visible: Vec<Arc<WorkLogEntry>> = entries
        .iter()
        .filter(|e| pending_approval(e, env).is_some() || work_entry_is_visible_in_group(e, *expanded_group))
        .cloned()
        .collect();
    if visible.is_empty() {
        return div().into_any_element();
    }
    if *expanded_group {
        return group_list(&row.id, visible, env, cx);
    }
    let rows: Vec<AnyElement> = visible.iter().map(|entry| entry_row(entry, display_label.as_deref(), false, env, markdown, cx)).collect();
    v_flex().w_full().min_w_0().children(rows).into_any_element()
}

/// The open group's members in their own virtualized list (`ExpandedWorkGroupEntries`, a
/// LegendList on the web): `min(288px, 50% of the window)` tall, plus room for members the user
/// opened, so only the rows in view are built. A group of thoughts is the reasoning trace,
/// capped at 384.
fn group_list(id: &str, members: Vec<Arc<WorkLogEntry>>, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let count = members.len();
    let reasoning = members.iter().all(|e| e.item_type == Some("reasoning"));
    let opened = members.iter().filter(|e| env.ui.expanded_entries.contains(&e.id)).count() as f32;
    let scale = cx.theme().font_size / px(16.);
    let cap = if reasoning { 384. * scale } else { (288. * scale).min(env.window_height * 0.5) + opened * 264. * scale };
    let state = {
        let mut lists = env.group_lists.borrow_mut();
        let state = lists.entry(id.to_owned()).or_insert_with(|| ListState::new(count, ListAlignment::Top, px(240.)));
        let previous = state.item_count();
        if count > previous {
            state.splice(previous..previous, count - previous);
        } else if count < previous {
            state.reset(count);
        }
        state.clone()
    };
    let members = Arc::new(members);
    let list = list(
        state,
        cx.processor(move |view: &mut ThreadView, ix: usize, _window, cx| match members.get(ix) {
            Some(entry) => view.render_group_member(entry, cx),
            None => div().into_any_element(),
        }),
    )
    .with_sizing_behavior(ListSizingBehavior::Infer)
    .w_full()
    .max_h(px(cap));
    let body = div().w_full().min_w_0().child(list);
    if reasoning { body.ml(rem(28.)).px(rem(2.)).py(rem(4.)).into_any_element() } else { body.into_any_element() }
}

/// One member of an open group: a work row, or one thought of a reasoning trace.
pub fn group_member(entry: &Arc<WorkLogEntry>, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    if entry.item_type == Some("reasoning")
        && let Some(detail) = entry.detail.as_deref()
    {
        let text: Arc<str> = Arc::from(detail);
        let state = markdown.state(&format!("reasoning:{}", entry.id), &text, cx);
        return div().pb(rem(12.)).child(prose(&state, cx)).into_any_element();
    }
    entry_row(entry, None, true, env, markdown, cx)
}

/// `WorkEntryLogRow`.
fn entry_row(
    entry: &Arc<WorkLogEntry>,
    display_label: Option<&str>,
    grouped: bool,
    env: &RowEnv,
    markdown: &mut MarkdownStore,
    cx: &mut Context<ThreadView>,
) -> AnyElement {
    if pending_approval(entry, env).is_some() {
        return approval_row(entry, cx);
    }
    let p = palette(cx);
    if let Some(item) = entry.item.as_ref()
        && let TurnItemKind::Error { failure, .. } = &item.kind
        && item.status == "failed"
    {
        let warning = failure.class == "usage_limit";
        let color = hsla(if warning { p.warning } else { p.destructive });
        let label = if warning {
            match failure.reset_at.as_deref() {
                Some(reset) => format!("Usage limit reached. Retry after {}.", env.clock.format(reset)),
                None => "Usage limit reached.".into(),
            }
        } else {
            entry.label.clone()
        };
        return work_row_frame(SharedString::from(entry.id.clone()), grouped)
            .child(work_line(
                Some(icon_slot(Icon::CircleAlert, color)),
                div().text_size(rem(14.)).font_weight(FontWeight::MEDIUM).text_color(color).child(label).into_any_element(),
                vec![timestamp(&env.clock, &entry.created_at, true, cx)],
                true,
                cx,
            ))
            .when(!warning, |this| {
                this.child(
                    div()
                        .ml(rem(28.))
                        .py(rem(4.))
                        .text_size(rem(14.))
                        .line_height(rem(22.75))
                        .text_color(hsla(p.foreground).opacity(0.8))
                        .child(failure.message.clone()),
                )
            })
            .into_any_element();
    }
    let expanded = env.ui.expanded_entries.contains(&entry.id);
    let warning = entry.source_activity == Some(t3_core::timeline::SourceActivity::RuntimeWarning);
    let failed = !warning && work_entry_display_indicates_tool_failure(entry);
    let severe = failed && (entry.item_type == Some("error") || !work_log_entry_is_tool_like(entry));
    let icon = if warning || severe { Icon::WorkCircleAlert } else { work_icon(work_entry_icon(entry)) };
    let icon_color = if warning {
        hsla(p.warning)
    } else if severe {
        hsla(p.destructive)
    } else if failed {
        hsla(p.tool_error_icon).opacity(0.4)
    } else {
        hsla(p.icon_muted)
    };
    let reasoning = entry.item_type == Some("reasoning");
    let question = entry.question_answer.as_ref().map(question_text_preview).filter(|q| !q.is_empty());
    let answer = entry.question_answer.as_ref().filter(|a| has_question_answer(a)).map(question_answer_preview);
    let preview = if reasoning && expanded {
        if entry.status == Some(ToolStatus::InProgress) { "Thinking".to_owned() } else { "Thought".to_owned() }
    } else {
        question.or_else(|| display_label.map(str::to_owned)).unwrap_or_else(|| work_entry_display_label(entry, env.workspace_root))
    };
    let label_color = if warning {
        hsla(p.warning)
    } else if severe {
        hsla(p.destructive)
    } else if work_log_entry_is_tool_like(entry) {
        hsla(p.secondary_label)
    } else {
        hsla(p.foreground).opacity(0.8)
    };
    let can_expand = entry_can_expand(entry, env.workspace_root);
    let mut trailing = Vec::new();
    if failed && !severe {
        trailing.push(Icon::WorkX.render(rem(12.).to_pixels(cx.theme().font_size), hsla(p.tool_error_icon).opacity(0.4)).into_any_element());
    }
    trailing.push(timestamp(&env.clock, &entry.created_at, false, cx));
    trailing.push(chevron(expanded, can_expand, cx));
    let label = h_flex()
        .min_w_0()
        .gap(rem(6.))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(label_color)
                .when(warning || severe, |this| this.font_weight(FontWeight::MEDIUM))
                .child(preview),
        )
        .children(answer.map(|answer| {
            let color = if expanded { hsla(p.muted_foreground) } else { hsla(p.foreground) };
            div().min_w_0().truncate().text_color(color).child(answer)
        }))
        .into_any_element();
    let entry_id = entry.id.clone();
    let mut frame = work_row_frame(SharedString::from(format!("work-entry:{}", entry.id)), grouped)
        .child(work_line(Some(icon_slot(icon, icon_color)), label, trailing, false, cx));
    if can_expand {
        frame = focusable(frame, cx).cursor_pointer().on_click(cx.listener(move |view, _, _, cx| view.toggle_entry(&entry_id, cx)));
    }
    if expanded && reasoning && let Some(detail) = entry.detail.as_deref() {
        let text: Arc<str> = Arc::from(detail);
        let state = markdown.state(&format!("reasoning:{}", entry.id), &text, cx);
        frame = frame.child(
            div()
                .id(SharedString::from(format!("work-trace:{}", entry.id)))
                .ml(rem(28.))
                .px(rem(2.))
                .py(rem(4.))
                .max_h(rem(384.))
                .overflow_y_scroll()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(prose(&state, cx)),
        );
    } else if expanded && let Some(body) = entry_body(entry, env.workspace_root) {
        frame = frame.child(expanded_body(entry, body, env, cx));
    }
    frame.into_any_element()
}

/// What an open row shows under its line: the answers it was given, or a panel with the
/// inspector or a read's or skill's plain output (`WorkLogDetails kind="panel"`).
fn expanded_body(entry: &WorkLogEntry, body: EntryBody, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let Some(item) = entry.item.clone() else { return div().into_any_element() };
    let content = match body {
        EntryBody::QuestionAnswer => {
            let questions = entry.question_answer.as_ref().map(answered_questions).unwrap_or_default();
            let id = SharedString::from(format!("work-answers:{}", entry.id));
            return div().id(id).on_click(|_, _, cx| cx.stop_propagation()).child(question_history(&entry.id, &questions, cx)).into_any_element();
        }
        EntryBody::Plain { text, fetches } => v_flex()
            .w_full()
            .min_w_0()
            .children(text.map(|text| plain_body(&entry.id, &text, cx)))
            .when(fetches, |this| this.child(ItemInspector::new(&entry.id, item.clone(), fetch_state(entry, &item, env), Mode::OutputOnly, cx.entity_id())))
            .into_any_element(),
        EntryBody::Inspector => {
            let mut inspector =
                ItemInspector::new(&entry.id, item.clone(), fetch_state(entry, &item, env), Mode::Full, cx.entity_id()).workspace_root(env.workspace_root);
            // The server streams the output, live and then final, in place of the item's own.
            if let Some(outputs) = env.command_outputs
                && matches!(item.kind, TurnItemKind::CommandExecution { .. })
            {
                inspector = inspector.output_slot(command_output::panel(&entry.id, outputs.get(&entry.id), cx));
            }
            if matches!(item.kind, TurnItemKind::FileChange { .. }) {
                let target = t3_core::diff::ToolCallTarget::of(&item);
                let open = cx.listener(move |view, _, _, cx| view.open_tool_call_diff(target.clone(), cx));
                inspector = inspector.open_diff(Some(outline_button(SharedString::from(format!("open-tool-diff:{}", item.id)), "Open diff", open, cx)));
            }
            inspector.into_any_element()
        }
    };
    div()
        .id(SharedString::from(format!("work-detail:{}", entry.id)))
        .w_full()
        .min_w_0()
        .mt(rem(2.))
        .mb(rem(6.))
        .pb(rem(8.))
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(content)
        .into_any_element()
}

/// Where the fetch of the entry's withheld detail stands for its item's current revision.
fn fetch_state(entry: &WorkLogEntry, item: &TurnItem, env: &RowEnv) -> Fetch {
    if !entry_fetches_detail(entry, env.command_outputs.is_some()) {
        return Fetch::None;
    }
    match env.details.get(&item.id) {
        Some(detail) if detail.revision() == detail_revision(item) => match detail {
            Detail::Loading { .. } => Fetch::Pending,
            Detail::Loaded { item, .. } => Fetch::Loaded(item.clone()),
            Detail::Missing { .. } => Fetch::Missing,
            Detail::Failed { message, .. } => Fetch::Failed(SharedString::from(message.clone())),
        },
        _ => Fetch::Pending,
    }
}

fn approval_icon(kind: &str) -> (Icon, &'static str) {
    match kind {
        "command" => (Icon::WorkTerminal, "Command approval"),
        "file-change" => (Icon::WorkSquarePen, "File change approval"),
        "file-read" => (Icon::WorkEye, "File read approval"),
        "permission" => (Icon::WorkWrench, "App permission approval"),
        _ => (Icon::WorkHammer, "App access approval"),
    }
}

/// `TimelinePendingApprovalRow`, without the actions the composer owns.
fn approval_row(entry: &WorkLogEntry, cx: &App) -> AnyElement {
    let p = palette(cx);
    let Some(item) = entry.item.as_ref() else { return div().into_any_element() };
    let TurnItemKind::ApprovalRequest { request_kind, prompt, reason, app_name, .. } = &item.kind else { return div().into_any_element() };
    let (icon, label) = approval_icon(request_kind);
    let detail = prompt.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| label.to_owned());
    work_row_frame(SharedString::from(format!("approval:{}", entry.id)), false)
        .child(work_line(
            Some(icon_slot(icon, hsla(p.icon_muted))),
            div()
                .id(SharedString::from(format!("approval-detail:{}", entry.id)))
                .max_h(rem(160.))
                .overflow_y_scroll()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .line_height(rem(20.))
                .text_color(hsla(p.foreground))
                .child(detail)
                .into_any_element(),
            Vec::new(),
            true,
            cx,
        ))
        .child(
            v_flex()
                .ml(rem(30.))
                .gap(rem(4.))
                .pb(rem(4.))
                .text_xs()
                .children(app_name.clone().map(|name| div().text_color(hsla(p.muted_foreground)).child(name)))
                .children(reason.clone().map(|reason| div().text_color(hsla(p.muted_foreground)).child(reason)))
                .child(
                    h_flex()
                        .gap(rem(4.))
                        .text_color(hsla(p.warning_foreground))
                        .child(Icon::Shield.render(rem(12.).to_pixels(cx.theme().font_size), hsla(p.warning_foreground)))
                        .child("Waiting for approval"),
                ),
        )
        .into_any_element()
}

/// `LiveActivityContent`: icon, label and a trailing failure mark.
fn live_activity(label: AnyElement, icon: Icon, failed: bool, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let color = if failed { hsla(p.tool_error_icon).opacity(0.4) } else { hsla(p.icon_muted) };
    let trailing = if failed { vec![Icon::WorkX.render(rem(12.).to_pixels(cx.theme().font_size), color).into_any_element()] } else { Vec::new() };
    work_line(Some(icon_slot(icon, color)), label, trailing, false, cx)
}

/// `LiveWorkEntryTimelineRow`.
pub fn live_row(row: &TimelineRow, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let RowKind::WorkLive { entry, group_id, expanded, active, thought, .. } = &row.kind else { return div().into_any_element() };
    let (id, expanded, active, thought) = (row.id.as_str(), *expanded, *active, thought.as_ref());
    if pending_approval(entry, env).is_some() {
        return approval_row(entry, cx);
    }
    let p = palette(cx);
    let failed = work_entry_display_indicates_tool_failure(entry);
    let thought_line = thought.filter(|_| !expanded).map(|t| live_thought_line(t.detail.as_deref().unwrap_or_default())).unwrap_or_default();
    let thought_is_status = !thought_line.is_empty() && thought.is_some_and(|t| Arc::ptr_eq(t, entry));
    let label = if thought_is_status {
        if active { "Thinking".to_owned() } else { "Thought".to_owned() }
    } else {
        live_work_entry_label(entry, env.workspace_root, active)
    };
    let group_id = group_id.to_owned();
    let row_id = id.to_owned();
    focusable(work_row_frame(SharedString::from(format!("live:{id}")), false), cx)
        .cursor_pointer()
        .on_click(cx.listener(move |view, _, _, cx| view.toggle_group(&group_id, &row_id, cx)))
        .when(!thought_line.is_empty(), |this| {
            this.child(work_line(
                Some(icon_slot(Icon::WorkBrain, hsla(p.icon_muted))),
                div().line_clamp(4).child(thought_line.clone()).into_any_element(),
                Vec::new(),
                true,
                cx,
            ))
        })
        .child(live_activity(
            div().truncate().text_color(hsla(p.secondary_label)).child(label).into_any_element(),
            work_icon(work_entry_icon(entry)),
            failed,
            cx,
        ))
        .into_any_element()
}

/// `ThinkingTimelineRow`.
pub fn thinking_row(id: &str, group_id: Option<&str>, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let activity = live_activity(div().text_color(hsla(p.secondary_label)).child("Thinking").into_any_element(), Icon::WorkBrain, false, cx);
    let frame = work_row_frame(SharedString::from(format!("thinking:{id}")), false).child(activity);
    match group_id {
        Some(group_id) => {
            let (group_id, row_id) = (group_id.to_owned(), id.to_owned());
            focusable(frame, cx).cursor_pointer().on_click(cx.listener(move |view, _, _, cx| view.toggle_group(&group_id, &row_id, cx))).into_any_element()
        }
        None => frame.into_any_element(),
    }
}

/// `WorkGroupToggleTimelineRow`: the "N tool calls" summary that opens the group.
pub fn group_header(row: &TimelineRow, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let RowKind::WorkToggle { group_id, summary, summary_kind, tool_surface, .. } = &row.kind else { return div().into_any_element() };
    let id = row.id.as_str();
    let p = palette(cx);
    let icon = if tool_surface.is_some() { Icon::WorkMonitor } else { work_icon(summary_icon(*summary_kind)) };
    let trailing = row.created_at.as_deref().map(|at| vec![timestamp(&env.clock, at, false, cx)]).unwrap_or_default();
    let (group_id, row_id) = (group_id.to_owned(), id.to_owned());
    focusable(work_row_frame(SharedString::from(format!("toggle:{id}")), false), cx)
        .cursor_pointer()
        .on_click(cx.listener(move |view, _, _, cx| view.toggle_group(&group_id, &row_id, cx)))
        .child(work_line(
            Some(icon_slot(icon, hsla(p.icon_muted))),
            div().truncate().child(summary.to_owned()).into_any_element(),
            trailing,
            false,
            cx,
        ))
        .into_any_element()
}
