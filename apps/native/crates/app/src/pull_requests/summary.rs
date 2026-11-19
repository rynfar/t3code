//! The Summary tab (`PullRequestSummaryTab.tsx`): reviewers and labels with the label picker
//! (`PullRequestLabelPicker.tsx`), the description as markdown whose task checkboxes save back
//! to the host (`saveDescription`), media read through the pull request's own repository
//! (`sourceControlMediaRepository`), and the collapsible Checks section.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use t3_core::pull_requests::detail::{EventKind, check_status_label, describe_checks, ordered_checks};
use t3_core::pull_requests::list::label_color;
use t3_core::pull_requests::tasks::{Segment, task_segments};
use t3_core::pull_requests::wire::Detail;

use super::controls::{BadgeKind, Button, Size, Variant, avatar, badge, check_glyph};
use super::detail::DetailView;
use crate::icons::Icon;
use crate::right_panel::tooltip_view;
use crate::theme::{hsla, palette};
use crate::transcript::markdown::prose;

/// The indent of one level of list nesting in a split description.
const NEST: f32 = 20.;

pub fn render(view: &mut DetailView, _window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let Some(detail) = view.detail.clone() else { return div().into_any_element() };
    let p = palette(cx);
    let open = view.checks_open;
    if detail.capabilities.labels == Some(true) {
        view.load_label_candidates(cx);
    }
    let description = description(view, &detail, cx);
    let comments = comments_section(view, cx);
    v_flex()
        .id("pr-summary")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&view.summary_scroll)
        .child(v_flex().gap(px(8.)).px(px(16.)).pt(px(10.)).pb(px(4.)).child(reviewers(&detail, cx)).when(!detail.labels.is_empty() || detail.capabilities.labels == Some(true), |this| this.child(labels(view, &detail, cx))))
        .child(section_heading("Description", None, cx))
        .child(div().px(px(16.)).pb(px(16.)).child(description))
        .child(section_heading("Checks", Some(open), cx).id("pr-checks-toggle").cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
            this.checks_open = !this.checks_open;
            cx.notify();
        })))
        .when(open, |this| this.child(checks(&detail, cx)))
        .when(!open, |this| this.child(div().px(px(16.)).pb(px(16.)).text_xs().text_color(hsla(p.muted_foreground)).child(describe_checks(&detail.checks))))
        .child(comments)
        .into_any_element()
}

/// The description: one piece of markdown, or, where the viewer may edit it and it has tasks,
/// split so each checkbox writes its marker back.
fn description(view: &mut DetailView, detail: &Detail, cx: &mut Context<DetailView>) -> AnyElement {
    let body = view.description().unwrap_or_default();
    let segments = if view.can_edit_description(cx) { task_segments(&body) } else { None };
    let scope = view.media_scope(detail, cx);
    let Some(segments) = segments else {
        let text: Arc<str> = if body.trim().is_empty() { Arc::from("_No description provided._") } else { Arc::from(body.as_str()) };
        let state = view.markdown.state_with_media_scope(&format!("body:{}", detail.url), &text, None, Some(scope), cx);
        return prose(&state, cx).into_any_element();
    };
    let p = palette(cx);
    let mut rows: Vec<AnyElement> = Vec::with_capacity(segments.len());
    for (index, segment) in segments.into_iter().enumerate() {
        let (lead, text, depth): (Option<AnyElement>, String, usize) = match segment {
            Segment::Markdown { text, depth } => (None, text, depth),
            Segment::Task { marker, checked, text, depth } => (Some(checkbox(index, marker, checked, cx)), text, depth),
            Segment::Item { bullet, text, depth } => (Some(div().flex_none().min_w(px(16.)).text_color(hsla(p.muted_foreground)).child(bullet).into_any_element()), text, depth),
        };
        let text: Arc<str> = Arc::from(text.as_str());
        let state = view.markdown.state_with_media_scope(&format!("body:{}:{index}", detail.url), &text, None, Some(scope.clone()), cx);
        let content = div().flex_1().min_w_0().child(prose(&state, cx));
        rows.push(h_flex().items_start().gap(px(6.)).pl(px(NEST * depth as f32)).children(lead).child(content).into_any_element());
    }
    v_flex().gap(px(4.)).children(rows).into_any_element()
}

/// A task's checkbox, drawn as the markdown view draws one; read-only when its marker was not
/// found in the source.
fn checkbox(index: usize, marker: Option<usize>, checked: bool, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    div()
        .id(SharedString::from(format!("pr-task-{index}")))
        .flex_none()
        .mt(px(5.))
        .size(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .border_color(hsla(p.primary))
        .text_color(hsla(p.primary_foreground))
        .when(checked, |this| this.bg(hsla(p.primary)).child(Icon::Check.sized(px(10.))))
        .when_some(marker, |this, marker| {
            this.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_task(marker, !checked, cx);
            }))
        })
        .into_any_element()
}

fn section_heading(title: impl Into<SharedString>, open: Option<bool>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    h_flex()
        .gap(px(6.))
        .px(px(16.))
        .py(px(12.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .child(title.into())
        .children(open.map(|open| (if open { Icon::ChevronDown } else { Icon::ChevronRight }).sized(px(14.)).text_color(hsla(p.muted_foreground).opacity(0.6))))
}

/// `MetaRow`: a 96px label column, then the value.
fn meta_row(icon: Icon, label: &'static str, value: impl IntoElement, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    h_flex()
        .min_h(px(24.))
        .gap(px(8.))
        .text_xs()
        .child(h_flex().w(px(96.)).flex_none().gap(px(6.)).text_color(hsla(p.muted_foreground)).child(icon.sized(px(14.))).child(label))
        .child(div().flex_1().min_w_0().text_color(hsla(p.foreground)).child(value))
}

fn reviewers(detail: &Detail, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let value = if detail.reviewers.is_empty() {
        div().text_color(hsla(p.muted_foreground)).child("None").into_any_element()
    } else {
        h_flex()
            .flex_wrap()
            .gap(px(6.))
            .children(detail.reviewers.iter().map(|reviewer| {
                let named = match &reviewer.name {
                    Some(name) if *name != reviewer.login => format!("{name} (@{})", reviewer.login),
                    _ => reviewer.login.clone(),
                };
                h_flex()
                    .id(SharedString::from(format!("pr-reviewer-{}", reviewer.login)))
                    .gap(px(4.))
                    .child(avatar(&reviewer.login, 16., cx))
                    .child(reviewer.login.clone())
                    .tooltip(move |window, cx| tooltip_view(named.clone().into(), window, cx))
            }))
            .into_any_element()
    };
    meta_row(Icon::Users, "Reviewers", value, cx)
}

fn labels(view: &DetailView, detail: &Detail, cx: &mut Context<DetailView>) -> impl IntoElement {
    let p = palette(cx);
    let applied = h_flex()
        .flex_wrap()
        .gap(px(4.))
        .when(detail.labels.is_empty(), |this| this.child(div().text_color(hsla(p.muted_foreground)).child("None")))
        .children(detail.labels.iter().map(|label| badge(label.name.clone(), label_color(label.color.as_deref()).map_or(BadgeKind::Secondary, BadgeKind::Label), true, cx)))
        .when(detail.capabilities.labels == Some(true), |this| this.child(label_picker(view, detail, cx)));
    meta_row(Icon::Tag, "Labels", applied, cx)
}

/// `PullRequestLabelPicker`: the repository's labels, each toggled on or off in one write.
fn label_picker(view: &DetailView, detail: &Detail, cx: &mut Context<DetailView>) -> AnyElement {
    let allowed = view.can_write_source_control(cx) && detail.viewer_permissions.labels != Some(false);
    let button = Button::new("pr-labels-edit", Variant::GhostMuted, Size::Xs).icon(Icon::Tag);
    if !allowed {
        return button.disabled(true).tooltip("Changing labels needs triage access on this repository").into_any_element();
    }
    let candidates = view.label_candidates.clone();
    let pending = view.label_pending;
    let entity = cx.entity().downgrade();
    button
        .tooltip("Change labels")
        .dropdown_menu(move |menu, _, _| {
            let note = |menu: gpui_component::menu::PopupMenu, text: &'static str| menu.item(PopupMenuItem::new(text).disabled(true));
            match &candidates {
                None => note(menu, "Loading labels…"),
                Some(Err(_)) => note(menu, "The labels could not be read."),
                Some(Ok(list)) if list.candidates.is_empty() => note(menu, "This repository has no labels."),
                Some(Ok(list)) => {
                    let mut menu = menu;
                    for candidate in &list.candidates {
                        let label = match candidate.description.as_deref().filter(|text| !text.is_empty()) {
                            Some(text) => format!("{} · {text}", candidate.name),
                            None => candidate.name.clone(),
                        };
                        let (name, applied, entity) = (candidate.name.clone(), candidate.is_applied, entity.clone());
                        menu = menu.item(PopupMenuItem::new(label).checked(applied).disabled(pending).on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |view, cx| view.toggle_label(name.clone(), applied, cx));
                        }));
                    }
                    if list.truncated {
                        menu = note(menu.separator(), "This repository has more labels than are listed here. Apply the rest on the host.");
                    }
                    menu
                }
            }
        })
        .into_any_element()
}

fn checks(detail: &Detail, cx: &mut Context<DetailView>) -> impl IntoElement {
    let p = palette(cx);
    if detail.checks.is_empty() {
        return v_flex()
            .px(px(16.))
            .pb(px(16.))
            .gap(px(8.))
            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("No checks reported."))
            .child(
                div().child(Button::new("pr-checks-refresh", Variant::Ghost, Size::Xs).label("Refresh").on_click(cx.listener(|this, _, _, cx| this.refresh_checks(cx)))),
            );
    }
    v_flex().px(px(12.)).pb(px(16.)).children(ordered_checks(&detail.checks).into_iter().enumerate().map(|(index, check)| {
        let (icon, color) = check_glyph(check.status, cx);
        let url = check.url.clone();
        h_flex()
            .id(SharedString::from(format!("pr-check-{index}")))
            .items_start()
            .gap(px(8.))
            .px(px(8.))
            .py(px(8.))
            .rounded(px(6.))
            .text_xs()
            .line_height(px(20.))
            .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
            .when(url.is_some(), |this| this.cursor_pointer())
            .child(div().mt(px(3.)).child(icon.sized(px(14.)).text_color(color)))
            .child(div().flex_1().min_w_0().text_color(hsla(p.foreground)).child(check.name.clone()))
            .child(div().flex_none().text_color(hsla(p.muted_foreground)).child(check_status_label(check)))
            .on_click(move |_, _, cx| {
                if let Some(url) = &url {
                    cx.open_url(url);
                }
            })
    }))
}

/// Conversation comments and review/overview threads on Summary, so threads without a diff line
/// (such as GitLab overview threads) can be resolved and added to chat from Summary as well as
/// Timeline (`PullRequestSummaryTab.tsx`, `pingdotgg/t3code#30575`, `pingdotgg/t3code#30579`).
fn comments_section(view: &mut DetailView, cx: &mut Context<DetailView>) -> AnyElement {
    let p = palette(cx);
    let Some(activity) = view.activity.as_ref() else {
        let heading = section_heading("Comments", None, cx);
        let text = view.activity_error.clone().unwrap_or_else(|| "Loading activity…".to_owned());
        return v_flex()
            .child(heading)
            .child(div().px(px(16.)).pb(px(16.)).text_xs().text_color(hsla(p.muted_foreground)).child(text))
            .into_any_element();
    };
    let count = activity.comment_count.max(activity.comments.len() as u64);
    let truncated = activity.comments_truncated.then_some(activity.comments.len());
    let items: Vec<_> = view.timeline.iter().filter(|event| matches!(event.kind, EventKind::Comment | EventKind::Review)).cloned().collect();
    let heading = section_heading(format!("Comments ({count})"), None, cx);
    let body = if items.is_empty() {
        div().px(px(16.)).pb(px(16.)).text_xs().text_color(hsla(p.muted_foreground)).child("No comments yet.").into_any_element()
    } else {
        let cards: Vec<AnyElement> = items.iter().map(|event| super::timeline::event_card(view, event, false, cx)).collect();
        v_flex().px(px(16.)).pb(px(16.)).gap(px(12.)).children(cards).into_any_element()
    };
    v_flex()
        .child(heading)
        .children(truncated.map(|loaded_count| {
            let loaded = if loaded_count == 1 { "1 comment is".to_owned() } else { format!("{loaded_count} comments are") };
            div()
                .mx(px(16.))
                .mb(px(8.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .border_1()
                .border_color(hsla(p.warning).opacity(0.3))
                .bg(hsla(p.warning_surface))
                .text_xs()
                .child(format!("This conversation is only partially loaded. {loaded} loaded; open it on the host to read the rest."))
        }))
        .child(body)
        .into_any_element()
}
