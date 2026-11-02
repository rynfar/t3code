//! A thread's linked pull requests, as a right panel surface (`ThreadPullRequestsPanel.tsx`):
//! each link's state, checks and source, open in the pull requests page, unlink, and the footer
//! counts. Stacks show flat, newest first.

use gpui::{App, Context, Entity, FontWeight, Global, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px, uniform_list};
use gpui_component::{h_flex, v_flex};
use t3_core::commands::method::DISPATCH_COMMAND;
use t3_core::pull_requests::PrRef;
use t3_core::pull_requests::detail::{StateKind, checks_state_label};
use t3_core::pull_requests::link::{ThreadLink, unlink_command, visible_links};
use t3_core::pull_requests::wire::PrState;
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::controls::{Button, Size, Variant, checks_glyph, glyph, state_glyph};
use super::{LinkPullRequest, OpenPullRequests};
use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::icons::Icon;
use crate::right_panel::tooltip_view;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 44.;

/// A pull request the page should select when it next shows.
#[derive(Default)]
pub(super) struct PendingSelection(pub Option<PrRef>);

impl Global for PendingSelection {}

pub struct ThreadPullRequestsPanel {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl ThreadPullRequestsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::Selection) {
                cx.notify();
            }
        });
        ThreadPullRequestsPanel { state, _subscription: subscription }
    }

    fn links(&self, cx: &App) -> (Option<String>, Vec<ThreadLink>) {
        let state = self.state.read(cx);
        let thread = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id));
        let links = thread.map(|thread| visible_links(&thread.pull_requests).into_iter().cloned().collect()).unwrap_or_default();
        (thread.map(|thread| thread.project_id.clone()), links)
    }

    fn unlink(&mut self, link: &ThreadLink, cx: &mut Context<Self>) {
        let Some(thread_id) = self.state.read(cx).selected_thread.clone() else { return };
        let task = self.state.read(cx).rpc(DISPATCH_COMMAND, unlink_command(&uuid_v4(), &thread_id, link));
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = task.await {
                let toast = crate::toast::Toast::new(crate::toast::ToastKind::Error, "Could not unlink the pull request").description(error.to_string());
                state.update(cx, |state, cx| state.show_toast(toast, cx));
            }
        })
        .detach();
    }

    fn row(&self, link: &ThreadLink, project_id: Option<&str>, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let snapshot = link.snapshot.as_ref();
        let (icon, color, state_label) = match snapshot {
            Some(snapshot) => {
                let kind = StateKind::of(snapshot.state, snapshot.is_draft);
                let (icon, color) = state_glyph(kind, cx);
                (icon, color, kind.label())
            }
            None => (Icon::GitPullRequestArrow, hsla(p.muted_foreground), "Waiting for host state"),
        };
        let open = snapshot.is_none_or(|snapshot| snapshot.state == PrState::Open);
        let title = snapshot.map_or_else(|| link.repository.clone(), |snapshot| snapshot.title.clone());
        let linked = parse_iso(&link.linked_at).map(|at| compact_relative_label(at, now_ms())).unwrap_or_default();
        let source = format!("{} · {linked}", link.source_label());
        let reference = project_id.map(|project| PrRef { project_id: project.to_owned(), host: Some(link.host.clone()), repository: link.repository.clone(), number: link.number });
        let unlink_label = if link.source == "stack" { "Dismiss from thread" } else { "Unlink from thread" };
        let target = link.clone();
        h_flex()
            .id(SharedString::from(format!("thread-pr-{index}")))
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
            .child(glyph(SharedString::from(format!("thread-pr-state-{index}")), icon, color, 16., state_label))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .min_w_0()
                            .child(
                                div()
                                    .id(SharedString::from(format!("thread-pr-number-{index}")))
                                    .flex_none()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!("#{}", link.number))
                                    .tooltip(move |window, cx| tooltip_view(source.clone().into(), window, cx)),
                            )
                            .child(div().min_w_0().truncate().text_sm().text_color(hsla(p.foreground)).child(title))
                            .when(open, |this| {
                                this.when(link.watch.is_some(), |this| this.child(glyph(SharedString::from(format!("thread-pr-watch-{index}")), Icon::Eye, hsla(p.muted_foreground), 14., "Watching")))
                                    .children(snapshot.and_then(|s| s.checks_state).map(|state| {
                                        let (icon, color) = checks_glyph(state, cx);
                                        glyph(SharedString::from(format!("thread-pr-checks-{index}")), icon, color, 14., checks_state_label(state))
                                    }))
                            }),
                    )
                    .child(div().truncate().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(format!("{} · {}", link.repository, link.source_label()))),
            )
            .child(
                Button::new(SharedString::from(format!("thread-pr-unlink-{index}")), Variant::GhostMuted, Size::IconSm)
                    .icon(Icon::Unlink2)
                    .tooltip(unlink_label)
                    .on_click(cx.listener(move |this, _, _, cx| this.unlink(&target, cx))),
            )
            .on_click(move |_, window, cx| {
                if let Some(reference) = reference.clone() {
                    cx.set_global(PendingSelection(Some(reference)));
                    window.dispatch_action(Box::new(OpenPullRequests), cx);
                }
            })
    }
}

impl Render for ThreadPullRequestsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let (_, links) = self.links(cx);
        let link_button = |id: &'static str, label: &'static str, variant: Variant, size: Size| {
            Button::new(id, variant, size).icon(Icon::Plus).label(label).on_click(|_, window, cx| window.dispatch_action(Box::new(LinkPullRequest), cx))
        };
        if links.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .p(px(24.))
                .text_center()
                .child(Icon::Link2.sized(px(24.)).text_color(hsla(p.muted_foreground).opacity(0.6)))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("No linked pull requests"))
                .child(div().max_w(px(240.)).text_xs().text_color(hsla(p.muted_foreground)).child("Pull requests the agent opens from this thread land here. Link one yourself from a URL or a number."))
                .child(link_button("thread-pr-link-empty", "Link pull request", Variant::Outline, Size::Sm))
                .into_any_element();
        }
        let open = links.iter().filter(|link| link.snapshot.as_ref().is_none_or(|s| s.state == PrState::Open)).count();
        let count = links.len();
        let entity = cx.entity();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                uniform_list("thread-pull-requests", count, move |range, _, cx| {
                    entity.update(cx, |panel, cx| {
                        let (project_id, links) = panel.links(cx);
                        range.filter_map(|index| links.get(index).map(|link| panel.row(link, project_id.as_deref(), index, cx).into_any_element())).collect()
                    })
                })
                .flex_1()
                .p(px(6.)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .justify_between()
                    .px(px(8.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .text_size(px(11.))
                    .text_color(hsla(p.muted_foreground))
                    .child(format!("{open} open · {count} linked"))
                    .child(link_button("thread-pr-link", "Link", Variant::Ghost, Size::Xs)),
            )
            .into_any_element()
    }
}
