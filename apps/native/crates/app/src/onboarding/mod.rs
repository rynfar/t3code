//! The first-run welcome wizard (ledger CN-WELCOME). When it opens follows
//! `web/src/components/onboarding/FirstRunGate.tsx`, the wizard follows `WelcomeWizard.tsx` in the
//! frame of `ui/wizard.tsx`; the gate and agent readiness are `t3_core::onboarding`, and connecting
//! reuses the connect screen's pairing (`environments.rs`). One environment is connected at a
//! time, so the connection step picks the computer to set up. Pending evidence never blocks the
//! app as the web's blank gate does: the wizard opens over it once a fresh workspace is confirmed.
//! Not here yet: T3 Connect (CN-RELAY) and simultaneous multi-computer selection.

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
    _finished: Option<Subscription>,
}

pub struct WelcomeWizard {
    chatgpt: std::collections::HashMap<String, Entity<crate::chatgpt_setup::ChatGptSetup>>,
    created_chatgpt: Option<String>,
    chatgpt_subscriptions: Vec<Subscription>,
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
    /// Whether the "More agents" disclosure is expanded on the single Welcome page (`#33209`).
    more_agents_open: bool,
    /// Local `t3` CLI installation / PATH status (`#33150`, `#33153`).
    cli_status: Option<t3_core::onboarding::CliCommandStatus>,
    cli_path_added: bool,
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
            chatgpt: Default::default(),created_chatgpt:None,chatgpt_subscriptions:Vec::new(),
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
            more_agents_open: false,
            cli_status: t3_client::local::cli_command_status(),
            cli_path_added: false,
            terminal_access: false,
            terminal_grants: None,
            copied: None,
            projects: projects::Projects::default(),
            _subscriptions: subscriptions,
        };
        wizard.watch_terminal_grants(cx);
        wizard.evaluate(cx);
        cx.set_global(Host(cx.weak_entity()));
        if !crate::chatgpt_setup::fixture_active(cx)
            && let Some(environment)=t3_client::store::load_ui_state("welcome-resume").and_then(|v|v.as_str().map(str::to_owned))
        {
            let matches=wizard.state.read(cx).active_environment().is_some_and(|e|e.environment_id.as_deref()==Some(&environment)||e.id==environment);
            if matches {wizard.show(Step::Agents,cx);}
        }
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
            self.chatgpt.clear();self.created_chatgpt=None;self.chatgpt_subscriptions.clear();
            self.watch_terminal_grants(cx);
            let (pairing, failed, unpaired, connected) = {
                let app = self.state.read(cx);
                (
                    app.connect.pairing.is_some(),
                    app.connect.error.is_some(),
                    app.phase == Phase::Unpaired,
                    app.phase == Phase::Online(ConnectionStatus::Connected),
                )
            };
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
            if self.open && connected {
                self.refresh_providers(cx);
                self.enter_projects(cx);
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
        self.cli_status = t3_client::local::cli_command_status();
        if self.state.read(cx).phase == Phase::Online(ConnectionStatus::Connected) {
            self.refresh_providers(cx);
            self.enter_projects(cx);
        }
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
        self.chatgpt.clear();self.created_chatgpt=None;self.chatgpt_subscriptions.clear();
        if !crate::chatgpt_setup::fixture_active(cx) {t3_client::store::save_ui_state("welcome-resume", &serde_json::Value::Null);}
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
        if !crate::chatgpt_setup::fixture_active(cx) {
            let environment=(step==Step::Agents).then(||self.state.read(cx).active_environment().map(|e|e.environment_id.clone().unwrap_or_else(||e.id.clone()))).flatten();
            t3_client::store::save_ui_state("welcome-resume", &environment);
        }
        if step != Step::Agents { self.chatgpt.clear();self.created_chatgpt=None;self.chatgpt_subscriptions.clear(); }
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

    /// Appends the `t3` PATH export line to the user's shell profile if not already present
    /// (`DesktopCliCommand.ts` `addToPath`, `pingdotgg/t3code#33153`).
    fn add_cli_to_path(&mut self, profile: &str, export_line: &str, cx: &mut Context<Self>) {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return };
        let path = profile
            .strip_prefix("~/")
            .map_or_else(|| std::path::PathBuf::from(profile), |rest| home.join(rest));
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.contains(export_line) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let prefix = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
            let next = format!("{existing}{prefix}{export_line}\n");
            if let Err(error) = std::fs::write(&path, next) {
                tracing::warn!(%error, path = %path.display(), "failed to update shell profile PATH");
                return;
            }
        }
        self.cli_path_added = true;
        cx.notify();
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

}

/// Section heading inside the single Welcome page (`WelcomePage.tsx`, `pingdotgg/t3code#33209`).
fn section_heading(title: &str, description: &str, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    v_flex()
        .gap(r(2.))
        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(title.to_owned()))
        .child(div().text_xs().line_height(r(18.)).text_color(hsla(p.muted_foreground)).child(description.to_owned()))
}

/// The bordered row the welcome page lists computers and agents in (`rounded-lg border bg-background`).
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
        let connected = self.state.read(cx).phase == Phase::Online(ConnectionStatus::Connected);

        // Single scrollable Welcome page (`WelcomePage.tsx`, `pingdotgg/t3code#33209`):
        // replaces the 3-step stepper with unified sections on one page.
        let mut sections: Vec<AnyElement> = Vec::new();
        if connected {
            sections.push(self.render_agents(window, cx));
            sections.push(self.render_projects(window, cx));
            sections.push(self.render_connection(window, cx));
        } else {
            sections.push(self.render_connection(window, cx));
        }
        if let Some(cli) = self.render_cli_setup(cx) {
            sections.push(cli);
        }
        sections.push(self.render_welcome_footer(cx));

        let header = v_flex()
            .gap(r(4.))
            .p(r(24.))
            .pb(r(16.))
            .child(
                h_flex()
                    .items_baseline()
                    .gap(r(6.))
                    .text_2xl()
                    .child(div().font_weight(FontWeight::BOLD).text_color(ink).child("T3"))
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Code")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child("Set up your agents, import projects, and configure the command line in one place."),
            );
        let panel = v_flex()
            .gap(r(24.))
            .px(r(24.))
            .py(r(20.))
            .border_t_1()
            .border_color(ink.opacity(0.05))
            .rounded_b(r(15.))
            .bg(if dark { ink.opacity(0.02) } else { hsla(p.background).opacity(0.8) })
            .children(sections);
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

struct Host(gpui::WeakEntity<WelcomeWizard>);

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
        wizard.setup = Some(AgentSetup {
            driver: "codex",
            terminal,
            _closed: closed,
            _finished: None,
        });
        cx.notify();
        Ok(())
    })
}

/// Native has no browser route to unload; a return link or restart resumes its Agents step.
pub fn resume(environment: String, cx: &mut App) {
    if let Some(wizard)=cx.try_global::<Host>().and_then(|h|h.0.upgrade()) {
        wizard.update(cx,|wizard,cx| {
            let saved=wizard.state.read(cx).environments.environments.iter().find(|e|e.environment_id.as_deref()==Some(&environment)||e.id==environment).map(|e|e.id.clone());
            if let Some(id)=saved {wizard.state.update(cx,|app,cx|app.connect_environment(&id,cx));wizard.show(Step::Agents,cx);}
        });
    }
}
