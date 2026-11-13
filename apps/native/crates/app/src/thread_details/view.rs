//! The card itself (`ThreadDetailsCard.tsx`): dropdown glass, radius 16, scrolling content, and
//! the right-click menu that chooses which items show. Re-renders only when the open thread's
//! details, its Git state, or the card's preferences change. Coming back to the window rechecks
//! the checkout's status.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Render, Styled, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, v_flex};
use serde_json::Value;
use t3_core::thread_details::{DensityInputs, item_visible_at_density, resolve_card_density};

use super::automations::AutomationsFeed;
use super::controls::Row;
use super::items::ITEMS;
use super::sections::{self, Ctx};
use super::servers::ServersFeed;
use super::usage::{context_available, context_window_section, limit_row_count, usage_limits_section};
use super::{Details, DetailsEvent, Presentation, available_height, layout, panel_key, project_scripts};
use crate::app_state::{AppEvent, AppState};
use crate::git::{BranchPicker, GitDialogs, GitEvent, GitStore};
use crate::icons::Icon;
use crate::terminal::TerminalDrawer;
use crate::theme::{hsla, metrics, palette};

/// What the card shows from the shell; a shell update that leaves it alone skips the render.
#[derive(Debug, Clone, PartialEq)]
struct Inputs {
    panel_key: String,
    thread_id: Option<String>,
    branch: Option<String>,
    worktree_path: Option<String>,
    project_root: String,
    scripts: Vec<Value>,
    no_runs_yet: bool,
    lineage: crate::lineage::Relationships,
}

fn inputs(state: &AppState) -> Option<Inputs> {
    if let Some(thread) = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)) {
        let project = state.shell.project(&thread.project_id)?;
        return Some(Inputs {
            panel_key: thread.id.clone(),
            thread_id: Some(thread.id.clone()),
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            project_root: project.workspace_root.clone(),
            scripts: project.scripts.clone(),
            no_runs_yet: thread.latest_run_id.is_none(),
            lineage: crate::lineage::relationships(state, thread),
        });
    }
    let project_id = state.draft_project.as_deref()?;
    let project = state.shell.project(project_id)?;
    Some(Inputs {
        panel_key: format!("draft:{project_id}"),
        thread_id: None,
        branch: None,
        worktree_path: None,
        project_root: project.workspace_root.clone(),
        scripts: project.scripts.clone(),
        no_runs_yet: true,
        lineage: crate::lineage::Relationships::default(),
    })
}

pub struct DetailsView {
    state: Entity<AppState>,
    details: Entity<Details>,
    git: Entity<GitStore>,
    dialogs: Entity<GitDialogs>,
    picker: Entity<BranchPicker>,
    terminal: Entity<TerminalDrawer>,
    pub(super) servers: ServersFeed,
    pub(super) automations: AutomationsFeed,
    lineage_shown: usize,
    inputs: Option<Inputs>,
    focus: FocusHandle,
    popover_shown: bool,
    row_menu_claim: Rc<Cell<bool>>,
    _usage_tick: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl DetailsView {
    pub fn new(
        state: Entity<AppState>,
        details: Entity<Details>,
        dialogs: Entity<GitDialogs>,
        terminal: Entity<TerminalDrawer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let git = details.read(cx).git.clone();
        let picker = cx.new(|cx| BranchPicker::new(git.clone(), window, cx));
        let subscriptions = vec![
            cx.subscribe(&state, |this, state, event: &AppEvent, cx| match event {
                AppEvent::Selection => {
                    this.lineage_shown = sections::LINEAGE_INITIAL;
                    this.servers.reset_pagination();
                    this.picker.update(cx, |picker, cx| picker.close(cx));
                    this.inputs = inputs(state.read(cx));
                    cx.notify();
                }
                // Lineage reads the open thread's subagents and runs as well as the shell.
                AppEvent::Shell | AppEvent::Thread => {
                    let next = inputs(state.read(cx));
                    if next != this.inputs {
                        this.inputs = next;
                        cx.notify();
                    }
                }
                AppEvent::ServerConfig | AppEvent::Phase | AppEvent::RightPanel => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&details, |_, _, _: &DetailsEvent, cx| cx.notify()),
            cx.observe(&terminal, |_, _, cx| cx.notify()),
            cx.subscribe(&git, |_, git, _: &GitEvent, cx| {
                git.update(cx, |git, _| git.settle_optimistic_branch());
                cx.notify();
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.git.update(cx, |git, cx| git.revalidate_on_return(cx));
                }
            }),
        ];
        let _usage_tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                let keep_going = this.update(cx, |this, cx| {
                    if this.should_tick_usage(cx) {
                        cx.notify();
                    }
                });
                if keep_going.is_err() {
                    return;
                }
            }
        });
        let initial = inputs(state.read(cx));
        DetailsView {
            state,
            details,
            git,
            dialogs,
            picker,
            terminal,
            servers: ServersFeed::new(),
            automations: AutomationsFeed::new(),
            lineage_shown: sections::LINEAGE_INITIAL,
            inputs: initial,
            focus: cx.focus_handle(),
            popover_shown: false,
            row_menu_claim: Rc::default(),
            _usage_tick,
            _subscriptions: subscriptions,
        }
    }

    fn should_tick_usage(&self, cx: &App) -> bool {
        let Some(key) = panel_key(self.state.read(cx)) else { return false };
        let details = self.details.read(cx);
        details.shows("usage") && (details.is_open(&key, Presentation::Inline) || details.is_open(&key, Presentation::Popover))
    }

    fn ctx(&self) -> Ctx {
        Ctx { row_menu_claim: self.row_menu_claim.clone(), state: self.state.clone(), details: self.details.clone(), git: self.git.clone(), dialogs: self.dialogs.clone(), picker: self.picker.clone() }
    }

    fn close_popover(&mut self, cx: &mut Context<Self>) {
        let Some(key) = panel_key(self.state.read(cx)) else { return };
        self.details.update(cx, |details, cx| details.close_popover(&key, cx));
    }
}

/// `ThreadDetailsItemsMenuItems`: the card's context menu and the empty card's chooser.
fn items_menu(menu: gpui_component::menu::PopupMenu, details: &Entity<Details>, cx: &App) -> gpui_component::menu::PopupMenu {
    let is_default = details.read(cx).items_are_default();
    let mut menu = menu.label("Show in thread details");
    for (id, label, _) in ITEMS {
        let details_ref = details.clone();
        let shown = details.read(cx).shows(id);
        menu = menu.item(PopupMenuItem::new(*label).checked(shown).on_click(move |_, _, cx| details_ref.update(cx, |details, cx| details.set_item(id, !shown, cx))));
    }
    let reset = details.clone();
    menu.separator().item(PopupMenuItem::new("Reset to default").disabled(is_default).on_click(move |_, _, cx| reset.update(cx, |details, cx| details.reset_items(cx))))
}

impl Render for DetailsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = layout(&self.state, window, cx);
        self.details.update(cx, |details, _| details.set_presentation(layout.presentation));
        let popover = layout.presentation == Presentation::Popover;
        let popover_shown = popover && layout.card_open && self.inputs.is_some();
        if popover_shown && !self.popover_shown {
            // The popover takes focus as it opens, so Escape closes it (`initialFocus`).
            let focus = self.focus.clone();
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
        self.popover_shown = popover_shown;

        let active_thread = self.inputs.as_ref().and_then(|inputs| inputs.thread_id.clone());
        let (show_servers, show_automations, show_usage, show_context) = {
            let details = self.details.read(cx);
            (details.shows("servers"), details.shows("automations"), details.shows("usage"), details.shows("context"))
        };
        let feed_active = layout.card_open && active_thread.is_some();
        self.servers.sync(&self.state, feed_active && show_servers, cx);
        self.automations.sync(&self.state, feed_active && show_automations, cx);

        if !layout.card_open || self.inputs.is_none() {
            return div().into_any_element();
        }

        let thread_id = active_thread.as_deref();
        let has_thread = thread_id.is_some();
        let terminal_height = self.terminal.read(cx).visible_height(f32::from(window.viewport_size().height), cx);
        let density_inputs = {
            let details = self.details.read(cx);
            let app = self.state.read(cx);
            let git = self.git.read(cx);
            let is_repo = has_thread && git.status.as_ref().is_none_or(|status| status.local.is_repo);
            let has_scripts = project_scripts(app).is_some_and(|(_, s)| !s.is_empty());
            let rel = self.inputs.as_ref().map(|i| &i.lineage);
            DensityInputs {
                environment: details.shows("environment"),
                open_in: details.shows("openIn"),
                scripts: details.shows("scripts") && has_scripts,
                branch: details.shows("branch") && is_repo,
                pr_row: false,
                git_actions: details.shows("gitActions") && is_repo,
                changes: details.shows("changes") && is_repo,
                servers_rows: if show_servers { thread_id.map_or(0, |id| self.servers.count_for(id)) } else { 0 },
                automations_rows: if show_automations { thread_id.map_or(0, |id| self.automations.count_for(id)) } else { 0 },
                lineage_rows: if details.shows("lineage") { rel.map_or(0, |r| r.lineage.len()) } else { 0 },
                side_chats_rows: if details.shows("sideChats") { rel.map_or(0, |r| r.side_chats.len()) } else { 0 },
                usage_rows: if show_usage { limit_row_count(app) } else { 0 },
                context: show_context && context_available(app),
            }
        };
        let density = resolve_card_density(available_height(window, terminal_height), &density_inputs);

        let p = palette(cx);
        let ctx = self.ctx();
        let lineage_shown = self.lineage_shown;
        let workspace = sections::workspace(&ctx, density, cx);
        let mut any_above = workspace.is_some();
        let version_control = sections::version_control(&ctx, any_above, density, cx);
        any_above |= version_control.is_some();
        let servers = (show_servers && item_visible_at_density("servers", density))
            .then(|| thread_id.and_then(|id| self.servers.render(&self.state, id, any_above && density.is_full(), window, cx)))
            .flatten();
        any_above |= servers.is_some();
        let automations = (show_automations && item_visible_at_density("automations", density))
            .then(|| thread_id.and_then(|id| self.automations.render(&self.state, id, any_above && density.is_full(), cx)))
            .flatten();
        any_above |= automations.is_some();
        let more = cx.listener(|this, _, _, cx| {
            this.lineage_shown += sections::LINEAGE_PAGE;
            cx.notify();
        });
        let relationships = self.inputs.as_ref().map(|inputs| inputs.lineage.clone()).unwrap_or_default();
        let lineage = sections::lineage(&ctx, &relationships, lineage_shown, any_above && density.is_full(), density, more, cx);
        any_above |= lineage.is_some();
        let side_chats = sections::side_chats(&ctx, &relationships, any_above && density.is_full(), density, cx);
        any_above |= side_chats.is_some();
        let usage_limits = (show_usage && item_visible_at_density("usage", density))
            .then(|| usage_limits_section(self.state.read(cx), any_above && density.is_full(), density.is_essential(), cx))
            .flatten();
        any_above |= usage_limits.is_some();
        let context_window = (show_context && item_visible_at_density("context", density))
            .then(|| context_window_section(self.state.read(cx), any_above && density.is_full(), density.is_essential(), cx))
            .flatten();
        let empty = workspace.is_none()
            && version_control.is_none()
            && servers.is_none()
            && automations.is_none()
            && lineage.is_none()
            && side_chats.is_none()
            && usage_limits.is_none()
            && context_window.is_none();

        let details = self.details.clone();
        let content = v_flex()
            .w_full()
            .children(workspace)
            .children(version_control)
            .children(servers)
            .children(automations)
            .children(lineage)
            .children(side_chats)
            .children(usage_limits)
            .children(context_window)
            .when(empty, |this| {
                // Only when no section has content, so an emptied card can be refilled.
                this.child(div().p(px(8.)).child(Row::new("details-choose", "Choose what to show").icon(Icon::SlidersHorizontal).opens_menu().dropdown_menu(move |menu, _, cx| items_menu(menu, &details, cx))))
            });
        let details = self.details.clone();
        let claim = self.row_menu_claim.clone();
        let card = div()
            .id("thread-details-card")
            .relative()
            .w(px(metrics::THREAD_DETAILS_PANEL_WIDTH))
            .max_h_full()
            .flex()
            .flex_col()
            .rounded(px(16.))
            .child(crate::glass::backdrop(16., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .child(crate::webview::occluder("thread-details-cover", crate::webview::Layer::TranscriptChrome))
            .when(popover, |this| {
                this.occlude()
                    .shadow(crate::composer::controls::floating_shadow(cx.theme().is_dark()))
                    .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                        if !super::point_in_find_bar(event.position, this.state.read(cx), window) {
                            this.close_popover(cx);
                        }
                    }))
            })
            .child(div().id("thread-details-scroll").relative().min_h_0().overflow_y_scroll().child(content))
            .context_menu(move |menu, _, cx| if claim.replace(false) { menu } else { items_menu(menu, &details, cx) });
        // Anchored top and bottom so the card never grows past the canvas (or under the drawer).
        let top = if popover { 4. } else { super::CARD_GAP };
        div()
            .id("thread-details")
            .absolute()
            .top(px(top))
            .bottom(px(super::CARD_GAP))
            .right(px(super::CARD_GAP))
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .when(popover, |this| {
                this.on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_popover(cx);
                    }
                }))
            })
            .child(card)
            .into_any_element()
    }
}
