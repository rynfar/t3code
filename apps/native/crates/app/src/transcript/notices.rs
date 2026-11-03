//! Turn folds, the working row, compaction dividers and lifecycle events:
//! `MessagesTimeline.tsx` (`TurnFoldTimelineRow`, `AttemptFoldTimelineRow`,
//! `ContextCompactionTimelineRow`), `TimelineWorkGroupRows.tsx` (`WorkingTimelineRow`),
//! `TimelineV2EventRow.tsx`, `V2LifecycleRow.tsx` and the request cards
//! (`SecretRequestCard.tsx`, `LinkRequestCard.tsx`) in their answered and waiting states.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};
use gpui_component::{h_flex, v_flex};
use t3_core::timeline::{CompactionTokens, format_duration, parse_iso_ms};
use t3_core::turn_item::{TurnItem, TurnItemKind};

use super::parts::{ROW_GROUP, DividerTone, focusable, icon_slot, system_divider, timestamp, work_line, work_row_frame};
use super::RowEnv;
use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;

pub fn turn_fold(id: &str, run_id: &str, label: &str, expanded: bool, created_at: Option<&str>, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let run_id = run_id.to_owned();
    h_flex()
        .id(SharedString::from(format!("fold:{id}")))
        .group(ROW_GROUP)
        .w_full()
        .gap(px(4.))
        .pt(px(4.))
        .pb(px(8.))
        .pr(px(2.))
        .border_b_1()
        .border_color(hsla(p.border).opacity(0.6))
        .child(focusable(
            h_flex()
                .id(SharedString::from(format!("fold-button:{id}")))
                .gap(px(4.))
                .px(px(4.))
                .rounded(px(6.))
                .text_size(px(14.))
                .line_height(px(22.75))
                .text_color(hsla(p.muted_foreground))
                .cursor_pointer()
                .hover(|style| style.text_color(hsla(p.foreground)))
                .on_click(cx.listener(move |view, _, _, cx| view.toggle_run(&run_id, cx)))
                .child(label.to_owned())
                .child((if expanded { Icon::ChevronDown } else { Icon::ChevronRight }).render(px(14.), hsla(p.muted_foreground))),
            cx,
        ))
        .child(div().flex_1())
        .children(created_at.map(|at| timestamp(&env.clock, at, false, cx)))
        .into_any_element()
}

pub fn attempt_fold(id: &str, attempt_id: &str, label: &str, expanded: bool, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let attempt_id = attempt_id.to_owned();
    let fold = h_flex()
        .id(SharedString::from(format!("attempt:{id}")))
        .w_full()
        .gap(px(8.))
        .px(px(10.))
        .py(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(hsla(p.border).opacity(0.6))
        .bg(hsla(p.muted).opacity(0.2))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(p.muted).opacity(0.35)))
        .on_click(cx.listener(move |view, _, _, cx| view.toggle_attempt(&attempt_id, cx)))
        .child((if expanded { Icon::ChevronDown } else { Icon::ChevronRight }).render(px(14.), hsla(p.muted_foreground)))
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.8)).child(label.to_owned()))
        .child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child("Cut off by a steer"));
    focusable(fold, cx).into_any_element()
}

/// `WorkingTimelineRow`: "Working for 12s", whose label is its own view that ticks itself, or
/// "Setting up worktree…" while the worktree is prepared, with the background setup chip.
pub fn working_row(compacting: bool, env: &RowEnv, chip: Option<AnyElement>, cx: &App) -> AnyElement {
    let p = palette(cx);
    let preparing = env.setup.view.preparing;
    let label = match env.working_timer {
        _ if preparing => div().whitespace_nowrap().child(t3_core::worktree_setup::PREPARING_LABEL).into_any_element(),
        Some(timer) => timer.clone().into_any_element(),
        None => div().child(if compacting { "Compacting…" } else { "Working..." }).into_any_element(),
    };
    div()
        .w_full()
        .pt(px(4.))
        .pb(px(8.))
        .border_b_1()
        .border_color(hsla(p.border).opacity(0.6))
        .child(
            h_flex()
                .h(px(24.))
                .gap(px(6.))
                .px(px(4.))
                .text_size(px(14.))
                .text_color(hsla(p.muted_foreground))
                .when(compacting && !preparing, |this| this.child(Icon::Minimize2.render(px(12.), hsla(p.muted_foreground))))
                .child(label)
                .children(chip),
        )
        .into_any_element()
}

/// `TimelineHistoryControl`: "Load earlier turns" between two hairlines, its loading state, and
/// the last error. Clicking again retries.
pub fn history_control(has_more: bool, loading: bool, error: Option<&str>, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let line = || div().h(px(1.)).flex_1().bg(hsla(p.border).opacity(0.7));
    let button = has_more.then(|| {
        let button = h_flex()
            .id("load-earlier-turns")
            .w_full()
            .gap(px(12.))
            .py(px(6.))
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(line())
            .child(div().flex_none().child(if loading { "Loading earlier turns…" } else { "Load earlier turns" }))
            .child(line());
        if loading {
            button.into_any_element()
        } else {
            focusable(button, cx)
                .cursor_pointer()
                .hover(|style| style.text_color(hsla(p.foreground)))
                .on_click(cx.listener(|view, _, _, cx| view.load_earlier(cx)))
                .into_any_element()
        }
    });
    v_flex()
        .w_full()
        .gap(px(6.))
        .pb(px(8.))
        .children(button)
        .children(error.map(|error| div().w_full().text_center().text_xs().text_color(hsla(p.muted_foreground)).child(error.to_owned())))
        .into_any_element()
}

/// `ContextCompactionTimelineRow`.
pub fn compaction_row(label: &str, tokens: Option<&CompactionTokens>, title: Option<&str>, failed: bool, failure: Option<&str>, cx: &App) -> AnyElement {
    let p = palette(cx);
    let color = hsla(if failed { p.destructive } else { p.muted_foreground });
    let line_color = if failed { hsla(p.destructive).opacity(0.3) } else { hsla(p.border).opacity(0.7) };
    let line = || div().h(px(1.)).flex_1().bg(line_color);
    let text = match tokens {
        Some(tokens) => format!("{} {} → {} tokens", title.unwrap_or("Context compacted"), tokens.before, tokens.after),
        None => label.to_owned(),
    };
    h_flex()
        .w_full()
        .gap(px(12.))
        .py(px(4.))
        .text_xs()
        .text_color(color)
        .child(line())
        .child(
            h_flex()
                .min_w_0()
                .gap(px(6.))
                .child((if failed { Icon::CircleAlert } else { Icon::Minimize2 }).render(px(12.), color))
                .child(text)
                .children(failure.map(|failure| div().min_w_0().truncate().opacity(0.8).child(format!("· {failure}")))),
        )
        .child(line())
        .into_any_element()
}

fn subagent_title(item: &TurnItem) -> String {
    let title = item.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).unwrap_or("Subagent");
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn status_label(status: &str) -> &'static str {
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

fn plain_detail(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let line = line.trim_start();
        let line = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).unwrap_or(line);
        out.push_str(line);
        out.push(' ');
    }
    out.replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn elapsed_label(item: &TurnItem, now_ms: i64) -> Option<String> {
    let start = parse_iso_ms(item.started_at.as_deref()?)?;
    let end = if item.is_running() { now_ms } else { parse_iso_ms(item.completed_at.as_deref()?)? };
    Some(format_duration(((end - start).max(0) / 1_000 * 1_000).max(1_000) as f64))
}

/// `SubagentTimelineLink`: avatar, title, latest progress or result, status and elapsed time.
fn subagent_row(item: &Arc<TurnItem>, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let TurnItemKind::Subagent { child_thread_id, progress, result, .. } = &item.kind else { return div().into_any_element() };
    let settled = matches!(item.status.as_str(), "completed" | "failed" | "cancelled" | "interrupted");
    let raw = if settled { result.as_deref().or(progress.as_deref()) } else { progress.as_deref().or(result.as_deref()) };
    let detail = raw
        .map(str::trim)
        .filter(|d| !d.is_empty() && !d.starts_with("Child task ended with status"))
        .map(plain_detail)
        .filter(|d| !d.is_empty());
    let failed = item.status == "failed";
    let status = status_label(&item.status);
    let dot = match item.status.as_str() {
        "pending" | "running" | "waiting" => hsla(p.info),
        "completed" => hsla(p.success),
        "failed" => hsla(p.destructive),
        _ => hsla(p.muted_foreground).opacity(0.6),
    };
    let detail_color = hsla(if failed { p.destructive } else { p.muted_foreground });
    let mut row = h_flex()
        .id(SharedString::from(format!("subagent:{}", item.id)))
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
                .child(Icon::WorkBot.render(px(14.), hsla(p.secondary_label)))
                .child(div().absolute().right_0().bottom_0().size(px(7.)).rounded_full().bg(dot)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().min_w_0().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(subagent_title(item)))
                .child(div().min_w_0().truncate().text_size(px(11.)).text_color(detail_color).child(detail.clone().unwrap_or_else(|| status.to_owned()))),
        )
        .when(detail.is_some() && item.status != "completed", |this| {
            this.child(div().flex_none().text_xs().text_color(detail_color).child(status))
        })
        .children(elapsed_label(item, env.clock.now_ms).map(|elapsed| div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(elapsed)));
    if let Some(child) = child_thread_id.clone() {
        row = focusable(row, cx)
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent).opacity(0.2)))
            .child(Icon::ChevronRight.render(px(14.), hsla(p.muted_foreground).opacity(0.6)))
            .on_click(cx.listener(move |view, _, _, cx| view.open_thread(&child, cx)));
    }
    row.into_any_element()
}

/// `V2SubagentGroup`: adjacent subagents of one turn behind an "N subagents" disclosure.
fn subagent_group(id: &str, members: &[Arc<TurnItem>], env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let expanded = env.ui.expanded_entries.contains(&format!("subagent-group:{id}"));
    let running = members.iter().filter(|m| m.is_running()).count();
    let failed = members.iter().filter(|m| m.status == "failed").count();
    let done = members.iter().filter(|m| m.status == "completed").count();
    let mut summary = Vec::new();
    if running > 0 {
        summary.push(format!("{running} running"));
    }
    if done > 0 {
        summary.push(format!("{done} completed"));
    }
    if failed > 0 {
        summary.push(format!("{failed} failed"));
    }
    let label = format!("{} {}", members.len(), if members.len() == 1 { "subagent" } else { "subagents" });
    let key = format!("subagent-group:{id}");
    let ring = hsla(p.ring);
    let header = h_flex()
        .id(SharedString::from(format!("subagents:{id}")))
        .tab_index(0)
        .focus_visible(move |style| style.border_2().border_color(ring))
        .w_full()
        .gap(px(10.))
        .py(px(8.))
        .pr(px(13.))
        .cursor_pointer()
        .text_color(if expanded || running > 0 { hsla(p.foreground) } else { hsla(p.muted_foreground) })
        .on_click(cx.listener(move |view, _, _, cx| view.toggle_entry(&key, cx)))
        .child(Icon::WorkBot.render(px(16.), hsla(p.icon_muted)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(label))
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(if running > 0 { hsla(p.info_foreground) } else if failed > 0 { hsla(p.destructive) } else { hsla(p.muted_foreground) })
                        .child(summary.join(", ")),
                ),
        )
        .child((if expanded { Icon::ChevronDown } else { Icon::ChevronRight }).render(px(14.), hsla(p.muted_foreground)));
    let list = expanded.then(|| {
        v_flex()
            .mt(px(4.))
            .mb(px(4.))
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.card).opacity(0.3))
            .children(members.iter().map(|member| subagent_row(member, env, cx)).collect::<Vec<_>>())
    });
    v_flex().w_full().child(header).children(list).into_any_element()
}

fn request_card(title: String, body: Option<String>, note: Option<String>, cx: &App) -> AnyElement {
    let p = palette(cx);
    v_flex()
        .w_full()
        .gap(px(4.))
        .p(px(16.))
        .rounded(px(12.))
        .border_1()
        .border_color(hsla(p.border).opacity(0.6))
        .bg(hsla(p.card))
        .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
        .children(body.filter(|b| !b.trim().is_empty()).map(|body| div().text_size(px(14.)).text_color(hsla(p.muted_foreground)).child(body)))
        .children(note.map(|note| {
            h_flex()
                .mt(px(8.))
                .gap(px(6.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(Icon::Shield.render(px(14.), hsla(p.muted_foreground)))
                .child(note)
        }))
        .into_any_element()
}

fn answered_line(id: &str, icon: Icon, text: String, cx: &App) -> AnyElement {
    work_row_frame(SharedString::from(format!("answered:{id}")), false)
        .child(work_line(Some(icon_slot(icon, hsla(palette(cx).icon_muted))), div().child(text).into_any_element(), Vec::new(), false, cx))
        .into_any_element()
}

/// `V2EventTimelineRow`.
pub fn event_row(id: &str, item: &Arc<TurnItem>, subagents: &[Arc<TurnItem>], resource_summary: bool, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    match &item.kind {
        TurnItemKind::Subagent { .. } if subagents.len() > 1 => subagent_group(id, subagents, env, cx),
        TurnItemKind::Subagent { .. } => subagent_row(item, env, cx),
        TurnItemKind::SecretRequest { label, reason, secret_status } => match secret_status.as_str() {
            "pending" => request_card(label.clone(), Some(reason.clone()), Some("Stored securely, never shown to the agent".into()), cx),
            "saved" => answered_line(id, Icon::Check, format!("{label} · Saved securely and kept private"), cx),
            "declined" => answered_line(id, Icon::X, format!("{label} · Declined"), cx),
            _ => answered_line(id, Icon::X, format!("{label} · Request ended"), cx),
        },
        TurnItemKind::LinkRequest { reason, link_status, label, failure, linked_label } => {
            let name = label.clone().unwrap_or_else(|| "Link a machine to this environment".into());
            match link_status.as_str() {
                "pending" => request_card(name, Some(reason.clone()), None, cx),
                "linked" => answered_line(id, Icon::Link, format!("Linked {}", linked_label.clone().or(label.clone()).unwrap_or_default()).trim().to_owned(), cx),
                "declined" => answered_line(id, Icon::X, format!("{name} · Declined"), cx),
                "failed" => answered_line(id, Icon::CircleAlert, format!("{name} · {}", failure.clone().unwrap_or_else(|| "Failed".into())), cx),
                _ => answered_line(id, Icon::X, format!("{name} · Request ended"), cx),
            }
        }
        TurnItemKind::RunInterruptResult { message } => {
            system_divider("Run interrupted".into(), Some(message.clone().into()).filter(|m: &SharedString| !m.is_empty()), Some(Icon::CircleStop), DividerTone::Neutral, cx)
        }
        TurnItemKind::Handoff { to_model, .. } => system_divider(
            "Context handoff".into(),
            to_model.clone().map(SharedString::from),
            Some(Icon::ArrowRightLeft),
            if item.status == "failed" { DividerTone::Danger } else { DividerTone::Neutral },
            cx,
        ),
        TurnItemKind::Fork { target_thread_id, source } => {
            let from_run = source.as_ref().and_then(|s| s.get("type")).and_then(|t| t.as_str()) == Some("run");
            let target = if from_run {
                source.as_ref().and_then(|s| s.get("threadId")).and_then(|t| t.as_str()).map(str::to_owned).unwrap_or_else(|| target_thread_id.clone())
            } else {
                target_thread_id.clone()
            };
            focusable(div().id(SharedString::from(format!("fork:{id}"))), cx)
                .cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| view.open_thread(&target, cx)))
                .child(system_divider(
                    if from_run { "Forked from conversation".into() } else { "Conversation fork".into() },
                    None,
                    Some(Icon::GitFork),
                    DividerTone::Neutral,
                    cx,
                ))
                .into_any_element()
        }
        TurnItemKind::ThreadCreated { target_thread_id, .. } => {
            let title = item.title.clone().unwrap_or_else(|| "Created thread".into());
            let target = target_thread_id.clone();
            let open = focusable(div().id(SharedString::from(format!("open-thread:{id}"))), cx)
                .flex_none()
                .text_size(rem(14.))
                .text_color(hsla(p.primary))
                .cursor_pointer()
                .hover(|style| style.underline())
                .child("Open chat")
                .on_click(cx.listener(move |view, _, _, cx| view.open_thread(&target, cx)));
            if resource_summary {
                h_flex()
                    .w_full()
                    .gap(px(12.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .child(div().size(px(36.)).flex_none().rounded(px(8.)).bg(hsla(p.muted).opacity(0.6)).flex().items_center().justify_center().child(Icon::MessageSquare.render(px(16.), hsla(p.secondary_label))))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child(title))
                    .child(open)
                    .into_any_element()
            } else {
                work_row_frame(SharedString::from(format!("created:{id}")), false)
                    .child(work_line(
                        Some(icon_slot(Icon::WorkMessageSquare, hsla(p.icon_muted))),
                        div().child(match &item.title {
                            Some(title) => format!("Created thread · {title}"),
                            None => "Created thread".into(),
                        }).into_any_element(),
                        vec![open.into_any_element()],
                        false,
                        cx,
                    ))
                    .into_any_element()
            }
        }
        _ => div().into_any_element(),
    }
}
