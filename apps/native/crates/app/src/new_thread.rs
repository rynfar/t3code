//! New threads (ledger CO-NEWTHREAD, TV-DRAFT): the `chat.new` action, the chat header's
//! new-thread controls, and the draft hero above the composer. Ports `resolveThreadActionProjectRef`
//! and `startNewThreadFromContext` (`web/src/lib/chatThreadActions.ts`), `resolveDefaultProjectRef`
//! (`web/src/hooks/useHandleNewThread.ts`), the project breadcrumb in `ChatHeader.tsx`, and
//! `DraftHeroProjectHeading.tsx` and `DraftHeroHeadline.tsx` (the project picker, "or start without
//! a project", and the scratch draft's "What should we work on?"). The draft's first send launches
//! the thread (see `composer`). `TriageDraftHero` waits on the triage flow, which is not ported.

use std::time::{Duration, Instant};

use gpui::{
    App, Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, actions, div, prelude::*, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};

use crate::app_state::{AppEvent, AppState};
use crate::composer::Composer;
use crate::composer::controls::{Float, MenuItem, floating, menu_surface};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

actions!(t3, [NewThread, NewThreadWithoutProject]);

pub fn init(state: Entity<AppState>, cx: &mut App) {
    let scratch = state.clone();
    cx.on_action(move |_: &NewThread, cx| state.update(cx, start_new_thread));
    cx.on_action(move |_: &NewThreadWithoutProject, cx| crate::home::start_without_project(&scratch, cx));
}

/// `resolveThreadActionProjectRef`: the open thread's project, else the draft's, else the first
/// project that is not archived.
pub fn default_project(state: &AppState) -> Option<String> {
    let thread_project = state.selected_thread.as_deref().and_then(|id| state.thread_shell(id)).map(|t| t.project_id.clone());
    thread_project.or_else(|| state.draft_project.clone()).or_else(|| {
        let snapshot = state.shell.snapshot.as_ref()?;
        snapshot.projects.iter().find(|project| project.archived_at.is_none()).map(|project| project.id.clone())
    })
}

pub fn start_new_thread(state: &mut AppState, cx: &mut Context<AppState>) {
    if let Some(project_id) = default_project(state) {
        state.start_draft(&project_id, cx);
    }
}

/// The header's breadcrumb: the project (a button that starts a new thread in it, as on the web),
/// a separator, and the thread's title.
pub fn breadcrumb(state: &Entity<AppState>, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let app = state.read(cx);
    let thread = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id));
    let project_id = thread.map(|thread| thread.project_id.clone()).or_else(|| app.draft_project.clone());
    let project = project_id.as_deref().and_then(|id| app.shell.project(id)).map(|project| (project.id.clone(), project.title.clone()));
    let title: Option<SharedString> = match thread {
        Some(thread) => Some(thread.title.clone().into()),
        None if app.draft_project.is_some() => Some("New thread".into()),
        None => None,
    };
    let state = state.clone();
    h_flex()
        .flex_1()
        .min_w_0()
        .gap(px(12.))
        .text_sm()
        .children(project.map(|(project_id, name)| {
            let tooltip: SharedString = format!("New thread in {name}").into();
            h_flex()
                .id("breadcrumb-project")
                .flex_shrink(1.)
                .min_w_0()
                .gap(px(6.))
                .cursor_pointer()
                .text_color(hsla(p.muted_foreground))
                .hover(|this| this.text_color(hsla(p.foreground)))
                .child(Icon::Folder.render(px(14.), hsla(p.icon_muted)))
                .child(div().max_w(px(160.)).truncate().child(name))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(move |_, _, cx| state.update(cx, |state, cx| state.start_draft(&project_id, cx)))
        }))
        .when(title.is_some(), |this| this.child(div().flex_none().text_color(hsla(p.icon_muted)).child("/")))
        .children(title.map(|title| div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title)))
}

/// The header's New thread button: a ghost 28px toggle with the square-pen icon.
pub fn new_thread_button(cx: &App) -> impl IntoElement {
    let p = palette(cx);
    div()
        .id("new-thread")
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|this| this.bg(hsla(p.accent)))
        .child(Icon::SquarePen.render(px(16.), hsla(p.foreground).opacity(0.8)))
        .tooltip(|window, cx| Tooltip::new("New thread (⌘N)").build(window, cx))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewThread), cx))
}

/// The draft's page: "What should we build in <project>?" over the composer, centered.
pub struct DraftHero {
    state: Entity<AppState>,
    composer: Entity<Composer>,
    projects_open: bool,
    dismissed: Option<Instant>,
    _subscription: Subscription,
}

impl DraftHero {
    pub fn new(state: Entity<AppState>, composer: Entity<Composer>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Selection | AppEvent::Shell) {
                this.projects_open = false;
                cx.notify();
            }
        });
        DraftHero { state, composer, projects_open: false, dismissed: None, _subscription: subscription }
    }
}

impl DraftHero {
    /// The project picker (`InlineButton tone="picker"`): every project but the scratch one, then
    /// "Add project".
    fn picker(&self, label: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let current = app.draft_project.clone();
        let projects: Vec<(String, String)> = app
            .shell
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .projects
                    .iter()
                    .filter(|project| project.archived_at.is_none() && !app.is_scratch_project(&project.id))
                    .map(|project| (project.id.clone(), project.title.clone()))
                    .collect()
            })
            .unwrap_or_default();
        div()
            .relative()
            .min_w_0()
            .child(
                h_flex()
                    .id("draft-project")
                    .min_w_0()
                    .cursor_pointer()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .when(self.projects_open, |this| this.underline())
                    .hover(|this| this.underline())
                    .child(div().min_w_0().truncate().child(label))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.dismissed.take().is_some_and(|at| at.elapsed() < Duration::from_millis(300)) {
                            return;
                        }
                        this.projects_open = !this.projects_open;
                        cx.notify();
                    })),
            )
            .when(self.projects_open, |this| {
                this.child(floating(
                    Float::BelowStart,
                    menu_surface(cx)
                        .occlude()
                        .mt(px(4.))
                        .w(px(288.))
                        .max_h(px(320.))
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .font_weight(FontWeight::NORMAL)
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.projects_open = false;
                            this.dismissed = Some(Instant::now());
                            cx.notify();
                        }))
                        .children(projects.into_iter().map(|(id, title)| {
                            let checked = current.as_deref() == Some(id.as_str());
                            MenuItem::new(SharedString::from(format!("project-{id}")), title).checked(checked).on_click(cx.listener(move |this, _, window, cx| {
                                this.projects_open = false;
                                this.state.update(cx, |state, cx| state.start_draft(&id, cx));
                                this.composer.update(cx, |composer, cx| composer.focus(window, cx));
                            }))
                        }))
                        .child(MenuItem::new("project-add", "Add project").checked(false).icon(Icon::FolderPlus, hsla(p.muted_foreground)).on_click(cx.listener(|this, _, window, cx| {
                            this.projects_open = false;
                            cx.notify();
                            window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx);
                        }))),
                ))
            })
    }
}

impl Render for DraftHero {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let project = app.draft_project.as_deref().and_then(|id| app.shell.project(id)).map(|project| (project.id.clone(), project.title.clone()));
        let scratch_draft = project.as_ref().is_some_and(|(id, _)| app.is_scratch_project(id));
        let scratch_available = app.scratch_root().is_some();
        let shortcut = crate::keymap::label("chat.newWithoutProject", cx);
        let heading = |children: Vec<gpui::AnyElement>| {
            h_flex()
                .w_full()
                .px(px(48.))
                .justify_center()
                .flex_wrap()
                .gap(px(8.))
                .text_size(px(30.))
                .line_height(px(36.))
                .text_color(hsla(p.foreground))
                .children(children)
        };
        let headline = match &project {
            Some((_, title)) if !scratch_draft => {
                let picker = self.picker(title.clone().into(), cx).into_any_element();
                heading(vec!["What should we build in".into_any_element(), h_flex().min_w_0().child(picker).child("?").into_any_element()])
            }
            Some(_) => heading(vec!["What should we work on?".into_any_element()]),
            None => {
                let picker = self.picker("Choose a project".into(), cx).into_any_element();
                heading(vec![picker, "to start".into_any_element()])
            }
        };
        // The line under the headline: the scratch draft's picker, or the way to start without one.
        let below = if !scratch_available {
            None
        } else if scratch_draft {
            Some(self.picker("No project".into(), cx).into_any_element())
        } else {
            let state = self.state.clone();
            Some(
                div()
                    .id("draft-without-project")
                    .cursor_pointer()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.muted_foreground))
                    .hover(|this| this.underline().text_color(hsla(p.foreground)))
                    .child("or start without a project")
                    .when_some(shortcut, |this, shortcut| this.tooltip(move |window, cx| Tooltip::new(shortcut.clone()).build(window, cx)))
                    .on_click(move |_, _, cx| state.update(cx, |state, cx| state.start_without_project(cx)))
                    .into_any_element(),
            )
        };
        v_flex()
            .size_full()
            .justify_center()
            .child(headline)
            .children(below.map(|below| h_flex().mt(px(8.)).h(px(24.)).w_full().justify_center().text_sm().child(below)))
            .child(div().h(px(16.)))
            .child(self.composer.clone())
    }
}

impl AppState {
    pub(crate) fn start_branch_draft(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture(self, thread_id) else { return };
        let Some(thread) = self.thread_shell(thread_id) else { return };
        let project_id = thread.project_id.clone();
        let Some(project) = self.shell.project(&project_id) else { return };
        let choice = t3_core::workspace::DraftWorkspace {
            destination: Some((self.server_config.config.as_ref().map(|config| config.environment.environment_id.clone()).unwrap_or_default(), project.created_at.clone())),
            branch: thread.branch.clone(),
            environment_id: self.environments.active.clone().unwrap_or_default(),
            project_root: project.workspace_root.clone(),
            mode: if thread.worktree_path.is_some() { t3_core::server_config::ThreadEnvMode::Worktree } else { t3_core::server_config::ThreadEnvMode::Local },
            previous: thread.worktree_path.as_ref().map(|path| t3_core::workspace::PreviousWorktreeSeed { worktree_path:path.clone(), branch:thread.branch.clone() }),
        };
        self.start_draft(&project_id, cx);
        self.branch_draft = Some((owner, choice));
        cx.emit(AppEvent::BranchDraft);
    }
}
