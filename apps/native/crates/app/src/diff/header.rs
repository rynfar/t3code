//! The diff toolbar (`headerRow` in `components/DiffPanel.tsx`): the scope menu and comparison
//! refs on the left; stats, refresh, fold-all, layout, wrap, whitespace and file tree on the right.

use gpui::{Context, IntoElement, ParentElement, Styled, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use t3_core::diff::{DiffScope, DiffSelection, Layout, line_stat};
use t3_core::sidebar::time::{compact_relative_label, parse_iso, to_local};
use t3_core::timeline::TimestampLocale;

use super::DiffPanel;
use super::lines::stat_label;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant, segment, segmented, tooltip_view};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

pub fn render(panel: &mut DiffPanel, window: &mut Window, cx: &mut Context<DiffPanel>) -> impl IntoElement {
    let p = palette(cx);
    let entity = cx.entity().downgrade();
    let summaries = panel.summaries.clone();
    let scope = panel.scope.clone();
    let selected_commit = match &scope {
        DiffScope::Commit { sha, .. } => Some(sha.clone()),
        _ => None,
    };
    let label = match &scope {
        DiffScope::Commit { sha, .. } => panel
            .review()
            .and_then(|review| review.source.as_ref())
            .filter(|source| source.head_ref.as_deref() == Some(sha.as_str()))
            .and_then(|source| source.commit_subject.clone())
            .unwrap_or_else(|| scope.label(&summaries)),
        _ => scope.label(&summaries),
    };
    let thread_id = panel.thread_id.clone().unwrap_or_default();
    let remembered_base = panel.state.read(cx).panels.branch_base_refs.get(&thread_id).cloned();
    let selected_turn = match &panel.selection {
        DiffSelection::Turn { turn_id, .. } => Some(turn_id.clone()),
        _ => None,
    };
    let external = panel.is_external();
    let git_scope = !external && matches!(scope, DiffScope::Branch { .. } | DiffScope::Unstaged | DiffScope::Commit { .. });
    let header_commit_url = selected_commit.as_deref().and_then(|sha| panel.commit_url(sha, cx));
    let can_read = panel.state.read(cx).can_read_threads();
    let commits_snapshot = panel.branch_commits.clone();
    let commits_error = panel.branch_commits_error.clone();
    let project_identity = panel
        .thread_id
        .as_deref()
        .and_then(|id| {
            let state = panel.state.read(cx);
            let thread = state.thread_shell(id)?;
            state.shell.project(&thread.project_id)?.repository_identity.clone()
        });
    let parsed = panel.view.as_ref().map(|view| view.parsed.clone());
    let has_files = parsed.as_ref().is_some_and(|parsed| !parsed.files().is_empty());
    let stats = parsed.as_ref().filter(|_| has_files).map(|parsed| {
        parsed.totals.unwrap_or_else(|| {
            let stat = line_stat(parsed.files());
            (stat.additions, stat.deletions)
        })
    });
    let base_ref_picker = super::base_ref::picker(panel, window, cx);
    let layout = crate::settings::client_settings(cx).diff_layout();
    let tree_open = panel.state.read(cx).panels.diff_file_tree_open;
    let all_collapsed = panel.all_collapsed();
    let test_files = panel.test_file_count();
    let hides_tests = panel.hides_test_files();

    let menu_selected_commit = selected_commit.clone();
    let scope_menu = Button::new("diff-scope")
        .variant(Variant::Secondary)
        .size(Size::Xs)
        .label(label.clone())
        .trailing(Icon::ChevronDown)
        .dropdown_menu(move |menu, window, cx| {
            let select = |selection: DiffSelection| {
                let entity = entity.clone();
                move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |panel, cx| panel.select(selection.clone(), cx));
                    }
                }
            };
            let latest = summaries.first();
            let latest_selected = matches!(scope, DiffScope::Turn { latest: true, .. });
            let turns = summaries.clone();
            let selected = selected_turn.clone();
            let turn_entity = entity.clone();
            let commits_entity = entity.clone();
            let commits_selected = menu_selected_commit.clone();
            let commits_data = commits_snapshot.clone();
            let commits_err = commits_error.clone();
            let commits_identity = project_identity.clone();
            if let Some(panel_entity) = entity.upgrade() {
                cx.defer(move |cx| {
                    panel_entity.update(cx, |panel, cx| panel.refresh_branch_commits(cx));
                });
            }
            menu.item(
                PopupMenuItem::new("Changes")
                    .checked(matches!(scope, DiffScope::Branch { .. }))
                    .on_click(select(DiffSelection::Branch { base_ref: remembered_base.clone() })),
            )
            .item(PopupMenuItem::new("Uncommitted").checked(scope == DiffScope::Unstaged).on_click(select(DiffSelection::Unstaged)))
            .item({
                let entity = entity.clone();
                let run_id = latest.map(|summary| summary.run_id.clone()).unwrap_or_default();
                PopupMenuItem::new("Latest turn")
                    .checked(latest_selected)
                    .disabled(latest.is_none())
                    .on_click(move |_, _, cx| open_turn(&entity, &run_id, cx))
            })
            .submenu("Turn", window, cx, move |menu, _, cx| {
                let format = crate::settings::store::client(cx).timestamp_format();
                let zone = crate::timestamp::viewer_zone();
                let muted = hsla(palette(cx).muted_foreground);
                turns.iter().fold(menu, |menu, summary| {
                    let entity = turn_entity.clone();
                    let run_id = summary.run_id.clone();
                    let label: gpui::SharedString = format!("Turn {}", summary.turn_count).into();
                    // `formatShortTimestamp(summary.completedAt, timestampFormat)`.
                    let time: gpui::SharedString = parse_iso(&summary.completed_at)
                        .map(|ms| crate::timestamp::SystemTimestampLocale.time(to_local(ms, &zone), format))
                        .unwrap_or_default()
                        .into();
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            h_flex().w_full().gap(px(8.)).child(label.clone()).child(div().ml_auto().text_xs().text_color(muted).child(time.clone()))
                        })
                        .checked(selected.as_deref() == Some(summary.run_id.as_str()))
                        .on_click(move |_, _, cx| open_turn(&entity, &run_id, cx)),
                    )
                })
            })
            .submenu("Commits", window, cx, move |mut menu, _, cx| {
                let muted = hsla(palette(cx).muted_foreground);
                let now = now_ms();
                if let Some(commits) = &commits_data {
                    for (commit_ix, commit) in commits.commits.iter().enumerate() {
                        let entity = commits_entity.clone();
                        let sha = commit.sha.clone();
                        let short: gpui::SharedString = commit.sha[..commit.sha.len().min(7)].to_owned().into();
                        let subject: gpui::SharedString = commit.subject.clone().into();
                        let age: gpui::SharedString = parse_iso(&commit.committed_at)
                            .map(|ms| compact_relative_label(ms, now))
                            .unwrap_or_default()
                            .into();
                        let checked = commits_selected.as_deref() == Some(commit.sha.as_str());
                        let commit_url = t3_core::diff::project_commit_url(commits_identity.as_ref(), &commit.sha);
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                let family = cx.theme().mono_font_family.clone();
                                h_flex()
                                    .w_full()
                                    .min_w(px(260.))
                                    .max_w(px(360.))
                                    .gap(px(8.))
                                    .child(div().flex_1().min_w_0().truncate().child(subject.clone()))
                                    .child(div().flex_none().text_xs().text_color(muted).child(age.clone()))
                                    .child(if let Some(url) = commit_url.clone() {
                                        div()
                                            .id(("diff-commit-url", commit_ix))
                                            .flex_none()
                                            .font_family(family)
                                            .text_xs()
                                            .text_color(muted)
                                            .underline()
                                            .cursor_pointer()
                                            .on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                cx.open_url(&url);
                                            })
                                            .child(short.clone())
                                            .into_any_element()
                                    } else {
                                        div().flex_none().font_family(family).text_xs().text_color(muted).child(short.clone()).into_any_element()
                                    })
                            })
                            .checked(checked)
                            .on_click(move |_, _, cx| {
                                if let Some(entity) = entity.upgrade() {
                                    entity.update(cx, |panel, cx| panel.select_commit(&sha, cx));
                                }
                            }),
                        );
                    }
                    if commits.commits.is_empty() {
                        menu = menu.item(PopupMenuItem::new("No commits on this branch.").disabled(true));
                    } else if commits.truncated {
                        menu = menu.item(PopupMenuItem::new(format!("Showing latest {} commits", commits.commits.len())).disabled(true));
                    }
                } else if let Some(err) = &commits_err {
                    menu = menu.item(PopupMenuItem::new(err.clone()).disabled(true));
                } else if !can_read {
                    menu = menu.item(PopupMenuItem::new("This connection cannot read local diffs.").disabled(true));
                } else {
                    menu = menu.item(PopupMenuItem::new("Loading commits...").disabled(true));
                }
                menu
            })
        });

    let entity = cx.entity().downgrade();
    let act = move |run: fn(&mut DiffPanel, &mut Context<DiffPanel>)| {
        let entity = entity.clone();
        move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            if let Some(entity) = entity.upgrade() {
                entity.update(cx, run);
            }
        }
    };
    let stacked = cx.listener(|panel, _, _, cx| panel.set_layout(Layout::Stacked, cx));
    let split = cx.listener(|panel, _, _, cx| panel.set_layout(Layout::Split, cx));

    h_flex()
        .flex_none()
        .h(px(28.))
        .mb(px(12.))
        .px(px(8.))
        .gap(px(8.))
        .justify_between()
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(12.))
                .when(!external, |this| this.child(scope_menu))
                .when(!external, |this| {
                    this.children(selected_commit.map(|sha| {
                        let short = sha[..sha.len().min(7)].to_owned();
                        let family = cx.theme().mono_font_family.clone();
                        match header_commit_url {
                            Some(url) => {
                                let tooltip_url: gpui::SharedString = url.clone().into();
                                div()
                                    .id("diff-header-commit-sha")
                                    .flex_none()
                                    .font_family(family)
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .underline()
                                    .cursor_pointer()
                                    .tooltip(move |window, cx| tooltip_view(tooltip_url.clone(), window, cx))
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                                    .child(short)
                                    .into_any_element()
                            }
                            None => div()
                                .flex_none()
                                .font_family(family)
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(short)
                                .into_any_element(),
                        }
                    }))
                })
                .child(base_ref_picker),
        )
        .child(
            h_flex()
                .flex_none()
                .gap(px(4.))
                .children(stats.map(|(additions, deletions)| div().mr(px(4.)).text_size(px(11.)).child(stat_label(additions, deletions, cx))))
                .when(git_scope, |this| {
                    this.child(
                        Button::new("diff-refresh")
                            .variant(Variant::Ghost)
                            .size(Size::IconSm)
                            .icon(Icon::RefreshCw)
                            .icon_size(14.)
                            .tooltip(crate::i18n::t(cx, if panel.pending || panel.review().is_some_and(|r| r.states.values().any(|s| s.pending)) { "Refreshing diff…" } else { "Refresh diff" }))
                            .on_click(act(DiffPanel::refresh)),
                    )
                })
                .when(has_files, |this| {
                    this.child(
                        Button::new("diff-fold-all")
                            .variant(Variant::Ghost)
                            .size(Size::IconSm)
                            .icon(if all_collapsed { Icon::ChevronsUpDown } else { Icon::ChevronsDownUp })
                            .icon_size(14.)
                            .tooltip(crate::i18n::t(cx, if all_collapsed { "Expand all files" } else { "Collapse all files" }))
                            .on_click(act(DiffPanel::toggle_all_files)),
                    )
                })
                .child(
                    segmented("diff-layout", cx)
                        .child(segment("diff-stacked", Icon::Rows3, layout == Layout::Stacked, crate::i18n::t(cx, "Stacked diff view"), window, cx).on_click(stacked))
                        .child(segment("diff-split", Icon::Columns2, layout == Layout::Split, crate::i18n::t(cx, "Split diff view"), window, cx).on_click(split)),
                )
                .child(
                    Button::new("diff-wrap")
                        .variant(Variant::Toggle)
                        .size(Size::IconSm)
                        .icon(Icon::TextWrap)
                        .icon_size(14.)
                        .pressed(panel.wrap)
                        .tooltip(crate::i18n::t(cx, if panel.wrap { "Disable line wrapping" } else { "Enable line wrapping" }))
                        .on_click(act(DiffPanel::toggle_wrap)),
                )
                .when(!external, |this| this.child(
                    Button::new("diff-whitespace")
                        .variant(Variant::Toggle)
                        .size(Size::IconSm)
                        .icon(Icon::Pilcrow)
                        .icon_size(14.)
                        .pressed(panel.ignore_whitespace)
                        .tooltip(crate::i18n::t(cx, if panel.ignore_whitespace { "Show whitespace changes" } else { "Hide whitespace changes" }))
                        .on_click(act(DiffPanel::toggle_whitespace)),
                ))
                .when(test_files > 0, |this| this.child(
                    Button::new("diff-hide-tests")
                        .variant(Variant::Toggle)
                        .size(Size::IconSm)
                        .icon(Icon::FlaskConical)
                        .icon_size(14.)
                        .pressed(hides_tests)
                        .tooltip(if hides_tests {
                            crate::i18n::t_fmt(cx, "{count, plural, one {Show {count} hidden test file} other {Show {count} hidden test files}}", &[("count", test_files.into())])
                        } else {
                            crate::i18n::t(cx, "Hide test files").to_owned()
                        })
                        .on_click(act(DiffPanel::toggle_hide_test_files)),
                ))
                .when(has_files, |this| {
                    this.child(
                        Button::new("diff-tree")
                            .variant(Variant::Toggle)
                            .size(Size::IconSm)
                            .icon(Icon::FolderTree)
                            .icon_size(14.)
                            .pressed(tree_open)
                            .tooltip(if tree_open { "Hide file tree" } else { "Show file tree" })
                            .on_click(act(DiffPanel::toggle_tree)),
                    )
                }),
        )
}

/// `selectTurn` through the same entry point a turn's changed-files summary uses.
fn open_turn(entity: &gpui::WeakEntity<DiffPanel>, run_id: &str, cx: &mut gpui::App) {
    if let Some(entity) = entity.upgrade() {
        let state = entity.read(cx).state.clone();
        state.update(cx, |state, cx| state.open_turn_diff(run_id, None, cx));
    }
}
