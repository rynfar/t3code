//! New threads (ledger CO-NEWTHREAD, TV-DRAFT): the `chat.new` action, the chat header's
//! new-thread controls, and the draft hero above the composer. Ports `resolveThreadActionProjectRef`
//! and `startNewThreadFromContext` (`web/src/lib/chatThreadActions.ts`), `resolveDefaultProjectRef`
//! (`web/src/hooks/useHandleNewThread.ts`), the project breadcrumb in `ChatHeader.tsx`, and
//! `DraftHeroProjectHeading.tsx`. The draft's first send launches the thread (see `composer`).

use std::time::{Duration, Instant};

use gpui::{
    App, Context, Entity, FontWeight, IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled,
    Subscription, Window, actions, div, prelude::*, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};

use crate::app_state::{AppEvent, AppState};
use crate::composer::Composer;
use crate::composer::controls::{Float, MenuItem, floating, menu_surface};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

actions!(t3, [NewThread]);

pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-n", NewThread, None)]);
    cx.on_action(move |_: &NewThread, cx| state.update(cx, start_new_thread));
}

/// `resolveThreadActionProjectRef`: the open thread's project, else the draft's, else the first
/// project that is not archived.
pub fn default_project(state: &AppState) -> Option<String> {
    let thread_project = state.selected_thread.as_deref().and_then(|id| state.shell.thread(id)).map(|t| t.project_id.clone());
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
    let thread = app.selected_thread.as_deref().and_then(|id| app.shell.thread(id));
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

impl Render for DraftHero {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let current = app.draft_project.clone();
        let name: SharedString =
            current.as_deref().and_then(|id| app.shell.project(id)).map(|project| project.title.clone()).unwrap_or_else(|| "Choose a project".into()).into();
        let projects: Vec<(String, String)> = app
            .shell
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot.projects.iter().filter(|project| project.archived_at.is_none()).map(|project| (project.id.clone(), project.title.clone())).collect()
            })
            .unwrap_or_default();
        let picker = div()
            .relative()
            .child(
                h_flex()
                    .id("draft-project")
                    .gap(px(4.))
                    .cursor_pointer()
                    .font_weight(FontWeight::MEDIUM)
                    .hover(|this| this.text_color(hsla(p.foreground).opacity(0.8)))
                    .child(name)
                    .child(Icon::ChevronDown.render(px(20.), hsla(p.muted_foreground)))
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
                                .max_h(px(320.))
                                .text_size(px(14.))
                                .font_weight(FontWeight::NORMAL)
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    this.projects_open = false;
                                    this.dismissed = Some(Instant::now());
                                    cx.notify();
                                }))
                                .children(projects.into_iter().map(|(id, title)| {
                                    let checked = current.as_deref() == Some(id.as_str());
                                    MenuItem::new(SharedString::from(format!("project-{id}")), title).checked(checked).on_click(
                                        cx.listener(move |this, _, window, cx| {
                                            this.projects_open = false;
                                            this.state.update(cx, |state, cx| state.start_draft(&id, cx));
                                            this.composer.update(cx, |composer, cx| composer.focus(window, cx));
                                        }),
                                    )
                                })),
                ))
            });
        v_flex()
            .size_full()
            .justify_center()
            .child(
                h_flex()
                    .w_full()
                    .px(px(48.))
                    .justify_center()
                    .flex_wrap()
                    .gap(px(8.))
                    .text_size(px(30.))
                    .line_height(px(36.))
                    .text_color(hsla(p.foreground))
                    .child("What should we build in")
                    .child(h_flex().child(picker).child("?")),
            )
            .child(div().h(px(16.)))
            .child(self.composer.clone())
    }
}
