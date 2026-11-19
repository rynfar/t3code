//! Native MCP OAuth agent sign-in approval surface.
//! Ports `apps/web/src/components/auth/ApproveAgentSurface.tsx`,
//! `apps/web/src/components/auth/ConnectAgentSurface.tsx`,
//! `apps/web/src/components/auth/ConnectAgentSurface.logic.ts`,
//! `apps/web/src/routes/approve-agent.tsx`,
//! `apps/web/src/state/connectMcpApproval.ts`,
//! and `packages/client-runtime/src/state/connectMcpApproval.ts`.

use std::time::Duration;

use gpui::{
    AnyElement, App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Task,
    Window, deferred, div, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use t3_client::supervisor::{ClientHandle, ConnectionStatus};
use t3_core::mcp::approval::{
    ARM_DELAY_MS, ApprovalUrlKind, CONNECT_UNAVAILABLE, McpApprovalAnswer, McpApprovalDecision,
    McpApprovalDecisionRequest, McpApprovalDetails, McpAuthorizationRequest, ParsedApprovalUrl,
    UNREACHABLE, UNSAFE_REDIRECT, agent_redirect_url, ceiling_reason, connect_agent_fallback_url,
    redirects_to_this_computer, resolve_approval_decision, resolve_mcp_access,
};
use t3_core::mcp::{McpClientAccess, parse_mcp_approval_code};

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

use super::access_option_card;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovalLoadState {
    Incomplete,
    NeedsCloudSignIn {
        raw_url: String,
    },
    HostedBrowserHandoff {
        environment_label: String,
        environment_url: Option<String>,
        raw_url: String,
        search_without_environment: String,
    },
    Loading,
    Invalid(String),
    Ready(McpApprovalDetails),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingDecision {
    Approve,
    Deny,
}

pub struct AgentApprovalDialog {
    state: Entity<AppState>,
    open: bool,
    generation: u64,
    parsed: Option<ParsedApprovalUrl>,
    client: Option<ClientHandle>,
    environment_label: Option<String>,
    environment_url: Option<String>,
    accepts_code: bool,
    require_level_choice: bool,
    pub(crate) load_state: ApprovalLoadState,
    pub(crate) requested_access: Option<McpClientAccess>,
    code_input: Entity<InputState>,
    pub(crate) pairing_code: String,
    pub(crate) armed: bool,
    error_message: Option<String>,
    pending: Option<PendingDecision>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    _load_task: Option<Task<()>>,
    _decide_task: Option<Task<()>>,
    _arm_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for AgentApprovalDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AgentApprovalDialog {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let code_input = cx.new(|cx| InputState::new(window, cx).placeholder("Paste a code"));
        let input_sub = cx.subscribe(&code_input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.pairing_code = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        let state_sub = cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
            if !this.open {
                return;
            }
            if matches!(event, AppEvent::Phase)
                && matches!(this.load_state, ApprovalLoadState::NeedsCloudSignIn { .. })
                && this.state.read(cx).cloud.identity.is_some()
                && let Some(parsed) = this.parsed.clone()
            {
                let handle = window.window_handle();
                this.open(parsed, handle, cx);
            }
        });
        let focus_sub = cx.observe_window_activation(window, |this, window, cx| {
            if this.open
                && window.is_window_active()
                && matches!(this.load_state, ApprovalLoadState::Ready(_))
            {
                this.arm_after_delay(cx);
            }
        });
        Self {
            state,
            open: false,
            generation: 0,
            parsed: None,
            client: None,
            environment_label: None,
            environment_url: None,
            accepts_code: false,
            require_level_choice: false,
            load_state: ApprovalLoadState::Loading,
            requested_access: None,
            code_input,
            pairing_code: String::new(),
            armed: false,
            error_message: None,
            pending: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            _load_task: None,
            _decide_task: None,
            _arm_task: None,
            _subscriptions: vec![input_sub, state_sub, focus_sub],
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(
        &mut self,
        parsed: ParsedApprovalUrl,
        window: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        self.open = true;
        self.generation += 1;
        self.parsed = Some(parsed.clone());
        self.client = None;
        self.environment_label = None;
        self.environment_url = None;
        self.requested_access = None;
        self.pairing_code.clear();
        self.armed = false;
        self.error_message = None;
        self.pending = None;
        self._load_task = None;
        self._decide_task = None;
        self._arm_task = None;

        let dialog = cx.entity().clone();
        let code_input = self.code_input.clone();
        cx.defer(move |cx| {
            let _ = cx.update_window(window, |_, window, cx| {
                code_input.update(cx, |input, cx| input.set_value("", window, cx));
                dialog.update(cx, |dialog, cx| {
                    dialog.previous_focus = window.focused(cx);
                    window.focus(&dialog.focus, cx);
                });
            });
        });

        match &parsed.kind {
            ApprovalUrlKind::Hosted {
                environment_id,
                raw_url,
                search_without_environment,
            } => {
                self.accepts_code = false;
                self.require_level_choice = true;
                let Some(env_id) = environment_id.clone() else {
                    self.load_state = ApprovalLoadState::Incomplete;
                    cx.notify();
                    return;
                };
                if let Some((saved_id, label, url)) = self.find_connected_environment(&env_id, cx) {
                    self.environment_label = Some(label);
                    self.environment_url = Some(url);
                    self.load_from_client(saved_id, parsed.authorization, window, cx);
                    return;
                }
                // Not connected locally: check T3 Connect account and relay discovery.
                let cloud = &self.state.read(cx).cloud;
                if cloud.identity.is_none() {
                    if !cloud.busy && !cloud.discovery.listed && cloud.error.is_none() {
                        self.state.update(cx, |state, cx| state.refresh_cloud(cx));
                    }
                    self.load_state = ApprovalLoadState::NeedsCloudSignIn {
                        raw_url: raw_url.clone(),
                    };
                    cx.notify();
                    return;
                }
                let discovered = cloud
                    .discovery
                    .environments
                    .iter()
                    .find(|e| e.environment_id == env_id);
                let (label, env_url) = match discovered {
                    Some(entry) => (entry.label.clone(), Some(entry.endpoint.http_base_url.clone())),
                    None => (env_id, None),
                };
                self.environment_label = Some(label.clone());
                self.environment_url = env_url.clone();
                self.load_state = ApprovalLoadState::HostedBrowserHandoff {
                    environment_label: label,
                    environment_url: env_url,
                    raw_url: raw_url.clone(),
                    search_without_environment: search_without_environment.clone(),
                };
                cx.notify();
            }
            ApprovalUrlKind::Direct { origin } => {
                self.accepts_code = true;
                self.require_level_choice = false;
                if let Some((saved_id, label, url)) =
                    self.find_environment_for_origin(origin.as_deref(), cx)
                {
                    self.environment_label = Some(label);
                    self.environment_url = Some(url);
                    self.load_from_client(saved_id, parsed.authorization, window, cx);
                } else {
                    self.load_state = ApprovalLoadState::Invalid(UNREACHABLE.to_owned());
                    cx.notify();
                }
            }
        }
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.generation += 1;
        self._load_task = None;
        self._decide_task = None;
        self._arm_task = None;
        if let Some(prev) = self.previous_focus.take() {
            window.focus(&prev, cx);
        }
        cx.notify();
    }

    fn find_connected_environment(
        &self,
        env_id: &str,
        cx: &App,
    ) -> Option<(String, String, String)> {
        let state = self.state.read(cx);
        let active_id = state.environments.active.as_deref();
        for env in &state.environments.environments {
            let matches = env.environment_id.as_deref() == Some(env_id) || env.id == env_id;
            if !matches {
                continue;
            }
            let connected = if active_id == Some(env.id.as_str()) {
                state.phase == crate::app_state::Phase::Online(ConnectionStatus::Connected)
            } else {
                state
                    .fleet
                    .environment(&env.id)
                    .is_some_and(|v| v.status == ConnectionStatus::Connected)
            };
            if connected {
                return Some((env.id.clone(), env.label.clone(), env.http_base.clone()));
            }
        }
        None
    }

    fn find_environment_for_origin(
        &self,
        origin: Option<&str>,
        cx: &App,
    ) -> Option<(String, String, String)> {
        let state = self.state.read(cx);
        if let Some(origin) = origin {
            let normalized = origin.trim_end_matches('/');
            for env in &state.environments.environments {
                if env.http_base.trim_end_matches('/') == normalized {
                    return Some((env.id.clone(), env.label.clone(), env.http_base.clone()));
                }
            }
        }
        let active_id = state.environments.active.as_deref()?;
        let env = state
            .environments
            .environments
            .iter()
            .find(|e| e.id == active_id)?;
        Some((env.id.clone(), env.label.clone(), env.http_base.clone()))
    }

    fn load_from_client(
        &mut self,
        saved_id: String,
        authorization: McpAuthorizationRequest,
        window: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        self.load_state = ApprovalLoadState::Loading;
        let generation = self.generation;
        let active_client = {
            let state = self.state.read(cx);
            if state.environments.active.as_deref() == Some(saved_id.as_str()) {
                state.client_handle()
            } else {
                None
            }
        };
        let client_future = if active_client.is_none() {
            Some(self.state.read(cx).connections.client(&saved_id))
        } else {
            None
        };
        let runtime = self.state.read(cx).runtime().clone();
        let payload = serde_json::to_value(&authorization).unwrap_or(serde_json::Value::Null);
        cx.notify();

        self._load_task = Some(cx.spawn(async move |this, cx| {
            let client = match active_client {
                Some(c) => Some(c),
                None => match client_future {
                    Some(fut) => fut.await,
                    None => None,
                },
            };
            let Some(client) = client else {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.load_state = ApprovalLoadState::Invalid(UNREACHABLE.to_owned());
                        cx.notify();
                    }
                });
                return;
            };
            let net_client = client.clone();
            let result = runtime
                .spawn(async move {
                    match net_client
                        .post_access("/oauth/mcp/approval", Some(payload))
                        .await
                    {
                        Ok(val) => match McpApprovalAnswer::from_response_json(&val) {
                            Ok(answer) => answer,
                            Err(msg) => McpApprovalAnswer::Refused(msg),
                        },
                        Err(msg) => McpApprovalAnswer::Refused(msg),
                    }
                })
                .await;
            let answer = result.unwrap_or_else(|_| McpApprovalAnswer::Refused(UNREACHABLE.to_owned()));
            let _ = this.update(cx, |this, cx| {
                if !this.open || this.generation != generation {
                    return;
                }
                this.client = Some(client);
                this._load_task = None;
                match answer {
                    McpApprovalAnswer::Redirect(url) => {
                        if let Some(safe) = agent_redirect_url(&url) {
                            cx.open_url(&safe);
                            let dialog = cx.entity().clone();
                            cx.defer(move |cx| {
                                let _ = cx.update_window(window, |_, window, cx| {
                                    dialog.update(cx, |d, cx| d.close(window, cx));
                                });
                            });
                        } else {
                            this.load_state =
                                ApprovalLoadState::Invalid(UNSAFE_REDIRECT.to_owned());
                        }
                    }
                    McpApprovalAnswer::Refused(msg) => {
                        this.load_state = ApprovalLoadState::Invalid(msg);
                    }
                    McpApprovalAnswer::Details(details) => {
                        this.load_state = ApprovalLoadState::Ready(details);
                        this.arm_after_delay(cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn arm_after_delay(&mut self, cx: &mut Context<Self>) {
        self.armed = false;
        let generation = self.generation;
        self._arm_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(ARM_DELAY_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.open && this.generation == generation {
                    this.armed = true;
                    this._arm_task = None;
                    cx.notify();
                }
            });
        }));
    }

    fn effective_access(&self, details: &McpApprovalDetails) -> Option<McpClientAccess> {
        let has_code = !self.pairing_code.trim().is_empty();
        if self.require_level_choice && self.requested_access.is_none() && !has_code {
            return None;
        }
        Some(resolve_mcp_access(
            self.requested_access,
            details.runtime_mode_ceiling.as_ref(),
            has_code,
        ))
    }

    fn submit_decision(
        &mut self,
        choice: PendingDecision,
        details: McpApprovalDetails,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(authorization) = self.parsed.as_ref().map(|p| p.authorization.clone()) else {
            return;
        };
        let decision = match choice {
            PendingDecision::Deny => Some(McpApprovalDecision::Deny {}),
            PendingDecision::Approve => self.effective_access(&details).and_then(|access| {
                resolve_approval_decision(&self.pairing_code, access, &details)
            }),
        };
        let Some(decision) = decision else {
            return;
        };

        let generation = self.generation;
        let window_handle = window.window_handle();
        self.pending = Some(choice);
        self.error_message = None;
        cx.notify();

        let payload = serde_json::to_value(&McpApprovalDecisionRequest {
            authorization,
            decision,
        })
        .unwrap_or(serde_json::Value::Null);
        let runtime = self.state.read(cx).runtime().clone();
        self._decide_task = Some(cx.spawn(async move |this, cx| {
            let answer = runtime
                .spawn(async move {
                    match client
                        .post_access("/oauth/mcp/decision", Some(payload))
                        .await
                    {
                        Ok(val) => match McpApprovalAnswer::from_response_json(&val) {
                            Ok(answer) => answer,
                            Err(msg) => McpApprovalAnswer::Refused(msg),
                        },
                        Err(msg) => McpApprovalAnswer::Refused(msg),
                    }
                })
                .await
                .unwrap_or_else(|_| McpApprovalAnswer::Refused(UNREACHABLE.to_owned()));
            let _ = this.update(cx, |this, cx| {
                if !this.open || this.generation != generation {
                    return;
                }
                this._decide_task = None;
                match answer {
                    McpApprovalAnswer::Redirect(url) => {
                        if let Some(safe) = agent_redirect_url(&url) {
                            cx.open_url(&safe);
                            let dialog = cx.entity().clone();
                            cx.defer(move |cx| {
                                let _ = cx.update_window(window_handle, |_, window, cx| {
                                    dialog.update(cx, |d, cx| d.close(window, cx));
                                });
                            });
                            return;
                        }
                        this.pending = None;
                        this.error_message = Some(UNSAFE_REDIRECT.to_owned());
                    }
                    McpApprovalAnswer::Refused(msg) => {
                        this.pending = None;
                        this.error_message = Some(msg);
                    }
                    McpApprovalAnswer::Details(_) => {
                        this.pending = None;
                        this.error_message = Some("The sign-in could not continue.".to_owned());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn render_heading(title: impl Into<SharedString>, description: AnyElement, cx: &App) -> AnyElement {
        let p = palette(cx);
        v_flex()
            .gap(px(6.))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.muted_foreground))
                    .child("Agent sign-in"),
            )
            .child(
                div()
                    .text_xl()
                    .line_height(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(hsla(p.popover_foreground))
                    .child(title.into()),
            )
            .child(description)
            .into_any_element()
    }

    fn render_ready(&self, details: &McpApprovalDetails, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let access = self.effective_access(details);
        let can_approve = self.armed
            && self.pending.is_none()
            && access.is_some_and(|a| resolve_approval_decision(&self.pairing_code, a, details).is_some());
        let code_access = parse_mcp_approval_code(&self.pairing_code).map(|parsed| parsed.access);
        let approve_level = code_access.or(access);
        let env_name = self
            .environment_label
            .as_deref()
            .unwrap_or(&details.environment_host);
        let env_desc = if self.environment_label.is_some() {
            format!(
                "This agent wants to use the threads in every project on {env_name} ({}).",
                details.environment_host
            )
        } else {
            format!("This agent wants to use the threads in every project on {env_name}.")
        };
        let redirect_note = if redirects_to_this_computer(&details.redirect_host) {
            " on the computer that opened this page"
        } else {
            " \u{00b7} whoever runs it gets this access"
        };
        let approve_label = if self.pending == Some(PendingDecision::Approve) {
            "Approving\u{2026}".to_owned()
        } else if let Some(level) = approve_level {
            format!("Approve {}", level.label())
        } else {
            "Choose what it may do".to_owned()
        };
        let deny_label = if self.pending == Some(PendingDecision::Deny) {
            "Denying\u{2026}"
        } else {
            "Deny"
        };
        let details_approve = details.clone();
        let details_deny = details.clone();
        let view = cx.entity().clone();

        v_flex()
            .gap(px(16.))
            .child(Self::render_heading(
                format!("Connect {}", details.client_name),
                div()
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child(env_desc)
                    .into_any_element(),
                cx,
            ))
            .child(
                v_flex()
                    .gap(px(6.))
                    .text_sm()
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .child(div().w(px(108.)).flex_none().text_color(hsla(p.muted_foreground)).child("App"))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(4.))
                                    .child(div().text_color(hsla(p.foreground)).child(details.client_name.clone()))
                                    .child(
                                        div()
                                            .text_color(hsla(p.muted_foreground))
                                            .child("\u{00b7} name set by the app"),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .child(
                                div()
                                    .w(px(108.))
                                    .flex_none()
                                    .text_color(hsla(p.muted_foreground))
                                    .child("Access goes to"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .flex_wrap()
                                            .child(
                                                div()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(hsla(p.foreground))
                                                    .child(details.redirect_host.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_color(hsla(p.muted_foreground))
                                                    .child(redirect_note),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .pt(px(2.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child("Only approve a sign-in you just started."),
                    ),
            )
            .when(self.accepts_code, |this| {
                this.child(
                    v_flex()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground))
                                .child("Sign-in code"),
                        )
                        .child(Input::new(&self.code_input).disabled(self.pending.is_some()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child("Get one from Use T3 Code in another app (MCP), Settings \u{2192} Sharing & access, or t3 auth pairing create on this machine."),
                        ),
                )
            })
            .child(if let Some(level) = code_access {
                h_flex()
                    .gap(px(6.))
                    .text_sm()
                    .child(div().text_color(hsla(p.muted_foreground)).child("Access"))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(level.label()),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child("What it may do"),
                    )
                    .children(McpClientAccess::ALL.into_iter().map(|option| {
                        let view = view.clone();
                        let reason = ceiling_reason(details, option);
                        access_option_card(
                            SharedString::from(format!("mcp-consent-{}", option.as_str())),
                            option,
                            access == Some(option),
                            reason.as_deref(),
                            None,
                            move |_, cx| {
                                view.update(cx, |this, cx| {
                                    this.requested_access = Some(option);
                                    cx.notify();
                                });
                            },
                            cx,
                        )
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child("Beyond read only, it can start, message and stop threads, and none of them can run with more than the mode you pick."),
                    )
                    .into_any_element()
            })
            .when_some(self.error_message.as_ref(), |this, err| {
                this.child(
                    div()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(hsla(p.error).opacity(0.4))
                        .bg(hsla(p.error_surface))
                        .px(px(12.))
                        .py(px(8.))
                        .text_xs()
                        .text_color(hsla(p.error_foreground))
                        .child(err.clone()),
                )
            })
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        Button::new("mcp-consent-approve", ButtonVariant::Default, ButtonSize::Sm)
                            .label(approve_label)
                            .disabled(!can_approve)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.submit_decision(
                                    PendingDecision::Approve,
                                    details_approve.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("mcp-consent-deny", ButtonVariant::Outline, ButtonSize::Sm)
                            .label(deny_label)
                            .disabled(self.pending.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.submit_decision(
                                    PendingDecision::Deny,
                                    details_deny.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
            .when(!self.accepts_code, |this| {
                this.child(
                    div()
                        .id("mcp-use-code-instead")
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.muted_foreground))
                        .cursor_pointer()
                        .hover(|style| style.underline().text_color(hsla(p.foreground)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.accepts_code = true;
                            cx.notify();
                        }))
                        .child("Use a sign-in code instead"),
                )
            })
            .into_any_element()
    }
}

impl Render for AgentApprovalDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let body = match &self.load_state {
            ApprovalLoadState::Incomplete => Self::render_heading(
                "This sign-in link is incomplete",
                div()
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child("Close this page and start the sign-in again from your agent.")
                    .into_any_element(),
                cx,
            ),
            ApprovalLoadState::NeedsCloudSignIn { raw_url } => {
                let browser_url = raw_url.clone();
                v_flex()
                    .gap(px(16.))
                    .child(Self::render_heading(
                        "Sign in to approve",
                        div()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child("Sign in to T3 Connect with the account this environment is linked to.")
                            .into_any_element(),
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                Button::new(
                                    "mcp-approve-signin",
                                    ButtonVariant::Default,
                                    ButtonSize::Sm,
                                )
                                .label("Sign in")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.update(cx, |state, cx| state.sign_in_cloud(cx));
                                })),
                            )
                            .child(
                                Button::new(
                                    "mcp-approve-browser",
                                    ButtonVariant::Outline,
                                    ButtonSize::Sm,
                                )
                                .label("Open in browser")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(safe) = agent_redirect_url(&browser_url) {
                                        cx.open_url(&safe);
                                    }
                                    this.close(window, cx);
                                })),
                            ),
                    )
                    .into_any_element()
            }
            ApprovalLoadState::HostedBrowserHandoff {
                environment_label,
                environment_url,
                raw_url,
                search_without_environment,
            } => {
                let browser_url = raw_url.clone();
                let fallback_code_url = environment_url
                    .as_deref()
                    .and_then(|url| connect_agent_fallback_url(url, search_without_environment));
                v_flex()
                    .gap(px(16.))
                    .child(Self::render_heading(
                        format!("Approve agent on {environment_label}"),
                        div()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child(if environment_url.is_some() {
                                "This environment is not connected in T3 Code Native yet. Complete approval in your browser with T3 Connect, or use a sign-in code."
                            } else {
                                CONNECT_UNAVAILABLE
                            })
                            .into_any_element(),
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                Button::new(
                                    "mcp-hosted-open-browser",
                                    ButtonVariant::Default,
                                    ButtonSize::Sm,
                                )
                                .label("Continue in browser")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(safe) = agent_redirect_url(&browser_url) {
                                        cx.open_url(&safe);
                                    }
                                    this.close(window, cx);
                                })),
                            )
                            .when_some(fallback_code_url, |this, code_url| {
                                this.child(
                                    Button::new(
                                        "mcp-hosted-code-fallback",
                                        ButtonVariant::Outline,
                                        ButtonSize::Sm,
                                    )
                                    .label("Use a sign-in code instead")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(safe) = agent_redirect_url(&code_url) {
                                            cx.open_url(&safe);
                                        }
                                        this.close(window, cx);
                                    })),
                                )
                            }),
                    )
                    .into_any_element()
            }
            ApprovalLoadState::Loading => v_flex()
                .gap(px(16.))
                .child(Self::render_heading(
                    "Checking the sign-in request",
                    div()
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child("One moment while this environment verifies the agent's request.")
                        .into_any_element(),
                    cx,
                ))
                .child(Icon::LoaderCircle.render(px(20.), hsla(p.muted_foreground)))
                .into_any_element(),
            ApprovalLoadState::Invalid(message) => v_flex()
                .gap(px(12.))
                .child(Self::render_heading(
                    "This sign-in cannot continue",
                    div()
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child(message.clone())
                        .into_any_element(),
                    cx,
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child("Close this page and start the sign-in again from your agent."),
                )
                .into_any_element(),
            ApprovalLoadState::Ready(details) => self.render_ready(details, cx),
        };

        deferred(
            div()
                .id("mcp-approval-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close(window, cx)),
                )
                .child(
                    v_flex()
                        .id("mcp-approval-dialog")
                        .track_focus(&self.focus)
                        .w_full()
                        .max_w(px(540.))
                        .max_h(px(740.))
                        .rounded(px(16.))
                        .relative()
                        .child(crate::glass::backdrop(
                            16.,
                            hsla(p.dialog_surface),
                            hsla(p.dialog_border),
                            cx,
                        ))
                        .shadow(vec![BoxShadow {
                            color: gpui::black().opacity(0.65),
                            offset: point(px(0.), px(24.)),
                            blur_radius: px(64.),
                            spread_radius: px(-24.),
                            inset: false,
                        }])
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                            this.close(window, cx);
                            cx.stop_propagation();
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                this.close(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(
                            div()
                                .id("mcp-approval-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p(px(24.))
                                .child(body),
                        ),
                ),
        )
        .with_priority(52)
        .into_any_element()
    }
}
