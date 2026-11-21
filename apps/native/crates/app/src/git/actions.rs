//! The Git rows of the thread details card (`GitActionsControl.tsx` in panel mode): the quick
//! action split button with its menu, its running and success states, and the Changes row.

use gpui::{AnyElement, App, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::git_actions::{
    self, MenuItemId, QuickAction, QuickActionIcon, QuickActionKind, StackedAction, build_menu_items, format_elapsed, has_usable_menu_content,
    menu_disabled_reason, resolve_changes_totals, resolve_quick_action, terminology,
};
use t3_core::vcs::format_diff_count;

use super::dialogs::ActionRequest;
use super::{GitDialogs, GitStore};
use crate::app_state::AppState;
use crate::components::motion::Spinner;
use crate::i18n::{t, t_fmt};
use crate::icons::{Icon, ProviderLogo, source_control_logo};
use crate::thread_details::changes::Comparison;
use t3_core::diff::{ChangedFileKind, classify_changed_file};
use t3_core::i18n::IcuArg;
use crate::right_panel::tooltip_view;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::thread_details::controls::{MenuTrigger, ROW_HEIGHT, ROW_WIDTH, Row, split_row};

pub(super) fn change_request_logo(provider_kind: Option<&str>) -> ProviderLogo {
    source_control_logo(provider_kind.unwrap_or("github"))
        .unwrap_or(ProviderLogo::Mono(Icon::GitPullRequest.path()))
}

fn menu_item_logo(id: MenuItemId, provider_kind: Option<&str>) -> ProviderLogo {
    match id {
        MenuItemId::Commit => ProviderLogo::Mono(Icon::GitCommit.path()),
        MenuItemId::Push => ProviderLogo::Mono(Icon::CloudUpload.path()),
        MenuItemId::Pr => change_request_logo(provider_kind),
    }
}

fn action_menu_item(label: SharedString, logo: ProviderLogo, reason: Option<SharedString>) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        h_flex()
            .id(label.clone())
            .aria_label(label.clone())
            .w_full()
            .gap(px(8.))
            .child(div().opacity(0.8).child(logo.render(px(16.), hsla(palette(cx).muted_foreground), cx.theme().is_dark())))
            .child(label.clone())
            .when_some(reason.clone(), |row, reason| {
                row.tooltip(move |window, cx| tooltip_view(reason.clone(), window, cx))
            })
    })
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
        QuickActionKind::Publish => dialogs.update(cx, |dialogs, cx| dialogs.open_publish(window, cx)),
        QuickActionKind::Hint => {}
    }
}

/// The Git actions split row, or `None` while a thread with no runs has nothing to act on.
/// When `compact` is true (`CardDensity::Compact` or `Essential`), only active progress or an inline
/// success indicator renders.
pub fn git_actions_row(git: &Entity<GitStore>, dialogs: &Entity<GitDialogs>, no_runs_yet: bool, compact: bool, cx: &App) -> Option<AnyElement> {
    let store = git.read(cx);
    let p = palette(cx);
    let status = store.status.as_ref();
    let can_write = store.can_write_source_control();
    if status.is_some_and(|status| !status.local.is_repo) {
        if compact {
            return None;
        }
        let label = if store.initializing { "Initializing..." } else { "Initialize Git" };
        let git = git.clone();
        return Some(Row::new("git-init", label).icon(Icon::GitBranchPlus).tooltip(label).disabled(!can_write || store.initializing).on_click(move |_, _, cx| git.update(cx, |git, cx| git.init_repo(cx))).into_any_element());
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
        let spinner = Spinner::new("git-action-progress", px(16.), hsla(p.muted_foreground)).into_any_element();
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
    if compact {
        return None;
    }
    let primary = Row::new("git-quick-action", quick.label.clone());
    let primary = match quick.icon() {
        QuickActionIcon::Commit => primary.icon(Icon::GitCommit),
        QuickActionIcon::Push => primary.icon(Icon::CloudUpload),
        QuickActionIcon::Pull => primary.icon(Icon::CloudDownload),
        QuickActionIcon::Info => primary.icon(Icon::Info),
        QuickActionIcon::ChangeRequest => {
            let logo = change_request_logo(status.and_then(|status| status.provider_kind()));
            primary.leading(logo.render(px(16.), hsla(p.muted_foreground), cx.theme().is_dark()))
        }
    };
    let disabled_reason = if !can_write {
        Some("This connection cannot change source control.".to_owned())
    } else {
        quick.disabled_reason()
    };
    let primary = match disabled_reason {
        Some(reason) => primary.disabled(true).tooltip(reason),
        None => {
            let (git, dialogs) = (git.clone(), dialogs.clone());
            primary.tooltip(quick.label.clone()).on_click(move |_, window, cx| run_quick_action(&quick, &git, &dialogs, window, cx))
        }
    };
    Some(split_row("git-actions", primary, menu, cx).into_any_element())
}

fn menu_has_notice(store: &GitStore) -> bool {
    let detached = store.status.as_ref().is_some_and(|status| status.local.ref_name.is_none());
    let clean_behind = store.status.as_ref().is_some_and(|status| {
        status.local.ref_name.is_some()
            && !status.local.has_working_tree_changes
            && status.remote.behind_count > 0
            && status.remote.ahead_count == 0
    });
    detached || clean_behind || store.status_error.is_some()
}

/// The chevron and its menu (`gitItems`): Commit, Push and Create PR, with why each is
/// unavailable, then the notes about a detached HEAD, a branch behind upstream, or a status error.
fn menu_trigger(git: &Entity<GitStore>, dialogs: &Entity<GitDialogs>, cx: &App) -> impl IntoElement {
    let store = git.read(cx);
    let running = store.is_running();
    let can_write = store.can_write_source_control();
    let has_primary_remote = store.status.as_ref().is_some_and(|status| status.local.has_primary_remote);
    let can_publish = can_write && store.status.as_ref().is_some_and(|status| status.local.is_repo && !status.local.has_primary_remote);
    let menu_items = build_menu_items(store.status.as_ref(), running, has_primary_remote);
    let has_content = has_usable_menu_content(&menu_items, can_write, can_publish, menu_has_notice(store));
    let (git, dialogs) = (git.clone(), dialogs.clone());
    MenuTrigger::new("git-actions-menu", "Git action options").disabled(running || !has_content).dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, cx| {
        git.update(cx, |git, cx| git.refresh_status(false, cx));
        let store = git.read(cx);
        let p = palette(cx);
        let status = store.status.clone();
        let busy = store.is_running();
        let can_write = store.can_write_source_control();
        let has_primary_remote = status.as_ref().is_some_and(|status| status.local.has_primary_remote);
        let can_publish = can_write && status.as_ref().is_some_and(|status| status.local.is_repo && !status.local.has_primary_remote);
        let provider_kind = status.as_ref().and_then(|status| status.provider_kind());
        let terms = terminology(provider_kind);
        let status_error = store.status_error.clone();
        let mut menu = menu.min_w(px(ROW_WIDTH));
        for item in build_menu_items(status.as_ref(), busy, has_primary_remote) {
            let reason = menu_disabled_reason(&item, status.as_ref(), busy, has_primary_remote);
            let disabled = reason.is_some() || !can_write || busy;
            let logo = menu_item_logo(item.id, provider_kind);
            let dialogs = dialogs.clone();
            menu = menu.item(
                action_menu_item(item.label.into(), logo, reason.map(Into::into))
                    .disabled(disabled)
                    .on_click(move |_, window, cx| match item.id {
                        MenuItemId::Commit => dialogs.update(cx, |dialogs, cx| dialogs.open_commit(window, cx)),
                        MenuItemId::Push => dialogs.update(cx, |dialogs, cx| dialogs.request_action(ActionRequest::new(StackedAction::Push), window, cx)),
                        MenuItemId::Pr => dialogs.update(cx, |dialogs, cx| dialogs.request_action(ActionRequest::new(StackedAction::CreatePr), window, cx)),
                    }),
            );
        }
        if can_publish {
            let dialogs = dialogs.clone();
            menu = menu.item(
                action_menu_item("Publish repository...".into(), ProviderLogo::Mono(Icon::CloudUpload.path()), None)
                    .disabled(busy)
                    .on_click(move |_, window, cx| dialogs.update(cx, |dialogs, cx| dialogs.open_publish(window, cx))),
            );
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

/// The Changes row: line totals for the branch (or the dirty tree) beside what they compare
/// against (`vs main` or `Uncommitted`); opens the diff panel.
pub fn changes_row(git: &Entity<GitStore>, state: &Entity<AppState>, no_runs_yet: bool, comparison: &Comparison, cx: &App) -> Option<AnyElement> {
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
    let resolved = resolve_changes_totals(status, comparison.preview.as_ref());
    let breakdown = comparison.preview.as_ref().map(|preview| {
        change_breakdown(preview.files.iter().map(|file| (file.path.as_str(), file.additions, file.deletions)), cx)
    }).or_else(|| {
        (resolved.as_ref().is_some_and(|totals| totals.base_ref.is_none()) && status.is_some()).then(|| {
            change_breakdown(status.into_iter().flat_map(|status| &status.local.working_tree.files)
                .map(|file| (file.path.as_str(), file.insertions, file.deletions)), cx)
        })
    }).filter(|text| !text.is_empty());
    let base_label: Option<SharedString> = resolved.as_ref().map(|totals| match &totals.base_ref {
        Some(reference) => t_fmt(cx, "vs {ref}", &[("ref", IcuArg::Text(reference.as_str().into()))]).into(),
        None => t(cx, "Uncommitted").into(),
    });
    let (insertions, deletions) = resolved.map(|totals| (totals.insertions, totals.deletions)).unwrap_or((0, 0));
    let count = |sign: &str, value: u64, color: u32| {
        div().text_color(hsla(if value > 0 { color } else { p.muted_foreground })).child(format!("{sign}{}", format_diff_count(value)))
    };
    let counts = h_flex()
        .id("git-changes-counts")
        .flex_none()
        .gap(px(6.))
        .font_family(gpui_component::ActiveTheme::theme(cx).mono_font_family.clone())
        .text_xs()
        .child(count("+", insertions, p.diff_addition_foreground))
        .child(count("-", deletions, p.diff_deletion_foreground))
        .when_some(breakdown, |el, tip| {
            let tooltip: SharedString = tip.into();
            el.tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
        });
    // "Changes" keeps its width and the base label after it truncates first, so the row draws
    // its own label: `Row`'s empty one takes no room and its gap is taken back.
    let mut content = h_flex()
        .flex_1()
        .min_w_0()
        .ml(px(-10.))
        .gap(px(6.))
        .child(div().flex_none().child(t(cx, "Changes")))
        .children(base_label.map(|label| {
            let tooltip = label.clone();
            div()
                .id("git-changes-base")
                .debug_selector(|| "git-changes-base".to_owned())
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::NORMAL)
                .text_color(hsla(p.muted_foreground))
                .child(label)
                .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
        }))
        .child(div().flex_1().min_w(px(8.)))
        .child(if comparison.pending && comparison.preview.is_none() {
            Spinner::new("git-changes-loading", px(14.), hsla(p.muted_foreground)).into_any_element()
        } else {
            counts.into_any_element()
        });
    content.style().flex_grow = Some(1_000_000.);
    let state = state.clone();
    let can_open = state.read(cx).selected_thread.is_some();
    Some(
        Row::new("git-changes", "")
            .icon(Icon::FileDiff)
            .trailing(content)
            .disabled(!can_open)
            .on_click(move |_, _, cx| state.update(cx, |state, cx| state.add_diff_surface(cx)))
            .into_any_element(),
    )
}


fn change_breakdown<'a>(files: impl Iterator<Item = (&'a str, u64, u64)>, cx: &App) -> String {
    let mut counts = [(0, 0); 5];
    for (path, added, removed) in files {
        let index = match classify_changed_file(path) {
            ChangedFileKind::Code => 0,
            ChangedFileKind::Tests => 1,
            ChangedFileKind::Mocks => 2,
            ChangedFileKind::Docs => 3,
            ChangedFileKind::Generated => 4,
        };
        let (additions, deletions) = &mut counts[index];
        *additions += added;
        *deletions += removed;
    }
    ["Code", "Tests", "Mocks", "Docs", "Generated"].into_iter().zip(counts)
        .filter(|(_, (added, removed))| *added > 0 || *removed > 0)
        .map(|(label, (added, removed))| format!("{}  +{}  −{}", t(cx, label), format_diff_count(added), format_diff_count(removed)))
        .collect::<Vec<_>>()
        .join("\n")
}
