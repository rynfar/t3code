//! Work, tool and reasoning rows: `TimelineWorkGroupRows.tsx`, `TimelineWorkEntryRow.tsx` and
//! `TimelinePendingApprovalRow.tsx`, laid out per design system §7.7; an open tool row shows the
//! inspector (`inspector.rs`).

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListSizingBehavior, ListState, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, list, prelude::FluentBuilder, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use serde_json::Value;
use t3_core::commands::ApprovalDecision;
use t3_core::lineage::{Subagent, subagents_of};
use t3_core::requests::{self, PendingApproval};
use t3_core::timeline::{
    EntryBody, RowKind, TimelineRow, ToolStatus, WorkIcon, WorkLogEntry, answered_questions, detail_revision, entry_body, entry_can_expand, entry_fetches_detail,
    format_duration, format_subagent_display_title, has_question_answer, live_thought_line, live_work_entry_label, parse_iso_ms, question_answer_preview,
    question_text_preview, summary_icon, thread_read_label_prefix, thread_read_target_id, thread_read_target_title, tool_surface_icon,
    work_entry_display_indicates_tool_failure, work_entry_display_label, work_entry_icon, work_entry_is_visible_in_group, work_entry_viewed_image,
    work_log_entry_is_tool_like,
};
use t3_core::turn_item::{TurnItem, TurnItemKind};

use super::command_output;
use super::inspector::{Fetch, ItemInspector, Mode, plain_body, question_history};
use super::markdown::{MarkdownStore, prose};
use super::media::tool_output_image;
use super::notices::{SubagentLiveElapsed, plain_detail, provider_marks, subagent_avatar};
use super::parts::{InlineTone, chevron, focusable, icon_slot, inline_button, inline_button_with, outline_button, timestamp, work_line, work_row_frame};
use super::{Detail, RowEnv};
use crate::composer::controls::{Float, MenuItem, floating, menu_surface};
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
        WorkIcon::Computer | WorkIcon::Browser => Icon::WorkMonitor,
        WorkIcon::Device => Icon::Smartphone,
        WorkIcon::PullRequest => Icon::GitPullRequest,
        WorkIcon::T3 => Icon::WorkMessageSquare,
    }
}

fn pending_approval<'a>(entry: &WorkLogEntry, env: &'a RowEnv) -> Option<&'a PendingApproval> {
    let Some(TurnItemKind::ApprovalRequest { request_id, .. }) = entry.item.as_ref().map(|i| &i.kind) else {
        return None;
    };
    env.approvals.iter().find(|approval| approval.request_id == *request_id)
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
    let rows: Vec<AnyElement> = visible
        .iter()
        .map(|entry| {
            subagent_notification_link(entry, env, cx)
                .unwrap_or_else(|| entry_row(entry, display_label.as_deref(), false, env, markdown, cx))
        })
        .collect();
    v_flex().w_full().min_w_0().children(rows).into_any_element()
}

fn subagent_status_label(status: &str) -> &'static str {
    match status {
        "pending" => "Queued",
        "running" => "Running",
        "waiting" => "Waiting",
        "idle" => "Idle · resumable",
        "completed" => "Completed",
        "failed" => "Failed",
        _ => "Stopped",
    }
}

fn notification_child_thread_id(source: &Value) -> Option<&str> {
    matches!(source.get("kind").and_then(Value::as_str), Some("subagent" | "delegated_task"))
        .then(|| source.get("childThreadId").or_else(|| source.get("threadId")).and_then(Value::as_str))
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `SubagentNotificationLink` (`SimpleWorkEntryRow` + `V2LifecycleRow.tsx`).
fn subagent_notification_link(entry: &WorkLogEntry, env: &RowEnv, cx: &mut Context<ThreadView>) -> Option<AnyElement> {
    if pending_approval(entry, env).is_some() {
        return None;
    }
    let item = entry.item.as_ref()?;
    let TurnItemKind::Notification { source, outcome, .. } = &item.kind else { return None };
    let child_thread_id = notification_child_thread_id(source)?;
    let task_ids: Option<Vec<&str>> = (source.get("kind").and_then(Value::as_str) == Some("delegated_task"))
        .then(|| source.get("taskIds").and_then(Value::as_array).map(|arr| arr.iter().filter_map(Value::as_str).collect::<Vec<_>>()))
        .flatten();
    let app = env.state.read(cx);
    let projection = env.thread_id.and_then(|tid| app.thread_state(tid).projection.as_ref())?;
    let agent: Subagent = subagents_of(projection).into_iter().find(|agent| match &task_ids {
        Some(ids) => ids.contains(&agent.id.as_str()),
        None => agent.child_thread_id.as_deref() == Some(child_thread_id),
    })?;
    let provider_instance_id = projection.visible_turn_items.iter().find_map(|projected| match &projected.item.kind {
        TurnItemKind::Subagent { provider_instance_id, .. } if projected.item.id == agent.id => provider_instance_id.as_deref(),
        _ => None,
    });
    let marks = provider_marks(app);
    let event_status = match outcome.as_str() {
        "completed" => Some("completed"),
        "failed" => Some("failed"),
        "cancelled" => Some("cancelled"),
        _ => None,
    };
    let outcome_label = match outcome.as_str() {
        "failed" => "Failed",
        "cancelled" => "Stopped",
        "updated" => "Updated",
        _ => "Finished",
    };
    let active = matches!(agent.status.as_str(), "pending" | "running" | "waiting");
    let started_after = parse_iso_ms(&entry.created_at)
        .zip(agent.started_at.as_deref().and_then(parse_iso_ms))
        .is_some_and(|(notified, started)| started > notified);
    let resumed = event_status.is_some() && (active || started_after);
    let live_status = agent.status.as_str();
    let status = if resumed { Some(live_status) } else { event_status };
    let status_text = if resumed { subagent_status_label(live_status) } else { outcome_label };
    let effective = status.unwrap_or(live_status);
    let settled = matches!(effective, "completed" | "failed" | "cancelled" | "interrupted");
    let raw = if settled {
        agent.result.as_deref().or(agent.progress.as_deref())
    } else {
        agent.progress.as_deref().or(agent.result.as_deref())
    };
    let detail = raw
        .map(str::trim)
        .filter(|d| !d.is_empty() && !d.starts_with("Child task ended with status"))
        .map(plain_detail)
        .filter(|d| !d.is_empty());
    let remote_label = agent
        .remote_child
        .as_ref()
        .and_then(|v| v.get("environmentLabel").or_else(|| v.get("label")).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|label| format!("on {label}"));
    let failed = status == Some("failed");
    let p = palette(cx);
    let dot = match effective {
        "pending" | "running" | "waiting" => hsla(p.info),
        "completed" => hsla(p.success),
        "failed" => hsla(p.destructive),
        _ => hsla(p.muted_foreground).opacity(0.6),
    };
    let detail_color = hsla(if failed { p.destructive } else { p.muted_foreground });
    let title = format_subagent_display_title(agent.title.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Subagent"));
    let right_time = if !resumed {
        Some(timestamp(&env.clock, &entry.created_at, false, cx))
    } else if active {
        agent.started_at.clone().map(|started_at| {
            SubagentLiveElapsed { item_id: SharedString::from(format!("notif:{}", entry.id)), started_at }.into_any_element()
        })
    } else {
        agent
            .started_at
            .as_deref()
            .and_then(parse_iso_ms)
            .zip(agent.completed_at.as_deref().and_then(parse_iso_ms))
            .map(|(start, end)| {
                let text = format_duration(((end - start).max(0) / 1_000 * 1_000).max(1_000) as f64);
                div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(text).into_any_element()
            })
    };
    let child = child_thread_id.to_owned();
    let row = h_flex()
        .id(SharedString::from(format!("subagent-notif:{}", entry.id)))
        .w_full()
        .min_w_0()
        .gap(px(10.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(6.))
        .child(
            div()
                .relative()
                .size(px(24.))
                .flex_none()
                .rounded_full()
                .bg(hsla(p.muted))
                .flex()
                .items_center()
                .justify_center()
                .child(subagent_avatar(provider_instance_id, &marks, 16., cx))
                .child(div().absolute().right_0().bottom_0().size(px(7.)).rounded_full().bg(dot)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap(px(6.))
                        .child(div().min_w_0().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                        .when_some(remote_label, |this, remote| {
                            this.child(div().flex_none().text_size(px(10.)).text_color(hsla(p.muted_foreground)).child(remote))
                        }),
                )
                .child(div().min_w_0().truncate().text_size(px(11.)).text_color(detail_color).child(detail.clone().unwrap_or_else(|| status_text.to_owned()))),
        )
        .when(detail.is_some() && (!resumed || status != Some("completed")), |this| {
            this.child(div().flex_none().text_xs().text_color(detail_color).child(status_text))
        })
        .children(right_time);
    Some(
        focusable(row, cx)
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent).opacity(0.2)))
            .child(Icon::ChevronRight.render(px(14.), hsla(p.muted_foreground).opacity(0.6)))
            .on_click(cx.listener(move |view, _, _, cx| view.open_thread(&child, cx)))
            .into_any_element(),
    )
}

fn thread_read_target(entry: &WorkLogEntry, preview: &str, env: &RowEnv, cx: &App) -> Option<(String, String, String)> {
    let target_id = thread_read_target_id(entry)?;
    let prefix = thread_read_label_prefix(preview)?;
    let shell = env.state.read(cx).thread_shell(&target_id)?;
    let title = thread_read_target_title(&shell.title, shell.archived_at.as_deref(), shell.deleted_at.as_deref())?;
    Some((prefix, target_id, title))
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
    subagent_notification_link(entry, env, cx).unwrap_or_else(|| entry_row(entry, None, true, env, markdown, cx))
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
    if let Some(approval) = pending_approval(entry, env) {
        return approval_card(approval, Some(&entry.created_at), grouped, env, cx);
    }
    let p = palette(cx);
    if let Some(item) = entry.item.as_ref()
        && let TurnItemKind::Error { failure, .. } = &item.kind
        && item.status == "failed"
    {
        let warning = failure.class == "usage_limit";
        let color = hsla(if warning { p.warning } else { p.destructive });
        let label = if warning {
            let zone = env.clock.zone;
            t3_core::timeline::usage_limit_label(
                failure.reset_at.as_deref(),
                env.clock.now_ms,
                &zone,
                zone.name(),
                crate::settings::store::client(cx).timestamp_format(),
                &crate::timestamp::SystemTimestampLocale,
            )
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
    let viewed_image = (!reasoning).then(|| work_entry_viewed_image(entry, env.thread_id, env.workspace_root)).flatten();
    let thread_read = (!reasoning).then(|| thread_read_target(entry, &preview, env, cx)).flatten();
    let can_expand = viewed_image.is_some() || entry_can_expand(entry, env.workspace_root);
    let mut trailing = Vec::new();
    if let Some(TurnItemKind::ThreadCreated { target_thread_id, .. }) = entry.item.as_ref().map(|item| &item.kind) {
        let target_thread_id = target_thread_id.clone();
        trailing.push(inline_button(
            SharedString::from(format!("thread-created-open:{}", entry.id)),
            "Open chat",
            InlineTone::Default,
            false,
            cx.listener(move |view, _, _, cx| {
                cx.stop_propagation();
                view.open_thread(&target_thread_id, cx);
            }),
            cx,
        ));
    }
    if let Some(TurnItemKind::Notification { source, .. }) = entry.item.as_ref().map(|item| &item.kind)
        && let Some(child_thread_id) = notification_child_thread_id(source).map(str::to_owned)
    {
        trailing.push(inline_button(
            SharedString::from(format!("notif-open:{}", entry.id)),
            "Open subagent",
            InlineTone::Default,
            false,
            cx.listener(move |view, _, _, cx| {
                cx.stop_propagation();
                view.open_thread(&child_thread_id, cx);
            }),
            cx,
        ));
    }
    if failed && !severe {
        trailing.push(Icon::WorkX.render(rem(12.).to_pixels(cx.theme().font_size), hsla(p.tool_error_icon).opacity(0.4)).into_any_element());
    }
    trailing.push(timestamp(&env.clock, &entry.created_at, false, cx));
    trailing.push(chevron(expanded, can_expand, cx));
    let heading = if let Some((prefix, target_id, title)) = thread_read {
        h_flex()
            .min_w_0()
            .text_color(label_color)
            .when(warning || severe, |this| this.font_weight(FontWeight::MEDIUM))
            .child(div().flex_none().child(format!("{prefix} “")))
            .child(
                focusable(
                    div()
                        .id(SharedString::from(format!("thread-read:{}:{}", entry.id, target_id)))
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_color(hsla(p.foreground))
                        .hover(|style| style.underline())
                        .child(title),
                    cx,
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.open_thread(&target_id, cx);
                })),
            )
            .child(div().flex_none().child("”"))
            .into_any_element()
    } else {
        div()
            .min_w_0()
            .truncate()
            .text_color(label_color)
            .when(warning || severe, |this| this.font_weight(FontWeight::MEDIUM))
            .child(preview)
            .into_any_element()
    };
    let label = h_flex()
        .min_w_0()
        .gap(rem(6.))
        .child(heading)
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
    } else if expanded {
        if let Some(viewed) = viewed_image {
            frame = frame.child(
                div()
                    .id(SharedString::from(format!("work-media:{}", entry.id)))
                    .ml(rem(28.))
                    .mt(rem(4.))
                    .mb(rem(6.))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(tool_output_image(SharedString::from(format!("work-viewed:{}", entry.id)), viewed.source, &viewed.alt, cx)),
            );
        }
        if let Some(body) = entry_body(entry, env.workspace_root) {
            frame = frame.child(expanded_body(entry, body, env, cx));
        }
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

fn approval_icon(kind: &str) -> Icon {
    match kind {
        "command" => Icon::WorkTerminal,
        "file-change" => Icon::WorkSquarePen,
        "file-read" => Icon::WorkEye,
        "permission" => Icon::WorkWrench,
        _ => Icon::WorkHammer,
    }
}

/// `TimelinePendingApprovalRow`: inline approval card for matched timeline rows and unmatched
/// footer approvals.
pub fn approval_card(
    approval: &PendingApproval,
    created_at: Option<&str>,
    grouped: bool,
    env: &RowEnv,
    cx: &mut Context<ThreadView>,
) -> AnyElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let icon = approval_icon(&approval.request_kind);
    let (label, _) = approval.presentation();
    let detail = approval.detail.as_deref().filter(|text| !text.is_empty()).unwrap_or(label).to_owned();
    let request_id = approval.request_id.clone();
    let disabled = !env.can_operate_thread || env.rewinding || env.responding_approvals.contains(&request_id);
    let menu_open = env.open_approval_menu == Some(request_id.as_str()) && !disabled;
    let override_open = env.open_approval_override == Some(request_id.as_str());
    let (primary, more) = approval.split_options();
    let more: Vec<(ApprovalDecision, String, bool)> =
        more.into_iter().map(|option| (option.decision, option.label.clone(), option.warning.is_some())).collect();

    let primary_actions: Vec<AnyElement> = primary
        .into_iter()
        .map(|option| {
            let (request_id, decision) = (request_id.clone(), option.decision);
            let tone = if decision == ApprovalDecision::Accept { InlineTone::Default } else { InlineTone::Muted };
            let icon = option.warning.as_ref().map(|_| Icon::TriangleAlert);
            let warning = option.warning.clone().map(SharedString::from);
            inline_button_with(
                SharedString::from(format!("approval-{}:{}", decision.as_str(), request_id)),
                option.label.clone(),
                icon,
                warning,
                tone,
                disabled,
                cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.respond_to_approval(request_id.clone(), decision, cx);
                }),
                cx,
            )
        })
        .collect();
    let more_control = (!more.is_empty()).then(|| {
        let menu_request_id = request_id.clone();
        let item_request_id = request_id.clone();
        div()
            .relative()
            .child(inline_button(
                SharedString::from(format!("approval-more:{request_id}")),
                "More",
                InlineTone::Muted,
                disabled,
                cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    if !disabled {
                        view.toggle_approval_menu(menu_request_id.clone(), cx);
                    }
                }),
                cx,
            ))
            .when(menu_open, |this| {
                this.child(floating(
                    Float::AboveEnd,
                    menu_surface(cx)
                        .occlude()
                        .mb(px(4.))
                        .on_mouse_down_out(cx.listener(|view, _, _, cx| view.close_approval_menu(cx)))
                        .children(more.iter().map(|(decision, label, warning)| {
                            let (request_id, decision) = (item_request_id.clone(), *decision);
                            let item = MenuItem::new(SharedString::from(format!("more-{}:{request_id}", decision.as_str())), label.clone())
                                .disabled(disabled)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    cx.stop_propagation();
                                    view.respond_to_approval(request_id.clone(), decision, cx);
                                }));
                            if *warning { item.icon(Icon::TriangleAlert, hsla(p.warning)) } else { item }
                        })),
                ))
            })
            .into_any_element()
    });

    let override_control = approval.full_access_override.as_deref().map(|kind| {
        let explanation = requests::full_access_override_explanation(kind);
        let override_request_id = request_id.clone();
        div()
            .relative()
            .child(inline_button_with(
                SharedString::from(format!("approval-override:{request_id}")),
                requests::FULL_ACCESS_OVERRIDE_QUESTION,
                None,
                Some(SharedString::from(explanation)),
                InlineTone::Muted,
                false,
                cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.toggle_approval_override(override_request_id.clone(), cx);
                }),
                cx,
            ))
            .when(override_open, |this| {
                this.child(floating(
                    Float::AboveStart,
                    div()
                        .occlude()
                        .mb(px(4.))
                        .w(px(300.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(if dark { p.popover } else { p.card }))
                        .p(px(10.))
                        .text_xs()
                        .line_height(px(16.))
                        .text_color(hsla(p.foreground))
                        .on_mouse_down_out(cx.listener(|view, _, _, cx| view.close_approval_override(cx)))
                        .child(explanation),
                ))
            })
            .into_any_element()
    });

    let ts = created_at.or_else(|| (!approval.created_at.is_empty()).then_some(approval.created_at.as_str()));
    let trailing: Vec<AnyElement> = ts.map(|iso| timestamp(&env.clock, iso, false, cx)).into_iter().collect();

    work_row_frame(SharedString::from(format!("approval:{}", approval.request_id)), grouped)
        .child(work_line(
            Some(icon_slot(icon, hsla(p.icon_muted))),
            div()
                .id(SharedString::from(format!("approval-detail:{}", approval.request_id)))
                .max_h(rem(160.))
                .overflow_y_scroll()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .line_height(rem(20.))
                .text_color(hsla(p.foreground))
                .child(detail)
                .into_any_element(),
            trailing,
            true,
            cx,
        ))
        .child(
            v_flex()
                .ml(rem(30.))
                .gap(rem(4.))
                .pb(rem(4.))
                .text_xs()
                .children(approval.app_name.clone().map(|name| div().text_color(hsla(p.muted_foreground)).child(name)))
                .children(approval.reason.clone().map(|reason| div().text_color(hsla(p.muted_foreground)).child(reason)))
                .child(
                    h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x(px(8.))
                        .gap_y(px(4.))
                        .when(approval.live, |this| {
                            this.child(
                                h_flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .text_color(hsla(p.warning_foreground))
                                    .child(Icon::Shield.render(rem(12.).to_pixels(cx.theme().font_size), hsla(p.warning_foreground)))
                                    .child("Waiting for approval"),
                            )
                            .child(div().text_color(hsla(p.muted_foreground)).child("·"))
                            .children(primary_actions)
                            .children(more_control)
                        })
                        .when(!approval.live, |this| {
                            this.child(
                                div()
                                    .text_color(hsla(p.warning_foreground))
                                    .child("Provider process is gone. Interrupt or restart the run to respond."),
                            )
                        })
                        .children(override_control),
                )
                .children(env.approval_errors.get(&approval.request_id).map(|err| {
                    div().text_color(hsla(p.destructive)).child(err.clone())
                })),
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
    if let Some(approval) = pending_approval(entry, env) {
        return approval_card(approval, Some(&entry.created_at), false, env, cx);
    }
    let p = palette(cx);
    let failed = work_entry_display_indicates_tool_failure(entry);
    let thought_line = thought.filter(|_| !expanded).map(|t| live_thought_line(t.detail.as_deref().unwrap_or_default())).unwrap_or_default();
    let thought_is_status = !thought_line.is_empty() && thought.is_some_and(|t| Arc::ptr_eq(t, entry));
    let label = if thought_is_status {
        if active { "Thinking".to_owned() } else { "Thought".to_owned() }
    } else {
        let base = live_work_entry_label(entry, env.workspace_root, active);
        if let Some((prefix, _id, title)) = thread_read_target(entry, &base, env, cx) {
            format!("{prefix} “{title}”")
        } else {
            base
        }
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
    let icon = work_icon(tool_surface.as_deref().map(tool_surface_icon).unwrap_or_else(|| summary_icon(*summary_kind)));
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
