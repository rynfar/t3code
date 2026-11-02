//! List rows (`PullRequestRow.tsx`, `PullRequestListRow.tsx`, `PullRequestListGroups.tsx`) and the
//! list's loading, unavailable and empty states (`PullRequestGhosts.tsx`,
//! `PullRequestsUnavailableState.tsx`, `PullRequestListEmptyState.tsx`).

use gpui::{AnyElement, App, ClickEvent, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::pull_requests::PrRef;
use t3_core::pull_requests::detail::StateKind;
use t3_core::pull_requests::list::{GroupKey, entry_key, host_actions, label_color, matched_elsewhere, quick_actions, QuickAction};
use t3_core::pull_requests::query::parse_query;
use t3_core::pull_requests::wire::{ListEntry, Mergeability, PrState};
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::controls::{BadgeKind, Button, Size, Variant, avatar, badge, checks_glyph, diff_stat, glyph, review_glyph, state_glyph};
use super::page::{PullRequestsPage, Row};
use crate::icons::Icon;
use crate::right_panel::{focusable, tooltip_view};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

pub fn render(page: &mut PullRequestsPage, index: usize, window: &mut Window, cx: &mut Context<PullRequestsPage>) -> AnyElement {
    match page.rows.get(index).copied() {
        Some(Row::Header(group)) => group_header(page, group, cx),
        Some(Row::Entry(entry)) => entry_row(page, entry, window, cx),
        Some(Row::Footer) => footer(page, cx),
        None => div().into_any_element(),
    }
}

fn group_header(page: &PullRequestsPage, index: usize, cx: &App) -> AnyElement {
    let p = palette(cx);
    let Some(group) = page.groups.get(index) else { return div().into_any_element() };
    let icon = match group.key {
        GroupKey::Authored => Icon::PenLine,
        GroupKey::ReviewRequested => Icon::Eye,
        GroupKey::Others => Icon::Users,
    };
    h_flex()
        .gap(px(8.))
        .px(px(12.))
        .pt(px(if index == 0 { 4. } else { 12. }))
        .pb(px(4.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground).opacity(0.7))
        .child(icon.sized(px(14.)))
        .child(group.label)
        .child(div().text_color(hsla(p.muted_foreground).opacity(0.5)).child(group.entries.len().to_string()))
        .child(div().flex_1().min_w(px(8.)).h(px(1.)).bg(hsla(p.border)))
        .into_any_element()
}

fn entry_row(page: &PullRequestsPage, index: usize, window: &mut Window, cx: &mut Context<PullRequestsPage>) -> AnyElement {
    let Some(result) = page.data.result.as_ref() else { return div().into_any_element() };
    let Some(entry) = result.entries.get(index) else { return div().into_any_element() };
    let p = palette(cx);
    let reference = PrRef::of(entry);
    let key = entry_key(entry);
    let selected = page.selected.as_ref() == Some(&reference);
    let acting = page.acting.contains(&key);
    let actions = quick_actions(entry.state, entry.is_draft, &host_actions(&result.providers, entry));
    let show_actions = !actions.is_empty() && (page.shift || acting);
    let elsewhere = matched_elsewhere(entry, &parse_query(&page.typed).text);
    let select = cx.listener(move |this, _: &ClickEvent, _, cx| this.select(reference.clone(), cx));
    let id = SharedString::from(format!("pr-row-{key}"));
    let row = focusable(div().id(id.clone()), id.into(), true, window, cx)
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .cursor_pointer()
        .child(row_glyph(entry, cx))
        .child(lines(entry, elsewhere, cx))
        .on_click(select);
    h_flex()
        .h(px(super::page::ROW_HEIGHT))
        .rounded(px(6.))
        .when(selected, |this| this.bg(hsla(p.accent)))
        .when(!selected, |this| this.hover(|style| style.bg(hsla(p.accent).opacity(0.6))))
        .child(row)
        .when(show_actions, |this| this.child(speed_actions(entry, index, &actions, acting, cx)))
        .into_any_element()
}

/// `PullRequestRowGlyph`: the state icon, with the conflict mark on an open, ready pull request.
fn row_glyph(entry: &ListEntry, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let kind = StateKind::of(entry.state, entry.is_draft);
    let (icon, color) = state_glyph(kind, cx);
    let conflicting = entry.state == PrState::Open && !entry.is_draft && entry.mergeability == Mergeability::Conflicting;
    let label = if conflicting { format!("Conflicts with {}", entry.base_branch) } else { kind.label().to_owned() };
    div()
        .id(SharedString::from(format!("pr-glyph-{}", entry_key(entry))))
        .relative()
        .self_start()
        .mt(px(3.))
        .flex_none()
        .child(icon.sized(px(16.)).text_color(color))
        .when(conflicting, |this| {
            this.child(div().absolute().right(px(-4.)).bottom(px(-4.)).rounded_full().bg(hsla(p.background)).child(Icon::TriangleAlert.sized(px(12.)).text_color(hsla(p.destructive))))
        })
        .tooltip(move |window, cx| tooltip_view(label.clone().into(), window, cx))
}

fn lines(entry: &ListEntry, elsewhere: bool, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let key = entry_key(entry);
    let mono = cx.theme().mono_font_family.clone();
    let updated = parse_iso(&entry.updated_at).map(|at| compact_relative_label(at, now_ms()));
    let author = entry.author.as_ref().map_or("ghost", |author| author.login.as_str()).to_owned();
    let labels = entry.labels.iter().take(3).enumerate().map(|(index, label)| {
        let kind = label_color(label.color.as_deref()).map_or(BadgeKind::Secondary, BadgeKind::Label);
        let more = entry.labels.len().saturating_sub(index + 1);
        let text = if index == 2 && more > 0 { format!("{} +{more}", label.name) } else { label.name.clone() };
        badge(text, kind, false, cx)
    });
    v_flex()
        .flex_1()
        .min_w_0()
        .child(
            h_flex()
                .min_w_0()
                .gap(px(6.))
                .child(div().flex_none().font_family(mono).text_xs().text_color(hsla(p.muted_foreground)).child(format!("#{}", entry.number)))
                .child(div().min_w_0().truncate().text_sm().text_color(hsla(p.foreground)).child(entry.title.clone()))
                .children(entry.checks_state.map(|state| {
                    let (icon, color) = checks_glyph(state, cx);
                    glyph(SharedString::from(format!("pr-checks-{key}")), icon, color, 14., t3_core::pull_requests::detail::checks_state_label(state))
                }))
                .children(entry.review_decision.and_then(|decision| review_glyph(decision, cx)).map(|(icon, color, label)| {
                    glyph(SharedString::from(format!("pr-review-{key}")), icon, color, 14., label)
                }))
                .child(div().flex_1())
                .when(entry.additions + entry.deletions > 0, |this| this.child(diff_stat(entry.additions, entry.deletions, cx))),
        )
        .child(
            h_flex()
                .min_w_0()
                .gap(px(6.))
                .overflow_hidden()
                .text_size(px(11.))
                .line_height(px(16.))
                .text_color(hsla(p.muted_foreground))
                .when(elsewhere, |this| {
                    this.child(
                        h_flex()
                            .flex_none()
                            .gap(px(4.))
                            .px(px(4.))
                            .rounded_full()
                            .border_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .text_size(px(10.))
                            .child(Icon::Search.sized(px(12.)))
                            .child("matched in the description"),
                    )
                })
                .child(h_flex().flex_none().max_w(px(160.)).gap(px(4.)).child(avatar(&author, 14., cx)).child(div().truncate().child(author)))
                .child(div().min_w_0().truncate().child(entry.repository.clone()))
                .children(labels)
                .child(div().flex_1())
                .children(updated.map(|label| div().flex_none().whitespace_nowrap().child(label))),
        )
}

/// `PullRequestSpeedActions`: shown while Shift is held or an action runs.
fn speed_actions(entry: &ListEntry, index: usize, actions: &[QuickAction], acting: bool, cx: &mut Context<PullRequestsPage>) -> impl IntoElement {
    let stacked_merge = entry.stack.is_some();
    h_flex().flex_none().gap(px(4.)).pr(px(12.)).children(actions.iter().map(|action| {
        let action = *action;
        let icon = match action {
            QuickAction::Close => Icon::GitPullRequestClosed,
            QuickAction::Merge => Icon::GitMerge,
            QuickAction::Ready | QuickAction::Reopen => Icon::GitPullRequestArrow,
        };
        let blocked = action == QuickAction::Merge && stacked_merge;
        let tooltip = if blocked { "Open this pull request to merge its stack".to_owned() } else { format!("{} immediately", action.label()) };
        Button::new(SharedString::from(format!("pr-quick-{}-{index}", action.label())), if action == QuickAction::Close { Variant::DestructiveOutline } else { Variant::Outline }, Size::Xs)
            .icon(icon)
            .label(action.label())
            .busy(acting)
            .disabled(acting || blocked)
            .tooltip(tooltip)
            .on_click(cx.listener(move |this, _, _, cx| this.quick_action(index, action.action(), cx)))
    }))
}

fn footer(page: &PullRequestsPage, cx: &mut Context<PullRequestsPage>) -> AnyElement {
    let p = palette(cx);
    let content = if page.data.loading_more {
        div().child("Loading more").into_any_element()
    } else if page.can_load_more() {
        Button::new("pr-load-more", Variant::Outline, Size::Sm)
            .label("Load more pull requests")
            .disabled(page.data.pending)
            .on_click(cx.listener(|this, _, _, cx| this.load_more(cx)))
            .into_any_element()
    } else {
        div().child("Narrow your search to find more pull requests.").into_any_element()
    };
    h_flex().justify_center().py(px(12.)).text_xs().text_color(hsla(p.muted_foreground)).child(content).into_any_element()
}

/// `PullRequestListGhost`: placeholder rows under a caption. Static: no pulse.
pub fn ghost(caption: &str, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let block = |width: f32, height: f32| div().w(px(width)).h(px(height)).rounded(px(4.)).bg(hsla(p.muted_foreground).opacity(0.15));
    v_flex()
        .flex_1()
        .gap(px(2.))
        .pt(px(4.))
        .children((0..7).map(|index| {
            let width = [280., 220., 320., 180., 260., 240., 300.][index];
            h_flex()
                .h(px(super::page::ROW_HEIGHT))
                .px(px(12.))
                .gap(px(8.))
                .child(div().size(px(16.)).rounded_full().bg(hsla(p.muted_foreground).opacity(0.15)))
                .child(v_flex().gap(px(6.)).child(block(width, 12.)).child(block(width * 0.6, 8.)))
        }))
        .when(!caption.is_empty(), |this| this.child(div().px(px(12.)).pt(px(8.)).text_xs().text_color(hsla(p.muted_foreground)).child(caption.to_owned())))
}

fn empty(icon: Icon, title: impl Into<SharedString>, description: impl Into<SharedString>, cx: &App) -> gpui::Div {
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
        .child(div().text_base().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title.into()))
        .child(div().max_w(px(360.)).text_sm().text_color(hsla(p.muted_foreground)).child(description.into()))
}

pub fn unavailable(error: String, refreshing: bool, retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> impl IntoElement {
    empty(Icon::GitPullRequestArrow, "Could not load pull requests", error, cx)
        .child(Button::new("pr-retry", Variant::Outline, Size::Sm).icon(Icon::RefreshCw).label("Retry").disabled(refreshing).on_click(retry))
}

/// `PullRequestListEmptyState`.
pub fn empty_state(page: &PullRequestsPage, _window: &mut Window, cx: &mut Context<PullRequestsPage>) -> impl IntoElement {
    let has_projects = page.state.read(cx).shell.snapshot.as_ref().is_none_or(|snapshot| !snapshot.projects.is_empty());
    if !has_projects {
        return empty(Icon::GitBranch, "No projects in this workspace", "Add a project, and the pull requests from its repository appear here.", cx);
    }
    let query = page.typed.trim().to_owned();
    let searching = !query.is_empty() && (query != page.sent || page.data.pending);
    if searching {
        let shown: String = if query.chars().count() > 48 { format!("{}…", query.chars().take(48).collect::<String>()) } else { query };
        return v_flex().flex_1().child(ghost(&format!("Searching every host for “{shown}”"), cx));
    }
    let refreshing = page.data.pending;
    let refresh = Button::new("pr-check-again", Variant::Outline, Size::Sm)
        .icon(Icon::RefreshCw)
        .label(if refreshing { "Checking..." } else { "Check again" })
        .disabled(refreshing)
        .on_click(cx.listener(|this, _, _, cx| this.refresh_from_host(cx)));
    if !query.is_empty() {
        let shown: String = if query.chars().count() > 48 { format!("{}…", query.chars().take(48).collect::<String>()) } else { query };
        return empty(Icon::GitBranch, format!("Nothing matches “{shown}”"), "The hosts were searched for it. Try fewer words, or search by number, author or branch.", cx).child(
            h_flex()
                .gap(px(8.))
                .child(Button::new("pr-clear-search", Variant::Outline, Size::Sm).icon(Icon::Search).label("Clear search").on_click(cx.listener(|this, _, window, cx| this.clear_query(window, cx))))
                .child(refresh),
        );
    }
    let filtered = page.prefs.filtered();
    let (title, description) = if filtered {
        ("Nothing under these filters", "Widen the state, involvement or project filter to see more.")
    } else {
        ("No pull requests", "Pull requests from every project in this workspace appear here.")
    };
    let load_more = page.can_load_more().then(|| {
        Button::new("pr-empty-load-more", Variant::Outline, Size::Sm)
            .label(if page.data.loading_more { "Loading..." } else { "Load more pull requests" })
            .disabled(page.data.loading_more)
            .on_click(cx.listener(|this, _, _, cx| this.load_more(cx)))
    });
    empty(Icon::GitBranch, title, description, cx).child(h_flex().gap(px(8.)).children(load_more).child(refresh))
}
