//! Work, tool and reasoning rows: `TimelineWorkGroupRows.tsx`, `TimelineWorkEntryRow.tsx`,
//! `TimelinePendingApprovalRow.tsx` and the details of `V2ItemInspector.tsx`, laid out per
//! design system §7.7. Approval actions belong to the composer, so the row shows state only.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListSizingBehavior, ListState, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, list, prelude::FluentBuilder, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use serde_json::Value;
use t3_core::timeline::{
    RowKind, TimelineRow, WorkIcon, has_question_answer, question_answer_preview, question_text_preview, WorkLogEntry, format_workspace_relative_path, live_thought_line, live_work_entry_label, skill_args, summary_icon,
    tool_group_action, work_entry_display_indicates_tool_failure, work_entry_display_label, work_entry_icon, work_entry_is_visible_in_group,
    work_entry_read_output, work_log_entry_is_tool_like, ToolGroupAction, ToolStatus, command_output_text,
};
use t3_core::turn_item::TurnItemKind;

use super::markdown::{MarkdownStore, prose};
use super::parts::{chevron, focusable, icon_slot, mono_block, outline_button, timestamp, work_line, work_row_frame};
use super::{Detail, RowEnv};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
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
    let cap = if reasoning { 384. } else { 288f32.min(env.window_height * 0.5) + opened * 264. };
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
    if reasoning { body.ml(px(28.)).px(px(2.)).py(px(4.)).into_any_element() } else { body.into_any_element() }
}

/// One member of an open group: a work row, or one thought of a reasoning trace.
pub fn group_member(entry: &Arc<WorkLogEntry>, env: &RowEnv, markdown: &mut MarkdownStore, cx: &mut Context<ThreadView>) -> AnyElement {
    if entry.item_type == Some("reasoning")
        && let Some(detail) = entry.detail.as_deref()
    {
        let text: Arc<str> = Arc::from(detail);
        let state = markdown.state(&format!("reasoning:{}", entry.id), &text, cx);
        return div().pb(px(12.)).child(prose(&state, cx)).into_any_element();
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
                div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(color).child(label).into_any_element(),
                vec![timestamp(&env.clock, &entry.created_at, true, cx)],
                true,
                cx,
            ))
            .when(!warning, |this| {
                this.child(
                    div()
                        .ml(px(28.))
                        .py(px(4.))
                        .text_size(px(14.))
                        .line_height(px(22.75))
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
    let can_expand = if reasoning { entry.detail.as_deref().is_some_and(|d| !d.trim().is_empty()) } else { has_details(entry, env) };
    let mut trailing = Vec::new();
    if failed && !severe {
        trailing.push(Icon::WorkX.render(px(12.), hsla(p.tool_error_icon).opacity(0.4)).into_any_element());
    }
    trailing.push(timestamp(&env.clock, &entry.created_at, false, cx));
    trailing.push(chevron(expanded, can_expand, cx));
    let label = h_flex()
        .min_w_0()
        .gap(px(6.))
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
                .ml(px(28.))
                .px(px(2.))
                .py(px(4.))
                .max_h(px(384.))
                .overflow_y_scroll()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(prose(&state, cx)),
        );
    } else if expanded && let Some(details) = entry_details(entry, env, cx) {
        frame = frame.child(
            div()
                .id(SharedString::from(format!("work-detail:{}", entry.id)))
                .mt(px(2.))
                .mb(px(6.))
                .ml(px(28.))
                .max_h(px(256.))
                .overflow_y_scroll()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(details),
        );
    }
    frame.into_any_element()
}

/// Whether a row opens to anything: `turnItemHasDetail` plus the plain-output cases.
fn has_details(entry: &WorkLogEntry, env: &RowEnv) -> bool {
    if tool_group_action(entry) == ToolGroupAction::Read {
        return work_entry_read_output(entry, env.workspace_root).is_some();
    }
    if let Some(args) = skill_args(entry) {
        return args.is_some();
    }
    let non_empty = |text: &str| !text.trim().is_empty();
    let from_item = entry.item.as_ref().is_some_and(|item| match &item.kind {
        TurnItemKind::CommandExecution { input, output, output_omitted, exit_code, .. } => {
            non_empty(input) || *output_omitted || output.as_deref().is_some_and(non_empty) || exit_code.is_some_and(|c| c != 0)
        }
        TurnItemKind::FileChange { .. } => true,
        TurnItemKind::FileSearch { pattern, results } => results.as_ref().is_some_and(|r| !r.is_empty()) || pattern.as_deref().is_some_and(non_empty),
        TurnItemKind::WebSearch { patterns, results } => results.as_ref().is_some_and(|r| !r.is_empty()) || patterns.as_ref().is_some_and(|p| !p.is_empty()),
        TurnItemKind::DynamicTool { input, output_omitted, .. } => *output_omitted || !input.is_null(),
        TurnItemKind::ApprovalRequest { prompt, .. } => prompt.as_deref().is_some_and(non_empty),
        TurnItemKind::UserInputRequest { questions, .. } => !questions.is_empty(),
        TurnItemKind::Notification { detail, .. } => detail.as_deref().is_some_and(non_empty),
        TurnItemKind::SystemNotice { message } => non_empty(message),
        TurnItemKind::Error { failure, .. } => non_empty(&failure.message),
        TurnItemKind::TodoList { steps, .. } => !steps.is_empty(),
        TurnItemKind::Compaction { summary, .. } => summary.as_deref().is_some_and(non_empty),
        _ => false,
    });
    from_item || (entry.item.is_none() && [entry.command.as_deref(), entry.detail.as_deref()].into_iter().flatten().any(non_empty))
}

fn json_text(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// `formatToolValue`: text content blocks as text, other values as pretty JSON.
fn tool_value_text(value: &Value) -> Option<String> {
    fn blocks(value: &Value, depth: usize) -> Option<String> {
        if depth > 4 {
            return None;
        }
        match value {
            Value::String(text) => Some(text.clone()),
            Value::Array(items) => items.iter().map(|item| blocks(item, depth + 1)).collect::<Option<Vec<_>>>().map(|parts| {
                parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n")
            }),
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("text") {
                    return object.get("text").and_then(Value::as_str).map(str::to_owned);
                }
                if object.get("type").and_then(Value::as_str) == Some("image") {
                    return Some("[image]".into());
                }
                let keys: Vec<&String> = object.keys().filter(|k| *k != "isError" && *k != "is_error").collect();
                if keys.len() == 1 && keys[0] == "content" {
                    return object.get("content").and_then(|content| blocks(content, depth + 1));
                }
                None
            }
            _ => None,
        }
    }
    match value {
        Value::Null => None,
        other => match blocks(other, 0) {
            Some(text) => Some(text).filter(|t| !t.trim().is_empty()),
            None => Some(json_text(other)).filter(|t| t != "{}" && t != "[]"),
        },
    }
}

fn muted_line(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div().text_xs().italic().text_color(hsla(palette(cx).muted_foreground)).child(text.into()).into_any_element()
}

fn fetched_item<'a>(entry: &WorkLogEntry, env: &'a RowEnv) -> Option<&'a Arc<t3_core::turn_item::TurnItem>> {
    match env.details.get(&entry.item.as_ref()?.id)? {
        Detail::Loaded { item, .. } => Some(item),
        _ => None,
    }
}

/// `ToolOutput`'s states for output fetched with `getTurnItem`.
fn fetch_status(entry: &WorkLogEntry, env: &RowEnv, cx: &App) -> AnyElement {
    let detail = entry.item.as_ref().and_then(|item| env.details.get(&item.id));
    match detail {
        Some(Detail::Failed { message, .. }) => {
            div().text_xs().text_color(hsla(palette(cx).destructive)).child(format!("Couldn't load output: {message}")).into_any_element()
        }
        Some(Detail::Missing { .. }) => muted_line("Output is no longer available.", cx),
        Some(Detail::Loaded { .. }) => muted_line("No output.", cx),
        Some(Detail::Loading { .. }) | None => muted_line("Loading output…", cx),
    }
}

/// The expanded body: `V2ItemInspector` for projected items, plain output for reads and skills.
fn entry_details(entry: &WorkLogEntry, env: &RowEnv, cx: &mut Context<ThreadView>) -> Option<AnyElement> {
    let p = palette(cx);
    if tool_group_action(entry) == ToolGroupAction::Read {
        return work_entry_read_output(entry, env.workspace_root).map(|paths| mono_block(paths, cx).into_any_element());
    }
    if let Some(args) = skill_args(entry) {
        return args.map(|args| mono_block(args, cx).into_any_element());
    }
    let item = entry.item.as_ref()?;
    let mut parts: Vec<AnyElement> = Vec::new();
    match &item.kind {
        TurnItemKind::CommandExecution { input, output, output_omitted, exit_code, .. } => {
            parts.push(mono_block(format!("$ {}", t3_core::timeline::command_display_text(input)), cx).text_color(hsla(p.foreground).opacity(0.85)).into_any_element());
            let fetched = fetched_item(entry, env);
            let output = match (*output_omitted, fetched) {
                (true, Some(item)) => match &item.kind {
                    TurnItemKind::CommandExecution { output, .. } => output.clone(),
                    _ => None,
                },
                _ => output.clone(),
            };
            match output.as_deref().and_then(command_output_text) {
                Some(output) => parts.push(mono_block(output.trim_end().to_owned(), cx).into_any_element()),
                None if *output_omitted => parts.push(fetch_status(entry, env, cx)),
                None => {}
            }
            let exit_code = match fetched.map(|item| &item.kind) {
                Some(TurnItemKind::CommandExecution { exit_code, .. }) => exit_code.or(*exit_code),
                _ => *exit_code,
            };
            if let Some(code) = exit_code.filter(|code| *code != 0) {
                parts.push(mono_block(format!("exit {code}"), cx).text_color(hsla(p.destructive)).into_any_element());
            }
        }
        TurnItemKind::FileChange { file_name, additions, deletions, diff_str, changes, .. } => {
            let mut header = h_flex().gap(px(8.)).flex_wrap().child(mono_block(format_workspace_relative_path(file_name, env.workspace_root), cx).w_auto());
            if additions.is_some() || deletions.is_some() {
                header = header.child(
                    h_flex()
                        .gap(px(4.))
                        .text_xs()
                        .child(div().text_color(hsla(p.diff_addition_foreground)).child(format!("+{}", additions.unwrap_or(0))))
                        .child(div().text_color(hsla(p.diff_deletion_foreground)).child(format!("-{}", deletions.unwrap_or(0)))),
                );
            }
            let target = t3_core::diff::ToolCallTarget::of(item);
            let open = cx.listener(move |view, _, _, cx| view.open_tool_call_diff(target.clone(), cx));
            header = header.child(outline_button(SharedString::from(format!("open-tool-diff:{}", item.id)), "Open diff", open, cx));
            parts.push(header.into_any_element());
            if item.status == "failed" && let Some(diff) = diff_str.as_deref().filter(|d| !d.trim().is_empty()) {
                parts.push(mono_block(diff.to_owned(), cx).into_any_element());
            }
            for change in changes.iter().flatten() {
                let old = change.old_path.as_deref().map(|old| format!("{old} → ")).unwrap_or_default();
                parts.push(mono_block(format!("{} {old}{}", change.operation, format_workspace_relative_path(&change.path, env.workspace_root)), cx).into_any_element());
            }
        }
        TurnItemKind::FileSearch { pattern, results } => {
            if let Some(pattern) = pattern.as_deref().filter(|p| !p.trim().is_empty()) {
                parts.push(mono_block(pattern.to_owned(), cx).into_any_element());
            }
            for result in results.iter().flatten() {
                let file = result.get("fileName").and_then(Value::as_str).unwrap_or_default();
                let line = result.get("line").and_then(Value::as_u64).map(|l| format!(":{l}")).unwrap_or_default();
                let preview = result.get("preview").and_then(Value::as_str).map(|p| format!("\n{p}")).unwrap_or_default();
                parts.push(mono_block(format!("{}{line}{preview}", format_workspace_relative_path(file, env.workspace_root)), cx).into_any_element());
            }
        }
        TurnItemKind::WebSearch { patterns, results } => {
            if let Some(patterns) = patterns.as_ref().filter(|p| !p.is_empty()) {
                parts.push(mono_block(patterns.join("\n"), cx).into_any_element());
            }
            for result in results.iter().flatten() {
                let get = |key: &str| result.get(key).and_then(Value::as_str).map(str::to_owned);
                let title = get("title").or_else(|| get("url")).unwrap_or_else(|| "Search result".into());
                parts.push(
                    v_flex()
                        .text_xs()
                        .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                        .children(get("snippet").map(|snippet| div().text_color(hsla(p.muted_foreground)).child(snippet)))
                        .into_any_element(),
                );
            }
        }
        TurnItemKind::DynamicTool { input, output, output_omitted, .. } => {
            match input {
                Value::Object(object) if object.get("truncated") != Some(&Value::Bool(true)) => {
                    for (key, value) in object {
                        let text = match value {
                            Value::String(text) if !text.is_empty() => text.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        parts.push(
                            h_flex()
                                .items_start()
                                .gap(px(6.))
                                .child(mono_block(key.clone(), cx).w_auto().flex_none().text_color(hsla(p.muted_foreground)))
                                .child(mono_block(text, cx).text_color(hsla(p.foreground).opacity(0.85)))
                                .into_any_element(),
                        );
                    }
                }
                other => {
                    if let Some(text) = tool_value_text(other) {
                        parts.push(mono_block(text, cx).into_any_element());
                    }
                }
            }
            let output = match (*output_omitted, fetched_item(entry, env).map(|item| &item.kind)) {
                (true, Some(TurnItemKind::DynamicTool { output, .. })) => output.clone(),
                (true, _) => None,
                (false, _) => output.clone(),
            };
            match output.as_ref().and_then(tool_value_text) {
                Some(text) => parts.push(mono_block(text, cx).text_color(hsla(p.muted_foreground)).into_any_element()),
                None if *output_omitted => parts.push(fetch_status(entry, env, cx)),
                None => {}
            }
        }
        TurnItemKind::ApprovalRequest { prompt: Some(prompt), .. } => parts.push(mono_block(prompt.clone(), cx).into_any_element()),
        TurnItemKind::UserInputRequest { questions, question_answer, .. } => {
            let text = questions
                .iter()
                .map(|question| {
                    let mut lines = vec![question.get("question").and_then(Value::as_str).unwrap_or_default().to_owned()];
                    for option in question.get("options").and_then(Value::as_array).into_iter().flatten() {
                        lines.push(format!("• {}", option.get("label").and_then(Value::as_str).unwrap_or_default()));
                    }
                    lines.join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            if !text.trim().is_empty() {
                parts.push(mono_block(text, cx).into_any_element());
            }
            if let Some(answers) = question_answer.as_ref().and_then(|a| a.get("answers")).and_then(Value::as_object) {
                for answer in answers.values() {
                    let text = match answer {
                        Value::String(text) => text.clone(),
                        Value::Array(values) => values.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                        other => answer_text(other),
                    };
                    if !text.is_empty() {
                        parts.push(div().ml(px(12.)).text_size(px(14.)).text_color(hsla(p.muted_foreground)).child(text).into_any_element());
                    }
                }
            }
        }
        TurnItemKind::Notification { detail: Some(detail), .. } => parts.push(mono_block(detail.clone(), cx).into_any_element()),
        TurnItemKind::SystemNotice { message } if !message.trim().is_empty() => parts.push(mono_block(message.clone(), cx).into_any_element()),
        TurnItemKind::Error { failure, .. } => parts.push(mono_block(failure.message.clone(), cx).into_any_element()),
        TurnItemKind::TodoList { steps, explanation, .. } => {
            if let Some(explanation) = explanation.as_deref().filter(|e| !e.trim().is_empty()) {
                parts.push(div().text_xs().text_color(hsla(p.muted_foreground)).child(explanation.to_owned()).into_any_element());
            }
            let text = steps
                .iter()
                .map(|step| {
                    let mark = match step.status.as_str() {
                        "completed" => "✓",
                        "running" | "in_progress" => "◉",
                        _ => "○",
                    };
                    format!("{mark} {}", step.text)
                })
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(mono_block(text, cx).into_any_element());
        }
        TurnItemKind::Compaction { summary: Some(summary), .. } => parts.push(mono_block(summary.clone(), cx).into_any_element()),
        _ => {}
    }
    if parts.is_empty() {
        let fallback = [entry.command.as_deref(), entry.detail.as_deref()].into_iter().flatten().map(str::trim).filter(|t| !t.is_empty()).collect::<Vec<_>>();
        if fallback.is_empty() {
            return None;
        }
        parts.push(mono_block(fallback.join("\n\n"), cx).into_any_element());
    }
    Some(v_flex().gap(px(6.)).w_full().min_w_0().children(parts).into_any_element())
}

fn answer_text(value: &Value) -> String {
    match value {
        Value::Object(object) => object
            .get("answers")
            .or_else(|| object.get("text"))
            .map(|inner| match inner {
                Value::Array(items) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                Value::String(text) => text.clone(),
                _ => String::new(),
            })
            .unwrap_or_default(),
        _ => String::new(),
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
                .max_h(px(160.))
                .overflow_y_scroll()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .line_height(px(20.))
                .text_color(hsla(p.foreground))
                .child(detail)
                .into_any_element(),
            Vec::new(),
            true,
            cx,
        ))
        .child(
            v_flex()
                .ml(px(30.))
                .gap(px(4.))
                .pb(px(4.))
                .text_xs()
                .children(app_name.clone().map(|name| div().text_color(hsla(p.muted_foreground)).child(name)))
                .children(reason.clone().map(|reason| div().text_color(hsla(p.muted_foreground)).child(reason)))
                .child(
                    h_flex()
                        .gap(px(4.))
                        .text_color(hsla(p.warning_foreground))
                        .child(Icon::Shield.render(px(12.), hsla(p.warning_foreground)))
                        .child("Waiting for approval"),
                ),
        )
        .into_any_element()
}

/// `LiveActivityContent`: icon, label and a trailing failure mark.
fn live_activity(label: AnyElement, icon: Icon, failed: bool, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let color = if failed { hsla(p.tool_error_icon).opacity(0.4) } else { hsla(p.icon_muted) };
    let trailing = if failed { vec![Icon::WorkX.render(px(12.), color).into_any_element()] } else { Vec::new() };
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
