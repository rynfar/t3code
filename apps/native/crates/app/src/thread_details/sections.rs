//! The card's sections (`ThreadDetailsPanel.tsx`): Workspace (environment and worktree, open in
//! editor, project scripts), Version Control (branch and pull request, Git actions, changes)
//! Lineage and Side chats (`ThreadRelationshipsControl.tsx`, rows from `crate::lineage`).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{AnyElement, App, ClipboardItem, Entity, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use t3_core::lineage::{AgentTiming, PhaseState, WorkflowMember, relationship_status_label, subagent_display_title};
use t3_core::project_scripts::{ProjectScript, ScriptIcon, command_for_project_script, menu_label};
use t3_core::thread_details::{CardDensity, item_visible_at_density};
use t3_core::vcs::{self, editor_label, editor_options};

use super::controls::{MenuTrigger, ROW_WIDTH, Row, menu_icon, menu_shortcut, section, section_with_action, section_with_options, split_row};
use super::{Details, project_scripts, run_button_script};
use crate::app_state::AppState;
use crate::lineage::{DelegatedFromRow, LineageRow, MergeBack, Relationships, RemoteSubagentRow, StopAction, elapsed::agent_elapsed_element};
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
fn environment_row(ctx: &Ctx, worktree: Option<&str>, project_root: &str, cx: &App) -> AnyElement {
    let app = ctx.state.read(cx);
    let label = app.server_config.config.as_ref().map(|config| config.environment.label.trim().to_owned()).filter(|label| !label.is_empty()).unwrap_or_else(|| "This device".to_owned());
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
    let can_operate = ctx.git.read(cx).can_operate_thread();
    let preferred = ctx.details.read(cx).preferred_editor(&available);
    let mac = cfg!(target_os = "macos");
    let label = format!("Open in {}", preferred.map_or("editor", |id| editor_label(id, mac)));
    let details = ctx.details.clone();
    let primary = Row::new("details-open-in", label).icon(Icon::SquareArrowOutUpRight).disabled(preferred.is_none() || !can_operate).on_click(move |_, _, cx| {
        if let Some(editor) = preferred {
            details.update(cx, |details, cx| details.open_in_editor(editor, cx));
        }
    });
    let details = ctx.details.clone();
    let shortcut = crate::keymap::label("editor.openFavorite", cx);
    let trigger = MenuTrigger::new("details-open-in-menu", "Choose editor").disabled(!can_operate).dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
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
    let can_operate = git.can_operate_thread();
    let primary = run_button_script(ctx.details.read(cx), &project_id, &scripts).cloned();
    let running: Vec<bool> = scripts.iter().map(|script| git.script_running(&script.id)).collect();
    let menu = {
        let (details, project_id, scripts, running) = (ctx.details.clone(), project_id.clone(), scripts.clone(), running.clone());
        MenuTrigger::new("details-scripts-menu", "Script actions").disabled(!can_operate).dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            scripts.iter().zip(running.iter().copied()).fold(menu.min_w(px(ROW_WIDTH)), |menu, (script, running)| {
                let (details, project_id, script) = (details.clone(), project_id.clone(), script.clone());
                let label = menu_label(&script);
                let shortcut = command_for_project_script(&script.id);
                let item = PopupMenuItem::element(move |_, cx| {
                    let shortcut = shortcut.as_deref().and_then(|command| crate::keymap::label(command, cx));
                    h_flex().w_full().gap(px(8.)).child(div().flex_1().min_w_0().truncate().child(label.clone())).child(running_indicator(running, cx)).children(shortcut.map(|shortcut| menu_shortcut(shortcut, cx)))
                });
                menu.item(item.icon(menu_icon(script_icon(script.icon))).disabled(!can_operate).on_click(move |_, _, cx| details.update(cx, |details, cx| details.run_script(&project_id, &script, cx))))
            })
        })
    };
    let primary_row = match primary {
        Some(script) => {
            let details = ctx.details.clone();
            let running = git.script_running(&script.id);
            Row::new("details-run-script", script.name.clone())
                .icon(script_icon(script.icon))
                .disabled(!can_operate)
                .trailing(running_indicator(running, cx))
                .tooltip(format!("Run {}", script.name))
                .on_click(move |_, _, cx| run_script(&details, &project_id, &script, cx))
        }
        None => Row::new("details-scripts", "Actions").icon(Icon::Wrench).disabled(!can_operate),
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
        (details.shows("openIn") && item_visible_at_density("openIn", density)).then(|| open_in_row(ctx, cx)),
        (details.shows("scripts") && item_visible_at_density("scripts", density)).then(|| scripts_row(ctx, cx)).flatten(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!rows.is_empty()).then(|| section_with_options(false, None, density.is_essential(), cx).child(v_flex().children(rows)).into_any_element())
}

/// The branch row and its picker, and the PR row under it while the checkout is the thread's
/// branch (`BranchToolbarBranchSelector` in panel mode).
fn branch_rows(ctx: &Ctx, thread_branch: Option<&str>, cx: &App) -> AnyElement {
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
    let pr = vcs::pr_branch(thread_branch, active.as_deref())
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

pub fn version_control(ctx: &Ctx, separated: bool, density: CardDensity, cx: &App) -> Option<AnyElement> {
    let app = ctx.state.read(cx);
    let thread = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id));
    let project_id = thread.map(|thread| thread.project_id.as_str()).or(app.draft_project.as_deref())?;
    let project = app.shell.project(project_id)?;
    if app.is_scratch_project(project_id) || project.workspace_root.is_empty() {
        return None;
    }
    let details = ctx.details.read(cx);
    let is_repo = ctx.git.read(cx).status.as_ref().is_none_or(|status| status.local.is_repo);
    let no_runs_yet = thread.is_none_or(|thread| thread.latest_run_id.is_none());
    let thread_branch = thread.and_then(|thread| thread.branch.as_deref());
    let compact_actions = !density.is_full();
    let rows: Vec<AnyElement> = [
        (is_repo && details.shows("branch") && item_visible_at_density("branch", density)).then(|| branch_rows(ctx, thread_branch, cx)),
        (details.shows("gitActions") && item_visible_at_density("gitActions", density)).then(|| git::git_actions_row(&ctx.git, &ctx.dialogs, no_runs_yet, compact_actions, cx)).flatten(),
        (is_repo && details.shows("changes") && item_visible_at_density("changes", density)).then(|| git::changes_row(&ctx.git, &ctx.state, no_runs_yet, cx)).flatten(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!rows.is_empty()).then(|| section_with_options(separated && density.is_full(), None, density.is_essential(), cx).child(v_flex().children(rows)).into_any_element())
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
        .icon(row.icon)
        .trailing(trailing_label)
        .tooltip(format!("{}\n{} · Open thread", row.title, row.relationship))
        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.open_related_thread(&id, cx)));
    if let Some(workflow) = &row.workflow {
        let expanded = window.use_keyed_state(SharedString::from(format!("details-wf-open:{}", row.thread_id)), cx, |_, _| false);
        let is_expanded = *expanded.read(cx);
        let stop_btn = row.stop.clone().map(|action| {
            let state = ctx.state.clone();
            crate::right_panel::Button::new(SharedString::from(format!("lineage-stop-{}", row.thread_id)))
                .icon(Icon::Square)
                .icon_size(12.)
                .tooltip("Stop entire workflow")
                .on_click(move |_, _, cx| state.update(cx, |state, cx| state.run_stop_action(&action, cx)))
        });
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
    let mut row = Row::new("lineage-delegated-from", item.title.clone()).icon(Icon::CornerLeftUp).disabled(!item.connected);
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
        row = row.on_click(move |_, _, cx| state.update(cx, |state, cx| state.open_linked_thread(&env_id, &thread_id, cx)));
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
        .icon(Icon::Bot)
        .disabled(!item.connected)
        .trailing(div().flex_none().text_size(px(11.)).text_color(color).child(format!("on {} · {}", item.env_label, item.status)))
        .tooltip(format!("{}\n{hint}", item.title));
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
    let previous_block = (!rows.previous.is_empty()).then(|| {
        let prev_open = window.use_keyed_state(SharedString::from(format!("details-prev-open:{key_thread}")), cx, |_, _| false);
        let prev_shown = window.use_keyed_state(SharedString::from(format!("details-prev-shown:{key_thread}")), cx, |_, _| LINEAGE_INITIAL);
        let is_open = *prev_open.read(cx);
        let shown_prev = *prev_shown.read(cx);
        let failed_count = rows.previous.iter().filter(|row| row.failed).count();
        let p = palette(cx);
        let trailing = h_flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .when(failed_count > 0, |this| {
                this.child(div().text_size(px(11.)).font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.destructive)).child(format!("{failed_count} failed")))
            })
            .when(!is_open, |this| {
                this.child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(rows.previous.len().to_string()))
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
            let prev_hidden = rows.previous.len().saturating_sub(shown_prev);
            let prev_items: Vec<AnyElement> =
                rows.previous.iter().take(shown_prev).map(|row| relationship_row(ctx, row, rows.merge.as_ref(), window, cx)).collect();
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
    if let Some(prev) = previous_block {
        rendered.push(prev);
    }
    rendered.extend(rows.remote_subagents.iter().map(|item| remote_subagent_row(ctx, item, cx)));
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
