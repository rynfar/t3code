//! The card's sections (`ThreadDetailsPanel.tsx`): Workspace (environment and worktree, open in
//! editor, project scripts), Version Control (branch and pull request, Git actions, changes)
//! Lineage and Side chats (`ThreadRelationshipsControl.tsx`, rows from `crate::lineage`).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{AnyElement, App, ClipboardItem, Entity, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::lineage::{AgentTiming, PhaseState, WorkflowMember, relationship_status_label, subagent_display_title};
use t3_core::project_scripts::{ProjectScript, ScriptIcon, command_for_project_script, menu_label};
use t3_core::thread_details::{
    CardDensity, RemoteOpenState, effective_open_in_editors, item_visible_at_density, open_in_unavailable_reason,
};
use t3_core::vcs::{self, editor_label, editor_options};

use super::controls::{MenuTrigger, ROW_WIDTH, Row, menu_icon, menu_shortcut, section, section_with_action, section_with_options, split_row};
use super::{Details, project_scripts, run_button_script};
use crate::app_state::AppState;
use crate::composer::Composer;
use crate::i18n::{t, t_fmt};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use t3_core::i18n::IcuArg;
use crate::lineage::{DelegatedFromRow, LineageRow, MergeBack, Relationships, RemoteSubagentRow, StopAction, elapsed::agent_elapsed_element};
use crate::git::{self, BranchPicker, GitDialogs, GitStore};
use crate::icons::{Icon, editor_logo};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// `THREAD_LINEAGE_INITIAL_COUNT` and `THREAD_LINEAGE_PAGE_COUNT`.
pub const LINEAGE_INITIAL: usize = 6;
pub const LINEAGE_PAGE: usize = 12;

pub struct Ctx {
    /// Set when a right-click lands on a row with its own menu, so the card's menu stays shut.
    pub row_menu_claim: Rc<Cell<bool>>,
    pub state: Entity<AppState>,
    pub composer: Option<Entity<Composer>>,
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
fn environment_row(ctx: &Ctx, worktree: Option<&str>, project_root: &str, cx: &App) -> AnyElement {
    let app = ctx.state.read(cx);
    let label = app.active_open_in_config()
        .map(|config| config.environment.label.trim())
        .filter(|label| !label.is_empty())
        .or_else(|| app.active_environment().map(|environment| environment.label.as_str()))
        .unwrap_or_else(|| t(cx, "This device")).to_owned();
    let machine = app.environments.active.as_deref().or_else(|| app.environment())
        .map_or(Icon::Server, |id| crate::context_chip::environment_machine_icon(app, id));
    let draft = ctx.composer.as_ref().filter(|_| app.selected_thread.is_none() && app.draft_project.is_some());
    let workspace_glyph = if worktree.is_some() { Icon::FolderGit2 } else { Icon::Folder };
    let path = worktree.unwrap_or(project_root).to_owned();
    let p = palette(cx);
    let workspace_icon = div()
        .id("details-workspace-icon")
        .flex_none()
        .child(workspace_glyph.sized(px(16.)).text_color(hsla(p.muted_foreground)))
        .tooltip({
            let path: SharedString = path.clone().into();
            move |window, cx| crate::right_panel::tooltip_view(path.clone(), window, cx)
        });
    let environment = if let Some(draft) = draft {
        let host = draft.clone();
        let workspace = draft.clone();
        split_row(
            "details-run-context",
            Row::new("details-environment", label).icon(machine).select()
                .tooltip(t(cx, "Choose environment"))
                .on_click(move |_, window, cx| host.update(cx, |composer, cx| composer.open_host(window, cx))),
            Button::new("details-workspace", ButtonVariant::Ghost, ButtonSize::IconSm)
                .icon(workspace_glyph).tooltip(t(cx, "Choose workspace"))
                .on_click(move |_, window, cx| workspace.update(cx, |composer, cx| composer.open_workspace(window, cx))),
            cx,
        ).into_any_element()
    } else {
        Row::new("details-environment", label).icon(machine).trailing(workspace_icon).into_any_element()
    };
    let state = ctx.state.clone();
    let claim = ctx.row_menu_claim.clone();
    div()
        .w_full()
        .capture_any_mouse_down(move |event, _, _| claim.set(event.button == gpui::MouseButton::Right))
        .context_menu(move |menu, _, _| {
            let (state, path) = (state.clone(), path.clone());
            menu.item(PopupMenuItem::new("Copy full path").icon(menu_icon(Icon::Copy)).on_click(move |_, _, cx| copy_with_toast(path.clone(), "Path copied", &state, cx)))
        })
        .child(environment)
        .into_any_element()
}

/// `OpenInPicker` in panel mode: opens the checkout in the preferred editor; the chevron picks
/// another (the shortcut is `editor.openFavorite`).
fn open_in_row(ctx: &Ctx, cx: &App) -> AnyElement {
    let app = ctx.state.read(cx);
    let active_config = app.active_open_in_config();
    let available = active_config.map(|config| config.available_editors.clone()).unwrap_or_default();
    let env_label = active_config
        .map(|config| config.environment.label.trim().to_owned())
        .filter(|label| !label.is_empty())
        .or_else(|| app.active_environment().map(|env| env.label.clone()))
        .unwrap_or_else(|| "this machine".to_owned());
    let remote_state = app.active_remote_open_state();
    let can_operate = ctx.git.read(cx).can_operate_thread();
    let is_host_editor_denied = matches!(remote_state, RemoteOpenState::LocalExec) && !can_operate;
    let can_open_editor = !matches!(remote_state, RemoteOpenState::RemoteUnavailable) && !is_host_editor_denied;
    let remote_capable = ctx.details.read(cx).remote_capable_editors();
    let effective = effective_open_in_editors(&remote_state, &available, &remote_capable);
    let unavailable_reason = open_in_unavailable_reason(&remote_state, effective.len(), &env_label);
    let preferred = ctx.details.read(cx).preferred_editor(&effective);
    let platform = std::env::consts::OS;
    let label = format!("Open in {}", preferred.map_or("editor", |id| editor_label(id, platform == "macos")));
    let tooltip = unavailable_reason.clone().unwrap_or_else(|| label.clone());
    let details = ctx.details.clone();
    let state = ctx.state.clone();
    let render_mark = move |id, cx: &App| {
        let p = palette(cx);
        editor_logo(id, platform).map_or_else(
            || Icon::Folder.sized(px(16.)).text_color(hsla(p.muted_foreground)).into_any_element(),
            |logo| logo.render(px(16.), hsla(p.foreground), cx.theme().is_dark()),
        )
    };
    let mark = preferred.map(|id| render_mark(id, cx));
    let primary = Row::new("details-open-in", label)
        .when_none(&mark, |row| row.icon(Icon::SquareArrowOutUpRight))
        .when_some(mark, Row::leading)
        .tooltip(tooltip)
        .disabled(preferred.is_none() || !can_open_editor)
        .on_click(move |_, _, cx| {
            if let Some(editor) = preferred {
                details.update(cx, |details, cx| details.open_in_editor(&state, editor, cx));
            }
        });
    let details = ctx.details.clone();
    let shortcut = crate::keymap::label("editor.openFavorite", cx);
    let trigger = MenuTrigger::new("details-open-in-menu", "Choose editor").disabled(is_host_editor_denied).dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
        let mut menu = menu.min_w(px(ROW_WIDTH));
        if let Some(reason) = unavailable_reason.clone() {
            return menu.item(PopupMenuItem::new(reason).disabled(true));
        }
        let options = editor_options(&effective);
        if options.is_empty() {
            return menu.item(PopupMenuItem::new("No installed editors found").disabled(true));
        }
        for id in options {
            let details = details.clone();
            let label = editor_label(id, platform == "macos");
            let shortcut = shortcut.as_ref().filter(|_| Some(id) == preferred).cloned();
            let item = PopupMenuItem::element(move |_, cx| {
                h_flex()
                    .w_full()
                    .gap(px(8.))
                    .child(render_mark(id, cx))
                    .child(div().id(label).aria_label(label).flex_1().child(label))
                    .children(shortcut.as_ref().map(|shortcut| menu_shortcut(shortcut.clone(), cx)))
            });
            menu = menu.item(item.disabled(!can_open_editor).on_click(move |_, _, cx| details.update(cx, |details, cx| details.set_last_editor(id, cx))));
        }
        menu
    });
    split_row("details-open-in", primary, trigger, cx).into_any_element()
}

/// `RUN_SCRIPT_UNAVAILABLE`: why Run is off without terminal access.
const RUN_SCRIPT_UNAVAILABLE: &str = "Pair this client again with permission to run terminal commands.";
const ADD_PROJECT_SCRIPT: &str = "Add project script";

/// One script in the chevron's menu: run it, or edit it from the gear beside its shortcut.
fn script_menu_item(details: &Entity<Details>, menu: gpui::WeakEntity<gpui_component::menu::PopupMenu>, project_id: &str, script: &ProjectScript, running: bool, can_run: bool) -> PopupMenuItem {
    let label = menu_label(script);
    let shortcut = command_for_project_script(&script.id);
    let edit_id = SharedString::from(format!("details-script-edit-{}", script.id));
    let edit_label: SharedString = format!("Edit {}", script.name).into();
    let (edit_project, edit_script) = (project_id.to_owned(), script.id.clone());
    let item = PopupMenuItem::element(move |_, cx| {
        let shortcut = shortcut.as_deref().and_then(|command| crate::keymap::label(command, cx));
        let (menu, project_id, script_id, tooltip) = (menu.clone(), edit_project.clone(), edit_script.clone(), edit_label.clone());
        let edit_debug_id = edit_id.clone();
        let edit = div()
            .id(edit_id.clone())
            .debug_selector(move || edit_debug_id.to_string())
            .flex_none()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .text_color(hsla(palette(cx).muted_foreground))
            .hover(|style| style.text_color(hsla(palette(cx).foreground)))
            .child(Icon::Settings.sized(px(14.)))
            .tooltip(move |window, cx| crate::right_panel::tooltip_view(tooltip.clone(), window, cx))
            // The gear edits instead of running, and closes the menu itself.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                let _ = menu.update(cx, |_, cx| cx.emit(gpui::DismissEvent));
                git::open_script_editor(&project_id, Some(&script_id), cx);
            });
        h_flex()
            .w_full()
            .gap(px(8.))
            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
            .child(running_indicator(running, cx))
            .children(shortcut.map(|shortcut| menu_shortcut(shortcut, cx)))
            .child(edit)
    });
    let (details, project_id, script) = (details.clone(), project_id.to_owned(), script.clone());
    item.icon(menu_icon(script_icon(script.icon))).disabled(!can_run).on_click(move |_, _, cx| details.update(cx, |details, cx| details.run_script(&project_id, &script, cx)))
}

/// `ProjectScriptsControl` in panel mode: runs the preferred script; the chevron lists them all
/// with their shortcuts, an edit button each, quick-imports from `t3.json`, and Add project script.
/// With no scripts or importable `t3.json` entries, the row adds one. A terminal glyph marks
/// scripts still running.
fn scripts_row(ctx: &Ctx, cx: &App) -> Option<AnyElement> {
    let (project_id, scripts) = project_scripts(ctx.state.read(cx))?;
    let git = ctx.git.read(cx);
    let importable: Vec<t3_core::settings::project_actions::FileScript> =
        t3_core::settings::project_actions::importable(git.file_scripts(), &scripts).into_iter().cloned().collect();
    if scripts.is_empty() && importable.is_empty() {
        let row = Row::new("details-add-script", ADD_PROJECT_SCRIPT).icon(Icon::Plus).tooltip(ADD_PROJECT_SCRIPT).on_click(move |_, _, cx| git::open_script_editor(&project_id, None, cx));
        return Some(row.into_any_element());
    }
    let can_run = git.can_run_scripts();
    let can_import = git.can_write_settings();
    let primary = run_button_script(ctx.details.read(cx), &project_id, &scripts).cloned();
    let running: Vec<bool> = scripts.iter().map(|script| git.script_running(&script.id)).collect();
    let build_menu = {
        let (details, git_entity, project_id, scripts, running, importable) =
            (ctx.details.clone(), ctx.git.clone(), project_id.clone(), scripts.clone(), running.clone(), importable.clone());
        move |menu: gpui_component::menu::PopupMenu, _: &mut Window, cx: &mut gpui::Context<gpui_component::menu::PopupMenu>| {
            let handle = cx.entity().downgrade();
            let mut menu = scripts.iter().zip(running.iter().copied()).fold(menu.min_w(px(ROW_WIDTH)), |menu, (script, running)| {
                menu.item(script_menu_item(&details, handle.clone(), &project_id, script, running, can_run))
            });
            if !importable.is_empty() {
                if !scripts.is_empty() {
                    menu = menu.separator();
                }
                menu = menu.item(
                    PopupMenuItem::element(|_, cx| {
                        div().text_xs().font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(palette(cx).muted_foreground)).child("From t3.json")
                    })
                    .disabled(true),
                );
                for file_script in &importable {
                    let name = file_script.name.clone();
                    let icon = script_icon(file_script.icon.unwrap_or_default());
                    let git_entity = git_entity.clone();
                    let project_id = project_id.clone();
                    let pick = file_script.clone();
                    let item = PopupMenuItem::element(move |_, cx| {
                        h_flex()
                            .w_full()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                            .child(Icon::Download.sized(px(14.)).text_color(hsla(palette(cx).muted_foreground)))
                    })
                    .icon(menu_icon(icon))
                    .disabled(!can_import)
                    .on_click(move |_, _, cx| {
                        git_entity.update(cx, |git, cx| git.import_file_script(&project_id, pick.clone(), cx));
                    });
                    menu = menu.item(item);
                }
            }
            let project_id = project_id.clone();
            menu.item(PopupMenuItem::new(ADD_PROJECT_SCRIPT).icon(menu_icon(Icon::Plus)).on_click(move |_, _, cx| git::open_script_editor(&project_id, None, cx)))
        }
    };
    let Some(script) = primary else {
        return Some(Row::new("details-scripts", t(cx, "Actions")).icon(Icon::Wrench)
            .select().opens_menu().tooltip(t(cx, "Project actions"))
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, build_menu).into_any_element());
    };
    let menu = MenuTrigger::new("details-scripts-menu", t(cx, "Script actions"))
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, build_menu);
    let primary_row = {
        let details = ctx.details.clone();
            let running = git.script_running(&script.id);
            Row::new("details-run-script", script.name.clone())
                .icon(script_icon(script.icon))
                .disabled(!can_run)
                .trailing(running_indicator(running, cx))
                .tooltip(if can_run { format!("Run {}", script.name) } else { RUN_SCRIPT_UNAVAILABLE.to_owned() })
                .on_click(move |_, _, cx| run_script(&details, &project_id, &script, cx))
    };
    Some(split_row("details-scripts", primary_row, menu, cx).into_any_element())
}

fn run_script(details: &Entity<Details>, project_id: &str, script: &ProjectScript, cx: &mut App) {
    details.update(cx, |details, cx| details.run_script(project_id, script, cx));
}

pub fn workspace(ctx: &Ctx, density: CardDensity, cx: &App) -> Option<AnyElement> {
    let app = ctx.state.read(cx);
    let (project_id, worktree) = match app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) {
        Some(thread) => (thread.project_id.as_str(), thread.worktree_path.as_deref()),
        None => (app.draft_project.as_deref()?, None),
    };
    let project_root = app.shell.project(project_id)?.workspace_root.clone();
    let details = ctx.details.read(cx);
    let rows: Vec<AnyElement> = [
        (details.shows("environment") && item_visible_at_density("environment", density)).then(|| environment_row(ctx, worktree, &project_root, cx)),
        (details.shows("openIn") && item_visible_at_density("openIn", density) && app.should_show_open_in_picker(true)).then(|| open_in_row(ctx, cx)),
        (details.shows("scripts") && item_visible_at_density("scripts", density)).then(|| scripts_row(ctx, cx)).flatten(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!rows.is_empty()).then(|| section_with_options(false, None, density.is_essential(), cx).child(v_flex().children(rows)).into_any_element())
}

pub(super) fn open_thread_pr_url(state: &Entity<AppState>, url: &str, cx: &mut App) {
    if crate::settings::client_settings(cx).pull_request_links_open_in_app() {
        crate::pull_requests::open_context_link(state, url, cx);
    } else {
        cx.open_url(url);
    }
}

/// `ThreadDetailsThreadPrRows`: linked PR rows shown when a thread has no git repository
/// (e.g. a scratch/`No project` thread or non-git folder).
fn thread_pr_rows(ctx: &Ctx, thread: &t3_core::shell::ThreadShell, _cx: &App) -> Option<AnyElement> {
    use t3_core::pull_requests::detail::StateKind;
    use t3_core::pull_requests::link::visible_links;
    use t3_core::pull_requests::thread_badge::{linked_snapshot_status, pr_status_indicator, resolve_thread_current_pull_request_link};

    let visible = visible_links(&thread.pull_requests);
    if visible.is_empty() {
        return None;
    }
    let current = resolve_thread_current_pull_request_link(&thread.pull_requests);
    let mut ordered = Vec::with_capacity(visible.len());
    if let Some(current) = current {
        ordered.push(current);
    }
    for link in visible {
        if current.is_none_or(|c| c.number != link.number || !c.repository.eq_ignore_ascii_case(&link.repository) || !c.host.eq_ignore_ascii_case(&link.host)) {
            ordered.push(link);
        }
    }
    let rows: Vec<AnyElement> = ordered
        .into_iter()
        .enumerate()
        .map(|(index, link)| {
            let status = linked_snapshot_status(link);
            let title = status
                .as_ref()
                .map(|s| s.pr.title.trim())
                .filter(|t| !t.is_empty())
                .map(|t| format!("#{}: {t}", link.number))
                .unwrap_or_else(|| format!("#{}", link.number));
            let tooltip = status
                .as_ref()
                .map(|s| pr_status_indicator(&s.pr, Some(s.provider)).label)
                .unwrap_or_else(|| link.url.clone());
            let icon = match status.as_ref().map(|s| StateKind::of(s.pr.state, s.pr.is_draft)) {
                Some(StateKind::Merged) => Icon::GitMerge,
                Some(StateKind::Closed) => Icon::GitPullRequestClosed,
                Some(StateKind::Draft) => Icon::GitPullRequestDraft,
                _ => Icon::GitPullRequest,
            };
            let state = ctx.state.clone();
            let url = link.url.clone();
            Row::new(SharedString::from(format!("details-linked-pr-{index}")), title)
                .icon(icon)
                .tooltip(tooltip)
                .on_click(move |_, _, cx| open_thread_pr_url(&state, &url, cx))
                .into_any_element()
        })
        .collect();
    Some(v_flex().w_full().children(rows).into_any_element())
}

/// The branch row and its picker, and the PR row under it while the checkout is the thread's
/// branch (`BranchToolbarBranchSelector` in panel mode).
fn branch_row(ctx: &Ctx, cx: &App) -> AnyElement {
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
    div()
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
        })
        .into_any_element()
}

pub fn version_control(ctx: &Ctx, separated: bool, density: CardDensity, pull_requests: Option<AnyElement>, comparison: &super::changes::Comparison, cx: &App) -> Option<AnyElement> {
    let app = ctx.state.read(cx);
    let thread = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id));
    let project_id = thread.map(|thread| thread.project_id.as_str()).or(app.draft_project.as_deref())?;
    let project = app.shell.project(project_id)?;
    // Scratch (`No project`) threads never offer git actions / Initialize Git, nor before config loads.
    let offers_git = app.server_config.config.is_some() && !app.is_scratch_project(project_id) && !project.workspace_root.is_empty();
    let details = ctx.details.read(cx);
    let is_repo = offers_git && ctx.git.read(cx).status.as_ref().is_none_or(|status| status.local.is_repo);
    let no_runs_yet = thread.is_none_or(|thread| thread.latest_run_id.is_none());
    let compact_actions = !density.is_full();
    let show_branch = offers_git && is_repo && details.shows("branch") && item_visible_at_density("branch", density);
    let show_pull_requests = !(offers_git && is_repo) && details.shows("branch") && item_visible_at_density("branch", density);
    let show_git_actions = offers_git && details.shows("gitActions") && item_visible_at_density("gitActions", density);
    let show_changes = offers_git && is_repo && details.shows("changes") && item_visible_at_density("changes", density);
    let rows: Vec<AnyElement> = [
        show_branch.then(|| branch_row(ctx, cx)),
        pull_requests.or_else(|| show_pull_requests.then(|| thread.and_then(|t| thread_pr_rows(ctx, t, cx))).flatten()),
        show_git_actions.then(|| git::git_actions_row(&ctx.git, &ctx.dialogs, no_runs_yet, compact_actions, cx)).flatten(),
        show_changes.then(|| git::changes_row(&ctx.git, &ctx.state, no_runs_yet, comparison, cx)).flatten(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!rows.is_empty()).then(|| section_with_options(separated && density.is_full(), None, density.is_essential(), cx).child(v_flex().children(rows)).into_any_element())
}

fn lineage_icon(state: &AppState, environment: Option<&str>, thread_id: &str, fallback: Icon, cx: &App) -> AnyElement {
    let fleet = environment.and_then(|environment| state.fleet.environments.iter().find(|entry| {
        entry.summary.owner.canonical_id.as_deref() == Some(environment) || entry.summary.owner.saved_id == environment
    }));
    let provider = if let Some(entry) = fleet {
        entry.summary.threads.iter().find(|thread| thread.thread.id == thread_id)
            .and_then(|thread| entry.config.config.as_ref()?.provider(&thread.thread.model_selection.instance_id))
    } else if environment.is_none() || state.server_config.config.as_ref().is_some_and(|config| {
        Some(config.environment.environment_id.as_str()) == environment
    }) {
        state.thread_shell(thread_id).and_then(|thread| {
            state.server_config.config.as_ref()?.provider(&thread.model_selection.instance_id)
        })
    } else {
        None
    };
    let ink = hsla(palette(cx).muted_foreground);
    match provider {
        Some(provider) if crate::icons::provider_logo(&provider.driver).is_some() => crate::usage::brand_mark(&provider.driver, 16., ink, cx),
        _ => fallback.render(px(16.), ink).into_any_element(),
    }
}

fn stop_button(ctx: &Ctx, row: &LineageRow, action: StopAction, window: &mut Window, cx: &mut App) -> AnyElement {
    let app = ctx.state.read(cx);
    let (connection, selected) = (app.connection_id(), app.selected_thread.clone());
    let group = SharedString::from(format!("lineage-split-{}-split", row.thread_id));
    let workflow = matches!(action, StopAction::Workflow { .. });
    let title = if workflow { "Stop entire workflow?" } else { "Stop subagent?" };
    let label = t(cx, if workflow { "Stop workflow" } else { "Stop subagent" });
    let name = row.title.clone();
    let button_id = gpui::ElementId::Name(SharedString::from(format!("lineage-stop-{}", row.thread_id)));
    let focus = crate::settings::controls::focus_handle(&button_id, window, cx);
    let button = Button::new(button_id, ButtonVariant::GhostDestructive, ButtonSize::IconMicro)
        .icon(Icon::Square).tooltip(label).focus_handle(focus.clone())
        .on_click(move |_, _, cx| {
            let (action, selected) = (action.clone(), selected.clone());
            let message = t_fmt(cx, "Stop {name}?", &[("name", IcuArg::Text(name.as_str().into()))]);
            crate::dialog::ask_with_options(
                &format!("{}\n{message}", t(cx, title)),
                t(cx, "Stop"), crate::dialog::ConfirmVariant::Destructive, crate::dialog::InitialFocus::Cancel,
                move |app, cx| {
                    if app.connection_id() == connection && app.selected_thread == selected {
                        app.run_stop_action(&action, cx);
                    }
                },
                None, cx,
            );
        });
    div().id("lineage-stop-reveal").track_focus(&focus).opacity(0.)
        .group_hover(group, |style| style.opacity(1.))
        .focus(|style| style.opacity(1.))
        .child(button)
        .into_any_element()
}

/// One related thread: opens it (a side chat in its tab), with Merge back on the fork's source
/// and Stop on a subagent or workflow that can still be stopped.
fn relationship_row(ctx: &Ctx, row: &LineageRow, merge: Option<&MergeBack>, window: &mut Window, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let (state, id) = (ctx.state.clone(), row.thread_id.clone());
    let status_color = if row.failed { hsla(p.destructive) } else { hsla(p.muted_foreground) };
    let trailing_label: AnyElement = if !row.failed && (row.timing.is_some() || row.workflow.is_some()) {
        match row.timing {
            Some(timing) => div()
                .flex_none()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(agent_elapsed_element(SharedString::from(format!("lineage-elapsed:{}", row.thread_id)), timing, true, window, cx))
                .into_any_element(),
            None => div().flex_none().into_any_element(),
        }
    } else if !row.merge_target && row.workflow.is_none() {
        div().flex_none().text_size(px(11.)).font_weight(gpui::FontWeight::MEDIUM).text_color(status_color).child(row.status).into_any_element()
    } else {
        div().flex_none().into_any_element()
    };
    let primary = Row::new(SharedString::from(format!("lineage-{}", row.thread_id)), row.title.clone())
        .leading(lineage_icon(ctx.state.read(cx), None, &row.thread_id, row.icon, cx))
        .trailing(trailing_label)
        .tooltip(format!("{}\n{} · {}\n{}", row.title, t(cx, row.relationship), t(cx, row.status), t(cx, "Open thread")))
        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.open_related_thread(&id, cx)));
    if let Some(workflow) = &row.workflow {
        let expanded = window.use_keyed_state(SharedString::from(format!("details-wf-open:{}", row.thread_id)), cx, |_, _| false);
        let is_expanded = *expanded.read(cx);
        let stop_btn = row.stop.clone().map(|action| stop_button(ctx, row, action, window, cx));
        let toggle = crate::right_panel::Button::new(SharedString::from(format!("lineage-wf-toggle-{}", row.thread_id)))
            .icon(if is_expanded { Icon::ChevronDown } else { Icon::ChevronRight })
            .icon_size(12.)
            .tooltip(if is_expanded { format!("Collapse {}", row.title) } else { format!("Expand {}", row.title) })
            .on_click({
                let expanded = expanded.clone();
                move |_, _, cx| expanded.update(cx, |open, cx| { *open = !*open; cx.notify(); })
            });
        let trailing = h_flex().flex_none().items_center().gap(px(2.)).children(stop_btn).child(toggle);
        let header = split_row(format!("lineage-split-{}", row.thread_id), primary, trailing, cx).into_any_element();
        if !is_expanded {
            return header.into_any_element();
        }
        let mut children: Vec<AnyElement> = Vec::new();
        let phases: Vec<_> = workflow.phases.iter().filter(|phase| !phase.members.is_empty()).collect();
        for phase in &phases {
            let phase_open: Entity<Option<bool>> = window.use_keyed_state(
                SharedString::from(format!("details-wf-phase:{}:{}", row.thread_id, phase.index)),
                cx,
                |_, _| None,
            );
            let is_phase_open = phase_open.read(cx).unwrap_or(phase.state == PhaseState::Running);
            let phase_failed = phase.state != PhaseState::Running && phase.members.iter().any(|m| m.status == "failed");
            let count_text = format!(
                "{}/{} {}",
                phase.settled_count,
                phase.members.len(),
                if phase.members.len() == 1 { "agent" } else { "agents" }
            );
            let count_color = if phase.state == PhaseState::Running { hsla(p.foreground) } else { hsla(p.muted_foreground) };
            let phase_elapsed = AgentTiming::from_phase(phase).map(|timing| {
                div().text_color(hsla(p.muted_foreground)).child(agent_elapsed_element(
                    SharedString::from(format!("wf-phase-elapsed:{}:{}", row.thread_id, phase.index)),
                    timing,
                    false,
                    window,
                    cx,
                ))
            });
            let accessory = h_flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.))
                .when(phase_failed, |this| {
                    this.child(div().font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.destructive)).child("Failed"))
                })
                .child(div().text_color(count_color).child(count_text))
                .children(phase_elapsed);
            let phase_row = Row::new(SharedString::from(format!("wf-phase-{}-{}", row.thread_id, phase.index)), phase.title.clone())
                .icon(if is_phase_open { Icon::ChevronDown } else { Icon::ChevronRight })
                .trailing(accessory)
                .on_click({
                    let phase_open = phase_open.clone();
                    move |_, _, cx| phase_open.update(cx, |open, cx| { *open = Some(!is_phase_open); cx.notify(); })
                })
                .into_any_element();
            children.push(phase_row);
            if is_phase_open {
                for member in &phase.members {
                    children.push(workflow_member_row(&ctx.state, &row.thread_id, &format!("p{}", phase.index), member, window, cx));
                }
            }
        }
        for member in &workflow.unphased {
            children.push(workflow_member_row(&ctx.state, &row.thread_id, "u", member, window, cx));
        }
        if phases.is_empty() && workflow.unphased.is_empty() {
            children.push(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(hsla(p.muted_foreground).opacity(0.7))
                    .child("No agents yet")
                    .into_any_element(),
            );
        }
        return v_flex().w_full().child(header).child(v_flex().pl(px(16.)).children(children)).into_any_element();
    }
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
        return split_row(format!("lineage-split-{}", row.thread_id), primary, stop_button(ctx, row, action, window, cx), cx).into_any_element();
    }
    primary.into_any_element()
}

fn workflow_member_row(
    state: &Entity<AppState>,
    coordinator_id: &str,
    scope: &str,
    member: &WorkflowMember,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let p = palette(cx);
    let failed = member.status == "failed";
    let trailing: AnyElement = match AgentTiming::from_member(member) {
        Some(timing) => div()
            .flex_none()
            .text_size(px(11.))
            .text_color(hsla(p.muted_foreground))
            .child(agent_elapsed_element(
                SharedString::from(format!("wf-member-elapsed:{coordinator_id}:{scope}:{}", member.index)),
                timing,
                false,
                window,
                cx,
            ))
            .into_any_element(),
        None if failed => div()
            .flex_none()
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(hsla(p.destructive))
            .child(relationship_status_label(Some(&member.status)))
            .into_any_element(),
        None => div().flex_none().into_any_element(),
    };
    let mut row = Row::new(
        SharedString::from(format!("wf-member-{coordinator_id}-{scope}-{}", member.index)),
        subagent_display_title(&member.label),
    )
    .icon(Icon::Bot)
    .disabled(member.child_thread_id.is_none())
    .trailing(trailing);
    if let Some(child_id) = member.child_thread_id.clone() {
        let state = state.clone();
        row = row.on_click(move |_, _, cx| state.update(cx, |state, cx| state.open_related_thread(&child_id, cx)));
    }
    row.into_any_element()
}

fn delegated_from_row(ctx: &Ctx, item: &DelegatedFromRow, cx: &App) -> AnyElement {
    let p = palette(cx);
    let mut row = Row::new("lineage-delegated-from", item.title.clone()).leading(lineage_icon(ctx.state.read(cx), Some(&item.environment_id), &item.thread_id, Icon::CornerLeftUp, cx)).disabled(!item.connected);
    if let Some(label) = &item.env_label {
        row = row.trailing(div().flex_none().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(format!("on {label}")));
    }
    let hint = if item.connected {
        "Open parent agent".to_owned()
    } else {
        format!("Delegated from {}. Connect to it to open the parent thread.", item.env_label.as_deref().unwrap_or("a linked environment"))
    };
    row = row.tooltip(format!("{}\n{hint}", item.title));
    if item.connected {
        let (state, env_id, thread_id) = (ctx.state.clone(), item.environment_id.clone(), item.thread_id.clone());
        row = row.on_click(move |_, _, cx| state.update(cx, |state, cx| state.open_delegated_parent(&env_id, &thread_id, cx)));
    }
    row.into_any_element()
}

fn remote_subagent_row(ctx: &Ctx, item: &RemoteSubagentRow, cx: &App) -> AnyElement {
    let p = palette(cx);
    let color = if item.failed { hsla(p.destructive) } else { hsla(p.muted_foreground) };
    let hint = if item.connected {
        format!("Open on {}", item.env_label)
    } else {
        format!("Runs on {}. Connect to it to open the thread there.", item.env_label)
    };
    let mut row = Row::new(SharedString::from(format!("lineage-remote-{}", item.id)), item.title.clone())
        .leading(lineage_icon(ctx.state.read(cx), Some(&item.environment_id), &item.thread_id, Icon::Bot, cx))
        .disabled(!item.connected)
        .trailing(div().flex_none().text_size(px(11.)).text_color(color).child(t(cx, item.status)))
        .tooltip(format!("{}\n{} · {}\n{hint}", item.title, item.env_label, t(cx, item.status)));
    if item.connected {
        let (state, env_id, thread_id) = (ctx.state.clone(), item.environment_id.clone(), item.thread_id.clone());
        row = row.on_click(move |_, _, cx| state.update(cx, |state, cx| state.open_linked_thread(&env_id, &thread_id, cx)));
    }
    row.into_any_element()
}

pub fn lineage(
    ctx: &Ctx,
    rows: &Relationships,
    thread_id: Option<&str>,
    separated: bool,
    density: CardDensity,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    if !ctx.details.read(cx).shows("lineage") || !item_visible_at_density("lineage", density) || !rows.has_lineage() {
        return None;
    }
    let key_thread = thread_id.unwrap_or("none");
    let shown_state = window.use_keyed_state(SharedString::from(format!("details-lineage-shown:{key_thread}")), cx, |_, _| LINEAGE_INITIAL);
    let shown = *shown_state.read(cx);
    let expanded_rows: Vec<&LineageRow> = rows.related.iter().chain(&rows.active).collect();
    let hidden = expanded_rows.len().saturating_sub(shown);
    let mut rendered: Vec<AnyElement> = rows.delegated_from.iter().map(|item| delegated_from_row(ctx, item, cx)).collect();
    rendered.extend(expanded_rows.iter().take(shown).map(|row| relationship_row(ctx, row, rows.merge.as_ref(), window, cx)));
    let more = (hidden > 0).then(|| {
        Row::new("lineage-more", format!("Show {} more", hidden.min(LINEAGE_PAGE))).icon(Icon::Plus).on_click({
            let shown_state = shown_state.clone();
            move |_, _, cx| shown_state.update(cx, |count, cx| { *count += LINEAGE_PAGE; cx.notify(); })
        })
    });
    let (remote_active, remote_previous): (Vec<_>, Vec<_>) = rows.remote_subagents.iter().partition(|item| {
        matches!(item.status, "Starting" | "Running" | "Queued" | "Waiting")
    });
    let previous_count = rows.previous.len() + remote_previous.len();
    let previous_block = (previous_count > 0).then(|| {
        let prev_open = window.use_keyed_state(SharedString::from(format!("details-prev-open:{key_thread}")), cx, |_, _| false);
        let prev_shown = window.use_keyed_state(SharedString::from(format!("details-prev-shown:{key_thread}")), cx, |_, _| LINEAGE_INITIAL);
        let is_open = *prev_open.read(cx);
        let shown_prev = *prev_shown.read(cx);
        let failed_count = rows.previous.iter().filter(|row| row.failed).count()
            + remote_previous.iter().filter(|row| row.failed).count();
        let p = palette(cx);
        let trailing = h_flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .when(failed_count > 0, |this| {
                this.child(div().text_size(px(11.)).font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.destructive)).child(format!("{failed_count} failed")))
            })
            .when(!is_open, |this| {
                this.child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(previous_count.to_string()))
            })
            .child((if is_open { Icon::ChevronDown } else { Icon::ChevronRight }).sized(px(14.)).text_color(hsla(p.muted_foreground)));
        let toggle = Row::new("lineage-previous-toggle", "Previous agents")
            .icon(Icon::Bot)
            .trailing(trailing)
            .on_click({
                let prev_open = prev_open.clone();
                move |_, _, cx| prev_open.update(cx, |open, cx| { *open = !*open; cx.notify(); })
            });
        let mut group = v_flex().w_full().child(toggle);
        if is_open {
            let prev_hidden = previous_count.saturating_sub(shown_prev);
            let mut prev_items: Vec<AnyElement> =
                rows.previous.iter().take(shown_prev).map(|row| relationship_row(ctx, row, rows.merge.as_ref(), window, cx)).collect();
            prev_items.extend(remote_previous.iter().take(shown_prev.saturating_sub(rows.previous.len())).map(|item| remote_subagent_row(ctx, item, cx)));
            let prev_more = (prev_hidden > 0).then(|| {
                Row::new("lineage-previous-more", format!("Show {} more", prev_hidden.min(LINEAGE_PAGE))).icon(Icon::Plus).on_click({
                    let prev_shown = prev_shown.clone();
                    move |_, _, cx| prev_shown.update(cx, |count, cx| { *count += LINEAGE_PAGE; cx.notify(); })
                })
            });
            group = group.child(v_flex().w_full().children(prev_items).children(prev_more));
        }
        group.into_any_element()
    });
    if let Some(more_row) = more {
        rendered.push(more_row.into_any_element());
    }
    rendered.extend(remote_active.iter().map(|item| remote_subagent_row(ctx, item, cx)));
    if let Some(prev) = previous_block {
        rendered.push(prev);
    }
    let heading = rows.show_heading.then(|| {
        if rows.running_count > 0 { format!("Lineage · {} running", rows.running_count) } else { "Lineage".to_owned() }.into()
    });
    let action = rows.can_detach.then(|| {
        let state = ctx.state.clone();
        crate::right_panel::Button::new("lineage-more-actions")
            .icon(Icon::Ellipsis)
            .icon_size(12.)
            .tooltip("More thread actions")
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                let state = state.clone();
                menu.min_w(px(ROW_WIDTH)).item(
                    PopupMenuItem::new("Disconnect agent session")
                        .icon(menu_icon(Icon::Power))
                        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.detach_provider_session(cx))),
                )
            })
            .into_any_element()
    });
    let list = (!rendered.is_empty()).then(|| v_flex().id("details-lineage").max_h(px(216.)).overflow_y_scroll().children(rendered));
    Some(section_with_action(separated, heading, action, false, cx).children(list).into_any_element())
}

/// The thread's side chats, which open in their right panel tab.
pub fn side_chats(ctx: &Ctx, rows: &Relationships, separated: bool, density: CardDensity, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if !ctx.details.read(cx).shows("sideChats") || !item_visible_at_density("sideChats", density) || rows.side_chats.is_empty() {
        return None;
    }
    let heading = if rows.running_side_chats > 0 {
        format!("Side chats · {} running", rows.running_side_chats)
    } else {
        "Side chats".to_owned()
    };
    let rendered: Vec<AnyElement> = rows.side_chats.iter().map(|row| relationship_row(ctx, row, None, window, cx)).collect();
    let list = v_flex().id("details-side-chats").max_h(px(216.)).overflow_y_scroll().children(rendered);
    Some(section(separated, Some(heading.into()), cx).child(list).into_any_element())
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, Render, TestAppContext, VisualTestContext, Window};
    use serde_json::json;
    use t3_client::supervisor::ConnectionStatus;
    use t3_core::vcs::{VcsLocalStatus, VcsRemoteStatus, VcsStatus};

    use super::*;
    use crate::app_state::Phase;

    struct SectionHost {
        ctx: Ctx,
    }

    impl Render for SectionHost {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            v_flex().w(px(300.)).children(version_control(&self.ctx, false, CardDensity::Full, None, &super::super::changes::Comparison::default(), cx))
        }
    }

    fn seed_shell(state: &mut AppState, scratch_root: &str, cx: &mut gpui::Context<AppState>) {
        let link = json!({
            "host": "github.com",
            "repository": "example/demo-app",
            "number": 42,
            "url": "https://github.com/example/demo-app/pull/42",
            "source": "manual",
            "linkedAt": "2026-11-14T10:00:00.000Z"
        });
        let thread = |id: &str, project_id: &str| {
            json!({
                "id": id,
                "projectId": project_id,
                "title": id,
                "providerInstanceId": "devReplay",
                "modelSelection": { "instanceId": "devReplay", "model": "showcase" },
                "runtimeMode": "auto-accept-edits",
                "interactionMode": "default",
                "branch": null,
                "worktreePath": null,
                "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": id },
                "latestRunId": null,
                "activeRunId": null,
                "status": "idle",
                "pendingRuntimeRequest": null,
                "latestVisibleMessage": null,
                "latestUserMessageAt": null,
                "hasActionableProposedPlan": false,
                "itemCount": 0,
                "visibleItemCount": 0,
                "createdAt": "2026-11-14T10:00:00.000Z",
                "updatedAt": "2026-11-14T10:00:00.000Z",
                "archivedAt": null,
                "settledOverride": null,
                "settledAt": null,
                "deletedAt": null,
                "pullRequests": [link]
            })
        };
        let snapshot = serde_json::from_value(json!({
            "kind": "snapshot",
            "snapshot": {
                "snapshotSequence": 1,
                "projects": [
                    {
                        "id": "proj-scratch",
                        "title": "No project",
                        "workspaceRoot": scratch_root,
                        "scripts": [],
                        "createdAt": "2026-11-14T10:00:00.000Z",
                        "updatedAt": "2026-11-14T10:00:00.000Z"
                    },
                    {
                        "id": "proj-plain",
                        "title": "plain",
                        "workspaceRoot": "/work/plain",
                        "scripts": [],
                        "createdAt": "2026-11-14T10:00:00.000Z",
                        "updatedAt": "2026-11-14T10:00:00.000Z"
                    }
                ],
                "threads": [
                    thread("t-scratch", "proj-scratch"),
                    thread("t-plain", "proj-plain")
                ]
            }
        }))
        .expect("shell snapshot");
        state.shell.apply(snapshot);
        cx.notify();
    }

    fn apply_config(state: &mut AppState, scratch_root: &str) {
        let config = serde_json::from_value(json!({
            "type": "snapshot",
            "config": {
                "environment": { "environmentId": "env-1", "label": "Local", "platform": { "os": "macos", "arch": "arm64", "machine": "local" } },
                "cwd": "/",
                "scratchWorkspaceRoot": scratch_root,
                "settings": {}
            }
        }))
        .expect("server config");
        state.server_config.apply(config);
    }

    #[gpui::test]
    fn version_control_omits_git_on_scratch_and_keeps_linked_prs(cx: &mut TestAppContext) {
        const SCRATCH_ROOT: &str = "/home/user/.t3/scratch";
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::keymap::init(cx);
            crate::settings::store::init(cx);
        });
        let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
        cx.update(|cx| crate::settings::init(state.clone(), cx));
        let git = cx.new(|cx| GitStore::new(state.clone(), cx));
        let details = cx.new(|cx| super::super::Details::new(state.clone(), git.clone(), cx));
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
            apply_config(state, SCRATCH_ROOT);
            seed_shell(state, SCRATCH_ROOT, cx);
            state.selected_thread = Some("t-scratch".into());
        });
        git.update(cx, |git, _| {
            git.status = Some(VcsStatus {
                local: VcsLocalStatus { is_repo: false, ..Default::default() },
                remote: VcsRemoteStatus::default(),
            });
        });
        let window = cx.add_window(|window, cx| {
            let dialogs = cx.new(|cx| GitDialogs::new(git.clone(), window, cx));
            let picker = cx.new(|cx| BranchPicker::new(git.clone(), window, cx));
            SectionHost {
                ctx: Ctx { row_menu_claim: Rc::new(Cell::new(false)), state: state.clone(), composer: None, details, git: git.clone(), dialogs, picker },
            }
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("details-linked-pr-0").is_some(), "scratch thread must keep linked PR rows");
        assert!(vcx.debug_bounds("git-init").is_none(), "scratch thread must omit Initialize Git");
        assert!(vcx.debug_bounds("details-branch").is_none(), "scratch thread must omit branch picker");

        state.update(&mut vcx, |state, _| {
            state.selected_thread = Some("t-plain".into());
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("details-linked-pr-0").is_some(), "non-repo plain folder must keep linked PR rows");
        assert!(vcx.debug_bounds("git-init").is_some(), "non-repo plain folder must offer Initialize Git");

        state.update(&mut vcx, |state, _| {
            state.selected_thread = Some("t-scratch".into());
            state.server_config.config = None;
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("git-init").is_none(), "pending server config must not offer Initialize Git");
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    fn open_in_row_checks_host_environment_and_remote_unavailable_menu(cx: &mut TestAppContext) {
        const SCRATCH_ROOT: &str = "/home/user/.t3/scratch";
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.set_global(
                serde_json::from_value::<crate::chatgpt_setup::Fixture>(json!({
                    "provider": {"instanceId":"codex","driver":"codex","enabled":true,"installed":true,"status":"ready"},
                    "auth":{"phase":"idle"},"install":{"phase":"idle"}
                }))
                .unwrap(),
            );
        });
        let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
        let git = cx.new(|cx| GitStore::new(state.clone(), cx));
        let details = cx.new(|cx| super::super::Details::new(state.clone(), git.clone(), cx));
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
            apply_config(state, SCRATCH_ROOT);
            let config = state.server_config.config.as_mut().unwrap();
            config.environment.environment_id = "env-remote".into();
            config.environment.label = "Test host".into();
            config.available_editors = vec!["cursor".into()];
            state.environments = t3_client::store::SavedEnvironments {
                active: Some("saved-remote".into()),
                environments: vec![t3_client::store::SavedEnvironment {
                    id: "saved-remote".into(),
                    environment_id: Some("env-remote".into()),
                    label: "Test host".into(),
                    disabled: false,
                    http_base: "https://remote.tailnet.ts.net".into(),
                    access_token: Some("tok".into()),
                    server_version: None,
                    local: false,
                    ssh: Some(t3_core::ssh::SshTarget { alias: "sol".into(), hostname: "sol".into(), username: None, port: None }),
                }],
            };
            seed_shell(state, SCRATCH_ROOT, cx);
            state.selected_thread = Some("t-plain".into());
        });
        details.update(cx, |details, _| details.set_remote_capable_editors(Some(Vec::new())));
        struct WorkspaceHost {
            ctx: Ctx,
        }
        impl Render for WorkspaceHost {
            fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
                v_flex().w(px(300.)).children(workspace(&self.ctx, CardDensity::Full, cx))
            }
        }
        let window = cx.add_window(|window, cx| {
            let dialogs = cx.new(|cx| GitDialogs::new(git.clone(), window, cx));
            let picker = cx.new(|cx| BranchPicker::new(git.clone(), window, cx));
            WorkspaceHost {
                ctx: Ctx { row_menu_claim: Rc::new(Cell::new(false)), state: state.clone(), composer: None, details: details.clone(), git: git.clone(), dialogs, picker },
            }
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| window.draw(cx).clear());
        let trigger = vcx.debug_bounds("details-open-in-menu").expect("open-in menu trigger should render");
        vcx.simulate_click(trigger.center(), gpui::Modifiers::none());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        // Now verify host_environment_id match switches back to LocalExec with server's `cursor`.
        state.update(&mut vcx, |state, _| {
            state.connect.local_server = None;
            state.environments.environments.push(t3_client::store::SavedEnvironment {
                id: "saved-host".into(),
                environment_id: Some("env-remote".into()),
                label: "Local".into(),
                disabled: false,
                http_base: "http://127.0.0.1:3773".into(),
                access_token: Some("tok".into()),
                server_version: None,
                local: true,
                ssh: None,
            });
        });
        assert_eq!(state.read_with(&vcx, |state, _| state.active_remote_open_state()), RemoteOpenState::LocalExec);
        assert!(state.read_with(&vcx, |state, _| state.should_show_open_in_picker(true)));

        // Non-SSH connection with advertised `ServerConfig.remoteOpenTargets` resolves to RemoteLinks.
        state.update(&mut vcx, |state, _| {
            state.environments.environments[0].ssh = None;
            state.environments.environments[1].environment_id = Some("env-other-host".into());
            state.server_config.config.as_mut().unwrap().remote_open_targets = vec![t3_core::server_config::RemoteOpenTarget {
                kind: "tailscale".into(),
                host: "box.tailnet.ts.net".into(),
                username: Some("alice".into()),
            }];
        });
        assert_eq!(
            state.read_with(&vcx, |state, _| state.active_remote_open_state()),
            RemoteOpenState::RemoteLinks(t3_core::thread_details::RemoteOpenHost {
                host: "box.tailnet.ts.net".into(),
                username: Some("alice".into()),
            })
        );

        // Port-forwarded non-local loopback environment != primary/host hides the OpenInPicker row.
        state.update(&mut vcx, |state, _| {
            state.environments.environments[0].http_base = "http://127.0.0.1:4777".into();
            state.server_config.config.as_mut().unwrap().remote_open_targets.clear();
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert_eq!(state.read_with(&vcx, |state, _| state.active_remote_open_state()), RemoteOpenState::LocalExec);
        assert!(!state.read_with(&vcx, |state, _| state.should_show_open_in_picker(true)));
        assert!(vcx.debug_bounds("details-open-in").is_none());

        state.update(&mut vcx, |state, _| state.stop_pump());
    }

    #[gpui::test]
    fn changes_base_label_and_scripts_row_render_expected_controls(cx: &mut TestAppContext) {
        const SCRATCH_ROOT: &str = "/home/user/.t3/scratch";
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::keymap::init(cx);
            crate::settings::store::init(cx);
        });
        let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
        cx.update(|cx| crate::settings::init(state.clone(), cx));
        let git = cx.new(|cx| GitStore::new(state.clone(), cx));
        let details = cx.new(|cx| super::super::Details::new(state.clone(), git.clone(), cx));
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
            apply_config(state, SCRATCH_ROOT);
            seed_shell(state, SCRATCH_ROOT, cx);
            state.selected_thread = Some("t-plain".into());
        });
        git.update(cx, |git, _| {
            let mut local = VcsLocalStatus {
                is_repo: true,
                has_working_tree_changes: true,
                ..Default::default()
            };
            local.working_tree.insertions = 4;
            local.working_tree.deletions = 1;
            git.status = Some(VcsStatus { local, remote: VcsRemoteStatus::default() });
        });
        struct CombinedHost {
            ctx: Ctx,
        }
        impl Render for CombinedHost {
            fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
                v_flex()
                    .w(px(300.))
                    .children(workspace(&self.ctx, CardDensity::Full, cx))
                    .children(version_control(&self.ctx, false, CardDensity::Full, None, &super::super::changes::Comparison::default(), cx))
            }
        }
        let window = cx.add_window(|window, cx| {
            let dialogs = cx.new(|cx| GitDialogs::new(git.clone(), window, cx));
            let picker = cx.new(|cx| BranchPicker::new(git.clone(), window, cx));
            CombinedHost {
                ctx: Ctx { row_menu_claim: Rc::new(Cell::new(false)), state: state.clone(), composer: None, details: details.clone(), git: git.clone(), dialogs, picker },
            }
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("git-changes-base").is_some(), "Changes row should show comparison/Uncommitted label");
        assert!(vcx.debug_bounds("details-add-script").is_some(), "empty scripts should render Add project script row");
        assert!(vcx.debug_bounds("details-run-script").is_none());

        state.update(&mut vcx, |state, cx| {
            let config = state.server_config.config.as_mut().unwrap();
            config.settings.default_project_scripts = vec![ProjectScript {
                id: "dev".into(),
                name: "Dev".into(),
                command: "bun dev".into(),
                ..Default::default()
            }];
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("details-add-script").is_none());
        assert!(vcx.debug_bounds("details-run-script").is_some(), "configured script should render split run row");
        assert!(vcx.debug_bounds("details-scripts-menu").is_some());
    }

    #[gpui::test]
    fn script_run_respects_terminal_operate_while_edit_stays_clickable(cx: &mut TestAppContext) {
        const SCRATCH_ROOT: &str = "/home/user/.t3/scratch";
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::keymap::init(cx);
            crate::settings::store::init(cx);
        });
        let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
        cx.update(|cx| crate::settings::init(state.clone(), cx));
        let git = cx.new(|cx| GitStore::new(state.clone(), cx));
        let details = cx.new(|cx| super::super::Details::new(state.clone(), git.clone(), cx));
        let script = ProjectScript {
            id: "dev".into(),
            name: "Dev".into(),
            command: "bun dev".into(),
            ..Default::default()
        };
        state.update(cx, |state, cx| {
            state.phase = Phase::Online(ConnectionStatus::Connected);
            apply_config(state, SCRATCH_ROOT);
            state.server_config.config.as_mut().unwrap().settings.default_project_scripts = vec![script.clone()];
            seed_shell(state, SCRATCH_ROOT, cx);
            state.selected_thread = Some("t-plain".into());
        });
        assert!(!git.read_with(cx, |git, _| git.can_run_scripts()));

        // Invoking the keybinding/details entrypoint without `terminal:operate` must neither mutate
        // `last_scripts` nor dispatch terminal RPCs.
        details.update(cx, |details, cx| details.run_script("proj-plain", &script, cx));
        assert_eq!(details.read_with(cx, |details, _| details.last_script("proj-plain").map(str::to_owned)), None);
        git.update(cx, |git, cx| git.run_script(&script, cx));

        struct MenuHost {
            menu: Entity<gpui_component::menu::PopupMenu>,
        }
        impl Render for MenuHost {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                v_flex().w(px(300.)).child(self.menu.clone())
            }
        }
        let window = cx.add_window(|window, cx| {
            let menu = gpui_component::menu::PopupMenu::build(window, cx, |menu, _, cx| {
                let handle = cx.entity().downgrade();
                menu.min_w(px(ROW_WIDTH)).item(script_menu_item(&details, handle, "proj-plain", &script, false, false))
            });
            MenuHost { menu }
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| window.draw(cx).clear());
        let edit_bounds = vcx.debug_bounds("details-script-edit-dev").expect("disabled script item should still render Edit button");
        vcx.simulate_click(edit_bounds.center(), gpui::Modifiers::none());
        let pending = vcx.update(|_, cx| {
            cx.has_global::<git::PendingScriptEditor>()
                .then(|| cx.global::<git::PendingScriptEditor>().0.clone())
                .flatten()
        });
        assert_eq!(
            pending,
            Some(git::ScriptEditorRequest { project_id: "proj-plain".into(), script_id: Some("dev".into()), request: None }),
            "Edit button inside a disabled script menu item must remain clickable when !terminal:operate"
        );
    }
}
