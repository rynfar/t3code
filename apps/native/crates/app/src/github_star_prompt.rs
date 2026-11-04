//! Adapter for `web/src/components/GitHubStarPrompt.tsx`; storage and RPC never run in render.
use crate::{
    app_state::{AppEvent, AppState},
    toast::{Toast, ToastKind},
};
use gpui::{App, Context, Entity, FocusHandle, Subscription, Task, Window};
use std::time::{Duration, Instant};
use t3_client::{
    store::{self, StarPromptStorage},
    supervisor::ClientHandle,
};
use t3_core::github_star_prompt::{self as core, Prompt, Stage};

#[derive(Clone)]
pub(crate) struct Origin {
    id: String,
    credential: Option<String>,
    client: ClientHandle,
    generation: u64,
}
impl Origin {
    fn valid(&self, state: &AppState) -> bool {
        let captured = core::Destination {
            id: &self.id,
            endpoint: self.client.environment(),
            credential: self.credential.as_deref(),
            generation: self.generation,
        };
        let current = state
            .environments
            .get(&self.id)
            .filter(|e| !e.disabled)
            .map(|e| core::Destination {
                id: &e.id,
                endpoint: &e.http_base,
                credential: e.access_token.as_deref(),
                generation: *self.client.connection_generation().borrow(),
            });
        let connected = state.environment_statuses.get(&self.id)
            == Some(&t3_client::supervisor::ConnectionStatus::Connected);
        captured.is_current(current, connected)
    }
}
pub(crate) struct GitHubStarPrompt {
    machine: Prompt,
    origin: Option<Origin>,
    clock: Instant,
    eligible: bool,
    timer: Option<Task<()>>,
    _events: Subscription,
    #[cfg(feature = "snapshot")]
    fixture: Option<bool>,
}
impl GitHubStarPrompt {
    pub fn new(cx: &mut Context<AppState>) -> Self {
        let events = cx.subscribe(&cx.entity(), |state, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Phase | AppEvent::ServerConfig)
                && state
                    .github_star
                    .origin
                    .as_ref()
                    .is_some_and(|o| !o.valid(state))
            {
                state.retire_star_prompt(cx);
            } else if matches!(event, AppEvent::Selection | AppEvent::RightPanel) {
                state.star_prompt_activity(cx);
            }
        });
        Self {
            machine: Prompt::default(),
            origin: None,
            clock: Instant::now(),
            eligible: false,
            timer: None,
            _events: events,
            #[cfg(feature = "snapshot")]
            fixture: None,
        }
    }
}
impl AppState {
    pub(crate) fn star_prompt_origin(&self) -> Option<Origin> {
        let client = self.client_handle()?;
        let saved = self
            .active_environment()
            .filter(|e| !e.disabled && e.http_base == client.environment())?;
        let id = saved.id.clone();
        let credential = saved.access_token.clone();
        let generation = *client.connection_generation().borrow();
        Some(Origin {
            id,
            credential,
            client,
            generation,
        })
    }
    pub(crate) fn record_prompt_sent(&mut self, origin: Option<Origin>, cx: &mut Context<Self>) {
        if self.github_star.machine.stage != Stage::Counting {
            return;
        }
        cx.spawn(async move |this, cx| {
            let stored = store::github_star_prompt(StarPromptStorage::Count).await;
            let _ = this.update(cx, |state, cx| {
                if !state.github_star.machine.record(stored) {
                    return;
                }
                let Some(origin) = origin.filter(|o| o.valid(state)) else {
                    state.retire_star_prompt(cx);
                    return;
                };
                state.github_star.origin = Some(origin.clone());
                let runtime = state.runtime().clone();
                cx.spawn(async move |this, cx| {
                    let result = runtime
                        .spawn(async move {
                            origin
                                .client
                                .request(core::CHECK, core::input())
                                .await
                                .ok()
                                .and_then(|v| v.as_bool())
                        })
                        .await
                        .ok()
                        .flatten();
                    let _ = this.update(cx, |state, cx| {
                        if !state.star_origin_valid() {
                            state.retire_star_prompt(cx);
                            return;
                        }
                        if state.github_star.machine.checked(result) {
                            state.persist_star_dismissal(cx);
                        }
                        state.star_prompt_activity(cx);
                    });
                })
                .detach();
            });
        })
        .detach();
    }
    fn star_origin_valid(&self) -> bool {
        #[cfg(feature = "snapshot")]
        if self.github_star.fixture.is_some() {
            return true;
        }
        self.github_star
            .origin
            .as_ref()
            .is_some_and(|o| o.valid(self))
    }
    fn star_idle_eligible(&self) -> bool {
        // Conservatively exclude the whole panel: embedded preview input bypasses GPUI capture.
        self.github_star.eligible
            && !embedded_focused()
            && !self
                .selected_thread
                .as_ref()
                .is_some_and(|id| self.panels.panels.panel(id).is_open)
    }
    pub(crate) fn star_prompt_focus(&mut self, eligible: bool, cx: &mut Context<Self>) {
        self.github_star.eligible = eligible;
        self.star_prompt_activity(cx);
    }
    pub(crate) fn star_prompt_activity(&mut self, cx: &mut Context<Self>) {
        let eligible = self.star_idle_eligible();
        let p = &mut self.github_star;
        let now = p.clock.elapsed().as_millis() as u64;
        p.machine.activity(now, eligible);
        p.timer = None;
        let Some(due) = p.machine.due() else {
            return;
        };
        p.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(due.saturating_sub(now)))
                .await;
            let dismissed = store::github_star_prompt(StarPromptStorage::Read)
                .await
                .map(|(_, dismissed)| dismissed);
            let _ = this.update(cx, |state, cx| {
                if !state.star_origin_valid() {
                    state.retire_star_prompt(cx);
                    return;
                }
                let eligible = state.star_idle_eligible();
                let p = &mut state.github_star;
                if p.machine
                    .idle(p.clock.elapsed().as_millis() as u64, eligible, dismissed)
                {
                    state.show_toast(
                        Toast::new(ToastKind::Info, "Enjoying T3 Code?")
                            .description("Give us a star on GitHub to let us know!")
                            .github_star(),
                        cx,
                    );
                }
            });
        }));
    }
    fn persist_star_dismissal(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |_, _| {
            let _ = store::github_star_prompt(StarPromptStorage::Dismiss).await;
        })
        .detach();
    }
    pub(crate) fn close_star_prompt(&mut self, cx: &mut Context<Self>) {
        self.github_star.machine.suppress();
        self.github_star.timer = None;
        self.persist_star_dismissal(cx);
    }
    fn retire_star_prompt(&mut self, cx: &mut Context<Self>) {
        self.github_star.machine.suppress();
        self.github_star.timer = None;
        self.remove_star_toast(cx);
    }
    pub(crate) fn star_prompt_stage(&self) -> Stage {
        self.github_star.machine.stage
    }
    pub(crate) fn activate_star_prompt(&mut self, cx: &mut Context<Self>) {
        if !self.star_origin_valid() {
            self.retire_star_prompt(cx);
            return;
        }
        if self.github_star.machine.stage == Stage::Fallback {
            self.close_star_prompt(cx);
            self.remove_star_toast(cx);
            #[cfg(feature = "snapshot")]
            if self.github_star.fixture.is_some() {
                eprintln!(
                    "star prompt spy: explicit fallback {url}; no URL opened",
                    url = core::URL
                );
                return;
            }
            cx.open_url(core::URL);
            return;
        }
        if !self.github_star.machine.start_star() {
            return;
        }
        cx.emit(AppEvent::Notice);
        #[cfg(feature = "snapshot")]
        if let Some(result) = self.github_star.fixture {
            eprintln!(
                "star prompt pre-RPC spy: {} {}; no RPC",
                core::STAR,
                core::input()
            );
            self.finish_star_prompt(result, cx);
            return;
        }
        let Some(origin) = self.github_star.origin.clone() else {
            self.retire_star_prompt(cx);
            return;
        };
        let runtime = self.runtime().clone();
        cx.spawn(async move |this, cx| {
            let dismissed = store::github_star_prompt(StarPromptStorage::Read).await;
            if !dismissed.is_ok_and(|(_, dismissed)| !dismissed) {
                let _ = this.update(cx, |state, cx| state.retire_star_prompt(cx));
                return;
            }
            let request = this.update(cx, |state, cx| {
                if !origin.valid(state) || state.github_star.machine.stage != Stage::Pending {
                    state.retire_star_prompt(cx);
                    return None;
                }
                Some(runtime.spawn(async move {
                    if *origin.client.connection_generation().borrow() != origin.generation
                        || !core::can_star(origin.client.session_state().borrow().as_ref())
                    {
                        return false;
                    }
                    origin
                        .client
                        .request(core::STAR, core::input())
                        .await
                        .ok()
                        .and_then(|v| v.as_bool())
                        == Some(true)
                }))
            });
            let Ok(Some(request)) = request else {
                return;
            };
            let result = request.await.unwrap_or(false);
            let _ = this.update(cx, |state, cx| state.finish_star_prompt(result, cx));
        })
        .detach();
    }
    fn finish_star_prompt(&mut self, result: bool, cx: &mut Context<Self>) {
        if !self.star_origin_valid() {
            self.retire_star_prompt(cx);
            return;
        }
        if self.github_star.machine.starred(result) {
            self.persist_star_dismissal(cx);
            self.thank_star_toast(cx);
        }
        cx.emit(AppEvent::Notice);
    }
}

/// Watches actual focus changes without polling or persisting input activity.
pub(crate) struct FocusObserver {
    state: Entity<AppState>,
    watched: Option<(FocusHandle, Subscription)>,
    _subscriptions: Vec<Subscription>,
}
impl FocusObserver {
    pub fn new(
        state: Entity<AppState>,
        root: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.on_focus_in(root, window, |this, window, cx| this.observe(window, cx)),
            cx.on_focus_out(root, window, |this, _, _, cx| {
                this.watched = None;
                this.state
                    .update(cx, |state, cx| state.star_prompt_focus(false, cx));
            }),
            cx.observe_window_activation(window, |this, window, cx| this.observe(window, cx)),
        ];
        let mut this = Self {
            state,
            watched: None,
            _subscriptions: subscriptions,
        };
        this.observe(window, cx);
        this
    }
    fn observe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = window
            .is_window_active()
            .then(|| window.focused(cx))
            .flatten();
        self.state
            .update(cx, |state, cx| state.star_prompt_focus(focus.is_some(), cx));
        if self.watched.as_ref().map(|(h, _)| h) != focus.as_ref() {
            self.watched = focus.map(|handle| {
                let subscription =
                    cx.on_blur(&handle, window, |this, window, cx| this.observe(window, cx));
                (handle, subscription)
            });
        }
    }
}

pub(crate) fn actions(id: u64, state: &Entity<AppState>, cx: &App) -> Option<gpui::AnyElement> {
    use crate::theme::{hsla, palette};
    use gpui::{
        InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, px,
    };
    let stage = state.read(cx).star_prompt_stage();
    let label = match stage {
        Stage::Prompt => "Star on GitHub",
        Stage::Pending => "Starring…",
        Stage::Fallback => "Open GitHub",
        _ => return None,
    };
    let pending = stage == Stage::Pending;
    let p = palette(cx);
    let dismiss = state.clone();
    let activate = state.clone();
    let dismiss_key = state.clone();
    let activate_key = state.clone();
    Some(
        div()
            .tab_group()
            .flex()
            .justify_end()
            .gap(px(8.))
            .px(px(14.))
            .pb(px(12.))
            .child(
                div()
                    .id(("star-dismiss", id))
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label("Dismiss")
                    .on_key_down(move |event, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                            dismiss_key.update(cx, |s, cx| s.dismiss_toast(id, cx));
                        }
                    })
                    .cursor_pointer()
                    .text_xs()
                    .rounded(px(6.))
                    .px(px(7.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .text_color(hsla(p.foreground))
                    .hover(|d| d.bg(hsla(p.accent)))
                    .child("Dismiss")
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        dismiss.update(cx, |s, cx| s.dismiss_toast(id, cx));
                    }),
            )
            .child(
                div()
                    .id(("star-activate", id))
                    .tab_index(0)
                    .tab_stop(!pending)
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .on_key_down(move |event, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                            activate_key.update(cx, |s, cx| s.activate_star_prompt(cx));
                        }
                    })
                    .text_xs()
                    .rounded(px(6.))
                    .px(px(7.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .bg(hsla(p.primary))
                    .text_color(hsla(p.primary_foreground))
                    .opacity(if pending { 0.64 } else { 1. })
                    .child(label)
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        activate.update(cx, |s, cx| s.activate_star_prompt(cx));
                    }),
            )
            .into_any_element(),
    )
}

#[cfg(target_os = "macos")]
fn embedded_focused() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView};
    use objc2_web_kit::WKWebView;
    let Some(mtm) = MainThreadMarker::new() else {
        return true;
    };
    let Some(responder) = NSApplication::sharedApplication(mtm)
        .keyWindow()
        .and_then(|w| w.firstResponder())
    else {
        return false;
    };
    let mut view = responder.downcast::<NSView>().ok();
    while let Some(current) = view {
        if current.downcast_ref::<WKWebView>().is_some() {
            return true;
        }
        // SAFETY: the key window retains this main-thread view hierarchy during traversal.
        view = unsafe { current.superview() };
    }
    false
}
#[cfg(not(target_os = "macos"))]
fn embedded_focused() -> bool {
    false
}

/// Paint-only listener, so descendant scroll handlers cannot swallow idle activity.
pub(crate) fn activity_listener(state: Entity<AppState>) -> impl gpui::IntoElement {
    use gpui::{Styled, canvas};
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let state = state.clone();
            window.on_mouse_event(move |_: &gpui::ScrollWheelEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    state.update(cx, |state, cx| state.star_prompt_activity(cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(feature = "snapshot")]
impl AppState {
    pub(crate) fn install_star_fixture(&mut self, scenario: &str, cx: &mut Context<Self>) {
        self.github_star.fixture = Some(scenario == "success");
        let p = &mut self.github_star.machine;
        *p = Prompt::default();
        for sent in 1..=6 {
            p.record(Ok((sent, false)));
        }
        p.checked(Some(false));
        p.activity(0, true);
        assert!(p.idle(core::IDLE_MS, true, Ok(false)));
        self.show_toast(
            Toast::new(ToastKind::Info, "Enjoying T3 Code?")
                .description("Give us a star on GitHub to let us know!")
                .github_star(),
            cx,
        );
        match scenario {
            "success" | "fallback" => self.activate_star_prompt(cx),
            "pending" => {
                self.github_star.machine.start_star();
            }
            "dismissed" => {
                self.close_star_prompt(cx);
                self.remove_star_toast(cx);
            }
            "stale" => self.retire_star_prompt(cx),
            "prompt" => {}
            _ => panic!("unknown star fixture"),
        }
        eprintln!(
            "star prompt fixture: {scenario}; stage {:?}; six synthetic sends; no check/star RPC",
            self.github_star.machine.stage
        );
    }
}
