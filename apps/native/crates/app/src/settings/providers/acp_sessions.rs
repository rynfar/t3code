//! Project-scoped session management for ACP Registry agents.

use std::collections::BTreeMap;
use std::future::Future;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, IntoElement, ParentElement, SharedString, Styled,
    Task, Window, div,
};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use serde::Deserialize;
use serde_json::{Value, json};
use t3_client::session::RpcError;
use t3_core::{
    auth::{ORCHESTRATION_OPERATE_SCOPE, ORCHESTRATION_READ_SCOPE},
    settings::providers::ACP_REGISTRY,
    sidebar::grouping::EnvironmentOwner,
    usage::PROVIDERS_MANAGE_SCOPE,
};

use super::{Selected, SettingsView, failure_text, flat_section};
use crate::{
    app_state::ScopedRouteAction,
    components::motion::Spinner,
    i18n::{t, t_fmt},
    settings::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, r},
    theme::{ThemeSlot, ThemeSlotExt, hsla, palette},
    toast::{Toast, ToastKind},
};

#[derive(Clone, PartialEq, Eq)]
struct Target {
    owner: Option<EnvironmentOwner>,
    connection: Option<u64>,
    instance: String,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SessionsCapability {
    can_list: bool,
    can_load: bool,
    can_resume: bool,
    can_delete: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Capabilities {
    native_sessions: Option<SessionsCapability>,
    configurable_providers: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    session_id: String,
    title: Option<String>,
    updated_at: Option<String>,
    imported_thread_id: Option<String>,
    #[serde(default)]
    native_history_pending: bool,
}

impl Session {
    fn needs_import(&self) -> bool {
        self.imported_thread_id.is_none() || self.native_history_pending
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionPage {
    sessions: Vec<Session>,
    next_cursor: Option<String>,
    // The live list result can narrow availability after the config snapshot.
    can_load: bool,
    can_resume: bool,
    can_delete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Imported {
    thread_id: String,
    imported: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Routing {
    api_type: String,
    base_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Provider {
    provider_id: String,
    supported: Vec<String>,
    required: bool,
    current: Option<Routing>,
}

struct ProviderDraft {
    provider: Provider,
    protocol: String,
    url: Entity<InputState>,
    headers: Entity<InputState>,
    _url_change: gpui::Subscription,
}

#[derive(Default)]
pub(super) struct AcpSessionsState {
    target: Option<Target>,
    project: Option<String>,
    projects: Vec<SelectOption>,
    capabilities: Option<Result<Capabilities, String>>,
    capabilities_task: Option<Task<()>>,
    can_logout: bool,
    page: ProjectData,
}

impl AcpSessionsState {
    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        self.page
            .providers
            .iter()
            .flat_map(|draft| [&draft.url, &draft.headers])
    }
}

#[derive(Default)]
struct ProjectData {
    sessions: Vec<Session>,
    cursor: Option<String>,
    listed: bool,
    capabilities: Option<SessionsCapability>,
    providers: Vec<ProviderDraft>,
    pending: Option<Task<()>>,
    error: Option<String>,
}

fn decode<T: serde::de::DeserializeOwned>(
    result: Result<Value, RpcError>,
    fallback: &str,
) -> Result<T, String> {
    serde_json::from_value(result.map_err(|error| failure_text(&error, fallback))?)
        .map_err(|_| fallback.to_owned())
}

impl SettingsView {
    pub(super) fn sync_acp_sessions(
        &mut self,
        selected: Option<&Selected>,
        cx: &mut Context<Self>,
    ) {
        let live = selected
            .and_then(|selected| selected.live.as_ref())
            .filter(|live| {
                live.driver == ACP_REGISTRY && live.installed && !super::fixture_active(cx)
            });
        let Some(live) = live else {
            self.providers.acp_sessions = AcpSessionsState::default();
            return;
        };
        let app = self.state.read(cx);
        let owner = self.scoped_primary_owner(cx);
        let explicitly_scoped = self.scope.machine.is_some() || self.scope.project.is_some();
        if owner.is_none() && explicitly_scoped {
            self.providers.acp_sessions = AcpSessionsState::default();
            return;
        }
        let target = Target {
            owner: owner.clone(),
            connection: app.connection_id(),
            instance: live.instance_id.clone(),
        };
        let changed = self.providers.acp_sessions.target.as_ref() != Some(&target);
        if changed {
            self.providers.acp_sessions = AcpSessionsState {
                target: Some(target),
                ..Default::default()
            };
        }
        let projects: Vec<_> = if self.scope.project.is_some() {
            self.scoped_provider_project_members(cx)
                .into_iter()
                .map(|member| SelectOption::new(member.project.id, member.project.title))
                .collect()
        } else if let Some(entry) = owner
            .as_ref()
            .and_then(|owner| app.scoped_environment(&owner.saved_id))
        {
            entry
                .summary
                .projects
                .iter()
                .filter(|member| !member.is_scratch)
                .map(|member| {
                    SelectOption::new(member.project.id.clone(), member.project.title.clone())
                })
                .collect()
        } else {
            app.shell
                .snapshot
                .iter()
                .flat_map(|snapshot| &snapshot.projects)
                .map(|project| SelectOption::new(project.id.clone(), project.title.clone()))
                .collect()
        };
        let state = &mut self.providers.acp_sessions;
        let project = state
            .project
            .as_ref()
            .filter(|id| projects.iter().any(|option| option.value.as_ref() == *id))
            .cloned()
            .or_else(|| projects.first().map(|option| option.value.to_string()));
        if state.project != project {
            state.page = ProjectData::default();
        }
        state.project = project;
        state.projects = projects;
        state.can_logout = live.auth.can_logout == Some(true)
            && !live
                .setup
                .as_ref()
                .is_some_and(|setup| setup.can_authenticate);
        if state.capabilities.is_none()
            && state.capabilities_task.is_none()
            && self.acp_allowed(ORCHESTRATION_READ_SCOPE, cx)
        {
            self.load_acp_capabilities(cx);
        }
    }

    fn acp_allowed(&self, scope: &str, cx: &App) -> bool {
        self.scoped_connected_now(cx)
            && self
                .scoped_session(cx)
                .is_none_or(|session| session.grants(scope))
    }

    fn load_acp_capabilities(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.providers.acp_sessions.target.clone() else {
            return;
        };
        if !self.acp_allowed(ORCHESTRATION_READ_SCOPE, cx) {
            return;
        }
        // The native provider snapshot currently omits these two optional capability fields.
        let request = self.scoped_rpc(ORCHESTRATION_READ_SCOPE, "server.getConfig", json!({}), cx);
        self.providers.acp_sessions.capabilities = None;
        self.providers.acp_sessions.capabilities_task = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.providers.acp_sessions;
                if state.target.as_ref() != Some(&target) {
                    return;
                }
                let fallback = t(cx, "Could not load ACP capabilities.");
                state.capabilities = Some(
                    result
                        .map_err(|error| failure_text(&error, fallback))
                        .and_then(|value| {
                            let provider = value["providers"]
                                .as_array()
                                .and_then(|providers| {
                                    providers
                                        .iter()
                                        .find(|p| p["instanceId"] == target.instance)
                                })
                                .cloned()
                                .ok_or_else(|| fallback.to_owned())?;
                            serde_json::from_value(provider).map_err(|_| fallback.to_owned())
                        }),
                );
                state.capabilities_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn acp_operation(
        &self,
        scope: &'static str,
        method: &'static str,
        mut payload: Value,
        needs_project: bool,
        cx: &mut Context<Self>,
    ) -> Option<(
        Target,
        Option<String>,
        impl Future<Output = Result<Value, RpcError>> + use<>,
    )> {
        let state = &self.providers.acp_sessions;
        let target = state.target.clone()?;
        if state.page.pending.is_some()
            || target.owner != self.scoped_primary_owner(cx)
            || target.connection != self.state.read(cx).connection_id()
            || !self.acp_allowed(scope, cx)
        {
            return None;
        }
        let project = state.project.clone();
        if needs_project {
            payload["projectId"] = json!(project.as_ref()?);
        }
        payload["instanceId"] = json!(target.instance);
        Some((target, project, self.scoped_rpc(scope, method, payload, cx)))
    }

    fn acp_page_current(&self, target: &Target, project: Option<&str>, cx: &App) -> bool {
        let state = &self.providers.acp_sessions;
        self.section == t3_core::settings::search::Section::Providers
            && state.target.as_ref() == Some(target)
            && state.project.as_deref() == project
            && target.owner == self.scoped_primary_owner(cx)
            && target.connection == self.state.read(cx).connection_id()
    }

    fn load_acp_sessions(&mut self, more: bool, cx: &mut Context<Self>) {
        let cursor = if more {
            let Some(cursor) = self.providers.acp_sessions.page.cursor.clone() else {
                return;
            };
            json!({ "cursor": cursor })
        } else {
            json!({})
        };
        let Some((target, project, request)) = self.acp_operation(
            ORCHESTRATION_READ_SCOPE,
            "server.listAcpRegistrySessions",
            cursor,
            true,
            cx,
        ) else {
            return;
        };
        self.providers.acp_sessions.page.pending = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if !this.acp_page_current(&target, project.as_deref(), cx) {
                    return;
                }
                let page = &mut this.providers.acp_sessions.page;
                page.pending = None;
                match decode::<SessionPage>(result, t(cx, "Could not list ACP sessions")) {
                    Ok(reply) => {
                        if !more {
                            page.sessions.clear();
                        }
                        for session in reply.sessions {
                            if let Some(current) = page
                                .sessions
                                .iter_mut()
                                .find(|row| row.session_id == session.session_id)
                            {
                                *current = session;
                            } else {
                                page.sessions.push(session);
                            }
                        }
                        page.cursor = reply.next_cursor;
                        page.capabilities = Some(SessionsCapability {
                            can_list: true,
                            can_load: reply.can_load,
                            can_resume: reply.can_resume,
                            can_delete: reply.can_delete,
                        });
                        page.listed = true;
                        page.error = None;
                    }
                    Err(error) => page.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn mutate_acp_session(&mut self, action: SessionAction, cx: &mut Context<Self>) {
        let (scope, method, payload, needs_project, error) = match &action {
            SessionAction::Import(session) => (
                ORCHESTRATION_OPERATE_SCOPE,
                "server.importAcpRegistrySession",
                json!({
                    "sessionId": session.session_id,
                    "title": session.title.as_deref().filter(|s| !s.trim().is_empty()),
                    "updatedAt": session.updated_at.as_deref().filter(|s| !s.trim().is_empty()),
                }),
                true,
                "Could not import ACP session",
            ),
            SessionAction::Delete(id) => (
                ORCHESTRATION_OPERATE_SCOPE,
                "server.deleteAcpRegistrySession",
                json!({ "sessionId": id }),
                true,
                "Could not delete ACP session",
            ),
            SessionAction::Logout => (
                PROVIDERS_MANAGE_SCOPE,
                "server.logoutAcpRegistry",
                json!({}),
                false,
                "Could not log out of ACP agent",
            ),
        };
        let Some((target, project, request)) =
            self.acp_operation(scope, method, payload, needs_project, cx)
        else {
            return;
        };
        self.providers.acp_sessions.page.pending = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if !this.acp_page_current(&target, project.as_deref(), cx) {
                    return;
                }
                let page = &mut this.providers.acp_sessions.page;
                page.pending = None;
                page.error = match (action, result) {
                    (_, Err(failure)) => Some(failure_text(&failure, t(cx, error))),
                    (SessionAction::Delete(id), Ok(_)) => {
                        page.sessions.retain(|session| session.session_id != id);
                        None
                    }
                    (SessionAction::Logout, Ok(_)) => {
                        *page = ProjectData::default();
                        None
                    }
                    (SessionAction::Import(session), Ok(value)) => {
                        match serde_json::from_value::<Imported>(value) {
                            Ok(reply) => {
                                if let Some(row) = page
                                    .sessions
                                    .iter_mut()
                                    .find(|row| row.session_id == session.session_id)
                                {
                                    row.imported_thread_id = Some(reply.thread_id);
                                    row.native_history_pending = false;
                                }
                                let message = t(
                                    cx,
                                    if reply.imported {
                                        "ACP session imported"
                                    } else {
                                        "ACP session already imported"
                                    },
                                );
                                this.state.update(cx, |state, cx| {
                                    state.show_toast(Toast::new(ToastKind::Success, message), cx)
                                });
                                None
                            }
                            Err(_) => Some(t(cx, error).to_owned()),
                        }
                    }
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn confirm_acp_session_action(
        &self,
        action: SessionAction,
        title: String,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        let state = &self.providers.acp_sessions;
        let Some(target) = state.target.clone() else {
            return;
        };
        let project = state.project.clone();
        let view = cx.entity().downgrade();
        crate::dialog::ask(
            title,
            "",
            t(cx, label),
            move |_, cx| {
                cx.defer(move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        if this.acp_page_current(&target, project.as_deref(), cx) {
                            this.mutate_acp_session(action, cx);
                        }
                    });
                });
            },
            cx,
        );
    }

    fn open_acp_thread(&self, thread: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.providers.acp_sessions.target.as_ref() else {
            return;
        };
        let destination = target.owner.clone();
        if !self.acp_allowed(ORCHESTRATION_READ_SCOPE, cx)
            || destination != self.scoped_primary_owner(cx)
            || destination.as_ref().is_some_and(|owner| {
                !self
                    .state
                    .read(cx)
                    .scoped_ready(owner, ORCHESTRATION_READ_SCOPE)
            })
        {
            return;
        }
        self.state.update(cx, |app, cx| {
            if let Some(owner) = destination {
                app.navigate_scoped(owner, ScopedRouteAction::Thread(thread.to_owned()), cx);
            } else {
                app.open_related_thread(thread, cx);
            }
        });
        super::super::close(window, cx);
    }

    fn load_acp_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((target, project, request)) = self.acp_operation(
            ORCHESTRATION_READ_SCOPE,
            "server.listAcpRegistryProviders",
            json!({}),
            true,
            cx,
        ) else {
            return;
        };
        self.providers.acp_sessions.page.pending =
            Some(cx.spawn_in(window, async move |this, cx| {
                let result = request.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if !this.acp_page_current(&target, project.as_deref(), cx) {
                        return;
                    }
                    #[derive(Deserialize)]
                    struct Reply {
                        providers: Vec<Provider>,
                    }
                    let page = &mut this.providers.acp_sessions.page;
                    page.pending = None;
                    match decode::<Reply>(result, t(cx, "Could not list ACP providers")) {
                        Ok(result) => {
                            page.providers = result
                                .providers
                                .into_iter()
                                .map(|provider| ProviderDraft::new(provider, window, cx))
                                .collect();
                            page.error = None;
                        }
                        Err(error) => page.error = Some(error),
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn save_acp_routing(
        &mut self,
        id: &str,
        disable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self
            .providers
            .acp_sessions
            .page
            .providers
            .iter()
            .find(|draft| draft.provider.provider_id == id)
        else {
            return;
        };
        let mut payload = json!({ "providerId": id });
        if disable {
            if draft.provider.required || draft.provider.current.is_none() {
                return;
            }
        } else {
            let url = draft.url.read(cx).value();
            if draft.protocol.is_empty() || url.trim().is_empty() {
                return;
            }
            payload["apiType"] = json!(draft.protocol);
            payload["baseUrl"] = json!(url.trim());
            match parse_headers(draft.headers.read(cx).value().trim()) {
                Ok(Some(headers)) => payload["headers"] = json!(headers),
                Ok(None) => {}
                Err(_) => {
                    self.providers.acp_sessions.page.error =
                        Some(t(cx, "Headers must be a JSON object with string values.").to_owned());
                    cx.notify();
                    return;
                }
            }
        }
        let (method, fallback) = if disable {
            (
                "server.disableAcpRegistryProvider",
                "Could not disable ACP provider",
            )
        } else {
            (
                "server.setAcpRegistryProvider",
                "Could not configure ACP provider",
            )
        };
        let Some((target, project, request)) =
            self.acp_operation(PROVIDERS_MANAGE_SCOPE, method, payload, true, cx)
        else {
            return;
        };
        self.providers.acp_sessions.page.pending =
            Some(cx.spawn_in(window, async move |this, cx| {
                let result = request.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if !this.acp_page_current(&target, project.as_deref(), cx) {
                        return;
                    }
                    let page = &mut this.providers.acp_sessions.page;
                    page.pending = None;
                    match result {
                        Ok(_) => this.load_acp_providers(window, cx),
                        Err(error) => page.error = Some(failure_text(&error, t(cx, fallback))),
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }

    pub(super) fn render_acp_sessions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = &self.providers.acp_sessions;
        state.target.as_ref()?;
        let p = palette(cx);
        let capabilities = state
            .capabilities
            .as_ref()
            .and_then(|result| result.as_ref().ok());
        let caps = state
            .page
            .capabilities
            .or_else(|| capabilities.and_then(|capabilities| capabilities.native_sessions))
            .unwrap_or_default();
        let routing = capabilities.is_some_and(|caps| caps.configurable_providers);
        let capability_error = state
            .capabilities
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .cloned();
        let busy = state.page.pending.is_some();
        let reading = state.capabilities_task.is_some();
        if !caps.can_list && !routing && !state.can_logout && !reading && capability_error.is_none()
        {
            return None;
        }
        let can_read = self.acp_allowed(ORCHESTRATION_READ_SCOPE, cx);
        let can_operate = self.acp_allowed(ORCHESTRATION_OPERATE_SCOPE, cx);
        let can_manage = self.acp_allowed(PROVIDERS_MANAGE_SCOPE, cx);
        let mut control = h_flex().gap(r(6.)).flex_wrap().justify_end();
        if reading || busy {
            control = control.child(Spinner::new(
                "acp-sessions-loading",
                gpui::px(14.),
                hsla(p.muted_foreground),
            ));
        }
        if (caps.can_list || routing) && state.projects.len() > 1 {
            let select = cx.listener(|this, value: &SharedString, _, cx| {
                let state = &mut this.providers.acp_sessions;
                if state.page.pending.is_none()
                    && state.projects.iter().any(|option| &option.value == value)
                    && state.project.as_deref() != Some(value.as_ref())
                {
                    state.project = Some(value.to_string());
                    state.page = ProjectData::default();
                    cx.notify();
                }
            });
            control = control.child(
                Select::new("acp-session-project", state.projects.clone())
                    .selected(state.project.clone().unwrap_or_default())
                    .show_check()
                    .width(148.)
                    .disabled(busy || !can_read)
                    .on_change(move |value, window, cx| select(&value, window, cx)),
            );
        }
        if caps.can_list && state.project.is_some() {
            control = control.child(
                Button::new("acp-list-sessions", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(t(
                        cx,
                        if state.page.listed {
                            "Refresh"
                        } else {
                            "List sessions"
                        },
                    ))
                    .disabled(busy || !can_read)
                    .on_click(cx.listener(|this, _, _, cx| this.load_acp_sessions(false, cx))),
            );
        }
        if state.can_logout {
            control = control.child(
                Button::new("acp-logout", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(t(cx, "Log out"))
                    .disabled(busy || !can_manage)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_acp_session_action(
                            SessionAction::Logout,
                            t(cx, "Log out of ACP agent?").to_owned(),
                            "Log out",
                            cx,
                        )
                    })),
            );
        }
        if capability_error.is_some() {
            control = control.child(
                Button::new(
                    "acp-retry-capabilities",
                    ButtonVariant::Outline,
                    ButtonSize::Sm,
                )
                .label(t(cx, "Retry"))
                .disabled(!can_read)
                .on_click(cx.listener(|this, _, _, cx| this.load_acp_capabilities(cx))),
            );
        }
        let mut sessions = v_flex().gap(r(4.));
        for (index, session) in state.page.sessions.iter().enumerate() {
            let label = session.title.as_ref().unwrap_or(&session.session_id);
            let needs_import = session.needs_import();
            let import = session.clone();
            let mut actions = h_flex().gap(r(4.)).child(
                Button::new(
                    ("acp-import", index),
                    ButtonVariant::GhostMuted,
                    ButtonSize::Xs,
                )
                .label(t(cx, if needs_import { "Import" } else { "Imported" }))
                .disabled(
                    busy || !can_operate || !needs_import || !(caps.can_load || caps.can_resume),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mutate_acp_session(SessionAction::Import(import.clone()), cx)
                })),
            );
            if let Some(thread) = session.imported_thread_id.clone() {
                actions = actions.child(
                    Button::new(
                        ("acp-open-thread", index),
                        ButtonVariant::GhostMuted,
                        ButtonSize::Xs,
                    )
                    .label(t(cx, "Open thread"))
                    .disabled(!can_read)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_acp_thread(&thread, window, cx)
                    })),
                );
            } else if caps.can_delete {
                let id = session.session_id.clone();
                let title = t_fmt(
                    cx,
                    "Permanently delete native ACP session \"{session}\"?",
                    &[("session", label.as_str().into())],
                );
                actions = actions.child(
                    Button::new(
                        ("acp-delete-session", index),
                        ButtonVariant::GhostDestructive,
                        ButtonSize::Xs,
                    )
                    .label(t(cx, "Delete"))
                    .disabled(busy || !can_operate)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_acp_session_action(
                            SessionAction::Delete(id.clone()),
                            title.clone(),
                            "Delete",
                            cx,
                        )
                    })),
                );
            }
            sessions = sessions.child(
                h_flex()
                    .gap(r(8.))
                    .justify_between()
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .py(r(6.))
                    .child(
                        v_flex()
                            .min_w_0()
                            .text_xs()
                            .child(div().truncate().child(label.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(session.session_id.clone()),
                            ),
                    )
                    .child(actions.flex_none()),
            );
        }
        if state.page.cursor.is_some() {
            sessions = sessions.child(
                Button::new("acp-load-more", ButtonVariant::GhostMuted, ButtonSize::Xs)
                    .label(t(cx, "Load more"))
                    .disabled(busy || !can_read)
                    .on_click(cx.listener(|this, _, _, cx| this.load_acp_sessions(true, cx))),
            );
        } else if state.page.listed && state.page.sessions.is_empty() {
            sessions = sessions.child(
                div()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(t(cx, "No sessions found.")),
            );
        }
        let mut rows = vec![
            Row::new("acp-sessions", t(cx, "Native sessions"))
                .description(t(
                    cx,
                    if state.projects.is_empty() && (caps.can_list || routing) {
                        "Add a project before importing sessions."
                    } else {
                        "Resume agent-owned conversations as T3 threads."
                    },
                ))
                .control(control)
                .below(sessions)
                .into_any_element(),
        ];
        if routing {
            let mut routing_rows = v_flex().gap(r(8.));
            for (index, draft) in state.page.providers.iter().enumerate() {
                routing_rows = routing_rows.child(self.render_acp_routing_row(
                    draft,
                    index,
                    busy || !can_manage,
                    cx,
                ));
            }
            rows.push(
                Row::new("acp-routing", t(cx, "Agent providers"))
                    .description(t(
                        cx,
                        "Configure non-secret routing. Headers are write-only.",
                    ))
                    .control(
                        Button::new("acp-list-providers", ButtonVariant::Outline, ButtonSize::Sm)
                            .label(t(
                                cx,
                                if state.page.providers.is_empty() {
                                    "List providers"
                                } else {
                                    "Refresh"
                                },
                            ))
                            .disabled(busy || !can_read || state.project.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.load_acp_providers(window, cx)
                            })),
                    )
                    .below(routing_rows)
                    .into_any_element(),
            );
        }
        if let Some(error) = state.page.error.clone().or(capability_error) {
            rows.push(
                div()
                    .px(r(16.))
                    .text_xs()
                    .text_color(hsla(p.error_foreground))
                    .child(error)
                    .into_any_element(),
            );
        }
        Some(flat_section(t(cx, "ACP Registry"), rows, cx).into_any_element())
    }

    fn render_acp_routing_row(
        &self,
        draft: &ProviderDraft,
        index: usize,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = draft.provider.provider_id.clone();
        let saved = draft.provider.current.is_some();
        let pick_id = id.clone();
        let change = cx.listener(move |this, protocol: &SharedString, _, cx| {
            if let Some(draft) = this
                .providers
                .acp_sessions
                .page
                .providers
                .iter_mut()
                .find(|draft| draft.provider.provider_id == pick_id)
            {
                draft.protocol = protocol.to_string();
                cx.notify();
            }
        });
        let save_id = id.clone();
        let title = t_fmt(
            cx,
            "Disable ACP provider \"{provider}\"?",
            &[("provider", id.as_str().into())],
        );
        let p = palette(cx);
        v_flex()
            .gap(r(6.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6))
            .pt(r(8.))
            .child(
                h_flex()
                    .gap(r(6.))
                    .justify_between()
                    .child(
                        v_flex()
                            .min_w_0()
                            .text_xs()
                            .child(div().truncate().child(id.clone()))
                            .child(
                                div()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(t(cx, if saved { "Configured" } else { "Disabled" }))
                                    .children(
                                        draft
                                            .provider
                                            .required
                                            .then(|| format!(" · {}", t(cx, "Required"))),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(r(4.))
                            .child(
                                Button::new(
                                    ("acp-save-routing", index),
                                    ButtonVariant::Outline,
                                    ButtonSize::Xs,
                                )
                                .label(t(cx, "Save"))
                                .disabled(
                                    disabled
                                        || draft.protocol.is_empty()
                                        || draft.url.read(cx).value().trim().is_empty(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.save_acp_routing(&save_id, false, window, cx)
                                    },
                                )),
                            )
                            .children((saved && !draft.provider.required).then(|| {
                                Button::new(
                                    ("acp-disable-routing", index),
                                    ButtonVariant::GhostDestructive,
                                    ButtonSize::Xs,
                                )
                                .label(t(cx, "Disable"))
                                .disabled(disabled)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        let state = &this.providers.acp_sessions;
                                        let Some(target) = state.target.clone() else {
                                            return;
                                        };
                                        let project = state.project.clone();
                                        let id = id.clone();
                                        let answer = window.prompt(
                                            gpui::PromptLevel::Warning,
                                            &title,
                                            None,
                                            &[t(cx, "Disable"), t(cx, "Cancel")],
                                            cx,
                                        );
                                        cx.spawn_in(window, async move |this, cx| {
                                            if answer.await == Ok(0) {
                                                let _ = this.update_in(cx, |this, window, cx| {
                                                    if this.acp_page_current(
                                                        &target,
                                                        project.as_deref(),
                                                        cx,
                                                    ) {
                                                        this.save_acp_routing(
                                                            &id, true, window, cx,
                                                        );
                                                    }
                                                });
                                            }
                                        })
                                        .detach();
                                    },
                                ))
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap(r(6.))
                    .child(
                        Select::new(
                            ("acp-protocol", index),
                            draft
                                .provider
                                .supported
                                .iter()
                                .map(|protocol| {
                                    SelectOption::new(protocol.clone(), protocol.clone())
                                })
                                .collect(),
                        )
                        .selected(draft.protocol.clone())
                        .width(120.)
                        .disabled(disabled)
                        .on_change(move |value, window, cx| change(&value, window, cx)),
                    )
                    .child(
                        div().min_w_0().flex_1().child(
                            Input::new(&draft.url)
                                .small()
                                .disabled(disabled)
                                .theme_slot(ThemeSlot::Input, cx),
                        ),
                    ),
            )
            .child(
                Input::new(&draft.headers)
                    .small()
                    .disabled(disabled)
                    .theme_slot(ThemeSlot::Input, cx),
            )
            .into_any_element()
    }
}

enum SessionAction {
    Import(Session),
    Delete(String),
    Logout,
}

impl ProviderDraft {
    fn new(provider: Provider, window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let protocol = provider
            .current
            .as_ref()
            .map(|current| current.api_type.clone())
            .or_else(|| provider.supported.first().cloned())
            .unwrap_or_default();
        let base_url = provider
            .current
            .as_ref()
            .map(|current| current.base_url.as_str())
            .unwrap_or("");
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.example.com")
                .default_value(base_url.to_owned())
        });
        let placeholder = t(cx, "Write-only headers JSON");
        let headers = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(placeholder)
        });
        let url_change = cx.subscribe(&url, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            provider,
            protocol,
            url,
            headers,
            _url_change: url_change,
        }
    }
}

fn parse_headers(text: &str) -> Result<Option<BTreeMap<String, String>>, serde_json::Error> {
    if text.is_empty() {
        Ok(None)
    } else {
        serde_json::from_str::<BTreeMap<String, String>>(text).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_headers_are_write_only_string_maps() {
        assert_eq!(parse_headers("").unwrap(), None);
        assert_eq!(
            parse_headers(r#"{"Authorization":"Bearer x"}"#)
                .unwrap()
                .unwrap()["Authorization"],
            "Bearer x"
        );
        for input in ["[]", "null", r#"{"Authorization":42}"#] {
            assert!(parse_headers(input).is_err());
        }
    }
}
