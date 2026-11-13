//! Turn folds, the working row, compaction dividers and lifecycle events:
//! `MessagesTimeline.tsx` (`TurnFoldTimelineRow`, `AttemptFoldTimelineRow`,
//! `ContextCompactionTimelineRow`), `TimelineWorkGroupRows.tsx` (`WorkingTimelineRow`),
//! `TimelineV2EventRow.tsx`, `V2LifecycleRow.tsx` and the request cards
//! (`SecretRequestCard.tsx`, `LinkRequestCard.tsx`) in their answered and waiting states.

use std::sync::Arc;

use gpui::{AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use t3_core::providers::entries as provider_entries;
use t3_core::timeline::{
    CompactionTokens, ExistingPeerLink, HandoffEndpoint, LINK_ACCESS_LEVELS, LinkTone,
    MachineCandidate, SecretTone, can_manage_peer_links, format_duration, link_access_description,
    link_access_label, link_request_default_access, link_request_display, linkable_machines,
    mcp_client_scopes, parse_iso_ms, rank_machines_by_hint, resolve_handoff_endpoints,
    sanitize_link_request_error, sanitize_secret_request_error, secret_request_display,
    secret_request_privacy_note,
};
use t3_core::turn_item::{TurnItem, TurnItemKind};

use super::parts::{
    ROW_GROUP, DividerTone, focusable, ghost_button, icon_slot, system_divider,
    system_divider_button, timestamp, work_line, work_row_frame,
};
use super::timer::{TimerKind, WorkingTimer};
use super::{FoldWorkState, RowEnv};
use crate::app_state::AppState;
use crate::composer::controls::{Button, ButtonSize, ButtonVariant};
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;

fn fold_label_text(label: &str, work: FoldWorkState) -> String {
    if work.error {
        format!("{label} · Couldn't load work. Click to retry.")
    } else {
        label.to_owned()
    }
}

fn work_history_skeleton(cx: &App) -> AnyElement {
    let p = palette(cx);
    let fill = hsla(p.muted_foreground).opacity(0.15);
    v_flex()
        .gap(px(8.))
        .px(px(4.))
        .pt(px(12.))
        .pb(px(4.))
        .children([(2. / 5.), (3. / 5.), (1. / 3.)].into_iter().map(|fraction| {
            h_flex()
                .w_full()
                .items_center()
                .gap(px(8.))
                .child(div().size(px(12.)).flex_none().rounded(px(4.)).bg(fill))
                .child(div().h(px(10.)).w(gpui::relative(fraction)).rounded_full().bg(fill))
        }))
        .into_any_element()
}

pub fn turn_fold(id: &str, run_id: &str, label: &str, expanded: bool, created_at: Option<&str>, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let work = if expanded { env.ui.work_fold_for_run(run_id) } else { FoldWorkState::default() };
    let run_id = run_id.to_owned();
    v_flex()
        .id(SharedString::from(format!("fold:{id}")))
        .group(ROW_GROUP)
        .w_full()
        .pt(px(4.))
        .pb(px(8.))
        .pr(px(2.))
        .border_b_1()
        .border_color(hsla(p.border).opacity(0.6))
        .child(
            h_flex()
                .w_full()
                .gap(px(4.))
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
                        .child(fold_label_text(label, work))
                        .child((if expanded { Icon::ChevronDown } else { Icon::ChevronRight }).render(px(14.), hsla(p.muted_foreground))),
                    cx,
                ))
                .child(div().flex_1())
                .children(created_at.map(|at| timestamp(&env.clock, at, false, cx))),
        )
        .when(work.loading, |this| this.child(work_history_skeleton(cx)))
        .into_any_element()
}

pub fn attempt_fold(id: &str, attempt_id: &str, label: &str, expanded: bool, work: FoldWorkState, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let work = if expanded { work } else { FoldWorkState::default() };
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
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.8)).child(fold_label_text(label, work)))
        .child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child("Cut off by a steer"));
    v_flex()
        .w_full()
        .child(focusable(fold, cx))
        .when(work.loading, |this| this.child(work_history_skeleton(cx)))
        .into_any_element()
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

pub(super) fn plain_detail(text: &str) -> String {
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

#[derive(IntoElement)]
pub(super) struct SubagentLiveElapsed {
    pub(super) item_id: SharedString,
    pub(super) started_at: String,
}

impl RenderOnce for SubagentLiveElapsed {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let timer = window
            .use_keyed_state(SharedString::from(format!("subagent-timer:{}", self.item_id)), cx, |_, cx| {
                cx.new(|cx| WorkingTimer::new(self.started_at, TimerKind::Elapsed, cx))
            })
            .read(cx)
            .clone();
        div().flex_none().text_xs().text_color(hsla(palette(cx).muted_foreground)).child(timer)
    }
}

pub(super) fn provider_marks(app: &AppState) -> Vec<(String, ProviderMark)> {
    app.server_config
        .config
        .as_ref()
        .map(|config| {
            let entries = provider_entries(config, None);
            entries
                .iter()
                .map(|entry| (entry.instance_id().to_owned(), ProviderMark::of(entry, &entries)))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn subagent_avatar(provider_instance_id: Option<&str>, marks: &[(String, ProviderMark)], size: f32, cx: &App) -> AnyElement {
    let p = palette(cx);
    if let Some(id) = provider_instance_id
        && let Some((_, mark)) = marks.iter().find(|(instance_id, _)| instance_id == id)
    {
        return ProviderIcon::new(mark.clone()).size(px(size)).into_any_element();
    }
    Icon::WorkBot.render(px(size - 2.), hsla(p.secondary_label)).into_any_element()
}

/// `SubagentTimelineLink`: avatar, title, latest progress or result, status and elapsed time.
fn subagent_row(item: &Arc<TurnItem>, marks: &[(String, ProviderMark)], env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let TurnItemKind::Subagent { child_thread_id, provider_instance_id, remote_child, progress, result, .. } = &item.kind else {
        return div().into_any_element();
    };
    let settled = matches!(item.status.as_str(), "completed" | "failed" | "cancelled" | "interrupted");
    let raw = if settled { result.as_deref().or(progress.as_deref()) } else { progress.as_deref().or(result.as_deref()) };
    let detail = raw
        .map(str::trim)
        .filter(|d| !d.is_empty() && !d.starts_with("Child task ended with status"))
        .map(plain_detail)
        .filter(|d| !d.is_empty());
    let remote_label = remote_child
        .as_ref()
        .and_then(|v| v.get("environmentLabel").or_else(|| v.get("label")).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|label| format!("on {label}"));
    let failed = item.status == "failed";
    let status = status_label(&item.status);
    let dot = match item.status.as_str() {
        "pending" | "running" | "waiting" => hsla(p.info),
        "completed" => hsla(p.success),
        "failed" => hsla(p.destructive),
        _ => hsla(p.muted_foreground).opacity(0.6),
    };
    let detail_color = hsla(if failed { p.destructive } else { p.muted_foreground });
    let elapsed_view = if item.is_running()
        && let Some(started_at) = item.started_at.clone()
    {
        Some(SubagentLiveElapsed { item_id: SharedString::from(item.id.clone()), started_at }.into_any_element())
    } else {
        elapsed_label(item, env.clock.now_ms).map(|elapsed| div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(elapsed).into_any_element())
    };
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
                .child(subagent_avatar(provider_instance_id.as_deref(), marks, 16., cx))
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
                        .child(div().min_w_0().truncate().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(subagent_title(item)))
                        .when_some(remote_label, |this, remote| {
                            this.child(div().flex_none().text_size(px(10.)).text_color(hsla(p.muted_foreground)).child(remote))
                        }),
                )
                .child(div().min_w_0().truncate().text_size(px(11.)).text_color(detail_color).child(detail.clone().unwrap_or_else(|| status.to_owned()))),
        )
        .when(detail.is_some() && item.status != "completed", |this| {
            this.child(div().flex_none().text_xs().text_color(detail_color).child(status))
        })
        .children(elapsed_view);
    if let Some(child) = child_thread_id.clone() {
        row = focusable(row, cx)
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent).opacity(0.2)))
            .child(Icon::ChevronRight.render(px(14.), hsla(p.muted_foreground).opacity(0.6)))
            .on_click(cx.listener(move |view, _, _, cx| view.open_thread(&child, cx)));
    }
    row.into_any_element()
}

/// `V2SubagentGroup`: adjacent subagents of one turn behind a `"Kicked off / Ran N subagents"` disclosure.
fn subagent_group(id: &str, members: &[Arc<TurnItem>], marks: &[(String, ProviderMark)], env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let p = palette(cx);
    let expanded = env.ui.expanded_entries.contains(&format!("subagent-group:{id}"));
    let group_summary = t3_core::lineage::subagent_group_summary(members.iter().map(|m| m.status.as_str()));
    let summary = t3_core::lineage::summarize_subagent_statuses(members.iter().map(|m| m.status.as_str()));
    let active = group_summary.active;
    let failed = group_summary.failed;
    let label = group_summary.label;
    let key = format!("subagent-group:{id}");
    let ring = hsla(p.ring);
    let visible_avatars: Vec<_> = members.iter().take(3).collect();
    let overflow = members.len().saturating_sub(3);
    let avatar_stack = h_flex()
        .items_center()
        .children(visible_avatars.into_iter().enumerate().map(|(index, member)| {
            let instance_id = match &member.kind {
                TurnItemKind::Subagent { provider_instance_id, .. } => provider_instance_id.as_deref(),
                _ => None,
            };
            div()
                .when(index > 0, |this| this.ml(px(-6.)))
                .size(px(20.))
                .rounded_full()
                .border_1()
                .border_color(hsla(p.background))
                .bg(hsla(p.muted))
                .flex()
                .items_center()
                .justify_center()
                .child(subagent_avatar(instance_id, marks, 13., cx))
        }))
        .when(overflow > 0, |this| {
            this.child(
                div()
                    .ml(px(-6.))
                    .px(px(4.))
                    .h(px(20.))
                    .rounded_full()
                    .border_1()
                    .border_color(hsla(p.background))
                    .bg(hsla(p.secondary))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.secondary_foreground))
                    .child(format!("+{overflow}")),
            )
        });
    let header = h_flex()
        .id(SharedString::from(format!("subagents:{id}")))
        .tab_index(0)
        .focus_visible(move |style| style.border_2().border_color(ring))
        .w_full()
        .gap(px(10.))
        .py(px(8.))
        .pr(px(13.))
        .cursor_pointer()
        .text_color(if expanded || active { hsla(p.foreground) } else { hsla(p.muted_foreground) })
        .on_click(cx.listener(move |view, _, _, cx| view.toggle_entry(&key, cx)))
        .child(avatar_stack)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(label))
                .when(!summary.is_empty(), |col| {
                    col.child(
                        div()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(if active { hsla(p.info_foreground) } else if failed { hsla(p.destructive) } else { hsla(p.muted_foreground) })
                            .child(summary),
                    )
                }),
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
            .children(members.iter().map(|member| subagent_row(member, marks, env, cx)).collect::<Vec<_>>())
    });
    v_flex().w_full().child(header).children(list).into_any_element()
}

fn rpc_error_tag_and_message(err: &t3_client::session::RpcError) -> (Option<&str>, &str) {
    match err {
        t3_client::session::RpcError::Failure(failure) => (failure.tag.as_deref(), failure.message.as_str()),
        t3_client::session::RpcError::Closed => (None, ""),
    }
}

fn merge_link_candidates(saved: &[t3_client::store::SavedEnvironment], relay: &[t3_core::relay::Environment]) -> Vec<MachineCandidate> {
    let mut candidates: Vec<MachineCandidate> = saved
        .iter()
        .map(|env| MachineCandidate {
            id: env.environment_id.clone().unwrap_or_else(|| env.id.clone()),
            label: env.label.clone(),
            urls: vec![env.http_base.clone()],
            reachable: !env.disabled && env.credentials().is_some(),
        })
        .collect();
    for env in relay {
        if let Some(existing) = candidates.iter_mut().find(|candidate| candidate.id == env.environment_id) {
            if !env.endpoint.http_base_url.trim().is_empty() && !existing.urls.iter().any(|u| u == &env.endpoint.http_base_url) {
                existing.urls.push(env.endpoint.http_base_url.clone());
            }
        } else {
            let urls = if env.endpoint.http_base_url.trim().is_empty() {
                Vec::new()
            } else {
                vec![env.endpoint.http_base_url.clone()]
            };
            candidates.push(MachineCandidate {
                id: env.environment_id.clone(),
                label: env.label.clone(),
                urls,
                reachable: false,
            });
        }
    }
    candidates
}

fn link_card_headline(default_label: Option<&str>, default_url: Option<&str>, default_environment_id: Option<&str>) -> (String, Option<String>) {
    fn clean(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    let lbl = clean(default_label);
    let url = clean(default_url);
    let env_id = clean(default_environment_id);
    let title = match lbl.or(url).or(env_id) {
        Some(target) => format!("Link {target} to this environment"),
        None => "Link a machine to this environment".to_owned(),
    };
    let subtitle = if lbl.is_some() { url.map(str::to_owned) } else { None };
    (title, subtitle)
}

fn link_access_full_description(access: &str) -> String {
    format!(
        "{} An agent never gets more access there than its own mode here. Below full access, it can change only threads this link started, not that machine's own threads.",
        link_access_description(access)
    )
}

struct SecretCardState {
    input: Entity<InputState>,
    submitting: bool,
    error: Option<String>,
}

#[derive(IntoElement)]
struct SecretRequestCard {
    id: SharedString,
    thread_id: String,
    turn_item_id: String,
    label: String,
    reason: String,
    placeholder: Option<String>,
    delivery: Option<String>,
    state: Entity<AppState>,
}

impl RenderOnce for SecretRequestCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let placeholder = self.placeholder.clone().unwrap_or_else(|| "Paste secret".to_owned());
        let card_state = window.use_keyed_state(SharedString::from(format!("secret-ui:{}", self.id)), cx, |window, cx| SecretCardState {
            input: cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(placeholder)),
            submitting: false,
            error: None,
        });
        let (input, submitting, error) = {
            let s = card_state.read(cx);
            (s.input.clone(), s.submitting, s.error.clone())
        };
        let privacy_note = secret_request_privacy_note(self.delivery.as_deref());
        let save_state = card_state.clone();
        let save_input = input.clone();
        let save_app = self.state.clone();
        let save_thread = self.thread_id.clone();
        let save_item = self.turn_item_id.clone();

        let decline_state = card_state.clone();
        let decline_app = self.state.clone();
        let decline_thread = self.thread_id.clone();
        let decline_item = self.turn_item_id.clone();

        v_flex()
            .w_full()
            .gap(px(8.))
            .p(px(16.))
            .rounded(px(12.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.card))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(Icon::KeyRound.render(px(15.), hsla(p.muted_foreground)))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.label)),
            )
            .when(!self.reason.trim().is_empty(), |this| {
                this.child(div().text_size(px(13.)).text_color(hsla(p.muted_foreground)).child(self.reason))
            })
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(Input::new(&input).disabled(submitting)))
                    .child(
                        Button::new(SharedString::from(format!("secret-save:{}", self.id)), ButtonVariant::Outline, ButtonSize::Xs)
                            .label(if submitting { "Saving..." } else { "Save securely" })
                            .disabled(submitting)
                            .on_click(move |_, _, cx| {
                                if submitting {
                                    return;
                                }
                                let secret = save_input.read(cx).value().to_string();
                                if secret.trim().is_empty() {
                                    save_state.update(cx, |s, cx| {
                                        s.error = Some("Enter a secret value first.".to_owned());
                                        cx.notify();
                                    });
                                    return;
                                }
                                save_state.update(cx, |s, cx| {
                                    s.submitting = true;
                                    s.error = None;
                                    cx.notify();
                                });
                                let task = save_app.update(cx, |app, _| {
                                    app.rpc(
                                        t3_core::commands::method::SECRETS_ANSWER_REQUEST,
                                        t3_core::commands::secret_request_answer_save(&save_thread, &save_item, &secret),
                                    )
                                });
                                let weak = save_state.downgrade();
                                cx.spawn(async move |cx| {
                                    let res = task.await;
                                    let _ = weak.update(cx, |s, cx| {
                                        s.submitting = false;
                                        if let Err(err) = res {
                                            let (tag, msg) = rpc_error_tag_and_message(&err);
                                            s.error = Some(sanitize_secret_request_error(tag, msg));
                                        }
                                        cx.notify();
                                    });
                                })
                                .detach();
                            }),
                    )
                    .child(ghost_button(
                        SharedString::from(format!("secret-decline:{}", self.id)),
                        "Decline",
                        submitting,
                        move |_, _, cx| {
                            if submitting {
                                return;
                            }
                            decline_state.update(cx, |s, cx| {
                                s.submitting = true;
                                s.error = None;
                                cx.notify();
                            });
                            let task = decline_app.update(cx, |app, _| {
                                app.rpc(
                                    t3_core::commands::method::SECRETS_ANSWER_REQUEST,
                                    t3_core::commands::secret_request_answer_decline(&decline_thread, &decline_item),
                                )
                            });
                            let weak = decline_state.downgrade();
                            cx.spawn(async move |cx| {
                                let res = task.await;
                                let _ = weak.update(cx, |s, cx| {
                                    s.submitting = false;
                                    if let Err(err) = res {
                                        let (tag, msg) = rpc_error_tag_and_message(&err);
                                        s.error = Some(sanitize_secret_request_error(tag, msg));
                                    }
                                    cx.notify();
                                });
                            })
                            .detach();
                        },
                        cx,
                    )),
            )
            .when_some(error, |this, err| this.child(div().text_xs().text_color(hsla(p.destructive)).child(err)))
            .child(
                h_flex()
                    .gap(px(6.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(Icon::Shield.render(px(14.), hsla(p.muted_foreground)))
                    .child(privacy_note),
            )
    }
}

struct LinkCardState {
    dialog_open: bool,
    manual_mode: bool,
    default_manual_mode: bool,
    selected_machine_id: Option<String>,
    access: String,
    loading_links: bool,
    links: Vec<ExistingPeerLink>,
    submitting: bool,
    error: Option<String>,
    url_input: Entity<InputState>,
    label_input: Entity<InputState>,
    code_input: Entity<InputState>,
}

impl LinkCardState {
    fn reset(&mut self, default_access: String, default_url: &str, default_label: &str, window: &mut Window, cx: &mut App) {
        self.dialog_open = false;
        self.manual_mode = self.default_manual_mode;
        self.selected_machine_id = None;
        self.access = default_access;
        self.submitting = false;
        self.error = None;
        self.url_input.update(cx, |input, cx| input.set_value(default_url.to_owned(), window, cx));
        self.label_input.update(cx, |input, cx| input.set_value(default_label.to_owned(), window, cx));
        self.code_input.update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn seed_manual_machine(&mut self, machine_id: &str, url: Option<&str>, label: &str, window: &mut Window, cx: &mut App) {
        self.selected_machine_id = Some(machine_id.to_owned());
        if let Some(url) = url.filter(|u| !u.trim().is_empty()) {
            self.url_input.update(cx, |input, cx| input.set_value(url.to_owned(), window, cx));
        }
        if !label.trim().is_empty() {
            self.label_input.update(cx, |input, cx| input.set_value(label.to_owned(), window, cx));
        }
    }
}

#[derive(IntoElement)]
struct LinkRequestCard {
    id: SharedString,
    thread_id: String,
    turn_item_id: String,
    reason: String,
    default_url: Option<String>,
    default_environment_id: Option<String>,
    default_label: Option<String>,
    hint: Option<String>,
    requested_access: Option<String>,
    state: Entity<AppState>,
}

impl RenderOnce for LinkRequestCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let default_access = link_request_default_access(self.requested_access.as_deref()).to_owned();
        let default_url = self.default_url.clone().unwrap_or_default();
        let default_label = self.default_label.clone().unwrap_or_default();
        let initial_manual_mode = self.default_environment_id.is_none()
            && self.default_url.as_deref().is_some_and(|u| !u.trim().is_empty());
        let wh = window.window_handle();
        let init_access = default_access.clone();
        let init_url = default_url.clone();
        let init_label = default_label.clone();
        let card_state = window.use_keyed_state(SharedString::from(format!("link-ui:{}", self.id)), cx, |window, cx| LinkCardState {
            dialog_open: false,
            manual_mode: initial_manual_mode,
            default_manual_mode: initial_manual_mode,
            selected_machine_id: None,
            access: init_access,
            loading_links: false,
            links: Vec::new(),
            submitting: false,
            error: None,
            url_input: cx.new(|cx| InputState::new(window, cx).default_value(init_url).placeholder("https://machine:3773")),
            label_input: cx.new(|cx| InputState::new(window, cx).default_value(init_label).placeholder("Machine label (optional)")),
            code_input: cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste pairing code")),
        });
        let (dialog_open, manual_mode, access, loading_links, links, submitting, error, url_input, label_input, code_input) = {
            let s = card_state.read(cx);
            (
                s.dialog_open,
                s.manual_mode,
                s.access.clone(),
                s.loading_links,
                s.links.clone(),
                s.submitting,
                s.error.clone(),
                s.url_input.clone(),
                s.label_input.clone(),
                s.code_input.clone(),
            )
        };
        let app_read = self.state.read(cx);
        let current_env_id = app_read
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.clone());
        let session = app_read.client_handle().and_then(|h| h.session_state().borrow().clone());
        let can_manage = can_manage_peer_links(session.as_ref());
        let candidates = merge_link_candidates(&app_read.environments.environments, &app_read.cloud.discovery.environments);
        let saved_envs = app_read.environments.environments.clone();
        let _ = app_read;

        let machines = rank_machines_by_hint(
            linkable_machines(&candidates, &links, current_env_id.as_deref()),
            self.hint.as_deref(),
            self.default_label.as_deref(),
            self.default_environment_id.as_deref(),
        );

        let has_machines = !machines.is_empty();
        let (title, url_subtitle) = link_card_headline(self.default_label.as_deref(), self.default_url.as_deref(), self.default_environment_id.as_deref());
        let open_state = card_state.clone();
        let open_app = self.state.clone();
        let open_default_access = default_access.clone();
        let open_default_url = default_url.clone();
        let open_default_label = default_label.clone();
        let decline_state = card_state.clone();
        let decline_app = self.state.clone();
        let decline_thread = self.thread_id.clone();
        let decline_item = self.turn_item_id.clone();
        let decline_default_access = default_access.clone();
        let decline_default_url = default_url.clone();
        let decline_default_label = default_label.clone();
        let decline_wh = wh;
        let manual_toggle_state = card_state.clone();
        let manual_submit_state = card_state.clone();
        let manual_submit_app = self.state.clone();
        let manual_thread = self.thread_id.clone();
        let manual_item = self.turn_item_id.clone();
        let manual_url_input = url_input.clone();
        let manual_label_input = label_input.clone();
        let manual_code_input = code_input.clone();
        let manual_access = access.clone();
        let manual_default_access = default_access.clone();
        let manual_default_url = default_url.clone();
        let manual_default_label = default_label.clone();
        let manual_wh = wh;

        v_flex()
            .w_full()
            .gap(px(8.))
            .p(px(16.))
            .rounded(px(12.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.card))
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .items_start()
                            .gap(px(8.))
                            .child(div().pt(px(2.)).child(Icon::Link2.render(px(15.), hsla(p.muted_foreground))))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap(px(2.))
                                    .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                                    .when_some(url_subtitle, |col, subtitle| {
                                        col.child(div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(subtitle))
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(px(6.))
                            .when(can_manage, |this| {
                                this.child(
                                    Button::new(SharedString::from(format!("link-open:{}", self.id)), ButtonVariant::Outline, ButtonSize::Xs)
                                        .label(if dialog_open { "Hide options" } else { "Link machine" })
                                        .disabled(submitting)
                                        .on_click(move |_, window, cx| {
                                            let next_open = !open_state.read(cx).dialog_open;
                                            let reset_access = open_default_access.clone();
                                            let reset_url = open_default_url.clone();
                                            let reset_label = open_default_label.clone();
                                            open_state.update(cx, |s, cx| {
                                                if next_open {
                                                    s.dialog_open = true;
                                                    s.error = None;
                                                    s.loading_links = true;
                                                } else {
                                                    s.reset(reset_access, &reset_url, &reset_label, window, cx);
                                                }
                                                cx.notify();
                                            });
                                            if next_open {
                                                let task = open_app.update(cx, |app, cx| {
                                                    if app.cloud.discovery.environments.is_empty() && !app.cloud.discovery.refreshing {
                                                        app.refresh_cloud(cx);
                                                    }
                                                    app.rpc(t3_core::commands::method::PEER_LINKS_LIST, serde_json::json!({}))
                                                });
                                                let weak = open_state.downgrade();
                                                cx.spawn(async move |cx| {
                                                    let res = task.await;
                                                    let _ = weak.update(cx, |s, cx| {
                                                        s.loading_links = false;
                                                        if let Ok(val) = res
                                                            && let Some(arr) = val.get("links").and_then(Value::as_array)
                                                        {
                                                            s.links = arr
                                                                .iter()
                                                                .filter_map(|item| {
                                                                    Some(ExistingPeerLink {
                                                                        environment_id: item.get("environmentId")?.as_str()?.to_owned(),
                                                                        label: item.get("label").and_then(Value::as_str).unwrap_or("").to_owned(),
                                                                    })
                                                                })
                                                                .collect();
                                                        }
                                                        cx.notify();
                                                    });
                                                })
                                                .detach();
                                            }
                                        }),
                                )
                            })
                            .child(ghost_button(
                                SharedString::from(format!("link-decline:{}", self.id)),
                                "Decline",
                                submitting || !can_manage,
                                move |_, _, cx| {
                                    if submitting || !can_manage {
                                        return;
                                    }
                                    decline_state.update(cx, |s, cx| {
                                        s.submitting = true;
                                        s.error = None;
                                        cx.notify();
                                    });
                                    let task = decline_app.update(cx, |app, _| {
                                        app.rpc(
                                            t3_core::commands::method::PEER_LINKS_ANSWER_REQUEST,
                                            t3_core::commands::link_request_answer_decline(&decline_thread, &decline_item),
                                        )
                                    });
                                    let weak = decline_state.downgrade();
                                    let reset_access = decline_default_access.clone();
                                    let reset_url = decline_default_url.clone();
                                    let reset_label = decline_default_label.clone();
                                    cx.spawn(async move |cx| {
                                        let res = task.await;
                                        let _ = decline_wh.update(cx, |_, window, cx| {
                                            let _ = weak.update(cx, |s, cx| {
                                                s.submitting = false;
                                                match &res {
                                                    Ok(_) => s.reset(reset_access.clone(), &reset_url, &reset_label, window, cx),
                                                    Err(err) => {
                                                        let (tag, msg) = rpc_error_tag_and_message(err);
                                                        s.error = Some(sanitize_link_request_error(tag, msg));
                                                    }
                                                }
                                                cx.notify();
                                            });
                                        });
                                    })
                                    .detach();
                                },
                                cx,
                            )),
                    ),
            )
            .when(!self.reason.trim().is_empty(), |this| {
                this.child(div().text_size(px(13.)).text_color(hsla(p.muted_foreground)).child(self.reason))
            })
            .when(!can_manage, |this| {
                this.child(
                    h_flex()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(Icon::Lock.render(px(13.), hsla(p.muted_foreground)))
                        .child("You need access:write permission on this connection to link machines."),
                )
            })
            .when(dialog_open && can_manage, |this| {
                let id_str = self.id.clone();
                let access_picker = h_flex().flex_wrap().gap(px(6.)).children(
                    LINK_ACCESS_LEVELS.into_iter().map(|opt| {
                        let active = access == opt;
                        let st = card_state.clone();
                        let val = opt.to_owned();
                        focusable(
                            div()
                                .id(SharedString::from(format!("link-access:{id_str}:{opt}")))
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(if active { hsla(p.primary) } else { hsla(p.border).opacity(0.6) })
                                .bg(if active { hsla(p.primary).opacity(0.14) } else { hsla(p.secondary).opacity(0.3) })
                                .text_xs()
                                .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                                .text_color(if active { hsla(p.foreground) } else { hsla(p.muted_foreground) })
                                .when(!submitting, |d| d.cursor_pointer())
                                .child(link_access_label(opt))
                                .on_click(move |_, _, cx| {
                                    if submitting {
                                        return;
                                    }
                                    st.update(cx, |s, cx| {
                                        s.access = val.clone();
                                        cx.notify();
                                    });
                                }),
                            cx,
                        )
                    }),
                );

                this.child(
                    v_flex()
                        .gap(px(10.))
                        .mt(px(4.))
                        .p(px(12.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.secondary).opacity(0.25))
                        .child(
                            v_flex()
                                .gap(px(4.))
                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Access level"))
                                .child(access_picker)
                                .child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(link_access_full_description(&access))),
                        )
                        .when(loading_links, |panel| {
                            panel.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Loading existing links..."))
                        })
                        .when(!manual_mode && has_machines, |panel| {
                            panel.child(
                                v_flex().gap(px(6.)).children(machines.into_iter().map(|machine| {
                                    let existing = machine.existing_link_environment_id.clone();
                                    let machine_id = machine.id.clone();
                                    let machine_label = machine.label.clone();
                                    let mut effective_urls = machine.urls.clone();
                                    if self.default_environment_id.as_deref() == Some(machine_id.as_str())
                                        && let Some(url) = self
                                            .default_url
                                            .as_deref()
                                            .map(str::trim)
                                            .filter(|u| !u.is_empty() && !effective_urls.iter().any(|existing_url| existing_url == u))
                                    {
                                        effective_urls.push(url.to_owned());
                                    }
                                    let creds = saved_envs
                                        .iter()
                                        .find(|e| e.environment_id.as_deref() == Some(machine_id.as_str()) || e.id == machine_id)
                                        .and_then(|e| e.credentials());
                                    let st = card_state.clone();
                                    let app = self.state.clone();
                                    let thread_id = self.thread_id.clone();
                                    let turn_item_id = self.turn_item_id.clone();
                                    let cur_access = access.clone();
                                    let reset_access = default_access.clone();
                                    let reset_url = default_url.clone();
                                    let reset_label = default_label.clone();
                                    let button_label = if existing.is_some() {
                                        "Use existing link"
                                    } else if effective_urls.is_empty() {
                                        "Enter address"
                                    } else {
                                        "Link"
                                    };
                                    h_flex()
                                        .justify_between()
                                        .gap(px(8.))
                                        .px(px(10.))
                                        .py(px(8.))
                                        .rounded(px(8.))
                                        .border_1()
                                        .border_color(hsla(p.border).opacity(0.5))
                                        .bg(hsla(p.card).opacity(0.7))
                                        .child(
                                            v_flex()
                                                .min_w_0()
                                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(machine.label))
                                                .when_some(effective_urls.first().cloned(), |row, url| {
                                                    row.child(div().truncate().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(url))
                                                }),
                                        )
                                        .child(
                                            Button::new(SharedString::from(format!("link-machine:{id_str}:{machine_id}")), ButtonVariant::Outline, ButtonSize::Xs)
                                                .label(button_label)
                                                .disabled(submitting || loading_links)
                                                .on_click(move |_, window, cx| {
                                                    if submitting || loading_links {
                                                        return;
                                                    }
                                                    if existing.is_none() && effective_urls.is_empty() {
                                                        st.update(cx, |s, cx| {
                                                            s.manual_mode = true;
                                                            s.seed_manual_machine(&machine_id, None, &machine_label, window, cx);
                                                            s.error = Some("No direct address is saved for that machine. Enter its URL and pairing code below.".to_owned());
                                                            cx.notify();
                                                        });
                                                        return;
                                                    }
                                                    st.update(cx, |s, cx| {
                                                        s.submitting = true;
                                                        s.error = None;
                                                        cx.notify();
                                                    });
                                                    let weak = st.downgrade();
                                                    if let Some(existing_id) = existing.clone() {
                                                        let task = app.update(cx, |app, _| {
                                                            app.rpc(
                                                                t3_core::commands::method::PEER_LINKS_ANSWER_REQUEST,
                                                                t3_core::commands::link_request_answer_use_existing(&thread_id, &turn_item_id, &existing_id),
                                                            )
                                                        });
                                                        let reset_access = reset_access.clone();
                                                        let reset_url = reset_url.clone();
                                                        let reset_label = reset_label.clone();
                                                        cx.spawn(async move |cx| {
                                                            let res = task.await;
                                                            let _ = wh.update(cx, |_, window, cx| {
                                                                let _ = weak.update(cx, |s, cx| {
                                                                    s.submitting = false;
                                                                    match &res {
                                                                        Ok(_) => s.reset(reset_access.clone(), &reset_url, &reset_label, window, cx),
                                                                        Err(err) => {
                                                                            let (tag, msg) = rpc_error_tag_and_message(err);
                                                                            s.error = Some(sanitize_link_request_error(tag, msg));
                                                                        }
                                                                    }
                                                                    cx.notify();
                                                                });
                                                            });
                                                        })
                                                        .detach();
                                                    } else if let Some(creds) = creds.clone()
                                                        && !effective_urls.is_empty()
                                                    {
                                                        let body = t3_core::settings::access::pairing_token_body(
                                                            "T3 Code peer link",
                                                            &mcp_client_scopes(&cur_access),
                                                        );
                                                        let app_handle = app.clone();
                                                        let runtime = app.read(cx).runtime().clone();
                                                        let thread_id = thread_id.clone();
                                                        let turn_item_id = turn_item_id.clone();
                                                        let machine_id = machine_id.clone();
                                                        let machine_label = machine_label.clone();
                                                        let machine_urls = effective_urls.clone();
                                                        let cur_access = cur_access.clone();
                                                        let reset_access = reset_access.clone();
                                                        let reset_url = reset_url.clone();
                                                        let reset_label = reset_label.clone();
                                                        cx.spawn(async move |cx| {
                                                            let minted = runtime
                                                                .spawn(async move {
                                                                    t3_client::auth::post_access(
                                                                        &creds,
                                                                        "/api/auth/pairing-token",
                                                                        Some(body),
                                                                    )
                                                                    .await
                                                                })
                                                                .await;
                                                            let code = match minted {
                                                                Ok(Ok(val)) => val
                                                                    .get("credential")
                                                                    .or_else(|| val.get("pairingCode"))
                                                                    .or_else(|| val.get("token"))
                                                                    .and_then(Value::as_str)
                                                                    .map(str::to_owned),
                                                                Ok(Err(_)) | Err(_) => {
                                                                    let _ = wh.update(cx, |_, window, cx| {
                                                                        let _ = weak.update(cx, |s, cx| {
                                                                            s.submitting = false;
                                                                            s.manual_mode = true;
                                                                            s.seed_manual_machine(
                                                                                &machine_id,
                                                                                machine_urls.first().map(String::as_str),
                                                                                &machine_label,
                                                                                window,
                                                                                cx,
                                                                            );
                                                                            s.error = Some("Could not reach that machine automatically. Enter a pairing code manually.".to_owned());
                                                                            cx.notify();
                                                                        });
                                                                    });
                                                                    return;
                                                                }
                                                            };
                                                            let Some(code) = code.filter(|c| !c.is_empty()) else {
                                                                let _ = wh.update(cx, |_, window, cx| {
                                                                    let _ = weak.update(cx, |s, cx| {
                                                                        s.submitting = false;
                                                                        s.manual_mode = true;
                                                                        s.seed_manual_machine(
                                                                            &machine_id,
                                                                            machine_urls.first().map(String::as_str),
                                                                            &machine_label,
                                                                            window,
                                                                            cx,
                                                                        );
                                                                        s.error = Some("Could not mint a pairing code automatically; paste one below.".to_owned());
                                                                        cx.notify();
                                                                    });
                                                                });
                                                                return;
                                                            };
                                                            let task = app_handle.update(cx, |app, _| {
                                                                app.rpc(
                                                                    t3_core::commands::method::PEER_LINKS_ANSWER_REQUEST,
                                                                    t3_core::commands::link_request_answer_link(
                                                                        &thread_id,
                                                                        &turn_item_id,
                                                                        Some(&machine_id),
                                                                        Some(&machine_label),
                                                                        &machine_urls,
                                                                        &cur_access,
                                                                        &code,
                                                                    ),
                                                                )
                                                            });
                                                            let res = task.await;
                                                            let _ = wh.update(cx, |_, window, cx| {
                                                                let _ = weak.update(cx, |s, cx| {
                                                                    s.submitting = false;
                                                                    match &res {
                                                                        Ok(_) => s.reset(reset_access.clone(), &reset_url, &reset_label, window, cx),
                                                                        Err(err) => {
                                                                            let (tag, msg) = rpc_error_tag_and_message(err);
                                                                            s.error = Some(sanitize_link_request_error(tag, msg));
                                                                        }
                                                                    }
                                                                    cx.notify();
                                                                });
                                                            });
                                                        })
                                                        .detach();
                                                    } else {
                                                        let machine_id = machine_id.clone();
                                                        let machine_label = machine_label.clone();
                                                        let first_url = effective_urls.first().cloned();
                                                        let _ = wh.update(cx, |_, window, cx| {
                                                            st.update(cx, |s, cx| {
                                                                s.submitting = false;
                                                                s.manual_mode = true;
                                                                s.seed_manual_machine(&machine_id, first_url.as_deref(), &machine_label, window, cx);
                                                                s.error = Some("No active credentials for that machine; enter a pairing code manually.".to_owned());
                                                                cx.notify();
                                                            });
                                                        });
                                                    }
                                                }),
                                        )
                                }))
                            )
                        })
                        .when(!manual_mode && !has_machines && !loading_links, |panel| {
                            panel.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("No other saved machines found. Enter connection details manually."))
                        })
                        .when(manual_mode || !has_machines, |panel| {
                            panel.child(
                                v_flex()
                                    .gap(px(6.))
                                    .child(Input::new(&url_input).disabled(submitting))
                                    .child(Input::new(&label_input).disabled(submitting))
                                    .child(Input::new(&code_input).disabled(submitting))
                                    .child(
                                        h_flex().justify_end().child(
                                            Button::new(SharedString::from(format!("link-manual-submit:{id_str}")), ButtonVariant::Outline, ButtonSize::Xs)
                                                .label(if submitting { "Linking..." } else { "Link machine" })
                                                .disabled(submitting)
                                                .on_click(move |_, _, cx| {
                                                    if submitting {
                                                        return;
                                                    }
                                                    let url = manual_url_input.read(cx).value().trim().to_owned();
                                                    let lbl = manual_label_input.read(cx).value().trim().to_owned();
                                                    let code = manual_code_input.read(cx).value().trim().to_owned();
                                                    if url.is_empty() || code.is_empty() {
                                                        manual_submit_state.update(cx, |s, cx| {
                                                            s.error = Some("URL and pairing code are required.".to_owned());
                                                            cx.notify();
                                                        });
                                                        return;
                                                    }
                                                    manual_submit_state.update(cx, |s, cx| {
                                                        s.submitting = true;
                                                        s.error = None;
                                                        cx.notify();
                                                    });
                                                    let lbl_opt = (!lbl.is_empty()).then_some(lbl.as_str());
                                                    let selected_env_id = manual_submit_state.read(cx).selected_machine_id.clone();
                                                    let task = manual_submit_app.update(cx, |app, _| {
                                                        app.rpc(
                                                            t3_core::commands::method::PEER_LINKS_ANSWER_REQUEST,
                                                            t3_core::commands::link_request_answer_link(
                                                                &manual_thread,
                                                                &manual_item,
                                                                selected_env_id.as_deref(),
                                                                lbl_opt,
                                                                &[url],
                                                                &manual_access,
                                                                &code,
                                                            ),
                                                        )
                                                    });
                                                    let weak = manual_submit_state.downgrade();
                                                    let reset_access = manual_default_access.clone();
                                                    let reset_url = manual_default_url.clone();
                                                    let reset_label = manual_default_label.clone();
                                                    cx.spawn(async move |cx| {
                                                        let res = task.await;
                                                        let _ = manual_wh.update(cx, |_, window, cx| {
                                                            let _ = weak.update(cx, |s, cx| {
                                                                s.submitting = false;
                                                                match &res {
                                                                    Ok(_) => s.reset(reset_access.clone(), &reset_url, &reset_label, window, cx),
                                                                    Err(err) => {
                                                                        let (tag, msg) = rpc_error_tag_and_message(err);
                                                                        s.error = Some(sanitize_link_request_error(tag, msg));
                                                                    }
                                                                }
                                                                cx.notify();
                                                            });
                                                        });
                                                    })
                                                    .detach();
                                                }),
                                        ),
                                    ),
                            )
                        })
                        .when(has_machines, |panel| {
                            panel.child(h_flex().justify_start().child(ghost_button(
                                SharedString::from(format!("link-toggle-manual:{id_str}")),
                                if manual_mode { "Choose saved machine" } else { "Connect manually" },
                                submitting,
                                move |_, _, cx| {
                                    manual_toggle_state.update(cx, |s, cx| {
                                        s.manual_mode = !s.manual_mode;
                                        s.selected_machine_id = None;
                                        s.error = None;
                                        cx.notify();
                                    });
                                },
                                cx,
                            )))
                        })
                        .when_some(error, |panel, err| panel.child(div().text_xs().text_color(hsla(p.destructive)).child(err))),
                )
            })
    }
}

#[derive(IntoElement)]
struct HandoffCard {
    id: SharedString,
    from: Vec<HandoffEndpoint>,
    to: HandoffEndpoint,
    pending: bool,
    failed: bool,
    summary: Option<String>,
}

impl RenderOnce for HandoffCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let expanded_state = window.use_keyed_state(SharedString::from(format!("handoff-open:{}", self.id)), cx, |_, _| false);
        let expanded = *expanded_state.read(cx);
        let color = hsla(if self.failed { p.destructive } else { p.muted_foreground });
        let line = || div().h(px(1.)).flex_1().bg(if self.failed { hsla(p.destructive).opacity(0.3) } else { hsla(p.border).opacity(0.7) });
        let render_endpoint = |ep: HandoffEndpoint| {
            let label = ep.label.clone();
            h_flex()
                .gap(px(4.))
                .when_some(ep.driver, |row, driver| {
                    let accent = ep.is_custom.then_some(p.primary);
                    row.child(ProviderIcon::new(ProviderMark::configured(&driver, &label, accent)).size(px(12.)))
                })
                .child(ep.label)
        };
        let mut from_views: Vec<AnyElement> = Vec::new();
        let from_count = self.from.len();
        for (idx, ep) in self.from.into_iter().enumerate() {
            from_views.push(render_endpoint(ep).into_any_element());
            if idx + 1 < from_count {
                from_views.push(div().child(", ").into_any_element());
            }
        }
        let has_endpoints = from_count > 0 || !self.to.label.is_empty();
        let title = if self.pending { "Context handoff pending" } else { "Context handoff" };
        let toggle = self.summary.as_ref().map(|_| {
            let st = expanded_state.clone();
            focusable(
                div()
                    .id(SharedString::from(format!("handoff-toggle:{}", self.id)))
                    .px(px(4.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(p.accent).opacity(0.5)))
                    .child((if expanded { Icon::ChevronDown } else { Icon::ChevronRight }).render(px(12.), color))
                    .on_click(move |_, _, cx| {
                        st.update(cx, |open, cx| {
                            *open = !*open;
                            cx.notify();
                        });
                    }),
                cx,
            )
        });

        v_flex()
            .w_full()
            .gap(px(4.))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(px(8.))
                    .py(px(8.))
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(color)
                    .child(line())
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(px(6.))
                            .px(px(8.))
                            .py(px(4.))
                            .child(Icon::ArrowRightLeft.render(px(12.), color))
                            .child(div().font_weight(FontWeight::MEDIUM).child(title))
                            .when(has_endpoints, |row| {
                                row.child(
                                    h_flex()
                                        .min_w_0()
                                        .gap(px(4.))
                                        .opacity(0.8)
                                        .children(from_views)
                                        .when(from_count > 0 && !self.to.label.is_empty(), |r| r.child(Icon::ArrowRight.render(px(11.), color)))
                                        .when(!self.to.label.is_empty(), |r| r.child(render_endpoint(self.to))),
                                )
                            })
                            .children(toggle),
                    )
                    .child(line()),
            )
            .when(expanded, |this| {
                this.when_some(self.summary, |this, summary| {
                    this.child(
                        div()
                            .mx(px(16.))
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(hsla(p.border).opacity(0.5))
                            .bg(hsla(p.card).opacity(0.5))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(summary),
                    )
                })
            })
    }
}

fn answered_line(id: &str, icon: Icon, text: String, cx: &App) -> AnyElement {
    work_row_frame(SharedString::from(format!("answered:{id}")), false)
        .child(work_line(Some(icon_slot(icon, hsla(palette(cx).icon_muted))), div().child(text).into_any_element(), Vec::new(), false, cx))
        .into_any_element()
}

/// `V2EventTimelineRow`.
pub fn event_row(
    id: &str,
    item: &Arc<TurnItem>,
    visibility: t3_core::thread::Visibility,
    subagents: &[Arc<TurnItem>],
    resource_summary: bool,
    env: &RowEnv,
    cx: &mut Context<ThreadView>,
) -> AnyElement {
    let p = palette(cx);
    let can_answer_here = visibility == t3_core::thread::Visibility::Local && env.thread_id.is_some();
    let marks = provider_marks(env.state.read(cx));
    match &item.kind {
        TurnItemKind::Subagent { .. } if subagents.len() > 1 => subagent_group(id, subagents, &marks, env, cx),
        TurnItemKind::Subagent { .. } => subagent_row(item, &marks, env, cx),
        TurnItemKind::SecretRequest { label, reason, placeholder, delivery, secret_status } => {
            if secret_status == "pending"
                && can_answer_here
                && let Some(thread_id) = env.thread_id
            {
                return SecretRequestCard {
                    id: SharedString::from(id.to_owned()),
                    thread_id: thread_id.to_owned(),
                    turn_item_id: item.id.clone(),
                    label: label.clone(),
                    reason: reason.clone(),
                    placeholder: placeholder.clone(),
                    delivery: delivery.clone(),
                    state: env.state.clone(),
                }
                .into_any_element();
            }
            let display = secret_request_display(secret_status, label, can_answer_here);
            let icon = match display.tone {
                SecretTone::Saved => Icon::Check,
                SecretTone::Declined | SecretTone::TimedOut => Icon::X,
                SecretTone::Pending | SecretTone::PendingElsewhere => Icon::KeyRound,
            };
            let summary = if display.detail.is_empty() { display.label } else { format!("{} · {}", display.label, display.detail) };
            answered_line(id, icon, summary, cx)
        }
        TurnItemKind::LinkRequest {
            reason,
            link_status,
            url,
            environment_id,
            label,
            hint,
            requested_access,
            failure,
            linked_label,
            linked_access,
            ..
        } => {
            if link_status == "pending"
                && can_answer_here
                && let Some(thread_id) = env.thread_id
            {
                return LinkRequestCard {
                    id: SharedString::from(id.to_owned()),
                    thread_id: thread_id.to_owned(),
                    turn_item_id: item.id.clone(),
                    reason: reason.clone(),
                    default_url: url.clone(),
                    default_environment_id: environment_id.clone(),
                    default_label: label.clone(),
                    hint: hint.clone(),
                    requested_access: requested_access.clone(),
                    state: env.state.clone(),
                }
                .into_any_element();
            }
            let display = link_request_display(
                link_status,
                label.as_deref(),
                url.as_deref(),
                linked_label.as_deref(),
                linked_access.as_deref(),
                failure.as_deref(),
                can_answer_here,
            );
            let icon = match display.tone {
                LinkTone::Linked => Icon::Link,
                LinkTone::Failed => Icon::CircleAlert,
                LinkTone::Declined | LinkTone::TimedOut => Icon::X,
                LinkTone::Pending | LinkTone::PendingElsewhere => Icon::Link2,
            };
            let summary = if display.detail.is_empty() { display.label } else { format!("{} · {}", display.label, display.detail) };
            answered_line(id, icon, summary, cx)
        }
        TurnItemKind::RunInterruptResult { message } => {
            system_divider("Run interrupted".into(), Some(message.clone().into()).filter(|m: &SharedString| !m.is_empty()), Some(Icon::CircleStop), DividerTone::Neutral, cx)
        }
        TurnItemKind::Handoff {
            from_provider_instance_ids,
            to_provider_instance_id,
            from_model_selections,
            to_model,
            summary,
            ..
        } => {
            let app_read = env.state.read(cx);
            let providers = app_read
                .server_config
                .config
                .as_ref()
                .map(|config| provider_entries(config, None))
                .unwrap_or_default();
            let runs = env
                .thread_id
                .and_then(|tid| app_read.thread_state(tid).projection.as_ref())
                .map(|projection| projection.runs.as_slice())
                .unwrap_or(&[]);
            let (from, to) = resolve_handoff_endpoints(
                item.run_id.as_deref(),
                from_model_selections,
                from_provider_instance_ids,
                to_provider_instance_id.as_deref(),
                to_model.as_deref(),
                runs,
                &providers,
            );
            HandoffCard {
                id: SharedString::from(id.to_owned()),
                from,
                to,
                pending: matches!(item.status.as_str(), "pending" | "waiting" | "running"),
                failed: item.status == "failed",
                summary: summary.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_owned),
            }
            .into_any_element()
        }
        TurnItemKind::Fork { target_thread_id, source, .. } => {
            let from_run = source.as_ref().and_then(|s| s.get("type")).and_then(|t| t.as_str()) == Some("run");
            let target = if from_run {
                source.as_ref().and_then(|s| s.get("threadId")).and_then(|t| t.as_str()).map(str::to_owned).unwrap_or_else(|| target_thread_id.clone())
            } else {
                target_thread_id.clone()
            };
            system_divider_button(
                SharedString::from(format!("fork:{id}")),
                if from_run { "Forked from conversation".into() } else { "Conversation fork".into() },
                Some(Icon::GitFork),
                if from_run { "Open source conversation" } else { "Open fork" },
                cx.listener(move |view, _, _, cx| view.open_thread(&target, cx)),
                cx,
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn subagent_item(status: &str, started_at: &str, completed_at: Option<&str>) -> TurnItem {
        serde_json::from_value(serde_json::json!({
            "id": "item-1",
            "type": "subagent",
            "subagentId": "subagent-1",
            "status": status,
            "title": "  Worker  task\n  here ",
            "startedAt": started_at,
            "completedAt": completed_at,
            "childThreadId": "thread-child",
        }))
        .expect("valid subagent item")
    }

    #[test]
    fn subagent_helpers_normalize_title_detail_and_elapsed() {
        let running = subagent_item("running", "2026-01-01T00:00:00Z", None);
        assert_eq!(subagent_title(&running), "Worker task here");
        let start_ms = parse_iso_ms("2026-01-01T00:00:00Z").expect("valid iso");
        assert_eq!(elapsed_label(&running, start_ms + 300).as_deref(), Some("1s"));

        let settled = subagent_item("completed", "2026-01-01T00:00:00Z", Some("2026-01-01T00:01:05Z"));
        assert_eq!(elapsed_label(&settled, start_ms + 300).as_deref(), Some("1m 5s"));
        assert_eq!(plain_detail("- Running `cargo test`\n* Done"), "Running cargo test Done");
    }

    #[test]
    fn link_card_headline_and_candidate_merge_cover_url_label_and_relay() {
        assert_eq!(
            link_card_headline(None, None, None),
            ("Link a machine to this environment".to_owned(), None)
        );
        assert_eq!(
            link_card_headline(Some("Studio Mac"), Some("https://studio.ts.net"), None),
            ("Link Studio Mac to this environment".to_owned(), Some("https://studio.ts.net".to_owned()))
        );
        assert_eq!(
            link_card_headline(None, Some("https://studio.ts.net"), None),
            ("Link https://studio.ts.net to this environment".to_owned(), None)
        );

        let relay = vec![
            t3_core::relay::Environment {
                environment_id: "env-relay-1".to_owned(),
                label: "Relay Box".to_owned(),
                endpoint: t3_core::relay::Endpoint {
                    http_base_url: "https://relay.example/env-1".to_owned(),
                    ws_base_url: "wss://relay.example/env-1".to_owned(),
                    provider_kind: "relay".to_owned(),
                },
            },
        ];
        let merged = merge_link_candidates(&[], &relay);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "env-relay-1");
        assert_eq!(merged[0].label, "Relay Box");
        assert_eq!(merged[0].urls, vec!["https://relay.example/env-1".to_owned()]);
        assert!(!merged[0].reachable);
    }

    #[test]
    fn rpc_errors_sanitize_untagged_and_preserve_domain_tags() {
        let domain_err = t3_client::session::RpcError::Failure(t3_core::rpc::RpcFailure {
            tag: Some("PeerPairingCodeError".to_owned()),
            message: "Pairing code expired.".to_owned(),
            error: Value::Null,
        });
        let (tag, msg) = rpc_error_tag_and_message(&domain_err);
        assert_eq!(sanitize_link_request_error(tag, msg), "Pairing code expired.");

        let raw_err = t3_client::session::RpcError::Failure(t3_core::rpc::RpcFailure {
            tag: Some("ParseError".to_owned()),
            message: "secret_value=hunter2 leaked".to_owned(),
            error: Value::Null,
        });
        let (tag, msg) = rpc_error_tag_and_message(&raw_err);
        assert_eq!(sanitize_secret_request_error(tag, msg), t3_core::timeline::RPC_REQUEST_FALLBACK_ERROR);
    }

    #[test]
    fn subagent_group_summary_matches_client_runtime_wording() {
        let active = t3_core::lineage::subagent_group_summary(["running", "completed", "cancelled"]);
        assert_eq!(active.label, "Kicked off 3 subagents");
        assert!(active.active);
        assert!(!active.failed);
        assert_eq!(
            t3_core::lineage::summarize_subagent_statuses(["running", "completed", "cancelled"]),
            "1 working · 1 done · 1 stopped"
        );

        let done = t3_core::lineage::subagent_group_summary(["completed", "failed", "idle"]);
        assert_eq!(done.label, "Ran 3 subagents");
        assert!(!done.active);
        assert!(done.failed);
        assert_eq!(
            t3_core::lineage::summarize_subagent_statuses(["completed", "failed", "idle"]),
            "1 done · 1 failed · 1 idle"
        );
    }
}
