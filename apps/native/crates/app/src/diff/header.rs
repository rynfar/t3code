//! The diff toolbar (`headerRow` in `components/DiffPanel.tsx`): the scope menu and comparison
//! refs on the left; stats, refresh, fold-all, layout, wrap, whitespace and file tree on the right.

use gpui::{Context, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::h_flex;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use t3_core::diff::{DiffScope, DiffSelection, Layout, line_stat};

use super::DiffPanel;
use super::lines::stat_label;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant, segment, segmented};
use crate::theme::{hsla, palette};

pub fn render(panel: &mut DiffPanel, cx: &mut Context<DiffPanel>) -> impl IntoElement {
    let p = palette(cx);
    let entity = cx.entity().downgrade();
    let summaries = panel.summaries.clone();
    let scope = panel.scope.clone();
    let label = scope.label(&summaries);
    let thread_id = panel.thread_id.clone().unwrap_or_default();
    let remembered_base = panel.state.read(cx).panels.branch_base_refs.get(&thread_id).cloned();
    let selected_turn = match &panel.selection {
        DiffSelection::Turn { turn_id, .. } => Some(turn_id.clone()),
        _ => None,
    };
    let git_scope = matches!(scope, DiffScope::Branch { .. } | DiffScope::Unstaged);
    let parsed = panel.view.as_ref().map(|view| view.parsed.clone());
    let has_files = parsed.as_ref().is_some_and(|parsed| !parsed.files().is_empty());
    let stats = parsed.as_ref().filter(|_| has_files).map(|parsed| {
        parsed.totals.unwrap_or_else(|| {
            let stat = line_stat(parsed.files());
            (stat.additions, stat.deletions)
        })
    });
    let refs = match (&scope, parsed.as_ref()) {
        (DiffScope::Branch { base_ref }, Some(parsed)) => {
            let head = parsed.head_ref.clone().unwrap_or_else(|| "HEAD".to_owned());
            let base = parsed.base_ref.clone().or(base_ref.clone()).unwrap_or_else(|| "Automatic".to_owned());
            Some(format!("{head} → {base}"))
        }
        _ => None,
    };
    let layout = panel.state.read(cx).panels.diff_layout;
    let tree_open = panel.state.read(cx).panels.diff_file_tree_open;
    let all_collapsed = panel.all_collapsed();

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
            .submenu("Turn", window, cx, move |menu, _, _| {
                turns.iter().fold(menu, |menu, summary| {
                    let entity = turn_entity.clone();
                    let run_id = summary.run_id.clone();
                    menu.item(
                        PopupMenuItem::new(format!("Turn {}", summary.turn_count))
                            .checked(selected.as_deref() == Some(summary.run_id.as_str()))
                            .on_click(move |_, _, cx| open_turn(&entity, &run_id, cx)),
                    )
                })
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
                .child(scope_menu)
                .children(refs.map(|refs| div().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(refs))),
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
                            .tooltip(if panel.pending { "Refreshing diff…" } else { "Refresh diff" })
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
                            .tooltip(if all_collapsed { "Expand all files" } else { "Collapse all files" })
                            .on_click(act(DiffPanel::toggle_all_files)),
                    )
                })
                .child(
                    segmented("diff-layout", cx)
                        .child(segment("diff-stacked", Icon::Rows3, layout == Layout::Stacked, "Stacked diff view", cx).on_click(stacked))
                        .child(segment("diff-split", Icon::Columns2, layout == Layout::Split, "Split diff view", cx).on_click(split)),
                )
                .child(
                    Button::new("diff-wrap")
                        .variant(Variant::Toggle)
                        .size(Size::IconSm)
                        .icon(Icon::TextWrap)
                        .icon_size(14.)
                        .pressed(panel.wrap)
                        .tooltip(if panel.wrap { "Disable line wrapping" } else { "Enable line wrapping" })
                        .on_click(act(DiffPanel::toggle_wrap)),
                )
                .child(
                    Button::new("diff-whitespace")
                        .variant(Variant::Toggle)
                        .size(Size::IconSm)
                        .icon(Icon::Pilcrow)
                        .icon_size(14.)
                        .pressed(panel.ignore_whitespace)
                        .tooltip(if panel.ignore_whitespace { "Show whitespace changes" } else { "Hide whitespace changes" })
                        .on_click(act(DiffPanel::toggle_whitespace)),
                )
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
