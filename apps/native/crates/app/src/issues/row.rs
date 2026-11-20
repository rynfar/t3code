//! Issue list rows (`IssueRow.tsx`), tracker section headers, per-tracker Load more footers,
//! and loading/empty/unavailable states.

use std::collections::HashSet;

use gpui::{
    AnyElement, App, ClickEvent, Context, FontWeight, IntoElement, ParentElement, SharedString,
    Styled, Window, div, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::issues::list::{EnvironmentIssueEntry, issue_entry_key};
use t3_core::issues::wire::IssueRef;
use t3_core::pull_requests::list::label_color;
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::page::{IssuesPage, ROW_HEIGHT, Row};
use super::{state_glyph, tracker_icon};
use crate::i18n::t;
use crate::icons::Icon;
use crate::pull_requests::controls::{BadgeKind, Button, Size, Variant, avatar, badge};
use crate::right_panel::{focusable, tooltip_view};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

pub fn render(
    page: &mut IssuesPage,
    index: usize,
    window: &mut Window,
    cx: &mut Context<IssuesPage>,
) -> AnyElement {
    match page.rows.get(index).copied() {
        Some(Row::TrackerHeader(group_idx)) => tracker_header(page, group_idx, cx),
        Some(Row::TrackerNotice(group_idx)) => tracker_notice(page, group_idx, cx),
        Some(Row::ProjectError(group_idx, err_idx)) => project_error(page, group_idx, err_idx, cx),
        Some(Row::GroupEmpty(group_idx)) => group_empty(page, group_idx, cx),
        Some(Row::Entry(group_idx, row_idx)) => entry_row(page, group_idx, row_idx, window, cx),
        Some(Row::LoadMore(group_idx, env_idx)) => load_more_row(page, group_idx, env_idx, cx),
        None => div().into_any_element(),
    }
}

fn tracker_header(page: &IssuesPage, group_idx: usize, cx: &App) -> AnyElement {
    let p = palette(cx);
    let Some(group) = page.groups.get(group_idx) else {
        return div().into_any_element();
    };
    let provider_name = group.tracker.tracker.display_name();
    let show_host = !provider_name.eq_ignore_ascii_case(&group.tracker.host);
    let count = group.tracker.project_count;
    let project_label = format!(
        "· {}",
        crate::i18n::t_fmt(
            cx,
            "{count, plural, one {{count} project} other {{count} projects}}",
            &[("count", count.into())],
        )
    );
    h_flex()
        .gap(px(6.))
        .px(px(12.))
        .pt(px(if group_idx == 0 { 4. } else { 14. }))
        .pb(px(4.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .child(tracker_icon(&group.tracker.tracker).sized(px(14.)))
        .child(provider_name.to_owned())
        .when(show_host, |this| {
            this.child(
                div()
                    .font_weight(FontWeight::NORMAL)
                    .child(format!("· {}", group.tracker.host)),
            )
        })
        .child(div().font_weight(FontWeight::NORMAL).child(project_label))
        .child(div().flex_1().min_w(px(8.)).h(px(1.)).bg(hsla(p.border)))
        .into_any_element()
}

fn tracker_notice(page: &IssuesPage, group_idx: usize, cx: &App) -> AnyElement {
    let p = palette(cx);
    let Some(group) = page.groups.get(group_idx) else {
        return div().into_any_element();
    };
    let text = group
        .tracker
        .detail
        .as_deref()
        .map(t3_core::issues::list::issue_tracker_detail_text)
        .filter(|s| !s.is_empty())
        .map(|s| t(cx, s).to_owned())
        .unwrap_or_else(|| t(cx, "This tracker cannot be read right now.").to_owned());
    div()
        .px(px(12.))
        .py(px(6.))
        .text_sm()
        .text_color(hsla(p.muted_foreground))
        .child(text)
        .into_any_element()
}

fn project_error(page: &IssuesPage, group_idx: usize, err_idx: usize, cx: &App) -> AnyElement {
    let p = palette(cx);
    let Some(error) = page
        .groups
        .get(group_idx)
        .and_then(|group| group.errors.get(err_idx))
    else {
        return div().into_any_element();
    };
    div()
        .px(px(12.))
        .py(px(4.))
        .text_xs()
        .text_color(hsla(p.destructive_foreground))
        .child(format!("{}: {}", error.project_title, error.message))
        .into_any_element()
}

fn group_empty(page: &IssuesPage, _group_idx: usize, cx: &App) -> AnyElement {
    let p = palette(cx);
    let message = if !page.sent.trim().is_empty() {
        t(cx, "Nothing matches that.")
    } else {
        t(cx, "No issues under these filters.")
    };
    div()
        .px(px(12.))
        .py(px(8.))
        .text_sm()
        .text_color(hsla(p.muted_foreground))
        .child(message.to_owned())
        .into_any_element()
}

fn entry_row(
    page: &mut IssuesPage,
    group_idx: usize,
    row_idx: usize,
    window: &mut Window,
    cx: &mut Context<IssuesPage>,
) -> AnyElement {
    let Some(group) = page.groups.get(group_idx) else {
        return div().into_any_element();
    };
    let Some(env_entry) = group.rows.get(row_idx).cloned() else {
        return div().into_any_element();
    };
    let entry = &env_entry.entry;
    let p = palette(cx);
    let reference = IssueRef::of(entry);
    let key = issue_entry_key(&entry.project_id, &entry.host, &entry.scope, &entry.key);
    let selected = page.selected.as_ref() == Some(&reference);
    let show_tracker = page.groups.len() > 1;
    let show_scope = group
        .rows
        .iter()
        .map(|row| row.entry.scope.to_lowercase())
        .collect::<HashSet<_>>()
        .len()
        > 1;
    let env_label = page.environment_label_for_row(&env_entry.environment_id, cx);

    let select_ref = reference.clone();
    let select_entry = env_entry.clone();
    let click_url = entry.url.clone();
    let select = cx.listener(move |this, event: &ClickEvent, _, cx| {
        let mods = event.modifiers();
        if mods.platform || mods.control || mods.shift || mods.alt {
            cx.open_url(&click_url);
            return;
        }
        this.preview.clear();
        this.select(select_ref.clone(), select_entry.clone(), cx);
    });

    let hover_target = super::preview::IssuePreviewTarget::from_entry(
        Some(env_entry.environment_id.clone()),
        entry,
    );
    let hover = cx.listener(move |this, hovered: &bool, _, cx| {
        let state = this.state.clone();
        this.preview.hover(
            &state,
            (*hovered).then(|| hover_target.clone()),
            |page| &mut page.preview,
            cx,
        );
    });

    let copy_text = entry.display_key.clone();
    let open_url = entry.url.clone();
    let id = SharedString::from(format!("issue-row-{group_idx}-{row_idx}-{key}"));
    let row = focusable(div().id(id.clone()), id.into(), true, window, cx)
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .cursor_pointer()
        .child(issue_state_icon(&env_entry, group_idx, row_idx, cx))
        .child(row_lines(&env_entry, show_scope, show_tracker, env_label, cx))
        .on_hover(hover)
        .on_click(select);

    h_flex()
        .h(px(ROW_HEIGHT))
        .rounded(px(6.))
        .when(selected, |this| this.bg(hsla(p.accent)))
        .when(!selected, |this| {
            this.hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
        })
        .child(row)
        .when(selected, |this| {
            this.child(
                h_flex()
                    .flex_none()
                    .gap(px(4.))
                    .pr(px(8.))
                    .child(
                        Button::new(
                            SharedString::from(format!("issue-copy-{group_idx}-{row_idx}")),
                            Variant::GhostMuted,
                            Size::IconSm,
                        )
                        .icon(Icon::Copy)
                        .tooltip(t(cx, "Copy ID").to_owned())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_text(copy_text.clone(), "Issue key", cx);
                        })),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("issue-open-{group_idx}-{row_idx}")),
                            Variant::GhostMuted,
                            Size::IconSm,
                        )
                        .icon(Icon::ExternalLink)
                        .tooltip(t(cx, "Open in browser").to_owned())
                        .on_click(move |_, _, cx| cx.open_url(&open_url)),
                    ),
            )
        })
        .into_any_element()
}

fn issue_state_icon(
    env_entry: &EnvironmentIssueEntry,
    group_idx: usize,
    row_idx: usize,
    cx: &App,
) -> impl IntoElement {
    let (icon, color) = state_glyph(&env_entry.entry.state, cx);
    let label = env_entry.entry.state.name.clone();
    div()
        .id(SharedString::from(format!("issue-glyph-{group_idx}-{row_idx}")))
        .self_start()
        .mt(px(3.))
        .flex_none()
        .child(icon.sized(px(16.)).text_color(color))
        .tooltip(move |window, cx| tooltip_view(label.clone().into(), window, cx))
}

fn row_lines(
    env_entry: &EnvironmentIssueEntry,
    show_scope: bool,
    show_tracker: bool,
    env_label: Option<String>,
    cx: &App,
) -> impl IntoElement {
    let entry = &env_entry.entry;
    let p = palette(cx);
    let mono = cx.theme().mono_font_family.clone();
    let updated = parse_iso(&entry.updated_at).map(|at| compact_relative_label(at, now_ms()));
    let author = entry
        .author
        .as_ref()
        .map_or("ghost", |actor| actor.login.as_str())
        .to_owned();
    let labels = entry.labels.iter().take(3).enumerate().map(|(index, label)| {
        let kind = label_color(label.color.as_deref()).map_or(BadgeKind::Secondary, BadgeKind::Label);
        let more = entry.labels.len().saturating_sub(index + 1);
        let text = if index == 2 && more > 0 {
            format!("{} +{more}", label.name)
        } else {
            label.name.clone()
        };
        badge(text, kind, false, cx)
    });
    let assignees = entry.assignees.iter().take(3).map(|actor| avatar(&actor.login, 16., cx));

    v_flex()
        .flex_1()
        .min_w_0()
        .child(
            h_flex()
                .min_w_0()
                .gap(px(6.))
                .child(
                    div()
                        .flex_none()
                        .font_family(mono)
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(entry.display_key.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(hsla(p.foreground))
                        .child(entry.title.clone()),
                )
                .child(div().flex_1())
                .when(!entry.assignees.is_empty(), |this| {
                    this.child(h_flex().flex_none().gap(px(2.)).children(assignees))
                })
                .when(entry.comment_count > 0, |this| {
                    this.child(
                        h_flex()
                            .flex_none()
                            .gap(px(3.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(Icon::MessageSquare.sized(px(12.)))
                            .child(entry.comment_count.to_string()),
                    )
                }),
        )
        .child(
            h_flex()
                .min_w_0()
                .gap(px(6.))
                .overflow_hidden()
                .text_size(px(11.))
                .line_height(px(16.))
                .text_color(hsla(p.muted_foreground))
                .child(
                    h_flex()
                        .flex_none()
                        .max_w(px(160.))
                        .gap(px(4.))
                        .child(avatar(&author, 14., cx))
                        .child(div().truncate().child(author)),
                )
                .when(show_tracker, |this| {
                    this.child(
                        h_flex()
                            .flex_none()
                            .gap(px(3.))
                            .child(tracker_icon(&entry.tracker).sized(px(12.))),
                    )
                })
                .when(show_scope, |this| {
                    this.child(div().min_w_0().truncate().child(entry.scope.clone()))
                })
                .when_some(env_label, |this, label| {
                    this.child(div().min_w_0().truncate().child(label))
                })
                .children(labels)
                .child(div().flex_1())
                .children(updated.map(|label| div().flex_none().whitespace_nowrap().child(label))),
        )
}

fn load_more_row(
    page: &IssuesPage,
    group_idx: usize,
    env_idx: usize,
    cx: &mut Context<IssuesPage>,
) -> AnyElement {
    let p = palette(cx);
    let Some(group) = page.groups.get(group_idx) else {
        return div().into_any_element();
    };
    let Some(env_id) = group.more.get(env_idx).cloned() else {
        return div().into_any_element();
    };
    let host = group.tracker.host.clone();
    let loading_key = format!("{env_id} {host}");
    let is_loading_this = page.loading_more.as_deref() == Some(loading_key.as_str());
    let any_loading = page.loading_more.is_some();
    let failure = page
        .held_pages
        .get(&env_id)
        .and_then(|held| held.more_failure.clone());
    let button_label = if is_loading_this {
        t(cx, "Loading…")
    } else if failure.is_some() {
        t(cx, "Retry")
    } else {
        t(cx, "Load more")
    };

    v_flex()
        .px(px(12.))
        .py(px(6.))
        .gap(px(4.))
        .when_some(failure, |this, message| {
            this.child(
                div()
                    .text_xs()
                    .text_color(hsla(p.destructive_foreground))
                    .child(message),
            )
        })
        .child(
            h_flex().child(
                Button::new(
                    SharedString::from(format!("issue-load-more-{group_idx}-{env_idx}")),
                    Variant::GhostMuted,
                    Size::Sm,
                )
                .label(button_label.to_owned())
                .disabled(any_loading)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.load_more(env_id.clone(), host.clone(), cx);
                })),
            ),
        )
        .into_any_element()
}

/// Static placeholder skeleton rows while the first issue list loads.
pub fn ghost(caption: &str, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let block = |width: f32, height: f32| {
        div()
            .w(px(width))
            .h(px(height))
            .rounded(px(4.))
            .bg(hsla(p.muted_foreground).opacity(0.15))
    };
    v_flex()
        .flex_1()
        .gap(px(2.))
        .pt(px(4.))
        .children((0..7).map(|index| {
            let width = [280., 220., 320., 180., 260., 240., 300.][index];
            h_flex()
                .h(px(ROW_HEIGHT))
                .px(px(12.))
                .gap(px(8.))
                .child(
                    div()
                        .size(px(16.))
                        .rounded_full()
                        .bg(hsla(p.muted_foreground).opacity(0.15)),
                )
                .child(
                    v_flex()
                        .gap(px(6.))
                        .child(block(width, 12.))
                        .child(block(width * 0.6, 8.)),
                )
        }))
        .when(!caption.is_empty(), |this| {
            this.child(
                div()
                    .px(px(12.))
                    .pt(px(8.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(caption.to_owned()),
            )
        })
}

fn empty_box(
    icon: Icon,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &App,
) -> gpui::Div {
    let p = palette(cx);
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .px(px(24.))
        .text_center()
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(40.))
                .rounded(px(8.))
                .bg(hsla(p.muted))
                .child(icon.sized(px(20.)).text_color(hsla(p.muted_foreground))),
        )
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(360.))
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(description.into()),
        )
}

pub fn unsupported_state(cx: &App) -> impl IntoElement {
    empty_box(
        Icon::CircleDot,
        t(cx, "Issues unavailable").to_owned(),
        t(cx, "Update this environment's T3 Code server to browse issues.").to_owned(),
        cx,
    )
}

pub fn unavailable(
    error: String,
    refreshing: bool,
    retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    empty_box(
        Icon::CircleDot,
        t(cx, "Could not load issue trackers").to_owned(),
        error,
        cx,
    )
    .child(
        Button::new("issues-retry", Variant::Outline, Size::Sm)
            .icon(Icon::RefreshCw)
            .label(t(cx, "Retry").to_owned())
            .disabled(refreshing)
            .on_click(retry),
    )
}

pub fn empty_no_trackers(cx: &App) -> impl IntoElement {
    empty_box(
        Icon::CircleDot,
        t(cx, "No trackers to read").to_owned(),
        t(cx, "Add a project whose remote is on a supported tracker.").to_owned(),
        cx,
    )
}
