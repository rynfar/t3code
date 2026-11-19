//! Issue detail loading ghost state (`apps/web/src/components/issue/IssueDetailGhost.tsx`) and
//! unavailable retry state (`PullRequestsUnavailableState`).

use gpui::{AnyElement, App, FontWeight, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::issues::wire::IssueListEntry;
use t3_core::pull_requests::list::label_color;
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::{state_glyph, tracker_icon};
use crate::i18n::t;
use crate::icons::Icon;
use crate::pull_requests::controls::{BadgeKind, Button, Size, Variant, avatar, badge};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

fn ghost_bar(width_px: Option<f32>, height_px: f32, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let bar = div()
        .h(px(height_px))
        .rounded(px(4.))
        .bg(hsla(p.muted_foreground).opacity(0.16));
    match width_px {
        Some(w) => bar.w(px(w)),
        None => bar.w_full(),
    }
}

/// Renders the issue detail ghost state while `issues.detail` is loading, seeding the header,
/// title, author, labels, and assignees from `seed` when opened from a list row.
pub(crate) fn render_ghost(seed: Option<&IssueListEntry>, cx: &App) -> AnyElement {
    let p = palette(cx);
    let mono = cx.theme().mono_font_family.clone();

    let header: AnyElement = if let Some(entry) = seed {
        let (icon, color) = state_glyph(&entry.state, cx);
        let t_icon = tracker_icon(&entry.tracker);
        let author_login = entry
            .author
            .as_ref()
            .map_or("ghost", |actor| actor.login.as_str())
            .to_owned();
        let opened = parse_iso(&entry.created_at)
            .map(|at| compact_relative_label(at, now_ms()))
            .unwrap_or_default();
        let closed = entry
            .closed_at
            .as_deref()
            .and_then(parse_iso)
            .map(|at| compact_relative_label(at, now_ms()));
        let open_url = entry.url.clone();

        v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .h(px(28.))
                    .gap(px(8.))
                    .items_center()
                    .child(icon.sized(px(15.)).text_color(color))
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono)
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(entry.display_key.clone()),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(t_icon.sized(px(12.)))
                            .child(div().min_w_0().truncate().child(entry.scope.clone())),
                    )
                    .child(
                        Button::new("issue-ghost-open", Variant::GhostMuted, Size::IconSm)
                            .icon(Icon::ExternalLink)
                            .tooltip(format!(
                                "{} {}",
                                t(cx, "Open on"),
                                entry.tracker.display_name()
                            ))
                            .on_click(move |_, _, cx| cx.open_url(&open_url)),
                    ),
            )
            .child(
                div()
                    .pr(px(28.))
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(px(22.))
                    .text_color(hsla(p.foreground))
                    .child(entry.title.clone()),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .flex_wrap()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(avatar(&author_login, 16., cx))
                    .child(author_login)
                    .child(format!("{} {opened}", t(cx, "opened")))
                    .children(closed.map(|when| format!("· {} {when}", t(cx, "closed")))),
            )
            .child(
                h_flex()
                    .min_h(px(24.))
                    .gap(px(6.))
                    .flex_wrap()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(div().flex_none().child(t(cx, "Labels").to_owned()))
                    .when(entry.labels.is_empty(), |this| {
                        this.child(t(cx, "None").to_owned())
                    })
                    .children(entry.labels.iter().map(|label| {
                        let kind = label_color(label.color.as_deref())
                            .map_or(BadgeKind::Secondary, BadgeKind::Label);
                        badge(label.name.clone(), kind, false, cx)
                    })),
            )
            .child(
                h_flex()
                    .min_h(px(24.))
                    .gap(px(6.))
                    .flex_wrap()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(div().flex_none().child(t(cx, "Assignees").to_owned()))
                    .when(entry.assignees.is_empty(), |this| {
                        this.child(t(cx, "None").to_owned())
                    })
                    .children(entry.assignees.iter().map(|actor| {
                        h_flex()
                            .gap(px(4.))
                            .child(avatar(&actor.login, 16., cx))
                            .child(actor.login.clone())
                    })),
            )
            .into_any_element()
    } else {
        v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .h(px(28.))
                    .gap(px(8.))
                    .items_center()
                    .child(
                        div()
                            .size(px(16.))
                            .rounded_full()
                            .bg(hsla(p.muted_foreground).opacity(0.16)),
                    )
                    .child(ghost_bar(Some(48.), 12., cx))
                    .child(ghost_bar(Some(128.), 12., cx)),
            )
            .child(ghost_bar(Some(280.), 20., cx))
            .child(ghost_bar(Some(192.), 12., cx))
            .child(ghost_bar(Some(128.), 12., cx))
            .into_any_element()
    };

    v_flex()
        .id("issue-detail-ghost")
        .size_full()
        .min_h_0()
        .overflow_hidden()
        .px(px(16.))
        .pt(px(12.))
        .pb(px(16.))
        .gap(px(16.))
        .child(header)
        .child(
            v_flex()
                .gap(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.6))
                .p(px(12.))
                .child(ghost_bar(Some(80.), 12., cx))
                .child(ghost_bar(None, 14., cx))
                .child(ghost_bar(Some(260.), 14., cx))
                .child(ghost_bar(Some(200.), 14., cx))
                .child(ghost_bar(Some(230.), 14., cx)),
        )
        .child(ghost_bar(Some(64.), 12., cx))
        .children((0usize..2).map(|idx| {
            h_flex()
                .id(("issue-ghost-comment", idx))
                .items_start()
                .gap(px(8.))
                .child(
                    div()
                        .size(px(20.))
                        .flex_none()
                        .rounded_full()
                        .bg(hsla(p.muted_foreground).opacity(0.16)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.))
                        .child(ghost_bar(Some(110.), 12., cx))
                        .child(ghost_bar(None, 12., cx))
                        .child(ghost_bar(Some(210.), 12., cx)),
                )
        }))
        .into_any_element()
}

/// Renders the initial-load failure state with a Retry button (`PullRequestsUnavailableState`).
pub(crate) fn render_unavailable(
    error: &str,
    pending: bool,
    on_retry: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    v_flex()
        .id("issue-detail-unavailable")
        .size_full()
        .items_center()
        .justify_center()
        .px(px(24.))
        .gap(px(8.))
        .child(
            Icon::TriangleAlert
                .sized(px(20.))
                .text_color(hsla(p.destructive_foreground)),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .child(t(cx, "Could not load this issue").to_owned()),
        )
        .child(
            div()
                .max_w(px(360.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(error.to_owned()),
        )
        .child(
            div().pt(px(4.)).child(
                Button::new("issue-detail-retry", Variant::Outline, Size::Sm)
                    .label(t(cx, "Retry").to_owned())
                    .busy(pending)
                    .disabled(pending)
                    .on_click(on_retry),
            ),
        )
        .into_any_element()
}
