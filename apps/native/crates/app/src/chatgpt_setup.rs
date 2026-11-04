//! Shared Settings/onboarding presentation of `CodexSetupSection.tsx`, account picker,
//! `AddCodexAccountDialog.tsx` and `ChatGptWelcomeCoordinator.tsx`.
mod dialog_focus;
use crate::{
    app_state::{AppEvent, AppState},
    icons::Icon,
    provider_icon::{ProviderIcon, ProviderMark},
    settings::controls::{Button, ButtonSize, ButtonVariant, DialogFrame, Row, dialog_footer, r},
    theme::{hsla, palette},
};
use gpui::{prelude::*, *};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputState},
    v_flex,
};
use serde_json::{Value, json};
use t3_core::{
    server_config::ServerProvider,
    settings::providers::{self as providers, auth, chatgpt as model},
};

pub fn init(cx: &mut App) {
    dialog_focus::init(cx);
}

pub struct AccountCreated(pub String);
impl EventEmitter<AccountCreated> for ChatGptSetup {}

struct Network(tokio::task::AbortHandle);
impl Drop for Network {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(feature = "snapshot")]
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fixture {
    pub provider: ServerProvider,
    #[serde(default = "fixture_managed")]
    pub managed: bool,
    #[serde(default)]
    pub other_accounts: Vec<ServerProvider>,
    pub auth: auth::AuthState,
    pub install: auth::InstallState,
    #[serde(default)]
    pub handoff: bool,
    #[serde(default)]
    pub picker: bool,
    #[serde(default)]
    pub add_account: bool,
    #[serde(default)]
    pub welcome: bool,
}
#[cfg(feature = "snapshot")]
fn fixture_managed() -> bool {
    true
}
#[cfg(feature = "snapshot")]
impl Global for Fixture {}
pub fn fixture_active(_cx: &App) -> bool {
    #[cfg(feature = "snapshot")]
    {
        _cx.try_global::<Fixture>().is_some()
    }
    #[cfg(not(feature = "snapshot"))]
    {
        false
    }
}

pub struct ChatGptSetup {
    app: Entity<AppState>,
    instance: String,
    connection: Option<u64>,
    onboarding: bool,
    pub state: model::SetupState,
    streams: Vec<Network>,
    operation: Option<Network>,
    serial: u64,
    retry: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    picker: bool,
    selection: String,
    help: bool,
    revealed: bool,
    callback: Entity<InputState>,
    callback_flow: Option<String>,
    account_name: Entity<InputState>,
    adding: bool,
    added: Option<Entity<ChatGptSetup>>,
    welcome: bool,
    auto_start: bool,
    created_instance: Option<String>,
    acknowledged: Option<String>,
    _subscription: Subscription,
    _inputs: Vec<Subscription>,
    dialog_scope: dialog_focus::Scope,
    observed_managed: Option<bool>,
    watch_generation: u64,
}
impl ChatGptSetup {
    pub fn new(app: Entity<AppState>, instance: String, onboarding: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let connection = app.read(cx).connection_id();
        let subscription = cx.subscribe(&app, |this, _, event: &AppEvent, cx| {
            if *event == AppEvent::Phase && this.connection != this.app.read(cx).connection_id() {
                this.streams.clear();
                this.operation = None;
                this.serial += 1;
            }
            if *event == AppEvent::Phase && this.connection == this.app.read(cx).connection_id() && !fixture_active(cx) {
                this.streams.clear();
                this.observed_managed = None;
                this.sync_watch(cx);
            }
            if matches!(event, AppEvent::ServerConfig | AppEvent::Phase) {
                this.sync_watch(cx);
                if let Some(provider) = this.provider(cx)
                    && provider.auth.status == "authenticated"
                {
                    this.state.finishing = false;
                    let key = provider.auth.email.clone().unwrap_or_else(|| "default".into());
                    if provider.auth.subscription_sharing == Some(true) && this.acknowledged.as_ref() != Some(&key) {
                        this.acknowledged = Some(key.clone());
                        let identity = json!([this.app.read(cx).active_environment().map(|e| e.id.as_str()), this.instance, key]);
                        let seen = t3_client::store::load_ui_state("chatgpt-sharing-welcome").and_then(|v| v.as_array().cloned()).unwrap_or_default();
                        this.welcome = !seen.contains(&identity);
                    }
                }
                cx.notify();
            }
        });
        let account_name = cx.new(|cx| InputState::new(window, cx).default_value("Personal").placeholder("e.g. Personal or Work"));
        let callback = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste the URL from the sign-in tab"));
        let inputs = vec![
            cx.subscribe_in(&account_name, window, |this, _, event: &gpui_component::input::InputEvent, window, cx| {
                if matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) && this.adding {
                    this.create_account(window, cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(&callback, window, |this, _, event: &gpui_component::input::InputEvent, window, cx| {
                if matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) {
                    this.submit_callback(window, cx);
                }
            }),
        ];
        let mut view = Self {
            app,
            instance,
            connection,
            onboarding,
            state: Default::default(),
            streams: Vec::new(),
            operation: None,
            serial: 0,
            retry: None,
            picker: false,
            selection: String::new(),
            help: false,
            revealed: false,
            callback_flow: None,
            callback,
            account_name,
            adding: false,
            added: None,
            welcome: false,
            auto_start: false,
            created_instance: None,
            acknowledged: None,
            _subscription: subscription,
            _inputs: inputs,
            dialog_scope: dialog_focus::Scope::new(cx),
            observed_managed: None,
            watch_generation: 0,
        };
        #[cfg(feature = "snapshot")]
        if let Some(fixture) = cx.try_global::<Fixture>().cloned() {
            view.state.auth = Some(fixture.auth);
            view.state.requested_method = "chatgpt".into();
            view.state.install = fixture.managed.then_some(fixture.install);
            view.state.handoff = fixture.handoff;
            view.picker = fixture.picker;
            view.adding = fixture.add_account;
            view.welcome = fixture.welcome;
            return view;
        }
        view.sync_watch(cx);
        view
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }
    fn provider(&self, cx: &App) -> Option<ServerProvider> {
        #[cfg(feature = "snapshot")]
        if let Some(fixture) = cx.try_global::<Fixture>() {
            return Some(fixture.other_accounts.iter().find(|p| p.instance_id == self.instance).unwrap_or(&fixture.provider).clone());
        }
        self.app.read(cx).server_config.config.as_ref()?.providers.iter().find(|p| p.instance_id == self.instance).cloned()
    }
    fn config(&self, cx: &App) -> Option<Value> {
        #[cfg(feature = "snapshot")]
        if let Some(fixture) = cx.try_global::<Fixture>() {
            return fixture.managed.then(|| json!({"setupMode":"managed"}));
        }
        self.app.read(cx).server_config.config.as_ref()?.settings.provider_instances.get(&self.instance)?.config.clone()
    }
    fn allowed(&self, cx: &App) -> bool {
        if fixture_active(cx) {
            return true;
        }
        if self.connection != self.app.read(cx).connection_id() {
            return false;
        }
        let app = self.app.read(cx);
        let policy = app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref());
        t3_core::managed_policy::driver_allowed(policy, "codex")
            && !t3_core::managed_policy::setting_locked(policy, "providers")
            && !t3_core::managed_policy::instance_field_locked(policy, "codex", "config.setupMode")
            && app.client_handle().is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|s| s.grants("providers:manage")))
    }
    fn sync_watch(&mut self, cx: &mut Context<Self>) {
        if fixture_active(cx) {
            return;
        }
        let managed = model::managed(self.config(cx).as_ref());
        if self.observed_managed == Some(managed) {
            return;
        }
        self.observed_managed = Some(managed);
        self.watch_generation += 1;
        self.streams.clear();
        self.state = Default::default();
        self.watch(cx);
    }
    fn watch(&mut self, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let Some(client) = app.client_handle() else {
            return;
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let generation = self.watch_generation;
        for &method in model::managed_subscriptions(model::managed(self.config(cx).as_ref())) {
            let (tx, client, instance) = (tx.clone(), client.clone(), self.instance.clone());
            let network = app.runtime().spawn(async move {
                let Ok(mut stream) = client.stream(method, auth::subscribe_payload(&instance)).await else {
                    let _ = tx.send((method, Err(())));
                    return;
                };
                while let Some(item) = stream.receiver.recv().await {
                    if tx.send((method, item.map_err(|_| ()))).is_err() {
                        break;
                    }
                }
            });
            self.streams.push(Network(network.abort_handle()));
        }
        cx.spawn(async move |this, cx| {
            while let Some((method, item)) = rx.recv().await {
                if this
                    .update(cx, |this, cx| {
                        if this.watch_generation != generation {
                            return;
                        }
                        match item {
                            Ok(value) if method == auth::AUTH_SUBSCRIBE => {
                                if let Some(next) = auth::decode_auth_state(value) {
                                    if this.state.auth.as_ref().and_then(|s| s.flow_id.as_ref()) != next.flow_id.as_ref() {
                                        this.help = false;
                                    }
                                    if !this.state.handoff {
                                        this.state.auth = Some(next);
                                    }
                                }
                            }
                            Ok(value) => {
                                if let Some(state) = auth::decode_install_state(value) {
                                    this.state.observe_install(state);
                                }
                            }
                            Err(()) => {
                                if method==auth::INSTALL_SUBSCRIBE {this.state.install_load_error=true;}
                                this.state.error=Some("Could not read Codex setup status. Reconnect and try again.".into());
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
    fn change_mode(&mut self, managed: bool, start: bool, cx: &mut Context<Self>) {
        if !self.allowed(cx) || fixture_active(cx) {
            return;
        }
        let app = self.app.read(cx);
        let Some(config) = app.server_config.config.as_ref() else {
            return;
        };
        let payload = model::mode_payload(&config.settings, &self.instance, managed);
        self.save(payload, start, cx);
    }
    fn save(&mut self, payload: Value, start: bool, cx: &mut Context<Self>) {
        if fixture_active(cx) || !self.allowed(cx) {
            return;
        }
        self.state.pending = true;
        let app = self.app.read(cx);
        let revision = app.server_config.settings_revision();
        let request = app.rpc(t3_core::commands::method::SERVER_UPDATE_SETTINGS, payload);
        let connection = self.connection;
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |this, cx| {
                if this.connection != connection || this.app.read(cx).connection_id() != connection {
                    return;
                }
                this.state.pending = false;
                match reply {
                    Ok(value) => {
                        if let Some(settings) = t3_core::settings::decode_update_reply(value) {
                            this.app.update(cx, |app, cx| {
                                if app.server_config.apply_settings_reply(settings, revision) {
                                    cx.emit(AppEvent::ServerConfig);
                                }
                            });
                        }
                        if start {
                            this.auto_start = true;
                        }
                    }
                    Err(_) => this.state.error = Some("Codex setup failed. Try again.".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn start(&mut self, method: String, cx: &mut Context<Self>) -> bool {
        if fixture_active(cx) || !self.allowed(cx) || self.state.busy() {
            return false;
        }
        let app = self.app.read(cx);
        let Some(client) = app.client_handle() else {
            return false;
        };
        let Some(environment) = app.active_environment() else {
            return false;
        };
        let local = model::local_server(&environment.http_base, environment.ssh.is_some());
        let environment_id = environment.environment_id.clone().unwrap_or_else(|| environment.id.clone());
        let Some(return_url) = model::return_url(&environment.http_base, &environment_id, &self.instance, self.onboarding) else {
            return false;
        };
        let helper =
            if local { None } else { app.environments.environments.iter().find(|e| e.local && model::local_server(&e.http_base, e.ssh.is_some())).and_then(|e| e.credentials()) };
        let Some(install) = self.state.install.clone() else {
            return false;
        };
        let can_start = self.provider(cx).is_some_and(|p| p.enabled && p.setup.is_some_and(|s| if model::needs_install(&install) { s.can_install } else { s.can_authenticate }));
        if !can_start {
            return false;
        }
        let request = t3_client::chatgpt::SetupRequest { instance: self.instance.clone(), method: method.clone(), environment: environment_id, return_url, local, helper, install };
        self.state.requested_method = method;
        self.state.error = None;
        self.state.pending = true;
        self.serial += 1;
        let serial = self.serial;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (retry_tx, retry_rx) = tokio::sync::mpsc::unbounded_channel();
        self.retry = Some(retry_tx);
        self.state.transfer_failed = false;
        let network = app.runtime().spawn(async move { t3_client::chatgpt::setup(client, request, tx, retry_rx).await });
        self.operation = Some(Network(network.abort_handle()));
        cx.spawn(async move |this, cx| {
            while let Some(update) = rx.recv().await {
                if this
                    .update(cx, |this, cx| {
                        if this.serial != serial {
                            return;
                        }
                        use t3_client::chatgpt::Update;
                        update.apply_install(&mut this.state);
                        match update {
                            Update::Auth(auth) => {
                                this.state.pending = false;
                                this.state.auth = Some(auth);
                            }
                            Update::InstallStarted(_) | Update::Install(_) => {
                                this.state.pending = false;
                            }
                            Update::Handoff => {
                                this.state.pending = false;
                                this.state.handoff = true;
                            }
                            Update::TransferFailed => {
                                this.state.transfer_failed = true;
                                this.state.error = Some("Could not transfer ChatGPT sign-in. Retry connection.".into());
                            }
                            Update::Finishing => {
                                this.state.transfer_failed = false;
                                this.state.error = None;
                                this.state.handoff = false;
                                this.state.finishing = true;
                            }
                            Update::Page(url) => {
                                if local || this.state.handoff {
                                    cx.open_url(&url);
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let result = network.await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                this.operation = None;
                this.state.install_operation = None;
                this.state.pending = false;
                this.state.handoff = false;
                if let Ok(Err(error)) = result {
                    this.state.error = Some(error);
                    this.state.finishing = false;
                } else {
                    this.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        true
    }
    fn refresh(&self, cx: &mut Context<Self>) {
        if fixture_active(cx) {
            return;
        }
        let app = self.app.clone();
        let connection = self.connection;
        let task = app.read(cx).rpc(providers::SERVER_REFRESH_PROVIDERS, json!({"instanceId":self.instance}));
        cx.spawn(async move |_, cx| {
            if let Ok(value) = task.await
                && let Some(providers) = providers::decode_providers_reply(value)
            {
                app.update(cx, |app, cx| {
                    if app.connection_id() == connection && app.server_config.apply_providers(providers) {
                        cx.emit(AppEvent::ServerConfig);
                    }
                });
            }
        })
        .detach();
    }
    fn command(&mut self, method: &'static str, payload: Value, cx: &mut Context<Self>) {
        if fixture_active(cx) || !self.allowed(cx) || self.state.pending {
            return;
        }
        let task = self.app.read(cx).rpc(method, payload);
        self.state.pending = true;
        let serial = self.serial;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                this.state.pending = false;
                match result {
                    Ok(value) => {
                        if method == auth::INSTALL_CANCEL {
                            if let Some(state) = auth::decode_install_state(value) {
                                this.state.install_cancel_reply(state);
                            }
                        } else if let Some(auth) = auth::decode_auth_state(value) {
                            this.state.auth = Some(auth);
                        }
                        this.refresh(cx);
                    }
                    Err(_) => this.state.error = Some("Codex setup failed. Try again.".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn submit_callback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if fixture_active(cx) || self.state.handoff {
            return;
        }
        let value = self.callback.read(cx).value().trim().to_owned();
        if let Some(auth) = &self.state.auth
            && let Some(payload) = auth::complete_payload(&self.instance, auth, &value)
        {
            self.command(auth::AUTH_COMPLETE, payload, cx);
            self.callback.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if fixture_active(cx) || !self.allowed(cx) {
            return;
        }
        self.serial += 1;
        self.operation = None;
        self.retry = None;
        if let Some((method, payload)) = self.state.cancel_local(&self.instance, &mut self.auto_start) {
            self.command(method, payload, cx);
        }
        cx.notify();
    }
    fn connect(&mut self, cx: &mut Context<Self>) {
        if !model::managed(self.config(cx).as_ref()) {
            self.change_mode(true, true, cx);
        } else if self.state.has_profiles() {
            self.picker = true;
            self.selection = self.profiles().first().map(|m| m.id.clone()).unwrap_or_else(|| "chatgpt-change-account".into());
            cx.notify();
        } else {
            self.start("chatgpt".into(), cx);
        }
    }
    fn profiles(&self) -> Vec<auth::AuthMethod> {
        self.state.auth.as_ref().and_then(|a| a.methods.as_ref()).map(|m| m.iter().filter(|m| m.id.starts_with("chatgpt-profile:")).cloned().collect()).unwrap_or_default()
    }
    fn create_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if fixture_active(cx) || !self.allowed(cx) || self.state.pending {
            return;
        }
        let name = self.account_name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let instance = format!("codex_{}", uuid::Uuid::new_v4());
        let Some(payload) = model::new_account_payload(&instance, &name) else {
            return;
        };
        let _ = window;
        let task = self.app.read(cx).rpc(t3_core::commands::method::SERVER_UPDATE_SETTINGS, payload);
        let revision = self.app.read(cx).server_config.settings_revision();
        let serial = self.serial;
        self.state.pending = true;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial || this.app.read(cx).connection_id() != this.connection {
                    return;
                }
                this.state.pending = false;
                match result {
                    Ok(reply) => {
                        if let Some(settings) = t3_core::settings::decode_update_reply(reply) {
                            this.app.update(cx, |app, cx| {
                                if app.server_config.apply_settings_reply(settings, revision) {
                                    cx.emit(AppEvent::ServerConfig);
                                }
                            });
                        }
                        if this.onboarding {
                            this.adding = false;
                            cx.emit(AccountCreated(instance.clone()));
                        } else {
                            this.created_instance = Some(instance.clone());
                        }
                    }
                    Err(_) => this.state.error = Some("Could not add ChatGPT account. Try again.".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
impl ChatGptSetup {
    fn manual_callback(&self, cx: &App) -> bool {
        !self.state.handoff && self.app.read(cx).active_environment().is_some_and(|e| !model::local_server(&e.http_base, e.ssh.is_some()))
    }
    fn render_controls(&self, authenticated: bool, managed: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let busy = self.state.busy();
        let auth_active = self.state.auth_active() && !self.state.install_active();
        let provider = self.provider(cx);
        let available = provider.as_ref().is_none_or(|p| p.enabled)
            && (!managed
                || provider
                    .as_ref()
                    .and_then(|p| p.setup.as_ref())
                    .is_some_and(|s| if self.state.install.as_ref().is_some_and(model::needs_install) { s.can_install } else { s.can_authenticate }));
        let choice_enabled = self.state.choice_enabled(managed, self.allowed(cx), available);
        let disabled = !self.allowed(cx) || !available || (managed && self.state.error.as_deref() == Some("Could not read Codex setup status. Reconnect and try again."));
        let profiles = self.state.has_profiles();
        let url = self.state.auth.as_ref().and_then(auth::AuthState::url).map(str::to_owned);
        let waiting = self.state.auth.as_ref().is_some_and(|a| a.phase == "waiting");
        let mut controls = h_flex().gap(r(8.)).items_center().flex_wrap().justify_end();
        if self.onboarding && !managed && !busy && provider.as_ref().is_some_and(|p| t3_core::onboarding::agent_state(Some(p)) == t3_core::onboarding::AgentState::Checking) {
            controls = controls.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Checking..."));
        } else if authenticated && !busy && self.onboarding {
            controls =
                controls.child(h_flex().gap(r(6.)).text_xs().text_color(hsla(p.success_foreground)).child(Icon::Check.render(px(14.), hsla(p.success_foreground))).child("Ready"));
        } else if auth_active {
            controls = controls.child(
                Button::new("chatgpt-open", if self.manual_callback(cx) { ButtonVariant::Default } else { ButtonVariant::Outline }, ButtonSize::Sm)
                    .label(if self.state.transfer_failed {
                        "Retry connection"
                    } else if self.state.finishing {
                        "Finishing sign-in..."
                    } else if waiting && self.manual_callback(cx) {
                        "Open ChatGPT sign-in"
                    } else if waiting || self.onboarding {
                        "Open sign-in page"
                    } else {
                        "Signing in..."
                    })
                    .disabled(!self.state.transfer_failed && (!waiting || url.is_none() || self.state.finishing))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.state.transfer_failed {
                            if !fixture_active(cx)
                                && let Some(retry) = &this.retry
                            {
                                let _ = retry.send(());
                                this.state.transfer_failed = false;
                                this.state.error = None;
                                cx.notify();
                            }
                            return;
                        }
                        if !fixture_active(cx)
                            && let Some(url) = &url
                        {
                            cx.open_url(url);
                        }
                    })),
            );
            if !self.state.finishing {
                controls = controls.child(
                    Button::new("chatgpt-cancel", ButtonVariant::Ghost, ButtonSize::Sm)
                        .label("Cancel")
                        .disabled(disabled || self.state.pending)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                );
            }
        } else if self.state.install_active() {
            if self.onboarding {
                controls = controls.child(Button::new("chatgpt-setting-up", ButtonVariant::Default, ButtonSize::Sm).label("Setting up...").disabled(true));
            }
            controls = controls.child(
                Button::new("chatgpt-cancel-install", ButtonVariant::Ghost, ButtonSize::Sm)
                    .label("Cancel")
                    .disabled(disabled || self.state.pending)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
            );
        } else if authenticated && managed {
            controls = controls
                .child(Button::new("chatgpt-change", ButtonVariant::Outline, ButtonSize::Sm).label("Change account").disabled(disabled || busy).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.picker = true;
                        this.selection = this.profiles().first().map(|p| p.id.clone()).unwrap_or_else(|| "chatgpt-change-account".into());
                        cx.notify();
                    },
                )))
                .child(
                    Button::new("chatgpt-disconnect", ButtonVariant::Ghost, ButtonSize::Sm)
                        .label("Disconnect")
                        .disabled(disabled || busy)
                        .on_click(cx.listener(|this, _, _, cx| this.command(auth::AUTH_LOGOUT, auth::subscribe_payload(&this.instance), cx))),
                );
        } else {
            controls = controls.child(
                Button::new("chatgpt-connect", ButtonVariant::Default, ButtonSize::Sm)
                    .icon(Icon::OpenAi)
                    .label(if profiles { "Reconnect account" } else { "Continue with ChatGPT" })
                    .disabled(!choice_enabled)
                    .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
            );
            if profiles {
                controls = controls.child(
                    Button::new("chatgpt-different", ButtonVariant::GhostMuted, ButtonSize::Sm).label("Use a different account").disabled(disabled || busy).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.start("chatgpt-change-account".into(), cx);
                        },
                    )),
                );
            } else {
                controls = controls.child(
                    Button::new("chatgpt-existing", ButtonVariant::GhostMuted, ButtonSize::Sm)
                        .label("Use existing CLI")
                        .disabled(disabled || busy)
                        .on_click(cx.listener(|this, _, _, cx| this.change_mode(false, false, cx))),
                );
            }
        }
        controls.into_any_element()
    }
    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome {
            self.dismiss_welcome(cx);
        }
        self.picker = false;
        self.adding = false;
        self.added = None;
        self.dialog_scope.sync(false, window, cx);
        cx.notify();
    }
    fn render_dialog(&self, title: String, description: &'static str, body: AnyElement, footer: AnyElement, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.weak_entity();
        let frame = DialogFrame {
            id: "chatgpt-dialog",
            title: title.into(),
            description: description.into(),
            max_width: if self.adding { 768. } else { 512. },
            body,
            footer: dialog_footer(self.adding, vec![footer], cx).into_any_element(),
            focus: self.dialog_scope.focus.clone(),
        }
        .render_modal(
            window,
            65,
            move |window, cx| {
                let _ = this.update(cx, |this, cx| this.close_dialog(window, cx));
            },
            cx,
        );
        self.dialog_scope
            .wrap(frame)
            .on_action(cx.listener(|this, _: &dialog_focus::Close, window, cx| {
                this.close_dialog(window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    }
    fn render_picker(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let profiles = self.profiles();
        let selected = model::picker_selection(&profiles, &self.selection);
        let mut body = v_flex().gap(r(8.));
        for (id, label) in profiles.into_iter().map(|m| (m.id, m.name)).chain(std::iter::once(("chatgpt-change-account".into(), "Use a different account".into()))) {
            let checked = selected == id;
            body = body.child(
                Button::new(SharedString::from(format!("chatgpt-profile-{id}")), ButtonVariant::Outline, ButtonSize::Default)
                    .icon(if checked { Icon::CircleDot } else { Icon::WorkCircle })
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selection = id.clone();
                        cx.notify();
                    })),
            );
        }
        let footer = h_flex()
            .gap(r(8.))
            .child(Button::new("chatgpt-picker-close", ButtonVariant::Outline, ButtonSize::Sm).label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                this.picker = false;
                cx.notify();
            })))
            .child(Button::new("chatgpt-picker-continue", ButtonVariant::Default, ButtonSize::Sm).icon(Icon::OpenAi).label("Continue with ChatGPT").on_click(cx.listener(
                move |this, _, _, cx| {
                    this.picker = false;
                    let method = if this.state.auth.as_ref().and_then(|a| a.methods.as_ref()).is_some_and(|methods| methods.iter().any(|m| m.id == selected)) {
                        selected.clone()
                    } else {
                        "chatgpt-change-account".into()
                    };
                    this.start(method, cx);
                    cx.notify();
                },
            )));
        self.render_dialog("Reconnect ChatGPT".into(), "On OpenAI, sign in with the account you choose here.", body.into_any_element(), footer.into_any_element(), window, cx)
    }
    fn render_add(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let has_account = self.app.read(cx).server_config.config.as_ref().is_some_and(|c| c.providers.iter().any(|p| p.driver == "codex" && p.auth.status == "authenticated"));
        let body = if let Some(added) = &self.added {
            div().child(added.clone()).into_any_element()
        } else {
            v_flex()
                .gap(r(12.))
                .child(
                    Row::new("chatgpt-account-name", "Account name")
                        .description("Shown in the provider list and model picker.")
                        .control(div().w(r(224.)).child(Input::new(&self.account_name).small())),
                )
                .when(has_account, |v| {
                    v.child(
                        div()
                            .p(r(12.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child(model::MULTIPLE_ACCOUNTS)
                            .child(Button::new("chatgpt-multiple-info", ButtonVariant::GhostMuted, ButtonSize::Xs).label("More info").on_click(|_, _, cx| {
                                if !fixture_active(cx) {
                                    cx.open_url("https://github.com/pingdotgg/t3code/blob/main/docs/user/faq.md#can-i-add-a-bunch-of-accounts-for-one-provider");
                                }
                            })),
                    )
                })
                .into_any_element()
        };
        let footer = h_flex()
            .gap(r(8.))
            .child(
                Button::new("chatgpt-add-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                    .disabled(self.state.pending)
                    .label(if self.added.is_some() { "Finish later" } else { "Cancel" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.adding = false;
                        this.added = None;
                        cx.notify();
                    })),
            )
            .when(self.added.is_none(), |v| {
                v.child(
                    Button::new("chatgpt-add-continue", ButtonVariant::Default, ButtonSize::Sm)
                        .label(if self.state.pending { "Adding account…" } else { "Continue" })
                        .disabled(self.state.pending || !self.allowed(cx) || self.account_name.read(cx).value().trim().is_empty())
                        .on_click(cx.listener(|this, _, window, cx| this.create_account(window, cx))),
                )
            });
        self.render_dialog(
            "Add ChatGPT account".into(),
            "Each account has its own Codex instance and sign-in. Choose the other account on the sign-in page.",
            body,
            footer.into_any_element(),
            window,
            cx,
        )
    }
    fn dismiss_welcome(&mut self, cx: &mut Context<Self>) {
        self.welcome = false;
        if !fixture_active(cx) {
            let identity = json!([self.app.read(cx).active_environment().map(|e| e.id.as_str()), self.instance, self.acknowledged]);
            let mut seen = t3_client::store::load_ui_state("chatgpt-sharing-welcome").and_then(|v| v.as_array().cloned()).unwrap_or_default();
            if !seen.contains(&identity) {
                seen.push(identity);
                t3_client::store::save_ui_state("chatgpt-sharing-welcome", &seen);
            }
        }
        cx.notify();
    }
    pub fn start_automatically(&mut self, cx: &mut Context<Self>) {
        self.auto_start = true;
        self.refresh(cx);
        cx.notify();
    }
    pub fn can_manage(&self, cx: &App) -> bool {
        self.allowed(cx)
    }
    pub fn open_add(&mut self, cx: &mut Context<Self>) {
        self.adding = true;
        cx.notify();
    }
}
impl Render for ChatGptSetup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(instance) = self.created_instance.take() {
            let added = cx.new(|cx| Self::new(self.app.clone(), instance, self.onboarding, window, cx));
            added.update(cx, |view, cx| {
                view.auto_start = true;
                view.refresh(cx);
            });
            self.added = Some(added);
        }
        if self.added.as_ref().is_some_and(|v| v.read(cx).provider(cx).is_some_and(|p| p.auth.status == "authenticated")) {
            self.adding = false;
            self.added = None;
        }
        if self.state.auto_start_ready(self.auto_start, self.allowed(cx), self.provider(cx).and_then(|p| p.setup).is_some_and(|s| s.can_install)) {
            cx.spawn(async |this, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.start("chatgpt".into(), cx) {
                        this.auto_start = false;
                    }
                });
            })
            .detach();
        }
        let flow = self.state.auth.as_ref().and_then(|a| a.flow_id.clone());
        if flow != self.callback_flow {
            self.callback_flow = flow;
            self.callback.update(cx, |input, cx| input.set_value("", window, cx));
        }
        let provider = self.provider(cx);
        let authenticated = provider.as_ref().is_some_and(|p| p.auth.status == "authenticated");
        let p = palette(cx);
        let managed = model::managed(self.config(cx).as_ref());
        let email = provider.as_ref().and_then(|p| p.auth.email.as_deref());
        let description = if self.onboarding && !self.state.busy() && !authenticated && !self.state.auth.as_ref().is_some_and(|a| a.phase == "failed") {
            "Code with your ChatGPT subscription.".to_owned()
        } else {
            self.state.account_description(authenticated, email)
        };
        let description = if authenticated && !self.revealed && email.is_some() { "Signed in as ••••••••.".to_owned() } else { description };
        let controls = self.render_controls(authenticated, managed, cx);
        let description =
            div().id("chatgpt-email-description").mt(r(2.)).text_xs().text_color(hsla(p.muted_foreground)).child(description).when(authenticated && email.is_some(), |v| {
                v.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                    this.revealed = !this.revealed;
                    cx.notify();
                }))
            });
        let account = if self.onboarding {
            h_flex()
                .gap(r(12.))
                .p(r(16.))
                .child(ProviderIcon::new(ProviderMark::configured("codex", "Codex", None)).size(px(20.)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div().text_sm().font_weight(FontWeight::MEDIUM).child(
                                provider
                                    .as_ref()
                                    .and_then(|p| p.display_name.clone())
                                    .or_else(|| {
                                        self.app
                                            .read(cx)
                                            .server_config
                                            .config
                                            .as_ref()
                                            .and_then(|c| c.settings.provider_instances.get(&self.instance))
                                            .and_then(|p| p.display_name.clone())
                                    })
                                    .unwrap_or_else(|| "Codex".into()),
                            ),
                        )
                        .child(description),
                )
                .child(controls)
                .into_any_element()
        } else {
            Row::new("chatgpt-account", "ChatGPT account").description_content(description).control(controls).into_any_element()
        };
        let mut content = v_flex().child(account);
        if !self.onboarding && managed {
            content = content.child(Row::new("chatgpt-runtime", "Codex runtime").description(self.state.runtime_description()));
        }
        if self.state.auth.as_ref().is_some_and(|a| a.phase == "waiting") && !self.state.handoff {
            let page_label = self.state.auth.as_ref().map(model::authorization_page_label).unwrap_or_default();
            let callback_button = Button::new("chatgpt-callback", ButtonVariant::Outline, ButtonSize::Sm)
                .label("Connect")
                .disabled(self.state.pending || !self.allowed(cx))
                .on_click(cx.listener(|this, _, window, cx| this.submit_callback(window, cx)));
            let help = v_flex()
                .gap(r(8.))
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("If sign-in doesn't return to T3 Code, paste the URL from the final localhost page."))
                .child(h_flex().gap(r(8.)).child(div().flex_1().child(Input::new(&self.callback).small())).child(callback_button));
            content = content.child(
                v_flex()
                    .px(r(16.))
                    .pb(r(12.))
                    .gap(r(8.))
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(page_label))
                    .child(Button::new("chatgpt-help", ButtonVariant::GhostMuted, ButtonSize::Xs).label("Having trouble signing in?").on_click(cx.listener(|this, _, _, cx| {
                        this.help = !this.help;
                        cx.notify();
                    })))
                    .when(self.help || self.manual_callback(cx), |v| v.child(help)),
            );
        }
        if let Some(error) = self.state.error.clone().or_else(|| self.state.install.as_ref().filter(|i| i.phase == "failed").and_then(|i| i.message.clone())) {
            content = content.child(div().px(r(16.)).pb(r(12.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error));
        }
        if !self.onboarding && managed && !self.state.busy() && authenticated {
            content = content.child(
                Button::new("chatgpt-existing-signed-in", ButtonVariant::GhostMuted, ButtonSize::Xs)
                    .label("Use existing CLI")
                    .disabled(!self.allowed(cx))
                    .on_click(cx.listener(|this, _, _, cx| this.change_mode(false, false, cx))),
            );
        }
        if authenticated && !self.onboarding {
            content = content.child(
                Button::new("chatgpt-another", ButtonVariant::GhostMuted, ButtonSize::Xs).label("Connect another ChatGPT account").disabled(!self.allowed(cx)).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.adding = true;
                        cx.notify();
                    }),
                ),
            );
        }
        if self.onboarding {
            content = content.rounded(r(8.)).border_1().border_color(hsla(p.border)).bg(hsla(p.background));
        }
        let visible = self.adding || self.picker || self.welcome;
        self.dialog_scope.sync(visible, window, cx);
        let dialog = if self.adding {
            Some(self.render_add(window, cx))
        } else if self.picker {
            Some(self.render_picker(window, cx))
        } else if self.welcome {
            let footer = Button::new("chatgpt-welcome-continue", ButtonVariant::Default, ButtonSize::Sm).label("Continue").on_click(cx.listener(|this, _, _, cx| {
                this.dismiss_welcome(cx);
            }));
            Some(
                self.render_dialog(
                    "Your ChatGPT plan is connected".into(),
                    model::WELCOME,
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(
                            provider
                                .as_ref()
                                .and_then(|p| p.display_name.clone())
                                .or_else(|| {
                                    self.app
                                        .read(cx)
                                        .server_config
                                        .config
                                        .as_ref()
                                        .and_then(|c| c.settings.provider_instances.get(&self.instance))
                                        .and_then(|p| p.display_name.clone())
                                })
                                .unwrap_or_else(|| "Codex".into()),
                        )
                        .into_any_element(),
                    footer.into_any_element(),
                    window,
                    cx,
                ),
            )
        } else {
            None
        };
        content.children(dialog)
    }
}
