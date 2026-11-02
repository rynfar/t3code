//! The home page (ledger TV-HOME): `apps/web/src/routes/_chat.index.tsx` opens a draft in the
//! most recent project (`AppState::land_home`); with no project it shows `NoProjectsHero`. Also
//! the scratch "No project" flow of `useScratchProject.ts`: `projects.ensureScratch`, then a draft
//! in the project it returns. `HostedEmptyState` has no native counterpart: the native app shows
//! its connect screen until an environment is connected.

use gpui::{App, Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_client::supervisor::ConnectionStatus;
use t3_core::home::{PROJECTS_ENSURE_SCRATCH, ensure_scratch_project_id, is_scratch_project, most_recent_project};

use crate::app_state::{AppEvent, AppState, Phase};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

impl AppState {
    /// The project the home page drafts in: the most recently active unarchived project.
    pub(crate) fn home_project(&self) -> Option<String> {
        let snapshot = self.shell.snapshot.as_ref()?;
        most_recent_project(&snapshot.projects, &snapshot.threads).map(|project| project.id.clone())
    }

    /// The brand link (`to="/"`): a draft in the most recent project, or the no-projects hero.
    pub fn go_home(&mut self, cx: &mut Context<Self>) {
        match self.home_project() {
            Some(project_id) => self.start_draft(&project_id, cx),
            None => {
                self.pull_requests_open = false;
                self.draft_project = None;
                self.clear_selection(cx);
            }
        }
    }

    /// `availableScratchWorkspaceRoot`: the scratch workspace, while the environment is connected.
    pub fn scratch_root(&self) -> Option<&str> {
        let connected = self.phase == Phase::Online(ConnectionStatus::Connected);
        connected.then(|| self.server_config.config.as_ref()?.scratch_workspace_root.as_deref()).flatten()
    }

    pub fn is_scratch_project(&self, project_id: &str) -> bool {
        self.shell.project(project_id).is_some_and(|project| is_scratch_project(&project.workspace_root, self.scratch_root()))
    }

    /// `startScratchThread`: ensures the scratch project, then opens a draft in it.
    pub fn start_without_project(&mut self, cx: &mut Context<Self>) {
        if self.scratch_root().is_none() {
            return;
        }
        let rpc = self.rpc(PROJECTS_ENSURE_SCRATCH, json!({}));
        cx.spawn(async move |this, cx| {
            let result = rpc.await;
            let _ = this.update(cx, |state, cx| match result.as_ref().ok().and_then(ensure_scratch_project_id) {
                Some(project_id) => state.start_draft(&project_id, cx),
                None => {
                    let description = result.err().map_or_else(|| "An error occurred.".to_owned(), |error| error.to_string());
                    state.show_toast(Toast::new(ToastKind::Error, "Could not start without a project").description(description), cx);
                }
            });
        })
        .detach();
    }
}

/// What the thread area shows with nothing open: the no-projects hero once the shell proves the
/// workspace is empty, else nothing while home opens a draft.
pub struct HomeView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl HomeView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::Selection | AppEvent::ServerConfig | AppEvent::Phase) {
                cx.notify();
            }
        });
        HomeView { state, _subscription: subscription }
    }
}

/// Whether the workspace has no project a draft could open in.
fn workspace_is_empty(state: &AppState) -> bool {
    state.shell.synchronized && state.shell.snapshot.as_ref().is_some_and(|snapshot| snapshot.projects.iter().all(|project| project.archived_at.is_some()))
}

impl Render for HomeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.state.read(cx);
        if !workspace_is_empty(app) {
            return div().size_full().into_any_element();
        }
        let p = palette(cx);
        let scratch = app.scratch_root().is_some();
        let description = if scratch { "Add a project, or start without one." } else { "Add a project to start your first thread." };
        let state = self.state.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p(px(48.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(512.))
                    .px(px(32.))
                    .py(px(48.))
                    .items_center()
                    .text_center()
                    .child(
                        div()
                            .text_size(px(30.))
                            .line_height(px(36.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hsla(p.foreground))
                            .child("What should we work on?"),
                    )
                    .child(div().mt(px(4.)).text_sm().text_color(hsla(p.muted_foreground)).child(description))
                    .child(
                        h_flex()
                            .mt(px(24.))
                            .gap(px(8.))
                            .justify_center()
                            .child(
                                Button::new("home-add-project", ButtonVariant::Default, ButtonSize::Sm)
                                    .icon(Icon::Plus)
                                    .label("Add project")
                                    .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx)),
                            )
                            .when(scratch, |this| {
                                this.child(
                                    Button::new("home-start-without-project", ButtonVariant::Outline, ButtonSize::Sm)
                                        .icon(Icon::MessageSquareDashed)
                                        .label("Start without a project")
                                        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.start_without_project(cx))),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

/// Opens the no-project draft from anywhere (`chat.newWithoutProject`).
pub fn start_without_project(state: &Entity<AppState>, cx: &mut App) {
    state.update(cx, |state, cx| state.start_without_project(cx));
}
