//! The card itself (`ThreadDetailsCard.tsx`): dropdown glass, radius 16, scrolling content, and
//! the right-click menu that chooses which items show. Re-renders only when the open thread's
//! details, its Git state, or the card's preferences change. Coming back to the window rechecks
//! the checkout's status.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Render, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, v_flex};
use serde_json::Value;

use super::controls::Row;
use super::items::ITEMS;
use super::sections::{self, Ctx};
use super::{Details, DetailsEvent, Presentation, layout};
use crate::app_state::{AppEvent, AppState};
use crate::git::{BranchPicker, GitDialogs, GitEvent, GitStore};
use crate::icons::Icon;
use crate::theme::{hsla, metrics, palette};

/// What the card shows from the shell; a shell update that leaves it alone skips the render.
#[derive(Debug, Clone, PartialEq)]
struct Inputs {
    thread_id: String,
    branch: Option<String>,
    worktree_path: Option<String>,
    project_root: String,
    scripts: Vec<Value>,
    no_runs_yet: bool,
    lineage: crate::lineage::Relationships,
}

fn inputs(state: &AppState) -> Option<Inputs> {
    let thread = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id))?;
    let project = state.shell.project(&thread.project_id)?;
    Some(Inputs {
        thread_id: thread.id.clone(),
        branch: thread.branch.clone(),
        worktree_path: thread.worktree_path.clone(),
        project_root: project.workspace_root.clone(),
        scripts: project.scripts.clone(),
        no_runs_yet: thread.latest_run_id.is_none(),
        lineage: crate::lineage::relationships(state, thread),
    })
}

pub struct DetailsView {
    state: Entity<AppState>,
    details: Entity<Details>,
    git: Entity<GitStore>,
    dialogs: Entity<GitDialogs>,
    picker: Entity<BranchPicker>,
    lineage_shown: usize,
    inputs: Option<Inputs>,
    focus: FocusHandle,
    popover_shown: bool,
    row_menu_claim: Rc<Cell<bool>>,
    _subscriptions: Vec<Subscription>,
}

impl DetailsView {
    pub fn new(state: Entity<AppState>, details: Entity<Details>, dialogs: Entity<GitDialogs>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let git = details.read(cx).git.clone();
        let picker = cx.new(|cx| BranchPicker::new(git.clone(), window, cx));
        let subscriptions = vec![
            cx.subscribe(&state, |this, state, event: &AppEvent, cx| match event {
                AppEvent::Selection => {
                    this.lineage_shown = sections::LINEAGE_INITIAL;
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
        let initial = inputs(state.read(cx));
        DetailsView { state, details, git, dialogs, picker, lineage_shown: sections::LINEAGE_INITIAL, inputs: initial, focus: cx.focus_handle(), popover_shown: false, row_menu_claim: Rc::default(), _subscriptions: subscriptions }
    }

    fn ctx(&self) -> Ctx {
        Ctx { row_menu_claim: self.row_menu_claim.clone(), state: self.state.clone(), details: self.details.clone(), git: self.git.clone(), dialogs: self.dialogs.clone(), picker: self.picker.clone() }
    }

    fn close_popover(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.state.read(cx).selected_thread.clone() else { return };
        self.details.update(cx, |details, cx| details.close_popover(&thread, cx));
    }
}

/// `ThreadDetailsItemsMenuItems`: the card's context menu and the empty card's chooser.
fn items_menu(menu: gpui_component::menu::PopupMenu, details: &Entity<Details>, cx: &App) -> gpui_component::menu::PopupMenu {
    let visibility = details.read(cx).items.clone();
    let mut menu = menu.label("Show in thread details");
    for (id, label, _) in ITEMS {
        let details = details.clone();
        let shown = visibility.shows(id);
        menu = menu.item(PopupMenuItem::new(*label).checked(shown).on_click(move |_, _, cx| details.update(cx, |details, cx| details.set_item(id, !shown, cx))));
    }
    let reset = details.clone();
    menu.separator().item(PopupMenuItem::new("Reset to default").disabled(visibility.is_default()).on_click(move |_, _, cx| reset.update(cx, |details, cx| details.reset_items(cx))))
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
        if !layout.card_open || self.inputs.is_none() {
            return div().into_any_element();
        }
        let p = palette(cx);
        let ctx = self.ctx();
        let lineage_shown = self.lineage_shown;
        let workspace = sections::workspace(&ctx, cx);
        let version_control = sections::version_control(&ctx, workspace.is_some(), cx);
        let more = cx.listener(|this, _, _, cx| {
            this.lineage_shown += sections::LINEAGE_PAGE;
            cx.notify();
        });
        let relationships = self.inputs.as_ref().map(|inputs| inputs.lineage.clone()).unwrap_or_default();
        let lineage = sections::lineage(&ctx, &relationships, lineage_shown, workspace.is_some() || version_control.is_some(), more, cx);
        let side_chats = sections::side_chats(&ctx, &relationships, workspace.is_some() || version_control.is_some() || lineage.is_some(), cx);
        let empty = workspace.is_none() && version_control.is_none() && lineage.is_none() && side_chats.is_none();
        let details = self.details.clone();
        let content = v_flex().w_full().children(workspace).children(version_control).children(lineage).children(side_chats).when(empty, |this| {
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
            .when(popover, |this| this.occlude().shadow(crate::composer::controls::floating_shadow(cx.theme().is_dark())))
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
