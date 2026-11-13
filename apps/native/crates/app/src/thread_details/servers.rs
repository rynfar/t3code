//! `ThreadServersPanel` (`apps/web/src/components/chat/ThreadServersPanel.tsx`):
//! discovered local servers bound to the open thread, paginated at 4 per page,
//! opening in preview (or external browser on modifier/middle click) and stopping
//! when `terminal:operate` and `capabilities.localServerStop` permit it.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Task, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_client::session::RpcError;
use t3_client::supervisor::ConnectionStatus;
use t3_core::auth::TERMINAL_OPERATE_SCOPE;
use t3_core::preview::{DiscoveredServer, PREVIEW_OPERATE_SCOPE, decode_discovered_servers, method};
use t3_core::thread_details::{
    SERVER_PAGE_COUNT, can_stop_server, filter_thread_servers, resolve_discovered_server_url, server_subtitle, stop_server_error_message,
    stop_server_payload, visible_server_page,
};

use super::controls::{Row, hover_fill, section};
use super::view::DetailsView;
use crate::app_state::{AppState, Phase};
use crate::icons::Icon;
use crate::right_panel::{self, focusable, tooltip_view};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";

struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) struct ServersFeed {
    connection: Option<u64>,
    servers: Vec<DiscoveredServer>,
    stopping: HashSet<(i64, u16)>,
    pub visible_limit: usize,
    feed: Option<(Abort, Task<()>)>,
}

impl ServersFeed {
    pub fn new() -> Self {
        ServersFeed { connection: None, servers: Vec::new(), stopping: HashSet::new(), visible_limit: SERVER_PAGE_COUNT, feed: None }
    }

    pub fn reset_pagination(&mut self) {
        self.visible_limit = SERVER_PAGE_COUNT;
    }

    pub fn count_for(&self, thread_id: &str) -> usize {
        filter_thread_servers(&self.servers, thread_id).len()
    }

    /// Subscribes to `subscribeDiscoveredLocalServers` only while the details card is visible and
    /// `servers` is enabled; drops the stream immediately when hidden or offline.
    pub fn sync(&mut self, state: &Entity<AppState>, active: bool, cx: &mut Context<DetailsView>) {
        let app = state.read(cx);
        let online = matches!(app.phase, Phase::Online(ConnectionStatus::Connected));
        let connection = app.connection_id();
        if !active || !online || connection.is_none() {
            if self.feed.take().is_some() {
                self.connection = None;
                self.servers.clear();
                self.stopping.clear();
            }
            return;
        }
        if self.feed.is_some() && self.connection == connection {
            return;
        }
        self.feed = None;
        self.connection = connection;
        self.servers.clear();
        self.stopping.clear();

        let Some(client) = app.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<DiscoveredServer>>();
        let driver = app.runtime().spawn(async move {
            let mut attempt = 0u32;
            loop {
                match client.stream(method::SUBSCRIBE_DISCOVERED_SERVERS, json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    attempt = 0;
                                    if tx.send(decode_discovered_servers(&value)).is_err() {
                                        return;
                                    }
                                }
                                Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                    Err(_) => {}
                }
                if tx.is_closed() {
                    return;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((500u64 << attempt.min(4)).min(8_000))).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(mut servers) = rx.recv().await {
                while let Ok(newer) = rx.try_recv() {
                    servers = newer;
                }
                let applied = this.update(cx, |view, cx| {
                    if view.servers.servers != servers {
                        view.servers.servers = servers;
                        cx.notify();
                    }
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.feed = Some((Abort(driver), pump));
    }

    pub fn stop_server(&mut self, state: &Entity<AppState>, pid: i64, port: u16, cx: &mut Context<DetailsView>) {
        if !self.stopping.insert((pid, port)) {
            return;
        }
        let request = state.read(cx).rpc(method::STOP_LOCAL_SERVER, stop_server_payload(pid, port));
        let state = state.clone();
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |view, cx| {
                view.servers.stopping.remove(&(pid, port));
                let message = match reply {
                    Ok(_) => None,
                    Err(RpcError::Failure(failure)) => {
                        let raw = format!(
                            "{} {} {}",
                            failure.tag.as_deref().unwrap_or(""),
                            failure.error,
                            failure.message,
                        );
                        Some(stop_server_error_message(&raw))
                    }
                    Err(error) => Some(stop_server_error_message(&error.to_string())),
                };
                if let Some(message) = message {
                    state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not stop server").description(message), cx));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn render(&self, state: &Entity<AppState>, thread_id: &str, separated: bool, window: &mut Window, cx: &mut Context<DetailsView>) -> Option<AnyElement> {
        let matching = filter_thread_servers(&self.servers, thread_id);
        if matching.is_empty() {
            return None;
        }
        let app = state.read(cx);
        let grants = |scope: &str| {
            app.client_handle()
                .is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|session| session.grants(scope)))
        };
        let can_open = grants(PREVIEW_OPERATE_SCOPE);
        let can_stop = grants(TERMINAL_OPERATE_SCOPE);
        let local_server_stop = app.server_config.config.as_ref().and_then(|config| config.environment.capabilities.local_server_stop);
        let env_http_base = app.active_environment().map(|env| env.http_base.clone());
        let (visible_count, hidden_count) = visible_server_page(matching.len(), self.visible_limit);
        let any_stopping = !self.stopping.is_empty();
        let p = palette(cx);
        let fill = hover_fill(cx);
        let external_hint = if cfg!(target_os = "macos") { "\u{2318}-click" } else { "Ctrl-click" };
        let rows: Vec<AnyElement> = matching
            .iter()
            .take(visible_count)
            .map(|server| {
                let port = server.port;
                let preview_url = server.url.clone();
                let external_url = resolve_discovered_server_url(&server.url, env_http_base.as_deref());
                let middle_url = external_url.clone();
                let target_thread = thread_id.to_owned();
                let subtitle = server_subtitle(server);
                let row_id: SharedString = format!("details-server-{port}").into();
                let row_group: SharedString = format!("details-server-row-{port}").into();
                let tip: SharedString = format!("Open localhost:{port} ({external_hint} for external browser)").into();
                let open_area = focusable(div().id(row_id.clone()), gpui::ElementId::Name(row_id), can_open, window, cx)
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .when(!can_open, |this| this.opacity(0.64))
                    .when(can_open, |this| {
                        this.cursor_pointer()
                            .hover(move |style| style.bg(fill))
                            .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
                            .on_mouse_down(MouseButton::Middle, move |_, _, cx| cx.open_url(&middle_url))
                            .on_click(move |event, _, cx| {
                                if event.modifiers().platform || event.modifiers().control {
                                    cx.open_url(&external_url);
                                } else if let Some(previews) = crate::preview::previews(cx) {
                                    previews.update(cx, |previews, cx| previews.open_link(target_thread.clone(), preview_url.clone(), cx));
                                } else {
                                    cx.open_url(&external_url);
                                }
                            })
                    })
                    .child(Icon::Globe.sized(px(16.)).text_color(hsla(p.muted_foreground)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.85)).child(format!("localhost:{port}")))
                            .child(div().truncate().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(subtitle)),
                    );

                let stop_btn = can_stop_server(server, can_stop, local_server_stop)
                    .then_some(server.pid)
                    .flatten()
                    .map(|pid| {
                        let stopping_this = self.stopping.contains(&(pid, port));
                        let state = state.clone();
                        let button = right_panel::Button::new(SharedString::from(format!("details-server-stop-{pid}-{port}")))
                            .variant(right_panel::Variant::DestructiveGhost)
                            .icon(if stopping_this { Icon::LoaderCircle } else { Icon::Square })
                            .icon_size(12.)
                            .disabled(any_stopping)
                            .tooltip("Stop server")
                            .on_click(cx.listener(move |view, _, _, cx| view.servers.stop_server(&state, pid, port, cx)));
                        div()
                            .flex_none()
                            .when(!stopping_this, |this| this.invisible().group_hover(row_group.clone(), |style| style.visible()))
                            .child(button)
                    });

                h_flex()
                    .group(row_group)
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap(px(4.))
                    .px(px(4.))
                    .child(open_area)
                    .children(stop_btn)
                    .into_any_element()
            })
            .collect();

        let toggle_more = (matching.len() > SERVER_PAGE_COUNT).then(|| {
            if hidden_count > 0 {
                Row::new("details-servers-more", format!("Show {} more", hidden_count.min(SERVER_PAGE_COUNT)))
                    .icon(Icon::ChevronDown)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.servers.visible_limit += SERVER_PAGE_COUNT;
                        cx.notify();
                    }))
            } else {
                Row::new("details-servers-less", "Show less")
                    .icon(Icon::ChevronUp)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.servers.visible_limit = SERVER_PAGE_COUNT;
                        cx.notify();
                    }))
            }
        });

        Some(section(separated, Some("Servers".into()), cx).child(v_flex().gap(px(2.)).children(rows).children(toggle_more)).into_any_element())
    }
}
