//! Bidirectional issue↔thread linking controls (`IssueThreadLinks.tsx`, `ThreadLinkPicker.tsx`,
//! `pingdotgg/t3code#19270`, `pingdotgg/t3code#38771`).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled,
    Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_core::issues::links::{
    is_thread_linked_to_issue, link_issue_command, linked_threads_input, linked_threads_label,
    unlink_issue_command, visible_issue_linked_threads,
};
use t3_core::issues::wire::{
    IssueDetail, IssueLinkedThread, IssueLinkedThreadsResult, IssueRef, ThreadIssueKey,
    ThreadIssueLinkSource, decode, method,
};
use t3_core::sidebar::time::parse_iso;

use super::{
    can_operate_threads, dispatch_command_for_owner, environment_owner_for, read_for_owner, settle,
};
use crate::app_state::{AppState, ScopedRouteAction, uuid_v4};
use crate::i18n::t;
use crate::icons::Icon;
use crate::pull_requests::controls::{Button, Size, Variant};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// Manages `issues.linkedThreads`, live link refresh subscription, link/unlink dispatch, and
/// the `ThreadLinkPicker` modal state for one `IssueDetailPanel`.
#[derive(Default)]
pub(crate) struct IssueThreadLinksState {
    pub(crate) relations: Option<IssueLinkedThreadsResult>,
    /// Last known linked threads retained across refreshes so the count never flickers.
    pub(crate) last_relations: Option<IssueLinkedThreadsResult>,
    pub(crate) error: Option<String>,
    pub(crate) unsupported: bool,
    pub(crate) pending: bool,
    pub(crate) picker_open: bool,
    pub(crate) picker_input: Option<Entity<InputState>>,
    pub(crate) picker_query: String,
    _picker_sub: Option<Subscription>,
    _fetch: Option<Task<()>>,
    _mutate: Option<Task<()>>,
    _refresh_net: Option<tokio::task::JoinHandle<()>>,
    _refresh_ui: Option<Task<()>>,
}

impl Drop for IssueThreadLinksState {
    fn drop(&mut self) {
        if let Some(handle) = self._refresh_net.take() {
            handle.abort();
        }
    }
}

impl IssueThreadLinksState {
    pub(crate) fn reset(&mut self) {
        if let Some(handle) = self._refresh_net.take() {
            handle.abort();
        }
        self._refresh_ui = None;
        self._fetch = None;
        self._mutate = None;
        self.relations = None;
        self.last_relations = None;
        self.error = None;
        self.unsupported = false;
        self.pending = false;
        self.picker_open = false;
        self.picker_query.clear();
    }

    pub(crate) fn fetch<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        environment_id: Option<&str>,
        key: &ThreadIssueKey,
        get: fn(&mut V) -> &mut IssueThreadLinksState,
        cx: &mut Context<V>,
    ) {
        if self.unsupported {
            return;
        }
        let owner = environment_owner_for(state.read(cx), environment_id);
        let task = read_for_owner(
            state.read(cx),
            owner.as_ref(),
            method::LINKED_THREADS,
            linked_threads_input(key),
            decode::<IssueLinkedThreadsResult>,
        );
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |view, cx| {
                let links = get(view);
                match result {
                    Ok(data) => {
                        links.last_relations = Some(data.clone());
                        links.relations = Some(data);
                        links.error = None;
                    }
                    Err(failure) => {
                        if failure.unsupported {
                            links.unsupported = true;
                        }
                        links.error = Some(failure.message);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn ensure_subscription<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        environment_id: Option<String>,
        key: ThreadIssueKey,
        get: fn(&mut V) -> &mut IssueThreadLinksState,
        cx: &mut Context<V>,
    ) {
        if self.unsupported || self._refresh_net.is_some() {
            return;
        }
        let app = state.read(cx);
        let owner = environment_owner_for(app, environment_id.as_deref());
        let dest_fut = owner.as_ref().map(|owner| {
            app.connections
                .destination(owner, t3_core::auth::ORCHESTRATION_READ_SCOPE)
        });
        let fallback_client = if dest_fut.is_none() {
            let Some(client) = app.client_handle() else {
                return;
            };
            Some(client)
        } else {
            None
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let runtime = app.runtime().clone();
        let net = runtime.spawn(async move {
            let client = match dest_fut {
                Some(fut) => match fut.await {
                    Ok(dest) => dest.client,
                    Err(_) => return,
                },
                None => match fallback_client {
                    Some(client) => client,
                    None => return,
                },
            };
            let mut delay = Duration::from_secs(1);
            loop {
                match client
                    .stream(method::SUBSCRIBE_LINKED_THREAD_REFRESHES, json!({}))
                    .await
                {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(_) => {
                                    delay = Duration::from_secs(1);
                                    if tx.send(()).is_err() {
                                        return;
                                    }
                                }
                                Err(t3_client::session::RpcError::Failure(failure))
                                    if failure.tag.as_deref()
                                        == Some("EnvironmentAuthorizationError") =>
                                {
                                    return;
                                }
                                Err(t3_client::session::RpcError::Closed) => return,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(t3_client::session::RpcError::Failure(failure))
                        if failure.tag.as_deref() == Some("EnvironmentAuthorizationError") =>
                    {
                        return;
                    }
                    Err(t3_client::session::RpcError::Closed) => return,
                    Err(t3_client::session::RpcError::Failure(_)) => {
                        // Older servers reject `subscribeLinkedThreadRefreshes`; fall back to 10s polling.
                        loop {
                            tokio::time::sleep(Duration::from_secs(
                                t3_core::pull_requests::link::LINKED_THREADS_POLL_SECONDS,
                            ))
                            .await;
                            if tx.send(()).is_err() {
                                return;
                            }
                        }
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        });
        let state_clone = state.clone();
        let ui = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_some() {
                while rx.try_recv().is_ok() {}
                let keep = this.update(cx, |view, cx| {
                    get(view).fetch(&state_clone, environment_id.as_deref(), &key, get, cx);
                });
                if keep.is_err() {
                    break;
                }
            }
        });
        self._refresh_net = Some(net);
        self._refresh_ui = Some(ui);
    }

    pub(crate) fn open_picker<V: 'static>(
        &mut self,
        get: fn(&mut V) -> &mut IssueThreadLinksState,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let placeholder = t(cx, "Search threads or projects...").to_owned();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let sub = cx.subscribe_in(&input, window, move |view, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                get(view).picker_query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        self.picker_query.clear();
        self.picker_input = Some(input);
        self._picker_sub = Some(sub);
        self.picker_open = true;
        cx.notify();
    }

    pub(crate) fn close_picker(&mut self) {
        self.picker_open = false;
        self.picker_query.clear();
        self.picker_input = None;
        self._picker_sub = None;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn change_link<V: 'static>(
        &mut self,
        state: &Entity<AppState>,
        environment_id: Option<String>,
        reference: &IssueRef,
        issue: &IssueDetail,
        thread_id: String,
        remove: bool,
        get: fn(&mut V) -> &mut IssueThreadLinksState,
        cx: &mut Context<V>,
    ) {
        if self.pending {
            return;
        }
        let host = reference
            .host
            .clone()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| issue.host.clone());
        let key = ThreadIssueKey {
            tracker: reference.tracker.clone(),
            host,
            scope: reference.scope.clone(),
            key: reference.key.clone(),
        };
        let command_id = uuid_v4();
        let command = if remove {
            unlink_issue_command(&command_id, &thread_id, &key)
        } else {
            link_issue_command(
                &command_id,
                &thread_id,
                &key,
                &reference.project_id,
                &issue.url,
                &issue.display_key,
                &issue.title,
                ThreadIssueLinkSource::Manual,
            )
        };
        let owner = environment_owner_for(state.read(cx), environment_id.as_deref());
        let task = dispatch_command_for_owner(state.read(cx), owner.as_ref(), command);
        self.pending = true;
        let state_clone = state.clone();
        self._mutate = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |view, cx| {
                let links = get(view);
                links.pending = false;
                match result {
                    Ok(_) => {
                        links.close_picker();
                        links.fetch(&state_clone, environment_id.as_deref(), &key, get, cx);
                    }
                    Err(failure) => {
                        let title = if remove {
                            t(cx, "Could not unlink the issue").to_owned()
                        } else {
                            t(cx, "Could not link the issue").to_owned()
                        };
                        state_clone.update(cx, |app, cx| {
                            app.show_toast(
                                Toast::new(ToastKind::Error, title).description(failure.message),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn visible_threads(&self, state: &AppState, env_id: Option<&str>) -> Vec<IssueLinkedThread> {
        let source = self
            .relations
            .as_ref()
            .or(self.last_relations.as_ref())
            .map_or(&[][..], |r| r.threads.as_slice());
        let loaded = loaded_thread_ids(state, env_id);
        let hiding_anywhere =
            state.hiding_thread_tags() || !state.sidebar.hidden_thread_ids.is_empty();
        visible_issue_linked_threads(
            source,
            &loaded,
            &state.sidebar.hidden_thread_ids,
            hiding_anywhere,
        )
        .into_iter()
        .cloned()
        .collect()
    }
}

fn loaded_thread_ids(state: &AppState, env_id: Option<&str>) -> HashSet<String> {
    if let Some(target) = env_id
        && let Some(env) = state.fleet.environments.iter().find(|env| {
            env.summary.owner.saved_id == target
                || env.summary.owner.canonical_id.as_deref() == Some(target)
                || env
                    .config
                    .config
                    .as_ref()
                    .is_some_and(|c| c.environment.environment_id == target)
        })
    {
        return env
            .summary
            .threads
            .iter()
            .map(|scoped| scoped.thread.id.clone())
            .collect();
    }
    state
        .shell
        .snapshot
        .as_ref()
        .map(|snap| snap.threads.iter().map(|t| t.id.clone()).collect())
        .unwrap_or_default()
}

struct PickerThread {
    id: String,
    project_id: String,
    title: String,
    updated_at_ms: i64,
    linked: bool,
}

fn environment_threads_and_projects(
    state: &AppState,
    env_id: Option<&str>,
    key: &ThreadIssueKey,
    linked_ids: &HashSet<String>,
) -> (Vec<PickerThread>, HashMap<String, String>) {
    if let Some(target) = env_id
        && let Some(env) = state.fleet.environments.iter().find(|env| {
            env.summary.owner.saved_id == target
                || env.summary.owner.canonical_id.as_deref() == Some(target)
                || env
                    .config
                    .config
                    .as_ref()
                    .is_some_and(|c| c.environment.environment_id == target)
        })
    {
        let projects: HashMap<String, String> = env
            .summary
            .projects
            .iter()
            .map(|scoped| (scoped.project.id.clone(), scoped.project.title.clone()))
            .collect();
        let threads: Vec<PickerThread> = env
            .summary
            .threads
            .iter()
            .filter(|scoped| !scoped.thread.archived)
            .map(|scoped| {
                let linked = linked_ids.contains(&scoped.thread.id)
                    || state
                        .thread_shell(&scoped.thread.id)
                        .is_some_and(|shell| is_thread_linked_to_issue(shell, key));
                PickerThread {
                    id: scoped.thread.id.clone(),
                    project_id: scoped.thread.project_id.clone(),
                    title: scoped.thread.title.clone(),
                    updated_at_ms: scoped.thread.updated_at.unwrap_or(0),
                    linked,
                }
            })
            .collect();
        return (threads, projects);
    }
    let Some(snapshot) = &state.shell.snapshot else {
        return (Vec::new(), HashMap::new());
    };
    let projects: HashMap<String, String> = snapshot
        .projects
        .iter()
        .map(|p| (p.id.clone(), p.title.clone()))
        .collect();
    let threads: Vec<PickerThread> = snapshot
        .threads
        .iter()
        .filter(|t| t.archived_at.is_none())
        .map(|t| PickerThread {
            id: t.id.clone(),
            project_id: t.project_id.clone(),
            title: t.title.clone(),
            updated_at_ms: parse_iso(&t.updated_at).unwrap_or(0),
            linked: linked_ids.contains(&t.id) || is_thread_linked_to_issue(t, key),
        })
        .collect();
    (threads, projects)
}

fn navigate_to_linked_thread(
    state: &Entity<AppState>,
    environment_id: Option<String>,
    thread_id: String,
    cx: &mut App,
) {
    state.update(cx, |app, cx| {
        app.set_issues_open(false, cx);
        let active_env = app
            .environments
            .active
            .as_deref()
            .or_else(|| app.environment());
        if let Some(target_env) = environment_id.as_deref()
            && active_env != Some(target_env)
            && let Some(owner) = environment_owner_for(app, Some(target_env))
        {
            app.navigate_scoped(owner, ScopedRouteAction::Thread(thread_id), cx);
            return;
        }
        if app.thread_shell(&thread_id).is_some() {
            app.select_thread(&thread_id, cx);
        } else {
            app.open_related_thread(&thread_id, cx);
        }
    });
}

/// Renders the linked-threads count badge + link/unlink action buttons in the issue header
/// (`IssueThreadLinks.tsx`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_header_controls<V: 'static>(
    links: &IssueThreadLinksState,
    state: &Entity<AppState>,
    environment_id: Option<String>,
    reference: &IssueRef,
    issue: &IssueDetail,
    thread_id: Option<&str>,
    get: fn(&mut V) -> &mut IssueThreadLinksState,
    cx: &mut Context<V>,
) -> AnyElement {
    let app = state.read(cx);
    let host = reference
        .host
        .clone()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| issue.host.clone());
    let key = ThreadIssueKey {
        tracker: reference.tracker.clone(),
        host,
        scope: reference.scope.clone(),
        key: reference.key.clone(),
    };
    let can_operate = can_operate_threads(app, environment_id.as_deref());
    let thread_shell = thread_id.and_then(|id| app.thread_shell(id)).cloned();
    let linked_here = thread_shell
        .as_ref()
        .is_some_and(|thread| is_thread_linked_to_issue(thread, &key));
    let linked_threads = links.visible_threads(app, environment_id.as_deref());
    let relations_known = links.relations.is_some() || links.last_relations.is_some();
    let show_count = !linked_threads.is_empty() || (!relations_known && links.error.is_some());
    let count_text = if !linked_threads.is_empty() {
        linked_threads.len().to_string()
    } else {
        "?".to_owned()
    };
    let count_tooltip = linked_threads_label(linked_threads.len());

    let state_for_menu = state.clone();
    let env_for_menu = environment_id.clone();
    let menu_threads = linked_threads.clone();

    let count_btn = show_count.then(|| {
        Button::new("issue-linked-threads-btn", Variant::GhostMuted, Size::Xs)
            .icon(Icon::MessageSquare)
            .label(count_text)
            .tooltip(count_tooltip)
            .dropdown_menu(move |mut menu, _, cx| {
                menu = menu.label(t(cx, "Linked threads").to_owned());
                if menu_threads.is_empty() {
                    return menu.item(
                        PopupMenuItem::new(t(cx, "No linked threads loaded").to_owned())
                            .disabled(true),
                    );
                }
                for thread in &menu_threads {
                    let title = if thread.title.trim().is_empty() {
                        t(cx, "Untitled thread").to_owned()
                    } else {
                        thread.title.clone()
                    };
                    let state_nav = state_for_menu.clone();
                    let env_nav = env_for_menu.clone();
                    let tid = thread.id.clone();
                    menu = menu.item(PopupMenuItem::new(title).on_click(move |_, _, cx| {
                        navigate_to_linked_thread(&state_nav, env_nav.clone(), tid.clone(), cx);
                    }));
                }
                menu
            })
    });

    let link_btn = can_operate.then(|| {
        let tooltip = if linked_here {
            t(cx, "Unlink from this thread").to_owned()
        } else if thread_shell.is_some() {
            t(cx, "Link to this thread").to_owned()
        } else {
            t(cx, "Link to thread").to_owned()
        };
        let state_click = state.clone();
        let env_click = environment_id.clone();
        let ref_click = reference.clone();
        let issue_click = issue.clone();
        let current_tid = thread_shell.as_ref().map(|t| t.id.clone());
        Button::new("issue-link-thread-btn", Variant::GhostMuted, Size::IconSm)
            .icon(if linked_here { Icon::Unlink2 } else { Icon::Link2 })
            .tooltip(tooltip)
            .disabled(links.pending)
            .on_click(cx.listener(move |view, _, window, cx| {
                if let Some(tid) = current_tid.clone() {
                    get(view).change_link(
                        &state_click,
                        env_click.clone(),
                        &ref_click,
                        &issue_click,
                        tid,
                        linked_here,
                        get,
                        cx,
                    );
                } else {
                    get(view).open_picker(get, window, cx);
                }
            }))
    });

    h_flex()
        .gap(px(4.))
        .items_center()
        .children(count_btn)
        .children(link_btn)
        .into_any_element()
}

/// Renders the `ThreadLinkPicker` modal overlay (`ThreadLinkPicker.tsx`) when opened from a
/// standalone issue detail view.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_picker_overlay<V: 'static>(
    links: &IssueThreadLinksState,
    state: &Entity<AppState>,
    environment_id: Option<String>,
    reference: &IssueRef,
    issue: &IssueDetail,
    get: fn(&mut V) -> &mut IssueThreadLinksState,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    if !links.picker_open {
        return None;
    }
    let input = links.picker_input.as_ref()?.clone();
    let p = palette(cx);
    let host = reference
        .host
        .clone()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| issue.host.clone());
    let key = ThreadIssueKey {
        tracker: reference.tracker.clone(),
        host,
        scope: reference.scope.clone(),
        key: reference.key.clone(),
    };
    let linked_ids: HashSet<String> = links
        .visible_threads(state.read(cx), environment_id.as_deref())
        .into_iter()
        .map(|t| t.id)
        .collect();
    let (mut threads, project_names) =
        environment_threads_and_projects(state.read(cx), environment_id.as_deref(), &key, &linked_ids);
    let needle = links.picker_query.trim().to_lowercase();
    threads.retain(|thread| {
        if needle.is_empty() {
            return true;
        }
        let proj = project_names
            .get(&thread.project_id)
            .map(String::as_str)
            .unwrap_or("");
        format!("{} {}", thread.title, proj)
            .to_lowercase()
            .contains(&needle)
    });
    threads.sort_by_key(|b| std::cmp::Reverse(b.updated_at_ms));
    let pending = links.pending;

    let rows: Vec<AnyElement> = if threads.is_empty() {
        vec![
            div()
                .px(px(12.))
                .py(px(20.))
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(t(cx, "No active threads found.").to_owned())
                .into_any_element(),
        ]
    } else {
        threads
            .into_iter()
            .take(40)
            .enumerate()
            .map(|(idx, thread)| {
                let linked = thread.linked;
                let title = if thread.title.trim().is_empty() {
                    t(cx, "Untitled thread").to_owned()
                } else {
                    thread.title.clone()
                };
                let project = project_names
                    .get(&thread.project_id)
                    .cloned()
                    .unwrap_or_default();
                let tid = thread.id.clone();
                let state_pick = state.clone();
                let env_pick = environment_id.clone();
                let ref_pick = reference.clone();
                let issue_pick = issue.clone();
                let disabled = pending || linked;
                h_flex()
                    .id(SharedString::from(format!("issue-thread-pick-{idx}")))
                    .w_full()
                    .min_w_0()
                    .px(px(10.))
                    .py(px(8.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .items_center()
                    .when(disabled, |this| this.opacity(0.6))
                    .when(!disabled, |this| {
                        this.cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                get(view).change_link(
                                    &state_pick,
                                    env_pick.clone(),
                                    &ref_pick,
                                    &issue_pick,
                                    tid.clone(),
                                    false,
                                    get,
                                    cx,
                                );
                            }))
                    })
                    .child(
                        Icon::MessageSquare
                            .sized(px(14.))
                            .text_color(hsla(p.muted_foreground)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(hsla(p.foreground))
                                    .child(title),
                            )
                            .when(!project.is_empty(), |this| {
                                this.child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(project),
                                )
                            }),
                    )
                    .when(linked, |this| {
                        this.child(
                            h_flex()
                                .flex_none()
                                .gap(px(4.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(Icon::Check.sized(px(12.)))
                                .child(t(cx, "Linked").to_owned()),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    };

    Some(
        div()
            .id("issue-thread-picker-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(p.background).opacity(0.6))
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |view, _, _, cx| {
                    get(view).close_picker();
                    cx.notify();
                }),
            )
            .child(
                v_flex()
                    .id("issue-thread-picker-dialog")
                    .w(px(380.))
                    .max_w_full()
                    .max_h(px(420.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.popover))
                    .shadow_lg()
                    .p(px(12.))
                    .gap(px(8.))
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .child(t(cx, "Link issue to a thread").to_owned()),
                            )
                            .child(
                                Button::new("issue-thread-picker-close", Variant::GhostMuted, Size::IconSm)
                                    .icon(Icon::X)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        get(view).close_picker();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(Input::new(&input).disabled(pending))
                    .child(
                        div()
                            .id("issue-thread-picker-list")
                            .flex_1()
                            .min_h_0()
                            .max_h(px(300.))
                            .overflow_y_scroll()
                            .child(v_flex().gap(px(2.)).children(rows)),
                    ),
            )
            .into_any_element(),
    )
}
