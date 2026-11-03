//! The first-run welcome wizard (ledger CN-WELCOME). When it opens follows
//! `web/src/components/onboarding/FirstRunGate.tsx`, the wizard follows `WelcomeWizard.tsx` in the
//! frame of `ui/wizard.tsx`; the gate and agent readiness are `t3_core::onboarding`, and connecting
//! reuses the connect screen's pairing (`environments.rs`). One environment is connected at a
//! time, so the connection step picks the computer to set up. Pending evidence never blocks the
//! app as the web's blank gate does: the wizard opens over it once a fresh workspace is confirmed.
//! Not here yet: T3 Connect (CN-RELAY) and Codex's ChatGPT setup.

mod agents;
mod agent_terminal;
mod connection;
mod projects;
#[cfg(test)]
mod tests;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, BoxShadow, ClipboardItem, Context, ElementId, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement, KeyBinding,
    ParentElement, Render, Stateful, Styled, Subscription, Task, Window, actions, anchored, deferred, div, point, prelude::*, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use serde_json::json;
use t3_client::supervisor::ConnectionStatus;
use t3_core::client_settings::ClientSettingChange;
use t3_core::onboarding::{
    FirstRunDecision, FirstRunEvidence, SUBSCRIBE_SERVER_LIFECYCLE, ServerWelcome, is_fresh_first_run_workspace, lifecycle_welcome,
    resolve_first_run_decision, resolve_unpaired,
};
use t3_core::shell::ThreadShell;
use t3_core::sidebar::time::format_iso;

use crate::app_state::{AppEvent, AppState, Phase};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, activates, focus_handle, r};
use crate::theme::{hsla, palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Connection,
    Agents,
    Projects,
}

actions!(welcome, [FocusNextInWelcome, FocusPreviousInWelcome]);

const CONTEXT: &str = "WelcomeWizard";

/// Tab and Shift+Tab stay inside the wizard, as the web's modal dialog keeps focus.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNextInWelcome, Some(CONTEXT)),
        KeyBinding::new("shift-tab", FocusPreviousInWelcome, Some(CONTEXT)),
    ]);
}

/// Makes a row a control: a click, or Enter or Space while it has focus, runs `activate`, and Tab
/// reaches it. Keyboard focus paints a 2px `ring` inside the row (design system §4.2, the outline
/// that areas without a fill of their own use), so it never covers the row's contents.
fn key_stop(
    element: Stateful<gpui::Div>,
    id: &ElementId,
    activate: impl Fn(&(), &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<gpui::Div> {
    let focus = focus_handle(id, window, cx);
    let ring = focus.is_focused(window) && window.last_input_was_keyboard();
    let color = hsla(palette(cx).ring);
    let activate = std::rc::Rc::new(activate);
    let on_key = activate.clone();
    element
        .track_focus(&focus)
        .cursor_pointer()
        .when(ring, |this| this.shadow(vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: true }]))
        .on_click(move |event, window, cx| {
            if !event.is_keyboard() {
                activate(&(), window, cx);
            }
        })
        .on_key_down(move |event, window, cx| {
            if activates(event) {
                cx.stop_propagation();
                on_key(&(), window, cx);
            }
        })
}

/// `ONBOARDING_STAGES` up to which is not ported yet.
const STAGES: [&str; 3] = ["Connect", "Agents", "Projects"];

/// The wizard opened or closed; the workspace swaps what it shows.
pub struct Visibility;

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The connected server's welcome, read once per connection while the gate is undecided.
struct WelcomeFetch {
    connection: u64,
    welcome: Option<ServerWelcome>,
    _network: AbortOnDrop,
}

/// The agent whose setup command shows below the cards (`AgentInstallTerminal`'s session).
struct AgentSetup {
    driver: &'static str,
    terminal: Entity<agent_terminal::AgentInstallTerminal>,
    _closed: Subscription,
    _changed: Subscription,
}

pub struct WelcomeWizard {
    state: Entity<AppState>,
    open: bool,
    step: Step,
    focus: FocusHandle,
    focus_pending: bool,
    welcome: Option<WelcomeFetch>,
    link: Entity<InputState>,
    /// "Add a computer": `None` until toggled, open while there is nothing else to connect to.
    pairing_open: Option<bool>,
    pairing_help_open: bool,
    /// A pasted link is pairing; the field clears once it succeeds.
    submitted: bool,
    setup: Option<AgentSetup>,
    terminal_access: bool,
    terminal_grants: Option<(u64, Task<()>)>,
    copied: Option<(String, Task<()>)>,
    projects: projects::Projects,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Visibility> for WelcomeWizard {}

fn now_iso() -> String {
    format_iso(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64))
}

impl WelcomeWizard {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| InputState::new(window, cx).placeholder("https://your-server:5230/pair#token=…"));
        let subscriptions = vec![
            cx.subscribe_in(&link, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => this.pair(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| this.on_app_event(*event, window, cx)),
        ];
        let mut wizard = WelcomeWizard {
            state,
            open: false,
            step: Step::Connection,
            focus: cx.focus_handle(),
            focus_pending: false,
            welcome: None,
            link,
            pairing_open: None,
            pairing_help_open: false,
            submitted: false,
            setup: None,
            terminal_access: false,
            terminal_grants: None,
            copied: None,
            projects: projects::Projects::default(),
            _subscriptions: subscriptions,
        };
        wizard.watch_terminal_grants(cx);
        wizard.evaluate(cx);
        #[cfg(feature = "snapshot")]
        cx.set_global(Host(cx.weak_entity()));
        wizard
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Moves to the next or previous control inside the wizard (the dialog's focus trap).
    fn cycle_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Bounded: every control in the window is at most one step each.
        for _ in 0..512 {
            if forward {
                window.focus_next(cx);
            } else {
                window.focus_prev(cx);
            }
            if self.focus.contains_focused(window, cx) && !self.focus.is_focused(window) {
                return;
            }
        }
        window.focus(&self.focus, cx);
    }

    fn on_app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(event, AppEvent::Phase | AppEvent::Shell | AppEvent::ServerConfig) {
            self.projects_changed(cx);
        }
        if event == AppEvent::Phase {
            self.watch_terminal_grants(cx);
            let app = self.state.read(cx);
            let (pairing, failed, unpaired) = (app.connect.pairing.is_some(), app.connect.error.is_some(), app.phase == Phase::Unpaired);
            if self.submitted && !pairing {
                self.submitted = false;
                if !failed {
                    self.link.update(cx, |input, cx| input.set_value("", window, cx));
                    self.pairing_open = Some(false);
                }
            }
            if unpaired && self.step != Step::Connection {
                self.step = Step::Connection;
            }
        }
        if matches!(event, AppEvent::Phase | AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Selection) {
            self.evaluate(cx);
        }
        if self.open && matches!(event, AppEvent::Phase | AppEvent::ServerConfig) {
            cx.notify();
        }
    }

    /// `FirstRunGate`: opens the wizard for a first launch or a confirmed fresh workspace, and
    /// saves onboarding as done once the server confirms an established one.
    fn evaluate(&mut self, cx: &mut Context<Self>) {
        if self.open || crate::settings::client_settings(cx).onboarding_completed() {
            self.welcome = None;
            return;
        }
        let app = self.state.read(cx);
        let Phase::Online(status) = &app.phase else {
            if resolve_unpaired(false, app.environments.environments.len()) == FirstRunDecision::Wizard {
                self.show(Step::Connection, cx);
            }
            return;
        };
        let connected = *status == ConnectionStatus::Connected;
        let connection = app.connection_id();
        // Setup opens by itself only from home, so it never takes over an open thread.
        let at_home = app.selected_thread.is_none() && !app.pull_requests_open;
        let config = app.server_config.config.as_ref();
        let (projects, threads): (&[_], Vec<&ThreadShell>) = match app.shell.snapshot.as_ref() {
            Some(snapshot) => (&snapshot.projects, snapshot.threads.iter().chain(&snapshot.archived_threads).collect()),
            None => (&[], Vec::new()),
        };
        let welcome = self.welcome.as_ref().filter(|fetch| Some(fetch.connection) == connection).and_then(|fetch| fetch.welcome.as_ref());
        let cwd = config.map(|config| config.cwd.as_str()).filter(|cwd| !cwd.is_empty());
        let evidence = FirstRunEvidence {
            completed: false,
            live: connected && app.shell.synchronized,
            provenance_authoritative: welcome.is_some(),
            server_config_available: config.is_some(),
            workspace_fresh: threads.len() <= 1 && is_fresh_first_run_workspace(cwd, welcome.unwrap_or(&ServerWelcome::default()), projects, &threads),
            project_count: projects.len(),
            thread_count: threads.len(),
        };
        let resolution = resolve_first_run_decision(&evidence);
        if resolution.persist_completion {
            return self.complete(cx);
        }
        match (resolution.decision, connection) {
            (FirstRunDecision::Wizard, _) if at_home => self.show(Step::Connection, cx),
            (FirstRunDecision::Pending, Some(connection)) if connected => self.fetch_welcome(connection, cx),
            _ => {}
        }
    }

    /// Reads `subscribeServerLifecycle` until the welcome arrives, then lets the stream go.
    fn fetch_welcome(&mut self, connection: u64, cx: &mut Context<Self>) {
        if self.welcome.as_ref().is_some_and(|fetch| fetch.connection == connection) {
            return;
        }
        let app = self.state.read(cx);
        let Some(client) = app.client_handle() else { return };
        let network = app.runtime().spawn(async move {
            let mut stream = client.stream(SUBSCRIBE_SERVER_LIFECYCLE, json!({})).await.ok()?;
            while let Some(Ok(event)) = stream.receiver.recv().await {
                if let Some(welcome) = lifecycle_welcome(&event).filter(ServerWelcome::is_authoritative) {
                    return Some(welcome);
                }
            }
            None
        });
        self.welcome = Some(WelcomeFetch { connection, welcome: None, _network: AbortOnDrop(network.abort_handle()) });
        cx.spawn(async move |this, cx| {
            let welcome = network.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                if !this.welcome.as_ref().is_some_and(|fetch| fetch.connection == connection) {
                    return;
                }
                match welcome {
                    Some(welcome) => {
                        if let Some(fetch) = this.welcome.as_mut() {
                            fetch.welcome = Some(welcome);
                        }
                        this.evaluate(cx);
                    }
                    // The stream ended with the connection; ask again once it is back.
                    None => this.welcome = None,
                }
            });
        })
        .detach();
    }

    fn show(&mut self, step: Step, cx: &mut Context<Self>) {
        self.open = true;
        self.step = step;
        self.focus_pending = true;
        self.welcome = None;
        cx.emit(Visibility);
        cx.notify();
    }

    /// `completeOnboarding`.
    fn complete(&mut self, cx: &mut Context<Self>) {
        self.welcome = None;
        crate::settings::store::update_client(&self.state, ClientSettingChange::OnboardingCompletedAt(now_iso()), cx);
    }

    /// Closing or finishing the wizard: setup is done either way, as on the web.
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.cancel_projects();
        self.open = false;
        self.setup = None;
        self.complete(cx);
        cx.emit(Visibility);
        cx.notify();
    }

    fn go_to(&mut self, step: Step, cx: &mut Context<Self>) {
        self.cancel_projects();
        if step == Step::Agents && self.step != Step::Agents {
            self.refresh_providers(cx);
        }
        self.step = step;
        if step == Step::Projects {
            self.enter_projects(cx);
        }
        self.setup = None;
        self.focus_pending = true;
        cx.notify();
    }

    fn pair(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().trim().to_owned();
        if link.is_empty() || self.state.read(cx).connect.pairing.is_some() {
            return;
        }
        self.state.update(cx, |state, cx| state.pair_link(&link, cx));
        self.submitted = self.state.read(cx).connect.pairing.is_some();
    }

    fn copy(&mut self, command: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(command.to_owned()));
        let reset = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            });
        });
        self.copied = Some((command.to_owned(), reset));
        cx.notify();
    }

    /// `CommandBlock`: a command in mono with a Copy button.
    fn command_block(&self, id: &'static str, command: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let copied = self.copied.as_ref().is_some_and(|(copied, _)| copied == command);
        let text = command.to_owned();
        h_flex()
            .items_center()
            .justify_between()
            .gap(r(12.))
            .px(r(12.))
            .py(r(10.))
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.muted).opacity(0.5))
            .font_family(cx.theme().mono_font_family.clone())
            .text_sm()
            .child(h_flex().min_w_0().overflow_hidden().child(div().mr(r(8.)).text_color(hsla(p.muted_foreground)).child("$")).child(div().truncate().child(text.clone())))
            .child(
                Button::new(id, ButtonVariant::Ghost, ButtonSize::IconXs)
                    .icon(if copied { Icon::Check } else { Icon::Copy })
                    .tooltip("Copy command")
                    .on_click(cx.listener(move |this, _, _, cx| this.copy(&text, cx))),
            )
    }

    /// `WizardSteps`: done steps take you back; the current and later ones are inert.
    fn render_steps(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let ink = hsla(p.foreground);
        let current = match self.step {
            Step::Connection => 0,
            Step::Agents => 1,
            Step::Projects => 2,
        };
        h_flex()
            .w_full()
            .gap(r(4.))
            .p(r(4.))
            .rounded(r(12.))
            .border_1()
            .border_color(ink.opacity(0.05))
            .bg(if dark { ink.opacity(0.04) } else { hsla(p.background) })
            .children(STAGES.iter().enumerate().map(|(index, label)| {
                let (done, here) = (index < current, index == current);
                let (badge_fill, badge_text, badge_ring) = if done {
                    (hsla(p.primary), hsla(p.primary_foreground), hsla(p.primary))
                } else if here {
                    (hsla(p.primary).opacity(0.1), hsla(p.primary), hsla(p.primary).opacity(0.3))
                } else {
                    (if dark { ink.opacity(0.05) } else { hsla(p.card) }, hsla(p.muted_foreground), ink.opacity(0.1))
                };
                let badge = div()
                    .size(r(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(badge_ring)
                    .bg(badge_fill)
                    .text_color(badge_text)
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .map(|this| if done { this.child(Icon::Check.render(px(16.), badge_text)) } else { this.child((index + 1).to_string()) });
                let id = ElementId::from(("welcome-step", index));
                let tab = h_flex()
                    .id(id.clone())
                    .flex_1()
                    .min_w_0()
                    .gap(r(8.))
                    .px(r(10.))
                    .py(r(8.))
                    .rounded(r(8.))
                    .border_1()
                    .border_color(if here { ink.opacity(0.05) } else { gpui::transparent_black() })
                    .when(here, |this| {
                        this.bg(hsla(p.card)).when(!dark, |this| {
                            this.shadow(vec![BoxShadow { color: gpui::black().opacity(0.05), offset: point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false }])
                        })
                    })
                    .child(badge)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if here { hsla(p.foreground) } else { hsla(p.muted_foreground) })
                            .child(*label),
                    );
                if done {
                    let back = cx.listener(move |this, _: &(), _, cx| this.go_to(if index == 0 { Step::Connection } else { Step::Agents }, cx));
                    key_stop(tab, &id, back, window, cx).hover(move |style| style.bg(hsla(p.card)))
                } else {
                    tab
                }
            }))
    }
}

/// `StepShell`'s title and description.
fn step_title(title: &'static str, description: &'static str, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    v_flex()
        .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(title))
        .child(div().mt(r(10.)).text_sm().line_height(r(22.75)).text_color(hsla(p.muted_foreground)).child(description))
}

/// The bordered row the wizard lists computers and agents in (`rounded-lg border bg-background`).
fn row_card(cx: &App) -> gpui::Div {
    let p = palette(cx);
    h_flex().items_center().rounded(r(8.)).border_1().border_color(hsla(p.border)).bg(hsla(p.background))
}

impl Render for WelcomeWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.focus, cx);
        }
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let ink = hsla(p.foreground);
        let viewport = window.viewport_size();
        let body: AnyElement = match self.step {
            Step::Connection => self.render_connection(window, cx),
            Step::Agents => self.render_agents(window, cx),
            Step::Projects => self.render_projects(window, cx),
        };
        let header = v_flex()
            .gap(r(8.))
            .p(r(24.))
            .pb(r(12.))
            .child(
                h_flex()
                    .items_baseline()
                    .gap(r(6.))
                    .text_2xl()
                    .child(div().font_weight(FontWeight::BOLD).text_color(ink).child("T3"))
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Code")),
            )
            .child(self.render_steps(window, cx));
        let panel = div()
            .px(r(24.))
            .py(r(20.))
            .border_t_1()
            .border_color(ink.opacity(0.05))
            .rounded_b(r(15.))
            .bg(if dark { ink.opacity(0.02) } else { hsla(p.background).opacity(0.8) })
            .child(body);
        let dialog = v_flex()
            .id("welcome-wizard")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &FocusNextInWelcome, window, cx| this.cycle_focus(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPreviousInWelcome, window, cx| this.cycle_focus(false, window, cx)))
            .occlude()
            .relative()
            .w_full()
            .max_w(r(768.))
            .max_h(viewport.height - px(32.))
            .rounded(r(16.))
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(if dark { 0.9 } else { 0.65 }),
                offset: point(px(0.), px(24.)),
                blur_radius: px(64.),
                spread_radius: px(-24.),
                inset: false,
            }])
            .text_color(hsla(p.popover_foreground))
            .child(div().id("welcome-wizard-scroll").flex_1().min_h_0().overflow_y_scroll().child(header).child(panel))
            .child(
                div().absolute().top(r(8.)).right(r(8.)).child(
                    Button::new("welcome-close", ButtonVariant::Ghost, ButtonSize::Icon)
                        .icon(Icon::X)
                        .tooltip("Close")
                        .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                ),
            );
        deferred(
            anchored().position_mode(gpui::AnchoredPositionMode::Window).position(point(px(0.), px(0.))).child(
                div()
                    .id("welcome-wizard-backdrop")
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(16.))
                    .bg(hsla(p.dialog_backdrop))
                    .child(dialog),
            ),
        )
        .with_priority(50)
        .into_any_element()
    }
}

#[cfg(feature = "snapshot")]
struct Host(gpui::WeakEntity<WelcomeWizard>);

#[cfg(feature = "snapshot")]
impl gpui::Global for Host {}

/// Opens the wizard at `step` regardless of the gate, for offscreen captures.
#[cfg(feature = "snapshot")]
pub fn open(step: Step, cx: &mut App) {
    if let Some(wizard) = cx.try_global::<Host>().and_then(|host| host.0.upgrade()) {
        wizard.update(cx, |wizard, cx| {
            wizard.show(Step::Connection, cx);
            if step != Step::Connection {
                wizard.go_to(step, cx);
            }
        });
    }
}

#[cfg(feature = "snapshot")]
pub use agent_terminal::Fixture as TerminalFixture;

#[cfg(feature = "snapshot")]
pub fn terminal_fixture(fixture: TerminalFixture, window: &mut Window, cx: &mut App) -> Result<(), String> {
    let wizard = cx
        .try_global::<Host>()
        .and_then(|host| host.0.upgrade())
        .ok_or("no welcome wizard")?;
    wizard.update(cx, |wizard, cx| {
        let app = wizard.state.read(cx);
        let client = app
            .client_handle()
            .ok_or("no connection for terminal fixture")?;
        wizard.terminal_access = fixture.can_operate;
        let terminal =
            agent_terminal::AgentInstallTerminal::fixture(fixture, client, app.runtime().clone())?;
        let terminal = cx.new(|_| terminal);
        terminal.update(cx, |terminal, cx| terminal.prepare_pane(window, cx));
        let closed = cx.subscribe(&terminal, |this, _, _: &agent_terminal::Closed, cx| {
            this.setup = None;
            this.refresh_providers(cx);
            cx.notify();
        });
        let changed = cx.observe(&terminal, |_, _, cx| cx.notify());
        wizard.setup = Some(AgentSetup {
            driver: "codex",
            terminal,
            _closed: closed,
            _changed: changed,
        });
        cx.notify();
        Ok(())
    })
}
