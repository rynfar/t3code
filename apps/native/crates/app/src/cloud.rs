//! Connect screen account lifecycle, ported from web/cloud/managedAuth.tsx and relay discovery.
use std::collections::HashMap;

use gpui::{AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{h_flex, v_flex};
use t3_client::connect::{
    ClerkAuthOutcome, PendingEmailSignIn, PendingSecondFactor,
};
use t3_core::connect::{
    CLERK_NATIVE_CALLBACK_SCHEME, FirstFactorKind, SecondFactorKind, SocialProvider,
};
use t3_core::relay::{
    Discovery, Environment, EnvironmentHealth, EnvironmentStatusValue, ManagedTunnelUpgrade,
    T3CHAT_SUBSCRIPTION_URL, T3ChatLinkResponse, managed_tunnel_cap, tunnel_upgrade_copy,
};

use crate::app_state::{AppEvent, AppState};
use crate::auth_session_macos::{self, ActiveAuthSession, AuthSessionError};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// Active step of an interactive Clerk Native sign-in on the Connect screen.
#[derive(Debug, Clone, Default)]
pub enum CloudAuthFlow {
    #[default]
    Idle,
    /// Native macOS `ASWebAuthenticationSession` is open for a social provider.
    OAuth(SocialProvider),
    /// Email submitted; awaiting `email_code` or `password` first factor.
    EmailFirstFactor {
        pending: PendingEmailSignIn,
        code_sent: bool,
    },
    /// First factor / OAuth succeeded; Clerk requires 2FA (`totp` or `backup_code`).
    SecondFactor(PendingSecondFactor),
}

pub struct CloudState {
    pub service: std::sync::Arc<t3_client::connect::Cloud>,
    pub identity: Option<String>,
    /// True when signed in via Clerk Native Frontend API (`cloud-clerk.json`), enabling DPoP relay
    /// status checks and direct relay connections. False when only a legacy OAuth token exists.
    pub has_clerk_session: bool,
    pub discovery: Discovery,
    pub t3_chat_link: Option<T3ChatLinkResponse>,
    pub t3_chat_linking: bool,
    pub last_tunnel_limit: Option<u32>,
    pub pending_onboarding: bool,
    pub statuses: HashMap<String, EnvironmentHealth>,
    pub connecting_env_id: Option<String>,
    pub busy: bool,
    pub signing_in: bool,
    pub auth_flow: CloudAuthFlow,
    pub error: Option<String>,
    active_auth_session: Option<ActiveAuthSession>,
    generation: u64,
    task: Option<tokio::task::AbortHandle>,
    status_tasks: Vec<tokio::task::AbortHandle>,
}

impl Default for CloudState {
    fn default() -> Self {
        Self {
            service: t3_client::connect::Cloud::new(),
            identity: None,
            has_clerk_session: false,
            discovery: Default::default(),
            t3_chat_link: None,
            t3_chat_linking: false,
            last_tunnel_limit: None,
            pending_onboarding: false,
            statuses: HashMap::new(),
            connecting_env_id: None,
            busy: false,
            signing_in: false,
            auth_flow: CloudAuthFlow::Idle,
            error: None,
            active_auth_session: None,
            generation: 0,
            task: None,
            status_tasks: Vec::new(),
        }
    }
}

impl CloudState {
    fn advance_generation(&mut self) -> u64 {
        self.connecting_env_id = None;
        self.generation += 1;
        self.generation
    }

    fn cancel_sign_in(&mut self) {
        if let Some(session) = self.active_auth_session.take() {
            session.cancel();
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.advance_generation();
        self.busy = false;
        self.signing_in = false;
        self.auth_flow = CloudAuthFlow::Idle;
    }
}

impl AppState {
    fn abort_cloud_status_tasks(&mut self) {
        for task in self.cloud.status_tasks.drain(..) {
            task.abort();
        }
    }

    pub fn refresh_cloud(&mut self, cx: &mut Context<Self>) {
        if self.cloud.busy {
            return;
        }
        self.abort_cloud_status_tasks();
        self.cloud.busy = true;
        self.cloud.error = None;
        self.cloud.discovery = Discovery {
            refreshing: true,
            ..Default::default()
        };
        let (service, generation) = (self.cloud.service.clone(), self.cloud.generation);
        let task = self.runtime().spawn(async move {
            let account = service.account_state().await?;
            let (discovery, t3_chat) = if account.identity.is_some() {
                if account.has_clerk_session {
                    let (discovery, t3_chat) = tokio::join!(service.discover(), service.t3_chat_link());
                    (discovery, t3_chat.ok())
                } else {
                    (service.discover().await, None)
                }
            } else {
                (Ok(Discovery::default()), None)
            };
            Ok::<_, String>((account, discovery, t3_chat))
        });
        self.cloud.task = Some(task.abort_handle());
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.task = None;
                state.cloud.discovery.refreshing = false;
                let mut should_probe_statuses = false;
                match result {
                    Ok(Ok((account, discovery, t3_chat))) => {
                        state.cloud.identity = account.identity;
                        state.cloud.has_clerk_session = account.has_clerk_session;
                        state.cloud.t3_chat_link = t3_chat;
                        if state.cloud.identity.is_none() {
                            state.cloud.pending_onboarding = false;
                        }
                        match discovery {
                            Ok(d) => {
                                should_probe_statuses =
                                    account.has_clerk_session && !d.environments.is_empty();
                                state.cloud.discovery = d;
                            }
                            Err(error) => state.cloud.error = Some(error),
                        }
                    }
                    Ok(Err(error)) => state.cloud.error = Some(error),
                    Err(_) => state.cloud.error = Some("Could not load T3 Connect data.".into()),
                }
                if should_probe_statuses {
                    state.refresh_cloud_statuses(cx);
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    /// Probes live online/offline status for each discovered T3 Connect environment via DPoP.
    pub fn refresh_cloud_statuses(&mut self, cx: &mut Context<Self>) {
        if !self.cloud.has_clerk_session {
            return;
        }
        self.abort_cloud_status_tasks();
        let environments = self.cloud.discovery.environments.clone();
        let generation = self.cloud.generation;
        for env in environments {
            let env_id = env.environment_id.clone();
            self.cloud
                .statuses
                .insert(env_id.clone(), EnvironmentHealth::Checking);
            let service = self.cloud.service.clone();
            let task = self
                .runtime()
                .spawn(async move { service.environment_status(&env_id).await });
            self.cloud.status_tasks.push(task.abort_handle());
            let env_id = env.environment_id;
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |state, cx| {
                    if state.cloud.generation != generation {
                        return;
                    }
                    let health = match result {
                        Ok(Ok(resp)) => match resp.status {
                            EnvironmentStatusValue::Online => resp.descriptor
                                .as_ref()
                                .and_then(|descriptor| t3_core::compatibility::protocol_mismatch(descriptor)
                                    .map(|mismatch| mismatch.message(&descriptor.label)))
                                .map_or(EnvironmentHealth::Online, EnvironmentHealth::Incompatible),
                            EnvironmentStatusValue::Offline => EnvironmentHealth::Offline {
                                offline_reason: resp.offline_reason,
                                error: resp.error,
                                trace_id: resp.trace_id,
                            },
                        },
                        Ok(Err(error)) => EnvironmentHealth::Error(error),
                        Err(_) => return,
                    };
                    state.cloud.statuses.insert(env_id, health);
                    cx.emit(AppEvent::Phase);
                });
            })
            .detach();
        }
    }

    /// Starts Clerk Native social OAuth (`ASWebAuthenticationSession` on macOS).
    pub fn sign_in_cloud_oauth(&mut self, provider: SocialProvider, cx: &mut Context<Self>) {
        if self.cloud.busy {
            return;
        }
        self.cloud.cancel_sign_in();
        self.cloud.busy = true;
        self.cloud.signing_in = true;
        self.cloud.auth_flow = CloudAuthFlow::OAuth(provider);
        self.cloud.error = None;
        let generation = self.cloud.generation;
        let service = self.cloud.service.clone();
        let start_task = self
            .runtime()
            .spawn(async move { service.start_oauth_sign_in(provider).await });
        self.cloud.task = Some(start_task.abort_handle());
        cx.emit(AppEvent::Phase);

        cx.spawn(async move |this, cx| {
            let started = start_task.await;
            let pending = match started {
                Ok(Ok(pending)) => pending,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |state, cx| {
                        if state.cloud.generation != generation {
                            return;
                        }
                        state.cloud.busy = false;
                        state.cloud.signing_in = false;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.cloud.task = None;
                        state.cloud.error = Some(error);
                        cx.emit(AppEvent::Phase);
                    });
                    return;
                }
                Err(_) => return,
            };

            let redirect_url = pending.redirect_url.clone();
            let mut callback_rx = None;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                match auth_session_macos::start(&redirect_url, CLERK_NATIVE_CALLBACK_SCHEME, None) {
                    Ok((active, rx)) => {
                        state.cloud.active_auth_session = Some(active);
                        callback_rx = Some(rx);
                    }
                    Err(AuthSessionError::Cancelled) => {
                        state.cloud.busy = false;
                        state.cloud.signing_in = false;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.cloud.task = None;
                    }
                    Err(AuthSessionError::Unavailable(msg) | AuthSessionError::Failed(msg)) => {
                        state.cloud.busy = false;
                        state.cloud.signing_in = false;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.cloud.task = None;
                        state.cloud.error = Some(msg);
                    }
                }
                cx.emit(AppEvent::Phase);
            });

            let Some(rx) = callback_rx else {
                return;
            };
            let callback_url = match rx.await {
                Ok(Ok(url)) => url,
                Ok(Err(AuthSessionError::Cancelled)) | Err(_) => {
                    let _ = this.update(cx, |state, cx| {
                        if state.cloud.generation != generation {
                            return;
                        }
                        state.cloud.active_auth_session = None;
                        state.cloud.busy = false;
                        state.cloud.signing_in = false;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.cloud.task = None;
                        cx.emit(AppEvent::Phase);
                    });
                    return;
                }
                Ok(Err(AuthSessionError::Unavailable(msg) | AuthSessionError::Failed(msg))) => {
                    let _ = this.update(cx, |state, cx| {
                        if state.cloud.generation != generation {
                            return;
                        }
                        state.cloud.active_auth_session = None;
                        state.cloud.busy = false;
                        state.cloud.signing_in = false;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.cloud.task = None;
                        state.cloud.error = Some(msg);
                        cx.emit(AppEvent::Phase);
                    });
                    return;
                }
            };

            let complete_task = {
                let mut handle = None;
                let _ = this.update(cx, |state, _| {
                    if state.cloud.generation != generation {
                        return;
                    }
                    state.cloud.active_auth_session = None;
                    let service = state.cloud.service.clone();
                    let task = state
                        .runtime()
                        .spawn(async move { service.complete_oauth_sign_in(pending, &callback_url).await });
                    state.cloud.task = Some(task.abort_handle());
                    handle = Some(task);
                });
                handle
            };
            let Some(complete_task) = complete_task else {
                return;
            };
            let result = complete_task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.signing_in = false;
                state.cloud.task = None;
                match result {
                    Ok(Ok(ClerkAuthOutcome::Complete(session))) => {
                        let was_signed_in = state.cloud.identity.is_some();
                        state.complete_interactive_cloud_sign_in(was_signed_in, session.identity, cx);
                    }
                    Ok(Ok(ClerkAuthOutcome::NeedsSecondFactor(pending_2fa))) => {
                        state.cloud.auth_flow = CloudAuthFlow::SecondFactor(pending_2fa);
                    }
                    Ok(Err(error)) => {
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.cloud.error = Some(error);
                    }
                    Err(_) => {
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.cloud.error = Some("T3 Connect sign-in was cancelled.".into());
                    }
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    /// Starts inline email sign-in via Clerk Native Frontend API.
    pub fn start_email_cloud_sign_in(&mut self, email: String, cx: &mut Context<Self>) {
        if self.cloud.busy {
            return;
        }
        self.cloud.cancel_sign_in();
        self.cloud.busy = true;
        self.cloud.error = None;
        let generation = self.cloud.generation;
        let service = self.cloud.service.clone();
        let task = self.runtime().spawn(async move {
            let mut pending = service.start_email_sign_in(&email).await?;
            let mut code_sent = false;
            if pending.challenge.supports_email_code && !pending.challenge.supports_password {
                service.prepare_email_code(&mut pending).await?;
                code_sent = true;
            }
            Ok::<_, String>((pending, code_sent))
        });
        self.cloud.task = Some(task.abort_handle());
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.task = None;
                match result {
                    Ok(Ok((pending, code_sent))) => {
                        state.cloud.auth_flow = CloudAuthFlow::EmailFirstFactor { pending, code_sent };
                    }
                    Ok(Err(error)) => state.cloud.error = Some(error),
                    Err(_) => {}
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    /// Sends (or resends) the one-time email verification code for the active email sign-in.
    pub fn send_email_cloud_code(&mut self, cx: &mut Context<Self>) {
        if self.cloud.busy {
            return;
        }
        let CloudAuthFlow::EmailFirstFactor { pending, .. } = &self.cloud.auth_flow else {
            return;
        };
        let mut pending = pending.clone();
        self.cloud.busy = true;
        self.cloud.error = None;
        let generation = self.cloud.generation;
        let service = self.cloud.service.clone();
        let task = self.runtime().spawn(async move {
            service.prepare_email_code(&mut pending).await?;
            Ok::<_, String>(pending)
        });
        self.cloud.task = Some(task.abort_handle());
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.task = None;
                match result {
                    Ok(Ok(updated)) => {
                        state.cloud.auth_flow = CloudAuthFlow::EmailFirstFactor {
                            pending: updated,
                            code_sent: true,
                        };
                    }
                    Ok(Err(error)) => state.cloud.error = Some(error),
                    Err(_) => {}
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    /// Submits an email verification code or password for the active email sign-in.
    pub fn submit_email_cloud_first_factor(
        &mut self,
        kind: FirstFactorKind,
        secret: String,
        cx: &mut Context<Self>,
    ) {
        if self.cloud.busy {
            return;
        }
        let CloudAuthFlow::EmailFirstFactor { pending, .. } = &self.cloud.auth_flow else {
            return;
        };
        let pending = pending.clone();
        self.cloud.busy = true;
        self.cloud.error = None;
        let generation = self.cloud.generation;
        let service = self.cloud.service.clone();
        let task = self.runtime().spawn(async move {
            service
                .attempt_email_first_factor(&pending, kind, &secret)
                .await
        });
        self.cloud.task = Some(task.abort_handle());
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.task = None;
                match result {
                    Ok(Ok(ClerkAuthOutcome::Complete(session))) => {
                        let was_signed_in = state.cloud.identity.is_some();
                        state.complete_interactive_cloud_sign_in(was_signed_in, session.identity, cx);
                    }
                    Ok(Ok(ClerkAuthOutcome::NeedsSecondFactor(pending_2fa))) => {
                        state.cloud.auth_flow = CloudAuthFlow::SecondFactor(pending_2fa);
                    }
                    Ok(Err(error)) => state.cloud.error = Some(error),
                    Err(_) => {}
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    /// Submits a 2FA code (`totp` or `backup_code`) for an active `SecondFactor` challenge.
    pub fn submit_cloud_second_factor(
        &mut self,
        kind: SecondFactorKind,
        code: String,
        cx: &mut Context<Self>,
    ) {
        if self.cloud.busy {
            return;
        }
        let CloudAuthFlow::SecondFactor(pending) = &self.cloud.auth_flow else {
            return;
        };
        let pending = pending.clone();
        self.cloud.busy = true;
        self.cloud.error = None;
        let generation = self.cloud.generation;
        let service = self.cloud.service.clone();
        let task = self
            .runtime()
            .spawn(async move { service.attempt_second_factor(&pending, kind, &code).await });
        self.cloud.task = Some(task.abort_handle());
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.task = None;
                match result {
                    Ok(Ok(session)) => {
                        let was_signed_in = state.cloud.identity.is_some();
                        state.complete_interactive_cloud_sign_in(was_signed_in, session.identity, cx);
                    }
                    Ok(Err(error)) => state.cloud.error = Some(error),
                    Err(_) => {}
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    /// Default sign-in entry point (used by Onboarding / MCP prompts): uses native Clerk OAuth on
    /// macOS and legacy browser PKCE on other platforms.
    pub fn sign_in_cloud(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        {
            self.sign_in_cloud_oauth(SocialProvider::Google, cx);
        }
        #[cfg(not(target_os = "macos"))]
        {
            if self.cloud.busy {
                return;
            }
            self.cloud.busy = true;
            self.cloud.signing_in = true;
            self.cloud.error = None;
            let generation = self.cloud.advance_generation();
            let service = self.cloud.service.clone();
            let (browser, url) = tokio::sync::oneshot::channel();
            let task = self.runtime().spawn(async move { service.sign_in(browser).await });
            self.cloud.task = Some(task.abort_handle());
            cx.emit(AppEvent::Phase);
            cx.spawn(async move |this, cx| {
                if let Ok(url) = url.await {
                    let _ = this.update(cx, |state, cx| {
                        if state.cloud.generation == generation {
                            cx.open_url(&url);
                        }
                    });
                }
                let result = task.await;
                let _ = this.update(cx, |state, cx| {
                    if state.cloud.generation != generation {
                        return;
                    }
                    state.cloud.busy = false;
                    state.cloud.signing_in = false;
                    state.cloud.task = None;
                    match result {
                        Ok(Ok(identity)) => {
                            state.cloud.identity = Some(identity);
                            state.cloud.has_clerk_session = false;
                            state.refresh_cloud(cx);
                        }
                        Ok(Err(error)) => state.cloud.error = Some(error),
                        Err(_) => state.cloud.error = Some("T3 Connect sign-in was cancelled.".into()),
                    }
                    cx.emit(AppEvent::Phase);
                });
            })
            .detach();
        }
    }

    pub(crate) fn cloud_environment_compatibility_error(&self, environment_id: &str) -> Option<String> {
        if let Some(EnvironmentHealth::Incompatible(error)) = self.cloud.statuses.get(environment_id) {
            return Some(error.clone());
        }
        let saved = self.environments.find_by_environment_id(environment_id)?;
        let config = self.fleet.environment(&saved.id)?.config.config.as_ref()?;
        t3_core::compatibility::protocol_mismatch(&config.environment)
            .map(|mismatch| mismatch.message(&config.environment.label))
    }

    /// Registers the relay route under the canonical environment.
    pub fn connect_cloud_environment(&mut self, env: Environment, activate_existing: bool, cx: &mut Context<Self>) {
        if self.cloud.connecting_env_id.is_some() {
            return;
        }
        if let Some(error) = self.cloud_environment_compatibility_error(&env.environment_id) {
            self.cloud.error = Some(error);
            cx.emit(AppEvent::Phase);
            return;
        }
        let env_id = env.environment_id.clone();
        let label = env.label.clone();
        let saved = self.environments.find_by_environment_id(&env_id);
        let alternate = saved.is_some_and(|saved| !saved.is_relay_managed());
        if let Some(saved) = saved.filter(|saved| {
            saved.local || t3_client::store::has_saved_relay_route(&saved.id)
        }) {
            if activate_existing {
                self.connect_environment(&saved.id.clone(), cx);
            }
            return;
        }
        self.cloud.connecting_env_id = Some(env_id.clone());
        self.cloud.error = None;
        let service = self.cloud.service.clone();
        let generation = self.cloud.generation;
        let task = self.runtime().spawn(async move {
            if alternate {
                service.connect_environment(&env_id).await
            } else {
                service.connect_and_register_environment(&env_id, None).await
            }
        });
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.connecting_env_id = None;
                match result {
                    Ok(Ok(resp)) => {
                        let existing = activate_existing.then(|| state.environments
                            .find_by_environment_id(&resp.environment_id)
                            .filter(|saved| !saved.is_relay_managed())
                            .map(|saved| saved.id.clone())).flatten();
                        state.adopt_relay_environment(resp, label, cx);
                        if let Some(id) = existing {
                            state.connect_environment(&id, cx);
                        }
                    }
                    Ok(Err(error)) => {
                        state.cloud.error = Some(error.clone());
                        state.show_toast(
                            Toast::new(ToastKind::Error, "Could not connect environment")
                                .description(error),
                            cx,
                        );
                    }
                    Err(_) => {
                        state.cloud.error = Some("Could not connect the T3 Connect environment.".into());
                    }
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    pub fn cancel_cloud_sign_in(&mut self, cx: &mut Context<Self>) {
        self.cloud.cancel_sign_in();
        self.cloud.error = None;
        cx.emit(AppEvent::Phase);
    }

    pub fn sign_out_cloud(&mut self, cx: &mut Context<Self>) {
        self.abort_cloud_status_tasks();
        self.cloud.cancel_sign_in();
        self.cloud.identity = None;
        self.cloud.has_clerk_session = false;
        self.cloud.discovery = Default::default();
        self.cloud.t3_chat_link = None;
        self.cloud.t3_chat_linking = false;
        self.cloud.last_tunnel_limit = None;
        self.cloud.pending_onboarding = false;
        self.cloud.statuses.clear();
        self.cloud.error = None;
        self.cloud.busy = true;
        let service = self.cloud.service.clone();
        let generation = self.cloud.generation;
        let task = self.runtime().spawn(async move { service.sign_out().await });
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                if let Ok(Err(error)) = result {
                    state.cloud.error = Some(error);
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    fn complete_interactive_cloud_sign_in(
        &mut self,
        was_signed_in: bool,
        identity: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.cloud.identity = Some(identity.unwrap_or_else(|| "T3 Connect account".into()));
        self.cloud.has_clerk_session = true;
        self.cloud.auth_flow = CloudAuthFlow::Idle;
        if !was_signed_in && crate::connect_onboarding::should_open_on_fresh_sign_in() {
            self.cloud.pending_onboarding = true;
        }
        self.refresh_cloud(cx);
    }

    pub fn set_tunnel_limit_reached(&mut self, limit: Option<u32>, cx: &mut Context<Self>) {
        if self.cloud.last_tunnel_limit != limit {
            self.cloud.last_tunnel_limit = limit;
            cx.emit(AppEvent::Phase);
        }
    }

    pub fn start_t3_chat_link(&mut self, cx: &mut Context<Self>) {
        if self.cloud.t3_chat_linking || !self.cloud.has_clerk_session {
            return;
        }
        self.cloud.t3_chat_linking = true;
        let generation = self.cloud.generation;
        let service = self.cloud.service.clone();
        let task = self
            .runtime()
            .spawn(async move { service.create_t3_chat_link_request().await });
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.t3_chat_linking = false;
                match result {
                    Ok(Ok(url)) => cx.open_url(&url),
                    Ok(Err(error)) => {
                        state.show_toast(
                            Toast::new(ToastKind::Error, "Could not start T3 Chat link")
                                .description(error),
                            cx,
                        );
                    }
                    Err(_) => {}
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }
}

const MAX_USAGE_SEGMENTS: u32 = 10;

fn tunnel_upgrade_cta(
    id: &'static str,
    upgrade: Option<ManagedTunnelUpgrade>,
    linking: bool,
    state: &Entity<AppState>,
    cx: &App,
) -> Option<AnyElement> {
    match upgrade {
        Some(ManagedTunnelUpgrade::Link) => {
            let state = state.clone();
            Some(
                Button::new(id, ButtonVariant::Outline, ButtonSize::Xs)
                    .label(crate::i18n::t(cx, "Link T3 Chat"))
                    .disabled(linking)
                    .on_click(move |_, _, cx| {
                        state.update(cx, |state, cx| state.start_t3_chat_link(cx));
                    })
                    .into_any_element(),
            )
        }
        Some(ManagedTunnelUpgrade::Pro | ManagedTunnelUpgrade::Premier) => Some(
            Button::new(id, ButtonVariant::Outline, ButtonSize::Xs)
                .label(crate::i18n::t(cx, "Upgrade on T3 Chat"))
                .on_click(|_, _, cx| cx.open_url(T3CHAT_SUBSCRIPTION_URL))
                .into_any_element(),
        ),
        None => None,
    }
}

/// Renders `"2 of 3 tunnels in use"` + segmented/continuous bar and T3 Chat upgrade prompt.
pub fn render_tunnel_usage_meter(state: &Entity<AppState>, cx: &App) -> Option<AnyElement> {
    let cloud = &state.read(cx).cloud;
    if !cloud.has_clerk_session || !cloud.discovery.listed {
        return None;
    }
    let cap = managed_tunnel_cap(&cloud.discovery.environments, cloud.t3_chat_link.as_ref());
    if cap.limit == 0 {
        return None;
    }
    let p = palette(cx);
    let full = cap.used as u32 >= cap.limit;
    let fill = if full { hsla(p.warning) } else { hsla(p.foreground).opacity(0.72) };
    let label_color = if full { hsla(p.warning_foreground) } else { hsla(p.foreground) };
    let used_label = crate::i18n::t_fmt(
        cx,
        "{used} of {limit} tunnels in use",
        &[("used", (cap.used as i64).into()), ("limit", i64::from(cap.limit).into())],
    );
    let action = tunnel_upgrade_cta("tunnel-usage-cta", cap.upgrade, cloud.t3_chat_linking, state, cx);
    let bar = if cap.limit <= MAX_USAGE_SEGMENTS {
        h_flex()
            .h(px(6.))
            .w_full()
            .gap(px(4.))
            .children((0..cap.limit).map(|index| {
                div()
                    .flex_1()
                    .h_full()
                    .rounded_full()
                    .bg(if (index as usize) < cap.used { fill } else { hsla(p.muted) })
            }))
            .into_any_element()
    } else {
        let pct = (cap.used as f32 / cap.limit as f32).clamp(0.0, 1.0);
        div()
            .w_full()
            .h(px(6.))
            .rounded_full()
            .bg(hsla(p.muted))
            .overflow_hidden()
            .child(div().h_full().rounded_full().bg(fill).w(gpui::relative(pct)))
            .into_any_element()
    };
    Some(
        v_flex()
            .max_w(px(384.))
            .gap(px(6.))
            .child(
                h_flex()
                    .items_baseline()
                    .justify_between()
                    .gap(px(12.))
                    .text_xs()
                    .child(div().text_color(label_color).child(used_label))
                    .children(action),
            )
            .child(bar)
            .children(cap.upgrade.map(|upgrade| {
                div()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(tunnel_upgrade_copy(upgrade))
            }))
            .into_any_element(),
    )
}

/// Warning alert shown when creating a managed tunnel failed at the account limit.
pub fn render_tunnel_limit_alert(state: &Entity<AppState>, cx: &App) -> Option<AnyElement> {
    let cloud = &state.read(cx).cloud;
    let _limit = cloud.last_tunnel_limit?;
    let cap = managed_tunnel_cap(&cloud.discovery.environments, cloud.t3_chat_link.as_ref());
    let p = palette(cx);
    let title = crate::i18n::t(cx, "You're out of tunnels (they cost us money).");
    let description = match cap.upgrade {
        Some(ManagedTunnelUpgrade::Link) => crate::i18n::t(
            cx,
            "Switch to Tailscale or get more tunnels by linking your T3 Chat sub (sorry!)",
        ),
        Some(_) => crate::i18n::t(
            cx,
            "Switch to Tailscale or get more tunnels by upgrading your T3 Chat sub (sorry!)",
        ),
        None => crate::i18n::t(
            cx,
            "Switch to Tailscale or deregister an environment you no longer use (sorry!)",
        ),
    };
    let cta = tunnel_upgrade_cta(
        "tunnel-limit-alert-cta",
        cap.upgrade,
        cloud.t3_chat_linking,
        state,
        cx,
    )
    .unwrap_or_else(|| {
        let state = state.clone();
        Button::new("tunnel-limit-alert-dismiss", ButtonVariant::Outline, ButtonSize::Xs)
            .label(crate::i18n::t(cx, "Dismiss"))
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| state.set_tunnel_limit_reached(None, cx));
            })
            .into_any_element()
    });
    Some(
        h_flex()
            .w_full()
            .items_start()
            .gap(px(10.))
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.warning).opacity(0.45))
            .bg(hsla(p.warning).opacity(0.08))
            .child(div().pt(px(2.)).child(Icon::TriangleAlert.sized(px(14.)).text_color(hsla(p.warning_foreground))))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.warning_foreground))
                            .child(title),
                    )
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(description)),
            )
            .child(cta)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canceling_sign_in_releases_the_pending_environment_connection() {
        let mut cloud = CloudState {
            connecting_env_id: Some("pending-environment".into()),
            ..CloudState::default()
        };
        let generation = cloud.generation;
        cloud.cancel_sign_in();
        assert_ne!(cloud.generation, generation);
        assert!(cloud.connecting_env_id.is_none());
    }
}
