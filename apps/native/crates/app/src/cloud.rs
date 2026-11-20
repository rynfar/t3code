//! Connect screen account lifecycle, ported from web/cloud/managedAuth.tsx and relay discovery.
use std::collections::HashMap;

use gpui::Context;
use t3_client::connect::{
    ClerkAuthOutcome, PendingEmailSignIn, PendingSecondFactor,
};
use t3_core::connect::{
    CLERK_NATIVE_CALLBACK_SCHEME, FirstFactorKind, SecondFactorKind, SocialProvider,
};
use t3_core::relay::{Discovery, Environment, EnvironmentHealth, EnvironmentStatusValue};

use crate::app_state::{AppEvent, AppState};
use crate::auth_session_macos::{self, ActiveAuthSession, AuthSessionError};
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
            let discovery = if account.identity.is_some() {
                service.discover().await
            } else {
                Ok(Discovery::default())
            };
            Ok::<_, String>((account, discovery))
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
                    Ok(Ok((account, discovery))) => {
                        state.cloud.identity = account.identity;
                        state.cloud.has_clerk_session = account.has_clerk_session;
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
                            EnvironmentStatusValue::Online => EnvironmentHealth::Online,
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
        self.cancel_cloud_sign_in_internal();
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
                        state.cloud.identity = Some(
                            session
                                .identity
                                .unwrap_or_else(|| "T3 Connect account".into()),
                        );
                        state.cloud.has_clerk_session = true;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.refresh_cloud(cx);
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
        self.cancel_cloud_sign_in_internal();
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
                        state.cloud.identity = Some(
                            session
                                .identity
                                .unwrap_or_else(|| "T3 Connect account".into()),
                        );
                        state.cloud.has_clerk_session = true;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.refresh_cloud(cx);
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
                        state.cloud.identity = Some(
                            session
                                .identity
                                .unwrap_or_else(|| "T3 Connect account".into()),
                        );
                        state.cloud.has_clerk_session = true;
                        state.cloud.auth_flow = CloudAuthFlow::Idle;
                        state.refresh_cloud(cx);
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
            self.cloud.generation += 1;
            let generation = self.cloud.generation;
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

    /// Connects a discovered T3 Connect environment via DPoP exchange + `/v1/environments/{id}/connect`,
    /// registers its DPoP credential, upserts it as a relay-managed saved environment, and activates it.
    pub fn connect_cloud_environment(&mut self, env: Environment, cx: &mut Context<Self>) {
        if self.cloud.connecting_env_id.is_some() {
            return;
        }
        let env_id = env.environment_id.clone();
        let label = env.label.clone();
        self.cloud.connecting_env_id = Some(env_id.clone());
        self.cloud.error = None;
        let service = self.cloud.service.clone();
        let generation = self.cloud.generation;
        let task = self.runtime().spawn(async move {
            service.connect_and_register_environment(&env_id, None).await
        });
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                state.cloud.connecting_env_id = None;
                if state.cloud.generation != generation {
                    cx.emit(AppEvent::Phase);
                    return;
                }
                match result {
                    Ok(Ok(resp)) => {
                        state.adopt_relay_environment(resp, label, cx);
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

    fn cancel_cloud_sign_in_internal(&mut self) {
        if let Some(session) = self.cloud.active_auth_session.take() {
            session.cancel();
        }
        if let Some(task) = self.cloud.task.take() {
            task.abort();
        }
        self.cloud.generation += 1;
        self.cloud.busy = false;
        self.cloud.signing_in = false;
        self.cloud.auth_flow = CloudAuthFlow::Idle;
    }

    pub fn cancel_cloud_sign_in(&mut self, cx: &mut Context<Self>) {
        self.cancel_cloud_sign_in_internal();
        self.cloud.error = None;
        cx.emit(AppEvent::Phase);
    }

    pub fn sign_out_cloud(&mut self, cx: &mut Context<Self>) {
        self.abort_cloud_status_tasks();
        self.cancel_cloud_sign_in_internal();
        self.cloud.identity = None;
        self.cloud.has_clerk_session = false;
        self.cloud.discovery = Default::default();
        self.cloud.statuses.clear();
        self.cloud.connecting_env_id = None;
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
}
