//! `AgentInstallTerminal` from `web/src/components/onboarding/WelcomeWizard.tsx`.
//! A serial queue keeps cleanup behind the pending open/write and ahead of a retry.

use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, Focusable, FontWeight, IntoElement, ParentElement,
    Render, Styled, Subscription, Task, Window, div,
};
use gpui_component::{h_flex, v_flex};
use t3_client::supervisor::ClientHandle;
use t3_core::onboarding::terminal::{self as model, Setup, SetupState};
use t3_core::terminal::{LaunchLocation, TerminalAccess, method};
use tokio::sync::mpsc;

use crate::settings::controls::{Button, ButtonSize, ButtonVariant, r};
use crate::terminal::{PaneEvent, Target, TerminalPane};
use crate::theme::{hsla, palette};

pub(super) struct Closed;

enum Operation {
    Begin(u64),
    Close,
}

pub(super) struct AgentInstallTerminal {
    model: Arc<Mutex<Setup>>,
    command: String,
    terminal_id: String,
    cwd: String,
    provider_instance_id: Option<String>,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
    queue: Option<mpsc::UnboundedSender<Operation>>,
    pane: Option<Entity<TerminalPane>>,
    pane_events: Option<Subscription>,
    focus_pending: bool,
    _pump: Option<Task<()>>,
    #[cfg(feature = "snapshot")]
    permission_fixture: Option<bool>,
}

impl EventEmitter<Closed> for AgentInstallTerminal {}

pub(super) fn can_operate(client: &ClientHandle) -> bool {
    TerminalAccess::of(client.session_state().borrow().as_ref()).operate
}

impl AgentInstallTerminal {
    pub fn new(
        session: model::Session,
        client: ClientHandle,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let model::Session {
            driver,
            command,
            cwd,
            provider_instance_id,
        } = session;
        let terminal_id = model::terminal_id(&driver, &uuid::Uuid::new_v4().to_string());
        let model = Arc::new(Mutex::new(Setup::default()));
        let (queue, mut operations) = mpsc::unbounded_channel();
        let (changed, mut changes) = mpsc::unbounded_channel();
        let (setup, target, text, network) = (
            model.clone(),
            terminal_id.clone(),
            command.clone(),
            client.clone(),
        );
        let directory = cwd.clone();
        let pane_provider_instance_id = Some(provider_instance_id.clone());
        runtime.spawn(async move {
            while let Some(operation) = operations.recv().await {
                match operation {
                    Operation::Close => {
                        if let Some(input) = model::close_input(&target, can_operate(&network)) {
                            let _ = network.request(method::CLOSE, input).await;
                        }
                    }
                    Operation::Begin(generation) => {
                        let open = {
                            let mut setup = setup.lock().unwrap();
                            setup.can_open(generation, can_operate(&network)).then(|| {
                                network.request(
                                    method::OPEN,
                                    model::open_input(&target, &directory, &provider_instance_id),
                                )
                            })
                        };
                        let Some(open) = open else {
                            let _ = changed.send(());
                            continue;
                        };
                        let opened = open.await.is_ok();
                        let write = {
                            let mut setup = setup.lock().unwrap();
                            setup
                                .opened(generation, opened, can_operate(&network))
                                .then(|| {
                                    network
                                        .request(method::WRITE, model::write_input(&target, &text))
                                })
                        };
                        if let Some(write) = write {
                            let wrote = write.await.is_ok();
                            setup.lock().unwrap().wrote(generation, wrote);
                        }
                        let _ = changed.send(());
                    }
                }
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while changes.recv().await.is_some() {
                if this.update_in(cx, |this, window, cx| {
                    this.prepare_pane(window, cx);
                    cx.notify();
                }).is_err() {
                    return;
                }
            }
        });
        let mut terminal = Self {
            model,
            command,
            terminal_id,
            cwd,
            provider_instance_id: pane_provider_instance_id,
            client,
            runtime,
            queue: Some(queue),
            pane: None,
            pane_events: None,
            focus_pending: true,
            _pump: Some(pump),
            #[cfg(feature = "snapshot")]
            permission_fixture: None,
        };
        terminal.begin();
        terminal
    }

    fn begin(&mut self) {
        let generation = self.model.lock().unwrap().begin();
        if let Some(queue) = &self.queue {
            let _ = queue.send(Operation::Begin(generation));
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if !self.can_operate() || self.model.lock().unwrap().state != SetupState::OpenFailed {
            return;
        }
        self.model.lock().unwrap().cancel();
        if let Some(queue) = &self.queue {
            let _ = queue.send(Operation::Close);
        }
        self.pane_events = None;
        self.pane = None;
        self.focus_pending = true;
        self.begin();
        cx.notify();
    }

    fn can_operate(&self) -> bool {
        #[cfg(feature = "snapshot")]
        if let Some(permission) = self.permission_fixture {
            return permission;
        }
        can_operate(&self.client)
    }

    fn message(&self, state: SetupState, allowed: bool, cx: &gpui::App) -> AnyElement {
        let p = palette(cx);
        let message = div()
            .flex_1()
            .min_w_0()
            .text_size(r(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(hsla(p.muted_foreground));
        if state == SetupState::WriteFailed && allowed {
            use gpui_component::ActiveTheme;
            message
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_x(r(4.))
                        .child("Run")
                        .child(
                            div()
                                .rounded(r(4.))
                                .bg(hsla(p.muted))
                                .px(r(4.))
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(self.command.clone()),
                        )
                        .child("in this terminal."),
                )
                .into_any_element()
        } else {
            message
                .child(state.message(&self.command, allowed))
                .into_any_element()
        }
    }

    pub(super) fn prepare_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.model.lock().unwrap().state.terminal_ready() {
            return;
        }
        if self.pane.is_none() {
            let target = Target {
                thread_id: model::THREAD_ID.to_owned(),
                terminal_id: self.terminal_id.clone(),
                location: LaunchLocation {
                    cwd: self.cwd.clone(),
                    worktree_path: None,
                    env: Default::default(),
                    provider_instance_id: self.provider_instance_id.clone(),
                },
            };
            let pane = cx.new(|cx| {
                TerminalPane::new(
                    target,
                    self.client.clone(),
                    self.runtime.clone(),
                    window,
                    cx,
                )
            });
            self.pane_events = Some(cx.subscribe(&pane, |_, _, event: &PaneEvent, cx| {
                if *event == PaneEvent::Exited {
                    cx.emit(Closed);
                }
            }));
            self.pane = Some(pane);
        }
        if std::mem::take(&mut self.focus_pending)
            && let Some(pane) = &self.pane
        {
            window.focus(&pane.focus_handle(cx), cx);
        }
    }
}

impl Drop for AgentInstallTerminal {
    fn drop(&mut self) {
        self.model.lock().unwrap().cancel();
        if let Some(queue) = &self.queue {
            let _ = queue.send(Operation::Close);
        }
    }
}

impl Render for AgentInstallTerminal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let state = self.model.lock().unwrap().state;
        let allowed = self.can_operate();
        v_flex()
            .mt(r(16.))
            .rounded(r(8.))
            .overflow_hidden()
            .border_1()
            .border_color(hsla(p.border).opacity(0.7))
            .bg(hsla(p.background))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(r(12.))
                    .px(r(12.))
                    .py(r(6.))
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.background).opacity(0.6))
                    .child(self.message(state, allowed, cx))
                    .child(
                        h_flex()
                            .gap(r(4.))
                            .when(state == SetupState::OpenFailed, |row| {
                                row.child(
                                    Button::new(
                                        "welcome-agent-setup-retry",
                                        ButtonVariant::Ghost,
                                        ButtonSize::Xs,
                                    )
                                    .label("Retry")
                                    .disabled(!allowed)
                                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                                )
                            })
                            .child(
                                Button::new(
                                    "welcome-agent-setup-close",
                                    ButtonVariant::GhostMuted,
                                    ButtonSize::Xs,
                                )
                                .label("Close")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                            ),
                    ),
            )
            .child(div().h(r(256.)).children(self.pane.clone()))
    }
}

/// Fixtures never open or pre-type. Ready states may attach to an explicitly supplied smoke PTY.
#[cfg(feature = "snapshot")]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fixture {
    state: SetupState,
    command: String,
    cwd: String,
    terminal_id: Option<String>,
    provider_instance_id: Option<String>,
    pub can_operate: bool,
}

#[cfg(feature = "snapshot")]
impl AgentInstallTerminal {
    pub fn fixture(
        fixture: Fixture,
        client: ClientHandle,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        if fixture.state.terminal_ready() && fixture.terminal_id.is_none() {
            return Err("ready fixtures require an existing smoke terminalId".to_owned());
        }
        let mut model = Setup::default();
        model.state = fixture.state;
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            command: fixture.command,
            terminal_id: fixture
                .terminal_id
                .unwrap_or_else(|| "onboarding-fixture".to_owned()),
            cwd: fixture.cwd,
            provider_instance_id: fixture.provider_instance_id,
            client,
            runtime,
            queue: None,
            pane: None,
            pane_events: None,
            focus_pending: true,
            _pump: None,
            permission_fixture: Some(fixture.can_operate),
        })
    }
}
