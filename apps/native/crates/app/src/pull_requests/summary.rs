//! The Summary tab (`PullRequestSummaryTab.tsx`): reviewers and labels, the description as
//! markdown, and the collapsible Checks section with each check's status.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::pull_requests::detail::{check_status_label, describe_checks, ordered_checks};
use t3_core::pull_requests::list::label_color;
use t3_core::pull_requests::wire::Detail;

use super::controls::{BadgeKind, Button, Size, Variant, avatar, badge, check_glyph};
use super::detail::DetailView;
use crate::icons::Icon;
use crate::right_panel::tooltip_view;
use crate::theme::{hsla, palette};
use crate::transcript::markdown::prose;

pub fn render(view: &mut DetailView, _window: &mut Window, cx: &mut Context<DetailView>) -> AnyElement {
    let Some(detail) = view.detail.clone() else { return div().into_any_element() };
    let p = palette(cx);
    let body: Arc<str> = if detail.body.trim().is_empty() { Arc::from("_No description provided._") } else { Arc::from(detail.body.as_str()) };
    let description = view.markdown.state(&format!("body:{}", detail.url), &body, cx);
    let open = view.checks_open;
    v_flex()
        .id("pr-summary")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(v_flex().gap(px(8.)).px(px(16.)).pt(px(10.)).pb(px(4.)).child(reviewers(&detail, cx)).when(!detail.labels.is_empty() || detail.capabilities.labels == Some(true), |this| this.child(labels(&detail, cx))))
        .child(section_heading("Description", None, cx))
        .child(div().px(px(16.)).pb(px(16.)).child(prose(&description, cx)))
        .child(section_heading("Checks", Some(open), cx).id("pr-checks-toggle").cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
            this.checks_open = !this.checks_open;
            cx.notify();
        })))
        .when(open, |this| this.child(checks(&detail, cx)))
        .when(!open, |this| this.child(div().px(px(16.)).pb(px(16.)).text_xs().text_color(hsla(p.muted_foreground)).child(describe_checks(&detail.checks))))
        .into_any_element()
}

fn section_heading(title: &'static str, open: Option<bool>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    h_flex()
        .gap(px(6.))
        .px(px(16.))
        .py(px(12.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .child(title)
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

fn labels(detail: &Detail, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let value = if detail.labels.is_empty() {
        div().text_color(hsla(p.muted_foreground)).child("None").into_any_element()
    } else {
        h_flex()
            .flex_wrap()
            .gap(px(4.))
            .children(detail.labels.iter().map(|label| badge(label.name.clone(), label_color(label.color.as_deref()).map_or(BadgeKind::Secondary, BadgeKind::Label), true, cx)))
            .into_any_element()
    };
    meta_row(Icon::Tag, "Labels", value, cx)
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
