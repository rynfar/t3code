//! The Git rows of the thread details card (`GitActionsControl.tsx` in panel mode): the quick
//! action split button with its menu, its running and success states, and the Changes row.

use gpui::{AnyElement, App, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::git_actions::{
    self, MenuItemId, QuickAction, QuickActionIcon, QuickActionKind, StackedAction, build_menu_items, format_elapsed, menu_disabled_reason,
    resolve_quick_action, terminology,
};
use t3_core::vcs::{changes_totals, format_diff_count};

use super::dialogs::ActionRequest;
use super::{GitDialogs, GitStore};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::tooltip_view;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::thread_details::controls::{MenuTrigger, ROW_HEIGHT, ROW_WIDTH, Row, menu_icon, split_row};

fn quick_icon(icon: QuickActionIcon) -> Icon {
    match icon {
        QuickActionIcon::Commit => Icon::GitCommit,
        QuickActionIcon::Push => Icon::CloudUpload,
        QuickActionIcon::Pull => Icon::CloudDownload,
        QuickActionIcon::ChangeRequest => Icon::GitPullRequest,
        QuickActionIcon::Info => Icon::Info,
    }
}

fn menu_item_icon(id: MenuItemId) -> Icon {
    match id {
        MenuItemId::Commit => Icon::GitCommit,
        MenuItemId::Push => Icon::CloudUpload,
        MenuItemId::Pr => Icon::GitPullRequest,
    }
}

/// A two-line status cell: the label, and a smaller muted line under it when there is one.
fn status_cell(icon: AnyElement, title: SharedString, detail: Option<SharedString>, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(px(ROW_HEIGHT))
        .py(px(3.))
        .pl(px(10.))
        .pr(px(10.))
        .gap(px(12.))
        .items_start()
        .child(div().h(px(24.)).flex().items_center().child(icon))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().h(px(24.)).flex().items_center().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.8)).child(div().truncate().child(title)))
                .children(detail.map(|detail| {
                    let tooltip = detail.clone();
                    div()
                        .id("git-action-detail")
                        .pt(px(2.))
                        .text_size(px(11.))
                        .text_color(hsla(p.muted_foreground))
                        .truncate()
                        .child(detail)
                        .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                })),
        )
}

/// `runQuickAction`.
fn run_quick_action(quick: &QuickAction, git: &Entity<GitStore>, dialogs: &Entity<GitDialogs>, window: &mut gpui::Window, cx: &mut App) {
    match quick.kind {
        QuickActionKind::Pull => git.update(cx, |git, cx| git.pull(cx)),
        QuickActionKind::Run(action) => dialogs.update(cx, |dialogs, cx| dialogs.request_action(ActionRequest::new(action), window, cx)),
        QuickActionKind::Publish | QuickActionKind::Hint => {}
    }
}

/// The Git actions split row, or `None` while a thread with no runs has nothing to act on.
pub fn git_actions_row(git: &Entity<GitStore>, dialogs: &Entity<GitDialogs>, no_runs_yet: bool, cx: &App) -> Option<AnyElement> {
    let store = git.read(cx);
    let p = palette(cx);
    let status = store.status.as_ref();
    if status.is_some_and(|status| !status.local.is_repo) {
        let label = if store.initializing { "Initializing..." } else { "Initialize Git" };
        let git = git.clone();
        return Some(Row::new("git-init", label).icon(Icon::GitBranchPlus).disabled(store.initializing).on_click(move |_, _, cx| git.update(cx, |git, cx| git.init_repo(cx))).into_any_element());
    }
    let has_primary_remote = status.is_some_and(|status| status.local.has_primary_remote);
    let is_default_ref = status.is_some_and(|status| status.local.is_default_ref);
    let running = store.is_running();
    let quick = resolve_quick_action(status, running, is_default_ref, has_primary_remote);
    if git_actions::quiet_empty_thread(no_runs_yet, running, store.status_error.is_some(), &quick, status) {
        return None;
    }
    if let Some(progress) = store.progress.as_ref() {
        let presentation = progress.presentation();
        let elapsed = format_elapsed(presentation.started_at_ms, now_ms());
        let spinner = Icon::LoaderCircle.sized(px(16.)).text_color(hsla(p.muted_foreground)).into_any_element();
        let fill = hsla(p.foreground).opacity(if cx.theme().is_dark() { 0.055 } else { 0.035 });
        return Some(
            h_flex()
                .w_full()
                .rounded(px(8.))
                .bg(fill)
                .items_start()
                .child(div().flex_1().min_w_0().child(status_cell(spinner, presentation.status.into(), presentation.output.map(SharedString::from), cx)))
                .child(div().flex_none().h(px(ROW_HEIGHT)).flex().items_center().pr(px(10.)).text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(elapsed))
                .into_any_element(),
        );
    }
    let menu = menu_trigger(git, dialogs, cx);
    if let Some(success) = store.success.as_ref() {
        let check = Icon::Check.sized(px(14.)).text_color(hsla(p.success_foreground)).into_any_element();
        let cell = status_cell(check, success.title.clone().into(), success.description.clone().map(SharedString::from), cx);
        return Some(split_row("git-actions", cell, menu, cx).into_any_element());
    }
    let icon = quick_icon(quick.icon());
    let disabled_reason = match quick.kind {
        QuickActionKind::Publish => Some("Publishing a repository is not available in this app yet.".to_owned()),
        _ => quick.disabled_reason(),
    };
    let primary = match disabled_reason {
        Some(reason) => Row::new("git-quick-action", quick.label.clone()).icon(icon).disabled(true).tooltip(reason),
        None => {
            let (git, dialogs) = (git.clone(), dialogs.clone());
            Row::new("git-quick-action", quick.label.clone()).icon(icon).on_click(move |_, window, cx| run_quick_action(&quick, &git, &dialogs, window, cx))
        }
    };
    Some(split_row("git-actions", primary, menu, cx).into_any_element())
}

/// The chevron and its menu (`gitItems`): Commit, Push and Create PR, with why each is
/// unavailable, then the notes about a detached HEAD, a branch behind upstream, or a status error.
fn menu_trigger(git: &Entity<GitStore>, dialogs: &Entity<GitDialogs>, cx: &App) -> impl IntoElement {
    let running = git.read(cx).is_running();
    let (git, dialogs) = (git.clone(), dialogs.clone());
    MenuTrigger::new("git-actions-menu", "Git action options").disabled(running).dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, cx| {
        git.update(cx, |git, cx| git.refresh_status(false, cx));
        let store = git.read(cx);
        let p = palette(cx);
        let status = store.status.clone();
        let busy = store.is_running();
        let has_primary_remote = status.as_ref().is_some_and(|status| status.local.has_primary_remote);
        let terms = terminology(status.as_ref().and_then(|status| status.provider_kind()));
        let status_error = store.status_error.clone();
        let mut menu = menu.min_w(px(ROW_WIDTH));
        for item in build_menu_items(status.as_ref(), busy, has_primary_remote) {
            let icon = menu_icon(menu_item_icon(item.id));
            let entry = match menu_disabled_reason(&item, status.as_ref(), busy, has_primary_remote) {
                Some(reason) => {
                    let label: SharedString = item.label.clone().into();
                    let reason: SharedString = reason.into();
                    PopupMenuItem::element(move |_, _| {
                        let reason = reason.clone();
                        div().id(label.clone()).w_full().child(label.clone()).tooltip(move |window, cx| tooltip_view(reason.clone(), window, cx))
                    })
                    .icon(icon)
                    .disabled(true)
                }
                None => {
                    let (git, dialogs) = (git.clone(), dialogs.clone());
                    PopupMenuItem::new(item.label.clone()).icon(icon).on_click(move |_, window, cx| match item.id {
                        MenuItemId::Commit => dialogs.update(cx, |dialogs, cx| dialogs.open_commit(window, cx)),
                        MenuItemId::Push => dialogs.update(cx, |dialogs, cx| dialogs.request_action(ActionRequest::new(StackedAction::Push), window, cx)),
                        MenuItemId::Pr => dialogs.update(cx, |dialogs, cx| dialogs.request_action(ActionRequest::new(StackedAction::CreatePr), window, cx)),
                    })
                    .disabled(git.read(cx).is_running())
                }
            };
            menu = menu.item(entry);
        }
        let note = |text: String, color: gpui::Hsla| PopupMenuItem::element(move |_, _| div().text_xs().text_color(color).child(text.clone())).disabled(true);
        if let Some(status) = status.as_ref() {
            if status.local.ref_name.is_none() {
                menu = menu.item(note(format!("Detached HEAD: create and check out a branch to enable push and {} actions.", terms.singular), hsla(p.warning_foreground)));
            } else if !status.local.has_working_tree_changes && status.remote.behind_count > 0 && status.remote.ahead_count == 0 {
                menu = menu.item(note("Behind upstream. Pull/rebase first.".into(), hsla(p.warning_foreground)));
            }
        }
        if let Some(error) = status_error {
            menu = menu.item(note(error, hsla(p.destructive_foreground)));
        }
        menu
    })
}

/// The Changes row: line totals for the branch (or the dirty tree); opens the diff panel.
pub fn changes_row(git: &Entity<GitStore>, state: &Entity<AppState>, no_runs_yet: bool, cx: &App) -> Option<AnyElement> {
    let store = git.read(cx);
    let status = store.status.as_ref();
    if status.is_some_and(|status| !status.local.is_repo) {
        return None;
    }
    let has_primary_remote = status.is_some_and(|status| status.local.has_primary_remote);
    let is_default_ref = status.is_some_and(|status| status.local.is_default_ref);
    let quick = resolve_quick_action(status, store.is_running(), is_default_ref, has_primary_remote);
    if git_actions::quiet_empty_thread(no_runs_yet, store.is_running(), store.status_error.is_some(), &quick, status) {
        return None;
    }
    let p = palette(cx);
    let (insertions, deletions) = status.map(changes_totals).unwrap_or((0, 0));
    let count = |sign: &str, value: u64, color: u32| {
        div().text_color(hsla(if value > 0 { color } else { p.muted_foreground })).child(format!("{sign}{}", format_diff_count(value)))
    };
    let totals = h_flex()
        .flex_none()
        .gap(px(6.))
        .font_family(gpui_component::ActiveTheme::theme(cx).mono_font_family.clone())
        .text_xs()
        .child(count("+", insertions, p.diff_addition_foreground))
        .child(count("-", deletions, p.diff_deletion_foreground));
    let state = state.clone();
    Some(Row::new("git-changes", "Changes").icon(Icon::FileDiff).trailing(totals).on_click(move |_, _, cx| state.update(cx, |state, cx| state.add_diff_surface(cx))).into_any_element())
}
