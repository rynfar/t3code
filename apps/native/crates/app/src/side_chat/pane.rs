//! `SideChatPane.tsx`: a side chat tab's header (title, Bring back, Delete, Close) over the side
//! chat's own transcript and composer.

use std::time::Duration;

use gpui::{Context, Entity, IntoElement, ParentElement, PromptLevel, Render, Styled, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::lineage::latest_merge_back_run;
use t3_core::right_panel::Surface;
use t3_core::side_chat::{BRING_BACK_HINT, bring_back_blocked_reason};
use t3_core::sidebar::visits::{OpenEnvironment, VisitDecision, VisitTracker};

use crate::app_state::{AppEvent, AppState};
use crate::composer::Composer;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::thread_view::ThreadView;

pub struct SideChatPane {
    state: Entity<AppState>,
    thread_id: String,
    transcript: Entity<ThreadView>,
    composer: Entity<Composer>,
    visit: Option<VisitTracker>,
    visit_scope: Option<(OpenEnvironment, Option<u64>)>,
    visit_revision: u64,
    visit_timer: Option<Task<()>>,
    syncing_visit: bool,
    _subscription: Subscription,
}

impl SideChatPane {
    pub fn new(state: Entity<AppState>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(|cx| ThreadView::embedded(state.clone(), thread_id.clone(), window, cx));
        let composer = cx.new(|cx| Composer::embedded(state.clone(), thread_id.clone(), window, cx));
        transcript.update(cx, |view, cx| view.bind_composer(&composer, cx));
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Phase | AppEvent::SideThread) {
                this.sync_visit(cx);
            }
            if matches!(event, AppEvent::Shell | AppEvent::SideThread | AppEvent::Selection) {
                cx.notify();
            }
        });
        let mut pane = SideChatPane {
            state,
            thread_id,
            transcript,
            composer,
            visit: None,
            visit_scope: None,
            visit_revision: 0,
            visit_timer: None,
            syncing_visit: false,
            _subscription: subscription,
        };
        pane.sync_visit(cx);
        pane
    }

    fn sync_visit(&mut self, cx: &mut Context<Self>) {
        if self.syncing_visit {
            return;
        }
        self.syncing_visit = true;
        let now = now_ms();
        let thread_id = self.thread_id.clone();
        let state_entity = self.state.clone();
        let decision = state_entity.update(cx, |state, cx| {
            let environment = state.visit_environment();
            let connection = state.connection_id();
            let Some(environment) = environment else {
                self.visit = None;
                self.visit_scope = None;
                self.visit_timer = None;
                self.visit_revision += 1;
                return None;
            };
            let scope = (environment.clone(), connection);
            if self.visit_scope.as_ref() != Some(&scope) || self.visit.is_none() {
                let opened = state.open_history.claim(&environment, &thread_id, now);
                self.visit_scope = Some(scope.clone());
                self.visit = Some(VisitTracker::new(&thread_id, opened));
                self.visit_timer = None;
                self.visit_revision += 1;
            }
            let shell = state.visited_shell_for(&thread_id, now)?;
            let decision = self.visit.as_mut()?.on_shell(shell, now);
            if let VisitDecision::Now { visited_at, opened } = decision {
                state.send_visit_with(&thread_id, visited_at, opened, None, cx);
                return None;
            }
            Some((decision, scope))
        });
        if let Some((VisitDecision::At { due, visited_at }, scope)) = decision {
            self.visit_revision += 1;
            let revision = self.visit_revision;
            self.visit_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis((due - now).max(0) as u64))
                    .await;
                let _ = this.update(cx, |pane, cx| {
                    pane.fire_visit(revision, &scope, visited_at, cx);
                });
            }));
        }
        self.syncing_visit = false;
    }

    fn fire_visit(
        &mut self,
        revision: u64,
        expected_scope: &(OpenEnvironment, Option<u64>),
        visited_at: i64,
        cx: &mut Context<Self>,
    ) {
        if self.visit_revision != revision || self.visit_scope.as_ref() != Some(expected_scope) {
            return;
        }
        let thread_id = self.thread_id.clone();
        self.state.update(cx, |state, cx| {
            let Some(environment) = state.visit_environment() else { return };
            let connection = state.connection_id();
            if (&environment, connection) != (&expected_scope.0, expected_scope.1) {
                return;
            }
            let Some(tracker) = self.visit.as_mut() else { return };
            if let VisitDecision::Now { visited_at, opened } = tracker.fire(visited_at, now_ms()) {
                state.send_visit_with(&thread_id, visited_at, opened, None, cx);
            }
        });
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let id = Surface::side_chat(&self.thread_id).id;
        self.state.update(cx, |state, cx| state.close_surface(&id, cx));
    }

    /// Confirms first when the user asked to (`confirmThreadDelete`), with the OS dialog the web uses.
    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.state.read(cx).thread_shell(&self.thread_id).map(|thread| thread.title.clone()) else { return };
        let id = self.thread_id.clone();
        if !crate::settings::store::client(cx).confirm_thread_delete() {
            self.state.update(cx, |state, cx| state.delete_side_chat(&id, cx));
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete side chat \"{title}\"?"),
            Some("This permanently clears its conversation history."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                let _ = this.update(cx, |this, cx| this.state.update(cx, |state, cx| state.delete_side_chat(&id, cx)));
            }
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let state = self.state.read(cx);
        let shell = state.thread_shell(&self.thread_id);
        let title = shell.map_or_else(|| "Side chat".to_owned(), |thread| thread.title.clone());
        let busy = state.side_threads.busy.contains(&self.thread_id);
        let parent_active = state.selected_thread.as_deref().is_some_and(|parent| state.is_thread_running(parent));
        let merge_run = state.thread_state(&self.thread_id).projection.as_ref().and_then(|projection| latest_merge_back_run(&projection.runs));
        let blocked = bring_back_blocked_reason(parent_active, merge_run);
        let missing = shell.is_none();
        h_flex()
            .h(px(36.))
            .flex_none()
            .items_center()
            .gap(px(4.))
            .pl(px(12.))
            .pr(px(4.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(div().flex_1().min_w_0().truncate().text_xs().font_weight(gpui::FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
            .child(
                Button::new("side-chat-bring-back")
                    .size(Size::Xs)
                    .label("Bring back")
                    .disabled(blocked.is_some() || busy || missing)
                    .tooltip(blocked.unwrap_or(BRING_BACK_HINT))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let id = this.thread_id.clone();
                        this.state.update(cx, |state, cx| state.bring_back_side_chat(&id, cx));
                    })),
            )
            .child(
                Button::new("side-chat-delete")
                    .size(Size::IconXs)
                    .icon(Icon::Trash2)
                    .disabled(missing || busy)
                    .tooltip("Delete side chat")
                    .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
            )
            .child(
                Button::new("side-chat-close")
                    .variant(Variant::Ghost)
                    .size(Size::IconXs)
                    .icon(Icon::X)
                    .tooltip("Close side chat")
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .into_any_element()
    }
}

impl Render for SideChatPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let muted = hsla(palette(cx).muted_foreground);
        let state = self.state.read(cx);
        let missing = state.thread_shell(&self.thread_id).is_none();
        let body = if missing {
            let deleted = state.shell.synchronized.then(|| div().p(px(12.)).text_xs().text_color(muted).child("This side chat was deleted."));
            div().flex_1().children(deleted).into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(div().flex_1().min_h_0().child(self.transcript.clone()))
                .child(self.composer.clone())
                .into_any_element()
        };
        // The open thread's chat shortcuts ignore keys from a side chat (`isInSideChatPane`); its
        // own composer handles them before they get here.
        v_flex()
            .size_full()
            .min_h_0()
            .on_action(|_: &crate::composer::ToggleModelPicker, _, _| {})
            .on_action(|_: &crate::composer::OpenPermissions, _, _| {})
            .on_action(|_: &crate::thread_view::FindInThread, _, _| {})
            .on_action(|_: &crate::keymap::SettleThread, _, _| {})
            .on_action(|_: &crate::keymap::PinThread, _, _| {})
            .on_action(|_: &crate::keymap::CopyThreadReference, _, _| {})
            .child(header)
            .child(body)
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;

    #[gpui::test]
    async fn side_chat_pane_records_open_visit_and_dedupes_reopen(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            let mut side: t3_core::shell::ThreadShell = serde_json::from_value(serde_json::json!({
                "id": "side-t",
                "projectId": "checkout-a",
                "title": "Side chat",
                "createdAt": "2026-04-01T00:00:00.000Z",
                "updatedAt": "2026-04-02T00:00:00.000Z",
                "lastVisitedAt": "2026-04-02T00:00:00.000Z",
                "providerInstanceId": "replay",
                "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto",
                "interactionMode": "default",
                "lineage": {"rootThreadId":"menu-thread","parentThreadId":"menu-thread"},
                "status": "idle",
                "hasActionableProposedPlan": false,
                "itemCount": 0,
                "visibleItemCount": 0
            })).unwrap();
            side.last_visited_at = Some(Some("2026-04-02T00:00:00.000Z".into()));
            state.shell.snapshot.as_mut().unwrap().threads.push(side);
            state.rebuild_sidebar(cx);
        });

        let (pane, _pcx) = cx.add_window_view(|window, cx| SideChatPane::new(state.clone(), "side-t".into(), window, cx));
        let (method, command, reply) = fixture.next_request().await;
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(command["type"], "thread.visit");
        assert_eq!(command["threadId"], "side-t");
        assert_eq!(command["opened"], true);
        reply.send(Ok(serde_json::json!({"sequence": 1}))).unwrap();

        drop(pane);
        // Reopening within 10 minutes must not resend an opened visit while already read.
        let (_reopened, _rcx) = cx.add_window_view(|window, cx| SideChatPane::new(state.clone(), "side-t".into(), window, cx));
        assert!(fixture.take_commands().is_empty());
    }
}
