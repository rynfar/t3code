//! The card's sections (`ThreadDetailsPanel.tsx`): Workspace (environment and worktree, open in
//! editor, project scripts), Version Control (branch and pull request, Git actions, changes)
//! Lineage and Side chats (`ThreadRelationshipsControl.tsx`, rows from `crate::lineage`).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{AnyElement, App, ClickEvent, ClipboardItem, Entity, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use t3_core::project_scripts::{ProjectScript, ScriptIcon, command_for_project_script, menu_label};
use t3_core::shell::ThreadShell;
use t3_core::vcs::{self, editor_label, editor_options};

use super::controls::{MenuTrigger, ROW_WIDTH, Row, menu_icon, menu_shortcut, section, split_row};
use super::{Details, project_scripts, run_button_script};
use crate::app_state::AppState;
use crate::lineage::{LineageRow, MergeBack, Relationships, StopAction};
use crate::git::{self, BranchPicker, GitDialogs, GitStore};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// `THREAD_LINEAGE_INITIAL_COUNT` and `THREAD_LINEAGE_PAGE_COUNT`.
pub const LINEAGE_INITIAL: usize = 6;
pub const LINEAGE_PAGE: usize = 12;

pub struct Ctx {
    /// Set when a right-click lands on a row with its own menu, so the card's menu stays shut.
    pub row_menu_claim: Rc<Cell<bool>>,
    pub state: Entity<AppState>,
    pub details: Entity<Details>,
    pub git: Entity<GitStore>,
    pub dialogs: Entity<GitDialogs>,
    pub picker: Entity<BranchPicker>,
}

fn copy_with_toast(text: String, title: &'static str, state: &Entity<AppState>, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
    state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Success, title).description(text), cx));
}

fn script_icon(icon: ScriptIcon) -> Icon {
    match icon {
        ScriptIcon::Test => Icon::FlaskConical,
        ScriptIcon::Lint => Icon::ListChecks,
        ScriptIcon::Configure => Icon::Wrench,
        ScriptIcon::Build => Icon::Hammer,
        ScriptIcon::Debug => Icon::Bug,
        ScriptIcon::Play => Icon::Play,
    }
}

fn running_indicator(running: bool, cx: &App) -> impl IntoElement {
    let color = hsla(palette(cx).terminal_running);
    div().flex_none().children(running.then(|| Icon::Terminal.sized(px(14.)).text_color(color)))
}

/// The run context row (`RunContextSelector` in panel mode, locked): the machine, then the
/// checkout, whose path shows on hover and copies from the context menu.
fn environment_row(ctx: &Ctx, thread: &ThreadShell, project_root: &str, cx: &App) -> AnyElement {
    let app = ctx.state.read(cx);
    let label = app.server_config.config.as_ref().map(|config| config.environment.label.trim().to_owned()).filter(|label| !label.is_empty()).unwrap_or_else(|| "This device".to_owned());
    let worktree = thread.worktree_path.as_deref();
    let path = worktree.unwrap_or(project_root).to_owned();
    let p = palette(cx);
    let workspace_icon = div()
        .id("details-workspace-icon")
        .flex_none()
        .child((if worktree.is_some() { Icon::FolderGit2 } else { Icon::Folder }).sized(px(16.)).text_color(hsla(p.muted_foreground)))
        .tooltip({
            let path: SharedString = path.clone().into();
            move |window, cx| crate::right_panel::tooltip_view(path.clone(), window, cx)
        });
    let state = ctx.state.clone();
    let claim = ctx.row_menu_claim.clone();
    div()
        .w_full()
        .capture_any_mouse_down(move |event, _, _| claim.set(event.button == gpui::MouseButton::Right))
        .context_menu(move |menu, _, _| {
            let (state, path) = (state.clone(), path.clone());
            menu.item(PopupMenuItem::new("Copy full path").icon(menu_icon(Icon::Copy)).on_click(move |_, _, cx| copy_with_toast(path.clone(), "Path copied", &state, cx)))
        })
        .child(Row::new("details-environment", label).icon(Icon::Server).trailing(workspace_icon))
        .into_any_element()
}

/// `OpenInPicker` in panel mode: opens the checkout in the preferred editor; the chevron picks
/// another (the shortcut is `editor.openFavorite`).
fn open_in_row(ctx: &Ctx, cx: &App) -> AnyElement {
    let available = ctx.state.read(cx).server_config.config.as_ref().map(|config| config.available_editors.clone()).unwrap_or_default();
    let preferred = ctx.details.read(cx).preferred_editor(&available);
    let mac = cfg!(target_os = "macos");
    let label = format!("Open in {}", preferred.map_or("editor", |id| editor_label(id, mac)));
    let details = ctx.details.clone();
    let primary = Row::new("details-open-in", label).icon(Icon::SquareArrowOutUpRight).disabled(preferred.is_none()).on_click(move |_, _, cx| {
        if let Some(editor) = preferred {
            details.update(cx, |details, cx| details.open_in_editor(editor, cx));
        }
    });
    let details = ctx.details.clone();
    let shortcut = crate::keymap::label("editor.openFavorite", cx);
    let trigger = MenuTrigger::new("details-open-in-menu", "Choose editor").dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
        let options = editor_options(&available);
        let mut menu = menu.min_w(px(ROW_WIDTH));
        if options.is_empty() {
            return menu.item(PopupMenuItem::new("No installed editors found").disabled(true));
        }
        for id in options {
            let details = details.clone();
            let label = editor_label(id, mac);
            let item = match (Some(id) == preferred, shortcut.clone()) {
                (true, Some(shortcut)) => PopupMenuItem::element(move |_, cx| h_flex().w_full().gap(px(8.)).child(div().flex_1().child(label)).child(menu_shortcut(shortcut.clone(), cx))),
                _ => PopupMenuItem::new(label),
            };
            menu = menu.item(item.on_click(move |_, _, cx| details.update(cx, |details, cx| details.set_last_editor(id, cx))));
        }
        menu
    });
    split_row("details-open-in", primary, trigger, cx).into_any_element()
}

/// `ProjectScriptsControl` in panel mode: runs the preferred script; the chevron lists them all
/// with their shortcuts. A terminal glyph marks scripts still running.
fn scripts_row(ctx: &Ctx, cx: &App) -> Option<AnyElement> {
    let (project_id, scripts) = project_scripts(ctx.state.read(cx))?;
    if scripts.is_empty() {
        return None;
    }
    let git = ctx.git.read(cx);
    let primary = run_button_script(ctx.details.read(cx), &project_id, &scripts).cloned();
    let running: Vec<bool> = scripts.iter().map(|script| git.script_running(&script.id)).collect();
    let menu = {
        let (details, project_id, scripts, running) = (ctx.details.clone(), project_id.clone(), scripts.clone(), running.clone());
        MenuTrigger::new("details-scripts-menu", "Script actions").dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            scripts.iter().zip(running.iter().copied()).fold(menu.min_w(px(ROW_WIDTH)), |menu, (script, running)| {
                let (details, project_id, script) = (details.clone(), project_id.clone(), script.clone());
                let label = menu_label(&script);
                let shortcut = command_for_project_script(&script.id);
                let item = PopupMenuItem::element(move |_, cx| {
                    let shortcut = shortcut.as_deref().and_then(|command| crate::keymap::label(command, cx));
                    h_flex().w_full().gap(px(8.)).child(div().flex_1().min_w_0().truncate().child(label.clone())).child(running_indicator(running, cx)).children(shortcut.map(|shortcut| menu_shortcut(shortcut, cx)))
                });
                menu.item(item.icon(menu_icon(script_icon(script.icon))).on_click(move |_, _, cx| details.update(cx, |details, cx| details.run_script(&project_id, &script, cx))))
            })
        })
    };
    let primary_row = match primary {
        Some(script) => {
            let details = ctx.details.clone();
            let running = git.script_running(&script.id);
            Row::new("details-run-script", script.name.clone())
                .icon(script_icon(script.icon))
                .trailing(running_indicator(running, cx))
                .tooltip(format!("Run {}", script.name))
                .on_click(move |_, _, cx| run_script(&details, &project_id, &script, cx))
        }
        None => Row::new("details-scripts", "Actions").icon(Icon::Wrench),
    };
    Some(split_row("details-scripts", primary_row, menu, cx).into_any_element())
}

fn run_script(details: &Entity<Details>, project_id: &str, script: &ProjectScript, cx: &mut App) {
    details.update(cx, |details, cx| details.run_script(project_id, script, cx));
}

pub fn workspace(ctx: &Ctx, cx: &App) -> Option<AnyElement> {
    let app = ctx.state.read(cx);
    let thread = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id))?;
    let project_root = app.shell.project(&thread.project_id)?.workspace_root.clone();
    let items = &ctx.details.read(cx).items;
    let rows: Vec<AnyElement> = [
        items.shows("environment").then(|| environment_row(ctx, thread, &project_root, cx)),
        items.shows("openIn").then(|| open_in_row(ctx, cx)),
        items.shows("scripts").then(|| scripts_row(ctx, cx)).flatten(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!rows.is_empty()).then(|| section(false, None, cx).child(v_flex().children(rows)).into_any_element())
}

/// The branch row and its picker, and the PR row under it while the checkout is the thread's
/// branch (`BranchToolbarBranchSelector` in panel mode).
fn branch_rows(ctx: &Ctx, thread: &ThreadShell, cx: &App) -> AnyElement {
    let git = ctx.git.read(cx);
    let active = git.active_branch();
    let label = vcs::branch_trigger_label(false, active.as_deref(), None, false);
    let picker = ctx.picker.clone();
    let row = Row::new("details-branch", label)
        .icon(Icon::GitBranch)
        .select()
        .open(picker.read(cx).is_open())
        .disabled(git.branch_pending)
        .on_click(move |_, window, cx| picker.update(cx, |picker, cx| picker.toggle(window, cx)));
    let state = ctx.state.clone();
    let branch_for_menu = active.clone();
    let claim = ctx.row_menu_claim.clone();
    let trigger = div()
        .relative()
        .w_full()
        .capture_any_mouse_down(move |event, _, _| claim.set(event.button == gpui::MouseButton::Right))
        .child(row)
        .child(ctx.picker.clone())
        .context_menu(move |menu, _, _| match branch_for_menu.clone() {
            Some(branch) => {
                let state = state.clone();
                menu.item(PopupMenuItem::new("Copy branch name").icon(menu_icon(Icon::Copy)).on_click(move |_, _, cx| copy_with_toast(branch.clone(), "Branch name copied", &state, cx)))
            }
            None => menu,
        });
    let status = git.status.as_ref();
    let pr = vcs::pr_branch(thread.branch.as_deref(), active.as_deref())
        .filter(|branch| status.and_then(|status| status.local.ref_name.as_deref()) == Some(*branch))
        .and_then(|_| status.and_then(|status| status.remote.pr.clone()));
    let terms = t3_core::git_actions::terminology(status.and_then(|status| status.provider_kind()));
    v_flex()
        .w_full()
        .child(trigger)
        .children(pr.map(|pr| {
            let url = pr.url.clone();
            let state_label = match pr.state.as_str() {
                "merged" => "Merged",
                "closed" => "Closed",
                _ if pr.is_draft == Some(true) => "Draft",
                _ => "Open",
            };
            Row::new("details-pr", format!("#{}: {}", pr.number, pr.title))
                .icon(Icon::GitPullRequest)
                .tooltip(format!("{} #{} - {}: {}", terms.short_label, pr.number, state_label, pr.title))
                .on_click(move |_, _, cx| cx.open_url(&url))
        }))
        .into_any_element()
}

pub fn version_control(ctx: &Ctx, separated: bool, cx: &App) -> Option<AnyElement> {
    let app = ctx.state.read(cx);
    let thread = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id))?;
    let items = &ctx.details.read(cx).items;
    let is_repo = ctx.git.read(cx).status.as_ref().is_none_or(|status| status.local.is_repo);
    let no_runs_yet = thread.latest_run_id.is_none();
    let rows: Vec<AnyElement> = [
        (is_repo && items.shows("branch")).then(|| branch_rows(ctx, thread, cx)),
        items.shows("gitActions").then(|| git::git_actions_row(&ctx.git, &ctx.dialogs, no_runs_yet, cx)).flatten(),
        items.shows("changes").then(|| git::changes_row(&ctx.git, &ctx.state, no_runs_yet, cx)).flatten(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!rows.is_empty()).then(|| section(separated, None, cx).child(v_flex().children(rows)).into_any_element())
}

/// One related thread: opens it (a side chat in its tab), with Merge back on the fork's source
/// and Stop on a subagent or workflow that can still be stopped.
fn relationship_row(ctx: &Ctx, row: &LineageRow, merge: Option<&MergeBack>, cx: &App) -> AnyElement {
    let p = palette(cx);
    let (state, id) = (ctx.state.clone(), row.thread_id.clone());
    let primary = Row::new(SharedString::from(format!("lineage-{}", row.thread_id)), row.title.clone())
        .icon(row.icon)
        .trailing(div().flex_none().text_size(px(11.)).font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(row.status))
        .tooltip(format!("{}\n{} · Open thread", row.title, row.relationship))
        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.open_related_thread(&id, cx)));
    if let (true, Some(merge)) = (row.merge_target, merge) {
        let state = ctx.state.clone();
        let tooltip = match (&merge.target_title, merge.ready) {
            (_, false) => "Complete a run in this fork before merging it back".to_owned(),
            (Some(title), true) => format!("Merge this conversation back into {title}"),
            (None, true) => "Merge this conversation back into its source".to_owned(),
        };
        let button = crate::right_panel::Button::new(SharedString::from(format!("lineage-merge-{}", row.thread_id)))
            .icon(Icon::GitMerge)
            .icon_size(12.)
            .disabled(!merge.ready)
            .tooltip(tooltip)
            .on_click(move |_, _, cx| state.update(cx, |state, cx| state.merge_back_selected(cx)));
        return split_row(format!("lineage-split-{}", row.thread_id), primary, button, cx).into_any_element();
    }
    if let Some(action) = row.stop.clone() {
        let state = ctx.state.clone();
        let label = if matches!(action, StopAction::Workflow { .. }) { "Stop workflow" } else { "Stop subagent" };
        let button = crate::right_panel::Button::new(SharedString::from(format!("lineage-stop-{}", row.thread_id)))
            .icon(Icon::Square)
            .icon_size(12.)
            .tooltip(label)
            .on_click(move |_, _, cx| state.update(cx, |state, cx| state.run_stop_action(&action, cx)));
        return split_row(format!("lineage-split-{}", row.thread_id), primary, button, cx).into_any_element();
    }
    primary.into_any_element()
}

pub fn lineage(ctx: &Ctx, rows: &Relationships, shown: usize, separated: bool, on_more: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> Option<AnyElement> {
    if !ctx.details.read(cx).items.shows("lineage") || rows.lineage.is_empty() {
        return None;
    }
    let hidden = rows.lineage.len().saturating_sub(shown);
    let rendered: Vec<AnyElement> = rows.lineage.iter().take(shown).map(|row| relationship_row(ctx, row, rows.merge.as_ref(), cx)).collect();
    let list = v_flex().id("details-lineage").max_h(px(216.)).overflow_y_scroll().children(rendered);
    let more = (hidden > 0).then(|| Row::new("lineage-more", format!("Show {} more", hidden.min(LINEAGE_PAGE))).icon(Icon::Plus).on_click(on_more));
    Some(section(separated, Some("Lineage".into()), cx).child(list).children(more).into_any_element())
}

/// The thread's side chats, which open in their right panel tab.
pub fn side_chats(ctx: &Ctx, rows: &Relationships, separated: bool, cx: &App) -> Option<AnyElement> {
    if !ctx.details.read(cx).items.shows("sideChats") || rows.side_chats.is_empty() {
        return None;
    }
    let rendered: Vec<AnyElement> = rows.side_chats.iter().map(|row| relationship_row(ctx, row, None, cx)).collect();
    let list = v_flex().id("details-side-chats").max_h(px(216.)).overflow_y_scroll().children(rendered);
    Some(section(separated, Some("Side chats".into()), cx).child(list).into_any_element())
}
